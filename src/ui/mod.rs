//! UI 层：布局、绘制、交互。
//!
//! 结构对照原软件 `explorer_frame.xml` 的控件树：
//! ```text
//! Window
//!  └ VerticalLayout
//!     ├ HorizontalLayout  height=32   ← 标题栏（标签 + 窗口按钮）
//!     ├ FavoritesPanel    height=30   ← 收藏书签栏
//!     ├ ToolContainer     height=48   ← 工具栏
//!     ├ HorizontalVCenter height=40   ← 导航栏（后退/前进/地址/搜索）
//!     └ HwndControl hostView          ← 文件视图（我们自绘）
//! ```

pub mod draw;
pub mod interact;

use windows_sys::Win32::Foundation::RECT;

use crate::win::paint::rect;

/// 布局常量（96 DPI 下的逻辑像素；实际使用时会乘 DPI 缩放）。
pub mod metrics {
    pub const TITLEBAR_H: i32 = 32;
    pub const FAVBAR_H: i32 = 30;
    pub const TOOLBAR_H: i32 = 48;
    pub const NAVBAR_H: i32 = 40;
    pub const STATUSBAR_H: i32 = 24;

    pub const TAB_MIN_W: i32 = 88;
    pub const TAB_MAX_W: i32 = 200;
    pub const TAB_PADDING: i32 = 12;

    pub const WINDOW_BUTTON_W: i32 = 32;

    pub const ADDR_MIN_W: i32 = 180;
    pub const SEARCH_W: i32 = 200;

    /// 窗口最小尺寸（对应 XML 的 mininfo="496,300"）
    pub const MIN_WINDOW_W: i32 = 496;
    pub const MIN_WINDOW_H: i32 = 300;
}

/// 各区域矩形。每帧在 `Layout::compute` 中重算。
#[derive(Debug, Clone, Copy, Default)]
pub struct Layout {
    pub client: RECT,

    pub titlebar: RECT,
    /// 标签条（标题栏内，除去窗口按钮）
    pub tabstrip: RECT,
    /// 新建标签按钮
    pub new_tab: RECT,
    /// 窗口按钮区（菜单/最小化/最大化/关闭）
    pub window_buttons: RECT,

    pub favbar: RECT,
    pub toolbar: RECT,
    pub navbar: RECT,

    /// 导航按钮
    pub btn_back: RECT,
    pub btn_forward: RECT,
    pub btn_up: RECT,
    pub btn_refresh: RECT,

    /// 地址栏（面包屑 + 编辑框合一）
    pub address: RECT,
    pub search: RECT,

    /// 文件视图
    pub view: RECT,
    pub statusbar: RECT,

    /// 导航窗格（左侧）
    pub nav_pane: RECT,
    /// 详细信息窗格（底部）
    pub detail_pane: RECT,

    /// DPI 缩放系数
    pub scale: f32,
}

impl Layout {
    /// 计算布局。`dpi_scale` 由窗口 DPI 决定。
    pub fn compute(
        client: RECT,
        dpi_scale: f32,
        show_favbar: bool,
        show_toolbar: bool,
        show_nav_pane: bool,
        show_detail_pane: bool,
    ) -> Layout {
        let s = |v: i32| -> i32 { (v as f32 * dpi_scale).round() as i32 };
        let w = client.right - client.left;
        let h = client.bottom - client.top;

        let mut y = client.top;

        // 标题栏 + 标签
        let titlebar = rect(client.left, y, client.right, y + s(metrics::TITLEBAR_H));
        y += s(metrics::TITLEBAR_H);

        let wb_w = s(metrics::WINDOW_BUTTON_W) * 4;
        let tabstrip = rect(
            client.left + s(6),
            titlebar.top,
            (client.right - wb_w - s(6)).max(client.left + s(6)),
            titlebar.bottom,
        );
        let window_buttons = rect(tabstrip.right, titlebar.top, client.right, titlebar.bottom);
        // 「+」按钮贴在标签条右端
        let new_tab = rect(
            (tabstrip.right - s(22)).max(tabstrip.left),
            titlebar.top + s(4),
            tabstrip.right,
            titlebar.bottom - s(4),
        );

        // 收藏栏
        let favbar = if show_favbar {
            let r = rect(client.left, y, client.right, y + s(metrics::FAVBAR_H));
            y += s(metrics::FAVBAR_H);
            r
        } else {
            rect(0, 0, 0, 0)
        };

        // 工具栏
        let toolbar = if show_toolbar {
            let r = rect(client.left, y, client.right, y + s(metrics::TOOLBAR_H));
            y += s(metrics::TOOLBAR_H);
            r
        } else {
            rect(0, 0, 0, 0)
        };

        // 导航栏
        let navbar = rect(client.left, y, client.right, y + s(metrics::NAVBAR_H));
        y += s(metrics::NAVBAR_H);

        // 导航栏内部
        let pad = s(12);
        let icon = s(16);
        let btn_h = navbar.bottom - navbar.top;
        let btn_y_top = navbar.top + (btn_h - icon) / 2;
        let btn_y_bot = btn_y_top + icon;

        let mut x = navbar.left + pad;
        let btn_back = rect(x, btn_y_top, x + icon, btn_y_bot);
        x += icon + s(8);
        let btn_forward = rect(x, btn_y_top, x + icon, btn_y_bot);
        x += icon + s(8);
        let btn_up = rect(x, btn_y_top, x + icon, btn_y_bot);
        x += icon + s(8);

        // 右侧：搜索框
        let search_w = s(metrics::SEARCH_W);
        let search = rect(
            (navbar.right - pad - search_w).max(x + s(metrics::ADDR_MIN_W)),
            navbar.top + s(6),
            navbar.right - pad,
            navbar.bottom - s(6),
        );

        // 地址栏占据剩余空间
        let addr_left = x;
        let addr_right = (search.left - s(10)).max(addr_left + s(60));
        let address = rect(
            addr_left,
            navbar.top + s(6),
            addr_right,
            navbar.bottom - s(6),
        );

        let btn_refresh = rect(
            address.right - s(24),
            btn_y_top,
            address.right - s(8),
            btn_y_bot,
        );

        // 底部状态栏
        let statusbar = rect(
            client.left,
            client.bottom - s(metrics::STATUSBAR_H),
            client.right,
            client.bottom,
        );

        // 中间区域：视图 + 可选侧栏
        let mid_top = y;
        let mid_bottom = statusbar.top;

        let detail_h = if show_detail_pane {
            s(120).min((mid_bottom - mid_top) / 2)
        } else {
            0
        };
        let detail_pane = if show_detail_pane {
            rect(client.left, mid_bottom - detail_h, client.right, mid_bottom)
        } else {
            rect(0, 0, 0, 0)
        };

        let nav_w = if show_nav_pane {
            s(180).min(w / 3)
        } else {
            0
        };
        let nav_pane = if show_nav_pane {
            rect(client.left, mid_top, client.left + nav_w, mid_bottom - detail_h)
        } else {
            rect(0, 0, 0, 0)
        };

        let view = rect(
            client.left + nav_w,
            mid_top,
            client.right,
            mid_bottom - detail_h,
        );

        let _ = w;
        let _ = h;

        Layout {
            client,
            titlebar,
            tabstrip,
            new_tab,
            window_buttons,
            favbar,
            toolbar,
            navbar,
            btn_back,
            btn_forward,
            btn_up,
            btn_refresh,
            address,
            search,
            view,
            statusbar,
            nav_pane,
            detail_pane,
            scale: dpi_scale,
        }
    }

    /// 标签的绘制矩形。`tabstrip` 内均分，宽度夹在 [MIN, MAX] 之间。
    pub fn tab_rect(&self, index: usize, count: usize, title_hint: usize) -> RECT {
        if count == 0 {
            return rect(0, 0, 0, 0);
        }
        let s = |v: i32| -> i32 { (v as f32 * self.scale).round() as i32 };
        let avail = (self.new_tab.left - self.tabstrip.left).max(0);
        let count_i = count.max(1) as i32;

        // 先按均分算，再夹紧
        let mut w = avail / count_i;
        w = w.clamp(s(metrics::TAB_MIN_W).min(avail), s(metrics::TAB_MAX_W));

        // 依据标题长度做轻微伸缩，但总宽不超过可用空间
        let hint = (title_hint as i32 * s(7)).clamp(s(metrics::TAB_MIN_W), s(metrics::TAB_MAX_W));
        let w = w.min(hint).max(s(48));
        let total = (w * count_i).min(avail);

        let x = self.tabstrip.left + index as i32 * w;
        let right = self.tabstrip.left + total;
        rect(
            x,
            self.tabstrip.top + s(4),
            (x + w).min(right),
            self.tabstrip.bottom,
        )
    }

    /// 视图中一行/一格是否可见（虚拟化绘制用）。
    pub fn view_visible_height(&self) -> i32 {
        (self.view.bottom - self.view.top).max(0)
    }
}

/// 命中测试结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    None,
    Titlebar,
    Tab(usize),
    TabClose(usize),
    NewTab,
    MenuButton,
    Minimize,
    Maximize,
    Close,
    Back,
    Forward,
    Up,
    Refresh,
    Address,
    Search,
    FavoriteItem(usize),
    FavoriteMore,
    ToolbarButton(usize),
    /// 文件视图中的条目
    ListItem(usize),
    /// 文件视图空白区
    ViewEmpty,
    Statusbar,
    NavPane,
    DetailPane,
}

/// 窗口按钮索引（供 create/命中共用）。
pub mod winbtn {
    pub const MENU: usize = 0;
    pub const MIN: usize = 1;
    pub const MAX: usize = 2;
    pub const CLOSE: usize = 3;
    pub const COUNT: usize = 4;
}
