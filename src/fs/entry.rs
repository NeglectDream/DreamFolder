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
    /// 回收站等虚拟项
    Virtual,
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
    /// Win32 文件属性位。
    pub attrs: u32,
    /// 类型描述（如"文本文档"），由 icons 模块填充；空表示尚未取到。
    pub type_name: String,
    /// 图标缓存键，由 icons 模块填充。
    pub icon_key: IconKey,
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

    pub fn is_hidden(&self) -> bool {
        const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
        const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
        self.attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
    }

    /// 人类可读的大小。目录与驱动器返回空串（资源管理器同款行为）。
    pub fn size_text(&self) -> String {
        if self.kind != EntryKind::File && self.kind != EntryKind::Shortcut {
            return String::new();
        }
        format_size(self.size)
    }

    pub fn modified_text(&self) -> String {
        if self.modified == 0 {
            return String::new();
        }
        filetime_to_local_text(self.modified)
    }

    /// 排序用的比较键（目录优先由调用方处理）。
    pub fn sort_name_key(&self) -> String {
        self.name.to_lowercase()
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

/// FILETIME → 本地时间文本 `2026-10-20 09:31`。
pub fn filetime_to_local_text(ft: u64) -> String {
    use windows_sys::Win32::Foundation::{
        FileTimeToLocalFileTime, FileTimeToSystemTime, FILETIME, SYSTEMTIME,
    };

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
