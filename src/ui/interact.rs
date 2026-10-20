//! 交互：命中测试、鼠标、键盘。
//!
//! 因为整个界面是自绘的，所有热区都由 `hit_test` 计算。
//! 命中优先级从「最上层、最小」到「最下层、最大」排列。

use crate::app::{Application, SearchMode};
use crate::win::paint::rect_contains;

use super::{navpane, scrollbar_geom, winbtn, Hit, ScrollBar, ScrollDrag, ScrollPane};

/// 命中测试。
pub fn hit_test(app: &Application, x: i32, y: i32) -> Hit {
    // 对话框是最高一层的模态：打开时其它一切都不参与命中
    if app.dialog.is_some() {
        return super::dialog::hit(app, x, y);
    }
    // 设置面板是模态的：打开时点它外面也算落在面板上，不往下穿
    if app.settings_open {
        return super::settingspane::hit(app, x, y).unwrap_or(Hit::SettingsPane);
    }

    let l = &app.layout;

    // ---- 窗口按钮（最上层）----
    let wb = l.window_buttons;
    if rect_contains(wb, x, y) {
        let bw = (wb.right - wb.left) / winbtn::COUNT as i32;
        if bw > 0 {
            let idx = ((x - wb.left) / bw) as usize;
            return match idx {
                0 => Hit::MenuButton,
                1 => Hit::Minimize,
                2 => Hit::Maximize,
                _ => Hit::Close,
            };
        }
    }

    // ---- 新建标签按钮 ----
    if rect_contains(l.new_tab_rect(app.tabs.len()), x, y) {
        return Hit::NewTab;
    }

    // ---- 标签 ----
    let count = app.tabs.len();
    for i in 0..count {
        let r = l.tab_rect(i, count);
        if rect_contains(r, x, y) {
            // 关闭按钮热区：右端 18 逻辑像素
            let close_w = (18.0 * l.scale).round() as i32;
            if x >= r.right - close_w {
                return Hit::TabClose(i);
            }
            return Hit::Tab(i);
        }
    }

    // ---- 标题栏空白 ----
    if rect_contains(l.titlebar, x, y) {
        return Hit::Titlebar;
    }

    // ---- 收藏栏 ----
    if l.favbar.right > l.favbar.left && rect_contains(l.favbar, x, y) {
        let fl = crate::ui::favbar::compute(app);
        if fl.overflow_at(x, y) {
            return Hit::FavoriteMore;
        }
        if let Some(idx) = fl.item_at(x, y) {
            return Hit::FavoriteItem(idx);
        }
        return Hit::None;
    }

    // ---- 工具栏 ----
    if l.toolbar.right > l.toolbar.left && rect_contains(l.toolbar, x, y) {
        for i in 0..crate::ui::draw::TOOLBAR_ITEMS.len() {
            if rect_contains(l.toolbar_button_rect(i), x, y) {
                return Hit::ToolbarButton(i);
            }
        }
        return Hit::None;
    }

    // ---- 导航栏 ----
    if rect_contains(l.navbar, x, y) {
        for (r, h) in [
            (l.btn_back, Hit::Back),
            (l.btn_forward, Hit::Forward),
            (l.btn_up, Hit::Up),
        ] {
            if rect_contains(r, x, y) {
                return h;
            }
        }
        if rect_contains(l.btn_refresh, x, y) {
            return Hit::Refresh;
        }
        if rect_contains(l.search, x, y) {
            return Hit::Search;
        }
        if rect_contains(l.address, x, y) {
            // 地址栏整条就是一个文本输入框：点哪儿都只是「开始编辑」，
            // 光标按点击位置落位（见 `begin_address_edit_at`）。
            return Hit::Address;
        }
        return Hit::None;
    }

    // ---- 导航窗格分隔条 ----
    // 必须排在文件视图之前：热区跨在 nav.right 上，一半落在 view 里。
    if l.nav_pane.right > l.nav_pane.left && rect_contains(l.nav_splitter(), x, y) {
        return Hit::NavSplitter;
    }

    // ---- 详细信息视图的列分隔条 ----
    // 排在条目命中之前：分隔条压着行的边缘，先让拖动赢。
    if app.active_tab < app.tabs.len()
        && app.tabs[app.active_tab].view_mode == crate::cfg::settings::ViewMode::Detail
        && rect_contains(l.view, x, y)
    {
        let w = app.cfg.settings.detail_col_w;
        for i in 0..3 {
            if rect_contains(l.detail_col_splitter(w, i), x, y) {
                return Hit::DetailCol(i);
            }
        }
    }

    // ---- 右侧详情窗格分隔条 ----
    // 和导航分隔条对称：热区跨在 detail_pane.left 上，必须排在文件视图之前
    if l.detail_pane.right > l.detail_pane.left
        && rect_contains(l.detail_pane_splitter(), x, y)
    {
        return Hit::DetailSplitter;
    }

    // ---- 文件视图 ----
    if rect_contains(l.view, x, y) {
        // 滚动条窄条先接住点击：滑块就压在行的右边缘上，如果让条目命中赢，
        // 按下滑块会变成「选中这一行」，再拖动就成了文件拖放。
        if let Some(bar) = scrollbar_for(app, ScrollPane::View) {
            if rect_contains(bar.hit, x, y) {
                return Hit::ViewScrollbar;
            }
        }
        // 表头先接住点击：点列头按该列排序，而不是选中第一行
        if app.active_tab < app.tabs.len()
            && app.tabs[app.active_tab].view_mode == crate::cfg::settings::ViewMode::Detail
        {
            let s = |v: i32| (v as f32 * l.scale).round() as i32;
            let hdr_h = s(crate::ui::metrics::DETAIL_HEADER_H);
            if y < l.view.top + hdr_h {
                let cols = l.detail_columns(app.cfg.settings.detail_col_w);
                for (i, (lft, rgt)) in cols.iter().enumerate() {
                    if x >= *lft && x < *rgt {
                        return Hit::DetailHeader(i);
                    }
                }
                return Hit::DetailHeader(0);
            }
        }
        if let Some(idx) = list_item_at(app, x, y) {
            return Hit::ListItem(idx);
        }
        return Hit::ViewEmpty;
    }

    // ---- 侧栏与状态栏 ----
    if l.nav_pane.right > l.nav_pane.left && rect_contains(l.nav_pane, x, y) {
        // 滚动条同样排在行之前：不然按下滑块会选中一条快捷项
        if let Some(bar) = scrollbar_for(app, ScrollPane::Nav) {
            if rect_contains(bar.hit, x, y) {
                return Hit::NavScrollbar;
            }
        }
        // 落在具体行上才有意义；空白处仍是 NavPane —— 它吞掉点击，
        // 不让事件穿透到下面的文件列表
        if let Some(i) = navpane::row_at(&app.nav_rows, x, y) {
            let row = &app.nav_rows[i];
            if row.expandable && rect_contains(navpane::arrow_rect(row, l.scale), x, y) {
                return Hit::NavArrow(i);
            }
            // 分隔线单独成一类：它是分组顺序的拖动把手
            if matches!(row.kind, navpane::NavKind::Separator { .. }) {
                return Hit::NavSeparator(i);
            }
            if !matches!(row.kind, navpane::NavKind::Hint { .. }) {
                return Hit::NavItem(i);
            }
        }
        return Hit::NavPane;
    }
    if l.detail_pane.bottom > l.detail_pane.top && rect_contains(l.detail_pane, x, y) {
        for (i, b) in l.detail_button_rects().iter().enumerate() {
            if rect_contains(*b, x, y) {
                return Hit::DetailButton(i);
            }
        }
        // 标签模块：只有「添加」和每行右端的叉是可点的，行本身不响应
        let tags = super::detailtags::compute(app);
        if rect_contains(tags.add, x, y) {
            return Hit::DetailTagAdd;
        }
        for (i, r) in tags.rows.iter().enumerate() {
            if rect_contains(r.close, x, y) {
                return Hit::DetailTagClose(i);
            }
        }
        return Hit::DetailPane;
    }
    if rect_contains(l.statusbar, x, y) {
        // 状态栏不是整块热区：右端两个按钮要单独认出来
        for (i, b) in l.status_view_buttons().iter().enumerate() {
            if rect_contains(*b, x, y) {
                return Hit::ViewToggle(i);
            }
        }
        return Hit::Statusbar;
    }

    Hit::None
}

/// 收藏栏里第几项在鼠标下。
/// 文件视图里第几项在鼠标下。
///
/// 必须与 draw.rs 的排布算法**完全一致**，否则点击会错位。
pub fn list_item_at(app: &Application, x: i32, y: i32) -> Option<usize> {
    let active = app.active_tab;
    if app.tabs.is_empty() {
        return None;
    }
    let entries_len = app.tabs[active].entries.len();
    if entries_len == 0 {
        return None;
    }

    let l = &app.layout;
    let vr = l.view;
    let s = |v: i32| (v as f32 * l.scale).round() as i32;

    // 「此电脑」是卡片网格，几何规则和普通列表完全不同
    if app.tabs[active].is_computer {
        let g = super::cards::Grid::compute(vr, l.scale);
        return g.card_at(x, y, entries_len, app.tabs[active].scroll_y);
    }

    let view_mode = app.tabs[active].view_mode;

    if view_mode.is_rows() {
        let row_h = s(24);
        let mut list_top = vr.top;
        if view_mode == crate::cfg::settings::ViewMode::Detail {
            list_top += row_h; // 表头
        }
        if y < list_top {
            return None;
        }
        let rel = y - list_top + app.tabs[active].scroll_y;
        if rel < 0 {
            return None;
        }
        let idx = (rel / row_h) as usize;
        if idx < entries_len {
            Some(idx)
        } else {
            None
        }
    } else {
        let (cell_w_cfg, cell_h_cfg) = view_mode.cell();
        let icon_px = s(view_mode.icon_size());
        let cell_w = if cell_w_cfg == 0 {
            s(112)
        } else {
            s(cell_w_cfg).max(icon_px + s(24))
        };
        let cell_h = s(cell_h_cfg).max(icon_px + s(34));

        let avail_w = (vr.right - vr.left).max(cell_w);
        let cols = (avail_w / cell_w).max(1) as usize;

        let rel_x = x - vr.left;
        let rel_y = y - vr.top + app.tabs[active].scroll_y;
        if rel_x < 0 || rel_y < 0 {
            return None;
        }
        let col = (rel_x / cell_w) as usize;
        let row = (rel_y / cell_h) as usize;
        if col >= cols {
            return None;
        }
        let idx = row * cols + col;
        if idx < entries_len {
            Some(idx)
        } else {
            None
        }
    }
}

// ------------------------------------------------------------------ 鼠标

pub fn on_mouse_move(app: &mut Application, x: i32, y: i32) {
    // 拖动滚动条：滑块跟手，直接按位置换算滚动量
    if app.scroll_drag.is_some() {
        app.mouse_x = x;
        app.mouse_y = y;
        apply_scroll_drag(app, y);
        return;
    }

    // 拖分组分隔线：整组跟着换位。也没有阈值——分隔线本身不可点，
    // 按下去就只可能是想挪它。光标要跟着走完流程（松手时还要用它算落点）。
    if app.nav_sep_drag.is_some() {
        app.mouse_x = x;
        app.mouse_y = y;
        app.nav_sep_move();
        return;
    }

    // 拖动缩放滑块：跟手即时生效，不设阈值——按下就是明确意图
    if app.dragging_scale {
        if let Some(i) = super::settingspane::ITEMS
            .iter()
            .position(|it| it.id == super::settingspane::Id::UiScale)
        {
            super::settingspane::apply_scale_at_x(app, i, x);
        }
        app.mouse_x = x;
        app.mouse_y = y;
        app.request_repaint();
        return;
    }

    // 收藏栏长按拖动排序：按住超过 400ms 且确实挪了位置，才转成拖动，
    // 否则单击、右键、双击的行为都会被这一下抢走。
    if let Some((idx, px, py, t0)) = app.fav_press {
        let held = t0.elapsed().as_millis() >= 400;
        let moved = (x - px).abs() > 3 || (y - py).abs() > 3;
        if app.fav_drag.is_none() && held && moved {
            app.fav_drag = Some(idx);
        }
        if app.fav_drag.is_some() {
            app.fav_drop = Some(x);
            app.request_repaint();
            return;
        }
    }

    // 导航窗格快捷项拖动排序：越过阈值才认（否则单击会被这一下抢走）
    if let Some((index, px, py)) = app.nav_press {
        if app.nav_drag.is_none() && drag_exceeded(px, py, x, y) {
            app.nav_drag = Some(index);
        }
        if app.nav_drag.is_some() {
            app.nav_drop_y = Some(y);
            app.request_repaint();
            return;
        }
    }

    // 拖分隔条：没有阈值，按下就是明确意图
    if app.resizing_nav {
        set_nav_width(app, x);
        return;
    }
    if app.resizing_detail {
        set_detail_width(app, x);
        return;
    }

    // 拖列宽：往左拖变宽（列右边缘是锚点）
    if let Some((i, sx, sw)) = app.drag_col {
        let scale = if app.layout.scale > 0.1 { app.layout.scale } else { 1.0 };
        let delta = ((x - sx) as f32 / scale).round() as i32;
        let w = (sw - delta).clamp(40, 600);
        if let Some(slot) = app.cfg.settings.detail_col_w.get_mut(i) {
            if *slot != w {
                *slot = w;
                app.request_repaint();
            }
        }
        return;
    }

    // 按住条目拖起来：越过系统阈值就转成真正的拖放，交给 OLE 接管
    if let Some((ox, oy, idx)) = app.drag_origin {
        if drag_exceeded(ox, oy, x, y) {
            app.drag_origin = None;
            start_item_drag(app, idx);
            return;
        }
    }

    // 框选进行中：先把矩形跟到当前位置，再看要不要真的开始选中
    if let Some((sx, sy, _, _)) = app.marquee {
        // 坐标要夹进文件视图：拖到窗口外时矩形贴边停住，
        // 既不会反向翻转，也不会越界盖到导航窗格和工具栏上。
        let (cx, cy) = clamp_to_view(app, x, y);
        app.marquee = Some((sx, sy, cx, cy));
        if drag_exceeded(sx, sy, cx, cy) {
            app.mouse_x = cx;
            app.mouse_y = cy;
            apply_marquee_selection(app);
        }
        app.request_repaint();
        return;
    }

    app.mouse_x = x;
    app.mouse_y = y;
    let h = hit_test(app, x, y);
    if h != app.hover {
        app.hover = h;
        // 工具栏按钮停满 500ms 出提示条（见 `update_hover_tooltip`）
        app.update_hover_tooltip();
        app.request_repaint();
    }
}

/// 把客户区坐标夹进文件视图。
///
/// 框选只属于文件视图：拖出窗口外时矩形贴边停住，而不是反向翻转；
/// 也不会越过分隔条盖住导航窗格、或往上盖住工具栏。
fn clamp_to_view(app: &Application, x: i32, y: i32) -> (i32, i32) {
    let v = app.layout.view;
    (
        x.clamp(v.left, (v.right - 1).max(v.left)),
        y.clamp(v.top, (v.bottom - 1).max(v.top)),
    )
}

/// 拖动分隔条：客户区坐标换回逻辑宽度，夹住上下限后立刻重排。
fn set_nav_width(app: &mut Application, x: i32) {
    let scale = if app.layout.scale > 0.1 {
        app.layout.scale
    } else {
        1.0
    };
    let w = (((x - app.layout.client.left) as f32) / scale).round() as i32;
    let w = w.clamp(140, 900);
    if w == app.cfg.settings.nav_pane_w {
        return;
    }
    app.cfg.settings.nav_pane_w = w;
    app.recompute_layout();
    app.request_repaint();
}

/// 拖动右侧详情窗格的分隔条：客户区坐标换回逻辑宽度。
///
/// 量的是「右边界到鼠标」的距离，所以窗口一缩，窗格宽度不变、文件区先让位。
fn set_detail_width(app: &mut Application, x: i32) {
    let scale = if app.layout.scale > 0.1 {
        app.layout.scale
    } else {
        1.0
    };
    let w = (((app.layout.client.right - x) as f32) / scale).round() as i32;
    let max = (app.layout.client.right - app.layout.client.left) / 2;
    let w = w.clamp(160, (max.max(160)).min(1200));
    if w == app.cfg.settings.detail_pane_w {
        return;
    }
    app.cfg.settings.detail_pane_w = w;
    app.recompute_layout();
    app.request_repaint();
}

/// 取某块内容的滚动条几何（内容没超出时返回 `None`）。
fn scrollbar_for(app: &Application, pane: ScrollPane) -> Option<ScrollBar> {
    let scale = app.layout.scale;
    match pane {
        ScrollPane::View => {
            let (content_h, max_scroll) = app.view_scroll_metrics();
            let scroll = app.active().map(|t| t.scroll_y).unwrap_or(0);
            scrollbar_geom(app.layout.view, scroll, max_scroll, content_h, scale)
        }
        ScrollPane::Nav => scrollbar_geom(
            app.layout.nav_pane,
            app.nav_scroll,
            app.nav_max_scroll,
            app.nav_content_h,
            scale,
        ),
    }
}

/// 按下滚动条：记下抓手位置，并立刻跳到位（点轨道＝把滑块挪到鼠标处）。
fn begin_scroll_drag(app: &mut Application, h: Hit, y: i32) {
    let pane = if h == Hit::ViewScrollbar {
        ScrollPane::View
    } else {
        ScrollPane::Nav
    };
    let Some(bar) = scrollbar_for(app, pane) else {
        return;
    };
    // 按在滑块上保持相对偏移（滑块不会跳到鼠标下面）；按在轨道上就以滑块中心对准
    let grab = if y >= bar.thumb.top && y <= bar.thumb.bottom {
        y - bar.thumb.top
    } else {
        bar.thumb_h / 2
    };
    app.scroll_drag = Some(ScrollDrag { pane, grab, bar });
    apply_scroll_drag(app, y);
    app.request_repaint();
}

/// 把鼠标的纵向位置换算成滚动量并应用。拖动期间每次移动都会走一遍。
fn apply_scroll_drag(app: &mut Application, y: i32) {
    let Some(d) = &app.scroll_drag else {
        return;
    };
    let pane = d.pane;
    let center = y - d.grab + d.bar.thumb_h / 2;
    let v = d.bar.scroll_for_center(center);
    match pane {
        ScrollPane::View => app.set_view_scroll(v),
        ScrollPane::Nav => app.set_nav_scroll(v),
    }
}

/// 移动距离是否超过系统的拖拽阈值。
///
/// 用 `SM_CXDRAG` 而不是写死像素：它跟的是「拖放起始灵敏度」这个系统设置，
/// 手抖的人把阈值调大后，点空白也不会误触发框选。
fn drag_exceeded(x0: i32, y0: i32, x1: i32, y1: i32) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXDRAG, SM_CYDRAG};
    let cx = unsafe { GetSystemMetrics(SM_CXDRAG) }.max(2);
    let cy = unsafe { GetSystemMetrics(SM_CYDRAG) }.max(2);
    (x1 - x0).abs() > cx || (y1 - y0).abs() > cy
}

/// 把条目拖起来交给 OLE。
///
/// 拖动集合的规则和资源管理器一致：拖的项若已在选中集合里，就拖整片；
/// 否则先把它单独选中再拖。
fn start_item_drag(app: &mut Application, idx: usize) {
    let active = app.active_tab;
    if active >= app.tabs.len() {
        return;
    }
    if !app.tabs[active].selected.contains(&idx) {
        app.select_entry(idx, false, false);
    }

    let paths: Vec<std::path::PathBuf> = {
        let t = &app.tabs[active];
        let mut v: Vec<std::path::PathBuf> = t
            .selected
            .iter()
            .filter_map(|&i| t.entries.get(i).map(|e| e.full.clone()))
            .collect();
        // HashSet 是无序的，排一下让拖出的顺序稳定
        v.sort();
        v
    };
    if paths.is_empty() {
        return;
    }

    // OLE 要自己接管鼠标，我们的捕获不松开就收不到松开消息
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
    }
    app.pressed = Hit::None;
    let dropped = crate::win::dragdrop::start(&paths, true);
    app.request_repaint();

    // 拖到收藏栏上松手 → 收藏。
    //
    // 主路径是 IDropTarget 回调留下的标记；下面的落点判定作为兜底，
    // 万一放置目标没注册成功（OLE 未初始化之类）也还能收藏。
    let on_favbar = crate::win::droptarget::take_drop_flag()
        || dropped.is_some_and(|(sx, sy)| {
            use windows_sys::Win32::Foundation::POINT;
            use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
            let mut pt = POINT { x: sx, y: sy };
            let ok = unsafe { ScreenToClient(app.hwnd, &mut pt) };
            ok != 0 && rect_contains(app.layout.favbar, pt.x, pt.y)
        });
    if on_favbar {
        app.add_paths_to_bookmarks(&paths);
        return;
    }

    // 拖到导航窗格上松手 → 加进「快捷访问」区
    //
    // 先取标记再算落点：标记必须先被消费掉，否则会残留到下一次拖动上。
    let nav_flag = crate::win::droptarget::take_nav_drop_flag();
    let on_nav = nav_flag
        || dropped.is_some_and(|(sx, sy)| {
            use windows_sys::Win32::Foundation::POINT;
            use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
            let mut pt = POINT { x: sx, y: sy };
            let ok = unsafe { ScreenToClient(app.hwnd, &mut pt) };
            ok != 0 && rect_contains(app.layout.nav_pane, pt.x, pt.y)
        });
    if on_nav {
        app.add_paths_to_nav_places(&paths);
        return;
    }

    // 拖到文件视图上松手：落在文件夹上就移进去，落在可执行文件上就运行它，
    // 落在空白处就移进当前目录。落点用「松手那一刻」的坐标算。
    let pt = crate::win::droptarget::take_drop_point().or_else(|| {
        dropped.and_then(|(sx, sy)| {
            use windows_sys::Win32::Foundation::POINT;
            use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
            let mut p = POINT { x: sx, y: sy };
            let ok = unsafe { ScreenToClient(app.hwnd, &mut p) };
            (ok != 0).then_some((p.x, p.y))
        })
    });
    if let Some(pt) = pt {
        app.drop_paths_onto_view(paths, pt);
    }
}

/// 把框内（含相交）的条目设为当前选择。
///
/// 只算可见行：框选本来就是屏幕上做的事，滚到视口外的项不该被顺手选中，
/// 顺带也避免了上万条目时的整表遍历。
fn apply_marquee_selection(app: &mut Application) {
    let Some((sx, sy, cx, cy)) = app.marquee else {
        return;
    };
    let vr = app.layout.view;
    // 归一化之后再裁到文件视图：起点终点谁大谁小都成立，
    // 而且框选永远只作用于文件区，不会波及导航窗格与工具栏。
    let box_r = crate::win::paint::rect(
        sx.min(cx).max(vr.left),
        sy.min(cy).max(vr.top),
        sx.max(cx).min(vr.right),
        sy.max(cy).min(vr.bottom),
    );

    if app.tabs.is_empty() {
        return;
    }
    let active = app.active_tab;
    let view_mode = app.tabs[active].view_mode;
    let scroll_y = app.tabs[active].scroll_y;
    let count = app.tabs[active].entries.len();

    // 「此电脑」是卡片网格，几何和列表完全是两套，必须分开取矩形
    let cards = if app.tabs[active].is_computer {
        Some(super::cards::Grid::compute(vr, app.layout.scale))
    } else {
        None
    };


    let mut hits: Vec<usize> = Vec::new();
    for i in 0..count {
        let r = match &cards {
            Some(g) => g.card_rect(i, scroll_y),
            None => app.layout.item_rect(view_mode, i, scroll_y),
        };
        // 完全在视口外就不必算了
        if r.bottom < vr.top || r.top > vr.bottom {
            continue;
        }
        if r.left < box_r.right
            && r.right > box_r.left
            && r.top < box_r.bottom
            && r.bottom > box_r.top
        {
            hits.push(i);
        }
    }

    let t = &mut app.tabs[active];
    t.selected.clear();
    t.selected.extend(hits);
    t.anchor = t.selected.iter().next().copied();
}

pub fn on_mouse_leave(app: &mut Application) {
    if app.hover != Hit::None {
        app.hover = Hit::None;
        // 指针离开窗口：提示条和它的计时都得撤掉
        app.update_hover_tooltip();
        app.request_repaint();
    }
}


pub fn on_lbutton_down(app: &mut Application, x: i32, y: i32) {
    // 对话框是模态的：点击只在它内部生效（选色、定位光标、按按钮）
    if app.dialog.is_some() {
        super::dialog::click(app, x, y);
        return;
    }
    // Ctrl/Shift 只在 WM_KEYDOWN 时更新的话，松开 Ctrl 后状态会一直留着，
    // 于是「松了 Ctrl 再点」还是多选。鼠标消息里重读一次真实键状态。
    update_modifiers(app);
    let h = hit_test(app, x, y);
    app.pressed = h;
    app.hover = h;
    app.mouse_x = x;
    app.mouse_y = y;
    // 真按下去了，提示条的使命就结束了：别让它一直挂在按钮下面
    app.tooltip_ready = false;


    // 编辑态点击别处就退出：以前点了文件列表还停在编辑状态，
    // 得再点一次才回到「未选中」，看着像卡住了。
    if app.address_editing || app.search_focused || app.rename_active() {
        let keep = matches!(h, Hit::Address | Hit::Search)
            || matches!(h, Hit::ListItem(_) if app.rename_active());
        if !keep {
            app.cancel_edit();
        }
    }

    // 收藏项按下：先记账，长按超过阈值才转成拖动排序
    if let Hit::FavoriteItem(idx) = h {
        app.fav_press = Some((idx, x, y, std::time::Instant::now()));
        app.fav_drag = None;
        app.fav_drop = None;
    }

    // 导航窗格的分隔线按下：立刻进入「拖动分组顺序」。它不是可点项，
    // 按下时除了这件事没有别的含义，所以不需要阈值，直接跟手。
    if let Hit::NavSeparator(i) = h {
        if let Some(navpane::NavKind::Separator { section }) =
            app.nav_rows.get(i).map(|r| r.kind.clone())
        {
            app.nav_sep_press(section);
            return;
        }
    }

    // 导航窗格快捷项按下：同样先记账，移动超过系统阈值才转成拖动排序
    if let Hit::NavItem(i) = h {
        if let Some(navpane::NavKind::Place { index, .. }) = app.nav_rows.get(i).map(|r| r.kind.clone())
        {
            app.nav_press = Some((index, x, y));
            app.nav_drag = None;
            app.nav_drop_y = None;
        }
    }

    // 缩放滑块：按在轨道/滑块上就定位并进入拖动；按在行内别处不算
    if let Hit::SettingsItem(i) = h {
        if let Some(item) = super::settingspane::ITEMS.get(i) {
            if item.id == super::settingspane::Id::UiScale
                && rect_contains(super::settingspane::scale_slider_rect(app, i), x, y)
            {
                app.dragging_scale = true;
                let t = super::settingspane::scale_track_rect(app, i);
                app.scale_drag_span = Some((t.left, t.right));
                super::settingspane::apply_scale_at_x(app, i, x);
                app.request_repaint();
                return;
            }
        }
    }

    // 拖分隔条：别的点击逻辑都不该参与
    if h == Hit::NavSplitter {
        app.resizing_nav = true;
        app.request_repaint();
        return;
    }
    if h == Hit::DetailSplitter {
        app.resizing_detail = true;
        app.request_repaint();
        return;
    }

    // 列分隔条按下：记下起点和当前宽度，移动时按差值调整
    if let Hit::DetailCol(i) = h {
        let mut w = app.cfg.settings.detail_col_w.get(i).copied().unwrap_or(0);
        if w <= 0 {
            // 自动宽度：把当前算出来的实际宽度当成拖动起点，
            // 否则第一下会把列直接甩到最小宽度。
            let cols = app.layout.detail_columns(app.cfg.settings.detail_col_w);
            let (lft, rgt) = cols[i + 1];
            let scale = if app.layout.scale > 0.1 { app.layout.scale } else { 1.0 };
            w = ((rgt - lft) as f32 / scale).round().max(40.0) as i32;
        }
        app.drag_col = Some((i, x, w));
        app.request_repaint();
        return;
    }

    // 滚动条：整条轨道都能按，按下即定位，之后进入拖动
    if matches!(h, Hit::ViewScrollbar | Hit::NavScrollbar) {
        begin_scroll_drag(app, h, y);
        return;
    }

    // 拖动窗口：标题栏空白处按下
    if h == Hit::Titlebar {
        app.begin_drag();
    }

    // 空白区按下：先记落点。真拖动起来会转成框选，没拖就是普通点击
    if h == Hit::ViewEmpty {
        app.marquee = Some((x, y, x, y));
    }
    // 条目上按下：记下来，拖过阈值就变拖放，没拖就是单击
    if let Hit::ListItem(i) = h {
        app.drag_origin = Some((x, y, i));
    }

    app.request_repaint();
}

pub fn on_lbutton_up(app: &mut Application, x: i32, y: i32) {
    // 对话框是模态的：只处理它自己的按钮
    if app.dialog.is_some() {
        super::dialog::release(app, x, y);
        return;
    }
    // 滚动条松手：拖动期间已经实时生效，不需要落盘，也不该再走别的落点逻辑
    if app.scroll_drag.take().is_some() {
        app.pressed = Hit::None;
        app.request_repaint();
        return;
    }

    // 缩放滑块松手：拖动期间只跟手改数值，这里才重排整窗并落盘
    if app.dragging_scale {
        app.dragging_scale = false;
        app.scale_drag_span = None;
        app.pressed = Hit::None;
        super::settingspane::commit_scale(app);
        return;
    }

    // 分组分隔线松手：顺序真变了才落盘
    if app.nav_sep_drag.is_some() {
        app.pressed = Hit::None;
        app.nav_sep_release();
        return;
    }

    // 导航窗格快捷项拖动松手：按落点 y 决定插到哪一项前面
    if let Some(idx) = app.nav_drag.take() {
        app.nav_press = None;
        let drop_y = app.nav_drop_y.take().unwrap_or(y);
        app.drop_nav_place(idx, drop_y);
        app.request_repaint();
        return;
    }
    app.nav_press = None;

    // 收藏栏拖动排序松手：落到最近的槽位边界
    if let Some(idx) = app.fav_drag.take() {
        let drop_x = app.fav_drop.take().unwrap_or(x);
        app.fav_press = None;
        app.drop_favorite(idx, drop_x);
        app.request_repaint();
        return;
    }
    app.fav_press = None;

    // 列宽松手：拖动期间已经实时生效，这里只负责落盘
    if let Some(_) = app.drag_col {
        app.drag_col = None;
        app.cfg.save_all();
        app.request_repaint();
        return;
    }

    // 分隔条松手：宽度已经实时生效，这里只负责落盘
    if app.resizing_detail {
        app.resizing_detail = false;
        app.pressed = Hit::None;
        app.cfg.save_all();
        app.request_repaint();
        return;
    }
    if app.resizing_nav {
        app.resizing_nav = false;
        app.pressed = Hit::None;
        app.cfg.save_all();
        app.request_repaint();
        return;
    }

    // 没拖起来就只是一次点击，别留着起点让下次移动误触发
    app.drag_origin = None;

    // 框选收尾优先于点击语义：拖动结束后鼠标多半落在某个条目上，
    // 走下面的点击分支会把它当成单击选中，把刚框出来的一片全丢掉。
    if let Some((sx, sy, _, _)) = app.marquee {
        app.marquee = None;
        app.pressed = Hit::None;
        if !drag_exceeded(sx, sy, x, y) {
            app.clear_selection();
        }
        app.request_repaint();
        return;
    }

    let pressed = app.pressed;
    app.pressed = Hit::None;

    // 抬起时再读一次修饰键：按下与抬起之间用户可能刚好松开了 Ctrl
    update_modifiers(app);

    let hit = hit_test(app, x, y);
    // 只有按下与抬起在同一热区才算点击
    if hit != pressed {
        app.request_repaint();
        return;
    }

    match hit {
        // 点表头：按该列排序（同一列再点一次翻转升降序）
        Hit::DetailHeader(col) => app.sort_by_column(col),
        Hit::Tab(i) => {
            // 中键语义：单击切换
            app.set_active_tab(i);
        }
        Hit::TabClose(i) => {
            app.close_tab(i, false);
        }
        Hit::NewTab => {
            app.new_tab(None);
        }
        Hit::MenuButton => {
            app.show_main_menu();
        }
        Hit::Minimize => app.minimize(),
        Hit::Maximize => app.toggle_maximize(),
        Hit::Close => app.request_close(),
        Hit::Back => app.go_back(),
        Hit::Forward => app.go_forward(),
        Hit::Up => app.go_up(),
        Hit::Refresh => app.refresh_active_tab(),
        Hit::Address => app.begin_address_edit_at(x),
        Hit::Search => app.focus_search(),
        Hit::FavoriteItem(i) => {
            // 长按拖动过：松手时只做排序，不再当成一次点击
            if app.fav_drag.is_none() {
                // 书签目录：点它＝展开，把里面的书签铺成下拉菜单
                let is_folder = app
                    .cfg
                    .bookmarks
                    .items
                    .get(i)
                    .map(|b| b.is_folder)
                    .unwrap_or(false);
                if is_folder {
                    favorite_folder_menu(app, i, x, y);
                } else {
                    app.activate_favorite(i);
                }
            }
        }
        Hit::FavoriteMore => favorite_more_menu(app, x, y),
        Hit::ListItem(i) => {
            let ctrl = app.modifiers.ctrl;
            let shift = app.modifiers.shift;
            app.select_entry(i, ctrl, shift);
        }
        Hit::ViewEmpty => {
            app.clear_selection();
        }
        Hit::ToolbarButton(i) => app.toolbar_action(i),
        Hit::NavItem(i) => app.nav_activate(i),
        Hit::NavArrow(i) => app.nav_toggle(i),
        Hit::DetailButton(i) => detail_button_action(app, i),
        // 标签模块的「添加」：弹出已有标签清单，已归属的带勾，最后一项是新建
        Hit::DetailTagAdd => detail_tag_add_menu(app, x, y),
        // 行右端的叉：把这一项从这个标签里摘掉（不动磁盘上的文件）
        Hit::DetailTagClose(i) => {
            let rows = super::detailtags::compute(app);
            if let Some(r) = rows.rows.get(i) {
                let tag = r.tag;
                app.tag_unassign_target(tag);
            }
        }
        Hit::ViewToggle(0) => app.set_view_mode(crate::cfg::settings::ViewMode::Detail),
        Hit::ViewToggle(1) => app.set_view_mode(crate::cfg::settings::ViewMode::Large),
        Hit::SettingsClose => {
            app.settings_open = false;
        }
        Hit::SettingsItem(i) => {
            if let Some(item) = super::settingspane::ITEMS.get(i) {
                let id = item.id;
                if id != super::settingspane::Id::UiScale {
                    // 滑块不分「开/关」：定位和拖动都在按下/移动里做完了，
                    // 这里要是再 toggle 一次就成了「点一下变回默认值」
                    super::settingspane::toggle(app, id);
                }
            }
        }
        _ => {}
    }

    app.request_repaint();
}

/// 右键抬起：按资源管理器语义先把鼠标下的条目选中，再弹出上下文菜单。
///
/// 空白处右键会清空选择——这样菜单里的「粘贴 / 新建文件夹」等
/// 目标明确的命令不会误作用在之前的选择上。
pub fn on_rbutton_up(app: &mut Application, x: i32, y: i32) {
    // 导航窗格的快捷项也有自己的菜单：那里同样没有「从快捷访问移除」这种命令
    let nav = app.layout.nav_pane;
    if nav.right > nav.left && rect_contains(nav, x, y) {
        if let Some(i) = navpane::row_at(&app.nav_rows, x, y) {
            match app.nav_rows[i].kind.clone() {
                navpane::NavKind::Place { index, .. } => {
                    nav_place_menu(app, index, x, y);
                    return;
                }
                navpane::NavKind::Tag { index, .. } => {
                    nav_tag_menu(app, index, x, y);
                    return;
                }
                navpane::NavKind::TagItem { tag, item, .. } => {
                    nav_tag_item_menu(app, tag, item, x, y);
                    return;
                }
                // 空标签分组上的提示行：右键就是「新建标签」的入口，
                // 否则一个标签都没有时，这里没有任何可点的东西
                navpane::NavKind::Hint {
                    section: crate::cfg::settings::NavSection::Tags,
                    ..
                } => {
                    nav_tags_container_menu(app, x, y);
                    return;
                }
                _ => {}
            }
        }
    }

    // 收藏栏上的图标走自己的菜单：文件系统的上下文菜单里没有「删除书签」
    if app.layout.favbar.right > app.layout.favbar.left
        && rect_contains(app.layout.favbar, x, y)
    {
        let fl = crate::ui::favbar::compute(app);
        if let Some(idx) = fl.item_at(x, y) {
            favorite_item_menu(app, idx, x, y);
            return;
        }
        if fl.overflow_at(x, y) {
            favorite_more_menu(app, x, y);
            return;
        }
        // 空白处：这里只提供「添加书签目录」，别把系统菜单拉出来——
        // 收藏栏不是文件面板，系统菜单里的「新建文件夹/粘贴」在这里没有意义
        favorite_container_menu(app, x, y);
        return;
    }

    if app.active_tab < app.tabs.len() {
        match list_item_at(app, x, y) {
            Some(i) => {
                if !app.tabs[app.active_tab].selected.contains(&i) {
                    app.select_entry(i, false, false);
                }
            }
            None => app.tabs[app.active_tab].selected.clear(),
        }
    }
    app.show_shell_context_menu();
}

/// 详情窗格四个按钮的动作。
///
/// 「打开」走默认关联；「新标签」对目录是进入它、对文件是打开它所在的目录
/// （文件没法在标签里被「打开」，能开的是它的位置）；「复制路径」把完整路径
/// 放进剪贴板；「更多」直接把系统右键菜单拉出来——那是用户真正想要扩展项时
/// 会去的地方。
fn detail_button_action(app: &mut Application, button: usize) {
    let Some(entry) = app
        .active()
        .and_then(|t| t.selected.iter().min().and_then(|&i| t.entries.get(i).cloned()))
    else {
        return;
    };

    match button {
        0 => {
            if let Some(idx) = app
                .active()
                .and_then(|t| t.entries.iter().position(|e| e.full == entry.full))
            {
                app.open_entry(idx);
            }
        }
        1 => {
            let target = if entry.is_dir() {
                entry.full.clone()
            } else {
                entry
                    .full
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| entry.full.clone())
            };
            app.new_tab(Some(target));
        }
        2 => {
            // Shell 那一侧不认 verbatim 前缀，复制出去的路径也顺手擦干净
            let clean = crate::fs::strip_verbatim(&entry.full);
            let text = clean.to_string_lossy().into_owned();
            if crate::fs::shell::set_clipboard_text(&text) {
                app.show_message("已复制路径");
            } else {
                app.show_message("复制失败：剪贴板被占用");
            }
        }
        3 => app.show_shell_context_menu(),
        _ => {}
    }
}

/// 导航窗格快捷项的右键菜单：打开 / 在新标签页打开 / 从快捷访问移除。
///
/// 移除只动这一份列表，磁盘上的目录原封不动——名字里写「从快捷访问移除」
/// 而不是「删除」，就是为了不让用户以为文件要没了。
fn nav_place_menu(app: &mut Application, index: usize, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    };

    const CMD_OPEN: usize = 1;
    const CMD_OPEN_NEW: usize = 2;
    const CMD_PIN: usize = 3;
    const CMD_RENAME: usize = 4;
    const CMD_REMOVE: usize = 5;

    let Some(place) = app.cfg.nav_places.items.get(index).cloned() else {
        return;
    };
    let path = place.path.clone();

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let open = crate::win::to_wide(crate::strings::NAV_QUICK_OPEN);
        let open_new = crate::win::to_wide(crate::strings::NAV_QUICK_OPEN_NEWTAB);
        let pin = crate::win::to_wide(crate::strings::NAV_QUICK_PIN);
        let rename = crate::win::to_wide(crate::strings::NAV_QUICK_RENAME);
        let remove = crate::win::to_wide(&format!(
            "{}「{}」",
            crate::strings::NAV_QUICK_REMOVE,
            place.name
        ));
        AppendMenuW(menu, MF_STRING, CMD_OPEN, open.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_OPEN_NEW, open_new.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_PIN, pin.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_RENAME, rename.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, CMD_REMOVE, remove.as_ptr());

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        // 没有前台归属的弹出菜单，点别处之前会先自己消失
        SetForegroundWindow(app.hwnd);
        // 菜单外壳由 uxtheme 画，弹之前把配色对齐到应用内主题
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        match cmd as usize {
            CMD_OPEN => app.open_dir(path, false),
            CMD_OPEN_NEW => app.open_dir(path, true),
            CMD_PIN => app.pin_nav_place(index),
            CMD_RENAME => app.dialog_prompt(
                crate::strings::NAV_QUICK_RENAME,
                crate::strings::LABEL_NAME,
                &place.name,
                false,
                crate::ui::dialog::DialogIntent::RenameNavPlace(index),
            ),
            CMD_REMOVE => app.remove_nav_place(index),
            _ => {}
        }
        app.request_repaint();
    }
}

/// 导航窗格里「标签」标题行的右键菜单：新建 / 重命名 / 删除。
///
/// 删除要确认：一个标签可能收了几十条，误点就全散了。不过散的只是归类，
/// 磁盘上的文件一个都不动，所以确认文案里要把这句话说出来。
fn nav_tag_menu(app: &mut Application, index: usize, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    };

    const CMD_NEW: usize = 1;
    const CMD_RENAME: usize = 2;
    const CMD_REMOVE: usize = 3;

    let name = app
        .cfg
        .tags
        .list
        .get(index)
        .map(|t| t.name.clone())
        .unwrap_or_default();

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let new_label = crate::win::to_wide(crate::strings::TAG_NEW_TITLE);
        let rename = crate::win::to_wide(&format!(
            "{}「{}」",
            crate::strings::TAG_MENU_RENAME,
            name
        ));
        let remove = crate::win::to_wide(&format!(
            "{}「{}」",
            crate::strings::TAG_MENU_REMOVE,
            name
        ));
        AppendMenuW(menu, MF_STRING, CMD_NEW, new_label.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, CMD_RENAME, rename.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_REMOVE, remove.as_ptr());

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        SetForegroundWindow(app.hwnd);
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        match cmd as usize {
            // 从标签标题新建：新标签先空着，条目靠详情窗格的「添加」或拖进来
            CMD_NEW => app.prompt_new_tag(Vec::new()),
            CMD_RENAME => app.prompt_rename_tag(index),
            CMD_REMOVE => app.confirm_remove_tag(index),
            _ => {}
        }
        app.request_repaint();
    }
}

/// 空标签分组上的右键菜单：只提供「新建标签」。
///
/// 一个标签都没有时导航窗格里只剩一行灰色提示，没有可点的目标，
/// 所以把「第一个标签怎么来」这件事放在这行的右键上。
fn nav_tags_container_menu(app: &mut Application, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    };

    const CMD_NEW: usize = 1;

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let w = crate::win::to_wide(crate::strings::TAG_NEW_TITLE);
        AppendMenuW(menu, MF_STRING, CMD_NEW, w.as_ptr());

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        SetForegroundWindow(app.hwnd);
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        if cmd as usize == CMD_NEW {
            // 新标签先空着：详情窗格的「添加」会把当前项挂进去
            app.prompt_new_tag(Vec::new());
        }
        app.request_repaint();
    }
}

/// 标签里某个条目的右键菜单：打开 / 在新标签页打开 / 从标签移除。
fn nav_tag_item_menu(app: &mut Application, tag: usize, item: usize, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    };

    const CMD_OPEN: usize = 1;
    const CMD_OPEN_NEW: usize = 2;
    const CMD_REMOVE: usize = 3;

    let Some(entry) = app
        .cfg
        .tags
        .list
        .get(tag)
        .and_then(|t| t.items.get(item))
        .cloned()
    else {
        return;
    };

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let open = crate::win::to_wide(crate::strings::NAV_QUICK_OPEN);
        let open_new = crate::win::to_wide(crate::strings::NAV_QUICK_OPEN_NEWTAB);
        let remove = crate::win::to_wide(&format!(
            "{}「{}」",
            crate::strings::TAG_ITEM_REMOVE,
            entry.label()
        ));
        AppendMenuW(menu, MF_STRING, CMD_OPEN, open.as_ptr());
        AppendMenuW(menu, MF_STRING, CMD_OPEN_NEW, open_new.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, CMD_REMOVE, remove.as_ptr());

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        SetForegroundWindow(app.hwnd);
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        let path = entry.path.clone();
        match cmd as usize {
            // 条目既可能是目录也可能是文件：交给统一的「打开位置」逻辑
            CMD_OPEN => app.open_path(path, false),
            CMD_OPEN_NEW => app.open_path(path, true),
            CMD_REMOVE => app.tag_remove_item(tag, item),
            _ => {}
        }
        app.request_repaint();
    }
}

/// 详情窗格「添加」按钮的下拉菜单：列出全部标签，已归属的打勾，最后一项是新建。
///
/// 打勾的项点一下就是「移除」——这比在清单里再找一次「移除」命令顺手，
/// 也和资源管理器的属性页、标签类软件的习惯一致。
fn detail_tag_add_menu(app: &mut Application, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_CHECKED, MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        TPM_TOPALIGN,
    };

    const CMD_NEW: usize = 1;
    /// 已有标签的菜单 id 从这里起排，避开固定项
    const CMD_TAG_BASE: usize = 100;

    // 没有目标（「此电脑」/回收站视图）就没什么可归类的
    let Some(target) = super::detailtags::target_path(app) else {
        return;
    };
    let assigned = super::detailtags::assigned(app);
    let names: Vec<String> = app.cfg.tags.list.iter().map(|t| t.name.clone()).collect();

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for (i, name) in names.iter().enumerate() {
            let flags = if assigned.contains(&i) {
                MF_STRING | MF_CHECKED
            } else {
                MF_STRING
            };
            let w = crate::win::to_wide(name);
            AppendMenuW(menu, flags, CMD_TAG_BASE + i, w.as_ptr());
        }
        if !names.is_empty() {
            AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        }
        let new_label = crate::win::to_wide(&format!("{}…", crate::strings::TAG_NEW_TITLE));
        AppendMenuW(menu, MF_STRING, CMD_NEW, new_label.as_ptr());

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        SetForegroundWindow(app.hwnd);
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        let cmd = cmd as usize;
        if cmd == CMD_NEW {
            app.prompt_new_tag(vec![target]);
        } else if cmd >= CMD_TAG_BASE {
            let index = cmd - CMD_TAG_BASE;
            if assigned.contains(&index) {
                app.tag_unassign_target(index);
            } else {
                app.tag_assign_target(index);
            }
        }
        app.request_repaint();
    }
}

/// 收藏栏图标的右键菜单：打开 / 打开所在位置 / 设为置顶 / 重命名 / 删除书签。
///
/// 之前右键收藏图标走的是文件系统的上下文菜单，那里压根没有「删除书签」，
/// 所以看起来就是右键没反应。这五项对应原软件的 `favoriate_menu.xml`。
fn favorite_item_menu(app: &mut Application, index: usize, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_GRAYED, MF_SEPARATOR, MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
        TPM_TOPALIGN,
    };

    const CMD_OPEN: usize = 1;
    const CMD_LOCATION: usize = 2;
    const CMD_PIN: usize = 3;
    const CMD_RENAME: usize = 4;
    const CMD_REMOVE: usize = 5;

    let Some(item) = app.cfg.bookmarks.items.get(index).cloned() else {
        return;
    };
    // 目录型书签没有对应路径，「打开所在位置」对它没有意义
    let has_path = !item.is_folder && !item.path.as_os_str().is_empty();

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let open = crate::win::to_wide(crate::strings::FAV_OPEN);
        let loc = crate::win::to_wide(crate::strings::FAV_OPEN_LOCATION);
        let pin = crate::win::to_wide(crate::strings::FAV_PIN);
        let rename = crate::win::to_wide(crate::strings::FAV_RENAME);
        let label = crate::win::to_wide(&format!(
            "{}「{}」",
            crate::strings::FAV_DELETE,
            item.name
        ));
        AppendMenuW(menu, MF_STRING, CMD_OPEN, open.as_ptr());
        AppendMenuW(
            menu,
            if has_path { MF_STRING } else { MF_GRAYED },
            CMD_LOCATION,
            loc.as_ptr(),
        );
        AppendMenuW(
            menu,
            // 已经在最前面就没有「置顶」可言
            if item.pinned { MF_GRAYED } else { MF_STRING },
            CMD_PIN,
            pin.as_ptr(),
        );
        AppendMenuW(menu, MF_STRING, CMD_RENAME, rename.as_ptr());
        AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
        AppendMenuW(menu, MF_STRING, CMD_REMOVE, label.as_ptr());

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        // 没有前台归属的弹出菜单，点别处之前会先自己消失
        SetForegroundWindow(app.hwnd);
        // 菜单外壳由 uxtheme 画，弹之前把配色对齐到应用内主题
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        match cmd as usize {
            CMD_OPEN => {
                if item.is_folder {
                    favorite_folder_menu(app, index, x, y);
                } else {
                    app.activate_favorite(index);
                }
            }
            CMD_LOCATION => {
                // 定位到书签所在的文件夹：用当前标签跳过去，而不是再开一个系统窗口
                let parent = item.path.parent().map(|p| p.to_path_buf());
                match parent {
                    Some(p) if !p.as_os_str().is_empty() => app.open_dir(p, false),
                    _ => {
                        let _ = crate::fs::ops::reveal_in_explorer(&item.path);
                    }
                }
            }
            CMD_PIN => {
                app.cfg.bookmarks.pin_at(index);
                app.cfg.save_all();
            }
            CMD_RENAME => app.dialog_prompt(
                crate::strings::RENAME_BOOKMARK_TITLE,
                crate::strings::LABEL_NAME,
                &item.name,
                false,
                crate::ui::dialog::DialogIntent::RenameBookmark(index),
            ),
            CMD_REMOVE => {
                app.cfg.bookmarks.remove_at(index);
                app.cfg.save_all();
            }
            _ => {}
        }
        app.request_repaint();
    }
}

/// 把一组书签递归铺成多级弹出菜单。
///
/// 返回值是子树的菜单句柄；`ids` 记录每个可点条目对应的「下标路径」，
/// 菜单项 id 用 `MENU_ID_BASE + ids 下标` 编码——这样任意层级都能精确回指，
/// 不必为「第几层的第几个」写一堆偏移换算。
unsafe fn build_bookmark_menu(
    items: &[crate::cfg::bookmarks::Bookmark],
    base: &[usize],
    ids: &mut Vec<Vec<usize>>,
) -> Option<windows_sys::Win32::UI::WindowsAndMessaging::HMENU> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, MF_POPUP, MF_STRING,
    };

    let menu = CreatePopupMenu();
    if menu.is_null() {
        return None;
    }
    for (i, b) in items.iter().enumerate() {
        let mut path = base.to_vec();
        path.push(i);
        let label = crate::win::to_wide(&b.name);
        if b.is_folder && !b.children.is_empty() {
            if let Some(sub) = build_bookmark_menu(&b.children, &path, ids) {
                AppendMenuW(menu, MF_POPUP, sub as usize, label.as_ptr());
            }
        } else {
            ids.push(path);
            AppendMenuW(
                menu,
                MF_STRING,
                MENU_ID_BASE + ids.len() - 1,
                label.as_ptr(),
            );
        }
    }
    Some(menu)
}

/// 可点菜单项的 id 起点：固定命令用 1..，书签条目从这里往上排，永不撞车。
const MENU_ID_BASE: usize = 1000;

/// 书签目录的「展开」：把它里面的书签铺成下拉菜单（点别处即收纳）。
fn favorite_folder_menu(app: &mut Application, index: usize, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu, MF_GRAYED,
        MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    };

    let Some(folder) = app.cfg.bookmarks.items.get(index).cloned() else {
        return;
    };
    let mut ids: Vec<Vec<usize>> = Vec::new();

    unsafe {
        // 空目录也要有个菜单：什么都不弹会让人以为右键/点击坏了
        let menu = if folder.children.is_empty() {
            let m = CreatePopupMenu();
            if m.is_null() {
                return;
            }
            let w = crate::win::to_wide(crate::strings::FAV_EMPTY_FOLDER);
            AppendMenuW(m, MF_STRING | MF_GRAYED, 0, w.as_ptr());
            m
        } else {
            match build_bookmark_menu(&folder.children, &[index], &mut ids) {
                Some(m) => m,
                None => return,
            }
        };

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        SetForegroundWindow(app.hwnd);
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        let cmd = cmd as usize;
        if cmd >= MENU_ID_BASE {
            if let Some(path) = ids.get(cmd - MENU_ID_BASE) {
                // 先取出路径再交给 app：`&Bookmark` 借着 cfg，直接调用会借用打架
                let target = app.cfg.bookmarks.get(path).map(|b| b.path.clone());
                if let Some(p) = target {
                    app.open_path(p, false);
                }
            }
        }
        app.request_repaint();
    }
}

/// 收藏栏空白处的右键菜单：添加书签目录。
///
/// 这是原软件 `favoriate_container_menu.xml` 的唯一动作；书签目录建成后是空的，
/// 往里放东西靠「详情窗格/菜单里添加书签」或之后再做拖放。
fn favorite_container_menu(app: &mut Application, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    };

    const CMD_ADD_FOLDER: usize = 1;

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        let w = crate::win::to_wide(crate::strings::FAV_ADD_FOLDER);
        AppendMenuW(menu, MF_STRING, CMD_ADD_FOLDER, w.as_ptr());

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        SetForegroundWindow(app.hwnd);
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        if cmd as usize == CMD_ADD_FOLDER {
            app.dialog_prompt(
                crate::strings::ADD_BOOKMARK_FOLDER_TITLE,
                crate::strings::LABEL_NAME,
                "",
                false,
                crate::ui::dialog::DialogIntent::NewBookmarkFolder,
            );
        }
        app.request_repaint();
    }
}

/// 收藏栏末尾箭头：把折叠起来的书签摊成一个下拉菜单。
fn favorite_more_menu(app: &mut Application, x: i32, y: i32) {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, DestroyMenu, SetForegroundWindow, TrackPopupMenu,
        MF_STRING, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, TPM_TOPALIGN,
    };

    let hidden = crate::ui::favbar::compute(app).hidden;
    if hidden.is_empty() {
        return;
    }

    unsafe {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for (i, &idx) in hidden.iter().enumerate() {
            let name = app
                .cfg
                .bookmarks
                .items
                .get(idx)
                .map(|b| b.name.clone())
                .unwrap_or_default();
            let w = crate::win::to_wide(&name);
            AppendMenuW(menu, MF_STRING, i + 1, w.as_ptr());
        }

        let mut pt = POINT { x, y };
        ClientToScreen(app.hwnd, &mut pt);
        SetForegroundWindow(app.hwnd);
        // 菜单外壳由 uxtheme 画，弹之前把配色对齐到应用内主题
        crate::win::init::apply_menu_theme();
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            app.hwnd,
            std::ptr::null(),
        );
        DestroyMenu(menu);

        let pick = cmd as usize;
        if pick >= 1 && pick <= hidden.len() {
            let idx = hidden[pick - 1];
            // 这里的项可能是书签目录：展开它还得再弹一层，坐标沿用这一次的位置
            let is_folder = app
                .cfg
                .bookmarks
                .items
                .get(idx)
                .map(|b| b.is_folder)
                .unwrap_or(false);
            if is_folder {
                favorite_folder_menu(app, idx, x, y);
            } else {
                app.activate_favorite(idx);
            }
        }
    }
}

pub fn on_lbutton_dblclk(app: &mut Application, x: i32, y: i32) {
    let hit = hit_test(app, x, y);
    match hit {
        Hit::Tab(i) => {
            if app.cfg.settings.dbclick_close_current_tab {
                app.close_tab(i, false);
            }
        }
        Hit::Titlebar => app.toggle_maximize(),
        Hit::ListItem(i) => {
            // Ctrl+双击：右侧新标签打开（对应 ctrl_dbclick_open_righttab）
            if app.modifiers.ctrl && app.cfg.settings.ctrl_dbclick_open_righttab {
                let path = app.entry_path(i);
                if let Some(p) = path {
                    if p.is_dir() {
                        app.new_tab(Some(p));
                        return;
                    }
                }
            }
            app.open_entry(i);
        }
        Hit::ViewEmpty => {
            // 双击空白区域返回上级
            if app.cfg.settings.dbclick_to_parent {
                app.go_up();
            }
        }
        _ => {}
    }
    app.request_repaint();
}

pub fn on_mbutton_up(app: &mut Application, x: i32, y: i32) {
    // 中键点击标签 = 关闭；中键点击文件夹 = 新标签打开
    match hit_test(app, x, y) {
        Hit::Tab(i) => app.close_tab(i, false),
        Hit::ListItem(i) => {
            if let Some(p) = app.entry_path(i) {
                if p.is_dir() {
                    app.new_tab(Some(p));
                }
            }
        }
        _ => {}
    }
    app.request_repaint();
}

pub fn on_mousewheel(app: &mut Application, delta: i32) {
    // 标签栏上滚动切标签（对应 mousewheel_switchtab）
    if app.cfg.settings.mousewheel_switchtab {
        let over_tabs = app.mouse_y < app.layout.tabstrip.bottom
            && app.mouse_y >= app.layout.tabstrip.top;
        if over_tabs && app.tabs.len() > 1 {
            let dir = if delta < 0 { 1 } else { -1 };
            let n = app.tabs.len() as i32;
            let cur = app.active_tab as i32;
            let next = ((cur + dir) % n + n) % n;
            app.set_active_tab(next as usize);
            return;
        }
    }

    // Ctrl+滚轮：在图标尺寸档之间升降，和资源管理器一致
    if app.modifiers.ctrl {
        app.zoom_view(if delta > 0 { 1 } else { -1 });
        return;
    }

    // 鼠标停在导航窗格上：滚的是那棵树（盘符或快捷项多到一屏放不下时要在里面找）
    let nav = app.layout.nav_pane;
    if nav.right > nav.left && rect_contains(nav, app.mouse_x, app.mouse_y) {
        // 一次滚三行：和资源管理器的手感一致，一行一行挪太黏
        let step = (crate::ui::metrics::NAV_ROW_H as f32 * 3.0 * app.layout.scale) as i32;
        app.scroll_nav(if delta > 0 { -step } else { step });
        return;
    }

    let scroll_amount = if delta > 0 { -60 } else { 60 };
    app.scroll_view(scroll_amount);
}

pub fn on_hwheel(app: &mut Application, delta: i32) {
    // 横向滚轮：文件视图横向滚动（图标模式暂不支持，忽略）
    let _ = (app, delta);
}

/// 跟踪键状态（Ctrl/Shift），供选择逻辑使用。
pub fn update_modifiers(app: &mut Application) {
    // 用 `GetAsyncKeyState` 而不是 `GetKeyState`：后者返回的是「线程处理上一条
    // 消息时」的键盘状态，鼠标点击时读到的可能是松开 Ctrl 之前的旧值。那正是
    // 「松开 Ctrl 后再点文件还是多选」的根因。异步版本直接读实时物理状态。
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    // 自行定义虚拟键码，避免依赖 VIRTUAL_KEY 的类型别名差异
    const VK_CONTROL_CODE: i32 = 0x11;
    const VK_SHIFT_CODE: i32 = 0x10;
    unsafe {
        app.modifiers.ctrl = (GetAsyncKeyState(VK_CONTROL_CODE) as u16 & 0x8000) != 0;
        app.modifiers.shift = (GetAsyncKeyState(VK_SHIFT_CODE) as u16 & 0x8000) != 0;
    }
}

// ------------------------------------------------------------------ 键盘

/// 处理按键。返回 true 表示已消费。
///
/// `vk` 是原始虚拟键码（`WM_KEYDOWN` 的 wParam）。
pub fn on_key_down(app: &mut Application, vk_raw: usize) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

    // 统一成 u16，与 VK_* 常量同类型
    let vk = vk_raw as u16;
    update_modifiers(app);

    // 对话框是模态的：按键全部在它内部消化（Enter 确定、Esc 取消、编辑键）
    if app.dialog.is_some() {
        if super::dialog::key_down(app, vk) {
            return true;
        }
        // 对话框不吃这个键时也不往下传——否则打字会顺手触发主界面的快捷键
        return false;
    }

    // 地址栏/搜索框处于编辑态时，按键优先交给输入处理
    if app.address_editing || app.rename_active() {
        return on_edit_key(app, vk);
    }
    if app.search_focused {
        return on_edit_key(app, vk);
    }

    // 快捷键在设置里被禁用时只保留基础导航键
    let hotkeys_on = !app.cfg.settings.disable_all_hotkeys;

    // 字母/数字：快速定位。放在快捷键之前，Ctrl/Alt 组合不受影响。
    if !app.modifiers.ctrl && !app.modifiers.alt {
        if let Some(ch) = type_ahead_char(vk) {
            app.type_ahead(ch);
            return true;
        }
    }

    match vk {
    // ---- Alt 组合 ----
    VK_LEFT if app.modifiers.alt => {
        app.go_back();
        true
    }
    VK_RIGHT if app.modifiers.alt => {
        app.go_forward();
        true
    }
    VK_UP if app.modifiers.alt => {
        app.go_up();
        true
    }
    VK_RETURN if app.modifiers.alt => {
        if let Some(&i) = app.tabs[app.active_tab].selected.iter().next() {
            app.show_properties_entry(i);
        }
        true
    }
        // ---- 基础导航（不受快捷键开关影响）----
        VK_BACK => {
            app.go_up();
            true
        }
        VK_RETURN => {
            if let Some(&i) = app.tabs[app.active_tab].selected.iter().next() {
                app.open_entry(i);
            }
            true
        }
        VK_F5 => {
            app.refresh_active_tab();
            true
        }
        VK_DELETE => {
            app.delete_selection(false);
            true
        }
        VK_F2 => {
            app.begin_rename_selection();
            true
        }
        VK_UP => {
            app.move_selection(-1);
            true
        }
        VK_DOWN => {
            app.move_selection(1);
            true
        }
        VK_HOME => {
            app.select_first();
            true
        }
        VK_END => {
            app.select_last();
            true
        }

        // ---- 需要 Ctrl 的组合 ----
        _ if app.modifiers.ctrl => match vk {
            VK_A => {
                app.select_all();
                true
            }
            VK_C => {
                app.copy_selection();
                true
            }
            VK_X => {
                app.cut_selection();
                true
            }
            VK_V => {
                app.paste_into_active_tab();
                true
            }
            VK_T if hotkeys_on => {
                app.new_tab(None);
                true
            }
            VK_W if hotkeys_on => {
                let cur = app.active_tab;
                app.close_tab(cur, false);
                true
            }
            VK_N if hotkeys_on => {
                app.spawn_new_window();
                true
            }
            VK_L if hotkeys_on => {
                app.begin_address_edit();
                true
            }
            VK_F if hotkeys_on => {
                app.focus_search();
                true
            }
            VK_TAB => {
                app.next_tab(1);
                true
            }
            // Ctrl+1..8 切换视图（快捷键表里的「切换视图」）
            VK_1 => {
                app.set_view_mode(crate::cfg::settings::ViewMode::LargeEx);
                true
            }
            VK_2 => {
                app.set_view_mode(crate::cfg::settings::ViewMode::Large);
                true
            }
            VK_3 => {
                app.set_view_mode(crate::cfg::settings::ViewMode::Medium);
                true
            }
            VK_4 => {
                app.set_view_mode(crate::cfg::settings::ViewMode::Small);
                true
            }
            VK_5 => {
                app.set_view_mode(crate::cfg::settings::ViewMode::List);
                true
            }
            VK_6 => {
                app.set_view_mode(crate::cfg::settings::ViewMode::Detail);
                true
            }
            VK_7 => {
                app.set_view_mode(crate::cfg::settings::ViewMode::Tile);
                true
            }
            VK_8 => {
                app.set_view_mode(crate::cfg::settings::ViewMode::Content);
                true
            }
            _ => false,
        },


        _ => false,
    }
}

/// 文本编辑态按键。
fn on_edit_key(app: &mut Application, vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

    let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) < 0 };
    let shift = unsafe { GetKeyState(VK_SHIFT as i32) < 0 };

    match vk {
        VK_ESCAPE => {
            // 设置面板优先——它是模态的，ESC 先关它
            if app.settings_open {
                app.settings_open = false;
                app.request_repaint();
                return true;
            }
            app.cancel_edit();
            true
        }
        VK_RETURN => {
            app.commit_edit();
            true
        }
        VK_BACK => {
            if ctrl {
                app.edit_backspace_word();
            } else {
                app.edit_backspace();
            }
            true
        }
        VK_DELETE => {
            app.edit_delete();
            true
        }
        VK_LEFT => {
            if ctrl {
                app.edit_move_word(-1, shift);
            } else if shift {
                app.edit_move_caret_shift(-1);
            } else {
                app.edit_move_caret(-1);
            }
            true
        }
        VK_RIGHT => {
            if ctrl {
                app.edit_move_word(1, shift);
            } else if shift {
                app.edit_move_caret_shift(1);
            } else {
                app.edit_move_caret(1);
            }
            true
        }
        VK_HOME => {
            app.edit_caret_edge(false, shift);
            true
        }
        VK_END => {
            app.edit_caret_edge(true, shift);
            true
        }
        VK_A if ctrl => {
            app.edit_select_all();
            true
        }
        VK_C if ctrl => {
            app.edit_copy();
            true
        }
        VK_X if ctrl => {
            app.edit_cut();
            true
        }
        VK_V if ctrl => {
            app.edit_paste();
            true
        }
        _ => false,
    }
}

/// 字符输入。
pub fn on_char(app: &mut Application, ch: char) {
    // 对话框的输入框优先级最高
    if app.dialog.is_some() {
        super::dialog::char_input(app, ch);
        return;
    }
    if app.address_editing || app.search_focused {
        if !ch.is_control() {
            app.edit_insert(ch);
        }
    }
}

/// 视图滚动到使某项可见。
pub fn scroll_into_view(app: &mut Application, index: usize) {
    let active = app.active_tab;
    if app.tabs.is_empty() {
        return;
    }
    let l = &app.layout;
    let s = |v: i32| (v as f32 * l.scale).round() as i32;
    let view_h = l.view.bottom - l.view.top;
    let count = app.tabs[active].entries.len();
    if count == 0 {
        return;
    }

    let view_mode = app.tabs[active].view_mode;
    let (item_top, item_h);

    if view_mode.is_rows() {
        let row_h = s(24);
        let mut list_top = l.view.top;
        if view_mode == crate::cfg::settings::ViewMode::Detail {
            list_top += row_h;
        }
        item_top = list_top + index as i32 * row_h;
        item_h = row_h;
    } else {
        let (cell_w_cfg, cell_h_cfg) = view_mode.cell();
        let icon_px = s(view_mode.icon_size());
        let cell_w = if cell_w_cfg == 0 {
            s(112)
        } else {
            s(cell_w_cfg).max(icon_px + s(24))
        };
        let cell_h = s(cell_h_cfg).max(icon_px + s(34));
        let avail_w = (l.view.right - l.view.left).max(cell_w);
        let cols = (avail_w / cell_w).max(1) as usize;
        let row = index / cols;
        item_top = l.view.top + row as i32 * cell_h;
        item_h = cell_h;
    }

    let item_top_rel = item_top - l.view.top - app.tabs[active].scroll_y;
    if item_top_rel < 0 {
        app.tabs[active].scroll_y += item_top_rel;
    } else if item_top_rel + item_h > view_h {
        app.tabs[active].scroll_y += item_top_rel + item_h - view_h;
    }
}

/// 搜索模式循环切换。
pub fn cycle_search_mode(app: &mut Application) {
    app.search_mode = match app.search_mode {
        SearchMode::Quick => SearchMode::System,
        SearchMode::System => SearchMode::Web,
        SearchMode::Web => SearchMode::Quick,
    };
    app.request_repaint();
}

/// `WM_KEYDOWN` 的 vk → 真实字符。
///
/// 直接查表只能处理美式键盘且拿不到 Shift 状态，走 `ToUnicode` 一次到位。
fn type_ahead_char(vk: u16) -> Option<char> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardState, MapVirtualKeyW, ToUnicode};

    let mut state = [0u8; 256];
    let mut buf = [0u16; 4];
    unsafe {
        if GetKeyboardState(state.as_mut_ptr()) == 0 {
            return None;
        }
        let scan = MapVirtualKeyW(vk as u32, 0);
        let n = ToUnicode(vk as u32, scan, state.as_ptr(), buf.as_mut_ptr(), 4, 0);
        if n <= 0 {
            return None;
        }
        let ch = char::from_u32(buf[0] as u32)?;
        // 只吃可见的字母数字：空格、标点不该抢快捷键
        if ch.is_alphanumeric() {
            Some(ch)
        } else {
            None
        }
    }
}
