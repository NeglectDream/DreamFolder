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

/// 排序键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    Name,
    Size,
    Kind,
    Modified,
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

    // ---- 窗口几何 ----
    pub window_x: i32,
    pub window_y: i32,
    pub window_w: i32,
    pub window_h: i32,
    pub window_maximized: bool,
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

            dbclick_to_parent: false,
            ctrl_dbclick_open_righttab: true,
            dbclick_close_current_tab: false,
            mousewheel_switchtab: true,

            disable_all_hotkeys: false,
            hotkeys: Vec::new(),

            show_nav_pane: false,
            show_preview_pane: false,
            show_detail_pane: false,
            show_extension: true,
            show_hidden: false,
            view_mode: ViewMode::Medium,
            sort_key: SortKey::Name,
            sort_desc: false,
            theme: ThemeKind::Light,

            window_x: i32::MIN, // MIN 表示"未保存过"，启动时居中
            window_y: i32::MIN,
            window_w: 1000,
            window_h: 640,
            window_maximized: false,
        }
    }
}

impl Settings {
    fn file(root: &Path) -> PathBuf {
        root.join("settings.json")
    }

    pub fn load(root: &Path) -> Settings {
        let p = Self::file(root);
        match std::fs::read(&p) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| Settings::default()),
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
