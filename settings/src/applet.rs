//! `torq-applet`: TorqOS in COSMIC's panel (ADR-049). The machine's state at a glance and the one action
//! that settles it: a commit waiting for confirmation, a restart or log-out a switch needs, an alarm,
//! proposals waiting, an uncommitted draft. TorqOS Settings holds the detail; this only points there. It
//! changes nothing itself: it confirms through torqcli, approved in COSMIC's own dialog, and asks
//! COSMIC's own dialogs to log out or restart.

// No unsafe code and no dead code, ever: `forbid` cannot be undone by any `#[allow]` below it,
// so the compiler is the guard (docs/DECISIONS.md ADR-020).
#![forbid(unsafe_code)]
#![forbid(dead_code)]

use cosmic::cosmic_theme::palette::Srgba;
use cosmic::iced::window::Id;
use cosmic::iced::{Length, Rectangle, Subscription};
use cosmic::prelude::*;
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget::{self, icon};
use std::path::PathBuf;
use std::time::Duration;
use torq::app::{self, Console, Glance, Needs};

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

/// How often the broker is asked, when nothing else says to ask sooner.
const EVERY: Duration = Duration::from_secs(60);
/// How often the running system is looked at: a switch is seen within this, and the broker asked then.
const SWITCH: Duration = Duration::from_secs(3);
/// How often a person's desktop is saved, when saving is automatic.
const DESKTOP: Duration = Duration::from_secs(15 * 60);
/// A notification this long before an unconfirmed change rolls back.
const WARN_SECONDS: i64 = 120;

#[derive(Default)]
struct Applet {
    core: cosmic::Core,
    popup: Option<Id>,
    console: Option<Console>,
    glance: Option<Result<Glance, String>>,
    /// The system this login started on (ADR-049 1): what a log-out would take into use is the difference.
    session: Option<PathBuf>,
    /// The running system when last looked at: a change is a switch.
    running: Option<PathBuf>,
    /// The system a switch has just left: once it has settled, what it changed is started again.
    switched_from: Option<PathBuf>,
    /// The start of a polkit COSMIC's approval dialog was older than, seen at the last look, and the one acted on
    /// (the dialog started again): once per polkit, one look after it is seen, so the new one is on the bus. Lost
    /// with the applet, harmlessly: what is looked at is the processes themselves, never a remembered pid.
    agent_outlived: Option<u64>,
    agent_restarted_for: Option<u64>,
    /// The latest change, when last looked at: another (an undo, a pull) may change the person's desktop.
    last_change: Option<String>,
    /// What was last notified, so each state is said once.
    told: Option<String>,
    /// The confirmation already warned of as about to expire.
    warned: Option<u64>,
    /// Now, for the countdown.
    now: i64,
}

#[derive(Debug, Clone)]
enum Message {
    Ask,
    Looked,
    Tick,
    SyncDesktop,
    Answer(Result<Glance, String>),
    Confirm,
    Closed,
    LogOut,
    Restart,
    Open(&'static str),
    PopupClosed(Id),
    Surface(cosmic::surface::Action<Message>),
}

/// What the panel shows: the first that holds (ADR-049 2).
enum State<'a> {
    Confirm(&'a app::Confirm),
    Needs(&'a Needs),
    Alarm(&'a str),
    Waiting(usize, usize),
    Draft,
    Well,
    Unknown,
}

/// A colour as SVG writes it.
fn hex(c: Srgba) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.red), b(c.green), b(c.blue))
}

/// TorqOS's mark in the panel's own text colour, and a badge in the corner when something waits: drawn
/// here, because a symbolic icon has one colour and the badge is the news.
fn mark(badge: Option<Srgba>) -> icon::Handle {
    let theme = cosmic::theme::active();
    let ink = hex(theme.cosmic().on_bg_color().into());
    let (mask, dot) = match badge {
        Some(c) => (
            "<mask id='m'><rect width='16' height='16' fill='white'/><circle cx='12.4' cy='12.4' r='4.4' fill='black'/></mask>".to_string(),
            format!("<circle cx='12.4' cy='12.4' r='3.2' fill='{}'/>", hex(c)),
        ),
        None => (String::new(), String::new()),
    };
    let masked = if badge.is_some() { " mask='url(#m)'" } else { "" };
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16' width='16' height='16'>{mask}<g{masked}><g transform='scale(0.1333)'><path d='M97.59 46.32 A40 40 0 1 1 73.68 22.41' fill='none' stroke='{ink}' stroke-width='18'/><polygon points='115,40 80,53 91,26' fill='{ink}'/></g></g>{dot}</svg>"
    );
    icon::from_svg_bytes(svg.into_bytes())
}

/// A notification, and what clicking it opens (ADR-049 4): on a thread of its own, which waits for it to
/// be clicked or closed.
fn notify(summary: String, body: String, page: &'static str) {
    std::thread::spawn(move || {
        let _ = send(&summary, &body, page);
    });
}

fn send(summary: &str, body: &str, page: &str) -> zbus::Result<()> {
    let connection = zbus::blocking::Connection::session()?;
    let bus = zbus::blocking::Proxy::new(&connection, "org.freedesktop.Notifications", "/org/freedesktop/Notifications", "org.freedesktop.Notifications")?;
    let signals = bus.receive_all_signals()?;
    let hints = std::collections::HashMap::<&str, zbus::zvariant::Value>::new();
    let id: u32 = bus.call("Notify", &("TorqOS", 0u32, "torqos", summary, body, vec!["default", "Open"], hints, -1i32))?;
    for signal in signals {
        let header = signal.header();
        let member = header.member().map(|m| m.to_string()).unwrap_or_default();
        if member == "ActionInvoked" {
            let (of, _action): (u32, String) = signal.body().deserialize()?;
            if of == id {
                app::open_settings(page);
                break;
            }
        } else if member == "NotificationClosed" {
            let (of, _why): (u32, u32) = signal.body().deserialize()?;
            if of == id {
                break;
            }
        }
    }
    Ok(())
}

/// COSMIC's app library and launcher watch this folder, not the system's, whose path a switch replaces: a
/// file come and gone makes them read their menu again, so a new application appears (ADR-049 5).
fn refresh_menu() {
    let Some(home) = std::env::var_os("HOME") else { return };
    let dir = PathBuf::from(home).join(".local/share/applications");
    let _ = std::fs::create_dir_all(&dir);
    let touch = dir.join(".torq-switched");
    if std::fs::write(&touch, "").is_ok() {
        let _ = std::fs::remove_file(&touch);
    }
}

impl Applet {
    fn state(&self) -> State<'_> {
        let Some(Ok(g)) = &self.glance else { return State::Unknown };
        if let Some(c) = &g.confirm {
            return State::Confirm(c);
        }
        if matches!(g.needs, Needs::LogOut(_) | Needs::Restart(_)) {
            return State::Needs(&g.needs);
        }
        if let Some(a) = &g.alarm {
            return State::Alarm(a);
        }
        if g.waiting.0 > 0 {
            return State::Waiting(g.waiting.0, g.waiting.1);
        }
        if g.draft {
            return State::Draft;
        }
        State::Well
    }

    /// The badge: nothing when all is well; the theme's colours for what waits.
    fn icon(&self) -> icon::Handle {
        let theme = cosmic::theme::active();
        let c = theme.cosmic();
        mark(match self.state() {
            State::Confirm(_) | State::Needs(Needs::Restart(_)) => Some(c.warning_color()),
            State::Alarm(_) => Some(c.destructive_color()),
            State::Needs(_) | State::Waiting(..) | State::Draft => Some(c.accent_color()),
            State::Well | State::Unknown => None,
        })
    }

    /// What a state says when it starts: its words, and the page that has the detail.
    fn tell(&self) -> Option<(String, String, &'static str)> {
        match self.state() {
            State::Confirm(c) => Some((format!("Confirm change #{}", c.seq), format!("It rolls back at {} unless you confirm it.", c.deadline), "overview")),
            State::Needs(n) => Some((match n { Needs::Restart(_) => "Restart needed".to_string(), _ => "Log out needed".to_string() }, n.line().unwrap_or_default(), "overview")),
            State::Alarm(a) => Some(("TorqOS needs a look".to_string(), a.to_string(), "overview")),
            State::Waiting(n, t) => Some((
                if n == 1 { "A proposal is waiting".to_string() } else { format!("{n} proposals are waiting") },
                if t > 0 { format!("{t} came from a session that read untrusted content.") } else { "Review it in TorqOS Settings.".to_string() },
                "proposals",
            )),
            State::Draft | State::Well | State::Unknown => None,
        }
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut col = widget::column::with_capacity(6).spacing(spacing.space_s).padding(spacing.space_s).width(Length::Fixed(340.0));
        let version = match &self.glance {
            Some(Ok(g)) => format!("TorqOS {}", g.version),
            _ => "TorqOS".into(),
        };
        col = col.push(widget::text::heading(version));
        let line = |s: String| widget::text::body(s).width(Length::Fill);
        let mut open = "overview";
        match self.state() {
            State::Confirm(c) => {
                let left = c.at.map(|at| (at - self.now).max(0)).map(|s| format!("{}:{:02} left. ", s / 60, s % 60)).unwrap_or_default();
                col = col.push(line(format!("{left}Change #{} rolls back at {} unless you confirm it.", c.seq, c.deadline)));
                let both = matches!(&self.glance, Some(Ok(g)) if matches!(g.needs, Needs::Restart(_)));
                col = col.push(widget::button::suggested(if both { "Confirm, then restart" } else { "Confirm" }).on_press(Message::Confirm));
            }
            State::Needs(n) => {
                col = col.push(line(n.line().unwrap_or_default()));
                col = col.push(match n {
                    Needs::Restart(_) => widget::button::suggested("Restart…").on_press(Message::Restart),
                    _ => widget::button::suggested("Log out…").on_press(Message::LogOut),
                });
            }
            State::Alarm(a) => col = col.push(line(a.to_string())),
            State::Waiting(n, tainted) => {
                open = "proposals";
                let what = if n == 1 { "A proposal is".to_string() } else { format!("{n} proposals are") };
                let mut s = format!("{what} waiting for you to approve or refuse.");
                if tainted > 0 {
                    s.push_str(&format!(" {tainted} came from a session that read untrusted content."));
                }
                col = col.push(line(s));
            }
            State::Draft => {
                open = "changes";
                col = col.push(line("You have changes not yet committed.".into()));
            }
            State::Well => {
                col = col.push(line("This machine is exactly as its history says.".into()));
                if let Some(Ok(Glance { last: Some(last), .. })) = &self.glance {
                    col = col.push(widget::text::caption(format!("Last change: {last}")));
                }
            }
            State::Unknown => {
                let s = match &self.glance {
                    Some(Err(e)) => format!("The broker did not answer: {e}"),
                    _ => "Asking the broker…".into(),
                };
                col = col.push(line(s));
            }
        }
        col = col.push(widget::divider::horizontal::default());
        col = col.push(widget::button::text("Open TorqOS Settings").on_press(Message::Open(open)));
        self.core.applet.popup_container(col).into()
    }
}

impl cosmic::Application for Applet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "org.torqos.Applet";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _flags: ()) -> (Self, Task<cosmic::Action<Message>>) {
        let mut app = Applet { core, console: Some(Console::watcher()), session: app::session_system(), running: running(), now: app::now(), ..Default::default() };
        // A rebuilt machine's apps and desktop come back at the first login.
        std::thread::spawn(|| {
            let _ = app::reconcile_apps();
            let _ = app::sync_desktop(false);
        });
        let ask = app.update(Message::Ask);
        (app, ask)
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut all = vec![cosmic::iced::time::every(EVERY).map(|_| Message::Ask), cosmic::iced::time::every(SWITCH).map(|_| Message::Looked), cosmic::iced::time::every(DESKTOP).map(|_| Message::SyncDesktop)];
        // The countdown, only while there is one.
        if matches!(self.state(), State::Confirm(_)) {
            all.push(cosmic::iced::time::every(Duration::from_secs(1)).map(|_| Message::Tick));
        }
        Subscription::batch(all)
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Ask => {
                let session = self.session.clone();
                let console = self.console.clone();
                return cosmic::task::future(async move {
                    Message::Answer(blocking(move || app::glance(session.as_deref(), console.is_some_and(|c| c.uncommitted()))).await)
                });
            }
            Message::Looked => {
                // polkit replaced (a switch can): COSMIC's agent registered with the old one and must start again,
                // or no approval can be asked. Seen from the processes' start times, so also when this applet itself
                // started after the replacement (a panel restarted, a crash, a login while polkit restarts).
                match app::approval_agent_outlived() {
                    Some(polkit) if self.agent_restarted_for != Some(polkit) => {
                        if self.agent_outlived == Some(polkit) {
                            app::restart_approval_agent();
                            self.agent_restarted_for = Some(polkit);
                            self.agent_outlived = None;
                        } else {
                            self.agent_outlived = Some(polkit);
                        }
                    }
                    _ => self.agent_outlived = None,
                }
                let now = running();
                if now != self.running {
                    // The first system left, if a switch follows a switch: what is running came from it.
                    self.switched_from = self.switched_from.take().or_else(|| self.running.clone());
                    self.running = now;
                    refresh_menu();
                    return self.update(Message::Ask);
                }
                // Settled: the panel and the rest, where they changed, without a new login. Open windows stay.
                if let (Some(old), Some(new)) = (self.switched_from.take(), now) {
                    // The configuration may name other apps now: an undo, or a pull from the remote.
                    std::thread::spawn(|| {
                        let _ = app::reconcile_apps();
                    });
                    app::restart_parts(&app::stale_parts(&old, &new));
                }
            }
            // Not while a commit confirmed waits: the broker takes no change until it is confirmed or rolled back.
            Message::SyncDesktop if matches!(self.state(), State::Confirm(_)) => {}
            Message::SyncDesktop => {
                std::thread::spawn(|| {
                    let _ = app::sync_desktop(false);
                });
            }
            Message::Tick => {
                self.now = app::now();
                if let State::Confirm(c) = self.state() {
                    let (seq, deadline, left) = (c.seq, c.deadline.clone(), c.at.map(|at| at - self.now));
                    if left.is_some_and(|l| l <= WARN_SECONDS) && self.warned != Some(seq) {
                        self.warned = Some(seq);
                        notify(format!("Change #{seq} is about to roll back"), format!("Confirm it before {deadline}, or the machine undoes it."), "overview");
                    }
                }
            }
            Message::Answer(g) => {
                let first = self.glance.is_none();
                let failed = g.is_err();
                // A change since the last look may be to the person's desktop (an undo of one): put it on the desktop.
                if let Ok(now) = &g {
                    if self.last_change.is_some() && self.last_change != now.last && now.confirm.is_none() {
                        std::thread::spawn(|| {
                            let _ = app::sync_desktop(false);
                        });
                    }
                    self.last_change = now.last.clone();
                }
                self.glance = Some(g);
                self.now = app::now();
                // A failed ask says nothing of the state: what was said stands, and is not said again.
                if !failed {
                    let tell = self.tell();
                    let key = tell.as_ref().map(|(s, b, _)| format!("{s}{b}"));
                    // Each state said once, when it starts: not the one the session opened on.
                    if key != self.told {
                        if let (Some((summary, body, page)), false) = (tell, first) {
                            notify(summary, body, page);
                        }
                        self.told = key;
                    }
                }
            }
            Message::Confirm => {
                let restart = matches!(&self.glance, Some(Ok(g)) if matches!(g.needs, Needs::Restart(_)));
                return cosmic::task::future(async move {
                    let confirmed = blocking(app::confirm).await.is_ok();
                    if restart && confirmed {
                        app::ask_restart();
                    }
                    Message::Closed
                });
            }
            Message::Closed => return self.update(Message::Ask),
            Message::LogOut => app::ask_log_out(),
            Message::Restart => app::ask_restart(),
            Message::Open(page) => app::open_settings(page),
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::Surface(a) => return cosmic::task::message(cosmic::Action::Surface(a)),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let open = self.popup;
        // At a symbolic icon's size, as the panel's other status icons are: the mark is drawn SVG (its badge has its
        // own colour), which `icon_button_from_handle` would size as a full-colour app icon, a size larger.
        let (w, h) = self.core.applet.suggested_size(true);
        let mark = widget::icon(self.icon()).width(Length::Fixed(w as f32)).height(Length::Fixed(h as f32));
        let button = self.core.applet.button_from_element(mark, true).on_press_with_rectangle(move |offset, bounds| {
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
        self.core.applet.applet_tooltip::<Message>(button, "TorqOS", self.popup.is_some(), Message::Surface, None).into()
    }

    fn view_window(&self, _id: Id) -> Element<'_, Message> {
        self.popup_view()
    }

    fn style(&self) -> Option<cosmic::iced::theme::Style> {
        Some(cosmic::applet::style())
    }
}

/// The running system.
fn running() -> Option<PathBuf> {
    std::fs::read_link("/run/current-system").ok()
}

/// A call that blocks on a thread of its own, so the panel keeps drawing.
async fn off<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.map_err(|_| "the call stopped before it answered".to_string())
}

/// `off`, for a call of the library's: its error as torqcli would say it.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    off(move || f().map_err(|e| app::said(&e))).await.and_then(|r| r)
}
