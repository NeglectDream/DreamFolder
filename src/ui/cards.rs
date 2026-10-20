//! 「此电脑」的驱动器卡片：几何计算。
//!
//! 卡片比普通网格单元高——图标、名称、容量进度条、容量文字四样东西
//! 挤在一行里，上面还要留一条分组标题。绘制和命中测试都从这里取矩形：
//! 两边各算一份，缩放一变就会错位。

use windows_sys::Win32::Foundation::RECT;

use crate::win::paint::rect;

/// 卡片宽（逻辑像素）。参照图约 227，取 240 让「本地磁盘 (C:)」少被截断。
const CARD_W: i32 = 240;
/// 卡片高：32 图标 + 名称 + 进度条 + 容量文字 + 内边距。
const CARD_H: i32 = 84;
/// 卡片之间的空隙。
const GAP: i32 = 8;
/// 分组标题高度。
const HEADER_H: i32 = 32;
/// 内容区左右内边距。
const PAD_X: i32 = 12;

/// 一次布局算出的网格参数。
///
/// 列数随容器宽度变化，所以每帧都要重算——但绘制和命中测试必须
/// 用同一份结果，因此两边都调用 `compute` 并传入相同的 `vr` 和 `scale`。
#[derive(Clone, Copy)]
pub struct Grid {
    /// 列数，至少 1。
    pub cols: usize,
    /// 单格宽（含空隙）。
    pub cell_w: i32,
    /// 单格高（含空隙）。
    pub cell_h: i32,
    /// 第一行卡片的顶边，已含分组标题。
    pub top: i32,
    /// 卡片区左边界。
    pub left: i32,
    /// 卡片区右边界。
    pub right: i32,
    gap: i32,
}

impl Grid {
    pub fn compute(vr: RECT, scale: f32) -> Self {
        let s = |v: i32| (v as f32 * scale).round() as i32;
        let gap = s(GAP);
        let left = vr.left + s(PAD_X);
        // 容器太窄时至少留一列，宁可让卡片被裁也不排成 0 列
        let right = (vr.right - s(PAD_X)).max(left + s(CARD_W));
        let cell_w = s(CARD_W) + gap;
        let cols = (((right - left) + gap) / cell_w).max(1) as usize;
        Self {
            cols,
            cell_w,
            cell_h: s(CARD_H) + gap,
            top: vr.top + s(HEADER_H),
            left,
            right,
            gap,
        }
    }

    /// 分组标题「设备和驱动器 (N)」的矩形。
    pub fn header_rect(&self, vr: RECT) -> RECT {
        rect(self.left, vr.top, self.right, self.top)
    }

    /// 第 `i` 张卡片的矩形。
    pub fn card_rect(&self, i: usize, scroll_y: i32) -> RECT {
        let col = (i % self.cols) as i32;
        let row = (i / self.cols) as i32;
        let l = self.left + col * self.cell_w;
        let t = self.top + row * self.cell_h - scroll_y;
        rect(l, t, l + self.cell_w - self.gap, t + self.cell_h - self.gap)
    }

    /// 内容总高度，用于算滚动范围。
    pub fn content_height(&self, count: usize) -> i32 {
        if count == 0 {
            return 0;
        }
        let rows = count.div_ceil(self.cols) as i32;
        self.top + rows * self.cell_h - self.gap
    }

    /// 坐标落在哪张卡片上。空隙里返回 `None`。
    pub fn card_at(&self, x: i32, y: i32, count: usize, scroll_y: i32) -> Option<usize> {
        if y < self.top || x < self.left || x >= self.right {
            return None;
        }
        let rel_y = y - self.top + scroll_y;
        if rel_y < 0 {
            return None;
        }
        let col_x = x - self.left;
        let col = (col_x / self.cell_w) as usize;
        if col >= self.cols {
            return None;
        }
        // 落在卡片之间的空隙里不算命中——否则点空白也会打开某个盘
        if col_x % self.cell_w > self.cell_w - self.gap {
            return None;
        }
        if rel_y % self.cell_h > self.cell_h - self.gap {
            return None;
        }
        let row = (rel_y / self.cell_h) as usize;
        let idx = row * self.cols + col;
        (idx < count).then_some(idx)
    }
}
