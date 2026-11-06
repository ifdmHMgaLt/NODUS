// NODUS — plays your Graphwar soldier for you.
//
// The bot proxies 127.0.0.1:6113 and injects nodus_hook.dll into the client.
// The hook reroutes every game connection to the proxy with one header line
// naming the real destination; the proxy dials it, relays traffic and tracks
// the protocol.  On your turn it synthesises a curve through the enemies,
// verifies the rendered formula with an exact copy of the game's integrator
// and fires it as if the client itself had.

mod cli;
#[cfg(windows)]
mod inject;
mod proxy;
mod session;

use std::net::TcpListener;
use std::thread;

fn main() {
    let opts = match cli::parse_opts() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            cli::usage();
            std::process::exit(2);
        }
    };
    if opts.do_inject {
        cli::warn_listen(&opts.listen);
    }
    let listener = match TcpListener::bind(&opts.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("не удалось слушать {}: {e}", opts.listen);
            std::process::exit(1);
        }
    };
    println!("прокси слушает {}", opts.listen);

    if let Some(v6) = proxy::v6_listen(&opts.listen) {
        match TcpListener::bind(&v6) {
            Ok(l) => {
                println!("прокси слушает {}", v6);
                let o = opts.clone();
                thread::spawn(move || proxy::accept_loop(l, o));
            }
            Err(e) => eprintln!("не удалось слушать {v6}: {e}"),
        }
    }

    #[cfg(windows)]
    if opts.do_inject {
        let dll = opts.dll.clone().unwrap_or_else(cli::default_dll);
        let pid = opts.pid;
        thread::spawn(move || {
            println!("жду окно Graphwar; DLL: {}", dll.display());
            inject::watch(dll, pid);
        });
    }
    #[cfg(not(windows))]
    {
        let _ = opts.do_inject;
        let _ = opts.pid;
        let _ = opts.dll;
    }

    proxy::accept_loop(listener, opts);
}
