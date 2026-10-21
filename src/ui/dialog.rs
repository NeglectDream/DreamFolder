//! 自绘模态对话框：提示 / 确认 / 输入（可带颜色选择）。
//!
//! 取代 `MessageBoxW`。原生弹窗在深色主题下仍然是系统那套浅色框，
//! 而且它会阻塞消息循环（`MessageBox` 自己跑一个模态循环），
//! 在主窗口还没画完的时候弹出来尤其难看。
//!
//! 这里是一个状态机：`Application::dialog` 里存一份 [`Dialog`]，
//! 绘制层最后画它，命中测试最先判它，键盘先在它里面路由。
//! 不做子窗口——和整个界面其他部分一样，全部自绘。
//!
//! 几何全部集中在本模块：绘制与命中共用同一份矩形，避免「看得见点不到」。

use std::path::PathBuf;
use windows_sys::Win32::Foundation::RECT;

use crate::app::Application;
use crate::strings as S;
use crate::win::paint::{rect, rect_contains};

use super::Hit;

/// 对话框的种类，决定按钮排布与是否画输入框。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DialogKind {
    /// 只有「我知道了」。
    Info,
    /// 「确定 / 取消」。
    Confirm,
    /// 「确定 / 取消」+ 输入框（+ 可选的颜色行）。
    Prompt,
}

/// 点「确定」之后要执行的动作用它来标识。
///
/// 把「要做什么」放进对话框，而不是让每个调用点自己去接回调：
/// 自绘对话框没有回调机制，用枚举把意图带出来是最省事也最不容易漏的做法。
#[derive(Clone, Debug)]
pub enum DialogIntent {
    /// 纯提示，确定后什么都不做。
    Info,
    /// 确认后恢复系统文件管理。
    RestoreExplorer,
    /// 新建一个书签目录（名字取自输入框）。
    NewBookmarkFolder,
    /// 重命名第 N 个书签（索引是 `bookmarks.items` 的下标）。
    RenameBookmark(usize),
    /// 重命名第 N 个快捷访问项。
    RenameNavPlace(usize),
    /// 新建标签（名字与颜色取自对话框，路径是打开对话框时的选中项）。
    NewTag { paths: Vec<PathBuf> },
    /// 重命名第 N 个标签。
    RenameTag(usize),
    /// 删除第 N 个标签（只删归类，不动磁盘上的东西）。
    RemoveTag(usize),
    /// 多标签关窗确认通过：真正关掉窗口（见 `request_close`）。
    ExitApp,
    /// Shift+Delete 确认通过：永久删除这些路径（不经过回收站）。
    DeletePermanent { items: Vec<PathBuf> },
}

/// 标签预设色（`#RRGGBB`，绘制前转成 COLORREF）。
///
/// 直接复用配置层的色板：新建标签与详情窗格/导航窗格的色点必须是同一套，
/// 两处各写一份迟早会漂移成两种红。
pub const TAG_COLORS: [u32; 8] = crate::cfg::tags::PALETTE;

/// `#RRGGBB` → GDI 的 `COLORREF`（`0x00BBGGRR`）。
pub fn rgb_to_colorref(rgb: u32) -> u32 {
    ((rgb & 0xFF) << 16) | (rgb & 0xFF00) | ((rgb >> 16) & 0xFF)
}

/// 一个待显示的对话框。
pub struct Dialog {
    pub kind: DialogKind,
    pub title: String,
    pub body: String,
    /// 输入框上方的标签（Prompt 用）。
    pub input_label: String,
    pub input: String,
    /// 输入框里的光标位置（字符下标）。
    pub caret: usize,
    /// 是否显示颜色选择行。
    pub colors: bool,
    /// 当前选中的颜色下标（`TAG_COLORS`）。
    pub picked: usize,
    pub intent: DialogIntent,
}

impl Dialog {
    pub fn info(title: impl Into<String>, body: impl Into<String>) -> Dialog {
        Dialog {
            kind: DialogKind::Info,
            title: title.into(),
            body: body.into(),
            input_label: String::new(),
            input: String::new(),
            caret: 0,
            colors: false,
            picked: 4,
            intent: DialogIntent::Info,
        }
    }

    pub fn confirm(
        title: impl Into<String>,
        body: impl Into<String>,
        intent: DialogIntent,
    ) -> Dialog {
        let body = body.into();
        Dialog {
            kind: DialogKind::Confirm,
            title: title.into(),
            body,
            input_label: String::new(),
            input: String::new(),
            caret: 0,
            colors: false,
            picked: 4,
            intent,
        }
    }

    pub fn prompt(
        title: impl Into<String>,
        label: impl Into<String>,
        initial: impl Into<String>,
        colors: bool,
        intent: DialogIntent,
    ) -> Dialog {
        let input = initial.into();
        let caret = input.chars().count();
        Dialog {
            kind: DialogKind::Prompt,
            title: title.into(),
            body: String::new(),
            input_label: label.into(),
            input,
            caret,
            colors,
            picked: 4,
            intent,
        }
    }

    /// 是否是带输入框的对话框。
    pub fn has_input(&self) -> bool {
        self.kind == DialogKind::Prompt
    }
}

// ------------------------------------------------------------------ 几何

const PANEL_W: i32 = 420;
const PANEL_W_COLORS: i32 = 460;
const PAD: i32 = 20;
const BTN_W: i32 = 88;
const BTN_H: i32 = 32;

/// 缩放函数（客户区坐标，按 layout.scale）。
fn s(app: &Application, v: i32) -> i32 {
    (v as f32 * app.layout.scale).round() as i32
}

/// 一行正文的高度。
fn line_h(app: &Application) -> i32 {
    s(app, 20)
}

/// 正文在给定宽度内要占几行。
///
/// 用 `app::text_units`（半角=1、全角=2 单位）估算：对话框的正文都是
/// 一两句提示，为它去做一次真正的 `DrawTextW(DT_CALCRECT)` 测量既多一次
/// GDI 往返，还要在命中测试（没有 HDC）里再想办法。估算足够稳。
fn body_lines(app: &Application, text: &str, inner_w: i32) -> i32 {
    let unit = s(app, 7).max(1);
    let per_line = (inner_w / unit).max(8);
    let mut n = 0;
    for line in text.split('\n') {
        let chars: Vec<char> = line.chars().collect();
        let units = crate::app::text_units(&chars);
        n += ((units + per_line - 1) / per_line).max(1);
    }
    n.max(1)
}

/// 面板宽度：带颜色行时更宽，但永远不超过客户区。
fn panel_width(app: &Application) -> i32 {
    let base = match app.dialog.as_ref() {
        Some(d) if d.colors => PANEL_W_COLORS,
        _ => PANEL_W,
    };
    let cw = app.layout.client.right - app.layout.client.left;
    let margin = s(app, 16).max(8);
    s(app, base).min((cw - margin * 2).max(s(app, 240)))
}

/// 对话框的各个矩形。绘制与命中共用。
pub struct Parts {
    pub panel: RECT,
    pub title: RECT,
    pub body: RECT,
    pub label: RECT,
    pub input: RECT,
    /// 颜色环所在的整行（无颜色行时是空矩形）。
    pub colors: RECT,
}

/// 计算全部矩形。
pub fn parts(app: &Application) -> Parts {
    let cl = app.layout.client;
    let w = panel_width(app);
    let pad = s(app, PAD);
    let inner_w = (w - pad * 2).max(s(app, 60));

    let (kind, has_colors, body_text) = match app.dialog.as_ref() {
        Some(d) => (d.kind, d.colors, d.body.clone()),
        None => (DialogKind::Info, false, String::new()),
    };

    let title_h = s(app, 24);
    let gap = s(app, 10);
    let lines = body_lines(app, &body_text, inner_w);
    let body_h = lines * line_h(app);
    let label_h = s(app, 18);
    let input_h = s(app, 30);
    let color_h = s(app, 30);

    let mut h = pad + title_h;
    if !body_text.is_empty() {
        h += gap + body_h;
    }
    if kind == DialogKind::Prompt {
        h += s(app, 12) + label_h + s(app, 6) + input_h;
        if has_colors {
            h += s(app, 10) + color_h;
        }
    }
    h += s(app, 18) + s(app, BTN_H) + pad;

    let cw = cl.right - cl.left;
    let ch = cl.bottom - cl.top;
    // 高 DPI 或窗口很矮时把总高压到客户区内，并把整体上移一点
    let h = h.min((ch - s(app, 16)).max(s(app, 120)));
    let x = cl.left + (cw - w) / 2;
    let y = cl.top + ((ch - h) / 2).max(s(app, 4));

    let panel = rect(x, y, x + w, y + h);
    let left = panel.left + pad;
    let right = panel.left + pad + inner_w;

    let mut cy = panel.top + pad;
    let title = rect(left, cy, right, cy + title_h);
    cy += title_h;

    let body = if body_text.is_empty() {
        rect(left, cy, right, cy)
    } else {
        cy += gap;
        let r = rect(left, cy, right, cy + body_h);
        cy += body_h;
        r
    };

    let (label, input) = if kind == DialogKind::Prompt {
        cy += s(app, 12);
        let l = rect(left, cy, right, cy + label_h);
        cy += label_h + s(app, 6);
        let i = rect(left, cy, right, cy + input_h);
        cy += input_h;
        (l, i)
    } else {
        let z = rect(left, cy, right, cy);
        (z, z)
    };

    let colors = if kind == DialogKind::Prompt && has_colors {
        cy += s(app, 10);
        let r = rect(left, cy, right, cy + color_h);
        cy += color_h;
        r
    } else {
        rect(left, cy, right, cy)
    };
    let _ = cy;

    Parts {
        panel,
        title,
        body,
        label,
        input,
        colors,
    }
}

/// 按钮个数：`Info` 一个，其余两个。
pub fn button_count(app: &Application) -> usize {
    match app.dialog.as_ref().map(|d| d.kind) {
        Some(k) if k != DialogKind::Info => 2,
        _ => 1,
    }
}

/// 第 `i` 个按钮的矩形：从右往左排，`0` 是最右边的「确定」。
pub fn button_rect(app: &Application, i: usize) -> RECT {
    let p = parts(app);
    let bw = s(app, BTN_W);
    let bh = s(app, BTN_H);
    let gap = s(app, 8);
    let n = button_count(app);
    let right = p.panel.right - s(app, PAD) - (n - 1 - i) as i32 * (bw + gap);
    let y = p.panel.bottom - s(app, PAD) - bh;
    rect(right - bw, y, right, y + bh)
}

/// 第 `i` 个颜色环的矩形。
pub fn color_rect(app: &Application, i: usize) -> RECT {
    let p = parts(app);
    let d = s(app, 20); // 环的直径
    let gap = s(app, 12);
    let x = p.colors.left + i as i32 * (d + gap);
    let cy = (p.colors.top + p.colors.bottom) / 2;
    rect(x, cy - d / 2, x + d, cy + d / 2)
}

/// 命中测试。对话框是模态的：落在面板外的点返回 `DialogPane`，
/// 既不会穿透到下面的界面，也不会关闭对话框。
pub fn hit(app: &Application, x: i32, y: i32) -> Hit {
    if app.dialog.is_none() {
        return Hit::None;
    }
    let p = parts(app);
    if !rect_contains(p.panel, x, y) {
        return Hit::DialogPane;
    }
    for i in 0..button_count(app) {
        if rect_contains(button_rect(app, i), x, y) {
            return Hit::DialogButton(i);
        }
    }
    if let Some(d) = app.dialog.as_ref() {
        if d.has_input() && rect_contains(p.input, x, y) {
            return Hit::DialogInput;
        }
        if d.colors {
            for i in 0..TAG_COLORS.len() {
                if rect_contains(color_rect(app, i), x, y) {
                    return Hit::DialogColor(i);
                }
            }
        }
    }
    Hit::DialogPane
}

/// 主按钮（确定 / 我知道了）的文字。
pub fn primary_text(app: &Application) -> &'static str {
    match app.dialog.as_ref().map(|d| d.kind) {
        Some(DialogKind::Info) | None => S::BTN_KNOW,
        _ => S::BTN_OK,
    }
}

/// 输入框里文字的左边界（含内边距）。绘制与「点击定位光标」共用。
pub fn input_text_left(app: &Application) -> i32 {
    parts(app).input.left + s(app, 8)
}

// ------------------------------------------------------------------ 交互

/// 点击落在对话框上。返回是否已处理（对话框打开时总是已处理）。
///
/// 逻辑放在这里而不是 `interact`：对话框是自成一体的模态，
/// 它的输入语义（光标定位、选色、按钮）与主界面没有任何交集。
pub fn click(app: &mut Application, x: i32, y: i32) -> Hit {
    let h = hit(app, x, y);
    app.pressed = h;
    app.hover = h;
    app.mouse_x = x;
    app.mouse_y = y;
    match h {
        Hit::DialogColor(i) => {
            if let Some(d) = app.dialog.as_mut() {
                d.picked = i.min(TAG_COLORS.len() - 1);
            }
        }
        Hit::DialogInput => {
            let caret = caret_at_x(app, x);
            if let Some(d) = app.dialog.as_mut() {
                d.caret = caret;
            }
        }
        _ => {}
    }
    app.request_repaint();
    h
}

/// 松手：主按钮＝确定，次按钮＝取消。按下与松开必须在同一个按钮上。
pub fn release(app: &mut Application, x: i32, y: i32) {
    let now = hit(app, x, y);
    let pressed = app.pressed;
    app.pressed = Hit::None;
    if let (Hit::DialogButton(a), Hit::DialogButton(b)) = (pressed, now) {
        if a == b {
            if a == 0 {
                app.dialog_accept();
            } else {
                app.dialog_cancel();
            }
            return;
        }
    }
    app.request_repaint();
}

/// 横坐标 → 字符下标（按真实字宽找最近的字符边界）。
///
/// 和绘制插入符共用 `measure_prefixes`：按单位宽估算时，中文会偏出半个字，
/// 点下去光标就落在前一个字的中间。
fn caret_at_x(app: &mut Application, x: i32) -> usize {
    // 先把文本取出来：量宽度要可变借用字体缓存，不能同时借着 dialog
    let Some(input) = app.dialog.as_ref().map(|d| d.input.clone()) else {
        return 0;
    };
    let left = input_text_left(app);
    let widths = app.measure_prefixes(&input);
    let mut best = 0usize;
    let mut best_dist = (left - x).abs();
    for (i, w) in widths.iter().enumerate().skip(1) {
        let dist = (left + w - x).abs();
        if dist < best_dist {
            best_dist = dist;
            best = i;
        }
    }
    best
}

/// 编辑输入框：把文本取成字符数组交给 `f`，算完再写回。
///
/// 对话框的输入最多几十个字符，每次重建字符串的开销可以忽略；
/// 相比之下，在 UTF-8 字节串上做「按字符移动光标」才是真正麻烦的地方。
fn edit<F: FnOnce(&mut Vec<char>, &mut usize)>(app: &mut Application, f: F) {
    {
        let Some(d) = app.dialog.as_mut() else {
            return;
        };
        if !d.has_input() {
            return;
        }
        let mut chars: Vec<char> = d.input.chars().collect();
        let mut caret = d.caret.min(chars.len());
        f(&mut chars, &mut caret);
        let caret = caret.min(chars.len());
        d.caret = caret;
        d.input = chars.into_iter().collect();
    }
    app.request_repaint();
}

/// 字符输入（WM_CHAR）。
pub fn char_input(app: &mut Application, ch: char) {
    if ch.is_control() {
        return;
    }
    edit(app, |chars, caret| {
        chars.insert(*caret, ch);
        *caret += 1;
    });
}

/// 按键（WM_KEYDOWN）。返回是否已消费。
pub fn key_down(app: &mut Application, vk: u16) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;

    let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) < 0 };

    match vk {
        VK_ESCAPE => {
            app.dialog_cancel();
            true
        }
        VK_RETURN => {
            app.dialog_accept();
            true
        }
        VK_BACK => {
            edit(app, |chars, caret| {
                if ctrl {
                    while *caret > 0 && chars[*caret - 1].is_whitespace() {
                        chars.remove(*caret - 1);
                        *caret -= 1;
                    }
                    while *caret > 0 && !chars[*caret - 1].is_whitespace() {
                        chars.remove(*caret - 1);
                        *caret -= 1;
                    }
                } else if *caret > 0 {
                    chars.remove(*caret - 1);
                    *caret -= 1;
                }
            });
            true
        }
        VK_DELETE => {
            edit(app, |chars, caret| {
                if *caret < chars.len() {
                    chars.remove(*caret);
                }
            });
            true
        }
        VK_LEFT => {
            edit(app, |_, caret| *caret = caret.saturating_sub(1));
            true
        }
        VK_RIGHT => {
            edit(app, |chars, caret| *caret = (*caret + 1).min(chars.len()));
            true
        }
        VK_HOME => {
            edit(app, |_, caret| *caret = 0);
            true
        }
        VK_END => {
            edit(app, |chars, caret| *caret = chars.len());
            true
        }
        VK_V if ctrl => {
            if let Some(text) = crate::fs::shell::get_clipboard_text() {
                // 粘贴进来的换行会让单行输入框错位，统一换成空格
                let cleaned: String = text
                    .chars()
                    .map(|c| if c == '\r' || c == '\n' || c == '\t' { ' ' } else { c })
                    .filter(|c| !c.is_control())
                    .collect();
                edit(app, |chars, caret| {
                    for c in cleaned.chars() {
                        chars.insert(*caret, c);
                        *caret += 1;
                    }
                });
            }
            true
        }
        // Tab：在颜色环之间循环（有颜色行时才拦），否则交回主界面
        VK_TAB => {
            let has_colors = app.dialog.as_ref().map(|d| d.colors).unwrap_or(false);
            if !has_colors {
                return false;
            }
            let step: i32 = if ctrl { -1 } else { 1 };
            if let Some(d) = app.dialog.as_mut() {
                let n = TAG_COLORS.len() as i32;
                d.picked = (((d.picked as i32 + step) % n + n) % n) as usize;
            }
            app.request_repaint();
            true
        }
        _ => false,
    }
}
