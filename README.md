# HeroNotify

The notifications of [HeroineOS](https://heroineos.github.io): pop-ups for every app that sends
desktop notifications, a history (shown by HeroBar's `notifications` module), and
do-not-disturb with exceptions and schedules.

Apps send notifications through the freedesktop D-Bus interface
(`org.freedesktop.Notifications`): Firefox, Chromium, chat apps, `notify-send`, scripts. Every
one of them works with HeroNotify unchanged.

## What runs when

| | Memory | Runs |
|---|---|---|
| The service (`heronotify`) | 1.0 MB | started by D-Bus when an app sends a notification; leaves 30 s after the last pop-up |
| The pop-ups (`heronotify --popups`) | 2.7 MB | only while pop-ups are on screen |

So between notifications, nothing of it is running. (Measured on sway; private memory.)

Pop-ups stack in a corner of the screen over windows (a layer-shell surface on Wayland) and
take no keyboard. Click one to open what it's about (the app's default action) or close it; its
buttons are the app's actions; the × on the one pointed at, or any other mouse button, closes
it. Pointing at one keeps it on screen. Urgent ones stay until closed.

## Do not disturb

On by hand (`heronotify dnd on`, or HeroBar's switch) or by schedules. While it's on,
notifications go to the history without popping up, except urgent ones (alarms, low battery)
and apps you let through.

## Settings

`~/.config/hero/notifications.toml` (`heronotify --print-default-config` shows them all), read
for each notification:

```toml
position = "top-right"      # top-left, bottom-right, bottom-left, top, bottom
timeout = 6                 # seconds, unless the app says (0: until closed)
max-shown = 3
width = 360

[do-not-disturb]
on = false
urgent = true               # urgent ones still pop up
allow = ["Alarm Clock"]     # apps that still pop up (name or desktop id)
schedule = [
  { from = "22:00", to = "07:00" },
  { from = "09:00", to = "12:00", days = ["sat", "sun"] },
]

[history]
keep = 100
```

## Commands

```
heronotify list [--json]    the history, newest first
heronotify unread           how many are new
heronotify seen             mark the history seen
heronotify clear [ID]       empty the history, or take one out
heronotify dnd [on|off|toggle]
```

The history is `~/.local/state/hero/notifications.jsonl` (one notification per line, newest
last) with `notifications.json` (next id, newest seen); HeroBar reads them. Pictures apps send
as pixels are kept in `~/.cache/hero/notifications` while their notification is in the history.

## Install

From the [releases](https://github.com/HeroineOS/HeroNotify/releases) (or `heroupdate`). Only one
notification service can run: if another one (mako, dunst, swaync, a desktop's own) starts
first, stop it; HeroNotify says so and exits.

## Build

```
cargo build --release
```

Needs the Rust toolchain and the build dependencies of FLTK and D-Bus (see
`.github/workflows/build.yml`).
