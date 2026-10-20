//! 地址栏历史（对应 `Profile\addr_history.ini`）。
//!
//! 保持 ini 纯文本格式，方便用户手改，也避免为几条记录引入额外依赖。

use std::path::{Path, PathBuf};

const MAX_ITEMS: usize = 64;

#[derive(Debug, Clone, Default)]
pub struct AddrHistory {
    /// 最新的在最前。
    pub items: Vec<String>,
}

impl AddrHistory {
    fn file(root: &Path) -> PathBuf {
        root.join("addr_history.ini")
    }

    pub fn load(root: &Path) -> AddrHistory {
        let text = match std::fs::read_to_string(Self::file(root)) {
            Ok(t) => t,
            Err(_) => return AddrHistory::default(),
        };
        let mut items = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
                continue;
            }
            // 允许 "path=..." 与裸路径两种写法
            let value = line
                .strip_prefix("path=")
                .or_else(|| line.strip_prefix("Path="))
                .unwrap_or(line);
            if !value.is_empty() {
                items.push(value.to_string());
            }
            if items.len() >= MAX_ITEMS {
                break;
            }
        }
        AddrHistory { items }
    }

    pub fn save(&self, root: &Path) {
        if super::paths::ensure_dir(root).is_err() {
            return;
        }
        let mut out = String::with_capacity(self.items.len() * 48);
        out.push_str("; DreamFolder 地址栏历史（可手改，每行一条）\r\n");
        for it in self.items.iter().take(MAX_ITEMS) {
            out.push_str("path=");
            out.push_str(it);
            out.push_str("\r\n");
        }
        let _ = std::fs::write(Self::file(root), out);
    }

    /// 记录一次导航：去重、置顶、截断。
    pub fn record(&mut self, path: &str) {
        if path.is_empty() {
            return;
        }
        self.items.retain(|p| !p.eq_ignore_ascii_case(path));
        self.items.insert(0, path.to_string());
        if self.items.len() > MAX_ITEMS {
            self.items.truncate(MAX_ITEMS);
        }
    }

    /// 前缀匹配的补全候选（地址栏下拉用）。
    pub fn suggest(&self, prefix: &str, limit: usize) -> Vec<&str> {
        if prefix.is_empty() {
            return self.items.iter().take(limit).map(|s| s.as_str()).collect();
        }
        let lower = prefix.to_lowercase();
        self.items
            .iter()
            .filter(|p| p.to_lowercase().starts_with(&lower))
            .take(limit)
            .map(|s| s.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hist(items: &[&str]) -> AddrHistory {
        AddrHistory { items: items.iter().map(|s| s.to_string()).collect() }
    }

    /// 新记录置顶、去重（大小写不敏感）、超上限截断。
    #[test]
    fn record_dedups_and_moves_to_front() {
        let mut h = hist(&["C:/a", "C:/b"]);
        h.record("C:/B");
        assert_eq!(h.items, ["C:/B", "C:/a"]);

        h.record("C:/a");
        assert_eq!(h.items, ["C:/a", "C:/B"]);

        // 空串不记录
        h.record("");
        assert_eq!(h.items.len(), 2);

        for i in 0..MAX_ITEMS + 10 {
            h.record(&format!("C:/d{i}"));
        }
        assert_eq!(h.items.len(), MAX_ITEMS);
    }

    /// 前缀匹配不区分大小写，并且受 limit 限制。
    #[test]
    fn suggest_matches_prefix() {
        let h = hist(&["C:/Users/xiaos", "C:/Windows", "D:/Data"]);
        assert_eq!(h.suggest("c:/us", 8), ["C:/Users/xiaos"]);
        assert_eq!(h.suggest("C:/", 8).len(), 2);
        assert_eq!(h.suggest("C:/", 1).len(), 1);
        assert!(h.suggest("E:/", 8).is_empty());
    }

    /// 空前缀给出最近的几条（地址栏刚聚焦时用）。
    #[test]
    fn suggest_empty_prefix_returns_recent() {
        let h = hist(&["C:/a", "C:/b", "C:/c"]);
        assert_eq!(h.suggest("", 2), ["C:/a", "C:/b"]);
    }
}
