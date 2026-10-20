//! 会话：上次打开的标签页（对应 `Profile\lasttabs.dat`）。
//!
//! 行式文本，无依赖、可手改：
//! ```text
//! DreamFolder session v1
//! C:\Users\me\Desktop<TAB>medium<TAB>1
//! D:\Projects<TAB>detail<TAB>0
//! ```
//! 第三列 1 表示该标签为当前激活页。

use super::settings::ViewMode;
use std::path::{Path, PathBuf};

const HEADER: &str = "DreamFolder session v1";
const MAX_TABS: usize = 64;

#[derive(Debug, Clone)]
pub struct TabRecord {
    pub path: PathBuf,
    pub view: ViewMode,
    pub active: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Session {
    pub tabs: Vec<TabRecord>,
    /// 上次退出时窗口是否最大化。
    pub maximized: bool,
}

impl Session {
    fn file(root: &Path) -> PathBuf {
        root.join("lasttabs.dat")
    }

    pub fn load(root: &Path) -> Session {
        let text = match std::fs::read_to_string(Self::file(root)) {
            Ok(t) => t,
            Err(_) => return Session::default(),
        };
        let mut lines = text.lines();
        let mut tabs = Vec::new();
        let mut maximized = false;

        if let Some(first) = lines.next() {
            let first = first.trim();
            if first != HEADER {
                // 容错：不认识的头也继续按数据解析
                if !first.is_empty() {
                    if let Some(t) = parse_line(first) {
                        tabs.push(t);
                    }
                }
            }
        }
        for line in lines {
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("maximized=") {
                maximized = rest.trim() == "1";
                continue;
            }
            if let Some(t) = parse_line(line) {
                tabs.push(t);
            }
            if tabs.len() >= MAX_TABS {
                break;
            }
        }
        Session { tabs, maximized }
    }

    pub fn save(&self, root: &Path) {
        if super::paths::ensure_dir(root).is_err() {
            return;
        }
        let mut out = String::new();
        out.push_str(HEADER);
        out.push_str("\r\n");
        out.push_str(if self.maximized {
            "maximized=1\r\n"
        } else {
            "maximized=0\r\n"
        });
        for t in self.tabs.iter().take(MAX_TABS) {
            out.push_str(&t.path.to_string_lossy());
            out.push('\t');
            out.push_str(view_token(t.view));
            out.push('\t');
            out.push(if t.active { '1' } else { '0' });
            out.push_str("\r\n");
        }
        let _ = std::fs::write(Self::file(root), out);
    }
}

fn parse_line(line: &str) -> Option<TabRecord> {
    let mut it = line.split('\t');
    let path = it.next()?.trim();
    if path.is_empty() {
        return None;
    }
    let view = it.next().map(parse_view).unwrap_or(ViewMode::Medium);
    let active = it.next().map(|s| s.trim() == "1").unwrap_or(false);
    Some(TabRecord {
        path: PathBuf::from(path),
        view,
        active,
    })
}

fn view_token(v: ViewMode) -> &'static str {
    match v {
        ViewMode::LargeEx => "largeex",
        ViewMode::Large => "large",
        ViewMode::Medium => "medium",
        ViewMode::Small => "small",
        ViewMode::List => "list",
        ViewMode::Detail => "detail",
        ViewMode::Tile => "tile",
        ViewMode::Content => "content",
    }
}

fn parse_view(s: &str) -> ViewMode {
    match s.trim() {
        "largeex" => ViewMode::LargeEx,
        "large" => ViewMode::Large,
        "small" => ViewMode::Small,
        "list" => ViewMode::List,
        "detail" => ViewMode::Detail,
        "tile" => ViewMode::Tile,
        "content" => ViewMode::Content,
        _ => ViewMode::Medium,
    }
}
