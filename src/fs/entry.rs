//! 目录项数据模型。
//!
//! 设计原则：**枚举时一次拿全**，避免 UI 线程为每一项再去问系统。
//! 图标与类型描述是例外（代价高），由 icons 模块按需异步补。

use std::path::PathBuf;

/// 文件大类，决定图标与默认动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    /// 驱动器根（"此电脑"里的 C:\、D:\）
    Drive,
    /// 快捷方式（.lnk）
    Shortcut,
}

/// 驱动器容量。只有 `EntryKind::Drive` 会填；查询失败时留 `None`，
/// 调用方按「未知容量」处理即可（不阻塞枚举，也不算错误）。
#[derive(Debug, Clone, Copy)]
pub struct Volume {
    /// 可用字节数。
    pub free: u64,
    /// 总字节数。
    pub total: u64,
}

impl Volume {
    /// 已用比例，0.0..=1.0。总容量为 0 时返回 0。
    pub fn used_ratio(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        let used = self.total.saturating_sub(self.free) as f64;
        (used / self.total as f64) as f32
    }

    /// 资源管理器同款文案：`21.2 GB 可用，共 200 GB`。
    pub fn text(&self) -> String {
        format!(
            "{} 可用，共 {}",
            format_capacity(self.free),
            format_capacity(self.total)
        )
    }

    /// 极简文案：`53.6G/200G`。
    ///
    /// 导航窗格那一行窄得可怜，`53.6 GB / 200 GB` 一旦放不下就会被省略成
    /// `194.4 GB / 7…`——数字被吃掉比名字被裁掉更难接受。这里把单位和
    /// 分隔符前后的空格全去掉，同样的信息只占一半宽度。
    pub fn compact_text(&self) -> String {
        format!(
            "{}/{}",
            format_capacity_compact(self.free),
            format_capacity_compact(self.total)
        )
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// 文件系统名（含扩展名）。
    pub name: String,
    /// 完整路径。
    pub full: PathBuf,
    pub kind: EntryKind,
    pub size: u64,
    /// 修改时间，FILETIME（100ns 自 1601）。0 表示未知。
    pub modified: u64,
    /// 创建时间。语义同 `modified`。
    pub created: u64,
    /// 最后访问时间。语义同 `modified`。
    pub accessed: u64,
    /// 类型描述（如"文本文档"），由 icons 模块填充；空表示尚未取到。
    pub type_name: String,
    /// 图标缓存键，由 icons 模块填充。
    pub icon_key: IconKey,
    /// 驱动器容量。非驱动器恒为 `None`。
    pub volume: Option<Volume>,
    /// 删除前的原路径。只有回收站里的条目有值——资源管理器的回收站也
    /// 专门有一列「原位置」，否则一堆同名文件根本分不清谁是谁。
    pub origin: Option<PathBuf>,
}

/// 图标查找键：同一扩展名共用一张图标。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IconKey {
    /// 尚未解析。
    Pending,
    /// 目录。
    Folder,
    /// 驱动器。
    Drive,
    /// 用真实路径取图标（导航窗格的已知文件夹、具体驱动器）。
    Path(String),
    /// Shell 特殊项：0 = 此电脑，1 = 回收站，2 = 网络。
    Special(u8),
    /// 按扩展名。
    Ext(String),
    /// 无扩展名文件。
    NoExt,
}

impl Entry {
    /// 显示名：按设置决定是否隐藏已知扩展名。
    pub fn display_name(&self, show_extension: bool) -> &str {
        if show_extension || self.kind == EntryKind::Directory {
            return &self.name;
        }
        match self.name.rfind('.') {
            Some(pos) if pos > 0 => &self.name[..pos],
            _ => &self.name,
        }
    }

    /// 扩展名（小写，不含点）。目录返回空串。
    pub fn ext(&self) -> String {
        if self.kind == EntryKind::Directory {
            return String::new();
        }
        match self.name.rfind('.') {
            Some(pos) if pos + 1 < self.name.len() => {
                self.name[pos + 1..].to_ascii_lowercase()
            }
            _ => String::new(),
        }
    }

    pub fn is_dir(&self) -> bool {
        matches!(self.kind, EntryKind::Directory | EntryKind::Drive)
    }

    /// 是否是可解码成缩略图的图片文件。
    ///
    /// 按 GDI+ 内置编解码器的格式圈白名单；SVG 这类矢量格式不在其中，
    /// 交给扩展名图标，别拿解码失败去浪费一次后台请求。
    pub fn is_image(&self) -> bool {
        matches!(
            self.ext().as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "bmp" | "tif" | "tiff" | "webp" | "ico"
        )
    }

    /// 人类可读的大小。目录与驱动器返回空串（资源管理器同款行为）。
    pub fn size_text(&self) -> String {
        if self.kind != EntryKind::File && self.kind != EntryKind::Shortcut {
            return String::new();
        }
        format_size(self.size)
    }

    /// 驱动器容量文案（`21.2 GB 可用，共 200 GB`）。
    /// 非驱动器、或容量查询失败时返回空串，调用方直接不画那一行。
    pub fn volume_text(&self) -> String {
        self.volume.map(|v| v.text()).unwrap_or_default()
    }

    pub fn modified_text(&self) -> String {
        if self.modified == 0 {
            return String::new();
        }
        filetime_to_local_text(self.modified)
    }

    /// 创建时间文案。取不到时返回空串。
    pub fn created_text(&self) -> String {
        if self.created == 0 {
            return String::new();
        }
        filetime_to_local_text(self.created)
    }

    /// 最后访问时间文案。
    pub fn accessed_text(&self) -> String {
        if self.accessed == 0 {
            return String::new();
        }
        filetime_to_local_text(self.accessed)
    }

    /// 排序用的名称比较键：资源管理器的自然排序键（见 `fs::sortkey`）。
    ///
    /// 这里只负责名称本身；「目录置顶」由 `fs::enumerate::sort_entries`
    /// 在外层统一处理（四列一致），所以键里不再混入目录标志。
    pub fn sort_name_key(&self) -> Vec<u16> {
        crate::fs::sortkey::natural_key(&self.name)
    }
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} 字节")
    } else if b < MB {
        format!("{:.1} KB", b / KB)
    } else if b < GB {
        format!("{:.1} MB", b / MB)
    } else if b < TB {
        format!("{:.2} GB", b / GB)
    } else {
        format!("{:.2} TB", b / TB)
    }
}

/// 容量文案用的短格式。
///
/// 与 `format_size` 的区别：整数值不带小数（`200 GB` 而不是 `200.00 GB`），
/// 非整数保留一位（`21.2 GB`）。这是资源管理器在驱动器卡片上的写法。
fn format_capacity(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let b = bytes as f64;
    let (v, unit) = if b < KB {
        return format!("{bytes} 字节");
    } else if b < MB {
        (b / KB, "KB")
    } else if b < GB {
        (b / MB, "MB")
    } else if b < TB {
        (b / GB, "GB")
    } else {
        (b / TB, "TB")
    };
    // 近似整数就按整数写：200 GB 比 200.01 GB 更像系统自己的说法
    if (v - v.round()).abs() < 0.05 {
        format!("{:.0} {unit}", v)
    } else {
        format!("{v:.1} {unit}")
    }
}

/// 单字母单位、无空格的容量文本：`53.6G`、`200G`、`1.2T`。
///
/// 只给导航窗格这类「一行里塞名字 + 容量」的地方用，别处仍用带空格的可读写法。
fn format_capacity_compact(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let b = bytes as f64;
    let (v, unit) = if b < KB {
        return format!("{bytes}B");
    } else if b < MB {
        (b / KB, "K")
    } else if b < GB {
        (b / MB, "M")
    } else if b < TB {
        (b / GB, "G")
    } else {
        (b / TB, "T")
    };
    if (v - v.round()).abs() < 0.05 {
        format!("{:.0}{unit}", v)
    } else {
        format!("{v:.1}{unit}")
    }
}

/// FILETIME → 本地时间文本 `2026-10-20 09:31`。
pub fn filetime_to_local_text(ft: u64) -> String {
    // 注意：FILETIME/SYSTEMTIME 在 Foundation，转换函数在 System::Time
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::Storage::FileSystem::FileTimeToLocalFileTime;
    use windows_sys::Win32::System::Time::FileTimeToSystemTime;

    let file_time = FILETIME {
        dwLowDateTime: (ft & 0xFFFF_FFFF) as u32,
        dwHighDateTime: (ft >> 32) as u32,
    };
    let mut local: FILETIME = unsafe { std::mem::zeroed() };
    let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };

    unsafe {
        if FileTimeToLocalFileTime(&file_time, &mut local) == 0 {
            return String::new();
        }
        if FileTimeToSystemTime(&local, &mut st) == 0 {
            return String::new();
        }
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute
    )
}

/// FILETIME 中的 64 位值拆解。
pub fn filetime_from_parts(low: u32, high: u32) -> u64 {
    ((high as u64) << 32) | (low as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> Entry {
        Entry {
            name: name.to_string(),
            full: PathBuf::from(format!("C:/dir/{name}")),
            kind: EntryKind::File,
            size: 0,
            modified: 0,
            created: 0,
            accessed: 0,
            type_name: String::new(),
            icon_key: IconKey::Pending,
            volume: None,
            origin: None,
        }
    }

    fn dir(name: &str) -> Entry {
        Entry { kind: EntryKind::Directory, ..file(name) }
    }

    /// 大小文案按 1024 进制换算，各档都带单位。
    #[test]
    fn format_size_units() {
        assert_eq!(format_size(0), "0 字节");
        assert_eq!(format_size(999), "999 字节");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.00 GB");
        assert_eq!(format_size(2 * 1024u64.pow(4)), "2.00 TB");
    }

    /// 目录与驱动器的「大小」是空串（资源管理器同款）。
    #[test]
    fn directories_have_no_size_text() {
        assert_eq!(dir("Documents").size_text(), "");
        let drive = Entry { kind: EntryKind::Drive, ..file("C:\\") };
        assert_eq!(drive.size_text(), "");
        assert_eq!(file("a.txt").size_text(), "0 字节");
    }

    /// 扩展名取最后一段且小写；目录恒为空。
    #[test]
    fn extension_is_lowercase_and_last_segment() {
        assert_eq!(file("A.TXT").ext(), "txt");
        assert_eq!(file("archive.tar.gz").ext(), "gz");
        assert_eq!(file("README").ext(), "");
        assert_eq!(dir("folder.txt").ext(), "");
    }

    /// 显示名按设置隐藏已知扩展名，但目录名从不裁剪。
    #[test]
    fn display_name_respects_setting() {
        assert_eq!(file("a.txt").display_name(false), "a");
        assert_eq!(file("a.txt").display_name(true), "a.txt");
        assert_eq!(file("noext").display_name(false), "noext");
        // 点开头的文件：点在最前面，不当扩展名分隔符
        assert_eq!(file(".gitignore").display_name(false), ".gitignore");
        assert_eq!(dir("a.txt").display_name(false), "a.txt");
    }

    /// FILETIME 拆解与合成互为逆运算。
    #[test]
    fn filetime_parts_roundtrip() {
        let v = 0x0123_4567_89AB_CDEFu64;
        assert_eq!(filetime_from_parts(v as u32, (v >> 32) as u32), v);
    }

    /// 时间为 0 表示「未知」，文案是空串而不是 1601 年。
    #[test]
    fn zero_time_is_empty() {
        assert_eq!(file("a.txt").modified_text(), "");
        assert_eq!(file("a.txt").created_text(), "");
        assert_eq!(file("a.txt").accessed_text(), "");
    }
}
