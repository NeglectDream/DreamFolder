//! 双缓冲绘制。
//!
//! 自绘 UI 最大的性能陷阱是闪烁与重复分配。这里缓存离屏 DC 与位图，
//! 仅在窗口尺寸变化时重建；每帧只做一次 BitBlt。
//!
//! 冷启动相关：`BackBuffer` 是**懒创建**的——首次 WM_PAINT 才分配 GDI 资源，
//! 首帧之前不消耗任何句柄。

use windows_sys::Win32::Foundation::{COLORREF, HWND, POINT, RECT};
// Gdi 通配同时提供 BeginPaint / EndPaint / PAINTSTRUCT / DrawTextW / CreateFontW 等
use windows_sys::Win32::Graphics::Gdi::*;

use super::to_wide_raw;

/// 界面配色。与 360FileBrowser_Theme.ui 中的十六进制色值一一对应。
#[derive(Clone, Copy)]
pub struct Palette {
    pub window_bg: u32,
    pub titlebar_bg: u32,
    pub tabbar_bg: u32,
    pub tab_active: u32,
    pub tab_hover: u32,
    pub tab_inactive: u32,
    pub text: u32,
    pub text_dim: u32,
    pub text_on_accent: u32,
    pub accent: u32,
    pub toolbar_bg: u32,
    pub nav_bg: u32,
    pub border: u32,
    pub border_light: u32,
    pub address_bg: u32,
    pub address_border: u32,
    pub list_bg: u32,
    pub item_hover: u32,
    pub item_selected: u32,
    pub item_selected_inactive: u32,
    pub header_bg: u32,
    pub button_hover: u32,
    pub button_pushed: u32,
    pub close_hover: u32,
}

impl Palette {
    /// 浅色主题：取自 360FileBrowser_Theme.ui 的实际色值。
    pub const LIGHT: Palette = Palette {
        window_bg: 0x00FF_FFFF,
        titlebar_bg: 0x00DF_E8F1,
        tabbar_bg: 0x00DF_E8F1,
        tab_active: 0x00FF_FFFF,
        tab_hover: 0x00EC_F3_FA,
        tab_inactive: 0x00DF_E8F1,
        text: 0x00_33_33_33,
        text_dim: 0x00_99_99_99,
        text_on_accent: 0x00FF_FFFF,
        accent: 0x00FF_91_00, // 360 蓝 0x0091FF，这里用品牌橙作强调色
        toolbar_bg: 0x00FF_FFFF,
        nav_bg: 0x00FF_FFFF,
        border: 0x00D9_D9D9,
        border_light: 0x00EE_EEEE,
        address_bg: 0x00FF_FFFF,
        address_border: 0x00D9_D9D9,
        list_bg: 0x00FF_FFFF,
        item_hover: 0x00F0_F6_FC,
        item_selected: 0x00CC_E8_FF,
        item_selected_inactive: 0x00E5_E5E5,
        header_bg: 0x00FA_FA_FA,
        button_hover: 0x0C00_0000,
        button_pushed: 0x1900_0000,
        close_hover: 0x00E8_1123,
    };

    /// 深色主题：取自 360FileBrowser_BlackTheme.ui。
    pub const DARK: Palette = Palette {
        window_bg: 0x00_1E_1E_1E,
        titlebar_bg: 0x00_2B_2B_2B,
        tabbar_bg: 0x00_2B_2B_2B,
        tab_active: 0x00_1E_1E_1E,
        tab_hover: 0x00_38_38_38,
        tab_inactive: 0x00_2B_2B_2B,
        text: 0x00_E0_E0E0,
        text_dim: 0x00_88_8888,
        text_on_accent: 0x00FF_FFFF,
        accent: 0x0000_91FF,
        toolbar_bg: 0x00_25_2525,
        nav_bg: 0x00_25_2525,
        border: 0x00_44_4444,
        border_light: 0x00_3A_3A3A,
        address_bg: 0x00_1E_1E_1E,
        address_border: 0x00_44_4444,
        list_bg: 0x00_1E_1E_1E,
        item_hover: 0x00_2E_2E_2E,
        item_selected: 0x00_0B_4A_7A,
        item_selected_inactive: 0x00_3A_3A3A,
        header_bg: 0x00_28_28_28,
        button_hover: 0x14FF_FFFF,
        button_pushed: 0x28FF_FFFF,
        close_hover: 0x00_E8_1123,
    };
}

/// 离屏画布。所有绘制都先落在内存位图上，最后一次 BitBlt 上屏。
pub struct BackBuffer {
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    width: i32,
    height: i32,
}

impl BackBuffer {
    pub fn new() -> Self {
        BackBuffer {
            mem_dc: std::ptr::null_mut(),
            bitmap: std::ptr::null_mut(),
            old_bitmap: std::ptr::null_mut(),
            width: 0,
            height: 0,
        }
    }

    /// 取一张尺寸匹配的离屏 DC，必要时重建位图。
    pub fn acquire(&mut self, screen: HDC, w: i32, h: i32) -> HDC {
        unsafe {
            if self.mem_dc.is_null() {
                self.mem_dc = CreateCompatibleDC(screen);
            }
            if self.width != w || self.height != h || self.bitmap.is_null() {
                if !self.bitmap.is_null() {
                    SelectObject(self.mem_dc, self.old_bitmap);
                    DeleteObject(self.bitmap);
                }
                self.bitmap = CreateCompatibleBitmap(screen, w.max(1), h.max(1));
                self.old_bitmap = SelectObject(self.mem_dc, self.bitmap);
                self.width = w;
                self.height = h;
            }
            self.mem_dc
        }
    }

    pub fn blit_to(&self, dst: HDC, x: i32, y: i32, w: i32, h: i32) {
        if self.mem_dc.is_null() {
            return;
        }
        unsafe {
            BitBlt(dst, x, y, w, h, self.mem_dc, 0, 0, SRCCOPY);
        }
    }
}

impl Drop for BackBuffer {
    fn drop(&mut self) {
        unsafe {
            if !self.mem_dc.is_null() {
                if !self.bitmap.is_null() {
                    SelectObject(self.mem_dc, self.old_bitmap);
                    DeleteObject(self.bitmap);
                }
                DeleteDC(self.mem_dc);
            }
        }
    }
}

/// 绘制画布：对 HDC 的一层薄封装。
pub struct Canvas {
    pub hdc: HDC,
}

impl Canvas {
    pub fn new(hdc: HDC) -> Self {
        Canvas { hdc }
    }

    /// 用纯色填充矩形。
    pub fn fill(&self, r: RECT, color: u32) {
        unsafe {
            let brush = CreateSolidBrush(color);
            FillRect(self.hdc, &r, brush);
            DeleteObject(brush);
        }
    }

    /// 画 1 像素水平线。
    pub fn hline(&self, x1: i32, x2: i32, y: i32, color: u32) {
        let r = RECT {
            left: x1,
            top: y,
            right: x2,
            bottom: y + 1,
        };
        self.fill(r, color);
    }

    /// 画 1 像素垂直线。
    pub fn vline(&self, x: i32, y1: i32, y2: i32, color: u32) {
        let r = RECT {
            left: x,
            top: y1,
            right: x + 1,
            bottom: y2,
        };
        self.fill(r, color);
    }

    /// 画边框。
    pub fn border(&self, r: RECT, color: u32) {
        self.hline(r.left, r.right, r.top, color);
        self.hline(r.left, r.right, r.bottom - 1, color);
        self.vline(r.left, r.top, r.bottom, color);
        self.vline(r.right - 1, r.top, r.bottom, color);
    }

    /// 圆角填充（圆角矩形按钮/输入框）。
    pub fn round_fill(&self, r: RECT, radius: i32, fill_color: u32, border_color: Option<u32>) {
        unsafe {
            let brush = CreateSolidBrush(fill_color);
            let pen_color = border_color.unwrap_or(fill_color);
            let pen = CreatePen(PS_SOLID, 1, pen_color);
            let old_brush = SelectObject(self.hdc, brush);
            let old_pen = SelectObject(self.hdc, pen);

            RoundRect(self.hdc, r.left, r.top, r.right, r.bottom, radius * 2, radius * 2);

            SelectObject(self.hdc, old_pen);
            SelectObject(self.hdc, old_brush);
            DeleteObject(pen);
            DeleteObject(brush);
        }
    }

    /// 在指定矩形内绘制单行文本。
    ///
    /// `align` 为 DT_* 标志组合。文本超出宽度时自动省略号（DT_END_ELLIPSIS）。
    pub fn text(&self, s: &str, r: RECT, color: u32, font: HFONT, flags: u32) {
        unsafe {
            let mut wide = to_wide_raw(s);
            if wide.is_empty() {
                return;
            }
            let old_font = if font.is_null() {
                std::ptr::null_mut()
            } else {
                SelectObject(self.hdc, font)
            };
            SetBkMode(self.hdc, TRANSPARENT as i32);
            SetTextColor(self.hdc, color);

            let mut rect = r;
            DrawTextW(
                self.hdc,
                wide.as_mut_ptr(),
                wide.len() as i32,
                &mut rect,
                flags,
            );

            if !old_font.is_null() {
                SelectObject(self.hdc, old_font);
            }
        }
    }

    /// 绘制图标。
    pub fn icon(&self, hicon: HICON, x: i32, y: i32, cx: i32, cy: i32) {
        if hicon.is_null() {
            return;
        }
        unsafe {
            DrawIconEx(self.hdc, x, y, hicon, cx, cy, 0, std::ptr::null_mut(), DI_NORMAL);
        }
    }

    /// 以线性插值缩放绘制位图（用于缩略图）。
    pub fn stretch_blit(&self, src: HDC, r: RECT) {
        unsafe {
            SetStretchBltMode(self.hdc, HALFTONE as i32);
            StretchBlt(
                self.hdc,
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                src,
                0,
                0,
                0,
                0,
                SRCCOPY,
            );
        }
    }
}

/// 字体仓库：按 (字号, 粗体) 缓存 HFONT，避免每次绘制都 CreateFont。
///
/// 冷启动优化：字体是懒创建的，只有真正画到某档字号时才产生 GDI 句柄。
pub struct FontCache {
    entries: Vec<(i32, bool, HFONT)>,
    dpi_scale: f32,
}

impl FontCache {
    pub fn new(dpi_scale: f32) -> Self {
        FontCache {
            entries: Vec::with_capacity(6),
            dpi_scale,
        }
    }

    /// `px` 为 96 DPI 下的逻辑像素高度。
    pub fn get(&mut self, px: i32, bold: bool) -> HFONT {
        if let Some((_, _, f)) = self
            .entries
            .iter()
            .find(|(s, b, _)| *s == px && *b == bold)
        {
            return *f;
        }
        let f = self.create(px, bold);
        self.entries.push((px, bold, f));
        f
    }

    fn create(&mut self, px: i32, bold: bool) -> HFONT {
        let height = -((px as f32 * self.dpi_scale).round() as i32);
        let face = super::to_wide("微软雅黑");
        // 注意：CreateFontW 的 cWeight/iCharSet/... 均为 u32
        let weight: u32 = if bold { FW_SEMIBOLD as u32 } else { FW_NORMAL as u32 };
        unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_TT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                face.as_ptr(),
            )
        }
    }
}

impl Drop for FontCache {
    fn drop(&mut self) {
        unsafe {
            for (_, _, f) in self.entries.drain(..) {
                if !f.is_null() {
                    DeleteObject(f);
                }
            }
        }
    }
}

/// 用户区坐标。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PointI {
    pub x: i32,
    pub y: i32,
}

impl From<POINT> for PointI {
    fn from(p: POINT) -> Self {
        PointI { x: p.x, y: p.y }
    }
}

/// 便捷：矩形包含判定。
pub fn rect_contains(r: RECT, x: i32, y: i32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// 便捷：构造矩形。
pub fn rect(l: i32, t: i32, r: i32, b: i32) -> RECT {
    RECT {
        left: l,
        top: t,
        right: r,
        bottom: b,
    }
}

/// 便捷：色值转换（我们的色值统一写成 0x00BBGGRR，与 COLORREF 一致）。
pub fn colorref(c: u32) -> COLORREF {
    c
}

/// 开始/结束绘制的守卫。
pub struct PaintScope {
    hwnd: HWND,
    ps: PAINTSTRUCT,
}

impl PaintScope {
    /// # Safety
    /// 只能在 WM_PAINT 处理中调用。
    pub unsafe fn begin(hwnd: HWND) -> Self {
        let mut ps: PAINTSTRUCT = std::mem::zeroed();
        BeginPaint(hwnd, &mut ps);
        PaintScope { hwnd, ps }
    }

    pub fn hdc(&self) -> HDC {
        self.ps.hdc
    }

    pub fn rect(&self) -> RECT {
        self.ps.rcPaint
    }
}

impl Drop for PaintScope {
    fn drop(&mut self) {
        unsafe {
            EndPaint(self.hwnd, &self.ps);
        }
    }
}
