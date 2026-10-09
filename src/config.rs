//! `~/.config/hero/notifications.toml`: where pop-ups show, how long, and
//! do-not-disturb (on/off, apps let through, schedules).

use std::path::PathBuf;

use serde::Deserialize;

pub const DEFAULT: &str = include_str!("../res/notifications.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Position {
    #[default]
    TopRight,
    TopLeft,
    BottomRight,
    BottomLeft,
    Top,
    Bottom,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    pub position: Position,
    /// Seconds a pop-up stays when the app doesn't say (0: until closed).
    pub timeout: u32,
    /// Pop-ups on screen at once; more wait their turn.
    pub max_shown: usize,
    pub width: i32,
    pub do_not_disturb: Dnd,
    pub history: History,
}

impl Default for Config {
    fn default() -> Self {
        Config { position: Position::TopRight, timeout: 6, max_shown: 3, width: 360, do_not_disturb: Dnd::default(), history: History::default() }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Dnd {
    /// On by hand (the bar's switch).
    pub on: bool,
    /// Urgent notifications (alarms, low battery) still pop up.
    pub urgent: bool,
    /// Apps that still pop up, by name or desktop id (any case).
    pub allow: Vec<String>,
    /// Times it's on by itself.
    pub schedule: Vec<Quiet>,
}

impl Default for Dnd {
    fn default() -> Self {
        Dnd { on: false, urgent: true, allow: vec![], schedule: vec![] }
    }
}

/// Do-not-disturb from `from` to `to` ("22:00" to "07:00" goes past
/// midnight), on `days` (empty: every day; "mon".."sun", the day it
/// starts on).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quiet {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub days: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct History {
    /// Notifications kept (the oldest go first).
    pub keep: usize,
}

impl Default for History {
    fn default() -> Self {
        History { keep: 100 }
    }
}

pub fn path() -> PathBuf {
    crate::dirs::config().join("hero").join("notifications.toml")
}

impl Config {
    /// The file's settings; defaults where it's missing or wrong (said on
    /// stderr).
    pub fn load() -> Config {
        match std::fs::read_to_string(path()) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                eprintln!("heronotify: {}: {e}", path().display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    /// Whether do-not-disturb is on at `now` (local minutes since midnight,
    /// weekday 0 = Monday): by hand or by a schedule.
    pub fn quiet_at(&self, minutes: u32, weekday: u32) -> bool {
        self.do_not_disturb.on || self.do_not_disturb.schedule.iter().any(|q| q.covers(minutes, weekday))
    }

    /// Whether a notification from `app` (or desktop id `entry`) at
    /// `urgency` pops up while it's quiet.
    pub fn lets_through(&self, app: &str, entry: &str, urgency: u8) -> bool {
        let d = &self.do_not_disturb;
        (d.urgent && urgency >= 2) || d.allow.iter().any(|a| a.eq_ignore_ascii_case(app) || (!entry.is_empty() && a.eq_ignore_ascii_case(entry)))
    }
}

const DAYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

/// "22:30" → minutes since midnight.
fn hhmm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

fn day(s: &str) -> Option<u32> {
    let s = s.trim().to_ascii_lowercase();
    DAYS.iter().position(|d| s.starts_with(d)).map(|i| i as u32)
}

impl Quiet {
    fn covers(&self, now: u32, weekday: u32) -> bool {
        let (Some(from), Some(to)) = (hhmm(&self.from), hhmm(&self.to)) else { return false };
        let on_day = |d: u32| self.days.is_empty() || self.days.iter().filter_map(|s| day(s)).any(|x| x == d);
        if from <= to {
            from <= now && now < to && on_day(weekday)
        } else {
            // Past midnight: the evening part on its day, the morning part
            // on the day after.
            (now >= from && on_day(weekday)) || (now < to && on_day((weekday + 6) % 7))
        }
    }
}

/// Sets do-not-disturb by hand, keeping the file's comments.
pub fn set_dnd(on: bool) -> Result<(), String> {
    let p = path();
    let text = std::fs::read_to_string(&p).unwrap_or_else(|_| DEFAULT.to_string());
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{}: {e}", p.display()))?;
    if !doc.contains_table("do-not-disturb") {
        doc["do-not-disturb"] = toml_edit::table();
    }
    doc["do-not-disturb"]["on"] = toml_edit::value(on);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, doc.to_string()).map_err(|e| format!("{}: {e}", p.display()))
}

/// Local time now: (minutes since midnight, weekday with 0 = Monday).
pub fn local_now() -> (u32, u32) {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: std::ffi::c_long,
        zone: *const std::ffi::c_char,
    }
    extern "C" {
        fn time(t: *mut std::ffi::c_long) -> std::ffi::c_long;
        fn localtime_r(t: *const std::ffi::c_long, tm: *mut Tm) -> *mut Tm;
    }
    let mut tm: Tm = unsafe { std::mem::zeroed() };
    let t = unsafe { time(std::ptr::null_mut()) };
    unsafe { localtime_r(&t, &mut tm) };
    ((tm.hour * 60 + tm.min) as u32, ((tm.wday + 6) % 7) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(from: &str, to: &str, days: &[&str]) -> Quiet {
        Quiet { from: from.into(), to: to.into(), days: days.iter().map(|d| d.to_string()).collect() }
    }

    #[test]
    fn schedules() {
        let night = quiet("22:00", "07:00", &[]);
        assert!(night.covers(23 * 60, 0));
        assert!(night.covers(6 * 60 + 59, 3));
        assert!(!night.covers(7 * 60, 3));
        assert!(!night.covers(12 * 60, 3));
        let work = quiet("9:00", "17:30", &["mon", "Tuesday"]);
        assert!(work.covers(10 * 60, 0));
        assert!(work.covers(17 * 60 + 29, 1));
        assert!(!work.covers(10 * 60, 2));
        // Friday night into Saturday morning, set for Friday only.
        let fri = quiet("23:00", "08:00", &["fri"]);
        assert!(fri.covers(23 * 60 + 30, 4));
        assert!(fri.covers(7 * 60, 5));
        assert!(!fri.covers(7 * 60, 4));
        assert!(!quiet("25:00", "07:00", &[]).covers(0, 0));
    }

    #[test]
    fn default_file_parses() {
        let c: Config = toml::from_str(DEFAULT).unwrap();
        assert_eq!(c.timeout, 6);
        assert!(!c.do_not_disturb.on);
    }

    #[test]
    fn exceptions() {
        let c: Config = toml::from_str("[do-not-disturb]\non = true\nallow = [\"Alarm Clock\", \"org.gnome.Calendar\"]\n").unwrap();
        assert!(c.quiet_at(600, 0));
        assert!(c.lets_through("alarm clock", "", 1));
        assert!(c.lets_through("Calendar", "org.gnome.Calendar", 1));
        assert!(!c.lets_through("Firefox", "firefox", 1));
        assert!(c.lets_through("Firefox", "firefox", 2));
    }
}
