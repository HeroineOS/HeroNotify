//! HeroNotify: the notifications of HeroineOS. The notification service
//! (started by D-Bus), its pop-ups, and commands for HeroBar and scripts.

mod config;
mod dirs;
mod popups;
mod service;
mod store;
mod text;

fn usage() -> ! {
    println!(
        "heronotify {} - the HeroineOS notifications (pop-ups, history, do-not-disturb)

Usage:
  heronotify                  the notification service (D-Bus starts it when needed)
  heronotify list [--json]    the history, newest first
  heronotify unread           how many in the history are new
  heronotify seen             mark the history seen
  heronotify clear [ID]       empty the history, or take one out
  heronotify dnd [on|off|toggle]
                              do-not-disturb by hand (no word: whether it's on now)
  heronotify --print-default-config
  heronotify --version
  heronotify --help

Settings: ~/.config/hero/notifications.toml (read for each notification).
History: ~/.local/state/hero/notifications.jsonl",
        env!("CARGO_PKG_VERSION")
    );
    std::process::exit(0)
}

fn fail(msg: impl std::fmt::Display) -> ! {
    eprintln!("heronotify: {msg}");
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| args.get(i).map(String::as_str);
    match arg(0) {
        None => service::run().unwrap_or_else(|e| fail(e)),
        Some("--popups") => popups::run(),
        Some("list") => {
            let json = arg(1) == Some("--json");
            let now = store::now();
            for n in store::history().iter().rev() {
                if json {
                    println!("{}", serde_json::to_string(n).unwrap_or_default());
                } else {
                    let app = if n.app.is_empty() { "?" } else { &n.app };
                    println!("{:>5}  {:<9} {}: {}{}", n.id, text::ago(n.time, now), app, n.summary, if n.body.is_empty() { String::new() } else { format!(" - {}", n.body.replace('\n', " ")) });
                }
            }
        }
        Some("unread") => {
            let seen = store::state().seen;
            println!("{}", store::history().iter().filter(|n| n.id > seen).count());
        }
        Some("seen") => store::seen().unwrap_or_else(|e| fail(e)),
        Some("clear") => {
            let id = arg(1).map(|s| s.parse::<u32>().unwrap_or_else(|_| fail(format!("not an id: {s}"))));
            store::clear(id).unwrap_or_else(|e| fail(e));
        }
        Some("dnd") => {
            let c = config::Config::load();
            let on = match arg(1) {
                None => {
                    let (m, d) = config::local_now();
                    let now = c.quiet_at(m, d);
                    println!("{}", if now { "on" } else { "off" });
                    std::process::exit(if now { 0 } else { 1 });
                }
                Some("on") => true,
                Some("off") => false,
                Some("toggle") => !c.do_not_disturb.on,
                Some(other) => fail(format!("dnd {other}? (on, off or toggle)")),
            };
            config::set_dnd(on).unwrap_or_else(|e| fail(e));
        }
        Some("--print-default-config") => print!("{}", config::DEFAULT),
        Some("-V" | "--version") => println!("heronotify {}", env!("CARGO_PKG_VERSION")),
        Some("-h" | "--help") => usage(),
        Some(other) => fail(format!("unknown argument {other:?} (see --help)")),
    }
}
