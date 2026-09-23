//! Notices: one condition, one notice (Principle VI).
//!
//! A notice is keyed by the condition it reports. Raising a key that is already showing never adds a
//! second notice: it updates the one there and makes it louder (a flash), so however many callers
//! bounce off the same state, the user sees it once, with its remedies attached.

/// How serious a notice is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// An action offered on a notice. The owner of the condition decides what `id` does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoticeAction {
    pub id: String,
    pub label: String,
}

impl NoticeAction {
    #[must_use]
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self { id: id.into(), label: label.into() }
    }
}

/// A notice on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The condition this reports. Equal keys are the same condition.
    pub key: String,
    pub severity: Severity,
    /// What is wrong — not what the user may not do.
    pub message: String,
    pub detail: Option<String>,
    pub actions: Vec<NoticeAction>,
    /// Incremented each time the condition is raised again while showing.
    pub repeats: u32,
}

impl Notice {
    #[must_use]
    pub fn new(key: impl Into<String>, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            severity,
            message: message.into(),
            detail: None,
            actions: Vec::new(),
            repeats: 0,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn with_action(mut self, id: impl Into<String>, label: impl Into<String>) -> Self {
        self.actions.push(NoticeAction::new(id, label));
        self
    }
}

/// The notices currently showing, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NoticeCenter {
    notices: Vec<Notice>,
}

impl NoticeCenter {
    /// Raise a notice. If its condition is already showing, that notice takes the new wording and
    /// actions and is flashed rather than duplicated. Returns true when a new notice appeared.
    pub fn raise(&mut self, notice: Notice) -> bool {
        if let Some(existing) = self.notices.iter_mut().find(|n| n.key == notice.key) {
            let repeats = existing.repeats + 1;
            *existing = Notice { repeats, ..notice };
            false
        } else {
            self.notices.push(notice);
            true
        }
    }

    /// Raise a notice that is re-asserted every frame while its condition holds: it appears once
    /// and is kept current, but is not flashed each time.
    pub fn raise_quietly(&mut self, notice: Notice) {
        if let Some(existing) = self.notices.iter_mut().find(|n| n.key == notice.key) {
            let repeats = existing.repeats;
            *existing = Notice { repeats, ..notice };
        } else {
            self.notices.push(notice);
        }
    }

    /// Clear a condition (because it resolved, or the user dismissed it).
    pub fn dismiss(&mut self, key: &str) -> Option<Notice> {
        let index = self.notices.iter().position(|n| n.key == key)?;
        Some(self.notices.remove(index))
    }

    /// Clear every condition whose key starts with `prefix` (e.g. everything about one panel).
    pub fn dismiss_prefix(&mut self, prefix: &str) {
        self.notices.retain(|n| !n.key.starts_with(prefix));
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Notice> {
        self.notices.iter().find(|n| n.key == key)
    }

    #[must_use]
    pub fn all(&self) -> &[Notice] {
        &self.notices
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.notices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_condition_one_notice() {
        let mut center = NoticeCenter::default();
        assert!(center.raise(Notice::new("daemon", Severity::Error, "The terminal host stopped.")));
        assert!(
            !center.raise(
                Notice::new("daemon", Severity::Error, "The terminal host stopped.")
                    .with_action("restart", "Restart")
            )
        );
        assert_eq!(center.all().len(), 1);
        assert_eq!(center.all()[0].repeats, 1);
        assert_eq!(center.all()[0].actions.len(), 1);
        assert!(center.raise(Notice::new("other", Severity::Info, "x")));
        assert_eq!(center.all().len(), 2);
    }

    #[test]
    fn dismissal_by_key_and_prefix() {
        let mut center = NoticeCenter::default();
        center.raise(Notice::new("panel:1:exit", Severity::Warning, "a"));
        center.raise(Notice::new("panel:1:disk", Severity::Warning, "b"));
        center.raise(Notice::new("panel:2:exit", Severity::Warning, "c"));
        assert!(center.dismiss("panel:2:exit").is_some());
        assert!(center.dismiss("panel:2:exit").is_none());
        center.dismiss_prefix("panel:1:");
        assert!(center.is_empty());
    }
}
