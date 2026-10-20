//! 应用层：状态机、命令行解析、消息循环。
//!
//! 这里的结构与原软件的行为一一对应，命令行开关取自 PE 字符串
//! （`/OpenInNewWindow`、`/lowrun`、`/replace_explorer_all` 等）。

use  std::collections::HashSet;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetDC, GetMonitorInfoW, InvalidateRect, MonitorFromWindow, ReleaseDC,
    UpdateWindow, HFONT, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows_sys::Win32::System::Environment::GetCommandLineW;
// WM_MOUSELEAVE 在 windows-sys 里归到 UI::Controls，不在 WindowsAndMessaging。
use windows_sys::Win32::UI::Controls::WM_MOUSELEAVE;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{
    CommandLineToArgvW, DragAcceptFiles, DragFinish, DragQueryFileW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::cfg::settings::{self, StartMode, ThemeKind, ViewMode};
use crate::cfg::Config;
use crate::fs::entry::{Entry, EntryKind};
use crate::fs::enumerate::{self, ListOptions};
use crate::fs::icons::IconCache;
use crate::fs::icons::IconSize;
use crate::fs::{self, ops, shell, Sort};
use crate::ipc::PipeServer;
use crate::strings as S;
use crate::ui::{self, draw, interact, Layout};
use crate::win::paint::{BackBuffer, Canvas, FontCache, Palette};
use crate::win::{droptarget, from_wide, hwnd_valid, module_handle, to_wide, wnd};

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

// ------------------------------------------------------------------ 命令行

/// 命令行开关。全部来自原软件 PE 字符串中的实际参数。
#[derive(Debug, Clone, Default)]
pub struct CommandLine {
    /// 要打开的路径（裸参数）。
    pub path: Option<PathBuf>,
    /// `/OpenInNewWindow` —— 强制新窗口，不做单实例投递。
    pub new_window: bool,
    /// `/lowrun` —— 低优先级启动（原软件用于开机自启场景）。
    pub low_run: bool,
    /// `/autorun` —— 随系统启动。
    pub autorun: bool,
    /// `/replace_explorer_all` —— 接管资源管理器后退出。
    pub replace_explorer_all: bool,
    /// `/restore_explorer_all` —— 恢复资源管理器后退出。
    pub restore_explorer_all: bool,
    /// `/method=search /searchkey="..."` —— 直接发起搜索。
    pub search_key: Option<String>,
    /// `/curdir="..."` —— 启动目录。
    pub cur_dir: Option<PathBuf>,
    /// `/from=...` —— 来源标记（其余产品唤起）。
    pub from: Option<String>,
}

impl CommandLine {
    /// 解析真实命令行。使用 `GetCommandLineW` 以避免 ANSI 代码页损失。
    pub fn parse() -> CommandLine {
        let argv = raw_argv();
        let mut cl = CommandLine::default();

        let mut i = 1usize; // 跳过 exe 自身
        while i < argv.len() {
            let a = &argv[i];
            let lower = a.to_ascii_lowercase();

            if lower == "/openinnewwindow" {
                cl.new_window = true;
            } else if lower == "/lowrun" {
                cl.low_run = true;
            } else if lower == "/autorun" {
                cl.autorun = true;
            } else if lower == "/replace_explorer_all" {
                cl.replace_explorer_all = true;
            } else if lower == "/restore_explorer_all" {
                cl.restore_explorer_all = true;
            } else if let Some(v) = strip_prefix_ci(a, "/method=") {
                // 仅识别 search；其它方法暂未使用
                let _ = v;
            } else if let Some(v) = strip_prefix_ci(a, "/searchkey=") {
                cl.search_key = Some(unquote(v));
            } else if let Some(v) = strip_prefix_ci(a, "/curdir=") {
                cl.cur_dir = Some(PathBuf::from(unquote(v)));
            } else if let Some(v) = strip_prefix_ci(a, "/from=") {
                cl.from = Some(v.to_string());
            } else if a.starts_with('/') {
                // 未知开关：忽略（保持向前兼容）
            } else {
                // 裸路径参数
                let p = unquote(a);
                if !p.is_empty() {
                    cl.path = fs::resolve_path(&p);
                }
            }
            i += 1;
        }

        cl
    }
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    if s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&s[prefix.len()..])
    } else {
        None
    }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

/// 取原始命令行参数（UTF-16 → String）。
fn raw_argv() -> Vec<String> {
    unsafe {
        let cmd = GetCommandLineW();
        let mut argc = 0i32;
        let argv_ptr = CommandLineToArgvW(cmd, &mut argc);
        if argv_ptr.is_null() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(argc as usize);
        for i in 0..argc {
            let p = *argv_ptr.add(i as usize);
            out.push(from_wide_ptr_safe(p));
        }
        LocalFree(argv_ptr as *mut core::ffi::c_void);
        out
    }
}

unsafe fn from_wide_ptr_safe(p: *const u16) -> String {
    crate::win::from_wide_ptr(p)
}

#[allow(non_snake_case)] // 沿用 Win32 原名，便于与 MSDN 对照
unsafe fn LocalFree(p: *mut core::ffi::c_void) {
    let _ = windows_sys::Win32::Foundation::LocalFree(p);
}

// ------------------------------------------------------------------ 状态

/// 修饰键状态。
#[derive(Debug, Clone, Copy, Default)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// 搜索模式（对应搜索框左侧的三段切换）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// 快速搜索：在当前目录下按名称过滤
    Quick,
    /// 系统搜索：调用 Windows 搜索索引
    System,
    /// 网络搜索：交给默认浏览器
    Web,
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
    /// 正在搜索（同步执行，期间界面只刷一帧「正在搜索…」）
    Running,
    /// 搜索已结束，`hits` 是命中的条目数
    Done { hits: usize },
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
    pub scroll_y: i32,
    pub view_mode: ViewMode,
    pub loading: bool,
    pub error: Option<String>,
    /// 排序（每标签独立）。
    pub sort: Sort,
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
            scroll_y: 0,
            view_mode,
            loading: false,
            error: None,
            sort,
        }
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

    /// 快捷项拖动时的落点 y（画插入指示线用）。
    pub nav_drop_y: Option<i32>,
    /// 各分组自上而下的顺序与屏幕范围（拖动分隔线换位时算落点用）。
    pub nav_groups: Vec<crate::ui::navpane::GroupSpan>,
    /// 正在拖动某条分组分隔线（＝拖动它下面那一组）。
    pub nav_sep_drag: Option<crate::ui::navpane::SepDrag>,
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

    /// 设置面板是否打开。打开时它是模态的：先吃消息再往下传。
    pub settings_open: bool,

    /// 自绘模态对话框（提示 / 确认 / 输入）。
    ///
    /// 打开时它比设置面板更靠上层：命中测试、键盘、字符输入都先路由到这里。
    pub dialog: Option<crate::ui::dialog::Dialog>,

    pub fonts: FontCache,
    pub icons: IconCache,
    pub back_buffer: BackBuffer,

    pub hover: ui::Hit,
    pub pressed: ui::Hit,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub modifiers: Modifiers,
    /// 鼠标离开跟踪是否已安装。
    tracking_mouse: bool,

    // 地址栏编辑态
    pub address_editing: bool,
    /// 地址栏文本里的选区 `(anchor, caret)`，两个都是字符下标。
    ///
    /// `None` 表示没有选中——这样「有没有选区」和「选区是不是空的」不用分开判断。
    pub address_sel: Option<(usize, usize)>,
    pub address_text: String,
    pub address_caret: usize,

    // 搜索
    pub search_focused: bool,
    pub search_text: String,
    pub search_caret: usize,
    pub search_mode: SearchMode,
    /// 搜索进行到哪了（未在搜索时为 `None`）。
    pub search_state: Option<SearchState>,
    /// 搜索前的原始列表（退出搜索时恢复）。
    search_backup: Option<Vec<Entry>>,

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
            tooltip_ready: false,
            preview: None,
            nav_groups: Vec::new(),
            nav_sep_drag: None,
            nav_scroll: 0,
            nav_max_scroll: 0,
            nav_content_h: 0,
            scroll_drag: None,
            type_buffer: String::new(),
            type_time: None,
            settings_open: false,
            dialog: None,
            fonts: FontCache::new(scale),
            icons: IconCache::new(),
            back_buffer: BackBuffer::new(),
            hover: ui::Hit::None,
            pressed: ui::Hit::None,
            mouse_x: -1,
            mouse_y: -1,
            modifiers: Modifiers::default(),
            tracking_mouse: false,
            address_editing: false,
            address_sel: None,
            address_text: String::new(),
            address_caret: 0,
            search_focused: false,
            search_text: String::new(),
            search_caret: 0,
            search_mode: SearchMode::Quick,
            search_state: None,
            search_backup: None,
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
            let atom = wnd::register_class(CLASS_NAME, wnd_proc, std::ptr::null_mut(), arrow);
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
            wnd::apply_round_corners(hwnd);
            // 登记 OLE 放置目标：否则拖东西过来整块窗口都是禁止光标
            droptarget::register(hwnd);

            if centered {
                let (cx, cy) = wnd::center_on_monitor(hwnd, w, h);
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
            ShowWindow(hwnd, show);
            UpdateWindow(hwnd);

            // ---- 打开初始标签 ----
            self.open_initial_tabs();

            // ---- 消息循环 ----
            let code = wnd::message_loop();
            crate::win::init::shutdown_process();
            let _ = code;
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
        // 1) 命令行给出的路径优先
        if let Some(p) = self.cl.path.clone() {
            if p.is_dir() {
                self.new_tab(Some(p));
                return;
            }
            if let Some(parent) = p.parent() {
                let parent = parent.to_path_buf();
                self.new_tab(Some(parent));
                // 顺带选中该文件
                let name = p.file_name().map(|s| s.to_string_lossy().into_owned());
                if let Some(name) = name {
                    if let Some(idx) = self.tabs[self.active_tab]
                        .entries
                        .iter()
                        .position(|e| e.name.eq_ignore_ascii_case(&name))
                    {
                        self.tabs[self.active_tab].selected.clear();
                        self.tabs[self.active_tab].selected.insert(idx);
                        interact::scroll_into_view(self, idx);
                    }
                }
                return;
            }
        }

        // 2) `/searchkey` 直接进入搜索（原软件行为）
        if let Some(k) = self.cl.search_key.clone() {
            self.new_tab(Some(user_home()));
            self.search_text = k;
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
                    if rec.path.is_dir() {
                        let before = self.tabs.len();
                        self.new_tab(Some(rec.path.clone()));
                        if self.tabs.len() > before {
                            let t = &mut self.tabs[before];
                            t.view_mode = rec.view;
                            if rec.active {
                                self.active_tab = before;
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
    pub fn font_ui_bold(&mut self) -> HFONT {
        let px = self.font_px(draw::font::UI_BOLD);
        self.fonts.get(px, true)
    }
    pub fn font_title(&mut self) -> HFONT {
        let px = self.font_px(draw::font::TITLE);
        self.fonts.get(px, true)
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

    pub fn can_go_back(&self) -> bool {
        self.active().map(|t| t.history_pos > 0).unwrap_or(false)
    }

    pub fn can_go_forward(&self) -> bool {
        self.active()
            .map(|t| t.history_pos + 1 < t.history.len())
            .unwrap_or(false)
    }

    pub fn can_go_up(&self) -> bool {
        self.active()
            .map(|t| !t.is_computer && fs::parent_of(&t.path).is_some())
            .unwrap_or(false)
    }

    pub fn go_back(&mut self) {
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
            t.selected.clear();
            t.anchor = None;
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
                if let Some(tag) = self.cfg.tags.list.get_mut(index) {
                    tag.expanded = !tag.expanded;
                    save_tags = true;
                }
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
        }

        // 记录地址历史（原软件的 addr_history.ini）
        if !is_computer {
            self.cfg.history.record(&path.to_string_lossy());
        }

        self.reload_active_tab();
        self.sync_window_title();
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

        let is_computer = self.tabs[idx].is_computer;
        let path = self.tabs[idx].path.clone();

        // 枚举是同步的：目录通常在毫秒级完成；超大目录由 Shell 的
        // FIND_FIRST_EX_LARGE_FETCH 加速。异步化留作后续优化点。
        let is_recycle = self.tabs[idx].is_recycle;
        let (entries, error) = if is_recycle {
            (crate::fs::recycle::list(), None)
        } else if is_computer {
            (enumerate::list_computer(&opts).entries, None)
        } else {
            match enumerate::list_dir(&path, &opts) {
                Ok(listing) => (listing.entries, None),
                Err(e) => (Vec::new(), Some(e)),
            }
        };

        let t = &mut self.tabs[idx];
        t.entries = entries;
        t.error = error;
        t.loading = false;
        t.selected.clear();
        t.anchor = None;
        t.scroll_y = 0;
        self.search_backup = None;
        // 换了目录，上一次搜索的状态和结果都不再成立
        self.search_state = None;
        self.search_text.clear();
        self.search_caret = 0;

        // 标题跟着活动标签走。放在这里而不是只在 load_into_active 里：
        // 新建标签、刷新、切标签都走本函数，漏一处标题就会停在软件名上。
        self.sync_window_title();
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
        self.reload_active_tab();
        self.request_repaint();
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
        if index == self.active_tab {
            self.end_search_on_tab_switch();
        }
        self.tabs.remove(index);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len().saturating_sub(1);
        } else if self.active_tab > index {
            self.active_tab -= 1;
        }
        self.request_repaint();
    }

    pub fn set_active_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            if index != self.active_tab {
                self.end_search_on_tab_switch();
            }
            self.active_tab = index;
            self.sync_window_title();
            self.request_repaint();
        }
    }

    /// 离开当前标签前收起搜索。
    ///
    /// 搜索状态和「搜索前的列表」都只有一份、属于当前标签：留着它们，
    /// 新标签的状态栏就会报上一个标签的匹配数。恢复列表要趁还没换
    /// `active_tab`，否则会写进新标签里。
    fn end_search_on_tab_switch(&mut self) {
        self.restore_search_backup();
        self.search_state = None;
        self.search_text.clear();
        self.search_caret = 0;
    }

    pub fn next_tab(&mut self, delta: i32) {
        if self.tabs.len() < 2 {
            return;
        }
        self.end_search_on_tab_switch();
        let n = self.tabs.len() as i32;
        let cur = self.active_tab as i32;
        let next = ((cur + delta) % n + n) % n;
        self.active_tab = next as usize;
        self.sync_window_title();
        self.request_repaint();
    }

    // ------------------------------------------------------------ 选择与打开

    pub fn entry_path(&self, index: usize) -> Option<PathBuf> {
        self.active()
            .and_then(|t| t.entries.get(index))
            .map(|e| e.full.clone())
    }

    pub fn select_entry(&mut self, index: usize, ctrl: bool, shift: bool) {
        {
            let Some(t) = self.active_mut() else { return };
            if index >= t.entries.len() {
                return;
            }

            if shift {
                let anchor = t.anchor.unwrap_or(index);
                let (lo, hi) = if anchor <= index {
                    (anchor, index)
                } else {
                    (index, anchor)
                };
                t.selected.clear();
                for i in lo..=hi {
                    t.selected.insert(i);
                }
            } else if ctrl {
                if t.selected.contains(&index) {
                    t.selected.remove(&index);
                } else {
                    t.selected.insert(index);
                }
                t.anchor = Some(index);
            } else {
                t.selected.clear();
                t.selected.insert(index);
                t.anchor = Some(index);
            }
        }
        self.request_repaint();
    }

    pub fn select_all(&mut self) {
        let Some(t) = self.active_mut() else { return };
        t.selected = (0..t.entries.len()).collect();
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
        }
        self.request_repaint();
    }

    pub fn move_selection(&mut self, delta: i32) {
        // 先把需要的信息取出来，避免 &Tab 与后续 &mut self 冲突
        let (n, cur) = {
            let Some(t) = self.active() else { return };
            let n = t.entries.len();
            let cur = t.selected.iter().next().copied().unwrap_or(0) as i32;
            (n, cur)
        };
        if n == 0 {
            return;
        }
        let next = (cur + delta).clamp(0, n as i32 - 1) as usize;
        self.select_entry(next, false, false);
        interact::scroll_into_view(self, next);
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
    /// 实现策略：小于 1 MiB 的文本类文件在状态栏滚动显示前若干行；
    /// 图片交给系统照片查看器。这里保持轻量，不引入图像解码依赖。
    pub fn quick_preview_selection(&mut self) {
        let Some(&i) = self.active().and_then(|t| t.selected.iter().next()) else {
            return;
        };
        let Some(entry) = self.active().and_then(|t| t.entries.get(i)).cloned() else {
            return;
        };
        if entry.is_dir() {
            return;
        }

        const TEXT_MAX: u64 = 1024 * 1024;
        let ext = entry.ext();
        let is_text = matches!(
            ext.as_str(),
            "txt" | "md" | "json" | "toml" | "yaml" | "yml" | "ini" | "log" | "rs" | "c" | "h"
                | "cpp" | "hpp" | "cs" | "py" | "js" | "ts" | "html" | "css" | "xml" | "bat"
                | "cmd" | "ps1" | "sh"
        );

        if is_text && entry.size <= TEXT_MAX {
            match std::fs::read_to_string(&entry.full) {
                Ok(text) => {
                    let preview: String = text.lines().take(12).collect::<Vec<_>>().join("\n");
                    self.show_message(&preview);
                }
                Err(e) => self.show_message(&format!("无法预览：{e}")),
            }
        } else {
            let _ = shell::open_default(self.hwnd, &entry.full);
        }
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

    // ------------------------------------------------------------ 文件操作

    fn selected_paths(&self) -> Vec<PathBuf> {
        let Some(t) = self.active() else {
            return Vec::new();
        };
        let mut v: Vec<usize> = t.selected.iter().copied().collect();
        v.sort_unstable();
        v.into_iter()
            .filter_map(|i| t.entries.get(i).map(|e| e.full.clone()))
            .collect()
    }

    /// 点击详细信息表头第 `col` 列：切换成按这一列排序。
    ///
    /// 规则和资源管理器一致——点同一列翻转升降序，点新的一列从升序开始。
    pub fn sort_by_column(&mut self, col: usize) {
        use crate::fs::enumerate::SortKey;
        let key = match col {
            0 => SortKey::Name,
            1 => SortKey::Modified,
            2 => SortKey::Kind,
            _ => SortKey::Size,
        };
        let idx = self.active_tab;
        if idx >= self.tabs.len() {
            return;
        }
        let same = self.tabs[idx].sort.key == key;
        self.tabs[idx].sort = crate::fs::enumerate::Sort {
            key,
            descending: if same { !self.tabs[idx].sort.descending } else { false },
        };
        self.cfg.settings.sort_key = key;
        self.cfg.settings.sort_desc = self.tabs[idx].sort.descending;
        self.reload_active_tab();
        self.request_repaint();
    }

    pub fn delete_selection(&mut self, permanent: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if self.in_recycle_bin() {
            // 在回收站里再删一次＝彻底删除，连元数据一起清掉
            let mut err = None;
            for p in &paths {
                if let Err(e) = crate::fs::recycle::purge(p) {
                    err = Some(e);
                }
            }
            if let Some(e) = err {
                self.show_message(&e);
            }
            self.refresh_active_tab();
            return;
        }
        let res = if permanent {
            ops::delete_permanent(self.hwnd, &paths)
        } else {
            ops::delete_to_recycle(self.hwnd, &paths)
        };
        self.handle_op_result(res);
    }

    pub fn copy_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if !shell::set_clipboard_files(&paths) {
            self.show_message("无法写入剪贴板");
        }
    }

    pub fn cut_selection(&mut self) {
        // 剪切与复制共用剪贴板；粘贴时按同样方式移动。
        // 为区分语义，把「剪切」状态记在应用内。
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if shell::set_clipboard_files(&paths) {
            self.cut_pending = true;
        } else {
            self.show_message("无法写入剪贴板");
        }
    }

    pub fn paste_into_active_tab(&mut self) {
        let sources = shell::get_clipboard_files();
        if sources.is_empty() {
            return;
        }
        let Some(target) = self.active().map(|t| t.path.clone()) else {
            return;
        };
        if target.as_os_str().is_empty() {
            return;
        }

        let res = if self.cut_pending {
            ops::move_to(self.hwnd, &sources, &target)
        } else {
            ops::copy_to(self.hwnd, &sources, &target)
        };
        if self.cut_pending && res == ops::OpResult::Ok {
            self.cut_pending = false;
        }
        self.handle_op_result(res);
    }

    pub fn begin_rename_selection(&mut self) {
        let Some(&i) = self.active().and_then(|t| t.selected.iter().next()) else {
            return;
        };
        let Some(name) = self.active().and_then(|t| t.entries.get(i)).map(|e| e.name.clone())
        else {
            return;
        };
        // 进入内联重命名：复用地址栏的编辑机制，锚定到该项
        self.rename_target = Some(i);
        self.address_editing = false;
        self.search_focused = false;
        self.inline_edit_text = name.clone();
        self.inline_edit_caret = name.chars().count();
        self.request_repaint();
    }

    fn handle_op_result(&mut self, res: ops::OpResult) {
        match res {
            ops::OpResult::Ok => self.refresh_active_tab(),
            ops::OpResult::Cancelled => {}
            ops::OpResult::Failed(msg) => {
                self.show_message(&msg);
                self.refresh_active_tab();
            }
        }
    }

    pub fn toolbar_action(&mut self, index: usize) {
        match index {
            0 => {
                // 新建文件
                if let Some(dir) = self.active().map(|t| t.path.clone()) {
                    match ops::create_file(&dir, "新建文本文档", "txt") {
                        Ok(p) => {
                            self.refresh_active_tab();
                            self.select_by_path(&p);
                        }
                        Err(e) => self.show_message(&e),
                    }
                }
            }
            1 => {
                // 新建文件夹
                if let Some(dir) = self.active().map(|t| t.path.clone()) {
                    match ops::create_folder(&dir) {
                        Ok(p) => {
                            self.refresh_active_tab();
                            self.select_by_path(&p);
                        }
                        Err(e) => self.show_message(&e),
                    }
                }
            }
            2 => self.cut_selection(),
            3 => self.copy_selection(),
            4 => self.paste_into_active_tab(),
            5 => self.delete_selection(false),
            6 => self.begin_rename_selection(),
            7 => self.cycle_view_mode(),
            8 => {
                self.search_focused = true;
                self.request_repaint();
            }
            9 => {
                // 快速访问：回到此电脑
                self.navigate_to(PathBuf::new());
            }
            _ => {}
        }
    }

    fn select_by_path(&mut self, path: &std::path::Path) {
        let Some(idx) = self
            .active()
            .and_then(|t| t.entries.iter().position(|e| e.full == path))
        else {
            return;
        };
        self.select_entry(idx, false, false);
        interact::scroll_into_view(self, idx);
    }

    pub fn cycle_view_mode(&mut self) {
        let Some(t) = self.active_mut() else { return };
        let all = ViewMode::ALL;
        let pos = all.iter().position(|&v| v == t.view_mode).unwrap_or(0);
        t.view_mode = all[(pos + 1) % all.len()];
        t.scroll_y = 0;
        self.request_repaint();
    }

    /// Ctrl+滚轮：在视图档之间升降。
    ///
    /// 档位顺序从「最大信息量」到「最大图标」：详细信息排第一个，
    /// 往上是它、往下依次是大图标→中→小图标。`dir > 0` 表示向上滚（更详细）。
    pub fn zoom_view(&mut self, dir: i32) {
        const STEPS: [ViewMode; 5] = [
            ViewMode::Detail,
            ViewMode::LargeEx,
            ViewMode::Large,
            ViewMode::Medium,
            ViewMode::Small,
        ];
        let Some(t) = self.active_mut() else { return };
        let cur = STEPS.iter().position(|&v| v == t.view_mode).unwrap_or(2);
        // 数组是从大到小排的，所以放大 = 索引减
        let next = if dir > 0 {
            cur.saturating_sub(1)
        } else {
            (cur + 1).min(STEPS.len() - 1)
        };
        if next == cur {
            return;
        }
        t.view_mode = STEPS[next];
        t.scroll_y = 0;
        self.cfg.settings.view_mode = STEPS[next];
        self.request_repaint();
    }

    pub fn set_view_mode(&mut self, mode: ViewMode) {
        if let Some(t) = self.active_mut() {
            t.view_mode = mode;
            t.scroll_y = 0;
        }
        self.cfg.settings.view_mode = mode;
        self.request_repaint();
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

    /// 打开任意路径：目录就进标签，文件交给默认关联。
    ///
    /// `new_tab` 只对目录有意义——文件本来就是在外部程序里打开的。
    /// 标签条目（`cfg.tags`）既可能是目录也可能是文件，导航窗格与详情窗格
    /// 点击时都走这里，省得每处各写一遍「是目录还是文件」的判断。
    pub fn open_any_path(&mut self, path: PathBuf, new_tab: bool) {
        if path.is_dir() {
            self.open_dir(path, new_tab);
        } else if let Err(e) = shell::open_default(self.hwnd, &path) {
            self.show_message(&e);
        }
    }

    pub fn add_current_to_bookmarks(&mut self) {
        let path = match self.active() {
            Some(t) if !t.is_computer => t.path.clone(),
            _ => return,
        };
        self.cfg.bookmarks.add_or_update(path, None);
        self.request_repaint();
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
        self.request_repaint();
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
    /// 变了才重新解码/读取。之所以放在绘制里而不是选中逻辑里，
    /// 是因为选中会从十几个地方改（点击、框选、键盘、刷新、拖放），
    /// 漏掉一处就会出现「选中换了但预览还是上一张」。
    pub fn ensure_preview(&mut self, path: Option<PathBuf>, ext: &str, size: u64) {
        match path {
            None => self.preview = None,
            Some(p) => {
                if self
                    .preview
                    .as_ref()
                    .map(|pr| pr.path == p)
                    .unwrap_or(false)
                {
                    return;
                }
                self.preview = Some(crate::ui::preview::build(&p, ext, size));
            }
        }
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
        self.search_focused = false;
        if self.search_backup.is_some() && self.search_text.is_empty() {
            self.restore_search_backup();
        }
        // 失焦后「按回车搜索」就没人执行了：留着这句提示只会误导
        if matches!(self.search_state, Some(SearchState::Pending)) {
            self.search_state = None;
        }
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
                    let res = ops::rename(self.hwnd, &p, &new_name);
                    self.handle_op_result(res);
                }
            }
            return;
        }

        if self.address_editing {
            let text = self.address_text.clone();
            self.address_editing = false;
            self.address_sel = None;
            let lowered = text.trim().to_ascii_lowercase();
            if lowered == "回收站" || lowered == "shell:recyclebinfolder" || lowered == RECYCLE_PATH {
                self.open_recycle_bin();
                return;
            }
            if let Some(p) = fs::resolve_path(&text) {
                if p.is_dir() {
                    self.navigate_to(p);
                } else if p.is_file() {
                    if let Some(parent) = p.parent() {
                        self.navigate_to(parent.to_path_buf());
                        let fname = p.file_name().map(|s| s.to_string_lossy().into_owned());
                        if let Some(f) = fname {
                            if let Some(idx) = self
                                .active()
                                .and_then(|t| t.entries.iter().position(|e| e.name == f))
                            {
                                self.select_entry(idx, false, false);
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

    /// 按回车（或搜索框失焦）时触发搜索。
    ///
    /// 先把「正在搜索…」这一帧真正刷到屏幕上再开始干活：搜索是同步的，
    /// 递归扫描最长要 400 ms，而 `request_repaint` 只是 `InvalidateRect`，
    /// 不主动 `UpdateWindow` 的话这一帧会一直排在消息队列里，等搜索结束才
    /// 画得出来——那时显示的已经是结果，用户根本看不到「正在搜索」。
    pub fn begin_search(&mut self) {
        if self.search_text.trim().is_empty() {
            self.restore_search_backup();
            return;
        }
        self.search_state = Some(SearchState::Running);
        self.request_repaint();
        if hwnd_valid(self.hwnd) {
            unsafe {
                UpdateWindow(self.hwnd);
            }
        }
        self.run_search();
        self.request_repaint();
    }

    /// 执行搜索。
    ///
    /// 命中来源按优先级回退：
    ///   1. Windows 搜索索引（`SystemIndex`，与资源管理器搜索框同源）；
    ///   2. 索引不可用 / 目录未被索引 → 当前目录子树递归扫描；
    ///   3. 仍未命中 → 当前已加载条目里的名称包含匹配。
    ///
    /// 这样在关闭了索引服务的机器上依然有资源管理器同等的行为。
    fn run_search(&mut self) {
        /// 结果上限。搜索是「找得到就行」，不做全量列举。
        const SEARCH_LIMIT: usize = 500;
        /// 递归降级的时间预算，超时就用已扫到的部分。
        const SCAN_BUDGET_MS: u32 = 400;

        let key = self.search_text.trim().to_lowercase();
        let idx = self.active_tab;
        if idx >= self.tabs.len() {
            self.search_state = None;
            return;
        }

        if key.is_empty() {
            self.restore_search_backup();
            return;
        }

        if self.search_backup.is_none() {
            self.search_backup = Some(self.tabs[idx].entries.clone());
        }

        let current = self.tabs[idx].path.clone();

        // 1) 系统索引
        let mut hits: Vec<Entry> = Vec::new();
        if let Some(scope) = current.to_str() {
            if let Ok(found) = crate::win::search::search_index(scope, &key, SEARCH_LIMIT) {
                for h in found {
                    if let Some(e) = crate::fs::enumerate::entry_from_path(std::path::Path::new(&h.path))
                    {
                        hits.push(e);
                    }
                }
            }
        }

        // 2) 子树递归
        if hits.is_empty() {
            hits = crate::fs::enumerate::scan_subtree(
                &current,
                &key,
                SEARCH_LIMIT,
                SCAN_BUDGET_MS,
            );
        }

        // 3) 已加载条目兜底（「此电脑」等虚拟视图没有真实目录可扫）
        if hits.is_empty() {
            if let Some(backup) = self.search_backup.as_ref() {
                hits = backup
                    .iter()
                    .filter(|e| e.name.to_lowercase().contains(&key))
                    .cloned()
                    .collect();
            }
        }

        let found = hits.len();
        let t = &mut self.tabs[idx];
        t.entries = hits;
        t.selected.clear();
        t.anchor = None;
        t.scroll_y = 0;
        self.search_state = Some(SearchState::Done { hits: found });
        self.request_repaint();
    }

    fn restore_search_backup(&mut self) {
        self.search_state = None;
        if let Some(backup) = self.search_backup.take() {
            let idx = self.active_tab;
            if idx < self.tabs.len() {
                self.tabs[idx].entries = backup;
                self.tabs[idx].selected.clear();
                self.tabs[idx].scroll_y = 0;
            }
        }
        self.request_repaint();
    }

    // ------------------------------------------------- 编辑框（地址/搜索/重命名共用）

    /// 当前正在编辑的文本缓冲（只读）。
    fn edit_buffer(&self) -> Option<&String> {
        if self.rename_target.is_some() {
            Some(&self.inline_edit_text)
        } else if self.address_editing {
            Some(&self.address_text)
        } else if self.search_focused {
            Some(&self.search_text)
        } else {
            None
        }
    }

    /// 当前正在编辑的文本缓冲（可写）。
    fn edit_buffer_mut(&mut self) -> Option<&mut String> {
        if self.rename_target.is_some() {
            Some(&mut self.inline_edit_text)
        } else if self.address_editing {
            Some(&mut self.address_text)
        } else if self.search_focused {
            Some(&mut self.search_text)
        } else {
            None
        }
    }

    fn edit_buffer_len(&self) -> usize {
        self.edit_buffer().map(|b| b.chars().count()).unwrap_or(0)
    }

    /// 光标位置（可写）。
    ///
    /// 注意：不能与 `edit_buffer_mut` 的返回值同时存活——两者都借 `&mut self`。
    fn caret_mut(&mut self) -> &mut usize {
        if self.rename_target.is_some() {
            &mut self.inline_edit_caret
        } else if self.address_editing {
            &mut self.address_caret
        } else {
            &mut self.search_caret
        }
    }

    fn caret(&self) -> usize {
        if self.rename_target.is_some() {
            self.inline_edit_caret
        } else if self.address_editing {
            self.address_caret
        } else {
            self.search_caret
        }
    }

    /// 编辑结束后统一收尾。
    ///
    /// 搜索框不再逐字符搜：一次搜索要跑系统索引加最多 400ms 的递归扫描，
    /// 而且全在 UI 线程上同步执行，跟着每个按键跑一遍会让英文逐字母输入
    /// 一顿一顿的。这里只把状态标成「等回车」，真正的搜索放到 Enter 或失焦时做。
    fn after_edit(&mut self) {
        if self.search_focused {
            if self.search_text.trim().is_empty() {
                self.restore_search_backup();
            } else {
                self.search_state = Some(SearchState::Pending);
            }
        }
        self.request_repaint();
    }

    /// 当前选区（已归一化成 `(起, 止)`），没有选区返回 `None`。
    fn selection(&self) -> Option<(usize, usize)> {
        let (a, b) = self.address_sel?;
        if a == b {
            None
        } else {
            Some((a.min(b), a.max(b)))
        }
    }

    /// 把选区删掉，返回删除后的光标位置；没有选区返回 `None`。
    fn edit_delete_selection(&mut self) -> Option<usize> {
        let (s0, s1) = self.selection()?;
        let len = self.edit_buffer_len();
        let s0 = s0.min(len);
        let s1 = s1.min(len);
        if let Some(buf) = self.edit_buffer_mut() {
            let chars: Vec<char> = buf.chars().collect();
            let mut out: String = chars[..s0].iter().collect();
            out.extend(chars[s1..].iter());
            *buf = out;
        }
        self.address_sel = None;
        Some(s0)
    }

    pub fn edit_insert(&mut self, ch: char) {
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
        }
        let caret = self.caret();
        let new_caret = match self.edit_buffer_mut() {
            Some(buf) => {
                let chars: Vec<char> = buf.chars().collect();
                let pos = caret.min(chars.len());
                let mut s: String = chars[..pos].iter().collect();
                s.push(ch);
                s.extend(chars[pos..].iter());
                *buf = s;
                pos + 1
            }
            None => caret,
        };
        *self.caret_mut() = new_caret;
        self.address_sel = None;
        self.after_edit();
    }

    pub fn edit_backspace(&mut self) {
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
            self.after_edit();
            return;
        }
        let caret = self.caret();
        if caret == 0 {
            return;
        }
        let new_caret = match self.edit_buffer_mut() {
            Some(buf) => {
                let chars: Vec<char> = buf.chars().collect();
                let pos = caret.min(chars.len());
                if pos == 0 {
                    return;
                }
                let mut s: String = chars[..pos - 1].iter().collect();
                s.extend(chars[pos..].iter());
                *buf = s;
                pos - 1
            }
            None => caret,
        };
        *self.caret_mut() = new_caret;
        self.after_edit();
    }

    pub fn edit_delete(&mut self) {
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
            self.after_edit();
            return;
        }
        let caret = self.caret();
        let changed = match self.edit_buffer_mut() {
            Some(buf) => {
                let chars: Vec<char> = buf.chars().collect();
                let pos = caret.min(chars.len());
                if pos >= chars.len() {
                    false
                } else {
                    let mut s: String = chars[..pos].iter().collect();
                    s.extend(chars[pos + 1..].iter());
                    *buf = s;
                    true
                }
            }
            None => false,
        };
        if changed {
            self.after_edit();
        }
    }

    pub fn edit_move_caret(&mut self, delta: i32) {
        let len = self.edit_buffer_len();
        self.address_sel = None;
        let c = self.caret_mut();
        *c = (*c as i32 + delta).clamp(0, len as i32) as usize;
        self.request_repaint();
    }

    pub fn edit_caret_home(&mut self) {
        self.address_sel = None;
        *self.caret_mut() = 0;
        self.request_repaint();
    }

    pub fn edit_caret_end(&mut self) {
        self.address_sel = None;
        let len = self.edit_buffer_len();
        *self.caret_mut() = len;
        self.request_repaint();
    }

    /// 全选（Ctrl+A）。
    pub fn edit_select_all(&mut self) {
        let len = self.edit_buffer_len();
        self.address_sel = Some((0, len));
        *self.caret_mut() = len;
        self.request_repaint();
    }

    /// 带 Shift 的光标移动：扩展现有选区。
    pub fn edit_move_caret_shift(&mut self, delta: i32) {
        let len = self.edit_buffer_len();
        let caret = self.caret();
        let anchor = self.address_sel.map(|(a, _)| a).unwrap_or(caret);
        let next = (caret as i32 + delta).clamp(0, len as i32) as usize;
        *self.caret_mut() = next;
        self.address_sel = Some((anchor, next));
        self.request_repaint();
    }

    /// 按词左右跳（Ctrl+← / Ctrl+→）。`extend` 为真时扩选。
    pub fn edit_move_word(&mut self, dir: i32, extend: bool) {
        let text = self.edit_buffer().cloned().unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let caret = self.caret().min(len);
        let anchor = self.address_sel.map(|(a, _)| a).unwrap_or(caret);
        let mut i = caret as i32;
        if dir < 0 {
            // 先跳过左边空白，再跳过一整段非空白
            while i > 0 && chars[(i - 1) as usize].is_whitespace() {
                i -= 1;
            }
            while i > 0 && !chars[(i - 1) as usize].is_whitespace() {
                i -= 1;
            }
        } else {
            while (i as usize) < len && chars[i as usize].is_whitespace() {
                i += 1;
            }
            while (i as usize) < len && !chars[i as usize].is_whitespace() {
                i += 1;
            }
        }
        let next = i.clamp(0, len as i32) as usize;
        *self.caret_mut() = next;
        self.address_sel = if extend { Some((anchor, next)) } else { None };
        self.request_repaint();
    }

    /// 光标移到行首/行尾，`extend` 为真时扩选。
    pub fn edit_caret_edge(&mut self, end: bool, extend: bool) {
        let len = self.edit_buffer_len();
        let caret = self.caret();
        let anchor = self.address_sel.map(|(a, _)| a).unwrap_or(caret);
        let next = if end { len } else { 0 };
        *self.caret_mut() = next;
        self.address_sel = if extend { Some((anchor, next)) } else { None };
        self.request_repaint();
    }

    /// Ctrl+Backspace：删掉光标左边的一个词。
    pub fn edit_backspace_word(&mut self) {
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
            self.after_edit();
            return;
        }
        let text = self.edit_buffer().cloned().unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let caret = self.caret().min(chars.len());
        let mut i = caret;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.address_sel = Some((i, caret));
        *self.caret_mut() = caret;
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
        }
        self.after_edit();
    }

    /// 复制当前选区（Ctrl+C）。
    pub fn edit_copy(&mut self) {
        let Some((s0, s1)) = self.selection() else {
            return;
        };
        let text = self.edit_buffer().cloned().unwrap_or_default();
        let picked: String = text.chars().skip(s0).take(s1 - s0).collect();
        if !picked.is_empty() && !crate::fs::shell::set_clipboard_text(&picked) {
            self.show_message("无法写入剪贴板");
        }
    }

    /// 剪切当前选区（Ctrl+X）。
    pub fn edit_cut(&mut self) {
        self.edit_copy();
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
            self.after_edit();
        }
    }

    /// 粘贴（Ctrl+V），有选区时先替换掉。
    pub fn edit_paste(&mut self) {
        let Some(text) = crate::fs::shell::get_clipboard_text() else {
            return;
        };
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if text.is_empty() {
            return;
        }
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
        }
        let caret = self.caret();
        let new_caret = match self.edit_buffer_mut() {
            Some(buf) => {
                let chars: Vec<char> = buf.chars().collect();
                let pos = caret.min(chars.len());
                let mut s: String = chars[..pos].iter().collect();
                s.push_str(&text);
                s.extend(chars[pos..].iter());
                let add = text.chars().count();
                *buf = s;
                pos + add
            }
            None => caret,
        };
        *self.caret_mut() = new_caret;
        self.address_sel = None;
        self.after_edit();
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
        }
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
        let Some(tag) = self.cfg.tags.list.get_mut(index) else {
            return;
        };
        let before = tag.items.len();
        tag.items.retain(|i| !crate::fs::same_path(&i.path, &path));
        if tag.items.len() != before {
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

    /// 新开一个实例（`/OpenInNewWindow` 语义）。
    pub fn spawn_new_window(&self) {
        let exe = std::env::current_exe().unwrap_or_default();
        let arg = self
            .active()
            .map(|t| t.path.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut cmd = std::process::Command::new(exe);
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
                ID_SHORTCUT => match crate::shell_integration::create_desktop_shortcut() {
                    Ok(lnk) => self.dialog_info(&format!(
                        "已在桌面创建快捷方式：\n{}\n\n双击即可启动 DreamFolder。",
                        lnk.display()
                    )),
                    Err(e) => self.dialog_info(&format!("创建桌面快捷方式失败：\n{e}")),
                },
                ID_TAKEOVER => self.toggle_takeover(),
                ID_SETTINGS => self.show_settings_dialog(),
                ID_FEEDBACK => {
                    if let Err(e) = crate::fs::shell::open_url(S::FEEDBACK_URL) {
                        self.dialog_info(&format!(
                            "无法打开浏览器：{e}\n\n仓库地址：{}",
                            S::FEEDBACK_URL
                        ));
                    }
                }
                ID_ABOUT => {
                    self.dialog_info(&format!(
                        "{}\n\n版本 {}\nRust + Win32（windows-sys 0.59）\n\n仓库：{}",
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

    /// 文件列表的右键上下文菜单。
    ///
    /// 每一项都直接落到 `fs::shell` / `fs::ops` 已有的系统调用上；
    /// 选中条目与空白处两套菜单，对应资源管理器的常规语义。
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
        crate::win::shellmenu::show(self.hwnd, &paths, dir.as_deref());
        // 菜单里可能执行了「删除」「重命名」「新建」这类改动文件系统的命令，
        // 关掉之后必须重新列一遍——以前只重绘，列表还停在旧内容上。
        self.reload_active_tab();
        self.request_repaint();
    }

    pub fn show_context_menu(&mut self) {        // 先把选中路径取成 owned 数据，后面才能自由地对 self 取可变借用
        let paths = self.selected_paths();
        let has_selection = !paths.is_empty();
        let single_exe = paths.len() == 1
            && paths[0]
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    e.eq_ignore_ascii_case("exe")
                        || e.eq_ignore_ascii_case("bat")
                        || e.eq_ignore_ascii_case("cmd")
                        || e.eq_ignore_ascii_case("com")
                })
                .unwrap_or(false);

        const ID_OPEN: usize = 1;
        const ID_OPEN_WITH: usize = 2;
        const ID_RUN_AS: usize = 3;
        const ID_PRINT: usize = 4;
        const ID_REVEAL: usize = 5;
        const ID_CUT: usize = 6;
        const ID_COPY: usize = 7;
        const ID_DELETE: usize = 8;
        const ID_RENAME: usize = 9;
        const ID_COPY_PATH: usize = 10;
        const ID_PROPERTIES: usize = 11;
        const ID_PASTE: usize = 12;
        const ID_NEW_FOLDER: usize = 13;
        const ID_REFRESH: usize = 14;
        const ID_FOLDER_OPTIONS: usize = 15;

        let cmd = unsafe {
            let hmenu = CreatePopupMenu();
            if hmenu.is_null() {
                return;
            }
            let add = |id: usize, text: &str| {
                let w = to_wide(text);
                AppendMenuW(hmenu, MF_STRING, id, w.as_ptr());
            };
            let sep = || AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());

            if has_selection {
                add(ID_OPEN, "打开");
                add(ID_OPEN_WITH, "打开方式...");
                if single_exe {
                    add(ID_RUN_AS, "以管理员身份运行");
                }
                add(ID_PRINT, "打印");
                add(ID_REVEAL, "在资源管理器中显示");
                sep();
                add(ID_CUT, "剪切");
                add(ID_COPY, "复制");
                add(ID_DELETE, "删除");
                add(ID_RENAME, "重命名");
                add(ID_COPY_PATH, "复制路径");
                sep();
                add(ID_PROPERTIES, "属性");
            } else {
                add(ID_PASTE, "粘贴");
                add(ID_NEW_FOLDER, "新建文件夹");
                sep();
                add(ID_REFRESH, "刷新");
                sep();
                add(ID_FOLDER_OPTIONS, "文件夹选项...");
            }

            let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            // 菜单外壳由 uxtheme 画，弹之前把配色对齐到应用内主题
            crate::win::init::apply_menu_theme();
            let c = TrackPopupMenu(
                hmenu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                std::ptr::null(),
            ) as usize;
            DestroyMenu(hmenu);
            c
        };

        // 选中条目才有效的单选类命令，统一取第一条
        let first = || paths.first().cloned();

        match cmd {
            ID_OPEN => {
                if let Some(&i) = self.active().and_then(|t| t.selected.iter().next()) {
                    self.open_entry(i);
                }
            }
            ID_OPEN_WITH => {
                if let Some(p) = first() {
                    if let Err(e) = shell::open_with_dialog(self.hwnd, &p) {
                        self.show_message(&e);
                    }
                }
            }
            ID_RUN_AS => {
                if let Some(p) = first() {
                    if let Err(e) = shell::run_as_admin(self.hwnd, &p) {
                        self.show_message(&e);
                    }
                }
            }
            ID_PRINT => {
                if let Some(p) = first() {
                    if let Err(e) = shell::print(self.hwnd, &p) {
                        self.show_message(&e);
                    }
                }
            }
            ID_REVEAL => {
                if let Some(p) = first() {
                    if let Err(e) = shell::open_in_system_explorer(&p) {
                        self.show_message(&e);
                    }
                }
            }
            ID_CUT => self.cut_selection(),
            ID_COPY => self.copy_selection(),
            ID_DELETE => self.delete_selection(false),
            ID_RENAME => self.begin_rename_selection(),
            ID_COPY_PATH => {
                let text = paths
                    .iter()
                    .map(|p| p.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("\r\n");
                if !shell::set_clipboard_text(&text) {
                    self.show_message("无法写入剪贴板");
                }
            }
            ID_PROPERTIES => {
                if let Some(&i) = self.active().and_then(|t| t.selected.iter().next()) {
                    self.show_properties_entry(i);
                }
            }
            ID_PASTE => self.paste_into_active_tab(),
            ID_NEW_FOLDER => {
                if let Some(dir) = self.active().map(|t| t.path.clone()) {
                    match ops::create_folder(&dir) {
                        Ok(p) => {
                            self.refresh_active_tab();
                            self.select_by_path(&p);
                        }
                        Err(e) => self.show_message(&e),
                    }
                }
            }
            ID_REFRESH => self.refresh_active_tab(),
            ID_FOLDER_OPTIONS => {
                if let Err(e) = shell::open_folder_options() {
                    self.show_message(&e);
                }
            }
            _ => {}
        }
    }

    fn toggle_takeover(&mut self) {
        use crate::shell_integration::{self, TakeoverState};
        use crate::ui::dialog::DialogIntent;

        match shell_integration::state() {
            TakeoverState::Active => {
                // 已接管 → 提供恢复。确认后由 `dialog_accept` 落动作
                self.dialog_confirm(
                    S::MENU_TAKE_OVER_EXPLORER,
                    "当前已接管系统文件管理。\n\n选择「确定」恢复系统默认，选择「取消」保持现状。",
                    DialogIntent::RestoreExplorer,
                );
            }
            _ => match shell_integration::take_over() {
                Ok(scopes) => {
                    self.cfg.settings.replace_sysexplorer = true;
                    self.cfg.save_all();
                    self.dialog_info(&shell_integration::takeover_report(&scopes));
                }
                Err(e) => self.dialog_info(&format!(
                    "{e}\n\n提示：接管系统文件夹需要在管理员权限下运行本程序。"
                )),
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
        let client = wnd::client_rect(self.hwnd);
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
    /// 落在收藏栏 → 加书签；落在导航窗格 → 加进快捷访问。其它区域不接。
    pub fn handle_external_drop(&mut self) {
        let Some((zone, paths)) = crate::win::droptarget::take_external_drop() else {
            return;
        };
        match zone {
            crate::win::droptarget::DropZone::Favbar => self.add_paths_to_bookmarks(&paths),
            crate::win::droptarget::DropZone::NavPane => {
                self.add_paths_to_nav_places(&paths);
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
                let res = crate::fs::ops::move_to(self.hwnd, &items, &target);
                self.handle_op_result(res);
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

        // 会话标签
        self.cfg.session.tabs.clear();
        for (i, t) in self.tabs.iter().enumerate() {
            self.cfg.session.tabs.push(crate::cfg::session::TabRecord {
                path: t.path.clone(),
                view: t.view_mode,
                active: i == self.active_tab,
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

fn yesno(b: bool) -> &'static str {
    if b {
        "是"
    } else {
        "否"
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

// ------------------------------------------------------------------ 窗口过程

/// 窗口过程。所有消息都转发到 `Application` 上的方法。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // WM_NCCREATE 时把 Application 指针写入窗口
    if msg == WM_NCCREATE {
        let cs = lparam as *const CREATESTRUCTW;
        if !cs.is_null() {
            let p = (*cs).lpCreateParams as *mut Application;
            wnd::set_user_data(hwnd, p);
            if !p.is_null() {
                (*p).hwnd = hwnd;
            }
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let app = wnd::user_data::<Application>(hwnd);
    if app.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let app = &mut *app;

    match msg {
        WM_CREATE => {
            app.recompute_layout();
            0
        }
        WM_SIZE => {
            app.recompute_layout();
            app.request_repaint();
            0
        }
        WM_DPICHANGED => {
            app.update_dpi(hwnd);
            // 使用系统建议的窗口矩形
            let sug = lparam as *const RECT;
            if !sug.is_null() {
                let r = *sug;
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            0
        }
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let hdc = BeginPaint(hwnd, &mut ps);
            if !hdc.is_null() {
                let client = wnd::client_rect(hwnd);
                let w = client.right - client.left;
                let h = client.bottom - client.top;
                if w > 0 && h > 0 {
                    let mem = app.back_buffer.acquire(hdc, w, h);
                    if !mem.is_null() {
                        draw::paint(app, mem);
                        app.back_buffer.blit_to(hdc, 0, 0, w, h);
                    }
                }
            }
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1, // 背景由双缓冲覆盖，禁止系统擦除
        WM_MOUSEMOVE => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            app.ensure_mouse_leave_tracking();
            interact::on_mouse_move(app, x, y);
            0
        }
        WM_MOUSELEAVE => {
            app.tracking_mouse = false;
            interact::on_mouse_leave(app);
            0
        }
        WM_LBUTTONDOWN => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            SetCapture(hwnd);
            interact::on_lbutton_down(app, x, y);
            0
        }
        WM_LBUTTONUP => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            ReleaseCapture();
            interact::on_lbutton_up(app, x, y);
            0
        }
        // 自己处理右键抬起并返回 0，DefWindowProc 就不会再合成 WM_CONTEXTMENU，
        // 从而避免菜单被弹出两次。
        WM_RBUTTONUP => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            interact::on_rbutton_up(app, x, y);
            0
        }
        WM_LBUTTONDBLCLK => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            interact::on_lbutton_dblclk(app, x, y);
            0
        }
        WM_MBUTTONUP => {
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            interact::on_mbutton_up(app, x, y);
            0
        }
        // 鼠标侧键：XBUTTON1 后退、XBUTTON2 前进。键号在 wParam 高字里。
        WM_XBUTTONUP => {
            match ((wparam >> 16) & 0xFFFF) as u16 {
                1 => app.go_back(),
                2 => app.go_forward(),
                _ => {}
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = (wparam >> 16) as i16 as i32;
            interact::on_mousewheel(app, delta);
            0
        }
        WM_MOUSEHWHEEL => {
            let delta = (wparam >> 16) as i16 as i32;
            interact::on_hwheel(app, delta);
            0
        }
        WM_KEYDOWN => {
            // Alt 状态来自 GetKeyState
            use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_MENU};
            app.modifiers.alt = (GetKeyState(VK_MENU as i32) as u16 & 0x8000) != 0;
            interact::update_modifiers(app);
            if interact::on_key_down(app, wparam) {
                return 0;
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_CHAR => {
            let ch = wparam as u32;
            if let Some(c) = char::from_u32(ch) {
                interact::on_char(app, c);
            }
            0
        }
        WM_SETFOCUS => {
            app.active_tab_focused = true;
            app.request_repaint();
            0
        }
        WM_KILLFOCUS => {
            app.active_tab_focused = false;
            app.request_repaint();
            0
        }
        WM_DROPFILES => {
            app.on_drop_files(wparam as *mut core::ffi::c_void);
            0
        }
        crate::win::droptarget::WM_DREAMFOLDER_DROP => {
            app.handle_external_drop();
            0
        }
        WM_TIMER => {
            if wparam == TIMER_CARET {
                // 还有编辑态就催一帧重绘让插入符翻面；都结束了就自己停
                let dialog_caret = app
                    .dialog
                    .as_ref()
                    .map(|d| d.has_input())
                    .unwrap_or(false);
                if app.search_focused
                    || app.address_editing
                    || app.rename_target.is_some()
                    || dialog_caret
                {
                    app.request_repaint();
                } else {
                    KillTimer(hwnd, TIMER_CARET);
                }
                return 0;
            }
            if wparam == TIMER_HOVER {
                // 一次性：到点就停，只在指针仍停在该按钮上时才亮出提示条
                KillTimer(hwnd, TIMER_HOVER);
                if matches!(app.hover, ui::Hit::ToolbarButton(_)) {
                    app.tooltip_ready = true;
                    app.request_repaint();
                }
                return 0;
            }
            if wparam == TIMER_PIPE {
                // 轮询单实例投递。先把报文取出来再操作 app：
                // 管道借用活在 &mut app.pipe 上，直接在里面调 app 的方法会冲突。
                let incoming = match &mut app.pipe {
                    Some(pipe) => {
                        if pipe.try_accept() {
                            pipe.read_message()
                        } else {
                            None
                        }
                    }
                    None => None,
                };
                if let Some(msg) = incoming {
                    let (path, new_tab) = crate::ipc::parse_message(&msg);
                    if let Some(p) = path {
                        if new_tab {
                            app.new_tab(Some(p));
                        } else {
                            app.navigate_to(p);
                        }
                    }
                    // 把窗口带到前台
                    SetForegroundWindow(hwnd);
                }
            }
            0
        }
        WM_GETMINMAXINFO => {
            let mmi = lparam as *mut MINMAXINFO;
            if !mmi.is_null() {
                let s = app.dpi_scale;
                (*mmi).ptMinTrackSize.x =
                    (ui::metrics::MIN_WINDOW_W as f32 * s).round() as i32;
                (*mmi).ptMinTrackSize.y =
                    (ui::metrics::MIN_WINDOW_H as f32 * s).round() as i32;
            }
            0
        }
        WM_NCCALCSIZE => {
            // 无边框窗口：客户区必须等于整个窗口矩形。
            //
            // 返回值是关键——返回 1 才表示「lparam 里的 rgrc[0] 就是客户区」；
            // 返回 0 会让系统按默认规则**再扣一次** WS_THICKFRAME 的调整边框，
            // 于是客户区比窗口尺寸小一圈（左右各 8px 左右），布局在这个错误的
            // 尺寸上计算，整页内容随之偏移、挤压。
            if wparam == 0 {
                // lParam 是 RECT*：无边框窗口基本不走这条，交给系统默认
                return 0;
            }
            let params = lparam as *mut NCCALCSIZE_PARAMS;
            if params.is_null() {
                return 0;
            }
            if IsZoomed(hwnd) != 0 {
                // 最大化时客户区取显示器**工作区**，否则会盖住任务栏
                (*params).rgrc[0] = monitor_info(hwnd);
            }
            // 非最大化：保持系统传入的窗口矩形，即客户区铺满整个窗口
            1
        }
        WM_NCHITTEST => {
            // 允许拖动调整大小
            let x = lo_word(lparam);
            let y = hi_word(lparam);
            let mut r: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut r);
            let border = (6.0 * app.dpi_scale).round() as i32;

            let left = x < r.left + border;
            let right = x >= r.right - border;
            let top = y < r.top + border;
            let bottom = y >= r.bottom - border;

            let hit = if top && left {
                HTTOPLEFT
            } else if top && right {
                HTTOPRIGHT
            } else if bottom && left {
                HTBOTTOMLEFT
            } else if bottom && right {
                HTBOTTOMRIGHT
            } else if left {
                HTLEFT
            } else if right {
                HTRIGHT
            } else if top {
                HTTOP
            } else if bottom {
                HTBOTTOM
            } else {
                HTCLIENT
            };
            return hit as LRESULT;
        }
        WM_SETCURSOR => {
            // lParam 低字是 WM_NCHITTEST 的结果。
            //
            // 这里必须把每个 HT 都显式映射成光标：窗口类注册时用的是
            // IDC_ARROW，而 DefWindowProc 只在「系统非客户区」里替我们挑
            // 缩放光标；我们是无边框窗口、边框命中全由 WM_NCHITTEST 自己
            // 决定，返回 0 让系统兜底的结果就是一直显示箭头。
            let code = lo_word(lparam);
            let cursor = match code {
                x if x == HTLEFT as i32 || x == HTRIGHT as i32 => IDC_SIZEWE,
                x if x == HTTOP as i32 || x == HTBOTTOM as i32 => IDC_SIZENS,
                x if x == HTTOPLEFT as i32 || x == HTBOTTOMRIGHT as i32 => IDC_SIZENWSE,
                x if x == HTTOPRIGHT as i32 || x == HTBOTTOMLEFT as i32 => IDC_SIZENESW,
                x if x == HTCLIENT as i32 => {
                    if app.resizing_nav
                        || app.resizing_detail
                        || app.drag_col.is_some()
                        || app.hover == crate::ui::Hit::NavSplitter
                        || app.hover == crate::ui::Hit::DetailSplitter
                        || matches!(app.hover, crate::ui::Hit::DetailCol(_))
                    {
                        IDC_SIZEWE
                    } else if app.address_editing || app.search_focused {
                        IDC_IBEAM
                    } else {
                        IDC_ARROW
                    }
                }
                _ => IDC_ARROW,
            };
            SetCursor(LoadCursorW(std::ptr::null_mut(), cursor));
            1
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            app.recompute_layout();
            app.request_repaint();
            0
        }
        NAV_MSG => {
            // 只加载最终目标，中间被覆盖掉的目标直接丢弃
            if let Some(target) = app.pending_nav.take() {
                app.load_into_active(target, false);
            }
            0
        }
        WM_CLOSE => {
            app.request_close();
            0
        }
        WM_DESTROY => {
            app.save_state();
            if app.pipe.is_some() {
                KillTimer(hwnd, TIMER_PIPE);
            }
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 首次启动的窗口尺寸：按主显示器工作区推导。
///
/// 旧版本把 1000×640 硬编码在设置默认值里——那在 1080p 及以上的屏幕上偏小，
/// 且完全不随屏幕变化。这里改成按工作区算，并夹在最小尺寸与工作区之间。
///
/// 注意：本函数在**窗口创建前**调用，此时没有 hwnd 可用，因此走
/// `SystemParametersInfoW(SPI_GETWORKAREA)` 而不是 `MonitorFromWindow`。
/// 它返回主显示器工作区，多屏下可能与鼠标所在屏不同，但只会影响首次启动的
/// 一个初始位置，用户拖一次窗口后就被保存的几何取代。
fn default_window_size() -> (i32, i32) {
    /// 取不到工作区时的兜底（与旧默认值一致，至少不会崩）。
    const FALLBACK: (i32, i32) = (1000, 640);

    let mut work: RECT = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            &mut work as *mut RECT as *mut core::ffi::c_void,
            0,
        )
    };
    if ok == 0 {
        return FALLBACK;
    }

    let ww = work.right - work.left;
    let wh = work.bottom - work.top;
    if ww <= 0 || wh <= 0 {
        return FALLBACK;
    }

    // 宽取工作区的 5/8、高取 3/4：在 1920×1040 的工作区上得到 1200×780，
    // 与资源管理器在 1080p 下打开时的观感接近。
    let w = (ww * 5 / 8).clamp(ui::metrics::MIN_WINDOW_W, ww);
    let h = (wh * 3 / 4).clamp(ui::metrics::MIN_WINDOW_H, wh);
    (w, h)
}

unsafe fn monitor_info(hwnd: HWND) -> RECT {
    let mut mi: MONITORINFO = std::mem::zeroed();
    mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if GetMonitorInfoW(mon, &mut mi) != 0 {
        mi.rcWork
    } else {
        std::mem::zeroed()
    }
}

/// 取 lParam 低 16 位，**按有符号**解释（等价于 `GET_X_LPARAM`）。
///
/// 鼠标消息里的客户区坐标可以是负数：按下时 `SetCapture` 会持续把消息送进
/// 窗口，用户把光标拖到客户区左上角外面，x/y 就是负值。按无符号读会变成
/// 65xxx，框选的终点于是被夹到视图的**右下角**，整块选择框直接翻到对面
/// （拖动翻页、拖动滚动条也一样会被这一下带偏）。
fn lo_word(lparam: LPARAM) -> i32 {
    (lparam & 0xFFFF) as i16 as i32
}

/// 取 lParam 高 16 位，按有符号解释（等价于 `GET_Y_LPARAM`）。理由同 [`lo_word`]。
fn hi_word(lparam: LPARAM) -> i32 {
    ((lparam >> 16) & 0xFFFF) as i16 as i32
}

