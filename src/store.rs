//! The history: `~/.local/state/hero/notifications.jsonl`, one notification
//! per line (newest last), and `notifications.json` beside it: the next id
//! and the newest id seen in the history (fewer are unread). HeroBar reads
//! both; the service, `heronotify clear`/`seen` and HeroBar's buttons (through
//! those commands) change them, each holding `notifications.lock`.

use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One notification, as kept and as shown.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    pub id: u32,
    /// The app's name, as it gave it.
    pub app: String,
    /// Its desktop id ("firefox"), when it said.
    pub entry: String,
    /// An icon name or picture file.
    pub icon: String,
    pub summary: String,
    pub body: String,
    /// (key, label) pairs; "default" is clicking the notification.
    pub actions: Vec<(String, String)>,
    /// 0 low, 1 normal, 2 critical.
    pub urgency: u8,
    /// Unix seconds.
    pub time: u64,
    /// Milliseconds on screen (0: until closed).
    pub timeout: u32,
    /// Kept out of the history (the app said it's passing).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub transient: bool,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub next_id: u32,
    /// The newest id the history list was opened on.
    pub seen: u32,
}

fn history_path() -> PathBuf {
    crate::dirs::state().join("notifications.jsonl")
}

fn state_path() -> PathBuf {
    crate::dirs::state().join("notifications.json")
}

/// Held while changing the files.
pub struct Lock(#[allow(dead_code)] std::fs::File);

pub fn lock() -> std::io::Result<Lock> {
    extern "C" {
        fn flock(fd: std::ffi::c_int, op: std::ffi::c_int) -> std::ffi::c_int;
    }
    const LOCK_EX: std::ffi::c_int = 2;
    std::fs::create_dir_all(crate::dirs::state())?;
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(crate::dirs::state().join("notifications.lock"))?;
    use std::os::fd::AsRawFd;
    unsafe { flock(f.as_raw_fd(), LOCK_EX) };
    Ok(Lock(f))
}

pub fn state() -> State {
    std::fs::read_to_string(state_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_state(s: &State) -> std::io::Result<()> {
    write_atomic(&state_path(), serde_json::to_string(s).unwrap_or_default().as_bytes())
}

/// The history, oldest first (broken lines skipped).
pub fn history() -> Vec<Note> {
    std::fs::read_to_string(history_path()).unwrap_or_default().lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

fn save_history(notes: &[Note]) -> std::io::Result<()> {
    let mut text = String::new();
    for n in notes {
        text += &serde_json::to_string(n).unwrap_or_default();
        text.push('\n');
    }
    write_atomic(&history_path(), text.as_bytes())
}

/// Readers never see half a file.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::File::create(&tmp)?.write_all(bytes)?;
    std::fs::rename(tmp, path)
}

/// A new id (never reused, even after the history is cleared).
pub fn new_id() -> u32 {
    let _l = lock();
    let mut s = state();
    // Ids start at 1 (0 means "none" in the protocol).
    let id = s.next_id.max(1);
    s.next_id = id.wrapping_add(1).max(1);
    let _ = save_state(&s);
    id
}

/// Adds `note` to the history (in place of the one with its id, if any),
/// keeping the newest `keep`.
pub fn record(note: &Note, keep: usize) {
    if note.transient {
        return;
    }
    let _l = lock();
    let mut h = history();
    match h.iter_mut().find(|n| n.id == note.id) {
        Some(old) => *old = note.clone(),
        None => h.push(note.clone()),
    }
    let drop = h.len().saturating_sub(keep.max(1));
    let gone: Vec<Note> = h.drain(..drop).collect();
    let _ = save_history(&h);
    forget_images(&gone, &h);
}

/// Removes notification `id` from the history, or all of it.
pub fn clear(id: Option<u32>) -> std::io::Result<()> {
    let _l = lock()?;
    let mut h = history();
    let (gone, kept): (Vec<Note>, Vec<Note>) = h.drain(..).partition(|n| id.is_none_or(|id| n.id == id));
    save_history(&kept)?;
    forget_images(&gone, &kept);
    Ok(())
}

/// Marks the whole history as seen.
pub fn seen() -> std::io::Result<()> {
    let _l = lock()?;
    let mut s = state();
    s.seen = history().iter().map(|n| n.id).max().unwrap_or(s.seen).max(s.seen);
    save_state(&s)
}

/// Pictures apps sent as pixels go with the last notification using them.
fn forget_images(gone: &[Note], kept: &[Note]) {
    let dir = crate::dirs::images();
    for n in gone {
        if std::path::Path::new(&n.icon).starts_with(&dir) && !kept.iter().any(|k| k.icon == n.icon) {
            let _ = std::fs::remove_file(&n.icon);
        }
    }
}

/// Unix seconds now.
pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
