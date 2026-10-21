//! 用户自定义的颜色标签。
//!
//! 标签是**跨目录**的组织方式：同一次梳理常常要横跨好几个盘，把它们各自的
//! 路径挂到同一个标签下，比复制到一个「归档」文件夹更贴近实际用法。
//!
//! 一份标签 = 名字 + 颜色 + 一串条目（每个条目就是一个绝对路径，可另起别名）。
//! 存储用 `tags.json`；导航窗格的「标签」分组与详情窗格的标签模块共用这份数据。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 标签里的一条：一个绝对路径，外加可选的自定义显示名。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagItem {
    pub path: PathBuf,
    /// 空表示用路径本身的文件名。
    #[serde(default)]
    pub name: String,
}

impl TagItem {
    fn new(path: PathBuf) -> TagItem {
        TagItem {
            name: String::new(),
            path,
        }
    }

    /// 显示名：优先用户起的别名。
    pub fn label(&self) -> String {
        if self.name.trim().is_empty() {
            crate::cfg::bookmarks::display_name(&self.path)
        } else {
            self.name.clone()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tag {
    pub name: String,
    /// `0xRRGGBB`。存整数而不是 "#RRGGBB" 字符串，省得解析。
    pub color: u32,
    /// 在导航窗格里是否展开。
    #[serde(default)]
    pub expanded: bool,
    #[serde(default)]
    pub items: Vec<TagItem>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tags {
    pub list: Vec<Tag>,
    /// 归属查询缓存：`(版本, 路径, 结果)`。`tags_of` 是「标签数 × 每标签
    /// 条目数」的线性扫，详情窗格每帧绘制都要它，条目多时纯属性能税。
    /// 版本号在任何修改时自增（见 `touch`），缓存随之整批失效。
    #[serde(skip)]
    cache: std::cell::RefCell<Option<(u64, PathBuf, Vec<usize>)>>,
    #[serde(skip)]
    version: std::cell::Cell<u64>,
}

/// 新建标签时可选的预设颜色。
///
/// 取的是 Windows 强调色系附近的一组中饱和度色：既能区分，又不刺眼，
/// 深浅两套主题下都能看清名字。
pub const PALETTE: [u32; 8] = [
    0xC8_4B_4B, // 红
    0xD9_7A_2B, // 橙
    0xC9_A9_27, // 黄
    0x4C_A1_4E, // 绿
    0x2E_8F_A8, // 青
    0x3D_72_C4, // 蓝
    0x7A_5B_C0, // 紫
    0xB0_50_93, // 品红
];

impl Tags {
    fn file(root: &Path) -> PathBuf {
        root.join("tags.json")
    }

    pub fn load(root: &Path) -> Tags {
        std::fs::read(Self::file(root))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, root: &Path) {
        if super::paths::ensure_dir(root).is_err() {
            return;
        }
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::file(root), text);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// 新建一个标签，返回它的下标。同名标签已存在时返回已有的那个。
    pub fn add(&mut self, name: String, color: u32) -> usize {
        if let Some(i) = self.index_of(&name) {
            return i;
        }
        self.list.push(Tag {
            name,
            color,
            expanded: true,
            items: Vec::new(),
        });
        self.touch();
        self.list.len() - 1
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.list.iter().position(|t| t.name == name)
    }

    pub fn remove_at(&mut self, index: usize) -> bool {
        if index < self.list.len() {
            self.list.remove(index);
            self.touch();
            true
        } else {
            false
        }
    }

    pub fn rename_at(&mut self, index: usize, name: String) -> bool {
        match self.list.get_mut(index) {
            Some(t) => {
                t.name = name;
                self.touch();
                true
            }
            None => false,
        }
    }

    /// 这个标签里是否已经有该路径。
    pub fn has_item(&self, index: usize, path: &Path) -> bool {
        self.list
            .get(index)
            .map(|t| t.items.iter().any(|i| crate::fs::same_path(&i.path, path)))
            .unwrap_or(false)
    }

    /// 往标签里加一条。已经存在则不重复添加。
    pub fn add_item(&mut self, index: usize, path: PathBuf) -> bool {
        if self.has_item(index, &path) {
            return false;
        }
        match self.list.get_mut(index) {
            Some(t) => {
                t.items.push(TagItem::new(path));
                self.touch();
                true
            }
            None => false,
        }
    }

    pub fn remove_item_at(&mut self, index: usize, item: usize) -> bool {
        match self.list.get_mut(index) {
            Some(t) if item < t.items.len() => {
                t.items.remove(item);
                self.touch();
                true
            }
            _ => false,
        }
    }

    /// 按路径删掉标签里的一条归属（详情窗格里点 × 解除归类）。
    pub fn remove_item_by_path(&mut self, index: usize, path: &Path) -> bool {
        let Some(t) = self.list.get_mut(index) else {
            return false;
        };
        let before = t.items.len();
        t.items.retain(|i| !crate::fs::same_path(&i.path, path));
        let removed = t.items.len() != before;
        if removed {
            self.touch();
        }
        removed
    }

    /// 展开/收起导航窗格里这个标签的条目（不影响归属，缓存一并失效
    /// 只是多扫一次的代价，换来的是规则简单：任何修改都 bump）。
    pub fn toggle_expanded(&mut self, index: usize) {
        if let Some(t) = self.list.get_mut(index) {
            t.expanded = !t.expanded;
            self.touch();
        }
    }

    /// 展开某个标签（已展开则不变）。
    ///
    /// 拖文件到标签行上时用它：加完不展开，界面上没有任何变化，
    /// 用户会以为没加上又拖一次。
    pub fn expand(&mut self, index: usize) {
        if let Some(t) = self.list.get_mut(index) {
            if !t.expanded {
                t.expanded = true;
                self.touch();
            }
        }
    }

    /// 路径出现在哪些标签里（详情窗格据此列出「已归属的标签」）。
    ///
    /// 结果按 `(版本, 路径)` 缓存：绘制与命中测试每帧各查一次，
    /// 选中项不变时两次都是同一答案，没必要扫两遍标签列表。
    pub fn tags_of(&self, path: &Path) -> Vec<usize> {
        {
            let c = self.cache.borrow();
            if let Some((v, p, r)) = c.as_ref() {
                if *v == self.version.get() && p == path {
                    return r.clone();
                }
            }
        }
        let r: Vec<usize> = self
            .list
            .iter()
            .enumerate()
            .filter(|(_, t)| t.items.iter().any(|i| crate::fs::same_path(&i.path, path)))
            .map(|(i, _)| i)
            .collect();
        *self.cache.borrow_mut() = Some((self.version.get(), path.to_path_buf(), r.clone()));
        r
    }

    /// 任何修改后调用：版本号 +1，`tags_of` 的缓存随之失效。
    fn touch(&self) {
        self.version.set(self.version.get() + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    /// 归属查询命中缓存后结果一致；任何修改（版本号变化）后缓存失效，
    /// 解除归属立刻能被 `tags_of` 看见。
    #[test]
    fn tags_of_cache_invalidates_on_mutation() {
        let mut t = Tags::default();
        let work = t.add("工作".into(), 0x3D_72_C4);
        let life = t.add("生活".into(), 0x4C_A1_4E);
        assert!(t.add_item(work, p("C:/Docs/a.txt")));
        assert!(t.add_item(life, p("C:/Photos/b.jpg")));

        // 命中缓存的路径与未缓存的路径结果相同
        assert_eq!(t.tags_of(&p("C:/Docs/a.txt")), [work]);
        assert_eq!(t.tags_of(&p("C:/Docs/a.txt")), [work]);

        // 解除归属（版本号 bump）后缓存必须失效
        assert!(t.remove_item_by_path(work, &p("C:/Docs/a.txt")));
        assert!(t.tags_of(&p("C:/Docs/a.txt")).is_empty());

        // 标签整个删掉也走版本号失效
        assert!(t.remove_at(life));
        assert!(t.tags_of(&p("C:/Photos/b.jpg")).is_empty());
    }

    /// 按路径解除不存在的归属时返回 false，也不误伤缓存。
    #[test]
    fn remove_item_by_path_miss_is_false() {
        let mut t = Tags::default();
        let i = t.add("工作".into(), 0x3D_72_C4);
        assert!(t.add_item(i, p("C:/Docs/a.txt")));
        assert_eq!(t.tags_of(&p("C:/Docs/a.txt")), [i]);
        assert!(!t.remove_item_by_path(i, &p("C:/none.txt")));
        assert_eq!(t.tags_of(&p("C:/Docs/a.txt")), [i]);
        assert!(!t.remove_item_by_path(99, &p("C:/Docs/a.txt")));
    }
}
