//! 详情窗格里的「标签」模块。
//!
//! 标签是跨目录的，界面上的入口只有两处：导航窗格的标签分组（看全貌），
//! 以及这里（看**当前这一项**属于哪些标签，并就地增删）。
//!
//! 几何只算一遍，绘制与命中测试共用——右侧窗格很窄，标签行又要和下面的
//! 明细行对齐，两处各写一套很快就会错位。

use std::path::PathBuf;

use windows_sys::Win32::Foundation::RECT;

use crate::app::Application;
use crate::ui::metrics;
use crate::win::paint::rect;

/// 一行「已归属的标签」。
pub struct TagRow {
    /// 标签在 `cfg.tags.list` 里的下标。
    pub tag: usize,
    /// 整行（点它就把这一项从这个标签里移除）。
    pub row: RECT,
    /// 右端的叉。
    pub close: RECT,
}

pub struct TagsLayout {
    /// 「标签」两个字。
    pub label: RECT,
    /// 「添加」小按钮。
    pub add: RECT,
    pub rows: Vec<TagRow>,
    /// 本模块的下沿，明细行从这里往下排。
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

pub fn compute(app: &Application) -> TagsLayout {
    let p = app.layout.detail_pane;
    let s = |v: i32| (v as f32 * app.layout.scale).round() as i32;
    let pad = s(12);
    // 与下方明细行共用同一套列宽，标签名和明细值才会左对齐成一列
    let label_w = s(64);
    let row_h = s(metrics::DETAIL_ROW_H);

    let y0 = app.layout.detail_buttons_bottom() + s(14);
    let add_w = s(52);
    let label = rect(p.left + pad, y0, p.left + pad + label_w, y0 + row_h);
    let add = rect(p.right - pad - add_w, y0, p.right - pad, y0 + row_h);

    let value_left = p.left + pad + label_w + s(6);
    let mut rows = Vec::new();
    let mut y = y0 + row_h;
    for tag in assigned(app) {
        // 窗格矮到放不下就停手：宁可少显示几行，也不让标签压到窗格外面
        if y + row_h > p.bottom {
            break;
        }
        let close_w = s(18);
        let close = rect(p.right - pad - close_w, y, p.right - pad, y + row_h);
        // 这一行的可点区域只到叉的左边：点名字不该等于删除
        let row = rect(value_left, y, close.left - s(4), y + row_h);
        rows.push(TagRow { tag, row, close });
        y += row_h;
    }
    // 一个标签都没有时也要占一行，好让人看见「尚未添加标签」这句提示
    if rows.is_empty() {
        y += row_h;
    }
    TagsLayout {
        label,
        add,
        rows,
        bottom: y + s(10),
    }
}
