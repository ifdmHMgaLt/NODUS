//! TCP proxy: accepts hook connections, relays the game, feeds a session.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nodus::protocol::*;

use crate::cli::Opts;
use crate::session::{Session, Shared};

const READ_TIMEOUT_MS: u64 = 30;

/// Reads the `NODUS&<host>&<port>\n` line and returns it together with any
/// bytes that were already buffered behind it.
fn read_header(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Some((String::from_utf8_lossy(&line).into_owned(), buf));
        }
        if buf.len() > 16 * 1024 {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                return None;
            }
            Err(_) => return None,
        }
    }
}

fn parse_header(line: &str) -> Option<(String, u16)> {
    let p = split(line);
    if p.len() != 3 || p[0] != "NODUS" {
        return None;
    }
    let host = p[1].trim().to_string();
    let port: u16 = p[2].parse().ok()?;
    if host.is_empty() || port == 0 {
        return None;
    }
    Some((host, port))
}

fn serve(mut client: TcpStream, opts: Opts) {
    let peer = client
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "?".into());
    let _ = client.set_read_timeout(Some(Duration::from_secs(20)));
    let (header, leftover) = match read_header(&mut client) {
        Some(v) => v,
        None => {
            eprintln!("{peer}: заголовок не получен, закрываю");
            return;
        }
    };
    let Some((host, port)) = parse_header(&header) else {
        eprintln!("{peer}: чужой заголовок {header:?}, закрываю");
        return;
    };
    let addr = format!("{host}:{port}");
    println!("канал: {peer} -> {addr}");

    let wait = Duration::from_secs(opts.wait_secs.max(1));
    let mut up = match connect_with_wait(&addr, wait) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{addr}: соединение не открылось: {e}");
            return;
        }
    };

    let up_wr = match up.try_clone() {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(e) => {
            eprintln!("{addr}: сокет: {e}");
            return;
        }
    };
    let client_rd = match client.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{addr}: сокет: {e}");
            return;
        }
    };
    let shared = Arc::new(Shared::new(opts.debug));
    let pump_shared = shared.clone();
    let pump_up = up_wr.clone();
    thread::spawn(move || pump_client(client_rd, pump_up, pump_shared, leftover));

    let mut session = Session::new(addr.clone(), opts, up_wr, shared.clone());

    let _ = up.set_read_timeout(Some(Duration::from_millis(READ_TIMEOUT_MS)));
    let mut buf = [0u8; 8192];
    loop {
        match up.read(&mut buf) {
            Ok(0) => {
                println!("{}: сервер закрыл соединение", session.label());
                break;
            }
            Ok(n) => {
                if client.write_all(&buf[..n]).is_err() {
                    break;
                }
                session.feed(&buf[..n]);
                session.tick();
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                session.tick();
            }
            Err(e) => {
                eprintln!("{}: сеть: {e}", session.label());
                break;
            }
        }
        if session.client_closed() {
            break;
        }
    }
    let _ = client.shutdown(Shutdown::Both);
}

fn pump_client(
    mut client: TcpStream,
    up: Arc<Mutex<TcpStream>>,
    shared: Arc<Shared>,
    leftover: Vec<u8>,
) {
    client
        .set_read_timeout(Some(Duration::from_millis(200)))
        .ok();
    let mut buf = [0u8; 8192];
    let mut acc: Vec<u8> = Vec::new();
    let mut pending = leftover;
    loop {
        if !pending.is_empty() {
            let mut g = match up.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            if g.write_all(&pending).is_err() {
                break;
            }
            let _ = g.flush();
            drop(g);
            pending.clear();
        }
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.extend_from_slice(&buf[..n]);
                while let Some(pos) = acc.iter().position(|b| *b == b'\n') {
                    let line: Vec<u8> = acc.drain(..=pos).collect();
                    let text = String::from_utf8_lossy(&line[..line.len() - 1]);
                    let text = text.trim_end_matches('\r');
                    let p = split(text);
                    let ty = p.first().and_then(|s| s.parse::<i32>().ok());
                    if shared.debug && ty.is_some() {
                        let short: String = text.chars().take(160).collect();
                        eprintln!("клиент -> {short}");
                    }
                    if ty == Some(FIRE_FUNC) {
                        shared.client_fired.store(true, Ordering::SeqCst);
                    }
                    // Remember who the client plays as: these frames are
                    // always about its own player.
                    match ty {
                        Some(FIRE_FUNC) | Some(SET_READY) | Some(SET_ANGLE) | Some(ADD_SOLDIER) => {
                            if let Some(id) = p.get(1).and_then(|s| s.parse::<i32>().ok()) {
                                shared.note_own_id(id);
                            }
                        }
                        Some(SET_TEAM) => {
                            if let Some(id) = p.get(2).and_then(|s| s.parse::<i32>().ok()) {
                                shared.note_own_id(id);
                            }
                        }
                        _ => {}
                    }
                    if ty == Some(ADD_PLAYER) {
                        if let Some(name) = p.get(1) {
                            shared.note_own_name(&url_decode(name));
                        }
                    }
                    if let Ok(mut g) = up.lock() {
                        if g.write_all(&line).is_err() {
                            break;
                        }
                        let _ = g.flush();
                    }
                }
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
            }
            Err(_) => break,
        }
    }
    if let Ok(g) = up.lock() {
        let _ = g.shutdown(Shutdown::Write);
    }
    shared.client_closed.store(true, Ordering::SeqCst);
}

fn connect_with_wait(addr: &str, wait: Duration) -> io::Result<TcpStream> {
    let deadline = Instant::now() + wait;
    loop {
        match TcpStream::connect(addr) {
            Ok(s) => return Ok(s),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(e);
                }
                thread::sleep(Duration::from_millis(200));
            }
        }
    }
}

pub(crate) fn accept_loop(listener: TcpListener, opts: Opts) {
    for conn in listener.incoming() {
        match conn {
            Ok(sock) => {
                let o = opts.clone();
                thread::spawn(move || serve(sock, o));
            }
            Err(e) => eprintln!("accept: {e}"),
        }
    }
}

/// IPv6 loopback twin of the listen address.  The JVM's sockets are v6-only,
/// so the proxy has to answer on ::1 as well as 127.0.0.1.
pub(crate) fn v6_listen(addr: &str) -> Option<String> {
    let (host, port) = addr.rsplit_once(':')?;
    if host == "127.0.0.1" {
        Some(format!("[::1]:{port}"))
    } else {
        None
    }
}
