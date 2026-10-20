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

const FOF_MULTIDESTFILES: u16 = 0x0001;
const FOF_SILENT: u16 = 0x0004;
const FOF_NOCONFIRMATION: u16 = 0x0010;
const FOF_ALLOWUNDO: u16 = 0x0040;
const FOF_NOERRORUI: u16 = 0x0400;
const FOF_NOCONFIRMMKDIR: u16 = 0x0200;

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

/// 复制到目标目录。
pub fn copy_to(hwnd: HWND, items: &[PathBuf], target_dir: &Path) -> OpResult {
    let dest: Vec<PathBuf> = items.iter().map(|_| target_dir.to_path_buf()).collect();
    do_operation(hwnd, FO_COPY, items, &dest, 0)
}

/// 移动到目标目录。
pub fn move_to(hwnd: HWND, items: &[PathBuf], target_dir: &Path) -> OpResult {
    let dest: Vec<PathBuf> = items.iter().map(|_| target_dir.to_path_buf()).collect();
    do_operation(hwnd, FO_MOVE, items, &dest, 0)
}

/// 删除到回收站。
pub fn delete_to_recycle(hwnd: HWND, items: &[PathBuf]) -> OpResult {
    do_operation(hwnd, FO_DELETE, items, &[], 0)
}

/// 永久删除（Shift+Delete）。
pub fn delete_permanent(hwnd: HWND, items: &[PathBuf]) -> OpResult {
    // 清掉 ALLOWUNDO 位即可——重新走一次调用
    if items.is_empty() {
        return OpResult::Ok;
    }
    let from_buf = multi_string(items);
    let mut op: SHFILEOPSTRUCTW = unsafe { std::mem::zeroed() };
    op.hwnd = hwnd;
    op.wFunc = FO_DELETE;
    op.pFrom = from_buf.as_ptr();
    op.fFlags = FOF_NOCONFIRMMKDIR;
    let code = unsafe { SHFileOperationW(&mut op) };
    if op.fAnyOperationsAborted != 0 {
        return OpResult::Cancelled;
    }
    if code != 0 {
        return OpResult::Failed(describe_shell_error(code));
    }
    OpResult::Ok
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
    if target.exists() {
        return OpResult::Failed("同名项目已存在".to_string());
    }
    do_operation(hwnd, FO_RENAME, &[path.to_path_buf()], &[target], 0)
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
