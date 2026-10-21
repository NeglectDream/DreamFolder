//! 绘制：把布局渲染到离屏画布。
//!
//! 所有区域都由我们自己画——这正是原软件 DuiLib 的做法，
//! 好处是完全无子窗口，重绘一次 BitBlt 完成，没有控件层级开销。

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::{HFONT, HDC};

use crate::app::{Application, SearchState};
use crate::cfg::settings::{SortKey, ViewMode};
use crate::fs::entry::Entry;
use crate::fs::icons::IconSize;
use crate::strings as S;
use crate::win::paint::{rect, rect_contains, Canvas, Palette};

use super::navpane::NavKind;
use super::{cards, dialog, metrics, scrollbar_geom, settingspane, Hit, Layout, ScrollPane};

// DrawTextW 标志（自备常量，避免依赖导出差异）
const DT_LEFT: u32 = 0x0000_0000;
const DT_CENTER: u32 = 0x0000_0001;
const DT_RIGHT: u32 = 0x0000_0002;
const DT_VCENTER: u32 = 0x0000_0004;
const DT_SINGLELINE: u32 = 0x0000_0020;
const DT_END_ELLIPSIS: u32 = 0x0000_8000;
const DT_NOPREFIX: u32 = 0x0000_0800;
const DT_WORDBREAK: u32 = 0x0000_0010;

const TEXT_L: u32 = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;const TEXT_C: u32 = DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
/// 单行输入框：不省略号、不折行，超出部分交给裁剪。
///
/// 编辑态不能用 `DT_END_ELLIPSIS`：文本框滚动时矩形整体左移，省略号画在
/// 矩形右边界上，正好落在插入符左边，会把可见文字盖成「…」。
const TEXT_EDIT: u32 = DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX;

/// 单行输入框里文字的绘制矩形：从 `x` 起留足长度，多出的部分由裁剪处理。
///
/// 配合 [`TEXT_EDIT`] 使用——宽度只是「足够长」，真正的边界是调用方的裁剪区。
fn edit_text_rect(x: i32, r: RECT) -> RECT {
    rect(x, r.top, x + 1_000_000, r.bottom)
}
/// 左对齐、按宽度折行（对话框正文用）。`DT_NOPREFIX` 是为了让正文里的
/// `&` 原样显示——不然 DrawText 会把它当成助记符前缀吃掉。
const TEXT_WRAP_L: u32 = DT_LEFT | DT_WORDBREAK | DT_NOPREFIX;

// Segoe MDL2 Assets 字形。资源管理器用的是同一套字体，形状和比例一致。
const ICON_BACK: &str = "\u{E72B}";
const ICON_FORWARD: &str = "\u{E72A}";
const ICON_UP: &str = "\u{E74A}";
const ICON_REFRESH: &str = "\u{E72C}";
const ICON_SEARCH: &str = "\u{E721}";
/// 状态栏：详细列表视图。
const ICON_VIEW_DETAIL: &str = "\u{E8FD}";
/// 状态栏：大图标视图。
const ICON_VIEW_GRID: &str = "\u{E80A}";
const TEXT_R: u32 = DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX;
const TEXT_WRAP_C: u32 = DT_CENTER | DT_WORDBREAK | DT_END_ELLIPSIS | DT_NOPREFIX;

/// 字体档位（对应 explorer_frame.xml 声明的 Font）。
pub mod font {
    pub const TAB: i32 = 12;
    pub const UI: i32 = 12;
    pub const SMALL: i32 = 11;
    pub const STATUS: i32 = 11;
}

/// 绘制上下文：把绘制所需的只读状态打包，避免层层传参。
pub struct Painter<'a> {
    pub layout: &'a Layout,
    pub hover: Hit,
    pub pressed: Hit,
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
}

/// 顶层绘制入口。返回后由调用方把离屏位图 BitBlt 上屏。
///
/// `dirty` 是这次真正需要重画的区域。离屏位图在两次 `WM_PAINT` 之间是
/// 保留内容的，所以没脏的区域可以整块跳过——插入符每 530ms 闪一次，
/// 以前每次都把标题栏、导航窗格、整个文件列表重画一遍，纯属白烧 CPU。
/// 与脏区不相交的区域直接不画，它们留在位图里的还是上一帧的样子。
pub fn paint(app: &mut Application, hdc: HDC, dirty: RECT) {
    // 先克隆需要的只读数据，避免 &mut app 与 &canvas 的借用冲突
    let layout = app.layout;
    let palette = *app.palette_for_theme();
    let hover = app.hover;
    let pressed = app.pressed;

    let hit = |r: RECT| rects_overlap(r, dirty);

    // 背景：只刷脏区
    {
        let canvas = Canvas::new(hdc);
        canvas.fill(dirty, palette.window_bg);
    }

    // ---- 各区域 ----
    if hit(layout.titlebar) {
        paint_titlebar_and_tabs(app, hdc, &layout, &palette, hover, pressed);
    }
    if hit(layout.favbar) {
        paint_favbar(app, hdc, &layout, &palette, hover, pressed);
    }
    if hit(layout.toolbar) {
        paint_toolbar(app, hdc, &layout, &palette, hover, pressed);
    }
    if hit(layout.navbar) {
        paint_navbar(app, hdc, &layout, &palette, hover, pressed);
    }
    // 导航窗格画在文件视图之前：两者矩形本就不重叠（view.left 已让出 nav_w），
    // 但这个顺序能保证万一将来布局改动，窗格也不会被内容区盖住。
    if hit(layout.nav_pane) {
        paint_nav_pane(app, hdc, &layout, &palette, hover, pressed);
    }
    if hit(layout.view) {
        paint_fileview(app, hdc, &layout, &palette);
    }
    if hit(layout.detail_pane) {
        paint_detail_pane(app, hdc, &layout, &palette);
    }
    if hit(layout.view) {
        paint_marquee(app, hdc, &layout, &palette);
    }
    if hit(layout.statusbar) {
        paint_statusbar(app, hdc, &layout, &palette);
    }
    // 浮层跨区域：只要和脏区沾边就整块重画，否则脏区里被背景刷掉的那半截
    // 浮层就再也补不回来了。
    paint_tooltip(app, hdc, &layout, &palette);
    paint_addr_suggest(app, hdc, &layout, &palette, hover);
    // 模态层最后画，保证压在所有区域之上
    paint_settings(app, hdc, &layout, &palette);
    // 对话框比设置面板更靠上（命中测试也是先判它）
    paint_dialog(app, hdc, &layout, &palette);
}

/// 两个矩形是否有重叠。
fn rects_overlap(a: RECT, b: RECT) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

/// 框选矩形。只画边和一层很淡的填充——填充太实会盖住底下的文件名。
fn paint_marquee(app: &Application, hdc: HDC, layout: &Layout, palette: &Palette) {
    let Some((sx, sy, cx, cy)) = app.marquee else {
        return;
    };
    // 没拖动起来就不画，避免单击空白时闪一下
    let s = |v: i32| (v as f32 * layout.scale).round() as i32;
    if (cx - sx).abs() <= s(2) && (cy - sy).abs() <= s(2) {
        return;
    }
    let r = rect(sx.min(cx), sy.min(cy), sx.max(cx), sy.max(cy));
    let canvas = Canvas::new(hdc);
    // 半透明淡蓝填充 + 稍深的蓝色描边。填充走 AlphaBlend，底下的
    // 文件名与图标仍然透得出来——这就是资源管理器框选的观感。
    canvas.fill_alpha(r, palette.marquee_fill, 0x60);
    canvas.border(r, palette.marquee_border);
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
        layout,
        hover,
        pressed,
    };

    canvas.fill(layout.titlebar, palette.titlebar_bg);

    // ---- 标签 ----
    let count = app.tabs.len();
    let active = app.active_tab;
    for i in 0..count {
        let r = layout.tab_rect(i, count);
        let is_active = i == active;
        // 悬停在关闭叉上也算悬停在这个标签上：`hover` 只有一个命中，
        // 鼠标一进叉子的热区就变成 `TabClose`，标签底色会跟着消失——看起来像
        // 预选中被「点没了」。这里把两种命中都算作悬停。
        let is_hover = match hover {
            Hit::Tab(k) | Hit::TabClose(k) => k == i,
            _ => false,
        };

        // 悬停底色与标题栏的最小化/最大化按钮同色（`button_hover`），
        // 同一行标题栏上不能出现两种「悬停深度」。
        let bg = if is_active {
            palette.tab_active
        } else if is_hover {
            palette.button_hover
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
            draw_close_glyph(&canvas, cr, palette, bg, p.is_hover(Hit::TabClose(i)));
        }
    }

    // ---- 新建标签按钮 ----
    let nt = layout.new_tab_rect(count);
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
            // 四个按钮用同一套底色：关闭按钮不再是系统红底，
            // 否则标题栏右端会和标签/其它按钮的悬停深度不一致。
            let color = if prs {
                palette.button_pushed
            } else {
                palette.button_hover
            };
            canvas.fill(r, color);
        }
        draw_window_glyph(&canvas, r, hit, palette);
    }
}

fn draw_window_glyph(canvas: &Canvas, r: RECT, hit: Hit, palette: &Palette) {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    // 悬停时底色是浅一档的灰，字形一律保持正文色；
    // 以前关闭按钮配深红底才需要反白，现在不需要了。
    let color = palette.text;

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

fn draw_close_glyph(canvas: &Canvas, r: RECT, palette: &Palette, bg: u32, hot: bool) {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    // 悬停：叉号外面垫一个深色**圆**，叉号反白。
    //
    // 这里踩过两次坑，别再改回去：
    // 1) 曾经用 `round_fill(r, radius, ..)` 想画圆底，但 `round_fill` 现在按直角
    //    绘制（圆角是历史遗留参数，见 paint.rs），结果是一块近黑实心方块；
    // 2) 干脆不画底之后，叉号虽然在，却少了「这里可以点」的那圈提示。
    // 所以现在用 `circle_fill` 真画圆，并把标签当前底色传进去做边缘混合，
    // 抗锯齿不会出现锯齿黑边。叉号用 `tab_active` 反色，深浅主题都够醒目。
    let color = if hot {
        let radius = ((r.right - r.left).min(r.bottom - r.top) / 2 - 1).max(7);
        canvas.circle_fill(cx, cy, radius, palette.text, bg);
        palette.tab_active
    } else {
        palette.text_dim
    };
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
        layout,
        hover,
        pressed,
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
    let fl = crate::ui::favbar::compute(app);

    for slot in &fl.slots {
        let idx = slot.index;
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
        let r = slot.rect;

        if p.is_hover(Hit::FavoriteItem(idx)) || app.fav_drag == Some(idx) {
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

        // 书签目录右侧留出一个小三角的位置：提示「点它会展开里面」，
        // 一眼就能和普通书签区别开
        let marker_w = if is_folder { p.s(11) } else { 0 };
        let tr = rect(
            r.left + p.s(6) + isz + p.s(4),
            r.top,
            r.right - p.s(6) - marker_w,
            r.bottom,
        );
        canvas.text(&text, tr, palette.text, font, TEXT_L);
        if is_folder {
            canvas.text(
                "\u{25B8}",
                rect(tr.right, r.top, tr.right + marker_w, r.bottom),
                palette.text_dim,
                font,
                TEXT_C,
            );
        }
    }

    // 放不下的部分折叠到末尾的箭头里
    if fl.has_overflow() {
        let r = fl.overflow;
        if p.is_hover(Hit::FavoriteMore) {
            canvas.round_fill(r, p.s(3), palette.item_hover, None);
        }
        canvas.text("\u{00BB}", r, palette.text, font, TEXT_C);
    }

    // 拖动排序时画一条落点指示线
    if app.fav_drag.is_some() {
        if let Some(dx) = app.fav_drop {
            let x = dx.clamp(layout.favbar.left + p.s(4), layout.favbar.right - p.s(4));
            canvas.vline(
                x,
                layout.favbar.top + p.s(4),
                layout.favbar.bottom - p.s(4),
                palette.accent,
            );
        }
    }
}

// ------------------------------------------------------------------ 工具栏

/// 工具栏按钮定义。
///
/// 与原软件 `tool_*.svg` 的对应关系保留在名字里，便于日后对齐图标资源。
/// 注意：原软件的「截图/录屏」（tool_screencapture，外部 360ScreenCapture.exe）
/// 与「知识」（tool_knowledge）两个入口属于产品推广/附加功能，本重写不迁移。
/// 工具栏按钮。第一列是 Segoe MDL2 Assets 的字形码点，第二列是名称。
///
/// 原版用的是自带 svg；这里改用系统字体，码点是私用区的单个字符，
/// 不必往仓库里塞图片资源，换 DPI 也不会糊。名称留给提示条和无障碍。
pub const TOOLBAR_ITEMS: [(&str, &str); 10] = [
    ("\u{E710}", "新建"),
    ("\u{E8F4}", "新建文件夹"),
    ("\u{E8C6}", "剪切"),
    ("\u{E8C8}", "复制"),
    ("\u{E77F}", "粘贴"),
    ("\u{E74D}", "删除"),
    ("\u{E8AC}", "重命名"),
    ("\u{E890}", "视图"),
    ("\u{E721}", "搜索"),
    ("\u{E840}", "快速访问"),
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
        layout,
        hover,
        pressed,
    };

    canvas.fill(layout.toolbar, palette.toolbar_bg);

    // 图标用 Segoe MDL2 Assets，16px 居中；按钮热区仍是 36×36。
    // 几何全部来自 `layout.toolbar_button_rect`，命中测试用的是同一份，
    // 不会出现「画在这里、点在那里」。
    let font_icon = app.fonts.icon(p.s(16));
    for i in 0..TOOLBAR_ITEMS.len() {
        let r = layout.toolbar_button_rect(i);
        if r.right > layout.toolbar.right - p.s(8) {
            break;
        }
        if p.is_hover(Hit::ToolbarButton(i)) {
            canvas.round_fill(r, p.s(4), palette.item_hover, None);
        }
        canvas.text(TOOLBAR_ITEMS[i].0, r, palette.text, font_icon, TEXT_C);
    }

    // 底部 1px 分隔线（对应 ToolContainerBorder）
    canvas.hline(
        layout.toolbar.left,
        layout.toolbar.right,
        layout.toolbar.bottom - 1,
        palette.border_light,
    );
}

/// 工具栏的悬停提示气泡。
///
/// 单独一层、在所有区域之后画：提示条会盖到导航栏甚至文件列表上，
/// 如果在 `paint_toolbar` 里就画，后面的区域会把它整块刷掉。
/// 文字用正文字体——图标字体（Segoe MDL2）里没有汉字，拿它画只会是方框。
fn paint_tooltip(
    app: &mut Application,
    hdc: HDC,
    layout: &Layout,
    palette: &Palette,
) {
    if !app.tooltip_ready {
        return;
    }
    let Hit::ToolbarButton(i) = app.hover else {
        return;
    };
    let Some((_, label)) = TOOLBAR_ITEMS.get(i) else {
        return;
    };
    let canvas = Canvas::new(hdc);
    let font = app.font_ui();
    draw_tooltip(
        layout,
        &canvas,
        palette,
        layout.toolbar_button_rect(i),
        label,
        font,
    );
}

/// 工具栏的悬停提示气泡：按钮正下方一条圆角小条，居中，贴着窗口右边时往回缩。
fn draw_tooltip(
    layout: &Layout,
    canvas: &Canvas,
    palette: &Palette,
    anchor: RECT,
    text: &str,
    font: HFONT,
) {
    let s = |v: i32| (v as f32 * layout.scale).round() as i32;
    let pad = s(8);
    let h = s(24);
    let w = canvas.text_width(text, font) + pad * 2;
    // 以按钮中线为中心摆，但不许越出客户区
    let mut left = (anchor.left + anchor.right - w) / 2;
    if left + w > layout.client.right - s(4) {
        left = layout.client.right - s(4) - w;
    }
    left = left.max(layout.client.left + s(4));
    let top = anchor.bottom + s(2);
    let bubble = rect(left, top, left + w, top + h);
    canvas.round_fill(bubble, s(4), palette.header_bg, Some(palette.border));
    canvas.text(text, bubble, palette.text, font, TEXT_C);
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
        layout,
        hover,
        pressed,
    };

    canvas.fill(layout.navbar, palette.nav_bg);

    // ---- 导航按钮 ----
    // 用 Segoe MDL2 Assets 的字形而不是手绘线条：资源管理器用的是同一套，
    // 笔画粗细和比例天然一致，高 DPI 下还是矢量。
    let can_back = app.can_go_back();
    let can_forward = app.can_go_forward();
    let can_up = app.can_go_up();
    let font_icon = app.fonts.icon(p.s(16));

    for (r, hit, glyph, enabled) in [
        (layout.btn_back, Hit::Back, ICON_BACK, can_back),
        (layout.btn_forward, Hit::Forward, ICON_FORWARD, can_forward),
        (layout.btn_up, Hit::Up, ICON_UP, can_up),
    ] {
        if hover == hit && enabled {
            canvas.round_fill(r, p.s(4), palette.item_hover, None);
        }
        // 到不了的方向压暗，但仍然画出来——位置固定才好形成肌肉记忆
        let color = if enabled { palette.text } else { palette.text_dim };
        canvas.text(glyph, r, color, font_icon, TEXT_C);
    }

    // ---- 地址栏 ----
    let addr = layout.address;
    canvas.round_fill(
        addr,
        p.s(3),
        palette.address_bg,
        Some(palette.address_border),
    );

    // 地址栏就是一个普通文本框：平时也显示完整路径，点一下即可编辑
    {
        let font = app.font_ui();
        // 文本区、裁切区、插入符换算共用同一个矩形：右边界差 4px，
        // 滚动到末尾时最后一个字和光标之间就会空出一块。
        let tr = layout.address_text_rect();
        let text = if app.address_editing {
            app.address_text.clone()
        } else {
            app.address_display_text()
        };
        // 选区高亮与插入符都按真实字宽定位，和「点哪落哪」共用同一份像素
        // 布局：把中文折算成两个半角单位只是近似，比例字体下半角字符彼此
        // 也不等宽，误差会顺着路径累积，光标于是越走越偏。
        // 只在编辑态才去量——非编辑态既没有插入符也没有选区，逐帧量一遍
        // 几十个字符前缀是白费。
        let (xs, shift) = if app.address_editing {
            app.address_layout()
        } else {
            (Vec::new(), 0)
        };
        let save = canvas.clip(tr);
        if let Some((a, b)) = app.address_sel {
            let (s0, s1) = (a.min(b), a.max(b));
            if app.address_editing && s1 > s0 {
                let at = |i: usize| xs.get(i).copied().unwrap_or(tr.right);
                canvas.fill(
                    rect(at(s0), addr.top + p.s(4), at(s1), addr.bottom - p.s(4)),
                    palette.item_selected,
                );
            }
        }
        // 文本比框宽时整体左移（`shift`），让插入符留在框内。编辑态用超宽
        // 矩形 + 裁剪，非编辑态保留省略号（长路径平时也能看出被截断了）。
        let (draw_r, flags) = if app.address_editing {
            (edit_text_rect(tr.left - shift, tr), TEXT_EDIT)
        } else {
            (tr, TEXT_L)
        };
        canvas.text(&text, draw_r, palette.text, font, flags);
        if app.address_editing {
            let caret_x = xs.get(app.address_caret).copied().unwrap_or(tr.right - 2);
            if caret_visible() {
                canvas.vline(caret_x, addr.top + p.s(6), addr.bottom - p.s(6), palette.text);
            }
            // IME 锚点跟着插入符走（闪烁的隐相也要保持位置）
            app.ime_anchor = Some((caret_x, addr.top + p.s(6)));
        }
        canvas.unclip(save);
    }

    // 刷新按钮
    if hover == Hit::Refresh {
        canvas.round_fill(layout.btn_refresh, p.s(4), palette.item_hover, None);
    }
    canvas.text(
        ICON_REFRESH,
        layout.btn_refresh,
        palette.text,
        font_icon,
        TEXT_C,
    );

    // ---- 搜索框 ----
    let sr = layout.search;
    canvas.round_fill(sr, p.s(3), palette.address_bg, Some(palette.address_border));

    // 搜索框：放大镜 + 输入区。不再放模式标签——搜索统一走系统搜索，
    // 留一个「快速」字样只会让人以为这里还有别的模式可选。
    let font = app.font_ui();
    let glyph_r = rect(sr.left + p.s(4), sr.top, sr.left + p.s(22), sr.bottom);
    canvas.text(ICON_SEARCH, glyph_r, palette.text_dim, font_icon, TEXT_C);

    let er = rect(glyph_r.right + p.s(2), sr.top, sr.right - p.s(8), sr.bottom);
    let t = app.search_text().to_string();
    // 插入符同样按真实字宽量前缀：搜索词里的中文是双宽，按固定字宽算会
    // 直接偏到前一个字中间；词比框宽时整体左移，让光标留在框内。
    let caret_px = if app.search_focused {
        let prefix: String = t.chars().take(app.search_caret()).collect();
        canvas.text_width(&prefix, font)
    } else {
        0
    };
    let shift = if app.search_focused {
        (er.left + caret_px - (er.right - 2)).max(0)
    } else {
        0
    };
    if t.is_empty() && !app.search_focused {
        canvas.text(S::SEARCH_HINT, er, palette.text_dim, font, TEXT_L);
    } else {
        let save = canvas.clip(er);
        let (draw_r, flags) = if app.search_focused {
            (edit_text_rect(er.left - shift, er), TEXT_EDIT)
        } else {
            (er, TEXT_L)
        };
        canvas.text(&t, draw_r, palette.text, font, flags);
        // 获焦时画插入符，按系统光标闪烁间隔明灭
        if app.search_focused {
            if caret_visible() {
                canvas.vline(
                    er.left + caret_px - shift,
                    sr.top + p.s(6),
                    sr.bottom - p.s(6),
                    palette.text,
                );
            }
            app.ime_anchor = Some((er.left + caret_px - shift, sr.top + p.s(6)));
        }
        canvas.unclip(save);
    }

    canvas.hline(
        layout.navbar.left,
        layout.navbar.right,
        layout.navbar.bottom - 1,
        palette.border_light,
    );
}

// ------------------------------------------------------------------ 导航窗格

/// 左侧导航窗格：目录树。
///
/// 行的内容与矩形全部来自 `navpane::build`——同一份数据也供命中测试使用，
/// 所以这里只管把给定的行画出来，不重复推演层级和缩进。
fn paint_nav_pane(
    app: &mut Application,
    hdc: HDC,
    layout: &Layout,
    palette: &Palette,
    hover: Hit,
    pressed: Hit,
) {
    let pane = layout.nav_pane;
    if pane.right <= pane.left || pane.bottom <= pane.top {
        return;
    }

    let canvas = Canvas::new(hdc);
    let p = Painter {
        layout,
        hover,
        pressed,
    };

    canvas.fill(pane, palette.nav_bg);
    // 与内容区的分界线
    canvas.vline(pane.right - 1, pane.top, pane.bottom, palette.border_light);

    // 先取完所有 &mut app 的图标句柄，再借 nav_rows —— 否则借用冲突
    if app.nav_rows.is_empty() {
        return;
    }
    // 当前标签的位置，用来决定哪一行高亮
    let active_path = app.active().map(|t| t.path.clone()).unwrap_or_default();
    let active_is_computer = app.active().map(|t| t.is_computer).unwrap_or(false);
    let active_is_recycle = app.active().map(|t| t.is_recycle).unwrap_or(false);
    let focused = app.active_tab_focused;

    let font = app.font_ui();
    let scale = layout.scale;
    let s = |v: i32| (v as f32 * scale).round() as i32;
    let icon_px = s(16);

    // 容量文字用小一号的字：它只是一行辅助信息，不该和名称抢宽度
    let font_cap = app.fonts.get(s(11), false);
    let row_h = s(metrics::NAV_ROW_H);

    // 每行一个图标键：已知文件夹/驱动器按真实路径取，虚拟项按 CSIDL 取。
    // 先克隆出键再问 IconCache——否则 &app.nav_rows 和 &mut app.icons 借用打架。
    let keys: Vec<Option<crate::fs::entry::IconKey>> =
        app.nav_rows.iter().map(nav_icon_key).collect();
    let icons: Vec<isize> = keys
        .iter()
        .map(|k| match k {
            Some(k) => app.icons.get(k, IconSize::Small) as isize,
            None => 0,
        })
        .collect();

    let rows = &app.nav_rows;

    // 滚动后部分行会越出窗格上下沿：交给 GDI 裁掉，免得画到工具栏/状态栏上
    let clip = canvas.clip(pane);

    // 被拖动的那一组：它的分隔线要画成高亮，其余分隔线照常
    let dragged = app.nav_sep_drag.as_ref().map(|d| d.section);

    for (i, row) in rows.iter().enumerate() {
        // 完全在可视区外的行不必画（滚出上沿的那几行、以及长列表的下半截）
        if row.rect.bottom <= pane.top || row.rect.top >= pane.bottom {
            continue;
        }
        if let NavKind::Separator { section } = row.kind {
            let cy = row.rect.top + (row.rect.bottom - row.rect.top) / 2;
            let active = dragged == Some(section) || hover == Hit::NavSeparator(i);
            // 线的左右两端各缩进一点，别顶到窗格边缘；命中区仍是整行
            let (l, r) = (row.rect.left + s(10), row.rect.right);
            if active {
                // 高亮时画粗一档、用强调色：让人看出「这条线能抓」
                canvas.hline(l, r, cy, palette.accent);
                canvas.hline(l, r, cy + 1, palette.accent);
            } else {
                canvas.hline(l, r, cy, palette.border_light);
            }
            continue;
        }

        // 空快捷访问区的提示行：没有图标、不可点击，只是一句说明
        if let NavKind::Hint { text, .. } = &row.kind {
            let tr = rect(
                row.rect.left + s(4),
                row.rect.top,
                row.rect.right - s(4),
                row.rect.top + row_h,
            );
            canvas.text(text, tr, palette.text_dim, font, TEXT_L);
            continue;
        }

        let selected = match &row.kind {
            NavKind::Computer => active_is_computer,
            NavKind::RecycleBin => active_is_recycle,
            NavKind::Known { path, .. }
            | NavKind::Place { path, .. }
            | NavKind::Drive { path }
            | NavKind::TagItem { path, .. } => !active_is_computer && *path == active_path,
            _ => false,
        };

        if selected {
            canvas.round_fill(
                row.rect,
                s(3),
                if focused {
                    palette.item_selected
                } else {
                    palette.item_selected_inactive
                },
                None,
            );
        } else if hover == Hit::NavItem(i) && app.nav_drag.is_none() {
            canvas.round_fill(row.rect, s(3), palette.item_hover, None);
        }

        // 展开箭头占位：**每一行都留**，哪怕这一行没有子项。
        // 以前只有「此电脑」「网络」留位，其余行的图标就比它们靠左一截，
        // 一列看下去参差不齐。现在统一从箭头右侧起排，顺便给窗格左边空出一条。
        let ar = crate::ui::navpane::arrow_rect(row, scale);
        if row.expandable {
            if hover == Hit::NavArrow(i) {
                canvas.round_fill(ar, s(2), palette.item_hover, None);
            }
            draw_expander(&canvas, ar, palette, row.expanded);
        }
        let text_left = ar.right + s(2);

        // 图标（逐行不同：下载/图片/音乐各有自己的图标）
        //
        // 标签分组自己不是路径，没有系统图标可借，于是画一枚色点：
        // 它本来就是「用颜色分类」的东西，色点比一个通用文件夹图标更说明问题。
        let icon_x = text_left;
        let icon_y = row.rect.top + (row_h - icon_px) / 2;
        if let NavKind::Tag { color, .. } = row.kind {
            let d = s(10);
            let dot = rect(
                icon_x + (icon_px - d) / 2,
                icon_y + (icon_px - d) / 2,
                icon_x + (icon_px + d) / 2,
                icon_y + (icon_px + d) / 2,
            );
            canvas.round_fill(dot, d / 2, color, None);
        } else {
            let hicon = *icons.get(i).unwrap_or(&0) as *mut core::ffi::c_void;
            if !hicon.is_null() {
                canvas.icon(hicon, icon_x, icon_y, icon_px, icon_px);
            }
        }

        // 文字
        let label = match &row.kind {
            NavKind::Known { name, .. }
            | NavKind::Place { name, .. }
            | NavKind::Tag { name, .. }
            | NavKind::TagItem { name, .. } => name.clone(),
            NavKind::Computer => S::STATUS_DRIVES.to_string(),
            NavKind::RecycleBin => S::NAV_RECYCLE.to_string(),
            NavKind::Drive { path } => {
                let vol = crate::fs::enumerate::drive_label(path);
                let letter = path.to_string_lossy().trim_end_matches('\\').to_string();
                if vol.is_empty() {
                    letter
                } else {
                    format!("{vol} ({letter})")
                }
            }
            NavKind::Network => S::NAV_NETWORK.to_string(),
            NavKind::Separator { .. } | NavKind::Hint { .. } => {
                unreachable!("分隔线与提示行在上面的分支已 continue")
            }
        };
        // 驱动器行右侧留给容量。两个要点：
        // 1) 文案用紧凑写法（`53.6G/200G`），比「53.6 GB / 200 GB」省一半；
        // 2) 预留宽度按实测像素取，不再写死 78——写死时稍长的数字会被
        //    DT_END_ELLIPSIS 省略成「194.4 GB / 7…」，那正是「容量数字丢了」。
        let cap = row.volume.map(|v| v.compact_text()).unwrap_or_default();
        let cap_w = if cap.is_empty() {
            0
        } else {
            canvas.text_width(&cap, font_cap) + s(6)
        };
        let text_r = rect(
            icon_x + icon_px + s(8),
            row.rect.top,
            row.rect.right - s(4) - cap_w,
            row.rect.top + row_h,
        );
        canvas.text(&label, text_r, palette.text, font, TEXT_L);

        if let Some(vol) = row.volume {
            let cr = rect(
                row.rect.right - s(4) - cap_w,
                row.rect.top,
                row.rect.right - s(4),
                row.rect.top + row_h,
            );
            canvas.text(&cap, cr, palette.text_dim, font_cap, TEXT_R);

            // 名称下面那条容量条；可用量低于 10% 转红，其余蓝色（跟资源管理器一致）。
            // 宽度占满「图标右侧 → 窗格右边」整段。
            // 高度累计：原始 5dp ×3 ×3/4 ×2/3 ×5/4 ×5/4 ×2/3 = 25/16 ≈ 1.56 倍；底边仍留 2dp。
            let bar_left = icon_x + icon_px + s(8);
            let bar_h = (s(5) * 25) / 16;
            let bar = rect(
                bar_left,
                row.rect.bottom - s(2) - bar_h,
                row.rect.right - s(6),
                row.rect.bottom - s(2),
            );
            let ratio = vol.used_ratio();
            canvas.round_fill(bar, s(2), palette.progress_track, None);
            let fill_w = ((bar.right - bar.left) as f32 * ratio).round() as i32;
            if fill_w > 0 {
                let color = if 1.0 - ratio <= 0.10 {
                    palette.progress_low
                } else {
                    palette.progress_fill
                };
                let fr = rect(bar.left, bar.top, bar.left + fill_w.max(s(2)), bar.bottom);
                canvas.round_fill(fr, s(2), color, None);
            }
        }
    }

    // 拖动排序：在当前落点上画一条插入指示线，用户能看出会插到哪两项之间
    if app.nav_drag.is_some() {
        if let Some(dy) = app.nav_drop_y {
            let line = rect(
                pane.left + s(12),
                dy.clamp(pane.top, pane.bottom - s(2)),
                pane.right - s(14),
                dy.clamp(pane.top, pane.bottom - s(2)) + s(2),
            );
            canvas.fill(line, palette.accent);
        }
    }

    // 分组拖动：在被拖的那一组即将落脚的位置画一条通栏指示线
    if let Some(drag) = app.nav_sep_drag.as_ref() {
        if let Some(y) =
            crate::ui::navpane::insertion_y(&app.nav_groups, drag.section, drag.slot)
        {
            let y = y.clamp(pane.top, pane.bottom - s(2));
            canvas.fill(
                rect(pane.left + s(4), y, pane.right - s(6), y + s(2)),
                palette.accent,
            );
        }
    }

    // 纵向滚动条：内容超出窗格高度时才出现，且只在鼠标停在窗格上时显示
    draw_scrollbar(
        &canvas,
        app,
        ScrollPane::Nav,
        pane,
        app.nav_scroll,
        app.nav_max_scroll,
        app.nav_content_h,
        palette,
    );

    canvas.unclip(clip);

    let _ = p;
}

/// 表头上的排序指示三角。
///
/// 位置是「该列标题的正中间、标题上方」：先量出标题文字本身的宽度，
/// 再把宽 `w`、高 `h` 的小三角居中摆在它上面（`up` 为 ∧ 升序，否则 ∨ 降序）。
/// 逐行填水平线段而不是画字符，既不依赖字体里有没有这两个符号，也不会发虚。
fn draw_sort_arrow(canvas: &Canvas, center_x: i32, top: i32, w: i32, h: i32, up: bool, color: u32) {
    let half = (w.max(3) as f32) / 2.0;
    let h = h.max(2);
    for i in 0..h {
        let t = (i as f32 + 0.5) / h as f32;
        // ∧ 顶点在上：从窄到宽；∨ 顶点在下：从宽到窄
        let k = if up { t } else { 1.0 - t };
        let x0 = (center_x as f32 - half * k).round() as i32;
        let x1 = (center_x as f32 + half * k).round() as i32;
        if x1 > x0 {
            canvas.fill(rect(x0, top + i, x1, top + i + 1), color);
        }
    }
}

/// 展开箭头：折叠时 `▶`，展开时 `▼`。用两条斜线拼，不依赖任何图标字体。
fn draw_expander(canvas: &Canvas, r: RECT, palette: &Palette, expanded: bool) {
    let cx = (r.left + r.right) / 2;
    let cy = (r.top + r.bottom) / 2;
    let h = ((r.right - r.left) / 5).max(2);
    let w = ((r.bottom - r.top) / 6).max(1);

    // 用两条水平短线近似三角的两边——比逐像素画更快，肉眼无差别
    for i in 0..=w {
        let half = ((w - i) as f32 / w as f32 * h as f32).round() as i32;
        let (x1, x2) = if expanded {
            (cx - half, cx + half)
        } else {
            (cx - half, cx + half)
        };
        let y = if expanded { cy - w + i } else { cy - w / 2 + i };
        canvas.hline(x1, x2.max(x1 + 1), y, palette.text_dim);
    }
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

    // 空目录 / 空搜索结果
    //
    // 两种「空」必须分开说：搜索还没结束就说「此文件夹为空」，会让人以为
    // 那个文件真的不存在；只有搜索确实结束且一条没命中，才算「没找到」。
    if app.tabs[active].entries.is_empty() {
        let font = app.font_ui();
        let r = rect(vr.left + 16, vr.top + 16, vr.right - 16, vr.bottom - 16);
        let hint = match app.search_state() {
            Some(SearchState::Running) => S::SEARCH_RUNNING,
            Some(SearchState::Done { hits: 0 }) => S::SEARCH_NO_RESULT,
            _ => S::STATUS_EMPTY_FOLDER,
        };
        canvas.text(hint, r, palette.text_dim, font, DT_CENTER | DT_NOPREFIX);
        return;
    }

    // 「此电脑」固定走卡片网格——资源管理器在这里也不给切换视图
    if app.tabs[active].is_computer {
        paint_computer_view(app, &canvas, vr, palette);
        return;
    }

    let view_mode = app.tabs[active].view_mode;
    if view_mode.is_rows() {
        paint_rows_view(app, &canvas, vr, palette);
    } else {
        paint_grid_view(app, &canvas, vr, palette);
    }
}

/// 「此电脑」：按资源管理器的「设备和驱动器」分组画卡片。
///
/// 每张卡片是 图标 + 名称 + 容量进度条 + 「X 可用，共 Y」。
/// 拿不到容量的驱动器（空读卡器、断开的网络盘）只画图标和名称——
/// 补一条 0% 的灰条会让人误以为盘已写满。
fn paint_computer_view(app: &mut Application, canvas: &Canvas, vr: RECT, palette: &Palette) {
    let active = app.active_tab;
    let count = app.tabs[active].entries.len();
    let scroll_y = app.tabs[active].scroll_y;
    let scale = app.layout.scale;
    let s = |v: i32| (v as f32 * scale).round() as i32;
    let g = cards::Grid::compute(vr, scale);

    let font = app.font_ui();
    let font_small = app.fonts.get(s(12), false);
    let icon_drive = app.icons.get(&crate::fs::entry::IconKey::Drive, IconSize::Large);
    let icon_px = s(32);

    // 分组标题带数量：设备和驱动器 (6)
    let title = format!("{} ({})", S::COMPUTER_GROUP_DRIVES, count);
    canvas.text(&title, g.header_rect(vr), palette.text_dim, font, TEXT_L);

    // 逐个取好再借 entries——icons.get 要 &mut self
    let items: Vec<(String, f32, String)> = app.tabs[active]
        .entries
        .iter()
        .map(|e| {
            let ratio = e.volume.map(|v| v.used_ratio()).unwrap_or(-1.0);
            (e.name.clone(), ratio, e.volume_text())
        })
        .collect();

    for (i, (name, ratio, vol_text)) in items.iter().enumerate() {
        let card = g.card_rect(i, scroll_y);
        // 上下都出界就不用画了
        if card.bottom < vr.top || card.top > vr.bottom {
            continue;
        }

        // 选中态：以前这里只画 hover，框选/点选磁盘时看不出任何变化，
        // 看起来就像「磁盘选不中」。配色跟列表里的选中一致。
        if app.tabs[active].selected.contains(&i) {
            let bg = if app.active_tab_focused {
                palette.item_selected
            } else {
                palette.item_selected_inactive
            };
            canvas.fill(card, bg);
        } else if app.hover == Hit::ListItem(i) {
            canvas.fill(card, palette.item_hover);
        }

        // 图标垂直居中于整张卡片
        let icon_top = card.top + (card.bottom - card.top - icon_px) / 2;
        canvas.icon(icon_drive, card.left + s(12), icon_top, icon_px, icon_px);

        let text_left = card.left + s(12) + icon_px + s(10);
        let text_right = card.right - s(12);
        if text_right <= text_left {
            continue;
        }

        // 名称
        let nr = rect(text_left, card.top + s(12), text_right, card.top + s(34));
        canvas.text(name, nr, palette.text, font, TEXT_L);

        // 进度条：有容量才画。
        //
        // 宽度占满「图标右侧 → 卡片右边」整段。
        // 高度累计：原始 6dp ×3 ×3/4 ×2/3 ×5/4 ×5/4 = 75/32 ≈ 2.34 倍；上下各外扩一半，
        // 视觉中心仍落在原来的 45dp 上——名称、容量文字、卡片尺寸都不受影响。
        if *ratio >= 0.0 {
            let bar_h = (s(6) * 75) / 32;
            let cy = card.top + s(45);
            let tr = rect(
                text_left,
                cy - bar_h / 2,
                text_right,
                cy - bar_h / 2 + bar_h,
            );
            canvas.round_fill(tr, s(3), palette.progress_track, None);
            let fill_w = ((tr.right - tr.left) as f32 * ratio).round() as i32;
            if fill_w > 0 {
                let color = if 1.0 - ratio <= 0.10 {
                    palette.progress_low
                } else {
                    palette.progress_fill
                };
                let fr = rect(tr.left, tr.top, tr.left + fill_w.max(s(2)), tr.bottom);
                canvas.round_fill(fr, s(3), color, None);
            }
        }

        // 容量文字
        if !vol_text.is_empty() {
            let cr = rect(
                text_left,
                card.top + s(54),
                text_right,
                card.top + s(76),
            );
            canvas.text(vol_text, cr, palette.text_dim, font_small, TEXT_L);
        }
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
        s(cell_w_cfg).max(icon_px + s(24))
    };
    let cell_h = s(cell_h_cfg).max(icon_px + s(34));

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

            // 图标句柄要先取（icons 要 &mut），条目本身只借不可变引用。
            // 图片文件在大/中图标视图优先画缩略图；没有现成的就发一次
            // 后台解码请求，先用扩展名图标占位。
            let entry = &app.tabs[active].entries[idx];
            let thumb_views = matches!(view_mode, ViewMode::LargeEx | ViewMode::Large | ViewMode::Medium);
            let icon = if thumb_views && entry.is_image() {
                match app.thumbs.get(&entry.full) {
                    Some(h) if h != 0 => h as *mut core::ffi::c_void,
                    _ => {
                        if app.thumbs.request(&entry.full) {
                            app.io.send(crate::io::Job::Thumb {
                                path: entry.full.clone(),
                            });
                        }
                        resolve_icon(&mut app.icons, entry, IconSize::Large, icon_px)
                    }
                }
            } else {
                resolve_icon(&mut app.icons, entry, IconSize::Large, icon_px)
            };
            paint_grid_cell(
                app,
                canvas,
                r,
                entry,
                icon,
                idx,
                view_mode,
                icon_px,
                font,
                font_small,
                show_ext,
                &selection,
                palette,
            );
        }
    }

    // 滚动条
    if max_scroll > 0 {
        draw_scrollbar(canvas, app, ScrollPane::View, vr, scroll, max_scroll, content_h, palette);
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
fn paint_grid_cell(
    app: &Application,
    canvas: &Canvas,
    r: RECT,
    entry: &Entry,
    icon: *mut core::ffi::c_void,
    index: usize,
    view_mode: ViewMode,
    icon_px: i32,
    font: HFONT,
    font_small: HFONT,
    show_ext: bool,
    selection: &std::collections::HashSet<usize>,
    palette: &Palette,
) {
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

    // 焦点框：Ctrl+方向键只移动焦点、不改选择，没有可见标记的话
    // 按下去像是毫无反应。多选时也画，用来指示键盘的起点在哪。
    if app.tabs[app.active_tab].focus == Some(index) && (selection.len() > 1 || !selected) {
        canvas.border(r, palette.text_dim);
    }

    // 单元格内部的内边距同样要跟缩放走：写死像素值时，字号放大后
    // 文字行高还是老样子，名字会被裁掉一截。
    let sc = app.layout.scale;
    let s = |v: i32| (v as f32 * sc).round() as i32;
    let inner = rect(r.left + s(4), r.top + s(4), r.right - s(4), r.bottom - s(4));

    match view_mode {
        ViewMode::Tile | ViewMode::Content => {
            // 图标在左，两行文字在右
            let isz = icon_px.min(inner.bottom - inner.top);
            let ix = inner.left + s(4);
            let iy = inner.top + (inner.bottom - inner.top - isz) / 2;
            canvas.icon(icon, ix, iy, isz, isz);

            let tx = ix + isz + s(6);
            let name = entry.display_name(show_ext).to_string();
            let name_rect = rect(
                tx,
                inner.top + s(4),
                inner.right - s(2),
                inner.top + s(4) + s(20),
            );
            canvas.text(&name, name_rect, palette.text, font, TEXT_L);

            // 类型名在条目落地时就填好了，这里直接用
            let detail_rect = rect(
                tx,
                inner.top + s(24),
                inner.right - s(2),
                inner.top + s(24) + s(18),
            );
            canvas.text(
                &entry.type_name,
                detail_rect,
                palette.text_dim,
                font_small,
                TEXT_L,
            );
        }
        _ => {
            // 图标在上，文字在下居中
            let isz = icon_px.min((inner.bottom - inner.top) - s(28)).max(s(16));
            let cx = (inner.left + inner.right) / 2;
            let iy = inner.top + s(6);
            canvas.icon(icon, cx - isz / 2, iy, isz, isz);

            let name = entry.display_name(show_ext).to_string();
            let name_rect = rect(
                inner.left + s(2),
                iy + isz + s(4),
                inner.right - s(2),
                (iy + isz + s(4) + s(34)).min(inner.bottom),
            );
            canvas.text(&name, name_rect, palette.text, font, TEXT_WRAP_C);
        }
    }
}

/// 行模式（列表 / 详细信息 / 内容）：一行一项，可带列。
fn paint_rows_view(app: &mut Application, canvas: &Canvas, vr: RECT, palette: &Palette) {
    let active = app.active_tab;
    let view_mode = app.tabs[active].view_mode;
    let scale = app.layout.scale;
    let s = |v: i32| (v as f32 * scale).round() as i32;

    let row_h = s(24);
    let total = app.tabs[active].entries.len();
    // 详细信息模式下表头本身也占一行高度，内容高度必须把它算进去：
    // 少算这一行的后果是最后一行永远滚不到位（滚动量在这里被夹回去）。
    let header_h = if view_mode == ViewMode::Detail { row_h } else { 0 };
    let content_h = total as i32 * row_h + header_h;
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
    let content_right = app.layout.content_right();
    // 列宽只依赖布局与设置，与行无关：算一次给表头和所有行共用。
    // 以前在逐行循环里重算，纯属每帧白算 N 遍。
    let cols = app.layout.detail_columns(app.cfg.settings.detail_col_w);

    if show_ext_col {
        let hdr = rect(vr.left, vr.top, content_right, vr.top + row_h);
        canvas.fill(hdr, palette.header_bg);

        // 宽度不够时右侧列整列隐藏（放到可视区外），列头跟着一起藏
        let vis = |i: usize| app.layout.column_visible(cols[i]);

        let sort = app.tabs[active].sort;
        let sort_col = match sort.key {
            SortKey::Name => 0,
            SortKey::Modified => 1,
            SortKey::Kind => 2,
            SortKey::Size => 3,
        };
        // 第 0 列靠左、第 1 列靠右……「靠左」的标题从列左边起排，
        // 「靠右」的从列右边往回排；排序箭头的中心必须跟着这个基准算。
        const HDR: [(usize, &str, u32); 4] = [
            (0, "名称", TEXT_L),
            (1, "修改日期", TEXT_R),
            (2, "类型", TEXT_L),
            (3, "大小", TEXT_R),
        ];
        for (i, label, align) in HDR {
            if i > 0 && !vis(i) {
                continue;
            }
            let (lft, rgt) = cols[i];
            canvas.text(label, rect(lft, hdr.top, rgt, hdr.bottom), palette.text_dim, font, align);
            if i != sort_col {
                continue;
            }
            // 排序指示：标题正中间的正上方一个小三角，∧ 升序、∨ 降序
            let tw = canvas.text_width(label, font);
            let cx = if align == TEXT_R {
                rgt - tw / 2
            } else {
                lft + tw / 2
            };
            let aw = s(7);
            let ah = s(4).max(3);
            draw_sort_arrow(&canvas, cx, hdr.top + s(1), aw, ah, !sort.descending, palette.text_dim);
        }

        canvas.hline(vr.left, content_right, hdr.bottom - 1, palette.border);

        // 列之间的竖线：资源管理器同款，鼠标可以拖它改宽度
        for i in 1..4 {
            if !vis(i) {
                continue;
            }
            let edge = (cols[i - 1].1 + cols[i].0) / 2;
            canvas.vline(edge, hdr.top + s(4), hdr.bottom - s(4), palette.border);
        }
        list_top += row_h;
    }

    for idx in first..last {
        let y = list_top + idx as i32 * row_h - scroll;
        let r = rect(vr.left, y, content_right, y + row_h);
        if r.bottom < list_top || r.top > vr.bottom {
            continue;
        }
        // 超出表头区域的裁剪
        let r = rect(r.left, r.top.max(list_top), r.right, r.bottom);

        let entry = &app.tabs[active].entries[idx];
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

        // 焦点框（与网格视图同一规则）：Ctrl+方向键移动的可见落点
        if app.tabs[active].focus == Some(idx) && (selection.len() > 1 || !selected) {
            canvas.border(r, palette.text_dim);
        }

        let isz = s(16);
        let ix = r.left + s(6);
        let iy = r.top + (r.bottom - r.top - isz) / 2;
        let icon = resolve_icon(&mut app.icons, entry, IconSize::Small, isz);
        canvas.icon(icon, ix, iy, isz, isz);

        if show_ext_col {
            let name_rect = rect(cols[0].0, r.top, cols[0].1 - s(8), r.bottom);
            if let Some((target, text, caret)) = &rename {
                if *target == idx {
                    app.ime_anchor = Some(draw_inline_edit(canvas, name_rect, text, *caret, palette, font));
                } else {
                    let name = entry.display_name(show_ext).to_string();
                    canvas.text(&name, name_rect, palette.text, font, TEXT_L);
                }
            } else {
                let name = entry.display_name(show_ext).to_string();
                canvas.text(&name, name_rect, palette.text, font, TEXT_L);
            }

            let vis = |i: usize| app.layout.column_visible(cols[i]);
            if vis(1) {
                let modified = entry.modified_text();
                canvas.text(&modified, rect(cols[1].0, r.top, cols[1].1 - s(8), r.bottom), palette.text_dim, font, TEXT_R);
            }

            if vis(2) {
                // 类型名（目录是「文件夹」）在条目落地时就填好了
                canvas.text(
                    &entry.type_name,
                    rect(cols[2].0, r.top, cols[2].1 - s(8), r.bottom),
                    palette.text_dim,
                    font,
                    TEXT_L,
                );
            }

            if vis(3) {
                let size = entry.size_text();
                canvas.text(&size, rect(cols[3].0, r.top, cols[3].1 - s(8), r.bottom), palette.text_dim, font, TEXT_R);
            }
        } else {
            let name_rect = rect(ix + isz + s(6), r.top, r.right - s(8), r.bottom);
            if let Some((target, text, caret)) = &rename {
                if *target == idx {
                    app.ime_anchor = Some(draw_inline_edit(canvas, name_rect, text, *caret, palette, font));
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
        draw_scrollbar(canvas, app, ScrollPane::View, vr, scroll, max_scroll, content_h, palette);
    }
}

/// 右侧详细信息窗格。
///
/// 三段结构，和需求一致：上面一块预览、中间一行四个操作按钮、
/// 下面逐行列出文件属性。窗格没打开或没有选中项时也要画，
/// 只是内容留空——不然鼠标划过去会看到一块「不存在」的区域。
fn paint_detail_pane(app: &mut Application, hdc: HDC, layout: &Layout, palette: &Palette) {
    let p = layout.detail_pane;
    if p.right <= p.left || p.bottom <= p.top {
        return;
    }
    let s = |v: i32| (v as f32 * layout.scale).round() as i32;
    let canvas = Canvas::new(hdc);
    let font = app.font_ui();

    canvas.fill(p, palette.nav_bg);
    canvas.vline(p.left, p.top, p.bottom, palette.border);

    // 多选时按第一个选中项预览，和资源管理器一致
    let entry = app
        .active()
        .and_then(|t| t.selected.iter().min().and_then(|&i| t.entries.get(i).cloned()));

    // ---- 上：预览框 ----
    let pv = layout.detail_preview_rect();
    canvas.fill(pv, palette.list_bg);
    canvas.border(pv, palette.border_light);

    // 预览内容按当前选中项生成/复用（见 `Application::ensure_preview`）
    match entry.as_ref() {
        Some(e) => app.ensure_preview(Some(e.full.clone()), &e.ext(), e.size),
        None => app.ensure_preview(None, "", 0),
    }

    if let Some(e) = &entry {
        // 底部留一行放名字，内容区就是其余的部分
        let name_h = s(22);
        let area = rect(
            pv.left + s(6),
            pv.top + s(6),
            pv.right - s(6),
            (pv.bottom - name_h).max(pv.top + s(6)),
        );
        let mut drew = false;

        match app.preview.as_ref().map(|p| &p.kind) {
            Some(crate::ui::preview::PreviewKind::Image(img)) => {
                // 等比缩放居中：宁可留白，也不把照片拉变形
                let (iw, ih) = (img.width().max(1), img.height().max(1));
                let avail_w = (area.right - area.left).max(1);
                let avail_h = (area.bottom - area.top).max(1);
                let k = (avail_w as f32 / iw as f32).min(avail_h as f32 / ih as f32);
                let w = ((iw as f32 * k).round() as i32).clamp(1, avail_w);
                let h = ((ih as f32 * k).round() as i32).clamp(1, avail_h);
                let dst = rect(
                    area.left + (avail_w - w) / 2,
                    area.top + (avail_h - h) / 2,
                    area.left + (avail_w - w) / 2 + w,
                    area.top + (avail_h - h) / 2 + h,
                );
                drew = img.draw_scaled(hdc, dst);
            }
            Some(crate::ui::preview::PreviewKind::Text(lines)) => {
                // 行数完全由预览框高度决定：窗口越矮看得越少，绝不会溢出到
                // 下面的按钮和明细上
                let line_h = s(16);
                let max_lines = (((area.bottom - area.top) / line_h).max(0)) as usize;
                if max_lines > 0 {
                    let font_small = app.fonts.get(s(11), false);
                    let clip = canvas.clip(area);
                    let mut y = area.top;
                    for line in lines.iter().take(max_lines) {
                        canvas.text(
                            line,
                            rect(area.left + s(2), y, area.right - s(2), y + line_h),
                            palette.text,
                            font_small,
                            TEXT_L,
                        );
                        y += line_h;
                    }
                    canvas.unclip(clip);
                    drew = true;
                }
            }
            Some(crate::ui::preview::PreviewKind::Failed(msg)) => {
                let text = msg.clone();
                canvas.text(&text, area, palette.text_dim, font, TEXT_C);
                drew = true;
            }
            _ => {}
        }

        // 有图有字就不画大图标了；预览不了（目录、未知类型）才回到图标
        if !drew {
            let isz = s(64);
            let icon = resolve_icon(&mut app.icons, e, IconSize::Large, isz);
            canvas.icon(
                icon,
                (pv.left + pv.right - isz) / 2,
                (area.top + area.bottom - isz) / 2,
                isz,
                isz,
            );
        }

        let name = e.display_name(app.cfg.settings.show_extension).to_string();
        let name_r = rect(pv.left + s(8), pv.bottom - name_h + s(2), pv.right - s(8), pv.bottom - s(2));
        canvas.text(&name, name_r, palette.text, font, TEXT_C);
    }

    // ---- 中：打开 / 新标签 / 复制路径 / 更多 ----
    let labels = ["打开", "新标签", "复制路径", "更多"];
    let disabled = entry.is_none();
    for (i, r) in layout.detail_button_rects().iter().enumerate() {
        let hot = matches!(app.hover, Hit::DetailButton(k) if k == i) && !disabled;
        let bg = if hot {
            palette.button_hover
        } else {
            palette.header_bg
        };
        let fg = if disabled { palette.text_dim } else { palette.text };
        canvas.round_fill(*r, s(4), bg, Some(palette.border_light));
        canvas.text(labels[i], *r, fg, font, TEXT_C);
    }

    // ---- 两个内容模块：文件明细 / 标签，顺序由 detail_sections 决定 ----
    let sec = crate::ui::detailtags::sections(app);
    let tags = &sec.tags;
    let row_h = s(metrics::DETAIL_ROW_H);
    let pad = s(12);

    // 分隔线：按钮区下沿一条、两个模块之间一条（后者是顺序拖动把手）
    canvas.hline(p.left + pad, p.right - pad, sec.divider_buttons, palette.border_light);
    if sec.divider_mid > sec.divider_buttons + s(4) {
        // hover/拖动时高亮成强调色粗线：让人看出「这条线能抓」
        // （与导航窗格分组分隔线同一套反馈）
        let active = app.detail_sep_drag.is_some() || app.hover == Hit::DetailSeparator(0);
        if active {
            canvas.hline(p.left + pad, p.right - pad, sec.divider_mid, palette.accent);
            canvas.hline(p.left + pad, p.right - pad, sec.divider_mid + 1, palette.accent);
        } else {
            canvas.hline(p.left + pad, p.right - pad, sec.divider_mid, palette.border_light);
        }
    }

    // 模块顺序拖动中：在被拖模块即将落脚的位置画一条通栏指示线
    if let Some(drag) = app.detail_sep_drag.as_ref() {
        let y = crate::ui::detailtags::insertion_y(&sec, drag.slot);
        canvas.fill(
            rect(p.left + pad, y, p.right - pad, y + s(2)),
            palette.accent,
        );
    }

    canvas.text(
        crate::strings::DETAIL_TAGS,
        tags.label,
        palette.text_dim,
        font,
        TEXT_L,
    );
    // 「添加」chip：悬停才点亮，平时只描一条边，免得抢过上面的主按钮
    let add_hot = app.hover == Hit::DetailTagAdd;
    canvas.round_fill(
        tags.add,
        s(3),
        if add_hot {
            palette.button_hover
        } else {
            palette.header_bg
        },
        Some(palette.border_light),
    );
    canvas.text("＋ 添加", tags.add, palette.text, font, TEXT_C);

    if tags.chips.is_empty() {
        // 第一行那格让给提示文字，「添加」chip 已经单独排在下一行居中
        canvas.text(
            crate::strings::TAG_NONE,
            rect(tags.label.right + s(6), tags.label.top, p.right - pad, tags.label.top + row_h),
            palette.text_dim,
            font,
            TEXT_L,
        );
    }
    for (i, c) in tags.chips.iter().enumerate() {
        let Some((name, color)) = app
            .cfg
            .tags
            .list
            .get(c.tag)
            .map(|t| (t.name.clone(), t.color))
        else {
            continue;
        };
        // 悬停整个 chip 一起点亮：叉太小，只亮叉会看不出这一格是活的
        let hot = app.hover == Hit::DetailTagClose(i);
        canvas.round_fill(
            c.chip,
            s(3),
            if hot { palette.button_hover } else { palette.header_bg },
            Some(palette.border_light),
        );
        // 色点 + 名字：和导航窗格里的标签行是同一套视觉
        let d = s(8);
        let dot = rect(
            c.chip.left + s(8),
            c.chip.top + (s(26) - d) / 2,
            c.chip.left + s(8) + d,
            c.chip.top + (s(26) + d) / 2,
        );
        canvas.round_fill(dot, d / 2, color, None);
        canvas.text(
            &name,
            rect(dot.right + s(6), c.chip.top, c.close.left - s(2), c.chip.bottom),
            palette.text,
            font,
            TEXT_L,
        );
        canvas.text(
            "×",
            c.close,
            if hot { palette.accent } else { palette.text_dim },
            font,
            TEXT_C,
        );
    }

    // ---- 文件明细 ----
    let label_w = s(64);
    let mut y = sec.info_top;

    let mut rows: Vec<(&str, String)> = match &entry {
        Some(e) => vec![
            ("文件名", e.name.clone()),
            ("大小", {
                let t = e.size_text();
                if t.is_empty() {
                    "—".to_string()
                } else {
                    format!("{} ({})", t, e.size)
                }
            }),
            ("类型", {
                // 与排序键取同一个来源：列里显示的字符串就是比较用的字符串
                if e.is_dir() {
                    crate::fs::typename::folder().to_string()
                } else if !e.type_name.is_empty() {
                    e.type_name.clone()
                } else {
                    crate::fs::typename::for_ext(&e.ext())
                }
            }),
            ("修改时间", blank_dash(e.modified_text())),
            ("创建时间", blank_dash(e.created_text())),
            ("访问时间", blank_dash(e.accessed_text())),
        ],
        None => vec![("文件名", "未选择".to_string())],
    };
    // 回收站里的条目才有原位置——资源管理器的回收站同样把它列出来，
    // 否则一堆同名文件根本分不清哪个是从哪删的。
    if let Some(origin) = entry.as_ref().and_then(|e| e.origin.as_ref()) {
        rows.push(("原位置", origin.display().to_string()));
    }

    for (label, value) in rows {
        if y + row_h > p.bottom {
            break;
        }
        canvas.text(
            label,
            rect(p.left + pad, y, p.left + pad + label_w, y + row_h),
            palette.text_dim,
            font,
            TEXT_L,
        );
        canvas.text(
            &value,
            rect(p.left + pad + label_w + s(6), y, p.right - pad, y + row_h),
            palette.text,
            font,
            TEXT_L,
        );
        y += row_h;
    }
}

/// 空字符串换成破折号，避免整行留白看不出「取不到这一项」。
fn blank_dash(s: String) -> String {
    if s.is_empty() {
        "—".to_string()
    } else {
        s
    }
}

/// 内联重命名编辑框。
/// 返回插入符的客户区坐标，供调用点回填 IME 锚点（见 `ime_anchor`）
fn draw_inline_edit(
    canvas: &Canvas,
    r: RECT,
    text: &str,
    caret: usize,
    palette: &Palette,
    font: HFONT,
) -> (i32, i32) {
    // 白底 + 强调色边框，与原软件输入框观感一致
    canvas.fill(r, 0x00FF_FFFF);
    canvas.border(r, palette.accent);

    let inner = rect(r.left + 3, r.top, r.right - 3, r.bottom);
    // 插入符按真实字宽量前缀。重命名框里的中文按固定字宽算会偏出半个字，
    // 偏多少还随名字里的字符组合而变，看起来就像光标在乱跳。
    let prefix: String = text.chars().take(caret).collect();
    let caret_px = canvas.text_width(&prefix, font);
    // 名字比框长时整体左移，插入符才不会被裁在框外
    let shift = (inner.left + caret_px - (inner.right - 2)).max(0);

    let save = canvas.clip(inner);
    canvas.text(
        text,
        edit_text_rect(inner.left - shift, inner),
        0x00_00_0000,
        font,
        TEXT_EDIT,
    );
    let caret_x = inner.left + caret_px - shift;
    canvas.vline(caret_x, r.top + 3, r.bottom - 3, palette.text);
    canvas.unclip(save);
    (caret_x, r.top + 3)
}

/// 取条目图标。
///
/// `icon_key` 已经在数据落地时填好了（见 `Application::fill_entry_meta`），
/// 这里只是查缓存——所以参数是 `&mut IconCache` 而不是整个 `&mut Application`：
/// 否则调用点为了拿一个图标句柄就得先把 Entry 克隆一份出来，而绘制本来就
/// 只持有条目的不可变引用（一屏 30 行 × 每行 4 次分配的克隆）。
fn resolve_icon(
    icons: &mut crate::fs::icons::IconCache,
    entry: &Entry,
    size: IconSize,
    px: i32,
) -> *mut core::ffi::c_void {
    // 小图标档的源图只有 16px，界面放大后按 20px 以上绘制就会发糊。
    // 这种情况下改用大图标档（内部取的是 256px 源），缩放后依然清晰。
    let size = if px > 20 { IconSize::Large } else { size };
    icons.get(&entry.icon_key, size)
}

/// 地址栏历史下拉。
///
/// 编辑地址栏时把「最近去过的地方」按当前输入前缀匹配列出来，↓/↑ 选、
/// 回车或点击跳过去。历史本身早在 `cfg::history` 里存着（`addr_history.ini`），
/// 之前一直只写不读——有数据没有入口。
fn paint_addr_suggest(
    app: &mut Application,
    hdc: HDC,
    layout: &Layout,
    palette: &Palette,
    hover: Hit,
) {
    let _ = layout;
    if !app.addr_suggest_open() {
        return;
    }
    let canvas = Canvas::new(hdc);
    let pane = app.addr_suggest_pane_rect();
    let font = app.font_ui();
    let sel = app.addr_suggest_sel;
    let n = app.addr_suggest.len();
    let pad = (8.0 * app.layout.scale).round() as i32;

    canvas.round_fill(pane, 4, palette.header_bg, Some(palette.border));

    for i in 0..n {
        let r = app.addr_suggest_item_rect(i);
        if sel == Some(i) {
            canvas.fill(r, palette.item_selected);
        } else if hover == Hit::AddrSuggest(i) {
            canvas.fill(r, palette.item_hover);
        }
        let text_r = rect(r.left + pad, r.top, r.right - pad, r.bottom);
        canvas.text(&app.addr_suggest[i], text_r, palette.text, font, TEXT_L);
    }
}

/// 鼠标是否停在这个区域里。
fn pane_hovered(app: &Application, vr: RECT) -> bool {
    app.mouse_x >= vr.left
        && app.mouse_x < vr.right
        && app.mouse_y >= vr.top
        && app.mouse_y < vr.bottom
}

/// 画纵向滚动条。两个形态，**同一根柱子只是粗细不同**：
///
/// * 常态——一条细的灰色圆角矩形，只用来提示「还能翻」，几乎不占视觉；
/// * 鼠标停在滑块上（或正按着拖）——同一位置同一根柱子变粗，这时才是可以抓的东西。
///
/// 以前常态画的是两头尖的渐细剖面，逐行算浮点覆盖率再 AlphaBlend，既慢又在
/// 浅色底上显出毛边；改成纯矩形的粗细两档后形态更干净，重绘也不再狂建内存 DC。
///
/// 鼠标不在窗格里时两个形态都不画，和资源管理器一样：滚动条不抢内容。
fn draw_scrollbar(
    canvas: &Canvas,
    app: &Application,
    pane: ScrollPane,
    vr: RECT,
    scroll: i32,
    max_scroll: i32,
    content_h: i32,
    palette: &Palette,
) {
    let dragging = matches!(&app.scroll_drag, Some(d) if d.pane == pane);
    if !dragging && !pane_hovered(app, vr) {
        return;
    }
    let scale = app.layout.scale;
    let s = |v: i32| (v as f32 * scale).round() as i32;
    let Some(bar) = scrollbar_geom(vr, scroll, max_scroll, content_h, scale) else {
        return;
    };

    // 命中区比细线宽一圈：细线只有几个像素，直接按它做命中太难点中
    let on_thumb = rect_contains(bar.hit, app.mouse_x, app.mouse_y)
        && app.mouse_y >= bar.thumb.top - s(2)
        && app.mouse_y <= bar.thumb.bottom + s(2);

    let (w, radius) = if dragging || on_thumb {
        (s(7), s(2))
    } else {
        (s(4), s(1))
    };
    // 右边缘与几何一致：变粗时往左长，右边缘不动，滑块不会「跳」一下
    let right = vr.right - s(3);
    let r = rect(right - w, bar.thumb.top, right, bar.thumb.bottom);
    canvas.round_fill(r, radius, palette.scroll_thumb, None);
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

    // 多选时的体积统计优先；否则按搜索状态说事——搜索时列表要么还是旧的、
    // 要么已被结果替换，光看「N 个项目」根本看不出搜索跑没跑、跑完了没有。
    // 瞬态提示（「已复制路径」这类）最优先，它到点自己回落。
    let text = if let Some(note) = app.status_note.clone() {
        note
    } else if selected > 1 {
        let hint = format_size_hint(app, selected);
        format!("{selected} {}{hint}", S::STATUS_ITEMS_SELECTED)
    } else {
        match app.search_state() {
            Some(SearchState::Running) => S::SEARCH_RUNNING.to_string(),
            Some(SearchState::TimedOut) => S::SEARCH_TIMEOUT.to_string(),
            Some(SearchState::Pending) => S::SEARCH_PRESS_ENTER.to_string(),
            Some(SearchState::Done { hits: 0 }) => S::SEARCH_NO_RESULT.to_string(),
            // 命中的按「匹配项」说，否则和普通目录的「N 个项目」分不清
            Some(SearchState::Done { hits }) => format!("{hits} {}", S::SEARCH_HITS),
            None => format!("{total} {}", S::STATUS_ITEMS),
        }
    };

    let font = app.font_status();
    let tr = rect(r.left + 10, r.top, r.right - 10, r.bottom);
    canvas.text(&text, tr, palette.text_dim, font, TEXT_L);

    // 右侧：排序与视图提示，再往右是两个视图切换按钮
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
    let btns = layout.status_view_buttons();
    let hint = format!("{}  ↑{}", mode.label(), sort_label);
    let hint_r = rect(tr.left, r.top, btns[0].left - 8, r.bottom);
    canvas.text(&hint, hint_r, palette.text_dim, font, TEXT_R);

    let s = |v: i32| (v as f32 * layout.scale).round() as i32;
    let font_icon = app.fonts.icon(s(14));
    for (i, glyph) in [ICON_VIEW_DETAIL, ICON_VIEW_GRID].iter().enumerate() {
        // 当前模式那一个高亮，另一个压暗——资源管理器就是这样标出定位的
        let on = (i == 0 && mode == ViewMode::Detail) || (i == 1 && mode == ViewMode::Large);
        if on {
            canvas.round_fill(btns[i], s(3), palette.item_selected, None);
        }
        let color = if on { palette.text } else { palette.text_dim };
        canvas.text(glyph, btns[i], color, font_icon, TEXT_C);
    }
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

/// 插入符此刻该不该显示。
///
/// 相位取自系统的光标闪烁间隔，用户在控制面板改「光标闪烁速度」也能跟上。
/// 返回 0 或 u32::MAX 是 API 的「取不到」约定，退回默认 530ms。
fn caret_visible() -> bool {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCaretBlinkTime;
    let blink = unsafe { GetCaretBlinkTime() };
    let blink = if blink == 0 || blink == u32::MAX { 530 } else { blink };
    let now = unsafe { GetTickCount() };
    (now / blink) % 2 == 0
}

/// 设置面板。模态：点击落到它身上的都不再往下传递。
///
/// 不铺全屏遮罩：GDI 的纯色填充是不透明的，压一层会把整个界面糊掉，
/// 而半透明要额外走 AlphaBlend。用加重的边框和面板底色区分层级就够了。
fn paint_settings(app: &mut Application, hdc: HDC, layout: &Layout, palette: &Palette) {
    if !app.settings_open {
        return;
    }
    let canvas = Canvas::new(hdc);
    let s = |v: i32| (v as f32 * layout.scale).round() as i32;
    let p = settingspane::panel_rect(app);

    canvas.round_fill(p, s(8), palette.address_bg, Some(palette.border));

    let font = app.font_ui();
    let font_bold = app.fonts.get(s(15), true);
    let font_small = app.fonts.get(s(12), false);
    let font_icon = app.fonts.icon(s(13));

    // 标题
    let title_r = rect(p.left + s(24), p.top + s(16), p.right - s(48), p.top + s(44));
    canvas.text("设置", title_r, palette.text, font_bold, TEXT_L);

    // 关闭按钮
    let cr = settingspane::close_rect(app);
    if app.hover == Hit::SettingsClose {
        canvas.round_fill(cr, s(4), palette.item_hover, None);
    }
    canvas.text("\u{E711}", cr, palette.text, font_icon, TEXT_C);

    for (i, item) in settingspane::ITEMS.iter().enumerate() {
        let r = settingspane::row_rect(app, i);
        let hovered = app.hover == Hit::SettingsItem(i);
        if hovered {
            canvas.round_fill(r, s(4), palette.item_hover, None);
        }

        let lr = rect(r.left + s(8), r.top, r.right - s(240), r.bottom);
        canvas.text(item.label, lr, palette.text, font, TEXT_L);

        if item.id == settingspane::Id::UiScale {
            // 缩放滑块：灰轨道 + 已选段（强调色）+ 圆形滑块，右边跟当前百分比
            let track = settingspane::scale_track_rect(app, i);
            let th = (track.bottom - track.top).max(2);
            let cy = (track.top + track.bottom) / 2;
            canvas.round_fill(track, th / 2, palette.progress_track, None);

            let thumb_x = settingspane::scale_thumb_x(app, i);
            let radius = s(7);
            // 已选段停在滑块左缘：再往右交给圆本身，圆边缘才能跟灰轨道
            // 正确混色，不会在滑块左侧留一圈灰晕。
            let fill_end = (thumb_x - radius).max(track.left);
            if fill_end > track.left {
                let filled = rect(track.left, track.top, fill_end, track.bottom);
                canvas.round_fill(filled, th / 2, palette.accent, None);
            }
            canvas.circle_fill(thumb_x, cy, radius, palette.accent, palette.progress_track);

            let pct = format!("{}%", (app.cfg.settings.ui_scale * 100.0).round() as i32);
            canvas.text(
                &pct,
                settingspane::scale_value_rect(app, i),
                palette.text_dim,
                font_small,
                TEXT_R,
            );
        } else {
            // 复选方框
            let box_sz = s(18);
            let bx = r.right - s(16) - box_sz;
            let by = (r.top + r.bottom) / 2 - box_sz / 2;
            let br = rect(bx, by, bx + box_sz, by + box_sz);
            let on = settingspane::value_of(app, item.id);
            if on {
                canvas.round_fill(br, s(3), palette.accent, None);
                canvas.text("\u{E73E}", br, palette.text_on_accent, font_icon, TEXT_C);
            } else {
                canvas.round_fill(br, s(3), palette.address_bg, Some(palette.border));
            }
        }
    }
}

/// 模态对话框。
///
/// 先压一层半透明黑色遮罩（`AlphaBlend`），再把面板画上去——这是自绘 UI
/// 里唯一能表达「下面那层现在点不动」的方式，而且不会像纯色遮罩那样把
/// 底下的内容糊成一块死色。
fn paint_dialog(app: &mut Application, hdc: HDC, layout: &Layout, palette: &Palette) {
    if app.dialog.is_none() {
        return;
    }
    let canvas = Canvas::new(hdc);
    let s = |v: i32| (v as f32 * layout.scale).round() as i32;

    canvas.fill_alpha(layout.client, 0x000000, 0x60);

    let p = dialog::parts(app);
    canvas.round_fill(p.panel, s(10), palette.address_bg, Some(palette.border));

    let font = app.font_ui();
    let font_bold = app.fonts.get(s(15), true);
    let font_small = app.fonts.get(s(12), false);

    let (title, body, has_input, colors, picked, input_label, input_text, caret) =
        match app.dialog.as_ref() {
            Some(d) => (
                d.title.clone(),
                d.body.clone(),
                d.has_input(),
                d.colors,
                d.picked,
                d.input_label.clone(),
                d.input.clone(),
                d.caret,
            ),
            None => return,
        };

    canvas.text(&title, p.title, palette.text, font_bold, TEXT_L);
    if !body.is_empty() {
        canvas.text(&body, p.body, palette.text, font, TEXT_WRAP_L);
    }

    if has_input {
        canvas.text(&input_label, p.label, palette.text_dim, font_small, TEXT_L);
        canvas.round_fill(
            p.input,
            s(3),
            palette.address_bg,
            Some(palette.address_border),
        );
        let tr = rect(
            p.input.left + s(8),
            p.input.top,
            p.input.right - s(8),
            p.input.bottom,
        );
        // 插入符按真实字宽量前缀：在对话框里输入中文标签名时，按固定单位
        // 估算会偏出半个字。文本比框宽时整体左移，插入符留在框内。
        let prefix: String = input_text.chars().take(caret).collect();
        let caret_px = canvas.text_width(&prefix, font);
        let shift = (tr.left + caret_px - (tr.right - 2)).max(0);
        let save = canvas.clip(tr);
        canvas.text(
            &input_text,
            edit_text_rect(tr.left - shift, tr),
            palette.text,
            font,
            TEXT_EDIT,
        );
        // 插入符：和地址栏同一套相位
        if caret_visible() {
            canvas.vline(
                tr.left + caret_px - shift,
                p.input.top + s(6),
                p.input.bottom - s(6),
                palette.text,
            );
        }
        // 对话框输入框的 IME 锚点
        app.ime_anchor = Some((tr.left + caret_px - shift, p.input.top + s(6)));
        canvas.unclip(save);
    }
    if has_input && colors {
        for (i, rgb) in dialog::TAG_COLORS.iter().enumerate() {
            let r = dialog::color_rect(app, i);
            let cx = (r.left + r.right) / 2;
            let cy = (r.top + r.bottom) / 2;
            let fill = dialog::rgb_to_colorref(*rgb);
            // 选中项外面套一圈：先把外圈画出来，再用色块盖住中间，
            // 剩下的就是环。内圈的抗锯齿底色要用环的颜色，不然边上有灰边。
            if i == picked {
                let ring = palette.text;
                canvas.circle_fill(cx, cy, s(11), ring, palette.address_bg);
                canvas.circle_fill(cx, cy, s(8), fill, ring);
            } else {
                canvas.circle_fill(cx, cy, s(8), fill, palette.address_bg);
            }
        }
    }

    // 按钮：主按钮在右（强调色），次按钮在它左边
    let n = dialog::button_count(app);
    for i in 0..n {
        let r = dialog::button_rect(app, i);
        let is_primary = i == 0;
        let hovered = app.hover == Hit::DialogButton(i);
        if is_primary {
            let fill = if hovered {
                // 悬停：强调色里混一点白，避免另开一个色值
                mix_color(palette.accent, palette.text_on_accent, 0.18)
            } else {
                palette.accent
            };
            canvas.round_fill(r, s(3), fill, None);
            canvas.text(
                dialog::primary_text(app),
                r,
                palette.text_on_accent,
                font,
                TEXT_C,
            );
        } else {
            let fill = if hovered {
                palette.button_hover
            } else {
                palette.address_bg
            };
            canvas.round_fill(r, s(3), fill, Some(palette.address_border));
            canvas.text(S::BTN_CANCEL, r, palette.text, font, TEXT_C);
        }
    }
}

/// 两种颜色按比例混合（`t` = 0 取 a，1 取 b）。用于悬停态的强调色。
fn mix_color(a: u32, b: u32, t: f32) -> u32 {
    let ch = |shift: u32| {
        let x = ((a >> shift) & 0xFF) as f32;
        let y = ((b >> shift) & 0xFF) as f32;
        ((x + (y - x) * t).round() as u32) & 0xFF
    };
    ch(0) | (ch(8) << 8) | (ch(16) << 16)
}

/// 导航行 → 图标键。
///
/// 已知文件夹和驱动器都用真实路径问 Shell，这样「下载/图片/音乐」各自是
/// Windows 那一套图标，U 盘、光驱、网络盘也不会全变成同一个灰硬盘。
fn nav_icon_key(row: &crate::ui::navpane::NavRow) -> Option<crate::fs::entry::IconKey> {
    use crate::fs::entry::IconKey;
    use crate::ui::navpane::NavKind;
    match &row.kind {
        NavKind::Known { path, .. }
        | NavKind::Place { path, .. }
        | NavKind::Drive { path }
        | NavKind::TagItem { path, .. } => Some(IconKey::Path(path.to_string_lossy().into_owned())),
        NavKind::Computer => Some(IconKey::Special(0)),
        NavKind::RecycleBin => Some(IconKey::Special(1)),
        NavKind::Network => Some(IconKey::Special(2)),
        // 标签标题自己画色点，不要系统图标
        NavKind::Tag { .. } | NavKind::Separator { .. } | NavKind::Hint { .. } => None,
    }
}
