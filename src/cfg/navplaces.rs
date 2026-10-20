//! 导航窗格的「快捷访问」区。
//!
//! 这一块原本写死成桌面/下载/文档/图片/音乐/视频六个已知文件夹，
//! 用户既不能加自己常用的目录，也没法把不用的挪走。现在它是一份可编辑的
//! 列表：拖文件夹（或文件）进来就收藏，拖动改顺序，右键移除——
//! 和上方收藏栏是同一套心智模型，只是落在左侧导航窗格里。
//!
//! 存储用 `nav_places.json`。文件不存在时才播种默认项：用户把列表清空
//! 之后重启不该又冒出来（文件存在且 `items` 为空就是这个状态）。

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::fs::shell::{self, KnownFolder};
use crate::strings as S;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NavPlace {
    pub name: String,
    pub path: PathBuf,
}

impl NavPlace {
    pub fn new(path: PathBuf) -> NavPlace {
        let name = crate::cfg::bookmarks::display_name(&path);
        NavPlace { name, path }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NavPlaces {
    pub items: Vec<NavPlace>,
}

impl NavPlaces {
    fn file(root: &Path) -> PathBuf {
        root.join("nav_places.json")
    }

    /// 读配置。返回 `None` 表示文件还不存在（首次启动），调用方据此播种默认项。
    ///
    /// 解析失败也当作「没配过」：与其让一整块区域空着，不如回到默认的六个位置。
    pub fn load(root: &Path) -> Option<NavPlaces> {
        let bytes = std::fs::read(Self::file(root)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    pub fn save(&self, root: &Path) {
        if super::paths::ensure_dir(root).is_err() {
            return;
        }
        if let Ok(text) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::file(root), text);
        }
    }

    /// 首次启动的默认内容：与原先写死的六个已知文件夹一致。
    pub fn seeded() -> NavPlaces {
        let mut items = Vec::new();
        for (folder, label) in [
            (KnownFolder::Desktop, S::NAV_DESKTOP),
            (KnownFolder::Downloads, S::NAV_DOWNLOADS),
            (KnownFolder::Documents, S::NAV_DOCUMENTS),
            (KnownFolder::Pictures, S::NAV_PICTURES),
            (KnownFolder::Music, S::NAV_MUSIC),
            (KnownFolder::Videos, S::NAV_VIDEOS),
        ] {
            if let Some(p) = shell::known_folder(folder) {
                items.push(NavPlace {
                    name: label.to_string(),
                    path: p,
                });
            }
        }
        NavPlaces { items }
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.items
            .iter()
            .position(|p| crate::fs::same_path(&p.path, path))
    }

    /// 追加一项。已经在列表里的路径不会重复出现。
    ///
    /// 返回是否真的加了东西——调用方据此决定要不要重排、要不要提示。
    pub fn add(&mut self, path: PathBuf) -> bool {
        if self.index_of(&path).is_some() {
            return false;
        }
        self.items.push(NavPlace::new(path));
        true
    }

    /// 拖到末尾（或越界）时等同于追加。
    pub fn remove_at(&mut self, index: usize) -> bool {
        if index < self.items.len() {
            self.items.remove(index);
            true
        } else {
            false
        }
    }

    /// 把第 `from` 项挪到第 `to` 项之前（`to` 按移除后的序列理解）。
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from >= self.items.len() || from == to {
            return false;
        }
        let item = self.items.remove(from);
        let at = to.min(self.items.len());
        self.items.insert(at, item);
        true
    }

    /// 重命名第 `index` 项（只改显示名，不动路径）。
    pub fn rename_at(&mut self, index: usize, name: String) -> bool {
        match self.items.get_mut(index) {
            Some(p) => {
                p.name = name;
                true
            }
            None => false,
        }
    }

    /// 置顶：把第 `index` 项移到最前。
    pub fn pin_at(&mut self, index: usize) -> bool {
        if index >= self.items.len() {
            return false;
        }
        if index == 0 {
            return true;
        }
        let item = self.items.remove(index);
        self.items.insert(0, item);
        true
    }
}
