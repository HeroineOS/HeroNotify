//! Bringing a notification's app forward when it's clicked.
//!
//! The app is told through its "default" action (Telegram opens the chat
//! then), but on Wayland it may not raise its own window: compositors only
//! let a program that has the keyboard do that, and pop-ups never take it.
//! So its window is focused through the compositor (HeroWM's IPC, or
//! sway's), found by app id; if it has none, the app is started from its
//! desktop entry.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

/// Brings the app forward, on a thread of its own. `acted`: its default
/// action was invoked, so it may be opening a window itself.
pub fn open(app: String, entry: String, acted: bool) {
    let _ = std::thread::Builder::new().name("heronotify-open".into()).stack_size(64 * 1024).spawn(move || {
        let names = names(&app, &entry);
        if names.is_empty() {
            return;
        }
        // A window: focused (again a moment later, after the app has
        // answered its action and maybe moved things around).
        if focus(&names) {
            if acted {
                std::thread::sleep(Duration::from_millis(350));
                focus(&names);
            }
            return;
        }
        // None: the app may be bringing one up from its tray.
        if acted {
            for _ in 0..10 {
                std::thread::sleep(Duration::from_millis(150));
                if focus(&names) {
                    return;
                }
            }
        }
        launch(&app, &entry);
    });
}

/// What the app's windows may be called: its desktop id and its name,
/// lowercase ("org.telegram.desktop", "telegram desktop").
fn names(app: &str, entry: &str) -> Vec<String> {
    // ("org.telegram.desktop" is an id, not a file name: both kept.)
    let mut v: Vec<String> = [entry, entry.trim_end_matches(".desktop"), app].iter().map(|s| s.trim().to_lowercase()).filter(|s| !s.is_empty()).collect();
    v.dedup();
    v
}

/// Whether window app id `id` is one of the app's.
fn matches(id: &str, names: &[String]) -> bool {
    let id = id.to_lowercase();
    !id.is_empty()
        && names.iter().any(|n| {
            id == *n
                || id.ends_with(&format!(".{n}"))
                || n.ends_with(&format!(".{id}"))
                || id.replace(['-', '_'], " ") == n.replace(['-', '_'], " ")
                || n.split_whitespace().next() == Some(id.as_str())
        })
}

/// Focuses a window of the app; false if it has none (or there's no
/// compositor to ask).
fn focus(names: &[String]) -> bool {
    if let Some(path) = std::env::var_os("FHTC_SOCKET_PATH") {
        return herowm_focus(std::path::Path::new(&path), names).unwrap_or(false);
    }
    if std::env::var_os("SWAYSOCK").is_some() {
        return sway_focus(names);
    }
    false
}

/// One request on a fresh HeroWM IPC connection; its reply.
fn herowm(path: &std::path::Path, request: &str) -> std::io::Result<serde_json::Value> {
    let mut s = UnixStream::connect(path)?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.write_all(request.as_bytes())?;
    s.write_all(b"\n")?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    Ok(serde_json::from_str(&line).unwrap_or_default())
}

fn herowm_focus(path: &std::path::Path, names: &[String]) -> std::io::Result<bool> {
    let reply = herowm(path, r#""windows""#)?;
    let Some(windows) = reply.get("windows").and_then(|w| w.as_object()) else { return Ok(false) };
    let found = windows.values().find(|w| w.get("app-id").and_then(|a| a.as_str()).is_some_and(|a| matches(a, names)));
    let Some(id) = found.and_then(|w| w.get("id")).and_then(|i| i.as_u64()) else { return Ok(false) };
    herowm(path, &format!(r#"{{"action":{{"focus-window":{{"window-id":{id}}}}}}}"#))?;
    Ok(true)
}

fn sway_focus(names: &[String]) -> bool {
    let Ok(out) = std::process::Command::new("swaymsg").args(["-t", "get_tree", "-r"]).output() else { return false };
    let tree: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_default();
    fn find(n: &serde_json::Value, names: &[String]) -> Option<u64> {
        if let Some(a) = n.get("app_id").and_then(|a| a.as_str()) {
            if matches(a, names) {
                return n.get("id").and_then(|i| i.as_u64());
            }
        }
        ["nodes", "floating_nodes"].iter().filter_map(|k| n.get(k).and_then(|v| v.as_array())).flatten().find_map(|c| find(c, names))
    }
    let Some(id) = find(&tree, names) else { return false };
    std::process::Command::new("swaymsg").arg(format!("[con_id={id}] focus")).output().is_ok_and(|o| o.status.success())
}

/// Starts the app from its desktop entry (by id, else by its name).
fn launch(app: &str, entry: &str) {
    let Some(exec) = desktop_exec(app, entry) else { return };
    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(format!("exec {exec}")).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    {
        use std::os::unix::process::CommandExt;
        // Its own session: it outlives the notification service.
        unsafe {
            cmd.pre_exec(|| {
                extern "C" {
                    fn setsid() -> std::ffi::c_int;
                }
                setsid();
                Ok(())
            })
        };
    }
    if let Ok(mut child) = cmd.spawn() {
        let _ = std::thread::Builder::new().stack_size(32 * 1024).spawn(move || child.wait());
    }
}

fn application_dirs() -> Vec<std::path::PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    std::iter::once(home).chain(dirs.split(':').map(std::path::PathBuf::from)).map(|d| d.join("applications")).collect()
}

/// The Exec= of the app's desktop entry, field codes (%U...) taken out.
fn desktop_exec(app: &str, entry: &str) -> Option<String> {
    let exec_of = |text: &str| -> Option<String> {
        let main = text.split("\n[").next().unwrap_or(text);
        let exec = main.lines().find_map(|l| l.strip_prefix("Exec="))?;
        let words: Vec<&str> = exec.split_whitespace().filter(|w| !(w.len() == 2 && w.starts_with('%'))).collect();
        (!words.is_empty()).then(|| words.join(" "))
    };
    let dirs = application_dirs();
    if !entry.is_empty() && !entry.contains('/') {
        // "org.telegram.desktop" → org.telegram.desktop.desktop; a file
        // name given whole as it is.
        for file in [format!("{entry}.desktop"), entry.to_string()] {
            if let Some(e) = dirs.iter().find_map(|d| std::fs::read_to_string(d.join(&file)).ok()).and_then(|t| exec_of(&t)) {
                return Some(e);
            }
        }
    }
    // By its name ("Telegram Desktop").
    let app = app.trim();
    if app.is_empty() {
        return None;
    }
    dirs.iter().filter_map(|d| std::fs::read_dir(d).ok()).flatten().flatten().find_map(|f| {
        let text = std::fs::read_to_string(f.path()).ok()?;
        let main = text.split("\n[").next().unwrap_or(&text);
        let name = main.lines().find_map(|l| l.strip_prefix("Name="))?;
        name.trim().eq_ignore_ascii_case(app).then(|| exec_of(&text)).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids() {
        let tg = names("Telegram Desktop", "org.telegram.desktop");
        assert!(matches("org.telegram.desktop", &tg));
        assert!(!matches("firefox", &tg));
        let ff = names("Firefox", "firefox");
        assert!(matches("firefox", &ff));
        assert!(matches("Firefox", &ff));
        let disc = names("discord", "");
        assert!(matches("discord", &disc));
        // A reverse-DNS id and a short app name.
        assert!(matches("org.gnome.Calendar", &names("Calendar", "")));
        assert!(!matches("", &ff));
    }
}
