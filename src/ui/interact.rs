//! 交互：命中测试、鼠标、键盘。
//!
//! 因为整个界面是自绘的，所有热区都由 `hit_test` 计算。
//! 命中优先级从「最上层、最小」到「最下层、最大」排列。

use crate::app::{Application, SearchMode};
use crate::win::paint::rect_contains;

use super::{winbtn, Hit};

/// 命中测试。
pub fn hit_test(app: &Application, x: i32, y: i32) -> Hit {
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
    if rect_contains(l.new_tab, x, y) {
        return Hit::NewTab;
    }

    // ---- 标签 ----
    let count = app.tabs.len();
    for i in 0..count {
        let r = l.tab_rect(i, count, app.tabs[i].title.chars().count());
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
        if let Some(idx) = favorite_at(app, x, y) {
            return Hit::FavoriteItem(idx);
        }
        return Hit::None;
    }

    // ---- 工具栏 ----
    if l.toolbar.right > l.toolbar.left && rect_contains(l.toolbar, x, y) {
        let s = |v: i32| (v as f32 * l.scale).round() as i32;
        let btn = s(36);
        let gap = s(4);
        let mut bx = l.toolbar.left + s(12);
        let by = l.toolbar.top + (l.toolbar.bottom - l.toolbar.top - btn) / 2;
        for i in 0..12 {
            let r = crate::win::paint::rect(bx, by, bx + btn, by + btn);
            if rect_contains(r, x, y) {
                return Hit::ToolbarButton(i);
            }
            bx += btn + gap;
            if bx > l.toolbar.right {
                break;
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
            return Hit::Address;
        }
        return Hit::None;
    }

    // ---- 文件视图 ----
    if rect_contains(l.view, x, y) {
        if let Some(idx) = list_item_at(app, x, y) {
            return Hit::ListItem(idx);
        }
        return Hit::ViewEmpty;
    }

    // ---- 侧栏与状态栏 ----
    if l.nav_pane.right > l.nav_pane.left && rect_contains(l.nav_pane, x, y) {
        return Hit::NavPane;
    }
    if l.detail_pane.bottom > l.detail_pane.top && rect_contains(l.detail_pane, x, y) {
        return Hit::DetailPane;
    }
    if rect_contains(l.statusbar, x, y) {
        return Hit::Statusbar;
    }

    Hit::None
}

/// 收藏栏里第几项在鼠标下。
fn favorite_at(app: &Application, x: i32, y: i32) -> Option<usize> {
    let l = &app.layout;
    let s = |v: i32| (v as f32 * l.scale).round() as i32;
    let order = app.cfg.bookmarks.ordered();
    let mut bx = l.favbar.left + s(8);
    let font_w = s(8);

    for &idx in &order {
        let item = &app.cfg.bookmarks.items[idx];
        let w = (item.name.chars().count() as i32 * font_w + s(28)).clamp(s(48), s(160));
        if bx + w > l.favbar.right - s(24) {
            break;
        }
        let r = crate::win::paint::rect(
            bx,
            l.favbar.top + s(3),
            bx + w,
            l.favbar.bottom - s(3),
        );
        if rect_contains(r, x, y) {
            return Some(idx);
        }
        bx += w + s(2);
    }
    None
}

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
    app.mouse_x = x;
    app.mouse_y = y;
    let h = hit_test(app, x, y);
    if h != app.hover {
        app.hover = h;
        app.request_repaint();
    }
}

pub fn on_mouse_leave(app: &mut Application) {
    if app.hover != Hit::None {
        app.hover = Hit::None;
        app.request_repaint();
    }
}

pub fn on_lbutton_down(app: &mut Application, x: i32, y: i32) {
    let h = hit_test(app, x, y);
    app.pressed = h;
    app.hover = h;
    app.mouse_x = x;
    app.mouse_y = y;

    // 拖动窗口：标题栏空白处按下
    if h == Hit::Titlebar {
        app.begin_drag();
    }

    app.request_repaint();
}

pub fn on_lbutton_up(app: &mut Application, x: i32, y: i32) {
    let pressed = app.pressed;
    app.pressed = Hit::None;

    let hit = hit_test(app, x, y);
    // 只有按下与抬起在同一热区才算点击
    if hit != pressed {
        app.request_repaint();
        return;
    }

    match hit {
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
        Hit::Address => app.begin_address_edit(),
        Hit::Search => app.focus_search(),
        Hit::FavoriteItem(i) => app.activate_favorite(i),
        Hit::ListItem(i) => {
            let ctrl = app.modifiers.ctrl;
            let shift = app.modifiers.shift;
            app.select_entry(i, ctrl, shift);
        }
        Hit::ViewEmpty => {
            app.clear_selection();
        }
        Hit::ToolbarButton(i) => app.toolbar_action(i),
        _ => {}
    }

    app.request_repaint();
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

    let scroll_amount = if delta > 0 { -60 } else { 60 };
    app.scroll_view(scroll_amount);
}

pub fn on_hwheel(app: &mut Application, delta: i32) {
    // 横向滚轮：文件视图横向滚动（图标模式暂不支持，忽略）
    let _ = (app, delta);
}

/// 跟踪键状态（Ctrl/Shift），供选择逻辑使用。
pub fn update_modifiers(app: &mut Application) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
    // 自行定义虚拟键码，避免依赖 VIRTUAL_KEY 的类型别名差异
    const VK_CONTROL_CODE: i32 = 0x11;
    const VK_SHIFT_CODE: i32 = 0x10;
    unsafe {
        app.modifiers.ctrl = (GetKeyState(VK_CONTROL_CODE) as u16 & 0x8000) != 0;
        app.modifiers.shift = (GetKeyState(VK_SHIFT_CODE) as u16 & 0x8000) != 0;
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

    // 地址栏/搜索框处于编辑态时，按键优先交给输入处理
    if app.address_editing || app.rename_active() {
        return on_edit_key(app, vk);
    }
    if app.search_focused {
        return on_edit_key(app, vk);
    }

    // 快捷键在设置里被禁用时只保留基础导航键
    let hotkeys_on = !app.cfg.settings.disable_all_hotkeys;

    match vk {
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
        VK_SPACE => {
            if app.cfg.settings.enable_quickpreview {
                app.quick_preview_selection();
                true
            } else {
                false
            }
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

        _ => false,
    }
}

/// 文本编辑态按键。
fn on_edit_key(app: &mut Application, vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

    match vk {
        VK_ESCAPE => {
            app.cancel_edit();
            true
        }
        VK_RETURN => {
            app.commit_edit();
            true
        }
        VK_BACK => {
            app.edit_backspace();
            true
        }
        VK_DELETE => {
            app.edit_delete();
            true
        }
        VK_LEFT => {
            app.edit_move_caret(-1);
            true
        }
        VK_RIGHT => {
            app.edit_move_caret(1);
            true
        }
        VK_HOME => {
            app.edit_caret_home();
            true
        }
        VK_END => {
            app.edit_caret_end();
            true
        }
        _ => false,
    }
}

/// 字符输入。
pub fn on_char(app: &mut Application, ch: char) {
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
