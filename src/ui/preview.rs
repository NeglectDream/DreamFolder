//! 详情窗格顶部的预览：图片直接画出来，文本摊成前若干行。
//!
//! 这里只负责「把文件变成可画的东西」，不碰几何——预览框多大、能放几行，
//! 由绘制方按当前高度算（见 `ui::draw::paint_detail_pane`）。
//!
//! **分工**：读文件、判断尺寸、解文本都在后台线程做（`load_data`，
//! 见 [`crate::io::Job::Preview`]）；GDI+ 解码必须留在 UI 线程
//! （`Preview::from_data`），因为 GDI+ 对象不跨线程。
//!
//! **缓存**：结果挂在 `Application::preview` 上，只有选中项变了才重新生成。

use std::path::{Path, PathBuf};

use crate::win::gdiplus::Image;

/// 能预览的图片后缀（GDI+ 认识的那几种）。
const IMAGE_EXT: [&str; 9] = [
    "png", "jpg", "jpeg", "jpe", "bmp", "gif", "tif", "tiff", "webp",
];

/// 当作文本预览的后缀。
const TEXT_EXT: [&str; 24] = [
    "txt", "md", "markdown", "log", "json", "toml", "yaml", "yml", "ini", "cfg", "conf", "csv",
    "rs", "c", "h", "cpp", "hpp", "cs", "py", "js", "ts", "html", "css", "xml",
];

/// 文本预览的读取上限与行数上限。
///
/// 256 KiB 足够铺满一个预览框还绰绰有余（框里顶多十几行），
/// 而大日志动辄几百兆，全读进来会卡住界面。
const TEXT_MAX_BYTES: u64 = 256 * 1024;
const TEXT_MAX_LINES: usize = 400;

/// 图片预览的文件大小上限与像素上限。
///
/// 以前是「有多大就解码多大」：一张 2 万像素见方的 PNG 只有几兆，
/// 解码出来却要 1.6 GB，点一下就直接把进程顶死。现在先读文件头拿尺寸，
/// 超限就明确告诉用户「图片过大」，不碰解码器。
const IMAGE_MAX_BYTES: u64 = 24 * 1024 * 1024;
const IMAGE_MAX_PIXELS: u64 = 16_000_000;
/// 判断图片尺寸只需要文件头，读这么多足够（PNG/GIF/BMP 在头 32 字节内，
/// JPEG 的 SOF 段通常也在前几十 KB）。
const HEAD_BYTES: u64 = 64 * 1024;

/// 一次预览的结果。
pub enum PreviewKind {
    /// 预览不了（目录、未知类型、读失败……）。
    None,
    /// 已解码的位图，按预览框等比缩放绘制。
    Image(Image),
    /// 已解码的文本行（最多 [`TEXT_MAX_LINES`] 行）。
    Text(Vec<String>),
    /// 读失败的原因，直接显示给用户看。
    Failed(String),
}

/// 挂在 `Application` 上的预览缓存。
pub struct Preview {
    /// 这份预览对应的文件；选中项一变就重新生成。
    pub path: PathBuf,
    pub kind: PreviewKind,
}

/// 后台线程产出的「预览原料」。
///
/// 只有纯数据，可以跨线程搬运：文本已经解好码，图片只带一个结论
/// （能不能预览），真正的 GDI+ 解码留在 UI 线程——GDI+ 对象不跨线程。
pub enum PreviewData {
    None,
    Text(Vec<String>),
    /// 尺寸/大小都在限额内，可以交给 GDI+ 解码。
    Image,
    Failed(String),
}

pub fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXT.contains(&ext)
}

pub fn is_text_ext(ext: &str) -> bool {
    TEXT_EXT.contains(&ext)
}

/// 后台侧：把文件读成「预览原料」。会阻塞，只能在 IO 线程调用。
pub fn load_data(path: &Path, ext: &str, size: u64) -> PreviewData {
    if is_image_ext(ext) {
        if size > IMAGE_MAX_BYTES {
            return PreviewData::Failed("图片过大，已跳过预览".to_string());
        }
        // 尺寸未知（不认识的容器）时放行：宁可让 GDI+ 试一次，
        // 也不要因为解析不了文件头就什么都不显示。
        if let Some((w, h)) = read_head(path, HEAD_BYTES)
            .ok()
            .and_then(|head| image_dimensions(&head))
        {
            if u64::from(w) * u64::from(h) > IMAGE_MAX_PIXELS {
                return PreviewData::Failed(format!(
                    "图片过大（{}×{}），已跳过预览",
                    w, h
                ));
            }
        }
        return PreviewData::Image;
    }
    if is_text_ext(ext) {
        let bytes = if size > TEXT_MAX_BYTES {
            // 只截前面一段就够看，不因为文件大就什么都不显示
            read_head(path, TEXT_MAX_BYTES)
        } else {
            std::fs::read(path)
        };
        return match bytes {
            Ok(b) => PreviewData::Text(split_lines(&decode(&b))),
            Err(e) => PreviewData::Failed(format!("无法读取：{e}")),
        };
    }
    PreviewData::None
}

impl Preview {
    /// UI 侧：把原料变成可绘制的东西。图片在这里才真正解码。
    pub fn from_data(path: PathBuf, data: PreviewData) -> Preview {
        let kind = match data {
            PreviewData::None => PreviewKind::None,
            PreviewData::Text(lines) => PreviewKind::Text(lines),
            PreviewData::Image => match Image::load(&path) {
                Some(img) => PreviewKind::Image(img),
                // GDI+ 打不开（损坏、或该格式没装编解码器）：说清楚，别默默留白
                None => PreviewKind::Failed("无法解码这张图片".to_string()),
            },
            PreviewData::Failed(msg) => PreviewKind::Failed(msg),
        };
        Preview { path, kind }
    }
}

/// 从文件头里读出图片尺寸。认不出就返回 `None`（交给字节上限兜底）。
///
/// 只需要几种最常见容器的定长头：PNG / GIF / BMP 是固定偏移，
/// JPEG 要走一遍段表找 SOF0..SOF3。
fn image_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let be32 = |i: usize| -> Option<u32> {
        Some(u32::from_be_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
    };
    let le16 = |i: usize| -> Option<u32> {
        Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
    };
    let le32 = |i: usize| -> Option<u32> {
        Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
    };
    let be16 = |i: usize| -> Option<u32> {
        Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as u32)
    };

    // PNG：8 字节签名 + IHDR
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some((be32(16)?, be32(20)?));
    }
    // GIF87a / GIF89a
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    // BMP：DIB 头里是 i32
    if b.starts_with(b"BM") {
        let w = le32(18)? as i32;
        let h = le32(22)? as i32;
        return Some((w.unsigned_abs(), h.unsigned_abs()));
    }
    // JPEG：走段表，找到 SOF0..SOF3
    if b.starts_with(&[0xFF, 0xD8]) {
        let mut i = 2usize;
        while i + 9 < b.len() {
            if b[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            // 段长度（大端），0x01/0xD0..0xD8 之类没有长度字段
            if !(0xC0..=0xFE).contains(&marker) || matches!(marker, 0xD0..=0xD9) {
                i += 2;
                continue;
            }
            let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
            if (0xC0..=0xC3).contains(&marker) {
                let h = be16(i + 5)?;
                let w = be16(i + 7)?;
                return Some((w, h));
            }
            i += 2 + len;
        }
    }
    None
}

fn read_head(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    f.by_ref().take(limit).read_to_end(&mut buf)?;
    Ok(buf)
}

fn split_lines(text: &str) -> Vec<String> {
    text.lines()
        .take(TEXT_MAX_LINES)
        // 制表符在自绘文本里没有「制表位」概念，换成 4 个空格才不会挤成一团
        .map(|l| l.replace('\t', "    "))
        .collect()
}

/// 把文件字节解成字符串。
///
/// 顺序很重要：先看 BOM（UTF-16 / UTF-8），再试严格 UTF-8，最后回落到
/// 系统 ANSI 代码页。中文 Windows 上的记事本文件多半是 GBK，
/// 直接 `from_utf8_lossy` 会把整段汉字变成问号堆。
fn decode(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let body = &bytes[2..];
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let body = &bytes[2..];
        let units: Vec<u16> = body
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let body = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        &bytes[3..]
    } else {
        bytes
    };
    if let Ok(s) = std::str::from_utf8(body) {
        return s.to_string();
    }
    ansi_to_string(body)
}

/// 按系统 ANSI 代码页解码（中文系统即 GBK）。
fn ansi_to_string(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_ACP};
    if bytes.is_empty() {
        return String::new();
    }
    let len = bytes.len().min(i32::MAX as usize) as i32;
    let need = unsafe {
        MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0)
    };
    if need <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut buf = vec![0u16; need as usize];
    let got = unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, buf.as_mut_ptr(), need) };
    if got <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    buf.truncate(got as usize);
    String::from_utf16_lossy(&buf)
}
