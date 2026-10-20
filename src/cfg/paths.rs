//! 配置目录解析。
//!
//! 优先使用**可执行文件同目录**下的 `Profile\`（便携式，与原软件一致：
//! 原软件使用 `X:\Windows\WinXShell\360FileBrowser\Profile`）。
//! 该目录不可写时（如装在 Program Files）回退到漫游用户目录。

use std::path::{Path, PathBuf};

/// 程序所在目录。
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// 便携模式 Profile 目录（程序目录下）。
pub fn portable_profile() -> PathBuf {
    exe_dir().join("Profile")
}

/// 回退 Profile 目录（漫游用户目录下）。
pub fn roaming_profile() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("DreamFolder").join("Profile")
}

/// 选定的 Profile 目录。**只读判断**，不写盘。
///
/// 判定规则：若便携目录已存在则用它；否则若程序目录可写则用便携目录；
/// 再否则用漫游目录。三者取不到就退回便携路径（后续写入会失败但不会崩）。
pub fn profile_dir() -> PathBuf {
    let portable = portable_profile();
    if portable.is_dir() {
        return portable;
    }
    if dir_writable(&exe_dir()) {
        return portable;
    }
    let roaming = roaming_profile();
    if roaming.is_dir() {
        return roaming;
    }
    // 都不可用时仍返回漫游路径：至少不会污染程序目录
    roaming
}

/// 崩溃日志路径（程序目录优先，其次漫游目录）。
pub fn crash_log() -> PathBuf {
    let p = exe_dir().join("dreamfolder-crash.log");
    if dir_writable(&exe_dir()) {
        return p;
    }
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("DreamFolder").join("dreamfolder-crash.log")
}

/// 试探目录是否可写：创建一个临时文件再删掉。
/// 只在选目录时调用一次，代价可接受。
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".dreamfolder_write_probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// 确保目录存在（写盘前的唯一入口）。
pub fn ensure_dir(dir: &Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)
}
