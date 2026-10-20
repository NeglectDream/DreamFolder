//! 搜索的三级回退。
//!
//! 顺序与资源管理器一致，逐级降级：
//!   1. Windows 搜索索引（`SystemIndex`）——快，但目录未必被索引；
//!   2. 索引不可用 / 无命中 → 当前目录子树递归扫描（有 400 ms 预算）；
//!   3. 两级都没结果 → 由 UI 侧在「已加载条目」里做名称包含匹配
//!      （「此电脑」「回收站」这类虚拟视图没有真实目录可扫）。
//!
//! 第 3 级放在 UI 侧是刻意的：那份条目列表本来就只存在于 UI 状态里，
//! 为了在后台做匹配而整份克隆过去不划算。
//!
//! 整个函数跑在后台线程上（见 `io::Job::Search`），`cancelled` 用来在
//! 用户改词/换目录时提前收手。

use std::path::Path;

use super::entry::Entry;
use super::enumerate::{entry_from_path, scan_subtree};

/// 在 `scope` 范围内搜索 `key`。
///
/// `limit` 是结果条数上限，`budget_ms` 只约束第 2 级的递归扫描。
pub fn search(
    scope: &Path,
    key: &str,
    limit: usize,
    budget_ms: u32,
    cancelled: impl Fn() -> bool,
) -> Vec<Entry> {
    let key = key.trim();
    if key.is_empty() {
        return Vec::new();
    }

    // 1) 系统索引
    let mut hits: Vec<Entry> = Vec::new();
    if let Some(scope) = scope.to_str() {
        if let Ok(found) = crate::win::search::search_index(scope, key, limit) {
            for h in found {
                if cancelled() {
                    return Vec::new();
                }
                if let Some(e) = entry_from_path(Path::new(&h.path)) {
                    hits.push(e);
                }
            }
        }
    }

    // 2) 子树递归（索引不可用、目录未被索引、或索引里没有匹配）
    if hits.is_empty() && !cancelled() {
        hits = scan_subtree(scope, key, limit, budget_ms, &cancelled);
    }

    hits.truncate(limit);
    hits
}
