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

Downloads the latest release binary to `~/.local/bin/monitorzinho`, or to
`/usr/local/bin` when run as root (Linux x86_64 only for now). Then just run:

```sh
monitorzinho
```

Two builds are published, and the script picks by what the machine has:

| Asset | For | Note |
| --- | --- | --- |
| `monitorzinho-linux-x86_64` | glibc (Debian, Ubuntu, Fedora, RHEL…) | baseline glibc 2.17, so it runs on anything from RHEL 7 onwards |
| `monitorzinho-linux-x86_64-musl` | musl (Alpine, and many containers) | statically linked; no GPU panel, since NVML is `dlopen`ed and a static binary can't |

The distinction matters more than it looks: a glibc binary on Alpine installs
fine and then refuses to start, because the loader it names isn't there — and
the shell reports that as `not found`, pointing at the program rather than at
what is missing. The installer decides before downloading, and runs what it
downloaded before replacing anything, so it can't report success over a binary
that cannot execute here.

### Build from source

Requires a stable Rust toolchain ([rustup.rs](https://rustup.rs)).

```sh
cargo build --release
./target/release/monitorzinho
```

or just `cargo run --release` during development.

`monitorzinho --version` prints the installed version and `--help` a short
description; everything else is chosen from inside the program.

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
  refreshed live while fullscreened, **including the ones inside containers**.
  The netlink dump only answers for the namespace it is asked from, which is the
  host's, so a machine running five containers that talk to each other all day
  showed three SSH connections and called that the picture. Other namespaces are
  read through `/proc/<pid>/net/tcp` — that file is the socket table of that pid's
  namespace, so a process you may read is a namespace you may read — and each row
  is labelled with the container it belongs to, named from the runtime's own state
  file. What `/proc` doesn't carry is `tcp_info`, so those rows show a dash for
  traffic rather than a zero. Where a namespace can't be opened at all (root-owned
  containers seen from a normal user), the panel's corner says how many were left
  out.
- **Top CPU** / **Top Memory** — the heaviest processes by each metric, shown
  as a tree: parents expand to their children with `←`/`→`.
- **SSH Sessions** — who is logged in over SSH, from where, on which TTY,
  since when, and what they're running. Read from utmp, the file `who` has
  always read — and, on machines that no longer keep one (systemd built
  `-UTMP`, which is how Debian 13 ships it), from logind's session files
  instead. Those don't record the terminal, so it's read back off the
  session's own processes, and a session logind is still holding open for a
  process that outlived the login is dropped rather than shown as live.
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

Every table re-ranks on every tick, which is right and makes following one
particular row impossible: you find the connection you care about, look away, and
it has moved. `Ctrl+E` on a row **marks** it — it keeps whatever position the
ranking gives it and wears a ★ wherever it lands, in the compact panel as well as
fullscreen, across restarts.

What a mark is *about* differs by table, because the subject does: a port is a
number, a process is a command line, a session is a person. Ports and connections
mark by port (compared as a number, so `443` never catches `4433`), address or
process; the process tables mark by command, with the option to extend the mark to
everything below it in the tree; SSH sessions by user, origin or command. Typing
`postgres` doesn't require knowing what a regular expression is, and typing
`^ssh(d)?$` isn't taken literally. `Ctrl+E` again stops following.

Each mark has a **colour**, which is what makes several at once useful: with one
star everywhere, a list with four things followed in it says "four of these
matter" and nothing more. A new mark opens on the first colour its table isn't
already using, so marking three things in a row gives three different colours
without anyone choosing, and the box changes it before saving.

`Ctrl+G` opens the **list of every mark** on the machine — what it follows, in
which table, in what colour. Marks are cheap to make and outlive the app, so
somewhere has to answer "what am I following, and why is that row green". `←`/`→`
recolour the one under the cursor in place, `Enter` reopens the box on it, `Del`
drops it.

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

The picker searches, too: type and it finds, which matters most on the offers
that come from a tool's log — a sweep of a /24 comes back with a hundred
addresses and the one you want is somewhere in the middle. It never hides a row.
This is a list of things about to be *acted* on, with an "all of them at once"
row sitting at the top of it, so quietly narrowing what "all" means is how you
end up with forty executions you never saw. The cursor moves to the match and
marks it in place, `↑`/`↓` step between hits, `Esc` drops the search before it
drops the picker — and where a narrowed "all" is genuinely what you want, the
bulk row says so in as many words: "the 4 matching «5432»".

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

`Espaço` switches the selected execution off, and on again. Off is not removed
and not stopped-by-accident: the row stays where it is, with its log and its
counters, drawn dim and struck through — and it stays off across restarts, so the
app never comes back up doing the thing somebody turned off. What being off costs
depends on the tool, which is what its log says at the moment it goes off: a relay
gives its port back, a probe stops probing, and one that only works when asked
refuses to work — `Enter` on a switched-off scan opens the log to read rather than
starting a scan, and `r` does nothing until it is switched back on. Editing one
doesn't switch it on either; only `Espaço` does.

Switching one off takes its chart with it, where it had one: the measurement has
stopped, and a panel still reading that series would draw a flat line at the last
value — a picture of something that isn't happening. It comes back with the
execution, continuing the same line, since the history is kept under what is being
measured rather than under the execution.

A row whose **last** logged event was an error is drawn in red, and goes back to
normal by itself when something works again — the list is where several
executions are watched at once, so a failure that only exists inside one of them
makes the list lie by omission. Failing *now*, not "has failed": an error an hour
ago on something that has worked ever since would otherwise paint a row that
never turns back.

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

Four things it does beyond relaying:

- **Proxy mode.** Instead of one fixed target, it takes the destination from
  each request: point a client's `http_proxy` and `https_proxy` at it and every
  host it talks to shows up, which is what you want when the client is a program
  you didn't write. A plain request is logged whole, rewritten if there are
  rules, and forwarded with its request line put back into the origin form a
  server expects; a `CONNECT` is relayed byte for byte with only its volume
  counted, because what crosses it is TLS this process has no certificate to
  impersonate. Recording that payload was the first implementation, and it
  buried every readable line under thousands of bytes of ciphertext.

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

#### Receptor de requisições

A port that receives and writes down, and forwards nothing. The tunnel needs
somewhere to send what it catches; this is for when there is nowhere — a webhook
a provider was asked to call, an OAuth redirect, a device that POSTs every
minute, a script somebody swears is sending the right thing.

`nc -l` accepts the connection too, and then the bytes scroll past. What this
adds is the same log every other execution here writes — searchable, hex-viewable,
still there an hour later — plus an answer worth sending: a status and a body, so
the caller sees a 200 and stops retrying, or sees the 500 you asked for and shows
you what it does about errors. A JSON body is served as `application/json`,
because a caller expecting one and getting `text/plain` is an afternoon.

An HTTP request is answered the moment it is complete — at the blank line, or
once `Content-Length` bytes have arrived — so nothing waits on a timeout. What
isn't HTTP is answered as soon as the sender goes quiet.

Over UDP only `eco` and `nada` mean anything: there is no request to reply to,
but sending the datagram back proves the round trip, which is what someone
testing a NAT or a firewall is after. The row says which of the two it is instead
of promising a status it cannot send.

#### Seguir arquivo

`tail -f` inside the viewer every other execution already writes to. The reason
it's a tool rather than a suggestion to open another terminal is that viewer:
search as you type, jump between hits, hide what doesn't match, read it as hex,
and thousands of lines still there an hour later. `tail -f | grep` gives you one
of those and takes the rest away.

It opens with the last 200 lines rather than with an empty screen, found by
walking backwards in 64 KB blocks counting newlines — a 56 MB log opens
instantly. A filter can be applied at the source, so a line that doesn't match
never enters the log at all; the viewer's own search still works on top of that,
which is a different thing on purpose.

Rotation is handled: the file is recognised by device and inode rather than by
name, so a `logrotate` that renames it and creates a fresh one is noticed, said
out loud in the log, and followed from the start of the new file. Following the
old descriptor in silence is the classic way this goes wrong.

#### Sonda HTTP

Calls a URL on a schedule and says where the time went. "Is it up" is the easy
half; the half that decides what to do next is *which part* was slow — a name
that took 900 ms to resolve, a handshake that took two seconds, or a server that
accepted the connection instantly and then sat on the request. The total hides
all three, so the four phases are measured separately: DNS, connect, TLS,
first byte.

It keeps running rather than answering once, because an endpoint that answered
once is not an endpoint that is up. A status class can be required (`2xx`, or an
exact `204`), redirects are followed and shown hop by hop, and every failure is a
red line that moves the success rate on the row.

Two details it gets right on purpose, both found by comparing against `curl -w`
on the same target: `TCP_NODELAY`, without which a small request waits on the
other side's delayed ACK and the probe reports 40 ms of its own making as the
server's latency; and one TLS client per host kept for the life of the execution,
since building it parses the whole system trust store and did so on every request.

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

#### Inspetor de certificado

Everything a TLS certificate says about itself, from a host name or a bare
address. `openssl s_client` shows you the same bytes and leaves the reading to
you; this does the reading.

The whole chain the server sent, each certificate's names, dates, key, usages and
revocation endpoints laid out in order; the two questions a browser actually asks
— does the name match, does the chain lead to a root this machine trusts —
answered separately; and a closing list of what is wrong with it: expired, about
to expire, self-signed, a name it doesn't cover, an RSA key too small, a SHA-1
signature, a lifetime past what browsers accept, an intermediate the server
forgot to send.

It connects twice on purpose. The first handshake accepts anything, because a
certificate that fails verification is precisely the one worth reading; the
second verifies properly, and its error message is the verdict.

`STARTTLS` covers the ports that begin in plaintext and upgrade on request —
SMTP, IMAP, POP3 — so a mail server's certificate reads the same way a web
server's does.

It can also **watch** rather than read: give it an interval and the execution
stops being on-demand and checks on its own, saying nothing but a single line per
check until there is something to say — that the certificate is inside the number
of days you called close, or that it changed underneath, which is compared by
SHA-256 fingerprint and answered with a full re-read. A certificate expires on a
date, not when somebody remembers to look.

#### Rota até o host

Every router between here and a host, with the latency of each hop — the gap
between "the host is alive" and "the host doesn't answer". A silent host may be
off, filtered at its own door, or behind a link that dies three hops earlier, and
those are three different afternoons.

The path is found through the ICMP errors a packet provokes by running out of
hops, and those never arrive on a socket's receive queue: the kernel puts them on
its error queue, where `recvmsg(MSG_ERRQUEUE)` collects both the error and the
address of the router that sent it.

Which is why the probe is **UDP rather than ICMP**. An unprivileged ICMP socket
depends on `net.ipv4.ping_group_range`, which was empty on both machines this was
developed against — including for root — while a UDP socket with `IP_RECVERR`
depends on nobody and collects the same answers: routers reply "time exceeded" to
a datagram exactly as they would to an echo, and the destination replies "port
unreachable", which is how the trace knows it arrived. Same approach as
`tracepath`, and it works everywhere.

Three probes a hop, a silent router shown as `* * *` without ending the trace,
and five silent hops in a row called what it is — a wall — instead of thirty
lines of stars.

#### The form asks only what applies

A tool's parameters are not all relevant at once. A tunnel in proxy mode has no
single destination — it takes one from each request — and TLS to the target is a
TCP affair, and the name sent in a handshake means nothing when there is no
handshake. A field that does nothing where it stands teaches something false:
whoever fills it in believes they changed something, and whoever leaves it blank
wonders what they forgot.

So a parameter can declare what it depends on (`only_when`, chained when it takes
more than one thing to be true), and the wizard skips the rest — drawing,
navigating and confirming. Values of hidden fields are kept, so changing a mode
back brings back what was typed, and `Tool::start` still validates everything,
since a hand-edited `tools.json` never passes through a form.

#### Repetir requisição

The tunnel shows a request going past; the question that follows is always what
happens if it runs again. Rebuilding it in `curl` means copying header by header,
and the header that matters is usually the one nobody thought to copy — so the
request travels whole. What a relay saw is pre-filled here, escaped onto one line
(`\r`, `\n`, `\xNN`), and sending it again is one key. Repeating it unchanged
answers "was it me or them"; repeating it changed — a different path, one header
short — answers which part.

Requests are framed rather than grabbed: a `read` is not a request, so the head is
reassembled across chunks and the body ends where `Content-Length` says it does.
A chunked body isn't offered at all, since repeating a head whose body never ends
would leave the far side waiting. The destination comes from the tunnel that
captured it, because it is in that tunnel's configuration and not in the request;
a receiver has no destination to give, so that offer opens the form with
everything else filled in and the cursor on the one field only the user can
answer.

Nothing is sent until asked — on demand, like the port scanner. A request that
moves money is repeated when somebody means it, not because the app started.

#### Latência contínua

The same round trip `ping` measures, kept running and drawn as a line on the
Visão geral tab beside CPU, memory and network. `ping` in a terminal answers "is
it up right now"; this answers what the link did while you were looking at
something else, which is the question that actually comes up — the loss worth
finding lasts forty seconds, twice an hour.

Three ways to measure, because the classic one is often unavailable: a real ICMP
echo where `net.ipv4.ping_group_range` allows it, the time to open a TCP
connection (a refusal counts — the RST came from the host and times the round
trip just as well), or a UDP datagram to a dead port timed by the "port
unreachable" it provokes. `automático` prefers ICMP and falls back to TCP, and
the log says which one it settled on rather than pretending.

A lost packet is charted as a zero, never as the timeout: silence is not a
measurement, and charting it as one would put a spike where there was nothing and
drag the average up with numbers nobody measured. The panel's history is kept
under the target — `ping:1.1.1.1` — so it is the same line tomorrow, and a panel
a tool feeds keeps sampling on every tab, since leaving the measurement running
is the entire point.

#### Sonda SMTP

What a mail server says about itself before anyone tries to send anything through
it. The certificate reader already speaks STARTTLS, so "is the certificate fine"
is answered next door; what's left is everything else a mail server announces and
nobody checks — which extensions it offers, whether it will take a password over
a plaintext connection, how big a message it accepts, whether the name it greets
you with is its own, and whether it will relay mail for a stranger, which is the
one question whose wrong answer ends up on a blocklist.

The STARTTLS upgrade is real: the connection is taken over by TLS and the
conversation continues encrypted, with a second EHLO — because the extension list
changes once the connection is private, and `AUTH` usually only appears there.

Nothing is ever sent. The relay test stops at `RCPT TO` and resets, which is
after the server has already decided and before anybody gets an email.

#### Endereço público (STUN), Wake-on-LAN, Portas de saída

Three small questions that had nowhere to be asked.

**STUN** gives the address this machine appears as on the internet, the port NAT
gave it, and what the NAT does with both. `curl ifconfig.me` gives the address and
nothing else, and gets it by asking a web server to say — which works right up
until the thing being debugged is UDP not coming back. Two servers from different
operators are asked on purpose: same port for different destinations means a cone
NAT you can punch through, a different port per destination means a symmetric one
where direct peer-to-peer will not work however hard you try.

**Wake-on-LAN** sends the one packet a switched-off machine's card still listens
for: six `FF` bytes and its MAC sixteen times over, three times over because
nothing acknowledges a magic packet. The network scanner publishes the MAC of
everything it sees, so waking a machine is offered straight from a sweep — which
is how you have the MAC of a machine that is now off.

**Portas de saída** asks the question backwards: not what the destination
accepts, but what can leave from here. A refused connection counts as success —
the refusal had to get out — while a blocked port simply goes quiet, which is why
it waits three seconds and not three hundred milliseconds.

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

Before probing anything it asks the network what it has, and listens for four
seconds. mDNS and SSDP are how printers, televisions, speakers and NAS boxes
announce themselves, and every one of them answers a question anybody may ask —
so the silent address at `.112` stops being an address and becomes
`HP Smart Tank 580`, and `.113` becomes a Chromecast. A device that announced
itself is alive whatever it did with the probes, and the name it calls itself
beats whatever reverse DNS has for it.

The DNS-SD walk takes two rounds on purpose: the well-known "what services exist
here" question answers with service *types*, and treating those as names is how
six different machines end up called `_services._dns-sd._udp`. The second round
asks who offers each type, and those answers carry the label the owner typed.
Replies are asked for over unicast, so nothing has to bind port 5353 and fight
the `avahi-daemon` that already owns it.

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
| `Ctrl+E` | a fullscreened table | mark the row so it stays findable while the list reorders; again to unmark |
| `Ctrl+G` | a fullscreened table, Visão Geral, Processos | the list of every mark: `←`/`→` recolour, `Enter` edit, `Del` remove |
| `Del` | a fullscreened table | what it kills depends on the table — the confirmation says so before anything happens |
| any letter | a fullscreened table | search, live |
| `a` / `e` / `r` / `Del` | Ferramentas | add / edit / restart (or re-run) / remove an execution |
| `Espaço` | Ferramentas | switch that execution off — or back on |
| `Enter` | Ferramentas | open that execution's live log — and run it, for an on-demand tool |
| `Tab`, `Ctrl+F` | an execution's log | hex view, matches-only filter |
| `Ctrl+L`, `End` | an execution's log | clear the scrollback, jump back to the live edge |
| `Ctrl+P` | a detail view | turn what's on screen into an execution — a tunnel to either end of a connection, a recording of a port, a sweep of an interface's network |
| `Ctrl+P` | an execution's log | turn what it found into new executions — one, or all of them |
| any letter | the `Ctrl+P` picker | search the offers; the cursor moves to the match, nothing is hidden |
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
  until the user opens it. Implement it in `src/tools/<name>.rs` and add it to
  `all_tools()`.

Findings are typed, and what a finding is *worth doing* is decided by its type in
one place (`tools::offers_for`), not by the tool that found it. An address is an
address whether a network sweep, a DNS investigation or a certificate turned it
up, and it is always worth scanning and always worth reading a certificate off.
So a tool records what it found — `found("ip", …)`, `found("dominio", …)`,
`found("porta-tls", …)` — and is wired into every other tool for free; a new tool
that consumes addresses becomes reachable from every existing one at once.

Work that several panels need is done once per tick, not once per panel. Mapping
socket inodes to the processes holding them means reading every `/proc/<pid>/fd`
on the machine — thousands of syscalls on a busy server — and three panels want
that same map in the same tick; `SystemState` computes it, the socket tables and
the interface list at most once per tick and hands out the same answer. The
container namespace list is re-enumerated at most every five seconds, since
containers appear on the scale of deployments rather than of ticks, while their
socket tables are read fresh every tick because that is what the panel is about.
Facts that cannot change while the machine is running — DMI, kernel, model — are
read once at startup.

Switching to a tab draws it before sampling it, not after: on a busy node the
sample takes a third of a second, and paying for it before the first frame is
what turns a keypress into a wait. The two walks that dominate it — the container
namespaces' socket tables and the `/proc/<pid>/fd` scan — are read across cores,
since the expensive part is the kernel formatting those tables and that work
parallelises even though ours barely exists. `monitorzinho --bench` times one such
sample panel by panel, which is how any of this was decided.

On a Kubernetes node with 789 processes and 44 network namespaces, the Processes
tab costs about a fifth of one core; getting there meant finding that the first
version of the namespace scan read a socket table *per process* to test whether it
could, which cost a whole core on its own.

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
| `history.json` | chart history, so the sparklines survive a restart — including the ones a tool feeds, keyed by what they measure |
| `tools.json` | the executions to bring back on launch, and their parameters |
| `rewrites.json` | every rewrite rule ever written, offered as suggestions |
| `marks.json` | the rows you asked to keep an eye on, per table |

## License

MIT — see [LICENSE](LICENSE).
