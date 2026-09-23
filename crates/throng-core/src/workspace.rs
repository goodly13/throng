//! The docking workspace model (Principle XI): a project's Workspace Pane holds Tabs; each Tab holds
//! one split tree whose leaves hold Panels.
//!
//! This model is throng's own, versioned and serialisable. The UI converts it to and from its docking
//! widget at the edge, so upgrading the widget can never make a saved layout unreadable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ids::{PanelId, ProjectId, TabId};
use crate::terminal::TerminalPanelConfig;

/// Current layout document version. Bump it with a migration in [`Layout::from_json`].
pub const LAYOUT_SCHEMA_VERSION: u32 = 1;

/// Splits never give either side less than this share, so a pane cannot be dragged out of reach.
pub const MIN_FRACTION: f32 = 0.05;

/// A project's whole workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Layout {
    pub version: u32,
    pub tabs: Vec<Tab>,
    pub active_tab: Option<TabId>,
    pub panels: BTreeMap<PanelId, Panel>,
}

/// A tab: a titled split tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tab {
    pub id: TabId,
    pub title: String,
    #[serde(default)]
    pub title_is_custom: bool,
    pub root: SplitTree,
    /// The panel that last had focus in this tab.
    #[serde(default)]
    pub active_panel: Option<PanelId>,
}

/// Which way a split divides its space.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Axis {
    /// Side by side: `first` on the left.
    Horizontal,
    /// Stacked: `first` on top.
    Vertical,
}

/// A recursive split tree. Leaves hold one or more panels, one of them showing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SplitTree {
    #[serde(rename_all = "camelCase")]
    Leaf { panels: Vec<PanelId>, active: usize },
    #[serde(rename_all = "camelCase")]
    Split { axis: Axis, fraction: f32, first: Box<SplitTree>, second: Box<SplitTree> },
}

/// Where a new panel goes relative to an existing one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// In the same leaf, as another stacked panel.
    Stack,
    Right,
    Below,
    Left,
    Above,
}

/// A panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    pub id: PanelId,
    /// The project the panel was created in. Never changes.
    pub origin_project: ProjectId,
    /// The effective label: a default ("Panel 3") or the user's rename.
    pub title: String,
    #[serde(default)]
    pub title_is_custom: bool,
    #[serde(flatten)]
    pub kind: PanelKind,
}

/// What a panel shows. Untyped panels show the type picker.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "config", rename_all = "camelCase")]
pub enum PanelKind {
    #[default]
    Untyped,
    Terminal(TerminalPanelConfig),
    Editor(EditorPanelConfig),
    /// Find (and replace) in Files.
    Search(SearchPanelConfig),
    /// A read-only rendering of a file, bound to the file, never to an editor.
    Preview(PreviewPanelConfig),
    /// A project's panel shown in a sub-workspace: the same terminal session or document, not a
    /// copy. Only ever found in a sub-workspace's layout.
    Mirror(MirrorPanelConfig),
}

/// Which project's panel a mirror shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirrorPanelConfig {
    pub project: ProjectId,
    pub panel: PanelId,
}

/// What a preview panel remembers: the file it shows, and where it has been.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPanelConfig {
    pub path: PathBuf,
    /// Where it has been, oldest first; `at` is the entry it shows. Empty until it first moves on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<HistoryEntry>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub at: usize,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference
fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// One place a preview has shown: a file, and how far down it had been read when it was left.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub path: PathBuf,
    /// The scroll offset in points.
    #[serde(default)]
    pub scroll: u32,
}

impl PreviewPanelConfig {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path, history: Vec::new(), at: 0 }
    }

    /// The history as it stands, starting it (one entry, the file shown) when there is none or
    /// what was saved no longer matches the file shown.
    fn started(&mut self) {
        if self.history.get(self.at).is_none_or(|e| e.path != self.path) {
            self.history = vec![HistoryEntry { path: self.path.clone(), scroll: 0 }];
            self.at = 0;
        }
    }

    /// Show `path` next: a link followed in place, or a heading in the same file (an entry of its
    /// own, so Back returns to where the reader was). `scroll` is how far down the entry being left
    /// was read. The entries after it go, `path` becomes the newest, and at most `cap` are kept,
    /// oldest dropped first.
    pub fn open(&mut self, path: PathBuf, scroll: u32, cap: usize) {
        self.started();
        self.history[self.at].scroll = scroll;
        self.history.truncate(self.at + 1);
        self.history.push(HistoryEntry { path: path.clone(), scroll: 0 });
        self.path = path;
        self.at = self.history.len() - 1;
        self.cap(cap);
    }

    /// Step to the entry before this one; `scroll` is how far down this one was read.
    pub fn back(&mut self, scroll: u32) -> Option<HistoryEntry> {
        self.started();
        self.step(scroll, self.at.checked_sub(1)?)
    }

    /// Step to the entry after this one; `scroll` is how far down this one was read.
    pub fn forward(&mut self, scroll: u32) -> Option<HistoryEntry> {
        self.started();
        let next = self.at + 1;
        (next < self.history.len()).then_some(())?;
        self.step(scroll, next)
    }

    fn step(&mut self, scroll: u32, to: usize) -> Option<HistoryEntry> {
        self.history[self.at].scroll = scroll;
        self.at = to;
        let entry = self.history[to].clone();
        self.path = entry.path.clone();
        Some(entry)
    }

    #[must_use]
    pub fn can_back(&self) -> bool {
        self.at > 0 && self.history.get(self.at).is_some_and(|e| e.path == self.path)
    }

    #[must_use]
    pub fn can_forward(&self) -> bool {
        self.at + 1 < self.history.len() && self.history.get(self.at).is_some_and(|e| e.path == self.path)
    }

    /// Keep at most `cap` entries (at least one), dropping the oldest and never the current one.
    pub fn cap(&mut self, cap: usize) {
        let excess = self.history.len().saturating_sub(cap.max(1));
        let drop = excess.min(self.at);
        self.history.drain(..drop);
        self.at -= drop;
        self.history.truncate(cap.max(1).max(self.at + 1));
    }

    /// A file moved: every entry naming it, or a file under it, follows.
    pub fn rebase(&mut self, rebased: impl Fn(&Path) -> Option<PathBuf>) {
        if let Some(path) = rebased(&self.path) {
            self.path = path;
        }
        for entry in &mut self.history {
            if let Some(path) = rebased(&entry.path) {
                entry.path = path;
            }
        }
    }
}

/// What a Find in Files panel remembers across a restart: the query, never the results.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SearchPanelConfig {
    pub term: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    /// A folder or file inside the project, relative to its root; empty means the whole project.
    pub scope: String,
    pub replace: bool,
    pub replacement: String,
    pub group_by_folder: bool,
}

/// What an Editor Panel remembers. Encoding and line endings are NOT here: they belong to the
/// document and are re-derived from the file's bytes, never echoed back from a view.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorPanelConfig {
    /// `None` for a never-saved new document.
    #[serde(default)]
    pub path: Option<PathBuf>,
}

/// Why a layout document was rejected.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum LayoutError {
    #[error("the layout could not be parsed: {0}")]
    Parse(String),
    #[error("the layout was written by a newer throng (version {0})")]
    NewerVersion(u32),
    #[error("panel {0} appears in the tree but has no record")]
    MissingPanel(PanelId),
    #[error("panel {0} appears more than once")]
    DuplicatePanel(PanelId),
    #[error("a tab has an empty split tree")]
    EmptyTab,
}

impl SplitTree {
    #[must_use]
    pub fn leaf(panel: PanelId) -> Self {
        Self::Leaf { panels: vec![panel], active: 0 }
    }

    /// Every panel, in layout order (depth first, first child before second).
    #[must_use]
    pub fn panels(&self) -> Vec<PanelId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PanelId>) {
        match self {
            Self::Leaf { panels, .. } => out.extend(panels.iter().copied()),
            Self::Split { first, second, .. } => {
                first.collect(out);
                second.collect(out);
            }
        }
    }

    #[must_use]
    pub fn contains(&self, panel: PanelId) -> bool {
        match self {
            Self::Leaf { panels, .. } => panels.contains(&panel),
            Self::Split { first, second, .. } => first.contains(panel) || second.contains(panel),
        }
    }

    fn is_empty(&self) -> bool {
        matches!(self, Self::Leaf { panels, .. } if panels.is_empty())
    }

    /// Remove `panel`; returns whether it was found. Empty leaves collapse into their sibling.
    pub fn remove(&mut self, panel: PanelId) -> bool {
        let found = self.remove_inner(panel);
        self.normalize();
        found
    }

    fn remove_inner(&mut self, panel: PanelId) -> bool {
        match self {
            Self::Leaf { panels, active } => {
                let Some(i) = panels.iter().position(|p| *p == panel) else { return false };
                panels.remove(i);
                if *active >= panels.len() {
                    *active = panels.len().saturating_sub(1);
                } else if i < *active {
                    *active -= 1;
                }
                true
            }
            Self::Split { first, second, .. } => first.remove_inner(panel) || second.remove_inner(panel),
        }
    }

    /// Put `new` beside `anchor`. Returns false (and changes nothing) when `anchor` is absent.
    pub fn insert(&mut self, anchor: PanelId, new: PanelId, placement: Placement) -> bool {
        match self {
            Self::Leaf { panels, active } => {
                let Some(i) = panels.iter().position(|p| *p == anchor) else { return false };
                if placement == Placement::Stack {
                    panels.insert(i + 1, new);
                    *active = i + 1;
                    return true;
                }
                let existing = std::mem::replace(self, Self::leaf(new));
                let fresh = Self::leaf(new);
                let (axis, new_first) = match placement {
                    Placement::Right => (Axis::Horizontal, false),
                    Placement::Left => (Axis::Horizontal, true),
                    Placement::Below => (Axis::Vertical, false),
                    Placement::Above => (Axis::Vertical, true),
                    Placement::Stack => unreachable!(),
                };
                let (first, second) = if new_first { (fresh, existing) } else { (existing, fresh) };
                *self = Self::Split { axis, fraction: 0.5, first: Box::new(first), second: Box::new(second) };
                true
            }
            Self::Split { first, second, .. } => {
                first.insert(anchor, new, placement) || second.insert(anchor, new, placement)
            }
        }
    }

    /// Show `panel` in its leaf.
    pub fn activate(&mut self, panel: PanelId) -> bool {
        match self {
            Self::Leaf { panels, active } => match panels.iter().position(|p| *p == panel) {
                Some(i) => {
                    *active = i;
                    true
                }
                None => false,
            },
            Self::Split { first, second, .. } => first.activate(panel) || second.activate(panel),
        }
    }

    /// Collapse empty leaves and single-child splits; clamp fractions and active indices.
    pub fn normalize(&mut self) {
        if let Self::Split { first, second, fraction, .. } = self {
            first.normalize();
            second.normalize();
            if !fraction.is_finite() {
                *fraction = 0.5;
            }
            *fraction = fraction.clamp(MIN_FRACTION, 1.0 - MIN_FRACTION);
            if first.is_empty() {
                let keep = std::mem::replace(second.as_mut(), Self::Leaf { panels: vec![], active: 0 });
                *self = keep;
            } else if second.is_empty() {
                let keep = std::mem::replace(first.as_mut(), Self::Leaf { panels: vec![], active: 0 });
                *self = keep;
            }
        } else if let Self::Leaf { panels, active } = self
            && *active >= panels.len()
        {
            *active = panels.len().saturating_sub(1);
        }
    }
}

impl Layout {
    /// A fresh layout: one tab holding one untyped panel.
    #[must_use]
    pub fn new_default(project: ProjectId) -> Self {
        let mut layout = Self {
            version: LAYOUT_SCHEMA_VERSION,
            tabs: Vec::new(),
            active_tab: None,
            panels: BTreeMap::new(),
        };
        layout.add_tab(project, PanelKind::Untyped);
        layout
    }

    /// Parse a stored layout. Anything unreadable is an error, and the caller keeps the stored
    /// document aside rather than writing a default over it.
    pub fn from_json(text: &str) -> Result<Self, LayoutError> {
        let value: serde_json::Value =
            serde_json::from_str(text).map_err(|e| LayoutError::Parse(e.to_string()))?;
        let version = value.get("version").and_then(serde_json::Value::as_u64).unwrap_or(0) as u32;
        if version > LAYOUT_SCHEMA_VERSION {
            return Err(LayoutError::NewerVersion(version));
        }
        let layout: Self = serde_json::from_value(value).map_err(|e| LayoutError::Parse(e.to_string()))?;
        layout.check()?;
        Ok(layout)
    }

    /// Serialise for storage.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("a layout always serialises")
    }

    /// Structural invariants: every panel in a tree has a record, and appears once.
    pub fn check(&self) -> Result<(), LayoutError> {
        let mut seen = std::collections::BTreeSet::new();
        for tab in &self.tabs {
            let panels = tab.root.panels();
            if panels.is_empty() {
                return Err(LayoutError::EmptyTab);
            }
            for panel in panels {
                if !seen.insert(panel) {
                    return Err(LayoutError::DuplicatePanel(panel));
                }
                if !self.panels.contains_key(&panel) {
                    return Err(LayoutError::MissingPanel(panel));
                }
            }
        }
        Ok(())
    }

    /// Drop panel records no tree refers to (a crash between two writes can leave one behind).
    pub fn prune_orphans(&mut self) -> Vec<Panel> {
        let live: std::collections::BTreeSet<PanelId> =
            self.tabs.iter().flat_map(|t| t.root.panels()).collect();
        let orphans: Vec<PanelId> = self.panels.keys().filter(|id| !live.contains(id)).copied().collect();
        orphans.into_iter().filter_map(|id| self.panels.remove(&id)).collect()
    }

    #[must_use]
    pub fn tab(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    #[must_use]
    pub fn active_tab(&self) -> Option<&Tab> {
        self.active_tab.and_then(|id| self.tab(id)).or_else(|| self.tabs.first())
    }

    /// The tab holding `panel`.
    #[must_use]
    pub fn tab_of(&self, panel: PanelId) -> Option<TabId> {
        self.tabs.iter().find(|t| t.root.contains(panel)).map(|t| t.id)
    }

    /// The next free default panel title, "Panel N" with the smallest unused N.
    #[must_use]
    pub fn next_panel_title(&self) -> String {
        next_numbered("Panel", self.panels.values().map(|p| p.title.as_str()))
    }

    /// The next free default tab title.
    #[must_use]
    pub fn next_tab_title(&self) -> String {
        next_numbered("Tab", self.tabs.iter().map(|t| t.title.as_str()))
    }

    fn new_panel(&mut self, project: ProjectId, kind: PanelKind) -> PanelId {
        let panel = Panel {
            id: PanelId::new(),
            origin_project: project,
            title: self.next_panel_title(),
            title_is_custom: false,
            kind,
        };
        let id = panel.id;
        self.panels.insert(id, panel);
        id
    }

    /// Add a tab holding one new panel and make it active. Returns the tab and panel.
    pub fn add_tab(&mut self, project: ProjectId, kind: PanelKind) -> (TabId, PanelId) {
        let panel = self.new_panel(project, kind);
        let tab = Tab {
            id: TabId::new(),
            title: self.next_tab_title(),
            title_is_custom: false,
            root: SplitTree::leaf(panel),
            active_panel: Some(panel),
        };
        let id = tab.id;
        self.tabs.push(tab);
        self.active_tab = Some(id);
        (id, panel)
    }

    /// Add a panel beside `anchor` (or into the active tab's active panel's leaf when `anchor` is
    /// `None`), make it active, and return it. With no tabs at all, a tab is created.
    pub fn add_panel(
        &mut self,
        project: ProjectId,
        anchor: Option<PanelId>,
        placement: Placement,
        kind: PanelKind,
    ) -> PanelId {
        let anchor = anchor
            .or_else(|| self.active_tab().and_then(|t| t.active_panel.or(t.root.panels().first().copied())));
        let Some(anchor) = anchor.filter(|a| self.tab_of(*a).is_some()) else {
            return self.add_tab(project, kind).1;
        };
        let new = self.new_panel(project, kind);
        let tab_id = self.tab_of(anchor).expect("checked above");
        let tab = self.tab_mut(tab_id).expect("tab exists");
        tab.root.insert(anchor, new, placement);
        tab.active_panel = Some(new);
        self.active_tab = Some(tab_id);
        new
    }

    /// Remove a panel and return its record. Focus moves to the panel before it in layout order, or
    /// the one after when it was first. A tab left empty is closed.
    pub fn remove_panel(&mut self, panel: PanelId) -> Option<Panel> {
        let tab_id = self.tab_of(panel)?;
        let tab = self.tab_mut(tab_id)?;
        let order = tab.root.panels();
        let index = order.iter().position(|p| *p == panel)?;
        tab.root.remove(panel);
        if tab.root.panels().is_empty() {
            self.close_tab_inner(tab_id);
        } else if tab.active_panel == Some(panel) || tab.active_panel.is_none() {
            let next = if index > 0 { order[index - 1] } else { order[1] };
            tab.active_panel = Some(next);
            tab.root.activate(next);
        }
        self.panels.remove(&panel)
    }

    /// Close a tab, returning the records of every panel it held.
    pub fn close_tab(&mut self, tab: TabId) -> Vec<Panel> {
        let Some(t) = self.tab(tab) else { return Vec::new() };
        let ids = t.root.panels();
        self.close_tab_inner(tab);
        ids.into_iter().filter_map(|id| self.panels.remove(&id)).collect()
    }

    fn close_tab_inner(&mut self, tab: TabId) {
        let Some(index) = self.tabs.iter().position(|t| t.id == tab) else { return };
        self.tabs.remove(index);
        if self.active_tab == Some(tab) || self.active_tab.is_none() {
            let next = if index > 0 { index - 1 } else { 0 };
            self.active_tab = self.tabs.get(next).map(|t| t.id);
        }
    }

    /// Focus a panel: activate its tab, its leaf, and record it as the tab's active panel.
    pub fn focus_panel(&mut self, panel: PanelId) -> bool {
        let Some(tab_id) = self.tab_of(panel) else { return false };
        let tab = self.tab_mut(tab_id).expect("tab exists");
        tab.active_panel = Some(panel);
        tab.root.activate(panel);
        self.active_tab = Some(tab_id);
        true
    }

    /// Rename a panel; an empty name resets it to a fresh default.
    pub fn rename_panel(&mut self, panel: PanelId, title: &str) -> bool {
        let title = title.trim();
        if !self.panels.contains_key(&panel) {
            return false;
        }
        let fresh = if title.is_empty() { Some(self.next_panel_title()) } else { None };
        let record = self.panels.get_mut(&panel).expect("checked");
        match fresh {
            Some(default) => {
                record.title = default;
                record.title_is_custom = false;
            }
            None => {
                record.title = title.to_owned();
                record.title_is_custom = true;
            }
        }
        true
    }

    /// Rename a tab; an empty name resets it to a fresh default.
    pub fn rename_tab(&mut self, tab: TabId, title: &str) -> bool {
        let title = title.trim();
        let fresh = if title.is_empty() { Some(self.next_tab_title()) } else { None };
        let Some(record) = self.tab_mut(tab) else { return false };
        match fresh {
            Some(default) => {
                record.title = default;
                record.title_is_custom = false;
            }
            None => {
                record.title = title.to_owned();
                record.title_is_custom = true;
            }
        }
        true
    }

    /// Change what a panel shows.
    pub fn set_kind(&mut self, panel: PanelId, kind: PanelKind) -> bool {
        match self.panels.get_mut(&panel) {
            Some(record) => {
                record.kind = kind;
                true
            }
            None => false,
        }
    }

    /// Move a tab to `to` in the tab strip (clamped).
    pub fn move_tab(&mut self, tab: TabId, to: usize) {
        if let Some(from) = self.tabs.iter().position(|t| t.id == tab) {
            let t = self.tabs.remove(from);
            let to = to.min(self.tabs.len());
            self.tabs.insert(to, t);
        }
    }

    /// Panels mirroring `project`'s panels (only `panel`, when given).
    #[must_use]
    pub fn mirrors_of(&self, project: ProjectId, panel: Option<PanelId>) -> Vec<PanelId> {
        self.panels
            .values()
            .filter(|p| match &p.kind {
                PanelKind::Mirror(m) => m.project == project && panel.is_none_or(|x| x == m.panel),
                _ => false,
            })
            .map(|p| p.id)
            .collect()
    }

    /// Every terminal panel with its config.
    pub fn terminal_panels(&self) -> impl Iterator<Item = (&Panel, &TerminalPanelConfig)> {
        self.panels.values().filter_map(|p| match &p.kind {
            PanelKind::Terminal(cfg) => Some((p, cfg)),
            _ => None,
        })
    }
}

fn next_numbered<'a>(prefix: &str, taken: impl Iterator<Item = &'a str>) -> String {
    let used: std::collections::BTreeSet<u64> = taken
        .filter_map(|title| title.strip_prefix(prefix)?.strip_prefix(' ')?.parse::<u64>().ok())
        .collect();
    let n = (1..).find(|n| !used.contains(n)).expect("infinite range");
    // Ordinals are names, not quantities: never digit-grouped.
    format!("{prefix} {n}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_previews_history_steps_back_and_forward_and_a_new_file_drops_what_was_ahead() {
        let p = |s: &str| PathBuf::from(s);
        let mut config = PreviewPanelConfig::new(p("/a.md"));
        assert!(!config.can_back() && !config.can_forward());
        config.open(p("/b.md"), 120, 10);
        config.open(p("/c.md"), 40, 10);
        assert_eq!((config.path.clone(), config.at, config.history.len()), (p("/c.md"), 2, 3));
        let back = config.back(7).unwrap();
        assert_eq!((back.path, back.scroll), (p("/b.md"), 40));
        assert_eq!(config.back(0).unwrap().scroll, 120, "each entry keeps where it was read to");
        assert!(config.back(0).is_none() && !config.can_back());
        assert_eq!(config.forward(0).unwrap().path, p("/b.md"));
        assert!(config.can_forward());
        config.open(p("/d.md"), 0, 10);
        assert!(!config.can_forward(), "opening a file drops what was ahead");
        assert_eq!(
            config.history.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
            [p("/a.md"), p("/b.md"), p("/d.md")]
        );

        // A heading in the same file is an entry of its own, from the top of the file too.
        config.open(p("/d.md"), 0, 10);
        config.open(p("/d.md"), 300, 10);
        assert_eq!(config.history.len(), 5);
        // Each step passes where the reader is: the place the last step restored.
        assert_eq!(config.back(20).unwrap(), HistoryEntry { path: p("/d.md"), scroll: 300 });
        assert_eq!(config.back(300).unwrap(), HistoryEntry { path: p("/d.md"), scroll: 0 }, "the top");
        assert_eq!(config.forward(0).unwrap(), HistoryEntry { path: p("/d.md"), scroll: 300 });
        assert_eq!(config.forward(300).unwrap(), HistoryEntry { path: p("/d.md"), scroll: 20 });
    }

    #[test]
    fn a_previews_history_is_capped_oldest_first_and_follows_a_move() {
        let p = |s: &str| PathBuf::from(s);
        let mut config = PreviewPanelConfig::new(p("/0.md"));
        for i in 1..6 {
            config.open(p(&format!("/{i}.md")), 0, 3);
        }
        assert_eq!(
            config.history.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
            [p("/3.md"), p("/4.md"), p("/5.md")]
        );
        assert_eq!(config.at, 2);
        config.back(0);
        config.back(0);
        config.cap(1);
        assert_eq!(
            (config.history.len(), config.at, config.path.clone()),
            (1, 0, p("/3.md")),
            "never the current"
        );

        config.rebase(|path| path.strip_prefix("/").ok().map(|rest| p("/moved").join(rest)));
        assert_eq!(config.path, p("/moved/3.md"));
        assert_eq!(config.history[0].path, p("/moved/3.md"));

        let old: PreviewPanelConfig = serde_json::from_str(r#"{"path":"/x.md"}"#).unwrap();
        assert_eq!(old, PreviewPanelConfig::new(p("/x.md")), "a config saved before history reads");
        assert_eq!(serde_json::to_string(&old).unwrap(), r#"{"path":"/x.md"}"#);
    }

    fn project() -> ProjectId {
        ProjectId::new()
    }

    #[test]
    fn default_layout_has_one_tab_one_panel() {
        let p = project();
        let layout = Layout::new_default(p);
        assert_eq!(layout.tabs.len(), 1);
        assert_eq!(layout.panels.len(), 1);
        assert_eq!(layout.tabs[0].title, "Tab 1");
        let panel = layout.panels.values().next().unwrap();
        assert_eq!(panel.title, "Panel 1");
        assert_eq!(panel.kind, PanelKind::Untyped);
        assert_eq!(panel.origin_project, p);
        layout.check().unwrap();
    }

    #[test]
    fn titles_fill_the_smallest_gap_and_never_group_digits() {
        let p = project();
        let mut layout = Layout::new_default(p);
        let a = layout.add_panel(p, None, Placement::Right, PanelKind::Untyped);
        let b = layout.add_panel(p, None, Placement::Below, PanelKind::Untyped);
        assert_eq!(layout.panels[&a].title, "Panel 2");
        assert_eq!(layout.panels[&b].title, "Panel 3");
        layout.remove_panel(a);
        let c = layout.add_panel(p, None, Placement::Stack, PanelKind::Untyped);
        assert_eq!(layout.panels[&c].title, "Panel 2");
        assert_eq!(next_numbered("Panel", (1..1024).map(|_| "x")), "Panel 1");
        let taken: Vec<String> = (1..=1024).map(|n| format!("Panel {n}")).collect();
        assert_eq!(next_numbered("Panel", taken.iter().map(String::as_str)), "Panel 1025");
    }

    #[test]
    fn splits_place_panels_on_the_requested_side() {
        let p = project();
        let mut layout = Layout::new_default(p);
        let first = layout.tabs[0].root.panels()[0];
        let right = layout.add_panel(p, Some(first), Placement::Right, PanelKind::Untyped);
        let above = layout.add_panel(p, Some(right), Placement::Above, PanelKind::Untyped);
        let root = &layout.tabs[0].root;
        assert_eq!(root.panels(), vec![first, above, right]);
        match root {
            SplitTree::Split { axis: Axis::Horizontal, second, .. } => {
                assert!(matches!(second.as_ref(), SplitTree::Split { axis: Axis::Vertical, .. }));
            }
            other => panic!("unexpected tree {other:?}"),
        }
        layout.check().unwrap();
    }

    #[test]
    fn removing_focus_falls_back_to_the_previous_panel_then_the_next() {
        let p = project();
        let mut layout = Layout::new_default(p);
        let first = layout.tabs[0].root.panels()[0];
        let second = layout.add_panel(p, Some(first), Placement::Right, PanelKind::Untyped);
        let third = layout.add_panel(p, Some(second), Placement::Right, PanelKind::Untyped);
        layout.focus_panel(third);
        layout.remove_panel(third);
        assert_eq!(layout.tabs[0].active_panel, Some(second));
        layout.focus_panel(first);
        layout.remove_panel(first);
        assert_eq!(layout.tabs[0].active_panel, Some(second));
        assert_eq!(layout.tabs[0].root, SplitTree::leaf(second));
    }

    #[test]
    fn removing_the_last_panel_closes_the_tab() {
        let p = project();
        let mut layout = Layout::new_default(p);
        let (tab2, only) = layout.add_tab(p, PanelKind::Untyped);
        assert_eq!(layout.active_tab, Some(tab2));
        assert!(layout.remove_panel(only).is_some());
        assert_eq!(layout.tabs.len(), 1);
        assert_eq!(layout.active_tab, Some(layout.tabs[0].id));
    }

    #[test]
    fn stacking_keeps_one_leaf() {
        let p = project();
        let mut layout = Layout::new_default(p);
        let first = layout.tabs[0].root.panels()[0];
        let stacked = layout.add_panel(p, Some(first), Placement::Stack, PanelKind::Untyped);
        assert_eq!(layout.tabs[0].root, SplitTree::Leaf { panels: vec![first, stacked], active: 1 });
        layout.remove_panel(stacked);
        assert_eq!(layout.tabs[0].root, SplitTree::Leaf { panels: vec![first], active: 0 });
    }

    #[test]
    fn json_round_trips_and_rejects_bad_documents() {
        let p = project();
        let mut layout = Layout::new_default(p);
        let first = layout.tabs[0].root.panels()[0];
        layout.set_kind(
            first,
            PanelKind::Terminal(TerminalPanelConfig { shell: Some("bash".into()), ..Default::default() }),
        );
        layout.add_panel(
            p,
            Some(first),
            Placement::Below,
            PanelKind::Editor(EditorPanelConfig { path: Some("/p/a.rs".into()) }),
        );
        let text = layout.to_json();
        assert_eq!(Layout::from_json(&text).unwrap(), layout);

        assert!(matches!(Layout::from_json("{"), Err(LayoutError::Parse(_))));
        let newer = text.replacen("\"version\":1", "\"version\":99", 1);
        assert_eq!(Layout::from_json(&newer), Err(LayoutError::NewerVersion(99)));

        let mut broken = layout.clone();
        broken.panels.remove(&first);
        assert_eq!(Layout::from_json(&broken.to_json()), Err(LayoutError::MissingPanel(first)));
    }

    #[test]
    fn orphans_are_pruned() {
        let p = project();
        let mut layout = Layout::new_default(p);
        let orphan = Panel {
            id: PanelId::new(),
            origin_project: p,
            title: "x".into(),
            title_is_custom: false,
            kind: PanelKind::Untyped,
        };
        layout.panels.insert(orphan.id, orphan.clone());
        assert_eq!(layout.prune_orphans(), vec![orphan]);
        layout.check().unwrap();
    }

    #[test]
    fn normalize_clamps_fractions_and_collapses() {
        let a = PanelId::new();
        let mut tree = SplitTree::Split {
            axis: Axis::Horizontal,
            fraction: 7.0,
            first: Box::new(SplitTree::leaf(a)),
            second: Box::new(SplitTree::Leaf { panels: vec![], active: 3 }),
        };
        tree.normalize();
        assert_eq!(tree, SplitTree::leaf(a));
        let b = PanelId::new();
        let mut tree = SplitTree::Split {
            axis: Axis::Horizontal,
            fraction: f32::NAN,
            first: Box::new(SplitTree::leaf(a)),
            second: Box::new(SplitTree::leaf(b)),
        };
        tree.normalize();
        assert!(matches!(tree, SplitTree::Split { fraction, .. } if (fraction - 0.5).abs() < f32::EPSILON));
    }

    #[test]
    fn renaming_to_empty_resets_to_a_default() {
        let p = project();
        let mut layout = Layout::new_default(p);
        let first = layout.tabs[0].root.panels()[0];
        layout.rename_panel(first, "  build  ");
        assert_eq!(layout.panels[&first].title, "build");
        assert!(layout.panels[&first].title_is_custom);
        layout.rename_panel(first, " ");
        assert_eq!(layout.panels[&first].title, "Panel 1");
        assert!(!layout.panels[&first].title_is_custom);
    }
}
