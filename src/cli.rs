//! Command line options and the paths they imply.

use std::env;
use std::path::PathBuf;

pub(crate) const DEFAULT_ADDR: &str = "127.0.0.1:6113";

#[derive(Clone)]
pub(crate) struct Opts {
    pub(crate) listen: String,
    pub(crate) auto: bool,
    pub(crate) delay_ms: u64,
    pub(crate) map: bool,
    pub(crate) debug: bool,
    pub(crate) wait_secs: u64,
    pub(crate) do_inject: bool,
    pub(crate) dll: Option<PathBuf>,
    pub(crate) pid: Option<u32>,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            listen: DEFAULT_ADDR.into(),
            auto: true,
            delay_ms: 700,
            map: false,
            debug: false,
            wait_secs: 10,
            do_inject: true,
            dll: None,
            pid: None,
        }
    }
}

pub(crate) fn usage() {
    println!(
        "nodus [--listen 127.0.0.1:6113] [--no-inject] [--dll PATH] [--pid N]\n\
         \x20         [--manual] [--delay MS] [--map] [--debug] [--wait SECONDS]\n\n\
         Запустите бота, затем Graphwar, и заходите в комнату через обычное меню.\n\
         Бот сам встроится в игру и будет стрелять за вашего игрока.\n\
         --listen должен совпадать с адресом, зашитым в хук (127.0.0.1:6113)."
    );
}

pub(crate) fn parse_opts() -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next().ok_or_else(|| format!("{name} требует значения"))
        };
        match a.as_str() {
            "-h" | "--help" => {
                usage();
                std::process::exit(0);
            }
            "--listen" => o.listen = value("--listen")?,
            "--delay" => {
                o.delay_ms = value("--delay")?
                    .parse()
                    .map_err(|_| "--delay: нужно число".to_string())?
            }
            "--manual" => o.auto = false,
            "--map" => o.map = true,
            "--debug" => o.debug = true,
            "--wait" => {
                o.wait_secs = value("--wait")?
                    .parse()
                    .map_err(|_| "--wait: нужно число".to_string())?
            }
            "--no-inject" => o.do_inject = false,
            "--dll" => o.dll = Some(PathBuf::from(value("--dll")?)),
            "--pid" => {
                o.pid = Some(
                    value("--pid")?
                        .parse()
                        .map_err(|_| "--pid: нужно число".to_string())?,
                )
            }
            other => return Err(format!("неизвестный аргумент: {other}")),
        }
    }
    Ok(o)
}

/// Warn when `--listen` cannot see traffic from the hook.
pub(crate) fn warn_listen(listen: &str) {
    let ok = listen == DEFAULT_ADDR || listen == "localhost:6113" || listen == "0.0.0.0:6113";
    if !ok {
        eprintln!(
            "внимание: хук всегда идёт на {DEFAULT_ADDR}, а вы слушаете {listen} — замените --listen"
        );
    }
}

/// Prefer `nodus_hook.dll` next to the executable, then the working directory.
pub(crate) fn default_dll() -> PathBuf {
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("nodus_hook.dll");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("nodus_hook.dll")
}
