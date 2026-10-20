//! 搜索：一次搜索整个丢给后台线程，以及每个标签各自的搜索状态。
//!
//! 搜索状态（关键词、光标、结果状态、搜索前的列表）挂在 `Tab` 上，
//! 所以切标签不会互相干扰；这里的方法只操作「当前标签」那一份。
//!
//! 为什么必须后台：系统索引查询没有超时，索引服务抽风时会同步死等，
//! 那一下会把窗口彻底冻住（见 `io::Job::Search` 与 `SEARCH_TIMEOUT_MS`）。

use super::*;

    impl Application {
    /// 当前标签的搜索框文本（绘制用）。
    pub fn search_text(&self) -> &str {
        self.active().map(|t| t.search_text.as_str()).unwrap_or("")
    }

    /// 当前标签的搜索框光标位置（绘制用）。
    pub fn search_caret(&self) -> usize {
        self.active().map(|t| t.search_caret).unwrap_or(0)
    }

    /// 当前标签的搜索状态（绘制用）。
    pub fn search_state(&self) -> Option<SearchState> {
        self.active().and_then(|t| t.search_state)
    }
    ///
    /// 后台结果按「标签下标」回填，而下标在关闭标签后会整体前移；不作废
    /// 的话，旧结果可能落进另一个标签。搜索是低频操作，重按一次回车就行。
    pub(crate) fn cancel_pending_search(&mut self) {
        self.search_seq.fetch_add(1, Ordering::SeqCst);
        self.kill_search_timeout();
        for t in &mut self.tabs {
            if matches!(t.search_state, Some(SearchState::Running)) {
                t.search_state = None;
            }
            t.pending_search = false;
        }
    }

    /// 按回车（或搜索框失焦）时触发搜索。
    ///
    /// 搜索整个丢给后台（`io::Job::Search`）：系统索引查询没有超时，
    /// 索引服务抽风时会同步死等；以前那一下直接把窗口冻住。
    /// 这里只负责置状态、备份原始列表、记住请求号。
    pub fn begin_search(&mut self) {
        self.kill_search_timeout();
        let key = self.cur().search_text.trim().to_lowercase();
        if key.is_empty() {
            self.restore_search_backup();
            return;
        }
        let idx = self.active_tab;
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        // 目录还在后台枚举：既没有可备份的原始列表，枚举结果回来也会把
        // 搜索结果顶掉。挂起等它加载完自动触发（本地目录是毫秒级）。
        // `/searchkey` 启动恰好撞上首轮枚举时，靠的就是这条挂起路径。
        if tab.loading {
            self.cur_mut().search_state = None;
            self.cur_mut().pending_search = true;
            self.request_repaint();
            return;
        }

        let mut backup = None;
        if tab.search_backup.is_none() {
            backup = Some(tab.entries.clone());
        }
        let scope = tab.path.clone();

        let id = self.search_seq.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let t = &mut self.tabs[idx];
            if let Some(b) = backup {
                t.search_backup = Some(b);
            }
            t.search_state = Some(SearchState::Running);
        }
        self.arm_search_timeout();
        self.request_repaint();
        self.io.send(Job::Search {
            id,
            tab: idx,
            scope,
            key,
            limit: SEARCH_LIMIT,
            budget_ms: SCAN_BUDGET_MS,
            latest: self.search_seq.clone(),
        });
    }

    /// 搜索等待上限的定时器：索引服务无响应时不能让「正在搜索…」一直转。
    pub(crate) fn arm_search_timeout(&mut self) {
        if !hwnd_valid(self.hwnd) {
            return;
        }
        unsafe {
            SetTimer(self.hwnd, TIMER_SEARCH, SEARCH_TIMEOUT_MS, None);
        }
    }

    pub(crate) fn kill_search_timeout(&mut self) {
        if !hwnd_valid(self.hwnd) {
            return;
        }
        unsafe {
            KillTimer(self.hwnd, TIMER_SEARCH);
        }
    }

    /// 搜索超时到点。后台可能还挂着，结果真回来时 `on_io` 仍会采纳。
    ///
    /// 定时器是全局的（一个窗口只有一个 `TIMER_SEARCH`），所以这里把
    /// **所有**还在跑的标签一起标成超时——搜索现在是每标签一份的。
    pub fn on_search_timeout(&mut self) {
        let mut any = false;
        for t in &mut self.tabs {
            if matches!(t.search_state, Some(SearchState::Running)) {
                t.search_state = Some(SearchState::TimedOut);
                any = true;
            }
        }
        if any {
            self.request_repaint();
        }
        self.kill_search_timeout();
    }

    pub(crate) fn restore_search_backup(&mut self) {
        // 让还在跑的搜索作废（请求号一变，后台自己会停）
        self.search_seq.fetch_add(1, Ordering::SeqCst);
        self.kill_search_timeout();
        self.cur_mut().search_state = None;
        // 用户主动退出搜索（清空关键词/ESC），挂起的那次也别再补
        self.cur_mut().pending_search = false;
        if let Some(backup) = self.cur_mut().search_backup.take() {
            let t = self.cur_mut();
            t.entries = backup;
            t.selected.clear();
            t.scroll_y = 0;
        }
        self.request_repaint();
}
}
