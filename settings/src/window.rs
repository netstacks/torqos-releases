//! The window: each page torqcli's output, laid out as COSMIC lays out settings, and each button a torqcli
//! command, run by torqcli's own session (`torq::app::Console`). A yes or no is asked here, an approval in
//! COSMIC's own dialog (ADR-045 5): a password is never typed in a window TorqOS draws.

use cosmic::app::context_drawer;
use cosmic::iced::{Alignment, Length};
use cosmic::prelude::*;
use cosmic::widget::segmented_button::{Entity, SingleSelectModel};
use cosmic::widget::{self, icon, nav_bar, settings};
use cosmic::iced::futures::channel::mpsc::Sender;
use cosmic::iced::futures::SinkExt as _;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use torq::app::{self, Action, App, At, Choice, Console, DeviceCode, GitHubToken, Glance, How, Machine, Mode, Needs, Proposal, Tool, Typed, View, VIEWS};
use torq::history::Row;

/// How many changes History shows.
const HISTORY: usize = 200;
/// The most rows a list or the tree shows: filtering narrows the rest.
const LISTED: usize = 400;
/// The most lines the diff viewer draws.
const DIFF_LINES: usize = 2000;

const DRAFT: &str = "show | display set";
const ACTIVE: &str = "show configuration | display set";
const COMPARE: &str = "show | compare";
const COMMITS: &str = "show system commit";
const REMOTE: &str = "show system remote";

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Configuration,
    History,
    Proposals,
    Apps,
    Desktop,
    System,
    Commands,
}

/// Configuration's tabs.
#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Settings,
    Changes,
    Earlier,
}

/// System's Backup tab, beside the pages that are commands' output.
#[derive(Clone, Copy)]
struct BackupTab;

/// The Apps page's tabs.
#[derive(Clone, Copy, PartialEq)]
enum AppTab {
    Apps,
    Tools,
}

/// What a diff page shows.
#[derive(Clone, Copy, PartialEq)]
enum DiffTab {
    Plain,
    Files,
}

/// What a page has to show: not yet asked, the answer, or why there is none.
enum Loaded<T> {
    Waiting,
    Got(T),
    Failed(String),
}

impl<T> From<Result<T, String>> for Loaded<T> {
    fn from(r: Result<T, String>) -> Self {
        match r {
            Ok(v) => Loaded::Got(v),
            Err(e) => Loaded::Failed(e),
        }
    }
}

/// A place in the configuration.
type Path = Vec<String>;
/// The configuration, each setting's path and its values.
type Values = BTreeMap<Path, Vec<String>>;


/// How a setting in the draft differs from the configuration in effect.
#[derive(Clone, Copy, PartialEq)]
enum Change {
    Same,
    New,
    Changed,
    Removed,
}

pub struct Settings {
    core: cosmic::Core,
    nav: nav_bar::Model,
    console: Console,
    glance: Loaded<Glance>,
    machine: Loaded<Machine>,
    history: Loaded<Vec<Row>>,
    proposals: Loaded<Vec<Proposal>>,
    /// Each command's output, by the command.
    outputs: HashMap<String, Loaded<String>>,
    /// What the last action did: its words, and whether it failed.
    note: Option<(String, bool)>,
    /// What is running now.
    busy: Option<String>,
    cfg_tabs: SingleSelectModel,
    sys_tabs: SingleSelectModel,
    diff_tabs: SingleSelectModel,
    app_tabs: SingleSelectModel,
    // The tree:
    draft: Values,
    active: Values,
    filter: String,
    /// The area of the configuration being shown.
    area: &'static str,
    /// Narrows the side panel's choices.
    pick_filter: String,
    // The side panel:
    selected: Option<Path>,
    own: Vec<Choice>,
    help: String,
    entry: String,
    // Changes:
    comment: String,
    confirmed: bool,
    minutes: String,
    at: String,
    /// The earlier configuration being compared with the one in effect.
    comparing: Option<u32>,
    // Detail pages:
    change: Option<u64>,
    // Apps:
    apps: Loaded<Vec<App>>,
    app_search: String,
    app_results: Option<Loaded<Vec<App>>>,
    desktop: Option<Loaded<app::DesktopStatus>>,
    /// What is being installed, removed or remembered.
    app_work: Option<String>,
    /// Remembering what is installed: one at a time.
    syncing: bool,
    tool_search: String,
    tool_results: Option<Loaded<Vec<Tool>>>,
    // Backup:
    remote_url: String,
    machine_key: Option<Result<String, String>>,
    gh_code: Option<DeviceCode>,
    /// Which sign-in is current: what an earlier or cancelled one reports is ignored.
    gh_attempt: u64,
    /// The GitHub sign-in, for this setup only: in memory, gone when the window closes.
    github: Option<(GitHubToken, String)>,
    repo_name: String,
    /// The repository the sign-in set up, waiting for this machine's key.
    gh_repo: Option<(String, String)>,
    proposal: Option<u64>,
    // Commands:
    line: String,
    typed_cf: bool,
    edit: Vec<String>,
    offers: Vec<Choice>,
    asked: u64,
    log: Vec<(String, String)>,
    /// Ask to restart once the terminal has confirmed the change (`Confirm, then restart`).
    restart_after: bool,
    /// What running commands are asking, one at a time.
    questions: VecDeque<(String, Reply)>,
}

/// The way back to a command waiting for a person's yes or no.
#[derive(Clone)]
pub struct Reply(Arc<Mutex<Option<std::sync::mpsc::Sender<bool>>>>);

impl std::fmt::Debug for Reply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Reply")
    }
}

impl Reply {
    fn send(&self, yes: bool) {
        if let Some(back) = self.0.lock().ok().and_then(|mut b| b.take()) {
            let _ = back.send(yes);
        }
    }
}

/// Puts a command's question on the window and waits for the answer; on a thread that may wait.
fn asker(ui: Sender<Message>) -> impl Fn(String) -> bool + Send {
    move |question| {
        let mut ui = ui.clone();
        let (back, answer) = std::sync::mpsc::channel();
        let sent = cosmic::iced::futures::executor::block_on(ui.send(Message::Asked(question, Reply(Arc::new(Mutex::new(Some(back)))))));
        sent.is_ok() && answer.recv().unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Glance(Result<Glance, String>),
    Machine(Result<Machine, String>),
    History(Result<Vec<Row>, String>),
    Proposals(Result<Vec<Proposal>, String>),
    Output(String, Result<String, String>),
    /// Do this command, here or in a terminal.
    Act(At, String),
    Did(String, Result<String, String>),
    Closed(String, Result<bool, String>),
    /// A command asks a person: its question, and the way back for the answer.
    Asked(String, Reply),
    Answer(bool),
    ConfirmDone(Result<String, String>),
    Confirm,
    Restart,
    LogOut,
    Review,
    Approve,
    CfgTab(Entity),
    SysTab(Entity),
    DiffTab(Entity),
    Fill(How, String),
    Filled(String, Typed),
    // The tree:
    Select(Path),
    Selected(Path, Vec<Choice>, Vec<Choice>),
    CloseDrawer,
    Pick(Path, String),
    Picked(Path, String, Vec<Choice>),
    Filter(String),
    Entry(String),
    SetEntry,
    SetBool(bool),
    SetAt(Path, bool),
    Area(&'static str),
    AddSetting,
    PickFilter(String),
    Find,
    // Changes:
    Comment(String),
    Confirmed(bool),
    Minutes(String),
    AtTime(String),
    Commit,
    Compare(u32),
    // Detail pages:
    OpenChange(u64),
    OpenProposal(u64),
    Back,
    // Apps:
    AppTab(Entity),
    Apps(Result<Vec<App>, String>),
    AppSearch(String),
    FindApp,
    AppsFound(Result<Vec<App>, String>),
    Install(String),
    Uninstall(String),
    OpenApp(String),
    AppDone(String, Result<(), String>),
    Synced(Result<bool, String>),
    DesktopLoaded(Result<app::DesktopStatus, String>),
    DesktopSave,
    DesktopSaved(Result<app::Synced, String>),
    DesktopSaving(app::Saving),
    DesktopSavingDone(Result<String, String>),
    ToolSearch(String),
    FindTool,
    ToolsFound(Result<String, String>),
    AddTool(String),
    RemoveTool(String),
    ToolDone(String, Result<(), String>),
    // Backup:
    RemoteUrl(String),
    Connect,
    Connected(Result<String, String>),
    ShowKey,
    KeyShown(Result<String, String>),
    CopyKey(String),
    OpenUrl(String),
    GhStart,
    GhCode(u64, Result<DeviceCode, String>),
    GhSigned(u64, Result<(GitHubToken, String), String>),
    GhCancel,
    RepoName(String),
    GhCreate,
    GhCreated(Result<String, String>),
    GhFinish,
    GhKey(Result<String, String>),
    GhKeyAdded(Result<(), String>),
    // Commands:
    Line(String),
    Offers(u64, Vec<Choice>),
    Offer(String),
    RunLine,
    Typed(String, Typed),
}

impl cosmic::Application for Settings {
    type Executor = cosmic::executor::Default;
    type Flags = Option<String>;
    type Message = Message;
    const APP_ID: &'static str = "org.torqos.Settings";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, start: Option<String>) -> (Self, Task<cosmic::Action<Message>>) {
        let start = start.unwrap_or_default();
        let want_page = match start.as_str() {
            "configuration" | "changes" => Page::Configuration,
            "history" => Page::History,
            "proposals" => Page::Proposals,
            "commands" => Page::Commands,
            "apps" => Page::Apps,
            "desktop" => Page::Desktop,
            "backup" => Page::System,
            s if VIEWS.iter().any(|v| v.id == s) => Page::System,
            _ => Page::Overview,
        };
        let mut nav = nav_bar::Model::default();
        for (name, page, glyph) in [
            ("Overview", Page::Overview, "emblem-ok-symbolic"),
            ("Configuration", Page::Configuration, "preferences-system-symbolic"),
            ("Apps", Page::Apps, "system-software-install-symbolic"),
            ("Desktop", Page::Desktop, "preferences-desktop-wallpaper-symbolic"),
            ("History", Page::History, "document-open-recent-symbolic"),
            ("Proposals", Page::Proposals, "mail-message-new-symbolic"),
            ("System", Page::System, "computer-symbolic"),
            ("Command Line", Page::Commands, "utilities-terminal-symbolic"),
        ] {
            let item = nav.insert().text(name).data(page).icon(icon::from_name(glyph));
            if page == want_page {
                item.activate();
            }
        }
        let mut cfg_tabs = SingleSelectModel::default();
        for (name, tab) in [("Settings", Tab::Settings), ("Your Changes", Tab::Changes), ("Earlier Versions", Tab::Earlier)] {
            let item = cfg_tabs.insert().text(name).data(tab);
            if (tab == Tab::Changes) == (start == "changes") && (tab == Tab::Settings || start == "changes") {
                item.activate();
            }
        }
        let mut sys_tabs = SingleSelectModel::default();
        for (i, v) in VIEWS.iter().enumerate() {
            let item = sys_tabs.insert().text(v.name).data(v);
            if v.id == start || (i == 0 && !VIEWS.iter().any(|v| v.id == start)) {
                item.activate();
            }
        }
        let backup = sys_tabs.insert().text("Backup").data(BackupTab).id();
        if start == "backup" {
            sys_tabs.activate(backup);
        }
        let mut diff_tabs = SingleSelectModel::default();
        diff_tabs.insert().text("What changed").data(DiffTab::Plain).activate();
        diff_tabs.insert().text("Exact files").data(DiffTab::Files);
        let mut app_tabs = SingleSelectModel::default();
        app_tabs.insert().text("Apps").data(AppTab::Apps).activate();
        app_tabs.insert().text("Command-line tools").data(AppTab::Tools);
        let mut app = Settings {
            core,
            nav,
            console: Console::start(),
            glance: Loaded::Waiting,
            machine: Loaded::Waiting,
            history: Loaded::Waiting,
            proposals: Loaded::Waiting,
            outputs: HashMap::new(),
            note: None,
            busy: None,
            cfg_tabs,
            sys_tabs,
            diff_tabs,
            app_tabs,
            draft: Values::new(),
            active: Values::new(),
            filter: String::new(),
            area: app::AREAS[0],
            pick_filter: String::new(),
            selected: None,
            own: vec![],
            help: String::new(),
            entry: String::new(),
            comment: String::new(),
            confirmed: false,
            minutes: "10".into(),
            at: String::new(),
            comparing: None,
            change: None,
            apps: Loaded::Waiting,
            app_search: String::new(),
            app_results: None,
            desktop: None,
            app_work: None,
            syncing: false,
            tool_search: String::new(),
            tool_results: None,
            remote_url: String::new(),
            machine_key: None,
            gh_code: None,
            gh_attempt: 0,
            github: None,
            repo_name: format!("torqos-{}", app::host()),
            gh_repo: None,
            proposal: None,
            line: String::new(),
            typed_cf: false,
            edit: vec![],
            offers: vec![],
            asked: 0,
            log: vec![],
            restart_after: false,
            questions: VecDeque::new(),
        };
        let title = match app.core.main_window_id() {
            Some(id) => app.set_window_title("TorqOS Settings".into(), id),
            None => Task::none(),
        };
        let load = app.load();
        (app, Task::batch([title, load]))
    }

    fn nav_model(&self) -> Option<&nav_bar::Model> {
        Some(&self.nav)
    }

    /// A page chosen in the navigation: libcosmic leaves switching to it to the application.
    fn on_nav_select(&mut self, id: nav_bar::Id) -> Task<cosmic::Action<Message>> {
        self.nav.activate(id);
        self.note = None;
        self.change = None;
        self.proposal = None;
        self.close_drawer();
        self.load()
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        vec![widget::button::icon(icon::from_name("view-refresh-symbolic")).tooltip("Ask the broker again").on_press(Message::Refresh).into()]
    }

    /// What a running command asks a person (commit's review, an undo, a delete): the yes or no a terminal
    /// would ask. An approval the system needs comes after it, in COSMIC's own dialog.
    fn dialog(&self) -> Option<Element<'_, Message>> {
        let (question, _) = self.questions.front()?;
        let body = widget::container(widget::scrollable(widget::text::monotext(question.trim_end().to_string()).width(Length::Fill))).max_height(360.0);
        Some(
            widget::dialog()
                .title("Go ahead?")
                .control(body)
                .primary_action(widget::button::suggested("Yes").on_press(Message::Answer(true)))
                .secondary_action(widget::button::standard("No").on_press(Message::Answer(false)))
                .into(),
        )
    }

    /// The side panel: one setting, what it is, and the control for it.
    fn context_drawer(&self) -> Option<context_drawer::ContextDrawer<'_, Message>> {
        if !self.core.window.show_context {
            return None;
        }
        let path = self.selected.as_ref()?;
        let title = path.last().map_or("Add a setting".to_string(), |w| app::label(w));
        Some(context_drawer::context_drawer(self.inspector(path), Message::CloseDrawer).title(title))
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Refresh => return self.load(),
            Message::Glance(r) => self.glance = r.into(),
            Message::Machine(r) => self.machine = r.into(),
            Message::History(r) => self.history = r.into(),
            Message::Proposals(r) => self.proposals = r.into(),
            Message::Output(c, r) => {
                // Nothing left to commit: what was typed for the last commit must not name the next.
                if c == COMPARE && matches!(&r, Ok(t) if t.starts_with("No changes")) {
                    self.comment.clear();
                    self.at.clear();
                    self.confirmed = false;
                }
                if let Ok(text) = &r {
                    if c == DRAFT {
                        self.draft = values(text);
                    } else if c == ACTIVE {
                        self.active = values(text);
                    }
                }
                let got_commits = c == COMMITS;
                self.outputs.insert(c, r.into());
                if got_commits {
                    return self.read_change();
                }
            }
            Message::Act(at, line) => return self.act(at, line),
            Message::Did(what, r) => {
                self.busy = None;
                self.note = Some(match r {
                    Ok(out) => (if out.trim().is_empty() { format!("Done: {what}") } else { out.trim().to_string() }, false),
                    Err(e) => (e, true),
                });
                return self.load();
            }
            Message::Closed(what, r) => {
                self.busy = None;
                let restart = std::mem::take(&mut self.restart_after);
                self.note = Some(match r {
                    Ok(true) => (format!("Done: {what}"), false),
                    Ok(false) => (format!("`{what}` did not finish: the terminal said why."), true),
                    Err(e) => (e, true),
                });
                if restart && self.note.as_ref().is_some_and(|(_, failed)| !failed) {
                    app::ask_restart();
                }
                return self.load();
            }
            Message::Confirm => {
                self.restart_after = matches!(&self.glance, Loaded::Got(g) if matches!(g.needs, Needs::Restart(_)));
                self.busy = Some("confirming".into());
                self.note = None;
                return cosmic::task::future(async { Message::ConfirmDone(blocking(app::confirm).await) });
            }
            Message::ConfirmDone(r) => {
                self.busy = None;
                let restart = std::mem::take(&mut self.restart_after);
                if restart && r.is_ok() {
                    app::ask_restart();
                }
                self.note = Some(match r {
                    Ok(out) => (out.trim().to_string(), false),
                    Err(e) => (e, true),
                });
                return self.load();
            }
            Message::Asked(question, reply) => self.questions.push_back((question, reply)),
            Message::Answer(yes) => {
                if let Some((_, reply)) = self.questions.pop_front() {
                    reply.send(yes);
                }
            }
            Message::Restart => app::ask_restart(),
            Message::LogOut => app::ask_log_out(),
            Message::Review => {
                self.go(Page::Configuration);
                let id = self.cfg_tabs.iter().find(|&i| self.cfg_tabs.data::<Tab>(i) == Some(&Tab::Changes));
                if let Some(id) = id {
                    self.cfg_tabs.activate(id);
                }
                return self.load();
            }
            Message::Approve => {
                self.go(Page::Proposals);
                return self.load();
            }
            Message::CfgTab(id) => {
                self.cfg_tabs.activate(id);
                self.close_drawer();
                return self.load();
            }
            Message::SysTab(id) => {
                self.sys_tabs.activate(id);
                return self.load();
            }
            Message::DiffTab(id) => self.diff_tabs.activate(id),
            Message::Fill(how, command) => {
                // The Command Line's session, in the mode the command is of.
                let console = self.console.clone();
                let step = match (how == How::FillCf, self.typed_cf) {
                    (true, false) => "configure",
                    (true, true) => "top",
                    (false, true) => "exit configuration-mode",
                    (false, false) => "",
                };
                return cosmic::task::future(async move {
                    let t = off(move || console.typed(step, |_| false)).await;
                    Message::Filled(command, t.unwrap_or_else(|e| Typed { output: Err(e), configuration: false, edit: vec![] }))
                });
            }
            Message::Filled(command, t) => {
                self.typed_cf = t.configuration;
                self.edit = t.edit;
                self.line = command;
                self.go(Page::Commands);
                return self.offer();
            }

            Message::Select(path) => {
                self.entry.clear();
                self.own.clear();
                self.help.clear();
                self.pick_filter.clear();
                self.selected = Some(path.clone());
                self.set_show_context(true);
                return self.read_selected(path);
            }
            Message::Selected(path, own, parent) => {
                if self.selected.as_ref() == Some(&path) {
                    self.help = path.last().and_then(|w| parent.iter().find(|c| &c.word == w)).map(|c| c.help.clone()).unwrap_or_default();
                    self.own = own;
                }
            }
            Message::CloseDrawer => self.close_drawer(),
            Message::Pick(path, word) => {
                // A word that ends the line is a value to set; any other is a place to go down to.
                let console = self.console.clone();
                let before = format!("{} ", self.line_at("set", &path, Some(&word)));
                return cosmic::task::future(async move {
                    let got = off(move || console.choices(At::Draft, &before)).await.unwrap_or_default();
                    Message::Picked(path, word, got)
                });
            }
            Message::Picked(path, word, next) => {
                // The person has moved on: this word was for a place they have left.
                if self.selected.as_ref() != Some(&path) {
                    return Task::none();
                }
                if next.is_empty() {
                    self.note = Some((format!("Could not read what may follow `{word}`."), true));
                    return Task::none();
                }
                // `<[Enter]>` alone: the line is whole. With other words (`enable`: true, false) there is a choice to show.
                if next.iter().all(|c| c.word == "<[Enter]>") {
                    let line = self.line_at("set", &path, Some(&word));
                    return self.act(At::Draft, line);
                }
                let mut below = path;
                below.push(word);
                return self.update(Message::Select(below));
            }
            Message::Filter(s) => self.filter = s,
            Message::Area(a) => {
                self.area = a;
                self.filter.clear();
            }
            Message::AddSetting => return self.update(Message::Select(vec![])),
            Message::PickFilter(s) => self.pick_filter = s,
            Message::SetAt(path, on) => {
                let line = self.line_at("set", &path, Some(if on { "true" } else { "false" }));
                return self.act(At::Draft, line);
            }
            Message::Entry(s) => self.entry = s,
            Message::SetEntry => {
                let value = self.entry.trim().to_string();
                if let (false, Some(path)) = (value.is_empty(), self.selected.clone()) {
                    self.entry.clear();
                    let line = self.line_at("set", &path, Some(&value));
                    return self.act(At::Draft, line);
                }
            }
            Message::SetBool(on) => {
                if let Some(path) = self.selected.clone() {
                    let line = self.line_at("set", &path, Some(if on { "true" } else { "false" }));
                    return self.act(At::Draft, line);
                }
            }
            Message::Find => {
                let terms = self.filter.trim().to_string();
                if !terms.is_empty() {
                    return self.read(At::Read, format!("help {terms}"));
                }
            }

            Message::Comment(s) => self.comment = s,
            Message::Confirmed(b) => self.confirmed = b,
            Message::Minutes(s) => self.minutes = s,
            Message::AtTime(s) => self.at = s,
            Message::Commit => {
                let mut line = "commit".to_string();
                if !self.comment.trim().is_empty() {
                    line.push_str(&format!(" comment {}", app::line(&[self.comment.trim()])));
                }
                if self.confirmed {
                    line.push_str(&format!(" confirmed {}", self.minutes.trim()));
                }
                if !self.at.trim().is_empty() {
                    line.push_str(&format!(" at {}", app::line(&[self.at.trim()])));
                }
                return self.act(At::Draft, line);
            }
            Message::Compare(n) => {
                self.comparing = Some(n);
                return self.read(At::Read, format!("show system rollback {n} compare 0"));
            }

            Message::OpenChange(seq) => {
                self.change = Some(seq);
                self.note = None;
                return self.read_change();
            }
            Message::OpenProposal(seq) => {
                self.proposal = Some(seq);
                self.note = None;
                let key = format!("proposal {seq}");
                return cosmic::task::future(async move {
                    let got = blocking(move || app::proposal_effect(seq)).await;
                    Message::Output(key, got)
                });
            }
            Message::Back => {
                self.change = None;
                self.proposal = None;
                self.note = None;
                return self.load();
            }

            Message::AppTab(id) => {
                self.app_tabs.activate(id);
                return self.load();
            }
            Message::Apps(r) => {
                let ok = r.is_ok();
                self.apps = r.into();
                // Whatever came from the Store, or was just installed or removed, is remembered.
                if ok && !self.syncing {
                    self.syncing = true;
                    return cosmic::task::future(async { Message::Synced(blocking(app::sync_apps).await) });
                }
            }
            Message::Synced(r) => {
                self.syncing = false;
                if let Err(e) = r {
                    self.note = Some((format!("Your apps are installed, but could not be saved to your configuration yet: {e}"), true));
                }
            }
            Message::AppSearch(s) => {
                self.app_search = s;
                if self.app_search.trim().is_empty() {
                    self.app_results = None;
                }
            }
            Message::FindApp => {
                let terms = self.app_search.trim().to_string();
                if !terms.is_empty() {
                    self.app_results = Some(Loaded::Waiting);
                    return cosmic::task::future(async move { Message::AppsFound(blocking(move || app::search_apps(&terms)).await) });
                }
            }
            Message::AppsFound(r) => self.app_results = Some(r.into()),
            Message::Install(id) => {
                self.app_work = Some(format!("Installing {id}…"));
                self.note = None;
                return cosmic::task::future(async move {
                    let shown = id.clone();
                    Message::AppDone(shown, blocking(move || app::install_app(&id)).await)
                });
            }
            Message::Uninstall(id) => {
                self.app_work = Some(format!("Removing {id}…"));
                self.note = None;
                return cosmic::task::future(async move {
                    let shown = id.clone();
                    Message::AppDone(shown, blocking(move || app::remove_app(&id)).await)
                });
            }
            Message::OpenApp(id) => app::open_app(&id),
            Message::AppDone(id, r) => {
                self.app_work = None;
                if let Err(e) = r {
                    self.note = Some((format!("{id}: {e}"), true));
                }
                self.app_search.clear();
                self.app_results = None;
                return self.load();
            }
            Message::ToolSearch(s) => {
                self.tool_search = s;
                if self.tool_search.trim().is_empty() {
                    self.tool_results = None;
                }
            }
            Message::FindTool => {
                let terms = self.tool_search.trim().to_string();
                if !terms.is_empty() {
                    self.tool_results = Some(Loaded::Waiting);
                    let console = self.console.clone();
                    return cosmic::task::future(async move {
                        let got = off(move || console.run(At::Read, &format!("show packages {terms}"))).await.and_then(|r| r);
                        Message::ToolsFound(got)
                    });
                }
            }
            Message::ToolsFound(r) => self.tool_results = Some(r.map(|t| app::tools_of(&t)).into()),
            Message::AddTool(name) => {
                self.app_work = Some(format!("Adding {name}: this builds your system, and can take a few minutes…"));
                self.note = None;
                return cosmic::task::future(async move {
                    let shown = name.clone();
                    Message::ToolDone(shown, blocking(move || app::add_tool(&name)).await)
                });
            }
            Message::RemoveTool(name) => {
                self.app_work = Some(format!("Removing {name}…"));
                self.note = None;
                return cosmic::task::future(async move {
                    let shown = name.clone();
                    Message::ToolDone(shown, blocking(move || app::remove_tool(&name)).await)
                });
            }
            Message::ToolDone(name, r) => {
                self.app_work = None;
                self.note = Some(match r {
                    Ok(()) => (format!("Done: {name}"), false),
                    Err(e) => (e, true),
                });
                self.tool_search.clear();
                self.tool_results = None;
                return self.load();
            }

            Message::RemoteUrl(s) => self.remote_url = s,
            Message::Connect => {
                let url = self.remote_url.trim().to_string();
                if !url.is_empty() {
                    let line = app::line(&["set", "torq", "remote", "url", &url]);
                    return self.act_then(At::Draft, line, Message::Connected);
                }
            }
            Message::Connected(r) => {
                self.busy = None;
                return match r {
                    Ok(_) => {
                        self.remote_url.clear();
                        self.update(Message::Review)
                    }
                    Err(e) => {
                        self.note = Some((e, true));
                        Task::none()
                    }
                };
            }
            Message::ShowKey => return self.act_then(At::Read, "request system remote key".into(), Message::KeyShown),
            Message::KeyShown(r) => {
                self.busy = None;
                self.machine_key = Some(r);
            }
            Message::CopyKey(key) => return cosmic::iced::clipboard::write(key),
            Message::OpenUrl(url) => app::open_url(&url),

            Message::GhStart => {
                self.gh_attempt += 1;
                let attempt = self.gh_attempt;
                self.note = None;
                return cosmic::task::future(async move { Message::GhCode(attempt, blocking(app::github_start).await) });
            }
            Message::GhCancel => {
                self.gh_attempt += 1;
                self.gh_code = None;
            }
            Message::GhCode(attempt, _) | Message::GhSigned(attempt, _) if attempt != self.gh_attempt => {}
            Message::GhCode(attempt, r) => match r {
                Ok(code) => {
                    let waiting_on = code.clone();
                    self.gh_code = Some(code);
                    return cosmic::task::future(async move { Message::GhSigned(attempt, blocking(move || app::github_wait(&waiting_on)).await) });
                }
                Err(e) => self.note = Some((e, true)),
            },
            Message::GhSigned(_, r) => {
                self.gh_code = None;
                match r {
                    Ok(signed) => self.github = Some(signed),
                    Err(e) => self.note = Some((e, true)),
                }
            }
            Message::RepoName(s) => self.repo_name = s,
            Message::GhCreate => {
                if let Some((token, login)) = self.github.clone() {
                    let name = self.repo_name.trim().to_string();
                    self.busy = Some(format!("Creating {login}/{name}"));
                    self.note = None;
                    return cosmic::task::future(async move { Message::GhCreated(blocking(move || app::github_create_repo(&token, &login, &name)).await) });
                }
            }
            Message::GhCreated(r) => {
                self.busy = None;
                match r {
                    Ok(url) => {
                        self.gh_repo = app::github_repo(&url);
                        self.remote_url = url;
                        return self.update(Message::Connect);
                    }
                    Err(e) => self.note = Some((e, true)),
                }
            }
            Message::GhFinish => return self.act_then(At::Read, "request system remote key".into(), Message::GhKey),
            Message::GhKey(r) => {
                self.busy = None;
                let public = r.as_ref().ok().and_then(|t| t.lines().next()).unwrap_or_default().to_string();
                self.machine_key = Some(r);
                if let (false, Some((token, _)), Some((owner, repo))) = (public.is_empty(), self.github.clone(), self.gh_repo.clone()) {
                    return cosmic::task::future(async move { Message::GhKeyAdded(blocking(move || app::github_add_key(&token, &owner, &repo, &public)).await) });
                }
            }
            Message::GhKeyAdded(r) => {
                match r {
                    Ok(()) => {
                        // Done with the sign-in: only the machine's own key, for this one repository, stays.
                        self.github = None;
                        self.gh_repo = None;
                        self.note = Some(("Connected. The first copy goes up within 15 minutes, and after every change from now on. TorqOS has let go of your sign-in; GitHub lists it under Settings, Applications, where you can revoke it.".into(), false));
                    }
                    Err(e) => self.note = Some((e, true)),
                }
                return self.load();
            }

            Message::DesktopLoaded(r) => self.desktop = Some(r.into()),
            Message::DesktopSave => {
                self.busy = Some("Saving your desktop".into());
                self.note = None;
                return cosmic::task::future(async { Message::DesktopSaved(blocking(|| app::sync_desktop(true)).await) });
            }
            Message::DesktopSaved(r) => {
                self.busy = None;
                self.note = Some(match r {
                    Ok(s) => {
                        let mut said = match s.saved {
                            Some(seq) => format!("Saved your desktop (change #{seq})."),
                            None => "Your desktop was saved already: nothing had changed.".to_string(),
                        };
                        if s.applied > 0 {
                            said.push_str(&format!(" {} settings from your configuration were put on this desktop.", s.applied));
                        }
                        (said, false)
                    }
                    Err(e) => (e, true),
                });
                return self.load();
            }
            Message::DesktopSaving(how) => {
                self.busy = Some("Changing when your desktop is saved".into());
                self.note = None;
                return cosmic::task::future(async move { Message::DesktopSavingDone(blocking(move || app::desktop_saving(how)).await) });
            }
            Message::DesktopSavingDone(r) => {
                self.busy = None;
                self.note = Some(match r {
                    Ok(s) => (s.trim().to_string(), false),
                    Err(e) => (e, true),
                });
                return self.load();
            }

            Message::Line(s) => {
                self.line = s;
                return self.offer();
            }
            Message::Offers(n, got) => {
                if n == self.asked {
                    self.offers = got;
                }
            }
            Message::Offer(word) => {
                let keep = if self.line.ends_with(char::is_whitespace) || self.line.is_empty() { self.line.len() } else { self.line.rfind(char::is_whitespace).map_or(0, |i| i + 1) };
                self.line.truncate(keep);
                self.line.push_str(&word);
                self.line.push(' ');
                return self.offer();
            }
            Message::RunLine => {
                let line = self.line.trim().to_string();
                if line.is_empty() {
                    return Task::none();
                }
                let mode = if self.typed_cf { Mode::Configuration } else { Mode::Operational };
                if app::needs_terminal(mode, &line) {
                    self.line.clear();
                    let (script, draft) = app::terminal_script(mode, &self.edit, &line);
                    return self.terminal(script, draft, line);
                }
                self.busy = Some(line.clone());
                let console = self.console.clone();
                return Task::stream(cosmic::iced::stream::channel(4, async move |mut out: Sender<Message>| {
                    let ask = asker(out.clone());
                    let shown = line.clone();
                    let t = off(move || console.typed(&line, ask)).await;
                    let _ = out.send(Message::Typed(shown, t.unwrap_or_else(|e| Typed { output: Err(e), configuration: false, edit: vec![] }))).await;
                }))
                .map(cosmic::Action::App);
            }
            Message::Typed(line, t) => {
                self.busy = None;
                self.typed_cf = t.configuration;
                self.edit = t.edit;
                self.log.insert(0, (line, t.output.unwrap_or_else(|e| format!("error: {e}"))));
                self.log.truncate(50);
                self.line.clear();
                return self.offer();
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let body = match self.page() {
            Page::Overview => self.overview(),
            Page::Configuration => self.configuration(),
            Page::Apps => self.apps_page(),
            Page::Desktop => self.desktop_page(),
            Page::History => match self.change {
                Some(seq) => self.change_page(seq),
                None => self.history(),
            },
            Page::Proposals => match self.proposal {
                Some(seq) => self.proposal_page(seq),
                None => self.proposals(),
            },
            Page::System => self.system(),
            Page::Commands => self.commands(),
        };
        let mut content = widget::column::with_capacity(3).spacing(spacing.space_m).max_width(960);
        if let Some(what) = &self.busy {
            content = content.push(widget::text::body(format!("Working: {what}…")));
        }
        if let Some((text, failed)) = &self.note {
            content = content.push(note(text, *failed));
        }
        content = content.push(body);
        widget::scrollable(widget::container(content).width(Length::Fill).center_x(Length::Fill).padding(spacing.space_m)).into()
    }
}

/// A configuration's `| display set` lines, by setting.
fn values(shown: &str) -> Values {
    let mut out = Values::new();
    for (path, value) in app::settings_of(shown) {
        out.entry(path).or_default().push(value);
    }
    out
}

/// The words that may follow a place, without what is not a word.
fn words_of(c: &[Choice]) -> impl Iterator<Item = &Choice> {
    c.iter().filter(|c| !c.is_placeholder() && c.word != "|")
}



impl Settings {
    fn page(&self) -> Page {
        self.nav.active_data::<Page>().copied().unwrap_or(Page::Overview)
    }

    fn go(&mut self, page: Page) {
        let id = self.nav.iter().find(|&i| self.nav.data::<Page>(i) == Some(&page));
        if let Some(id) = id {
            self.nav.activate(id);
        }
    }

    fn close_drawer(&mut self) {
        self.selected = None;
        self.set_show_context(false);
    }

    fn tab(&self) -> Tab {
        self.cfg_tabs.active_data::<Tab>().copied().unwrap_or(Tab::Settings)
    }

    fn view_shown(&self) -> Option<&'static View> {
        self.sys_tabs.active_data::<&'static View>().copied()
    }

    /// Ask again for what this page shows (and the state every page's actions depend on).
    fn load(&mut self) -> Task<cosmic::Action<Message>> {
        let console = self.console.clone();
        let mut tasks = vec![cosmic::task::future(async move {
            Message::Glance(blocking(move || app::glance(app::session_recorded().as_deref(), console.uncommitted())).await)
        })];
        match self.page() {
            Page::Overview => tasks.push(cosmic::task::future(async { Message::Machine(blocking(app::machine).await) })),
            Page::History => {
                tasks.push(cosmic::task::future(async { Message::History(blocking(|| app::history(HISTORY)).await) }));
                tasks.push(self.read(At::Read, COMMITS.into()));
            }
            Page::Proposals => tasks.push(cosmic::task::future(async { Message::Proposals(blocking(app::proposals).await) })),
            Page::Configuration => match self.tab() {
                Tab::Settings => {
                    tasks.push(self.read(At::Draft, DRAFT.into()));
                    tasks.push(self.read(At::Read, ACTIVE.into()));
                    if let Some(path) = self.selected.clone() {
                        tasks.push(self.read_selected(path));
                    }
                }
                Tab::Changes => tasks.push(self.read(At::Draft, COMPARE.into())),
                Tab::Earlier => tasks.push(self.read(At::Read, COMMITS.into())),
            },
            Page::Desktop => tasks.push(cosmic::task::future(async { Message::DesktopLoaded(blocking(app::desktop_status).await) })),
            Page::Apps => {
                tasks.push(cosmic::task::future(async { Message::Apps(blocking(app::installed_apps).await) }));
                tasks.push(self.read(At::Read, ACTIVE.into()));
            }
            Page::System => {
                if self.sys_tabs.active_data::<BackupTab>().is_some() {
                    tasks.push(self.read(At::Read, REMOTE.into()));
                    tasks.push(self.read(At::Read, ACTIVE.into()));
                }
                if let Some(v) = self.view_shown() {
                    tasks.extend(v.sections.iter().map(|(_, c)| self.read(At::Read, c.to_string())));
                }
            }
            Page::Commands => tasks.push(self.offer()),
        }
        Task::batch(tasks)
    }

    fn read(&self, at: At, command: String) -> Task<cosmic::Action<Message>> {
        let console = self.console.clone();
        cosmic::task::future(async move {
            let c = command.clone();
            let got = off(move || console.run(at, &c)).await.and_then(|r| r);
            Message::Output(command, got)
        })
    }

    /// `verb` at a place in the configuration, with one more word.
    fn line_at(&self, verb: &str, path: &[String], last: Option<&str>) -> String {
        let mut w: Vec<&str> = vec![verb];
        w.extend(path.iter().map(String::as_str));
        w.extend(last);
        app::line(&w)
    }

    /// What the side panel needs of a setting: what may follow it, and its help, from the place above it.
    fn read_selected(&self, path: Path) -> Task<cosmic::Action<Message>> {
        let console = self.console.clone();
        let own = format!("{} ", self.line_at("set", &path, None));
        let parent = format!("{} ", self.line_at("set", &path[..path.len().saturating_sub(1)], None));
        cosmic::task::future(async move {
            let (a, b) = off(move || (console.choices(At::Draft, &own), console.choices(At::Draft, &parent))).await.unwrap_or_default();
            Message::Selected(path, a, b)
        })
    }

    /// The two comparisons of the change being looked at, once `show system commit` says which it is.
    fn read_change(&self) -> Task<cosmic::Action<Message>> {
        let Some((older, newer)) = self.change_configurations() else { return Task::none() };
        let plain = format!("show system rollback {older} compare {newer}");
        let files = format!("{plain} | display diff");
        Task::batch([self.read(At::Read, plain), self.read(At::Read, files)])
    }

    /// The configuration before the change being looked at, and its own.
    fn change_configurations(&self) -> Option<(u32, u32)> {
        let Some(Loaded::Got(shown)) = self.outputs.get(COMMITS) else { return None };
        let n = app::rollback_of(shown, self.change?)?;
        app::configurations(shown).iter().any(|(k, _)| *k == n + 1).then_some((n + 1, n))
    }

    /// Run a command: in this window, or, if it asks a person, in a terminal.
    fn act(&mut self, at: At, line: String) -> Task<cosmic::Action<Message>> {
        // Slow, and only loads the draft: with the draft's session, so that the pages' reads go on.
        let (at, line) = if at == At::Read && app::loads_draft(&line) { (At::Draft, format!("run {line}")) } else { (at, line) };
        let mode = if at == At::Read { Mode::Operational } else { Mode::Configuration };
        if app::needs_terminal(mode, &line) {
            let (script, draft) = app::terminal_script(mode, &[], &line);
            return self.terminal(script, draft, line);
        }
        self.busy = Some(line.clone());
        self.note = None;
        self.act_then(at, line.clone(), move |r| Message::Did(line, r))
    }

    /// Run a line that may ask a person something, its answer going to `done`.
    fn act_then(&mut self, at: At, line: String, done: impl FnOnce(Result<String, String>) -> Message + Send + 'static) -> Task<cosmic::Action<Message>> {
        self.busy = Some(line.clone());
        self.note = None;
        let console = self.console.clone();
        Task::stream(cosmic::iced::stream::channel(4, async move |mut out: Sender<Message>| {
            let ask = asker(out.clone());
            let got = off(move || console.act(at, &line, ask)).await.and_then(|r| r);
            let _ = out.send(done(got)).await;
        }))
        .map(cosmic::Action::App)
    }

    fn terminal(&mut self, script: String, draft: bool, what: String) -> Task<cosmic::Action<Message>> {
        self.busy = Some(format!("{what} (in the terminal)"));
        self.note = None;
        cosmic::task::future(async move {
            let got = off(move || app::in_terminal(&script, draft)).await.and_then(|r| r);
            Message::Closed(what, got)
        })
    }

    /// What may follow the Command Line's line.
    fn offer(&mut self) -> Task<cosmic::Action<Message>> {
        self.asked += 1;
        let n = self.asked;
        let console = self.console.clone();
        let before = self.line.clone();
        cosmic::task::future(async move {
            let got = off(move || console.choices(At::Typed, &before)).await.unwrap_or_default();
            Message::Offers(n, got)
        })
    }

    fn output(&self, command: &str) -> Element<'_, Message> {
        match self.outputs.get(command) {
            None | Some(Loaded::Waiting) => waiting(),
            Some(Loaded::Failed(why)) => failed(why),
            Some(Loaded::Got(text)) => widget::text::monotext(text.trim_end().to_string()).width(Length::Fill).into(),
        }
    }

    fn diff(&self, command: &str) -> Element<'_, Message> {
        match self.outputs.get(command) {
            None | Some(Loaded::Waiting) => waiting(),
            Some(Loaded::Failed(why)) => failed(why),
            Some(Loaded::Got(text)) => diff_view(text),
        }
    }

    // ── The tree ─────────────────────────────────────────────────────────────────────────────────

    fn change_at(&self, path: &[String]) -> Change {
        match (self.draft.get(path), self.active.get(path)) {
            (Some(d), Some(a)) if d == a => Change::Same,
            (Some(_), Some(_)) => Change::Changed,
            (Some(_), None) => Change::New,
            (None, Some(_)) => Change::Removed,
            (None, None) => Change::Same,
        }
    }

    /// The side panel for a setting.
    fn inspector(&self, path: &Path) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut col = widget::column::with_capacity(8).spacing(spacing.space_m);
        if !path.is_empty() {
            col = col.push(widget::text::caption(path.iter().map(|w| app::label(w)).collect::<Vec<_>>().join(" › ")));
        }
        if !self.help.is_empty() {
            col = col.push(widget::text::body(self.help.clone()));
        }
        let draft = self.draft.get(path);
        let active = self.active.get(path);

        // A switch, a box for a value, or the choices: as what may follow this setting says.
        let booleans = !self.own.is_empty() && words_of(&self.own).count() > 0 && words_of(&self.own).all(|c| c.word == "true" || c.word == "false");
        if booleans {
            let on = draft.is_some_and(|v| v.iter().any(|x| x == "true"));
            col = col.push(settings::item::builder("On").control(widget::toggler(on).on_toggle(Message::SetBool)));
        } else {
            for c in self.own.iter().filter(|c| c.is_placeholder() && c.word != "<[Enter]>") {
                col = col.push(
                    widget::column::with_capacity(3)
                        .spacing(spacing.space_xxs)
                        .push(widget::text::heading("Value"))
                        .push(widget::text::caption(c.help.clone()))
                        .push(
                            widget::row::with_capacity(2)
                                .spacing(spacing.space_xs)
                                .align_y(Alignment::Center)
                                .push(widget::text_input(c.word.clone(), &self.entry).on_input(Message::Entry).on_submit(|_| Message::SetEntry).width(Length::Fill))
                                .push(widget::button::suggested("Set").on_press(Message::SetEntry)),
                        ),
                );
            }
            let wanted = self.pick_filter.to_lowercase();
            let all: Vec<&Choice> = words_of(&self.own).filter(|c| c.word != "<[Enter]>").collect();
            if all.len() > 8 {
                col = col.push(widget::search_input("Filter", &self.pick_filter).on_input(Message::PickFilter).on_clear(Message::PickFilter(String::new())));
            }
            let words: Vec<&Choice> = all.into_iter().filter(|c| wanted.is_empty() || c.word.to_lowercase().contains(&wanted) || c.help.to_lowercase().contains(&wanted)).collect();
            if !words.is_empty() {
                let mut list = settings::section().title("Choose");
                for c in words.iter().take(LISTED) {
                    let row = settings::item_row(vec![
                        widget::column::with_capacity(2).push(widget::text::body(app::label(&c.word))).push(widget::text::caption(c.help.clone())).width(Length::Fill).into(),
                        icon::from_name("go-next-symbolic").size(16).icon().into(),
                    ])
                    .spacing(spacing.space_s);
                    list = list.add(widget::mouse_area(row).on_press(Message::Pick(path.clone(), c.word.clone())));
                }
                col = col.push(list);
            }
            if self.own.len() == 1 && self.own[0].word == "<[Enter]>" && draft.is_none() {
                col = col.push(widget::button::suggested("Use this value").on_press(Message::Act(At::Draft, self.line_at("set", path, None))));
            }
        }

        // What it is set to, each value removable.
        if let Some(values) = draft {
            let mut section = settings::section().title("In your draft");
            for v in values {
                let control: Element<'_, Message> = if values.len() > 1 {
                    widget::button::icon(icon::from_name("edit-delete-symbolic")).tooltip("Remove this one").on_press(Message::Act(At::Draft, self.line_at("delete", path, Some(v)))).into()
                } else {
                    widget::text::body("").into()
                };
                section = section.add(settings::item_row(vec![widget::text::monotext(v.clone()).width(Length::Fill).into(), control]));
            }
            col = col.push(section);
        }
        if active != draft {
            let in_effect = active.map_or("Not set".to_string(), |v| v.join(", "));
            col = col.push(widget::column::with_capacity(2).push(widget::text::heading("In effect now")).push(widget::text::monotext(in_effect)).spacing(spacing.space_xxs));
        }

        let mut buttons = widget::row::with_capacity(2).spacing(spacing.space_xs);
        if active != draft {
            buttons = buttons.push(widget::button::standard("Undo my change").on_press(Message::Act(At::Draft, format!("rollback 0 {}", app::line(&path.iter().map(String::as_str).collect::<Vec<_>>())))));
        }
        let anything_here = !path.is_empty() && (draft.is_some() || self.draft.keys().any(|k| k.len() > path.len() && k[..path.len()] == **path));
        if anything_here {
            buttons = buttons.push(widget::button::destructive("Remove").on_press(Message::Act(At::Draft, self.line_at("delete", path, None))));
        }
        col = col.push(buttons);
        if !path.is_empty() {
            col = col.push(widget::text::caption(format!("The same as: set {}", path.join(" "))));
        }
        col.into()
    }

    // ── The pages ────────────────────────────────────────────────────────────────────────────────

    fn overview(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let g = match &self.glance {
            Loaded::Waiting => return waiting(),
            Loaded::Failed(why) => return failed(why),
            Loaded::Got(g) => g,
        };
        let (mark, colour, headline) = if g.well {
            ("emblem-ok-symbolic", Colour::Good, "This machine is exactly as its history says")
        } else {
            ("dialog-warning-symbolic", Colour::Warn, "This machine needs a look")
        };
        let banner = widget::row::with_capacity(2)
            .push(colour.icon(mark, 32))
            .push(widget::column::with_capacity(2).push(widget::text::title3(headline)).push(widget::text::body(format!("TorqOS {}", g.version))))
            .spacing(spacing.space_s)
            .align_y(Alignment::Center);
        let mut page = widget::column::with_capacity(3).push(banner).spacing(spacing.space_l);

        let mut waiting_on = settings::section().title("Waiting for you");
        let mut any = false;
        if let Some(c) = &g.confirm {
            any = true;
            let label = if matches!(g.needs, Needs::Restart(_)) { "Confirm, then restart" } else { "Confirm" };
            waiting_on = waiting_on.add(attention(format!("Change #{} rolls back at {} unless you confirm it.", c.seq, c.deadline), widget::button::suggested(label).on_press(Message::Confirm)));
        }
        match (&g.needs, g.needs.line()) {
            (Needs::Restart(_), Some(l)) => {
                any = true;
                waiting_on = waiting_on.add(attention(l, widget::button::standard("Restart…").on_press(Message::Restart)));
            }
            (Needs::LogOut(_), Some(l)) => {
                any = true;
                waiting_on = waiting_on.add(attention(l, widget::button::standard("Log out…").on_press(Message::LogOut)));
            }
            _ => {}
        }
        if g.draft {
            any = true;
            waiting_on = waiting_on.add(attention("You have changes not yet committed.".to_string(), widget::button::standard("Review").on_press(Message::Review)));
        }
        if g.waiting.0 > 0 {
            any = true;
            let mut s = if g.waiting.0 == 1 { "A proposal is waiting for you.".to_string() } else { format!("{} proposals are waiting for you.", g.waiting.0) };
            if g.waiting.1 > 0 {
                s.push_str(&format!(" {} came from a session that read untrusted content.", g.waiting.1));
            }
            waiting_on = waiting_on.add(attention(s, widget::button::standard("Review").on_press(Message::Approve)));
        }
        if any {
            page = page.push(waiting_on);
        }

        if let Some(n) = g.needs.note() {
            page = page.push(widget::text::body(n));
        }
        // `show system alarms`, line by line, as it prints them.
        let mut alarms = settings::section().title("Status");
        match &self.machine {
            Loaded::Got(m) => {
                let lines = m.alarms.lines().map(str::trim).filter(|l| !l.is_empty() && !(m.well && l.trim_start_matches("✓ ").trim_end_matches('.') == headline));
                for l in lines {
                    alarms = alarms.add(settings::item_row(vec![widget::text::body(l.to_string()).width(Length::Fill).into()]));
                }
            }
            Loaded::Failed(why) => alarms = alarms.add(failed(why)),
            Loaded::Waiting => alarms = alarms.add(waiting()),
        }
        page.push(alarms).into()
    }

    fn configuration(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let tabs = widget::tab_bar::horizontal(&self.cfg_tabs).on_activate(Message::CfgTab);
        let body = match self.tab() {
            Tab::Settings => self.settings_tab(),
            Tab::Changes => self.changes_tab(),
            Tab::Earlier => self.earlier_tab(),
        };
        widget::column::with_capacity(2).push(tabs).push(body).spacing(spacing.space_m).into()
    }

    /// What is set, in the area chosen or, while searching, in every area: its path, values and change.
    fn shown_settings(&self) -> Vec<(Path, Option<Vec<String>>, Change)> {
        let wanted = self.filter.trim().to_lowercase();
        let paths: BTreeSet<&Path> = self.draft.keys().chain(self.active.keys()).collect();
        paths
            .into_iter()
            .filter(|p| {
                if wanted.is_empty() {
                    return app::area_of(p) == self.area;
                }
                let values = self.draft.get(*p).into_iter().flatten();
                p.join(" ").to_lowercase().contains(&wanted) || app::card_and_row(p).1.to_lowercase().contains(&wanted) || values.into_iter().any(|v| v.to_lowercase().contains(&wanted))
            })
            .map(|p| (p.clone(), self.draft.get(p).cloned(), self.change_at(p)))
            .collect()
    }

    /// One setting on its card: a switch for what is on or off; anything else, its value and a way in.
    fn setting_row<'a>(&self, path: &Path, values: Option<&Vec<String>>, change: Change, row: String) -> Element<'a, Message> {
        let spacing = cosmic::theme::spacing();
        let mut line = widget::row::with_capacity(4).spacing(spacing.space_s).align_y(Alignment::Center);
        line = line.push(widget::text::body(row).width(Length::Fill));
        match change {
            Change::Same => {}
            Change::New => line = line.push(tagged("new", Colour::Good)),
            Change::Changed => line = line.push(tagged("changed", Colour::Warn)),
            Change::Removed => line = line.push(tagged("removed", Colour::Bad)),
        }
        let on_off = values.filter(|v| v.len() == 1).map(|v| v[0].as_str()).filter(|v| *v == "true" || *v == "false");
        if let Some(v) = on_off {
            let at = path.clone();
            line = line.push(widget::toggler(v == "true").on_toggle(move |on| Message::SetAt(at.clone(), on)));
            return settings::item_row(vec![widget::mouse_area(line.width(Length::Fill)).on_press(Message::Select(path.clone())).into()]).into();
        }
        if let Some(v) = values {
            line = line.push(widget::text::body(v.join(", ")));
        }
        line = line.push(icon::from_name("go-next-symbolic").size(16).icon());
        widget::mouse_area(settings::item_row(vec![line.into()])).on_press(Message::Select(path.clone())).into()
    }

    fn settings_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut counts: BTreeMap<&str, (usize, bool)> = BTreeMap::new();
        for p in self.draft.keys().chain(self.active.keys().filter(|k| !self.draft.contains_key(*k))) {
            let e = counts.entry(app::area_of(p)).or_default();
            e.0 += 1;
            e.1 |= self.change_at(p) != Change::Same;
        }

        // The areas: only those with something in them, and always the one being looked at.
        let mut areas = widget::column::with_capacity(app::AREAS.len()).spacing(spacing.space_xxxs).width(Length::Fixed(170.0));
        for &area in app::AREAS.iter().filter(|a| counts.contains_key(*a) || **a == self.area) {
            let (count, changed) = counts.get(area).copied().unwrap_or_default();
            let chosen = area == self.area && self.filter.trim().is_empty();
            let name = if chosen { widget::text::heading(area) } else { widget::text::body(area) };
            let mut line = widget::row::with_capacity(3).spacing(spacing.space_xs).align_y(Alignment::Center).push(name.width(Length::Fill));
            if changed {
                line = line.push(tagged("●", Colour::Warn));
            }
            line = line.push(widget::text::caption(count.to_string()));
            areas = areas.push(widget::button::custom(line).class(if chosen { cosmic::theme::Button::Standard } else { cosmic::theme::Button::Text }).width(Length::Fill).on_press(Message::Area(area)));
        }

        // The cards: a setting's place decides which, in the order of the first it holds.
        let mut cards: Vec<(String, Vec<_>)> = vec![];
        let settings = self.shown_settings();
        for (path, values, change) in &settings {
            let (card, row) = app::card_and_row(path);
            let entry = (path, values.as_ref(), *change, row);
            match cards.iter_mut().find(|(c, _)| *c == card) {
                Some((_, rows)) => rows.push(entry),
                None => cards.push((card, vec![entry])),
            }
        }
        let mut right = widget::column::with_capacity(cards.len() + 2).spacing(spacing.space_m).width(Length::Fill);
        if cards.is_empty() {
            right = right.push(widget::text::body(if self.filter.trim().is_empty() { "Nothing is set here yet." } else { "Nothing that is set matches. Press Enter to search every setting." }));
        }
        for (card, rows) in cards {
            let mut section = settings::section().title(card);
            for (path, values, change, row) in rows {
                section = section.add(self.setting_row(path, values, change, row));
            }
            right = right.push(section);
        }
        let terms = self.filter.trim();
        if !terms.is_empty() && self.outputs.contains_key(&format!("help {terms}")) {
            right = right.push(self.output(&format!("help {terms}")));
        }
        right = right.push(widget::row::with_capacity(1).push(widget::button::standard("Add a setting").on_press(Message::AddSetting)));

        widget::column::with_capacity(3)
            .spacing(spacing.space_m)
            .push(widget::search_input("Search settings", &self.filter).on_input(Message::Filter).on_clear(Message::Filter(String::new())).on_submit(|_| Message::Find))
            .push(widget::row::with_capacity(2).spacing(spacing.space_l).push(areas).push(right))
            .into()
    }

    fn changes_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let unchanged = matches!(self.outputs.get(COMPARE), Some(Loaded::Got(t)) if t.starts_with("No changes"));
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_l);
        if unchanged {
            return page.push(widget::text::body("Your draft is the configuration in effect: nothing to commit.")).into();
        }
        page = page.push(self.diff(COMPARE));
        let mut commit = settings::section().title("Commit");
        commit = commit.add(settings::item::builder("Comment").description("What the change is for: it goes in the history").control(widget::text_input("Optional", &self.comment).on_input(Message::Comment).width(Length::FillPortion(2))));
        commit = commit.add(settings::item::builder("Roll back unless confirmed").description("If the change cuts you off, the machine undoes it by itself").control(widget::toggler(self.confirmed).on_toggle(Message::Confirmed)));
        if self.confirmed {
            commit = commit.add(settings::item::builder("Minutes to confirm in").control(widget::text_input("10", &self.minutes).on_input(Message::Minutes).width(Length::Fixed(80.0))));
        }
        commit = commit.add(settings::item::builder("Apply at a later time").description("HH:MM, or \"YYYY-MM-DD HH:MM\"; approved now, applied then").control(widget::text_input("Now", &self.at).on_input(Message::AtTime).width(Length::FillPortion(2))));
        page = page.push(commit);
        page = page.push(
            widget::row::with_capacity(3)
                .push(widget::button::suggested("Commit…").on_press(Message::Commit))
                .push(widget::button::standard("Check it builds").on_press(Message::Act(At::Draft, "commit check".into())))
                .push(widget::button::destructive("Discard my changes").on_press(Message::Act(At::Draft, "rollback 0".into())))
                .spacing(spacing.space_xs),
        );
        page.push(widget::text::caption("Commit shows you the change, then asks for your approval in COSMIC's own password dialog.")).into()
    }

    fn earlier_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_l);
        page = page.push(widget::text::body("Every configuration this machine has had. Loading one puts it in your draft: you review it and commit it like any change."));
        if let Some(n) = self.comparing {
            page = page.push(widget::text::heading(format!("Configuration {n}, against the one in effect")));
            page = page.push(self.diff(&format!("show system rollback {n} compare 0")));
        }
        let mut earlier = settings::section();
        match self.outputs.get(COMMITS) {
            Some(Loaded::Got(t)) => {
                for (n, text) in app::configurations(t).into_iter().skip(1).take(50) {
                    earlier = earlier.add(
                        settings::item_row(vec![
                            widget::text::body(text).width(Length::Fill).into(),
                            widget::button::standard("Compare").on_press(Message::Compare(n)).into(),
                            widget::button::standard("Load into draft").on_press(Message::Act(At::Draft, format!("rollback {n}"))).into(),
                        ])
                        .spacing(spacing.space_s),
                    );
                }
            }
            Some(Loaded::Failed(why)) => earlier = earlier.add(failed(why)),
            _ => earlier = earlier.add(waiting()),
        }
        page.push(earlier).into()
    }

    fn history(&self) -> Element<'_, Message> {
        let rows = match &self.history {
            Loaded::Waiting => return waiting(),
            Loaded::Failed(why) => return failed(why),
            Loaded::Got(rows) => rows,
        };
        if rows.is_empty() {
            return widget::text::body("Nothing has changed yet.").into();
        }
        let spacing = cosmic::theme::spacing();
        // Newest first, a section per day; each change as `show system ledger` says it.
        let mut days: Vec<(String, Vec<&Row>)> = vec![];
        for r in rows.iter().rev() {
            match days.last_mut() {
                Some((day, list)) if *day == r.day => list.push(r),
                _ => days.push((r.day.clone(), vec![r])),
            }
        }
        let mut page = widget::column::with_capacity(days.len() + 1).spacing(spacing.space_l);
        page = page.push(widget::text::body("Every change this machine has been through. Choose one to see exactly what it did."));
        for (day, list) in days {
            let mut section = settings::section().title(day);
            for r in list {
                section = section.add(change(r));
            }
            page = page.push(section);
        }
        page.into()
    }

    /// One change: what it did, in words and exactly.
    fn change_page(&self, seq: u64) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let row = match &self.history {
            Loaded::Got(rows) => rows.iter().find(|r| r.seq == seq),
            _ => None,
        };
        let mut page = widget::column::with_capacity(5).spacing(spacing.space_m);
        page = page.push(widget::button::text("‹ History").on_press(Message::Back));
        let Some(r) = row else { return page.push(waiting()).into() };
        page = page.push(widget::text::title3(r.text.clone()));
        let mut detail = format!("#{} · {} {}", r.seq, r.day, r.time);
        if !r.who.is_empty() {
            detail.push_str(&format!(" · {}", r.who));
        }
        if r.tainted {
            detail.push_str(" · ⚠ tainted: made after reading untrusted content");
        }
        page = page.push(widget::text::caption(detail));
        if r.undoable {
            page = page.push(widget::button::standard("Undo this change…").on_press(Message::Act(At::Read, format!("request system revert {}", r.seq))));
        }
        let Some((older, newer)) = self.change_configurations() else {
            return page.push(widget::text::body("This entry made no new configuration: there is nothing to compare.")).into();
        };
        let plain = format!("show system rollback {older} compare {newer}");
        let tab = self.diff_tabs.active_data::<DiffTab>().copied().unwrap_or(DiffTab::Plain);
        page = page.push(widget::tab_bar::horizontal(&self.diff_tabs).on_activate(Message::DiffTab));
        page.push(match tab {
            DiffTab::Plain => match self.outputs.get(&plain) {
                Some(Loaded::Got(t)) if t.starts_with("No differences") => Element::from(widget::text::body("No settings changed: this change moved the software (TorqOS or NixOS). Exact files shows how.")),
                _ => self.diff(&plain),
            },
            DiffTab::Files => self.diff(&format!("{plain} | display diff")),
        })
        .into()
    }

    fn proposals(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_l);
        match &self.proposals {
            Loaded::Waiting => page = page.push(waiting()),
            Loaded::Failed(why) => page = page.push(failed(why)),
            Loaded::Got(list) if list.is_empty() => page = page.push(widget::text::body("No proposals are waiting. When the agent proposes a change, you review it here.")),
            Loaded::Got(list) => {
                let mut section = settings::section().title("Waiting for you");
                for p in list {
                    let mut who = format!("Proposed by {}", p.by);
                    if p.tainted {
                        who.push_str(" · ⚠ tainted: made after reading untrusted content");
                    }
                    let row = settings::item_row(vec![
                        widget::column::with_capacity(2).push(widget::text::body(format!("#{}  {}", p.seq, p.reason))).push(widget::text::caption(who)).width(Length::Fill).into(),
                        icon::from_name("go-next-symbolic").size(16).icon().into(),
                    ])
                    .spacing(spacing.space_s);
                    section = section.add(widget::mouse_area(row).on_press(Message::OpenProposal(p.seq)));
                }
                page = page.push(section);
            }
        }
        page.push(actions(app::AGENT)).into()
    }

    /// One proposal: what it does, and applying it.
    fn proposal_page(&self, seq: u64) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(5).spacing(spacing.space_m);
        page = page.push(widget::button::text("‹ Proposals").on_press(Message::Back));
        if let Loaded::Got(list) = &self.proposals {
            if let Some(p) = list.iter().find(|p| p.seq == seq) {
                page = page.push(widget::text::title3(p.reason.clone()));
                let mut who = format!("#{} · proposed by {}", p.seq, p.by);
                if p.tainted {
                    who.push_str(" · ⚠ tainted: made after reading untrusted content");
                }
                page = page.push(widget::text::caption(who));
            }
        }
        page = page.push(widget::button::suggested("Review and apply…").on_press(Message::Act(At::Read, format!("request system apply {seq}"))));
        page = page.push(widget::text::heading("What it does"));
        page.push(self.diff(&format!("proposal {seq}"))).into()
    }

    fn desktop_page(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(4).spacing(spacing.space_m);
        page = page.push(widget::text::body(
            "Your desktop's look and feel (theme, panel, dock, wallpaper, shortcuts) kept in your own configuration: a machine rebuilt from it comes back looking the same, and each save is in the history, undoable. It is yours: no password.",
        ));
        let s = match &self.desktop {
            Some(Loaded::Got(s)) => s,
            Some(Loaded::Failed(why)) => return failed(why),
            _ => return waiting(),
        };
        let automatic = s.saving == app::Saving::Automatic;
        let toggle = widget::toggler(automatic).on_toggle(|on| Message::DesktopSaving(if on { app::Saving::Automatic } else { app::Saving::Manual }));
        let state = if s.unsaved.is_empty() {
            format!("Saved: {} settings, nothing new since", s.saved)
        } else {
            format!("Not saved yet: {}", app::desktop_words(&s.unsaved))
        };
        let mut save = widget::button::suggested("Save now");
        if self.busy.is_none() {
            save = save.on_press(Message::DesktopSave);
        }
        page = page.push(
            settings::section()
                .title("Saving")
                .add(settings::item::builder("Save automatically").description("Your changes are saved by themselves, within 15 minutes. Off: only when you press Save now.").control(toggle))
                .add(settings::item::builder(state).description("request system desktop save").control(save)),
        );
        if s.to_apply > 0 {
            page = page.push(widget::text::caption(format!("{} settings from your configuration are still to be put on this desktop: at your next login, or now with Save now.", s.to_apply)));
        }
        if let Some(t) = app::desktop_taking_out(&s.to_take_out) {
            page = page.push(widget::text::caption(format!("{t} Save now does it now.")));
        }
        page.push(widget::text::caption("Left out: other programs' settings (only COSMIC's own are kept), and what is this machine's rather than yours: which display the panel and dock are on, and when the machine sleeps.")).into()
    }

    fn apps_page(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let tabs = widget::tab_bar::horizontal(&self.app_tabs).on_activate(Message::AppTab);
        let mut page = widget::column::with_capacity(3).push(tabs).spacing(spacing.space_m);
        if let Some(work) = &self.app_work {
            page = page.push(widget::text::body(work.clone()));
        }
        let tab = self.app_tabs.active_data::<AppTab>().copied().unwrap_or(AppTab::Apps);
        page.push(match tab {
            AppTab::Apps => self.apps_tab(),
            AppTab::Tools => self.tools_tab(),
        })
        .into()
    }

    fn apps_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let idle = self.app_work.is_none();
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_m);
        page = page.push(
            widget::row::with_capacity(2)
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center)
                .push(widget::search_input("Find an app to install", &self.app_search).on_input(Message::AppSearch).on_clear(Message::AppSearch(String::new())).on_submit(|_| Message::FindApp).width(Length::Fill))
                .push(widget::button::suggested("Search").on_press(Message::FindApp)),
        );
        let installed: Vec<&App> = match &self.apps {
            Loaded::Got(a) => a.iter().collect(),
            _ => vec![],
        };
        if let Some(found) = &self.app_results {
            let mut section = settings::section().title("Results");
            match found {
                Loaded::Waiting => section = section.add(waiting()),
                Loaded::Failed(why) => section = section.add(failed(why)),
                Loaded::Got(list) if list.is_empty() => section = section.add(widget::text::body("Nothing found.")),
                Loaded::Got(list) => {
                    for a in list {
                        let have = installed.iter().any(|i| i.id == a.id);
                        let action: Element<'_, Message> = if have {
                            widget::text::caption("Installed").into()
                        } else {
                            widget::button::standard("Install").on_press_maybe(idle.then(|| Message::Install(a.id.clone()))).into()
                        };
                        section = section.add(app_row(a, app::app_icon(&a.id), action));
                    }
                }
            }
            return page.push(section).into();
        }
        match &self.apps {
            Loaded::Waiting => page = page.push(waiting()),
            Loaded::Failed(why) => page = page.push(failed(why)),
            Loaded::Got(list) if list.is_empty() => page = page.push(widget::text::body("No apps yet. Search above to find one: it installs just for you, with no password.")),
            Loaded::Got(list) => {
                let mut section = settings::section().title("Your apps");
                for a in list {
                    let action = widget::row::with_capacity(2)
                        .spacing(spacing.space_xs)
                        .push(widget::button::standard("Open").on_press(Message::OpenApp(a.id.clone())))
                        .push(widget::button::destructive("Remove").on_press_maybe(idle.then(|| Message::Uninstall(a.id.clone()))));
                    section = section.add(app_row(a, app::app_icon(&a.id), action.into()));
                }
                page = page.push(section);
                page = page.push(widget::text::caption("Your apps are remembered in your configuration, so they come back on a rebuilt machine. What they saved stays in your home folder when you remove them."));
            }
        }
        page.into()
    }

    fn tools_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let idle = self.app_work.is_none();
        let me = app::who().unwrap_or_default();
        let path = vec!["users".to_string(), "users".to_string(), me, "packages".to_string()];
        let mine: Vec<String> = self.active.get(&path).cloned().unwrap_or_default();
        let mut page = widget::column::with_capacity(3).spacing(spacing.space_m);
        page = page.push(widget::text::body("Programs you run in a terminal, like htop or git. They are built into your system, so adding one takes longer than an app."));
        page = page.push(
            widget::row::with_capacity(2)
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center)
                .push(widget::search_input("Find a tool", &self.tool_search).on_input(Message::ToolSearch).on_clear(Message::ToolSearch(String::new())).on_submit(|_| Message::FindTool).width(Length::Fill))
                .push(widget::button::suggested("Search").on_press(Message::FindTool)),
        );
        if let Some(found) = &self.tool_results {
            let mut section = settings::section().title("Results");
            match found {
                Loaded::Waiting => section = section.add(waiting()),
                Loaded::Failed(why) => section = section.add(failed(why)),
                Loaded::Got(list) if list.is_empty() => section = section.add(widget::text::body("Nothing found.")),
                Loaded::Got(list) => {
                    for t in list.iter().take(40) {
                        let action: Element<'_, Message> = if mine.contains(&t.name) {
                            widget::text::caption("Added").into()
                        } else {
                            widget::button::standard("Add").on_press_maybe(idle.then(|| Message::AddTool(t.name.clone()))).into()
                        };
                        section = section.add(settings::item::builder(t.name.clone()).description(format!("{} · {}", t.version, t.summary)).control(action));
                    }
                }
            }
            return page.push(section).into();
        }
        if mine.is_empty() {
            return page.push(widget::text::body("No tools of your own yet.")).into();
        }
        let mut section = settings::section().title("Your tools");
        for name in mine {
            section = section.add(settings::item::builder(name.clone()).control(widget::button::destructive("Remove").on_press_maybe(idle.then(|| Message::RemoveTool(name.clone())))));
        }
        page.push(section).into()
    }

    fn backup_tab(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let mut page = widget::column::with_capacity(5).spacing(spacing.space_m);
        let status = match self.outputs.get(REMOTE) {
            Some(Loaded::Got(t)) => t,
            Some(Loaded::Failed(why)) => return failed(why),
            _ => return waiting(),
        };
        let source = vec!["system".to_string(), "software".to_string(), "torq".to_string(), "source".to_string()];
        if self.active.get(&source).is_some_and(|s| s.iter().any(|v| v.starts_with("path:"))) {
            page = page.push(widget::text::caption("This machine builds TorqOS from a folder on its own disk. Your configuration is backed up, but restoring it elsewhere needs a machine that takes TorqOS from its releases."));
        }
        if status.starts_with("No remote") {
            page = page.push(widget::text::body("Keep a copy of this machine's configuration and its history in a private repository. If the machine is ever lost, a new install can rebuild it from there. Passwords and secrets are never in it."));
            // GitHub, the easy way.
            let mut github = settings::section().title("GitHub");
            match (&self.gh_code, &self.github) {
                (Some(code), _) => {
                    github = github.add(settings::item_row(vec![widget::column::with_capacity(2).spacing(spacing.space_xxs).push(widget::text::title3(code.user_code.clone())).push(widget::text::caption("Enter this code at GitHub. This page moves on when you approve it.")).width(Length::Fill).into(), widget::button::standard("Copy").on_press(Message::CopyKey(code.user_code.clone())).into(), widget::button::suggested("Open GitHub").on_press(Message::OpenUrl(code.verification_uri.clone())).into(), widget::button::standard("Cancel").on_press(Message::GhCancel).into()]).spacing(spacing.space_xs).align_y(Alignment::Center));
                }
                (None, Some((_, login))) => {
                    github = github.add(settings::item::builder(format!("Signed in as {login}")).description("A private repository, for this machine alone").control(widget::row::with_capacity(2).spacing(spacing.space_xs).align_y(Alignment::Center).width(Length::FillPortion(2)).push(widget::text_input("Repository name", &self.repo_name).on_input(Message::RepoName).width(Length::Fill)).push(widget::button::suggested("Create and connect").on_press(Message::GhCreate))));
                }
                (None, None) => {
                    github = github.add(settings::item::builder("Sign in with GitHub").description("In your browser: your GitHub password never touches TorqOS, and TorqOS keeps only a key for the one repository").control(widget::button::suggested("Sign in").on_press(Message::GhStart)));
                }
            }
            page = page.push(github);
            let host = app::host();
            let new = format!("https://github.com/new?name=torqos-{host}&visibility=private&description=TorqOS%20configuration%20of%20{host}");
            page = page.push(
                settings::section()
                    .title("Or use another Git server")
                    .add(settings::item::builder("1. Create a private repository").description("On GitHub, or any Git server you reach over SSH").control(widget::button::standard("Open GitHub").on_press(Message::OpenUrl(new))))
                    .add(
                        settings::item::builder("2. Paste its SSH address").description("Like git@github.com:you/torqos-laptop.git").control(
                            widget::row::with_capacity(2)
                                .spacing(spacing.space_xs)
                                .align_y(Alignment::Center)
                                .width(Length::FillPortion(2))
                                .push(widget::text_input("git@github.com:you/repository.git", &self.remote_url).on_input(Message::RemoteUrl).on_submit(|_| Message::Connect).width(Length::Fill))
                                .push(widget::button::suggested("Connect").on_press(Message::Connect)),
                        ),
                    ),
            );
            return page.push(widget::text::caption("Connecting is a change like any other: you review it and commit it, then give the repository this machine's key.")).into();
        }
        // Only the repository this machine is really connected to gets its key.
        let connected = status.lines().next().and_then(|l| l.strip_prefix("Remote: ")).and_then(|u| app::github_repo(u.trim()));
        if self.github.is_some() && self.gh_repo.is_some() && self.gh_repo == connected {
            page = page.push(settings::section().title("One step left").add(settings::item::builder("Give the repository this machine's key").description("TorqOS Settings adds it for you: only this machine, and only this repository").control(widget::button::suggested("Finish").on_press(Message::GhFinish))));
        }
        page = page.push(settings::section().title("This machine's repository").add(settings::item_row(vec![widget::text::monotext(status.trim_end().to_string()).width(Length::Fill).into()])));
        let mut key = settings::section().title("This machine's key");
        key = key.add(settings::item::builder("Lets this machine, and only it, write to this one repository").control(widget::button::standard("Show key").on_press(Message::ShowKey)));
        match &self.machine_key {
            Some(Ok(text)) => {
                let public = text.lines().next().unwrap_or_default().to_string();
                let url = status.lines().next().and_then(|l| l.strip_prefix("Remote: ")).unwrap_or_default();
                let mut buttons = widget::row::with_capacity(2).spacing(spacing.space_xs).push(widget::button::standard("Copy").on_press(Message::CopyKey(public.clone())));
                if let Some((owner, repo)) = app::github_repo(url.trim()) {
                    buttons = buttons.push(widget::button::standard("Add it on GitHub").on_press(Message::OpenUrl(format!("https://github.com/{owner}/{repo}/settings/keys/new"))));
                }
                key = key.add(settings::item_row(vec![widget::text::monotext(public).width(Length::Fill).into()]));
                key = key.add(settings::item::builder("Add it as a deploy key, with write access").control(buttons));
            }
            Some(Err(e)) => key = key.add(failed(e)),
            None => {}
        }
        page.push(key).push(actions(app::BACKUP)).into()
    }

    fn system(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let tabs = widget::tab_bar::horizontal(&self.sys_tabs).on_activate(Message::SysTab);
        let mut page = widget::column::with_capacity(4).push(tabs).spacing(spacing.space_l);
        if self.sys_tabs.active_data::<BackupTab>().is_some() {
            return page.push(self.backup_tab()).into();
        }
        let Some(v) = self.view_shown() else { return page.into() };
        page = page.push(actions(v.actions));
        for (title, command) in v.sections {
            page = page.push(settings::section().title(*title).add(settings::item_row(vec![self.output(command)])));
        }
        page.into()
    }

    fn commands(&self) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let prompt = match (self.typed_cf, self.edit.is_empty()) {
            (false, _) => "torqcli>".to_string(),
            (true, true) => "torqcli#".to_string(),
            (true, false) => format!("[edit {}] torqcli#", self.edit.join(" ")),
        };
        let mut page = widget::column::with_capacity(4).spacing(spacing.space_m);
        page = page.push(widget::text::body("Anything torqcli can do. Choose what comes next, or type it."));
        page = page.push(
            widget::row::with_capacity(3)
                .push(widget::text::monotext(prompt))
                .push(widget::text_input("show system alarms", &self.line).on_input(Message::Line).on_submit(|_| Message::RunLine).width(Length::Fill))
                .push(widget::button::suggested("Run").on_press(Message::RunLine))
                .spacing(spacing.space_xs)
                .align_y(Alignment::Center),
        );
        if !self.offers.is_empty() {
            let mut list = settings::section().title("What may come next");
            for c in self.offers.iter().take(LISTED) {
                let row = settings::item_row(vec![
                    widget::text::monotext(c.word.clone()).width(Length::FillPortion(1)).into(),
                    widget::text::caption(c.help.clone()).width(Length::FillPortion(3)).into(),
                ])
                .spacing(spacing.space_s);
                list = list.add(if c.is_placeholder() { Element::from(row) } else { widget::mouse_area(row).on_press(Message::Offer(c.word.clone())).into() });
            }
            page = page.push(list);
        }
        for (line, out) in &self.log {
            page = page.push(
                widget::column::with_capacity(2)
                    .push(widget::text::heading(line.clone()))
                    .push(widget::text::monotext(out.trim_end().to_string()).width(Length::Fill))
                    .spacing(spacing.space_xxs),
            );
        }
        page.into()
    }
}

/// A diff, coloured and a card per file (a unified diff) or per place (`show | compare`).
fn diff_view(text: &str) -> Element<'static, Message> {
    let spacing = cosmic::theme::spacing();
    let theme = cosmic::theme::active();
    let cosmic = theme.cosmic();
    let (added, gone, hunk): (cosmic::iced::Color, cosmic::iced::Color, cosmic::iced::Color) = (cosmic.success_color().into(), cosmic.destructive_color().into(), cosmic.accent_color().into());
    let mut cards: Vec<(String, Vec<(String, Option<cosmic::iced::Color>)>)> = vec![];
    let all: Vec<&str> = text.lines().collect();
    for line in all.iter().take(DIFF_LINES) {
        let title = line
            .strip_prefix("[edit")
            .map(|t| t.trim().trim_end_matches(']').trim().to_string())
            .or_else(|| line.strip_prefix("+++ b/").map(str::to_string))
            .or_else(|| line.strip_prefix("--- a/").map(str::to_string));
        if line.starts_with("diff --git") || line.starts_with("index ") {
            continue;
        }
        if line.starts_with("+++ ") || line.starts_with("--- ") {
            // One card a file: its `---` opens it, its `+++` names it if it is new.
            let name = title.unwrap_or_default();
            if line.starts_with("--- ") || cards.is_empty() {
                cards.push((name, vec![]));
            } else if let (Some(last), false) = (cards.last_mut(), name.is_empty()) {
                last.0 = name;
            }
            continue;
        }
        if line.starts_with("[edit") {
            cards.push((title.unwrap_or_default(), vec![]));
            continue;
        }
        if cards.is_empty() {
            cards.push((String::new(), vec![]));
        }
        let colour = if line.starts_with("@@") {
            Some(hunk)
        } else if line.starts_with('+') {
            Some(added)
        } else if line.starts_with('-') {
            Some(gone)
        } else {
            None
        };
        if let Some(card) = cards.last_mut() {
            card.1.push((line.to_string(), colour));
        }
    }
    let mut column = widget::column::with_capacity(cards.len() + 1).spacing(spacing.space_m);
    for (title, lines) in cards {
        let mut body = widget::column::with_capacity(lines.len());
        for (line, colour) in lines {
            let t = widget::text::monotext(line).width(Length::Fill);
            body = body.push(match colour {
                Some(c) => t.class(cosmic::theme::Text::Color(c)),
                None => t,
            });
        }
        let mut section = settings::section();
        if !title.is_empty() {
            section = section.title(title);
        }
        column = column.push(section.add(settings::item_row(vec![body.into()])));
    }
    if all.len() > DIFF_LINES {
        column = column.push(widget::text::caption(format!("The first {DIFF_LINES} of {} lines.", all.len())));
    }
    column.into()
}

/// A page's buttons: each a torqcli command, run or finished on the Command Line.
fn actions(list: &'static [Action]) -> Element<'static, Message> {
    let mut section = settings::section().title("Actions");
    for a in list {
        let (button, command) = match a.how {
            How::Run => (widget::button::standard("Run").on_press(Message::Act(At::Read, a.command.to_string())), a.command.to_string()),
            how => (widget::button::standard("Open").on_press(Message::Fill(how, a.command.to_string())), format!("{}…", a.command.trim_end())),
        };
        section = section.add(settings::item::builder(a.label).description(command).control(button));
    }
    section.into()
}

fn attention<'a>(text: String, button: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    settings::item_row(vec![widget::text::body(text).width(Length::Fill).into(), button.into()]).spacing(cosmic::theme::spacing().space_s).into()
}

/// One change: its mark, what happened, who and when, its number; choosing it shows what it did.
fn change(r: &Row) -> Element<'_, Message> {
    let (name, colour) = match r.symbol {
        "✓" => ("emblem-ok-symbolic", Colour::Good),
        "✗" => ("dialog-error-symbolic", Colour::Bad),
        "⚠" => ("dialog-warning-symbolic", Colour::Warn),
        "↶" => ("edit-undo-symbolic", Colour::Plain),
        "↺" => ("view-refresh-symbolic", Colour::Plain),
        _ => ("media-record-symbolic", Colour::Plain),
    };
    let mut detail = r.time.clone();
    if !r.who.is_empty() {
        detail.push_str(&format!(" · {}", r.who));
    }
    if r.tainted {
        detail.push_str(" · ⚠ tainted: made after reading untrusted content");
    }
    let row = settings::item_row(vec![
        colour.icon(name, 16),
        widget::column::with_capacity(2).push(widget::text::body(r.text.clone())).push(widget::text::caption(detail)).width(Length::Fill).into(),
        widget::text::body(format!("#{}", r.seq)).into(),
        icon::from_name("go-next-symbolic").size(16).icon().into(),
    ])
    .spacing(cosmic::theme::spacing().space_s)
    .align_y(Alignment::Center);
    widget::mouse_area(row).on_press(Message::OpenChange(r.seq)).into()
}

/// How a mark is coloured: the theme's own success, warning and destructive colours.
#[derive(Clone, Copy)]
enum Colour {
    Good,
    Warn,
    Bad,
    Plain,
}

impl Colour {
    fn icon(self, name: &'static str, size: u16) -> Element<'static, Message> {
        let i = icon::from_name(name).size(size).icon();
        let style = match self {
            Colour::Good => cosmic::theme::Svg::custom(|t| cosmic::iced::widget::svg::Style { color: Some(t.cosmic().success_color().into()) }),
            Colour::Warn => cosmic::theme::Svg::custom(|t| cosmic::iced::widget::svg::Style { color: Some(t.cosmic().warning_color().into()) }),
            Colour::Bad => cosmic::theme::Svg::custom(|t| cosmic::iced::widget::svg::Style { color: Some(t.cosmic().destructive_color().into()) }),
            Colour::Plain => return i.into(),
        };
        i.class(style).into()
    }

    fn colour(self) -> cosmic::iced::Color {
        let theme = cosmic::theme::active();
        let c = theme.cosmic();
        match self {
            Colour::Good => c.success_color().into(),
            Colour::Warn => c.warning_color().into(),
            Colour::Bad => c.destructive_color().into(),
            Colour::Plain => c.on_bg_color().into(),
        }
    }
}

/// A short coloured word beside a setting.
fn tagged(word: &'static str, colour: Colour) -> Element<'static, Message> {
    widget::text::caption(word).class(cosmic::theme::Text::Color(colour.colour())).into()
}

fn note(text: &str, is_failure: bool) -> Element<'static, Message> {
    let spacing = cosmic::theme::spacing();
    let colour = if is_failure { Colour::Bad } else { Colour::Good };
    let name = if is_failure { "dialog-error-symbolic" } else { "emblem-ok-symbolic" };
    widget::row::with_capacity(2)
        .push(colour.icon(name, 16))
        .push(widget::text::body(text.to_string()).width(Length::Fill))
        .spacing(spacing.space_xs)
        .into()
}

fn waiting() -> Element<'static, Message> {
    widget::text::body("Asking…").into()
}

fn failed(why: &str) -> Element<'_, Message> {
    widget::column::with_capacity(2)
        .push(widget::text::title4("That did not work"))
        .push(widget::text::body(why))
        .spacing(cosmic::theme::spacing().space_xs)
        .into()
}

/// A call that blocks (a socket, a session) on a thread of its own, so the window keeps drawing.
async fn off<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (tx, rx) = cosmic::iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.map_err(|_| "the call stopped before it answered".to_string())
}

/// `off`, for a call of the library's that can fail: its error as torqcli would say it.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> Result<T, String> {
    off(move || f().map_err(|e| app::said(&e))).await.and_then(|r| r)
}

/// An application on a list: its icon, name, what it is, and what can be done about it.
fn app_row<'a>(a: &'a App, catalogue_icon: Option<std::path::PathBuf>, action: Element<'a, Message>) -> Element<'a, Message> {
    let picture: Element<'a, Message> = match catalogue_icon {
        Some(p) => icon::icon(icon::from_path(p)).size(40).into(),
        None => icon::from_name(a.id.clone()).size(40).icon().into(),
    };
    settings::item_row(vec![
        picture,
        widget::column::with_capacity(2).push(widget::text::body(a.name.clone())).push(widget::text::caption(a.summary.clone())).width(Length::Fill).into(),
        action,
    ])
    .spacing(cosmic::theme::spacing().space_s)
    .align_y(Alignment::Center)
    .into()
}
