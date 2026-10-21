//! 文件操作：新建、重命名、删除、复制、移动。
//!
//! 统一走 `SHFileOperationW`——由 Shell 完成，天然带：
//!   - 回收站支持（可撤销）
//!   - 长路径处理
//!   - 冲突对话框与进度条
//!   - 跨卷移动的正确回退（复制+删除）
//! 自己实现这些细节只会更慢更容易出错。

use std::path::{Path, PathBuf};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::{SHFileOperationW, SHFILEOPSTRUCTW};

use crate::win::to_wide;

// 操作码与标志（显式取值，不依赖常量导出）
const FO_MOVE: u32 = 0x0001;
const FO_COPY: u32 = 0x0002;
const FO_DELETE: u32 = 0x0003;
const FO_RENAME: u32 = 0x0004;

const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_NOCONFIRMMKDIR: u16 = 0x0200;
const FOF_NOCONFIRMATION: u16 = 0x0010;

/// 操作结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpResult {
    Ok,
    /// 用户取消。
    Cancelled,
    Failed(String),
}

/// 把路径列表编码成 `SHFileOperation` 需要的「双 NUL 结尾」格式。
fn multi_string(paths: &[PathBuf]) -> Vec<u16> {
    let mut buf = Vec::with_capacity(paths.len() * 64 + 2);
    for p in paths {
        buf.extend(p.to_string_lossy().encode_utf16());
        buf.push(0);
    }
    buf.push(0); // 结尾追加的额外 NUL
    buf
}

/// 执行一次 Shell 文件操作。
fn do_operation(
    hwnd: HWND,
    func: u32,
    from: &[PathBuf],
    to: &[PathBuf],
    extra_flags: u16,
) -> OpResult {
    if from.is_empty() {
        return OpResult::Ok;
    }

    let from_buf = multi_string(from);
    let to_buf = if to.is_empty() {
        Vec::new()
    } else {
        multi_string(to)
    };

    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.hwnd = hwnd;
    op.wFunc = func;
    op.pFrom = from_buf.as_ptr();
    op.pTo = if to_buf.is_empty() {
        std::ptr::null()
    } else {
        to_buf.as_ptr()
    };
    // 允许撤销（回收站）+ 由 Shell 弹冲突/进度 UI
    op.fFlags = FOF_ALLOWUNDO | FOF_NOCONFIRMMKDIR | extra_flags;

    let code = unsafe { SHFileOperationW(&mut op) };

    if op.fAnyOperationsAborted != 0 {
        return OpResult::Cancelled;
    }
    if code != 0 {
        return OpResult::Failed(describe_shell_error(code));
    }
    OpResult::Ok
}

/// 把 SHFileOperation 的错误码翻译成人话。
fn describe_shell_error(code: i32) -> String {
    match code {
        0x71 => "源和目标相同".to_string(),
        0x72 => "拒绝访问".to_string(),
        0x73 => "文件正在被使用".to_string(),
        0x74 => "磁盘空间不足".to_string(),
        0x75 => "路径过长或无效".to_string(),
        0x76 => "路径中包含非法字符".to_string(),
        0x78 => "目标已存在".to_string(),
        0x79 => "找不到源文件".to_string(),
        0x7A => "路径无效".to_string(),
        0x7B => "名称无效".to_string(),
        0x7C => "无法复制到自身".to_string(),
        0x7D => "无法移动自身".to_string(),
        _ => format!("操作失败（代码 0x{code:X}）"),
    }
}

// ---------------------------------------------------------------- 公开操作

/// 路径是不是已经长到 `SHFileOperationW` 处理不了。
///
/// `SHFileOperation` 的 `pFrom`/`pTo` **不接受** `\\?\` 前缀，于是超过
/// MAX_PATH 的路径它只会回「路径过长或无效」。这里按 248 个 UTF-16 码元
/// 预判（留出拼接文件名的余量），超了就换一条实现。
fn too_long_for_shell(paths: &[PathBuf]) -> bool {
    paths.iter().any(|p| {
        let units: usize = p.to_string_lossy().chars().map(|c| c.len_utf16()).sum();
        units >= 248
    })
}

/// 递归复制一项。目录自己建，文件走 `std::fs::copy`。
///
/// Rust 的 `std::fs` 在 Windows 上会自动给长路径加 `\\?\`，所以这条路径
/// 天然支持超过 MAX_PATH 的名字——这正是 Shell 那条路走不通的地方。
fn copy_entry_long(src: &Path, dst_dir: &Path) -> Result<(), String> {
    let Some(name) = src.file_name() else {
        return Err(format!("无法确定名称：{}", src.display()));
    };
    let dst = dst_dir.join(name);
    if src.is_dir() {
        std::fs::create_dir_all(&dst).map_err(|e| format!("{}：{e}", dst.display()))?;
        let rd = std::fs::read_dir(src).map_err(|e| format!("{}：{e}", src.display()))?;
        for entry in rd {
            let entry = entry.map_err(|e| format!("{}：{e}", src.display()))?;
            copy_entry_long(&entry.path(), &dst)?;
        }
        Ok(())
    } else {
        std::fs::copy(src, &dst)
            .map(|_| ())
            .map_err(|e| format!("{}：{e}", src.display()))
    }
}

fn long_copy(items: &[PathBuf], target_dir: &Path) -> OpResult {
    for it in items {
        if let Err(msg) = copy_entry_long(it, target_dir) {
            return OpResult::Failed(format!("复制失败：{msg}"));
        }
    }
    OpResult::Ok
}

fn long_move(items: &[PathBuf], target_dir: &Path) -> OpResult {
    for it in items {
        let Some(name) = it.file_name() else {
            return OpResult::Failed(format!("无法确定名称：{}", it.display()));
        };
        let dst = target_dir.join(name);
        if dst.exists() {
            // Shell 路径会弹「替换/跳过」，长路径降级路径也必须有同样的
            // 底线：直接落入 copy 会静默截断覆盖目标，数据没了还不可撤销。
            return OpResult::Failed(format!("目标已存在：{}", dst.display()));
        }
        // 同卷直接改名；跨卷会失败，退化成「先复制再删源」
        if std::fs::rename(it, &dst).is_err() {
            if let Err(msg) = copy_entry_long(it, target_dir) {
                return OpResult::Failed(format!("移动失败：{msg}"));
            }
            if let Err(e) = remove_entry_long(it) {
                return OpResult::Failed(format!("移动失败（源未能删除）：{e}"));
            }
        }
    }
    OpResult::Ok
}

fn remove_entry_long(path: &Path) -> Result<(), String> {
    let res = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    res.map_err(|e| format!("{}：{e}", path.display()))
}

fn long_delete(items: &[PathBuf]) -> OpResult {
    for it in items {
        if let Err(msg) = remove_entry_long(it) {
            return OpResult::Failed(format!("删除失败：{msg}"));
        }
    }
    OpResult::Ok
}

/// 复制到目标目录。
pub fn copy_to(hwnd: HWND, items: &[PathBuf], target_dir: &Path) -> OpResult {
    // pTo 只给一个目录：`SHFileOperation` 的约定是「pTo 条目数与 pFrom 相同
    // 时才配 FOF_MULTIDESTFILES」，否则 pTo 表示单一目标。以前按源条目数
    // 重复填同一个目录又没置标志，属于把两种协议混着写。
    let mut all: Vec<PathBuf> = items.to_vec();
    all.push(target_dir.to_path_buf());
    if too_long_for_shell(&all) {
        return long_copy(items, target_dir);
    }
    do_operation(hwnd, FO_COPY, items, &[target_dir.to_path_buf()], 0)
}

/// 移动到目标目录。
pub fn move_to(hwnd: HWND, items: &[PathBuf], target_dir: &Path) -> OpResult {
    let mut all: Vec<PathBuf> = items.to_vec();
    all.push(target_dir.to_path_buf());
    if too_long_for_shell(&all) {
        return long_move(items, target_dir);
    }
    do_operation(hwnd, FO_MOVE, items, &[target_dir.to_path_buf()], 0)
}

/// 删除到回收站。
pub fn delete_to_recycle(hwnd: HWND, items: &[PathBuf]) -> OpResult {
    if too_long_for_shell(items) {
        // 回收站只认 Shell 那条路，而 Shell 那条路不认 `\\?\`。
        // 与其让系统报一句语焉不详的「路径过长或无效」，不如说清楚：
        // 这条路径进不了回收站，想删得用永久删除。
        return OpResult::Failed(
            "路径过长，Shell 无法把它放进回收站；改用 Shift+Delete 可直接删除".to_string(),
        );
    }
    do_operation(hwnd, FO_DELETE, items, &[], 0)
}

/// 永久删除（Shift+Delete）。
pub fn delete_permanent(hwnd: HWND, items: &[PathBuf]) -> OpResult {
    // 长路径走不了 Shell：降级为逐条删除（带元数据清理），
    // 其余路径照旧。确认框由应用层弹过了（自绘、模态于主窗口）；
    // 不加 NOCONFIRMATION 的话 Shell 会从后台线程再弹一次系统确认框，
    // 不模态、可能落到主窗口后面，看起来就像卡死。
    if too_long_for_shell(items) {
        return long_delete(items);
    }
    do_operation(hwnd, FO_DELETE, items, &[], FOF_NOCONFIRMATION)
}

/// 重命名（同目录内改名）。单条目专用。
pub fn rename(hwnd: HWND, path: &Path, new_name: &str) -> OpResult {
    let name = new_name.trim();
    if name.is_empty() {
        return OpResult::Failed("名称不能为空".to_string());
    }
    if name.contains(['\\', '/', ':', '*', '?', '"', '<', '>', '|']) {
        return OpResult::Failed("名称中包含非法字符 \\ / : * ? \" < > |".to_string());
    }
    let parent = match path.parent() {
        Some(p) => p,
        None => return OpResult::Failed("无法确定父目录".to_string()),
    };
    let target = parent.join(name);
    if target == path {
        return OpResult::Ok;
    }

    // 只改大小写（`abc.txt` → `ABC.txt`）：NTFS 下 `target.exists()` 会因为
    // 「同一个文件」返回真，任何 `exists()` 预检都会把它误判成重名——这是一
    // 个再常见不过的操作。Shell 的 FO_RENAME 在这种同名文件上也不可靠，
    // 直接走 MoveFileExW：它按名字改大小写，且不带 REPLACE_EXISTING，
    // 不会覆盖任何别的东西。
    //
    // 其余情况不做预检：冲突交给 Shell 自己判定（它会弹「是否替换」），
    // 这样既没有 TOCTOU 窗口，也不会把「目标已存在」说死在应用层。
    if is_case_only_change(path, &target) {
        return move_file_exact(path, &target);
    }
    do_operation(hwnd, FO_RENAME, &[path.to_path_buf()], &[target], 0)
}

/// 两个路径是否只差大小写（其余部分完全一致）。
fn is_case_only_change(from: &Path, to: &Path) -> bool {
    let (a, b) = (from.to_string_lossy(), to.to_string_lossy());
    a != b && a.to_lowercase() == b.to_lowercase()
}

/// 改名且绝不覆盖：`MoveFileExW` 不带 `MOVEFILE_REPLACE_EXISTING`。
fn move_file_exact(from: &Path, to: &Path) -> OpResult {
    use windows_sys::Win32::Storage::FileSystem::MoveFileExW;

    let from_w = crate::win::to_wide_path(from);
    let to_w = crate::win::to_wide_path(to);
    let ok = unsafe { MoveFileExW(from_w.as_ptr(), to_w.as_ptr(), 0) };
    if ok != 0 {
        return OpResult::Ok;
    }
    let code = std::io::Error::last_os_error();
    OpResult::Failed(format!("重命名失败：{code}"))
}

/// 新建文件夹。返回创建出的路径。
///
/// 名称冲突时按「新建文件夹 (2)」的方式自动编号，与资源管理器一致。
pub fn create_folder(parent: &Path) -> Result<PathBuf, String> {
    if !parent.is_dir() {
        return Err("当前目录不可写".to_string());
    }
    let base = "新建文件夹";
    let mut candidate = parent.join(base);
    if candidate.exists() {
        let mut n = 2;
        loop {
            candidate = parent.join(format!("{base} ({n})"));
            if !candidate.exists() {
                break;
            }
            n += 1;
            if n > 9999 {
                return Err("无法生成唯一名称".to_string());
            }
        }
    }
    std::fs::create_dir(&candidate).map_err(|e| format!("创建失败：{e}"))?;
    Ok(candidate)
}

/// 新建空文件（右键「新建 → 文本文档」等）。
pub fn create_file(parent: &Path, base_name: &str, ext: &str) -> Result<PathBuf, String> {
    let full_name = if ext.is_empty() {
        base_name.to_string()
    } else {
        format!("{base_name}.{ext}")
    };
    let mut candidate = parent.join(&full_name);
    if candidate.exists() {
        let mut n = 2;
        loop {
            candidate = parent.join(if ext.is_empty() {
                format!("{base_name} ({n})")
            } else {
                format!("{base_name} ({n}).{ext}")
            });
            if !candidate.exists() {
                break;
            }
            n += 1;
            if n > 9999 {
                return Err("无法生成唯一名称".to_string());
            }
        }
    }
    std::fs::File::create(&candidate).map_err(|e| format!("创建失败：{e}"))?;
    Ok(candidate)
}

/// 在资源管理器中定位并选中该项目。
///
/// 三个参数都必须 NUL 结尾（`ShellExecuteW` 靠结尾 0 定界），
/// 所以用 `to_wide` 而不是 `to_wide_raw`。
pub fn reveal_in_explorer(path: &Path) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let arg = if path.is_dir() {
        to_wide(&path.to_string_lossy())
    } else {
        to_wide(&format!("/select,\"{}\"", path.display()))
    };
    let verb = to_wide("open");
    let exe = to_wide("explorer.exe");
    let ret = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            exe.as_ptr(),
            arg.as_ptr(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    ret as isize > 32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 双 NUL 结尾：每项一个 NUL，末尾再补一个。
    #[test]
    fn multi_string_is_double_nul_terminated() {
        let buf = multi_string(&[PathBuf::from("C:/a"), PathBuf::from("C:/b")]);
        assert_eq!(
            String::from_utf16_lossy(&buf),
            "C:/a\0C:/b\0\0".to_string()
        );
        assert_eq!(*buf.last().unwrap(), 0);
        // 单项也要两个 NUL：Shell 靠这个判断列表结束
        let one = multi_string(&[PathBuf::from("C:/only")]);
        assert_eq!(one.iter().filter(|&&c| c == 0).count(), 2);
    }

    /// 只差大小写的改名要认出来（`abc.txt` → `ABC.txt`）。
    #[test]
    fn case_only_change_detected() {
        assert!(is_case_only_change(
            Path::new("C:/dir/abc.txt"),
            Path::new("C:/dir/ABC.txt")
        ));
        // 完全一样不算「只差大小写」
        assert!(!is_case_only_change(
            Path::new("C:/dir/a.txt"),
            Path::new("C:/dir/a.txt")
        ));
        // 名字真的变了也不算
        assert!(!is_case_only_change(
            Path::new("C:/dir/a.txt"),
            Path::new("C:/dir/b.txt")
        ));
        // 换了目录也不算
        assert!(!is_case_only_change(
            Path::new("C:/dir/a.txt"),
            Path::new("C:/other/a.txt")
        ));
    }

    /// 长路径预判：接近 MAX_PATH 的路径要交给降级实现。
    #[test]
    fn long_path_threshold() {
        let short = PathBuf::from(format!("C:/{}", "a".repeat(240)));
        assert!(!too_long_for_shell(&[short]));

        let long = PathBuf::from(format!("C:/{}", "a".repeat(250)));
        assert!(too_long_for_shell(&[long]));

        // 多目标里只要有一个超限，整批都走降级实现
        let mixed = vec![
            PathBuf::from("C:/short.txt"),
            PathBuf::from(format!("C:/{}", "b".repeat(300))),
        ];
        assert!(too_long_for_shell(&mixed));

        assert!(!too_long_for_shell(&[]));
    }
}
