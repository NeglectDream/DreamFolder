//! 导航窗格的树模型与行几何。
//!
//! 树**不缓存**：每次需要时按当前系统状态现算（盘符列表、已知文件夹路径）。
//! 插拔 U 盘、OneDrive 重定向变化都能立刻反映出来，不需要失效通知——
//! 代价只是每次重绘几十次 `SHGetKnownFolderPath`，而这些调用本身很便宜
//! （结果由 Shell 缓存在内存里，不落盘）。
//!
//! 行矩形只在这里算一次，绘制与命中测试共用，避免两处各写一套缩进规则
//! 而慢慢走偏——这正是先前 `nav_pane` 有矩形却没绘制的原因之一。
//!
//! 「快捷访问」区的内容来自用户配置（`cfg.nav_places`），可以增删、拖动排序；
//! 驱动器多到一屏放不下时整棵树纵向滚动，滚动量存在 `app.nav_scroll`。
//!
//! 分组的先后顺序也交给用户（`cfg.settings.nav_sections`）：每条分组上方的
//! 分隔线就是拖动把手，按住它上下拖即可换位。

use std::path::PathBuf;

use windows_sys::Win32::Foundation::RECT;

use crate::app::Application;
use crate::cfg::settings::NavSection;
use crate::fs::entry::Volume;
use crate::fs::shell::{self, KnownFolder};
use crate::strings as S;
use crate::ui::metrics;
use crate::win::paint::rect;

/// 树里一行的内容。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavKind {
    /// 分组之间（严格说是每个分组**上方**）的细分隔线。
    ///
    /// 它同时是拖动把手：按住上下拖，`section` 指的这组就跟着换位置。
    Separator { section: NavSection },
    /// 固定项：主文件夹（不属于用户可编辑的快捷访问区）。
    Known {
        name: String,
        path: PathBuf,
        /// 是否显示右侧图钉（快速访问固定项）。
        pinned: bool,
    },
    /// 快捷访问区的用户自定义项。`index` 是它在 `cfg.nav_places.items` 里的下标，
    /// 右键菜单与拖动排序都要用它回指配置。
    Place {
        index: usize,
        name: String,
        path: PathBuf,
    },
    /// 标签分组的标题行。`index` 指向 `cfg.tags.list`。
    Tag {
        index: usize,
        name: String,
        color: u32,
    },
    /// 标签里的一个条目。`tag` 是标签下标，`item` 是条目下标。
    TagItem {
        tag: usize,
        item: usize,
        name: String,
        path: PathBuf,
        color: u32,
    },
    /// 分组为空时的提示行，不可点击。
    ///
    /// `section` 说明它属于哪一组：右键要按组给出不同的动作
    /// （标签组的空提示上右键＝新建标签）。
    Hint { text: String, section: NavSection },
    /// 「此电脑」。点击切到驱动器列表视图。
    Computer,
    /// 驱动器，是「此电脑」的子项。
    Drive { path: PathBuf },
    /// 回收站（虚拟项，点击切到回收站视图）。
    RecycleBin,
    /// 「网络」。
    Network,
}

/// 树里一行。
///
/// 不派生 `Debug`：`RECT` 是 FFI 结构，没实现它，
/// 而为了打印去包一层类型不划算。
#[derive(Clone)]
pub struct NavRow {
    pub kind: NavKind,
    /// 行矩形（屏幕坐标，已含缩进与滚动偏移）。
    pub rect: RECT,
    /// 缩进层级，0 为顶层。
    pub depth: i32,
    /// 是否画展开箭头。
    pub expandable: bool,
    /// 箭头是否为展开态。
    pub expanded: bool,
    /// 驱动器容量；非驱动器为 `None`。
    pub volume: Option<Volume>,
}

/// 一个分组在屏幕上的纵向范围（含它上方的分隔线）。
///
/// 拖动分隔线换位时要知道「指针现在落在哪两组之间」，这份数据就是答案；
/// 绘制插入指示线时也用它算 y。
#[derive(Debug, Clone, Copy)]
pub struct GroupSpan {
    pub section: NavSection,
    pub top: i32,
    pub bottom: i32,
}

impl GroupSpan {
    /// 分组中线：指针越过它就认为「要插到这一组下面」。
    pub fn mid(&self) -> i32 {
        self.top + (self.bottom - self.top) / 2
    }
}

/// 建好的整棵树。
pub struct NavTree {
    pub rows: Vec<NavRow>,
    /// 各分组自上而下的顺序与屏幕范围。
    pub groups: Vec<GroupSpan>,
    /// 内容总高度（逻辑同上，含顶部留白）。
    pub content_h: i32,
    /// 可滚动余量；0 表示一屏放得下，不需要滚动。
    pub max_scroll: i32,
}

/// 逐行往下排的游标，省得在构建函数里到处写 `y += row_h`。
///
/// `y` 记的是**内容坐标**（相对内容顶端），滚动只影响写入矩形时的偏移，
/// 这样内容总高度不会随滚动量变化。
struct Cursor {
    items: Vec<NavRow>,
    groups: Vec<GroupSpan>,
    pane: RECT,
    /// 内容顶端（未滚动时的第一个行顶）。
    base: i32,
    scroll: i32,
    /// 行右边界（给滚动条让位后会更靠左）。
    right: i32,
    y: i32,
    row_h: i32,
    indent_unit: i32,
    pad: i32,
}

impl Cursor {
    /// 内容坐标 → 屏幕坐标。
    fn screen_y(&self, content_y: i32) -> i32 {
        self.base + content_y - self.scroll
    }

    fn push(&mut self, kind: NavKind, depth: i32, expandable: bool, expanded: bool) {
        self.push_with(kind, depth, expandable, expanded, 0);
    }

    /// 同 `push`，但这一行额外高 `extra_h`：驱动器行要在名称下面再放一条容量条。
    fn push_with(
        &mut self,
        kind: NavKind,
        depth: i32,
        expandable: bool,
        expanded: bool,
        extra_h: i32,
    ) {
        let indent = self.pad + depth * self.indent_unit;
        let h = self.row_h + extra_h;
        let top = self.screen_y(self.y);
        self.items.push(NavRow {
            kind,
            rect: rect(self.pane.left + indent, top, self.right, top + h),
            depth,
            expandable,
            expanded,
            volume: None,
        });
        self.y += h;
    }

    /// 分隔线：高度只有普通行的一半。
    ///
    /// 分组上方那条线比普通行略高一点——它是拖动把手，太细按不住。
    /// 矩形横向铺满窗格（不只从文字缩进处起），这样贴着左边缘也能抓住它。
    fn separator(&mut self, section: NavSection) {
        let h = (self.row_h * 3 / 4).max(1);
        let top = self.screen_y(self.y);
        self.items.push(NavRow {
            kind: NavKind::Separator { section },
            rect: rect(self.pane.left, top, self.right, top + h),
            depth: 0,
            expandable: false,
            expanded: false,
            volume: None,
        });
        self.y += h;
    }
}

/// 按当前系统状态构建整棵树，并算好每行的矩形。
///
/// 窗格未开启（`nav_pane` 为空矩形）时返回空表，调用方无需再判断开关。
pub fn build(app: &Application) -> NavTree {
    let pane = app.layout.nav_pane;
    if pane.right <= pane.left || pane.bottom <= pane.top {
        return NavTree {
            rows: Vec::new(),
            groups: Vec::new(),
            content_h: 0,
            max_scroll: 0,
        };
    }
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;

    // 第一遍不留滚动条的位置：先量出内容到底有多高，才知道要不要滚
    let first = assemble(app, 0, 0);
    let content_h = first.y;
    let pane_h = pane.bottom - pane.top;
    let max_scroll = (content_h + s(6) - pane_h).max(0);
    if max_scroll == 0 {
        return NavTree {
            rows: first.items,
            groups: first.groups,
            content_h,
            max_scroll: 0,
        };
    }

    // 要滚了：给右侧滚动条让出一条，免得文字压到滑块上
    let scroll = app.nav_scroll.clamp(0, max_scroll);
    let c = assemble(app, scroll, s(10));
    NavTree {
        rows: c.items,
        groups: c.groups,
        content_h,
        max_scroll,
    }
}

/// 单趟装配。`scroll` 为纵向偏移，`reserve_right` 为滚动条预留宽度。
///
/// 分组顺序完全由 `cfg.settings.nav_sections` 决定；每一组都先画自己那条
/// 分隔线再画内容，所以「拖分隔线」＝「拖它下面那一组」，指向明确。
fn assemble(app: &Application, scroll: i32, reserve_right: i32) -> Cursor {
    let pane = app.layout.nav_pane;
    let scale = app.layout.scale;
    let s = |v: i32| (v as f32 * scale).round() as i32;

    let mut c = Cursor {
        items: Vec::with_capacity(28),
        groups: Vec::with_capacity(NavSection::ALL.len()),
        pane,
        base: pane.top + s(6),
        scroll,
        right: pane.right - s(10) - reserve_right,
        y: 0,
        row_h: s(metrics::NAV_ROW_H),
        indent_unit: s(metrics::NAV_INDENT),
        pad: s(10),
    };

    for section in app.cfg.settings.nav_sections.iter().copied() {
        let top0 = c.y;
        c.separator(section);
        match section {
            NavSection::Home => assemble_home(&mut c),
            NavSection::Quick => assemble_quick(&mut c, app),
            NavSection::Tags => assemble_tags(&mut c, app),
            NavSection::Computer => assemble_computer(&mut c, app),
            NavSection::Network => assemble_network(&mut c, app),
        }
        c.groups.push(GroupSpan {
            section,
            top: c.screen_y(top0),
            bottom: c.screen_y(c.y),
        });
    }

    c
}

/// 主文件夹 + 回收站。资源管理器也是这个位置。
fn assemble_home(c: &mut Cursor) {
    if let Some(p) = shell::known_folder(KnownFolder::Profile) {
        c.push(
            NavKind::Known {
                name: S::NAV_HOME.to_string(),
                path: p,
                pinned: false,
            },
            0,
            false,
            false,
        );
    }
    c.push(NavKind::RecycleBin, 0, false, false);
}

/// 快捷访问区（用户可编辑）。
fn assemble_quick(c: &mut Cursor, app: &Application) {
    if app.cfg.nav_places.items.is_empty() {
        c.push(
            NavKind::Hint {
                text: S::NAV_QUICK_EMPTY.to_string(),
                section: NavSection::Quick,
            },
            0,
            false,
            false,
        );
        return;
    }
    for (index, place) in app.cfg.nav_places.items.iter().enumerate() {
        c.push(
            NavKind::Place {
                index,
                name: place.name.clone(),
                path: place.path.clone(),
            },
            0,
            false,
            false,
        );
    }
}

/// 标签区：每个标签是一个可展开的父项，展开后列出它收的条目。
fn assemble_tags(c: &mut Cursor, app: &Application) {
    if app.cfg.tags.is_empty() {
        c.push(
            NavKind::Hint {
                text: S::NAV_TAGS_EMPTY.to_string(),
                section: NavSection::Tags,
            },
            0,
            false,
            false,
        );
        return;
    }
    for (index, tag) in app.cfg.tags.list.iter().enumerate() {
        c.push(
            NavKind::Tag {
                index,
                name: tag.name.clone(),
                color: tag.color,
            },
            0,
            true,
            tag.expanded,
        );
        if !tag.expanded {
            continue;
        }
        for (item, entry) in tag.items.iter().enumerate() {
            c.push(
                NavKind::TagItem {
                    tag: index,
                    item,
                    name: entry.label(),
                    path: entry.path.clone(),
                    color: tag.color,
                },
                1,
                false,
                false,
            );
        }
    }
}

/// 「此电脑」（可展开，展开后列出各驱动器）。
fn assemble_computer(c: &mut Cursor, app: &Application) {
    let expanded = app.nav_expanded.computer;
    c.push(NavKind::Computer, 0, true, expanded);
    if !expanded {
        return;
    }
    // 名称一行 + 容量条一行，第二行靠 extra_h 撑出来。
    // 缩进和「此电脑」本身一致（不再多缩一级）：这几行本来就在
    // 「此电脑」正下方，再缩进一级既看不出层级，又把名字挤窄了
    let extra = (12.0 * app.layout.scale).round() as i32;
    for d in crate::fs::drives() {
        c.push_with(NavKind::Drive { path: d.clone() }, 0, false, false, extra);
        if let Some(last) = c.items.last_mut() {
            last.volume = crate::fs::enumerate::volume_of(&d);
        }
    }
}

fn assemble_network(c: &mut Cursor, app: &Application) {
    c.push(NavKind::Network, 0, true, app.nav_expanded.network);
}

/// 某一行的展开箭头矩形（绘制与命中测试共用）。
///
/// 箭头画在行左端固定位置、不随缩进移动，这样不同层级的箭头纵向对齐，
/// 和资源管理器一致。
pub fn arrow_rect(row: &NavRow, scale: f32) -> RECT {
    let s = |v: i32| (v as f32 * scale).round() as i32;
    let size = s(14);
    // 整体往右让一点：贴着窗格左边缘太挤，图标和文字都显得堵
    let x = row.rect.left + s(2);
    let y = row.rect.top + (row.rect.bottom - row.rect.top - size) / 2;
    rect(x, y, x + size, y + size)
}

/// 命中测试：返回鼠标落在第几行。
pub fn row_at(rows: &[NavRow], x: i32, y: i32) -> Option<usize> {
    rows.iter().position(|r| {
        x >= r.rect.left && x < r.rect.right && y >= r.rect.top && y < r.rect.bottom
    })
}

/// 拖动分隔线时，指针 `y` 处应该把 `drag` 这组插到第几个位置
/// （位置按「从顺序里摘掉 `drag` 之后」的列表计）。
///
/// 只跟**其它**分组的中线比：把自己也算进去的话，手指刚过自己那条中线
/// 就会跳到下一格，手感像在抽搐。
pub fn insertion_slot(groups: &[GroupSpan], drag: NavSection, y: i32) -> usize {
    let mut slot = 0;
    for g in groups {
        if g.section == drag {
            continue;
        }
        if y < g.mid() {
            return slot;
        }
        slot += 1;
    }
    slot
}

/// 按目标槽位重排分组，返回新的顺序。
///
/// `slot` 是「摘掉 `drag` 之后」的下标，所以先摘再插，不用修正偏移。
pub fn reorder(order: &[NavSection], drag: NavSection, slot: usize) -> Vec<NavSection> {
    let mut v: Vec<NavSection> = order.iter().copied().filter(|s| *s != drag).collect();
    let at = slot.min(v.len());
    v.insert(at, drag);
    v
}

/// 目标槽位对应的插入指示线 y（屏幕坐标）。
///
/// 落在第 `slot` 个（摘掉自己后的）分组上沿；越过最后一组则贴在最末一组下沿。
pub fn insertion_y(groups: &[GroupSpan], drag: NavSection, slot: usize) -> Option<i32> {
    let rest: Vec<&GroupSpan> = groups.iter().filter(|g| g.section != drag).collect();
    if rest.is_empty() {
        return None;
    }
    if slot < rest.len() {
        Some(rest[slot].top)
    } else {
        Some(rest[rest.len() - 1].bottom)
    }
}

/// 正在进行的分组拆分线拖动。
///
/// 记下按下时分组顺序的快照与当时的几何：拖动过程中顺序不变，
/// 所以几何也不会变，指示线位置可以直接由它算出来，不必每帧重排。
pub struct SepDrag {
    pub section: NavSection,
    /// 目标槽位（按摘掉本组后的列表计）。
    pub slot: usize,
}

impl SepDrag {
    /// 这次拖动会不会真的改变顺序。
    pub fn changed(&self, order: &[NavSection]) -> bool {
        !reorder(order, self.section, self.slot)
            .iter()
            .eq(order.iter())
    }
}
