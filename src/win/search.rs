//! Windows 搜索索引查询（OLEDB `Search.CollatorDSO` 提供程序，纯 FFI）。
//!
//! `windows-sys` 不导出 OLEDB 绑定，这里手写最小 vtable 访问。
//! 只做一件事：给定目录作用域与关键字，返回索引中匹配项的完整路径。
//!
//! 调用方负责保证 COM 已初始化（本进程在启动时调用了 `OleInitialize`）。

use std::ffi::c_void;

use windows_sys::core::GUID;
use windows_sys::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

type Hr = i32;
const S_OK: Hr = 0;

// ---------------------------------------------------------------- GUID 常量

const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> GUID {
    GUID {
        data1: d1,
        data2: d2,
        data3: d3,
        data4: d4,
    }
}

/// Windows 搜索 OLEDB 提供程序的类标识（`Search.CollatorDSO` 实际注册值）。
///
/// 注意：`9E175B68-...` 是历史遗留的抽象类，注册表里没有 `InprocServer32`，
/// `CoCreateInstance` 会返回 `REGDB_E_CLASSNOTREG`。真正可用的是下面这个。
const CLSID_COLLATOR_DSO: GUID = guid(
    0x9E17_5B8B,
    0xF52A,
    0x11D8,
    [0xB9, 0xA5, 0x50, 0x50, 0x54, 0x50, 0x30, 0x30],
);

const IID_IDB_INITIALIZE: GUID = guid(
    0x0C73_3A8B,
    0x2A1C,
    0x11CE,
    [0xAD, 0xE5, 0x00, 0xAA, 0x00, 0x44, 0x77, 0x3D],
);
const IID_IDB_CREATE_SESSION: GUID = guid(
    0x0C73_3A5D,
    0x2A1C,
    0x11CE,
    [0xAD, 0xE5, 0x00, 0xAA, 0x00, 0x44, 0x77, 0x3D],
);
const IID_IDB_CREATE_COMMAND: GUID = guid(
    0x0C73_3A1D,
    0x2A1C,
    0x11CE,
    [0xAD, 0xE5, 0x00, 0xAA, 0x00, 0x44, 0x77, 0x3D],
);
const IID_I_COMMAND_TEXT: GUID = guid(
    0x0C73_3A27,
    0x2A1C,
    0x11CE,
    [0xAD, 0xE5, 0x00, 0xAA, 0x00, 0x44, 0x77, 0x3D],
);
const IID_I_COMMAND: GUID = guid(
    0x0C73_3A63,
    0x2A1C,
    0x11CE,
    [0xAD, 0xE5, 0x00, 0xAA, 0x00, 0x44, 0x77, 0x3D],
);
const IID_I_ROWSET: GUID = guid(
    0x0C73_3A7C,
    0x2A1C,
    0x11CE,
    [0xAD, 0xE5, 0x00, 0xAA, 0x00, 0x44, 0x77, 0x3D],
);
const IID_I_ACCESSOR: GUID = guid(
    0x0C73_3A8C,
    0x2A1C,
    0x11CE,
    [0xAD, 0xE5, 0x00, 0xAA, 0x00, 0x44, 0x77, 0x3D],
);

/// SQL 方言标识（`DBGUID_DEFAULT`）。
const DBGUID_DEFAULT: GUID = guid(
    0xC8B5_21FB,
    0x5CF3,
    0x11CE,
    [0xAD, 0xE5, 0x00, 0xAA, 0x00, 0x44, 0x77, 0x3D],
);

// ---------------------------------------------------------------- OLEDB 常量

const DBTYPE_WSTR: u16 = 130;
const DBPART_VALUE: u32 = 0x1;
const DBPART_LENGTH: u32 = 0x2;
const DBPART_STATUS: u32 = 0x4;
const DBMEMOWNER_CLIENTOWNED: u32 = 0;
const DBSTATUS_S_OK: u32 = 0;

// ---------------------------------------------------------------- 结构布局

/// `DBBINDING`（x64 下 88 字节）。字段顺序与 `oledb.h` 完全一致。
#[repr(C)]
struct DbBinding {
    ordinal: usize,
    ob_value: usize,
    ob_length: usize,
    ob_status: usize,
    type_info: *mut c_void,
    object: *mut c_void,
    bind_ext: *mut c_void,
    part: u32,
    mem_owner: u32,
    param_io: u32,
    cb_max_len: usize,
    flags: u32,
    wtype: u16,
    precision: u8,
    scale: u8,
}

// ---------------------------------------------------------------- vtable 访问

/// 取 vtable 第 `idx` 个槽位并转成具体函数指针类型。
///
/// # Safety
/// `p` 必须指向一个仍然存活的 COM 对象，且槽位签名必须与 `T` 一致。
unsafe fn slot<T: Copy>(p: *mut c_void, idx: usize) -> T {
    let vtbl = *(p as *mut *const *const c_void);
    let fp = *vtbl.add(idx);
    std::mem::transmute_copy(&fp)
}

/// # Safety
/// `p` 必须指向存活 COM 对象。
unsafe fn release(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    let f: unsafe extern "system" fn(*mut c_void) -> u32 = slot(p, 2);
    f(p);
}

/// # Safety
/// `p` 必须指向存活 COM 对象。
unsafe fn query_interface(p: *mut c_void, iid: &GUID) -> *mut c_void {
    let f: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> Hr =
        slot(p, 0);
    let mut out: *mut c_void = std::ptr::null_mut();
    if f(p, iid, &mut out) == S_OK {
        out
    } else {
        std::ptr::null_mut()
    }
}

// ---------------------------------------------------------------- 公共接口

/// 一条命中结果。
pub struct Hit {
    /// 完整路径（`System.ItemPathDisplay`）。
    pub path: String,
}

/// 在 `scope`（本地目录，如 `C:\Users\me`）范围内按关键字查询 Windows 搜索索引。
///
/// `key` 走 `System.FileName LIKE '%key%'`，与资源管理器搜索框语义一致。
/// 索引未启用、目录未被索引、或 OLEDB 不可用时返回 `Err`，调用方应自行兜底。
pub fn search_index(scope: &str, key: &str, limit: usize) -> Result<Vec<Hit>, String> {
    let key = key.trim();
    if key.is_empty() || scope.is_empty() {
        return Ok(Vec::new());
    }
    match search_once(scope, key, limit.max(1)) {
        Ok(hits) => Ok(hits),
        Err(err) => {
            // 复用的数据源可能已经失效（索引服务重启过、COM 被卸载）：
            // 丢掉缓存重建一次再试。最坏情况与「每次重建连接」等价，
            // 所以这个降级不会比原来更差。
            drop_source();
            search_once(scope, key, limit.max(1)).map_err(|_| err)
        }
    }
}

thread_local! {
    /// 复用的搜索数据源。
    ///
    /// 搜索跑在后台 IO 线程上（`io::Job::Search`），线程是固定的一个，
    /// 所以 `Search.CollatorDSO` 的数据源可以留着复用：以前每次搜索都要
    /// `CoCreateInstance` + `Initialize` 走一遍，索引服务忙时要几十毫秒。
    static SOURCE: std::cell::RefCell<Option<SourceHandle>> =
        const { std::cell::RefCell::new(None) };
}

/// 数据源句柄的 RAII 包装：线程退出时释放。
struct SourceHandle(*mut c_void);

impl Drop for SourceHandle {
    fn drop(&mut self) {
        unsafe { release(self.0) };
    }
}

fn drop_source() {
    SOURCE.with(|cell| *cell.borrow_mut() = None);
}

/// 用（必要时新建的）数据源跑一次查询。
fn search_once(scope: &str, key: &str, limit: usize) -> Result<Vec<Hit>, String> {
    SOURCE.with(|cell| {
        let mut guard = cell.borrow_mut();
        if guard.is_none() {
            let init = unsafe {
                let mut init: *mut c_void = std::ptr::null_mut();
                let hr = CoCreateInstance(
                    &CLSID_COLLATOR_DSO,
                    std::ptr::null_mut(),
                    CLSCTX_INPROC_SERVER,
                    &IID_IDB_INITIALIZE,
                    &mut init,
                );
                if hr != S_OK || init.is_null() {
                    return Err(format!("无法创建搜索数据源 (0x{:08X})", hr as u32));
                }
                // IDBInitialize::Initialize() —— 每个数据源只做一次
                let f_init: unsafe extern "system" fn(*mut c_void) -> Hr = slot(init, 3);
                if f_init(init) != S_OK {
                    release(init);
                    return Err("搜索数据源初始化失败".into());
                }
                init
            };
            *guard = Some(SourceHandle(init));
        }
        let init = guard.as_ref().map(|s| s.0).unwrap_or(std::ptr::null_mut());
        unsafe { search_with(init, scope, key, limit) }
    })
}

/// 在已初始化的数据源上跑一次查询：建会话 → 建命令 → 执行 → 读行集。
unsafe fn search_with(
    init: *mut c_void,
    scope: &str,
    key: &str,
    limit: usize,
) -> Result<Vec<Hit>, String> {
    // ---- 2) 建会话 → 建命令
    let session = query_interface(init, &IID_IDB_CREATE_SESSION);
    if session.is_null() {
        return Err("搜索数据源不支持 IDBCreateSession".into());
    }
    let mut cmd_create: *mut c_void = std::ptr::null_mut();
    let f: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> Hr =
        slot(session, 3);
    let hr = f(session, &IID_IDB_CREATE_COMMAND, &mut cmd_create);
    release(session);
    if hr != S_OK || cmd_create.is_null() {
        return Err(format!("搜索会话创建失败 (0x{:08X})", hr as u32));
    }

    let mut cmd_text: *mut c_void = std::ptr::null_mut();
    let f: unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> Hr =
        slot(cmd_create, 3);
    let hr = f(cmd_create, &IID_I_COMMAND_TEXT, &mut cmd_text);
    release(cmd_create);
    if hr != S_OK || cmd_text.is_null() {
        return Err(format!("搜索命令创建失败 (0x{:08X})", hr as u32));
    }

    // ---- 3) 写 SQL
    let sql = build_sql(scope, key, limit);
    let sql_w: Vec<u16> = sql.encode_utf16().chain(std::iter::once(0)).collect();
    let f: unsafe extern "system" fn(*mut c_void, *const GUID, *const u16) -> Hr =
        slot(cmd_text, 4);
    let hr = f(cmd_text, &DBGUID_DEFAULT, sql_w.as_ptr());
    if hr != S_OK {
        release(cmd_text);
        return Err(format!("搜索语句被拒绝 (0x{:08X})", hr as u32));
    }

    // ---- 4) 执行，取行集
    let command = query_interface(cmd_text, &IID_I_COMMAND);
    release(cmd_text);
    if command.is_null() {
        return Err("搜索命令不支持 ICommand".into());
    }

    let mut rowset: *mut c_void = std::ptr::null_mut();
    let f: unsafe extern "system" fn(
        *mut c_void,
        *mut c_void,
        *const GUID,
        *mut c_void,
        *mut i32,
        *mut *mut c_void,
    ) -> Hr = slot(command, 4);
    let hr = f(
        command,
        std::ptr::null_mut(),
        &IID_I_ROWSET,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        &mut rowset,
    );
    release(command);
    if hr != S_OK || rowset.is_null() {
        return Err(format!("搜索执行失败 (0x{:08X})", hr as u32));
    }

    let rows = read_rows(rowset, limit);
    release(rowset);
    rows
}

/// 拼 SQL。作用域用 `file:` URL，关键字按 SQL 单引号规则转义（`'` → `''`）。
fn build_sql(scope: &str, key: &str, limit: usize) -> String {
    let scope = scope.trim_end_matches('\\');
    let scope = if scope.is_empty() { "C:" } else { scope };
    let escaped_key = key.replace('\'', "''");
    let escaped_scope = scope.replace('\'', "''");
    let like = format!("%{}%", escaped_key);
    let like = like.replace('\'', "''");
    format!(
        "SELECT TOP {} System.ItemPathDisplay FROM SystemIndex \
         WHERE SCOPE='file:{}' AND System.FileName LIKE '{}'",
        limit, escaped_scope, like
    )
}

/// 读取行集：单列 `DBTYPE_WSTR`，逐行 `GetData`。
unsafe fn read_rows(rowset: *mut c_void, limit: usize) -> Result<Vec<Hit>, String> {
    let accessor_if = query_interface(rowset, &IID_I_ACCESSOR);
    if accessor_if.is_null() {
        return Err("行集不支持 IAccessor".into());
    }

    // 每行 16384 个 UTF-16 码元足够容纳任何路径；超长则被标记为 TRUNCATED。
    const VALUE_BYTES: usize = 32 * 1024;
    let binding = DbBinding {
        ordinal: 1,
        ob_value: 0,
        ob_length: VALUE_BYTES,
        ob_status: VALUE_BYTES + std::mem::size_of::<u32>(),
        type_info: std::ptr::null_mut(),
        object: std::ptr::null_mut(),
        bind_ext: std::ptr::null_mut(),
        part: DBPART_VALUE | DBPART_LENGTH | DBPART_STATUS,
        mem_owner: DBMEMOWNER_CLIENTOWNED,
        param_io: 0,
        cb_max_len: VALUE_BYTES,
        flags: 0,
        wtype: DBTYPE_WSTR,
        precision: 0,
        scale: 0,
    };
    let row_size = VALUE_BYTES + 2 * std::mem::size_of::<u32>();

    let mut accessor: usize = 0;
    let f: unsafe extern "system" fn(
        *mut c_void,
        u32,
        usize,
        *const DbBinding,
        usize,
        *mut usize,
        *mut u32,
    ) -> Hr = slot(accessor_if, 4);
    let hr = f(
        accessor_if,
        0,
        1,
        &binding,
        row_size,
        &mut accessor,
        std::ptr::null_mut(),
    );
    if hr != S_OK {
        release(accessor_if);
        return Err(format!("列绑定失败 (0x{:08X})", hr as u32));
    }

    let mut out = Vec::new();
    let mut buf = vec![0u8; row_size];

    loop {
        if out.len() >= limit {
            break;
        }
        // IRowset::GetNextRows(0, 0, 1, &obtained, &hrows)
        let f: unsafe extern "system" fn(
            *mut c_void,
            usize,
            isize,
            isize,
            *mut isize,
            *mut *mut usize,
        ) -> Hr = slot(rowset, 5);
        let mut obtained: isize = 0;
        let mut hrows: *mut usize = std::ptr::null_mut();
        let hr = f(rowset, 0, 0, 1, &mut obtained, &mut hrows);
        if hr != S_OK || obtained <= 0 || hrows.is_null() {
            break;
        }

        let row = *hrows;
        let f: unsafe extern "system" fn(*mut c_void, usize, usize, *mut c_void) -> Hr =
            slot(rowset, 4);
        let hr = f(rowset, row, accessor, buf.as_mut_ptr() as *mut c_void);
        if hr == S_OK {
            let status = (buf.as_ptr().add(binding.ob_status) as *const u32).read_unaligned();
            if status == DBSTATUS_S_OK {
                let len =
                    (buf.as_ptr().add(binding.ob_length) as *const u32).read_unaligned() as usize;
                let chars = len / 2;
                let p = buf.as_ptr().add(binding.ob_value) as *const u16;
                let slice = std::slice::from_raw_parts(p, chars);
                let end = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
                let path = String::from_utf16_lossy(&slice[..end]);
                if !path.is_empty() {
                    out.push(Hit { path });
                }
            }
        }

        // IRowset::ReleaseRows(1, &row, null, null, null)
        let f: unsafe extern "system" fn(
            *mut c_void,
            usize,
            *const usize,
            *mut u32,
            *mut u32,
            *mut u32,
        ) -> Hr = slot(rowset, 6);
        f(
            rowset,
            1,
            &row,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }

    release(accessor_if);
    Ok(out)
}
