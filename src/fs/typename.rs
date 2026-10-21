//! 扩展名 → 类型显示名（"txt" → "文本文档"）。
//!
//! 独立成全局缓存而不是继续挂在 `IconCache` 上，是因为「类型」列排序发生在
//! 枚举工作线程（`sort_entries`），那里拿不到 UI 线程的 `IconCache`——
//! 而排序键必须与用户看到的文字**完全一致**，否则会出现「显示『文本文档』
//! 却排在『位图图像』后面」这种看着就像 bug 的效果。

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// 扩展名缓存。键是折叠成小写的扩展名，值是 Shell 给出的类型描述。
///
/// 一个目录里的不同扩展名通常只有几十种，缓存自然很小；不用做上限淘汰。
fn cache() -> &'static Mutex<HashMap<String, String>> {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::with_capacity(64)))
}

/// 取扩展名的类型显示名，带缓存。`ext` 传小写不带点的形式。
pub fn for_ext(ext: &str) -> String {
    let key = ext.to_ascii_lowercase();
    let guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = guard.get(&key) {
        return v.clone();
    }
    drop(guard);

    let name = if key.is_empty() {
        crate::strings::TYPE_FILE.to_string()
    } else {
        let fake = crate::win::to_wide(&format!("x.{key}"));
        // 先放锁外算：Shell 查询可能要几毫秒，不能让别的线程在锁上等
        unsafe { shell_type_name(fake.as_ptr()) }
    };
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, name.clone());
    name
}

/// 目录的类型显示名。
pub fn folder() -> &'static str {
    crate::strings::TYPE_FOLDER
}

/// 取文件类型描述。
///
/// # Safety
/// `path` 必须是 NUL 结尾的宽串指针。
unsafe fn shell_type_name(path: *const u16) -> String {
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows_sys::Win32::UI::Shell::{
        SHGetFileInfoW, SHGFI_TYPENAME, SHGFI_USEFILEATTRIBUTES, SHFILEINFOW,
    };

    let mut info: SHFILEINFOW = std::mem::zeroed();
    let ret = SHGetFileInfoW(
        path,
        FILE_ATTRIBUTE_NORMAL,
        &mut info,
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_TYPENAME | SHGFI_USEFILEATTRIBUTES,
    );
    if ret == 0 {
        return crate::strings::TYPE_FILE.to_string();
    }
    crate::win::from_wide(&info.szTypeName)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 无扩展名与重复查询都不能 panic，且同一扩展名两次拿到同一结果。
    #[test]
    fn caches_and_is_stable() {
        let a = for_ext("txt");
        let b = for_ext("TXT");
        assert_eq!(a, b);
        assert!(!for_ext("").is_empty());
        assert_eq!(folder(), crate::strings::TYPE_FOLDER);
    }
}
