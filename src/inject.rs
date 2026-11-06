// Injection of nodus_hook.dll into a running Graphwar client.
//
// The game is found by its window title.  The DLL path is written into the
// target process and a remote thread calls LoadLibraryW on it.  The hook then
// redirects every outgoing connection to our proxy.

#![allow(
    non_camel_case_types,
    clippy::upper_case_acronyms,
    clippy::manual_c_str_literals
)]

use std::collections::HashSet;
use std::ffi::{c_int, c_void};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

type HWND = *mut c_void;
type HANDLE = *mut c_void;
type BOOL = i32;
type DWORD = u32;
type LPVOID = *mut c_void;
type SIZE_T = usize;
type LPARAM = isize;

const PROCESS_CREATE_THREAD: u32 = 0x0002;
const PROCESS_VM_OPERATION: u32 = 0x0008;
const PROCESS_VM_READ: u32 = 0x0010;
const PROCESS_VM_WRITE: u32 = 0x0020;
const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_RELEASE: u32 = 0x8000;
const PAGE_READWRITE: u32 = 0x04;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: extern "system" fn(HWND, LPARAM) -> BOOL, param: LPARAM) -> BOOL;
    fn GetWindowTextW(hwnd: HWND, buf: *mut u16, max: c_int) -> c_int;
    fn GetWindowThreadProcessId(hwnd: HWND, pid: *mut DWORD) -> DWORD;
}

extern "system" {
    fn OpenProcess(access: DWORD, inherit: BOOL, pid: DWORD) -> HANDLE;
    fn CloseHandle(handle: HANDLE) -> BOOL;
    fn GetLastError() -> DWORD;
    fn VirtualAllocEx(
        proc: HANDLE,
        addr: LPVOID,
        size: SIZE_T,
        ty: DWORD,
        protect: DWORD,
    ) -> LPVOID;
    fn VirtualFreeEx(proc: HANDLE, addr: LPVOID, size: SIZE_T, ty: DWORD) -> BOOL;
    fn WriteProcessMemory(
        proc: HANDLE,
        addr: LPVOID,
        data: *const c_void,
        size: SIZE_T,
        written: *mut SIZE_T,
    ) -> BOOL;
    fn GetModuleHandleW(name: *const u16) -> HANDLE;
    fn GetProcAddress(module: HANDLE, name: *const u8) -> *const c_void;
    fn CreateRemoteThread(
        proc: HANDLE,
        attr: *mut c_void,
        stack: SIZE_T,
        start: *const c_void,
        param: LPVOID,
        flags: DWORD,
        id: *mut DWORD,
    ) -> HANDLE;
    fn WaitForSingleObject(handle: HANDLE, ms: DWORD) -> DWORD;
    fn GetExitCodeThread(thread: HANDLE, code: *mut DWORD) -> BOOL;
}

/// Name of the window Graphwar creates for itself.
const WINDOW_TITLE: &str = "Graphwar";

/// Watch for game windows and inject the hook once per process.
pub fn watch(dll: PathBuf, pid: Option<u32>) {
    if let Some(pid) = pid {
        println!("инжектирую хук в процесс {pid}");
        if !inject(pid, &dll) {
            eprintln!("инжекция не удалась");
        }
        return;
    }
    let mut done: HashSet<u32> = HashSet::new();
    loop {
        for pid in find_game_pids() {
            if !done.contains(&pid) {
                println!("найдена игра (pid {pid}), инжектирую хук");
                if inject(pid, &dll) {
                    done.insert(pid);
                }
            }
        }
        done.retain(|p| process_alive(*p));
        thread::sleep(Duration::from_secs(1));
    }
}

pub fn inject(pid: u32, dll: &Path) -> bool {
    let dll = match std::fs::canonicalize(dll) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("не вижу {}: {e}", dll.display());
            return false;
        }
    };
    let wide: Vec<u16> = dll
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let proc = OpenProcess(
            PROCESS_CREATE_THREAD
                | PROCESS_VM_OPERATION
                | PROCESS_VM_READ
                | PROCESS_VM_WRITE
                | PROCESS_QUERY_INFORMATION,
            0,
            pid,
        );
        if proc.is_null() {
            eprintln!("OpenProcess: ошибка {} (нужны права?", GetLastError());
            return false;
        }
        let bytes = wide.len() * 2;
        let remote = VirtualAllocEx(
            proc,
            std::ptr::null_mut(),
            bytes,
            MEM_COMMIT | MEM_RESERVE,
            PAGE_READWRITE,
        );
        if remote.is_null() {
            eprintln!("VirtualAllocEx: ошибка {}", GetLastError());
            CloseHandle(proc);
            return false;
        }
        let mut written = 0usize;
        let ok = WriteProcessMemory(
            proc,
            remote,
            wide.as_ptr() as *const c_void,
            bytes,
            &mut written,
        );
        if ok == 0 {
            eprintln!(
                "WriteProcessMemory: ошибка {} (скорее всего разрядность не совпадает с игрой)",
                GetLastError()
            );
            VirtualFreeEx(proc, remote, 0, MEM_RELEASE);
            CloseHandle(proc);
            return false;
        }
        let kernel32 = GetModuleHandleW(utf16("kernel32.dll").as_ptr());
        let load_library = GetProcAddress(kernel32, b"LoadLibraryW\0".as_ptr());
        if load_library.is_null() {
            eprintln!("не найден LoadLibraryW");
            VirtualFreeEx(proc, remote, 0, MEM_RELEASE);
            CloseHandle(proc);
            return false;
        }
        let thread = CreateRemoteThread(
            proc,
            std::ptr::null_mut(),
            0,
            load_library,
            remote,
            0,
            std::ptr::null_mut(),
        );
        if thread.is_null() {
            eprintln!("CreateRemoteThread: ошибка {}", GetLastError());
            VirtualFreeEx(proc, remote, 0, MEM_RELEASE);
            CloseHandle(proc);
            return false;
        }
        WaitForSingleObject(thread, 10_000);
        let mut code = 0u32;
        GetExitCodeThread(thread, &mut code);
        CloseHandle(thread);
        VirtualFreeEx(proc, remote, 0, MEM_RELEASE);
        CloseHandle(proc);
        if code == 0 {
            eprintln!("LoadLibraryW в игре вернул 0 — DLL не загрузилась");
            return false;
        }
        println!("хук загружен: {}", dll.display());
        println!("  хук видит только новые соединения: если игра уже была в комнате, выйдите и зайдите снова");
        true
    }
}

fn find_game_pids() -> Vec<u32> {
    extern "system" fn visit(hwnd: HWND, param: LPARAM) -> BOOL {
        let out = unsafe { &mut *(param as *mut Vec<u32>) };
        let mut buf = [0u16; 256];
        let len = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as c_int) };
        if len > 0 {
            let title = String::from_utf16_lossy(&buf[..len as usize]);
            if title.to_lowercase().contains(&WINDOW_TITLE.to_lowercase()) {
                let mut pid = 0u32;
                unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
                if pid != 0 && !out.contains(&pid) {
                    out.push(pid);
                }
            }
        }
        1
    }
    let mut pids = Vec::new();
    unsafe { EnumWindows(visit, &mut pids as *mut Vec<u32> as LPARAM) };
    pids
}

fn process_alive(pid: u32) -> bool {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        CloseHandle(handle);
        true
    }
}

fn utf16(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::time::Instant;

    /// Injection into our own process exercises the whole remote-thread path
    /// without needing a Java runtime.
    #[test]
    fn injects_into_itself_and_the_hook_takes_over() {
        let dll = std::env::current_exe()
            .unwrap()
            .parent()
            .and_then(|deps| deps.parent())
            .map(|dir| dir.join("nodus_hook.dll"))
            .unwrap();
        if !dll.exists() {
            eprintln!("{} is not built, skipping", dll.display());
            return;
        }
        let proxy = match TcpListener::bind("127.0.0.1:6113") {
            Ok(l) => l,
            Err(_) => {
                eprintln!("port 6113 is busy, skipping");
                return;
            }
        };
        proxy.set_nonblocking(true).unwrap();
        let dead_port = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };

        assert!(inject(std::process::id(), &dll), "self-injection failed");

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(mut client) = TcpStream::connect(("127.0.0.1", dead_port)) {
                if let Ok((mut server, _)) = proxy.accept() {
                    server
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    let mut header = Vec::new();
                    let mut byte = [0u8; 1];
                    while header.len() < 256 {
                        match server.read(&mut byte) {
                            Ok(1) if byte[0] == b'\n' => break,
                            Ok(1) => header.push(byte[0]),
                            other => panic!("header read failed: {other:?}"),
                        }
                    }
                    assert_eq!(
                        String::from_utf8_lossy(&header),
                        format!("NODUS&127.0.0.1&{dead_port}")
                    );
                    let mut reply = [0u8; 1];
                    client
                        .set_read_timeout(Some(Duration::from_secs(3)))
                        .unwrap();
                    server.write_all(b"!\n").unwrap();
                    client.read_exact(&mut reply).unwrap();
                    assert_eq!(&reply, b"!");
                    return;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("the injected hook never redirected a connection");
    }
}
