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
        #[cfg(windows)]
        if let Self::Pipe(pipe) = self {
            return win::connect(pipe);
        }
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
///
/// On Unix later reads fail too. On Windows only the read in progress is woken: a client ends its
/// own reads (see `Client`), and a daemon ends a client's with [`disconnect`].
pub fn shutdown(stream: &Stream) {
    #[cfg(unix)]
    match stream {
        Stream::UdSocket(socket) => {
            let _ = socket.inner().shutdown(std::net::Shutdown::Both);
        }
    }
    #[cfg(windows)]
    win::cancel(stream);
}

/// The daemon's end of a connection, once it has nothing more to send: the client reads what was
/// sent, then finds the connection closed, and the daemon's own reader is woken and reads no more.
pub fn disconnect(stream: &Stream) {
    #[cfg(unix)]
    shutdown(stream);
    #[cfg(windows)]
    win::disconnect(stream);
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod win {
    use std::io;
    use std::os::windows::io::{AsHandle, AsRawHandle};
    use std::time::Duration;

    use interprocess::local_socket::Stream;
    use interprocess::os::windows::named_pipe::{DuplexPipeStream, local_socket, pipe_mode};
    use windows_sys::Win32::Storage::FileSystem::FlushFileBuffers;
    use windows_sys::Win32::System::IO::CancelIoEx;
    use windows_sys::Win32::System::Pipes::DisconnectNamedPipe;

    /// How long a client waits while every instance of the pipe is taken.
    const BUSY_WAIT: Duration = Duration::from_secs(2);

    /// interprocess's own connect waits for ever while the pipe is busy (every instance held by a
    /// connection), so one connection the daemon never let go would hang every later client.
    pub fn connect(pipe: &str) -> io::Result<Stream> {
        let path = format!(r"\\.\pipe\{pipe}");
        let stream = DuplexPipeStream::<pipe_mode::Bytes>::connect_by_path_with_wait_mode(
            path.as_str(),
            interprocess::ConnectWaitMode::Timeout(BUSY_WAIT),
        )?;
        Ok(Stream::from(local_socket::Stream::from(stream)))
    }

    fn handle(stream: &Stream) -> windows_sys::Win32::Foundation::HANDLE {
        let Stream::NamedPipe(pipe) = stream;
        pipe.as_handle().as_raw_handle()
    }

    /// Wake whichever thread of this process is reading or writing the pipe.
    pub fn cancel(stream: &Stream) {
        // SAFETY: the handle is open while `stream` is borrowed; a null OVERLAPPED cancels every
        // operation this process has in progress on it, and none is an error.
        unsafe { CancelIoEx(handle(stream), std::ptr::null()) };
    }

    pub fn disconnect(stream: &Stream) {
        let handle = handle(stream);
        // SAFETY: the handle is open while `stream` is borrowed. Flushing waits until the client has
        // read everything sent (and fails at once if it has gone), since disconnecting discards
        // what it has not read; disconnecting a client end fails harmlessly.
        unsafe {
            FlushFileBuffers(handle);
            DisconnectNamedPipe(handle);
        }
        cancel(stream);
    }
}
