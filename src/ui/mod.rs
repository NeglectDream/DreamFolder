//! UI 层：布局、绘制、交互。
//!
//! 结构对照原软件 `explorer_frame.xml` 的控件树：
//! ```text
//! Window
//!  └ VerticalLayout
//!     ├ HorizontalLayout  height=32   ← 标题栏（标签 + 窗口按钮）
//!     ├ FavoritesPanel    height=30   ← 收藏书签栏
//!     ├ ToolContainer     height=48   ← 工具栏
//!     ├ HorizontalVCenter height=40   ← 导航栏（后退/前进/地址/搜索）
//!     └ HwndControl hostView          ← 文件视图（我们自绘）
//! ```

pub mod draw;
pub mod interact;
pub mod navpane;
pub mod cards;
pub mod favbar;
pub mod settingspane;
pub mod dialog;
pub mod detailtags;
pub mod preview;

use windows_sys::Win32::Foundation::RECT;

use crate::win::paint::rect;

/// 布局常量（96 DPI 下的逻辑像素；实际使用时会乘 DPI 缩放）。
///
/// 凡是系统有标准值的一律对齐 Windows 原生控件，而不是各写一个好看的数目——
/// 这样窗口按钮、标题栏、状态栏的宽高与系统其它窗口一致，不会显得突兀。
pub mod metrics {
    /// 标题栏高度。Windows 8 起为 32（含 1px 边框），Win11 同。
    pub const TITLEBAR_H: i32 = 32;
    pub const FAVBAR_H: i32 = 30;
    pub const TOOLBAR_H: i32 = 48;
    pub const NAVBAR_H: i32 = 40;
    /// 状态栏高度，对齐原生 `STATUSCLASSNAME` 的 24。
    pub const STATUSBAR_H: i32 = 24;

    pub const TAB_MIN_W: i32 = 64;
    pub const TAB_MAX_W: i32 = 200;
    /// 「新建标签」按钮宽度（逻辑值）。
    pub const NEW_TAB_W: i32 = 22;

    /// 窗口按钮（菜单/最小化/最大化/关闭）的单个宽度。
    ///
    /// Windows 10/11 的标题栏按钮固定为 46×32（非客户区自动缩放），
    /// 系统没有暴露这个值的 API，所以按平台标准硬编码。
    /// 早先这里是 32——比原生窄三成，四个按钮挤在右上角，一眼就不像系统窗口。
    pub const WINDOW_BUTTON_W: i32 = 46;

    /// 地址栏最小宽度。低于这个值面包屑会被截成「此...」，
    /// 所以分配空间时它优先于搜索框。
    pub const ADDR_MIN_W: i32 = 320;
    pub const SEARCH_W: i32 = 200;

    /// 导航窗格中一行的高度（含图标与文字）。
    pub const NAV_ROW_H: i32 = 28;

    /// 详细信息视图的表头高度，也是列表行高。
    pub const DETAIL_HEADER_H: i32 = 24;
    /// 右侧详情窗格里「标签 / 属性」每一行的高度。
    pub const DETAIL_ROW_H: i32 = 22;
    /// 每层缩进。
    pub const NAV_INDENT: i32 = 20;

    /// 文件视图的最大内容宽度（逻辑像素）。
    ///
    /// 窗口再宽，表格也停在这个宽度上：右侧留白，而不是把四列无限拉长。
    /// 这是「拉伸窗口不等于把内容拉变形」的实现点。
    pub const VIEW_MAX_W: i32 = 900;

    /// 窗口最小尺寸（对应 XML 的 mininfo="496,300"）
    pub const MIN_WINDOW_W: i32 = 496;
    pub const MIN_WINDOW_H: i32 = 300;
}

/// 各区域矩形。每帧在 `Layout::compute` 中重算。
///
/// 未派生 `Default`/`Debug`：`RECT` 是 FFI 结构，两者都没实现，
/// 这里手写 `Default`（全零矩形 + 缩放 1.0），而不是为了展示去包一层类型。
/// 右侧三列的默认逻辑宽度（修改日期 / 类型 / 大小）。
/// 用户拖过列宽之后由 `settings.detail_col_w` 覆盖。
const DEFAULT_DETAIL_COLS: [i32; 3] = [148, 85, 117];

#[derive(Clone, Copy)]
pub struct Layout {
    pub client: RECT,

    pub titlebar: RECT,
    /// 标签条（标题栏内，除去窗口按钮）
    pub tabstrip: RECT,
    /// 新建标签按钮
    /// 窗口按钮区（菜单/最小化/最大化/关闭）
    pub window_buttons: RECT,

    pub favbar: RECT,
    pub toolbar: RECT,
    pub navbar: RECT,

    /// 导航按钮
    pub btn_back: RECT,
    pub btn_forward: RECT,
    pub btn_up: RECT,
    pub btn_refresh: RECT,

    /// 地址栏（面包屑 + 编辑框合一）
    pub address: RECT,
    pub search: RECT,

    /// 文件视图
    pub view: RECT,
    pub statusbar: RECT,

    /// 导航窗格（左侧）
    pub nav_pane: RECT,
    /// 详细信息窗格（底部）
    pub detail_pane: RECT,

    /// DPI 缩放系数
    pub scale: f32,
}

impl Layout {
    /// 状态栏右侧的两个视图切换按钮：`[详细列表, 大图标]`。
    ///
    /// 不塞进 `Layout` 字段：布局阶段算它们会让 `compute` 更臃肿，
    /// 而它们只依赖 `statusbar` 和缩放，随用随算就够了。
    pub fn status_view_buttons(&self) -> [RECT; 2] {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let w = s(28);
        let h = s(22);
        let gap = s(4);
        let y = self.statusbar.top + (self.statusbar.bottom - self.statusbar.top - h) / 2;
        let right = self.statusbar.right - s(8);
        [
            crate::win::paint::rect(right - w * 2 - gap, y, right - w - gap, y + h),
            crate::win::paint::rect(right - w, y, right, y + h),
        ]
    }

    /// 地址栏里真正摆放文字的那条矩形（去掉左右内边距与右侧留白）。
    ///
    /// 绘制文字、插入符、选区高亮和「点哪落哪」必须用同一个矩形：这几处
    /// 以前各自按 `s(8)` / `s(4)` 现算，缩放一变就会悄无声息地错开。
    pub fn address_text_rect(&self) -> RECT {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        rect(
            self.address.left + s(8),
            self.address.top,
            self.address.right - s(4),
            self.address.bottom,
        )
    }

    /// 详细信息窗格上半部分的预览框。
    pub fn detail_preview_rect(&self) -> RECT {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let p = self.detail_pane;
        if p.right <= p.left {
            return rect(0, 0, 0, 0);
        }
        let pad = s(12);
        let h = ((p.bottom - p.top) / 3).max(s(96));
        rect(p.left + pad, p.top + pad, p.right - pad, p.top + pad + h)
    }

    /// 详情窗格里的四个按钮：打开 / 新标签打开 / 复制路径 / 更多。
    ///
    /// 一行四个，按窗格宽度均分，缩放变化时一起变，不用另算。
    /// 窗格窄到每个按钮放不下名字时改排成两行两列——
    /// 一行硬挤四个的话「复制路径」会被裁成「复制…」，点了也不知道是哪个。
    pub fn detail_button_rects(&self) -> [RECT; 4] {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let p = self.detail_pane;
        if p.right <= p.left {
            let z = rect(0, 0, 0, 0);
            return [z, z, z, z];
        }
        let pad = s(12);
        let gap = s(6);
        let top = self.detail_preview_rect().bottom + s(10);
        let h = s(28);
        let avail = (p.right - pad) - (p.left + pad);
        // 单行时的按钮宽度；低于「四个字 + 内边距」就换成两列
        let one_row_w = (avail - gap * 3) / 4;
        let cols = if one_row_w < s(60) { 2 } else { 4 };
        let w = if cols == 4 {
            one_row_w
        } else {
            (avail - gap) / 2
        }
        .max(s(20));
        let mut out = [rect(0, 0, 0, 0); 4];
        for (i, slot) in out.iter_mut().enumerate() {
            let col = (i % cols) as i32;
            let row = (i / cols) as i32;
            let l = p.left + pad + col * (w + gap);
            let t = top + row * (h + s(4));
            *slot = rect(l, t, l + w, t + h);
        }
        out
    }

    /// 详情窗格按钮区的最低点（下面的属性列表从这里往下排）。
    ///
    /// 单行时就是那一行的下沿，两行时要算上第二行——否则属性列表会直接压在
    /// 第二排按钮上。
    pub fn detail_buttons_bottom(&self) -> i32 {
        let rects = self.detail_button_rects();
        rects.iter().map(|r| r.bottom).max().unwrap_or(0)
    }

    /// 文件视图的实际内容右边界。
    ///
    /// 超过 `metrics::VIEW_MAX_W` 的部分不再参与布局——表格右侧留白，
    /// 而不是把列宽跟着窗口一起拉。
    pub fn content_right(&self) -> i32 {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let max_w = s(metrics::VIEW_MAX_W);
        let vr = self.view;
        if vr.right - vr.left <= max_w {
            vr.right
        } else {
            vr.left + max_w
        }
    }

    /// 工具栏第 `i` 个按钮的矩形。绘制与命中共用，避免两处各排一遍。
    ///
    /// 按钮之间是均等的宽间隔：早先按功能分组、组间画一条竖线再留空，
    /// 结果每一格宽窄不一，眼睛总在数「这是第几组」。现在只靠留白分节奏，
    /// 图标本身已经说明功能，竖线反而把 48px 高的条切成了碎块。
    pub fn toolbar_button_rect(&self, i: usize) -> RECT {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let btn = s(36);
        let gap = s(10);
        let x = self.toolbar.left + s(12) + (btn + gap) * i as i32;
        let y = self.toolbar.top + (self.toolbar.bottom - self.toolbar.top - btn) / 2;
        crate::win::paint::rect(x, y, x + btn, y + btn)
    }

    /// 详细信息视图的列矩形：`[名称, 修改日期, 类型, 大小]`。
    ///
    /// 从右往左排：右侧三列各自占固定宽度，名称列吃剩下的空间。
    /// 窗口窄到放不下时，被裁的是名称，不是右边——和资源管理器一致，
    /// 也和「列宽可拖」配套：拖动只会改右列的宽度。
    pub fn detail_columns(&self, widths: [i32; 3]) -> [(i32, i32); 4] {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let vr = self.view;
        let gap = s(8);
        let right = self.content_right() - s(8);
        let name_l = vr.left + s(28);

        // 右侧三列用各自配置的宽度（0 = 用默认值），**宽度不随窗口压缩**：
        // 放不下就整列不画，而不是把每一列都挤瘦一点。
        let w = |i: usize| -> i32 {
            let v = widths[i];
            if v > 0 {
                s(v)
            } else {
                s(DEFAULT_DETAIL_COLS[i])
            }
        };
        let min_name = s(160);

        // 从最右边开始丢列：先「大小」，再「类型」，最后「修改日期」。
        // 名称列至少留 `min_name`，剩下的宽度全归它。
        let mut count = 3;
        while count > 0 {
            let need: i32 = (0..count).map(|i| w(i) + gap).sum();
            if (right - name_l) - need >= min_name {
                break;
            }
            count -= 1;
        }

        let need: i32 = (0..count).map(|i| w(i) + gap).sum();
        let name_w = ((right - name_l) - need).max(min_name);
        let name_r = name_l + name_w;

        // 丢掉的列给一个空矩形：`column_visible` 会判成不可见，
        // 绘制与命中测试都跳过它——位置也就不用操心「记住」。
        let mut cols = [(name_l, name_r), (0, 0), (0, 0), (0, 0)];
        let mut x = name_r;
        for (i, slot) in cols.iter_mut().enumerate().skip(1) {
            if i - 1 >= count {
                break;
            }
            let left = x + gap;
            *slot = (left, left + w(i - 1));
            x = slot.1;
        }
        cols
    }

    /// 这一列是否还落在可视范围内。
    ///
    /// 宽度不足时右侧列会整列「推到可视区外」：既不压缩宽度，也不重排位置，
    /// 只是不画——所以窗口拉宽之后它还在原来那一格。
    pub fn column_visible(&self, col: (i32, i32)) -> bool {
        col.1 > col.0 && col.1 <= self.content_right()
    }

    /// 列分隔条的热区（拖动改列宽）：依次是日期/类型/大小的左边界。
    pub fn detail_col_splitter(&self, widths: [i32; 3], index: usize) -> RECT {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let cols = self.detail_columns(widths);
        let Some(&(left, right)) = cols.get(index + 1) else {
            return rect(0, 0, 0, 0);
        };
        // 列已经被推到可视区外时，它的分隔条也一并失效，
        // 否则会在看不见的列上出现「拖不动又挡点击」的热区
        if !self.column_visible((left, right)) {
            return rect(0, 0, 0, 0);
        }
        let half = s(3).max(2);
        let vr = self.view;
        // 只在表头那一条里生效：整列高度的热区会让列表里的点击变成拖列宽
        let h = s(metrics::DETAIL_HEADER_H);
        rect(left - half, vr.top, left + half, vr.top + h)
    }

    /// 右侧详细信息窗格左边的分隔条热区。
    pub fn detail_pane_splitter(&self) -> RECT {
        if self.detail_pane.right <= self.detail_pane.left {
            return rect(0, 0, 0, 0);
        }
        let half = (6.0 * self.scale).round() as i32;
        let x = self.detail_pane.left;
        rect(x - half, self.detail_pane.top, x + half, self.detail_pane.bottom)
    }

    /// 导航窗格与文件视图之间的分隔条热区。
    ///
    /// 比视觉线宽一些：真正能拖的目标只有 1~2px 的话根本抓不住。
    pub fn nav_splitter(&self) -> RECT {
        if self.nav_pane.right <= self.nav_pane.left {
            return rect(0, 0, 0, 0);
        }
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let half = s(3).max(2);
        rect(
            self.nav_pane.right - half,
            self.nav_pane.top,
            self.nav_pane.right + half,
            self.nav_pane.bottom,
        )
    }

    /// 文件视图里第 `i` 个条目的矩形（客户区坐标，未做可见性裁剪）。
    ///
    /// 绘制、命中测试、框选三处都走这一个函数：以前网格和行两条分支
    /// 各写一份宽高计算，缩放一变就可能对不齐。
    pub fn item_rect(
        &self,
        view_mode: crate::cfg::settings::ViewMode,
        i: usize,
        scroll_y: i32,
    ) -> RECT {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let vr = self.view;

        if view_mode.is_rows() {
            let row_h = s(24);
            let mut top = vr.top;
            if view_mode == crate::cfg::settings::ViewMode::Detail {
                top += row_h; // 表头占一行
            }
            let y = top + i as i32 * row_h - scroll_y;
            return crate::win::paint::rect(vr.left, y, vr.right, y + row_h);
        }

        let (cw, ch) = view_mode.cell();
        let icon_px = s(view_mode.icon_size());
        let cell_w = if cw == 0 { s(112) } else { s(cw).max(icon_px + s(24)) };
        let cell_h = s(ch).max(icon_px + s(34));
        // 可用宽度基准必须与命中测试/绘制一致（视图矩形全宽）。以前用
        // content_right() 封顶，窗口拉宽超过 VIEW_MAX_W 后列数和绘制
        // 对不上，框选选中的项与框住的格子错开几列。
        let avail_w = (vr.right - vr.left).max(cell_w);
        let cols = (avail_w / cell_w).max(1) as usize;

        let col = (i % cols) as i32;
        let row = (i / cols) as i32;
        let x = vr.left + col * cell_w;
        let y = vr.top + row * cell_h - scroll_y;
        crate::win::paint::rect(x, y, x + cell_w, y + cell_h)
    }
}

impl Default for Layout {
    fn default() -> Self {
        let r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        Layout {
            client: r,
            titlebar: r,
            tabstrip: r,
            window_buttons: r,
            favbar: r,
            toolbar: r,
            navbar: r,
            btn_back: r,
            btn_forward: r,
            btn_up: r,
            btn_refresh: r,
            address: r,
            search: r,
            view: r,
            statusbar: r,
            nav_pane: r,
            detail_pane: r,
            scale: 1.0,
        }
    }
}

impl Layout {
    /// 计算布局。`dpi_scale` 由窗口 DPI 决定。
    pub fn compute(
        client: RECT,
        dpi_scale: f32,
        ui_scale: f32,
        show_favbar: bool,
        show_toolbar: bool,
        show_nav_pane: bool,
        show_detail_pane: bool,
        nav_pane_w: i32,
        detail_pane_w: i32,
    ) -> Layout {
        // 用户偏好叠加在系统 DPI 上，之后所有尺寸都从这一个系数派生
        let scale = dpi_scale * ui_scale;
        let s = |v: i32| -> i32 { (v as f32 * scale).round() as i32 };
        let w = client.right - client.left;
        let h = client.bottom - client.top;

        let mut y = client.top;

        // 极端缩放下，固定行（标题栏/工具栏/导航栏/状态栏）加起来可能比窗口还高，
        // 文件视图会被压成零甚至负数——那时点击、框选会整个失效。
        // 这里逐级让位：先收工具栏，再收收藏栏，保证文件区至少留 s(80)。
        // 只影响这一次布局，用户设置本身不动，空间回来时它们自己会回来。
        let fixed_h =
            s(metrics::TITLEBAR_H) + s(metrics::NAVBAR_H) + s(metrics::STATUSBAR_H);
        let need_view = s(80);
        let toolbar_ok = show_toolbar && h - fixed_h - s(metrics::TOOLBAR_H) >= need_view;
        let favbar_ok = show_favbar
            && h - fixed_h
                - if toolbar_ok { s(metrics::TOOLBAR_H) } else { 0 }
                - s(metrics::FAVBAR_H)
                >= need_view;

        // 标题栏 + 标签
        let titlebar = rect(client.left, y, client.right, y + s(metrics::TITLEBAR_H));
        y += s(metrics::TITLEBAR_H);

        let wb_w = s(metrics::WINDOW_BUTTON_W) * 4;
        let tabstrip = rect(
            client.left + s(6),
            titlebar.top,
            (client.right - wb_w - s(6)).max(client.left + s(6)),
            titlebar.bottom,
        );
        let window_buttons = rect(tabstrip.right, titlebar.top, client.right, titlebar.bottom);
        // 「+」按钮的位置随最后一个标签走，见 `new_tab_rect`

        // 收藏栏
        let favbar = if favbar_ok {
            let r = rect(client.left, y, client.right, y + s(metrics::FAVBAR_H));
            y += s(metrics::FAVBAR_H);
            r
        } else {
            rect(0, 0, 0, 0)
        };

        // 工具栏
        let toolbar = if toolbar_ok {
            let r = rect(client.left, y, client.right, y + s(metrics::TOOLBAR_H));
            y += s(metrics::TOOLBAR_H);
            r
        } else {
            rect(0, 0, 0, 0)
        };

        // 导航栏
        let navbar = rect(client.left, y, client.right, y + s(metrics::NAVBAR_H));
        y += s(metrics::NAVBAR_H);

        // 导航栏内部
        let pad = s(12);
        let icon = s(16);
        let btn_h = navbar.bottom - navbar.top;
        let btn_y_top = navbar.top + (btn_h - icon) / 2;
        let btn_y_bot = btn_y_top + icon;

        let mut x = navbar.left + pad;
        let btn_back = rect(x, btn_y_top, x + icon, btn_y_bot);
        x += icon + s(8);
        let btn_forward = rect(x, btn_y_top, x + icon, btn_y_bot);
        x += icon + s(8);
        let btn_up = rect(x, btn_y_top, x + icon, btn_y_bot);
        x += icon + s(8);

        // 右侧：搜索框。**地址栏优先**——空间不够时先压缩搜索框，
        // 而不是把搜索框往右顶出 navbar.right（那会让两者都画错，
        // 面包屑被截成「此...」正是这么来的）。
        let gap = s(10);
        let nav_avail = (navbar.right - pad - x).max(0);
        let search_w = s(metrics::SEARCH_W)
            .min((nav_avail - s(metrics::ADDR_MIN_W) - gap).max(s(120)));
        let search = rect(
            navbar.right - pad - search_w,
            navbar.top + s(6),
            navbar.right - pad,
            navbar.bottom - s(6),
        );

        // 地址栏吃掉中间剩下的全部空间
        let address = rect(
            x,
            navbar.top + s(6),
            (search.left - gap).max(x + s(60)),
            navbar.bottom - s(6),
        );

        let btn_refresh = rect(
            address.right - s(24),
            btn_y_top,
            address.right - s(8),
            btn_y_bot,
        );

        // 底部状态栏
        let statusbar = rect(
            client.left,
            client.bottom - s(metrics::STATUSBAR_H),
            client.right,
            client.bottom,
        );

        // 中间区域：视图 + 可选侧栏
        let mid_top = y;
        let mid_bottom = statusbar.top;

        // 视图区高度先定下来：右侧窗格和文件区共用这一条竖带
        let view_bottom = mid_bottom.max(mid_top + s(40));

        // 详细信息窗格 = **右侧竖条**，和左边导航栏对称，不再占底部一条。
        // 宽度和导航窗格一样由用户拖动决定：夹一个下限，再保证它不超过窗口的 45%，
        // 否则窗口一窄，文件区就被挤没了。
        let detail_w = if show_detail_pane {
            let want = s(detail_pane_w.max(120));
            let max = (w * 45 / 100).max(s(160));
            want.clamp(s(160), max)
        } else {
            0
        };
        let detail_pane = if show_detail_pane {
            rect(client.right - detail_w, mid_top, client.right, view_bottom)
        } else {
            rect(0, 0, 0, 0)
        };

        // 导航窗格宽度由用户拖分隔条决定（逻辑宽度 × scale）。
        // 只夹下限和「最多占窗口 60%」：以前写死 `min(w/3)`，缩放一大
        // 两块区域就一起缩水，看起来左右不对等。
        let nav_w = if show_nav_pane {
            let want = s(nav_pane_w.max(120));
            let max = (w * 3 / 5).max(s(160));
            want.clamp(s(140), max)
        } else {
            0
        };
        let nav_pane = if show_nav_pane {
            rect(client.left, mid_top, client.left + nav_w, view_bottom)
        } else {
            rect(0, 0, 0, 0)
        };

        let view = rect(
            client.left + nav_w,
            mid_top,
            (client.right - detail_w).max(client.left + nav_w + s(120)),
            view_bottom,
        );

        let _ = w;
        let _ = h;

        Layout {
            client,
            titlebar,
            tabstrip,
            window_buttons,
            favbar,
            toolbar,
            navbar,
            btn_back,
            btn_forward,
            btn_up,
            btn_refresh,
            address,
            search,
            view,
            statusbar,
            nav_pane,
            detail_pane,
            scale,
        }
    }

    /// 单个标签的宽度。
    ///
    /// 浏览器式：标签少时每个都用最大宽度（不铺满整条），多到装不下才一起收窄，
    /// 收到底线 `TAB_MIN_W` 为止。之前是「先均分再夹紧」，两个标签时就被拉成
    /// 半屏宽、五个标签时又齐刷刷变窄，看着不像浏览器。
    fn tab_width(&self, count: usize) -> i32 {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let count_i = count.max(1) as i32;
        // 给紧跟其后的「+」按钮留出位置
        let avail = (self.tabstrip.right - self.tabstrip.left - s(metrics::NEW_TAB_W) - s(6))
            .max(s(48));
        let fit = avail / count_i;
        let max_w = s(metrics::TAB_MAX_W);
        let min_w = s(metrics::TAB_MIN_W).min(avail);
        if fit >= max_w {
            max_w
        } else {
            fit.max(min_w)
        }
    }

    /// 标签的绘制矩形。宽度见 [`Layout::tab_width`]。
    pub fn tab_rect(&self, index: usize, count: usize) -> RECT {
        if count == 0 {
            return rect(0, 0, 0, 0);
        }
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let w = self.tab_width(count);
        let x = self.tabstrip.left + index as i32 * w;
        // 右端留 2px 缝，标签之间不糊成一片
        rect(
            x,
            self.tabstrip.top + s(4),
            x + w - s(2),
            self.tabstrip.bottom,
        )
    }

    /// 「+」按钮：**紧贴最后一个标签**，而不是钉在标签条最右端。
    ///
    /// 标签铺满整条时它会贴着右边界停住，不会越界压到窗口按钮上。
    pub fn new_tab_rect(&self, count: usize) -> RECT {
        let s = |v: i32| (v as f32 * self.scale).round() as i32;
        let w = self.tab_width(count);
        let right_limit = (self.tabstrip.right - s(metrics::NEW_TAB_W)).max(self.tabstrip.left);
        let left = (self.tabstrip.left + w * count as i32 + s(4)).min(right_limit);
        rect(
            left,
            self.titlebar.top + s(4),
            left + s(metrics::NEW_TAB_W),
            self.titlebar.bottom - s(4),
        )
    }
}

/// 命中测试结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    None,
    Titlebar,
    Tab(usize),
    TabClose(usize),
    NewTab,
    MenuButton,
    Minimize,
    Maximize,
    Close,
    Back,
    Forward,
    Up,
    Refresh,
    Address,
    /// 地址栏里落在某一层路径上（点它＝跳到该层，而不是进编辑态）
    AddressLevel,
    Search,
    FavoriteItem(usize),
    FavoriteMore,
    ToolbarButton(usize),
    /// 文件视图中的条目
    ListItem(usize),
    /// 文件视图空白区
    ViewEmpty,
    /// 详细信息视图里第 N 条列分隔条（0=日期 1=类型 2=大小）。
    DetailCol(usize),
    /// 详细信息视图表头的第 N 列（0=名称 1=日期 2=类型 3=大小），点击切换排序。
    DetailHeader(usize),
    /// 右侧详情窗格的第 N 个按钮（0=打开 1=新标签 2=复制路径 3=更多）。
    DetailButton(usize),
    /// 详情窗格标签模块里的「添加」按钮。
    DetailTagAdd,
    /// 详情窗格标签模块的第 N 行右端那个叉（把这一项从该标签移除）。
    DetailTagClose(usize),
    /// 地址栏历史下拉里的第 N 条建议（点击＝跳到该路径）。
    AddrSuggest(usize),
    /// 地址栏历史下拉的浮层本身（吃掉点击，别穿透到下面的视图）。
    AddrSuggestPane,
    Statusbar,
    /// 导航窗格里的第 N 行（点击＝跳转到该位置）。
    NavItem(usize),
    /// 导航窗格第 N 行左端的展开箭头。
    NavArrow(usize),
    /// 导航窗格第 N 行是分组分隔线：按住上下拖可以调整分组顺序。
    NavSeparator(usize),
    /// 导航窗格的空白区域（点击不做事）。
    NavPane,
    /// 导航窗格右边界的分隔条（左右拖动改宽度）。
    NavSplitter,
    /// 右侧详细信息窗格左边界的分隔条（左右拖动改宽度）。
    DetailSplitter,
    /// 详情窗格内容模块之间的分隔线：按住上下拖可以调整模块顺序。
    /// 0 = 上面那个模块（与 `cfg.settings.detail_sections` 的下标对应）。
    DetailSeparator(usize),
    /// 状态栏右侧的视图切换按钮：0 = 详细列表，1 = 大图标。
    ViewToggle(usize),
    /// 设置面板里第 N 项（索引见 `settingspane::ITEMS`）。
    SettingsItem(usize),
    /// 设置面板的关闭按钮。
    SettingsClose,
    /// 设置面板本身（吃掉点击，不让它穿到底下）。
    SettingsPane,
    DetailPane,
    /// 文件视图的滚动条（滑块可以按住拖动）。
    ViewScrollbar,
    /// 导航窗格的滚动条（滑块可以按住拖动）。
    NavScrollbar,
    /// 对话框的第 N 个按钮（0 = 最右边的主按钮）。
    DialogButton(usize),
    /// 对话框的输入框。
    DialogInput,
    /// 对话框第 N 个颜色环。
    DialogColor(usize),
    /// 对话框面板本身或面板外（模态：吃掉点击，不穿透也不关闭）。
    DialogPane,
}

/// 一条纵向滚动条的几何。
///
/// 绘制、命中测试、拖动三处必须用同一份数字，否则会出现「看得见但点不到」
/// 或者「拖动时滑块和鼠标错位」。所以只在这里算一次。
pub struct ScrollBar {
    /// 滑块（细线形态时是那条细矩形）。
    pub thumb: RECT,
    /// 鼠标命中区：比细线宽一圈，否则一两个像素宽的东西根本按不住。
    pub hit: RECT,
    /// 轨道的上端。
    pub track_top: i32,
    /// 滑块高度与可移动距离（拖动时按比例换算滚动量）。
    pub thumb_h: i32,
    pub travel: i32,
    pub max_scroll: i32,
}

/// 哪一块内容的滚动条。两块各自独立拖动，互不干扰。
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScrollPane {
    /// 文件视图。
    View,
    /// 导航窗格。
    Nav,
}

/// 正在被拖动的滚动条。
pub struct ScrollDrag {
    pub pane: ScrollPane,
    /// 按下点相对滑块顶端的偏移：拖动时滑块跟手而不跳到鼠标上。
    pub grab: i32,
    /// 按下那一刻的几何。拖动过程中不重算，避免「滚动 → 滑块变高 → 又改滚动」的抖动。
    pub bar: ScrollBar,
}

impl ScrollBar {
    /// 滑块纵向中心点上移/下移到 `y` 时对应的滚动量。
    pub fn scroll_for_center(&self, y: i32) -> i32 {
        if self.travel <= 0 {
            return 0;
        }
        let top = (y - self.thumb_h / 2 - self.track_top).clamp(0, self.travel);
        ((top as f32 / self.travel as f32) * self.max_scroll as f32).round() as i32
    }
}

/// 算一条纵向滚动条的形状。`None` 表示内容没超出，不需要滚动条。
///
/// 导航窗格与文件列表共用：`vr` 是那块内容的矩形（右边缘即滚动条的贴边位置），
/// `content_h` 是内容总高度，`max_scroll` 是可滚动距离。
pub fn scrollbar_geom(
    vr: RECT,
    scroll: i32,
    max_scroll: i32,
    content_h: i32,
    scale: f32,
) -> Option<ScrollBar> {
    if max_scroll <= 0 || content_h <= 0 {
        return None;
    }
    let track_h = vr.bottom - vr.top;
    if track_h <= 0 {
        return None;
    }
    let s = |v: i32| (v as f32 * scale).round() as i32;
    // 滑块高度按「可视高度 / 内容高度」的比例取，最短也要能看清、能点
    let thumb_h = (((track_h * track_h) as f32) / content_h as f32).round() as i32;
    let thumb_h = thumb_h.max(s(24)).min(track_h);
    let travel = (track_h - thumb_h).max(0);
    let ratio = (scroll as f32 / max_scroll as f32).clamp(0.0, 1.0);
    let y = vr.top + (travel as f32 * ratio).round() as i32;

    // 滑块的右边缘统一内缩 3dp，两种形态（细线 / 悬停长条）共用同一个右边缘，
    // 鼠标靠近时只是「变粗」，位置不会跳。
    let right = vr.right - s(3);
    let thumb = rect(right - s(6), y, right, y + thumb_h);
    let hit = rect(vr.right - s(14), vr.top, vr.right, vr.bottom);
    Some(ScrollBar {
        thumb,
        hit,
        track_top: vr.top,
        thumb_h,
        travel,
        max_scroll,
    })
}

/// 窗口按钮索引（供 create/命中共用）。
pub mod winbtn {
    pub const MENU: usize = 0;
    pub const MIN: usize = 1;
    pub const MAX: usize = 2;
    pub const CLOSE: usize = 3;
    pub const COUNT: usize = 4;
}
