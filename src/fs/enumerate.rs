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

use serde::{Deserialize, Serialize};

use super::entry::{filetime_from_parts, Entry, EntryKind, IconKey, Volume};
use crate::win::to_wide;
use std::path::{Path, PathBuf};

// 常量在本 crate 内自备，避免依赖 windows-sys 的导出差异。
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// 排序键。
///
/// 在一个类型上同时派生 serde：它既驱动排序，又是设置文件里的
/// `sort_key` 字段；`cfg::settings` 直接 re-export 它，避免出现两个
/// 同名但互不兼容的 `SortKey`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

    // 构造 \\?\ 前缀避开 MAX_PATH 限制（长路径支持）。
    //
    // 注意：`\\?\` 前缀会**关掉系统对路径的规范化**，里面只能出现反斜杠。
    // 地址栏手输、配置文件、从别处粘来的路径常常带 `/`，不先统一掉的话
    // `\\?\C:/x/y/*` 会直接失败，整个文件夹看起来是空的。
    let (pattern, dir_prefix) = match dir.to_str() {
        Some(s) if s.starts_with(r"\\") => {
            // UNC：\\?\UNC\server\share\*
            let stripped = s.trim_start_matches('\\').replace('/', "\\");
            (
                format!(r"\\?\UNC\{stripped}\*"),
                format!(r"\\{stripped}\"),
            )
        }
        Some(s) => {
            let mut base = s.replace('/', "\\");
            if !base.ends_with('\\') {
                base.push('\\');
            }
            (format!(r"\\?\{base}*"), base)
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
                let created = filetime_from_parts(
                    data.ftCreationTime.dwLowDateTime,
                    data.ftCreationTime.dwHighDateTime,
                );
                let accessed = filetime_from_parts(
                    data.ftLastAccessTime.dwLowDateTime,
                    data.ftLastAccessTime.dwHighDateTime,
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
                    created,
                    accessed,
                    attrs,
                    type_name: String::new(),
                    icon_key: IconKey::Pending,
                    // 容量只对驱动器有意义，普通枚举不查盘
                    volume: None,
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
            full: d.clone(),
            kind: EntryKind::Drive,
            size: 0,
            modified: 0,
            created: 0,
            accessed: 0,
            attrs: FILE_ATTRIBUTE_DIRECTORY,
            type_name: "本地磁盘".to_string(),
            icon_key: IconKey::Drive,
            volume: volume_of(&d),
        });
    }
    // 「此电脑」固定按盘符字母顺序排，不跟随列表当前的排序方式——
    // 资源管理器就是这个行为，盘符顺序也是找盘的第一直觉。
    // 卷标各机器不同，按名字排会让 C/D/E 打乱成看似随机的次序。
    entries.sort_by(|a, b| {
        a.full
            .to_string_lossy()
            .to_ascii_uppercase()
            .cmp(&b.full.to_string_lossy().to_ascii_uppercase())
    });
    let _ = opts;
    Listing { entries, skipped: 0 }
}

/// 查询驱动器容量。
///
/// 失败返回 `None` —— 空的光驱、断开的网络盘、无权限的 BitLocker 卷都会失败，
/// 这是正常状态而不是错误：卡片上少一行容量文字即可，不该影响枚举。
pub fn volume_of(drive: &Path) -> Option<Volume> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let root = to_wide(&drive.to_string_lossy());
    let mut free_to_caller = 0u64;
    let mut total = 0u64;
    let mut total_free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 || total == 0 {
        return None;
    }
    // 用 free_to_caller 而不是 total_free：前者已扣除磁盘配额，
    // 与资源管理器显示的「可用」口径一致。
    Some(Volume {
        free: free_to_caller,
        total,
    })
}

/// 取驱动器卷标（失败返回空串）。
///
/// 导航窗格也要用它拼 `系统 (C:)`，所以公开。调用点在重绘路径上而非每帧，
/// 且 `GetVolumeInformationW` 对已挂载卷有系统级缓存，开销可接受。
pub fn drive_label(drive: &Path) -> String {
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

// ---------------------------------------------------------------- 搜索结果

/// 从绝对路径构造一条目录项。搜索命中走这里，失败返回 `None`。
pub fn entry_from_path(path: &Path) -> Option<Entry> {
    use std::os::windows::fs::MetadataExt;

    let meta = std::fs::metadata(path).ok()?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());

    let is_dir = meta.is_dir();
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

    let written = meta.last_write_time();
    Some(Entry {
        full: path.to_path_buf(),
        name,
        kind,
        size: if is_dir { 0 } else { meta.len() },
        #[allow(clippy::cast_possible_truncation)]
        modified: filetime_from_parts(written as u32, (written >> 32) as u32),
        #[allow(clippy::cast_possible_truncation)]
        created: filetime_from_parts(meta.creation_time() as u32, (meta.creation_time() >> 32) as u32),
        #[allow(clippy::cast_possible_truncation)]
        accessed: filetime_from_parts(meta.last_access_time() as u32, (meta.last_access_time() >> 32) as u32),
        attrs: meta.file_attributes(),
        type_name: String::new(),
        icon_key: IconKey::Pending,
        volume: None,
    })
}

/// 索引不可用时的降级路径：在 `root` 子树内按文件名包含关系查找。
///
/// 语义与「资源管理器关掉索引后的搜索」一致——递归遍历。为避免 UI 卡死，
/// 这里有硬上限：最多返回 `limit` 条、最多运行 `budget_ms` 毫秒，
/// 且不跟随符号链接（防止目录环）。
pub fn scan_subtree(root: &Path, key: &str, limit: usize, budget_ms: u32) -> Vec<Entry> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;

    let needle = key.to_lowercase();
    let start = unsafe { GetTickCount() };
    let mut out: Vec<Entry> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if out.len() >= limit || unsafe { GetTickCount() }.wrapping_sub(start) > budget_ms {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in rd.flatten() {
            if out.len() >= limit || unsafe { GetTickCount() }.wrapping_sub(start) > budget_ms {
                break;
            }
            let Ok(ft) = item.file_type() else { continue };
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();

            if name.to_lowercase().contains(&needle) {
                if let Some(e) = entry_from_path(&path) {
                    out.push(e);
                }
            }
            if ft.is_dir() && !ft.is_symlink() {
                stack.push(path);
            }
        }
    }
    out
}
