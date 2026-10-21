//! 设置面板：自绘的模态对话框。
//!
//! 取代原来的 `MessageBox` 逐项询问——那种做法每改一个开关都要走一轮
//! 「确定/取消」，而且看不到其他项的当前状态。
//!
//! 这里是一个表驱动的面板：`ITEMS` 决定有哪些行、画什么、点了改哪个字段。
//! 加一项设置只需要往表里加一行，绘制和命中自动跟上。

use windows_sys::Win32::Foundation::RECT;

use crate::app::Application;
use crate::cfg::settings::{UI_SCALE_MAX, UI_SCALE_MIN};
use crate::win::paint::{rect, rect_contains};

/// 面板宽（逻辑像素）。
const PANEL_W: i32 = 520;
/// 面板高：头部 + 12 行 + 上下余量。
const PANEL_H: i32 = 614;
/// 一行的高度。
const ROW_H: i32 = 44;
/// 面板内边距。
const PAD: i32 = 24;
/// 标题占的高度。
const HEADER_H: i32 = 56;

/// 设置的标识。
///
/// 用枚举而不是字段名字符串：将来改字段名时编译器会指着这里报错，
/// 而字符串只会在运行时悄悄失效。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Id {
    Favbar,
    Toolbar,
    NavPane,
    DetailPane,
    Extensions,
    Hidden,
    DbClickParent,
    CtrlDbClickTab,
    /// 多标签关窗前弹确认框。
    CloseAlert,
    /// 禁用除基础导航外的所有快捷键。
    Hotkeys,
    /// 深色模式。
    Theme,
    /// 不是开关，而是五档滑块。
    UiScale,
}

/// 一项设置：显示什么、改哪个字段。
pub struct Item {
    pub label: &'static str,
    pub id: Id,
}

/// 面板里的全部行，顺序即显示顺序。
pub const ITEMS: [Item; 12] = [
    Item { label: "深色模式", id: Id::Theme },
    Item { label: "显示书签栏", id: Id::Favbar },
    Item { label: "显示工具栏", id: Id::Toolbar },
    Item { label: "显示导航窗格", id: Id::NavPane },
    Item { label: "显示详细信息窗格", id: Id::DetailPane },
    Item { label: "显示文件扩展名", id: Id::Extensions },
    Item { label: "显示隐藏文件", id: Id::Hidden },
    Item { label: "双击空白返回上级", id: Id::DbClickParent },
    Item { label: "Ctrl+双击在新标签打开", id: Id::CtrlDbClickTab },
    Item { label: "多标签关闭时确认", id: Id::CloseAlert },
    Item { label: "禁用所有快捷键", id: Id::Hotkeys },
    Item { label: "界面缩放", id: Id::UiScale },
];

/// 面板矩形，居中于客户区。
///
/// 缩放档位拉到 300% 以上时，按逻辑尺寸算出来的面板会比客户区还大，
/// 所以这里夹到「客户区减去一圈边距」——否则关闭按钮会被顶到屏幕外。
pub fn panel_rect(app: &Application) -> RECT {
    let c = app.layout.client;
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let margin = s(8).max(4);
    let max_w = ((c.right - c.left) - margin * 2).max(s(200));
    let max_h = ((c.bottom - c.top) - margin * 2).max(s(200));
    let w = s(PANEL_W).min(max_w);
    let h = s(PANEL_H).min(max_h);
    let x = c.left + ((c.right - c.left) - w) / 2;
    let y = c.top + ((c.bottom - c.top) - h) / 2;
    rect(x, y, x + w, y + h)
}

/// 一行的高度。面板被夹小之后行高跟着压缩，保证所有行始终放得下。
fn row_h(app: &Application) -> i32 {
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let p = panel_rect(app);
    let avail = (p.bottom - p.top) - s(HEADER_H) - s(PAD);
    let wanted = s(ROW_H);
    if ITEMS.is_empty() {
        return wanted;
    }
    wanted.min((avail / ITEMS.len() as i32).max(s(18)))
}

/// 第 `i` 行的矩形。
pub fn row_rect(app: &Application, i: usize) -> RECT {
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let p = panel_rect(app);
    let left = p.left + s(PAD);
    let right = p.right - s(PAD);
    let top = p.top + s(HEADER_H) + row_h(app) * i as i32;
    rect(left, top, right, top + row_h(app))
}

/// 关闭按钮的矩形（面板右上角）。
pub fn close_rect(app: &Application) -> RECT {
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let p = panel_rect(app);
    let size = s(32);
    let m = s(8);
    rect(p.right - size - m, p.top + m, p.right - m, p.top + m + size)
}

/// 缩放滑块的可交互区域（只有轨道那一段，不含右侧百分比）。
///
/// 行内其它地方按下去不算拖滑块——那里既不是轨道也没有滑块，
/// 误触会把缩放一下跳到量程尽头；点数字同理，那只是读数。
pub fn scale_slider_rect(app: &Application, i: usize) -> RECT {
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let r = row_rect(app, i);
    let right = r.right - s(8) - s(46) - s(8);
    let left = right - s(240) + s(46) + s(8);
    let cy = (r.top + r.bottom) / 2;
    let half = s(10);
    rect(left, cy - half, right, cy + half)
}

/// 轨道本身：两端各留一个滑块半径，滑块拖到两端时仍完整落在轨道内。
pub fn scale_track_rect(app: &Application, i: usize) -> RECT {
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let a = scale_slider_rect(app, i);
    let cy = (a.top + a.bottom) / 2;
    let radius = s(7);
    let half_h = s(2).max(2);
    rect(a.left + radius, cy - half_h, a.right - radius, cy + half_h)
}

/// 当前缩放值对应的滑块中心 x。
pub fn scale_thumb_x(app: &Application, i: usize) -> i32 {
    let t = scale_track_rect(app, i);
    let span = (t.right - t.left).max(1);
    let ratio = ((app.cfg.settings.ui_scale - UI_SCALE_MIN) / (UI_SCALE_MAX - UI_SCALE_MIN))
        .clamp(0.0, 1.0);
    t.left + (ratio * span as f32).round() as i32
}

/// 右侧百分比文字的位置（行尾，跟轨道留一段空隙）。
pub fn scale_value_rect(app: &Application, i: usize) -> RECT {
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let r = row_rect(app, i);
    let right = r.right - s(8);
    let cy = (r.top + r.bottom) / 2;
    let half = s(10);
    rect(right - s(46), cy - half, right, cy + half)
}

/// 横坐标 → 缩放值（按步进对齐并夹在量程内）。
///
/// `span` 用拖动开始那一刻的轨道范围：缩放一变，面板和轨道都会跟着重排，
/// 要是每一步都拿新轨道来换算，光标底下那根轨道会「跑」，拖到一半就失控。
pub fn scale_from_span(span: (i32, i32), x: i32) -> f32 {
    let (left, right) = span;
    let width = (right - left).max(1);
    let ratio = ((x - left) as f32 / width as f32).clamp(0.0, 1.0);
    crate::cfg::settings::clamp_ui_scale(UI_SCALE_MIN + ratio * (UI_SCALE_MAX - UI_SCALE_MIN))
}

/// 拖动滑块：只更新数值并重绘（滑块跟手），重排与落盘留到松手。
pub fn apply_scale_at_x(app: &mut Application, i: usize, x: i32) {
    let span = match app.scale_drag_span {
        Some(sp) => sp,
        None => {
            let t = scale_track_rect(app, i);
            (t.left, t.right)
        }
    };
    let v = scale_from_span(span, x);
    if (v - app.cfg.settings.ui_scale).abs() < 0.001 {
        return;
    }
    app.cfg.settings.ui_scale = v;
    app.request_repaint();
}

/// 松手：按新数值重排整窗并落盘。
pub fn commit_scale(app: &mut Application) {
    app.recompute_layout();
    app.refresh_active_tab();
    app.request_repaint();
    app.cfg.save_all();
}

/// 命中：面板内第几行，或关闭按钮。
pub fn hit(app: &Application, x: i32, y: i32) -> Option<crate::ui::Hit> {
    if !app.settings_open {
        return None;
    }
    if rect_contains(close_rect(app), x, y) {
        return Some(crate::ui::Hit::SettingsClose);
    }
    for i in 0..ITEMS.len() {
        if rect_contains(row_rect(app, i), x, y) {
            return Some(crate::ui::Hit::SettingsItem(i));
        }
    }
    if rect_contains(panel_rect(app), x, y) {
        return Some(crate::ui::Hit::SettingsPane);
    }
    None
}

/// 读一个开关的当前值。
pub fn value_of(app: &Application, id: Id) -> bool {
    let s = &app.cfg.settings;
    match id {
        Id::Favbar => s.bookmark_visible,
        Id::Toolbar => s.toolbar_visible,
        Id::NavPane => s.show_nav_pane,
        Id::DetailPane => s.show_detail_pane,
        Id::Extensions => s.show_extension,
        Id::Hidden => s.show_hidden,
        Id::DbClickParent => s.dbclick_to_parent,
        Id::CtrlDbClickTab => s.ctrl_dbclick_open_righttab,
        Id::CloseAlert => s.close_alter,
        Id::Hotkeys => s.disable_all_hotkeys,
        Id::Theme => s.theme == crate::cfg::settings::ThemeKind::Dark,
        Id::UiScale => false, // 滑块没有布尔值
    }
}

/// 切换一个开关。会立即生效并落盘。
pub fn toggle(app: &mut Application, id: Id) {
    {
        let s = &mut app.cfg.settings;
        match id {
            Id::Favbar => s.bookmark_visible = !s.bookmark_visible,
            Id::Toolbar => s.toolbar_visible = !s.toolbar_visible,
            Id::NavPane => s.show_nav_pane = !s.show_nav_pane,
            Id::DetailPane => s.show_detail_pane = !s.show_detail_pane,
            Id::Extensions => s.show_extension = !s.show_extension,
            Id::Hidden => s.show_hidden = !s.show_hidden,
            Id::DbClickParent => s.dbclick_to_parent = !s.dbclick_to_parent,
            Id::CtrlDbClickTab => s.ctrl_dbclick_open_righttab = !s.ctrl_dbclick_open_righttab,
            Id::CloseAlert => s.close_alter = !s.close_alter,
            Id::Hotkeys => s.disable_all_hotkeys = !s.disable_all_hotkeys,
            Id::Theme => {
                s.theme = match s.theme {
                    crate::cfg::settings::ThemeKind::Dark => crate::cfg::settings::ThemeKind::Light,
                    crate::cfg::settings::ThemeKind::Light => crate::cfg::settings::ThemeKind::Dark,
                };
            }
            Id::UiScale => {}
        }
    }
    // 主题必须同步到运行时字段：`palette_for_theme` 读的是 app.theme，
    // 只改 cfg 的话界面颜色纹丝不动——「深色模式丢失」就是这么来的。
    app.theme = app.cfg.settings.theme;
    // 系统菜单也跟着切换，否则深色界面会配一套浅色菜单（反之亦然）
    crate::win::init::set_menu_dark(app.theme == crate::cfg::settings::ThemeKind::Dark);
    // 影响布局的几项要立刻重排；其余靠重绘即可
    app.recompute_layout();
    app.refresh_active_tab();
    app.cfg.save_all();
}
