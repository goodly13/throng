//! The wire protocol between the throng UI and its terminal daemon.
//!
//! One connection per client carries one ordered stream of messages in each direction, so a write,
//! a resize and a detach can never overtake each other (one connection per call can deliver
//! "throng" as "hrongt", and let a detach race a queued attach).
//!
//! Frames are a little-endian `u32` length followed by a postcard-encoded message. A frame larger
//! than [`MAX_FRAME`] is a protocol error that closes that one connection — never the daemon — and
//! large writes are chunked by the sender.
//!
//! Terminal output carries the byte offset of its first byte in the session's lifetime stream, and
//! an attach snapshot carries the offset where it ends. A client drops any output below the snapshot
//! end, so no byte is painted twice however attach and output interleave.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use throng_core::ids::{ProjectId, TerminalId};
use throng_core::terminal::ExitStatus;

/// Bumped whenever a message shape changes. [`ClientMsg::Hello`] and [`ServerMsg::Welcome`] are the
/// first variants of their enums and must keep their shape forever, so any two versions can at least
/// tell each other apart.
pub const PROTOCOL_VERSION: u32 = 1;

/// The largest frame either side accepts.
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Writes larger than this are split by the sender.
pub const WRITE_CHUNK: usize = 64 * 1024;

/// A request id that asks for no reply.
pub const NO_REPLY: u64 = 0;

/// The refusal an attach gets when the daemon holds no session with that id; the client then
/// starts one. Shared so the two sides cannot drift apart.
pub const NO_SUCH_TERMINAL: &str = "No such terminal.";

/// Client → daemon.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMsg {
    /// First message on every connection.
    Hello { protocol: u32, build: String, pid: u32 },
    /// A request; the daemon answers with [`ServerMsg::Reply`] carrying the same id. Id
    /// [`NO_REPLY`] asks for no answer (keystrokes, resizes); failures are then only logged.
    Request { id: u64, request: Request },
}

/// Everything a client can ask.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Start a terminal and attach to it. Fails if a live session with that id exists — reattaching
    /// is an explicit [`Request::Attach`], so identity comes from the caller's intent.
    Spawn(SpawnSpec),
    /// Attach to an existing session (live or exited-but-unread) and receive its output.
    Attach {
        terminal: TerminalId,
        cols: u16,
        rows: u16,
    },
    /// Stop receiving a session's output. Never ends the session.
    Detach {
        terminal: TerminalId,
    },
    /// Input for the shell.
    Write {
        terminal: TerminalId,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// This client's view size. The PTY takes the smallest size across attached views.
    Resize {
        terminal: TerminalId,
        cols: u16,
        rows: u16,
    },
    /// End a session at the user's request (its whole process group).
    Kill {
        terminal: TerminalId,
    },
    /// Drop an exited session's record once the client has shown its exit.
    Forget {
        terminal: TerminalId,
    },
    /// Every session this daemon holds.
    List,
    /// Close each listed session whose shell is idle (no foreground command); busy ones keep running
    /// (Principle III). Replies with the ids that were closed.
    CloseIdle {
        terminals: Vec<TerminalId>,
    },
    /// End the daemon; `kill_all` ends every session first, otherwise it refuses while any session is
    /// live.
    Shutdown {
        kill_all: bool,
    },
    Ping,
}

/// How to start a terminal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnSpec {
    pub terminal: TerminalId,
    pub project: ProjectId,
    /// For listings and diagnostics ("Web › Panel 2").
    pub label: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The complete environment, taken from the UI at spawn time so a shell never inherits the
    /// daemon's stale one. `THRONG_*` variables are already removed.
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    /// Written once after the shell's first output; never on reattach.
    pub startup_command: Option<String>,
}

/// Daemon → client.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        protocol: u32,
        build: String,
        pid: u32,
    },
    Reply {
        id: u64,
        result: Result<Reply, String>,
    },
    /// Output for an attached session. `offset` is the position of `data[0]` in the session's
    /// lifetime output.
    Output {
        terminal: TerminalId,
        offset: u64,
        #[serde(with = "serde_bytes")]
        data: Vec<u8>,
    },
    /// An attached session ended. Output that preceded the exit has already been sent.
    Exited {
        terminal: TerminalId,
        status: ExitStatus,
    },
    /// The client fell too far behind and output for this session was dropped: re-attach for a
    /// fresh snapshot. Nothing is ever silently skipped.
    Resync {
        terminal: TerminalId,
    },
}

/// Successful answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Ok,
    /// Spawn and Attach both answer with a snapshot.
    Snapshot(Snapshot),
    Terminals(Vec<TerminalInfo>),
    Closed(Vec<TerminalId>),
    Pong,
}

/// What a client needs to rebuild a view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub terminal: TerminalId,
    /// The retained tail of the session's output, starting at a line boundary.
    #[serde(with = "serde_bytes")]
    pub tail: Vec<u8>,
    /// Offset just past the tail's last byte. Output below this is already in `tail`.
    pub end_offset: u64,
    /// Present when the session ended while nobody was watching.
    pub exited: Option<ExitStatus>,
    /// The program was on the alternate screen; the daemon nudges it to repaint.
    pub alt_screen: bool,
}

/// A session, as listed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub terminal: TerminalId,
    pub project: ProjectId,
    pub label: String,
    pub pid: Option<u32>,
    /// A command other than the shell holds the foreground. Unknown is reported as busy.
    pub busy: bool,
    pub exited: Option<ExitStatus>,
    pub views: usize,
}

/// A framing or decoding failure.
#[derive(Debug, Error)]
pub enum FrameError {
    #[error("connection closed")]
    Closed,
    #[error("frame of {0} bytes exceeds the {MAX_FRAME}-byte limit")]
    TooLarge(usize),
    #[error("malformed message: {0}")]
    Decode(#[from] postcard::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Write one message as a frame.
pub fn write_frame<T: Serialize>(writer: &mut impl Write, message: &T) -> Result<(), FrameError> {
    let payload = postcard::to_stdvec(message)?;
    if payload.len() > MAX_FRAME {
        return Err(FrameError::TooLarge(payload.len()));
    }
    let len = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge(payload.len()))?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(&payload);
    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Read one frame and decode it.
pub fn read_frame<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> Result<T, FrameError> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).map_err(|e| {
        if e.kind() == io::ErrorKind::UnexpectedEof { FrameError::Closed } else { FrameError::Io(e) }
    })?;
    Ok(postcard::from_bytes(&payload)?)
}

/// Split `data` into [`Request::Write`]s of at most [`WRITE_CHUNK`] bytes, in order.
#[must_use]
pub fn chunked_writes(terminal: TerminalId, data: &[u8]) -> Vec<Request> {
    data.chunks(WRITE_CHUNK).map(|chunk| Request::Write { terminal, data: chunk.to_vec() }).collect()
}

/// The part of an output chunk a client has not seen, given the offset its snapshot ended at.
#[must_use]
pub fn unseen(offset: u64, data: &[u8], seen_until: u64) -> &[u8] {
    let end = offset + data.len() as u64;
    if end <= seen_until {
        &[]
    } else if offset >= seen_until {
        data
    } else {
        &data[usize::try_from(seen_until - offset).unwrap_or(data.len())..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let msg =
            ServerMsg::Output { terminal: TerminalId::new(), offset: 42, data: b"\x1b[31mhi\r\n".to_vec() };
        let mut buf = Vec::new();
        write_frame(&mut buf, &msg).unwrap();
        let back: ServerMsg = read_frame(&mut buf.as_slice()).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn oversized_and_truncated_frames_are_errors() {
        let mut huge = Vec::new();
        huge.extend_from_slice(&((MAX_FRAME as u32) + 1).to_le_bytes());
        assert!(matches!(read_frame::<ServerMsg>(&mut huge.as_slice()), Err(FrameError::TooLarge(_))));
        assert!(matches!(read_frame::<ServerMsg>(&mut [1u8, 0].as_slice()), Err(FrameError::Closed)));
        assert!(matches!(read_frame::<ServerMsg>(&mut [].as_slice()), Err(FrameError::Closed)));
        let garbage = [3u8, 0, 0, 0, 0xFF, 0xFF, 0xFF];
        assert!(matches!(read_frame::<ClientMsg>(&mut garbage.as_slice()), Err(FrameError::Decode(_))));
    }

    #[test]
    fn hello_and_welcome_stay_first() {
        // The handshake must decode across versions: variant index 0 on both enums.
        let hello =
            postcard::to_stdvec(&ClientMsg::Hello { protocol: 1, build: String::new(), pid: 0 }).unwrap();
        assert_eq!(hello[0], 0);
        let welcome =
            postcard::to_stdvec(&ServerMsg::Welcome { protocol: 1, build: String::new(), pid: 0 }).unwrap();
        assert_eq!(welcome[0], 0);
    }

    #[test]
    fn large_writes_are_chunked_in_order() {
        let terminal = TerminalId::new();
        let data: Vec<u8> = (0..(WRITE_CHUNK * 2 + 10)).map(|i| (i % 251) as u8).collect();
        let chunks = chunked_writes(terminal, &data);
        assert_eq!(chunks.len(), 3);
        let mut joined = Vec::new();
        for chunk in chunks {
            let Request::Write { data, .. } = chunk else { panic!() };
            assert!(data.len() <= WRITE_CHUNK);
            joined.extend(data);
        }
        assert_eq!(joined, data);
    }

    #[test]
    fn unseen_drops_exactly_the_overlap() {
        assert_eq!(unseen(0, b"abc", 0), b"abc");
        assert_eq!(unseen(0, b"abc", 3), b"");
        assert_eq!(unseen(0, b"abc", 2), b"c");
        assert_eq!(unseen(10, b"abc", 5), b"abc");
        assert_eq!(unseen(10, b"abc", 99), b"");
    }
}
