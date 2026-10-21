//! 双缓冲绘制。
//!
//! 自绘 UI 最大的性能陷阱是闪烁与重复分配。这里缓存离屏 DC 与位图，
//! 仅在窗口尺寸变化时重建；每帧只做一次 BitBlt。
//!
//! 冷启动相关：`BackBuffer` 是**懒创建**的——首次 WM_PAINT 才分配 GDI 资源，
//! 首帧之前不消耗任何句柄。

use windows_sys::Win32::Foundation::{RECT, SIZE};
// Gdi 通配同时提供 BeginPaint / EndPaint / PAINTSTRUCT / DrawTextW / CreateFontW 等
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL, HICON};

use super::to_wide_raw;

/// 缓存上限。正常主题只有几十个色值，超过这个数说明调用方在按像素调色，
/// 那就别再涨了——清空重来。
const SOLID_CACHE_MAX: usize = 384;

thread_local! {
    /// 纯色画刷缓存。
    ///
    /// `Canvas::fill` 一帧要被调用几百次（每行背景、每条分隔线、每个按钮），
    /// 用到的颜色其实就那么二十来个；每次 `CreateSolidBrush` 都走一趟内核
    /// 对象创建、用完再 `DeleteObject` 销毁，纯属浪费。画刷是进程级对象，
    /// 可以跨 DC、跨帧一直复用。
    static SOLID_BRUSHES: std::cell::RefCell<std::collections::HashMap<u32, HBRUSH>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn solid_brush(color: u32) -> HBRUSH {
    SOLID_BRUSHES.with(|cell| {
        let mut map = cell.borrow_mut();
        if let Some(&h) = map.get(&color) {
            return h;
        }
        if map.len() >= SOLID_CACHE_MAX {
            for (_, h) in map.drain() {
                unsafe {
                    DeleteObject(h);
                }
            }
        }
        let h = unsafe { CreateSolidBrush(color) };
        map.insert(color, h);
        h
    })
}


/// 界面配色。
///
/// 一套语义 token 服务两套主题：功能指到哪，颜色就跟着走到哪。
/// 强调色（`accent`）承载四种「当前」语义——激活标签、选中行、焦点框、
/// 排序列——它们必须是同一个颜色，否则「当前在哪」要靠猜。
///
/// 全部为 GDI 的 `COLORREF`（`0x00BBGGRR`），不是 `#RRGGBB`。
#[derive(Clone, Copy)]
pub struct Palette {
    pub window_bg: u32,
    pub titlebar_bg: u32,
    pub tab_active: u32,
    pub tab_inactive: u32,
    pub text: u32,
    /// 更重的正文色：标题、选中项文字、表头。比 `text` 深/亮一档。
    pub text_strong: u32,
    pub text_dim: u32,
    pub text_on_accent: u32,
    pub accent: u32,
    /// 强调色的淡底：选中行、激活区背景。浅浅一层，文字仍然压得住。
    pub accent_soft: u32,
    /// 强调色的描边：焦点框、选中项左标记。
    pub accent_border: u32,
    pub toolbar_bg: u32,
    pub nav_bg: u32,
    pub border: u32,
    pub border_light: u32,
    /// 面板之间的硬分隔线（比 `border` 明显一档，用来切结构）。
    pub divider: u32,
    pub address_bg: u32,
    pub address_border: u32,
    pub list_bg: u32,
    pub item_hover: u32,
    pub item_selected: u32,
    pub item_selected_inactive: u32,
    /// 详细信息视图的斑马纹（偶数行）。极轻，只用来防串行。
    pub row_alt: u32,
    /// 表格的列分隔竖线。
    pub grid_line: u32,
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
    /// 状态栏底色。比工具栏再沉一点，让底部成为真正的「托盘」。
    pub statusbar_bg: u32,
    /// 标签胶囊/详情 chips 的底与描边（详情窗格的标签色块）。
    pub chip_bg: u32,
    pub chip_border: u32,
    /// 设置面板里「关」状态的开关底。开状态直接用 `accent`。
    pub toggle_off: u32,
    /// 滑块的轨道。已填充部分用 `accent`。
    pub slider_track: u32,
    /// 图标主色与次要图标色（工具按钮、导航箭头）。
    pub icon: u32,
    pub icon_dim: u32,
}

impl Palette {
    /// 浅色主题：纸白底 + 冷灰分层 + 单一强调蓝。
    ///
    /// 分三层：内容（最白）→ 工具栏（微灰）→ 导航/表头（再深一点）。
    /// 层级靠亮度差表达，不靠圆角和阴影——直角 + 1px 硬边就是这个主题的骨架。
    pub const LIGHT: Palette = Palette {
        window_bg: 0x00FF_FFFF,
        titlebar_bg: 0x00F7_F5F4,
        tab_active: 0x00FF_FFFF,
        tab_inactive: 0x00F7_F5F4,
        text: 0x00_1D_1816,
        text_strong: 0x00_12_0D0B,
        text_dim: 0x00_85_7768,
        text_on_accent: 0x00FF_FFFF,
        // 强调蓝 #0B6BCB：比资源管理器的 #0067C0 更冷、更饱和一分，
        // 在白底上压得住，也能和深色主题的青蓝形成同一族。
        accent: 0x00_CB_6B0B,
        accent_soft: 0x00_FD_F1E8,
        // 描边版强调蓝 #1B5FA8：比 `accent` 沉一档，做 1px 焦点框时不会
        // 抢过实心填充，但仍然是同一个蓝——「当前在哪」永远同一个答案。
        accent_border: 0x00_A8_5F1B,
        toolbar_bg: 0x00_FC_FBFA,
        nav_bg: 0x00_FA_F8F7,
        border: 0x00_EB_E6E2,
        border_light: 0x00_F3_F0ED,
        divider: 0x00_E4_DDD8,
        address_bg: 0x00FF_FFFF,
        address_border: 0x00_DA_D2CB,
        list_bg: 0x00FF_FFFF,
        item_hover: 0x00_F8_F4F1,
        // 选中态＝强调色的淡底，与激活标签、焦点框同一族
        item_selected: 0x00_FB_EEE3,
        item_selected_inactive: 0x00_F4_F0ED,
        row_alt: 0x00_FE_FCFA,
        grid_line: 0x00_F3_F0ED,
        header_bg: 0x00_F7_F4F2,
        button_hover: 0x00_F1_ECE8,
        button_pushed: 0x00_E4_DCD6,
        progress_track: 0x00_EB_E6E2,
        progress_fill: 0x00_CB_6B0B,
        progress_low: 0x00_25_30D9,
        marquee_border: 0x00_CB_6B0B,
        marquee_fill: 0x00_FA_E7D6,
        scroll_thumb: 0x00_D4_CBC4,
        statusbar_bg: 0x00_F7_F5F4,
        chip_bg: 0x00_F7_F2EE,
        chip_border: 0x00_EB_E3DC,
        toggle_off: 0x00_D4_CBC4,
        slider_track: 0x00_EB_E6E2,
        icon: 0x00_54_4B44,
        icon_dim: 0x00_9E_938A,
    };

    /// 深色主题：石墨底 + 亮度分层 + 青蓝强调。
    ///
    /// 底色不用纯黑：非 OLED 屏（LCD/IPS）根本显示不出 #000000，
    /// 纯黑在那些屏上会发灰、出现色带，且与 UI 里的深灰控件拉不开层次。
    /// 这里统一抬到 #1B1D21，视觉上仍然是深色，但不依赖面板能不能做到全黑。
    pub const DARK: Palette = Palette {
        window_bg: 0x00_21_1D1B,
        titlebar_bg: 0x00_2C_2724,
        tab_active: 0x00_21_1D1B,
        tab_inactive: 0x00_2C_2724,
        text: 0x00_F0_EAE6,
        text_strong: 0x00_FF_FFFF,
        text_dim: 0x00_AE_A298,
        text_on_accent: 0x00_21_1D1B,
        // 强调青蓝 #58A6FF：深底上亮度足够，做选中底也不会刺眼
        accent: 0x00_FF_A658,
        accent_soft: 0x00_38_2A1E,
        // 描边版强调蓝 #4A8CD9：在深底上要压得住底色，又不能亮到跟
        // 实心 `accent` 混淆——描边读「边界」，填充读「整块」。
        accent_border: 0x00_D9_8C4A,
        toolbar_bg: 0x00_2A_2522,
        nav_bg: 0x00_26_211E,
        border: 0x00_40_3733,
        border_light: 0x00_35_2E2A,
        divider: 0x00_4C_423D,
        address_bg: 0x00_30_2A26,
        address_border: 0x00_53_4741,
        list_bg: 0x00_21_1D1B,
        item_hover: 0x00_32_2B27,
        item_selected: 0x00_49_3422,
        item_selected_inactive: 0x00_36_2E2A,
        row_alt: 0x00_26_211E,
        grid_line: 0x00_35_2E2A,
        header_bg: 0x00_2B_2623,
        button_hover: 0x00_3E_3631,
        button_pushed: 0x00_4C_423C,
        progress_track: 0x00_40_3733,
        progress_fill: 0x00_FF_A658,
        progress_low: 0x00_5E_6BF2,
        marquee_border: 0x00_FF_A658,
        marquee_fill: 0x00_5E_4023,
        scroll_thumb: 0x00_51_4741,
        statusbar_bg: 0x00_2A_2522,
        chip_bg: 0x00_32_2B27,
        chip_border: 0x00_4A_403A,
        toggle_off: 0x00_55_4B45,
        slider_track: 0x00_40_3733,
        icon: 0x00_C4_B8AE,
        icon_dim: 0x00_86_796F,
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
    ///
    /// 返回值第二项表示这块位图是不是**刚重建**的：重建意味着上一帧的内容
    /// 已经没了，调用方必须按全窗重画，不能只画脏区。
    pub fn acquire(&mut self, screen: HDC, w: i32, h: i32) -> (HDC, bool) {
        unsafe {
            if self.mem_dc.is_null() {
                self.mem_dc = CreateCompatibleDC(screen);
            }
            let mut recreated = false;
            if self.width != w || self.height != h || self.bitmap.is_null() {
                if !self.bitmap.is_null() {
                    SelectObject(self.mem_dc, self.old_bitmap);
                    DeleteObject(self.bitmap);
                }
                self.bitmap = CreateCompatibleBitmap(screen, w.max(1), h.max(1));
                self.old_bitmap = SelectObject(self.mem_dc, self.bitmap);
                self.width = w;
                self.height = h;
                recreated = true;
            }
            (self.mem_dc, recreated)
        }
    }

    /// 把离屏位图的一块贴到屏幕。
    ///
    /// 源坐标必须和目标坐标一致：离屏位图与客户区一一对应，只传脏区时
    /// 要从位图的同一位置取。这里曾经写死源 `(0, 0)`，等于把位图左上角
    /// 那一块贴到脏区上——局部重绘（插入符闪烁、拖动高亮）于是把标签栏
    /// 的内容搬到地址栏和导航窗格的位置，看起来就是「错位」。
    pub fn blit_to(&self, dst: HDC, x: i32, y: i32, w: i32, h: i32) {
        if self.mem_dc.is_null() {
            return;
        }
        unsafe {
            BitBlt(dst, x, y, w, h, self.mem_dc, x, y, SRCCOPY);
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
        let brush = solid_brush(color);
        unsafe {
            FillRect(self.hdc, &r, brush);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 包含判定是「左上闭、右下开」，相邻矩形不会同时命中同一点。
    #[test]
    fn rect_contains_is_half_open() {
        let r = rect(10, 20, 30, 40);
        assert!(rect_contains(r, 10, 20));
        assert!(rect_contains(r, 29, 39));
        assert!(!rect_contains(r, 30, 20));
        assert!(!rect_contains(r, 10, 40));
        assert!(!rect_contains(r, 9, 20));
    }

    /// 空矩形（宽或高为 0）不含任何点。
    #[test]
    fn empty_rect_contains_nothing() {
        assert!(!rect_contains(rect(5, 5, 5, 5), 5, 5));
        assert!(!rect_contains(rect(0, 0, 0, 100), 0, 50));
    }

    /// `rect` 的四个字段按参数就位（自绘代码全靠它拼矩形）。
    #[test]
    fn rect_fields_are_in_order() {
        let r = rect(1, 2, 3, 4);
        assert_eq!((r.left, r.top, r.right, r.bottom), (1, 2, 3, 4));
    }
}
