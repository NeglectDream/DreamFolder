//! 文件操作：复制 / 剪切 / 粘贴 / 删除 / 重命名，以及排序与视图切换。
//!
//! 真正的动作都丢给后台线程（`io::Job::Op`）——删一批文件、把几 GB
//! 复制到另一块盘，`SHFileOperationW` 会阻塞到全部完成；放在消息循环里
//! 就是整个窗口假死。这里只负责组织参数、置状态、处理回来的结果。

use super::*;

impl Application {
    // ------------------------------------------------------------ 文件操作

    pub(crate) fn selected_paths(&self) -> Vec<PathBuf> {
        let Some(t) = self.active() else {
            return Vec::new();
        };
        let mut v: Vec<usize> = t.selected.iter().copied().collect();
        v.sort_unstable();
        v.into_iter()
            .filter_map(|i| t.entries.get(i).map(|e| e.full.clone()))
            .collect()
    }

    /// 点击详细信息表头第 `col` 列：切换成按这一列排序。
    ///
    /// 规则和资源管理器一致——点同一列翻转升降序，点新的一列从升序开始。
    pub fn sort_by_column(&mut self, col: usize) {
        use crate::fs::enumerate::SortKey;
        let key = match col {
            0 => SortKey::Name,
            1 => SortKey::Modified,
            2 => SortKey::Kind,
            _ => SortKey::Size,
        };
        let idx = self.active_tab;
        if idx >= self.tabs.len() {
            return;
        }
        let same = self.tabs[idx].sort.key == key;
        self.tabs[idx].sort = crate::fs::enumerate::Sort {
            key,
            descending: if same { !self.tabs[idx].sort.descending } else { false },
        };
        self.cfg.settings.sort_key = key;
        self.cfg.settings.sort_desc = self.tabs[idx].sort.descending;
        self.remember_folder_view();

        // 原地重排，不重新枚举：切列只是换一个比较键，磁盘内容一点没变。
        // 重新扫盘会让大目录肉眼可见地卡一下，而资源管理器是瞬间完成的。
        let keep: std::collections::HashSet<PathBuf> =
            self.selected_paths().into_iter().collect();
        {
            let Some(t) = self.tabs.get_mut(idx) else {
                return;
            };
            let sort = t.sort;
            crate::fs::enumerate::sort_entries(&mut t.entries, &sort);

            // 重排后按下标记的选中/锚点全部失效，按路径找回
            t.selected.clear();
            t.anchor = None;
            t.focus = None;
            if !keep.is_empty() {
                for (i, e) in t.entries.iter().enumerate() {
                    if keep.contains(&e.full) {
                        t.selected.insert(i);
                        if t.anchor.is_none() {
                            t.anchor = Some(i);
                        }
                        if t.focus.is_none() {
                            t.focus = Some(i);
                        }
                    }
                }
            }
        }
        self.request_repaint();
    }

    pub fn delete_selection(&mut self, permanent: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if self.in_recycle_bin() {
            // 在回收站里再删一次＝彻底删除，连元数据一起清掉
            let mut err = None;
            for p in &paths {
                if let Err(e) = crate::fs::recycle::purge(p) {
                    err = Some(e);
                }
            }
            if let Some(e) = err {
                self.show_message(&e);
            }
            self.refresh_active_tab();
            return;
        }
        self.send_op(OpWhat::Delete {
            items: paths,
            permanent,
        });
    }

    /// 回收站里把选中项还原到删除前的位置。
    ///
    /// 目标处已有同名文件时自动改名（见 `recycle::restore`），不会覆盖。
    /// 一件都没成功时把最后一条错误留在状态栏，避免"点了没反应"。
    pub fn restore_recycle_selection(&mut self) {
        if !self.in_recycle_bin() {
            return;
        }
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let mut ok = 0usize;
        let mut err: Option<String> = None;
        let mut last_target: Option<PathBuf> = None;
        for p in &paths {
            match crate::fs::recycle::restore(p) {
                Ok(t) => {
                    ok += 1;
                    last_target = Some(t);
                }
                Err(e) => err = Some(e),
            }
        }
        if ok == 0 {
            if let Some(e) = err {
                self.show_message(&e);
            }
            return;
        }
        let note = if ok == 1 {
            match last_target {
                Some(t) => format!("已还原到 {}", t.display()),
                None => "已还原".to_string(),
            }
        } else {
            format!("已还原 {ok} 项")
        };
        self.show_status_note(&note);
        self.refresh_active_tab();
    }

    /// Shift+Delete：永久删除，先问一次。
    ///
    /// 资源管理器的这条路径会弹「确实要永久删除吗」，我们没有理由不问——
    /// 它绕过了回收站，是唯一一个按下去就真的找不回来的操作。
    /// 在回收站里按 Shift+Delete 只是加快彻底删除，不再重复问一遍。
    pub fn confirm_delete_permanent(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if self.in_recycle_bin() {
            self.delete_selection(true);
            return;
        }
        let body = if paths.len() == 1 {
            let name = paths[0]
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| paths[0].to_string_lossy().into_owned());
            format!("确实要永久删除“{name}”吗？此操作无法撤销。")
        } else {
            format!(
                "确实要永久删除这 {} 项吗？此操作无法撤销。",
                paths.len()
            )
        };
        self.dialog_confirm(
            "永久删除",
            &body,
            crate::ui::dialog::DialogIntent::DeletePermanent { items: paths },
        );
    }

    /// 把一次耗时不可控的文件操作丢给后台线程。
    ///
    /// 删一批文件、把几 GB 复制到另一块盘，`SHFileOperationW` 会阻塞到
    /// 全部完成为止；以前这期间窗口完全冻住（连重绘都不响应）。
    /// 后台线程用 0 当 owner，Shell 自己弹进度/冲突对话框。
    pub(crate) fn send_op(&mut self, what: OpWhat) {
        self.io.send(Job::Op { what });
    }

    pub fn copy_selection(&mut self) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if !shell::set_clipboard_files(&paths) {
            self.show_message("无法写入剪贴板");
        } else {
            // 复制必须顶掉之前的「剪切」语义，否则粘贴会变成移动
            self.cut_pending = false;
        }
    }

    pub fn cut_selection(&mut self) {
        // 剪切与复制共用剪贴板；粘贴时按同样方式移动。
        // 为区分语义，把「剪切」状态记在应用内。
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if shell::set_clipboard_files(&paths) {
            self.cut_pending = true;
        } else {
            self.show_message("无法写入剪贴板");
        }
    }

    pub fn paste_into_active_tab(&mut self) {
        // 外部程序写进剪贴板的路径可能带 `\\?\` 前缀，而 Shell 那一侧
        // 不认（`SHFileOperation` 会回「路径无效」）；统一剥掉再走。
        let sources: Vec<PathBuf> = shell::get_clipboard_files()
            .into_iter()
            .map(|p| crate::fs::strip_verbatim(&p))
            .collect();
        if sources.is_empty() {
            return;
        }
        let Some(target) = self.active().map(|t| t.path.clone()) else {
            return;
        };
        if target.as_os_str().is_empty() {
            return;
        }

        // 剪贴板里可能是别的程序留下的旧内容，源早就挪走了。直接交给
        // Shell 只会换回一句语焉不详的「操作失败（代码 0x2）」——
        // 这里先看一眼，把到底哪条不见了说出来。
        if let Some(gone) = sources.iter().find(|p| !p.exists()) {
            let msg = format!("源已不存在：{}", gone.display());
            self.show_message(&msg);
            return;
        }

        if self.cut_pending {
            self.send_op(OpWhat::Move {
                items: sources,
                target,
            });
        } else {
            self.send_op(OpWhat::Copy {
                items: sources,
                target,
            });
        }
    }

    pub fn begin_rename_selection(&mut self) {
        let Some(&i) = self.active().and_then(|t| t.selected.iter().next()) else {
            return;
        };
        let Some(name) = self.active().and_then(|t| t.entries.get(i)).map(|e| e.name.clone())
        else {
            return;
        };
        // 进入内联重命名：复用地址栏的编辑机制，锚定到该项
        self.rename_target = Some(i);
        self.address_editing = false;
        self.search_focused = false;
        self.inline_edit_text = name.clone();
        // 只选中主干、扩展名保持不选——资源管理器就是这样：直接打字是改名字，
        // 想改扩展名再按 End。以前全选整个文件名，顺手敲一个字符就把
        // `.docx` 一起替换掉了，改完文件类型都变了。
        let stem_len = match name.rfind('.') {
            Some(pos) if pos > 0 && pos + 1 < name.len() => name[..pos].chars().count(),
            _ => name.chars().count(),
        };
        self.inline_edit_caret = stem_len;
        self.address_sel = Some((0, stem_len));
        self.request_repaint();
    }

    pub(crate) fn handle_op_result(&mut self, res: fsops::OpResult) {
        match res {
            fsops::OpResult::Ok => self.refresh_active_tab(),
            fsops::OpResult::Cancelled => {}
            fsops::OpResult::Failed(msg) => {
                self.show_message(&msg);
                self.refresh_active_tab();
            }
        }
    }

    /// 重命名成功后本地就地更新列表，不整目录重扫。
    ///
    /// 重命名是唯一「结果完全确定」的文件操作：落地后的名字就是用户给的那个。
    /// 删除 / 复制 / 移动则可能被用户在 Shell 的进度框里中途「跳过」，
    /// 结果不是全有全无，本地猜不准——那几种仍旧走后台重扫
    /// （`refresh_active_tab` 只发请求，枚举与排序都在工作线程上做）。
    pub(crate) fn apply_rename_locally(&mut self, idx: usize, new_name: &str) {
        let tab = self.active_tab;
        {
            let Some(t) = self.tabs.get_mut(tab) else {
                return;
            };
            let Some(e) = t.entries.get_mut(idx) else {
                return;
            };
            let new_path = e.full.with_file_name(new_name);
            e.name = new_name.to_string();
            e.full = new_path.clone();
            // 名字变了，类型名与图标键都得重算（绘制路径上只读，这里补齐）
            e.type_name.clear();
            e.icon_key = crate::fs::entry::IconKey::Pending;

            let sort = t.sort;
            crate::fs::enumerate::sort_entries(&mut t.entries, &sort);

            // 重排后位置变了，按路径把选中项找回来
            let Some(i) = t.entries.iter().position(|e| e.full == new_path) else {
                return;
            };
            t.selected.clear();
            t.selected.insert(i);
            t.anchor = Some(i);
            t.focus = Some(i);
        }
        self.fill_entry_meta(tab);
        self.request_repaint();
    }

    pub fn toolbar_action(&mut self, index: usize) {
        match index {
            0 => {
                // 新建文件
                if let Some(dir) = self.active().map(|t| t.path.clone()) {
                    match fsops::create_file(&dir, "新建文本文档", "txt") {
                        Ok(p) => {
                            self.refresh_active_tab();
                            self.select_by_path(&p);
                        }
                        Err(e) => self.show_message(&e),
                    }
                }
            }
            1 => {
                // 新建文件夹
                if let Some(dir) = self.active().map(|t| t.path.clone()) {
                    match fsops::create_folder(&dir) {
                        Ok(p) => {
                            self.refresh_active_tab();
                            self.select_by_path(&p);
                        }
                        Err(e) => self.show_message(&e),
                    }
                }
            }
            2 => self.cut_selection(),
            3 => self.copy_selection(),
            4 => self.paste_into_active_tab(),
            5 => self.delete_selection(false),
            6 => self.begin_rename_selection(),
            7 => self.cycle_view_mode(),
            8 => {
                self.search_focused = true;
                self.request_repaint();
            }
            9 => {
                // 快速访问：回到此电脑
                self.navigate_to(PathBuf::new());
            }
            _ => {}
        }
    }

    pub(crate) fn select_by_path(&mut self, path: &std::path::Path) {
        let Some(idx) = self
            .active()
            .and_then(|t| t.entries.iter().position(|e| e.full == path))
        else {
            return;
        };
        self.select_entry(idx, false, false);
        interact::scroll_into_view(self, idx);
    }

    pub fn cycle_view_mode(&mut self) {
        let Some(t) = self.active_mut() else { return };
        let all = ViewMode::ALL;
        let pos = all.iter().position(|&v| v == t.view_mode).unwrap_or(0);
        t.view_mode = all[(pos + 1) % all.len()];
        t.scroll_y = 0;
        self.remember_folder_view();
        self.request_repaint();
    }

    /// Ctrl+滚轮：在视图档之间升降。
    ///
    /// 档位顺序从「最大信息量」到「最大图标」：详细信息排第一个，
    /// 往上是它、往下依次是大图标→中→小图标。`dir > 0` 表示向上滚（更详细）。
    pub fn zoom_view(&mut self, dir: i32) {
        const STEPS: [ViewMode; 5] = [
            ViewMode::Detail,
            ViewMode::LargeEx,
            ViewMode::Large,
            ViewMode::Medium,
            ViewMode::Small,
        ];
        let Some(t) = self.active_mut() else { return };
        let cur = STEPS.iter().position(|&v| v == t.view_mode).unwrap_or(2);
        // 数组是从大到小排的，所以放大 = 索引减
        let next = if dir > 0 {
            cur.saturating_sub(1)
        } else {
            (cur + 1).min(STEPS.len() - 1)
        };
        if next == cur {
            return;
        }
        t.view_mode = STEPS[next];
        t.scroll_y = 0;
        self.cfg.settings.view_mode = STEPS[next];
        self.remember_folder_view();
        self.request_repaint();
    }

    pub fn set_view_mode(&mut self, mode: ViewMode) {
        if let Some(t) = self.active_mut() {
            t.view_mode = mode;
            t.scroll_y = 0;
        }
        self.cfg.settings.view_mode = mode;
        self.remember_folder_view();
        self.request_repaint();
    }
}
