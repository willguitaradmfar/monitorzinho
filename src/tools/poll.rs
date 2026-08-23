//! Just enough of `poll(2)` for the relays: wait until a descriptor has something to
//! say, with a timeout short enough that a stopped execution notices.
//!
//! The alternative — a non-blocking socket and a `sleep` between tries — costs the
//! sleep interval on every connection and every message, which on a loopback tunnel is
//! most of the latency the user would see.

use std::ffi::c_ulong;
use std::io::ErrorKind;
use std::os::fd::RawFd;

/// Data (or an EOF) is available to read.
pub const IN: i16 = 0x001;
/// The descriptor will accept a write.
pub const OUT: i16 = 0x004;
const ERR: i16 = 0x008;
const HUP: i16 = 0x010;
/// What counts as "go read this": a hangup wants a `read` too, since that's the call
/// that turns it into the EOF the relay acts on.
pub const READABLE: i16 = IN | ERR | HUP;

/// How long a relay waits before re-checking its stop flag.
pub const TIMEOUT_MS: i32 = 200;

#[repr(C)]
pub struct Fd {
    fd: i32,
    events: i16,
    revents: i16,
}

impl Fd {
    pub fn new(fd: RawFd, events: i16) -> Self {
        Self {
            fd,
            events,
            revents: 0,
        }
    }

    /// What was asked for, so a caller can tell "not ready" from "never polled for".
    pub fn watching(&self, events: i16) -> bool {
        self.events & events != 0
    }

    pub fn ready(&self, events: i16) -> bool {
        self.revents & events != 0
    }
}

unsafe extern "C" {
    fn poll(fds: *mut Fd, nfds: c_ulong, timeout: i32) -> i32;
}

/// Waits for any of `fds`. `Ok(false)` means the timeout expired with nothing ready; an
/// interrupted call reports the same, since the caller's next move is identical.
pub fn wait(fds: &mut [Fd], timeout_ms: i32) -> std::io::Result<bool> {
    let ready = unsafe { poll(fds.as_mut_ptr(), fds.len() as c_ulong, timeout_ms) };
    if ready >= 0 {
        return Ok(ready > 0);
    }
    let err = std::io::Error::last_os_error();
    match err.kind() {
        ErrorKind::Interrupted => Ok(false),
        _ => Err(err),
    }
}

/// Waits for a descriptor to have either data or a queued error. An ICMP socket doing
/// a traceroute needs both: the reply from the destination arrives the ordinary way,
/// and every router along the path answers onto the error queue, which announces itself
/// as `POLLERR`.
pub fn readable_or_error(fd: RawFd, timeout_ms: i32) -> bool {
    let mut fds = [Fd::new(fd, IN | ERR)];
    wait(&mut fds, timeout_ms).unwrap_or(false)
}

/// Waits for one descriptor to become readable, reporting a failure the same as a
/// timeout — the callers here loop either way, and a genuinely broken descriptor shows
/// up on the next `accept` or `read` with a better message than `poll` could give.
pub fn readable(fd: RawFd, timeout_ms: i32) -> bool {
    let mut fds = [Fd::new(fd, IN)];
    wait(&mut fds, timeout_ms).unwrap_or(false)
}
