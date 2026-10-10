//! The pop-ups: one window in a corner of the screen (a layer-shell
//! surface over windows, taking no keyboard) holding a stack of cards,
//! drawn by one widget. The service sends orders on stdin (show, close);
//! this says on stdout what the person did (closed one, picked an action,
//! opened one). It quits when the last card goes.
//!
//! Each card's time runs out along a bar at its bottom; pointing at the
//! card stops it there until the pointer leaves. A click opens what it's
//! about; only its × closes it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::time::{Duration, Instant};

use heroui::fltk::enums::{Align, Event as FlEvent, FrameType};
use heroui::fltk::frame::Frame;
use heroui::fltk::prelude::*;
use heroui::fltk::{app, draw};
use heroui::prelude::*;
use serde::{Deserialize, Serialize};

use crate::config::{Config, Position};
use crate::service::{DISMISSED, EXPIRED};
use crate::store::Note;

/// From the service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Order {
    Show(Note),
    Close(u32),
}

/// To the service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    Closed(u32, u32),
    Action(u32, String),
    /// Clicked: bring its app forward (start it if it isn't running).
    Open(u32),
}

fn say(e: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", serde_json::to_string(e).unwrap_or_default());
    let _ = out.flush();
}

/// Room around the cards for their shadow.
const SHADOW: i32 = 10;
const PAD: i32 = 16;
const ICON: i32 = 40;
const GAP: i32 = 10;
const BUTTON_H: i32 = 32;
/// The close button (big enough to hit).
const CLOSE: i32 = 28;
const BAR: i32 = 3;
const BODY_LINES: i32 = 4;

/// Where a card's parts are, from its top-left corner.
#[derive(Debug, Clone, Default, PartialEq)]
struct Layout {
    h: i32,
    text_x: i32,
    text_w: i32,
    summary: (i32, i32),
    body: (i32, i32),
    /// (x, y, w, h, key, label)
    buttons: Vec<(i32, i32, i32, i32, String, String)>,
    close: (i32, i32, i32, i32),
}

fn layout(n: &Note, w: i32) -> Layout {
    let t = heroui::theme::current();
    let text_x = PAD + if n.icon.is_empty() { 0 } else { ICON + 14 };
    let close = (w - 8 - CLOSE, 8, CLOSE, CLOSE);
    let text_w = (close.0 - 4 - text_x).max(40);
    // The app's name and the time come first, on one line.
    let mut y = PAD + 18;
    draw::set_font(t.bold_font(), t.font_size + 1);
    let sh = if n.summary.is_empty() { 0 } else { draw::wrap_measure(&n.summary, text_w, false).1.min((t.font_size + 1) * 3 + 8) };
    let summary = (y + 2, sh);
    y = summary.0 + sh;
    draw::set_font(t.font(), t.font_size);
    let line = draw::height();
    let bh = if n.body.is_empty() { 0 } else { draw::wrap_measure(&n.body, text_w, false).1.min(line * BODY_LINES) };
    let body = (y + if sh > 0 { 3 } else { 0 }, bh);
    y = body.0 + bh;
    let mut buttons = vec![];
    let shown: Vec<&(String, String)> = n.actions.iter().filter(|(k, l)| k != "default" && !l.is_empty()).take(3).collect();
    if !shown.is_empty() {
        y += 12;
        let span = w - text_x - PAD;
        let bw = (span - 8 * (shown.len() as i32 - 1)) / shown.len() as i32;
        for (i, (k, l)) in shown.into_iter().enumerate() {
            buttons.push((text_x + i as i32 * (bw + 8), y, bw, BUTTON_H, k.clone(), l.clone()));
        }
        y += BUTTON_H;
    }
    let h = (y + PAD + BAR).max(PAD * 2 + ICON + BAR);
    Layout { h, text_x, text_w, summary, body, buttons, close }
}

/// A card's time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Clock {
    total: Duration,
    /// What was left when it last stopped (or started).
    left: Duration,
    /// Running since (None: stopped, pointed at).
    since: Option<Instant>,
}

impl Clock {
    fn new(ms: u32) -> Option<Clock> {
        (ms > 0).then(|| {
            let d = Duration::from_millis(ms as u64);
            Clock { total: d, left: d, since: Some(Instant::now()) }
        })
    }

    fn left(&self, now: Instant) -> Duration {
        self.since.map_or(self.left, |s| self.left.saturating_sub(now.saturating_duration_since(s)))
    }

    /// 1.0 (all of it) to 0.0 (gone).
    fn fraction(&self, now: Instant) -> f64 {
        self.left(now).as_secs_f64() / self.total.as_secs_f64().max(0.001)
    }

    fn pause(&mut self, now: Instant) {
        if self.since.is_some() {
            self.left = self.left(now);
            self.since = None;
        }
    }

    fn resume(&mut self, now: Instant) {
        if self.since.is_none() {
            self.since = Some(now);
        }
    }
}

struct Card {
    note: Note,
    layout: Layout,
    /// None: stays until closed (urgent ones).
    clock: Option<Clock>,
}

/// What a click hit.
#[derive(Debug, Clone, PartialEq)]
enum Part {
    Card,
    Close,
    Button(String),
}

#[derive(Clone)]
enum Msg {
    Order(Order),
    /// The service is gone.
    Eof,
    Tick,
    Hover(Option<u32>),
    Click(u32, Part),
}

/// One card as the widget draws it.
#[derive(Clone)]
struct Drawn {
    note: Note,
    layout: Layout,
    y: i32,
    clock: Option<Clock>,
}

struct Popups {
    config: Config,
    cards: Vec<Card>,
    waiting: VecDeque<Note>,
    hovered: Option<u32>,
    view: Rc<RefCell<Vec<Drawn>>>,
}

impl Popups {
    fn width(&self) -> i32 {
        self.config.width.clamp(240, 800)
    }

    /// Newest nearest the corner's edge.
    fn bottom_up(&self) -> bool {
        matches!(self.config.position, Position::BottomLeft | Position::BottomRight | Position::Bottom)
    }

    /// How long a notification stays: urgent ones until closed; apps asking
    /// for "never" get the usual time (only the × or a click closes those
    /// otherwise, which is what made pop-ups pile up).
    fn time(&self, n: &Note) -> u32 {
        if n.urgency >= 2 {
            0
        } else if n.timeout == 0 {
            self.config.timeout.saturating_mul(1000)
        } else {
            n.timeout
        }
    }

    fn show(&mut self, n: Note) {
        let clock = Clock::new(self.time(&n));
        let layout = layout(&n, self.width());
        if let Some(c) = self.cards.iter_mut().find(|c| c.note.id == n.id) {
            // Replaced (a download's progress...): its time starts over.
            *c = Card { note: n, layout, clock };
        } else if let Some(w) = self.waiting.iter_mut().find(|w| w.id == n.id) {
            *w = n;
        } else if self.cards.len() < self.config.max_shown.max(1) {
            self.cards.insert(0, Card { note: n, layout, clock });
        } else {
            self.waiting.push_back(n);
        }
        self.sync_hover();
    }

    fn close(&mut self, id: u32, reason: Option<u32>) {
        if let Some(i) = self.cards.iter().position(|c| c.note.id == id) {
            self.cards.remove(i);
            if let Some(r) = reason {
                say(&Event::Closed(id, r));
            }
            if self.hovered == Some(id) {
                self.hovered = None;
            }
            if let Some(next) = self.waiting.pop_front() {
                self.show(next);
            }
        } else {
            self.waiting.retain(|w| w.id != id);
        }
    }

    /// Only the card pointed at stands still.
    fn sync_hover(&mut self) {
        let now = Instant::now();
        for c in &mut self.cards {
            if let Some(k) = &mut c.clock {
                if Some(c.note.id) == self.hovered {
                    k.pause(now);
                } else {
                    k.resume(now);
                }
            }
        }
    }

    /// Lays the cards out and sizes the window to them; quits when none.
    fn refresh(&mut self) -> Task<Msg> {
        if self.cards.is_empty() && self.waiting.is_empty() {
            return Task::quit();
        }
        let mut order: Vec<&Card> = self.cards.iter().collect();
        if self.bottom_up() {
            order.reverse();
        }
        let mut y = SHADOW;
        let mut view = vec![];
        for c in order {
            view.push(Drawn { note: c.note.clone(), layout: c.layout.clone(), y, clock: c.clock });
            y += c.layout.h + GAP;
        }
        *self.view.borrow_mut() = view;
        heroui::resize_window(self.width() + 2 * SHADOW, y - GAP + SHADOW);
        if let Some(mut w) = app::first_window() {
            heroui::widgets::repaint(&mut w);
        }
        Task::none()
    }

    /// The clocks as drawn (after pausing or resuming one).
    fn sync_view(&mut self) {
        let mut v = self.view.borrow_mut();
        for d in v.iter_mut() {
            if let Some(c) = self.cards.iter().find(|c| c.note.id == d.note.id) {
                d.clock = c.clock;
            }
        }
    }
}

impl App for Popups {
    type Message = Msg;

    fn update(&mut self, msg: Msg) -> Task<Msg> {
        match msg {
            Msg::Order(Order::Show(n)) => self.show(n),
            Msg::Order(Order::Close(id)) => self.close(id, None),
            Msg::Eof => return Task::quit(),
            Msg::Tick => {
                let now = Instant::now();
                let gone: Vec<u32> = self.cards.iter().filter(|c| c.clock.is_some_and(|k| k.left(now).is_zero())).map(|c| c.note.id).collect();
                if gone.is_empty() {
                    // Without animations the bars move with this tick.
                    if !heroui::anim::enabled() {
                        if let Some(mut w) = app::first_window() {
                            heroui::widgets::repaint(&mut w);
                        }
                    }
                    return Task::none();
                }
                for id in gone {
                    self.close(id, Some(EXPIRED));
                }
            }
            Msg::Hover(id) => {
                if self.hovered != id {
                    self.hovered = id;
                    self.sync_hover();
                    self.sync_view();
                }
                return Task::none();
            }
            Msg::Click(id, part) => {
                let Some(card) = self.cards.iter().find(|c| c.note.id == id) else { return Task::none() };
                let has_default = card.note.actions.iter().any(|(k, _)| k == "default");
                match part {
                    Part::Close => {}
                    Part::Button(key) => {
                        say(&Event::Action(id, key));
                        say(&Event::Open(id));
                    }
                    Part::Card => {
                        if has_default {
                            say(&Event::Action(id, "default".into()));
                        }
                        say(&Event::Open(id));
                    }
                }
                self.close(id, Some(DISMISSED));
            }
        }
        self.refresh()
    }

    fn view(&self) -> Element<Self, Msg> {
        let view = self.view.clone();
        Element::new(move |ctx| {
            let mut f = Frame::default();
            f.set_frame(FrameType::NoBox);
            // The pointer: inside the window, where, and what it's on.
            let pointer: Rc<Cell<Option<(i32, i32)>>> = Rc::default();
            let hover: Rc<RefCell<(Option<u32>, Option<Part>)>> = Rc::default();
            {
                let (view, hover) = (view.clone(), hover.clone());
                f.draw(move |f| {
                    let h = hover.borrow().clone();
                    let now = Instant::now();
                    for d in view.borrow().iter() {
                        let part = if h.0 == Some(d.note.id) { h.1.clone() } else { None };
                        draw_card(d, f.x() + SHADOW, f.y() + d.y, f.w() - 2 * SHADOW, part, now);
                    }
                });
            }
            // The bars run down smoothly: a frame each time the screen
            // draws one, while any is running.
            {
                let (view, mut w) = (view.clone(), f.clone());
                heroui::anim::each_frame(move |_| {
                    if w.was_deleted() {
                        return false;
                    }
                    if view.borrow().iter().any(|d| d.clock.is_some_and(|k| k.since.is_some())) {
                        heroui::widgets::repaint(&mut w);
                    }
                    true
                });
            }
            let at = {
                let view = view.clone();
                move |f: &Frame, (x, y): (i32, i32)| -> (Option<u32>, Option<Part>) {
                    let (x, y) = (x - f.x() - SHADOW, y - f.y());
                    for d in view.borrow().iter() {
                        let l = &d.layout;
                        if y >= d.y && y < d.y + l.h && x >= 0 && x < f.w() - 2 * SHADOW {
                            let cy = y - d.y;
                            let inside = |r: (i32, i32, i32, i32)| x >= r.0 && x < r.0 + r.2 && cy >= r.1 && cy < r.1 + r.3;
                            if inside(l.close) {
                                return (Some(d.note.id), Some(Part::Close));
                            }
                            for b in &l.buttons {
                                if inside((b.0, b.1, b.2, b.3)) {
                                    return (Some(d.note.id), Some(Part::Button(b.4.clone())));
                                }
                            }
                            return (Some(d.note.id), Some(Part::Card));
                        }
                    }
                    (None, None)
                }
            };
            let emit = ctx.emitter();
            let set_hover = {
                let (hover, emit) = (hover.clone(), emit.clone());
                move |f: &mut Frame, now: (Option<u32>, Option<Part>)| {
                    let before = hover.replace(now.clone());
                    if before != now {
                        if before.0 != now.0 {
                            emit(Msg::Hover(now.0));
                        }
                        heroui::widgets::repaint(f);
                    }
                }
            };
            // Cards move under a resting pointer as others come and go:
            // what it's on is worked out again after each change.
            {
                let (pointer, at, set_hover) = (pointer.clone(), at.clone(), set_hover.clone());
                let mut w = f.clone();
                ctx.bind(move |_: &Popups| {
                    let now = pointer.get().map_or((None, None), |p| at(&w, p));
                    set_hover(&mut w, now);
                });
            }
            f.handle(move |f, ev| match ev {
                FlEvent::Enter | FlEvent::Move => {
                    let p = (app::event_x(), app::event_y());
                    pointer.set(Some(p));
                    set_hover(f, at(f, p));
                    true
                }
                FlEvent::Leave => {
                    pointer.set(None);
                    set_hover(f, (None, None));
                    true
                }
                FlEvent::Push => true,
                FlEvent::Released => {
                    if app::event_mouse_button() == app::MouseButton::Left {
                        if let (Some(id), Some(part)) = at(f, (app::event_x(), app::event_y())) {
                            emit(Msg::Click(id, part));
                        }
                    }
                    true
                }
                _ => false,
            });
            f.as_base_widget()
        })
    }

    fn subscriptions(&self) -> Vec<Subscription<Msg>> {
        vec![
            Subscription::every(Duration::from_millis(100), Msg::Tick),
            Subscription::worker(|tx| {
                for line in std::io::stdin().lock().lines().map_while(Result::ok) {
                    if let Ok(o) = serde_json::from_str::<Order>(&line) {
                        if !tx.send(Msg::Order(o)) {
                            return;
                        }
                    }
                }
                tx.send(Msg::Eof);
            }),
        ]
    }

    fn close_requested(&self) -> Option<Msg> {
        Some(Msg::Eof)
    }
}

fn draw_card(d: &Drawn, x: i32, y: i32, w: i32, hover: Option<Part>, now: Instant) {
    let (n, l) = (&d.note, &d.layout);
    let t = heroui::theme::current();
    let mix = heroui::widgets::mix;
    let r = t.radius.clamp(10, 18) as f64;
    let (fx, fy, fw, fh) = (x as f64, y as f64, w as f64, l.h as f64);
    // A soft shadow, then the edge, then the face.
    for (grow, alpha) in [(8.0, 0.04), (5.0, 0.06), (2.5, 0.09)] {
        heroui::fx::fill_rounded(fx - grow, fy - grow + 2.0, fw + 2.0 * grow, fh + 2.0 * grow, r + grow, t.text.darker(), alpha);
    }
    heroui::fx::fill_rounded(fx, fy, fw, fh, r, mix(t.surface, t.text, 0.12), 1.0);
    let face = if hover.is_some() { mix(t.surface, t.text, 0.035) } else { t.surface };
    heroui::fx::fill_rounded(fx + 1.0, fy + 1.0, fw - 2.0, fh - 2.0, r - 1.0, face, 1.0);
    if n.urgency >= 2 {
        heroui::fx::fill_rounded(fx + 1.0, fy + 14.0, 3.0, fh - 28.0, 1.5, t.accent, 1.0);
    }
    if !n.icon.is_empty() && !heroui::icons::draw(&n.icon, x + PAD, y + PAD + 2, ICON, t.text) {
        heroui::fx::fill_rounded((x + PAD) as f64, (y + PAD + 2) as f64, ICON as f64, ICON as f64, 10.0, t.surface_alt, 1.0);
        heroui::icons::draw("bell", x + PAD + 10, y + PAD + 12, ICON - 20, t.text_dim);
    }
    let tx = x + l.text_x;
    // App · time
    let app = if n.app.is_empty() { "Notification" } else { &n.app };
    let when = crate::text::ago(n.time, crate::store::now());
    draw::set_font(t.bold_font(), t.font_size - 2);
    draw::set_draw_color(mix(t.text, t.text_dim, 0.5));
    draw::push_clip(tx, y + PAD - 2, l.text_w, 18);
    draw::draw_text2(&format!("{app}  ·  {when}"), tx, y + PAD - 2, l.text_w, 18, Align::Left | Align::Inside);
    draw::pop_clip();
    if l.summary.1 > 0 {
        draw::set_font(t.bold_font(), t.font_size + 1);
        draw::set_draw_color(t.text);
        draw::push_clip(tx, y + l.summary.0, l.text_w, l.summary.1);
        draw::draw_text2(&n.summary, tx, y + l.summary.0, l.text_w, l.summary.1, Align::Left | Align::Top | Align::Wrap | Align::Inside);
        draw::pop_clip();
    }
    if l.body.1 > 0 {
        draw::set_font(t.font(), t.font_size);
        draw::set_draw_color(mix(t.text, t.text_dim, 0.35));
        draw::push_clip(tx, y + l.body.0, l.text_w, l.body.1);
        draw::draw_text2(&n.body, tx, y + l.body.0, l.text_w, l.body.1, Align::Left | Align::Top | Align::Wrap | Align::Inside);
        draw::pop_clip();
    }
    for (bx, by, bw, bh, key, label) in &l.buttons {
        let lit = hover == Some(Part::Button(key.clone()));
        let c = if lit { mix(t.surface_alt, t.text, 0.1) } else { t.surface_alt };
        heroui::fx::fill_rounded((x + bx) as f64, (y + by) as f64, *bw as f64, *bh as f64, t.radius.min(10) as f64, c, 1.0);
        draw::set_font(t.font(), t.font_size);
        draw::set_draw_color(t.text);
        draw::draw_text2(label, x + bx + 4, y + by, bw - 8, *bh, Align::Center | Align::Clip);
    }
    // The close button: always there, lit when pointed at.
    let (cx, cy, cw, ch) = l.close;
    let lit = hover == Some(Part::Close);
    if lit {
        heroui::fx::fill_rounded((x + cx) as f64, (y + cy) as f64, cw as f64, ch as f64, (cw / 2) as f64, mix(t.surface_alt, t.text, 0.08), 1.0);
    }
    draw::set_draw_color(if lit { t.text } else { t.text_dim });
    draw::set_line_style(draw::LineStyle::Solid, 2);
    let (mx, my, k) = (x + cx + cw / 2, y + cy + ch / 2, 5);
    draw::draw_line(mx - k, my - k, mx + k, my + k);
    draw::draw_line(mx - k, my + k, mx + k, my - k);
    draw::set_line_style(draw::LineStyle::Solid, 0);
    // Time left, running down along the bottom.
    if let Some(c) = d.clock {
        let inset = r;
        let (bx, by, span) = (fx + inset, fy + fh - 1.0 - BAR as f64 - 5.0, fw - 2.0 * inset);
        heroui::fx::fill_rounded(bx, by, span, BAR as f64, BAR as f64 / 2.0, mix(t.surface, t.text, 0.08), 1.0);
        let left = span * c.fraction(now).clamp(0.0, 1.0);
        if left > 0.5 {
            let color = if c.since.is_none() { mix(t.accent, t.text_dim, 0.45) } else { t.accent };
            heroui::fx::fill_rounded(bx, by, left, BAR as f64, BAR as f64 / 2.0, color, 1.0);
        }
    }
}

pub fn run() {
    let config = Config::load();
    let corner = match config.position {
        Position::TopRight => heroui::Corner::TopRight,
        Position::TopLeft => heroui::Corner::TopLeft,
        Position::BottomRight => heroui::Corner::BottomRight,
        Position::BottomLeft => heroui::Corner::BottomLeft,
        Position::Top => heroui::Corner::Top,
        Position::Bottom => heroui::Corner::Bottom,
    };
    let width = config.width.clamp(240, 800) + 2 * SHADOW;
    let app = Popups { config, cards: vec![], waiting: VecDeque::new(), hovered: None, view: Rc::default() };
    let settings = Settings::new("Notifications").class("heronotify").kind(WindowKind::Notification).size(width, 1).corner(corner, 2).decorated(false).resizable(false).above(true).sticky(true).skip_taskbar(true).transparent(true);
    if let Err(e) = heroui::run(app, settings) {
        eprintln!("heronotify: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_stop_and_go_on() {
        let t0 = Instant::now();
        let mut c = Clock { total: Duration::from_secs(6), left: Duration::from_secs(6), since: Some(t0) };
        assert!((c.fraction(t0 + Duration::from_secs(3)) - 0.5).abs() < 1e-6);
        c.pause(t0 + Duration::from_secs(3));
        // Pointed at: it stays at half however long.
        assert!((c.fraction(t0 + Duration::from_secs(60)) - 0.5).abs() < 1e-6);
        c.resume(t0 + Duration::from_secs(60));
        assert_eq!(c.left(t0 + Duration::from_secs(62)), Duration::from_secs(1));
        assert!(c.left(t0 + Duration::from_secs(70)).is_zero());
        assert!(Clock::new(0).is_none());
    }
}
