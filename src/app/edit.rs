//! 编辑框：地址栏 / 搜索框 / 表格内重命名共用的文本编辑逻辑。
//!
//! 三个输入框共用一套「缓冲区 + 光标 + 选区」的实现（`edit_buffer` 决定
//! 当前在编哪一个）。单独成文件后，`app.rs` 留下的主要是状态机与窗口过程。

use super::*;

impl Application {

    // ------------------------------------------------- 编辑框（地址/搜索/重命名共用）

    /// 当前正在编辑的文本缓冲（只读）。
    pub(crate) fn edit_buffer(&self) -> Option<&String> {
        if self.rename_target.is_some() {
            Some(&self.inline_edit_text)
        } else if self.address_editing {
            Some(&self.address_text)
        } else if self.search_focused {
            Some(&self.cur().search_text)
        } else {
            None
        }
    }

    /// 当前正在编辑的文本缓冲（可写）。
    pub(crate) fn edit_buffer_mut(&mut self) -> Option<&mut String> {
        if self.rename_target.is_some() {
            Some(&mut self.inline_edit_text)
        } else if self.address_editing {
            Some(&mut self.address_text)
        } else if self.search_focused {
            Some(&mut self.cur_mut().search_text)
        } else {
            None
        }
    }

    pub(crate) fn edit_buffer_len(&self) -> usize {
        self.edit_buffer().map(|b| b.chars().count()).unwrap_or(0)
    }

    /// 光标位置（可写）。
    ///
    /// 注意：不能与 `edit_buffer_mut` 的返回值同时存活——两者都借 `&mut self`。
    pub(crate) fn caret_mut(&mut self) -> &mut usize {
        if self.rename_target.is_some() {
            &mut self.inline_edit_caret
        } else if self.address_editing {
            &mut self.address_caret
        } else {
            &mut self.cur_mut().search_caret
        }
    }

    pub(crate) fn caret(&self) -> usize {
        if self.rename_target.is_some() {
            self.inline_edit_caret
        } else if self.address_editing {
            self.address_caret
        } else {
            self.cur().search_caret
        }
    }

    /// 编辑结束后统一收尾。
    ///
    /// 搜索框不再逐字符搜：一次搜索要跑系统索引加最多 400ms 的递归扫描，
    /// 而且全在 UI 线程上同步执行，跟着每个按键跑一遍会让英文逐字母输入
    /// 一顿一顿的。这里只把状态标成「等回车」，真正的搜索放到 Enter 或失焦时做。
    pub(crate) fn after_edit(&mut self) {
        if self.search_focused {
            if self.cur().search_text.trim().is_empty() {
                self.restore_search_backup();
            } else {
                self.cur_mut().search_state = Some(SearchState::Pending);
            }
        }
        // 地址栏每次文本变化后重算历史候选（前缀匹配）。
        // 放在这里而不是绘制里：键入才需要重算，重绘不需要。
        if self.address_editing {
            self.refresh_addr_suggest();
        }
        self.request_repaint();
    }

    /// 当前选区（已归一化成 `(起, 止)`），没有选区返回 `None`。
    pub(crate) fn selection(&self) -> Option<(usize, usize)> {
        let (a, b) = self.address_sel?;
        if a == b {
            None
        } else {
            Some((a.min(b), a.max(b)))
        }
    }

    /// 把选区删掉，返回删除后的光标位置；没有选区返回 `None`。
    pub(crate) fn edit_delete_selection(&mut self) -> Option<usize> {
        let (s0, s1) = self.selection()?;
        let len = self.edit_buffer_len();
        let s0 = s0.min(len);
        let s1 = s1.min(len);
        if let Some(buf) = self.edit_buffer_mut() {
            let chars: Vec<char> = buf.chars().collect();
            let mut out: String = chars[..s0].iter().collect();
            out.extend(chars[s1..].iter());
            *buf = out;
        }
        self.address_sel = None;
        Some(s0)
    }

    pub fn edit_insert(&mut self, ch: char) {
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
        }
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
        self.address_sel = None;
        self.after_edit();
    }

    pub fn edit_backspace(&mut self) {
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
            self.after_edit();
            return;
        }
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
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
            self.after_edit();
            return;
        }
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
        self.address_sel = None;
        let c = self.caret_mut();
        *c = (*c as i32 + delta).clamp(0, len as i32) as usize;
        self.request_repaint();
    }

    /// 全选（Ctrl+A）。
    pub fn edit_select_all(&mut self) {
        let len = self.edit_buffer_len();
        self.address_sel = Some((0, len));
        *self.caret_mut() = len;
        self.request_repaint();
    }

    /// 带 Shift 的光标移动：扩展现有选区。
    pub fn edit_move_caret_shift(&mut self, delta: i32) {
        let len = self.edit_buffer_len();
        let caret = self.caret();
        let anchor = self.address_sel.map(|(a, _)| a).unwrap_or(caret);
        let next = (caret as i32 + delta).clamp(0, len as i32) as usize;
        *self.caret_mut() = next;
        self.address_sel = Some((anchor, next));
        self.request_repaint();
    }

    /// 按词左右跳（Ctrl+← / Ctrl+→）。`extend` 为真时扩选。
    pub fn edit_move_word(&mut self, dir: i32, extend: bool) {
        let text = self.edit_buffer().cloned().unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let len = chars.len();
        let caret = self.caret().min(len);
        let anchor = self.address_sel.map(|(a, _)| a).unwrap_or(caret);
        let mut i = caret as i32;
        if dir < 0 {
            // 先跳过左边空白，再跳过一整段非空白
            while i > 0 && chars[(i - 1) as usize].is_whitespace() {
                i -= 1;
            }
            while i > 0 && !chars[(i - 1) as usize].is_whitespace() {
                i -= 1;
            }
        } else {
            while (i as usize) < len && chars[i as usize].is_whitespace() {
                i += 1;
            }
            while (i as usize) < len && !chars[i as usize].is_whitespace() {
                i += 1;
            }
        }
        let next = i.clamp(0, len as i32) as usize;
        *self.caret_mut() = next;
        self.address_sel = if extend { Some((anchor, next)) } else { None };
        self.request_repaint();
    }

    /// 光标移到行首/行尾，`extend` 为真时扩选。
    pub fn edit_caret_edge(&mut self, end: bool, extend: bool) {
        let len = self.edit_buffer_len();
        let caret = self.caret();
        let anchor = self.address_sel.map(|(a, _)| a).unwrap_or(caret);
        let next = if end { len } else { 0 };
        *self.caret_mut() = next;
        self.address_sel = if extend { Some((anchor, next)) } else { None };
        self.request_repaint();
    }

    /// Ctrl+Backspace：删掉光标左边的一个词。
    pub fn edit_backspace_word(&mut self) {
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
            self.after_edit();
            return;
        }
        let text = self.edit_buffer().cloned().unwrap_or_default();
        let chars: Vec<char> = text.chars().collect();
        let caret = self.caret().min(chars.len());
        let mut i = caret;
        while i > 0 && chars[i - 1].is_whitespace() {
            i -= 1;
        }
        while i > 0 && !chars[i - 1].is_whitespace() {
            i -= 1;
        }
        self.address_sel = Some((i, caret));
        *self.caret_mut() = caret;
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
        }
        self.after_edit();
    }

    /// 复制当前选区（Ctrl+C）。
    pub fn edit_copy(&mut self) {
        let Some((s0, s1)) = self.selection() else {
            return;
        };
        let text = self.edit_buffer().cloned().unwrap_or_default();
        let picked: String = text.chars().skip(s0).take(s1 - s0).collect();
        if !picked.is_empty() && !crate::fs::shell::set_clipboard_text(&picked) {
            self.show_message("无法写入剪贴板");
        }
    }

    /// 剪切当前选区（Ctrl+X）。
    pub fn edit_cut(&mut self) {
        self.edit_copy();
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
            self.after_edit();
        }
    }

    /// 粘贴（Ctrl+V），有选区时先替换掉。
    pub fn edit_paste(&mut self) {
        let Some(text) = crate::fs::shell::get_clipboard_text() else {
            return;
        };
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if text.is_empty() {
            return;
        }
        if let Some(pos) = self.edit_delete_selection() {
            *self.caret_mut() = pos;
        }
        let caret = self.caret();
        let new_caret = match self.edit_buffer_mut() {
            Some(buf) => {
                let chars: Vec<char> = buf.chars().collect();
                let pos = caret.min(chars.len());
                let mut s: String = chars[..pos].iter().collect();
                s.push_str(&text);
                s.extend(chars[pos..].iter());
                let add = text.chars().count();
                *buf = s;
                pos + add
            }
            None => caret,
        };
        *self.caret_mut() = new_caret;
        self.address_sel = None;
        self.after_edit();
    }

    /// 当前是否有文本编辑框接收输入（地址栏 / 搜索框 / 重命名 / 对话框）。
    ///
    /// IME 与插入符闪烁都按这个判据决定行止，所以只留一份定义：
    /// 两处各写一遍，早晚会有一边漏掉「对话框输入框」这类新形态。
    pub fn has_text_edit(&self) -> bool {
        self.address_editing
            || self.search_focused
            || self.rename_active()
            || self.dialog.as_ref().map(|d| d.has_input()) == Some(true)
    }

    /// 把 IME 的组合窗与候选窗移到当前编辑框的插入符旁。
    ///
    /// 自绘窗口没有系统编辑控件，输入法默认把候选窗摆在屏幕左上角，
    /// 中文输入时眼睛要在正文和屏幕角落之间来回跳。绘制插入符的
    /// 那几处会顺手把锚点记进 `ime_anchor`，这里在 IME 激活前告诉它。
    pub fn place_ime_window(&self, hwnd: windows_sys::Win32::Foundation::HWND) {
        use windows_sys::Win32::Foundation::{POINT, RECT};
        use windows_sys::Win32::Graphics::Gdi::ClientToScreen;
        use windows_sys::Win32::UI::Input::Ime::{
            CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_POINT, COMPOSITIONFORM, ImmGetContext,
            ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow,
        };

        // 编辑态退出后锚点可能残留（退出点散落各处），这里消费前
        // 先确认确有编辑框激活，非编辑态不挪窗口
        if !self.has_text_edit() {
            return;
        }
        let Some((x, y)) = self.ime_anchor.or_else(|| self.ime_fallback_anchor()) else {
            return;
        };
        unsafe {
            let himc = ImmGetContext(hwnd);
            if himc.is_null() {
                return;
            }
            let mut pt = POINT { x, y };
            ClientToScreen(hwnd, &mut pt);
            // 组合窗（拼音串）贴着插入符；候选窗也跟着走
            let form = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: pt,
                rcArea: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            };
            ImmSetCompositionWindow(himc, &form);
            let cand = CANDIDATEFORM {
                dwIndex: 0,
                dwStyle: CFS_CANDIDATEPOS,
                ptCurrentPos: pt,
                rcArea: RECT { left: 0, top: 0, right: 0, bottom: 0 },
            };
            ImmSetCandidateWindow(himc, &cand);
            ImmReleaseContext(hwnd, himc);
        }
    }

    /// `ime_anchor` 还没被绘制层写下时的安全落点。
    ///
    /// 插入符锚点是绘制时记录的，而 IME 可能在那之前就发
    /// `WM_IME_STARTCOMPOSITION`：点进地址栏后立刻敲输入法第一个键，
    /// 中间那一帧还没画，锚点是 `None`。以前这里直接放弃定位，系统就把
    /// 组合窗摆到客户区左上角——正好压在标签栏上，中文还没上屏时看着
    /// 像标签栏在闪。退到编辑控件左上角，最差也只是偏左一点。
    fn ime_fallback_anchor(&self) -> Option<(i32, i32)> {
        let l = &self.layout;
        let s = |v: i32| (v as f32 * l.scale).round() as i32;
        if self.address_editing {
            return Some((l.address.left + s(8), l.address.top + s(6)));
        }
        if self.search_focused {
            return Some((l.search.left + s(8), l.search.top + s(6)));
        }
        if self.dialog.as_ref().map(|d| d.has_input()) == Some(true) {
            let p = crate::ui::dialog::parts(self);
            return Some((p.input.left + s(8), p.input.top + s(6)));
        }
        if let Some(idx) = self.rename_target {
            let t = self.active()?;
            let r = l.item_rect(t.view_mode, idx, t.scroll_y);
            return Some((r.left + s(8), r.top + s(4)));
        }
        None
    }
}
