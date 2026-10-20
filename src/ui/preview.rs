//! 详情窗格顶部的预览：图片直接画出来，文本摊成前若干行。
//!
//! 这里只负责「把文件变成可画的东西」，不碰几何——预览框多大、能放几行，
//! 由绘制方按当前高度算（见 `ui::draw::paint_detail_pane`）。
//!
//! **缓存**：解码一张图或读一个文件都不便宜，而预览框每次重绘都会问一遍。
//! 所以结果挂在 `Application::preview` 上，只有选中项变了才重新生成。

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

pub fn is_image_ext(ext: &str) -> bool {
    IMAGE_EXT.contains(&ext)
}

pub fn is_text_ext(ext: &str) -> bool {
    TEXT_EXT.contains(&ext)
}

/// 为 `path` 生成一份预览。`ext` 是已小写化的后缀，`size` 是文件大小。
pub fn build(path: &Path, ext: &str, size: u64) -> Preview {
    let kind = if is_image_ext(ext) {
        match Image::load(path) {
            Some(img) => PreviewKind::Image(img),
            // GDI+ 打不开（损坏、或该格式没装编解码器）：说清楚，别默默留白
            None => PreviewKind::Failed("无法解码这张图片".to_string()),
        }
    } else if is_text_ext(ext) {
        if size > TEXT_MAX_BYTES {
            // 只截前面一段就够看，不因为文件大就什么都不显示
            match read_head(path, TEXT_MAX_BYTES) {
                Ok(bytes) => PreviewKind::Text(split_lines(&decode(&bytes))),
                Err(e) => PreviewKind::Failed(format!("无法读取：{e}")),
            }
        } else {
            match std::fs::read(path) {
                Ok(bytes) => PreviewKind::Text(split_lines(&decode(&bytes))),
                Err(e) => PreviewKind::Failed(format!("无法读取：{e}")),
            }
        }
    } else {
        PreviewKind::None
    };
    Preview {
        path: path.to_path_buf(),
        kind,
    }
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
