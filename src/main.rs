//! HeroNotify: the notifications of HeroineOS. The notification service
//! (started by D-Bus), its pop-ups, and commands for HeroBar and scripts.

mod config;
mod dirs;
mod focus;
mod popups;
mod service;
mod store;
mod takeover;
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
  heronotify send [--app NAME] [--urgent] SUMMARY [BODY]
                              send a notification (like notify-send)
  heronotify status            which program shows notifications
  heronotify use               make it HeroNotify (stops another one, like XFCE's)
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
        Some("send") => {
            let (mut app, mut urgent, mut words) = ("heronotify".to_string(), false, vec![]);
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--app" => app = it.next().cloned().unwrap_or_default(),
                    "--urgent" => urgent = true,
                    _ => words.push(a.clone()),
                }
            }
            let Some(summary) = words.first() else { fail("send what? (see --help)") };
            let body = words.get(1).cloned().unwrap_or_default();
            send(&app, summary, &body, urgent).unwrap_or_else(|e| fail(e));
        }
        Some("status") => {
            use takeover::Holder;
            let rivals = takeover::rivals();
            let chosen = takeover::chosen();
            match takeover::holder().unwrap_or_else(|e| fail(e)) {
                Holder::HeroNotify => println!("heronotify"),
                Holder::Other(name, _) => {
                    println!("other {name}");
                    std::process::exit(3);
                }
                // Nothing runs: the next notification starts HeroNotify
                // for sure, or maybe another one.
                Holder::Nobody if chosen || rivals.is_empty() => println!("heronotify (starts with the next notification)"),
                Holder::Nobody => {
                    println!("maybe {}", rivals.join(", "));
                    std::process::exit(3);
                }
            }
        }
        Some("use") => match takeover::take_over() {
            Ok(said) => println!("{said}"),
            Err(e) => fail(e),
        },
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

/// Sends a notification through the session bus (to whichever service
/// runs; D-Bus starts HeroNotify if none does).
fn send(app: &str, summary: &str, body: &str, urgent: bool) -> Result<(), String> {
    use dbus::arg::{PropMap, Variant};
    let c = dbus::blocking::Connection::new_session().map_err(|e| format!("no session bus ({e})"))?;
    let proxy = c.with_proxy(service::NAME, "/org/freedesktop/Notifications", std::time::Duration::from_secs(10));
    // Which service shows it (another one may hold the name).
    let (server, _, _, _): (String, String, String, String) = proxy.method_call(service::NAME, "GetServerInformation", ()).map_err(|e| {
        if e.name() == Some("org.freedesktop.DBus.Error.ServiceUnknown") {
            "no notification service: D-Bus can't start HeroNotify (is the heronotify package installed?)".to_string()
        } else {
            e.to_string()
        }
    })?;
    let mut hints: PropMap = PropMap::new();
    if urgent {
        hints.insert("urgency".into(), Variant(Box::new(2u8)));
    }
    let actions: Vec<String> = vec![];
    let (_id,): (u32,) = proxy
        .method_call(service::NAME, "Notify", (app, 0u32, "", summary, body, actions, hints, -1i32))
        .map_err(|e| e.to_string())?;
    if server == "HeroNotify" {
        let c = config::Config::load();
        let (m, d) = config::local_now();
        if c.quiet_at(m, d) && !c.lets_through(app, "", if urgent { 2 } else { 1 }) {
            let how = if c.do_not_disturb.on { "on" } else { "on (scheduled)" };
            println!("sent, but do not disturb is {how}: it only went to the list");
        } else {
            println!("sent");
        }
    } else {
        println!("sent, but {server} shows notifications here, not HeroNotify (stop {server} to use HeroNotify)");
    }
    Ok(())
}
