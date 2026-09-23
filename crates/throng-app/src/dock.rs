//! Conversion between throng's layout model and the docking widget.
//!
//! The widget's own serialised form is never persisted: a tab's split tree is converted to a
//! `DockState` when shown, and back after the widget has run, so an upgrade of the widget can never
//! make a saved layout unreadable.

use egui_dock::{DockState, Node, NodeIndex, Split, SurfaceIndex, TabIndex, Tree};
use throng_core::ids::PanelId;
use throng_core::workspace::{Axis, SplitTree};

/// Build a dock from a split tree.
#[must_use]
pub fn to_dock(tree: &SplitTree) -> DockState<PanelId> {
    let mut dock = DockState::new(first_leaf(tree).0.clone());
    let surface = dock.main_surface_mut();
    build(surface, NodeIndex::root(), tree);
    dock
}

fn first_leaf(tree: &SplitTree) -> (&Vec<PanelId>, usize) {
    match tree {
        SplitTree::Leaf { panels, active } => (panels, *active),
        SplitTree::Split { first, .. } => first_leaf(first),
    }
}

/// `tree[index]` already holds a leaf with `first_leaf(node)`'s panels.
fn build(tree: &mut Tree<PanelId>, index: NodeIndex, node: &SplitTree) {
    match node {
        SplitTree::Leaf { panels, active } => {
            if let Ok(leaf) = tree.leaf_mut(index) {
                let _ = leaf.set_active_tab(TabIndex((*active).min(panels.len().saturating_sub(1))));
            }
        }
        SplitTree::Split { axis, fraction, first, second } => {
            let split = match axis {
                Axis::Horizontal => Split::Right,
                Axis::Vertical => Split::Below,
            };
            let second_tabs = first_leaf(second).0.clone();
            if second_tabs.is_empty() {
                build(tree, index, first);
                return;
            }
            let [old, new] =
                tree.split(index, split, fraction.clamp(0.05, 0.95), Node::leaf_with(second_tabs));
            build(tree, old, first);
            build(tree, new, second);
        }
    }
}

/// Read a dock back into a split tree. `None` when the dock holds no panels at all.
#[must_use]
pub fn from_dock(dock: &DockState<PanelId>) -> Option<SplitTree> {
    let (tree, torn) = read_dock(dock);
    let mut tree = tree?;
    // Panels dragged out into floating windows are folded back into the main tree: a panel must
    // never disappear from the saved layout.
    for panel in torn {
        if let Some(anchor) = tree.panels().last().copied() {
            tree.insert(anchor, panel, throng_core::workspace::Placement::Stack);
        }
    }
    tree.normalize();
    Some(tree)
}

/// The dock's main tree, and the panels dragged out of it into floating windows (a tear-off: the
/// caller moves them somewhere of their own, or folds them back with [`from_dock`]).
pub fn read_dock(dock: &DockState<PanelId>) -> (Option<SplitTree>, Vec<PanelId>) {
    let mut tree = read(dock.main_surface(), NodeIndex::root());
    let mut torn = Vec::new();
    for (surface_index, surface) in dock.iter_surfaces_indexed() {
        if surface_index == SurfaceIndex::main() {
            continue;
        }
        if let Some(tree_of_window) = surface.node_tree() {
            torn.extend(
                tree_of_window.tabs().copied().filter(|p| !tree.as_ref().is_some_and(|t| t.contains(*p))),
            );
        }
    }
    if let Some(tree) = tree.as_mut() {
        tree.normalize();
    }
    (tree, torn)
}

fn read(tree: &Tree<PanelId>, index: NodeIndex) -> Option<SplitTree> {
    if index.0 >= tree.len() {
        return None;
    }
    match &tree[index] {
        Node::Empty => None,
        Node::Leaf(leaf) => {
            if leaf.tabs.is_empty() {
                None
            } else {
                Some(SplitTree::Leaf { panels: leaf.tabs.clone(), active: leaf.active.0 })
            }
        }
        Node::Horizontal(split) | Node::Vertical(split) => {
            let axis =
                if matches!(tree[index], Node::Horizontal(_)) { Axis::Horizontal } else { Axis::Vertical };
            let first = read(tree, index.left());
            let second = read(tree, index.right());
            match (first, second) {
                (Some(a), Some(b)) => Some(SplitTree::Split {
                    axis,
                    fraction: split.fraction,
                    first: Box::new(a),
                    second: Box::new(b),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use throng_core::ids::ProjectId;
    use throng_core::workspace::{Layout, PanelKind, Placement};

    #[test]
    fn a_panel_dragged_into_a_floating_window_is_reported_torn_off_and_otherwise_folded_back() {
        let (a, b) = (PanelId::new(), PanelId::new());
        let tree = SplitTree::Leaf { panels: vec![a, b], active: 0 };
        let mut dock = to_dock(&tree);
        // What egui_dock does when a tab is dropped away from every drop target.
        let at = dock.find_tab(&b).unwrap();
        dock.remove_tab(at);
        dock.add_window(vec![b]);
        let (main, torn) = read_dock(&dock);
        assert_eq!(main, Some(SplitTree::leaf(a)));
        assert_eq!(torn, [b]);
        assert_eq!(from_dock(&dock).map(|t| t.panels()), Some(vec![a, b]), "never lost");
    }

    #[test]
    fn a_single_leaf_round_trips() {
        let a = PanelId::new();
        let b = PanelId::new();
        let tree = SplitTree::Leaf { panels: vec![a, b], active: 1 };
        assert_eq!(from_dock(&to_dock(&tree)), Some(tree));
    }

    #[test]
    fn nested_splits_round_trip_with_fractions_and_active_panels() {
        let project = ProjectId::new();
        let mut layout = Layout::new_default(project);
        let first = layout.tabs[0].root.panels()[0];
        let right = layout.add_panel(project, Some(first), Placement::Right, PanelKind::Untyped);
        let below = layout.add_panel(project, Some(right), Placement::Below, PanelKind::Untyped);
        let stacked = layout.add_panel(project, Some(first), Placement::Stack, PanelKind::Untyped);
        let left = layout.add_panel(project, Some(below), Placement::Left, PanelKind::Untyped);
        let mut tree = layout.tabs[0].root.clone();
        if let SplitTree::Split { fraction, .. } = &mut tree {
            *fraction = 0.3;
        }
        let back = from_dock(&to_dock(&tree)).unwrap();
        assert_eq!(back, tree);
        assert_eq!(back.panels(), vec![first, stacked, right, left, below]);
    }

    #[test]
    fn an_empty_dock_reads_as_none() {
        let mut dock = DockState::new(vec![PanelId::new()]);
        let only = dock.main_surface().tabs().next().copied().unwrap();
        dock.main_surface_mut().retain_tabs(|t| *t != only);
        assert_eq!(from_dock(&dock), None);
    }
}
