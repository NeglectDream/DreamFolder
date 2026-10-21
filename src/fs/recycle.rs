//! 回收站：直接在应用内枚举、显示、删除。
//!
//! 回收站是 Shell 命名空间里的虚拟目录，没有盘符路径可用 `read_dir` 枚举。
//! 但它背后的存储格式是公开且稳定的：每个卷根下的 `$Recycle.Bin\<用户 SID>\`
//! 里成对存放
//!
//!   - `$Ixxxxxx`：元数据（版本、大小、删除时间、原始完整路径）
//!   - `$Rxxxxxx`：被删除的数据本身
//!
//! 两个文件后缀相同，所以拿到 `$I` 就能定位 `$R`。按这个格式解码，
//! 就能把回收站做成普通列表——点进去、排序、删除都和别的目录一样，
//! 不需要为了看一眼回收站去开一个系统窗口。

use std::path::{Path, PathBuf};

use super::entry::{Entry, EntryKind, IconKey};

/// 回收站里的一项。
#[derive(Debug, Clone)]
pub struct Slot {
    /// 数据本体（`$R...`），打开、看属性都用它。
    pub data: PathBuf,
    /// 删除前的完整路径。
    pub orig: PathBuf,
    /// 字节大小（目录为 0，与资源管理器口径一致）。
    pub size: u64,
    /// 删除时间（FILETIME）。
    pub deleted: u64,
}

/// 由 `$R` 推出同目录下同后缀的 `$I`。
pub fn meta_of(data: &Path) -> Option<PathBuf> {
    let name = data.file_name()?.to_string_lossy().into_owned();
    let rest = name.strip_prefix("$R").or_else(|| name.strip_prefix("$r"))?;
    let parent = data.parent()?;
    Some(parent.join(format!("$I{rest}")))
}

/// 解析 `$I` 文件的内容。格式见模块注释；返回（原始路径，大小，删除时间）。
fn parse_meta(bytes: &[u8]) -> Option<(PathBuf, u64, u64)> {
    if bytes.len() < 28 {
        return None;
    }
    let version = u64::from_le_bytes(bytes[0..8].try_into().ok()?);
    let size = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let deleted = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
    let name_len = u32::from_le_bytes(bytes[24..28].try_into().ok()?) as usize;
    // 版本 1 的路径长度字段不可信，固定 520 字节；版本 2 才按实际长度取。
    // v2 的 name_len 是 **UTF-16 字符数**（含结尾 NUL），不是字节数——
    // 当字节数用会把路径截半，还原时就会把文件扔进一个不存在的半截目录。
    let name_bytes = if version == 2 { name_len * 2 } else { 520 };
    let end = (28 + name_bytes).min(bytes.len());
    let raw = &bytes[28..end];
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let text: String = String::from_utf16_lossy(&units);
    let text = text.trim_end_matches('\0');
    if text.is_empty() {
        return None;
    }
    Some((PathBuf::from(text), size, deleted))
}

/// 扫描所有卷的回收站目录。
pub fn slots() -> Vec<Slot> {
    let mut out = Vec::new();
    for drive in super::drives() {
        let bin = drive.join("$Recycle.Bin");
        let Ok(sids) = std::fs::read_dir(&bin) else {
            continue;
        };
        for sid in sids.flatten() {
            let Ok(files) = std::fs::read_dir(sid.path()) else {
                continue;
            };
            for f in files.flatten() {
                let name = f.file_name().to_string_lossy().into_owned();
                let Some(suffix) = name.strip_prefix("$I").or_else(|| name.strip_prefix("$i")) else {
                    continue;
                };
                let meta = f.path();
                let Ok(bytes) = std::fs::read(&meta) else {
                    continue;
                };
                let Some((orig, size, deleted)) = parse_meta(&bytes) else {
                    continue;
                };
                let data = sid.path().join(format!("$R{suffix}"));
                if !data.exists() {
                    // 数据被别的工具清掉了，元数据没跟着删——跳过
                    continue;
                }
                out.push(Slot {
                    data,
                    orig,
                    size,
                    deleted,
                });
            }
        }
    }
    out
}

/// 回收站列表。按删除时间倒序（和资源管理器一致）。
pub fn list() -> Vec<Entry> {
    let mut entries: Vec<Entry> = slots()
        .into_iter()
        .map(|slot| {
            let is_dir = slot.data.is_dir();
            let name = slot
                .orig
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| slot.orig.to_string_lossy().into_owned());
            let ext = if is_dir {
                String::new()
            } else {
                Path::new(&name)
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default()
            };
            let icon_key = if is_dir {
                IconKey::Folder
            } else if ext.is_empty() {
                IconKey::NoExt
            } else {
                IconKey::Ext(ext)
            };
            Entry {
                name,
                full: slot.data,
                kind: if is_dir {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                size: if is_dir { 0 } else { slot.size },
                modified: slot.deleted,
                created: slot.deleted,
                accessed: slot.deleted,
                type_name: String::new(),
                icon_key,
                volume: None,
                origin: Some(slot.orig.clone()),
            }
        })
        .collect();
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));
    entries
}

/// 永久删除回收站里的一项（数据 + 元数据）。
pub fn purge(data: &Path) -> Result<(), String> {
    let meta = meta_of(data);
    let res = if data.is_dir() {
        std::fs::remove_dir_all(data)
    } else {
        std::fs::remove_file(data)
    };
    // 数据删干净了才动元数据：文件被占用（正在播放的视频等）时删除会失败，
    // 若照样删 `$I`，条目从列表消失、`$R` 残留占空间，而且再也无法通过
    // 界面删它——枚举只认 `$I`。留着元数据，用户还能再试一次。
    if res.is_ok() {
        if let Some(meta) = meta {
            let _ = std::fs::remove_file(meta);
        }
    }
    res.map_err(|e| format!("{}: {e}", data.display()))
}

/// 把回收站里的一项还原到它删除前的位置，返回实际落地的路径。
///
/// 目标位置已经有同名文件时不覆盖，改成「名字 (2)」这类后缀——和资源管理器
/// 的「保留两者」一个思路：宁可多出一个文件，也不能悄悄盖掉用户的东西。
/// 成功才清掉 `$I` 元数据；失败留着，还能再试一次。
pub fn restore(data: &Path) -> Result<PathBuf, String> {
    let meta = meta_of(data).ok_or_else(|| "找不到回收站元数据".to_string())?;
    let bytes = std::fs::read(&meta).map_err(|e| format!("读取元数据失败：{e}"))?;
    let (orig, _, _) = parse_meta(&bytes).ok_or_else(|| "回收站元数据已损坏".to_string())?;
    let dir = orig.parent().ok_or_else(|| "原始路径无效".to_string())?;
    if !dir.is_dir() {
        std::fs::create_dir_all(dir).map_err(|e| format!("无法重建原目录：{e}"))?;
    }

    let target = unique_target(&orig);
    std::fs::rename(data, &target).map_err(|e| format!("还原失败：{e}"))?;
    let _ = std::fs::remove_file(&meta);
    Ok(target)
}

/// 目标已存在时找一个不冲突的名字：`报告.docx` → `报告 (2).docx`。
fn unique_target(p: &Path) -> PathBuf {
    if !p.exists() {
        return p.to_path_buf();
    }
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "还原项".to_string());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for n in 2..1000u32 {
        let cand = p.with_file_name(format!("{stem} ({n}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    p.with_file_name(format!("{stem} (还原){ext}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一个 v2 版 `$I` 的字节布局。
    fn v2_meta(path: &str, size: u64, deleted: u64) -> Vec<u8> {
        let units: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut b = Vec::new();
        b.extend_from_slice(&2u64.to_le_bytes()); // 版本
        b.extend_from_slice(&size.to_le_bytes());
        b.extend_from_slice(&deleted.to_le_bytes());
        // namelen 字段：字符数（含 NUL），与系统写盘口径一致
        b.extend_from_slice(&(units.len() as u32).to_le_bytes());
        for u in units {
            b.extend_from_slice(&u.to_le_bytes());
        }
        b
    }

    /// `$I` v2 的 namelen 是 UTF-16 字符数而非字节数。
    ///
    /// 拿真实 `$I` 文件实证过：namelen=47（字符），名字区 94 字节。
    /// 旧代码按字节取，中文路径被截到一半，「原位置」列显示半截，
    /// 还原时 `create_dir_all` 会造出一个乱码目录。
    #[test]
    fn v2_namelen_is_utf16_units_not_bytes() {
        let orig = r"C:\Users\小明\Downloads\报告文档 v3.docx";
        let bytes = v2_meta(orig, 12345, 67890);
        let (path, size, deleted) = parse_meta(&bytes).expect("v2 元数据应能解析");
        assert_eq!(path, PathBuf::from(orig));
        assert_eq!(size, 12345);
        assert_eq!(deleted, 67890);
    }

    /// v1（固定 520 字节名字区）的解析不受影响。
    #[test]
    fn v1_fixed_name_area_still_works() {
        let orig = r"C:\old\a.txt";
        let units: Vec<u16> = orig.encode_utf16().chain(std::iter::once(0)).collect();
        let mut b = Vec::new();
        b.extend_from_slice(&1u64.to_le_bytes());
        b.extend_from_slice(&7u64.to_le_bytes());
        b.extend_from_slice(&9u64.to_le_bytes());
        b.extend_from_slice(&(units.len() as u32).to_le_bytes());
        b.extend_from_slice(&units.iter().flat_map(|u| u.to_le_bytes()).collect::<Vec<u8>>());
        // 补齐到 28+520，与 Win9x-era 布局一致
        while b.len() < 28 + 520 {
            b.push(0);
        }
        let (path, size, _) = parse_meta(&b).expect("v1 元数据应能解析");
        assert_eq!(path, PathBuf::from(orig));
        assert_eq!(size, 7);
    }
}
