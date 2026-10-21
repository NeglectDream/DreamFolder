//! 目录枚举与排序。
//!
//! 性能要点（这是打开大目录时的热点）：
//!   1. `FindExInfoBasic` —— 不请求 8.3 短名，省一次目录查找
//!   2. `FIND_FIRST_EX_LARGE_FETCH` —— 内核一次返回更多条目，减少往返
//!   3. 只做一次 `Vec` 预留，避免反复扩容
//!   4. 排序在枚举线程完成，UI 线程不做 N log N

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
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
            // 共享根（\\server\share）的规范化路径带尾反斜杠，本地盘分支
            // 处理了它，这里不剥就会拼出 share\\*——verbatim 路径不做
            // 规范化，双斜杠直接让 FindFirstFileExW 失败。
            let stripped = s
                .trim_start_matches('\\')
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_string();
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

                // 图标键是纯字符串计算（扩展名小写），枚举时就定死；
                // exe/lnk 这些要按完整路径取图标，所以得先有 full。
                let full = PathBuf::from(format!("{dir_prefix}{name}"));
                let icon_key =
                    super::icons::key_for_entry(&name, &full, kind == EntryKind::Directory, false);
                entries.push(Entry {
                    full,
                    name,
                    kind,
                    size,
                    modified,
                    created,
                    accessed,
                    type_name: String::new(),
                    icon_key,
                    // 容量只对驱动器有意义，普通枚举不查盘
                    volume: None,
                    origin: None,
                });
            }
        }

        let ok = unsafe { FindNextFileW(handle, &mut data) };
        if ok == 0 {
            break;
        }

        // 防御：极端情况下（如设备目录）条数异常时截断
        if entries.len() > 200_000 {
            break;
        }
    }

    unsafe {
        FindClose(handle);
    }

    sort_entries(&mut entries, &opts.sort);

    Ok(Listing { entries })
}

/// 枚举「此电脑」视图：驱动器列表。
///
/// 刻意只列盘符，**不查**卷标和容量：`GetVolumeInformationW` 和
/// `GetDiskFreeSpaceExW` 都是同步系统调用，遇到离线的映射网络盘会等
/// 几十秒 SMB 超时。以前这些都在 UI 线程上做，打开「此电脑」就像死机。
/// 现在由后台补查（`io::Job::Volumes`），查到一个回填一个。
pub fn list_computer(opts: &ListOptions) -> Listing {
    let mut entries = Vec::with_capacity(8);
    for d in super::drives() {
        entries.push(Entry {
            // 先只显示盘符（`C:`），卷标到了再换成「本地磁盘 (C:)」
            name: d.to_string_lossy().trim_end_matches('\\').to_string(),
            full: d,
            kind: EntryKind::Drive,
            size: 0,
            modified: 0,
            created: 0,
            accessed: 0,
            type_name: "本地磁盘".to_string(),
            icon_key: IconKey::Drive,
            volume: None,
            origin: None,
        });
    }
    // 「此电脑」固定按盘符字母顺序排，不跟随列表当前的排序方式——
    // 资源管理器就是这个行为，盘符顺序也是找盘的第一直觉。
    // key 先算好：比较函数里现算会为每次比较分配两个 String。
    entries.sort_by_cached_key(|a| a.full.to_string_lossy().to_ascii_uppercase());
    let _ = opts;
    Listing { entries }
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

/// 排序：完整复刻资源管理器的四列语义（名称 / 大小 / 类型 / 修改日期）。
///
/// 名称列不是字典序，而是 `StrCmpLogicalW` 的自然排序——数字按数值、
/// 中文按拼音、忽略大小写。详见 `fs::sortkey`。
/// 目录恒在文件之前，四列都是（降序也不翻转），见下方比较函数里的说明。
///
/// 比较用的 key 一律**先算一遍再排**：如果放在比较函数里现算，
/// UTF-16 转换 / `ext()` / 类型名查询会被调用几十万次（1 万条目 ≈ 13 万次
/// 比较，每次 2 次分配），打开大目录时这部分比枚举本身还贵。
pub fn sort_entries(entries: &mut [Entry], sort: &Sort) {
    use crate::fs::sortkey::{natural_cmp, natural_key};
    use std::cmp::Ordering;

    let n = entries.len();
    if n < 2 {
        return;
    }
    let names: Vec<Vec<u16>> = entries.iter().map(|e| e.sort_name_key()).collect();
    // 「类型」排序必须按用户看到的那串文字比（「文本文档」「位图图像」），
    // 而不是按扩展名字母序：扩展名序会把「png」和「txt」的先后强加给两个
    // 中文类型名，屏幕上就成了看不懂的顺序。同名共享一份全局缓存。
    let kinds: Vec<Vec<u16>> = match sort.key {
        SortKey::Kind => entries
            .iter()
            .map(|e| {
                if e.is_dir() {
                    natural_key(crate::fs::typename::folder())
                } else {
                    natural_key(&crate::fs::typename::for_ext(&e.ext()))
                }
            })
            .collect(),
        _ => Vec::new(),
    };

    let src: &[Entry] = entries;

    // 先排下标，最后按置换把条目就位：比较时不碰条目本身，借用冲突最小。
    //
    // 目录恒在文件之前：Windows 资源管理器的详情视图里「类型」列排在最前的
    // 永远是文件夹，列表也把目录归在最上——这是用户读目录的第一直觉。
    // 降序时目录同样留在最上面（只把文件倒过来），否则点一下「降序」
    // 整屏目录瞬间沉底，反而更难找。
    let mut idx: Vec<u32> = (0..n as u32).collect();
    idx.sort_by(|&a, &b| {
        let (ia, ib) = (a as usize, b as usize);
        let dir = match (src[ia].is_dir(), src[ib].is_dir()) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => Ordering::Equal,
        };
        if dir != Ordering::Equal {
            return dir;
        }
        let ord = match sort.key {
            SortKey::Name => natural_cmp(&names[ia], &names[ib]),
            SortKey::Size => src[ia].size.cmp(&src[ib].size),
            SortKey::Kind => {
                natural_cmp(&kinds[ia], &kinds[ib]).then_with(|| natural_cmp(&names[ia], &names[ib]))
            }
            SortKey::Modified => src[ia].modified.cmp(&src[ib].modified),
        };
        // 同值时用名称兜底，保证稳定可预期
        let ord = if ord == Ordering::Equal && sort.key != SortKey::Name {
            natural_cmp(&names[ia], &names[ib])
        } else {
            ord
        };
        if sort.descending {
            ord.reverse()
        } else {
            ord
        }
    });

    // 把排序结果搬回原切片。
    //
    // `idx[k]` 是「排序后第 k 位应当装入的原始下标」。直接在切片上做
    // `swap(i, idx[i])` 是错的：一次交换之后 `idx` 的语义就变了，
    // 只有自逆置换（全是 2-循环）碰巧正确，3 个及以上元素的循环会被打乱——
    // 例如 `[b, c, a]` 这种最常见的轮换，会排成 `[c, a, b]`。
    // 正确做法是先转成逆置换 `pos[orig] = 目标位置`，再逐位把元素换过去。
    let mut pos = vec![0u32; n];
    for (k, &orig) in idx.iter().enumerate() {
        pos[orig as usize] = k as u32;
    }
    for i in 0..n {
        while pos[i] as usize != i {
            let j = pos[i] as usize;
            entries.swap(i, j);
            pos.swap(i, j);
        }
    }
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
    let icon_key = super::icons::key_for_entry(&name, path, is_dir, false);
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
        type_name: String::new(),
        icon_key,
        volume: None,
        origin: None,
    })
}

/// 索引不可用时的降级路径：在 `root` 子树内按文件名包含关系查找。
///
/// 语义与「资源管理器关掉索引后的搜索」一致——递归遍历。为避免长时间占用
/// 后台线程，这里有硬上限：最多返回 `limit` 条、最多运行 `budget_ms` 毫秒、
/// 且不跟随符号链接（防止目录环）。`cancelled` 每次循环都会问一遍，
/// 用户改了搜索词或换了目录就能立刻收手。
pub fn scan_subtree(
    root: &Path,
    key: &str,
    limit: usize,
    budget_ms: u32,
    cancelled: &dyn Fn() -> bool,
) -> Vec<Entry> {
    use windows_sys::Win32::System::SystemInformation::GetTickCount;

    let needle = key.to_lowercase();
    let start = unsafe { GetTickCount() };
    let mut out: Vec<Entry> = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let stop = |out_len: usize| -> bool {
        out_len >= limit || unsafe { GetTickCount() }.wrapping_sub(start) > budget_ms
    };

    while let Some(dir) = stack.pop() {
        if stop(out.len()) || cancelled() {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in rd.flatten() {
            if stop(out.len()) || cancelled() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::entry::{Entry, EntryKind, IconKey};

    fn e(name: &str, kind: EntryKind, size: u64, modified: u64) -> Entry {
        Entry {
            name: name.to_string(),
            full: PathBuf::from(format!("C:/dir/{name}")),
            kind,
            size,
            modified,
            created: 0,
            accessed: 0,
            type_name: String::new(),
            icon_key: IconKey::Pending,
            volume: None,
            origin: None,
        }
    }

    fn names(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// 名称排序复刻资源管理器：忽略大小写，目录恒在文件之前。
    #[test]
    fn sort_name_keeps_dirs_first() {
        let mut items = vec![
            e("beta.txt", EntryKind::File, 0, 0),
            e("Alpha", EntryKind::Directory, 0, 0),
            e("gamma.txt", EntryKind::File, 0, 0),
        ];
        sort_entries(
            &mut items,
            &Sort { key: SortKey::Name, descending: false },
        );
        assert_eq!(names(&items), ["Alpha", "beta.txt", "gamma.txt"]);

        // 降序：文件倒过来，目录仍留在最前
        sort_entries(
            &mut items,
            &Sort { key: SortKey::Name, descending: true },
        );
        assert_eq!(names(&items), ["Alpha", "gamma.txt", "beta.txt"]);
    }

    /// 目录置顶对四列都成立，包括「类型」列（文件夹永远是第一个类型）。
    #[test]
    fn dirs_first_for_every_sort_key() {
        for key in [SortKey::Name, SortKey::Size, SortKey::Kind, SortKey::Modified] {
            for descending in [false, true] {
                let mut items = vec![
                    e("a_file.txt", EntryKind::File, 999, 99),
                    e("z_dir", EntryKind::Directory, 0, 0),
                ];
                sort_entries(&mut items, &Sort { key, descending });
                assert_eq!(names(&items), ["z_dir", "a_file.txt"], "{key:?} desc={descending}");
            }
        }
    }

    /// 名字里的数字按数值排：`file2` 在 `file10` 之前（字典序恰好相反）。
    #[test]
    fn sort_name_orders_digits_numerically() {
        let mut items = vec![
            e("file10.txt", EntryKind::File, 0, 0),
            e("file2.txt", EntryKind::File, 0, 0),
            e("file1.txt", EntryKind::File, 0, 0),
            e("File1.txt", EntryKind::File, 0, 0),
        ];
        sort_entries(
            &mut items,
            &Sort { key: SortKey::Name, descending: false },
        );
        let got = names(&items);
        // 大小写等价：`File1.txt` / `file1.txt` 同档，一起排在 file2 之前
        let head: Vec<&str> = got[..2].to_vec();
        assert!(head.contains(&"file1.txt"), "{got:?}");
        assert!(head.contains(&"File1.txt"), "{got:?}");
        assert_eq!(got[2], "file2.txt");
        assert_eq!(got[3], "file10.txt");
    }

    /// 「类型」排序把同类型的文件聚在一起，同类型内再按名称。
    #[test]
    fn sort_kind_groups_same_type_together() {
        let mut items = vec![
            e("a.txt", EntryKind::File, 0, 0),
            e("b.png", EntryKind::File, 0, 0),
            e("c.txt", EntryKind::File, 0, 0),
            e("d.png", EntryKind::File, 0, 0),
        ];
        sort_entries(
            &mut items,
            &Sort { key: SortKey::Kind, descending: false },
        );
        let got = names(&items);
        let pos = |n: &str| got.iter().position(|x| *x == n).unwrap() as i64;
        // 同类相邻：两个 txt 挨着、两个 png 挨着（输入是交错的）
        assert_eq!((pos("a.txt") - pos("c.txt")).abs(), 1, "{got:?}");
        assert_eq!((pos("b.png") - pos("d.png")).abs(), 1, "{got:?}");
        // 同一类型内仍是名称序
        assert!(pos("a.txt") < pos("c.txt"), "{got:?}");
        assert!(pos("b.png") < pos("d.png"), "{got:?}");
    }

    /// 按大小排序时同值用名称兜底，结果可预期（不依赖原始顺序）。
    #[test]
    fn sort_size_falls_back_to_name() {
        let mut items = vec![
            e("b.dat", EntryKind::File, 100, 0),
            e("a.dat", EntryKind::File, 100, 0),
            e("c.dat", EntryKind::File, 300, 0),
        ];
        sort_entries(
            &mut items,
            &Sort { key: SortKey::Size, descending: false },
        );
        assert_eq!(names(&items), ["a.dat", "b.dat", "c.dat"]);
    }

    /// 按修改时间排序（值越大越新）。
    #[test]
    fn sort_modified() {
        let mut items = vec![
            e("old.txt", EntryKind::File, 0, 1),
            e("new.txt", EntryKind::File, 0, 9),
        ];
        sort_entries(
            &mut items,
            &Sort { key: SortKey::Modified, descending: false },
        );
        assert_eq!(names(&items), ["old.txt", "new.txt"]);
    }

    /// 单元素与空表直接返回，不 panic。
    #[test]
    fn sort_handles_tiny_input() {
        let mut one = vec![e("only.txt", EntryKind::File, 0, 0)];
        sort_entries(&mut one, &Sort::default());
        assert_eq!(names(&one), ["only.txt"]);

        let mut none: Vec<Entry> = Vec::new();
        sort_entries(&mut none, &Sort::default());
        assert!(none.is_empty());
    }

}
