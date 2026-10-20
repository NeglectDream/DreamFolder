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

    pub fn len(&self) -> usize {
        self.list.len()
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
        self.list.len() - 1
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.list.iter().position(|t| t.name == name)
    }

    pub fn remove_at(&mut self, index: usize) -> bool {
        if index < self.list.len() {
            self.list.remove(index);
            true
        } else {
            false
        }
    }

    pub fn rename_at(&mut self, index: usize, name: String) -> bool {
        match self.list.get_mut(index) {
            Some(t) => {
                t.name = name;
                true
            }
            None => false,
        }
    }

    pub fn set_color(&mut self, index: usize, color: u32) -> bool {
        match self.list.get_mut(index) {
            Some(t) => {
                t.color = color;
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
                true
            }
            None => false,
        }
    }

    pub fn remove_item_at(&mut self, index: usize, item: usize) -> bool {
        match self.list.get_mut(index) {
            Some(t) if item < t.items.len() => {
                t.items.remove(item);
                true
            }
            _ => false,
        }
    }

    /// 路径出现在哪些标签里（详情窗格据此列出「已归属的标签」）。
    pub fn tags_of(&self, path: &Path) -> Vec<usize> {
        self.list
            .iter()
            .enumerate()
            .filter(|(_, t)| t.items.iter().any(|i| crate::fs::same_path(&i.path, path)))
            .map(|(i, _)| i)
            .collect()
    }

    /// 把一条移出所有标签（删除文件后清理用）。
    pub fn purge_path(&mut self, path: &Path) {
        for t in &mut self.list {
            t.items.retain(|i| !crate::fs::same_path(&i.path, path));
        }
    }
}
