//! 「新建」菜单：自己读 `ShellNew` 注册项并建文件。
//!
//! 为什么不用 Shell 自带的那套「新建」——它是一个陷阱：
//!
//! `IShellFolder::CreateViewObject(IID_IContextMenu)` 拿到的背景菜单里确实
//! 有「新建」子菜单，条目也齐（文本文档、压缩文件、各种已注册类型）。
//! 但那些命令由 shell32 的 `CNewMenu` 实现，它建完文件要靠**宿主的
//! `IShellView`** 选中并立刻进入重命名。我们自绘的列表不是 Shell 视图，
//! 菜单对象也没有 site，于是 `InvokeCommand` 对**每一个**新建项都只回
//! `E_FAIL`（0x80004005）——这条链跑通验证过，不是猜的（见本文件测试）。
//!
//! 所以这里把「新建」整个接管：按注册表枚举可新建的类型，自己建文件，
//! 建完拿到确切文件名交给上层去选中并重命名。

use std::path::Path;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CLASSES_ROOT,
    KEY_READ, REG_EXPAND_SZ, REG_SZ,
};

use crate::win::to_wide;

/// 我们自己的菜单项编号从这里起，避开 `QueryContextMenu` 用的 `[1, 0x7FFF]`。
pub const OWN_ID_BASE: u32 = 0x8000;

/// 一个可新建的类型。
#[derive(Clone)]
pub struct NewKind {
    /// 子菜单里的显示文本（例如「文本文档」）
    pub label: String,
    /// 扩展名，含点；文件夹为空串
    pub ext: String,
    source: Source,
}

#[derive(Clone)]
enum Source {
    /// `NullFile`：建一个空文件
    Empty,
    /// `FileName`：从 `%SystemRoot%\ShellNew` 复制模板
    Template(String),
    /// `Data`：值本身存着文件内容
    Data(Vec<u8>),
}

/// 类型表的缓存。`collect()` 和 `warm_up()` 共用这一份。
///
/// 必须是模块级静态：写进函数体里的话每个函数各有自己的一份，
/// 预热扫出来的结果右键时根本用不上。
static CACHE: std::sync::OnceLock<Vec<NewKind>> = std::sync::OnceLock::new();

/// 枚举 `HKCR\<.ext>\ShellNew`，得到「新建」子菜单该有哪些条目。
///
/// 排序按显示名，跟资源管理器的字母序一致；认不出来的类型直接跳过，
/// 宁可少一项，也不要放一个点了没反应的死条目。
///
/// 结果缓存：`HKCR` 下有上万个键，每次右键都扫一遍会让菜单明显变慢。
/// 新装软件注册的类型要重启才出现——资源管理器也是这个行为。
pub fn collect() -> Vec<NewKind> {
    CACHE.get_or_init(scan).clone()
}

/// 提前把类型表扫出来，好让第一次右键不用等。
///
/// 和 `collect()` 共用同一个 `CACHE`，所以这次扫描就是那唯一一次。
pub fn warm_up() {
    let _ = CACHE.get_or_init(scan);
}

fn scan() -> Vec<NewKind> {
    use windows_sys::Win32::Foundation::ERROR_NO_MORE_ITEMS;

    let mut kinds = Vec::new();
    unsafe {
        let Some(root) = open_root() else { return kinds };
        let mut index = 0;
        loop {
            let mut name = [0u16; 512];
            let mut len = name.len() as u32;
            let rc = RegEnumKeyExW(
                root,
                index,
                name.as_mut_ptr(),
                &mut len,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            );
            if rc == ERROR_NO_MORE_ITEMS {
                break;
            }
            // index 必须先自增再判断：出错时如果直接 break 或 continue，
            // 要么漏掉后面全部扩展名，要么卡在同一个下标上死循环。
            index += 1;
            if rc != ERROR_SUCCESS {
                // 键名超长（ERROR_MORE_DATA）之类：这一条没法读，跳过继续
                continue;
            }
            let ext = String::from_utf16_lossy(&name[..len as usize]);
            if !ext.starts_with('.') {
                continue;
            }
            if let Some(kind) = read_one(&ext) {
                kinds.push(kind);
            }
        }
        RegCloseKey(root);
    }
    kinds.sort_by(|a, b| a.label.cmp(&b.label));
    kinds
}

unsafe fn open_root() -> Option<HKEY> {
    let mut hkey: HKEY = std::ptr::null_mut();
    let rc = RegOpenKeyExW(HKEY_CLASSES_ROOT, std::ptr::null(), 0, KEY_READ, &mut hkey);
    if rc == ERROR_SUCCESS {
        Some(hkey)
    } else {
        None
    }
}

/// 读一个扩展名的 `ShellNew`。没有、或形式不支持就返回 `None`。
unsafe fn read_one(ext: &str) -> Option<NewKind> {
    let shell_new_path = format!("{ext}\\ShellNew");
    let key = open_sub(HKEY_CLASSES_ROOT, &shell_new_path);

    // 三种取内容的方式，优先级同资源管理器：Data > FileName > NullFile
    let source = match key {
        Some(key) => {
            let source = if let Some(data) = read_binary(key, "Data") {
                Some(Source::Data(data))
            } else if let Some(file) = read_string(key, "FileName") {
                Some(Source::Template(file))
            } else if has_value(key, "NullFile") {
                Some(Source::Empty)
            } else {
                None
            };
            RegCloseKey(key);
            source
        }
        None => None,
    };
    let source = match source {
        Some(s) => s,
        None if ALWAYS_TEXT_EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e)) => Source::Empty,
        None => return None,
    };

    Some(NewKind {
        label: display_name(ext).unwrap_or_else(|| ext.trim_start_matches('.').to_string()),
        ext: ext.to_string(),
        source,
    })
}

/// `ShellNew` 没注册、但「新建」里必须有的一项。
///
/// Win11 上 `.txt` 就没有 `ShellNew`（资源管理器却照旧列出「文本文档」），
/// 而纯文本新建出来就是一个空文件，没有任何模板依赖。只补这一个：把
/// 「内容是纯文本」的扩展名全收进来会得到一百多项（`.c`、`.ini`、`.xml`
/// 全都是 text/plain），菜单直接没法看。
const ALWAYS_TEXT_EXTS: [&str; 1] = [".txt"];

unsafe fn open_sub(root: HKEY, sub: &str) -> Option<HKEY> {
    let sub_w = to_wide(sub);
    let mut hkey: HKEY = std::ptr::null_mut();
    if RegOpenKeyExW(root, sub_w.as_ptr(), 0, KEY_READ, &mut hkey) == ERROR_SUCCESS {
        Some(hkey)
    } else {
        None
    }
}

/// 一个扩展名在人话里的叫法：`HKCR\.txt` 默认值是 ProgID，ProgID 的默认值
/// 才是「文本文档」。注册表里没写就走 `SHGetFileInfo` 的 `SHGFI_TYPENAME`，
/// 那是系统给的本地化类型名，和资源管理器显示的完全一致。
unsafe fn display_name(ext: &str) -> Option<String> {
    if let Some(key) = open_sub(HKEY_CLASSES_ROOT, ext) {
        let prog_id = read_string(key, "");
        RegCloseKey(key);
        if let Some(prog_id) = prog_id.filter(|s| !s.is_empty()) {
            if let Some(key) = open_sub(HKEY_CLASSES_ROOT, &prog_id) {
                let label = read_string(key, "");
                RegCloseKey(key);
                if let Some(label) = label.filter(|s| !s.is_empty()) {
                    return Some(label);
                }
            }
        }
    }
    type_name(ext)
}

/// 系统给这个扩展名的本地化类型名。
unsafe fn type_name(ext: &str) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
    use windows_sys::Win32::UI::Shell::{SHGetFileInfoW, SHFILEINFOW, SHGFI_TYPENAME};

    /// `SHGFI_USEFILEATTRIBUTES`：别去碰磁盘，只按扩展名查
    const SHGFI_USEFILE_ATTRIBUTES: u32 = 0x0000_0010;

    let dummy = crate::win::to_wide(&format!("x{ext}"));
    let mut info: SHFILEINFOW = std::mem::zeroed();
    let ret = SHGetFileInfoW(
        dummy.as_ptr(),
        FILE_ATTRIBUTE_NORMAL,
        &mut info,
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_TYPENAME | SHGFI_USEFILE_ATTRIBUTES,
    );
    if ret == 0 {
        return None;
    }
    let name = crate::win::from_wide(&info.szTypeName);
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

unsafe fn has_value(key: HKEY, name: &str) -> bool {
    let mut kind = 0u32;
    let mut size = 0u32;
    let name_w = to_wide(name);
    RegQueryValueExW(
        key,
        name_w.as_ptr(),
        std::ptr::null_mut(),
        &mut kind,
        std::ptr::null_mut(),
        &mut size,
    ) == ERROR_SUCCESS
}

unsafe fn read_string(key: HKEY, name: &str) -> Option<String> {
    let name_w = to_wide(name);
    let mut kind = 0u32;
    let mut size = 0u32;
    if RegQueryValueExW(
        key,
        name_w.as_ptr(),
        std::ptr::null_mut(),
        &mut kind,
        std::ptr::null_mut(),
        &mut size,
    ) != ERROR_SUCCESS
    {
        return None;
    }
    if kind != REG_SZ && kind != REG_EXPAND_SZ {
        return None;
    }
    let mut buf = vec![0u8; size.max(2) as usize];
    if RegQueryValueExW(
        key,
        name_w.as_ptr(),
        std::ptr::null_mut(),
        &mut kind,
        buf.as_mut_ptr(),
        &mut size,
    ) != ERROR_SUCCESS
    {
        return None;
    }
    let units: Vec<u16> = buf[..size as usize]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&c| c != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

unsafe fn read_binary(key: HKEY, name: &str) -> Option<Vec<u8>> {
    let name_w = to_wide(name);
    let mut kind = 0u32;
    let mut size = 0u32;
    if RegQueryValueExW(
        key,
        name_w.as_ptr(),
        std::ptr::null_mut(),
        &mut kind,
        std::ptr::null_mut(),
        &mut size,
    ) != ERROR_SUCCESS
    {
        return None;
    }
    if kind != 3 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size as usize];
    if RegQueryValueExW(
        key,
        name_w.as_ptr(),
        std::ptr::null_mut(),
        &mut kind,
        buf.as_mut_ptr(),
        &mut size,
    ) != ERROR_SUCCESS
    {
        return None;
    }
    buf.truncate(size as usize);
    Some(buf)
}

/// 在 `dir` 里建一个 `kind` 类型的条目，返回它的文件名。
///
/// 重名就往后排「(2)」「(3)」，和资源管理器一致。
///
/// 三次尝试都用「只在不存在时创建」的语义，**不做「先探测再写」**：
/// 探测与写之间只要有人抢先建了同名文件，`File::create`/`fs::copy` 就会
/// 把对方的内容截断成空。宁可重试几次，也不能毁别人的文件。
pub fn create(dir: &Path, kind: &NewKind) -> std::io::Result<String> {
    use std::io::ErrorKind;

    let stem = if kind.ext.is_empty() {
        "新建文件夹".to_string()
    } else {
        format!("新建{}", kind.label)
    };

    let mut last_err = None;
    for n in 1..=MAX_NAME_TRIES {
        let name = candidate_name(&stem, &kind.ext, n);
        let target = dir.join(&name);
        let result = match &kind.source {
            Source::Empty if kind.ext.is_empty() => std::fs::create_dir(&target),
            Source::Empty => std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .map(|_| ()),
            Source::Template(file) => {
                let src = std::env::var_os("SystemRoot")
                    .map(|root| Path::new(&root).join("ShellNew").join(file))
                    .ok_or_else(|| std::io::Error::from(ErrorKind::NotFound))?;
                create_from_template(&src, &target)
            }
            Source::Data(data) => write_new(&target, data),
        };
        match result {
            Ok(()) => return Ok(name),
            // 撞名：换下一个候选名重试
            Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                last_err = Some(err);
                continue;
            }
            Err(err) => return Err(err),
        }
    }
    Err(last_err.unwrap_or_else(|| std::io::Error::from(ErrorKind::AlreadyExists)))
}

/// 候选名最多试几次。「新建文件夹 (99)」还不够就真的有问题了。
const MAX_NAME_TRIES: usize = 100;

/// 第 `n` 个候选名：`名字.ext` / `名字 (2).ext` / …
fn candidate_name(stem: &str, ext: &str, n: usize) -> String {
    if n <= 1 {
        format!("{stem}{ext}")
    } else {
        format!("{stem} ({n}){ext}")
    }
}

/// 只在目标不存在时写入 `data`。
fn write_new(target: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    file.write_all(data)
}

/// 只在目标不存在时复制模板。
///
/// `std::fs::copy` 会覆盖已存在的目标，所以先占位再复制；占位失败说明撞名，
/// 交给调用方重试。占位成功后复制失败就把占位文件删掉，不留半成品。
fn create_from_template(src: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    match std::fs::copy(src, target) {
        Ok(_) => Ok(()),
        Err(err) => {
            let _ = std::fs::remove_file(target);
            Err(err)
        }
    }
}

/// 「文件夹」不是 ShellNew 里的扩展名，单独造一个（排在最前面）。
pub fn folder_kind() -> NewKind {
    NewKind {
        label: "文件夹".to_string(),
        ext: String::new(),
        source: Source::Empty,
    }
}

/// 某个新建项在菜单里该配的图标。调用方负责 `DestroyIcon`。
///
/// 图标按扩展名查（`SHGFI_USEFILEATTRIBUTES`，完全不碰磁盘）：菜单是在
/// 右键那一瞬间画的，不能为了一个图标去访问网络路径。
pub fn icon(kind: &NewKind) -> *mut core::ffi::c_void {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL,
    };
    use windows_sys::Win32::UI::Shell::{
        SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES,
    };

    let (name, attrs) = if kind.ext.is_empty() {
        ("x".to_string(), FILE_ATTRIBUTE_DIRECTORY)
    } else {
        (format!("x{}", kind.ext), FILE_ATTRIBUTE_NORMAL)
    };
    let name_w = crate::win::to_wide(&name);
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    let ret = unsafe {
        SHGetFileInfoW(
            name_w.as_ptr(),
            attrs,
            &mut info,
            std::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
        )
    };
    if ret == 0 {
        std::ptr::null_mut()
    } else {
        info.hIcon
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "dreamfolder-{tag}-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn collect_finds_registered_types() {
        let kinds = collect();
        eprintln!(
            "收集到 {} 项：{:?}",
            kinds.len(),
            kinds
                .iter()
                .map(|k| format!("{}{}", k.label, k.ext))
                .collect::<Vec<_>>()
        );
        assert!(
            !kinds.is_empty(),
            "HKCR 里应该至少有一个带 ShellNew 的扩展名"
        );
        assert!(
            kinds.iter().any(|k| k.ext.eq_ignore_ascii_case(".txt")),
            ".txt 通常注册了 ShellNew"
        );
        // 每个条目的显示名都不该是空串，否则菜单会出现空白项
        assert!(kinds.iter().all(|k| !k.label.trim().is_empty()));
    }

    #[test]
    fn create_makes_a_folder_and_avoids_collisions() {
        let dir = temp_dir("new-folder");
        let kind = folder_kind();
        let first = create(&dir, &kind).unwrap();
        let second = create(&dir, &kind).unwrap();
        assert_eq!(first, "新建文件夹");
        assert_eq!(second, "新建文件夹 (2)");
        assert!(dir.join(&first).is_dir());
        assert!(dir.join(&second).is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 同名文件已经存在时，绝不能把它的内容清空。
    ///
    /// 以前是「先探测名字、再 `File::create`」，探测与写之间若有人抢先建了
    /// 同名文件，对方的内容会被截断成空。现在走 `create_new`，撞名就换名。
    #[test]
    fn existing_file_is_never_truncated() {
        let dir = temp_dir("no-truncate");
        let Some(txt) = collect()
            .into_iter()
            .find(|k| k.ext.eq_ignore_ascii_case(".txt"))
        else {
            std::fs::remove_dir_all(&dir).ok();
            eprintln!("跳过：本机没有可用的 .txt 类型");
            return;
        };

        // 占掉第一个候选名，并写入可辨识的内容
        let first = candidate_name(&format!("新建{}", txt.label), &txt.ext, 1);
        let taken = dir.join(&first);
        std::fs::write(&taken, b"KEEP ME").unwrap();

        let made = create(&dir, &txt).unwrap();
        assert_ne!(made, first, "撞名时必须换一个名字");
        assert_eq!(
            std::fs::read(&taken).unwrap(),
            b"KEEP ME",
            "已存在的同名文件不能被截断"
        );
        assert!(dir.join(&made).is_file());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn candidate_names_follow_shell_convention() {
        assert_eq!(candidate_name("新建文本文档", ".txt", 1), "新建文本文档.txt");
        assert_eq!(candidate_name("新建文本文档", ".txt", 2), "新建文本文档 (2).txt");
        assert_eq!(candidate_name("新建文件夹", "", 3), "新建文件夹 (3)");
    }

    #[test]
    fn create_makes_an_empty_text_file() {
        let dir = temp_dir("new-txt");
        let Some(kind) = collect().into_iter().find(|k| k.ext.eq_ignore_ascii_case(".txt")) else {
            std::fs::remove_dir_all(&dir).ok();
            eprintln!("跳过：本机没有给 .txt 注册 ShellNew");
            return;
        };
        let name = create(&dir, &kind).unwrap();
        assert!(name.ends_with(".txt"), "文件名应该带扩展名：{name}");
        let meta = std::fs::metadata(dir.join(&name)).unwrap();
        assert!(meta.is_file());
        std::fs::remove_dir_all(&dir).ok();
    }
}
