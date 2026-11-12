//! `torq-tunnels`: the person's SSH tunnels in COSMIC's panel (ADR-050 4). Each tunnel with a switch to
//! start or stop it, its state in a line, and whether it starts at login; a tunnel added, or the file opened
//! to edit. It is the person's own program: it asks their own user manager (`systemctl --user`) and writes
//! their own file, and changes nothing of the system. Not in the panel by default (ADR-050, settled 2): the
//! person adds it from COSMIC's applet list.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use cosmic::cosmic_theme::palette::Srgba;
use cosmic::iced::window::Id;
use cosmic::iced::{Alignment, Length, Rectangle, Subscription};
use cosmic::prelude::*;
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget::{self, icon};
use std::time::Duration;
use torq::tunnels::{self, State};

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

/// How often the tunnels are looked at: a reconnect is seen within this.
const EVERY: Duration = Duration::from_secs(3);

struct Applet {
    core: cosmic::Core,
    popup: Option<Id>,
    /// The file's tunnels (and its wrong ones), or why it cannot be read.
    tunnels: Result<tunnels::File, String>,
    /// Each tunnel's state and whether it starts at login, in the file's order.
    states: Vec<(State, bool)>,
    /// A look under way: no second one while it is (a user manager slow to answer must not pile them up).
    looking: bool,
    /// The last thing that went wrong, said until the next action.
    error: Option<String>,
    /// The add form, while open.
    adding: Option<Form>,
}

#[derive(Default, Clone)]
struct Form {
    name: String,
    destination: String,
    socks: String,
    local: String,
}

#[derive(Debug, Clone)]
enum Message {
    Look,
    Looked(Result<tunnels::File, String>, Vec<(State, bool)>),
    Switch(String, bool),
    AtLogin(String, bool),
    Done(Result<(), String>),
    Add,
    Name(String),
    Destination(String),
    Socks(String),
    Local(String),
    Save,
    Cancel,
    Edit,
    PopupClosed(Id),
    Surface(cosmic::surface::Action<Message>),
}

/// A colour as SVG writes it.
fn hex(c: Srgba) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.red), b(c.green), b(c.blue))
}

/// A tunnel's mouth in the panel's own text colour, filled with the badge's when one is up or failing:
/// drawn here, as `torq-applet`'s mark is, because a symbolic icon has one colour.
fn glyph(fill: Option<Srgba>) -> icon::Handle {
    let theme = cosmic::theme::active();
    let ink = hex(theme.cosmic().on_bg_color().into());
    let inside = match fill {
        Some(c) => format!("<path d='M5 14 V9 a3 3 0 0 1 6 0 V14 Z' fill='{}'/>", hex(c)),
        None => String::new(),
    };
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' width='16' height='16'><path d='M1.5 14.5 V8 a6.5 6.5 0 0 1 13 0 V14.5' fill='none' stroke='{ink}' stroke-width='1.6'/><path d='M0.5 14.5 H15.5' stroke='{ink}' stroke-width='1.2'/>{inside}</svg>"
    );
    icon::from_svg_bytes(svg.into_bytes())
}

/// The file and each tunnel's state, off the panel's thread.
async fn look() -> Message {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let all = tunnels::load().map_err(|e| format!("{e:#}"));
        let states = all.as_ref().map(|f| tunnels::status(&f.tunnels)).unwrap_or_default();
        let _ = tx.send(Message::Looked(all, states));
    });
    rx.await.unwrap_or(Message::Looked(Err("the look stopped before it answered".into()), Vec::new()))
}

/// An action of the user manager's, off the panel's thread, then a fresh look.
fn act(f: impl FnOnce() -> anyhow::Result<()> + Send + 'static) -> Task<cosmic::Action<Message>> {
    cosmic::task::future(async move {
        let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f().map_err(|e| format!("{e:#}")));
        });
        Message::Done(rx.await.unwrap_or(Err("the action stopped before it answered".into())))
    })
}

impl Applet {
    /// The badge: nothing with no tunnel on; the accent while one is up; the warning while one that should
    /// be up is not.
    fn icon(&self) -> icon::Handle {
        let theme = cosmic::theme::active();
        let c = theme.cosmic();
        let failing = self.tunnels.as_ref().map_or(true, |f| !f.wrong.is_empty()) || self.states.iter().any(|(s, _)| matches!(s, State::Retrying(_) | State::Failed(_)));
        let up = self.states.iter().any(|(s, _)| *s == State::Up);
        glyph(if failing { Some(c.warning_color()) } else if up { Some(c.accent_color()) } else { None })
    }

    fn tooltip(&self) -> String {
        let up = self.states.iter().filter(|(s, _)| *s == State::Up).count();
        match up {
            0 => "SSH tunnels".into(),
            1 => "SSH tunnels: 1 up".into(),
            n => format!("SSH tunnels: {n} up"),
        }
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut col = widget::column::with_capacity(8).spacing(spacing.space_s).padding(spacing.space_s).width(Length::Fixed(360.0));
        col = col.push(widget::text::heading("SSH tunnels"));
        match &self.tunnels {
            Err(e) => col = col.push(widget::text::body(format!("The tunnels file is not right: {e}")).width(Length::Fill)),
            Ok(file) if file.tunnels.is_empty() && file.wrong.is_empty() && self.adding.is_none() => {
                col = col.push(widget::text::body("No tunnels yet. Add one, or edit the file.").width(Length::Fill));
            }
            Ok(file) => {
                // What is wrong in the file first: the rest still works.
                for (n, why) in &file.wrong {
                    col = col.push(widget::text::body(format!("{n}: not right, so not started. {why}")).width(Length::Fill));
                }
                for c in &file.clashes {
                    col = col.push(widget::text::caption(format!("Only one can be up at a time: {c}.")).width(Length::Fill));
                }
                for (t, (state, login)) in file.tunnels.iter().zip(self.states.iter()) {
                    let line = match state {
                        State::Off => t.describe(),
                        State::Connecting => format!("Connecting to {}…", t.destination),
                        State::Up => format!("Up · {}", t.forwards.iter().map(|f| f.describe()).collect::<Vec<_>>().join(", ")),
                        State::Retrying(why) => why.clone(),
                        State::Failed(why) => format!("Stopped: {why}"),
                    };
                    let name = t.name.clone();
                    let head = widget::row::with_capacity(2)
                        .align_y(Alignment::Center)
                        .push(widget::text::body(t.name.clone()).width(Length::Fill))
                        .push(widget::toggler(state.on()).on_toggle(move |on| Message::Switch(name.clone(), on)));
                    let name = t.name.clone();
                    let row = widget::column::with_capacity(3)
                        .spacing(spacing.space_xxs)
                        .push(head)
                        .push(widget::text::caption(line).width(Length::Fill))
                        .push(widget::checkbox(*login).label("Start at login").on_toggle(move |on| Message::AtLogin(name.clone(), on)));
                    col = col.push(row);
                }
            }
        }
        if let Some(f) = &self.adding {
            col = col.push(widget::divider::horizontal::default());
            col = col.push(widget::text::body("A new tunnel"));
            col = col.push(widget::text_input("Name (lower-case, such as work)", &f.name).on_input(Message::Name));
            col = col.push(widget::text_input("Destination: user@host, or a Host of your ssh config", &f.destination).on_input(Message::Destination));
            col = col.push(widget::text_input("SOCKS port here (such as 1080), optional", &f.socks).on_input(Message::Socks));
            col = col.push(widget::text_input("Or a forward: <port> <host>:<port>, optional", &f.local).on_input(Message::Local).on_submit(|_| Message::Save));
            col = col.push(widget::text::caption("The host's key must already be known: run `ssh <destination>` once in a terminal.").width(Length::Fill));
            col = col.push(
                widget::row::with_capacity(2)
                    .spacing(spacing.space_xs)
                    .push(widget::button::standard("Cancel").on_press(Message::Cancel))
                    .push(widget::button::suggested("Add").on_press(Message::Save)),
            );
        }
        if let Some(e) = &self.error {
            col = col.push(widget::text::caption(e.clone()).width(Length::Fill));
        }
        col = col.push(widget::divider::horizontal::default());
        let mut buttons = widget::row::with_capacity(2).spacing(spacing.space_xs);
        if self.adding.is_none() {
            buttons = buttons.push(widget::button::text("Add tunnel…").on_press(Message::Add));
        }
        buttons = buttons.push(widget::button::text("Edit tunnels…").on_press(Message::Edit));
        col = col.push(buttons);
        self.core.applet.popup_container(col).into()
    }

    /// The form's forwards, in the file's words.
    fn forwards(f: &Form) -> Vec<String> {
        let mut out = Vec::new();
        if !f.socks.trim().is_empty() {
            out.push(format!("socks {}", f.socks.trim()));
        }
        if !f.local.trim().is_empty() {
            out.push(format!("local {}", f.local.trim()));
        }
        out
    }
}

impl cosmic::Application for Applet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "org.torqos.Tunnels";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: ()) -> (Self, Task<cosmic::Action<Message>>) {
        let app = Applet { core, popup: None, tunnels: Ok(tunnels::File::default()), states: Vec::new(), looking: true, error: None, adding: None };
        (app, cosmic::task::future(look()))
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        cosmic::iced::time::every(EVERY).map(|_| Message::Look)
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Look => {
                if self.looking {
                    return Task::none();
                }
                self.looking = true;
                return cosmic::task::future(look());
            }
            Message::Looked(all, states) => {
                self.looking = false;
                self.tunnels = all;
                self.states = states;
            }
            Message::Switch(name, on) => {
                self.error = None;
                return act(move || if on { tunnels::start(&name) } else { tunnels::stop(&name) });
            }
            Message::AtLogin(name, on) => {
                self.error = None;
                return act(move || tunnels::at_login(&name, on));
            }
            Message::Done(r) => {
                self.error = r.err();
                return self.update(Message::Look);
            }
            Message::Add => {
                self.error = None;
                self.adding = Some(Form::default());
            }
            Message::Name(s) => self.adding.iter_mut().for_each(|f| f.name = s.clone()),
            Message::Destination(s) => self.adding.iter_mut().for_each(|f| f.destination = s.clone()),
            Message::Socks(s) => self.adding.iter_mut().for_each(|f| f.socks = s.clone()),
            Message::Local(s) => self.adding.iter_mut().for_each(|f| f.local = s.clone()),
            Message::Save => {
                if let Some(f) = &self.adding {
                    let forwards = Self::forwards(f);
                    if forwards.is_empty() {
                        self.error = Some("Give it a SOCKS port or a forward.".into());
                        return Task::none();
                    }
                    match tunnels::add_to_file(f.name.trim(), f.destination.trim(), &forwards) {
                        Ok(()) => {
                            self.error = None;
                            self.adding = None;
                            return self.update(Message::Look);
                        }
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
            }
            Message::Cancel => {
                self.adding = None;
                self.error = None;
            }
            Message::Edit => match tunnels::ensure_file() {
                // COSMIC's own editor; whatever opens text files, where it is not there.
                Ok(path) => {
                    if std::process::Command::new("cosmic-edit").arg(&path).spawn().is_err() {
                        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
                    }
                }
                Err(e) => self.error = Some(format!("{e:#}")),
            },
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                    self.adding = None;
                }
            }
            Message::Surface(a) => return cosmic::task::message(cosmic::Action::Surface(a)),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let open = self.popup;
        // At a symbolic icon's size, as the panel's other status icons are: the handle is drawn SVG (its badge has
        // its own colour), which `icon_button_from_handle` would size as a full-colour app icon, a size larger.
        let (w, h) = self.core.applet.suggested_size(true);
        let glyph = widget::icon(self.icon()).width(Length::Fixed(w as f32)).height(Length::Fixed(h as f32));
        let button = self.core.applet.button_from_element(glyph, true).on_press_with_rectangle(move |offset, bounds| {
            if let Some(id) = open {
                return Message::Surface(destroy_popup(id));
            }
            Message::Surface(app_popup::<Applet>(
                |_| Default::default(),
                move |state: &mut Applet| {
                    let id = Id::unique();
                    state.popup = Some(id);
                    let mut settings = state.core.applet.get_popup_settings(state.core.main_window_id().unwrap(), id, None, None, None);
                    settings.positioner.anchor_rect = Rectangle {
                        x: (bounds.x - offset.x) as i32,
                        y: (bounds.y - offset.y) as i32,
                        width: bounds.width as i32,
                        height: bounds.height as i32,
                    };
                    settings
                },
                Some(Box::new(|state: &Applet| state.popup_view().map(cosmic::Action::App))),
            ))
        });
        self.core.applet.applet_tooltip::<Message>(button, self.tooltip(), self.popup.is_some(), Message::Surface, None).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        self.popup_view()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}
