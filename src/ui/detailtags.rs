//! 详情窗格里的「标签」模块。
//!
//! 标签是跨目录的，界面上的入口只有两处：导航窗格的标签分组（看全貌），
//! 以及这里（看**当前这一项**属于哪些标签，并就地增删）。
//!
//! 标签按 chip 网格排：宽度够时一行四个、放不下时一行两个往下延伸，
//! 与上方四个操作按钮共用同一套降级规则，整个窗格里所有网格观感一致。
//! 几何只算一遍，绘制与命中测试共用。

use std::path::PathBuf;

use windows_sys::Win32::Foundation::RECT;

use crate::app::Application;
use crate::ui::metrics;
use crate::win::paint::rect;

/// 一个「已归属的标签」chip。
pub struct TagChip {
    /// 标签在 `cfg.tags.list` 里的下标。
    pub tag: usize,
    /// 整个 chip 的矩形（悬停效果用；点它本身不做事）。
    pub chip: RECT,
    /// chip 右端的叉（点它把这一项从标签里移除）。
    pub close: RECT,
}

pub struct TagsLayout {
    /// 「标签」标题。
    pub label: RECT,
    /// 「＋ 添加」chip。
    pub add: RECT,
    pub chips: Vec<TagChip>,
    /// 本模块的下沿（含底部留白），下一个模块从这里往下排。
    pub bottom: i32,
}

/// 这个模块关注的是哪个路径。
///
/// 多选时按**最靠前**的那个选中项算，和上方预览框保持一致：同一个窗格里
/// 两处指向不同对象会让人反复怀疑自己点错了。
pub fn target_path(app: &Application) -> Option<PathBuf> {
    let tab = app.active()?;
    if let Some(&i) = tab.selected.iter().min() {
        if let Some(e) = tab.entries.get(i) {
            return Some(e.full.clone());
        }
    }
    // 没有选中项时，看的是当前这个文件夹本身
    if tab.is_computer || tab.is_recycle {
        return None;
    }
    Some(tab.path.clone())
}

/// 当前目标所属的标签下标。
pub fn assigned(app: &Application) -> Vec<usize> {
    match target_path(app) {
        Some(p) => app.cfg.tags.tags_of(&p),
        None => Vec::new(),
    }
}

/// 文件明细的行数（有选中项时；无选中项只有一行「未选择」）。
///
/// 排两个模块的上下顺序时按它预留高度：明细行数随选中项变化，
/// 若跟着重排，标签模块会在点选文件时上下跳动。
pub const INFO_ROWS: i32 = 6;

/// 从 `y0` 开始排标签模块。
///
/// `y0` 由 [`sections`] 决定——模块可以排在明细上方也可以排在下方。
pub fn compute(app: &Application, y0: i32) -> TagsLayout {
    let p = app.layout.detail_pane;
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let pad = s(12);
    let row_h = s(metrics::DETAIL_ROW_H);

    // 窗格矮到连标题行都放不下：整个模块收起来（矩形挪到窗格外），
    // 不然会把字画到下面的状态栏上
    if y0 + row_h > p.bottom {
        let gone = rect(p.right, p.bottom, p.right, p.bottom);
        return TagsLayout {
            label: gone,
            add: gone,
            chips: Vec::new(),
            bottom: y0,
        };
    }
    let label = rect(p.left + pad, y0, p.left + pad + s(64), y0 + row_h);

    // chip 网格：与上方四个按钮同一套「一行放不下四个就改两列」的规则
    let avail = (p.right - pad) - (p.left + pad);
    let chip_h = s(26);
    let gap = s(6);
    let one_row_w = (avail - gap * 3) / 4;
    let cols = if one_row_w < s(60) { 2 } else { 4 };
    let chip_w = if cols == 4 {
        one_row_w
    } else {
        (avail - gap) / 2
    }
    .max(s(44));

    let assigned = assigned(app);
    let total = assigned.len(); // 「＋ 添加」chip 排在所有标签之后
    let grid_y0 = y0 + row_h + s(4);
    let mut bottom_row = grid_y0;
    let mut chips = Vec::new();
    for (i, tag) in assigned.into_iter().enumerate() {
        let (col, row) = ((i % cols) as i32, (i / cols) as i32);
        let l = p.left + pad + col * (chip_w + gap);
        let t = grid_y0 + row * (chip_h + gap);
        // 窗格矮到放不下就停手：宁可少显示几个，也不让 chip 压到窗格外面
        if t + chip_h > p.bottom {
            break;
        }
        let close_w = s(18);
        let chip = rect(l, t, l + chip_w, t + chip_h);
        let close = rect(chip.right - close_w, t, chip.right, t + chip_h);
        chips.push(TagChip { tag, chip, close });
        bottom_row = bottom_row.max(t + chip_h);
    }

    // 「＋ 添加」chip：
    // - 有标签时：跟在最后一个标签后面，占网格的下一个格子；
    // - 没有标签时：单独一行、在整行里居中——第一行那格已经让给了
    //   「暂无标签」提示，再排同一行就会和文字叠在一起。
    let add = if chips.is_empty() && total == 0 {
        let add_w = s(64);
        let cx = p.left + pad + (avail - add_w) / 2;
        let t = grid_y0;
        if t + chip_h > p.bottom {
            rect(p.right, p.bottom, p.right, p.bottom) // 放不下：收到窗格外，不可点
        } else {
            bottom_row = bottom_row.max(t + chip_h);
            rect(cx, t, cx + add_w, t + chip_h)
        }
    } else {
        let i = total; // 紧跟在标签后面
        let (col, row) = ((i % cols) as i32, (i / cols) as i32);
        let l = p.left + pad + col * (chip_w + gap);
        let t = grid_y0 + row * (chip_h + gap);
        if t + chip_h > p.bottom {
            rect(p.right, p.bottom, p.right, p.bottom)
        } else {
            bottom_row = bottom_row.max(t + chip_h);
            rect(l, t, l + chip_w.min(s(64)), t + chip_h)
        }
    };

    TagsLayout {
        label,
        add,
        chips,
        bottom: bottom_row + s(10),
    }
}

/// 排布详情窗格的两个内容模块（标签 / 文件明细）。
///
/// 顺序由设置 `detail_tags_first` 决定，默认标签在明细下面。
/// 分隔线的纵坐标也一并给出：按钮区下沿一条、两个模块之间一条。
pub struct DetailSections {
    pub tags: TagsLayout,
    /// 文件明细第一行的 y。
    pub info_top: i32,
    /// 按钮区下方分隔线的 y。
    pub divider_buttons: i32,
    /// 两个模块之间分隔线的 y。
    pub divider_mid: i32,
}

pub fn sections(app: &Application) -> DetailSections {
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let row_h = s(metrics::DETAIL_ROW_H);
    let top = app.layout.detail_buttons_bottom() + s(14);
    let divider_buttons = top - s(8);

    if app.cfg.settings.detail_tags_first {
        let tags = compute(app, top);
        let info_top = tags.bottom + s(2);
        DetailSections {
            divider_mid: tags.bottom - s(7),
            tags,
            info_top,
            divider_buttons,
        }
    } else {
        let info_top = top;
        let tags = compute(app, top + INFO_ROWS * row_h + s(10));
        DetailSections {
            divider_mid: tags.label.top - s(7),
            tags,
            info_top,
            divider_buttons,
        }
    }
}
