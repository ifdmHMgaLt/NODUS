// nodus_hook.dll — redirects every outgoing connection of the hosting
// process (the Graphwar client) to the NODUS proxy.  The IAT of every
// loaded module is patched, so socket calls are caught wherever they originate.
// Each redirected connection is prefixed with one line:
//
//     NODUS&<original ip>&<original port>\n
//
// If the proxy is unreachable the original connect is used, so the game keeps
// working without NODUS.  Both AF_INET and AF_INET6 are handled: the JVM
// opens v6 sockets even for IPv4 destinations (IPv4-mapped addresses).

#![allow(
    non_snake_case,
    non_camel_case_types,
    dead_code,
    clippy::upper_case_acronyms,
    clippy::manual_c_str_literals
)]

use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

type HANDLE = *mut c_void;
type HMODULE = *mut c_void;
type BOOL = i32;
type DWORD = u32;
type LPVOID = *mut c_void;
type SIZE_T = usize;
type SOCKET = usize;
type FARPROC = *const c_void;

const AF_INET: u16 = 2;
const AF_INET6: u16 = 23;
const PROXY_PORT: u16 = 6113;
const TH32CS_SNAPMODULE: DWORD = 0x0000_0008;
const TH32CS_SNAPMODULE32: DWORD = 0x0000_0010;
const DLL_PROCESS_ATTACH: DWORD = 1;
const WSAEWOULDBLOCK: c_int = 10035;
const PAGE_READWRITE: DWORD = 0x04;

const HOOK_LOG: &str = "nodus-hook.log";

static ORIG_CONNECT: AtomicUsize = AtomicUsize::new(0);
static ORIG_WSACONNECT: AtomicUsize = AtomicUsize::new(0);
static SEND_FN: AtomicUsize = AtomicUsize::new(0);
static LAST_ERROR_FN: AtomicUsize = AtomicUsize::new(0);
static SET_ERROR_FN: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrIn {
    family: u16,
    port: u16,
    addr: u32,
    zero: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SockaddrIn6 {
    family: u16,
    port: u16,
    flowinfo: u32,
    addr: [u8; 16],
    scope: u32,
}

/// The original destination of a connect call.  The JVM uses AF_INET6
/// sockets even for plain IPv4 addresses (IPv4-mapped), so both families
/// have to be understood.
#[derive(Clone, Copy)]
enum Dest {
    V4 { addr: [u8; 4], port: u16 },
    V6 { addr: [u8; 16], port: u16 },
}

#[repr(C)]
struct ModuleEntry32W {
    size: DWORD,
    module_id: DWORD,
    process_id: DWORD,
    global_usage: DWORD,
    process_usage: DWORD,
    base: *mut u8,
    size_of_image: DWORD,
    module: HMODULE,
    module_name: [u16; 256],
    exe_path: [u16; 260],
}

#[repr(C)]
struct ImportDescriptor {
    original_first_thunk: u32,
    timestamp: u32,
    forwarder_chain: u32,
    name: u32,
    first_thunk: u32,
}

extern "system" {
    fn GetModuleHandleW(name: *const u16) -> HMODULE;
    fn GetProcAddress(module: HMODULE, name: *const u8) -> FARPROC;
    fn GetCurrentProcessId() -> DWORD;
    fn VirtualProtect(addr: LPVOID, size: SIZE_T, new: DWORD, old: *mut DWORD) -> BOOL;
    fn CreateToolhelp32Snapshot(flags: DWORD, pid: DWORD) -> HANDLE;
    fn Module32FirstW(snap: HANDLE, entry: *mut ModuleEntry32W) -> BOOL;
    fn Module32NextW(snap: HANDLE, entry: *mut ModuleEntry32W) -> BOOL;
    fn CloseHandle(handle: HANDLE) -> BOOL;
    fn CreateThread(
        attr: *mut c_void,
        stack: SIZE_T,
        start: extern "system" fn(LPVOID) -> DWORD,
        param: LPVOID,
        flags: DWORD,
        id: *mut DWORD,
    ) -> HANDLE;
    fn Sleep(ms: DWORD);
}

#[no_mangle]
pub extern "system" fn DllMain(_instance: HMODULE, reason: DWORD, _reserved: LPVOID) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            CreateThread(
                std::ptr::null_mut(),
                0,
                worker,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
            );
        }
    }
    1
}

extern "system" fn worker(_: LPVOID) -> DWORD {
    log_line("worker started");
    unsafe {
        // ws2_32.dll may be loaded lazily, long after graphwar opens its
        // window, so keep trying instead of giving up once.
        let mut resolved = false;
        loop {
            if !resolved {
                resolved = resolve();
            }
            if resolved {
                let patched = patch_all();
                if patched > 0 {
                    log_line(&format!("patched {patched} import table(s)"));
                }
            }
            Sleep(500);
        }
    }
}

unsafe fn resolve() -> bool {
    let ws2 = GetModuleHandleW(wide("ws2_32.dll").as_ptr());
    if ws2.is_null() {
        return false;
    }
    let connect = GetProcAddress(ws2, b"connect\0".as_ptr());
    let wsa = GetProcAddress(ws2, b"WSAConnect\0".as_ptr());
    let send = GetProcAddress(ws2, b"send\0".as_ptr());
    let last = GetProcAddress(ws2, b"WSAGetLastError\0".as_ptr());
    let set = GetProcAddress(ws2, b"WSASetLastError\0".as_ptr());
    if connect.is_null() || wsa.is_null() || send.is_null() {
        return false;
    }
    ORIG_CONNECT.store(connect as usize, Ordering::SeqCst);
    ORIG_WSACONNECT.store(wsa as usize, Ordering::SeqCst);
    SEND_FN.store(send as usize, Ordering::SeqCst);
    LAST_ERROR_FN.store(last as usize, Ordering::SeqCst);
    SET_ERROR_FN.store(set as usize, Ordering::SeqCst);
    log_line("ws2_32 resolved");
    true
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn log_line(msg: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join(HOOK_LOG);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{}] {}", std::process::id(), msg);
    }
}

unsafe fn patch_all() -> usize {
    let snap = CreateToolhelp32Snapshot(
        TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32,
        GetCurrentProcessId(),
    );
    if snap as isize == -1 {
        return 0;
    }
    let mut entry: ModuleEntry32W = std::mem::zeroed();
    entry.size = std::mem::size_of::<ModuleEntry32W>() as DWORD;
    let mut total = 0;
    if Module32FirstW(snap, &mut entry) != 0 {
        loop {
            if !entry.base.is_null() {
                let n = patch_module(entry.base);
                if n > 0 {
                    let name = String::from_utf16_lossy(
                        &entry.module_name
                            [..entry.module_name.iter().position(|c| *c == 0).unwrap_or(0)],
                    );
                    log_line(&format!("patched {n} entr(ies) in {name}"));
                }
                total += n;
            }
            if Module32NextW(snap, &mut entry) == 0 {
                break;
            }
        }
    }
    CloseHandle(snap);
    total
}

#[inline]
unsafe fn rd_u16(p: *const u8) -> u16 {
    (p as *const u16).read_unaligned()
}

#[inline]
unsafe fn rd_u32(p: *const u8) -> u32 {
    (p as *const u32).read_unaligned()
}

unsafe fn rd_cstr(p: *const u8, max: usize) -> Option<String> {
    let mut out = Vec::new();
    for i in 0..max {
        let b = *p.add(i);
        if b == 0 {
            return Some(String::from_utf8_lossy(&out).into_owned());
        }
        out.push(b);
    }
    None
}

unsafe fn patch_module(base: *mut u8) -> usize {
    if rd_u16(base) != 0x5A4D {
        return 0;
    }
    let nt_off = rd_u32(base.add(0x3C)) as usize;
    let nt = base.add(nt_off);
    if rd_u32(nt) != 0x0000_4550 {
        return 0;
    }
    let optional = nt.add(24);
    let magic = rd_u16(optional);
    let (dir_off, thunk_size) = match magic {
        0x20b => (112usize, 8usize),
        0x10b => (96usize, 4usize),
        _ => return 0,
    };
    // Data directory entry #1 is the import table.
    let import_rva = rd_u32(optional.add(dir_off + 8)) as usize;
    if import_rva == 0 {
        return 0;
    }
    let mut desc = base.add(import_rva) as *const ImportDescriptor;
    let mut patched = 0;
    loop {
        let d = &*desc;
        if d.name == 0 || d.first_thunk == 0 {
            break;
        }
        if let Some(dll) = rd_cstr(base.add(d.name as usize), 64) {
            if dll.eq_ignore_ascii_case("ws2_32.dll") {
                let orig = if d.original_first_thunk != 0 {
                    base.add(d.original_first_thunk as usize)
                } else {
                    base.add(d.first_thunk as usize)
                };
                patched += patch_thunks(orig, base.add(d.first_thunk as usize), thunk_size);
            }
        }
        desc = desc.add(1);
    }
    patched
}

/// Replaces every IAT slot that currently points at the real `connect` or
/// `WSAConnect` with our hook.  Comparing addresses instead of names also
/// catches ordinal imports: the MSVC import library pulls `connect` in as
/// ws2_32 ordinal 4, with no name in the import table.
unsafe fn patch_thunks(names: *const u8, iat: *mut u8, thunk_size: usize) -> usize {
    let real_connect = ORIG_CONNECT.load(Ordering::SeqCst);
    let real_wsa = ORIG_WSACONNECT.load(Ordering::SeqCst);
    let hook_connect = hook_connect as ConnectFn as usize;
    let hook_wsa = hook_wsaconnect as WsaConnectFn as usize;
    let mut count = 0;
    let mut i = 0usize;
    loop {
        let term = if thunk_size == 8 {
            (names.add(i * thunk_size) as *const u64).read_unaligned()
        } else {
            (names.add(i * thunk_size) as *const u32).read_unaligned() as u64
        };
        if term == 0 {
            break;
        }
        let slot = iat.add(i * thunk_size);
        let old = if thunk_size == 8 {
            (slot as *const u64).read_unaligned() as usize
        } else {
            (slot as *const u32).read_unaligned() as usize
        };
        i += 1;
        let replacement = if old == real_connect {
            hook_connect
        } else if old == real_wsa {
            hook_wsa
        } else {
            continue;
        };
        let mut protect = 0u32;
        if VirtualProtect(slot as LPVOID, thunk_size, PAGE_READWRITE, &mut protect) == 0 {
            continue;
        }
        if thunk_size == 8 {
            (slot as *mut u64).write_unaligned(replacement as u64);
        } else {
            (slot as *mut u32).write_unaligned(replacement as u32);
        }
        VirtualProtect(slot as LPVOID, thunk_size, protect, &mut protect);
        count += 1;
    }
    count
}

type ConnectFn = unsafe extern "system" fn(SOCKET, *const u8, c_int) -> c_int;
type WsaConnectFn = unsafe extern "system" fn(
    SOCKET,
    *const u8,
    c_int,
    *mut c_void,
    *mut c_void,
    *mut c_void,
    *mut c_void,
) -> c_int;

enum Orig {
    Plain(ConnectFn),
    Wsa(WsaConnectFn),
}

impl Orig {
    unsafe fn call(&self, s: SOCKET, name: *const u8, namelen: c_int) -> c_int {
        match self {
            Orig::Plain(f) => f(s, name, namelen),
            Orig::Wsa(f) => f(
                s,
                name,
                namelen,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            ),
        }
    }
}

unsafe extern "system" fn hook_connect(s: SOCKET, name: *const u8, namelen: c_int) -> c_int {
    let orig: ConnectFn = std::mem::transmute(ORIG_CONNECT.load(Ordering::SeqCst));
    redirect(s, name, namelen, Orig::Plain(orig))
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn hook_wsaconnect(
    s: SOCKET,
    name: *const u8,
    namelen: c_int,
    caller_data: *mut c_void,
    _callee_data: *mut c_void,
    _sqos: *mut c_void,
    _gqos: *mut c_void,
) -> c_int {
    let orig: WsaConnectFn = std::mem::transmute(ORIG_WSACONNECT.load(Ordering::SeqCst));
    // Caller data is attached to a particular destination; leave it alone.
    if !caller_data.is_null() {
        return orig(
            s,
            name,
            namelen,
            caller_data,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
    redirect(s, name, namelen, Orig::Wsa(orig))
}

unsafe fn redirect(s: SOCKET, name: *const u8, namelen: c_int, orig: Orig) -> c_int {
    let Some(dest) = read_dest(name, namelen) else {
        log_line(&format!(
            "connect on socket {s}: unknown family (len {namelen}), passing through"
        ));
        return orig.call(s, name, namelen);
    };
    log_line(&format!(
        "connect on socket {s}: {}:{}",
        dest_ip(&dest),
        dest_port(&dest)
    ));
    if is_proxy(&dest) {
        return orig.call(s, name, namelen);
    }
    // Both proxy addresses live until the connect call below; a temporary
    // inside the match arm would be dropped before it.
    let proxy_v4 = SockaddrIn {
        family: AF_INET,
        port: PROXY_PORT.to_be(),
        addr: u32::from_ne_bytes([127, 0, 0, 1]),
        zero: [0; 8],
    };
    // The JVM opens v6-only sockets, which reject IPv4-mapped addresses, so
    // the proxy is reached through the real IPv6 loopback.
    let mut addr6 = [0u8; 16];
    addr6[15] = 1;
    let proxy_v6 = SockaddrIn6 {
        family: AF_INET6,
        port: PROXY_PORT.to_be(),
        flowinfo: 0,
        addr: addr6,
        scope: 0,
    };
    let (proxy_ptr, proxy_len) = match &dest {
        Dest::V4 { .. } => (&proxy_v4 as *const SockaddrIn as *const u8, 16),
        Dest::V6 { .. } => (&proxy_v6 as *const SockaddrIn6 as *const u8, 28),
    };
    let ret = orig.call(s, proxy_ptr, proxy_len);
    let header = format!("NODUS&{}&{}\n", dest_ip(&dest), dest_port(&dest));
    if ret != 0 {
        let err = last_error();
        if err == WSAEWOULDBLOCK {
            // Non-blocking socket: the connect to the proxy is still in
            // flight.  The header must still go out before the client's own
            // bytes, so wait for the socket to become writable.
            send_all(s, header.as_bytes());
            log_line(&format!(
                "{}:{} -> proxy (async)",
                dest_ip(&dest),
                dest_port(&dest)
            ));
            // send_all and other calls change the thread's winsock error;
            // the JVM reads it right after connect returns.
            set_last_error(WSAEWOULDBLOCK);
            return ret;
        }
        log_line(&format!("proxy connect failed: {err}, passing through"));
        // Nobody listens on the proxy: behave exactly like the real client.
        return orig.call(s, name, namelen);
    }
    send_all(s, header.as_bytes());
    log_line(&format!("{}:{} -> proxy", dest_ip(&dest), dest_port(&dest)));
    0
}

unsafe fn read_dest(name: *const u8, namelen: c_int) -> Option<Dest> {
    if name.is_null() || namelen < 2 {
        return None;
    }
    let family = rd_u16(name);
    match family {
        AF_INET => {
            if namelen < std::mem::size_of::<SockaddrIn>() as c_int {
                return None;
            }
            let s = (name as *const SockaddrIn).read_unaligned();
            Some(Dest::V4 {
                addr: s.addr.to_ne_bytes(),
                port: u16::from_be(s.port),
            })
        }
        AF_INET6 => {
            if namelen < std::mem::size_of::<SockaddrIn6>() as c_int {
                return None;
            }
            let s = (name as *const SockaddrIn6).read_unaligned();
            Some(Dest::V6 {
                addr: s.addr,
                port: u16::from_be(s.port),
            })
        }
        _ => None,
    }
}

fn v4_mapped(addr: &[u8; 16]) -> Option<[u8; 4]> {
    if addr[..10].iter().all(|b| *b == 0) && addr[10] == 0xff && addr[11] == 0xff {
        Some([addr[12], addr[13], addr[14], addr[15]])
    } else {
        None
    }
}

fn is_proxy(dest: &Dest) -> bool {
    match dest {
        Dest::V4 { addr, port } => *addr == [127, 0, 0, 1] && *port == PROXY_PORT,
        Dest::V6 { addr, port } => {
            if *port != PROXY_PORT {
                return false;
            }
            match v4_mapped(addr) {
                Some(v4) => v4 == [127, 0, 0, 1],
                None => addr[..15].iter().all(|b| *b == 0) && addr[15] == 1,
            }
        }
    }
}

fn dest_port(dest: &Dest) -> u16 {
    match dest {
        Dest::V4 { port, .. } | Dest::V6 { port, .. } => *port,
    }
}

/// Text form for the proxy header.  IPv4-mapped IPv6 collapses to plain
/// dotted IPv4; real IPv6 goes into brackets so the proxy can dial it.
fn dest_ip(dest: &Dest) -> String {
    match dest {
        Dest::V4 { addr, .. } => format!("{}.{}.{}.{}", addr[0], addr[1], addr[2], addr[3]),
        Dest::V6 { addr, .. } => {
            if let Some(v4) = v4_mapped(addr) {
                return format!("{}.{}.{}.{}", v4[0], v4[1], v4[2], v4[3]);
            }
            let mut groups = [0u16; 8];
            for (i, g) in groups.iter_mut().enumerate() {
                *g = u16::from_be_bytes([addr[i * 2], addr[i * 2 + 1]]);
            }
            format!(
                "[{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}:{:x}]",
                groups[0],
                groups[1],
                groups[2],
                groups[3],
                groups[4],
                groups[5],
                groups[6],
                groups[7]
            )
        }
    }
}

unsafe fn last_error() -> c_int {
    let p = LAST_ERROR_FN.load(Ordering::SeqCst);
    if p == 0 {
        return 0;
    }
    let f: unsafe extern "system" fn() -> c_int = std::mem::transmute(p);
    f()
}

unsafe fn set_last_error(code: c_int) {
    let p = SET_ERROR_FN.load(Ordering::SeqCst);
    if p == 0 {
        return;
    }
    let f: unsafe extern "system" fn(c_int) = std::mem::transmute(p);
    f(code);
}

unsafe fn send_all(s: SOCKET, mut data: &[u8]) {
    let p = SEND_FN.load(Ordering::SeqCst);
    if p == 0 {
        return;
    }
    let send: unsafe extern "system" fn(SOCKET, *const u8, c_int, c_int) -> c_int =
        std::mem::transmute(p);
    let mut spin = 0u32;
    while !data.is_empty() {
        let n = send(s, data.as_ptr(), data.len() as c_int, 0);
        if n > 0 {
            data = &data[n as usize..];
            spin = 0;
            continue;
        }
        // A non-blocking socket reports "would block" until the connection
        // to the proxy is established and the send buffer has room.
        if n < 0 && last_error() == WSAEWOULDBLOCK && spin < 2000 {
            Sleep(1);
            spin += 1;
            continue;
        }
        return;
    }
}
