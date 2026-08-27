//! Os números ao vivo de um container, lidos do kernel desta máquina.
//!
//! Nada aqui sabe qual engine criou o container, e é de propósito: um cgroup é um
//! cgroup. `monitor::netns` já reconhecia `docker-<id>.scope`, `libpod-<id>` e
//! `crio-<id>` antes desta aba existir — a metade que mede sempre foi agnóstica, só o
//! inventário e as ações precisavam de um trait.
//!
//! Duas coisas fazem isto valer a pena em vez de perguntar à engine:
//!
//! * **Custo.** Ler o cgroup de todos os containers da máquina custa cerca de 1 ms. A
//!   mesma resposta pela API custa 7 ms por container com `one-shot=true` — e, sem ele,
//!   **um segundo inteiro por container**, porque o daemon coleta duas amostras para
//!   calcular o CPU% no seu lugar. Aqui o delta é nosso, como o do painel de conexões.
//! * **Permissão.** Os arquivos de cgroup são legíveis por todo mundo (`-r--r--r--`),
//!   inclusive os de containers do root. Isso é mais do que o painel de conexões
//!   consegue: lá, um namespace de root exige ser root.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::Pressure;

const ROOT: &str = "/sys/fs/cgroup";

/// Uma leitura crua de um cgroup, antes de virar taxa.
#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub usage_usec: u64,
    pub memory: u64,
    pub memory_limit: Option<u64>,
    pub memory_peak: Option<u64>,
    pub pids: Option<u64>,
    pub cpu_quota: Option<String>,
    pub throttled: Option<(u64, u64)>,
    pub oom_kills: Option<u64>,
    pub cpu_pressure: Option<Pressure>,
    pub memory_pressure: Option<Pressure>,
    pub io_pressure: Option<Pressure>,
    pub net_rx: u64,
    pub net_tx: u64,
}

/// O que precisa sobreviver entre duas leituras para virar taxa: sem a anterior, um
/// contador acumulado não diz nada sobre agora.
#[derive(Clone, Copy)]
struct Previous {
    at: Instant,
    usage_usec: u64,
    net_rx: u64,
    net_tx: u64,
}

/// Lê cgroups de tick em tick e devolve taxas.
///
/// Guarda a leitura anterior por container — mesma razão pela qual `ConnectionsMonitor`
/// guarda a dele: CPU e rede são contadores que só sobem, e a diferença entre duas
/// leituras é a única coisa que responde «quanto agora».
#[derive(Default)]
pub struct Reader {
    previous: HashMap<String, Previous>,
}

/// O que uma leitura produziu depois de virar taxa.
pub struct Reading {
    pub cpu_percent: Option<f64>,
    pub sample: Sample,
    pub net_rx_rate: Option<f64>,
    pub net_tx_rate: Option<f64>,
}

impl Reader {
    /// Lê um container e devolve seus números. `pid` é usado para achar o cgroup e para
    /// ler a rede do namespace; `id` é a identidade entre ticks e o caminho alternativo
    /// para achar o cgroup quando o pid não é legível.
    pub fn read(&mut self, id: &str, pid: u32) -> Option<Reading> {
        let path = cgroup_path(id, pid)?;
        let mut sample = read_sample(&path);
        if pid != 0 {
            let (rx, tx) = net_counters(pid);
            sample.net_rx = rx;
            sample.net_tx = tx;
        }

        Some(self.rate(id, sample))
    }

    /// Transforma contadores acumulados em taxas, guardando a leitura desta vez para a
    /// próxima. Separado de `read` porque num endpoint remoto não há cgroup para ler: a
    /// amostra vem da API da engine e a conta do delta é a mesma.
    pub fn rate(&mut self, id: &str, sample: Sample) -> Reading {
        let now = Instant::now();
        let previous = self.previous.insert(
            id.to_string(),
            Previous {
                at: now,
                usage_usec: sample.usage_usec,
                net_rx: sample.net_rx,
                net_tx: sample.net_tx,
            },
        );

        let (cpu_percent, net_rx_rate, net_tx_rate) = match previous {
            Some(previous) => {
                let elapsed = now.duration_since(previous.at).as_secs_f64();
                // Duas leituras no mesmo instante dividiriam por zero; e um contador que
                // andou para trás é um container que reiniciou, não uma taxa negativa.
                if elapsed <= 0.0 {
                    (None, None, None)
                } else {
                    let cpu = sample.usage_usec.saturating_sub(previous.usage_usec) as f64;
                    // Microssegundos de CPU sobre segundos de relógio: % de um núcleo,
                    // que passa de 100 num container usando vários. É a mesma conta que
                    // as ferramentas de container fazem, e por isso o número bate.
                    let percent = cpu / 10_000.0 / elapsed;
                    (
                        Some(percent),
                        Some(sample.net_rx.saturating_sub(previous.net_rx) as f64 / elapsed),
                        Some(sample.net_tx.saturating_sub(previous.net_tx) as f64 / elapsed),
                    )
                }
            }
            // Primeira leitura: não há delta, e um zero aqui seria um container ocioso
            // que não é.
            None => (None, None, None),
        };

        Reading {
            cpu_percent,
            sample,
            net_rx_rate,
            net_tx_rate,
        }
    }

    /// Esquece containers que não existem mais, para o mapa não crescer com o tempo de
    /// vida do app numa máquina onde containers vão e vêm.
    pub fn retain(&mut self, alive: &[String]) {
        self.previous.retain(|id, _| alive.contains(id));
    }
}

/// Onde mora o cgroup de um container.
///
/// Pelo pid quando ele é legível, que é a resposta exata. Quando não é — container de
/// root visto de usuário comum — procura um diretório com o id no nome, que funciona
/// porque o cgroupfs é legível por todos mesmo quando `/proc/<pid>` não é.
pub fn cgroup_path(id: &str, pid: u32) -> Option<PathBuf> {
    if pid != 0
        && let Some(path) = path_of_pid(pid)
        && path.is_dir()
    {
        return Some(path);
    }
    find_by_id(id)
}

fn path_of_pid(pid: u32) -> Option<PathBuf> {
    let text = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    // cgroup v2: uma linha só, `0::<caminho>`.
    let path = text.lines().next()?.rsplit(':').next()?;
    Some(PathBuf::from(ROOT).join(path.trim_start_matches('/')))
}

/// Quantos níveis descer procurando o cgroup de um container. Os caminhos reais têm
/// entre dois e seis (`user.slice/user-1000.slice/user@1000.service/user.slice/…`), e um
/// limite é o que impede a busca de virar uma varredura da árvore inteira.
const MAX_DEPTH: usize = 8;

fn find_by_id(id: &str) -> Option<PathBuf> {
    if id.is_empty() {
        return None;
    }
    let mut queue = vec![(PathBuf::from(ROOT), 0usize)];
    while let Some((dir, depth)) = queue.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains(id) {
                return Some(entry.path());
            }
            queue.push((entry.path(), depth + 1));
        }
    }
    None
}

fn read_sample(path: &Path) -> Sample {
    let mut sample = Sample {
        usage_usec: field(path, "cpu.stat", "usage_usec").unwrap_or(0),
        memory: memory_used(path),
        memory_limit: limit(path, "memory.max"),
        memory_peak: number(path, "memory.peak"),
        pids: number(path, "pids.current"),
        cpu_quota: cpu_quota(path),
        oom_kills: field(path, "memory.events", "oom_kill"),
        cpu_pressure: pressure(path, "cpu.pressure"),
        memory_pressure: pressure(path, "memory.pressure"),
        io_pressure: pressure(path, "io.pressure"),
        ..Sample::default()
    };
    if let (Some(count), Some(usec)) = (
        field(path, "cpu.stat", "nr_throttled"),
        field(path, "cpu.stat", "throttled_usec"),
    ) {
        sample.throttled = Some((count, usec));
    }
    sample
}

/// Memória em uso, com o cache de arquivo inativo descontado.
///
/// Sem esse desconto o número discorda das ferramentas de container e parece errado: num
/// mesmo container medimos `memory.current` = 23,9 MiB e `docker stats` = 20,8 MiB, e a
/// diferença é exatamente o `inactive_file`. É cache que o kernel devolve sob pressão,
/// então contá-lo como uso do container é contar memória que ninguém está segurando.
fn memory_used(path: &Path) -> u64 {
    let current = number(path, "memory.current").unwrap_or(0);
    let inactive = field(path, "memory.stat", "inactive_file").unwrap_or(0);
    current.saturating_sub(inactive)
}

fn read(path: &Path, file: &str) -> Option<String> {
    fs::read_to_string(path.join(file)).ok()
}

fn number(path: &Path, file: &str) -> Option<u64> {
    read(path, file)?.trim().parse().ok()
}

/// Um limite, ou `None` quando o cgroup diz `max` — que é «sem limite», não um número
/// grande.
fn limit(path: &Path, file: &str) -> Option<u64> {
    let text = read(path, file)?;
    let text = text.trim();
    if text == "max" {
        return None;
    }
    text.parse().ok()
}

/// Um campo `nome valor` de um arquivo de estatísticas do cgroup.
fn field(path: &Path, file: &str, name: &str) -> Option<u64> {
    let text = read(path, file)?;
    text.lines()
        .find_map(|line| line.strip_prefix(name)?.trim().parse().ok())
}

/// O teto de CPU, escrito como as ferramentas o escrevem: `1.5` núcleos, ou nada quando
/// `cpu.max` diz `max`.
fn cpu_quota(path: &Path) -> Option<String> {
    let text = read(path, "cpu.max")?;
    let mut parts = text.split_whitespace();
    let quota = parts.next()?;
    if quota == "max" {
        return None;
    }
    let quota: f64 = quota.parse().ok()?;
    let period: f64 = parts.next()?.parse().ok()?;
    if period <= 0.0 {
        return None;
    }
    Some(format!("{:.2} núcleo(s)", quota / period))
}

/// A linha `some` do PSI: a fração do tempo em que *alguma* tarefa do cgroup ficou
/// parada esperando o recurso.
fn pressure(path: &Path, file: &str) -> Option<Pressure> {
    let text = read(path, file)?;
    let line = text.lines().find(|line| line.starts_with("some "))?;
    let mut value = Pressure::default();
    for part in line.split_whitespace() {
        let Some((name, number)) = part.split_once('=') else {
            continue;
        };
        let Ok(number) = number.parse::<f64>() else {
            continue;
        };
        match name {
            "avg10" => value.avg10 = number,
            "avg60" => value.avg60 = number,
            "avg300" => value.avg300 = number,
            _ => {}
        }
    }
    Some(value)
}

/// Bytes recebidos e enviados por todas as interfaces do namespace de rede do container,
/// menos o loopback — tráfego de um processo consigo mesmo não é rede.
///
/// `/proc/<pid>/net/dev` **é** a tabela daquele namespace: ler o processo é ler o
/// namespace, sem `setns` e sem root para o caso comum. É a mesma descoberta que fez o
/// painel de conexões enxergar containers.
fn net_counters(pid: u32) -> (u64, u64) {
    let Ok(text) = fs::read_to_string(format!("/proc/{pid}/net/dev")) else {
        return (0, 0);
    };
    let mut rx = 0u64;
    let mut tx = 0u64;
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let mut fields = rest.split_whitespace();
        if let Some(value) = fields.next().and_then(|v| v.parse::<u64>().ok()) {
            rx += value;
        }
        // Recebidos: bytes, packets, errs, drop, fifo, frame, compressed, multicast.
        // Enviados começam no nono campo.
        if let Some(value) = fields.nth(7).and_then(|v| v.parse::<u64>().ok()) {
            tx += value;
        }
    }
    (rx, tx)
}

/// Os prefixos que as engines dão aos cgroups que criam. Reconhecidos todos, e não só o
/// da engine que hoje tem suporte: um cgroup é um cgroup, e esta metade do programa
/// nunca precisou saber quem o criou.
const PREFIXES: [&str; 4] = ["docker-", "libpod-", "crio-", "containerd-"];

/// Todo container que o cgroup mostra, com um pid dentro dele.
///
/// É o inventário do modo leitura: numa máquina cujo daemon é do root e cujo usuário não
/// está no grupo dele, o socket é negado e isto continua respondendo, porque os arquivos
/// de cgroup são legíveis por todos.
pub fn list() -> Vec<(String, u32)> {
    let mut found = Vec::new();
    fn scan(dir: &Path, depth: usize, found: &mut Vec<(String, u32)>) {
        if depth > MAX_DEPTH {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|k| k.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = container_id(&name) {
                let pid = first_pid(&entry.path());
                found.push((id, pid));
                // Sem descer: o que está dentro do cgroup de um container são as partes
                // dele, não outros containers.
                continue;
            }
            scan(&entry.path(), depth + 1, found);
        }
    }
    scan(Path::new(ROOT), 0, &mut found);
    found.sort();
    found.dedup_by(|a, b| a.0 == b.0);
    found
}

/// O id dentro de um nome de cgroup: `docker-<id>.scope`, `libpod-<id>`, `crio-<id>`.
fn container_id(name: &str) -> Option<String> {
    let id = PREFIXES
        .iter()
        .find_map(|prefix| name.strip_prefix(prefix))?
        .trim_end_matches(".scope");
    // Um id é hexadecimal e longo. Sem essa checagem, um serviço chamado
    // `docker-cleanup.service` entraria na lista como se fosse um container.
    (id.len() >= 12 && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| id.to_string())
}

fn first_pid(path: &Path) -> u32 {
    fs::read_to_string(path.join("cgroup.procs"))
        .ok()
        .and_then(|text| text.lines().next()?.trim().parse().ok())
        .unwrap_or(0)
}

/// Se esta máquina tem cgroups de container, seja qual for a engine que os criou.
///
/// É metade do teste que decide se a aba existe: mesmo sem conseguir falar com nenhum
/// daemon, uma máquina com containers rodando tem o que mostrar.
pub fn any_containers() -> bool {
    !list().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("monitorzinho-cgroup-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, content: &str) {
        let mut file = fs::File::create(dir.join(name)).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn memory_discounts_inactive_file_cache() {
        let dir = scratch("memoria");
        write(&dir, "memory.current", "25063424\n");
        write(&dir, "memory.stat", "anon 9170944\ninactive_file 3256320\n");
        // 25063424 − 3256320 = 21807104, que é o que as ferramentas de container mostram
        // para esse mesmo container.
        assert_eq!(memory_used(&dir), 21_807_104);
    }

    #[test]
    fn no_limit_reads_as_no_limit() {
        let dir = scratch("limite");
        write(&dir, "memory.max", "max\n");
        assert_eq!(limit(&dir, "memory.max"), None);
        write(&dir, "memory.max", "5368709120\n");
        assert_eq!(limit(&dir, "memory.max"), Some(5_368_709_120));
    }

    #[test]
    fn quota_reads_as_cores() {
        let dir = scratch("quota");
        write(&dir, "cpu.max", "max 100000\n");
        assert_eq!(cpu_quota(&dir), None);
        write(&dir, "cpu.max", "150000 100000\n");
        assert_eq!(cpu_quota(&dir), Some("1.50 núcleo(s)".to_string()));
    }

    #[test]
    fn pressure_reads_the_some_line() {
        let dir = scratch("pressao");
        write(
            &dir,
            "cpu.pressure",
            "some avg10=1.50 avg60=0.25 avg300=0.00 total=468666382\nfull avg10=9.99 avg60=9.99 avg300=9.99 total=1\n",
        );
        let value = pressure(&dir, "cpu.pressure").unwrap();
        assert_eq!(value.avg10, 1.50);
        assert_eq!(value.avg60, 0.25);
        assert_eq!(value.avg300, 0.00);
    }

    #[test]
    fn every_container_found_in_the_cgroup_tree_has_a_real_id() {
        // O inventário do modo leitura, conferido contra si mesmo. Numa máquina sem
        // containers a lista é vazia e o teste passa sem afirmar nada — que é o certo:
        // ele existe para garantir que, quando *há* o que achar, o que se acha é um
        // container e não um serviço com nome parecido.
        for (id, pid) in list() {
            assert!(id.len() >= 12, "id curto demais: {id}");
            assert!(
                id.chars().all(|c| c.is_ascii_hexdigit()),
                "id não é hexadecimal: {id}"
            );
            // Um pid zero é um cgroup sem processo dentro, que acontece com um container
            // que acabou de morrer; qualquer outro valor tem que ser um pid plausível.
            assert!(pid < 1 << 22, "pid implausível: {pid}");
        }
    }

    #[test]
    fn a_service_with_a_container_ish_name_is_not_a_container() {
        // `docker-cleanup.service` existe em máquinas de verdade. Sem a checagem de
        // «hexadecimal e longo» ele entraria na lista como se fosse um container.
        assert_eq!(container_id("docker-cleanup.service"), None);
        assert_eq!(container_id("docker-compose"), None);
        assert_eq!(
            container_id("docker-a5ac2aebeffcc97b39969506c19182029bc5b88d.scope"),
            Some("a5ac2aebeffcc97b39969506c19182029bc5b88d".to_string())
        );
        assert_eq!(
            container_id("libpod-a5ac2aebeffcc97b39969506c19182029bc5b88d"),
            Some("a5ac2aebeffcc97b39969506c19182029bc5b88d".to_string())
        );
    }

    #[test]
    fn stat_fields_are_read_by_name() {
        let dir = scratch("cpu");
        write(
            &dir,
            "cpu.stat",
            "usage_usec 2319215792\nuser_usec 1360881167\nnr_throttled 7\nthrottled_usec 1234\n",
        );
        assert_eq!(field(&dir, "cpu.stat", "usage_usec"), Some(2_319_215_792));
        assert_eq!(field(&dir, "cpu.stat", "nr_throttled"), Some(7));
    }
}
