//! 图片缩略图缓存：按路径存解码好的 `HICON`。
//!
//! 大/中图标视图里图片文件不该只有扩展名图标——那和一排「JPEG 图标」
//! 没区别，用户找图全靠读文件名。这里把「文件内容 → 缩略图句柄」的
//! 结果缓存起来；解码在后台线程做（见 `io::Job::Thumb`），绘制路径
//! 只查表，查不到就发请求 + 用普通图标占位。
//!
//! 句柄生命周期同 `IconCache`：capped 表，超限整表清空（见
//! `store_capped` 的注释——简单比 LRU 链表值钱）。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use windows_sys::Win32::UI::WindowsAndMessaging::{DestroyIcon, HICON};

/// 每档最多缓存多少张缩略图。
///
/// 96×96×4 字节 ≈ 36KB 一张，256 张约 9MB，一屏可见条目最多几十个，
/// 这个量级翻页也够用；超限整表清空，代价只是回头再解一遍。
const THUMB_CACHE_MAX: usize = 256;

/// 缩略图统一解到 96px（最大视图档）。
///
/// 小视图画的时候拉小即可；按路径缓存只有一个档位，切视图不重解。
pub const THUMB_SIDE: i32 = 96;

pub struct ThumbCache {
    /// 路径 → `HICON`。`0` 表示「解码失败」，也占着键防反复重试
    /// （GIF 等少数格式 GDI+ 解不动时，不能每帧都再解一次）。
    map: HashMap<PathBuf, isize>,
    /// 已发给后台、还没回来的请求，防同一帧内重复投递。
    inflight: HashSet<PathBuf>,
}

impl Default for ThumbCache {
    fn default() -> Self {
        ThumbCache {
            map: HashMap::new(),
            inflight: HashSet::new(),
        }
    }
}

impl ThumbCache {
    /// 查缩略图。`None` = 还没解过（该发请求）；
    /// `Some(0)` = 解过了但失败（画占位图标，别再请求）。
    pub fn get(&self, path: &Path) -> Option<isize> {
        self.map.get(path).copied()
    }

    /// 要求发起一次解码。返回 `true` 表示确实需要发（调用方据此投递
    /// `Job::Thumb`）；已在途或已有结果的路径不会再发。
    pub fn request(&mut self, path: &Path) -> bool {
        if self.map.contains_key(path) || self.inflight.contains(path) {
            return false;
        }
        self.inflight.insert(path.to_path_buf());
        true
    }

    /// 回填一个结果，返回需要销毁的旧句柄（整表清空时交出来）。
    ///
    /// 与 `icons::store_capped` 相同的取舍：命中率来自「同一目录的
    /// 图片反复可见」，超限时整表清空比维护 LRU 链表简单得多。
    pub fn store(&mut self, path: PathBuf, icon: isize) -> Vec<isize> {
        self.inflight.remove(&path);
        let mut stale = Vec::new();
        if !self.map.contains_key(&path) && self.map.len() >= THUMB_CACHE_MAX {
            stale.extend(self.map.values().copied().filter(|h| *h != 0));
            self.map.clear();
        }
        self.map.insert(path, icon);
        stale
    }

    /// 销毁一批句柄。
    ///
    /// 独立成一个函数而不是在 `Drop` 里静默做：换目录清缓存时调用方
    /// 也走同一条销毁路径，句柄的生与死都看得到。
    pub fn destroy(handles: &[isize]) {
        for h in handles {
            if *h != 0 {
                unsafe {
                    DestroyIcon(*h as HICON);
                }
            }
        }
    }
}

impl Drop for ThumbCache {
    fn drop(&mut self) {
        let handles: Vec<isize> = self.map.values().copied().collect();
        Self::destroy(&handles);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// 在途防抖：请求过一次的路径，结果回来之前不再发。
    #[test]
    fn request_dedups_until_store() {
        let mut c = ThumbCache::default();
        let path = p("C:/a.jpg");
        assert!(c.request(&path));
        assert!(!c.request(&path)); // 在途，不重发
        c.store(path.clone(), 0x1234);
        assert!(!c.request(&path)); // 已有结果，不重发
        assert_eq!(c.get(&path), Some(0x1234));
    }

    /// 解码失败（句柄为 0）也要占住键：否则每帧重试一次。
    #[test]
    fn failed_decode_is_cached() {
        let mut c = ThumbCache::default();
        let path = p("C:/broken.gif");
        assert!(c.request(&path));
        c.store(path.clone(), 0);
        assert_eq!(c.get(&path), Some(0));
        assert!(!c.request(&path));
    }
}
