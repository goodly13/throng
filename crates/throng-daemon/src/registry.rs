//! Connected clients and the queues that feed them.
//!
//! Each client has one outgoing queue drained by its own writer thread, so a slow client never
//! stalls a PTY reader or another client. A client that falls more than [`MAX_QUEUED_BYTES`] behind
//! stops receiving output and is told to re-attach ([`ServerMsg::Resync`]) once it catches up:
//! output is never skipped silently, and memory stays bounded.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crossbeam_channel::{Receiver, Sender};
use interprocess::local_socket::Stream;
use parking_lot::Mutex;
use throng_core::ids::TerminalId;
use throng_protocol::ServerMsg;

/// How far behind a client may fall before its output is dropped in favour of a resync.
pub const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;

/// A connection's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

/// One client's outgoing side.
pub struct ClientQueue {
    stream: Arc<Stream>,
    tx: Sender<ServerMsg>,
    queued: AtomicUsize,
    resync: Mutex<HashSet<TerminalId>>,
    closed: AtomicBool,
}

impl ClientQueue {
    fn size_of(message: &ServerMsg) -> usize {
        match message {
            ServerMsg::Output { data, .. } => data.len(),
            ServerMsg::Reply { result: Ok(throng_protocol::Reply::Snapshot(s)), .. } => s.tail.len(),
            _ => 64,
        }
    }

    /// Tell the writer thread its connection is finished.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Called by the writer thread after a message has been written.
    pub fn written(&self, message: &ServerMsg) {
        self.queued.fetch_sub(Self::size_of(message), Ordering::AcqRel);
    }

    /// Terminals owed a resync, once the queue has drained.
    pub fn take_resyncs(&self) -> Vec<TerminalId> {
        if self.queued.load(Ordering::Acquire) > MAX_QUEUED_BYTES / 2 {
            return Vec::new();
        }
        self.resync.lock().drain().collect()
    }
}

/// Every connected client.
#[derive(Default)]
pub struct Registry {
    clients: Mutex<HashMap<ClientId, Arc<ClientQueue>>>,
    next: AtomicU64,
}

impl Registry {
    /// Register a client; returns its id, its queue, and the receiver its writer thread drains.
    pub fn add(&self, stream: Arc<Stream>) -> (ClientId, Arc<ClientQueue>, Receiver<ServerMsg>) {
        let id = ClientId(self.next.fetch_add(1, Ordering::Relaxed) + 1);
        let (tx, rx) = crossbeam_channel::unbounded();
        let queue = Arc::new(ClientQueue {
            stream,
            tx,
            queued: AtomicUsize::new(0),
            resync: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
        });
        self.clients.lock().insert(id, Arc::clone(&queue));
        (id, queue, rx)
    }

    pub fn remove(&self, id: ClientId) {
        self.clients.lock().remove(&id);
    }

    /// End every connection (daemon shutdown), so clients learn at once rather than on a timeout.
    pub fn shutdown_all(&self) {
        for queue in self.clients.lock().values() {
            queue.close();
            crate::endpoint::shutdown(&queue.stream);
        }
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.clients.lock().len()
    }

    /// Queue a control message (never dropped).
    pub fn send(&self, id: ClientId, message: ServerMsg) {
        if let Some(queue) = self.clients.lock().get(&id) {
            queue.queued.fetch_add(ClientQueue::size_of(&message), Ordering::AcqRel);
            let _ = queue.tx.send(message);
        }
    }

    /// Queue output, or mark the terminal for resync if the client is too far behind.
    pub fn send_output(&self, id: ClientId, terminal: TerminalId, offset: u64, data: &[u8]) {
        let clients = self.clients.lock();
        let Some(queue) = clients.get(&id) else { return };
        let pending_resync = queue.resync.lock().contains(&terminal);
        if pending_resync || queue.queued.load(Ordering::Acquire) + data.len() > MAX_QUEUED_BYTES {
            queue.resync.lock().insert(terminal);
            return;
        }
        queue.queued.fetch_add(data.len(), Ordering::AcqRel);
        let _ = queue.tx.send(ServerMsg::Output { terminal, offset, data: data.to_vec() });
    }
}
