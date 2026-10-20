//! 应用层：状态机、命令行解析、消息循环。
//!
//! 这里的结构与原软件的行为一一对应，命令行开关取自 PE 字符串
//! （`/OpenInNewWindow`、`/lowrun`、`/replace_explorer_all` 等）。

use std::collections::HashSet;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::HFONT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::Shell::{
    CommandLineToArgvW, DragAcceptFiles, DragFinish, DragQueryFileW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::cfg::settings::{Sort, SortKey, StartMode, ThemeKind, ViewMode};
use crate::cfg::Config;
use crate::fs::entry::{Entry, EntryKind};
use crate::fs::enumerate::{self, ListOptions};
use crate::fs::icons::IconCache;
use crate::fs::icons::IconSize;
use crate::fs::{self, ops, shell};
use crate::ipc::PipeServer;
use crate::strings as S;
use crate::ui::{self, draw, interact, Layout};
use crate::win::paint::{BackBuffer, FontCache, Palette};
use crate::win::{from_wide, hwnd_valid, module_handle, to_wide, to_wide_raw, wnd};

/// 窗口类名。
const CLASS_NAME: &str = "360Folder_Frame";

/// 定时器 ID：轮询命名管道。
const TIMER_PIPE: usize = 1;
/// 管道轮询间隔（毫秒）。空闲时开销可忽略。
const PIPE_POLL_MS: u32 = 400;

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

/// 一个标签页。
pub struct Tab {
    /// 当前目录。空表示「此电脑」（驱动器列表）。
    pub path: PathBuf,
    /// 是否为「此电脑」视图。
    pub is_computer: bool,
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
            path,
            is_computer,
            title,
            history: Vec::with_capacity(16),
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
    pub address_text: String,
    pub address_caret: usize,

    // 搜索
    pub search_focused: bool,
    pub search_text: String,
    pub search_caret: usize,
    pub search_mode: SearchMode,
    /// 搜索前的原始列表（退出搜索时恢复）。
    search_backup: Option<Vec<Entry>>,

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
        let cfg = Config::load();
        let theme = cfg.settings.theme;
        let scale = 1.0f32;

        Application {
            hwnd: std::ptr::null_mut(),
            tabs: Vec::new(),
            active_tab: 0,
            active_tab_focused: true,
            cfg,
            theme,
            layout: Layout::default(),
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
            address_text: String::new(),
            address_caret: 0,
            search_focused: false,
            search_text: String::new(),
            search_caret: 0,
            search_mode: SearchMode::Quick,
            search_backup: None,
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
            let atom = wnd::register_class(CLASS_NAME, wnd_proc, std::ptr::null_mut(), std::ptr::null_mut());
            if atom == 0 {
                // 类已注册（同进程二次创建）也继续
            }

            let self_ptr: *mut Application = self;

            let (w, h) = (self.cfg.settings.window_w, self.cfg.settings.window_h);
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
            self.apply_quick_search();
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
                        self.new_tab(Some(user_home()));
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
                    self.new_tab(Some(user_home()));
                }
            }
            StartMode::NewTab => {
                self.new_tab(Some(user_home()));
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

    pub fn font_tab(&mut self) -> HFONT {
        self.fonts.get(draw::font::TAB, false)
    }
    pub fn font_ui(&mut self) -> HFONT {
        self.fonts.get(draw::font::UI, false)
    }
    pub fn font_small(&mut self) -> HFONT {
        self.fonts.get(draw::font::SMALL, false)
    }
    pub fn font_status(&mut self) -> HFONT {
        self.fonts.get(draw::font::STATUS, false)
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

    fn active(&self) -> Option<&Tab> {
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
        self.load_into_active(target, false);
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
        self.load_into_active(target, false);
    }

    pub fn go_up(&mut self) {
        let target = {
            let Some(t) = self.active() else { return };
            if t.is_computer {
                return;
            }
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
    pub fn navigate_to(&mut self, path: PathBuf) {
        self.load_into_active(path, true);
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
        let (entries, error) = if is_computer {
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
            self.active_tab = index;
            self.request_repaint();
        }
    }

    pub fn next_tab(&mut self, delta: i32) {
        if self.tabs.len() < 2 {
            return;
        }
        let n = self.tabs.len() as i32;
        let cur = self.active_tab as i32;
        let next = ((cur + delta) % n + n) % n;
        self.active_tab = next as usize;
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
        let active = self.active_tab;
        if active >= self.tabs.len() {
            return;
        }
        let l = &self.layout;
        let s = |v: i32| (v as f32 * l.scale).round();
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
        let max_scroll = (content_h - view_h).max(0);
        let t = &mut self.tabs[active];
        t.scroll_y = (t.scroll_y + delta).clamp(0, max_scroll);
        self.request_repaint();
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

    pub fn delete_selection(&mut self, permanent: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() {
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
                    if let Ok(p) = ops::create_file(&dir, "新建文本文档", "txt") {
                        self.refresh_active_tab();
                        self.select_by_path(&p);
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
                // 知识：打开帮助
                let _ = shell::open_url("https://www.360.cn/");
            }
            10 => {
                // 截图：调用同目录的截图工具（若存在）
                self.launch_sibling_tool("360ScreenCapture.exe");
            }
            11 => {
                // 快速访问：回到此电脑
                self.navigate_to(PathBuf::new());
            }
            _ => {}
        }
    }

    /// 启动同目录下的兄弟工具（原软件 Utils 目录里的 360ScreenCapture.exe 等）。
    fn launch_sibling_tool(&mut self, name: &str) {
        let path = crate::cfg::paths::exe_dir().join(name);
        if path.is_file() {
            let _ = shell::open_default(self.hwnd, &path);
        } else {
            self.show_message(&format!("未找到 {name}"));
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

    pub fn set_view_mode(&mut self, mode: ViewMode) {
        if let Some(t) = self.active_mut() {
            t.view_mode = mode;
            t.scroll_y = 0;
        }
        self.cfg.settings.view_mode = mode;
        self.request_repaint();
    }

    // ------------------------------------------------------------ 收藏栏

    pub fn activate_favorite(&mut self, index: usize) {
        let Some(item) = self.cfg.bookmarks.items.get(index).cloned() else {
            return;
        };
        if item.is_folder {
            // 目录型书签：暂以位置为准展开为其第一个子项
            if let Some(first) = item.children.first() {
                self.open_favorite_target(&first.path);
            }
            return;
        }
        self.open_favorite_target(&item.path);
    }

    fn open_favorite_target(&mut self, path: &std::path::Path) {
        if path.is_dir() {
            if self.cfg.settings.bookmark_open_in_newtab {
                self.new_tab(Some(path.to_path_buf()));
            } else {
                self.navigate_to(path.to_path_buf());
            }
        } else if let Err(e) = shell::open_default(self.hwnd, path) {
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

    // ------------------------------------------------------------ 地址栏与搜索

    pub fn path_segments(&self) -> Vec<String> {
        let Some(t) = self.active() else {
            return vec![S::STATUS_DRIVES.to_string()];
        };
        if t.is_computer {
            return vec![S::STATUS_DRIVES.to_string()];
        }

        let text = t.path.to_string_lossy().into_owned();
        let mut out = vec![S::STATUS_DRIVES.to_string()];

        // 拆分：驱动器段保留 "C:"；UNC 保留 "\\server"
        let mut rest = text.as_str();
        if let Some(after) = rest.strip_prefix(r"\\") {
            let mut it = after.splitn(2, '\\');
            if let Some(server) = it.next() {
                out.push(format!(r"\\{server}"));
            }
            rest = it.next().unwrap_or("");
        } else if rest.len() >= 2 && rest.as_bytes()[1] == b':' {
            out.push(rest[..2].to_string());
            rest = rest[2..].trim_start_matches(['\\', '/']);
        }

        for seg in rest.split(['\\', '/']) {
            if !seg.is_empty() {
                out.push(seg.to_string());
            }
        }
        out
    }

    pub fn begin_address_edit(&mut self) {
        // 先取出文本，避免 &Tab 借用与随后的 &mut self 赋值同时存在
        let (is_computer, text) = {
            let Some(t) = self.active() else { return };
            (t.is_computer, t.path.to_string_lossy().into_owned())
        };
        self.address_text = if is_computer { String::new() } else { text };
        self.address_caret = self.address_text.chars().count();
        self.address_editing = true;
        self.search_focused = false;
        self.request_repaint();
    }

    pub fn focus_search(&mut self) {
        self.search_focused = true;
        self.address_editing = false;
        self.request_repaint();
    }

    pub fn cancel_edit(&mut self) {
        if self.rename_target.is_some() {
            self.rename_target = None;
            self.inline_edit_text.clear();
        }
        self.address_editing = false;
        self.search_focused = false;
        if self.search_backup.is_some() && self.search_text.is_empty() {
            self.restore_search_backup();
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
            self.apply_quick_search();
            return;
        }

        self.request_repaint();
    }

    /// 执行快速搜索（在当前目录内按名称过滤）。
    pub fn apply_quick_search(&mut self) {
        let key = self.search_text.trim().to_lowercase();
        let idx = self.active_tab;
        if idx >= self.tabs.len() {
            return;
        }

        if key.is_empty() {
            self.restore_search_backup();
            return;
        }

        if self.search_backup.is_none() {
            self.search_backup = Some(self.tabs[idx].entries.clone());
        }

        // 块作用域确保 backup 的不可变借用在此之前结束
        let filtered: Vec<Entry> = {
            let Some(backup) = self.search_backup.as_ref() else {
                return;
            };
            backup
                .iter()
                .filter(|e| e.name.to_lowercase().contains(&key))
                .cloned()
                .collect()
        };

        let t = &mut self.tabs[idx];
        t.entries = filtered;
        t.selected.clear();
        t.anchor = None;
        t.scroll_y = 0;
        self.request_repaint();
    }

    fn restore_search_backup(&mut self) {
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

    /// 编辑结束后统一收尾（搜索框需要重新过滤）。
    fn after_edit(&mut self) {
        if self.search_focused {
            self.apply_quick_search();
        }
        self.request_repaint();
    }

    pub fn edit_insert(&mut self, ch: char) {
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
        self.after_edit();
    }

    pub fn edit_backspace(&mut self) {
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
        let c = self.caret_mut();
        *c = (*c as i32 + delta).clamp(0, len as i32) as usize;
        self.request_repaint();
    }

    pub fn edit_caret_home(&mut self) {
        *self.caret_mut() = 0;
        self.request_repaint();
    }

    pub fn edit_caret_end(&mut self) {
        let len = self.edit_buffer_len();
        *self.caret_mut() = len;
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

    /// 请求关闭窗口。多个标签时按设置给出提示（对应 `close_alter`）。
    pub fn request_close(&mut self) {
        if self.cfg.settings.close_alter && self.tabs.len() > 1 && !self.closing {
            match self.ask_confirm(S::ALERT_CLOSE_TITLE, S::ALERT_CLOSE_BODY) {
                ConfirmResult::Yes => {}
                ConfirmResult::No => return,
            }
        }
        self.closing = true;
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }

    /// 弹出确认框。
    fn ask_confirm(&mut self, title: &str, body: &str) -> ConfirmResult {
        const MB_YESNO: u32 = 0x0000_0004;
        const MB_ICONQUESTION: u32 = 0x0000_0020;
        const IDYES: i32 = 6;

        let t = to_wide(title);
        let b = to_wide(body);
        let r = unsafe {
            MessageBoxW(
                self.hwnd,
                b.as_ptr(),
                t.as_ptr(),
                MB_YESNO | MB_ICONQUESTION,
            )
        };
        if r == IDYES {
            ConfirmResult::Yes
        } else {
            ConfirmResult::No
        }
    }

    /// 简单消息框（用于错误提示）。
    pub fn show_message(&self, text: &str) {
        const MB_OK: u32 = 0x0000_0000;
        const MB_ICONINFORMATION: u32 = 0x0000_0040;
        let t = to_wide(S::APP_NAME);
        let b = to_wide(text);
        unsafe {
            MessageBoxW(
                self.hwnd,
                b.as_ptr(),
                t.as_ptr(),
                MB_OK | MB_ICONINFORMATION,
            );
        }
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
            const ID_ABOUT: usize = 4;
            const ID_EXIT: usize = 5;

            add(ID_SHORTCUT, S::MENU_ADD_DESKTOP_SHORTCUT);
            add(ID_TAKEOVER, S::MENU_TAKE_OVER_EXPLORER);
            AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
            add(ID_SETTINGS, S::MENU_SETTINGS);
            add(ID_ABOUT, S::MENU_FEEDBACK);
            AppendMenuW(hmenu, MF_SEPARATOR, 0, std::ptr::null());
            add(ID_EXIT, S::MENU_EXIT);

            let mut pt = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
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
                    if let Err(e) = crate::shell_integration::create_desktop_shortcut() {
                        self.show_message(&e);
                    }
                }
                ID_TAKEOVER => self.toggle_takeover(),
                ID_SETTINGS => self.show_settings_dialog(),
                ID_ABOUT => {
                    let _ = shell::open_url("https://www.360.cn/");
                }
                ID_EXIT => self.request_close(),
                _ => {}
            }
        }
    }

    fn toggle_takeover(&mut self) {
        use crate::shell_integration::{self, TakeoverState};
        match shell_integration::state() {
            TakeoverState::Active => {
                // 已接管 → 提供恢复
                let r = unsafe {
                    let t = to_wide(S::MENU_TAKE_OVER_EXPLORER);
                    let b = to_wide("当前已接管系统文件管理。\n\n选择「是」恢复系统默认，选择「否」保持现状。");
                    MessageBoxW(
                        self.hwnd,
                        b.as_ptr(),
                        t.as_ptr(),
                        0x0000_0004 | 0x0000_0020,
                    )
                };
                if r == 6 {
                    if let Err(e) = shell_integration::restore() {
                        self.show_message(&e);
                    } else {
                        self.cfg.settings.replace_sysexplorer = false;
                    }
                }
            }
            _ => match shell_integration::take_over() {
                Ok(()) => {
                    self.cfg.settings.replace_sysexplorer = true;
                    self.show_message("已接管。在文件夹上右键即可用 360文件夹 打开。");
                }
                Err(e) => self.show_message(&format!(
                    "{e}\n\n提示：接管系统文件夹需要在管理员权限下运行本程序。"
                )),
            },
        }
    }

    /// 设置对话框。用系统对话框承载，字段与 DlgSetting.xml 一一对应。
    fn show_settings_dialog(&mut self) {
        // 简版实现：逐项询问关键开关。完整的多页设置面板
        // 需要自绘控件树，属于后续迭代范围（见 README 的「已完成/待办」）。
        let msg = format!(
            "当前设置：\n\n\
             · 启动方式：{}\n\
             · 显示隐藏项目：{}\n\
             · 显示扩展名：{}\n\
             · 显示工具栏：{}\n\
             · 显示收藏栏：{}\n\
             · 快速预览：{}\n\
             · 接管系统文件管理：{}\n\
             · 主题：{}\n\n\
             选择「是」切换主题，选择「否」取消。",
            match self.cfg.settings.start_mode {
                StartMode::NewTab => "打开新标签页",
                StartMode::RestoreLast => "恢复上次标签页",
                StartMode::Custom => "打开指定标签页",
            },
            yesno(self.cfg.settings.show_hidden),
            yesno(self.cfg.settings.show_extension),
            yesno(self.cfg.settings.toolbar_visible),
            yesno(self.cfg.settings.bookmark_visible),
            yesno(self.cfg.settings.enable_quickpreview),
            yesno(self.cfg.settings.replace_sysexplorer),
            match self.theme {
                ThemeKind::Light => "浅色",
                ThemeKind::Dark => "深色",
            },
        );
        let t = to_wide(S::SET_TITLE);
        let b = to_wide(&msg);
        let r = unsafe { MessageBoxW(self.hwnd, b.as_ptr(), t.as_ptr(), 0x0000_0004 | 0x0000_0020) };
        if r == 6 {
            self.theme = match self.theme {
                ThemeKind::Light => ThemeKind::Dark,
                ThemeKind::Dark => ThemeKind::Light,
            };
            self.cfg.settings.theme = self.theme;
            self.request_repaint();
        }
    }

    // ------------------------------------------------------------ 布局与命中

    fn recompute_layout(&mut self) {
        let client = wnd::client_rect(self.hwnd);
        self.layout = Layout::compute(
            client,
            self.dpi_scale,
            self.cfg.settings.bookmark_visible,
            self.cfg.settings.toolbar_visible,
            self.cfg.settings.show_nav_pane,
            self.cfg.settings.show_detail_pane,
        );
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

    /// 处理拖放（`WM_DROPFILES`）。
    unsafe fn on_drop_files(&mut self, hdrop: *mut core::ffi::c_void) {
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, std::ptr::null_mut(), 0);
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

enum ConfirmResult {
    Yes,
    No,
}

fn yesno(b: bool) -> &'static str {
    if b {
        "是"
    } else {
        "否"
    }
}

/// 用户主目录。
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
        WM_TIMER => {
            if wparam == TIMER_PIPE {
                // 轮询单实例投递
                if let Some(pipe) = &app.pipe {
                    if pipe.try_accept() {
                        if let Some(msg) = pipe.read_message() {
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
            // 无边框：客户区覆盖整个窗口（保留最大化时的边框修正）
            if wparam != 0 && IsZoomed(hwnd) != 0 {
                let params = lparam as *mut NCCALCSIZE_PARAMS;
                if !params.is_null() {
                    let mi = monitor_info(hwnd);
                    (*params).rgrc[0] = mi;
                }
                return 0;
            }
            0
        }
        WM_NCHITTEST => {
            // 允许拖动调整大小
            let x = lo_word_signed(lparam);
            let y = hi_word_signed(lparam);
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
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            app.recompute_layout();
            app.request_repaint();
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

fn lo_word(lparam: LPARAM) -> i32 {
    (lparam & 0xFFFF) as i32
}

fn hi_word(lparam: LPARAM) -> i32 {
    ((lparam >> 16) & 0xFFFF) as i32
}

fn lo_word_signed(lparam: LPARAM) -> i32 {
    (lparam & 0xFFFF) as i16 as i32
}

fn hi_word_signed(lparam: LPARAM) -> i32 {
    ((lparam >> 16) & 0xFFFF) as i16 as i32
}
