//! throng's windows as one focus group: bringing any of them forward from another app brings them
//! all forward, the one the user chose last so it keeps the keyboard. Moving between throng's own
//! windows is not that, and raises nothing.

use std::time::{Duration, Instant};

use egui::ViewportId;

/// How long throng must have had no focused window for a focus to count as coming back from
/// another app. Moving between its own windows leaves none focused only for an instant.
const AWAY: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
pub struct FocusGroup {
    /// When throng last had no focused window (`None` while one is focused).
    unfocused_since: Option<Instant>,
    focused_before: bool,
}

impl FocusGroup {
    /// This frame's focus: which of throng's `windows` has it, if any. Returns the windows to raise,
    /// in order, ending with the focused one, when throng has just come back from another app.
    pub fn update(
        &mut self,
        focused: Option<ViewportId>,
        windows: &[ViewportId],
        now: Instant,
    ) -> Vec<ViewportId> {
        let Some(focused) = focused else {
            if self.focused_before || self.unfocused_since.is_none() {
                self.unfocused_since = Some(now);
            }
            self.focused_before = false;
            return Vec::new();
        };
        let returning = !self.focused_before
            && self.unfocused_since.is_some_and(|since| now.duration_since(since) >= AWAY);
        self.focused_before = true;
        self.unfocused_since = None;
        if !returning || windows.len() < 2 {
            return Vec::new();
        }
        windows.iter().copied().filter(|w| *w != focused).chain(std::iter::once(focused)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coming_back_from_another_app_raises_every_window_ending_with_the_chosen_one() {
        let (main, sub) = (ViewportId::ROOT, ViewportId::from_hash_of("sub"));
        let windows = [main, sub];
        let start = Instant::now();
        let mut group = FocusGroup::default();
        assert!(group.update(Some(main), &windows, start).is_empty(), "the first focus is no return");
        // Another app for a while, then the sub-workspace window is clicked.
        assert!(group.update(None, &windows, start + Duration::from_secs(1)).is_empty());
        assert_eq!(group.update(Some(sub), &windows, start + Duration::from_secs(3)), [main, sub]);
        assert!(group.update(Some(sub), &windows, start + Duration::from_secs(4)).is_empty(), "once");
    }

    #[test]
    fn moving_between_throngs_own_windows_raises_nothing() {
        let (main, sub) = (ViewportId::ROOT, ViewportId::from_hash_of("sub"));
        let windows = [main, sub];
        let start = Instant::now();
        let mut group = FocusGroup::default();
        group.update(Some(main), &windows, start);
        // No window focused for a moment, as the focus passes from one to the other.
        assert!(group.update(None, &windows, start + Duration::from_millis(10)).is_empty());
        assert!(group.update(Some(sub), &windows, start + Duration::from_millis(40)).is_empty());
        // One window alone is never "raised".
        group.update(None, &[main], start + Duration::from_secs(1));
        assert!(group.update(Some(main), &[main], start + Duration::from_secs(5)).is_empty());
    }
}
