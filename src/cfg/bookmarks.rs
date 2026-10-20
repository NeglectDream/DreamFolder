//! 收藏书签栏。
//!
//! 对应原软件 `FavoritesPanel` 与 `favoriate_menu.xml` / `favoriate_folder_menu.xml`
//! / `favoriate_container_menu.xml` 三个菜单的全部操作：
//! 打开、打开所在位置、设为置顶、重命名、删除、添加书签目录。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub name: String,
    pub path: PathBuf,
    /// 「设为置顶」——置顶项排在最前。
    #[serde(default)]
    pub pinned: bool,
    /// true 表示这是一个「书签目录」（可容纳子书签），点击展开而非跳转。
    #[serde(default)]
    pub is_folder: bool,
    #[serde(default)]
    pub children: Vec<Bookmark>,
}

impl Bookmark {
    pub fn file(path: PathBuf) -> Self {
        let name = display_name(&path);
        Bookmark {
            name,
            path,
            pinned: false,
            is_folder: false,
            children: Vec::new(),
        }
    }

    pub fn folder(name: String) -> Self {
        Bookmark {
            name,
            path: PathBuf::new(),
            pinned: false,
            is_folder: true,
            children: Vec::new(),
        }
    }
}

/// 取路径末段作为显示名；根路径（如 `C:\`）保留全串。
pub fn display_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| p.to_string_lossy().into_owned())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Bookmarks {
    pub items: Vec<Bookmark>,
}

impl Bookmarks {
    fn file(root: &Path) -> PathBuf {
        root.join("favoriates.json")
    }

    pub fn load(root: &Path) -> Bookmarks {
        match std::fs::read(Self::file(root)) {
            Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
            Err(_) => Bookmarks::default(),
        }
    }

    pub fn save(&self, root: &Path) {
        if super::paths::ensure_dir(root).is_err() {
            return;
        }
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::file(root), text);
        }
    }

    /// 首次启动时生成一组常用位置（对应原软件引导页「可拖动…添加书签」的初始状态）。
    pub fn seed_defaults(&mut self) {
        if !self.items.is_empty() {
            return;
        }
        for p in default_places() {
            self.items.push(Bookmark::file(p));
        }
    }

    /// 添加或更新一个书签；已存在同路径项则只更新名称。
    pub fn add_or_update(&mut self, path: PathBuf, name: Option<String>) {
        if let Some(b) = self.items.iter_mut().find(|b| b.path == path) {
            if let Some(n) = name {
                b.name = n;
            }
            return;
        }
        let mut b = Bookmark::file(path);
        if let Some(n) = name {
            b.name = n;
        }
        self.items.push(b);
    }

    pub fn remove_at(&mut self, index: usize) {
        if index < self.items.len() {
            self.items.remove(index);
        }
    }

    /// 置顶：把该项移到最前，并打上 pinned 标记。
    pub fn pin_at(&mut self, index: usize) {
        if index >= self.items.len() {
            return;
        }
        let mut b = self.items.remove(index);
        b.pinned = true;
        self.items.insert(0, b);
    }

    pub fn rename_at(&mut self, index: usize, new_name: String) {
        if let Some(b) = self.items.get_mut(index) {
            b.name = new_name;
        }
    }

    /// 按「置顶优先，其后保持插入序」排序后的迭代顺序。
    pub fn ordered(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.items.len()).collect();
        idx.sort_by_key(|&i| !self.items[i].pinned);
        idx
    }
}

/// 默认书签：与原软件一致的常见位置。
fn default_places() -> Vec<PathBuf> {
    let mut v = Vec::new();
    // 以 SHGetKnownFolderPath 的常用等价物构造
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let p = PathBuf::from(&profile);
        v.push(p.join("Desktop"));
        v.push(p.join("Downloads"));
        v.push(p.join("Documents"));
        v.push(p.clone());
    }
    a_drive_if_exists(&mut v);
    v.retain(|p| p.is_dir());
    v
}

fn a_drive_if_exists(v: &mut Vec<PathBuf>) {
    let p = PathBuf::from("D:\\");
    if p.is_dir() {
        v.push(p);
    }
}
