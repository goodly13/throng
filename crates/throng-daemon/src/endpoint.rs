//! Where the daemon listens: a Unix socket in the instance's private runtime directory, or a
//! per-user, per-instance named pipe on Windows. Separate instances never share an endpoint, so a UI
//! can never adopt another instance's daemon.

use std::io;
use std::path::PathBuf;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{Listener, ListenerOptions, Name, Stream};
use throng_platform::dirs::AppDirs;

/// A daemon endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Socket(PathBuf),
    Pipe(String),
}

impl Endpoint {
    /// The endpoint for an instance.
    #[must_use]
    pub fn for_dirs(dirs: &AppDirs) -> Self {
        if cfg!(windows) { Self::Pipe(dirs.pipe_name()) } else { Self::Socket(dirs.daemon_socket()) }
    }

    fn name(&self) -> io::Result<Name<'_>> {
        match self {
            Self::Socket(path) => {
                use interprocess::local_socket::GenericFilePath;
                path.as_os_str().to_fs_name::<GenericFilePath>()
            }
            Self::Pipe(pipe) => {
                use interprocess::local_socket::GenericNamespaced;
                pipe.as_str().to_ns_name::<GenericNamespaced>()
            }
        }
    }

    /// Connect to a listening daemon.
    pub fn connect(&self) -> io::Result<Stream> {
        Stream::connect(self.name()?)
    }

    /// Listen. The caller must already hold the daemon lock, which is what makes removing a stale
    /// socket file left by a crashed daemon safe.
    pub fn listen(&self) -> io::Result<Listener> {
        if let Self::Socket(path) = self
            && path.exists()
            && self.connect().is_err()
        {
            let _ = std::fs::remove_file(path);
        }
        // Accept is non-blocking so the accept loop can notice a stop request on its own. Waking a
        // blocked accept by connecting to ourselves fails once the socket file is gone, and a daemon
        // then hangs with its terminals alive and nobody able to reach them.
        ListenerOptions::new()
            .name(self.name()?)
            .nonblocking(interprocess::local_socket::ListenerNonblockingMode::Accept)
            .create_sync()
    }

    /// Whether the endpoint can still be found by clients. A deleted socket file cannot.
    #[must_use]
    pub fn reachable(&self) -> bool {
        match self {
            Self::Socket(path) => path.exists(),
            Self::Pipe(_) => true,
        }
    }

    /// Remove the socket file after the listener is gone.
    pub fn cleanup(&self) {
        if let Self::Socket(path) = self {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// End a connection from any thread, waking a reader blocked on it. A connection is shared (reader
/// thread, writer thread, owner), so dropping one handle would close nothing.
pub fn shutdown(stream: &Stream) {
    #[cfg(unix)]
    match stream {
        Stream::UdSocket(socket) => {
            let _ = socket.inner().shutdown(std::net::Shutdown::Both);
        }
    }
    #[cfg(windows)]
    {
        // Named pipes disconnect when their last handle closes; the Windows port revisits this.
        let _ = stream;
    }
}
