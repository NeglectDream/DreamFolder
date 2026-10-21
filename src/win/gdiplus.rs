//! GDI+ 的最小封装：只做三件事——载入位图、缩放绘制、栅格化取像素。
//!
//! 用 GDI+ 而不是 `LoadImage`/`StretchBlt` 的原因：PNG 的 alpha 通道和
//! 高质量双三次缩放这两件事，GDI 都做不对（`StretchBlt` 只能最近邻或
//! 半色调，透明会变成黑底）。而本项目的日志、预览框、应用图标全都依赖
//! 这两点。
//!
//! **初始化时机**：`GdiplusStartup` 走 `OnceLock`，只在第一次真正要用
//! GDI+ 的时候才调（画窗口图标、打开图片预览、生成 .ico 文件），
//! 不进入 `main` 的启动路径。GDI+ 的 startup 会起后台线程并初始化
//! 编解码器，这一步能省掉就是几十毫秒的冷启动优势。

use std::path::Path;
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::Graphics::Gdi::HDC;
use windows_sys::Win32::Graphics::GdiPlus::{
    GdipBitmapLockBits, GdipBitmapUnlockBits, GdipCreateBitmapFromFile, GdipCreateBitmapFromScan0,
    GdipCreateBitmapFromStream, GdipCreateFromHDC, GdipCreateHICONFromBitmap, GdipDeleteGraphics,
    GdipDisposeImage, GdipDrawImageRectI, GdipGetImageGraphicsContext, GdipGetImageHeight,
    GdipGetImageWidth, GdipGraphicsClear, GdipSetInterpolationMode, GdipSetPixelOffsetMode,
    GdiplusStartup, GdiplusStartupInput, GdiplusStartupOutput, BitmapData,
    GpBitmap, GpGraphics, GpImage, ImageLockModeRead, InterpolationModeHighQualityBicubic,
    PixelOffsetModeHighQuality, Rect,
};
use windows_sys::Win32::UI::Shell::SHCreateMemStream;
use windows_sys::Win32::UI::WindowsAndMessaging::HICON;

use super::to_wide;

/// `PixelFormat32bppARGB`。
///
/// windows-sys 0.59 的 `Win32_Graphics_GdiPlus` 只导出了 `PixelFormat*` 的
/// 几个**掩码位**常量（`Alpha`/`GDI`/`PAlpha`/…），没有导出合成的具体格式值，
/// 所以按 `Gdipluspixelformats.h` 的定义在这里补一个：
///
/// ```text
/// PixelFormat32bppARGB = (PixelFormatGDI | PixelFormatAlpha
///                         | PixelFormatCanonical | 32bpp)
///                      = 0x00020000 | 0x00040000 | 0x00200000 | 10
///                      = 0x0026200A
/// ```
const PF_32BPP_ARGB: i32 = 0x0026_200A;

/// GDI+ 的启动令牌。`None` 表示还没启动或启动失败。
static TOKEN: OnceLock<usize> = OnceLock::new();

/// 确保 GDI+ 已启动。返回是否可用。
///
/// 失败时返回 `false` 而不是 panic：图标取不到就退化成系统图标，
/// 预览取不到就退化成图标预览，整个程序不该因为 GDI+ 起不来而崩。
pub fn ensure_init() -> bool {
    if TOKEN.get().is_some() {
        return true;
    }
    let input = GdiplusStartupInput {
        GdiplusVersion: 1,
        DebugEventCallback: 0,
        SuppressBackgroundThread: 0,
        SuppressExternalCodecs: 0,
    };
    let mut token = 0usize;
    let mut output: GdiplusStartupOutput = unsafe { std::mem::zeroed() };
    // 两个 BOOL 都传 FALSE：我们要 GDI+ 的后台线程（否则部分绘制会同步阻塞），
    // 也要它的内置编解码器（否则 PNG/JPEG 都解不了）。
    let status = unsafe { GdiplusStartup(&mut token, &input, &mut output) };
    if status != 0 {
        return false;
    }
    // 竞态下可能别的线程先 set 了，用哪个 token 都能工作，忽略返回值。
    let _ = TOKEN.set(token);
    true
}

/// 一张已解码的位图。`Drop` 时释放 GDI+ 对象。
pub struct Image {
    raw: *mut GpBitmap,
}

impl Image {
    /// 从磁盘文件解码。支持 GDI+ 认识的格式（PNG/JPEG/BMP/GIF/TIFF/ICO）。
    /// 不含 WebP：GDI+ 内置编解码器没有它，传进去必然失败。
    pub fn load(path: &Path) -> Option<Image> {
        if !ensure_init() {
            return None;
        }
        let wide = to_wide(&path.to_string_lossy());
        let mut raw: *mut GpBitmap = std::ptr::null_mut();
        let status = unsafe { GdipCreateBitmapFromFile(wide.as_ptr(), &mut raw) };
        if status != 0 || raw.is_null() {
            None
        } else {
            Some(Image { raw })
        }
    }

    /// 从内存里的图片字节解码（内存流，不落盘、不依赖路径）。
    ///
    /// 用 `SHCreateMemStream` 拿到 `IStream` 再交给 GDI+：这样内嵌的
    /// `include_bytes!` 资源也能直接解码，不用先写临时文件。
    pub fn from_bytes(bytes: &[u8]) -> Option<Image> {
        if !ensure_init() || bytes.is_empty() {
            return None;
        }
        let stream = unsafe { SHCreateMemStream(bytes.as_ptr(), bytes.len() as u32) };
        if stream.is_null() {
            return None;
        }
        let mut raw: *mut GpBitmap = std::ptr::null_mut();
        let status = unsafe { GdipCreateBitmapFromStream(stream, &mut raw) };
        // SHCreateMemStream 的 IStream 由调用方 Release。
        unsafe { release_stream(stream) };
        if status != 0 || raw.is_null() {
            None
        } else {
            Some(Image { raw })
        }
    }

    pub fn width(&self) -> i32 {
        let mut w: u32 = 0;
        unsafe { GdipGetImageWidth(self.raw as *mut GpImage, &mut w) };
        w as i32
    }

    pub fn height(&self) -> i32 {
        let mut h: u32 = 0;
        unsafe { GdipGetImageHeight(self.raw as *mut GpImage, &mut h) };
        h as i32
    }

    /// 生成 `side × side` 的正方形缩放副本（源图等比缩放后居中，四周透明）。
    ///
    /// 返回的 GDI+ 位图**归调用方所有**，用完必须 `GdipDisposeImage`。
    fn scaled_raw(&self, side: i32) -> Option<*mut GpBitmap> {
        if side <= 0 || !ensure_init() {
            return None;
        }
        let side = side.min(1024);
        unsafe {
            let mut bmp: *mut GpBitmap = std::ptr::null_mut();
            if GdipCreateBitmapFromScan0(side, side, 0, PF_32BPP_ARGB, std::ptr::null(), &mut bmp)
                != 0
                || bmp.is_null()
            {
                return None;
            }
            let mut g: *mut GpGraphics = std::ptr::null_mut();
            if GdipGetImageGraphicsContext(bmp as *mut GpImage, &mut g) != 0 || g.is_null() {
                GdipDisposeImage(bmp as *mut GpImage);
                return None;
            }
            GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHighQuality);
            GdipGraphicsClear(g, 0x00FF_FFFF); // 全透明
            let (iw, ih) = (self.width().max(1), self.height().max(1));
            let dw = if iw >= ih {
                side as i64
            } else {
                (side as i64 * iw as i64) / ih as i64
            }
            .max(1) as i32;
            let dh = if ih >= iw {
                side as i64
            } else {
                (side as i64 * ih as i64) / iw as i64
            }
            .max(1) as i32;
            let dx = (side - dw) / 2;
            let dy = (side - dh) / 2;
            GdipDrawImageRectI(g, self.raw as *mut GpImage, dx, dy, dw, dh);
            GdipDeleteGraphics(g);
            Some(bmp)
        }
    }

    /// 栅格化成 `size × size` 的 **BGRA、自上而下** 像素，供 .ico 写入用。
    ///
    /// 透明区域保持 alpha=0；源图小于目标尺寸时用高质量双三次放大
    /// （logo 是 130×127，生成 256 需要放大）。
    pub fn rasterize(&self, size: i32) -> Option<Vec<u8>> {
        let side = size.min(1024);
        let bmp = self.scaled_raw(side)?;
        unsafe {
            let rect = Rect {
                X: 0,
                Y: 0,
                Width: side,
                Height: side,
            };
            let mut data: BitmapData = std::mem::zeroed();
            let st = GdipBitmapLockBits(
                bmp,
                &rect,
                ImageLockModeRead as u32,
                PF_32BPP_ARGB,
                &mut data,
            );
            if st != 0 || data.Scan0.is_null() {
                GdipDisposeImage(bmp as *mut GpImage);
                return None;
            }
            let stride = data.Stride as usize;
            let row_bytes = (side as usize) * 4;
            let mut out = vec![0u8; row_bytes * side as usize];
            for y in 0..side as usize {
                let src = (data.Scan0 as *const u8).add(y * stride);
                std::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(y * row_bytes), row_bytes);
            }
            GdipBitmapUnlockBits(bmp, &mut data);
            GdipDisposeImage(bmp as *mut GpImage);
            Some(out)
        }
    }

    /// 缩放成 `side × side` 的 `HICON`。调用方负责 `DestroyIcon`。
    ///
    /// 与 `rasterize` 走同一条缩放路径，所以图标的像素和 .ico 里的像素一致。
    pub fn to_hicon_at(&self, side: i32) -> HICON {
        let Some(bmp) = self.scaled_raw(side) else {
            return std::ptr::null_mut();
        };
        let mut icon: HICON = std::ptr::null_mut();
        let status = unsafe { GdipCreateHICONFromBitmap(bmp, &mut icon) };
        unsafe { GdipDisposeImage(bmp as *mut GpImage) };
        if status != 0 {
            std::ptr::null_mut()
        } else {
            icon
        }
    }

    /// 把整张图缩放进目标矩形并直接画到 `hdc` 上。
    ///
    /// 详情窗格的图片预览走这里，而不是先 `to_hicon_at` 再 `DrawIconEx`：
    /// 后者要先建图标再销毁，每帧一次分配；而预览是随滚动/选中反复重画的。
    /// 缩放用双三次插值，缩小的照片不会变成锯齿。
    pub fn draw_scaled(&self, hdc: HDC, target: RECT) -> bool {
        if !ensure_init() {
            return false;
        }
        let w = target.right - target.left;
        let h = target.bottom - target.top;
        if w <= 0 || h <= 0 {
            return false;
        }
        let mut g: *mut GpGraphics = std::ptr::null_mut();
        if unsafe { GdipCreateFromHDC(hdc, &mut g) } != 0 || g.is_null() {
            return false;
        }
        unsafe {
            GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHighQuality);
        }
        let status = unsafe {
            GdipDrawImageRectI(g, self.raw as *mut GpImage, target.left, target.top, w, h)
        };
        unsafe { GdipDeleteGraphics(g) };
        status == 0
    }
}

impl Drop for Image {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { GdipDisposeImage(self.raw as *mut GpImage) };
            self.raw = std::ptr::null_mut();
        }
    }
}

/// `IStream::Release`：`SHCreateMemStream` 返回的对象**只有** vtable，
/// windows-sys 没有导出手动声明的 IStream 结构体，所以按 vtable 下标调。
///
/// # Safety
/// `stream` 必须是 `SHCreateMemStream` 返回的有效 IStream。
unsafe fn release_stream(stream: *mut core::ffi::c_void) {
    // IUnknown 的 vtable 布局：QueryInterface(0) / AddRef(1) / Release(2)。
    let vtbl = *(stream as *mut *const usize);
    if vtbl.is_null() {
        return;
    }
    let release: unsafe extern "system" fn(*mut core::ffi::c_void) -> u32 =
        std::mem::transmute(*vtbl.add(2));
    release(stream);
}

/// 把 BGRA 像素写成一个多尺寸 BMP-in-ICO 文件。
///
/// ICO 结构（`RT_ICON` 的两种合法载荷之一）：
/// ```text
/// ICONDIR         6 字节：reserved(0) / type(1) / count
/// ICONDIRENTRY   16 字节 × N
/// 每张图：BITMAPINFOHEADER(40) + BGRA 自底向上 + AND 掩码(1bpp, 全零, 行按 4 字节对齐)
/// ```
/// `biHeight` 要写成 **两倍高度**——ICO 里的 DIB 把 XOR 位图与 AND 掩码
/// 竖着拼在一起，这是 Win32 的历史包袱。
///
/// 返回写出的字节数。
pub fn write_ico(images: &[(i32, Vec<u8>)], path: &Path) -> Option<usize> {
    if images.is_empty() {
        return None;
    }
    let mut payloads: Vec<Vec<u8>> = Vec::with_capacity(images.len());
    for (side, bgra) in images {
        let s = *side;
        if s <= 0 || s > 256 {
            return None;
        }
        let need = (s as usize) * (s as usize) * 4;
        if bgra.len() < need {
            return None;
        }
        let mut bmp = Vec::with_capacity(40 + need + ((s as usize).div_ceil(8).div_ceil(4)) * 4 * s as usize);
        // BITMAPINFOHEADER
        bmp.extend_from_slice(&40u32.to_le_bytes()); // biSize
        bmp.extend_from_slice(&s.to_le_bytes()); // biWidth
        bmp.extend_from_slice(&(s * 2).to_le_bytes()); // biHeight（XOR + AND）
        bmp.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        bmp.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
        bmp.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
        bmp.extend_from_slice(&(need as u32).to_le_bytes()); // biSizeImage
        bmp.extend_from_slice(&0u32.to_le_bytes()); // biXPelsPerMeter
        bmp.extend_from_slice(&0u32.to_le_bytes()); // biYPelsPerMeter
        bmp.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        bmp.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
        // XOR 位图：BGRA 自底向上（源是自上而下，所以要倒着抄）
        let row = (s as usize) * 4;
        for y in (0..s as usize).rev() {
            bmp.extend_from_slice(&bgra[y * row..y * row + row]);
        }
        // AND 掩码：全零（不额外遮罩），行按 4 字节对齐
        let mask_row = ((s as usize).div_ceil(8) + 3) / 4 * 4;
        bmp.extend(std::iter::repeat(0u8).take(mask_row * s as usize));
        payloads.push(bmp);
    }

    let n = payloads.len();
    let mut out: Vec<u8> = Vec::with_capacity(
        6 + 16 * n + payloads.iter().map(|p| p.len()).sum::<usize>(),
    );
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    out.extend_from_slice(&(n as u16).to_le_bytes());
    let mut offset = 6 + 16 * n;
    for (i, payload) in payloads.iter().enumerate() {
        let side = images[i].0;
        // 256 在 ICO 里用 0 表示（字节放不下 256 这个数）
        out.push(if side >= 256 { 0 } else { side as u8 });
        out.push(if side >= 256 { 0 } else { side as u8 });
        out.push(0); // bColorCount：32bpp 不用调色板
        out.push(0); // bReserved
        out.extend_from_slice(&1u16.to_le_bytes()); // wPlanes
        out.extend_from_slice(&32u16.to_le_bytes()); // wBitCount
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += payload.len();
    }
    for payload in &payloads {
        out.extend_from_slice(payload);
    }

    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, &out).ok().map(|_| out.len())
}

/// 退化路径：把原始 PNG 字节直接塞进 ICO（Vista 起 GDI+/Shell 都认）。
///
/// 用于 DIB 编码出问题的情况——比生成不出图标好。
pub fn write_png_ico(png: &[u8], side: i32, path: &Path) -> Option<usize> {
    if png.len() < 8 || &png[1..4] != b"PNG" {
        return None;
    }
    let side = side.clamp(1, 256);
    let mut out: Vec<u8> = Vec::with_capacity(22 + png.len());
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type = icon
    out.extend_from_slice(&1u16.to_le_bytes()); // count = 1
    out.push(if side >= 256 { 0 } else { side as u8 });
    out.push(if side >= 256 { 0 } else { side as u8 });
    out.push(0);
    out.push(0);
    out.extend_from_slice(&1u16.to_le_bytes()); // wPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // wBitCount
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes()); // 偏移：紧跟 ICONDIR + 1 个 entry
    out.extend_from_slice(png);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(path, &out).ok().map(|_| out.len())
}
