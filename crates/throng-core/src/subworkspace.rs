//! Sub-workspaces: windows of their own, each holding tabs
//! of panels. A panel there either shows a project's panel (a mirror: the same session or document)
//! or belongs to the sub-workspace itself. The list, each one's name and where its window sits are
//! kept here; each one's tabs are a [`crate::workspace::Layout`] stored like a project's, under the
//! sub-workspace's id.

use serde::{Deserialize, Serialize};

use crate::ids::ProjectId;

/// Where a window sits, in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Place {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Place {
    /// This place moved (and shrunk if it must) to lie on a screen of `screen` points, so a window
    /// saved on a monitor that is gone opens where it can be seen.
    #[must_use]
    pub fn on_screen(self, screen: (f32, f32)) -> Self {
        let width = self.width.clamp(320.0, screen.0.max(320.0));
        let height = self.height.clamp(200.0, screen.1.max(200.0));
        let x = self.x.clamp(0.0, (screen.0 - width).max(0.0));
        let y = self.y.clamp(0.0, (screen.1 - height).max(0.0));
        Self { x, y, width, height }
    }
}

/// One sub-workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubWorkspace {
    /// Its layout is stored under this id, as a project's is under the project's.
    pub id: ProjectId,
    pub name: String,
    /// Whether its window is open (closing the window keeps the sub-workspace).
    #[serde(default = "yes")]
    pub open: bool,
    #[serde(default)]
    pub place: Option<Place>,
}

fn yes() -> bool {
    true
}

/// Every sub-workspace, in the order the sidebar lists them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SubWorkspaces {
    pub list: Vec<SubWorkspace>,
}

impl SubWorkspaces {
    /// Read the stored list; an unreadable one is empty (and says why).
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("sub-workspaces serialise")
    }

    #[must_use]
    pub fn get(&self, id: ProjectId) -> Option<&SubWorkspace> {
        self.list.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: ProjectId) -> Option<&mut SubWorkspace> {
        self.list.iter_mut().find(|s| s.id == id)
    }

    #[must_use]
    pub fn contains(&self, id: ProjectId) -> bool {
        self.get(id).is_some()
    }

    /// Add a new, open sub-workspace with the first free "Sub-workspace N" name.
    pub fn create(&mut self) -> ProjectId {
        let used: std::collections::BTreeSet<u64> =
            self.list.iter().filter_map(|s| s.name.strip_prefix("Sub-workspace ")?.parse().ok()).collect();
        let n = (1..).find(|n| !used.contains(n)).expect("infinite range");
        let id = ProjectId::new();
        self.list.push(SubWorkspace { id, name: format!("Sub-workspace {n}"), open: true, place: None });
        id
    }

    /// Rename one; an empty name is refused.
    pub fn rename(&mut self, id: ProjectId, name: &str) -> bool {
        let name = name.trim();
        match self.get_mut(id) {
            Some(sub) if !name.is_empty() => {
                name.clone_into(&mut sub.name);
                true
            }
            _ => false,
        }
    }

    pub fn remove(&mut self, id: ProjectId) -> Option<SubWorkspace> {
        let index = self.list.iter().position(|s| s.id == id)?;
        Some(self.list.remove(index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_fill_the_smallest_gap_and_the_list_round_trips() {
        let mut subs = SubWorkspaces::default();
        let a = subs.create();
        let b = subs.create();
        assert_eq!(subs.get(b).unwrap().name, "Sub-workspace 2");
        subs.remove(a);
        subs.create();
        assert_eq!(subs.list[1].name, "Sub-workspace 1");
        assert!(subs.rename(b, "  Logs ") && !subs.rename(b, "  "));
        assert_eq!(subs.get(b).unwrap().name, "Logs");
        subs.get_mut(b).unwrap().place = Some(Place { x: 1.0, y: 2.0, width: 800.0, height: 600.0 });
        assert_eq!(SubWorkspaces::parse(&subs.to_json()).unwrap(), subs);
        assert!(SubWorkspaces::parse("{").is_err());
    }

    #[test]
    fn a_window_saved_off_screen_comes_back_on_it() {
        let screen = (1920.0, 1080.0);
        let gone = Place { x: 3000.0, y: -400.0, width: 800.0, height: 600.0 };
        assert_eq!(gone.on_screen(screen), Place { x: 1120.0, y: 0.0, width: 800.0, height: 600.0 });
        let huge = Place { x: 10.0, y: 10.0, width: 5000.0, height: 3000.0 };
        assert_eq!(huge.on_screen(screen), Place { x: 0.0, y: 0.0, width: 1920.0, height: 1080.0 });
        let fine = Place { x: 100.0, y: 100.0, width: 800.0, height: 600.0 };
        assert_eq!(fine.on_screen(screen), fine);
    }
}
