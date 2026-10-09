//! The notification service on the session bus: the freedesktop
//! `org.freedesktop.Notifications` interface every app sends notifications
//! through. Each goes to the history; pop-ups show in a process of their
//! own (`heronotify --popups`) that runs only while some are on screen.
//! D-Bus starts the service when an app sends one; it leaves when nothing
//! has shown for a while.

use std::cell::RefCell;
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use dbus::arg::{PropMap, RefArg};
use dbus::blocking::LocalConnection;
use dbus::channel::MatchingReceiver;
use dbus::message::MatchRule;
use dbus::Message;

use crate::config::Config;
use crate::popups::{Event, Order};
use crate::store::{self, Note};

pub const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// How long to stay after the last pop-up goes.
const LINGER: Duration = Duration::from_secs(30);

const INTROSPECTION: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.freedesktop.Notifications">
    <method name="GetCapabilities"><arg type="as" direction="out"/></method>
    <method name="Notify"><arg type="s" name="app_name" direction="in"/><arg type="u" name="replaces_id" direction="in"/><arg type="s" name="app_icon" direction="in"/><arg type="s" name="summary" direction="in"/><arg type="s" name="body" direction="in"/><arg type="as" name="actions" direction="in"/><arg type="a{sv}" name="hints" direction="in"/><arg type="i" name="expire_timeout" direction="in"/><arg type="u" name="id" direction="out"/></method>
    <method name="CloseNotification"><arg type="u" name="id" direction="in"/></method>
    <method name="GetServerInformation"><arg type="s" name="name" direction="out"/><arg type="s" name="vendor" direction="out"/><arg type="s" name="version" direction="out"/><arg type="s" name="spec_version" direction="out"/></method>
    <signal name="NotificationClosed"><arg type="u" name="id"/><arg type="u" name="reason"/></signal>
    <signal name="ActionInvoked"><arg type="u" name="id"/><arg type="s" name="action_key"/></signal>
  </interface>
  <interface name="org.freedesktop.DBus.Introspectable"><method name="Introspect"><arg type="s" name="xml" direction="out"/></method></interface>
</node>"#;

/// Why a notification closed (the spec's reasons).
pub const EXPIRED: u32 = 1;
pub const DISMISSED: u32 = 2;
pub const CLOSED: u32 = 3;
pub const UNDEFINED: u32 = 4;

/// The pop-ups' process, while it runs.
struct Popups {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<Event>,
}

#[derive(Default)]
struct Service {
    popups: Option<Popups>,
    /// Ids handed to the pop-ups and not closed yet.
    shown: HashSet<u32>,
}

pub fn run() -> Result<(), String> {
    let c = LocalConnection::new_session().map_err(|e| format!("no session bus ({e})"))?;
    use dbus::blocking::stdintf::org_freedesktop_dbus::RequestNameReply;
    match c.request_name(NAME, false, false, true).map_err(|e| e.to_string())? {
        RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner => {}
        _ => return Err("another notification service is running (mako, dunst, a desktop's own...)".into()),
    }
    let service: Rc<RefCell<Service>> = Rc::default();
    {
        let service = service.clone();
        c.start_receive(
            MatchRule::new_method_call(),
            Box::new(move |msg, conn| {
                handle(msg, conn, &service);
                true
            }),
        );
    }
    let mut idle_since = Instant::now();
    loop {
        let busy = service.borrow().popups.is_some();
        if c.process(if busy { Duration::from_millis(100) } else { Duration::from_secs(5) }).is_err() {
            // The session ended.
            return Ok(());
        }
        let events: Vec<Event> = service.borrow().popups.as_ref().map(|p| p.events.try_iter().collect()).unwrap_or_default();
        for e in events {
            match e {
                Event::Closed(id, reason) => {
                    service.borrow_mut().shown.remove(&id);
                    let _ = c.channel().send(closed(id, reason));
                }
                Event::Action(id, key) => {
                    let _ = c.channel().send(Message::new_signal(PATH, NAME, "ActionInvoked").unwrap().append2(id, key));
                }
            }
        }
        // Pop-ups gone: their process has quit (or crashed).
        let ended = service.borrow_mut().popups.as_mut().is_some_and(|p| !matches!(p.child.try_wait(), Ok(None)));
        if ended {
            let mut s = service.borrow_mut();
            s.popups = None;
            for id in std::mem::take(&mut s.shown) {
                let _ = c.channel().send(closed(id, UNDEFINED));
            }
        }
        if service.borrow().popups.is_some() {
            idle_since = Instant::now();
        } else if idle_since.elapsed() > LINGER {
            return Ok(());
        }
    }
}

fn closed(id: u32, reason: u32) -> Message {
    Message::new_signal(PATH, NAME, "NotificationClosed").unwrap().append2(id, reason)
}

fn handle(msg: Message, conn: &LocalConnection, service: &Rc<RefCell<Service>>) {
    let (Some(iface), Some(member)) = (msg.interface(), msg.member()) else { return };
    let (iface, member) = (iface.to_string(), member.to_string());
    let send = |m: Message| {
        let _ = conn.channel().send(m);
    };
    let error = |name: &str, text: &str| msg.error(&name.into(), &std::ffi::CString::new(text).unwrap_or_default());
    match (iface.as_str(), member.as_str()) {
        ("org.freedesktop.DBus.Introspectable", "Introspect") => send(msg.method_return().append1(INTROSPECTION)),
        ("org.freedesktop.DBus.Peer", "Ping") => send(msg.method_return()),
        (NAME, "GetCapabilities") => {
            // No markup: apps send plain text then (some send it anyway;
            // tags are taken out).
            let caps: Vec<&str> = vec!["body", "actions", "icon-static", "persistence"];
            send(msg.method_return().append1(caps))
        }
        (NAME, "GetServerInformation") => send(msg.method_return().append2("HeroNotify", "HeroineOS").append2(env!("CARGO_PKG_VERSION"), "1.2")),
        (NAME, "CloseNotification") => {
            let id: u32 = msg.read1().unwrap_or(0);
            let mut s = service.borrow_mut();
            if s.shown.remove(&id) {
                if let Some(p) = &mut s.popups {
                    tell(p, &Order::Close(id));
                }
                drop(s);
                send(closed(id, CLOSED));
            }
            send(msg.method_return());
        }
        (NAME, "Notify") => match read_notify(&msg) {
            Ok(n) => {
                let id = notify(n, service);
                send(msg.method_return().append1(id));
            }
            Err(e) => send(error("org.freedesktop.DBus.Error.InvalidArgs", &e.to_string())),
        },
        _ => {
            if msg.msg_type() == dbus::MessageType::MethodCall {
                send(error("org.freedesktop.DBus.Error.UnknownMethod", "no such method"));
            }
        }
    }
}

/// A Notify call as it came.
struct Incoming {
    app: String,
    replaces: u32,
    icon: String,
    summary: String,
    body: String,
    actions: Vec<String>,
    hints: PropMap,
    timeout: i32,
}

fn read_notify(msg: &Message) -> Result<Incoming, dbus::arg::TypeMismatchError> {
    let mut i = msg.iter_init();
    Ok(Incoming {
        app: i.read()?,
        replaces: i.read()?,
        icon: i.read()?,
        summary: i.read()?,
        body: i.read()?,
        actions: i.read()?,
        hints: i.read()?,
        timeout: i.read()?,
    })
}

/// Records and shows a notification; its id.
fn notify(n: Incoming, service: &Rc<RefCell<Service>>) -> u32 {
    let config = Config::load();
    // Replacing one still around keeps its id (progress updates...).
    let id = if n.replaces != 0 && (service.borrow().shown.contains(&n.replaces) || store::history().iter().any(|h| h.id == n.replaces)) { n.replaces } else { store::new_id() };
    let hint = |k: &str| n.hints.get(k).map(|v| &v.0);
    let hint_str = |k: &str| hint(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let urgency = hint("urgency").and_then(|v| v.as_u64()).unwrap_or(1).min(2) as u8;
    let entry = hint_str("desktop-entry");
    let transient = hint("transient").and_then(|v| v.as_u64().or_else(|| v.as_i64().map(|x| x as u64))).is_some_and(|v| v != 0);
    // The picture: pixels sent along, a picture file, the app's icon, the
    // desktop entry's icon.
    let pixels = ["image-data", "image_data", "icon_data"].iter().find_map(|k| hint(k)).and_then(|v| save_pixels(id, &**v));
    let image_path = hint_str(if n.hints.contains_key("image-path") { "image-path" } else { "image_path" });
    let icon = pixels.or_else(|| icon_file(&image_path)).or_else(|| icon_file(&n.icon)).or_else(|| desktop_icon(&entry)).unwrap_or_default();
    let actions = n.actions.chunks_exact(2).map(|p| (p[0].clone(), p[1].clone())).collect();
    let timeout = if urgency >= 2 {
        0
    } else if n.timeout < 0 {
        config.timeout.saturating_mul(1000)
    } else {
        n.timeout as u32
    };
    let note = Note {
        id,
        app: n.app,
        entry,
        icon,
        summary: crate::text::plain(&n.summary),
        body: crate::text::plain(&n.body),
        actions,
        urgency,
        time: store::now(),
        timeout,
        transient,
    };
    store::record(&note, config.history.keep);
    let (minutes, weekday) = crate::config::local_now();
    let pops = !config.quiet_at(minutes, weekday) || config.lets_through(&note.app, &note.entry, note.urgency);
    if pops {
        let mut s = service.borrow_mut();
        if s.popups.is_none() {
            s.popups = start_popups();
        }
        if let Some(p) = &mut s.popups {
            tell(p, &Order::Show(note));
            s.shown.insert(id);
        }
    }
    id
}

fn tell(p: &mut Popups, order: &Order) {
    let mut line = serde_json::to_string(order).unwrap_or_default();
    line.push('\n');
    let _ = p.stdin.write_all(line.as_bytes());
    let _ = p.stdin.flush();
}

fn start_popups() -> Option<Popups> {
    let exe = std::env::current_exe().ok()?;
    let mut child = Command::new(exe).arg("--popups").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(|e| eprintln!("heronotify: pop-ups: {e}")).ok()?;
    let stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let (tx, events) = mpsc::channel();
    std::thread::Builder::new()
        .name("heronotify-popups".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(e) = serde_json::from_str::<Event>(&line) {
                    if tx.send(e).is_err() {
                        break;
                    }
                }
            }
        })
        .ok()?;
    Some(Popups { child, stdin, events })
}

/// An icon name or picture file as given (a file:// URI made a path).
fn icon_file(s: &str) -> Option<String> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    Some(s.strip_prefix("file://").map(crate::text::unescape_uri).unwrap_or_else(|| s.to_string()))
}

/// The Icon= of desktop entry `id`.
fn desktop_icon(id: &str) -> Option<String> {
    if id.is_empty() || id.contains('/') {
        return None;
    }
    let home = crate::dirs::config().parent().map(|h| h.join(".local/share")).unwrap_or_default();
    let data = std::env::var("XDG_DATA_HOME").map(std::path::PathBuf::from).unwrap_or(home);
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let id = id.trim_end_matches(".desktop");
    std::iter::once(data).chain(dirs.split(':').map(std::path::PathBuf::from)).find_map(|d| {
        let text = std::fs::read_to_string(d.join("applications").join(format!("{id}.desktop"))).ok()?;
        text.lines().find_map(|l| l.strip_prefix("Icon=")).map(|i| i.trim().to_string())
    })
}

/// Pixels sent along (iiibiiay: width, height, row stride, alpha, bits,
/// channels, bytes), saved as a PNG for the pop-up and the history.
fn save_pixels(id: u32, v: &dyn RefArg) -> Option<String> {
    let mut f = v.as_iter()?;
    let mut int = || f.next().and_then(|x| x.as_i64());
    let (w, h, stride) = (int()? as usize, int()? as usize, int()? as usize);
    let _alpha = int()?;
    let (bits, channels) = (int()?, int()? as usize);
    let data: Vec<u8> = f.next()?.as_iter()?.filter_map(|b| b.as_u64().map(|b| b as u8)).collect();
    if bits != 8 || !(3..=4).contains(&channels) || w == 0 || h == 0 || w > 4096 || h > 4096 || stride < w * channels || data.len() < stride * (h - 1) + w * channels {
        return None;
    }
    let mut px = Vec::with_capacity(w * h * channels);
    for row in 0..h {
        px.extend_from_slice(&data[row * stride..row * stride + w * channels]);
    }
    let dir = crate::dirs::images();
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{id}.png"));
    let file = std::fs::File::create(&path).ok()?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(if channels == 4 { png::ColorType::Rgba } else { png::ColorType::Rgb });
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().ok()?.write_image_data(&px).ok()?;
    Some(path.to_string_lossy().into_owned())
}
