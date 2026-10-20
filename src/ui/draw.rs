//! 绘制：把布局渲染到离屏画布。
//!
//! 所有区域都由我们自己画——这正是原软件 DuiLib 的做法，
//! 好处是完全无子窗口，重绘一次 BitBlt 完成，没有控件层级开销。

use windows_sys::Win32::Foundation::{HDC, RECT};
use windows_sys::Win32::Graphics::Gdi::HFONT;

use crate::app::{Application, SearchMode};
use crate::cfg::settings::{SortKey, ViewMode};
use crate::fs::entry::{Entry, EntryKind};
use crate::fs::icons::IconSize;
use crate::strings as S;
use crate::win::paint::{rect, Canvas, Palette};

use super::{Hit, Layout};

// DrawTextW 标志（自备常量，避免依赖导出差异）
const DT_LEFT: u32 = 0x0000_0000;
const DT_CENTER: u32 = 0x0000_0001;
const DT_RIGHT: u32 = 0x0000_0002;
const DT_VCENTER: u32 = 0x0000_0004;
const DT_SINGLELINE: u32 = 0x0000_0020;
const DT_END_ELLIPSIS: u32 = 0x0000_8000;
const DT_NOPREFIX: u32 = 0x0000_0800;
const DT_WORDBREAK: u32 = 0x0000_0010;

const TEXT_L: u32 = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
const TEXT_C: u32 = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
const TEXT_R: u32 = DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
const TEXT_WRAP_C: u32 = DT_CENTER | DT_WORDBREAK | DT_END_ELLIPSIS | DT_NOPREFIX;

/// 字体档位（对应 explorer_frame.xml 声明的 6 个 Font）。
pub mod font {
    pub const TAB: i32 = 12;
    pub const UI: i32 = 12;
    pub const UI_BOLD: i32 = 12;
    pub const TITLE: i32 = 13;
    pub const SMALL: i32 = 11;
    pub const STATUS: i32 = 11;
}

/// 绘制上下文：把绘制所需的可变借用打包，避免层层传参。
pub struct Painter<'a> {
    pub canvas: &'a Canvas,
    pub layout: &'a Layout,
    pub palette: &'a Palette,
    pub hover: Hit,
    pub pressed: Hit,
    /// 当前鼠标位置（客户区坐标），用于面包屑这类细分热区。
    pub mouse: (i32, i32),
}

impl<'a> Painter<'a> {
    fn s(&self, v: i32) -> i32 {
        (v as f32 * self.layout.scale).round() as i32
    }

    fn is_hover(&self, h: Hit) -> bool {
        self.hover == h
    }

    fn is_pressed(&self, h: Hit) -> bool {
        self.pressed == h
    }

    /// 鼠标是否落在该矩形内。
    fn contains(&self, r: RECT) -> bool {
        let (x, y) = self.mouse;
        x >= r.left && x < r.right && y >= r.top && y < r.bottom
    }
}

/// 顶层绘制入口。返回后由调用方把离屏位图 BitBlt 上屏。
pub fn paint(app: &mut Application, hdc: HDC) {
    // 先克隆需要的只读数据，避免 &mut app 与 &canvas 的借用冲突
    let layout = app.layout;
    let palette = *app.palette_for_theme();
    let hover = app.hover;
    let pressed = app.pressed;

    // 背景
    {
        let canvas = Canvas::new(hdc);
        canvas.fill(layout.client, palette.window_bg);
    }

    // ---- 各区域 ----
    paint_titlebar_and_tabs(app, hdc, &layout, &palette, hover, pressed);
    paint_favbar(app, hdc, &layout, &palette, hover, pressed);
    paint_toolbar(app, hdc, &layout, &palette, hover, pressed);
    paint_navbar(app, hdc, &layout, &palette, hover, pressed);
    paint_fileview(app, hdc, &layout, &palette);
    paint_statusbar(app, hdc, &layout, &palette);
}

// ------------------------------------------------------------------ 标题栏与标签

fn paint_titlebar_and_tabs(
    app: &mut Application,
    hdc: HDC,
    layout: &Layout,
    palette: &Palette,
    hover: Hit,
    pressed: Hit,
) {
    let canvas = Canvas::new(hdc);
    let p = Painter {
        canvas: &canvas,
        layout,
        palette,
        hover,
        pressed,
        mouse: (app.mouse_x, app.mouse_y),
    };

    canvas.fill(layout.titlebar, palette.titlebar_bg);

    // ---- 标签 ----
    let count = app.tabs.len();
    let active = app.active_tab;
    for i in 0..count {
        let r = layout.tab_rect(i, count, app.tabs[i].title.chars().count());
        let is_active = i == active;
        let is_hover = p.is_hover(Hit::Tab(i));

        let bg = if is_active {
            palette.tab_active
        } else if is_hover {
            palette.tab_hover
        } else {
            palette.tab_inactive
        };
        canvas.round_fill(r, p.s(4), bg, None);

        // 激活标签顶部强调线
        if is_active {
            let accent = rect(r.left + p.s(6), r.top, r.right - p.s(6), r.top + p.s(2));
            canvas.round_fill(accent, p.s(1), palette.accent, None);
        }

        // 标签图标：目录统一用文件夹图标（走缓存，无系统调用）
        let icon_size = p.s(16);
        let icon_x = r.left + p.s(8);
        let icon_y = r.top + (r.bottom - r.top - icon_size) / 2;
        let tab_icon = if app.tabs[i].path.is_dir() || app.tabs[i].is_computer {
            app.icon_folder(IconSize::Small)
        } else {
            app.icon_for_ext("", IconSize::Small)
        };
        canvas.icon(tab_icon, icon_x, icon_y, icon_size, icon_size);

        let close_w = if is_active || is_hover { p.s(18) } else { 0 };
        let text_rect = rect(
            icon_x + icon_size + p.s(4),
            r.top,
            r.right - close_w - p.s(4),
            r.bottom,
        );
        let font = app.font_tab();

        let fg = if is_active {
            palette.text
        } else {
            palette.text_dim
        };
        let title = if app.tabs[i].loading {
            format!("{}  {}", S::STATUS_LOADING, app.tabs[i].title)
        } else {
            app.tabs[i].title.clone()
        };
        canvas.text(&title, text_rect, fg, font, TEXT_L);

        // 关闭按钮
        if close_w > 0 {
            let cr = rect(
                r.right - close_w - p.s(2),
                r.top + (r.bottom - r.top - p.s(16)) / 2,
                r.right - p.s(2),
                r.top + (r.bottom - r.top + p.s(16)) / 2,
            );
            draw_close_glyph(&canvas, cr, palette, p.is_hover(Hit::TabClose(i)));
        }
    }

    // ---- 新建标签按钮 ----
    let nt = layout.new_tab;
    if p.is_hover(Hit::NewTab) {
        canvas.round_fill(nt, p.s(3), palette.button_hover, None);
    }
    draw_plus_glyph(&canvas, nt, palette);

    // ---- 窗口按钮 ----
    let wb = layout.window_buttons;
    let bw = (wb.right - wb.left) / super::winbtn::COUNT as i32;
    let btns = [
        (super::winbtn::MENU, Hit::MenuButton),
        (super::winbtn::MIN, Hit::Minimize),
        (super::winbtn::MAX, Hit::Maximize),
        (super::winbtn::CLOSE, Hit::Close),
    ];
    for (idx, hit) in btns {
        let r = rect(
            wb.left + idx as i32 * bw,
            wb.top,
            wb.left + (idx + 1) as i32 * bw,
            wb.bottom,
        );
        let hov = p.is_hover(hit);
        let prs = p.is_pressed(hit);
        if hov || prs {
            let color = if hit == Hit::Close {
                if prs {
                    0x00D00F1F
                } else {
                    palette.close_hover
                }
            } else if prs {
                palette.button_pushed
            } else {
                palette.button_hover
            };
            canvas.fill(r, color);
        }
        draw_window_glyph(&canvas, r, hit, palette, hov && hit == Hit::Close);
    }
}

fn draw_window_glyph(canvas: &Canvas, r: RECT, hit: Hit, palette: &Palette, hot: bool) {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    let color = if hit == Hit::Close && hot {
        0x00FF_FFFF
    } else {
        palette.text
    };

    match hit {
        Hit::MenuButton => {
            // 汉堡菜单：三条横线
            for i in -1..=1 {
                let y = cy + i * 4;
                canvas.hline(cx - 6, cx + 6, y, color);
            }
        }
        Hit::Minimize => {
            canvas.hline(cx - 6, cx + 6, cy + 4, color);
        }
        Hit::Maximize => {
            let rr = rect(cx - 6, cy - 6, cx + 6, cy + 6);
            canvas.border(rr, color);
        }
        Hit::Close => {
            // 两条对角线
            for i in -5..=5 {
                canvas.fill(rect(cx + i, cy + i, cx + i + 1, cy + i + 1), color);
                canvas.fill(rect(cx + i, cy - i, cx + i + 1, cy - i + 1), color);
            }
        }
        _ => {}
    }
}

fn draw_close_glyph(canvas: &Canvas, r: RECT, palette: &Palette, hot: bool) {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    let color = if hot { palette.accent } else { palette.text_dim };
    for i in -4..=4 {
        canvas.fill(rect(cx + i, cy + i, cx + i + 1, cy + i + 1), color);
        canvas.fill(rect(cx + i, cy - i, cx + i + 1, cy - i + 1), color);
    }
}

fn draw_plus_glyph(canvas: &Canvas, r: RECT, palette: &Palette) {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    let color = palette.text;
    canvas.hline(cx - 5, cx + 5, cy, color);
    canvas.vline(cx, cy - 5, cy + 5, color);
}

// ------------------------------------------------------------------ 收藏栏

fn paint_favbar(
    app: &mut Application,
    hdc: HDC,
    layout: &Layout,
    palette: &Palette,
    hover: Hit,
    pressed: Hit,
) {
    if layout.favbar.right <= layout.favbar.left {
        return;
    }
    let canvas = Canvas::new(hdc);
    let p = Painter {
        canvas: &canvas,
        layout,
        palette,
        hover,
        pressed,
        mouse: (app.mouse_x, app.mouse_y),
    };

    canvas.fill(layout.favbar, palette.toolbar_bg);

    if app.cfg.bookmarks.items.is_empty() {
        // 空态提示（对应 explorer_frame.xml 的 FavoritesTip）
        let font = app.font_ui();
        let r = rect(
            layout.favbar.left + p.s(12),
            layout.favbar.top,
            layout.favbar.right - p.s(12),
            layout.favbar.bottom,
        );
        canvas.text(S::FAVORITES_PLACEHOLDER, r, palette.text_dim, font, TEXT_L);
        return;
    }

    let font = app.font_ui();
    let mut x = layout.favbar.left + p.s(8);
    let order = app.cfg.bookmarks.ordered();

    for &idx in &order {
        // 先把需要的数据取出，避免持有 bookmarks 的不可变借用时又要 &mut app 取图标
        let (text, is_folder, path_is_dir, ext) = {
            let item = &app.cfg.bookmarks.items[idx];
            let ext = item
                .path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            (item.name.clone(), item.is_folder, item.path.is_dir(), ext)
        };

        let w = (text.chars().count() as i32 * p.s(8) + p.s(28)).clamp(p.s(48), p.s(160));
        if x + w > layout.favbar.right - p.s(24) {
            break;
        }
        let r = rect(
            x,
            layout.favbar.top + p.s(3),
            x + w,
            layout.favbar.bottom - p.s(3),
        );

        let hov = p.is_hover(Hit::FavoriteItem(idx));
        if hov {
            canvas.round_fill(r, p.s(3), palette.item_hover, None);
        }

        // 图标
        let isz = p.s(16);
        let icon = if is_folder || path_is_dir {
            app.icon_folder(IconSize::Small)
        } else {
            app.icon_for_ext(&ext, IconSize::Small)
        };
        canvas.icon(icon, r.left + p.s(6), r.top + (r.bottom - r.top - isz) / 2, isz, isz);

        let tr = rect(
            r.left + p.s(6) + isz + p.s(4),
            r.top,
            r.right - p.s(6),
            r.bottom,
        );
        canvas.text(&text, tr, palette.text, font, TEXT_L);

        x += w + p.s(2);
    }
}

// ------------------------------------------------------------------ 工具栏

/// 工具栏按钮定义（对应 tool_*.svg 图标名）。
const TOOLBAR_ITEMS: [(&str, &str); 12] = [
    ("tool_new.svg", "新建"),
    ("tool_createdir.svg", "新建文件夹"),
    ("tool_cut.svg", "剪切"),
    ("tool_copy.svg", "复制"),
    ("tool_paste.svg", "粘贴"),
    ("tool_delete.svg", "删除"),
    ("tool_rename.svg", "重命名"),
    ("tool_view.svg", "视图"),
    ("tool_sesearch.svg", "搜索"),
    ("tool_knowledge.svg", "知识"),
    ("tool_screencapture.svg", "截图"),
    ("tool_quickaccess.svg", "快速访问"),
];

fn paint_toolbar(
    app: &mut Application,
    hdc: HDC,
    layout: &Layout,
    palette: &Palette,
    hover: Hit,
    pressed: Hit,
) {
    if layout.toolbar.right <= layout.toolbar.left {
        return;
    }
    let canvas = Canvas::new(hdc);
    let p = Painter {
        canvas: &canvas,
        layout,
        palette,
        hover,
        pressed,
        mouse: (app.mouse_x, app.mouse_y),
    };

    canvas.fill(layout.toolbar, palette.toolbar_bg);

    let btn = p.s(36);
    let gap = p.s(4);
    let mut x = layout.toolbar.left + p.s(12);
    let y = layout.toolbar.top + (layout.toolbar.bottom - layout.toolbar.top - btn) / 2;

    let font = app.font_small();
    for (i, (_icon, label)) in TOOLBAR_ITEMS.iter().enumerate() {
        if x + btn > layout.toolbar.right - p.s(8) {
            break;
        }
        let r = rect(x, y, x + btn, y + btn);

        let hov = p.is_hover(Hit::ToolbarButton(i));
        if hov {
            canvas.round_fill(r, p.s(4), palette.item_hover, None);
        }

        // 无图标资源时绘制文字标签（保持可用性；图标可后续从 .ui 包加载）
        let tr = rect(r.left + p.s(2), r.top + p.s(10), r.right - p.s(2), r.bottom - p.s(2));
        canvas.text(label, tr, palette.text_dim, font, TEXT_C);

        x += btn + gap;
    }

    // 底部 1px 分隔线（对应 ToolContainerBorder）
    canvas.hline(
        layout.toolbar.left,
        layout.toolbar.right,
        layout.toolbar.bottom - 1,
        palette.border_light,
    );
}

// ------------------------------------------------------------------ 导航栏

fn paint_navbar(
    app: &mut Application,
    hdc: HDC,
    layout: &Layout,
    palette: &Palette,
    hover: Hit,
    pressed: Hit,
) {
    let canvas = Canvas::new(hdc);
    let p = Painter {
        canvas: &canvas,
        layout,
        palette,
        hover,
        pressed,
        mouse: (app.mouse_x, app.mouse_y),
    };

    canvas.fill(layout.navbar, palette.nav_bg);

    // ---- 导航按钮 ----
    let can_back = app.can_go_back();
    let can_forward = app.can_go_forward();
    let can_up = app.can_go_up();

    draw_nav_arrow(&canvas, layout.btn_back, palette, hover == Hit::Back, can_back, true);
    draw_nav_arrow(&canvas, layout.btn_forward, palette, hover == Hit::Forward, can_forward, true);
    draw_nav_arrow(&canvas, layout.btn_up, palette, hover == Hit::Up, can_up, false);

    // ---- 地址栏 ----
    let addr = layout.address;
    canvas.round_fill(
        addr,
        p.s(3),
        palette.address_bg,
        Some(palette.address_border),
    );

    if app.address_editing {
        // 编辑态：显示光标与文本
        let font = app.font_ui();
        let tr = rect(
            addr.left + p.s(8),
            addr.top,
            addr.right - p.s(8),
            addr.bottom,
        );
        let text = app.address_text.clone();
        canvas.text(&text, tr, palette.text, font, TEXT_L);
        // 光标
        let caret_x =
            addr.left + p.s(8) + app.address_caret as i32 * p.s(7);
        canvas.vline(caret_x, addr.top + p.s(6), addr.bottom - p.s(6), palette.text);
    } else {
        // 面包屑态
        paint_breadcrumb(app, &canvas, addr, palette, &p);
    }

    // 刷新按钮
    let hov = hover == Hit::Refresh;
    if hov {
        canvas.round_fill(layout.btn_refresh, p.s(3), palette.item_hover, None);
    }
    draw_refresh_glyph(&canvas, layout.btn_refresh, palette);

    // ---- 搜索框 ----
    let sr = layout.search;
    canvas.round_fill(sr, p.s(3), palette.address_bg, Some(palette.address_border));

    // 搜索模式按钮（快速/系统/网络）
    let mode_w = p.s(52);
    let mr = rect(sr.left + p.s(2), sr.top + p.s(2), sr.left + mode_w, sr.bottom - p.s(2));
    let mode_label = match app.search_mode {
        SearchMode::Quick => S::TOOL_QUICK_SEARCH,
        SearchMode::System => S::TOOL_SYSTEM_SEARCH,
        SearchMode::Web => S::TOOL_WEB_SEARCH,
    };
    let font_s = app.font_small();
    canvas.text(mode_label, mr, palette.accent, font_s, TEXT_C);

    // 输入内容或占位提示
    let font = app.font_ui();
    let er = rect(mr.right + p.s(4), sr.top, sr.right - p.s(8), sr.bottom);
    if app.search_text.is_empty() {
        canvas.text(S::TOOL_QUICK_SEARCH, er, palette.text_dim, font, TEXT_L);
    } else {
        let t = app.search_text.clone();
        canvas.text(&t, er, palette.text, font, TEXT_L);
    }

    canvas.hline(
        layout.navbar.left,
        layout.navbar.right,
        layout.navbar.bottom - 1,
        palette.border_light,
    );
}

/// 地址栏面包屑：`此电脑 > C: > Users > me`。
fn paint_breadcrumb(
    app: &mut Application,
    canvas: &Canvas,
    addr: RECT,
    palette: &Palette,
    p: &Painter,
) {
    let font = app.font_ui();
    let mut x = addr.left + p.s(8);
    let segments = app.path_segments();

    for (i, seg) in segments.iter().enumerate() {
        let w = (seg.chars().count() as i32 * p.s(8)).clamp(p.s(28), p.s(180));
        if x + w > addr.right - p.s(30) {
            // 空间不足：省略号
            canvas.text("…", rect(x, addr.top, addr.right - p.s(8), addr.bottom), palette.text_dim, font, TEXT_L);
            break;
        }
        let r = rect(x, addr.top + p.s(3), x + w, addr.bottom - p.s(3));
        let hov = p.contains(r);
        if hov {
            canvas.round_fill(r, p.s(3), palette.item_hover, None);
        }
        canvas.text(seg, r, palette.text, font, TEXT_C);
        x += w;

        // 分隔符
        if i + 1 < segments.len() {
            let sr = rect(x, addr.top, x + p.s(12), addr.bottom);
            canvas.text(">", sr, palette.text_dim, font, TEXT_C);
            x += p.s(12);
        }
    }
}

fn draw_nav_arrow(
    canvas: &Canvas,
    r: RECT,
    palette: &Palette,
    hot: bool,
    enabled: bool,
    is_left: bool,
) {
    let color = if !enabled {
        palette.text_dim
    } else if hot {
        palette.accent
    } else {
        palette.text
    };
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;

    // 用逐行缩短的横线画三角箭头
    for i in 0..6 {
        let dir: i32 = if is_left { i } else { 5 - i };
        let y = cy - 5 + i;
        if is_left {
            canvas.hline(cx - 3 + dir, cx + 4, y, color);
        } else {
            canvas.hline(cx - 4, cx + 3 - dir, y, color);
        }
    }
}

fn draw_refresh_glyph(canvas: &Canvas, r: RECT, palette: &Palette) {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    let color = palette.text;
    // 简化的环形：上下两条弧线加箭头
    canvas.hline(cx - 5, cx + 5, cy - 5, color);
    canvas.hline(cx - 5, cx + 5, cy + 5, color);
    canvas.vline(cx - 5, cy - 4, cy + 4, color);
    canvas.vline(cx + 5, cy - 4, cy + 4, color);
    // 箭头
    canvas.vline(cx + 5, cy - 5, cy - 1, color);
    canvas.hline(cx + 3, cx + 7, cy - 3, color);
}

// ------------------------------------------------------------------ 文件视图

fn paint_fileview(app: &mut Application, hdc: HDC, layout: &Layout, palette: &Palette) {
    let vr = layout.view;
    if vr.right <= vr.left || vr.bottom <= vr.top {
        return;
    }
    let canvas = Canvas::new(hdc);
    canvas.fill(vr, palette.list_bg);

    let active = app.active_tab;
    if app.tabs.is_empty() {
        return;
    }

    // 错误状态
    if let Some(err) = app.tabs[active].error.clone() {
        let font = app.font_ui();
        let r = rect(
            vr.left + 16,
            vr.top + 16,
            vr.right - 16,
            vr.bottom - 16,
        );
        canvas.text(&err, r, palette.text_dim, font, DT_CENTER | DT_WORDBREAK | DT_NOPREFIX);
        return;
    }

    // 加载中
    if app.tabs[active].loading && app.tabs[active].entries.is_empty() {
        let font = app.font_ui();
        let r = rect(vr.left + 16, vr.top + 16, vr.right - 16, vr.bottom - 16);
        canvas.text(S::STATUS_LOADING, r, palette.text_dim, font, DT_CENTER | DT_NOPREFIX);
        return;
    }

    // 空目录
    if app.tabs[active].entries.is_empty() {
        let font = app.font_ui();
        let r = rect(vr.left + 16, vr.top + 16, vr.right - 16, vr.bottom - 16);
        canvas.text(S::STATUS_EMPTY_FOLDER, r, palette.text_dim, font, DT_CENTER | DT_NOPREFIX);
        return;
    }

    let view_mode = app.tabs[active].view_mode;
    if view_mode.is_rows() {
        paint_rows_view(app, &canvas, vr, palette);
    } else {
        paint_grid_view(app, &canvas, vr, palette);
    }
}

/// 图标模式（大/中/小图标、平铺）：网格铺排 + 虚拟化。
fn paint_grid_view(app: &mut Application, canvas: &Canvas, vr: RECT, palette: &Palette) {
    let active = app.active_tab;
    let view_mode = app.tabs[active].view_mode;
    let scale = app.layout.scale;
    let s = |v: i32| (v as f32 * scale).round() as i32;

    let (cell_w_cfg, cell_h_cfg) = view_mode.cell();
    // 图标尺寸按 DPI 缩放
    let icon_px = s(view_mode.icon_size());
    let cell_w = if cell_w_cfg == 0 {
        s(112)
    } else {
        (s(cell_w_cfg).max(icon_px + s(24)))
    };
    let cell_h = (s(cell_h_cfg).max(icon_px + s(34)));

    let avail_w = (vr.right - vr.left).max(cell_w);
    let cols = (avail_w / cell_w).max(1) as usize;

    let total = app.tabs[active].entries.len();
    let rows = total.div_ceil(cols);
    let content_h = rows as i32 * cell_h;

    // 滚动夹紧
    let max_scroll = (content_h - (vr.bottom - vr.top)).max(0);
    let scroll = app.tabs[active].scroll_y.clamp(0, max_scroll);
    app.tabs[active].scroll_y = scroll;

    let first_row = (scroll / cell_h).max(0) as usize;
    let visible_rows = ((vr.bottom - vr.top) / cell_h + 2) as usize;
    let last_row = (first_row + visible_rows).min(rows);

    let font = app.font_ui();
    let font_small = app.font_small();
    let show_ext = app.cfg.settings.show_extension;
    let selection = app.tabs[active].selected.clone();

    for row in first_row..last_row {
        for col in 0..cols {
            let idx = row * cols + col;
            if idx >= total {
                break;
            }
            let x = vr.left + col as i32 * cell_w;
            let y = vr.top + row as i32 * cell_h - scroll;
            let r = rect(x, y, x + cell_w, y + cell_h);

            if r.bottom < vr.top || r.top > vr.bottom {
                continue;
            }

            paint_grid_cell(
                app, canvas, r, idx,
                view_mode, icon_px, font, font_small,
                show_ext, &selection, palette, vr,
            );
        }
    }

    // 滚动条
    if max_scroll > 0 {
        draw_scrollbar(canvas, vr, scroll, max_scroll, content_h, palette);
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_grid_cell(
    app: &mut Application,
    canvas: &Canvas,
    r: RECT,
    index: usize,
    view_mode: ViewMode,
    icon_px: i32,
    font: HFONT,
    font_small: HFONT,
    show_ext: bool,
    selection: &std::collections::HashSet<usize>,
    palette: &Palette,
    view: RECT,
) {
    let active = app.active_tab;
    let entry = app.tabs[active].entries[index].clone();
    let selected = selection.contains(&index);
    let hovered = app.hover == Hit::ListItem(index);
    let focused = app.active_tab_focused;

    if selected {
        let bg = if focused {
            palette.item_selected
        } else {
            palette.item_selected_inactive
        };
        canvas.round_fill(r, 3, bg, None);
    } else if hovered {
        canvas.round_fill(r, 3, palette.item_hover, None);
    }

    let inner = rect(r.left + 4, r.top + 4, r.right - 4, r.bottom - 4);

    match view_mode {
        ViewMode::Tile | ViewMode::Content => {
            // 图标在左，两行文字在右
            let isz = icon_px.min(inner.bottom - inner.top);
            let ix = inner.left + 4;
            let iy = inner.top + (inner.bottom - inner.top - isz) / 2;
            let icon = resolve_icon(app, &entry, IconSize::Large, isz);
            canvas.icon(icon, ix, iy, isz, isz);

            let tx = ix + isz + 6;
            let name = entry.display_name(show_ext).to_string();
            let name_rect = rect(tx, inner.top + 4, inner.right - 2, inner.top + 4 + 20);
            canvas.text(&name, name_rect, palette.text, font, TEXT_L);

            let detail = if entry.is_dir() {
                app.icons.dir_type_name()
            } else if !entry.type_name.is_empty() {
                entry.type_name.clone()
            } else {
                app.icons.type_name(&entry.ext())
            };
            let detail_rect = rect(tx, inner.top + 24, inner.right - 2, inner.top + 24 + 18);
            canvas.text(&detail, detail_rect, palette.text_dim, font_small, TEXT_L);
        }
        _ => {
            // 图标在上，文字在下居中
            let isz = icon_px.min((inner.bottom - inner.top) - 28).max(16);
            let cx = (inner.left + inner.right) / 2;
            let iy = inner.top + 6;
            let icon = resolve_icon(app, &entry, IconSize::Large, isz);
            canvas.icon(icon, cx - isz / 2, iy, isz, isz);

            let name = entry.display_name(show_ext).to_string();
            let name_rect = rect(
                inner.left + 2,
                iy + isz + 4,
                inner.right - 2,
                (iy + isz + 4 + 34).min(inner.bottom),
            );
            canvas.text(&name, name_rect, palette.text, font, TEXT_WRAP_C);
        }
    }

    let _ = view;
}

/// 行模式（列表 / 详细信息 / 内容）：一行一项，可带列。
fn paint_rows_view(app: &mut Application, canvas: &Canvas, vr: RECT, palette: &Palette) {
    let active = app.active_tab;
    let view_mode = app.tabs[active].view_mode;
    let scale = app.layout.scale;
    let s = |v: i32| (v as f32 * scale).round() as i32;

    let row_h = s(24);
    let total = app.tabs[active].entries.len();
    let content_h = total as i32 * row_h;
    let max_scroll = (content_h - (vr.bottom - vr.top)).max(0);
    let scroll = app.tabs[active].scroll_y.clamp(0, max_scroll);
    app.tabs[active].scroll_y = scroll;

    let first = (scroll / row_h).max(0) as usize;
    let visible = ((vr.bottom - vr.top) / row_h + 2) as usize;
    let last = (first + visible).min(total);

    let font = app.font_ui();
    let show_ext = app.cfg.settings.show_extension;
    let show_ext_col = view_mode == ViewMode::Detail;
    let selection = app.tabs[active].selected.clone();
    // 重命名编辑态：提前取出，避免与后面 &mut app 的调用冲突
    let rename = app.rename_state().map(|(i, t, c)| (i, t.to_string(), c));

    // ---- 表头（仅详细信息模式）----
    let mut list_top = vr.top;
    if show_ext_col {
        let hdr = rect(vr.left, vr.top, vr.right, vr.top + row_h);
        canvas.fill(hdr, palette.header_bg);

        let cols = detail_columns(vr, s);
        canvas.text("名称", rect(cols[0].0, hdr.top, cols[0].1, hdr.bottom), palette.text_dim, font, TEXT_L);
        canvas.text("修改日期", rect(cols[2].0, hdr.top, cols[2].1, hdr.bottom), palette.text_dim, font, TEXT_R);
        canvas.text("类型", rect(cols[3].0, hdr.top, cols[3].1, hdr.bottom), palette.text_dim, font, TEXT_L);
        canvas.text("大小", rect(cols[4].0, hdr.top, cols[4].1, hdr.bottom), palette.text_dim, font, TEXT_R);

        canvas.hline(vr.left, vr.right, hdr.bottom - 1, palette.border);
        list_top += row_h;
    }

    let list_rect = rect(vr.left, list_top, vr.right, vr.bottom);

    for idx in first..last {
        let y = list_top + idx as i32 * row_h - scroll;
        let r = rect(vr.left, y, vr.right, y + row_h);
        if r.bottom < list_top || r.top > vr.bottom {
            continue;
        }
        // 超出表头区域的裁剪
        let r = rect(r.left, r.top.max(list_top), r.right, r.bottom);

        let entry = app.tabs[active].entries[idx].clone();
        let selected = selection.contains(&idx);
        let hovered = app.hover == Hit::ListItem(idx);
        let focused = app.active_tab_focused;

        if selected {
            canvas.fill(
                r,
                if focused {
                    palette.item_selected
                } else {
                    palette.item_selected_inactive
                },
            );
        } else if hovered {
            canvas.fill(r, palette.item_hover);
        }

        let isz = s(16);
        let ix = r.left + s(6);
        let iy = r.top + (r.bottom - r.top - isz) / 2;
        let icon = resolve_icon(app, &entry, IconSize::Small, isz);
        canvas.icon(icon, ix, iy, isz, isz);

        if show_ext_col {
            let cols = detail_columns(vr, s);
            let name_rect = rect(cols[0].0, r.top, cols[0].1 - s(8), r.bottom);
            if let Some((target, text, caret)) = &rename {
                if *target == idx {
                    draw_inline_edit(canvas, name_rect, text, *caret, palette, font, s(7));
                } else {
                    let name = entry.display_name(show_ext).to_string();
                    canvas.text(&name, name_rect, palette.text, font, TEXT_L);
                }
            } else {
                let name = entry.display_name(show_ext).to_string();
                canvas.text(&name, name_rect, palette.text, font, TEXT_L);
            }

            let modified = entry.modified_text();
            canvas.text(&modified, rect(cols[2].0, r.top, cols[2].1 - s(8), r.bottom), palette.text_dim, font, TEXT_R);

            let kind = if entry.is_dir() {
                "文件夹".to_string()
            } else if !entry.type_name.is_empty() {
                entry.type_name.clone()
            } else {
                app.icons.type_name(&entry.ext())
            };
            canvas.text(&kind, rect(cols[3].0, r.top, cols[3].1 - s(8), r.bottom), palette.text_dim, font, TEXT_L);

            let size = entry.size_text();
            canvas.text(&size, rect(cols[4].0, r.top, cols[4].1 - s(8), r.bottom), palette.text_dim, font, TEXT_R);
        } else {
            let name_rect = rect(ix + isz + s(6), r.top, r.right - s(8), r.bottom);
            if let Some((target, text, caret)) = &rename {
                if *target == idx {
                    draw_inline_edit(canvas, name_rect, text, *caret, palette, font, s(7));
                } else {
                    let name = entry.display_name(show_ext).to_string();
                    canvas.text(&name, name_rect, palette.text, font, TEXT_L);
                }
            } else {
                let name = entry.display_name(show_ext).to_string();
                canvas.text(&name, name_rect, palette.text, font, TEXT_L);
            }
        }
    }

    if max_scroll > 0 {
        draw_scrollbar(canvas, vr, scroll, max_scroll, content_h, palette);
    }
}

/// 详细信息模式的列位置。
fn detail_columns(vr: RECT, s: impl Fn(i32) -> i32) -> [(i32, i32); 5] {
    let w = vr.right - vr.left;
    let name_l = vr.left + s(28);
    let name_r = vr.left + (w * 45 / 100);
    let size_l = vr.right - s(90);
    let size_r = vr.right - s(8);
    let kind_l = name_r + s(8);
    let kind_r = size_l - s(8);
    let date_l = kind_r - s(130);
    let date_r = kind_r;
    [
        (name_l, name_r),
        (0, 0),
        (date_l, date_r),
        (kind_l, kind_r),
        (size_l, size_r),
    ]
}

/// 内联重命名编辑框。
fn draw_inline_edit(
    canvas: &Canvas,
    r: RECT,
    text: &str,
    caret: usize,
    palette: &Palette,
    font: HFONT,
    char_w: i32,
) {
    // 白底 + 强调色边框，与原软件输入框观感一致
    canvas.fill(r, 0x00FF_FFFF);
    canvas.border(r, palette.accent);

    let inner = rect(r.left + 3, r.top, r.right - 3, r.bottom);
    canvas.text(text, inner, 0x00_00_0000, font, TEXT_L);

    // 光标
    let caret_x = inner.left + (caret as i32 * char_w).min((r.right - r.left) - 6);
    canvas.vline(caret_x, r.top + 3, r.bottom - 3, palette.text);
}

/// 确保条目的图标键已解析，然后取图标。
fn resolve_icon(
    app: &mut Application,
    entry: &Entry,
    size: IconSize,
    _px: i32,
) -> *mut core::ffi::c_void {
    use crate::fs::icons::key_for;
    // 懒解析：首次绘制时把 IconKey 从 Pending 补上
    let key = match &entry.icon_key {
        crate::fs::entry::IconKey::Pending => {
            key_for(&entry.name, entry.is_dir(), entry.kind == EntryKind::Drive)
        }
        k => k.clone(),
    };
    app.icons.get(&key, size)
}

fn draw_scrollbar(
    canvas: &Canvas,
    vr: RECT,
    scroll: i32,
    max_scroll: i32,
    content_h: i32,
    palette: &Palette,
) {
    let track_h = vr.bottom - vr.top;
    let thumb_h = ((track_h as f32 * track_h as f32) / content_h as f32).max(24.0) as i32;
    let ratio = scroll as f32 / max_scroll as f32;
    let y = vr.top + ((track_h - thumb_h) as f32 * ratio) as i32;

    let r = rect(vr.right - 10, y, vr.right - 4, y + thumb_h);
    canvas.round_fill(r, 3, palette.text_dim, None);
}

// ------------------------------------------------------------------ 状态栏

fn paint_statusbar(app: &mut Application, hdc: HDC, layout: &Layout, palette: &Palette) {
    let r = layout.statusbar;
    if r.right <= r.left {
        return;
    }
    let canvas = Canvas::new(hdc);
    canvas.fill(r, palette.toolbar_bg);
    canvas.hline(r.left, r.right, r.top, palette.border_light);

    let active = app.active_tab;
    let total = if app.tabs.is_empty() {
        0
    } else {
        app.tabs[active].entries.len()
    };
    let selected = if app.tabs.is_empty() {
        0
    } else {
        app.tabs[active].selected.len()
    };

    let text = if selected > 1 {
        let hint = format_size_hint(app, selected);
        format!("{selected} {}{hint}", S::STATUS_ITEMS_SELECTED)
    } else {
        format!("{total} {}", S::STATUS_ITEMS)
    };

    let font = app.font_status();
    let tr = rect(r.left + 10, r.top, r.right - 10, r.bottom);
    canvas.text(&text, tr, palette.text_dim, font, TEXT_L);

    // 右侧：排序与视图提示
    let mode = if app.tabs.is_empty() {
        ViewMode::Medium
    } else {
        app.tabs[active].view_mode
    };
    let sort_label = match app.cfg.settings.sort_key {
        SortKey::Name => "名称",
        SortKey::Size => "大小",
        SortKey::Kind => "类型",
        SortKey::Modified => "修改日期",
    };
    let hint = format!("{}  ↑{}", mode.label(), sort_label);
    canvas.text(&hint, tr, palette.text_dim, font, TEXT_R);
}

fn format_size_hint(app: &Application, _count: usize) -> String {
    let active = app.active_tab;
    if app.tabs.is_empty() {
        return String::new();
    }
    let total: u64 = app.tabs[active]
        .selected
        .iter()
        .filter_map(|&i| app.tabs[active].entries.get(i))
        .filter(|e| !e.is_dir())
        .map(|e| e.size)
        .sum();
    if total > 0 {
        format!("，共 {}", crate::fs::entry::format_size(total))
    } else {
        String::new()
    }
}
