//! 目录枚举与排序。
//!
//! 性能要点（这是打开大目录时的热点）：
//!   1. `FindExInfoBasic` —— 不请求 8.3 短名，省一次目录查找
//!   2. `FIND_FIRST_EX_LARGE_FETCH` —— 内核一次返回更多条目，减少往返
//!   3. 只做一次 `Vec` 预留，避免反复扩容
//!   4. 排序在枚举线程完成，UI 线程不做 N log N

use windows_sys::Win32::Foundation::{FILETIME, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FindClose, FindFirstFileExW, FindNextFileW, FindExInfoBasic, FindExSearchNameMatch,
    WIN32_FIND_DATAW, FIND_FIRST_EX_LARGE_FETCH,
};

use super::entry::{filetime_from_parts, Entry, EntryKind, IconKey};
use crate::win::to_wide;
use std::path::{Path, PathBuf};

// 常量在本 crate 内自备，避免依赖 windows-sys 的导出差异。
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// 排序键。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Kind,
    Modified,
}

/// 排序方式。
#[derive(Debug, Clone, Copy)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort {
            key: SortKey::Name,
            descending: false,
        }
    }
}

/// 枚举选项。
#[derive(Debug, Clone)]
pub struct ListOptions {
    pub show_hidden: bool,
    pub show_system: bool,
    pub sort: Sort,
}

impl Default for ListOptions {
    fn default() -> Self {
        ListOptions {
            show_hidden: false,
            show_system: false,
            sort: Sort::default(),
        }
    }
}

/// 枚举结果。
pub struct Listing {
    pub entries: Vec<Entry>,
    /// 因权限/占用等原因被跳过的条目数（非 0 时状态栏可提示）。
    pub skipped: usize,
}

/// 枚举目录内容。
///
/// 路径为驱动器根（`C:\`）时额外插入「此电脑」的驱动器列表之外的内容
/// 不变——驱动器列表由空路径视图单独处理，见 `list_computer`。
pub fn list_dir(dir: &Path, opts: &ListOptions) -> Result<Listing, String> {
    if !dir.is_dir() {
        return Err(format!("路径不是目录：{}", dir.display()));
    }

    // 构造 \\?\ 前缀避开 MAX_PATH 限制（长路径支持）
    let pattern = match dir.to_str() {
        Some(s) if s.starts_with(r"\\") => {
            // UNC：\\?\UNC\server\share\*
            let stripped = s.trim_start_matches('\\');
            format!(r"\\?\UNC\{stripped}\*")
        }
        Some(s) => {
            let mut base = s.to_string();
            if !base.ends_with('\\') && !base.ends_with('/') {
                base.push('\\');
            }
            format!(r"\\?\{base}*")
        }
        None => return Err("路径包含无法表示的字符".to_string()),
    };

    let pattern_w = to_wide(&pattern);

    let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
    let handle: HANDLE = unsafe {
        FindFirstFileExW(
            pattern_w.as_ptr(),
            FindExInfoBasic,
            &mut data as *mut _ as *mut core::ffi::c_void,
            FindExSearchNameMatch,
            std::ptr::null(),
            FIND_FIRST_EX_LARGE_FETCH,
        )
    };

    if handle == INVALID_HANDLE_VALUE {
        return Err(format!("无法访问：{}", dir.display()));
    }

    let dir_prefix = {
        let mut s = dir.to_string_lossy().into_owned();
        if !s.ends_with('\\') {
            s.push('\\');
        }
        s
    };

    // 预估容量：目录条目数常在 10~2000，先给 64 起步能减少多数扩容
    let mut entries: Vec<Entry> = Vec::with_capacity(64);
    let mut skipped = 0usize;

    loop {
        let name = crate::win::from_wide(&data.cFileName);
        // 跳过 "." 与 ".."
        if name != "." && name != ".." && !name.is_empty() {
            let attrs = data.dwFileAttributes;
            let hidden = attrs & 0x2 != 0;
            let system = attrs & 0x4 != 0;

            let keep = (!hidden || opts.show_hidden) && (!system || opts.show_system);
            if keep {
                let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                // 用字节比较判断 .lnk 后缀：UTF-8 多字节序列的后续字节 >= 0x80，
                // 不可能等于 b"k"，因此不会在多字节字符中间切断。
                let is_lnk = !is_dir
                    && name.len() >= 4
                    && name.as_bytes()[name.len() - 4..].eq_ignore_ascii_case(b".lnk");

                let kind = if is_dir {
                    EntryKind::Directory
                } else if is_lnk {
                    EntryKind::Shortcut
                } else {
                    EntryKind::File
                };

                let size = if is_dir {
                    0
                } else {
                    filetime_from_parts(data.nFileSizeLow, data.nFileSizeHigh)
                };

                let modified = filetime_from_parts(
                    data.ftLastWriteTime.dwLowDateTime,
                    data.ftLastWriteTime.dwHighDateTime,
                );

                // 跳过重解析点（符号链接/挂载点）的递归遍历风险由上层处理，
                // 这里仅正常列出。
                let _ = FILE_ATTRIBUTE_REPARSE_POINT;

                entries.push(Entry {
                    full: PathBuf::from(format!("{dir_prefix}{name}")),
                    name,
                    kind,
                    size,
                    modified,
                    attrs,
                    type_name: String::new(),
                    icon_key: IconKey::Pending,
                });
            }
        }

        let ok = unsafe { FindNextFileW(handle, &mut data) };
        if ok == 0 {
            break;
        }

        // 防御：极端情况下（如设备目录）条数异常时截断
        if entries.len() > 200_000 {
            skipped += 1;
            break;
        }
    }

    unsafe {
        FindClose(handle);
    }

    sort_entries(&mut entries, &opts.sort);

    Ok(Listing { entries, skipped })
}

/// 枚举「此电脑」视图：驱动器列表。
pub fn list_computer(opts: &ListOptions) -> Listing {
    let mut entries = Vec::with_capacity(8);
    for d in super::drives() {
        let label = drive_label(&d);
        let name = if label.is_empty() {
            d.to_string_lossy().into_owned()
        } else {
            format!("{} ({})", label, d.to_string_lossy().trim_end_matches('\\'))
        };
        entries.push(Entry {
            name,
            full: d,
            kind: EntryKind::Drive,
            size: 0,
            modified: 0,
            attrs: FILE_ATTRIBUTE_DIRECTORY,
            type_name: "本地磁盘".to_string(),
            icon_key: IconKey::Drive,
        });
    }
    sort_entries(&mut entries, &opts.sort);
    Listing { entries, skipped: 0 }
}

/// 取驱动器卷标（失败返回空串）。
fn drive_label(drive: &Path) -> String {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;
    let root = to_wide(&drive.to_string_lossy());
    let mut label = [0u16; 64];
    let mut fs_name = [0u16; 32];
    let mut serial = 0u32;
    let mut max_len = 0u32;
    let mut flags = 0u32;
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            &mut serial,
            &mut max_len,
            &mut flags,
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        String::new()
    } else {
        crate::win::from_wide(&label)
    }
}

/// 排序：目录恒在文件之前，随后按选定键比较。
pub fn sort_entries(entries: &mut [Entry], sort: &Sort) {
    entries.sort_by(|a, b| {
        // 1) 目录优先（始终升序，不受 descending 影响——资源管理器行为）
        let a_dir = a.is_dir();
        let b_dir = b.is_dir();
        if a_dir != b_dir {
            return if a_dir {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            };
        }

        // 2) 选定键
        let ord = match sort.key {
            SortKey::Name => a.sort_name_key().cmp(&b.sort_name_key()),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Kind => {
                let ka = a.ext();
                let kb = b.ext();
                ka.cmp(&kb).then_with(|| a.sort_name_key().cmp(&b.sort_name_key()))
            }
            SortKey::Modified => a.modified.cmp(&b.modified),
        };

        // 3) 同值时用名称兜底，保证稳定可预期
        let ord = if ord == std::cmp::Ordering::Equal && sort.key != SortKey::Name {
            a.sort_name_key().cmp(&b.sort_name_key())
        } else {
            ord
        };

        if sort.descending {
            ord.reverse()
        } else {
            ord
        }
    });
}

/// 供 UI 线程直接调用的小工具：把 FILETIME 结构转 u64。
pub fn ft64(ft: &FILETIME) -> u64 {
    filetime_from_parts(ft.dwLowDateTime, ft.dwHighDateTime)
}
