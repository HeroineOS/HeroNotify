//! The pop-ups: one window in a corner of the screen (a layer-shell
//! surface over windows, taking no keyboard) holding a stack of cards,
//! drawn by one widget. The service sends orders on stdin (show, close);
//! this says on stdout what the person did (closed one, picked an action).
//! It quits when the last card goes.

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
}

fn say(e: &Event) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", serde_json::to_string(e).unwrap_or_default());
    let _ = out.flush();
}

const PAD: i32 = 14;
const ICON: i32 = 36;
const GAP: i32 = 8;
const BUTTON_H: i32 = 30;
const CLOSE: i32 = 20;
const BODY_LINES: i32 = 5;

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
    let text_x = PAD + if n.icon.is_empty() { 0 } else { ICON + 12 };
    let text_w = (w - text_x - PAD - CLOSE).max(40);
    let mut y = PAD + 18;
    draw::set_font(t.bold_font(), t.font_size);
    let sh = if n.summary.is_empty() { 0 } else { draw::wrap_measure(&n.summary, text_w, false).1.min(t.font_size * 3 + 6) };
    let summary = (y, sh);
    y += sh;
    draw::set_font(t.font(), t.font_size - 1);
    let line = draw::height();
    let bh = if n.body.is_empty() { 0 } else { draw::wrap_measure(&n.body, text_w, false).1.min(line * BODY_LINES) };
    let body = (y + if sh > 0 { 2 } else { 0 }, bh);
    y = body.0 + bh;
    let mut buttons = vec![];
    let shown: Vec<&(String, String)> = n.actions.iter().filter(|(k, l)| k != "default" && !l.is_empty()).take(3).collect();
    if !shown.is_empty() {
        y += 10;
        let bw = (w - text_x - PAD - GAP * (shown.len() as i32 - 1)) / shown.len() as i32;
        for (i, (k, l)) in shown.into_iter().enumerate() {
            buttons.push((text_x + i as i32 * (bw + GAP), y, bw, BUTTON_H, k.clone(), l.clone()));
        }
        y += BUTTON_H;
    }
    let h = (y + PAD).max(PAD * 2 + ICON);
    Layout { h, text_x, text_w, summary, body, buttons, close: (w - PAD - CLOSE + 6, PAD - 4, CLOSE, CLOSE) }
}

struct Card {
    note: Note,
    layout: Layout,
    /// When it goes by itself (none: until closed).
    until: Option<Instant>,
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

struct Popups {
    config: Config,
    cards: Vec<Card>,
    waiting: VecDeque<Note>,
    hovered: Option<u32>,
    /// The cards as the widget draws them: (note, layout, y).
    view: Rc<RefCell<Vec<(Note, Layout, i32)>>>,
}

impl Popups {
    fn width(&self) -> i32 {
        self.config.width.clamp(240, 800)
    }

    /// Newest nearest the corner's edge.
    fn bottom_up(&self) -> bool {
        matches!(self.config.position, Position::BottomLeft | Position::BottomRight | Position::Bottom)
    }

    fn show(&mut self, n: Note) {
        let until = (n.timeout > 0).then(|| Instant::now() + Duration::from_millis(n.timeout as u64));
        let layout = layout(&n, self.width());
        if let Some(c) = self.cards.iter_mut().find(|c| c.note.id == n.id) {
            *c = Card { note: n, layout, until };
        } else if let Some(w) = self.waiting.iter_mut().find(|w| w.id == n.id) {
            *w = n;
        } else if self.cards.len() < self.config.max_shown.max(1) {
            self.cards.insert(0, Card { note: n, layout, until });
        } else {
            self.waiting.push_back(n);
        }
    }

    fn close(&mut self, id: u32, reason: Option<u32>) {
        if let Some(i) = self.cards.iter().position(|c| c.note.id == id) {
            self.cards.remove(i);
            if let Some(r) = reason {
                say(&Event::Closed(id, r));
            }
            if let Some(next) = self.waiting.pop_front() {
                self.show(next);
            }
        } else {
            self.waiting.retain(|w| w.id != id);
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
        let mut y = 0;
        let mut view = vec![];
        for c in order {
            view.push((c.note.clone(), c.layout.clone(), y));
            y += c.layout.h + GAP;
        }
        *self.view.borrow_mut() = view;
        heroui::resize_window(self.width(), (y - GAP).max(1));
        if let Some(mut w) = app::first_window() {
            heroui::widgets::repaint(&mut w);
        }
        Task::none()
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
                let gone: Vec<u32> = self.cards.iter().filter(|c| Some(c.note.id) != self.hovered && c.until.is_some_and(|u| u <= now)).map(|c| c.note.id).collect();
                if gone.is_empty() {
                    return Task::none();
                }
                for id in gone {
                    self.close(id, Some(EXPIRED));
                }
            }
            Msg::Hover(id) => {
                // Read while pointed at: its time starts over after.
                if let Some(c) = self.hovered.and_then(|h| self.cards.iter_mut().find(|c| c.note.id == h)) {
                    if let Some(u) = &mut c.until {
                        *u = (*u).max(Instant::now() + Duration::from_secs(3));
                    }
                }
                self.hovered = id;
                return Task::none();
            }
            Msg::Click(id, part) => {
                let has_default = self.cards.iter().find(|c| c.note.id == id).is_some_and(|c| c.note.actions.iter().any(|(k, _)| k == "default"));
                match part {
                    Part::Button(key) => say(&Event::Action(id, key)),
                    Part::Card if has_default => say(&Event::Action(id, "default".into())),
                    _ => {}
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
            let hover: Rc<Cell<(Option<u32>, Option<Part>)>> = Rc::new(Cell::new((None, None)));
            {
                let (view, hover) = (view.clone(), hover.clone());
                f.draw(move |f| {
                    let h = hover.take();
                    hover.set(h.clone());
                    for (n, l, y) in view.borrow().iter() {
                        let part = if h.0 == Some(n.id) { h.1.clone() } else { None };
                        draw_card(n, l, f.x(), f.y() + y, f.w(), part);
                    }
                });
            }
            let emit = ctx.emitter();
            f.handle(move |f, ev| {
                let at = |f: &Frame| -> (Option<u32>, Option<Part>) {
                    let (x, y) = (app::event_x() - f.x(), app::event_y() - f.y());
                    for (n, l, top) in view.borrow().iter() {
                        if y >= *top && y < top + l.h {
                            let (cy, inside) = (y - top, |r: (i32, i32, i32, i32), cy: i32| x >= r.0 && x < r.0 + r.2 && cy >= r.1 && cy < r.1 + r.3);
                            if inside(l.close, cy) {
                                return (Some(n.id), Some(Part::Close));
                            }
                            for b in &l.buttons {
                                if inside((b.0, b.1, b.2, b.3), cy) {
                                    return (Some(n.id), Some(Part::Button(b.4.clone())));
                                }
                            }
                            return (Some(n.id), Some(Part::Card));
                        }
                    }
                    (None, None)
                };
                match ev {
                    FlEvent::Enter | FlEvent::Move => {
                        let now = at(f);
                        let before = hover.replace(now.clone());
                        if before != now {
                            if before.0 != now.0 {
                                emit(Msg::Hover(now.0));
                            }
                            heroui::widgets::repaint(f);
                        }
                        true
                    }
                    FlEvent::Leave => {
                        if hover.replace((None, None)).0.is_some() {
                            emit(Msg::Hover(None));
                            heroui::widgets::repaint(f);
                        }
                        true
                    }
                    FlEvent::Push => {
                        if let (Some(id), Some(part)) = at(f) {
                            // Any other button: just close it.
                            let part = if app::event_mouse_button() == app::MouseButton::Left { part } else { Part::Close };
                            emit(Msg::Click(id, part));
                        }
                        true
                    }
                    _ => false,
                }
            });
            f.as_base_widget()
        })
    }

    fn subscriptions(&self) -> Vec<Subscription<Msg>> {
        vec![
            Subscription::every(Duration::from_millis(250), Msg::Tick),
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

fn draw_card(n: &Note, l: &Layout, x: i32, y: i32, w: i32, hover: Option<Part>) {
    let t = heroui::theme::current();
    let r = t.radius.min(14);
    let mix = heroui::widgets::mix;
    // Edge, then face.
    heroui::fx::fill_rounded(x as f64, y as f64, w as f64, l.h as f64, r as f64, mix(t.surface, t.text, 0.14), 1.0);
    let face = if hover.is_some() { mix(t.surface, t.text, 0.04) } else { t.surface };
    heroui::fx::fill_rounded(x as f64 + 1.0, y as f64 + 1.0, w as f64 - 2.0, l.h as f64 - 2.0, (r - 1).max(0) as f64, face, 1.0);
    if n.urgency >= 2 {
        heroui::fx::fill_rounded(x as f64 + 1.0, y as f64 + 10.0, 3.0, l.h as f64 - 20.0, 1.5, t.accent, 1.0);
    }
    if !n.icon.is_empty() && !heroui::icons::draw(&n.icon, x + PAD, y + PAD, ICON, t.text) {
        heroui::icons::draw("bell", x + PAD + 6, y + PAD + 6, ICON - 12, t.text_dim);
    }
    let tx = x + l.text_x;
    draw::set_font(t.font(), t.font_size - 2);
    draw::set_draw_color(t.text_dim);
    let app = if n.app.is_empty() { "Notification" } else { &n.app };
    draw::draw_text2(app, tx, y + PAD - 2, l.text_w, 18, Align::Left | Align::Inside | Align::Clip);
    if l.summary.1 > 0 {
        draw::set_font(t.bold_font(), t.font_size);
        draw::set_draw_color(t.text);
        draw::push_clip(tx, y + l.summary.0, l.text_w, l.summary.1);
        draw::draw_text2(&n.summary, tx, y + l.summary.0, l.text_w, l.summary.1, Align::Left | Align::Top | Align::Wrap | Align::Inside);
        draw::pop_clip();
    }
    if l.body.1 > 0 {
        draw::set_font(t.font(), t.font_size - 1);
        draw::set_draw_color(mix(t.text, t.text_dim, 0.4));
        draw::push_clip(tx, y + l.body.0, l.text_w, l.body.1);
        draw::draw_text2(&n.body, tx, y + l.body.0, l.text_w, l.body.1, Align::Left | Align::Top | Align::Wrap | Align::Inside);
        draw::pop_clip();
    }
    for (bx, by, bw, bh, key, label) in &l.buttons {
        let lit = hover == Some(Part::Button(key.clone()));
        let c = if lit { mix(t.surface_alt, t.text, 0.08) } else { t.surface_alt };
        heroui::fx::fill_rounded((x + bx) as f64, (y + by) as f64, *bw as f64, *bh as f64, t.radius.min(8) as f64, c, 1.0);
        draw::set_font(t.font(), t.font_size - 1);
        draw::set_draw_color(t.text);
        draw::draw_text2(label, x + bx + 4, y + by, bw - 8, *bh, Align::Center | Align::Clip);
    }
    // The close button shows on the card pointed at.
    if hover.is_some() {
        let (cx, cy, cw, ch) = l.close;
        if hover == Some(Part::Close) {
            heroui::fx::fill_rounded((x + cx) as f64, (y + cy) as f64, cw as f64, ch as f64, (cw / 2) as f64, t.surface_alt, 1.0);
        }
        draw::set_draw_color(t.text_dim);
        draw::set_line_style(draw::LineStyle::Solid, 2);
        let (mx, my, k) = (x + cx + cw / 2, y + cy + ch / 2, 4);
        draw::draw_line(mx - k, my - k, mx + k, my + k);
        draw::draw_line(mx - k, my + k, mx + k, my - k);
        draw::set_line_style(draw::LineStyle::Solid, 0);
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
    let width = config.width.clamp(240, 800);
    let app = Popups { config, cards: vec![], waiting: VecDeque::new(), hovered: None, view: Rc::default() };
    let settings = Settings::new("Notifications").class("heronotify").kind(WindowKind::Notification).size(width, 1).corner(corner, 12).decorated(false).resizable(false).above(true).sticky(true).skip_taskbar(true).transparent(true);
    if let Err(e) = heroui::run(app, settings) {
        eprintln!("heronotify: {e}");
    }
}
