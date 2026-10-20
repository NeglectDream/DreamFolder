//! 应用设置。字段与原软件 `DlgSetting.xml` 中的控件 name 一一对应，
//! 便于与原版本对照排查。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 启动行为（对应 `start_*` 三个单选项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartMode {
    NewTab,
    RestoreLast,
    Custom,
}

/// 文件视图模式（对应 `view_menu.xml` 的九个条目）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    LargeEx,
    Large,
    Medium,
    Small,
    List,
    Detail,
    Tile,
    Content,
}

impl ViewMode {
    /// 每种模式的图标边长（逻辑像素）。
    pub fn icon_size(self) -> i32 {
        match self {
            ViewMode::LargeEx => 96,
            ViewMode::Large => 64,
            ViewMode::Medium => 48,
            ViewMode::Small => 32,
            ViewMode::List => 16,
            ViewMode::Detail => 16,
            ViewMode::Tile => 48,
            ViewMode::Content => 32,
        }
    }

    /// 是否为「水平铺开」布局（图标在左、文字在右）。
    pub fn is_rows(self) -> bool {
        matches!(self, ViewMode::List | ViewMode::Detail | ViewMode::Content)
    }

    /// 单元格尺寸 (宽, 高)。0 表示按列自适应。
    pub fn cell(self) -> (i32, i32) {
        match self {
            ViewMode::LargeEx => (112, 118),
            ViewMode::Large => (92, 96),
            ViewMode::Medium => (76, 82),
            ViewMode::Small => (60, 66),
            ViewMode::List => (0, 24),
            ViewMode::Detail => (0, 24),
            ViewMode::Tile => (200, 60),
            ViewMode::Content => (280, 46),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ViewMode::LargeEx => "超大图标",
            ViewMode::Large => "大图标",
            ViewMode::Medium => "中图标",
            ViewMode::Small => "小图标",
            ViewMode::List => "列表",
            ViewMode::Detail => "详细信息",
            ViewMode::Tile => "平铺",
            ViewMode::Content => "内容",
        }
    }

    /// view_menu.xml 里九个条目的顺序，用于菜单勾选。
    pub const ALL: [ViewMode; 8] = [
        ViewMode::LargeEx,
        ViewMode::Large,
        ViewMode::Medium,
        ViewMode::Small,
        ViewMode::List,
        ViewMode::Detail,
        ViewMode::Tile,
        ViewMode::Content,
    ];
}

/// 主题（对应两个 .ui 资源包）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeKind {
    Light,
    Dark,
}

/// 排序键：直接复用文件系统层的定义（`fs::SortKey`），
/// 不在此处重复声明，否则 `app.rs` 里两个同名类型无法互相赋值。
pub use crate::fs::SortKey;

/// 界面缩放滑块的两端与步进（倍数，1.5 即 150%）。
///
/// 量程取 [100%, 200%]、步进 5%：150% 正好落在滑块正中，
/// 左半轨 100→150、右半轨 150→200，拖动时所见即所得。
pub const UI_SCALE_MIN: f32 = 1.0;
pub const UI_SCALE_MAX: f32 = 2.0;
pub const UI_SCALE_STEP: f32 = 0.05;

/// 缩放目标：不管屏幕分辨率是多少，界面都按 150% 起。
///
/// 这一条是写死的公式：`Layout::scale` 只乘 `ui_scale`（不叠系统 DPI），
/// 所以初始值就是 150%，滑块正好停在正中。
pub const DEFAULT_UI_SCALE: f32 = 1.5;

/// 缩放配置的版本号。
///
/// 3 起量程改成 [100%, 200%] 的滑块、默认固定 150%：
/// `0`（首次运行）与更老的版本在 `Application::new` 里一次性改写成 150%。
pub const UI_SCALE_VERSION: u32 = 3;

/// 夹进量程并对齐到步进。滑块拖动、读配置都用它。
pub fn clamp_ui_scale(v: f32) -> f32 {
    let q = (v / UI_SCALE_STEP).round() * UI_SCALE_STEP;
    q.clamp(UI_SCALE_MIN, UI_SCALE_MAX)
}


/// 导航窗格在 100% 缩放下的默认宽度，和资源管理器一致。
pub const DEFAULT_NAV_PANE_W: i32 = 280;

/// 右侧详细信息窗格在 100% 缩放下的默认宽度。
pub const DEFAULT_DETAIL_PANE_W: i32 = 280;

/// `serde` 的默认值入口——旧配置里没有这一项，读出来要落回 280。
fn default_detail_pane_w() -> i32 {
    DEFAULT_DETAIL_PANE_W
}

/// `serde` 的默认值入口——旧配置里没有这一项，读出来要落回 280。
fn default_nav_pane_w() -> i32 {
    DEFAULT_NAV_PANE_W
}

/// 导航窗格的分组。
///
/// 分组的**先后顺序**由用户拖动分组上方的分隔线决定，存进
/// [`Settings::nav_sections`]。资源管理器把「快捷访问 / 此电脑 / 网络」钉死成
/// 固定次序，这里放开：每个人关注的东西不一样，把常用的那组拖到最上面
/// 比反复滚动划算。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NavSection {
    /// 主文件夹（顺带回收站）。
    Home,
    /// 「快捷访问」：用户可编辑的常用位置。
    Quick,
    /// 「标签」：用户自定义的颜色标签。
    Tags,
    /// 「此电脑」：驱动器列表。
    Computer,
    /// 「网络」。
    Network,
}

impl NavSection {
    /// 全部分组，也是默认顺序。
    pub const ALL: [NavSection; 5] = [
        NavSection::Home,
        NavSection::Quick,
        NavSection::Tags,
        NavSection::Computer,
        NavSection::Network,
    ];

    pub fn default_order() -> Vec<NavSection> {
        NavSection::ALL.to_vec()
    }
}

/// `serde` 默认值入口：旧配置里没有这一项，读出来按默认顺序。
fn default_nav_sections() -> Vec<NavSection> {
    NavSection::default_order()
}

/// 三列默认宽度（修改日期 / 类型 / 大小）。
///
/// 0 表示「自动」：布局按内容宽度的比例弹性等宽，用户拖过之后才写死像素。
fn default_detail_col_w() -> [i32; 3] {
    [0, 0, 0]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // ---- 启动设置 ----
    /// 接管系统文件夹操作（双面板多标签）。写入注册表，见 shell_integration。
    pub replace_sysexplorer: bool,
    /// 空格键快速预览。
    pub enable_quickpreview: bool,
    pub start_mode: StartMode,
    /// `start_open_custom` 指定的文件夹列表。
    pub custom_start_folders: Vec<PathBuf>,

    // ---- 关闭行为 ----
    /// 多标签时关闭给出提示。
    pub close_alter: bool,

    // ---- 工具栏 ----
    pub toolbar_visible: bool,
    pub bookmark_visible: bool,
    pub bookmark_open_in_newtab: bool,

    // ---- 快捷操作 ----
    pub dbclick_to_parent: bool,
    pub ctrl_dbclick_open_righttab: bool,
    pub dbclick_close_current_tab: bool,
    pub mousewheel_switchtab: bool,

    // ---- 快捷键 ----
    pub disable_all_hotkeys: bool,
    /// 动作 ID → 快捷键描述（如 "Ctrl+T"）。空表示用内置默认。
    pub hotkeys: Vec<(String, String)>,

    // ---- 显示（option_menu.xml）----
    pub show_nav_pane: bool,
    pub show_preview_pane: bool,
    pub show_detail_pane: bool,
    pub show_extension: bool,
    pub show_hidden: bool,
    pub view_mode: ViewMode,
    pub sort_key: SortKey,
    pub sort_desc: bool,
    pub theme: ThemeKind,

    /// 界面整体缩放，叠加在 DPI 之上。
    ///
    /// 1.0 表示「按系统 DPI 原样」。用户嫌字小或嫌占地方时调它，
    /// 所有尺寸都从 `Layout::scale` 派生，所以一处生效全局。
    /// 量程见 [`UI_SCALE_MIN`] / [`UI_SCALE_MAX`]。
    pub ui_scale: f32,

    /// 缩放配置的版本号。见 [`UI_SCALE_VERSION`]。
    #[serde(default)]
    pub scale_version: u32,

    /// 详细信息视图里「修改日期 / 类型 / 大小」三列的逻辑宽度。
    ///
    /// 名称列不存宽度：它吃掉左边剩下的空间，所以右列永远不被压扁——
    /// 这就是「右侧优先级大于左侧」。
    #[serde(default = "default_detail_col_w")]
    pub detail_col_w: [i32; 3],

    /// 导航窗格的逻辑宽度（100% 缩放下的像素）。
    ///
    /// 实际宽度由 `Layout` 乘上 scale 得出，所以拖动分隔条存下来的是一份
    /// 「跟缩放无关」的宽度；换档位后视觉比例基本不变。
    #[serde(default = "default_nav_pane_w")]
    pub nav_pane_w: i32,

    /// 右侧详细信息窗格的逻辑宽度。
    ///
    /// 和 `nav_pane_w` 一样，存的是与缩放无关的逻辑像素；拖动分隔条只改这一个值。
    #[serde(default = "default_detail_pane_w")]
    pub detail_pane_w: i32,

    /// 导航窗格各分组的先后顺序（见 [`NavSection`]）。
    ///
    /// 老配置里没有这一项：读出来落回默认顺序。手改 JSON 导致缺项或重复时，
    /// [`Settings::load`] 会补齐去重，保证界面上永远五组俱全。
    #[serde(default = "default_nav_sections")]
    pub nav_sections: Vec<NavSection>,

    // ---- 窗口几何 ----
    /// `i32::MIN` 表示「未保存过」，启动时居中。
    pub window_x: i32,
    pub window_y: i32,
    /// `0` 表示「尚未确定」，启动时按显示器工作区计算。
    pub window_w: i32,
    pub window_h: i32,
    pub window_maximized: bool,
    /// 窗口几何是否由用户手动调整过。
    ///
    /// 老版本的配置里没有这一项，`serde(default)` 会让它为 false，
    /// 于是那批硬编码的 1000×640 会被一次性的自适应默认值取代；
    /// 用户此后每调整一次窗口大小就会置为 true，我们不再干预。
    #[serde(default)]
    pub window_geometry_set: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            replace_sysexplorer: false,
            enable_quickpreview: true,
            start_mode: StartMode::NewTab,
            custom_start_folders: Vec::new(),

            close_alter: true,

            toolbar_visible: true,
            bookmark_visible: true,
            // 原 XML 中该项默认 selected="true"
            bookmark_open_in_newtab: true,

            // 资源管理器里双击空白就是回上一级，默认开着才符合直觉
            dbclick_to_parent: true,
            ctrl_dbclick_open_righttab: true,
            dbclick_close_current_tab: false,
            mousewheel_switchtab: true,

            disable_all_hotkeys: false,
            hotkeys: Vec::new(),

            // 默认开启：这是资源管理器最显眼的特征，关掉会让界面失去参照
            show_nav_pane: true,
            show_preview_pane: false,
            // 新窗格默认开着：需求就是要它常驻在右侧
            show_detail_pane: true,
            show_extension: true,
            show_hidden: false,
            view_mode: ViewMode::Medium,
            sort_key: SortKey::Name,
            sort_desc: false,
            theme: ThemeKind::Light,
            ui_scale: DEFAULT_UI_SCALE,
            nav_pane_w: DEFAULT_NAV_PANE_W,
            detail_pane_w: DEFAULT_DETAIL_PANE_W,
            nav_sections: default_nav_sections(),
            detail_col_w: default_detail_col_w(),
            // 0 = 还没按屏幕 DPI 折算过，首次拿到 DPI 时补成「实际 150%」
            scale_version: 0,

            window_x: i32::MIN, // MIN 表示"未保存过"，启动时居中
            window_y: i32::MIN,
            window_w: 0, // 0 表示"尚未确定"，启动时按显示器工作区计算
            window_h: 0,
            window_maximized: false,
            window_geometry_set: false,
        }
    }
}

/// 分组顺序的清洗：保留用户给定次序，去掉重复项，再把漏掉的分组按默认位置补上。
///
/// 手改 `settings.json` 时最容易犯的两个错就是「少写一组」和「同一组写两遍」，
/// 两者都会让导航窗格缺块或画两次，所以读配置时就地修好。
pub fn sanitize_nav_sections(order: Vec<NavSection>) -> Vec<NavSection> {
    let mut out: Vec<NavSection> = Vec::with_capacity(NavSection::ALL.len());
    for s in order {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    // 补齐漏项：追加到末尾（按默认顺序的先后）
    for s in NavSection::ALL {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

impl Settings {
    fn file(root: &Path) -> PathBuf {
        root.join("settings.json")
    }

    pub fn load(root: &Path) -> Settings {
        let p = Self::file(root);
        match std::fs::read(&p) {
            Ok(bytes) => {
                let mut s: Settings =
                    serde_json::from_slice(&bytes).unwrap_or_else(|_| Settings::default());
                // 手改过 JSON 或旧版本留下的越界值：先夹回量程。
                // 「按屏幕折算」要等窗口 DPI 出来，交给 `update_dpi` 处理
                // （`scale_version < UI_SCALE_VERSION` 时它会把值重写并回写）。
                s.ui_scale = clamp_ui_scale(s.ui_scale);
                // 分组顺序同样可能被手改坏：补齐缺失的、去掉重复的
                s.nav_sections = sanitize_nav_sections(std::mem::take(&mut s.nav_sections));
                s
            }
            Err(_) => Settings::default(),
        }
    }

    pub fn save(&self, root: &Path) {
        if super::paths::ensure_dir(root).is_err() {
            return;
        }
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::file(root), text);
        }
    }

    /// 内置默认快捷键表（对应原软件的「恢复默认快捷键」）。
    /// 顺序即设置面板 HotKeyCollections 的显示顺序。
    pub fn default_hotkeys() -> Vec<(&'static str, &'static str)> {
        vec![
            ("新建标签", "Ctrl+T"),
            ("关闭当前标签", "Ctrl+W"),
            ("复制当前标签", "Ctrl+Shift+T"),
            ("上一个标签", "Ctrl+PageUp"),
            ("下一个标签", "Ctrl+PageDown"),
            ("后退", "Alt+Left"),
            ("前进", "Alt+Right"),
            ("上一级", "Alt+Up"),
            ("刷新", "F5"),
            ("编辑地址栏", "Ctrl+L"),
            ("搜索框", "Ctrl+F"),
            ("新建文件夹", "Ctrl+Shift+N"),
            ("重命名", "F2"),
            ("删除", "Delete"),
            ("复制", "Ctrl+C"),
            ("剪切", "Ctrl+X"),
            ("粘贴", "Ctrl+V"),
            ("全选", "Ctrl+A"),
            ("属性", "Alt+Enter"),
            ("快速预览", "Space"),
            ("切换视图", "Ctrl+1..8"),
            ("新窗口", "Ctrl+N"),
        ]
    }
}
