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
  rewrite bytes on the way through; a port scanner that asks each open port
  what it is rather than guessing from its number; a DNS investigation that
  sweeps everything a domain publishes, asks each authoritative server
  directly, and checks propagation across public resolvers; and a network
  sweep that finds what's alive on the LAN and hands the addresses straight to
  the port scanner.
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
- **Interfaces** — every network interface the kernel knows about, up or down:
  what kind it is (Wi-Fi, Ethernet, bridge, VPN, tunnel, loopback), whether it
  has a link, its addresses, and what's moving through it right now. Read from
  `/sys/class/net`, the same place `ip` reads, so a machine with a wired port,
  Wi-Fi, a WireGuard tunnel and three container bridges shows all seven rather
  than the one that happens to carry the default route.
- **System Info** — what this machine *is*: distribution and version, kernel,
  architecture, make and model, form factor, board, BIOS and its date, uptime,
  and whether it's running on top of a hypervisor or inside a container. Plus
  host, user, its address *and the card that address is on*, the interface
  list, gateway, DNS and a one-line summary of CPU, memory, disk and GPU.
  Hardware identity comes from the firmware's own DMI tables and the
  distribution from `/etc/os-release`, so it's what the machine says about
  itself rather than anything inferred.

Each panel has a shortcut key in its corner (`1`–`9`, then letters).
Pressing it fullscreens that panel with every row, not just the top ten the
compact grid shows. In a fullscreened table, typing searches immediately —
there's no search mode to enter first — and `Del` kills the selected process
(SIGKILL, with its children).

Pressing `Enter` on any row opens a **detail view** — everything that table's
monitor knows about that one subject, rebuilt every tick so it stays live:

- **a connection** — both endpoints with reverse-DNS and service names, the
  interface it's on, the owning process, throughput, and, for TCP, what the
  kernel knows about the path itself (RTT and its variance, congestion window,
  retransmits, MSS, and so on, read straight from `tcp_info`).
- **a port** — every address it's bound to and which cards those are, whether
  it's reachable from the network or only from this machine, the accept queue,
  the owning process, and who is connected to it right now, grouped by peer.
- **a process** — identity, state, parent, threads, open descriptors, OOM
  score, command and working directory, CPU and the full memory breakdown,
  its children, the sockets it holds, and live disk read/write sparklines.
- **an SSH session** — where the login came from (read off sshd's own socket
  where that's readable, and matched against the socket table where it isn't),
  its shell, and every process the session is running.
- **an interface** — kind, link state, MAC, MTU, negotiated speed, driver,
  Wi-Fi signal, every address, the routes through it, and packet/error/drop
  counters with live throughput.
- **a System Info row** — the rest of whatever the row summarises: the CPU line
  becomes topology, clocks, cache and feature flags; Memory becomes the full
  `/proc/meminfo` breakdown; DNS becomes every server, search domain and the
  stub's upstreams; Disk becomes every mount.

From there, `Ctrl+P` turns what's on screen into an execution. A connection
already names both ends and the protocol, which is the tunnel tool's entire
configuration, so it offers to relay to either end — listening on the same port
locally, so a client's config usually needs only its host changed to
`127.0.0.1`. Close the original connection, point the client at the tunnel, and
the same conversation now goes through something that writes it down. A
listening port offers a tunnel that records what it receives, on the first free
port above it; an interface offers a sweep of the network it's on.

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

`Enter` opens an execution's live log — every chunk in both directions, oldest
first with new traffic appending at the bottom, as text or hex (`Tab`), with
type-to-search, `↑`/`↓` to jump between matches, `Ctrl+F` to hide everything
that doesn't match, and `Ctrl+L` to clear the scrollback. Scrolling with the
arrows or `PgUp`/`PgDn` stops following the live edge and `End` resumes it; the
corner says which of the two it currently is. Paused, the view is anchored to
the event under its top line, so it holds still whether traffic arrives below
it or the oldest events fall off the buffer above it.

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
  the raw bytes, so they work on binary payloads too. When one fires, the log
  shows **both** versions of the chunk — what arrived, which rules fired, and
  what left — with the lines that actually changed marked, because a rule whose
  effect you can't see is a rule you can't tell is working. A chunk no rule
  touched is logged once, as before. Every rule you write is kept in a shared
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

#### Investigação DNS

Everything a domain publishes, and everything the servers behind it say when
asked directly. `dig` answers one question per invocation; this is the sweep
you'd otherwise run twenty times and correlate by hand.

The DNS client is written here rather than borrowed: `getaddrinfo` answers only
"what address is this name", and every other question needs queries built and
parsed on the wire — with EDNS0 to keep the DNSSEC records in the answer, and a
TCP fallback when one doesn't fit in a datagram.

One run covers:

- **The apex** — SOA, NS, A, AAAA, MX, TXT, CAA, DNSKEY, printed in `dig` shape.
- **The authoritative servers, asked one by one.** Each is queried directly for
  the zone's serial and the answers compared. A secondary that fell behind is
  invisible through a resolver and obvious here.
- **Propagation.** The same questions put to a list of resolvers you control —
  Google, Cloudflare, Quad9, OpenDNS and friends prefilled, editable, saved with
  the execution — with every answer compared against the majority. Mid-migration
  divergence is expected; the point is seeing how far along it is and which
  resolver is the odd one out.
- **Mail** — MX hosts resolved (a broken one shows up as such), SPF, DMARC,
  MTA-STS.
- **DNSSEC** at both ends of the delegation, which is how you catch the two
  half-signed states that break resolution.
- **Zone transfer.** AXFR against each nameserver, and it distinguishes a server
  that answered "no" from one that never answered at all — a firewall doesn't
  get credit for a policy decision it didn't make. A server that *does* hand the
  zone over is the finding, and its contents then feed everything below.
- **WHOIS**, following IANA → the TLD's registry → the registrar. The whole
  response is shown, minus the terms-of-use boilerplate, rather than the subset
  a hardcoded field list happens to know the name of.
- **Reverse DNS** for every address turned up along the way.

##### Finding subdomains

Two ways, and they're different in kind:

**Without guessing** reads names the zone itself publishes. A successful zone
transfer *is* the list. Failing that, a zone signed with NSEC proves a name
doesn't exist by naming the next one that does, so following that chain reads
the zone off the wire name by name — asked of the authoritative servers, since
a stub resolver answers `SERVFAIL` to NSEC and a server can be configured to
synthesise a minimal NSEC that says nothing. Every server is tried and the
longest chain wins, so the result doesn't depend on which one an NS record set
happened to list first. NSEC3 hashes those names and closes the door, and the
tool says so instead of pretending. What's left comes from the records
themselves: nameservers, mail exchangers, CNAME targets, the hosts an SPF record
authorises, where DMARC reports go.

**The common list** tries around seventy likely names. On a zone with a `*`
record every one of them "resolves", which would be seventy false positives —
so the wildcard is detected first, by asking for names that cannot exist, and
any candidate whose answer is only the wildcard's is dropped and named as such.
The row says `curinga *` when this is in play.

Every address found is offered to the port scanner: `Ctrl+P` in the log lists
them, and picking one creates a scan already filled in.

Like the port scanner, it runs on `Enter` and not before.

#### Scanner de rede

What's alive on the local network, with a MAC, a vendor and a name for each.

Without `CAP_NET_RAW` there's no ARP sweep and no raw ICMP, so it uses what a
normal user actually has, cheapest first:

- **The neighbour table.** Anything this machine has spoken to recently is
  already in `/proc/net/arp`, with its MAC. Free, and proof the host exists.
- **ICMP echo through an unprivileged datagram socket** — the mode `ping` uses
  when it isn't setuid, gated by `net.ipv4.ping_group_range`. Where the kernel
  declines, the sweep says so once and carries on.
- **TCP connect.** The point isn't finding a service: a *refused* connection is
  a reply, and a reply proves the host is there. A host that answers `RST` on
  every port is as discovered as one running a web server.

Networks come from the kernel's routing table, so a laptop on a VPN with
container bridges sweeps all of them rather than a guess at `192.168.x`, each
named with the interface it's reached through; a CIDR can be typed instead. MAC vendors are read from whatever OUI database the
system ships (`ieee-data`, `nmap`, `wireshark`) — a vendor table baked into the
binary would be wrong the month after it shipped.

`Ctrl+P` hands the results to the port scanner, one host or **all of them at
once**, which is the whole point of the pair: find what's out there, then look
at it properly.

## Keys

| Key | Where | What |
| --- | --- | --- |
| `Tab` / `Shift+Tab` | anywhere | next/previous tab |
| space | a tab | refresh now, without waiting for the tick |
| `1`–`9`, letters | Visão Geral, Processos | fullscreen that panel |
| `Enter` | a fullscreened table | open the row's detail view |
| `PgUp` / `PgDn` | any list or log | move ten rows / scroll fast, stopping at the ends |
| `←` / `→` | process tree | collapse/expand |
| `Del` | a fullscreened table | what it kills depends on the table — the confirmation says so before anything happens |
| any letter | a fullscreened table | search, live |
| `a` / `e` / `r` / `Del` | Ferramentas | add / edit / restart (or re-run) / remove an execution |
| `Enter` | Ferramentas | open that execution's live log — and run it, for an on-demand tool |
| `Tab`, `Ctrl+F` | an execution's log | hex view, matches-only filter |
| `Ctrl+L`, `End` | an execution's log | clear the scrollback, jump back to the live edge |
| `Ctrl+P` | a detail view | turn what's on screen into an execution — a tunnel to either end of a connection, a recording of a port, a sweep of an interface's network |
| `Ctrl+P` | an execution's log | turn what it found into new executions — one, or all of them |
| `Esc` | anywhere | back one level (clears a search first) |
| `Enter` / `Esc` | a confirmation | go through with it / leave it alone — no other key answers |
| `q` | a fullscreened chart or detail | close it and go back (in a table, `q` is search input like any other letter) |
| `Ctrl+C` twice | anywhere | quit — one press only arms it, so a stray `Ctrl+C` can't kill a session carrying live executions |

Quitting saves history cleanly before exit. Nothing but `Ctrl+C` twice closes the
app — a monitor left running for hours shouldn't die to a mistyped letter.

The footer of every screen lists only the keys that do something *there*: no
`←`/`→` over a table that isn't a tree, no `Del` over a table with nothing to
kill, no `Ctrl+P` on an execution that hasn't found anything yet. A hint for a
key that does nothing is worse than no hint — it sends you looking for a bug.

Anything irreversible stops and says what it is about to do first: which process
(and how many of its children) `Del` would SIGKILL, that killing the owner of a
port takes the port down with it, that disconnecting a session throws a person
off the machine mid-command, that removing an execution drops what's connected
through it and throws its log away, that forgetting a rewrite rule deletes it
from the shared history on disk. `Enter` goes ahead, `Esc` doesn't, and every
other key is ignored rather than taken as an answer.

## Architecture

Three small traits drive everything:

- `Monitor` — a metric tracked over time (sparkline + persisted history).
  Implement it in `src/monitor/<name>.rs` and add it to `all_monitors()` in
  `src/monitor/mod.rs`.
- `TableMonitor` — a ranked snapshot list, for data that doesn't fit a time
  series. Answer `detail()` and the row gets an `Enter` view; return `handoffs`
  from it and `Ctrl+P` turns what's on screen into a tool execution.
- `Tool` — something that runs: it declares the parameters the wizard should
  ask for, then starts threads and reports back through a shared event log and
  two columns of its execution's row. Say `on_demand()` and it starts nothing
  until the user opens it; return `handoffs()` and what it found becomes
  another tool's input. Implement it in `src/tools/<name>.rs` and add it to
  `all_tools()`.

There are no bindings crates for the Linux-specific parts: the socket tables
come from a hand-rolled netlink `SOCK_DIAG` client and from `/proc/net/{tcp,udp}`
parsed by hand, interfaces from `/sys/class/net`, names from `getnameinfo`
through NSS (on background threads, so a slow resolver can't stall the UI),
the relays wait on `poll(2)` directly, and DNS queries are built and parsed
byte by byte.

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
