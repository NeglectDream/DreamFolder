//! 应用层：状态机、命令行解析、消息循环。
//!
//! 这里的结构与原软件的行为一一对应，命令行开关取自 PE 字符串
//! （`/OpenInNewWindow`、`/lowrun`、`/replace_explorer_all` 等）。

use  std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetDC, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, ReleaseDC,
    UpdateWindow, ValidateRect, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
// WM_MOUSELEAVE 在 windows-sys 里归到 UI::Controls，不在 WindowsAndMessaging。
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::cfg::settings::{self, StartMode, ThemeKind, ViewMode};
use crate::cfg::Config;
use crate::fs::entry::{Entry, EntryKind};
use crate::fs::enumerate::ListOptions;
use crate::fs::icons::IconCache;
use crate::fs::icons::IconSize;
use crate::fs::{self, ops as fsops, shell, Sort};
use crate::ipc::PipeServer;
use crate::strings as S;
use crate::ui::{self, draw, interact, Layout};
use crate::win::paint::{BackBuffer, Canvas, FontCache, Palette};
use crate::win::{
    droptarget, from_wide, hwnd_valid, module_handle, to_wide, wnd as winwnd,
};

use crate::io::{Job, ListKind, OpWhat};

mod cli;
mod edit;
mod ops;
mod search;
mod wnd;

use wnd::{default_window_size, wnd_proc};

pub use cli::CommandLine;

/// 窗口类名。
const CLASS_NAME: &str = "DreamFolder_Frame";

/// 回收站视图用的伪路径。它不会被当成真实目录去 `read_dir`，
/// 只用于历史记录和「当前在哪」的比较。
const RECYCLE_PATH: &str = "::{645FF040-5081-101B-9F08-00AA002F954E}";

/// 单个字符在等宽近似里占几个单位：ASCII/半角记 1，CJK/全角记 2。
///
/// 只留给「正文要占几行」这类粗排估算用（见 `ui::dialog::body_lines`）。
/// 插入符和选区不要再用它：那两处必须按真实字宽量（`measure_prefixes`），
/// 否则中文会偏出半格、半角字符之间也会累积误差。
fn char_units(c: char) -> i32 {
    let v = c as u32;
    let wide = (0x1100..=0x115F).contains(&v)
        || (0x2E80..=0xA4CF).contains(&v)
        || (0xAC00..=0xD7A3).contains(&v)
        || (0xF900..=0xFAFF).contains(&v)
        || (0xFE30..=0xFE4F).contains(&v)
        || (0xFF00..=0xFF60).contains(&v)
        || (0xFFE0..=0xFFE6).contains(&v);
    if wide {
        2
    } else {
        1
    }
}

/// 一段文本的宽度单位总数（`char_units` 求和）。
pub fn text_units(chars: &[char]) -> i32 {
    chars.iter().map(|c| char_units(*c)).sum()
}

/// 定时器 ID：轮询命名管道。
const TIMER_PIPE: usize = 1;
/// 定时器 ID：插入符闪烁。只驱动重绘，相位在绘制时算。
const TIMER_CARET: usize = 2;
/// 定时器 ID：工具栏悬停提示。一次性的——响过之后就 KillTimer。
const TIMER_HOVER: usize = 3;
/// 定时器 ID：搜索超时。一次性。
const TIMER_SEARCH: usize = 4;
/// 定时器 ID：状态栏瞬态提示（如「已复制路径」）。一次性。
const TIMER_NOTE: usize = 5;
/// 定时器 ID：目录变化后的防抖刷新。一次性。
const TIMER_FS: usize = 6;
/// 目录变化的防抖窗口：一次复制几百个文件会连投上百条消息，凑成一次刷新。
const FS_DEBOUNCE_MS: u32 = 300;
/// 目录变化监听线程投过来的唤醒（只用来起防抖定时器）。
pub const WM_APP_FS: u32 = windows_sys::Win32::UI::WindowsAndMessaging::WM_APP + 2;
/// 状态栏提示停留多久。
const NOTE_MS: u32 = 3000;
/// 搜索结果上限。搜索是「找得到就行」，不做全量列举。
const SEARCH_LIMIT: usize = 500;
/// 递归扫描的时间预算，超时就用已扫到的部分。
const SCAN_BUDGET_MS: u32 = 400;
/// 最多等搜索多久。系统索引查询本身没有超时，索引服务无响应时
/// 界面不能一直停在「正在搜索…」。
const SEARCH_TIMEOUT_MS: u32 = 8000;
/// 地址栏历史下拉最多列几条。多到超过这个数就不如让用户接着打字了。
pub const ADDR_SUGGEST_MAX: usize = 8;
/// 悬停多久才出提示条（毫秒）。
///
/// 500ms 是 Windows 自己的默认值（`SPI_GETMOUSEHOVERTIME`）：再短，
/// 鼠标扫过整排按钮会挨个闪一遍；再长，用户会以为这排按钮没有提示。
const HOVER_TIP_MS: u32 = 500;
/// 管道轮询间隔（毫秒）。空闲时开销可忽略。
const PIPE_POLL_MS: u32 = 400;

/// 私有消息：执行被合并的导航（见 `Application::pending_nav`）。
const NAV_MSG: u32 = 0x8000 + 11;

/// 「此电脑」伪路径：空路径代表驱动器列表视图。
pub const COMPUTER_VIEW: &str = "";


// ------------------------------------------------------------------ 状态

/// 修饰键状态。
#[derive(Debug, Clone, Copy, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// 搜索进行到哪了。
///
/// 只在前端体现，不影响搜索本身怎么做：有了它，「正在搜」和「搜完了但没有」
/// 才能画成两种文案——以前两者都是空列表配「此文件夹为空」，
/// 用户会以为那个文件真的不存在。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchState {
    /// 输入框里有词，但还没按回车
    Pending,
    /// 正在搜索（后台线程执行，见 `io::Job::Search`）
    Running,
    /// 搜索已结束，`hits` 是命中的条目数
    Done { hits: usize },
    /// 超过等待上限还没回来。多半是索引服务没响应——此时后台线程可能还挂着，
    /// 结果真回来了仍会被采纳（见 `on_io`）。
    TimedOut,
}

/// 一个标签页。
pub struct Tab {
    /// 当前目录。空表示「此电脑」（驱动器列表）。
    pub path: PathBuf,
    /// 是否为「此电脑」视图。
    pub is_computer: bool,
    /// 是否为回收站视图（Shell 虚拟目录，走 `fs::recycle` 枚举）。
    pub is_recycle: bool,
    pub title: String,
    /// 后退/前进历史。
    pub history: Vec<PathBuf>,
    /// 当前在 history 中的位置。
    pub history_pos: usize,
    pub entries: Vec<Entry>,
    pub selected: HashSet<usize>,
    /// 选择锚点（Shift 连选用）。
    pub anchor: Option<usize>,
    /// 键盘焦点项。方向键从它出发；Shift 扩展、Ctrl 只移动焦点不改选择。
    /// 与 `selected` 分开是资源管理器的基本模型——不分离就做不出
    /// 「Ctrl+方向键移动焦点但不改变选中」。
    pub focus: Option<usize>,
    /// 刷新前记下的选中/焦点路径，枚举落地后按路径恢复（F5 后选中不丢）。
    /// 只在同一个目录内刷新时有效，换目录由导航路径清空。
    pub pending_keep: Option<(Vec<std::path::PathBuf>, Option<std::path::PathBuf>)>,
    pub scroll_y: i32,
    pub view_mode: ViewMode,
    pub loading: bool,
    pub error: Option<String>,
    /// 排序（每标签独立）。
    pub sort: Sort,
    /// 目录枚举完成后要选中的文件名。
    ///
    /// 枚举移到了后台，所以「导航到某文件并在列表里选中它」没法立刻做完：
    /// 先记下名字，等结果回来再落地（见 `Application::on_io`）。
    pub pending_select: Option<String>,
    /// 刚新建出来的文件名：枚举落地后选中它并直接进入重命名。
    ///
    /// 资源管理器新建完立刻就能打字改名，是因为它的视图能收到 Shell 的通知；
    /// 我们自己画的列表收不到，只能由建文件的那一侧把名字递过来。
    pub pending_rename: Option<String>,
    /// 驱动器的卷标与容量（「此电脑」视图用）。后台补查后回填。
    pub drive_info: Vec<crate::io::DriveInfo>,

    // ---- 搜索（每标签一份：切走再切回不会丢掉搜索框内容和结果）
    /// 搜索框文本。
    pub search_text: String,
    /// 搜索框里的光标位置（字符下标）。
    pub search_caret: usize,
    /// 搜索进行到哪了（未在搜索时为 `None`）。
    pub search_state: Option<SearchState>,
    /// 搜索前的原始列表（退出搜索时恢复）。
    pub search_backup: Option<Vec<Entry>>,
    /// 目录还在加载时按了回车（或 `/searchkey` 启动）：等枚举落地后
    /// 自动补一次搜索，而不是无声丢掉。见 `begin_search`。
    pub pending_search: bool,
    /// 本标签最近一次枚举请求的序号。
    ///
    /// 以前用全局 `nav_seq` 校验结果：启动时恢复多个标签会同时发出多份
    /// 枚举，只有最后一份数字对得上，其余标签的结果全被丢掉——列表
    /// 永远停在「正在加载」。序号改成每标签一份，各自只跟自己的最新
    /// 请求比对；全局 `nav_seq` 仍保留，用于生成不重复的 ID。
    pub list_id: u64,
    /// 本标签最近一次搜索请求的序号。
    ///
    /// 搜索结果按 `(标签, 序号)` 校验。以前只有一个全局序号：两个标签
    /// 先后发起搜索，先发起的那个序号立刻过期，结果被丢弃，那个标签
    /// 只能一直转到「搜索超时」。
    pub search_id: u64,
}

impl Tab {
    fn new(path: PathBuf, view_mode: ViewMode, sort: Sort) -> Tab {
        let is_computer = path.as_os_str().is_empty();
        let title = if is_computer {
            S::STATUS_DRIVES.to_string()
        } else {
            crate::cfg::bookmarks::display_name(&path)
        };
        Tab {
            // 首个路径就是历史的第 0 项。以前这里留空，导致 history_pos
            // 恒为 0，从「此电脑」进任何目录后后退键都直接 return，
            // 因为根本没有更早的一步可退。
            history: vec![path.clone()],
            path,
            is_computer,
            is_recycle: false,
            title,
            history_pos: 0,
            entries: Vec::new(),
            selected: HashSet::new(),
            anchor: None,
            focus: None,
            pending_keep: None,
            scroll_y: 0,
            view_mode,
            loading: false,
            error: None,
            sort,
            pending_select: None,
            pending_rename: None,
            drive_info: Vec::new(),
            search_text: String::new(),
            search_caret: 0,
            search_state: None,
            search_backup: None,
            pending_search: false,
            list_id: 0,
            search_id: 0,
        }
    }
}

/// 把已查到的卷标/容量回填到「此电脑」的驱动器条目上。
///
/// 卷标查询和盘符列表是两条并行请求，谁先回来不一定：这个函数两边都会调，
/// 已经填过的再填一次也不会出错。
fn apply_drive_info(t: &mut Tab) {
    if t.drive_info.is_empty() {
        return;
    }
    let (infos, entries) = (&t.drive_info, &mut t.entries);
    for info in infos {
        let Some(e) = entries.iter_mut().find(|e| e.full == info.root) else {
            continue;
        };
        let letter = info
            .root
            .to_string_lossy()
            .trim_end_matches('\\')
            .to_string();
        // 卷标到了就把「C:」换成「本地磁盘 (C:)」/「数据 (D:)」
        e.name = if info.label.is_empty() {
            format!("本地磁盘 ({letter})")
        } else {
            format!("{} ({letter})", info.label)
        };
        e.volume = info.volume;
    }
}

/// 导航窗格的展开状态。
///
/// 只记「展开与否」，不缓存树本身——树在每次绘制时按当前系统状态现算，
/// 这样插拔 U 盘、网络位置变化都能立刻反映出来，不用做失效通知。
#[derive(Debug, Clone, Copy)]
pub struct NavExpanded {
    /// 「此电脑」是否展开（展开后列出各驱动器）。
    pub computer: bool,
    /// 「网络」是否展开。
    pub network: bool,
}

impl Default for NavExpanded {
    fn default() -> Self {
        // 「此电脑」默认展开：原版与资源管理器打开时都是这个状态
        NavExpanded {
            computer: true,
            network: false,
        }
    }
}

/// 应用主体。
pub struct Application {
    pub hwnd: HWND,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// 窗口是否处于激活状态（决定选中项用亮色还是灰底）。
    pub active_tab_focused: bool,

    pub cfg: Config,
    pub theme: ThemeKind,
    pub layout: Layout,

    /// 导航窗格的展开状态。不参与布局计算，只影响绘制与命中测试。
    pub nav_expanded: NavExpanded,

    /// 导航树的行（含矩形）。布局变化或展开状态切换时重建，
    /// 绘制与命中测试共用同一份。
    ///
    /// 之所以缓存而不是每次现算：树里的驱动器名要查卷标，那是磁盘调用，
    /// 而命中测试跑在鼠标移动路径上——现算会让拖动鼠标明显发涩。
    pub nav_rows: Vec<crate::ui::navpane::NavRow>,

    /// 框选矩形 `(起点x, 起点y, 当前x, 当前y)`，客户区坐标。
    ///
    /// 按下时落在空白区就建起来，但只有拖动超过系统阈值才真的开始选中；
    /// 没超过就仍按「点空白＝清空选择」处理。
    pub marquee: Option<(i32, i32, i32, i32)>,

    /// 可能在拖动某个条目：`(按下x, 按下y, 条目索引)`。
    ///
    /// 按下时记下，移动超过系统拖拽阈值才真正发起 DoDragDrop；
    /// 没超过就当作普通点击。不用「按下即开始拖」是因为那会让单击变成闪烁。
    pub drag_origin: Option<(i32, i32, usize)>,

    /// 正在拖动导航窗格分隔条。
    pub resizing_nav: bool,
    /// 正在拖动右侧详情窗格的分隔条。
    pub resizing_detail: bool,
    /// 正在拖动设置面板里的「界面缩放」滑块。
    pub dragging_scale: bool,
    /// 拖动开始时冻结的轨道范围 `(left, right)`：缩放一变轨道就重排，
    /// 换算必须一直用这根不动的轨道，否则光标会把滑块「拖丢」。
    pub scale_drag_span: Option<(i32, i32)>,

    /// 正在拖动详细信息视图的列宽：`(列号, 按下x, 按下时的逻辑宽度)`。
    pub drag_col: Option<(usize, i32, i32)>,
    /// 收藏栏按下记录：(项下标, x, y, 按下时刻)。长按超过阈值才转拖动排序。
    pub fav_press: Option<(usize, i32, i32, std::time::Instant)>,
    /// 正在被拖动的收藏项下标。
    pub fav_drag: Option<usize>,
    /// 拖动时的落点 x（画指示线用）。
    pub fav_drop: Option<i32>,
    /// 导航窗格快捷项按下记录：`(配置下标, x, y)`。越过拖拽阈值才转成排序拖动。
    pub nav_press: Option<(usize, i32, i32)>,
    /// 正在被拖动的快捷项（配置下标）。
    pub nav_drag: Option<usize>,
    /// 悬停提示是否已经到了该出现的时刻。
    pub tooltip_ready: bool,

    /// 详情窗格预览的缓存（图片解码 / 文本前若干行）。
    ///
    /// 只在选中项变化时重建：解码一张照片要几毫秒，而重绘一帧就问一次。
    pub preview: Option<crate::ui::preview::Preview>,

    // ---- 后台 IO ----
    /// 后台工作线程：枚举、容量、预览、搜索、文件操作都从这里出去。
    pub io: crate::io::Io,

    /// 当前目录的变化监听（`ReadDirectoryChangesW`）。切目录时重建。
    ///
    /// 只在活动标签是真实目录时存在：外部程序往当前目录写文件时，
    /// 界面要像资源管理器那样自己更新，而不是等用户按 F5。
    pub watcher: Option<crate::fs::watch::DirWatcher>,
    /// 三类任务的请求号，只认最新的一个——过期结果直接丢，
    /// 这样「连续切目录」「连续换选中项」不会被旧结果覆盖。
    nav_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    preview_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// 搜索的请求序号（发号器，全局一份；每标签的有效序号看 `Tab::search_id`）。
    search_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// 正在等预览结果的文件：同一份不要反复投递。
    preview_pending: Option<PathBuf>,

    /// 快捷项拖动时的落点 y（画插入指示线用）。
    pub nav_drop_y: Option<i32>,
    /// 文件正被拖到哪一条导航行上（松手就打标签的那一行）。
    ///
    /// 拖动期间由 `IDropTarget::DragOver` 投消息驱动，只在落点是标签行时
    /// 有值：打标签和「加入快捷访问」外观完全一样，不给高亮用户没法预判。
    pub drop_tag_row: Option<usize>,
    /// 各分组自上而下的顺序与屏幕范围（拖动分隔线换位时算落点用）。
    pub nav_groups: Vec<crate::ui::navpane::GroupSpan>,
    /// 正在拖动某条分组分隔线（＝拖动它下面那一组）。
    pub nav_sep_drag: Option<crate::ui::navpane::SepDrag>,
    /// 正在拖动详情窗格的分割线（＝拖动它下面那个模块）。
    pub detail_sep_drag: Option<crate::ui::detailtags::DetailSepDrag>,
    /// 导航窗格的纵向滚动量（设备像素，0 为顶端）。
    pub nav_scroll: i32,
    /// 导航窗格内容的可滚动余量，由 `navpane::build` 算出。
    pub nav_max_scroll: i32,
    /// 导航窗格内容总高度（算滚动条滑块长度用）。
    pub nav_content_h: i32,
    /// 正在被拖动的滚动条（文件视图或导航窗格）。
    pub scroll_drag: Option<ui::ScrollDrag>,
    /// 键盘快速定位累积的前缀（小写）。
    pub type_buffer: String,
    /// 上一次字母键的时刻，超过 1 秒就重新开始累积。
    pub type_time: Option<std::time::Instant>,
    /// 目录变化时正在编辑态，刷新被推迟：编辑收尾时补一次刷新。
    pub pending_fs_refresh: bool,

    /// 设置面板是否打开。打开时它是模态的：先吃消息再往下传。
    pub settings_open: bool,

    /// 自绘模态对话框（提示 / 确认 / 输入）。
    ///
    /// 打开时它比设置面板更靠上层：命中测试、键盘、字符输入都先路由到这里。
    pub dialog: Option<crate::ui::dialog::Dialog>,

    pub fonts: FontCache,
    pub icons: IconCache,
    /// 图片缩略图（大/中图标视图用），按路径缓存解码好的句柄。
    pub thumbs: crate::fs::thumbs::ThumbCache,
    pub back_buffer: BackBuffer,

    pub hover: ui::Hit,
    pub pressed: ui::Hit,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub modifiers: Modifiers,
    /// 鼠标离开跟踪是否已安装。
    tracking_mouse: bool,

    /// IME（输入法）组合窗的锚点：当前编辑框里插入符的客户区坐标。
    ///
    /// 绘制插入符的那几处顺手更新；IME 激活时（见 `place_ime_window`）
    /// 把它换算成屏幕坐标告诉输入法，候选词才会跟在光标旁边而不是
    /// 飘在屏幕左上角。
    pub ime_anchor: Option<(i32, i32)>,

    /// 状态栏瞬态提示（如「已复制路径」），到点自动清（见 `TIMER_NOTE`）。
    ///
    /// 高频操作的结果不该用模态框打断——点一下复制路径还得再去点
    /// 一下「我知道了」，两次点击换一次复制。
    pub status_note: Option<String>,

    // 地址栏编辑态
    pub address_editing: bool,
    /// 地址栏文本里的选区 `(anchor, caret)`，两个都是字符下标。
    ///
    /// `None` 表示没有选中——这样「有没有选区」和「选区是不是空的」不用分开判断。
    pub address_sel: Option<(usize, usize)>,
    pub address_text: String,
    pub address_caret: usize,
    /// 非编辑态地址栏里每一段路径的可点区间：`(left, right, 前缀)`。
    ///
    /// 由绘制层在量完字宽之后回填——每段的边界必须和真正画出来的像素一致，
    /// 命中测试自己再估一遍就会「画在这里、点在那里」。点某段即跳去该层，
    /// 点空白才进编辑态。
    pub address_segments: Vec<(i32, i32, String)>,
    /// 地址栏历史下拉的候选（按当前输入前缀匹配，见 `cfg::history`）。
    pub addr_suggest: Vec<String>,
    /// 下拉里键盘高亮的那一条。
    pub addr_suggest_sel: Option<usize>,

    // 搜索
    /// 搜索框是否获得键盘焦点（焦点是全局的 UI 状态，不随标签保存）。
    pub search_focused: bool,

    /// 合并待处理的导航目标。
    ///
    /// 鼠标侧键连点时会连着来好几条消息，每条都同步重载目录的话，
    /// 中间的目录都要白枚举一遍，900ms 的卡顿感就是这么来的。
    /// 这里只记「最终要去哪」，真正的重载在消息循环里做一次。
    pending_nav: Option<PathBuf>,

    // 内联重命名（对应 RenameAlert，但做成表格内直接编辑）
    rename_target: Option<usize>,
    inline_edit_text: String,
    inline_edit_caret: usize,

    /// 剪贴板内容来自「剪切」而非「复制」。
    cut_pending: bool,

    pub pipe: Option<PipeServer>,

    /// DPI 缩放。
    pub dpi_scale: f32,
    /// 收到 WM_CLOSE 后置位，用于「多个标签时提示」逻辑。
    closing: bool,
    /// 启动参数（用于决定初始标签）。
    cl: CommandLine,
}

impl Application {
    /// 构造应用状态。**不创建窗口**——窗口在 `run` 中建立。
    ///
    /// 冷启动顺序被刻意排成：解析命令行 → 读配置 → 建窗口 → 首次枚举目录。
    /// 配置读取失败也绝不阻塞启动。
    pub fn new(cl: CommandLine) -> Application {
        let mut cfg = Config::load();
        // 界面缩放的默认值固定按 150% 折算（不再随屏幕分辨率变化）：
        // 旧配置或首次运行都统一到这个值，滑块正好停在正中。
        if cfg.settings.scale_version < settings::UI_SCALE_VERSION {
            cfg.settings.ui_scale = settings::DEFAULT_UI_SCALE;
            cfg.settings.scale_version = settings::UI_SCALE_VERSION;
            cfg.save_all();
        }
        let theme = cfg.settings.theme;
        let scale = 1.0f32;
        // 系统菜单的外壳默认跟随 Windows 本身的深浅设置，会和应用内选的主题打架，
        // 这里先按应用主题强制一次
        crate::win::init::set_menu_dark(theme == ThemeKind::Dark);

        Application {
            hwnd: std::ptr::null_mut(),
            tabs: Vec::new(),
            active_tab: 0,
            active_tab_focused: true,
            cfg,
            theme,
            layout: Layout::default(),
            nav_expanded: NavExpanded::default(),
            nav_rows: Vec::new(),
            marquee: None,
            drag_origin: None,
            resizing_nav: false,
            resizing_detail: false,
            dragging_scale: false,
            scale_drag_span: None,
            drag_col: None,
            fav_press: None,
            fav_drag: None,
            fav_drop: None,
            nav_press: None,
            nav_drag: None,
            nav_drop_y: None,
            drop_tag_row: None,
            tooltip_ready: false,
            preview: None,
            io: crate::io::Io::new(),
            watcher: None,
            nav_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            preview_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            search_seq: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            preview_pending: None,
            nav_groups: Vec::new(),
            nav_sep_drag: None,
            detail_sep_drag: None,
            nav_scroll: 0,
            nav_max_scroll: 0,
            nav_content_h: 0,
            scroll_drag: None,
            type_buffer: String::new(),
            pending_fs_refresh: false,
            type_time: None,
            settings_open: false,
            dialog: None,
            fonts: FontCache::new(scale),
            icons: IconCache::new(),
            thumbs: crate::fs::thumbs::ThumbCache::default(),
            back_buffer: BackBuffer::new(),
            hover: ui::Hit::None,
            pressed: ui::Hit::None,
            mouse_x: -1,
            mouse_y: -1,
            modifiers: Modifiers::default(),
            tracking_mouse: false,
            ime_anchor: None,
            status_note: None,
            address_editing: false,
            address_sel: None,
            address_text: String::new(),
            address_caret: 0,
            address_segments: Vec::new(),
            addr_suggest: Vec::new(),
            addr_suggest_sel: None,
            search_focused: false,
            pending_nav: None,
            rename_target: None,
            inline_edit_text: String::new(),
            inline_edit_caret: 0,
            cut_pending: false,
            pipe: None,
            dpi_scale: scale,
            closing: false,
            cl,
        }
    }

    /// 建立窗口并进入消息循环。
    pub fn run(&mut self) {
        // ---- 特殊开关：接管/恢复后立即退出，不创建窗口 ----
        if self.cl.replace_explorer_all {
            let _ = crate::shell_integration::take_over();
            return;
        }
        if self.cl.restore_explorer_all {
            let _ = crate::shell_integration::restore();
            return;
        }

        let hinst: HINSTANCE = module_handle();

        unsafe {
            // 背景刷为 null：所有绘制都在 WM_PAINT 中完成，避免系统先擦一遍造成闪烁
            // 类光标必须是箭头。传 null 时系统不会给客户区设任何光标，
            // 于是从边框（那里是左右/上下缩放光标）滑进客户区后箭头回不来，
            // 整个窗口就一直显示缩放图标。
            let arrow = LoadCursorW(std::ptr::null_mut(), IDC_ARROW);
            let atom =
                winwnd::register_class(CLASS_NAME, wnd_proc, std::ptr::null_mut(), arrow);
            if atom == 0 {
                // 类已注册（同进程二次创建）也继续
            }

            let self_ptr: *mut Application = self;

            let (w, h) = self.initial_window_size();
            let (mut x, mut y) = (self.cfg.settings.window_x, self.cfg.settings.window_y);
            let centered = x == i32::MIN || y == i32::MIN;

            let style = WS_POPUP | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_CLIPCHILDREN;
            let ex_style = WS_EX_APPWINDOW;

            let title = to_wide(S::APP_NAME);

            let hwnd = CreateWindowExW(
                ex_style,
                to_wide(CLASS_NAME).as_ptr(),
                title.as_ptr(),
                style,
                if centered { 200 } else { x },
                if centered { 120 } else { y },
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinst,
                self_ptr as *mut core::ffi::c_void,
            );

            if !hwnd_valid(hwnd) {
                return;
            }
            self.hwnd = hwnd;
            // 外圈小圆角，内部保持方正
            winwnd::apply_round_corners(hwnd);
            // 登记 OLE 放置目标：否则拖东西过来整块窗口都是禁止光标
            droptarget::register(hwnd);

            if centered {
                let (cx, cy) = winwnd::center_on_monitor(hwnd, w, h);
                x = cx;
                y = cy;
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    x,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }

            crate::fs::icons::apply_window_icons(hwnd);
            DragAcceptFiles(hwnd, 1);

            // 「新建」的类型表要扫一遍 HKCR（上万个键）。放在第一次右键里
            // 做会让那一下明显发顿，这里提前在后台扫好，结果进 OnceLock。
            // 失败无所谓：右键时会再扫一次。
            std::thread::Builder::new()
                .name("dreamfolder-shellnew".to_string())
                .spawn(|| {
                    let _ = crate::win::shellnew::warm_up();
                })
                .ok();

            // 后台 IO 在这里绑定句柄：此后后台干完活就能 PostMessage 唤醒我们。
            // 必须早于 open_initial_tabs——初始标签的目录枚举就走后台。
            self.io.bind(hwnd);

            // 管道服务端（失败不影响启动）
            self.pipe = PipeServer::create();
            if self.pipe.is_some() {
                SetTimer(hwnd, TIMER_PIPE, PIPE_POLL_MS, None);
            }

            let show = if self.cfg.settings.window_maximized {
                SW_SHOWMAXIMIZED
            } else {
                SW_SHOWNORMAL
            };

            // ---- 打开初始标签 ----
            // 先建好标签再画首帧：窗口露面的那一瞬间就该是「完成品」，
            // 而不是等着 UpdateWindow 再往里填内容。
            self.open_initial_tabs();

            // 显示之前先把第一帧画到窗口表面。
            //
            // 类背景刷为 null、WM_ERASEBKGND 又恒返回 1，系统从不擦背景；
            // 不做这一步的话，ShowWindow 让 DWM 开始合成的那一刻客户区里
            // 什么都还没有，会闪出一块未初始化的白，随后才被首帧覆盖。
            // 不可见窗口收不到 WM_PAINT，但窗口 DC 随时能画，所以直接画。
            self.paint_first_frame(hwnd);
            // 首帧已经落在表面上了，清掉 invalid 区，
            // 免得 UpdateWindow 再把同一帧全画一遍
            ValidateRect(hwnd, std::ptr::null());

            ShowWindow(hwnd, show);
            UpdateWindow(hwnd);

            // ---- 消息循环 ----
            let code = winwnd::message_loop();
            crate::win::init::shutdown_process();
            let _ = code;
        }
    }

    /// 把第一帧直接画到窗口表面（显示之前调用，防启动白闪）。
    ///
    /// 与 WM_PAINT 走同一套 `draw::paint` + 双缓冲，只是取 DC 用 `GetDC`
    /// 而不是 `BeginPaint`——后者只对系统标脏的区域有意义，而这里要的是
    /// 「整窗先有一份完整内容」。
    fn paint_first_frame(&mut self, hwnd: HWND) {
        unsafe {
            let hdc = GetDC(hwnd);
            if hdc.is_null() {
                return;
            }
            let client = winwnd::client_rect(hwnd);
            let (w, h) = (client.right - client.left, client.bottom - client.top);
            if w > 0 && h > 0 {
                let (mem, _) = self.back_buffer.acquire(hdc, w, h);
                if !mem.is_null() {
                    crate::ui::draw::paint(self, mem, client);
                    self.back_buffer.blit_to(hdc, 0, 0, w, h);
                }
            }
            ReleaseDC(hwnd, hdc);
        }
    }
    /// 起始窗口尺寸。
    ///
    /// 用户没手动调整过窗口时（`window_geometry_set` 为 false，或几何仍是
    /// 未确定的哨兵值），按显示器工作区算一个合适的默认大小；
    /// 否则沿用保存的尺寸。
    fn initial_window_size(&self) -> (i32, i32) {
        let s = &self.cfg.settings;
        if s.window_geometry_set && s.window_w > 0 && s.window_h > 0 {
            return (s.window_w, s.window_h);
        }
        default_window_size()
    }

    /// 依据启动设置打开初始标签。
    fn open_initial_tabs(&mut self) {
        // 0) /curdir= 启动目录（原软件语义：作为首个标签的落点）
        // 只在没有显式路径参数时生效
        if self.cl.path.is_none() {
            if let Some(d) = self.cl.cur_dir.clone() {
                if d.is_dir() {
                    self.new_tab(Some(d));
                    // 起始目录也算用户意图，直接返回，不再叠加启动模式
                    return;
                }
            }
        }

        // 1) 命令行给出的路径优先
        if let Some(p) = self.cl.path.clone() {
            if p.is_dir() {
                self.new_tab(Some(p));
                return;
            }
            if let Some(parent) = p.parent() {
                let parent = parent.to_path_buf();
                self.new_tab(Some(parent));
                // 枚举在后台进行，此刻列表还是空的。记下名字，
                // 枚举落地时由 on_io 选中并滚动到它。
                let name = p.file_name().map(|s| s.to_string_lossy().into_owned());
                if let Some(name) = name {
                    if let Some(t) = self.active_mut() {
                        t.pending_select = Some(name);
                    }
                }
                return;
            }
        }

        // 2) `/searchkey` 直接进入搜索（原软件行为）
        if let Some(k) = self.cl.search_key.clone() {
            self.new_tab(Some(user_home()));
            self.cur_mut().search_text = k;
            // 启动参数本来就等于用户已经按过一次回车，直接搜
            self.begin_search();
            return;
        }

        // 3) 按启动设置
        match self.cfg.settings.start_mode {
            StartMode::RestoreLast => {
                let saved = self.cfg.session.tabs.clone();
                let mut activated = false;
                for rec in &saved {
                    // 三类标签都能恢复：普通目录按路径，「此电脑」与回收站
                    // 是伪路径，`is_dir()` 恒为 false——以前这两类标签
                    // 退出再启动就凭空消失。
                    let ok = match rec.kind {
                        crate::cfg::session::TabKind::Dir => rec.path.is_dir(),
                        crate::cfg::session::TabKind::Computer => {
                            rec.path.as_os_str().is_empty()
                        }
                        crate::cfg::session::TabKind::Recycle => {
                            rec.path.to_string_lossy() == RECYCLE_PATH
                        }
                    };
                    if ok {
                        let before = self.tabs.len();
                        if rec.kind == crate::cfg::session::TabKind::Recycle {
                            self.new_tab(Some(user_home()));
                            self.open_recycle_bin();
                        } else {
                            self.new_tab(Some(rec.path.clone()));
                        }
                        if self.tabs.len() > before || rec.kind == crate::cfg::session::TabKind::Recycle {
                            // 回收站走 open_recycle_bin 改造的是当前标签
                            let i = if rec.kind == crate::cfg::session::TabKind::Recycle {
                                self.active_tab
                            } else {
                                before
                            };
                            let t = &mut self.tabs[i];
                            t.view_mode = rec.view;
                            if rec.active {
                                self.active_tab = i;
                                activated = true;
                            }
                        }
                    }
                }
                if self.tabs.is_empty() || !activated {
                    if self.tabs.is_empty() {
                        self.new_tab(Some(computer_view()));
                    }
                }
            }
            StartMode::Custom => {
                let folders = self.cfg.settings.custom_start_folders.clone();
                for f in folders {
                    if f.is_dir() {
                        self.new_tab(Some(f));
                    }
                }
                if self.tabs.is_empty() {
                    self.new_tab(Some(computer_view()));
                }
            }
            StartMode::NewTab => {
                self.new_tab(Some(computer_view()));
            }
        }
    }

    // ------------------------------------------------------------ 主题与字体

    pub fn palette_for_theme(&self) -> &'static Palette {
        match self.theme {
            ThemeKind::Light => &Palette::LIGHT,
            ThemeKind::Dark => &Palette::DARK,
        }
    }

    /// 把逻辑字号换算成实际像素字号。
    ///
    /// 字号的基准值（`draw::font::*`）是 96 DPI、100% 缩放下的逻辑值，
    /// 必须和矩形一样乘上 `layout.scale`——之前这里直接拿逻辑值建字体，
    /// 于是窗口、图标、间距都放大了，文字却纹丝不动，正是「部分元素没有
    /// 严格按比例放大」的根源。
    fn font_px(&self, logical: i32) -> i32 {
        let px = (logical as f32 * self.layout.scale).round() as i32;
        px.max(8)
    }

    pub fn font_tab(&mut self) -> HFONT {
        let px = self.font_px(draw::font::TAB);
        self.fonts.get(px, false)
    }
    pub fn font_ui(&mut self) -> HFONT {
        let px = self.font_px(draw::font::UI);
        self.fonts.get(px, false)
    }
    pub fn font_small(&mut self) -> HFONT {
        let px = self.font_px(draw::font::SMALL);
        self.fonts.get(px, false)
    }
    pub fn font_status(&mut self) -> HFONT {
        let px = self.font_px(draw::font::STATUS);
        self.fonts.get(px, false)
    }

    pub fn icon_folder(&mut self, size: IconSize) -> *mut core::ffi::c_void {
        self.icons.get(&crate::fs::entry::IconKey::Folder, size)
    }

    pub fn icon_for_ext(&mut self, ext: &str, size: IconSize) -> *mut core::ffi::c_void {
        let key = if ext.is_empty() {
            crate::fs::entry::IconKey::NoExt
        } else {
            crate::fs::entry::IconKey::Ext(ext.to_ascii_lowercase())
        };
        self.icons.get(&key, size)
    }

    // ------------------------------------------------------------ 导航

    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active_tab)
    }

    fn active_mut(&mut self) -> Option<&mut Tab> {
        let i = self.active_tab;
        self.tabs.get_mut(i)
    }

    /// 当前标签（`tabs` 永不为空：关掉最后一个标签走的是关窗）。
    fn cur(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }

    fn cur_mut(&mut self) -> &mut Tab {
        let i = self.active_tab;
        &mut self.tabs[i]
    }


    pub fn can_go_back(&self) -> bool {
        self.active()
            // 搜索视图也算一格「来路」：后退的第一下用来退出搜索
            .map(|t| t.search_backup.is_some() || t.history_pos > 0)
            .unwrap_or(false)
    }

    pub fn can_go_forward(&self) -> bool {
        self.active()
            .map(|t| t.history_pos + 1 < t.history.len())
            .unwrap_or(false)
    }

    pub fn can_go_up(&self) -> bool {
        self.active()
            .map(|t| {
                // 搜索视图下「上一级」先退出搜索，任何目录（含「此电脑」、
                // 盘符根）都得是能点的，否则这个出口就永远点不到
                t.search_backup.is_some()
                    || (!t.is_computer && fs::parent_of(&t.path).is_some())
            })
            .unwrap_or(false)
    }

    pub fn go_back(&mut self) {
        // 搜索结果上按后退＝退回搜索前的视图。搜索不算一次路径导航
        // （history 里没有它），先把它消化掉，否则会越过当前目录
        // 直接跳到上一个历史位置。
        if self.leave_search_view() {
            return;
        }
        let target = {
            let Some(t) = self.active_mut() else { return };
            if t.history_pos == 0 {
                return;
            }
            t.history_pos -= 1;
            t.history[t.history_pos].clone()
        };
        self.request_nav(target);
    }

    pub fn go_forward(&mut self) {
        let target = {
            let Some(t) = self.active_mut() else { return };
            if t.history_pos + 1 >= t.history.len() {
                return;
            }
            t.history_pos += 1;
            t.history[t.history_pos].clone()
        };
        self.request_nav(target);
    }

    /// 安排一次导航：只覆盖「最终目标」，重载推迟到消息循环里做。
    ///
    /// 连点时历史位置已经逐次走完（那是纯内存操作，代价可忽略），
    /// 但目录只枚举最后一个——中间那些目录用户根本没停下来看。
    fn request_nav(&mut self, target: PathBuf) {
        self.pending_nav = Some(target);
        if !self.hwnd.is_null() {
            unsafe {
                PostMessageW(self.hwnd, NAV_MSG, 0, 0);
            }
        }
    }

    pub fn go_up(&mut self) {
        // 搜索结果上按「上一级」先退出搜索、留在搜索起点那个目录。
        //
        // 搜索不改标签路径，所以直接按路径上跳时，`D:\A\B` 的搜索结果会
        // 一步落到 `D:\A`——用户搜索时所在的 `D:\A\B` 被跳过去了，
        // 看起来像「一下上了两级」。退出搜索才是这一步该做的事。
        if self.leave_search_view() {
            return;
        }
        let target = {
            let Some(t) = self.active() else { return };
            if t.is_computer {
                return;
            }
            // 回收站的上一级同样是「此电脑」
            if t.is_recycle {
                PathBuf::new()
            } else
            // 驱动器根的上一级是「此电脑」
            if fs::is_drive_root(&t.path) {
                PathBuf::new()
            } else {
                fs::parent_of(&t.path).unwrap_or_default()
            }
        };
        self.load_into_active(target, true);
    }

    /// 导航到指定路径（记录历史）。
    /// 把当前标签切成回收站视图。
    pub fn open_recycle_bin(&mut self) {
        let idx = self.active_tab;
        if idx >= self.tabs.len() {
            return;
        }
        {
            let t = &mut self.tabs[idx];
            t.path = PathBuf::from(RECYCLE_PATH);
            t.is_computer = false;
            t.is_recycle = true;
            t.title = "回收站".to_string();
            // 回收站默认按删除时间倒序：刚删掉的东西应该在最上面。
            // 资源管理器的回收站也是这个默认；用户点表头改排序后仍按他的选。
            t.sort = crate::fs::enumerate::Sort {
                key: crate::fs::enumerate::SortKey::Modified,
                descending: true,
            };
            t.selected.clear();
            t.anchor = None;
            t.focus = None;
            t.scroll_y = 0;
            t.error = None;
            t.history.truncate(t.history_pos + 1);
            t.history.push(t.path.clone());
            t.history_pos = t.history.len().saturating_sub(1);
        }
        self.reload_active_tab();
        self.request_repaint();
    }

    /// 是否正处在回收站视图。
    pub fn in_recycle_bin(&self) -> bool {
        self.active().map(|t| t.is_recycle).unwrap_or(false)
    }

    pub fn navigate_to(&mut self, path: PathBuf) {
        self.load_into_active(path, true);
    }

    /// 点击导航窗格某一行：切到该位置。
    ///
    /// 复用 `navigate_to`，所以历史、标题、状态栏的行为与地址栏输入一致。
    /// 「此电脑」用空路径表示——那是项目里既有的电脑视图约定。
    pub fn nav_activate(&mut self, i: usize) {
        let Some(row) = self.nav_rows.get(i) else {
            return;
        };
        let target = match &row.kind {
            // 分隔线不是可点项（它是拖动把手）；命中测试已经挡掉，这里再保险一次
            crate::ui::navpane::NavKind::Separator { .. } => return,
            crate::ui::navpane::NavKind::Computer => PathBuf::new(),
            crate::ui::navpane::NavKind::Known { path, .. }
            | crate::ui::navpane::NavKind::Place { path, .. }
            | crate::ui::navpane::NavKind::Drive { path } => path.clone(),
            // 标签里收的可能是文件：目录进标签，文件交给默认关联
            crate::ui::navpane::NavKind::TagItem { path, .. } => {
                let p = path.clone();
                self.open_path(p, false);
                return;
            }
            // 标签标题行点击＝展开/收起，不导航
            crate::ui::navpane::NavKind::Tag { .. } => {
                self.nav_toggle(i);
                return;
            }
            // 空分组（快捷访问 / 标签）的提示行不是可点项
            crate::ui::navpane::NavKind::Hint { .. } => return,
            // 回收站是命名空间虚拟目录：不进 `navigate_to`，单独走一条路，
            // 把当前标签切成回收站视图（以前是开一个系统窗口，点了像没反应）。
            crate::ui::navpane::NavKind::RecycleBin => {
                self.open_recycle_bin();
                return;
            }
            // 网络位置的枚举要 WNetEnumResourceW，尚无实现，先不导航
            crate::ui::navpane::NavKind::Network => return,
        };
        self.navigate_to(target);
    }

    /// 点导航行的展开箭头。
    ///
    /// 「此电脑」「网络」和每个标签都可折叠；其余行 `expandable` 为 false，
    /// 命中测试不会给出 `Hit::NavArrow`，这里再挡一次防御。
    pub fn nav_toggle(&mut self, i: usize) {
        let Some(row) = self.nav_rows.get(i) else {
            return;
        };
        let mut save_tags = false;
        match &row.kind {
            crate::ui::navpane::NavKind::Computer => {
                self.nav_expanded.computer = !self.nav_expanded.computer;
            }
            crate::ui::navpane::NavKind::Network => {
                self.nav_expanded.network = !self.nav_expanded.network;
            }
            crate::ui::navpane::NavKind::Tag { index, .. } => {
                let index = *index;
                self.cfg.tags.toggle_expanded(index);
                save_tags = true;
            }
            _ => return,
        }
        if save_tags {
            // 展开状态也存盘：下次开窗还是这个样子
            self.cfg.tags.save(&self.cfg.root);
        }
        // 展开状态变了，行数和矩形都会变
        self.rebuild_nav_rows();
    }

    /// 载入目录。`record` 为 true 时写入历史。
    fn load_into_active(&mut self, path: PathBuf, record: bool) {
        let idx = self.active_tab;
        if idx >= self.tabs.len() {
            return;
        }
        // 更新历史
        if record {
            let t = &mut self.tabs[idx];
            t.history.truncate(t.history_pos + 1);
            if t.history.last().map(|p| p != &path).unwrap_or(true) {
                t.history.push(path.clone());
            }
            t.history_pos = t.history.len().saturating_sub(1);
        }

        let is_computer = path.as_os_str().is_empty();
        let title = if is_computer {
            S::STATUS_DRIVES.to_string()
        } else {
            crate::cfg::bookmarks::display_name(&path)
        };

        {
            let t = &mut self.tabs[idx];
            t.path = path.clone();
            t.is_computer = is_computer;
            t.is_recycle = false;
            t.title = title;
            t.selected.clear();
            t.anchor = None;
            t.scroll_y = 0;
            t.error = None;
            t.loading = true;
            // 换了目录就不是原来那个搜索意图了，全部作废（同目录刷新
            // 不清这些，见 reload_active_tab）
            t.search_backup = None;
            t.search_state = None;
            t.pending_search = false;
            t.pending_rename = None;
            t.search_text.clear();
            t.search_caret = 0;
        }

        // 记录地址历史（原软件的 addr_history.ini）
        if !is_computer {
            self.cfg.history.record(&path.to_string_lossy());
        }

        // 这个目录以前用过什么视图/排序，就接着用（资源管理器的按文件夹记忆）
        self.apply_folder_view(idx);
        self.reload_active_tab();
        self.sync_window_title();
    }

    /// 按目录记忆设置某个标签的视图与排序；没有记忆就保持调用方给的值。
    fn apply_folder_view(&mut self, tab: usize) {
        let Some(path) = self.tabs.get(tab).map(|t| t.path.clone()) else {
            return;
        };
        if path.as_os_str().is_empty() || self.tabs[tab].is_recycle {
            return;
        }
        let Some(v) = crate::cfg::settings::lookup_folder_view(&self.cfg.settings.folder_views, &path)
            .cloned()
        else {
            return;
        };
        let t = &mut self.tabs[tab];
        t.view_mode = v.view;
        t.sort = Sort {
            key: v.sort_key,
            descending: v.sort_desc,
        };
    }

    /// 把活动标签当前的视图与排序记到它所在目录名下。
    pub(crate) fn remember_folder_view(&mut self) {
        let Some(t) = self.active() else { return };
        if t.is_computer || t.is_recycle || t.path.as_os_str().is_empty() {
            return;
        }
        let (path, view, sort) = (t.path.clone(), t.view_mode, t.sort);
        crate::cfg::settings::remember_folder_view(
            &mut self.cfg.settings.folder_views,
            &path,
            view,
            sort,
        );
    }

    /// 把活动标签的标题同步到窗口标题。
    ///
    /// 标题应当是「你现在在哪个文件夹」：写死一个产品名的话，同时开多个
    /// 窗口时任务栏里所有窗口看起来都一样。
    fn sync_window_title(&mut self) {
        let raw = self
            .active()
            .map(|t| t.title.clone())
            .unwrap_or_else(|| S::STATUS_DRIVES.to_string());
        let title = taskbar_title(&raw);
        if !self.hwnd.is_null() {
            let wide = to_wide(&title);
            unsafe {
                SetWindowTextW(self.hwnd, wide.as_ptr());
            }
        }
    }

    /// 重新枚举当前标签的目录。
    pub fn refresh_active_tab(&mut self) {
        self.reload_active_tab();
        self.request_repaint();
    }

    fn reload_active_tab(&mut self) {
        let idx = self.active_tab;
        if idx >= self.tabs.len() {
            return;
        }

        let opts = ListOptions {
            show_hidden: self.cfg.settings.show_hidden,
            show_system: self.cfg.settings.show_hidden,
            sort: self.tabs[idx].sort,
        };

        // 同目录刷新时按路径记住选中与焦点，落地后还原（资源管理器 F5 之后
        // 选中项不会丢）。换目录的路径会先把 `selected` 清空，
        // 这里记到的就是空集，天然不会把上一个目录的选择带过来。
        if self.tabs[idx].pending_keep.is_none() {
            let t = &self.tabs[idx];
            let paths: Vec<PathBuf> = t
                .selected
                .iter()
                .filter_map(|&i| t.entries.get(i).map(|e| e.full.clone()))
                .collect();
            let focus = t
                .focus
                .and_then(|i| t.entries.get(i))
                .map(|e| e.full.clone());
            self.tabs[idx].pending_keep = Some((paths, focus));
        }

        let is_computer = self.tabs[idx].is_computer;
        let is_recycle = self.tabs[idx].is_recycle;
        let path = self.tabs[idx].path.clone();
        let kind = if is_recycle {
            ListKind::Recycle
        } else if is_computer {
            ListKind::Computer
        } else {
            ListKind::Dir
        };

        // 枚举一律丢给后台：大目录、慢盘、离线网络位置都可能让 list_dir
        // 卡上好几秒，放在消息循环里就是整个窗口假死。这里只负责把标签
        // 置成「加载中」并发出请求，结果回来时再填（见 `on_io`）。
        {
            let t = &mut self.tabs[idx];
            t.entries.clear();
            t.selected.clear();
            t.anchor = None;
            t.scroll_y = 0;
            t.error = None;
            t.loading = true;
            // 同目录刷新（F5、Shell 菜单收起、目录变化自动刷新）不动搜索：
            // 用户敲到一半的词被清掉，比列表旧几秒更让人恼火。
            // 换目录（load_into_active）才把上一次的搜索状态全部作废。
            t.search_backup = None;
            t.search_state = None;
            t.pending_search = false;
        }

        let id = self.nav_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.tabs[idx].list_id = id;
        self.io.send(Job::List {
            id,
            tab: idx,
            path: path.clone(),
            opts,
            kind,
        });

        // 「此电脑」的卷标与容量单独再发一个请求：那两个系统调用在离线
        // 网络盘上会卡几十秒，必须和「先把盘符列出来」分开做。
        if kind == ListKind::Computer {
            self.io.send(Job::Volumes {
                id,
                tab: idx,
                drives: crate::fs::drives(),
            });
        }

        // 标题跟着活动标签走。放在这里而不是只在 load_into_active 里：
        // 新建标签、刷新、切标签都走本函数，漏一处标题就会停在软件名上。
        self.sync_window_title();
        self.resync_watcher();
        self.request_repaint();
    }

    /// 按活动标签重挂目录监听。
    ///
    /// 每次重挂都会先丢掉旧的：`DirWatcher::drop` 会通知监听线程退出并等它
    /// 收尾，所以这里不会攒下一堆僵尸线程。
    fn resync_watcher(&mut self) {
        // 顺手收掉上次交出去、现在已经退出的监听线程
        crate::fs::watch::reap_retired();
        if self.hwnd.is_null() {
            self.watcher = None;
            return;
        }
        let Some(t) = self.active() else {
            self.watcher = None;
            return;
        };
        let target = if t.is_computer || t.is_recycle || t.path.as_os_str().is_empty() {
            None
        } else {
            Some(t.path.clone())
        };
        // 已经是同一个目录就留着：自动刷新会反复走这里，重挂一次要 join
        // 监听线程，等于每次刷新白等一个等待周期。
        if self.watcher.as_ref().map(|w| &w.path) == target.as_ref() {
            return;
        }
        self.watcher = None;
        if let Some(dir) = target {
            self.watcher =
                crate::fs::watch::DirWatcher::spawn(&dir, self.hwnd as isize, WM_APP_FS);
        }
    }

    /// 目录变化防抖定时器到点：刷新当前列表。
    ///
    /// 编辑态（重命名 / 地址栏 / 搜索输入 / 对话框）下不刷新：列表一换，
    /// 正在编辑的下标就失效，回车会改到别的文件上。宁可晚一点刷。
    pub fn apply_fs_change(&mut self) {
        if self.rename_active()
            || self.address_editing
            || self.search_focused
            || self.dialog.is_some()
            || self.settings_open
        {
            // 编辑态不能立刻刷（列表一换，正在编辑的下标就失效）。
            // 但 TIMER_FS 是一次性的，直接丢掉这次变化就永远不刷了——
            // 记一个待刷新标志，编辑收尾处（cancel_edit/commit_edit/
            // 对话框与设置面板关闭）补上。
            self.pending_fs_refresh = true;
            return;
        }
        let Some(t) = self.active() else { return };
        // 正在加载、盘符视图都不刷；搜索结果也不能刷——刷新会把结果
        // 换成目录内容，用户敲到一半的筛选就没了
        if t.loading || t.is_computer || t.search_state.is_some() {
            return;
        }
        self.refresh_active_tab();
    }

    /// 编辑收尾时补上被推迟的目录刷新（见 `apply_fs_change`）。
    pub(crate) fn flush_pending_fs_refresh(&mut self) {
        if !self.pending_fs_refresh {
            return;
        }
        self.pending_fs_refresh = false;
        if !self.rename_active()
            && !self.address_editing
            && !self.search_focused
            && self.dialog.is_none()
            && !self.settings_open
        {
            self.refresh_active_tab();
        }
    }

    /// 应用一条后台结果。过期的（请求号不是最新的）直接丢。
    fn on_io(&mut self, done: crate::io::Done) {
        match done {
            crate::io::Done::Listed {
                id,
                tab,
                path,
                kind,
                entries,
                error,
            } => {
                let Some(t) = self.tabs.get_mut(tab) else {
                    return;
                };
                if id != t.list_id {
                    return;
                }
                // 标签可能已经换了目录（或用同一标签又发了一次请求）
                if t.path != path {
                    return;
                }
                t.entries = entries;
                t.error = error;
                t.loading = false;
                let _ = kind;
                // 卷标可能比列表先回来，这里补一次回填
                apply_drive_info(t);
                // 之前记下的「加载完成后要选中的名字」（新建、重命名）优先；
                // 否则还原刷新前的选中与焦点（按路径匹配，资源管理器 F5 之后
                // 选中项不会丢）。
                if let Some(name) = t.pending_select.take() {
                    if let Some(i) = t.entries.iter().position(|e| e.name == name) {
                        t.selected.clear();
                        t.selected.insert(i);
                        t.anchor = Some(i);
                        t.focus = Some(i);
                    }
                } else if let Some((paths, focus)) = t.pending_keep.take() {
                    if !paths.is_empty() || focus.is_some() {
                        let mut selected = std::collections::HashSet::new();
                        for (i, e) in t.entries.iter().enumerate() {
                            if paths.iter().any(|p| p == &e.full) {
                                selected.insert(i);
                            }
                        }
                        let focus_at = focus
                            .and_then(|p| t.entries.iter().position(|e| e.full == p));
                        t.selected = selected;
                        t.focus = focus_at;
                        t.anchor = focus_at;
                    }
                }
                // 「新建」后的补位：建文件那一步已经把名字递过来了，
                // 这里按名字找到它，选中并直接进入重命名——
                // 这就是资源管理器「新建完就能直接打字改名」的手感。
                let mut fresh_one = None;
                if let Some(name) = t.pending_rename.take() {
                    if let Some(i) = t.entries.iter().position(|e| e.name == name) {
                        t.selected.clear();
                        t.selected.insert(i);
                        t.anchor = Some(i);
                        t.focus = Some(i);
                        fresh_one = Some(i);
                    }
                }
                // 图标键/类型名在数据落地时补齐，绘制路径上只读
                self.fill_entry_meta(tab);
                if let Some(i) = fresh_one {
                    if tab == self.active_tab {
                        crate::ui::interact::scroll_into_view(self, i);
                        self.begin_rename_selection();
                    }
                }
                // 加载中按过回车（或 /searchkey 启动）的搜索，现在有可
                // 备份的原始列表了，补上那次没发出去的搜索
                if self.tabs[tab].pending_search && tab == self.active_tab {
                    self.tabs[tab].pending_search = false;
                    self.begin_search();
                }
            }
            crate::io::Done::Volumes { id, tab, infos } => {
                let Some(t) = self.tabs.get_mut(tab) else {
                    return;
                };
                if id != t.list_id {
                    return;
                }
                t.drive_info = infos;
                apply_drive_info(t);
            }
            crate::io::Done::Previewed { id, path, data } => {
                if id != self.preview_seq.load(Ordering::SeqCst) {
                    return;
                }
                if self.preview_pending.as_deref() != Some(path.as_path()) {
                    return;
                }
                self.preview_pending = None;
                // GDI+ 解码只能在这里做（UI 线程）
                self.preview = Some(crate::ui::preview::Preview::from_data(path, data));
            }
            crate::io::Done::Thumbed { path, icon } => {
                // 缓存超限被清出来的旧句柄在这里销毁
                let stale = self.thumbs.store(path, icon);
                crate::fs::thumbs::ThumbCache::destroy(&stale);
                self.request_repaint();
            }
            crate::io::Done::Searched { id, tab, key, hits } => {
                // 按发起搜索的那个标签自己的序号比对，两标签并发搜索互不干扰
                let Some(t) = self.tabs.get_mut(tab) else {
                    return;
                };
                if id != t.search_id {
                    return;
                }
                // 该标签的搜索词必须没变过（用户可能改了词或退出了搜索）
                if key != t.search_text.trim().to_lowercase() {
                    return;
                }
                let mut hits = hits;
                // 第三级兜底：「此电脑」「回收站」这类虚拟视图没有真实目录，
                // 索引和子树扫描都无从下手，只好在已加载条目里按名字过一遍。
                if hits.is_empty() {
                    if let Some(backup) = t.search_backup.as_ref() {
                        hits = backup
                            .iter()
                            .filter(|e| e.name.to_lowercase().contains(&key))
                            .cloned()
                            .collect();
                    }
                }
                let found = hits.len();
                t.entries = hits;
                // 搜索结果是扁平列表，按当前标签的排序方式归位。资源管理器的
                // 搜索结果同样跟随列表当前排序；不排的话用户看到的是目录遍历
                // 顺序，同一类文件散落在各处，看着像随机。
                let sort = t.sort;
                crate::fs::enumerate::sort_entries(&mut t.entries, &sort);
                t.selected.clear();
                t.anchor = None;
                t.scroll_y = 0;
                t.search_state = Some(SearchState::Done { hits: found });
                self.fill_entry_meta(tab);
                // 没有别的标签还在搜了，超时定时器就没必要再挂着
                if !self
                    .tabs
                    .iter()
                    .any(|t| matches!(t.search_state, Some(SearchState::Running)))
                {
                    self.kill_search_timeout();
                }
            }
            crate::io::Done::OpDone { result } => {
                if result == fsops::OpResult::Ok {
                    // 剪切粘贴（移动）成功后剪贴板语义才失效
                    self.cut_pending = false;
                    self.refresh_active_tab();
                } else if let fsops::OpResult::Failed(msg) = result {
                    self.show_message(&msg);
                    // 失败也要刷新：批量操作用户可能选了「跳过」，前面几项已经改了
                    self.refresh_active_tab();
                }
            }
        }
        self.request_repaint();
    }

    /// 取干所有已就绪的后台结果。由 `WM_APP_IO` 调用。
    pub fn drain_io(&mut self) {
        let mut guard = 0;
        while let Some(done) = self.io.try_recv() {
            self.on_io(done);
            // 一次涌入几百条时也不要长时间霸占消息循环
            guard += 1;
            if guard >= 64 {
                unsafe {
                    PostMessageW(self.hwnd, crate::io::WM_APP_IO, 0, 0);
                }
                break;
            }
        }
    }

    /// 条目落地后一次性补齐图标键与类型名。
    ///
    /// 这两样以前留在绘制路径上现算：每帧、每行都要重新切一次扩展名、
    /// 查一次类型名（还要分配一个 String），而 `Entry.icon_key` /
    /// `Entry.type_name` 这两个本就是缓存的字段从来没被写回过。
    /// 现在改在数据进入 UI 状态的那一刻收尾，一屏 30 行只算一次。
    fn fill_entry_meta(&mut self, tab: usize) {
        // 解构只为了拿到 `tabs` 的可变借用（索引表在 self 上，不会冲突）
        let Application { tabs, .. } = self;
        let Some(t) = tabs.get_mut(tab) else {
            return;
        };
        for e in &mut t.entries {
            if e.icon_key == crate::fs::entry::IconKey::Pending {
                e.icon_key = crate::fs::icons::key_for_entry(
                    &e.name,
                    &e.full,
                    e.is_dir(),
                    e.kind == EntryKind::Drive,
                );
            }
            if e.type_name.is_empty() {
                e.type_name = if e.is_dir() {
                    crate::fs::typename::folder().to_string()
                } else {
                    crate::fs::typename::for_ext(&e.ext())
                };
            }
        }
    }

    // ------------------------------------------------------------ 标签

    /// 新建标签。`path` 为 None 时使用当前标签的目录（对应「复制当前标签」）。
    pub fn new_tab(&mut self, path: Option<PathBuf>) {
        let path = path.unwrap_or_else(|| {
            self.active()
                .map(|t| t.path.clone())
                .unwrap_or_else(user_home)
        });

        let s = &self.cfg.settings;
        let tab = Tab::new(path, s.view_mode, Sort { key: s.sort_key, descending: s.sort_desc });
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        // 新标签打开的是一个具体的目录，用那个目录自己的记忆（如果有）
        let idx = self.active_tab;
        self.apply_folder_view(idx);
        self.reload_active_tab();
        self.request_repaint();
    }

    /// 标签栏「+」：开一个「此电脑」标签。
    ///
    /// 不再是「复制当前目录」——在同一个目录里连开两个标签没有任何用处，
    /// 而点加号的人通常是从零开始找盘。空路径在这个项目里就是「此电脑」，
    /// 标题、地址栏、导航高亮都按它走（同 `nav_activate` 的 `Computer` 分支）。
    ///
    /// Ctrl+T 保持「复制当前目录」：那是「在这里再开一个」的语义，
    /// 两者入口不同、意图不同，不必强行一致。
    pub fn new_computer_tab(&mut self) {
        self.new_tab(Some(PathBuf::new()));
    }

    /// 关闭标签。`force` 为 true 时忽略「最后一个标签」保护。
    pub fn close_tab(&mut self, index: usize, force: bool) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs.len() == 1 && !force {
            // 最后一个标签：关闭整个窗口
            self.request_close();
            return;
        }
        // 下标会整体前移，任何在途搜索的结果都可能落到别的标签上
        self.cancel_pending_search();
        self.tabs.remove(index);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        } else if self.active_tab > index {
            self.active_tab -= 1;
        }
        self.resync_watcher();
        self.request_repaint();
    }

    pub fn set_active_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            // 搜索状态是每标签一份的，切过去直接用它自己那份：
            // 谁在搜索、搜了什么、结果几条，都跟着标签走。
            self.active_tab = index;
            self.sync_window_title();
            // 监听要跟着活动标签走，否则后台标签的目录变动会刷新到前台
            self.resync_watcher();
            // 挂起的搜索若是在后台标签上加载完成的，切回来时补触发
            if self.tabs[index].pending_search && !self.tabs[index].loading {
                self.tabs[index].pending_search = false;
                self.begin_search();
            }
            self.request_repaint();
        }
    }

    /// 关掉标签前把在途的搜索作废。
    pub fn next_tab(&mut self, delta: i32) {
        if self.tabs.len() < 2 {
            return;
        }
        let n = self.tabs.len() as i32;
        let cur = self.active_tab as i32;
        let next = ((cur + delta) % n + n) % n;
        self.active_tab = next as usize;
        self.sync_window_title();
        self.resync_watcher();
        self.request_repaint();
    }

    // ------------------------------------------------------------ 选择与打开

    pub fn entry_path(&self, index: usize) -> Option<PathBuf> {
        self.active()
            .and_then(|t| t.entries.get(index))
            .map(|e| e.full.clone())
    }

    /// 当前焦点项：键盘移动的基准，没有时退回选择集中最小的一项。
    pub fn focus_index(&self) -> Option<usize> {
        let t = self.active()?;
        t.focus.or_else(|| t.selected.iter().min().copied())
    }

    pub fn select_entry(&mut self, index: usize, ctrl: bool, shift: bool) {
        {
            let Some(t) = self.active_mut() else { return };
            if index >= t.entries.len() {
                return;
            }

            if shift {
                // 以锚点为基准重建范围；焦点跟到目标项，锚点原地不动——
                // 这正是资源管理器里 Shift+点击 后再 Shift+点击别处的行为。
                let anchor = t.anchor.or(t.focus).unwrap_or(index);
                let (lo, hi) = if anchor <= index {
                    (anchor, index)
                } else {
                    (index, anchor)
                };
                t.selected.clear();
                for i in lo..=hi {
                    t.selected.insert(i);
                }
                t.anchor = Some(anchor);
                t.focus = Some(index);
            } else if ctrl {
                if t.selected.contains(&index) {
                    t.selected.remove(&index);
                } else {
                    t.selected.insert(index);
                }
                t.anchor = Some(index);
                t.focus = Some(index);
            } else {
                t.selected.clear();
                t.selected.insert(index);
                t.anchor = Some(index);
                t.focus = Some(index);
            }
        }
        self.request_repaint();
    }

    pub fn select_all(&mut self) {
        let Some(t) = self.active_mut() else { return };
        t.selected = (0..t.entries.len()).collect();
        // 全选后焦点留在原处（资源管理器也是），没有就落在第一项
        if t.focus.is_none() && !t.entries.is_empty() {
            t.focus = Some(0);
        }
        self.request_repaint();
    }

    pub fn select_first(&mut self) {
        self.select_entry(0, false, false);
        interact::scroll_into_view(self, 0);
    }

    pub fn select_last(&mut self) {
        let n = self.active().map(|t| t.entries.len()).unwrap_or(0);
        if n > 0 {
            self.select_entry(n - 1, false, false);
            interact::scroll_into_view(self, n - 1);
        }
    }

    pub fn clear_selection(&mut self) {
        if let Some(t) = self.active_mut() {
            t.selected.clear();
            t.anchor = None;
            t.focus = None;
        }
        self.request_repaint();
    }

    /// 方向键移动。`extend` = Shift（扩展选择），`focus_only` = Ctrl（只移焦点）。
    ///
    /// 资源管理器的三条规则：方向键单选移动、Shift+方向键从锚点扩展、
    /// Ctrl+方向键只挪焦点框而不改已选中的东西。以前只实现了第一条。
    pub fn move_selection_ex(&mut self, delta: i32, extend: bool, focus_only: bool) {
        let n = self.active().map(|t| t.entries.len()).unwrap_or(0);
        if n == 0 {
            return;
        }
        let cur = self.focus_index().unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, n as i32 - 1) as usize;

        if focus_only {
            if let Some(t) = self.active_mut() {
                t.focus = Some(next);
            }
            interact::scroll_into_view(self, next);
            self.request_repaint();
            return;
        }
        self.select_entry(next, false, extend);
        interact::scroll_into_view(self, next);
    }

    pub fn move_selection(&mut self, delta: i32) {
        self.move_selection_ex(delta, false, false);
    }

    /// 一屏能放多少项：按首项的宽高与视口算，行/列两种布局共用。
    fn page_items(&self) -> usize {
        let Some(t) = self.active() else { return 1 };
        let vr = self.layout.view;
        let vh = (vr.bottom - vr.top).max(1);
        let vw = (vr.right - vr.left).max(1);
        let r0 = self.layout.item_rect(t.view_mode, 0, 0);
        let item_h = (r0.bottom - r0.top).max(1);
        let item_w = (r0.right - r0.left).max(1);
        let rows = (vh / item_h).max(1) as usize;
        let cols = (vw / item_w).max(1) as usize;
        (rows * cols).max(1)
    }

    /// 翻页移动选择：`dir` 为 -1 上翻、1 下翻（PageUp / PageDown）。
    pub fn move_selection_page(&mut self, dir: i32) {
        let step = self.page_items() as i32;
        self.move_selection(dir * step);
    }

    /// 打开条目：目录则进入，文件则用默认程序打开。
    pub fn open_entry(&mut self, index: usize) {
        let Some(entry) = self
            .active()
            .and_then(|t| t.entries.get(index))
            .cloned()
        else {
            return;
        };

        match entry.kind {
            EntryKind::Directory | EntryKind::Drive => {
                self.navigate_to(entry.full);
            }
            _ => {
                if let Err(e) = shell::open_default(self.hwnd, &entry.full) {
                    self.show_message(&e);
                }
            }
        }
    }

    pub fn show_properties_entry(&mut self, index: usize) {
        if let Some(p) = self.entry_path(index) {
            let _ = shell::show_properties(self.hwnd, &p);
        }
    }

    /// 空格键快速预览（对应 `enable_quickpreview`）。
    ///
    /// 资源管理器的空格是「在预览窗格里看一眼当前项」，**从不调用外部程序**。
    /// 以前这里对非文本类型直接 `shell::open_default`——按一下空格，照片就在
    /// 照片查看器里弹出来，等于把「预览」变成了「打开」。现在统一落到详情
    /// 窗格顶部的预览框：图片走 GDI+，文本摊成若干行。
    pub fn quick_preview_selection(&mut self) {
        if !self.cfg.settings.enable_quickpreview {
            return;
        }
        let Some(&i) = self.active().and_then(|t| t.selected.iter().min()) else {
            return;
        };
        let Some(entry) = self.active().and_then(|t| t.entries.get(i)).cloned() else {
            return;
        };
        if entry.is_dir() {
            return;
        }
        // 详情窗格关着就无处显示：先把它打开，预览在它的顶部
        if !self.cfg.settings.show_detail_pane {
            self.cfg.settings.show_detail_pane = true;
            self.recompute_layout();
        }
        self.ensure_preview(Some(entry.full.clone()), &entry.ext(), entry.size);
        self.request_repaint();
    }

    // ------------------------------------------------------------ 视图滚动

    pub fn scroll_view(&mut self, delta: i32) {
        let (_, max_scroll) = self.view_scroll_metrics();
        let active = self.active_tab;
        if active >= self.tabs.len() {
            return;
        }
        let t = &mut self.tabs[active];
        t.scroll_y = (t.scroll_y + delta).clamp(0, max_scroll);
        self.request_repaint();
    }

    /// 文件视图的（内容总高度, 可滚动余量）。滚动条几何与滚轮都要用，
    /// 所以和 `scroll_view` 共用同一份算式，免得两边对不上。
    pub fn view_scroll_metrics(&self) -> (i32, i32) {
        let active = self.active_tab;
        if active >= self.tabs.len() {
            return (0, 0);
        }
        let l = &self.layout;
        let s = |v: i32| -> i32 { (v as f32 * l.scale).round() as i32 };
        let view_mode = self.tabs[active].view_mode;
        let count = self.tabs[active].entries.len();

        let content_h = if view_mode.is_rows() {
            let extra = if view_mode == ViewMode::Detail { s(24) } else { 0 };
            count as i32 * s(24) + extra
        } else {
            let (cw_cfg, ch_cfg) = view_mode.cell();
            let icon_px = s(view_mode.icon_size());
            let cell_w = if cw_cfg == 0 {
                s(112)
            } else {
                s(cw_cfg).max(icon_px + s(24))
            };
            let cell_h = s(ch_cfg).max(icon_px + s(34));
            let avail_w = (l.view.right - l.view.left).max(cell_w);
            let cols = (avail_w / cell_w).max(1) as usize;
            let rows = count.div_ceil(cols);
            rows as i32 * cell_h
        };

        let view_h = l.view.bottom - l.view.top;
        (content_h, (content_h - view_h).max(0))
    }

    /// 直接把文件视图滚到 `v`（拖动滚动条滑块时用）。
    pub fn set_view_scroll(&mut self, v: i32) {
        let (_, max_scroll) = self.view_scroll_metrics();
        let active = self.active_tab;
        if active >= self.tabs.len() {
            return;
        }
        let next = v.clamp(0, max_scroll);
        if self.tabs[active].scroll_y != next {
            self.tabs[active].scroll_y = next;
            self.request_repaint();
        }
    }


    // ------------------------------------------------------------ 收藏栏
    /// 键盘快速定位（资源管理器同款）。
    ///
    /// - 连续按不同字母 = 累积前缀，跳到以这串字母开头的项；
    /// - 同一个字母连按 = 在该字母的匹配项之间往下循环，
    ///   这一类走完了自然落到下一种类型里同字母开头的项；
    /// - 超过 1 秒没有新字母，前缀清空重新开始。
    pub fn type_ahead(&mut self, ch: char) {
        let Some(lc) = ch.to_lowercase().next() else {
            return;
        };
        let now = std::time::Instant::now();
        let elapsed = self
            .type_time
            .map(|t| now.duration_since(t).as_millis())
            .unwrap_or(u128::MAX);
        let repeat_one = self.type_buffer.len() == 1
            && self.type_buffer.chars().next() == Some(lc)
            && elapsed < 1000;

        if elapsed > 1000 {
            self.type_buffer.clear();
        }
        if !repeat_one {
            self.type_buffer.push(lc);
        }
        self.type_time = Some(now);
        let prefix = self.type_buffer.clone();

        let active = self.active_tab;
        if active >= self.tabs.len() || self.tabs[active].entries.is_empty() {
            return;
        }
        let total = self.tabs[active].entries.len();
        let start = self.tabs[active]
            .selected
            .iter()
            .max()
            .map(|&i| i + 1)
            .unwrap_or(0);
        let show_ext = self.cfg.settings.show_extension;

        let mut target = None;
        for k in 0..total {
            let i = (start + k) % total;
            let name = self.tabs[active].entries[i]
                .display_name(show_ext)
                .to_lowercase();
            if name.starts_with(&prefix) {
                target = Some(i);
                break;
            }
        }
        if target.is_none() && prefix.chars().count() > 1 {
            // 前缀太长没人匹配：退回单字符再找一次，别让用户干瞪眼
            self.type_buffer.truncate(lc.len_utf8());
            let single = self.type_buffer.clone();
            for k in 0..total {
                let i = (start + k) % total;
                let name = self.tabs[active].entries[i]
                    .display_name(show_ext)
                    .to_lowercase();
                if name.starts_with(&single) {
                    target = Some(i);
                    break;
                }
            }
        }

        if let Some(i) = target {
            self.select_entry(i, false, false);
            crate::ui::interact::scroll_into_view(self, i);
            self.request_repaint();
        }
    }

    /// 收藏栏拖动落位：按落点 x 找到插入位置，重排并落盘。
    pub fn drop_favorite(&mut self, index: usize, drop_x: i32) {
        let fl = crate::ui::favbar::compute(self);
        let order = self.cfg.bookmarks.ordered();
        let Some(from) = order.iter().position(|&i| i == index) else {
            return;
        };
        // 默认落到末尾（松在最右边就是挪到最后）
        let mut to = fl.slots.len().min(order.len());
        for (pos, slot) in fl.slots.iter().enumerate() {
            let mid = (slot.rect.left + slot.rect.right) / 2;
            if drop_x < mid {
                to = pos;
                break;
            }
        }
        // 自己往后挪时，移除自身会让后面的位置整体前移一位
        if to > from {
            to -= 1;
        }
        if to != from && self.cfg.bookmarks.move_item(from, to) {
            self.cfg.save_all();
        }
        self.request_repaint();
    }


    pub fn activate_favorite(&mut self, index: usize) {
        let Some(item) = self.cfg.bookmarks.items.get(index).cloned() else {
            return;
        };
        if item.is_folder {
            // 书签目录的「展开」是一次弹出菜单（见 `ui::interact::favorite_folder_menu`），
            // 由交互层拿鼠标位置去弹。这里的兜底不做任何跳转——
            // 早先这里会打开第一个子项，点一下目录就莫名跳走，很难理解。
            return;
        }
        self.open_favorite_target(&item.path);
    }

    /// 已有标签是否正停在这个目录上。
    ///
    /// 收藏栏里的位置往往就是某个标签已经打开的目录；再新开一个一模一样的
    /// 标签只会让标签条越堆越长，用户还得自己去找那个「其实已经开着」的窗口。
    pub fn tab_showing_dir(&self, path: &std::path::Path) -> Option<usize> {
        self.tabs.iter().position(|t| {
            !t.is_computer && !t.is_recycle && crate::fs::same_path(&t.path, path)
        })
    }

    /// 打开一个目录：已有标签正停在同一目录就切过去，否则按 `new_tab` 决定
    /// 新开标签还是就地进入。
    ///
    /// 收藏栏、导航窗格快捷项的点击都走这里——同一目录叠出好几个一模一样的
    /// 标签只会让标签条越来越长，用户还得自己去找那个「其实已经开着」的窗口。
    pub fn open_dir(&mut self, path: PathBuf, new_tab: bool) {
        if let Some(i) = self.tab_showing_dir(&path) {
            self.set_active_tab(i);
            return;
        }
        if new_tab {
            self.new_tab(Some(path));
        } else {
            self.navigate_to(path);
        }
    }

    fn open_favorite_target(&mut self, path: &std::path::Path) {
        if path.is_dir() {
            let new_tab = self.cfg.settings.bookmark_open_in_newtab;
            self.open_dir(path.to_path_buf(), new_tab);
        } else if let Err(e) = shell::open_default(self.hwnd, path) {
            self.show_message(&e);
        }
    }

    /// 打开任意路径：目录进标签，文件交给默认关联。
    ///
    /// 与收藏栏那条路的区别只在「是否尊重『书签在新标签打开』设置」——
    /// 这里 `new_tab` 由调用方明确给出。
    pub fn open_path(&mut self, path: PathBuf, new_tab: bool) {
        if path.is_dir() {
            self.open_dir(path, new_tab);
        } else if let Err(e) = shell::open_default(self.hwnd, &path) {
            self.show_message(&e);
        }
    }

    /// 把一批路径加进收藏夹（拖到收藏栏松手时调用）。
    pub fn add_paths_to_bookmarks(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        for p in paths {
            self.cfg.bookmarks.add_or_update(p.clone(), None);
        }
        self.request_repaint();
    }

    // ------------------------------------------------------------ 地址栏与搜索

    /// 量一段文本每个字符边界处的像素宽度（长度 = 字符数 + 1，首项为 0）。
    ///
    /// 插入符与选区必须按真实字宽定位：把中文折算成两个半角单位只是近似，
    /// 比例字体里半角字符彼此也不等宽（`i` 和 `W` 差近一倍），误差还会顺着
    /// 一段文本累积，光标于是越来越偏。
    pub fn measure_prefixes(&mut self, text: &str) -> Vec<i32> {
        let mut out = Vec::with_capacity(text.chars().count() + 1);
        out.push(0);
        if !hwnd_valid(self.hwnd) {
            return out;
        }
        let font = self.font_ui();
        unsafe {
            let hdc = GetDC(self.hwnd);
            if hdc.is_null() {
                return out;
            }
            let canvas = Canvas::new(hdc);
            let mut prefix = String::with_capacity(text.len());
            for ch in text.chars() {
                prefix.push(ch);
                out.push(canvas.text_width(&prefix, font));
            }
            ReleaseDC(self.hwnd, hdc);
        }
        out
    }

    /// 地址栏文本每个字符边界的屏幕 x，以及文本超宽时的整体左移量。
    ///
    /// 绘制插入符/选区和「点哪落哪」共用这一份结果：两边各算一遍迟早会在
    /// 长路径上错开。左移是让插入符停在框内——否则它会落在框外被裁掉，
    /// 看上去仍然像「没对准」。
    pub fn address_layout(&mut self) -> (Vec<i32>, i32) {
        let text = if self.address_editing {
            self.address_text.clone()
        } else {
            self.address_display_text()
        };
        let widths = self.measure_prefixes(&text);
        let r = self.layout.address_text_rect();
        let caret_w = widths.get(self.address_caret).copied().unwrap_or(0);
        // 留 2px 给插入符自身，让它贴在右内边距内侧而不是被裁掉
        let shift = if self.address_editing {
            (r.left + caret_w - (r.right - 2)).max(0)
        } else {
            0
        };
        (
            widths.iter().map(|w| r.left + w - shift).collect(),
            shift,
        )
    }

    /// 点地址栏：按点击的横向位置落光标，直接进编辑态。
    ///
    /// 地址栏是一个真正的文本框（整条显示完整路径），不再是只能点某一级的
    /// 面包屑——那样既选不了字，也用不了 Ctrl+C / Home / End 这些标准操作。
    pub fn begin_address_edit_at(&mut self, x: i32) {
        // 先量、再进编辑态：量的是「屏幕上此刻显示的东西」。进编辑态会把
        // 插入符挪到末尾，长路径下这一挪就带出一段左移，用那时的布局换算
        // 点击位置会整体偏掉一大截。
        let (xs, _) = self.address_layout();
        let mut best = 0usize;
        let mut best_d = i32::MAX;
        for (i, cx) in xs.iter().enumerate() {
            let d = (cx - x).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        if !self.address_editing {
            self.begin_address_edit();
        }
        self.address_caret = best;
        self.address_sel = None;
        self.request_repaint();
    }

    /// 地址栏平时显示的文本：整条完整路径（「此电脑」没有路径，显示空串）。
    pub fn address_display_text(&self) -> String {
        let Some(t) = self.active() else {
            return String::new();
        };
        if t.is_recycle {
            "回收站".to_string()
        } else if t.is_computer {
            String::new()
        } else {
            t.path.to_string_lossy().into_owned()
        }
    }

    /// 点击地址栏某个 x 处：落在某一段路径上就返回该段的完整前缀。
    ///
    /// 区间由绘制层量完字宽后回填（见 `address_segments`），所以这里只管
    /// 命中，不重算像素。段与段之间那一两个像素（分隔符）不算任何一段，
    /// 落在那儿就当作「没点到路径」，交给调用方进编辑态。
    pub fn address_segment_at(&self, x: i32) -> Option<&str> {
        let t = self.active()?;
        // 回收站、「此电脑」没有可下钻的层级：整条都是「点一下进编辑」
        if t.is_recycle || t.is_computer {
            return None;
        }
        self.address_segments
            .iter()
            .find(|(l, r, _)| x >= *l && x < *r)
            .map(|(_, _, prefix)| prefix.as_str())
    }

    /// 点地址栏某一层：直接跳过去。
    ///
    /// 靠 `address_segment_at` 拿前缀，所以和绘制用的是同一份像素区间。
    /// 点的就是当前这一层时改成进编辑态——跳到自己没有意义，但「点一下就
    /// 能改路径」得留个入口，否则长路径铺满整条地址栏时就没有空白可点了。
    pub fn jump_to_address_level(&mut self, x: i32) {
        let Some(prefix) = self.address_segment_at(x).map(|s| s.to_string()) else {
            return;
        };
        let path = std::path::PathBuf::from(prefix);
        let same = self
            .active()
            .map(|t| !t.is_computer && !t.is_recycle && t.path == path)
            .unwrap_or(false);
        if same {
            self.begin_address_edit_at(x);
            return;
        }
        if path.is_dir() {
            self.navigate_to(path);
        }
    }

    pub fn begin_address_edit(&mut self) {
        // 先取出文本，避免 &Tab 借用与随后的 &mut self 赋值同时存在
        let (is_computer, text) = {
            let Some(t) = self.active() else { return };
            (t.is_computer, t.path.to_string_lossy().into_owned())
        };
        self.address_text = if is_computer { String::new() } else { text };
        self.address_caret = self.address_text.chars().count();
        self.address_sel = None;
        self.address_editing = true;
        self.search_focused = false;
        self.ensure_caret_timer();
        // 一进编辑态就把历史候选挂出来：这是「最近去过哪」最自然的入口，
        // 不用先敲几个字母才看得见。
        self.refresh_addr_suggest();
        self.request_repaint();
    }

    /// 地址栏历史下拉是否该显示。
    pub fn addr_suggest_open(&self) -> bool {
        self.address_editing && !self.addr_suggest.is_empty()
    }

    /// 按当前输入刷新历史候选。
    ///
    /// 键入、退格、粘贴、进入编辑态都走 `after_edit` / 这里，别在绘制里做：
    /// 每帧重新查一遍历史就是纯浪费。已经和历史项完全相同的就不列出来，
    /// 否则下拉里会出现一条「和输入框里一模一样的路径」。
    pub fn refresh_addr_suggest(&mut self) {
        if !self.address_editing {
            self.addr_suggest.clear();
            self.addr_suggest_sel = None;
            return;
        }
        let cur = self.address_text.trim().to_string();
        let items: Vec<String> = self
            .cfg
            .history
            .suggest(&cur, ADDR_SUGGEST_MAX)
            .into_iter()
            .filter(|s| !s.eq_ignore_ascii_case(&cur))
            .map(str::to_string)
            .collect();
        self.addr_suggest = items;
        self.addr_suggest_sel = None;
    }

    pub fn close_addr_suggest(&mut self) {
        self.addr_suggest.clear();
        self.addr_suggest_sel = None;
    }

    /// 历史下拉一行的高度（跟随 DPI 缩放）。
    fn addr_suggest_row_h(&self) -> i32 {
        (26.0 * self.layout.scale).round() as i32
    }

    /// 历史下拉的整块浮层矩形（贴在地址栏下沿）。
    pub fn addr_suggest_pane_rect(&self) -> RECT {
        let a = self.layout.address;
        let h = self.addr_suggest_row_h() * self.addr_suggest.len() as i32;
        let top = a.bottom + (4.0 * self.layout.scale).round() as i32;
        RECT {
            left: a.left,
            top,
            right: a.right,
            bottom: top + h,
        }
    }

    /// 历史下拉第 `i` 条的矩形。绘制与命中测试共用同一份几何。
    pub fn addr_suggest_item_rect(&self, i: usize) -> RECT {
        let p = self.addr_suggest_pane_rect();
        let rh = self.addr_suggest_row_h();
        RECT {
            left: p.left + 2,
            top: p.top + i as i32 * rh,
            right: p.right - 2,
            bottom: p.top + (i as i32 + 1) * rh,
        }
    }

    /// 上下键在下拉里移动高亮。返回 `true` 表示这次按键被下拉吃掉了。
    pub fn addr_suggest_move(&mut self, down: bool) -> bool {
        if !self.addr_suggest_open() {
            return false;
        }
        let n = self.addr_suggest.len();
        let next = match (self.addr_suggest_sel, down) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        self.addr_suggest_sel = Some(next);
        self.request_repaint();
        true
    }

    /// 采纳下拉里的第 `i` 条：直接跳过去。
    pub fn accept_addr_suggest(&mut self, i: usize) {
        let Some(p) = self.addr_suggest.get(i).cloned() else {
            return;
        };
        self.close_addr_suggest();
        self.address_editing = false;
        self.address_sel = None;
        self.navigate_to(PathBuf::from(p));
    }

    pub fn focus_search(&mut self) {
        self.search_focused = true;
        self.address_editing = false;
        // 插入符要闪，得有人定期催重绘；失焦后定时器自己会停
        self.ensure_caret_timer();
        self.request_repaint();
    }

    /// 开启插入符定时器。已经在跑时重复调用无副作用。
    fn ensure_caret_timer(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime;
        if self.hwnd.is_null() {
            return;
        }
        let blink = unsafe { GetCaretBlinkTime() };
        let blink = if blink == 0 || blink == u32::MAX { 530 } else { blink };
        unsafe {
            SetTimer(self.hwnd, TIMER_CARET, blink, None);
        }
    }

    /// 确保详情窗格的预览对应当前选中项。
    ///
    /// 由绘制路径在每帧开头调用：传进来的路径没变就什么都不做（命中缓存），
    /// 变了才投递一次后台读取。之所以放在绘制里而不是选中逻辑里，
    /// 是因为选中会从十几个地方改（点击、框选、键盘、刷新、拖放），
    /// 漏掉一处就会出现「选中换了但预览还是上一张」。
    ///
    /// 读盘、判尺寸、解文本都在后台（`io::Job::Preview`）；真正的 GDI+
    /// 解码仍在 UI 线程，回到这里由 `on_io` 落地。以前整个流程都在这条
    /// 绘制路径上同步跑，选中一张网络盘上的大图就会把窗口卡住。
    pub fn ensure_preview(&mut self, path: Option<PathBuf>, ext: &str, size: u64) {
        let Some(p) = path else {
            self.preview = None;
            self.preview_pending = None;
            return;
        };
        // 已经显示着这一份，或已经有一份在等结果
        if self
            .preview
            .as_ref()
            .map(|pr| pr.path == p)
            .unwrap_or(false)
        {
            return;
        }
        if self.preview_pending.as_deref() == Some(p.as_path()) {
            return;
        }
        let id = self.preview_seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.preview_pending = Some(p.clone());
        self.io.send(Job::Preview {
            id,
            path: p,
            ext: ext.to_string(),
            size,
        });
    }

    /// 悬停提示的节流：停在一个工具栏按钮上满 [`HOVER_TIP_MS`] 才出气泡。
    ///
    /// 每次悬停目标变化都要重新计时——鼠标一路扫过整排按钮时，
    /// 不该每个按钮都闪一下提示条。
    pub fn update_hover_tooltip(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
        self.tooltip_ready = false;
        if self.hwnd.is_null() {
            return;
        }
        unsafe {
            if matches!(self.hover, ui::Hit::ToolbarButton(_)) {
                SetTimer(self.hwnd, TIMER_HOVER, HOVER_TIP_MS, None);
            } else {
                KillTimer(self.hwnd, TIMER_HOVER);
            }
        }
    }

    pub fn cancel_edit(&mut self) {
        if self.rename_target.is_some() {
            self.rename_target = None;
            self.inline_edit_text.clear();
        }
        self.address_editing = false;
        self.address_sel = None;
        self.close_addr_suggest();
        self.search_focused = false;
        if self.cur().search_backup.is_some() && self.cur().search_text.is_empty() {
            self.restore_search_backup();
        }
        // 失焦后「按回车搜索」就没人执行了：留着这句提示只会误导
        if matches!(self.cur().search_state, Some(SearchState::Pending)) {
            self.cur_mut().search_state = None;
        }
        self.flush_pending_fs_refresh();
        self.request_repaint();
    }

    pub fn commit_edit(&mut self) {
        if let Some(idx) = self.rename_target.take() {
            let new_name = self.inline_edit_text.clone();
            self.inline_edit_text.clear();
            if let Some(p) = self.entry_path(idx) {
                if new_name.trim().is_empty() {
                    self.show_message(S::ALERT_EMPTY_NAME);
                } else {
                    let res = fsops::rename(self.hwnd, &p, &new_name);
                    if res == fsops::OpResult::Ok {
                        // 名字就是我们给的那个：本地就地更新，不整目录重扫
                        self.apply_rename_locally(idx, &new_name);
                    } else {
                        self.handle_op_result(res);
                    }
                }
            }
            self.flush_pending_fs_refresh();
            return;
        }

        if self.address_editing {
            // 回车时优先采纳下拉里高亮的那一条，没有高亮才按输入框内容跳转
            if let Some(i) = self.addr_suggest_sel {
                self.accept_addr_suggest(i);
                return;
            }
            self.close_addr_suggest();
            let text = self.address_text.clone();
            self.address_editing = false;
            self.address_sel = None;
            let lowered = text.trim().to_ascii_lowercase();
            if lowered == "回收站" || lowered == "shell:recyclebinfolder" || lowered == RECYCLE_PATH {
                self.open_recycle_bin();
                return;
            }
            // 网址：交给默认浏览器。资源管理器的地址栏也接受网址，
            // 以前粘贴一个链接会被当成路径，回一句「无法访问该位置」。
            if fs::looks_like_url(&text) {
                if let Err(e) = shell::open_url(&fs::normalize_url(&text)) {
                    self.show_message(&e);
                }
                return;
            }
            // `shell:` 命名空间别名（shell:startup、shell:downloads 等）
            if let Some(p) = fs::resolve_shell_alias(&text) {
                self.navigate_to(p);
                return;
            }
            if let Some(p) = fs::resolve_path(&text) {
                if p.is_dir() {
                    self.navigate_to(p);
                } else if p.is_file() {
                    if let Some(parent) = p.parent() {
                        self.navigate_to(parent.to_path_buf());
                        // 枚举还没落地，列表此刻多半没有它：记下名字，
                        // on_io 补选中。已有条目时立刻选。
                        let fname = p.file_name().map(|s| s.to_string_lossy().into_owned());
                        if let Some(f) = fname {
                            let found = self
                                .active()
                                .and_then(|t| t.entries.iter().position(|e| e.name == f));
                            if let Some(idx) = found {
                                self.select_entry(idx, false, false);
                            } else if let Some(t) = self.active_mut() {
                                t.pending_select = Some(f);
                            }
                        }
                    }
                } else {
                    self.show_message(S::STATUS_NO_ACCESS);
                }
            }
            return;
        }

        if self.search_focused {
            self.search_focused = false;
            self.begin_search();
            return;
        }

        self.request_repaint();
    }


    // ------------------------------------------------------------ 内联重命名

    /// 是否正处于表格内重命名状态。
    pub fn rename_active(&self) -> bool {
        self.rename_target.is_some()
    }

    /// 供绘制层取重命名编辑框状态：(目标索引, 文本, 光标偏移)。
    pub fn rename_state(&self) -> Option<(usize, &str, usize)> {
        self.rename_target
            .map(|i| (i, self.inline_edit_text.as_str(), self.inline_edit_caret))
    }

    // ------------------------------------------------------------ 窗口控制

    pub fn request_repaint(&self) {
        if hwnd_valid(self.hwnd) {
            unsafe {
                InvalidateRect(self.hwnd, std::ptr::null(), 0);
            }
        }
    }

    /// 只让一块区域失效。
    ///
    /// 插入符闪烁是周期性事件（每 530ms 一次），以前每次都整窗重绘：
    /// 标题栏、导航窗格、整页文件列表全部重画一遍，只为让一个 1px 竖线翻面。
    pub fn request_repaint_rect(&self, r: RECT) {
        if hwnd_valid(self.hwnd) {
            unsafe {
                InvalidateRect(self.hwnd, &r, 0);
            }
        }
    }

    pub fn begin_drag(&self) {
        unsafe {
            ReleaseCapture();
            SendMessageW(self.hwnd, WM_NCLBUTTONDOWN, HTCAPTION as WPARAM, 0);
        }
    }

    pub fn minimize(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_MINIMIZE);
        }
    }

    pub fn toggle_maximize(&self) {
        unsafe {
            if IsZoomed(self.hwnd) != 0 {
                ShowWindow(self.hwnd, SW_RESTORE);
            } else {
                ShowWindow(self.hwnd, SW_MAXIMIZE);
            }
        }
    }

    /// 请求关闭窗口。
    ///
    /// 不再询问「确定关闭 N 个标签」：关闭按钮就该是关闭，确认框只会挡路。
    pub fn request_close(&mut self) {
        // 「关闭提示」（原软件 CloseAlert）：多标签时确认一次，防误关。
        // 确认框已开着就不重复弹（点确定/取消都在框上）；单标签直接关。
        if self.cfg.settings.close_alter
            && !self.closing
            && self.tabs.len() > 1
            && !matches!(
                self.dialog.as_ref().map(|d| &d.intent),
                Some(crate::ui::dialog::DialogIntent::ExitApp)
            )
        {
            self.dialog_confirm(
                S::ALERT_CLOSE_TITLE,
                S::ALERT_CLOSE_BODY,
                crate::ui::dialog::DialogIntent::ExitApp,
            );
            return;
        }
        self.closing = true;
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }

    // ------------------------------------------------------------ 对话框

    /// 弹一条提示（自绘，跟随主题）。
    pub fn dialog_info(&mut self, text: &str) {
        self.dialog = Some(crate::ui::dialog::Dialog::info(S::APP_NAME, text));
        self.request_repaint();
    }

    /// 弹一个确认框。点「确定」后按 `intent` 执行动作。
    pub fn dialog_confirm(
        &mut self,
        title: &str,
        body: &str,
        intent: crate::ui::dialog::DialogIntent,
    ) {
        self.dialog = Some(crate::ui::dialog::Dialog::confirm(title, body, intent));
        self.request_repaint();
    }

    /// 弹一个输入框。`colors` 为真时额外给一行 preselected 标签色。
    pub fn dialog_prompt(
        &mut self,
        title: &str,
        label: &str,
        initial: &str,
        colors: bool,
        intent: crate::ui::dialog::DialogIntent,
    ) {
        self.dialog = Some(crate::ui::dialog::Dialog::prompt(
            title, label, initial, colors, intent,
        ));
        // 输入框里的光标要闪，得有定时器催重绘；关掉对话框后它自己会停
        self.ensure_caret_timer();
        self.request_repaint();
    }

    /// 关掉对话框（不做任何动作）。
    pub fn dialog_cancel(&mut self) {
        if self.dialog.take().is_some() {
            self.flush_pending_fs_refresh();
            self.request_repaint();
        }
    }

    /// 点「确定 / 我知道了」：取出对话框，按意图落动作。
    pub fn dialog_accept(&mut self) {
        use crate::ui::dialog::DialogIntent;

        let Some(d) = self.dialog.take() else {
            return;
        };
        // 输入类对话框的名字不能是空白：直接关掉再提示，
        // 比留一个「点了确定但什么都没发生」的框要好理解。
        let name = d.input.trim().to_string();
        let needs_name = matches!(
            d.intent,
            DialogIntent::NewBookmarkFolder
                | DialogIntent::RenameBookmark(_)
                | DialogIntent::RenameNavPlace(_)
                | DialogIntent::NewTag { .. }
                | DialogIntent::RenameTag(_)
        );
        if needs_name && name.is_empty() {
            self.dialog_info(S::ALERT_EMPTY_NAME);
            return;
        }

        match d.intent.clone() {
            DialogIntent::Info => {}
            // Shift+Delete 确认通过：永久删除（不经过回收站）
            DialogIntent::DeletePermanent { items } => {
                self.send_op(crate::io::OpWhat::Delete {
                    items,
                    permanent: true,
                });
            }
            DialogIntent::RestoreExplorer => {
                if let Err(e) = crate::shell_integration::restore() {
                    self.dialog_info(&e);
                } else {
                    self.cfg.settings.replace_sysexplorer = false;
                    self.cfg.save_all();
                }
            }            DialogIntent::NewBookmarkFolder => {
                self.cfg
                    .bookmarks
                    .items
                    .push(crate::cfg::bookmarks::Bookmark::folder(name));
                self.cfg.save_all();
            }
            DialogIntent::RenameBookmark(i) => {
                self.cfg.bookmarks.rename_at(i, name);
                self.cfg.save_all();
            }
            DialogIntent::RenameNavPlace(i) => {
                self.cfg.nav_places.rename_at(i, name);
                self.cfg.save_all();
                self.rebuild_nav_rows();
            }
            DialogIntent::NewTag { paths } => {
                let color = crate::cfg::tags::PALETTE
                    .get(d.picked)
                    .copied()
                    .unwrap_or(crate::cfg::tags::PALETTE[0]);
                // 同名标签已存在就复用它，而不是造出第二个「重要」
                let index = self.cfg.tags.add(name, color);
                for p in paths {
                    self.cfg.tags.add_item(index, p);
                }
                self.cfg.tags.save(&self.cfg.root);
                self.rebuild_nav_rows();
            }
            DialogIntent::RenameTag(i) => {
                self.cfg.tags.rename_at(i, name);
                self.cfg.tags.save(&self.cfg.root);
                self.rebuild_nav_rows();
            }
            DialogIntent::RemoveTag(i) => {
                self.cfg.tags.remove_at(i);
                self.cfg.tags.save(&self.cfg.root);
                self.rebuild_nav_rows();
            }
            DialogIntent::ExitApp => {
                // 「关闭提示」确认通过：closing 置位后 request_close
                // 不再弹框，直接进销毁流程
                self.closing = true;
                unsafe {
                    DestroyWindow(self.hwnd);
                }
                return;
            }
        }
        // 对话框可能开着时目录变了（fs 刷新被推迟），关掉后补上
        self.flush_pending_fs_refresh();
        self.request_repaint();
    }

    // ------------------------------------------------------------ 标签

    /// 新建标签：名字 + 颜色，确定后把 `paths` 挂进去。
    pub fn prompt_new_tag(&mut self, paths: Vec<PathBuf>) {
        self.dialog_prompt(
            S::TAG_NEW_TITLE,
            S::TAG_NAME_LABEL,
            "",
            true,
            crate::ui::dialog::DialogIntent::NewTag { paths },
        );
    }

    /// 重命名第 N 个标签。
    pub fn prompt_rename_tag(&mut self, index: usize) {
        let Some(tag) = self.cfg.tags.list.get(index) else {
            return;
        };
        let initial = tag.name.clone();
        self.dialog_prompt(
            S::TAG_RENAME_TITLE,
            S::TAG_NAME_LABEL,
            &initial,
            false,
            crate::ui::dialog::DialogIntent::RenameTag(index),
        );
    }

    /// 删除第 N 个标签（先确认：标签里的条目会一起散掉）。
    ///
    /// 注意只说「删除标签」，绝不说「删除文件」——磁盘上的东西一个都不动。
    pub fn confirm_remove_tag(&mut self, index: usize) {
        let Some(tag) = self.cfg.tags.list.get(index) else {
            return;
        };
        let body = format!("{}\n\n{}", tag.name, S::TAG_REMOVE_CONFIRM);
        self.dialog_confirm(S::TAG_MENU_REMOVE, &body, crate::ui::dialog::DialogIntent::RemoveTag(index));
    }

    /// 把当前详情窗格的目标路径挂到第 N 个标签里。
    pub fn tag_assign_target(&mut self, index: usize) {
        let Some(path) = crate::ui::detailtags::target_path(self) else {
            return;
        };
        if self.cfg.tags.add_item(index, path) {
            self.cfg.tags.save(&self.cfg.root);
            self.rebuild_nav_rows();
            self.request_repaint();
        }
    }

    /// 把当前详情窗格的目标路径从第 N 个标签里摘掉。
    pub fn tag_unassign_target(&mut self, index: usize) {
        let Some(path) = crate::ui::detailtags::target_path(self) else {
            return;
        };
        if self.cfg.tags.remove_item_by_path(index, &path) {
            self.cfg.tags.save(&self.cfg.root);
            self.rebuild_nav_rows();
            self.request_repaint();
        }
    }

    /// 从导航窗格的某个标签里移除第 `item` 条。
    pub fn tag_remove_item(&mut self, tag: usize, item: usize) {
        if self.cfg.tags.remove_item_at(tag, item) {
            self.cfg.tags.save(&self.cfg.root);
            self.rebuild_nav_rows();
            self.request_repaint();
        }
    }

    /// 简单提示框（历史名字，28 处错误提示都用它）。
    ///
    /// 以前这里是 `MessageBoxW`：深色主题下弹一个系统浅色框，很突兀。
    /// 现在统一走自绘对话框。
    pub fn show_message(&mut self, text: &str) {
        self.dialog_info(text);
    }

    /// 状态栏瞬态提示：不打断操作，几秒后自己消失。
    ///
    /// 高频动作的反馈用它（复制路径、切换排序等）；会阻塞交互、需要
    /// 用户看见并确认的才走 `show_message`。
    pub fn show_status_note(&mut self, text: &str) {
        self.status_note = Some(text.to_string());
        if hwnd_valid(self.hwnd) {
            unsafe {
                SetTimer(self.hwnd, TIMER_NOTE, NOTE_MS, None);
            }
        }
        self.request_repaint();
    }

    /// 新开一个实例（`/OpenInNewWindow` 语义）。
    pub fn spawn_new_window(&self) {
        let exe = std::env::current_exe().unwrap_or_default();
        let arg = self
            .active()
            .map(|t| t.path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut cmd = std::process::Command::new(exe);
        // 必须显式带 /OpenInNewWindow：单实例接管对所有无开关启动生效，
        // 不带开关的新进程会把路径投递回当前实例然后退出——用户以为
        // 开了新窗口，实际只多了个标签。
        cmd.arg("/OpenInNewWindow");
        if !arg.is_empty() {
            cmd.arg(arg);
        }
        cmd.spawn().ok();
    }

    /// 主菜单（对应 main_menu.xml）。
    pub fn show_main_menu(&mut self) {
        let menu = self.active().map(|t| t.path.clone());
        let _ = menu;
        // 用系统弹出菜单实现，保证键盘可达性与可访问性。
        unsafe {
            let hmenu = CreatePopupMenu();
            if hmenu.is_null() {
                return;
            }
            let add = |id: usize, text: &str| {
                let w = to_wide(text);
                AppendMenuW(hmenu, MF_STRING, id, w.as_ptr());
            };

            const ID_SHORTCUT: usize = 1;
            const ID_TAKEOVER: usize = 2;
            const ID_SETTINGS: usize = 3;
            const ID_FEEDBACK: usize = 4;
            const ID_ABOUT: usize = 5;
            const ID_EXIT: usize = 6;

            // 接管项文案随状态变化：已经接管时写着「点击可恢复」，
            // 否则用户不知道再点一次会发生什么。
            let takeover_label = match crate::shell_integration::state() {
                crate::shell_integration::TakeoverState::Active => S::MENU_RESTORE_EXPLORER,
                _ => S::MENU_TAKE_OVER_EXPLORER,
            };

            add(ID_SHORTCUT, S::MENU_ADD_DESKTOP_SHORTCUT);
            add(ID_TAKEOVER, takeover_label);
            AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
            add(ID_SETTINGS, S::MENU_SETTINGS);
            add(ID_FEEDBACK, S::MENU_FEEDBACK);
            add(ID_ABOUT, S::MENU_ABOUT);
            AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
            add(ID_EXIT, S::MENU_EXIT);

            let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            // 菜单外壳由 uxtheme 画，弹之前把配色对齐到应用内主题
            crate::win::init::apply_menu_theme();
            let cmd = TrackPopupMenu(
                hmenu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                std::ptr::null(),
            ) as usize;
            DestroyMenu(hmenu);

            match cmd {
                ID_SHORTCUT => {
                    // 成功不再弹窗：桌面上看到图标就是反馈
                    if let Err(e) = crate::shell_integration::create_desktop_shortcut() {
                        self.dialog_info(&e);
                    }
                }
                ID_TAKEOVER => self.toggle_takeover(),
                ID_SETTINGS => self.show_settings_dialog(),
                ID_FEEDBACK => {
                    if let Err(e) = crate::fs::shell::open_url(S::FEEDBACK_URL) {
                        self.dialog_info(&format!("无法打开浏览器：{e}"));
                    }
                }
                ID_ABOUT => {
                    self.dialog_info(&format!(
                        "{} {}\n{}",
                        S::APP_NAME,
                        env!("CARGO_PKG_VERSION"),
                        S::FEEDBACK_URL
                    ));
                }
                ID_EXIT => self.request_close(),
                _ => {}
            }
        }
    }

    /// 弹系统的 Shell 上下文菜单。
    ///
    /// 自己画菜单永远追不上系统：第三方扩展（压缩工具、版本控制、
    /// 云盘客户端）都是往 Shell 里注册的，只有让它自己弹才全。
    pub fn show_shell_context_menu(&mut self) {
        let (paths, dir) = {
            let Some(t) = self.active() else { return };
            let paths: Vec<PathBuf> = t
                .selected
                .iter()
                .filter_map(|&i| t.entries.get(i).map(|e| e.full.clone()))
                .collect();
            let dir = if t.is_computer {
                None
            } else {
                Some(t.path.clone())
            };
            (paths, dir)
        };
        let inv = crate::win::shellmenu::show(self.hwnd, &paths, dir.as_deref());
        // 点了「新建」但没建成（只读目录、无权限、磁盘满……）：
        // 必须说出来，否则用户看到的就是“点了没反应”。
        if let Some(err) = inv.new_error {
            self.dialog_info(&format!("新建失败：{err}"));
        }
        // 「新建」是我们自己建的，名字已经知道了：枚举落地后按名字选中它
        // 并直接进入重命名（资源管理器新建完就是这个手感）。
        if let Some(name) = inv.created_new {
            if let Some(t) = self.tabs.get_mut(self.active_tab) {
                t.pending_rename = Some(name);
            }
        }
        // 菜单里可能执行了「删除」「重命名」「新建」这类改动文件系统的命令，
        // 关掉之后必须重新列一遍——以前只重绘，列表还停在旧内容上。
        self.reload_active_tab();
        self.request_repaint();
    }

    fn toggle_takeover(&mut self) {
        use crate::shell_integration::{self, TakeoverState};
        use crate::ui::dialog::DialogIntent;

        match shell_integration::state() {
            TakeoverState::Active => {
                // 已接管 → 提供恢复。确认后由 `dialog_accept` 落动作
                self.dialog_confirm(
                    S::MENU_TAKE_OVER_EXPLORER,
                    "确定恢复系统默认文件管理？",
                    DialogIntent::RestoreExplorer,
                );
            }
            _ => match shell_integration::take_over() {
                Ok(scopes) => {
                    self.cfg.settings.replace_sysexplorer = true;
                    self.cfg.save_all();
                    // 全部写成功了就不再弹窗：菜单项本身就是状态反馈
                    if let Some(missing) = shell_integration::admin_missing(&scopes) {
                        self.dialog_info(&format!("已接管；{missing} 需管理员权限"));
                    }
                }
                Err(e) => self.dialog_info(&e),
            },
        }
    }

    /// 打开设置面板。
    ///
    /// 以前这里是一个 MessageBox 逐项询问，改一项就得关掉再打开，
    /// 而且看不到其他项的当前状态。现在换成自绘面板（`ui::settingspane`），
    /// 开关即时生效。
    fn show_settings_dialog(&mut self) {
        self.settings_open = true;
        self.request_repaint();
    }

    // ------------------------------------------------------------ 布局与命中

    pub fn recompute_layout(&mut self) {
        let client = winwnd::client_rect(self.hwnd);
        self.layout = Layout::compute(
            client,
            self.dpi_scale,
            self.cfg.settings.ui_scale,
            self.cfg.settings.bookmark_visible,
            self.cfg.settings.toolbar_visible,
            self.cfg.settings.show_nav_pane,
            self.cfg.settings.show_detail_pane,
            self.cfg.settings.nav_pane_w,
            self.cfg.settings.detail_pane_w,
        );
        // 树的行矩形依赖 layout.nav_pane，必须在布局更新之后重建
        self.rebuild_nav_rows();
        // 放置目标要用收藏栏矩形判断「能不能放」，这里同步一份
        crate::win::droptarget::set_favbar_rect(self.layout.favbar);
        // 导航窗格同理：拖进这一块就是加入「快捷访问」
        crate::win::droptarget::set_nav_rect(self.layout.nav_pane);
        // 文件视图也要登记：拖到条目上/空白处才算有落点
        crate::win::droptarget::set_view_rect(self.layout.view);
    }

    /// 重建导航树。布局变化、展开状态切换、盘符增减后都要调用。
    pub fn rebuild_nav_rows(&mut self) {
        // 先算再赋值：build 借用 &self，而赋值需要 &mut self
        let tree = crate::ui::navpane::build(self);
        self.nav_rows = tree.rows;
        self.nav_groups = tree.groups;
        self.nav_max_scroll = tree.max_scroll;
        self.nav_content_h = tree.content_h;
        // 树可能缩短（折叠驱动器、删掉快捷项）：把滚动量收回可用范围后重建一次
        if self.nav_scroll > self.nav_max_scroll {
            self.nav_scroll = self.nav_max_scroll;
            let tree = crate::ui::navpane::build(self);
            self.nav_rows = tree.rows;
            self.nav_groups = tree.groups;
            self.nav_max_scroll = tree.max_scroll;
            self.nav_content_h = tree.content_h;
        }
    }

    // ---------------------------------------------------------- 分组顺序拖动

    /// 在导航窗格的某条分隔线上按下：开始拖动它下面那一组。
    pub fn nav_sep_press(&mut self, section: crate::cfg::settings::NavSection) {
        let slot = crate::ui::navpane::insertion_slot(&self.nav_groups, section, self.mouse_y);
        self.nav_sep_drag = Some(crate::ui::navpane::SepDrag { section, slot });
        self.request_repaint();
    }

    /// 拖动中：按指针位置更新目标槽位。
    pub fn nav_sep_move(&mut self) {
        let Some(drag) = self.nav_sep_drag.as_ref() else {
            return;
        };
        let section = drag.section;
        let slot = crate::ui::navpane::insertion_slot(&self.nav_groups, section, self.mouse_y);
        let Some(drag) = self.nav_sep_drag.as_mut() else {
            return;
        };
        if drag.slot != slot {
            drag.slot = slot;
            self.request_repaint();
        }
    }

    /// 松开：顺序真变了就落盘。
    pub fn nav_sep_release(&mut self) {
        let Some(drag) = self.nav_sep_drag.take() else {
            return;
        };
        let order = &self.cfg.settings.nav_sections;
        if !drag.changed(order) {
            self.request_repaint();
            return;
        }
        let next = crate::ui::navpane::reorder(order, drag.section, drag.slot);
        self.cfg.settings.nav_sections = next;
        self.cfg.settings.save(&self.cfg.root);
        self.rebuild_nav_rows();
        self.request_repaint();
    }

    /// 取消拖动（Esc 或窗口失焦）。
    pub fn nav_sep_cancel(&mut self) {
        if self.nav_sep_drag.take().is_some() {
            self.request_repaint();
        }
    }

    // ------------------------------------------------------ 详情窗格模块顺序

    /// 在详情窗格的两个模块之间按下：开始拖动下面那个模块。
    pub fn detail_sep_press(&mut self) {
        // 分割线下面那个模块就是被拖的对象；sanitize 保证列表里恰好两个模块
        let Some(module) = self.cfg.settings.detail_sections.get(1).copied() else {
            // 只有一个模块时没有可换位的对象（配置被手改坏时的兜底）
            return;
        };
        let sec = crate::ui::detailtags::sections(self);
        let slot = crate::ui::detailtags::insertion_slot(&sec, self.mouse_y);
        self.detail_sep_drag = Some(crate::ui::detailtags::DetailSepDrag { module, slot });
        self.request_repaint();
    }

    /// 拖动中：按指针位置更新目标槽位，并画插入指示线。
    pub fn detail_sep_move(&mut self) {
        if self.detail_sep_drag.is_none() {
            return;
        }
        let slot = crate::ui::detailtags::insertion_slot(
            &crate::ui::detailtags::sections(self),
            self.mouse_y,
        );
        let Some(drag) = self.detail_sep_drag.as_mut() else {
            return;
        };
        if drag.slot != slot {
            drag.slot = slot;
            self.request_repaint();
        }
    }

    /// 松开：顺序真变了就落盘。
    pub fn detail_sep_release(&mut self) {
        let Some(drag) = self.detail_sep_drag.take() else {
            return;
        };
        let order = &self.cfg.settings.detail_sections;
        if !drag.changed(order) {
            self.request_repaint();
            return;
        }
        self.cfg.settings.detail_sections =
            crate::ui::detailtags::reorder(order, drag.module, drag.slot);
        self.cfg.settings.save(&self.cfg.root);
        self.request_repaint();
    }

    /// 取消拖动（Esc 或窗口失焦）。
    pub fn detail_sep_cancel(&mut self) {
        if self.detail_sep_drag.take().is_some() {
            self.request_repaint();
        }
    }

    /// 滚动导航窗格。`delta` 为负向上、为正向下（设备像素）。
    pub fn scroll_nav(&mut self, delta: i32) {
        if self.nav_max_scroll <= 0 {
            return;
        }
        let next = (self.nav_scroll + delta).clamp(0, self.nav_max_scroll);
        if next != self.nav_scroll {
            self.nav_scroll = next;
            self.rebuild_nav_rows();
            self.request_repaint();
        }
    }

    /// 直接把导航窗格滚到 `v`（拖动滚动条滑块时用）。
    pub fn set_nav_scroll(&mut self, v: i32) {
        let next = v.clamp(0, self.nav_max_scroll.max(0));
        if next != self.nav_scroll {
            self.nav_scroll = next;
            self.rebuild_nav_rows();
            self.request_repaint();
        }
    }

    /// 把一批路径加进导航窗格的快捷访问区（拖到导航窗格松手时调用）。
    ///
    /// 返回是否真的加了新项：全是重复路径时不必重排。
    pub fn add_paths_to_nav_places(&mut self, paths: &[PathBuf]) -> bool {
        if paths.is_empty() {
            return false;
        }
        let mut changed = false;
        for p in paths {
            if self.cfg.nav_places.add(p.clone()) {
                changed = true;
            }
        }
        if changed {
            self.cfg.save_all();
            self.rebuild_nav_rows();
            self.request_repaint();
        }
        changed
    }

    /// 快捷项拖动落位：按落点 y 决定的插入位置重排并落盘。
    pub fn drop_nav_place(&mut self, index: usize, drop_y: i32) {
        // 只在快捷项之间算顺序：其余行（主文件夹/回收站/此电脑…）不参与
        let slots: Vec<(usize, i32)> = self
            .nav_rows
            .iter()
            .filter_map(|r| match r.kind {
                crate::ui::navpane::NavKind::Place { index, .. } => {
                    Some((index, (r.rect.top + r.rect.bottom) / 2))
                }
                _ => None,
            })
            .collect();
        if slots.is_empty() {
            return;
        }
        let mut to = slots.len();
        for (pos, (_, mid)) in slots.iter().enumerate() {
            if drop_y < *mid {
                to = pos;
                break;
            }
        }
        // from 是配置下标；先换算成显示位序（两者目前同序，仍按位置换算避免假设）
        let Some(from) = slots.iter().position(|&(i, _)| i == index) else {
            return;
        };
        if to > from {
            to -= 1;
        }
        if to != from && self.cfg.nav_places.move_item(from, to) {
            self.cfg.save_all();
            self.rebuild_nav_rows();
        }
        self.request_repaint();
    }

    /// 拖动经过时更新「会落在哪个标签行」的高亮。
    ///
    /// `DragOver` 已经把落点存进来了，这里只做命中：落在标签行或它的条目上
    /// 就高亮那一行，其余情况清空。只在状态真的变了才重绘——拖动期间这条
    /// 消息每移动一次就来一次，无脑重绘整块导航窗格纯属浪费。
    pub fn update_drop_hover(&mut self) {
        let next = crate::win::droptarget::take_drag_point().and_then(|(x, y)| {
            if !crate::win::paint::rect_contains(self.layout.nav_pane, x, y) {
                return None;
            }
            let i = crate::ui::navpane::row_at(&self.nav_rows, x, y)?;
            match self.nav_rows.get(i).map(|r| r.kind.clone()) {
                Some(crate::ui::navpane::NavKind::Tag { .. })
                | Some(crate::ui::navpane::NavKind::TagItem { .. }) => Some(i),
                _ => None,
            }
        });
        if next != self.drop_tag_row {
            self.drop_tag_row = next;
            let r = self.layout.nav_pane;
            self.request_repaint_rect(r);
        }
    }

    /// 拖放落到导航窗格：落在标签行上就给它加这些路径，否则加进「快捷访问」。
    ///
    /// 窗口内拖拽和外部拖入共用这一处判定——各写一份的话早晚出现
    /// 「从资源管理器拖能打标签、从自己列表里拖却只能加快捷访问」这种怪事。
    pub fn drop_paths_onto_nav(&mut self, paths: &[PathBuf], pt: Option<(i32, i32)>) {
        let tag = pt
            .and_then(|(x, y)| crate::ui::navpane::row_at(&self.nav_rows, x, y))
            .and_then(|i| match self.nav_rows.get(i).map(|r| r.kind.clone()) {
                Some(crate::ui::navpane::NavKind::Tag { index, .. }) => Some(index),
                // 拖到某个标签下的条目上也认这个标签：鼠标就在那一格里，
                // 说「那我不算」没有道理
                Some(crate::ui::navpane::NavKind::TagItem { tag, .. }) => Some(tag),
                _ => None,
            });
        match tag {
            Some(index) => {
                self.drop_tag_row = None;
                self.add_paths_to_tag(index, paths);
            }
            None => {
                self.add_paths_to_nav_places(paths);
            }
        }
    }

    /// 把一批路径加进第 N 个标签。
    ///
    /// 和详情窗格的「加标签」走同一份 `cfg::tags`，所以 `touch()` 会让
    /// 详情窗格的归属缓存一起失效，列表里的标签点也会跟着刷新。
    pub fn add_paths_to_tag(&mut self, tag: usize, paths: &[PathBuf]) -> bool {
        let mut changed = false;
        for p in paths {
            if self.cfg.tags.add_item(tag, p.clone()) {
                changed = true;
            }
        }
        if changed {
            // 加完顺手展开：不展开的话界面上一点变化都没有，
            // 用户会以为没加上，再拖一次
            self.cfg.tags.expand(tag);
            self.cfg.tags.save(&self.cfg.root);
            self.rebuild_nav_rows();
            self.request_repaint();
        }
        changed
    }

    /// 从快捷访问区移除一项（不动磁盘上的文件）。
    pub fn remove_nav_place(&mut self, index: usize) {
        if self.cfg.nav_places.remove_at(index) {
            self.nav_press = None;
            self.nav_drag = None;
            self.nav_drop_y = None;
            self.cfg.save_all();
            self.rebuild_nav_rows();
            self.request_repaint();
        }
    }

    /// 把第 N 个快捷访问项提到最前。
    pub fn pin_nav_place(&mut self, index: usize) {
        if self.cfg.nav_places.pin_at(index) {
            self.cfg.save_all();
            self.rebuild_nav_rows();
            self.request_repaint();
        }
    }

    fn update_dpi(&mut self, hwnd: HWND) {
        use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
        let dpi = unsafe { GetDpiForWindow(hwnd) };
        let scale = if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 };
        if (scale - self.dpi_scale).abs() > 0.01 {
            self.dpi_scale = scale;
            self.fonts = FontCache::new(scale);
            self.recompute_layout();
        }
    }

    /// 处理外部拖入（OLE `IDropTarget` 已把路径捞出来，投消息通知到这里）。
    ///
    /// 落在收藏栏 → 加书签；落在导航窗格 → 看落点是不是标签行；
    /// 落在文件视图 → 交给视图那条路径。其它区域不接。
    pub fn handle_external_drop(&mut self) {
        let Some((zone, paths)) = crate::win::droptarget::take_external_drop() else {
            return;
        };
        match zone {
            crate::win::droptarget::DropZone::Favbar => self.add_paths_to_bookmarks(&paths),
            crate::win::droptarget::DropZone::NavPane => {
                let pt = crate::win::droptarget::take_drop_point();
                self.drop_paths_onto_nav(&paths, pt);
            }
            crate::win::droptarget::DropZone::View => {
                if let Some(pt) = crate::win::droptarget::take_drop_point() {
                    self.drop_paths_onto_view(paths, pt);
                }
            }
            crate::win::droptarget::DropZone::None => {}
        }
    }

    /// 文件视图里的落点解析与执行：拖到文件夹上移进去、拖到可执行文件上运行它、
    /// 拖到空白处移进当前目录。
    ///
    /// `pt` 是松手那一刻的客户区坐标。之所以强调「那一刻」：拖动期间视图可能
    /// 已经滚动、甚至换过标签，落点必须按松手时的几何算一次，不能沿用按下去时
    /// 的记录。
    pub fn drop_paths_onto_view(&mut self, paths: Vec<PathBuf>, pt: (i32, i32)) {
        if paths.is_empty() || self.active_tab >= self.tabs.len() {
            return;
        }
        let (x, y) = pt;
        let in_view = x >= self.layout.view.left
            && x < self.layout.view.right
            && y >= self.layout.view.top
            && y < self.layout.view.bottom;
        if !in_view {
            return;
        }

        // 松手点下面是什么？没有条目就是空白区
        let hit = crate::ui::interact::list_item_at(self, x, y);
        let (kind, target) = match hit.and_then(|i| {
            self.tabs
                .get(self.active_tab)
                .and_then(|t| t.entries.get(i))
                .map(|e| (e.is_dir(), e.full.clone(), e.ext()))
        }) {
            Some((true, dir, _)) => (DropKind::Folder, dir),
            Some((false, file, ext)) => {
                if is_executable(&ext) {
                    (DropKind::Program, file)
                } else {
                    // 落在普通文件上：资源管理器也是拒绝的，这里什么都不做，
                    // 免得用户以为文件被「放进」了那个文件里
                    return;
                }
            }
            None => {
                let t = &self.tabs[self.active_tab];
                if t.is_computer || t.is_recycle || t.path.as_os_str().is_empty() {
                    return;
                }
                (DropKind::Folder, t.path.clone())
            }
        };

        match kind {
            DropKind::Folder => {
                // 别把文件夹拖进它自己里面，也别拖进它现在所在的目录（那是空操作）
                let items: Vec<PathBuf> = paths
                    .into_iter()
                    .filter(|p| !crate::fs::same_path(p, &target))
                    .filter(|p| {
                        p.parent()
                            .map(|par| !crate::fs::same_path(par, &target))
                            .unwrap_or(true)
                    })
                    .collect();
                if items.is_empty() {
                    return;
                }
                self.send_op(OpWhat::Move {
                    items,
                    target: target.clone(),
                });
            }
            DropKind::Program => {
                // 把拖过来的东西当命令行参数交给它——和资源管理器一致
                match crate::fs::shell::run_with_program(&target, &paths) {
                    Ok(()) => {}
                    Err(e) => self.show_message(&e),
                }
            }
        }
        self.request_repaint();
    }

    /// 处理拖放（`WM_DROPFILES`）。
    unsafe fn on_drop_files(&mut self, hdrop: *mut core::ffi::c_void) {        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
        let mut first_dir: Option<PathBuf> = None;
        for i in 0..count {
            let len = DragQueryFileW(hdrop, i, std::ptr::null_mut(), 0);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; (len + 1) as usize];
            DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1);
            let p = PathBuf::from(from_wide(&buf));
            if p.is_dir() && first_dir.is_none() {
                first_dir = Some(p);
            }
        }
        DragFinish(hdrop);

        if let Some(dir) = first_dir {
            self.navigate_to(dir);
        }
    }

    /// 处理「鼠标离开」跟踪。
    unsafe fn ensure_mouse_leave_tracking(&mut self) {
        if self.tracking_mouse {
            return;
        }
        let mut tme: TRACKMOUSEEVENT = std::mem::zeroed();
        tme.cbSize = std::mem::size_of::<TRACKMOUSEEVENT>() as u32;
        tme.dwFlags = TME_LEAVE;
        tme.hwndTrack = self.hwnd;
        if TrackMouseEvent(&mut tme) != 0 {
            self.tracking_mouse = true;
        }
    }

    // ------------------------------------------------------------ 保存与退出

    fn save_state(&mut self) {
        // 窗口几何
        if hwnd_valid(self.hwnd) {
            unsafe {
                // 最小化时 GetWindowRect 拿到的是 (-32000,-32000, 160×28) 这类
                // 伪坐标；直接存会让下次启动把窗口建在屏幕外。最小化期间
                // 关闭（任务栏右键）就是这个路径。IsIconic 检查放最前面。
                if IsIconic(self.hwnd) != 0 {
                    // 几何不更新，沿用上一次的合法值；最大化标志照实存
                    self.cfg.settings.window_maximized = IsZoomed(self.hwnd) != 0;
                } else {
                    let mut r: RECT = std::mem::zeroed();
                    if GetWindowRect(self.hwnd, &mut r) != 0 {
                        let maxed = IsZoomed(self.hwnd) != 0;
                        if !maxed {
                            self.cfg.settings.window_x = r.left;
                            self.cfg.settings.window_y = r.top;
                            self.cfg.settings.window_w = r.right - r.left;
                            self.cfg.settings.window_h = r.bottom - r.top;
                        }
                        self.cfg.settings.window_maximized = maxed;
                        // 退出时窗口几何已经落定——不管是用户拖出来的、还是启动时
                        // 按屏幕推出来的。标记一下，下次启动直接沿用，
                        // 不再重新推导（否则用户调好的尺寸每次都会被覆盖）。
                        self.cfg.settings.window_geometry_set = true;
                    }
                }
            }
        }

        // 会话标签
        self.cfg.session.tabs.clear();
        for (i, t) in self.tabs.iter().enumerate() {
            let kind = if t.is_computer {
                crate::cfg::session::TabKind::Computer
            } else if t.is_recycle {
                crate::cfg::session::TabKind::Recycle
            } else {
                crate::cfg::session::TabKind::Dir
            };
            self.cfg.session.tabs.push(crate::cfg::session::TabRecord {
                path: t.path.clone(),
                view: t.view_mode,
                active: i == self.active_tab,
                kind,
            });
        }
        // 直接把当前标签的视图/排序状态写回设置。
        // 用字段直取而非 self.active()，避免与 self.cfg 的可变借用冲突。
        if self.active_tab < self.tabs.len() {
            let t = &self.tabs[self.active_tab];
            self.cfg.settings.view_mode = t.view_mode;
            self.cfg.settings.sort_key = t.sort.key;
            self.cfg.settings.sort_desc = t.sort.descending;
        }

        // 首次运行时补上默认书签
        if self.cfg.bookmarks.items.is_empty() {
            self.cfg.bookmarks.seed_defaults();
        }

        self.cfg.save_all();
    }
}

/// 拖放落点解析出来的目标类型。
enum DropKind {
    /// 文件夹：把拖来的东西移进去。
    Folder,
    /// 可执行文件：把拖来的东西当参数运行它。
    Program,
}

/// 这个后缀算不算「可执行文件」。
///
/// 只认 Shell 会直接执行的那几个：`.exe`/`.com` 是真正的映像，
/// `.bat`/`.cmd` 是脚本但双击同样会跑。`.lnk` 故意不算——
/// 往快捷方式上扔文件，用户的预期通常不是「打开快捷方式的目标」。
fn is_executable(ext: &str) -> bool {
    matches!(ext, "exe" | "com" | "bat" | "cmd")
}

/// 任务栏上显示的窗口标题。
///
/// 文件夹名短的时候（比如 `C`、`tmp`）任务栏按钮会被挤成一小块，
/// 所以不足 10 个字符就用空格居中补到 10；超过 20 个字符直接截断，
/// 后面的部分不显示——两头的空格由系统自己会吃掉，不影响观感。
fn taskbar_title(name: &str) -> String {
    const MIN: usize = 10;
    const MAX: usize = 20;
    let count = name.chars().count();
    if count > MAX {
        return name.chars().take(MAX).collect();
    }
    if count >= MIN {
        return name.to_string();
    }
    let pad = MIN - count;
    let left = pad / 2;
    let right = pad - left;
    let mut out = String::with_capacity(MIN);
    for _ in 0..left {
        out.push(' ');
    }
    out.push_str(name);
    for _ in 0..right {
        out.push(' ');
    }
    out
}

/// 「此电脑」视图。空路径即驱动器列表，与 `Tab::is_computer` 的判定一致。
///
/// 这是启动时的默认落点：资源管理器、原软件打开后都停在盘符列表，
/// 而不是直接钻进某个用户目录。
fn computer_view() -> PathBuf {
    PathBuf::from(COMPUTER_VIEW)
}

/// 用户主目录。
///
/// 不再作为启动默认目录（默认是「此电脑」，见 `computer_view`）。
/// 仅在两种需要**真实可枚举目录**的场合使用：
/// `/searchkey` 搜索起点、以及「复制当前标签」在没有活动标签时的兜底。
fn user_home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("C:\\"))
}


