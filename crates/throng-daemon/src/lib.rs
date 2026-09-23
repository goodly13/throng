//! throng's terminal daemon (Principle III): a detached process that owns every PTY, so terminals
//! keep running when the UI closes and reattach — with their scrollback — when it reopens.

pub mod client;
pub mod endpoint;
mod registry;
pub mod server;
mod session;
#[cfg(unix)]
pub mod unix;

pub use client::{Client, ClientEvent, ConnectError, RequestError};
pub use endpoint::Endpoint;
pub use server::{DaemonConfig, RunError, run};

/// This build's identity, exchanged in the handshake.
pub const BUILD: &str = env!("CARGO_PKG_VERSION");
