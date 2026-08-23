//! The network namespaces this machine has besides its own, and how to read the sockets
//! inside them.
//!
//! Every socket panel here was blind to containers, and confidently so: the netlink
//! `SOCK_DIAG` dump answers for the namespace it is asked from, which is the host's, so
//! a machine running five containers that talk to each other all day showed three SSH
//! connections and nothing else. That is worse than showing nothing — it looks like an
//! answer.
//!
//! Reading another namespace's sockets does not need `setns` and does not need root:
//! `/proc/<pid>/net/tcp` is the socket table *of that pid's network namespace*, and a
//! process we may read is a namespace we may read. With rootless containers that's every
//! container on the machine; with root-owned ones it takes running as root, and the
//! panel says how many namespaces it could not open rather than pretending they aren't
//! there.
//!
//! What is lost compared to netlink is the per-socket byte counters, which live in
//! `tcp_info` and are not in `/proc`. Endpoints, state, queues and owner are all there,
//! which is what the question "who is talking to whom" is made of.

use std::collections::HashMap;
use std::fs;

/// A namespace, named as well as we can name it from `/proc` alone.
pub(super) struct Namespace {
    /// Inode of `/proc/<pid>/ns/net` — the kernel's identity for the namespace, and
    /// what tells two containers apart when they run the same image.
    pub id: u64,
    /// What to call it in a table: the container's name where a label carries one, the
    /// short container id where it doesn't, and the main process' name as a last
    /// resort. Never empty.
    pub label: String,
    /// A live pid inside it, to read `/proc/<pid>/net/*` through.
    pub pid: u32,
}

/// How many `/proc` entries to look at. Enough for any machine's worth of containers
/// several times over, and a bound on a walk that happens every tick.
const MAX_PIDS: usize = 8192;

/// Every network namespace on this machine except our own, with one readable pid each.
///
/// Also returns how many namespaces exist but could not be opened — root-owned
/// containers seen from a normal user — because a count of what is missing is the
/// difference between an incomplete answer and a wrong one.
pub(super) fn namespaces() -> (Vec<Namespace>, usize) {
    let Ok(own) = fs::read_link("/proc/self/ns/net") else {
        return (Vec::new(), 0);
    };
    let Ok(entries) = fs::read_dir("/proc") else {
        return (Vec::new(), 0);
    };

    let mut found: HashMap<u64, Namespace> = HashMap::new();
    let mut unreadable: Vec<String> = Vec::new();

    for entry in entries.flatten().take(MAX_PIDS) {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let link = format!("/proc/{pid}/ns/net");
        let Ok(target) = fs::read_link(&link) else {
            // Someone else's process — root's, most likely, which on a machine running
            // ordinary Docker is every container. We can't read its namespace, and a
            // count of what was missed is the difference between an incomplete answer
            // and a wrong one.
            if let Some(id) = unreadable_container(pid)
                && !unreadable.contains(&id)
            {
                unreadable.push(id);
            }
            continue;
        };
        if target == own {
            continue;
        }
        let Some(id) = inode_of(&target) else {
            continue;
        };
        // Tried rather than asked about: the file exists for every process, and
        // whether it opens is the whole question. A namespace we can name but not read
        // would be a row that is always empty.
        if fs::read_to_string(format!("/proc/{pid}/net/tcp")).is_err() {
            continue;
        }
        found.entry(id).or_insert_with(|| Namespace {
            id,
            label: label_for(pid),
            pid,
        });
    }

    let mut list: Vec<Namespace> = found.into_values().collect();
    // Stable order, so rows don't dance between ticks.
    list.sort_by(|a, b| a.label.cmp(&b.label).then(a.id.cmp(&b.id)));
    (list, unreadable.len())
}

/// `net:[4026532448]` → `4026532448`.
fn inode_of(target: &std::path::Path) -> Option<u64> {
    target
        .to_str()?
        .strip_prefix("net:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

/// The container a process we can't read belongs to, if it belongs to one.
///
/// Its namespace link is closed to us — that is what "can't read" means here — so the
/// namespace's own inode is out of reach and the container id stands in as the
/// identity. Counting cgroup paths instead would count a runtime's helper processes as
/// containers of their own; counting ids counts containers.
fn unreadable_container(pid: u32) -> Option<String> {
    container_id(pid)
}

/// The best name available for the namespace a pid lives in.
///
/// In order of how much it tells a person: the container's own name as `docker ps`
/// prints it, read from the runtime's state file on disk; the short container id, which
/// is what `docker ps` prints in its other column and so is still directly comparable;
/// the container's hostname; and finally the name of the process itself.
fn label_for(pid: u32) -> String {
    if let Some(id) = container_id(pid) {
        if let Some(name) = container_name(&id) {
            return name;
        }
        // Twelve characters is what `docker ps` shows, and what a person compares against.
        return id.chars().take(12).collect();
    }
    if let Ok(hostname) = fs::read_to_string(format!("/proc/{pid}/root/etc/hostname"))
        && !hostname.trim().is_empty()
    {
        return hostname.trim().to_string();
    }
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .ok()
        .map(|comm| comm.trim().to_string())
        .filter(|comm| !comm.is_empty())
        .unwrap_or_else(|| format!("pid {pid}"))
}

/// The full container id out of the cgroup path. Runtimes write it in a handful of
/// shapes — `docker-<id>.scope`, `/docker/<id>`, `libpod-<id>` — and what they have in
/// common is that the last path element carries the identity.
fn container_id(pid: u32) -> Option<String> {
    let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = cgroup.lines().next()?.rsplit(':').next()?;
    let last = path.rsplit('/').next()?;
    let id = last
        .trim_end_matches(".scope")
        .trim_start_matches("docker-")
        .trim_start_matches("libpod-")
        .trim_start_matches("crio-");
    (id.len() >= 12 && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| id.to_string())
}

/// The name the container was given, from the runtime's own state file.
///
/// Docker keeps it in `config.v2.json` beside the container's other state, readable by
/// whoever runs the daemon — the user for a rootless install, root for the ordinary
/// one, which is the same permission that decides whether its namespace is readable at
/// all. No daemon socket, no HTTP, no API version to keep up with: the file is either
/// there and ours to read, or the short id stands in.
fn container_name(id: &str) -> Option<String> {
    let mut roots = vec![std::path::PathBuf::from("/var/lib/docker/containers")];
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(std::path::PathBuf::from(data).join("docker/containers"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(std::path::PathBuf::from(home).join(".local/share/docker/containers"));
    }
    for root in roots {
        let Ok(text) = fs::read_to_string(root.join(id).join("config.v2.json")) else {
            continue;
        };
        let Ok(config) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(name) = config.get("Name").and_then(|name| name.as_str()) {
            // Stored with a leading slash, which nothing else ever shows.
            let name = name.trim_start_matches('/').trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Where to read one namespace's socket tables from.
pub(super) fn socket_tables(pid: u32) -> Vec<(&'static str, &'static str, String)> {
    [
        ("TCP", "IPv4", "tcp"),
        ("TCP", "IPv6", "tcp6"),
        ("UDP", "IPv4", "udp"),
        ("UDP", "IPv6", "udp6"),
    ]
    .into_iter()
    .map(|(proto, family, file)| (proto, family, format!("/proc/{pid}/net/{file}")))
    .collect()
}
