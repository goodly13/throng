//! Stable identities. Every id is a v4 UUID wrapped in its own type so a panel id can never be
//! passed where a project id is expected.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// A fresh random identity.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

id_type!(
    /// A project — the root context everything else is scoped beneath (Principle I).
    ProjectId
);
id_type!(
    /// A tab in a project's workspace pane.
    TabId
);
id_type!(
    /// A panel: the atomic, draggable content unit inside a tab.
    PanelId
);
id_type!(
    /// A terminal session owned by the daemon. It is the id of the panel that asked for it, so a
    /// reattach is decided by the caller's stated identity and never by comparing launch details.
    TerminalId
);

impl From<PanelId> for TerminalId {
    fn from(panel: PanelId) -> Self {
        Self(panel.0)
    }
}

impl From<TerminalId> for PanelId {
    fn from(terminal: TerminalId) -> Self {
        Self(terminal.0)
    }
}
