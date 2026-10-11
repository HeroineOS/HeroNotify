//! Which notification service shows notifications, and making it
//! HeroNotify. Only one program can hold the notification name on the
//! session bus; several (HeroNotify, XFCE's, mako, dunst...) may offer to,
//! and D-Bus picks one of the system's service files unpredictably. A
//! service file in the user's own directory wins over those, so `use`
//! writes one, has D-Bus read it, and stops the other program.

use std::path::PathBuf;
use std::time::Duration;

use dbus::blocking::Connection;

const NAME: &str = crate::service::NAME;
const BUS: &str = "org.freedesktop.DBus";

/// Who holds the notification name now.
pub enum Holder {
    /// No one (D-Bus starts one with the next notification).
    Nobody,
    HeroNotify,
    /// Another program: its name ("xfce4-notifyd") and process.
    Other(String, u32),
}

fn bus() -> Result<Connection, String> {
    Connection::new_session().map_err(|e| format!("no session bus ({e})"))
}

fn dbus_proxy(c: &Connection) -> dbus::blocking::Proxy<'_, &Connection> {
    c.with_proxy(BUS, "/org/freedesktop/DBus", Duration::from_secs(5))
}

pub fn holder() -> Result<Holder, String> {
    let c = bus()?;
    let p = dbus_proxy(&c);
    let (has,): (bool,) = p.method_call(BUS, "NameHasOwner", (NAME,)).map_err(|e| e.to_string())?;
    if !has {
        return Ok(Holder::Nobody);
    }
    let (pid,): (u32,) = p.method_call(BUS, "GetConnectionUnixProcessID", (NAME,)).map_err(|e| e.to_string())?;
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default().trim().to_string();
    if comm == "heronotify" {
        return Ok(Holder::HeroNotify);
    }
    // Its own name, else the one it gives.
    let name = if comm.is_empty() {
        let n = c.with_proxy(NAME, "/org/freedesktop/Notifications", Duration::from_secs(3));
        n.method_call::<(String, String, String, String), _, _, _>(NAME, "GetServerInformation", ()).map(|r| r.0).unwrap_or_else(|_| "another program".into())
    } else {
        comm
    };
    Ok(Holder::Other(name, pid))
}

/// The user's service file for the name.
fn user_file() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
        .join("dbus-1/services/org.freedesktop.Notifications.service")
}

/// Whether D-Bus is set to start HeroNotify whatever else is installed.
pub fn chosen() -> bool {
    std::fs::read_to_string(user_file()).is_ok_and(|t| t.lines().any(|l| l.starts_with("Exec=") && l.contains("heronotify")))
}

/// Other programs offering to show notifications (their service files).
pub fn rivals() -> Vec<String> {
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
    let mut found = vec![];
    for d in dirs.split(':') {
        let Ok(rd) = std::fs::read_dir(PathBuf::from(d).join("dbus-1/services")) else { continue };
        for f in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(f.path()) else { continue };
            if text.lines().any(|l| l.trim() == format!("Name={NAME}")) && !text.contains("heronotify") {
                let exec = text.lines().find_map(|l| l.strip_prefix("Exec=")).unwrap_or("");
                let prog = exec.split_whitespace().next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
                if !prog.is_empty() && !found.contains(&prog) {
                    found.push(prog);
                }
            }
        }
    }
    found
}

/// Makes HeroNotify the notification service: chosen by D-Bus from now
/// on, and the program holding the name now stopped. Says what it did.
pub fn take_over() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = user_file();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(&file, format!("[D-BUS Service]\nName={NAME}\nExec={}\n", exe.display())).map_err(|e| format!("{}: {e}", file.display()))?;
    let c = bus()?;
    let _: () = dbus_proxy(&c).method_call(BUS, "ReloadConfig", ()).map_err(|e| e.to_string())?;
    let mut stopped = None;
    if let Holder::Other(name, pid) = holder()? {
        stop(pid);
        // It lets go of the name as it exits.
        for _ in 0..30 {
            std::thread::sleep(Duration::from_millis(100));
            if !matches!(holder()?, Holder::Other(..)) {
                break;
            }
        }
        if let Holder::Other(n, _) = holder()? {
            return Err(format!("{n} is still running and won't stop"));
        }
        stopped = Some(name);
    }
    // Started now (D-Bus picks the new file), and checked.
    let n = c.with_proxy(NAME, "/org/freedesktop/Notifications", Duration::from_secs(10));
    let (server, _, _, _): (String, String, String, String) = n.method_call(NAME, "GetServerInformation", ()).map_err(|e| e.to_string())?;
    if server != "HeroNotify" {
        return Err(format!("{server} answered instead"));
    }
    Ok(match stopped {
        Some(name) => format!("HeroNotify shows notifications now ({name} stopped)"),
        None => "HeroNotify shows notifications now".into(),
    })
}

/// Stops a program: through systemd if it runs as a user service
/// (else systemd may start it again), else with SIGTERM.
fn stop(pid: u32) {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
    let unit = cgroup.lines().filter_map(|l| l.rsplit('/').next()).find(|u| u.ends_with(".service") && !u.starts_with("app-") && *u != "dbus.service");
    if let Some(u) = unit {
        let ok = std::process::Command::new("systemctl").args(["--user", "stop", u]).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
        if ok {
            return;
        }
    }
    extern "C" {
        fn kill(pid: std::ffi::c_int, sig: std::ffi::c_int) -> std::ffi::c_int;
    }
    const SIGTERM: std::ffi::c_int = 15;
    unsafe { kill(pid as std::ffi::c_int, SIGTERM) };
}
