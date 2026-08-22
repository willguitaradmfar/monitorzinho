# monitorzinho

A lightweight terminal system monitor, written in Rust.

- **Charts** for CPU, memory, disk (occupancy + read/write throughput),
  network (down/up), and GPU (NVIDIA, auto-detected) — last value, peak, and a
  recent-history sparkline for each.
- **Tables** for processes (as a tree), listening ports, live connections, SSH
  sessions, and system info — any of them fullscreenable, searchable as you
  type, with a per-connection detail view.
- **Tools** that don't just watch but *run*: a recording TCP/UDP tunnel that
  shows you the payload of a connection, speaks TLS to the target, and can
  rewrite bytes on the way through; and a port scanner that asks each open
  port what it is rather than guessing from its number.
- History is persisted to disk and restored on restart, so the charts aren't
  empty on launch. Tools come back running too.
- Small, fast, no garbage collector: a single ~5 MB binary with no runtime to
  install, starting instantly.
- Built to grow: adding a metric, a table, or a tool is implementing one trait
  and registering it — see [Architecture](#architecture).

The interface is in Portuguese; this README is not.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/willguitaradmfar/monitorzinho/main/install.sh | sh
```

Downloads the latest release binary to `~/.local/bin/monitorzinho` (Linux
x86_64 only for now). Then just run:

```sh
monitorzinho
```

### Build from source

Requires a stable Rust toolchain ([rustup.rs](https://rustup.rs)).

```sh
cargo build --release
./target/release/monitorzinho
```

or just `cargo run --release` during development.

## Tabs

`Tab` / `Shift+Tab` cycle between three tabs. Everything samples every 2
seconds; spacebar forces an immediate refresh, like `top`'s.

### Visão Geral — the charts

- **System** — CPU usage (with logical core count) and memory usage
  (+ used/total in GB).
- **Disk** — occupancy of the root filesystem as a compact numeric line
  (it changes too slowly for a chart to be useful), plus read/write throughput
  charts.
- **Network** — download/upload throughput.
- **GPU** — utilization and VRAM usage, only shown on machines with a working
  NVIDIA driver (via [NVML](https://developer.nvidia.com/nvidia-management-library-nvml),
  dynamically loaded — the binary runs fine without one).

Panels are grouped and color-coded by category, and turn yellow/red as a
metric approaches its natural limit (e.g. memory nearing 100%).

### Processos — the tables

- **Ports** — everything listening, TCP and UDP together, newest first.
- **Connections** — established sockets with per-connection rates and age,
  refreshed live while fullscreened.
- **Top CPU** / **Top Memory** — the heaviest processes by each metric, shown
  as a tree: parents expand to their children with `←`/`→`.
- **SSH Sessions** — who is logged in over SSH, from where, on which TTY,
  since when, and what they're running.
- **System Info** — host, user, gateway, DNS, and a one-line summary of CPU,
  memory, disk and GPU: the facts that don't belong on a chart.

Each panel has a shortcut key in its corner (`1`–`9`, then letters).
Pressing it fullscreens that panel with every row, not just the top ten the
compact grid shows. In a fullscreened table, typing searches immediately —
there's no search mode to enter first — and `Del` kills the selected process
(SIGKILL, with its children).

Pressing `Enter` on a connection opens a **detail view**: both endpoints with
reverse-DNS names and service names, the owning process, throughput, and — for
TCP — what the kernel knows about the path itself (RTT and its variance,
congestion window, retransmits, MSS, and so on, read straight from `tcp_info`).

### Ferramentas — the tools

Things monitorzinho runs, rather than watches. An execution keeps working while
you look at something else, and comes back the next time you launch the app —
or, where the tool says so, sits idle until you ask it for something.

`a` walks you through adding one: pick the tool, fill in what it needs, look at
it once, and confirm. `e` reopens that form on an execution that already
exists, `r` restarts one (or, for a tool that works on demand, runs it again),
`Del` stops and forgets it.

Two columns of each row are the tool's own — what it has to report, and a
summary beside it — so a tunnel's byte counters and a scan's open-port tally
share a place without pretending to be the same thing. The last column is where
the row stands: `pronta` for an on-demand execution nobody has asked anything
of yet, `rodando`, `concluída` once there's a result to read, `parada` for one
that was stopped or never started.

`Enter` opens an execution's live log — every chunk in both directions, newest
at the top, as text or hex (`Tab`), with type-to-search, `↑`/`↓` to jump
between matches, and `Ctrl+F` to hide everything that doesn't match.

#### Túnel TCP/UDP

Listens on a local port and forwards everything to another host:port,
recording both directions on the way through.

The point isn't the forwarding — `socat` does that — it's the recording.
Pointing a client at the tunnel instead of straight at the server is the one
way to read a connection's actual payload without `CAP_NET_RAW`, because the
bytes pass through this process rather than past it.

Three things it does beyond relaying:

- **TLS to the target.** The client still speaks plain TCP to the tunnel while
  the tunnel does the handshake with the real server, so what gets logged is
  the *decrypted* conversation with a server that would otherwise only ever
  show ciphertext. Certificates are checked against the system trust store plus
  the bundled Mozilla roots, or not checked at all if you say so — a debugging
  escape hatch for self-signed and internal CAs. Fill in a certificate name
  when the target is an IP.
- **regex/replace.** A list of rules applied to what the client sends, before
  it leaves for the target — the reason it exists is a header that has to
  change for the far side to accept the connection at all, like
  `Host: note:8080` becoming `Host: google.com.br`. Rules run in order, over
  the raw bytes, so they work on binary payloads too, and the log shows a note
  naming whichever ones fired. Every rule you write is kept in a shared
  history, per machine and not per execution: removing the execution leaves it
  there to pick again.
- **UDP.** Same idea, one flow per source address.

#### Scanner de portas

A TCP connect scan of a host, and as much as can be said about each open port.

It connects for real rather than sending half-open SYN probes, because raw
sockets need `CAP_NET_RAW` and this runs as a normal user. That's the honest
trade — it shows up in the target's logs, and it says nothing about UDP. What
it buys is that every open port is a socket already in hand, so the scan can
go on to *ask*: read whatever the service announces, try a TLS handshake, try
an HTTP request. So an open port reads as
`8080/tcp aberta · 0.3 ms · HTTP/1.1 200 OK · Server: MinIO` rather than as a
number and a guess from `/etc/services`.

Ports come from a preset (~90 common ones, `1-1024`, `1-10000`, or all 65535)
or from a spec typed the way nmap takes it: `22,80,443,8000-8100`. Concurrency
and per-port timeout are yours to set; the whole 65535 against localhost takes
a few seconds.

**Nothing runs until you open it.** A scan is a burst of work with an answer at
the end, not something to keep running in the background, so creating one —
or having one restored on launch — starts nothing at all. `Enter` runs it,
and once there's a result `Enter` just shows it again; `r` is how you ask for a
fresh scan. The list carries how many ports were open and what's listening,
from the moment there's a first result.

## Keys

| Key | Where | What |
| --- | --- | --- |
| `Tab` / `Shift+Tab` | anywhere | next/previous tab |
| space | a tab | refresh now, without waiting for the tick |
| `1`–`9`, letters | Visão Geral, Processos | fullscreen that panel |
| `Enter` | a fullscreened table | open the row's detail view, where there is one |
| `←` / `→` | process tree | collapse/expand |
| `Del` | a fullscreened table | kill the selected process (SIGKILL, with children) |
| any letter | a fullscreened table | search, live |
| `a` / `e` / `r` / `Del` | Ferramentas | add / edit / restart (or re-run) / remove an execution |
| `Enter` | Ferramentas | open that execution's live log — and run it, for an on-demand tool |
| `Tab`, `Ctrl+F` | an execution's log | hex view, matches-only filter |
| `Esc` | anywhere | back one level (clears a search first) |
| `q` | a tab | quit |
| `Ctrl+C` twice | anywhere | quit — one press only arms it, so a stray `Ctrl+C` can't kill a session carrying live executions |

Quitting saves history cleanly before exit.

## Architecture

Three small traits drive everything:

- `Monitor` — a metric tracked over time (sparkline + persisted history).
  Implement it in `src/monitor/<name>.rs` and add it to `all_monitors()` in
  `src/monitor/mod.rs`.
- `TableMonitor` — a ranked snapshot list, for data that doesn't fit a time
  series. Optionally answers `detail()` to get an `Enter` view.
- `Tool` — something that runs: it declares the parameters the wizard should
  ask for, then starts threads and reports back through a shared event log and
  two columns of its execution's row. Say `on_demand()` and it starts nothing
  until the user opens it. Implement it in `src/tools/<name>.rs` and add it to
  `all_tools()`.

There are no bindings crates for the Linux-specific parts: the socket tables
come from a hand-rolled netlink `SOCK_DIAG` client, names from `getnameinfo`
through NSS (on background threads, so a slow resolver can't stall the UI),
and the relays wait on `poll(2)` directly.

### Files on disk

Under `~/.local/share/monitorzinho/` (or your platform's equivalent data
directory):

| File | What |
| --- | --- |
| `history.json` | chart history, so the sparklines survive a restart |
| `tools.json` | the executions to bring back on launch, and their parameters |
| `rewrites.json` | every rewrite rule ever written, offered as suggestions |

## License

MIT — see [LICENSE](LICENSE).
