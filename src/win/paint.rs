//! 双缓冲绘制。
//!
//! 自绘 UI 最大的性能陷阱是闪烁与重复分配。这里缓存离屏 DC 与位图，
//! 仅在窗口尺寸变化时重建；每帧只做一次 BitBlt。
//!
//! 冷启动相关：`BackBuffer` 是**懒创建**的——首次 WM_PAINT 才分配 GDI 资源，
//! 首帧之前不消耗任何句柄。

use windows_sys::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
// Gdi 通配同时提供 BeginPaint / EndPaint / PAINTSTRUCT / DrawTextW / CreateFontW 等
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL, HICON};

use super::to_wide_raw;

/// 界面配色。与 360FileBrowser_Theme.ui 中的十六进制色值一一对应。
#[derive(Clone, Copy)]
pub struct Palette {
    pub window_bg: u32,
    pub titlebar_bg: u32,
    pub tabbar_bg: u32,
    pub tab_active: u32,
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
    /// 悬停底色：标签页与标题栏按钮（菜单/最小化/最大化/关闭）共用这一个值。
    ///
    /// 两处必须同色——标签悬停如果比最小化/最大化浅一档，同一行标题栏上就出现
    /// 两种「悬停深度」，看着像两块不同的控件拼在一起。所以不再单独给标签留
    /// `tab_hover`，直接复用这里；关闭按钮也不再画系统那个红底，同样用这一色，
    /// 靠字形本身表达「关」。
    ///
    /// **必须是实色**：GDI 的纯色填充（`fill`/`round_fill`）会丢掉 alpha 字节，
    /// 早先这里写成半透明黑 `0x0C000000`，结果被当成纯黑画出来，悬停时是一块
    /// 死黑的方框。所以改成「预先混合好的浅一档底色」，直接当实色用。
    pub button_hover: u32,
    /// 按下底色，与 `button_hover` 同一套控件共用，同样是预先混合好的实色。
    pub button_pushed: u32,
    /// 容量进度条的底槽。
    pub progress_track: u32,
    /// 容量进度条的已用部分。
    pub progress_fill: u32,
    /// 可用空间告急时的进度条颜色（资源管理器同款红）。
    pub progress_low: u32,
    /// 框选矩形的描边色。
    pub marquee_border: u32,
    /// 框选矩形的填充色（绘制时按半透明叠上去）。
    pub marquee_fill: u32,
    /// 滚动条滑块。刻意用很浅的灰：它只是一条提示「还能往下翻」的小滑块，
    /// 不该像内容一样抢眼睛（深色主题下同理，用深灰而不是浅灰）。
    pub scroll_thumb: u32,
}

impl Palette {
    /// 浅色主题：取自 360FileBrowser_Theme.ui 的实际色值。
    pub const LIGHT: Palette = Palette {
        window_bg: 0x00FF_FFFF,
        titlebar_bg: 0x00F3_F3F3,
        tabbar_bg: 0x00F3_F3F3,
        tab_active: 0x00FF_FFFF,
        tab_inactive: 0x00F3_F3F3,
        text: 0x00_1B_1B1B,
        text_dim: 0x00_76_7676,
        text_on_accent: 0x00FF_FFFF,
        // 资源管理器的强调蓝（Win11 #0067C0），不再用品牌橙。
        // 注意字面量是 COLORREF `0x00BBGGRR`：这里写成 0x00C06700 才渲染成 #0067C0。
        accent: 0x00_C0_6700,
        toolbar_bg: 0x00FF_FFFF,
        nav_bg: 0x00FF_FFFF,
        border: 0x00_E5_E5E5,
        border_light: 0x00_F0_F0F0,
        address_bg: 0x00FF_FFFF,
        address_border: 0x00_CC_CCCC,
        list_bg: 0x00FF_FFFF,
        // 注意：这些常量是 GDI 的 `COLORREF`（0x00BBGGRR），不是 `#RRGGBB`。
        // 以前按 `#RRGGBB` 写，红色和蓝色是反的——浅色模式里选中行会显示成
        // 橙色蒙版，而不是资源管理器那块浅蓝。
        item_hover: 0x00FC_F3_EA,
        // 选中态＝资源管理器那块浅蓝（#CCE8FF），GDI 纯色填充不支持 alpha，预先混合成实色
        item_selected: 0x00FF_E8_CC,
        item_selected_inactive: 0x00F2_F2F2,
        header_bg: 0x00FA_FA_FA,
        button_hover: 0x00_E0_E0E0,
        button_pushed: 0x00_CC_CCCC,
        progress_track: 0x00E0_E0E0,
        progress_fill: 0x00_C0_6700,
        progress_low: 0x00_23_11E8,
        marquee_border: 0x00_C0_6700,
        marquee_fill: 0x00_F0_C99C,
        scroll_thumb: 0x00_C8_C8C8,
    };

    /// 深色主题：取自 360FileBrowser_BlackTheme.ui，底部一层做了修正。
    ///
    /// 底色不用纯黑：非 OLED 屏（LCD/IPS）根本显示不出 #000000，
    /// 纯黑在那些屏上会发灰、出现色带，且与 UI 里的深灰控件拉不开层次。
    /// 这里统一抬到 #202020，视觉上仍然是深色，但不依赖面板能不能做到全黑。
    pub const DARK: Palette = Palette {
        window_bg: 0x00_20_2020,
        titlebar_bg: 0x00_2B_2B2B,
        tabbar_bg: 0x00_2B_2B2B,
        tab_active: 0x00_20_2020,
        tab_inactive: 0x00_2B_2B2B,
        text: 0x00FB_FB_FB,
        text_dim: 0x00_9D_9D9D,
        text_on_accent: 0x00FF_FFFF,
        accent: 0x00_FF_C24C, // #4CC2FF
        toolbar_bg: 0x00_2B_2B2B,
        nav_bg: 0x00_25_2525,
        border: 0x00_3D_3D3D,
        border_light: 0x00_33_3333,
        address_bg: 0x00_2D_2D2D,
        address_border: 0x00_45_4545,
        list_bg: 0x00_20_2020,
        item_hover: 0x00_2E_2E2E,
        // 选中态＝叠在 #202020 上的一层白色 15% 遮罩（预先混合成实色）。
        // 这就是 Win11 深色资源管理器的选中观感：提亮，而不是染色。
        item_selected: 0x00_41_4141,
        item_selected_inactive: 0x00_36_3636,
        header_bg: 0x00_28_28_28,
        button_hover: 0x00_40_4040,
        button_pushed: 0x00_52_5252,
        progress_track: 0x00_3A_3A3A,
        progress_fill: 0x00_FF_C24C,
        progress_low: 0x00_4A_4AE0,
        marquee_border: 0x00_D9_9A4C,
        marquee_fill: 0x00_8A_5F2A,
        scroll_thumb: 0x00_4A_4A4A,
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

    /// 把后续绘制裁进 `r`，返回可用于 [`Canvas::unclip`] 的保存标记。
    ///
    /// 导航窗格滚动时，行的矩形会有一部分跑到窗格外面（滚出上沿的那几行）；
    /// 逐行截断文字位置既麻烦又会把图标画歪，直接让 GDI 裁掉越界部分最省事，
    /// 也保证绝不会画到工具栏或状态栏上。
    pub fn clip(&self, r: RECT) -> i32 {
        unsafe {
            let saved = SaveDC(self.hdc);
            IntersectClipRect(self.hdc, r.left, r.top, r.right, r.bottom);
            saved
        }
    }

    pub fn unclip(&self, saved: i32) {
        if saved != 0 {
            unsafe {
                RestoreDC(self.hdc, saved);
            }
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
    /// 填充一个矩形（可选 1px 边框）。
    ///
    /// 名字里的 `round` 是历史遗留：早期这套 UI 跟原软件一样是圆角风格，
    /// `radius` 参数还在签名里，但现在**一律按直角绘制**——整体视觉已改为
    /// 方正的资源管理器风格，圆角反而成了噪音。
    pub fn round_fill(&self, r: RECT, radius: i32, fill_color: u32, border_color: Option<u32>) {
        let _ = radius;
        unsafe {
            let brush = CreateSolidBrush(fill_color);
            let pen_color = border_color.unwrap_or(fill_color);
            let pen = CreatePen(PS_SOLID, 1, pen_color);
            let old_brush = SelectObject(self.hdc, brush);
            let old_pen = SelectObject(self.hdc, pen);

            Rectangle(self.hdc, r.left, r.top, r.right, r.bottom);

            SelectObject(self.hdc, old_pen);
            SelectObject(self.hdc, old_brush);
            DeleteObject(pen);
            DeleteObject(brush);
        }
    }

    /// 画一个抗锯齿的实心圆。
    ///
    /// GDI 的 `Ellipse` 边缘是硬锯齿，20px 左右的圆看着像被啃过一圈；
    /// 这里按「像素中心到圆心的距离」算覆盖率，跟**已知底色**混合后再逐点写入，
    /// 边缘就有了过渡。底色必须由调用方给——回读 `GetPixel` 又慢又容易读到半成品。
    ///
    /// 只用在悬停指示这种几十像素的小图上，逐点开销可以忽略。
    pub fn circle_fill(&self, cx: i32, cy: i32, radius: i32, color: u32, backdrop: u32) {
        let radius = radius.max(1);
        let rad = radius as f32;
        let (fr, fg, fb) = (
            ((color >> 16) & 0xFF) as f32,
            ((color >> 8) & 0xFF) as f32,
            (color & 0xFF) as f32,
        );
        let (br, bg, bb) = (
            ((backdrop >> 16) & 0xFF) as f32,
            ((backdrop >> 8) & 0xFF) as f32,
            (backdrop & 0xFF) as f32,
        );
        let mix = |f: f32, b: f32, cov: f32| (b + (f - b) * cov).round().clamp(0.0, 255.0) as u32;
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let dist = (((dx * dx + dy * dy) as f32).sqrt()).min(rad + 1.0);
                let cov = (rad + 0.5 - dist).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let px: COLORREF = mix(fr, br, cov) << 16 | mix(fg, bg, cov) << 8 | mix(fb, bb, cov);
                unsafe {
                    SetPixelV(self.hdc, cx + dx, cy + dy, px);
                }
            }
        }
    }

    /// 半透明填充一个矩形。
    ///
    /// 纯色 `CreateSolidBrush` 会忽略颜色里的 alpha 字节，所以想画出
    /// 「半透明遮罩」只能真的走一次 `AlphaBlend`：在内存 DC 里摆一块同尺寸
    /// 的纯色位图，再用 `SourceConstantAlpha` 把它压上去。
    /// 用于框选矩形这类需要透出底下内容的地方。
    pub fn fill_alpha(&self, r: RECT, color: u32, alpha: u8) {
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        unsafe {
            let mem = CreateCompatibleDC(self.hdc);
            if mem.is_null() {
                return;
            }
            let bmp = CreateCompatibleBitmap(self.hdc, w, h);
            if bmp.is_null() {
                DeleteDC(mem);
                return;
            }
            let old_bmp = SelectObject(mem, bmp);

            let brush = CreateSolidBrush(color);
            let mut local = RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            FillRect(mem, &mut local, brush);
            DeleteObject(brush);

            let bf = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: alpha,
                AlphaFormat: 0,
            };
            AlphaBlend(self.hdc, r.left, r.top, w, h, mem, 0, 0, w, h, bf);

            SelectObject(mem, old_bmp);
            DeleteObject(bmp);
            DeleteDC(mem);
        }
    }

    /// 在指定矩形内绘制单行文本。
    ///
    /// 量一段文字在指定字体下的像素宽度。
    ///
    /// 给「先量再排版」的地方用（例如导航窗格要在名字右侧预留容量数字的位置）：
    /// 预留宽度写死的话，字号或缩放一变就会重影。
    pub fn text_width(&self, s: &str, font: HFONT) -> i32 {
        unsafe {
            let wide = to_wide_raw(s);
            if wide.is_empty() {
                return 0;
            }
            let old_font = if font.is_null() {
                std::ptr::null_mut()
            } else {
                SelectObject(self.hdc, font)
            };
            let mut size = SIZE { cx: 0, cy: 0 };
            GetTextExtentPoint32W(self.hdc, wide.as_ptr(), wide.len() as i32, &mut size);
            if !old_font.is_null() {
                SelectObject(self.hdc, old_font);
            }
            size.cx
        }
    }

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

/// 字体族。
///
/// 界面文字用雅黑；图标用 Segoe MDL2 Assets——资源管理器用的就是它，
/// Win10/11 都自带，所以不必往仓库里塞任何字体或图片资源。
/// 码点是私用区（0xE000 起），`DEFAULT_CHARSET` 就能正常取到字形。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Face {
    /// 微软雅黑
    Ui,
    /// Segoe MDL2 Assets
    Icon,
}

impl Face {
    fn name(self) -> &'static str {
        match self {
            Face::Ui => "微软雅黑",
            Face::Icon => "Segoe MDL2 Assets",
        }
    }
}

/// 字体仓库：按 (族, 字号, 粗体) 缓存 HFONT，避免每次绘制都 CreateFont。
///
/// 冷启动优化：字体是懒创建的，只有真正画到某档字号时才产生 GDI 句柄。
pub struct FontCache {
    entries: Vec<(Face, i32, bool, HFONT)>,
    dpi_scale: f32,
}

impl FontCache {
    pub fn new(dpi_scale: f32) -> Self {
        FontCache {
            entries: Vec::with_capacity(8),
            dpi_scale,
        }
    }

    /// 界面文字字体。`px` 为 96 DPI 下的逻辑像素高度。
    pub fn get(&mut self, px: i32, bold: bool) -> HFONT {
        self.face(Face::Ui, px, bold)
    }

    /// 图标字体。把 `strings` 里的 `ICON_*` 码点交给它就得到图标。
    pub fn icon(&mut self, px: i32) -> HFONT {
        self.face(Face::Icon, px, false)
    }

    /// 取任意字族的字体句柄。
    pub fn face(&mut self, face: Face, px: i32, bold: bool) -> HFONT {
        if let Some((_, _, _, f)) = self
            .entries
            .iter()
            .find(|(fc, s, b, _)| *fc == face && *s == px && *b == bold)
        {
            return *f;
        }
        let f = self.create(face, px, bold);
        self.entries.push((face, px, bold, f));
        f
    }

    fn create(&mut self, face: Face, px: i32, bold: bool) -> HFONT {
        let height = -((px as f32 * self.dpi_scale).round() as i32);
        let name = super::to_wide(face.name());
        // 注意：CreateFontW 的 cWeight/iCharSet/... 均为 u32
        // CreateFontW 的 cWeight 是 i32，而 FW_* 常量在 windows-sys 里声明为 u32
        let weight: i32 = if bold { FW_SEMIBOLD as i32 } else { FW_NORMAL as i32 };
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
                name.as_ptr(),
            )
        }
    }
}

impl Drop for FontCache {
    fn drop(&mut self) {
        unsafe {
            for (_, _, _, f) in self.entries.drain(..) {
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
