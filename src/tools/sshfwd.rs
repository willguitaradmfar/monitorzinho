//! A port that isn't here, answering here — or a port that isn't there, answering there.
//!
//! What this runs is `ssh -N -L`, the command everybody already knows, plus the three
//! things that command doesn't have. It is **remembered**: the tunnel comes back with the
//! app, and switching it off keeps the row instead of losing the configuration. It is
//! **watched**: the row says whether the tunnel is actually up, and the log keeps every
//! word the `ssh` said — which is where the answer lives on the day it isn't. And it
//! **comes back**: a laptop that slept, a link that dropped, a server that rebooted. A
//! terminal holding a tunnel is a terminal somebody has to notice has died and type into
//! again; this one reconnects on its own, backing off so a server that is genuinely gone
//! isn't hammered.
//!
//! This is the one tool here that runs another program. Everything else speaks its
//! protocol itself, down to the DNS wire format — and SSH is exactly where that stops
//! being worth it. Not because of the protocol, but because of everything around it:
//! `ssh` reads `~/.ssh/config`, asks the agent, checks `known_hosts`, hops through a
//! `ProxyJump`, and honours the ten years of options a working setup accumulates. A
//! client written here would be a second, worse `ssh` that ignored all of it — and the
//! machine you actually need a tunnel to is never the simple one.
//!
//! Two things follow from being a child process, and both are dealt with rather than
//! hoped about:
//!
//! * It can never ask a question. `BatchMode=yes` and a `/dev/null` stdin, because an
//!   `ssh` prompting for a passphrase behind an alternate screen is a hang with no
//!   visible cause. An encrypted key therefore works through the agent, or not at all.
//! * It can never outlive the app. `PR_SET_PDEATHSIG` on the way to `exec`, so closing
//!   monitorzinho can't leave a process forwarding traffic on behalf of something that
//!   no longer exists — and the port comes back with it.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::net::{IpAddr, TcpListener, ToSocketAddrs};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::{EventKind, Execution, ParamSpec, Recorder, Suggestion, Tool};

const DIRECTIONS: &[&str] = &["local (-L)", "remoto (-R)"];
/// The one that opens the port on the far side. Compared against rather than parsed, so
/// the stored value is the label and never drifts from what the form showed.
const REMOTE: &str = DIRECTIONS[1];

const HOST_KEYS: &[&str] = &["conferir known_hosts", "aceitar host novo"];
const ACCEPT_NEW: &str = HOST_KEYS[1];

/// How long the supervisor sleeps between looks at the child and at the stop flag.
/// Shorter than the app's own restart grace on purpose: `r` on a tunnel stops it and
/// starts the replacement a beat later, and the port has to be back by then.
const SLICE: Duration = Duration::from_millis(100);
/// How long an `ssh` has to stay alive before the tunnel counts as up, when nothing it
/// said said so. With `ExitOnForwardFailure=yes` everything that can go wrong goes wrong
/// in the first second — a process still running after this has a forward in place.
const CONFIRM: Duration = Duration::from_secs(3);
/// How long a connection has to last to count as one that worked. Anything shorter is a
/// failure however it looked, and keeps the backoff climbing.
const STABLE: Duration = Duration::from_secs(30);
/// Seconds between attempts, by how many have failed in a row. A link that comes back
/// two seconds later is the common case; a server that is off is the case this must not
/// spend the afternoon knocking on.
const BACKOFF: [u64; 5] = [2, 5, 15, 30, 60];

pub struct SshFwdTool;

impl Tool for SshFwdTool {
    fn id(&self) -> &'static str {
        "sshfwd"
    }

    fn name(&self) -> &'static str {
        "Port forward SSH"
    }

    fn description(&self) -> &'static str {
        "Leva uma porta de um lado do SSH para o outro — o -L e o -R do ssh, guardados, vigiados e reconectando sozinhos"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::choice(
                "sentido",
                "Sentido",
                DIRECTIONS,
                "«local» abre a porta nesta máquina e quem conectar aqui sai pelo servidor — é como se alcança o banco que só escuta lá dentro. «remoto» abre a porta no servidor e quem conectar lá sai por esta máquina — é como se expõe o que roda aqui",
            ),
            ParamSpec::text(
                "usuario",
                "Usuário SSH",
                "",
                "Vazio deixa o ssh decidir: o que estiver no ~/.ssh/config para esse host, ou o seu login",
            )
            .suggesting(user_suggestions()),
            ParamSpec::text(
                "host",
                "Servidor SSH",
                "",
                "IP ou nome do servidor. Um apelido do ~/.ssh/config vale igual, com tudo que estiver escrito lá — inclusive ProxyJump. «usuario@host» também é aceito aqui",
            )
            .suggesting(host_suggestions()),
            ParamSpec::text(
                "porta_ssh",
                "Porta do SSH",
                "",
                "Onde o sshd atende — não tem nada a ver com as portas do túnel. Vazio deixa o ssh decidir: 22, ou o que o ~/.ssh/config disser para esse host",
            )
            .suggesting(vec![
                Suggestion::new("", "o que o ssh usar — 22, ou o ~/.ssh/config"),
                Suggestion::new("22", "a de sempre, dita na mão"),
            ]),
            ParamSpec::text(
                "listen",
                "Porta que abre",
                "127.0.0.1:8080",
                "Onde o túnel passa a atender: «porta» ou «endereço:porta». No sentido local ela abre aqui; no remoto abre no servidor, e lá 0.0.0.0 só pega com GatewayPorts ligado no sshd",
            ),
            ParamSpec::text(
                "alvo",
                "Conectar em",
                "127.0.0.1:5432",
                "host:porta que recebe cada conexão que chegar. No sentido local quem resolve esse nome e conecta é o servidor; no remoto é esta máquina",
            ),
            ParamSpec::text(
                "chave",
                "Chave privada",
                "",
                "Arquivo da chave (-i), usada sozinha. Vazio usa o agente e as chaves padrão do ~/.ssh. Chave com senha só funciona pelo agente: o ssh daqui nunca pergunta nada",
            )
            .suggesting(key_suggestions()),
            ParamSpec::choice(
                "hostkey",
                "Chave do servidor",
                HOST_KEYS,
                "«conferir» exige o servidor já estar no known_hosts. «aceitar host novo» grava a chave de um servidor que você nunca visitou — e continua recusando uma que mudou, que é o aviso que importa",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        match Plan::from(params) {
            Ok(plan) => plan.summary(),
            // Before it is valid there is still a row to draw — the one that failed to
            // start — and the two fields that say what was meant are enough for it.
            Err(_) => {
                let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
                format!("{} → {}", get("listen"), get("alvo"))
            }
        }
    }

    /// Where the tunnel stands, and what it has carried. The state comes from the
    /// supervisor thread; the count comes from the `ssh` itself, which says so at `-v`
    /// every time a connection crosses.
    fn columns(&self, execution: &Execution) -> (String, String) {
        let (headline, detail) = execution.outcome();
        let opened = execution.stats.connections.load(Ordering::Relaxed);
        if opened == 0 {
            return (headline, detail);
        }
        let noun = if opened == 1 { "conexão" } else { "conexões" };
        (headline, format!("{opened} {noun} · {detail}"))
    }

    /// Switching this off doesn't only stop a thread: it drops the SSH connection, and
    /// with it the port — which is on the other machine half the time, and that is
    /// exactly the half nobody would guess.
    fn off_note(&self, params: &HashMap<&'static str, String>) -> Option<String> {
        let plan = Plan::from(params).ok()?;
        let onde = if plan.remote {
            "no servidor"
        } else {
            "nesta máquina"
        };
        Some(format!(
            "desligada — a conexão SSH caiu e a porta {} {onde} foi liberada. Espaço liga de novo",
            plan.listen_display()
        ))
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        plan.check_local_port()?;
        let (execution, recorder) = Execution::new(id, self.name(), plan.summary());
        // The server is an address like any other: from here the whole address menu is
        // one Ctrl+P away. An alias out of ~/.ssh/config is deliberately not recorded —
        // it is a name only this machine's ssh knows, and offering to resolve it would
        // be offering a lookup that cannot work.
        match plan.host.parse::<IpAddr>() {
            Ok(address) => recorder.found("ip", address.to_string()),
            Err(_) if plan.host.contains('.') => recorder.found("dominio", plan.host.clone()),
            Err(_) => {}
        }
        let finished = execution.finish_flag();
        thread::spawn(move || {
            supervise(plan, &recorder);
            finished.store(true, Ordering::Relaxed);
        });
        Ok(execution)
    }
}

/// Everything the form amounts to, once it has been read and found to make sense. The
/// one check that isn't here is the local port's — see `check_local_port`.
struct Plan {
    /// `-R`: the port opens on the server. The other way is `-L`.
    remote: bool,
    /// Empty means "whatever the ssh would have used" — usually the ~/.ssh/config entry.
    user: String,
    host: String,
    /// Empty for the same reason `user` can be: an alias out of `~/.ssh/config` usually
    /// already says which port that machine answers on, and a `-p 22` put on the command
    /// line by a form nobody filled in would silently overrule it.
    port: Option<u16>,
    /// Which address the listening side binds. Always filled in, never left to a
    /// default: `-L 8080:...` and `-L 127.0.0.1:8080:...` differ on the day somebody
    /// wonders why the port is reachable from the office network.
    bind: String,
    listen_port: u16,
    target: String,
    target_port: u16,
    key: Option<PathBuf>,
    accept_new: bool,
    ssh: PathBuf,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let mut user = get("usuario").to_string();
        let mut host = get("host").to_string();
        // `will@servidor` is what fingers type, whatever the form asked for. Taking it
        // apart here beats an error message explaining that there are two fields.
        if let Some((typed, rest)) = host.split_once('@') {
            if user.is_empty() {
                user = typed.trim().to_string();
            }
            host = rest.trim().to_string();
        }
        if host.is_empty() {
            return Err("informe o servidor SSH".to_string());
        }
        if host.split_whitespace().count() > 1 {
            return Err(format!("«{host}» não é um servidor"));
        }
        let remote = get("sentido") == REMOTE;
        let (bind, listen_port) = endpoint(get("listen"), "a porta que abre", Some("127.0.0.1"))?;
        let (target, target_port) = endpoint(get("alvo"), "o destino", None)?;
        let key = match get("chave") {
            "" => None,
            typed => {
                let path = expand(typed);
                if !path.is_file() {
                    return Err(format!("não achei a chave {}", path.display()));
                }
                Some(path)
            }
        };
        Ok(Self {
            remote,
            user,
            host,
            port: match get("porta_ssh") {
                "" => None,
                typed => Some(number(typed, "a porta do SSH")?),
            },
            bind,
            listen_port,
            target,
            target_port,
            key,
            accept_new: get("hostkey") == ACCEPT_NEW,
            ssh: ssh_binary().ok_or_else(|| "não achei o comando ssh no PATH".to_string())?,
        })
    }

    /// Only called from `start`, never from the parse: taking a port to see whether it
    /// is free is a thing to do when something is about to be started, and `summarize`
    /// and `off_note` build a plan just to read it — the second of those while the port
    /// is still held by the very execution being switched off.
    ///
    /// A local forward's port is taken by the `ssh`, which finds out a second after it
    /// starts and out of sight of the form. Taking it here first — and letting go of it
    /// at once — turns the common half of that into an error the user is still standing
    /// in front of. The gap between letting go and the `ssh` binding is a race nothing
    /// can close; losing it costs the same message, one screen later.
    fn check_local_port(&self) -> Result<(), String> {
        if self.remote {
            return Ok(());
        }
        let Some(address) = (self.bind.as_str(), self.listen_port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut found| found.next())
        else {
            return Err(format!("não resolvi o endereço de escuta {}", self.bind));
        };
        TcpListener::bind(address)
            .map(drop)
            .map_err(|e| format!("não consigo abrir {address} aqui: {e}"))
    }

    /// `127.0.0.1:8080` — the listening side as one string, for the row and for the log.
    fn listen_display(&self) -> String {
        format!("{}:{}", self.bind, self.listen_port)
    }

    fn target_display(&self) -> String {
        format!("{}:{}", self.target, self.target_port)
    }

    /// `will@10.0.0.5`, with the port only when it isn't the one everybody assumes.
    fn who(&self) -> String {
        let at = if self.user.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.user, self.host)
        };
        match self.port {
            Some(port) if port != 22 => format!("{at}:{port}"),
            _ => at,
        }
    }

    /// The row's Detalhe. `aqui` and `lá` are the whole point of the line: the two
    /// directions differ only in which machine the port opens on, and a summary that
    /// left that out would read identically for the two things it can't be.
    fn summary(&self) -> String {
        let (from, to) = if self.remote {
            ("lá", "aqui")
        } else {
            ("aqui", "lá")
        };
        format!(
            "{} {} {from} → {} {to}  ·  {}",
            if self.remote { "-R" } else { "-L" },
            self.listen_display(),
            self.target_display(),
            self.who()
        )
    }

    /// `bind:porta:host:porta`, the argument to `-L`/`-R`.
    fn forward(&self) -> String {
        format!(
            "{}:{}:{}:{}",
            self.bind, self.listen_port, self.target, self.target_port
        )
    }

    fn argv(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            // Verbose on purpose. Without it a working `ssh -N` says nothing at all and
            // a broken one says one line; with it the log holds the handshake, the
            // method that authenticated, the forward being set up and every connection
            // that crosses — which is the log this app exists to keep.
            "-v".to_string(),
            // No command, no shell, no pty: a tunnel and nothing else.
            "-N".to_string(),
            // Never ask. See the module note: a prompt behind the alternate screen is a
            // hang nobody can see the cause of.
            "-o".to_string(),
            "BatchMode=yes".to_string(),
            // Fail loudly when the port can't be opened, instead of a connection that is
            // up and forwarding nothing — which looks exactly like a working tunnel from
            // here and answers nothing at all from over there.
            "-o".to_string(),
            "ExitOnForwardFailure=yes".to_string(),
            // Notice a link that died silently — a laptop that slept, a NAT that forgot
            // — in about 45 seconds, rather than holding a socket that will never answer
            // again. Dying is what makes it reconnect.
            "-o".to_string(),
            "ServerAliveInterval=15".to_string(),
            "-o".to_string(),
            "ServerAliveCountMax=3".to_string(),
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            "-o".to_string(),
            format!(
                "StrictHostKeyChecking={}",
                if self.accept_new { "accept-new" } else { "yes" }
            ),
        ];
        if let Some(key) = &self.key {
            args.push("-i".to_string());
            args.push(key.display().to_string());
            // Naming a key means that key: without this the agent's identities are
            // offered first and a server counting attempts closes the door before the
            // one that was asked for is ever tried.
            args.push("-o".to_string());
            args.push("IdentitiesOnly=yes".to_string());
        }
        if let Some(port) = self.port {
            args.push("-p".to_string());
            args.push(port.to_string());
        }
        args.push(if self.remote { "-R" } else { "-L" }.to_string());
        args.push(self.forward());
        if !self.user.is_empty() {
            args.push("-l".to_string());
            args.push(self.user.clone());
        }
        args.push(self.host.clone());
        args
    }

    /// The command as it would be typed. Logged on every attempt: the fastest way to
    /// find out why a tunnel won't come up is to run the same line by hand.
    fn command_line(&self) -> String {
        let mut line = self.ssh.display().to_string();
        for arg in self.argv() {
            line.push(' ');
            line.push_str(&arg);
        }
        line
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.ssh);
        command
            .args(self.argv())
            // stdin is where a prompt would be read from and stdout is where nothing is
            // written; stderr is where ssh says everything it has to say.
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        // The kernel's own guarantee that no tunnel outlives the app: if the thread that
        // spawned this dies — which is what happens to every thread when the process
        // exits — the child is killed rather than inherited by init and left forwarding.
        unsafe {
            command.pre_exec(|| {
                // Best effort: a system that refuses this still gets the explicit kill
                // on the way out of every path that has one.
                prctl(PR_SET_PDEATHSIG, SIGKILL as u64);
                Ok(())
            });
        }
        command
    }
}

/// One `ssh`, from spawn to exit, with its stderr read into the log the whole time.
///
/// Reading the pipe happens on its own thread because the read blocks: the stop flag has
/// to be looked at every fraction of a second, and a tunnel nobody is using says nothing
/// for hours at a time.
fn run(plan: &Plan, rec: &Recorder) -> std::io::Result<ExitStatus> {
    let mut child = plan.command().spawn()?;
    let established = Arc::new(AtomicBool::new(false));
    let reader = child.stderr.take().map(|pipe| {
        let rec = rec.clone();
        let flag = Arc::clone(&established);
        thread::spawn(move || read_stderr(pipe, &rec, &flag))
    });

    let started = Instant::now();
    let mut announced = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if rec.stopping() {
            terminate(&mut child);
            break child.wait()?;
        }
        // Up, either because the ssh said so or because it is still alive after
        // everything that could have stopped it. The second test is what keeps this
        // working on an OpenSSH whose debug wording isn't the one read here.
        if !announced && (established.load(Ordering::Relaxed) || started.elapsed() >= CONFIRM) {
            announced = true;
            rec.record(0, EventKind::Note("túnel no ar".to_string()));
            rec.report("no ar", plan.who());
            // A local port that is now answering is a port like any other: pointing the
            // recording tunnel at it reads the traffic going through the SSH one.
            if !plan.remote {
                rec.found("porta", plan.listen_display());
            }
        }
        thread::sleep(SLICE);
    };
    if let Some(handle) = reader {
        let _ = handle.join();
    }
    Ok(status)
}

/// Keeps one tunnel up for as long as the execution exists.
fn supervise(plan: Plan, rec: &Recorder) {
    let mut failures = 0usize;
    while !rec.stopping() {
        rec.record(0, EventKind::Note(format!("$ {}", plan.command_line())));
        rec.report("conectando", plan.who());
        let started = Instant::now();
        match run(&plan, rec) {
            Ok(status) => {
                if rec.stopping() {
                    break;
                }
                rec.record(
                    0,
                    EventKind::Error(format!("o ssh terminou ({})", exit_reason(status))),
                );
            }
            Err(e) => {
                rec.record(
                    0,
                    EventKind::Error(format!("não consegui rodar o ssh: {e}")),
                );
            }
        }
        if rec.stopping() {
            break;
        }
        // A connection that lasted counts as one that worked, whatever ended it: the
        // backoff is there for a server that is off, not for a link that blinks once a
        // day. Anything shorter keeps the climb going.
        failures = if started.elapsed() >= STABLE {
            0
        } else {
            failures.saturating_add(1)
        };
        let wait = Duration::from_secs(BACKOFF[failures.min(BACKOFF.len() - 1)]);
        rec.record(
            0,
            EventKind::Note(format!("tentando de novo em {}s", wait.as_secs())),
        );
        rec.report(format!("caiu · volta em {}s", wait.as_secs()), plan.who());
        let until = Instant::now();
        while !rec.stopping() && until.elapsed() < wait {
            thread::sleep(SLICE.min(wait - until.elapsed()));
        }
    }
    rec.record(0, EventKind::Note("túnel encerrado".to_string()));
    rec.report("parado", plan.who());
}

/// Everything the `ssh` says, straight into the log, with two lines read for what they
/// mean along the way.
fn read_stderr(pipe: ChildStderr, rec: &Recorder, established: &AtomicBool) {
    for line in BufReader::new(pipe).lines().map_while(Result::ok) {
        let text = clean(&line);
        if text.is_empty() {
            continue;
        }
        if is_forward_up(&text) {
            established.store(true, Ordering::Relaxed);
        }
        if is_connection(&text) {
            rec.stats().connections.fetch_add(1, Ordering::Relaxed);
        }
        // What separates the two is only the colour of the row and whether the list
        // shows the execution as failing right now — the line is kept either way, which
        // is why guessing wrong about an unknown message costs nothing.
        if is_failure(&text) {
            rec.record(0, EventKind::Error(text));
        } else {
            rec.record(0, EventKind::Note(text));
        }
    }
}

/// Drops the `debug1: ` the verbose flag puts in front of most lines. It marks where the
/// line came from, which is the one thing a log of exactly one program's output already
/// says.
fn clean(line: &str) -> String {
    let trimmed = line.trim_end();
    for prefix in ["debug1: ", "debug2: ", "debug3: "] {
        if let Some(rest) = trimmed.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    trimmed.to_string()
}

/// The forward is in place. Several wordings because they differ by direction and by
/// OpenSSH version — and none of them is required: `run` confirms by the clock too.
///
/// Deliberately not "Entering interactive session", which the ssh prints before the
/// server has answered about the port: on a remote forward whose port is taken, that
/// line arrives and the failure arrives after it.
fn is_forward_up(text: &str) -> bool {
    const MARKS: [&str; 5] = [
        "Local connections to",
        "Local forwarding listening on",
        "remote forward success",
        "forwarding_success",
        "All remote forwarding requests processed",
    ];
    MARKS.iter().any(|mark| text.contains(mark))
}

/// Something crossed the tunnel. `-L` announces the connection it was asked for; `-R`
/// announces the one the server handed over.
fn is_connection(text: &str) -> bool {
    (text.contains("forwarding to") && text.contains("requested"))
        || text.contains("client_request_forwarded_tcpip")
}

/// Whether a line is the tunnel going wrong. Deliberately specific: the row is painted
/// red by the *last* thing logged, so a word matched too eagerly would leave a working
/// tunnel looking broken.
fn is_failure(text: &str) -> bool {
    const SIGNS: [&str; 16] = [
        "Permission denied",
        "Too many authentication failures",
        "No supported authentication methods",
        "Host key verification failed",
        "IDENTIFICATION HAS CHANGED",
        "Connection refused",
        "Connection timed out",
        "Connection closed by",
        "Connection reset",
        "No route to host",
        "Could not resolve",
        "Name or service not known",
        "port forwarding failed",
        "cannot listen to port",
        "administratively prohibited",
        "not responding",
    ];
    SIGNS.iter().any(|sign| text.contains(sign))
}

/// How the `ssh` ended, in the terms the log should say it. 255 is its own way of
/// saying "something went wrong", and the lines above it in the log say what.
fn exit_reason(status: ExitStatus) -> String {
    if let Some(signal) = status.signal() {
        return format!("morto pelo sinal {signal}");
    }
    match status.code() {
        Some(0) => "saída limpa".to_string(),
        Some(255) => "código 255 — a conexão SSH falhou".to_string(),
        Some(code) => format!("código {code}"),
        None => "sem código".to_string(),
    }
}

/// Asks the child to go, then makes it. `SIGTERM` first so the `ssh` closes the session
/// properly — which for a remote forward is what tells the server to give the port back
/// now rather than when it notices the socket is gone.
fn terminate(child: &mut Child) {
    unsafe { kill(child.id() as i32, SIGTERM) };
    let deadline = Instant::now();
    while deadline.elapsed() < Duration::from_secs(2) {
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
}

// --- the form's fields ---------------------------------------------------------------

/// `host:porta` or, where `default_host` says it's allowed, a bare port.
fn endpoint(text: &str, what: &str, default_host: Option<&str>) -> Result<(String, u16), String> {
    if text.is_empty() {
        return Err(format!("informe {what}"));
    }
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) => (host.trim(), port),
        None => match default_host {
            Some(host) => (host, text),
            None => return Err(format!("informe {what} como host:porta")),
        },
    };
    let host = match (host, default_host) {
        ("", Some(fallback)) => fallback,
        ("", None) => return Err(format!("informe o host em {what}")),
        (given, _) => given,
    };
    Ok((host.to_string(), number(port, what)?))
}

fn number(text: &str, what: &str) -> Result<u16, String> {
    match text.trim().parse::<u16>() {
        Ok(0) | Err(_) => Err(format!("{what} não é uma porta: «{text}»")),
        Ok(port) => Ok(port),
    }
}

/// `~/...` the way every ssh file is written down.
fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match dirs::home_dir() {
            Some(home) => home.join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    }
}

/// The `ssh` this will run, found the way the shell would find it. Looked up while the
/// form is still open so "there is no ssh here" is an answer, not a thread that dies.
fn ssh_binary() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("ssh"))
        .find(|candidate| executable(candidate))
}

fn executable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

fn user_suggestions() -> Vec<Suggestion> {
    // The blank is first because the blank is the default, and here it means something:
    // the ~/.ssh/config entry for this host usually already says who you are there.
    let mut suggestions = vec![Suggestion::new(
        "",
        "deixa o ssh decidir — ~/.ssh/config, ou o seu login",
    )];
    if let Some(user) = std::env::var_os("USER").or_else(|| std::env::var_os("LOGNAME")) {
        suggestions.push(Suggestion::new(
            user.to_string_lossy().into_owned(),
            "o seu login nesta máquina",
        ));
    }
    suggestions
}

/// The hosts `~/.ssh/config` already names. A machine you tunnel to is a machine you
/// have connected to before, so the answer is usually already written down — and the
/// `HostName` beside it is what tells two similar aliases apart.
fn host_suggestions() -> Vec<Suggestion> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let Ok(content) = fs::read_to_string(home.join(".ssh").join("config")) else {
        return Vec::new();
    };
    let mut hosts: Vec<Suggestion> = Vec::new();
    // Which entries the block being read produced, so its HostName can be written into
    // all of them — one `Host` line may name several aliases.
    let mut block: Vec<usize> = Vec::new();
    for line in content.lines() {
        let Some((keyword, rest)) = line.trim().split_once(char::is_whitespace) else {
            continue;
        };
        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                block.clear();
                for alias in rest.split_whitespace() {
                    // A pattern is not a host: `Host *` is where the options everybody
                    // shares live, and `ssh '*'` connects to nothing.
                    if alias.contains(['*', '?', '!']) {
                        continue;
                    }
                    block.push(hosts.len());
                    hosts.push(Suggestion::new(alias, "do ~/.ssh/config"));
                }
            }
            "hostname" => {
                if let Some(name) = rest.split_whitespace().next() {
                    for index in &block {
                        hosts[*index].note = format!("~/.ssh/config · {name}");
                    }
                }
            }
            _ => {}
        }
    }
    hosts
}

/// The private keys in `~/.ssh`, found by the public half sitting beside them — which
/// picks up the ones with names nobody would guess and leaves out `config`,
/// `known_hosts` and the `.pub` files themselves.
fn key_suggestions() -> Vec<Suggestion> {
    let mut suggestions = vec![Suggestion::new("", "o agente e as chaves padrão do ~/.ssh")];
    let Some(dir) = dirs::home_dir().map(|home| home.join(".ssh")) else {
        return suggestions;
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return suggestions;
    };
    let mut keys: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "pub"))
        .map(|path| path.with_extension(""))
        .filter(|path| path.is_file())
        .map(|path| path.display().to_string())
        .collect();
    keys.sort();
    suggestions.extend(
        keys.into_iter()
            .map(|key| Suggestion::new(key, "chave em ~/.ssh")),
    );
    suggestions
}

// --- the two calls the standard library doesn't have ---------------------------------

/// `PR_SET_PDEATHSIG` from <linux/prctl.h>: the signal this process gets when the thread
/// that created it goes away.
const PR_SET_PDEATHSIG: i32 = 1;
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;

unsafe extern "C" {
    /// Variadic in C, and it reads its second argument as an `unsigned long` — hence
    /// the `u64` at the call site rather than the `i32` a signal number looks like.
    fn prctl(option: i32, ...) -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plan built by hand, so the parsing tests and the rendering tests don't have to
    /// share a fixture — and so nothing here depends on this machine having an `ssh`.
    fn plan(remote: bool) -> Plan {
        Plan {
            remote,
            user: "will".to_string(),
            host: "10.0.0.5".to_string(),
            port: None,
            bind: "127.0.0.1".to_string(),
            listen_port: 8080,
            target: "127.0.0.1".to_string(),
            target_port: 5432,
            key: None,
            accept_new: false,
            ssh: PathBuf::from("/usr/bin/ssh"),
        }
    }

    #[test]
    fn a_bare_port_is_only_allowed_where_there_is_a_host_to_assume() {
        assert_eq!(
            endpoint("8080", "a porta", Some("127.0.0.1")),
            Ok(("127.0.0.1".to_string(), 8080))
        );
        assert_eq!(
            endpoint("0.0.0.0:8080", "a porta", Some("127.0.0.1")),
            Ok(("0.0.0.0".to_string(), 8080))
        );
        // The destination has nowhere to default to: a port with no host is a
        // destination nobody named.
        assert!(endpoint("5432", "o destino", None).is_err());
        assert!(endpoint("127.0.0.1:0", "a porta", None).is_err());
        assert!(endpoint("", "a porta", Some("127.0.0.1")).is_err());
    }

    #[test]
    fn a_user_typed_into_the_host_field_is_taken_apart() {
        let params = HashMap::from([
            ("host", "will@10.0.0.5".to_string()),
            ("listen", "8080".to_string()),
            ("alvo", "127.0.0.1:5432".to_string()),
        ]);
        let Ok(parsed) = Plan::from(&params) else {
            // No ssh on this machine — the one thing this test can't supply.
            return;
        };
        assert_eq!(parsed.user, "will");
        assert_eq!(parsed.host, "10.0.0.5");
        assert_eq!(parsed.who(), "will@10.0.0.5");
    }

    #[test]
    fn the_summary_says_which_machine_the_port_opens_on() {
        // The two directions differ in nothing else, so this is the whole difference.
        assert_eq!(
            plan(false).summary(),
            "-L 127.0.0.1:8080 aqui → 127.0.0.1:5432 lá  ·  will@10.0.0.5"
        );
        assert_eq!(
            plan(true).summary(),
            "-R 127.0.0.1:8080 lá → 127.0.0.1:5432 aqui  ·  will@10.0.0.5"
        );
    }

    #[test]
    fn the_command_carries_the_forward_and_never_a_question() {
        let args = plan(false).argv();
        assert!(args.contains(&"-L".to_string()));
        assert!(args.contains(&"127.0.0.1:8080:127.0.0.1:5432".to_string()));
        assert!(args.contains(&"BatchMode=yes".to_string()));
        assert!(args.contains(&"ExitOnForwardFailure=yes".to_string()));
        assert_eq!(args.last().map(String::as_str), Some("10.0.0.5"));
        assert!(plan(true).argv().contains(&"-R".to_string()));
    }

    #[test]
    fn the_ssh_port_is_only_named_when_it_was_typed() {
        // Left alone the field says nothing, and `-p` on the command line would overrule
        // the `Port` an alias in ~/.ssh/config had already answered with.
        assert!(!plan(false).argv().contains(&"-p".to_string()));
        let mut explicit = plan(false);
        explicit.port = Some(2222);
        let args = explicit.argv();
        let flag = args.iter().position(|arg| arg == "-p").expect("tem -p");
        assert_eq!(args[flag + 1], "2222");
    }

    #[test]
    fn a_named_key_is_the_only_key_tried() {
        let mut named = plan(false);
        named.key = Some(PathBuf::from("/home/will/.ssh/id_ed25519"));
        // Without this the agent's identities go first and a server counting attempts
        // closes the door before the one that was asked for is ever offered.
        assert!(named.argv().contains(&"IdentitiesOnly=yes".to_string()));
    }

    // The lines below are OpenSSH 9.6's, copied from a session of each kind.

    #[test]
    fn the_prefix_the_verbose_flag_adds_is_dropped() {
        assert_eq!(
            clean("debug1: Entering interactive session."),
            "Entering interactive session."
        );
        assert_eq!(
            clean("Authenticated to localhost."),
            "Authenticated to localhost."
        );
    }

    #[test]
    fn the_forward_reports_itself_in_both_directions() {
        assert!(is_forward_up(&clean(
            "debug1: Local forwarding listening on 127.0.0.1 port 18080."
        )));
        assert!(is_forward_up(&clean(
            "debug1: remote forward success for: listen 127.0.0.1:18081, connect 127.0.0.1:19000"
        )));
        // Printed before the server has answered about the port: on a remote forward
        // whose port is taken, this line arrives and the failure arrives after it.
        assert!(!is_forward_up(&clean(
            "debug1: Entering interactive session."
        )));
        assert!(!is_forward_up(&clean(
            "debug1: remote forward failure for: listen 127.0.0.1:19000, connect 127.0.0.1:19000"
        )));
    }

    #[test]
    fn each_direction_announces_a_crossing_its_own_way() {
        assert!(is_connection(&clean(
            "debug1: Connection to port 18080 forwarding to 127.0.0.1 port 19000 requested."
        )));
        assert!(is_connection(&clean(
            "debug1: client_request_forwarded_tcpip: listen 127.0.0.1 port 18081, originator 127.0.0.1 port 42524"
        )));
        // Setting the forward up is not a connection through it, and neither is the
        // channel being cleaned up afterwards — counting either would double the tally.
        assert!(!is_connection(&clean(
            "debug1: Local connections to 127.0.0.1:18080 forwarded to remote address 127.0.0.1:19000"
        )));
        assert!(!is_connection(&clean(
            "debug1: channel 1: free: direct-tcpip: listening port 18080 for 127.0.0.1 port 19000, connect from 127.0.0.1 port 57230 to 127.0.0.1 port 18080, nchannels 2"
        )));
    }

    #[test]
    fn the_lines_that_end_a_tunnel_read_as_failures() {
        for line in [
            "will@localhost: Permission denied (publickey).",
            "ssh: Could not resolve hostname nao.existe: Name or service not known",
            "Error: remote port forwarding failed for listen port 19000",
            "Host key verification failed.",
        ] {
            assert!(is_failure(&clean(line)), "não marcou como falha: {line}");
        }
    }

    #[test]
    fn the_ordinary_chatter_is_not_a_failure() {
        // The row is painted red by the *last* line logged, so a word matched too
        // eagerly would leave a working tunnel looking broken.
        for line in [
            "debug1: Authentications that can continue: publickey",
            "debug1: Offering public key: /home/will/.ssh/id_ed25519 ED25519",
            "debug1: Remote: authorized_keys:1: key options: agent-forwarding port-forwarding",
            "debug1: channel 0: free: port listener, nchannels 1",
            "Authenticated to localhost ([127.0.0.1]:2222) using \"publickey\".",
        ] {
            assert!(!is_failure(&clean(line)), "marcou como falha: {line}");
        }
    }
}
