//! Terminal panels ↔ daemon sessions.
//!
//! A terminal panel's session id is its panel id, so opening a panel always tries to *reattach*
//! first; only when the daemon has no such session is a new shell started. That is the whole
//! Principle III lifecycle: busy terminals survive the UI and come back with their scrollback, idle
//! ones were closed with the app and start fresh.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use throng_core::ids::{PanelId, ProjectId, TerminalId};
use throng_core::terminal::{
    ExitStatus, ShellInfo, TerminalPanelConfig, is_private_env_var, should_surface_exit,
};
use throng_daemon::{Client, ClientEvent};
use throng_protocol::{Reply, Request, SpawnSpec};

use super::{Status, TermEvent, TerminalView};

/// What a panel's terminal is started with when there is nothing to reattach to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpawnPlan {
    pub project: ProjectId,
    pub label: String,
    pub root: PathBuf,
    pub config: TerminalPanelConfig,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    Attach,
    Spawn,
}

/// Something the rest of the app must act on.
#[derive(Debug, PartialEq, Eq)]
pub enum HubEvent {
    /// Ended cleanly or by the user: the panel goes back to the type picker (Principle III).
    Ended {
        panel: PanelId,
        config: TerminalPanelConfig,
    },
    /// Ended with a failure: the panel keeps its last screen and shows the exit.
    Failed {
        panel: PanelId,
        status: ExitStatus,
    },
    TitleChanged(PanelId),
    Clipboard(String),
    /// The shell's working directory changed (for directory memory).
    Directory {
        panel: PanelId,
        dir: PathBuf,
    },
    /// What a remembering terminal runs now (`None`: nothing), for command memory.
    Observed {
        panel: PanelId,
        running: Option<String>,
    },
    /// A remembering terminal cold-started after an end nobody saw: what it last ran became its
    /// startup command.
    Captured(PanelId),
}

/// How often the shells' working directories are read.
const DIRECTORY_POLL: Duration = Duration::from_millis(1500);

/// Every terminal view.
#[derive(Default)]
pub struct TerminalHub {
    pub views: HashMap<PanelId, TerminalView>,
    plans: HashMap<PanelId, SpawnPlan>,
    pending: HashMap<u64, (PanelId, Pending)>,
    pub shells: Vec<ShellInfo>,
    pub default_shell: Option<String>,
    pub scrollback: usize,
    /// The `terminal.defaultRememberDirectory` preference, for panels with no value of their own.
    pub remember_directory: bool,
    pub rules: Option<throng_core::paths::PathRules>,
    /// A `List` in flight for the working directories, and when the last one went.
    listing: Option<u64>,
    listed_at: Option<Instant>,
    /// The app is quitting: no terminal is attached or started any more. Without this, the frames
    /// a closing window still draws would re-attach the idle shells quitting just closed, find
    /// them gone, and start new ones that outlive the app (Principle III).
    pub closing: bool,
    /// Panels whose terminal starts only when reloaded: every terminal a layout held when it loaded
    /// with `terminal.reloadMode` manual. One still running reattaches all the same.
    dormant: HashSet<PanelId>,
    /// Events raised outside a daemon event, for the app's next look.
    queued: Vec<HubEvent>,
}

impl TerminalHub {
    #[must_use]
    pub fn new(shells: Vec<ShellInfo>, default_shell: Option<String>, scrollback: usize) -> Self {
        Self { shells, default_shell, scrollback, ..Self::default() }
    }

    /// Make sure `panel` has a view, remembering how to start its shell.
    pub fn prepare(&mut self, panel: PanelId, plan: SpawnPlan) {
        let scrollback = self.scrollback;
        let label = plan.label.clone();
        self.plans.insert(panel, plan);
        let view = self.views.entry(panel).or_insert_with(|| TerminalView::new(panel, scrollback));
        view.label = label;
    }

    /// Once the view has been laid out (so its size is real), make sure a session request is in
    /// flight. Attaching before layout would start the shell at a placeholder size, and no resize
    /// would follow because the view's size never changed after it connected.
    pub fn connect(&mut self, panel: PanelId, client: Option<&Client>) {
        if self.closing {
            return;
        }
        if let Some(view) = self.views.get_mut(&panel)
            && view.status == Status::Connecting
            && view.pending.is_none()
            && let Some(client) = client
        {
            let (cols, rows) = view.size;
            let id = client.send(Request::Attach { terminal: TerminalId::from(panel), cols, rows });
            view.pending = Some(id);
            self.pending.insert(id, (panel, Pending::Attach));
        }
    }

    /// The connection came (back): every view re-attaches from a fresh snapshot.
    pub fn reconnected(&mut self) {
        self.pending.clear();
        for view in self.views.values_mut() {
            view.pending = None;
            if matches!(view.status, Status::Running | Status::Connecting | Status::Failed(_)) {
                view.status = Status::Connecting;
            }
        }
    }

    /// The connection dropped: requests in flight will never be answered.
    pub fn disconnected(&mut self) {
        self.pending.clear();
        for view in self.views.values_mut() {
            view.pending = None;
        }
    }

    /// Stop showing a panel's terminal without ending it (the panel was removed from view).
    pub fn forget_view(&mut self, panel: PanelId, client: Option<&Client>) {
        if self.views.remove(&panel).is_some()
            && let Some(client) = client
        {
            client.post(Request::Detach { terminal: panel.into() });
        }
        self.plans.remove(&panel);
    }

    /// These panels' terminals start only when reloaded (they reattach if still running).
    pub fn keep_dormant(&mut self, panels: impl IntoIterator<Item = PanelId>) {
        self.dormant.extend(panels);
    }

    /// Whether a panel's terminal waits to be reloaded rather than starting.
    #[must_use]
    pub fn is_dormant(&self, panel: PanelId) -> bool {
        self.views.get(&panel).is_some_and(|v| v.status == Status::Dormant)
    }

    /// Events raised since the app last looked, outside a daemon event.
    pub fn take_queued(&mut self) -> Vec<HubEvent> {
        std::mem::take(&mut self.queued)
    }

    /// What `panel`'s shell runs right now, read at the moment of an end the app is about to
    /// cause. `None` when the shell is not known yet: the last observation stands.
    #[must_use]
    pub fn observe_now(&self, panel: PanelId) -> Option<Option<String>> {
        let pid = self.views.get(&panel)?.shell_pid?;
        Some(throng_platform::process::ProcessTable::snapshot().running_command(pid))
    }

    /// End a panel's terminal at the user's request.
    pub fn kill(&mut self, panel: PanelId, client: Option<&Client>) {
        if let Some(client) = client {
            client.post(Request::Kill { terminal: panel.into() });
        }
        self.views.remove(&panel);
        self.plans.remove(&panel);
    }

    /// Start a fresh shell in a panel whose terminal failed, exited or is dormant.
    pub fn restart(&mut self, panel: PanelId, client: Option<&Client>) {
        self.dormant.remove(&panel);
        if let Some(client) = client {
            client.post(Request::Forget { terminal: panel.into() });
        }
        if let Some(view) = self.views.get_mut(&panel) {
            view.status = Status::Connecting;
            view.pending = None;
        }
        if let Some(client) = client {
            self.spawn(panel, client);
        }
    }

    /// A cold start of `panel`'s shell, from its plan.
    fn spawn(&mut self, panel: PanelId, client: &Client) {
        if self.closing {
            return;
        }
        let Some(plan) = self.plans.get_mut(&panel) else { return };
        // A remembering terminal that ended while nobody watched (a crash, a restart) captures
        // what it last ran now; the app records the same capture in the layout.
        if plan.config.capture() {
            self.queued.push(HubEvent::Captured(panel));
        }
        let plan = plan.clone();
        let Some(view) = self.views.get_mut(&panel) else { return };
        let shell = throng_platform::shells::choose(
            &self.shells,
            plan.config.shell.as_deref().or(self.default_shell.as_deref()),
        );
        let Some(shell) = shell else {
            view.status = Status::Failed("No shell was found on this system.".into());
            return;
        };
        let mut args = shell.args.clone();
        args.extend(plan.config.args.iter().cloned());
        let rules = self.rules.unwrap_or_else(throng_platform::path_rules);
        let cwd = throng_core::terminal::start_directory(
            &plan.config,
            &plan.root,
            self.remember_directory,
            &rules,
            std::path::Path::is_dir,
        );
        let env = std::env::vars().filter(|(k, _)| !is_private_env_var(k)).collect();
        let (cols, rows) = view.size;
        let spec = SpawnSpec {
            terminal: panel.into(),
            project: plan.project,
            label: plan.label.clone(),
            program: shell.program.clone(),
            args,
            cwd,
            env,
            cols,
            rows,
            startup_command: plan.config.startup_command.clone(),
        };
        let id = client.send(Request::Spawn(spec));
        view.pending = Some(id);
        view.status = Status::Connecting;
        self.pending.insert(id, (panel, Pending::Spawn));
    }

    /// Read the running shells' working directories now and then (one `List` at a time). Returns
    /// when to look again, while any terminal runs.
    pub fn poll_directories(&mut self, client: Option<&Client>) -> Option<Duration> {
        let running = self.views.values().any(|v| v.status == Status::Running);
        let client = client.filter(|_| running)?;
        let due = self.listed_at.is_none_or(|at| at.elapsed() >= DIRECTORY_POLL);
        if self.listing.is_none() && due {
            self.listing = Some(client.send(Request::List));
            self.listed_at = Some(Instant::now());
        }
        Some(DIRECTORY_POLL)
    }

    /// Apply one daemon event.
    pub fn on_daemon_event(&mut self, event: ClientEvent, client: Option<&Client>) -> Vec<HubEvent> {
        let mut out = Vec::new();
        match event {
            ClientEvent::Output { terminal, offset, data } => {
                let panel = PanelId::from(terminal);
                if let Some(view) = self.views.get_mut(&panel) {
                    view.feed(offset, &data);
                    Self::emulator_events(view, client, &mut out);
                    if let Some(dir) = view.settle_directory() {
                        out.push(HubEvent::Directory { panel, dir });
                    }
                }
            }
            ClientEvent::Exited { terminal, status } => {
                self.exited(terminal.into(), status, client, &mut out)
            }
            ClientEvent::Resync { terminal } => {
                let panel = PanelId::from(terminal);
                if let (Some(view), Some(client)) = (self.views.get_mut(&panel), client) {
                    let (cols, rows) = view.size;
                    let id = client.send(Request::Attach { terminal, cols, rows });
                    view.pending = Some(id);
                    self.pending.insert(id, (panel, Pending::Attach));
                }
            }
            ClientEvent::Reply { id, result } if self.listing == Some(id) => {
                self.listing = None;
                if let Ok(Reply::Terminals(list)) = result {
                    // One process snapshot serves every terminal, and only when one remembers.
                    let remembering = self.plans.values().any(|p| p.config.remember_command);
                    let table = remembering.then(throng_platform::process::ProcessTable::snapshot);
                    for info in list {
                        let panel = PanelId::from(info.terminal);
                        let Some(view) = self.views.get_mut(&panel) else { continue };
                        view.shell_pid = info.pid.filter(|_| info.exited.is_none());
                        view.process_cwd =
                            view.shell_pid.and_then(throng_platform::process::working_directory);
                        if let Some(dir) = view.settle_directory() {
                            out.push(HubEvent::Directory { panel, dir });
                        }
                        if let (Some(table), Some(pid), Some(plan)) =
                            (&table, view.shell_pid, self.plans.get(&panel))
                            && plan.config.remember_command
                        {
                            let running = table.running_command(pid);
                            if plan.config.running_command != running {
                                out.push(HubEvent::Observed { panel, running });
                            }
                        }
                    }
                }
            }
            ClientEvent::Reply { id, result } => {
                let Some((panel, kind)) = self.pending.remove(&id) else { return out };
                let Some(view) = self.views.get_mut(&panel) else { return out };
                view.pending = None;
                match (kind, result) {
                    (_, Ok(Reply::Snapshot(snapshot))) => {
                        view.apply_snapshot(&snapshot);
                        if let Some(status) = snapshot.exited {
                            self.exited(panel, status, client, &mut out);
                        }
                    }
                    (Pending::Attach, Err(message)) if message == throng_protocol::NO_SUCH_TERMINAL => {
                        if self.dormant.contains(&panel) {
                            view.status = Status::Dormant;
                        } else if let Some(client) = client {
                            self.spawn(panel, client);
                        }
                    }
                    (_, Err(message)) => view.status = Status::Failed(message),
                    (_, Ok(_)) => {}
                }
            }
            ClientEvent::Disconnected { .. } => {}
        }
        out.append(&mut self.queued);
        out
    }

    fn exited(
        &mut self,
        panel: PanelId,
        status: ExitStatus,
        client: Option<&Client>,
        out: &mut Vec<HubEvent>,
    ) {
        if !self.views.contains_key(&panel) && !self.plans.contains_key(&panel) {
            // The UI already ended this terminal (Kill, Close): its panel has moved on, and reporting
            // the end again would overwrite what the panel's picker remembered.
            return;
        }
        // The shell ended on its own: whatever it ran is not a command to remember.
        if let Some(plan) = self.plans.get_mut(&panel)
            && plan.config.running_command.take().is_some()
        {
            out.push(HubEvent::Observed { panel, running: None });
        }
        if should_surface_exit(status) {
            if let Some(view) = self.views.get_mut(&panel) {
                view.status = Status::Exited(status);
            }
            out.push(HubEvent::Failed { panel, status });
        } else {
            if let Some(client) = client {
                client.post(Request::Forget { terminal: panel.into() });
            }
            self.views.remove(&panel);
            let config = self.plans.remove(&panel).map(|p| p.config).unwrap_or_default();
            out.push(HubEvent::Ended { panel, config });
        }
    }

    /// Forward what the emulator produced: query answers go to the shell, titles and clipboard
    /// writes go to the app.
    pub fn emulator_events(view: &mut TerminalView, client: Option<&Client>, out: &mut Vec<HubEvent>) {
        for event in view.drain_events() {
            match event {
                TermEvent::Reply(text) => {
                    if let Some(client) = client {
                        client.write(view.panel.into(), text.as_bytes());
                    }
                }
                TermEvent::Title(title) => {
                    view.title = Some(title);
                    out.push(HubEvent::TitleChanged(view.panel));
                }
                TermEvent::ResetTitle => {
                    view.title = None;
                    out.push(HubEvent::TitleChanged(view.panel));
                }
                TermEvent::Clipboard(text) => out.push(HubEvent::Clipboard(text)),
                TermEvent::Bell => view.bell = true,
                // Programs ask what colours they are drawn in (to pick a dark or light theme), and
                // how big the text area is. Replayed queries never get here: a snapshot's
                // events are dropped.
                TermEvent::ColorQuery(index, answer) => {
                    let reply = answer.word(view.queried_colour(index));
                    if let Some(client) = client {
                        client.write(view.panel.into(), reply.as_bytes());
                    }
                }
                TermEvent::SizeQuery(answer) => {
                    let size = alacritty_terminal::event::WindowSize {
                        num_cols: view.size.0,
                        num_lines: view.size.1,
                        cell_width: view.cell_px.0,
                        cell_height: view.cell_px.1,
                    };
                    if let Some(client) = client {
                        client.write(view.panel.into(), answer.word(size).as_bytes());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exit_after_the_ui_killed_the_terminal_reports_nothing() {
        let mut hub = TerminalHub::new(Vec::new(), None, 1000);
        let panel = PanelId::new();
        let plan = SpawnPlan {
            project: ProjectId::new(),
            label: "p".into(),
            root: PathBuf::from("/"),
            config: TerminalPanelConfig { startup_command: Some("make".into()), ..Default::default() },
        };
        hub.prepare(panel, plan);
        hub.kill(panel, None);
        let status = ExitStatus { code: None, user_killed: true };
        let events = hub.on_daemon_event(ClientEvent::Exited { terminal: panel.into(), status }, None);
        assert!(events.is_empty(), "{events:?}");
    }

    #[test]
    fn a_clean_exit_hands_back_the_config_and_a_failure_keeps_the_view() {
        let mut hub = TerminalHub::new(Vec::new(), None, 1000);
        let (clean, failed) = (PanelId::new(), PanelId::new());
        let config = TerminalPanelConfig { startup_command: Some("make".into()), ..Default::default() };
        for panel in [clean, failed] {
            let plan = SpawnPlan {
                project: ProjectId::new(),
                label: "p".into(),
                root: PathBuf::from("/"),
                config: config.clone(),
            };
            hub.prepare(panel, plan);
        }
        let ok = ExitStatus { code: Some(0), user_killed: false };
        let events = hub.on_daemon_event(ClientEvent::Exited { terminal: clean.into(), status: ok }, None);
        assert_eq!(events, vec![HubEvent::Ended { panel: clean, config }]);
        assert!(!hub.views.contains_key(&clean));
        let bad = ExitStatus { code: Some(2), user_killed: false };
        let events = hub.on_daemon_event(ClientEvent::Exited { terminal: failed.into(), status: bad }, None);
        assert_eq!(events, vec![HubEvent::Failed { panel: failed, status: bad }]);
        assert_eq!(hub.views[&failed].status, Status::Exited(bad));
    }
}
