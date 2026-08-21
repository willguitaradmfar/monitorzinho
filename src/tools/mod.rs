//! Tools: things monitorzinho *runs*, as opposed to the monitors, which only watch.
//!
//! A tool owns background threads and outlives the screen that started it — the user
//! adds an execution from the Ferramentas tab, it keeps working while they go look at
//! something else, and it stops when they remove it or the app exits.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub mod persist;
pub mod tunnel;

/// How one parameter is edited in the add-execution wizard.
#[derive(Clone)]
pub enum ParamKind {
    /// Free text — an address, a path, anything typed.
    Text,
    /// One of a fixed set, cycled with ←/→ rather than typed, so it can't be spelled
    /// wrong.
    Choice(&'static [&'static str]),
}

/// One thing a tool needs to know before it can start.
#[derive(Clone)]
pub struct ParamSpec {
    /// Stable lookup key, passed back to `Tool::start`.
    pub key: &'static str,
    pub label: &'static str,
    /// Shown under the field while it's focused: what to put here, and why.
    pub help: &'static str,
    pub default: &'static str,
    pub kind: ParamKind,
}

impl ParamSpec {
    pub fn text(
        key: &'static str,
        label: &'static str,
        default: &'static str,
        help: &'static str,
    ) -> Self {
        Self {
            key,
            label,
            help,
            default,
            kind: ParamKind::Text,
        }
    }

    pub fn choice(
        key: &'static str,
        label: &'static str,
        options: &'static [&'static str],
        help: &'static str,
    ) -> Self {
        Self {
            key,
            label,
            help,
            default: options[0],
            kind: ParamKind::Choice(options),
        }
    }
}

/// A tool the user can launch from the Ferramentas tab.
pub trait Tool: Send + Sync {
    /// Stable key used to match a saved execution back to its tool on the next run —
    /// never change one once shipped, same rule as `Monitor::id`.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// One line explaining what it does, shown next to the name while picking.
    fn description(&self) -> &'static str;
    fn params(&self) -> Vec<ParamSpec>;
    /// Validates `params` and starts the work, returning the live execution.
    ///
    /// Everything that can predictably fail — a malformed address, a port already in
    /// use, an unresolvable host — must fail *here*, while the user is still standing
    /// in front of the form and can fix it. A tool that returns `Ok` and then dies in a
    /// thread nobody is watching is the failure mode this signature exists to prevent.
    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String>;
    /// One-line description of what these parameters amount to, e.g.
    /// `TCP 127.0.0.1:8080 → 10.0.0.5:5432`. Used both for a running execution's row
    /// and for one that failed to start, so the two read identically in the list.
    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        self.params()
            .iter()
            .filter_map(|spec| params.get(spec.key).cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

pub fn all_tools() -> Vec<Box<dyn Tool>> {
    vec![Box::new(tunnel::TunnelTool)]
}

/// Live counters for an execution, written by its threads and read by the UI every
/// tick. Atomics rather than another mutex: the UI must never be able to block behind
/// a relay thread that's mid-copy.
#[derive(Default)]
pub struct Stats {
    /// Connections (or, for UDP, distinct client addresses) handled since the start.
    pub connections: AtomicU64,
    pub active: AtomicUsize,
    /// Bytes relayed towards the target, and back from it.
    pub to_target: AtomicU64,
    pub from_target: AtomicU64,
}

/// Which way a chunk of relayed data was going.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// From whoever connected to us, onward to the target.
    ToTarget,
    /// From the target, back to the client.
    FromTarget,
}

impl Direction {
    /// Arrow used in the monitor. Reads as "leaving us" / "coming back to us".
    pub fn arrow(self) -> &'static str {
        match self {
            Direction::ToTarget => "→",
            Direction::FromTarget => "←",
        }
    }
}

pub enum EventKind {
    /// Someone connected to the listening side.
    Opened {
        peer: String,
    },
    Closed {
        reason: String,
    },
    /// A chunk of relayed bytes. `preview` is what got kept (see `Event::preview`),
    /// `len` is how big the chunk really was.
    Data {
        dir: Direction,
        len: usize,
        preview: Vec<u8>,
    },
    /// Something the tool itself wants to say — started, stopped, limit hit.
    Note(String),
    Error(String),
}

pub struct Event {
    /// Monotonic, assigned on append and never reused. The monitor shows events newest
    /// first, so new ones appear at the *top* and push everything down; comparing
    /// against the newest sequence seen last frame is how it works out exactly how far
    /// to shift the viewport to keep the reader looking at the same line.
    pub seq: u64,
    /// Time since the execution started, not wall clock. What a relay log is read for
    /// is the gap between a request and its answer, and a relative clock needs no
    /// timezone plumbing to be exact.
    pub at: Duration,
    /// Which relayed connection this belongs to; 0 for events about the tool itself.
    pub conn: u64,
    pub kind: EventKind,
}

/// A bounded log of what an execution has seen. Old events fall off the front — a busy
/// tunnel would otherwise grow without limit — and the count of what was dropped is
/// kept so the monitor can say so instead of quietly showing a partial story.
pub struct EventLog {
    events: VecDeque<Event>,
    dropped: u64,
    capacity: usize,
    next_seq: u64,
}

impl EventLog {
    fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::new(),
            dropped: 0,
            capacity,
            next_seq: 0,
        }
    }

    fn push(&mut self, at: Duration, conn: u64, kind: EventKind) {
        if self.events.len() == self.capacity {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.next_seq += 1;
        self.events.push_back(Event {
            seq: self.next_seq,
            at,
            conn,
            kind,
        });
    }

    /// The concrete deque iterator rather than `impl Iterator`, so the monitor can
    /// `.rev()` it — it renders newest-first.
    pub fn iter(&self) -> std::collections::vec_deque::Iter<'_, Event> {
        self.events.iter()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// A poisoned log mutex means a relay thread panicked while appending. The log itself
/// is still perfectly readable, and losing the whole monitor over it would be a worse
/// outcome than showing what's there, so recover rather than propagate.
pub fn lock_log(log: &Mutex<EventLog>) -> MutexGuard<'_, EventLog> {
    log.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How many events one execution keeps. Roughly a screenful per 30 events, so this is
/// a deep-enough scrollback to find the request you were looking for.
const LOG_CAPACITY: usize = 4000;
/// How much of each relayed chunk is kept for the monitor. A tunnel can move megabytes
/// a second; the log is there to read protocol traffic, not to store payloads.
pub const PREVIEW_BYTES: usize = 2048;

/// The write end of an execution: what its background threads hold. Bundles the log,
/// the counters and the stop flag so a relay thread carries one clone instead of four.
#[derive(Clone)]
pub struct Recorder {
    log: Arc<Mutex<EventLog>>,
    stats: Arc<Stats>,
    shutdown: Arc<AtomicBool>,
    started: Instant,
}

impl Recorder {
    pub fn record(&self, conn: u64, kind: EventKind) {
        lock_log(&self.log).push(self.started.elapsed(), conn, kind);
    }

    /// Records a chunk of relayed data, keeping at most `PREVIEW_BYTES` of it and
    /// adding its size to the right counter.
    pub fn record_data(&self, conn: u64, dir: Direction, chunk: &[u8]) {
        let counter = match dir {
            Direction::ToTarget => &self.stats.to_target,
            Direction::FromTarget => &self.stats.from_target,
        };
        counter.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        self.record(
            conn,
            EventKind::Data {
                dir,
                len: chunk.len(),
                preview: chunk[..chunk.len().min(PREVIEW_BYTES)].to_vec(),
            },
        );
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Whether the execution has been removed and every thread should wind down.
    pub fn stopping(&self) -> bool {
        self.shutdown.load(Ordering::Relaxed)
    }
}

/// One running instance of a tool.
pub struct Execution {
    pub id: u64,
    pub tool: &'static str,
    /// One-line description of what this instance is doing, e.g.
    /// `TCP 127.0.0.1:8080 → 10.0.0.5:5432`.
    pub summary: String,
    pub started: Instant,
    pub log: Arc<Mutex<EventLog>>,
    pub stats: Arc<Stats>,
    shutdown: Arc<AtomicBool>,
    /// Set by a tool's main loop when it exits for any reason — so the list can tell
    /// "still working" from "died on its own", instead of showing a dead row as live.
    finished: Arc<AtomicBool>,
    /// What would recreate this execution on the next run. `None` for one nobody asked
    /// to persist; the tool itself never sets it, since being saved isn't its concern.
    spec: Option<persist::ExecutionSpec>,
}

impl Execution {
    /// Creates an execution and the `Recorder` its threads will hold. The tool calls
    /// this once it has everything that could fail (bound sockets, resolved addresses)
    /// already in hand.
    pub fn new(id: u64, tool: &'static str, summary: String) -> (Self, Recorder) {
        let log = Arc::new(Mutex::new(EventLog::new(LOG_CAPACITY)));
        let stats = Arc::new(Stats::default());
        let shutdown = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let recorder = Recorder {
            log: Arc::clone(&log),
            stats: Arc::clone(&stats),
            shutdown: Arc::clone(&shutdown),
            started,
        };
        let execution = Self {
            id,
            tool,
            summary,
            started,
            log,
            stats,
            shutdown,
            finished: Arc::new(AtomicBool::new(false)),
            spec: None,
        };
        (execution, recorder)
    }

    /// An execution that never got off the ground, carrying the reason in its log.
    ///
    /// Kept in the list rather than dropped, because the alternative is a restored
    /// configuration silently disappearing when its port happens to be busy — the user
    /// would have no way to know it had ever been there.
    pub fn failed(id: u64, tool: &'static str, summary: String, error: String) -> Self {
        let (execution, recorder) = Self::new(id, tool, summary);
        recorder.record(0, EventKind::Error(error));
        execution.finished.store(true, Ordering::Relaxed);
        execution
    }

    /// Records what would recreate this execution across restarts.
    pub fn with_spec(mut self, spec: persist::ExecutionSpec) -> Self {
        self.spec = Some(spec);
        self
    }

    pub fn spec(&self) -> Option<&persist::ExecutionSpec> {
        self.spec.as_ref()
    }

    /// A second flag the tool's main loop sets on its way out. Handed over separately
    /// from the `Recorder` because only that one loop should ever set it.
    pub fn finish_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.finished)
    }

    /// Asks every thread this execution owns to wind down. They notice within one
    /// socket poll interval; nothing here blocks waiting for them.
    pub fn stop(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
    }

    pub fn is_running(&self) -> bool {
        !self.finished.load(Ordering::Relaxed) && !self.shutdown.load(Ordering::Relaxed)
    }
}
