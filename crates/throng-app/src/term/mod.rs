//! Terminal panels: an `alacritty_terminal` emulator per panel, fed by the daemon.

pub mod colors;
pub mod hub;
pub mod keys;
pub mod links;
pub mod osc7;
pub mod search;
pub mod view;

use std::path::PathBuf;

use alacritty_terminal::Term;
use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, TermMode};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use crossbeam_channel::{Receiver, Sender};
use throng_core::ids::PanelId;
use throng_core::terminal::ExitStatus;
use throng_protocol::{Snapshot, unseen};

/// How the emulator words an answer to a query, once throng supplies the value.
#[derive(Clone)]
pub struct Answer<T>(std::sync::Arc<dyn Fn(T) -> String + Sync + Send + 'static>);

impl<T> std::fmt::Debug for Answer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Answer")
    }
}

impl<T> Answer<T> {
    #[must_use]
    pub fn word(&self, value: T) -> String {
        (self.0)(value)
    }
}

/// Something the emulator wants done.
#[derive(Debug)]
pub enum TermEvent {
    /// Bytes to send back to the program (answers to queries).
    Reply(String),
    Title(String),
    ResetTitle,
    Clipboard(String),
    Bell,
    /// A program asked for a colour (OSC 10/11/12, OSC 4): palette index, or 256 foreground,
    /// 257 background, 258 cursor.
    ColorQuery(usize, Answer<alacritty_terminal::vte::ansi::Rgb>),
    /// A program asked for the text area's size in pixels (CSI 14 t).
    SizeQuery(Answer<alacritty_terminal::event::WindowSize>),
}

/// Forwards emulator events to the view.
pub struct Listener(Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let mapped = match event {
            Event::PtyWrite(text) => TermEvent::Reply(text),
            Event::Title(title) => TermEvent::Title(title),
            Event::ResetTitle => TermEvent::ResetTitle,
            Event::ClipboardStore(_, text) => TermEvent::Clipboard(text),
            Event::Bell => TermEvent::Bell,
            Event::ColorRequest(index, answer) => TermEvent::ColorQuery(index, Answer(answer)),
            Event::TextAreaSizeRequest(answer) => TermEvent::SizeQuery(Answer(answer)),
            _ => return,
        };
        let _ = self.0.send(mapped);
    }
}

/// Where a terminal panel's session stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    /// Waiting for the daemon to attach or spawn.
    Connecting,
    Running,
    /// Ended on its own with a failure: the last screen stays readable with a banner (Principle III).
    Exited(ExitStatus),
    /// Could not start; the panel keeps its type so the user can retry.
    Failed(String),
    /// Not started, by choice: terminals start only when reloaded (`terminal.reloadMode`
    /// manual). A state, not a failure; it holds no shell.
    Dormant,
}

/// What the screen's links depend on: the output seen, the scroll position and the size.
type LinkCacheKey = (u64, usize, (u16, u16));

/// One terminal panel's emulator and session bookkeeping.
pub struct TerminalView {
    pub panel: PanelId,
    term: Term<Listener>,
    parser: Processor<StdSyncHandler>,
    events: Receiver<TermEvent>,
    events_tx: Sender<TermEvent>,
    scrollback: usize,
    /// Output below this offset has been shown.
    seen_until: u64,
    pub status: Status,
    /// Columns × rows the emulator currently has.
    pub size: (u16, u16),
    pub title: Option<String>,
    /// The title a program running in the terminal set (FR-055): `title` when it was set while a
    /// program other than the shell ran, and gone when that program ends. The shell's own never
    /// counts.
    pub program_title: Option<String>,
    /// The title last known to be the shell's own: what showed while nothing ran.
    idle_title: Option<String>,
    /// Request id of an attach/spawn in flight.
    pub pending: Option<u64>,
    pub(crate) selecting: bool,
    pub(crate) scroll_remainder: f32,
    pub(crate) had_focus: bool,
    /// The widgets drawing this terminal: its own panel's, and any mirror's.
    pub(crate) widgets: std::collections::HashSet<egui::Id>,
    pub bell: bool,
    /// The terminal ended (FR-006) and its banner has not been drawn yet.
    pub unseen_exit: bool,
    /// A command other than the shell holds the foreground, as the daemon last listed it.
    pub busy: bool,
    /// Attached as an observer (FR-010): its first drawing must tell the daemon its real size.
    pub observing: bool,
    /// "Project › Tab › Panel", for accessibility and exit notices.
    pub label: String,
    /// The panel's find session, kept across detach and reattach.
    pub find: Option<search::TermFind>,
    /// The link a context menu was opened on.
    pub(crate) menu_link: Option<throng_core::links::Target>,
    /// Where the shell is working now, as far as throng can tell (its live working directory):
    /// its last OSC 7 report from this machine, else its process's own directory.
    pub cwd: Option<PathBuf>,
    /// The shell's own directory, read from outside it.
    pub(crate) process_cwd: Option<PathBuf>,
    /// The shell's pid while it runs, as the daemon last reported it.
    pub(crate) shell_pid: Option<u32>,
    /// The shell runs as administrator (Windows), as the daemon reported when it attached.
    pub elevated: bool,
    /// The last directory the shell reported itself.
    reported: Option<PathBuf>,
    /// The colours it is drawn with, for answering colour queries.
    pub(crate) palette: Option<colors::Palette>,
    /// A cell's size in pixels, for answering size queries.
    pub(crate) cell_px: (u16, u16),
    /// Text an input method is composing, drawn at the cursor until it is committed.
    pub(crate) preedit: Option<String>,
    osc7: osc7::Osc7,
    /// The screen's links, and the output version, scroll and size they were found at.
    link_cache: Option<(LinkCacheKey, Vec<links::ScreenLink>)>,
}

impl TerminalView {
    #[must_use]
    pub fn new(panel: PanelId, scrollback: usize) -> Self {
        let (events_tx, events) = crossbeam_channel::unbounded();
        let size = (80, 24);
        Self {
            panel,
            term: new_term(scrollback, size, events_tx.clone()),
            parser: Processor::new(),
            events,
            events_tx,
            scrollback,
            seen_until: 0,
            status: Status::Connecting,
            size,
            title: None,
            program_title: None,
            idle_title: None,
            pending: None,
            selecting: false,
            scroll_remainder: 0.0,
            had_focus: false,
            widgets: std::collections::HashSet::new(),
            bell: false,
            unseen_exit: false,
            busy: false,
            observing: false,
            label: String::new(),
            find: None,
            menu_link: None,
            link_cache: None,
            cwd: None,
            process_cwd: None,
            shell_pid: None,
            elevated: false,
            reported: None,
            osc7: osc7::Osc7::default(),
            palette: None,
            cell_px: (8, 16),
            preedit: None,
        }
    }

    /// A title set with OSC 0 or 2, or `None` for a reset. `busy` is whether a program other than
    /// the shell runs at this moment, when that can be told: a title set while the shell is idle
    /// is the shell's own; one set at a moment nobody can judge is settled by [`Self::set_busy`].
    pub fn set_title(&mut self, title: Option<String>, busy: Option<bool>) {
        match busy {
            Some(true) => self.program_title = title.clone(),
            Some(false) => {
                self.program_title = None;
                self.idle_title = title.clone();
            }
            None => {}
        }
        self.title = title;
    }

    /// The daemon's latest word on whether a program runs. A program's start adopts a title that
    /// arrived unjudged, unless it is what the idle shell showed; its end takes the title with it.
    pub fn set_busy(&mut self, busy: bool) {
        if busy
            && !self.busy
            && self.program_title.is_none()
            && self.title.is_some()
            && self.title != self.idle_title
        {
            self.program_title = self.title.clone();
        }
        if !busy {
            self.program_title = None;
            self.idle_title = self.title.clone();
        }
        self.busy = busy;
    }

    /// Whether the terminal needs attention (FR-055): its bell rang, or it ended, and nobody has
    /// looked yet.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.bell || self.unseen_exit
    }

    /// Grows with every byte of output shown (find re-runs when it changes).
    #[must_use]
    pub fn version(&self) -> u64 {
        self.seen_until
    }

    /// Rebuild the view from a snapshot. Anything the replayed bytes ask the program (cursor
    /// position, colours, device attributes) is dropped: the questions were asked long ago and
    /// answering them now would type garbage into the shell.
    pub fn apply_snapshot(&mut self, snapshot: &Snapshot) {
        self.term = new_term(self.scrollback, self.size, self.events_tx.clone());
        self.parser = Processor::new();
        self.parser.advance(&mut self.term, &snapshot.tail);
        self.scan_reports(&snapshot.tail);
        while self.events.try_recv().is_ok() {}
        self.seen_until = snapshot.end_offset;
        self.elevated = snapshot.elevated;
        self.shell_pid = snapshot.pid.filter(|_| snapshot.exited.is_none());
        self.status = match snapshot.exited {
            Some(status) => Status::Exited(status),
            None => Status::Running,
        };
    }

    /// Feed output; bytes already shown (offset below what the snapshot covered) are skipped.
    pub fn feed(&mut self, offset: u64, data: &[u8]) {
        let fresh = unseen(offset, data, self.seen_until);
        if !fresh.is_empty() {
            self.parser.advance(&mut self.term, fresh);
            self.scan_reports(fresh);
        }
        self.seen_until = self.seen_until.max(offset + data.len() as u64);
    }

    fn scan_reports(&mut self, data: &[u8]) {
        if let Some(uri) = self.osc7.scan(data)
            && let Some(dir) = osc7::local_directory(&uri, throng_platform::process::hostname().as_deref())
        {
            self.reported = Some(dir);
        }
    }

    /// The colour a query asked for: an override the program set, else throng's palette.
    #[must_use]
    pub fn queried_colour(&self, index: usize) -> alacritty_terminal::vte::ansi::Rgb {
        use alacritty_terminal::vte::ansi::Rgb;
        if let Some(rgb) = self.term.colors()[index] {
            return rgb;
        }
        let palette = self.palette.clone().unwrap_or_else(colors::Palette::dark);
        let c = match index {
            0..=255 => palette.indexed(u8::try_from(index).unwrap_or(0)),
            257 => palette.background,
            258 => palette.cursor,
            _ => palette.foreground,
        };
        Rgb { r: c.r(), g: c.g(), b: c.b() }
    }

    /// Settle where the shell is working; returns the directory when it changed.
    pub(crate) fn settle_directory(&mut self) -> Option<PathBuf> {
        let now = self.reported.clone().or_else(|| self.process_cwd.clone());
        if now.is_some() && now != self.cwd {
            self.cwd.clone_from(&now);
            return now;
        }
        None
    }

    /// Emulator events since the last call.
    pub fn drain_events(&mut self) -> Vec<TermEvent> {
        self.events.try_iter().collect()
    }

    /// Resize the emulator. Returns true when the size changed.
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        let size = (cols.max(2), rows.max(1));
        if size == self.size {
            return false;
        }
        self.size = size;
        self.term.resize(TermSize::new(usize::from(size.0), usize::from(size.1)));
        true
    }

    #[must_use]
    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    #[must_use]
    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }

    /// The links on the visible screen, found again only when the output, the scroll position or
    /// the size changed.
    pub fn screen_links(&mut self) -> Vec<links::ScreenLink> {
        let key = (self.seen_until, self.term.grid().display_offset(), self.size);
        match &self.link_cache {
            Some((k, found)) if *k == key => found.clone(),
            _ => {
                let found = links::scan(&self.term);
                self.link_cache = Some((key, found.clone()));
                found
            }
        }
    }

    /// `text` as the program should receive a paste of it: bracketed when it asked for that.
    #[must_use]
    pub fn paste_bytes(&self, text: &str) -> Vec<u8> {
        let bracketed = self.term.mode().contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
        keys::paste(text, bracketed)
    }

    pub fn term_mut(&mut self) -> &mut Term<Listener> {
        &mut self.term
    }

    /// Plain text of the whole buffer (scrollback and screen), for tests and "copy all". A line the
    /// terminal wrapped reads as the one line the program wrote, and a wide character once.
    #[must_use]
    pub fn text(&self) -> String {
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::cell::Flags;
        let grid = self.term.grid();
        let cols = grid.columns();
        let mut out = String::new();
        let top = -(grid.history_size() as i32);
        let bottom = grid.screen_lines() as i32;
        for line in top..bottom {
            let row = &grid[Line(line)];
            let spacer = Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER;
            out.extend(
                (0..cols)
                    .map(|c| &row[Column(c)])
                    .filter(|cell| !cell.flags.intersects(spacer))
                    .map(|cell| cell.c),
            );
            if cols > 0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE) {
                continue;
            }
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
        }
        out
    }
}

fn new_term(scrollback: usize, size: (u16, u16), tx: Sender<TermEvent>) -> Term<Listener> {
    // The kitty keyboard protocol is answered and its modes tracked, so a program can ask for
    // Shift+Enter to differ from Enter.
    let config = Config { scrolling_history: scrollback, kitty_keyboard: true, ..Config::default() };
    Term::new(config, &TermSize::new(usize::from(size.0), usize::from(size.1)), Listener(tx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use throng_core::ids::TerminalId;

    fn snapshot(tail: &[u8], end: u64) -> Snapshot {
        Snapshot {
            terminal: TerminalId::new(),
            tail: tail.to_vec(),
            end_offset: end,
            exited: None,
            alt_screen: false,
            elevated: false,
            pid: None,
        }
    }

    #[test]
    fn replayed_queries_are_not_answered() {
        let mut view = TerminalView::new(PanelId::new(), 1000);
        // DSR (cursor position report) and DA inside the replayed tail.
        view.apply_snapshot(&snapshot(b"hello\x1b[6n\x1b[c", 13));
        assert!(view.drain_events().iter().all(|e| !matches!(e, TermEvent::Reply(_))));
        // A live query is answered.
        view.feed(13, b"\x1b[6n");
        assert!(view.drain_events().iter().any(|e| matches!(e, TermEvent::Reply(_))));
    }

    #[test]
    fn colour_queries_are_answered_from_the_palette_and_program_overrides() {
        let mut view = TerminalView::new(PanelId::new(), 1000);
        view.palette = Some(colors::Palette::dark());
        view.feed(0, b"\x1b]11;?\x07");
        let answers: Vec<String> = view
            .drain_events()
            .into_iter()
            .filter_map(|e| match e {
                TermEvent::ColorQuery(index, answer) => Some(answer.word(view.queried_colour(index))),
                _ => None,
            })
            .collect();
        assert_eq!(answers, vec!["\x1b]11;rgb:1616/1818/1c1c\x07".to_owned()], "the background");
        // A colour the program set is the one it is told back.
        view.feed(8, b"\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07");
        let answer = view.drain_events().into_iter().find_map(|e| match e {
            TermEvent::ColorQuery(index, answer) => Some(answer.word(view.queried_colour(index))),
            _ => None,
        });
        assert_eq!(answer.as_deref(), Some("\x1b]4;1;rgb:1212/3434/5656\x07"));
    }

    #[test]
    fn a_program_can_switch_on_the_kitty_keyboard_protocol() {
        let mut view = TerminalView::new(PanelId::new(), 1000);
        assert!(!view.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        view.feed(0, b"\x1b[>1u");
        assert!(view.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        view.feed(5, b"\x1b[<u");
        assert!(!view.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES), "and off again");
    }

    #[test]
    fn output_already_in_the_snapshot_is_not_painted_twice() {
        let mut view = TerminalView::new(PanelId::new(), 1000);
        view.apply_snapshot(&snapshot(b"one\r\ntwo\r\n", 10));
        view.feed(5, b"two\r\nthree\r\n");
        let text = view.text();
        assert_eq!(text.matches("two").count(), 1, "{text}");
        assert!(text.contains("three"));
    }

    #[test]
    fn a_wrapped_line_reads_as_one_and_a_wide_character_once() {
        let mut view = TerminalView::new(PanelId::new(), 1000);
        view.resize(10, 5);
        view.feed(0, "$ '/a/long/path/x.txt' \r\n\u{6f22}\u{5b57} ok\r\n".as_bytes());
        assert_eq!(
            view.text().lines().take(2).collect::<Vec<_>>(),
            ["$ '/a/long/path/x.txt'", "\u{6f22}\u{5b57} ok"]
        );
    }

    #[test]
    fn titles_are_reported() {
        let mut view = TerminalView::new(PanelId::new(), 1000);
        view.feed(0, b"\x1b]0;build: web\x07");
        assert!(view.drain_events().iter().any(|e| matches!(e, TermEvent::Title(t) if t == "build: web")));
    }

    #[test]
    fn a_programs_title_is_told_from_the_shells() {
        let mut view = TerminalView::new(PanelId::new(), 100);
        // The shell's prompt sets a title while nothing runs: the shell's own.
        view.set_title(Some("me@host: ~".into()), Some(false));
        assert_eq!(view.program_title, None);
        // A command starts and sets no title of its own: the shell's stays its own.
        view.set_busy(true);
        assert_eq!(view.program_title, None, "the shell's title is not a program's");
        // The program sets one: that names the tab (FR-055).
        view.set_title(Some("vim notes.md".into()), Some(true));
        assert_eq!(view.program_title.as_deref(), Some("vim notes.md"));
        // The program ends: the title goes with it, whatever the shell says next.
        view.set_busy(false);
        assert_eq!(view.program_title, None);
        view.set_title(Some("me@host: ~".into()), Some(false));
        assert_eq!(view.program_title, None);
        // Before the daemon lists the shell nothing can be judged; the first listing settles it.
        let mut fresh = TerminalView::new(PanelId::new(), 100);
        fresh.set_title(Some("me@host: ~".into()), None);
        fresh.set_busy(false);
        fresh.set_busy(true);
        assert_eq!(fresh.program_title, None, "what the idle shell showed is the shell's own");
        let mut fresh = TerminalView::new(PanelId::new(), 100);
        fresh.set_title(Some("make".into()), None);
        fresh.set_busy(true);
        assert_eq!(fresh.program_title.as_deref(), Some("make"), "a program ran from the start");
    }

    #[test]
    fn resize_reports_changes_only() {
        let mut view = TerminalView::new(PanelId::new(), 1000);
        assert!(view.resize(100, 30));
        assert!(!view.resize(100, 30));
        assert_eq!(view.size, (100, 30));
    }
}
