//! 文件系统访问层。

pub mod entry;
pub mod enumerate;
pub mod icons;
pub mod ops;
pub mod shell;

pub use entry::{Entry, EntryKind};
pub use enumerate::{list_dir, ListOptions, Sort, SortKey};

use std::path::{Path, PathBuf};

/// 把用户输入解析成绝对路径。
///
/// 支持：
///   - 普通绝对/相对路径
///   - `~` → 用户主目录
///   - `%VAR%` 环境变量展开
///   - 带引号的路径（拖放与命令行常见）
pub fn resolve_path(input: &str) -> Option<PathBuf> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let s = s.trim_matches('"').trim();

    // 环境变量展开
    let expanded = expand_env(s);

    // ~ 展开
    let expanded = if let Some(rest) = expanded.strip_prefix('~') {
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let rest = rest.trim_start_matches(['\\', '/']);
        if rest.is_empty() {
            home
        } else {
            format!("{home}\\{rest}")
        }
    } else {
        expanded
    };

    let p = PathBuf::from(&expanded);
    if p.exists() {
        return Some(dunce_canonicalize(&p));
    }
    // 不存在也返回，让上层给出"路径无效"提示
    Some(p)
}

/// 展开 `%NAME%` 形式的环境变量。
fn expand_env(s: &str) -> String {
    let bytes: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == '%' {
            if let Some(close) = bytes[i + 1..].iter().position(|&c| c == '%') {
                let name: String = bytes[i + 1..i + 1 + close].iter().collect();
                if !name.is_empty() {
                    if let Ok(v) = std::env::var(&name) {
                        out.push_str(&v);
                        i += close + 2;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// 规范化路径但不解析 UNC 前缀为 `\\?\`（保持用户可读），
/// 并去掉 `.` / `..` 与冗余分隔符。
pub fn dunce_canonicalize(p: &Path) -> PathBuf {
    // 先试 std 的 canonicalize（返回的是 \\?\ 形式）
    if let Ok(c) = std::fs::canonicalize(p) {
        let s = c.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{stripped}"));
        }
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
        return PathBuf::from(s.into_owned());
    }
    clean_path(p)
}

/// 纯词法清理：折叠 `.` 与 `..`，统一分隔符。
pub fn clean_path(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out: Vec<std::ffi::OsString> = Vec::new();
    let mut prefix: Option<Component> = None;
    let mut rooted = false;

    for comp in p.components() {
        match comp {
            Component::Prefix(_) => prefix = Some(comp),
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.is_empty() {
                    out.pop();
                }
            }
            Component::Normal(s) => out.push(s.to_os_string()),
        }
    }

    let mut result = PathBuf::new();
    if let Some(Component::Prefix(pfx)) = prefix {
        result.push(pfx.as_os_str());
    }
    if rooted {
        result.push(std::path::MAIN_SEPARATOR.to_string());
    }
    for seg in out {
        result.push(seg);
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}

/// 父目录（根目录返回 None）。
pub fn parent_of(p: &Path) -> Option<PathBuf> {
    let cleaned = clean_path(p);
    let parent = cleaned.parent()?;
    if parent.as_os_str().is_empty() {
        None
    } else {
        Some(parent.to_path_buf())
    }
}

/// 路径是否为驱动器根（`C:\`、`D:\`）。
pub fn is_drive_root(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    (b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/'))
        || (b.len() == 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
}

/// 枚举本机可用驱动器（"此电脑"视图）。
pub fn drives() -> Vec<PathBuf> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
    let mut out = Vec::new();
    let mask = unsafe { GetLogicalDrives() };
    for i in 0..26u32 {
        if mask & (1 << i) != 0 {
            let letter = (b'A' + i as u8) as char;
            out.push(PathBuf::from(format!("{letter}:\\")));
        }
    }
    out
}
