//! 收藏栏的分栏几何：哪些图标放得下、被折叠的是哪些、末尾的展开箭头在哪。
//!
//! 绘制和命中测试都从这里取结果——以前两边各自按「字符数 × 8 + 28」
//! 算一遍宽度，任何一处改了就会错位。

use windows_sys::Win32::Foundation::RECT;

use crate::app::Application;
use crate::win::paint::rect;

/// 一个放得下的收藏项。
pub struct FavSlot {
    /// 在 `bookmarks.items` 里的下标。
    pub index: usize,
    pub rect: RECT,
}

pub struct FavLayout {
    pub slots: Vec<FavSlot>,
    /// 因为放不下被折叠的项，按显示顺序。
    pub hidden: Vec<usize>,
    /// 末尾展开箭头；不溢出时是空矩形。
    pub overflow: RECT,
}

impl FavLayout {
    pub fn has_overflow(&self) -> bool {
        self.overflow.right > self.overflow.left
    }

    pub fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        self.slots
            .iter()
            .find(|s| {
                x >= s.rect.left && x < s.rect.right && y >= s.rect.top && y < s.rect.bottom
            })
            .map(|s| s.index)
    }

    pub fn overflow_at(&self, x: i32, y: i32) -> bool {
        self.has_overflow()
            && x >= self.overflow.left
            && x < self.overflow.right
            && y >= self.overflow.top
            && y < self.overflow.bottom
    }
}

/// 算出当前窗口宽度下的收藏栏分栏。
pub fn compute(app: &Application) -> FavLayout {
    let l = &app.layout;
    let fb = l.favbar;
    let mut out = FavLayout {
        slots: Vec::new(),
        hidden: Vec::new(),
        overflow: rect(0, 0, 0, 0),
    };
    if fb.right <= fb.left {
        return out;
    }

    let s = |v: i32| (v as f32 * l.scale).round() as i32;
    let order = app.cfg.bookmarks.ordered();
    let top = fb.top + s(3);
    let bottom = fb.bottom - s(3);
    let mut x = fb.left + s(8);

    for (pos, &idx) in order.iter().enumerate() {
        let item = &app.cfg.bookmarks.items[idx];
        let w = (item.name.chars().count() as i32 * s(8) + s(28)).clamp(s(48), s(160));
        // 后面还有项时要给展开箭头留位置，否则永远不溢出
        let has_more = pos + 1 < order.len();
        let limit = fb.right - s(8) - if has_more { s(26) } else { 0 };
        if x + w > limit {
            out.hidden.extend_from_slice(&order[pos..]);
            break;
        }
        out.slots.push(FavSlot {
            index: idx,
            rect: rect(x, top, x + w, bottom),
        });
        x += w + s(2);
    }

    if !out.hidden.is_empty() {
        let right = (x + s(22)).min(fb.right - s(4));
        out.overflow = rect(x, top, right.max(x + s(14)), bottom);
    }
    out
}
