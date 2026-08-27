//! Onde os painéis leem, e onde nada bloqueia.
//!
//! Todo I/O de container acontece em threads de fundo; a UI lê um retrato sob mutex e
//! nunca espera por um socket. São três cadências, porque as três perguntas custam
//! coisas diferentes:
//!
//! * **Inventário e números ao vivo**, a cada segundo. A listagem custa quase nada e o
//!   cgroup de todos os containers custa cerca de 1 ms.
//! * **Tamanhos**, a cada minuto, numa thread só deles. A primeira medição custa 946 ms
//!   — medido —, e pagá-la no caminho do inventário atrasaria tudo por um número que
//!   muda devagar.
//! * **Ações**, uma thread por vez que uma acontece. Parar um container espera o
//!   processo sair sozinho, o que leva segundos; a linha diz «parando…» enquanto isso e
//!   a tela continua respondendo.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::cgroup;
use super::engine::{self, ContainerEngine};
use super::{Action, ActionKey, Container, Image, Network, StatsSource, Subject, Usage, Volume};

/// Com que frequência os containers e seus números ao vivo são relidos enquanto alguém
/// está olhando para eles.
///
/// Dois segundos, que é o mesmo tick do resto do programa. Ler uma vez por segundo
/// custava o dobro para deixar esta aba duas vezes mais fresca que todas as outras, o
/// que ninguém pediu — e o que de fato precisa ser instantâneo já é: uma ação avisa a
/// tela quando começa e quando termina, sem esperar volta nenhuma.
const REFRESH: Duration = Duration::from_secs(2);

/// Com que frequência são relidos quando nenhum painel de container está na tela.
///
/// Uma aba deste programa só custa enquanto está em foco — está escrito no comentário do
/// próprio `Tab`. Pesquisar a engine a cada segundo por trás da aba de gráficos seria
/// quebrar exatamente isso. Continua lendo devagar, e não parando de vez, para que
/// chegar na aba mostre números e não uma tela vazia enquanto a primeira volta acontece;
/// e a troca de aba acorda a thread na hora, então o atraso normal é zero.
const IDLE_REFRESH: Duration = Duration::from_secs(20);

/// De quantas em quantas voltas volumes, imagens e redes são relidos.
///
/// Medido: containers custam 12 ms por leitura, e volumes, imagens e redes somam outros
/// 24 ms. Só que containers mudam de estado o tempo todo, enquanto os outros três mudam
/// quando alguém faz um `pull`, cria um volume, sobe um projeto — escala de deployment,
/// não de segundo. É o mesmo raciocínio que o `netns::Watcher` já usa para reenumerar
/// namespaces a cada cinco segundos em vez de a cada tick.
const INVENTORY_EVERY: u32 = 10;

/// Com que frequência os tamanhos são remedidos.
const USAGE_REFRESH: Duration = Duration::from_secs(60);

/// O retrato que a UI lê. Trocado inteiro a cada volta, nunca editado no lugar.
#[derive(Default)]
pub struct Snapshot {
    pub containers: Vec<Container>,
    pub volumes: Vec<Volume>,
    pub images: Vec<Image>,
    pub networks: Vec<Network>,
    pub usage: Option<Usage>,
    /// Quando os tamanhos foram medidos. O painel mostra a idade: um número de 40 s
    /// atrás é útil; um número de 40 s atrás apresentado como atual não é.
    pub measured_at: Option<Instant>,
    /// O que a engine respondeu de errado na última tentativa, se respondeu.
    pub error: Option<String>,
    /// Ações em andamento, por id do sujeito — «parando…» na linha enquanto acontece.
    pub running: HashMap<String, String>,
    /// O resultado da última ação: a frase, e se deu certo. Fica até a próxima.
    pub outcome: Option<(String, bool)>,
}

impl Snapshot {
    /// Quantos containers em cada estado, para o painel de resumo.
    pub fn counts(&self) -> (usize, usize, usize) {
        let running = self.containers.iter().filter(|c| c.state.is_live()).count();
        let stopped = self.containers.len() - running;
        let orphan_volumes = self.volumes.iter().filter(|v| v.orphan()).count();
        (running, stopped, orphan_volumes)
    }
}

pub struct Store {
    engine: Option<Arc<dyn ContainerEngine>>,
    /// A versão longa, para o resumo — ver `EngineInfo::detail`.
    detail: Option<String>,
    snapshot: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    /// Se algum painel de container está na tela agora. Decide a cadência.
    watched: Arc<AtomicBool>,
    /// Como a thread é acordada antes da hora — na troca para a aba, para que o primeiro
    /// quadro dela já tenha números.
    wake: Arc<(Mutex<bool>, Condvar)>,
    /// Sobe a cada mudança publicada, para a UI saber que vale redesenhar sem comparar
    /// listas inteiras.
    revision: Arc<std::sync::atomic::AtomicU64>,
    label: Option<String>,
}

impl Store {
    /// Procura uma engine e, achando ou não, decide se há aba.
    ///
    /// A descoberta é síncrona: é um `/_ping` por endereço que existe, 12 ms medidos, e
    /// o resultado decide se a aba entra na barra — coisa que precisa estar resolvida
    /// antes do primeiro quadro.
    pub fn start() -> Option<Self> {
        let engine = engine::discover().map(Arc::from);
        // Sem engine, ainda pode haver containers: um daemon do root visto de quem não
        // está no grupo dele nega o socket, mas os cgroups continuam legíveis. A aba
        // aparece em modo leitura em vez de sumir numa máquina que roda containers.
        if engine.is_none() && !engine::any_local_containers() {
            return None;
        }
        let label = engine
            .as_ref()
            .map(|e: &Arc<dyn ContainerEngine>| e.info().label());
        let detail = engine
            .as_ref()
            .map(|e: &Arc<dyn ContainerEngine>| e.info().detail());

        let store = Self {
            detail,
            engine,
            snapshot: Arc::new(Mutex::new(Snapshot::default())),
            stop: Arc::new(AtomicBool::new(false)),
            watched: Arc::new(AtomicBool::new(true)),
            wake: Arc::new((Mutex::new(false), Condvar::new())),
            revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            label,
        };
        store.spawn_inventory();
        store.spawn_usage();
        Some(store)
    }

    /// Como a engine se apresenta, ou por que não há nenhuma. É a única linha da UI em
    /// que o nome de uma engine aparece.
    pub fn engine_label(&self) -> String {
        match &self.label {
            Some(label) => label.clone(),
            None => "sem engine — só o que o cgroup mostra".to_string(),
        }
    }

    /// Como a engine se apresenta por extenso: também a versão de API que ela oferece e
    /// por onde se fala com ela. É o que a linha «Engine» do resumo mostra, e o primeiro
    /// lugar a olhar quando as chamadas começam a falhar.
    pub fn engine_detail(&self) -> String {
        match &self.detail {
            Some(detail) => detail.clone(),
            None => "nenhuma — Enter para apontar para um endereço".to_string(),
        }
    }

    /// Diz se algum painel de container está na tela, o que decide a cadência da thread.
    /// Acorda a thread quando passa a estar: chegar na aba não pode custar uma tela vazia
    /// enquanto a primeira volta acontece.
    pub fn set_watched(&self, watched: bool) {
        if self.watched.swap(watched, Ordering::Relaxed) == watched {
            return;
        }
        if watched {
            let (lock, condvar) = &*self.wake;
            if let Ok(mut flag) = lock.lock() {
                *flag = true;
                condvar.notify_all();
            }
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// Lê o retrato. Nunca bloqueia por I/O — no pior caso espera outra thread soltar o
    /// mutex, que é o tempo de trocar quatro vetores.
    pub fn read<T>(&self, f: impl FnOnce(&Snapshot) -> T) -> T {
        match self.snapshot.lock() {
            Ok(snapshot) => f(&snapshot),
            // Um mutex envenenado é uma thread que morreu no meio da publicação. O
            // retrato de dentro dele ainda é o último bom.
            Err(poisoned) => f(&poisoned.into_inner()),
        }
    }

    /// O que se pode fazer com este sujeito, perguntado à engine.
    pub fn actions(&self, subject: &Subject) -> Vec<Action> {
        match &self.engine {
            Some(engine) => engine.actions(subject),
            None => Vec::new(),
        }
    }

    /// Executa numa thread e volta na hora. A linha do sujeito passa a dizer o que está
    /// acontecendo, e o resultado aparece quando chegar.
    pub fn perform(&self, action: ActionKey, subject: Subject, verb: &str) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let key = subject_key(&subject);
        let snapshot = self.snapshot.clone();
        let revision = self.revision.clone();
        if let Ok(mut current) = snapshot.lock() {
            current.running.insert(key.clone(), verb.to_string());
            current.outcome = None;
        }
        revision.fetch_add(1, Ordering::Relaxed);

        thread::spawn(move || {
            let result = engine.perform(action, &subject);
            if let Ok(mut current) = snapshot.lock() {
                current.running.remove(&key);
                current.outcome = Some(match result {
                    Ok(message) => (message, true),
                    // A mensagem da engine, sem tradução: quem sabe por que falhou é ela.
                    Err(message) => (message, false),
                });
            }
            revision.fetch_add(1, Ordering::Relaxed);
        });
    }

    /// Escreve uma frase na linha do resultado, para o que aconteceu fora de uma ação —
    /// um shell que não abriu, por exemplo.
    pub fn report(&self, message: String, ok: bool) {
        if let Ok(mut current) = self.snapshot.lock() {
            current.outcome = Some((message, ok));
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
    }

    /// Abre um shell dentro do container. Síncrono: quem pediu está olhando para a tela
    /// e o que vem a seguir é o terminal inteiro virando dele.
    pub fn open_shell(
        &self,
        container: &Container,
        size: (u16, u16),
    ) -> Result<super::exec::Session, String> {
        match &self.engine {
            Some(engine) => engine.open_shell(container, size),
            None => Err("nenhuma engine respondeu — não há como abrir um shell".to_string()),
        }
    }

    /// De onde sai o log deste container. Sem engine não há resposta: o caminho do
    /// arquivo é coisa que a engine escreve, não que o kernel saiba.
    pub fn log_source(&self, container: &Container) -> super::LogSource {
        match &self.engine {
            Some(engine) => engine.log_source(container),
            None => super::LogSource::Unavailable(
                "nenhuma engine respondeu — não sei onde fica o log".to_string(),
            ),
        }
    }

    /// Tudo que a engine sabe sobre o sujeito, como ela mesma escreve. Síncrono de
    /// propósito: é uma chamada só, pedida por quem está olhando para a tela.
    pub fn inspect(&self, subject: &Subject) -> Result<String, String> {
        match &self.engine {
            Some(engine) => engine.inspect(subject),
            None => Err("nenhuma engine respondeu nesta máquina".to_string()),
        }
    }

    fn spawn_inventory(&self) {
        let engine = self.engine.clone();
        let snapshot = self.snapshot.clone();
        let stop = self.stop.clone();
        let watched = self.watched.clone();
        let wake = self.wake.clone();
        let revision = self.revision.clone();
        thread::spawn(move || {
            let mut reader = cgroup::Reader::default();
            let mut round: u32 = 0;
            while !stop.load(Ordering::Relaxed) {
                // Volumes, imagens e redes só de vez em quando; containers sempre. Os
                // três primeiros custam o dobro dos containers juntos e mudam quando
                // alguém faz um deploy, não a cada segundo.
                let full = round.is_multiple_of(INVENTORY_EVERY);
                round = round.wrapping_add(1);
                let mut next = match &engine {
                    Some(engine) => collect(engine.as_ref(), &mut reader, full),
                    None => collect_from_cgroup(&mut reader),
                };
                // O que a volta anterior já sabia e esta não descobre de novo: tamanhos
                // são de outra thread, e uma ação em andamento é de outra ainda.
                if let Ok(current) = snapshot.lock() {
                    next.usage = current.usage.clone();
                    next.measured_at = current.measured_at;
                    next.running = current.running.clone();
                    next.outcome = current.outcome.clone();
                    // Numa volta curta os três não foram relidos: o que já estava é o que
                    // continua valendo, e recruzá-los com os containers de agora mantém a
                    // coluna «usado por» certa mesmo entre leituras.
                    if !full {
                        next.volumes = current.volumes.clone();
                        next.images = current.images.clone();
                        next.networks = current.networks.clone();
                        cross_reference(&mut next);
                    }
                }
                apply_sizes(&mut next);
                if let Ok(mut current) = snapshot.lock() {
                    *current = next;
                }
                revision.fetch_add(1, Ordering::Relaxed);

                let interval = if watched.load(Ordering::Relaxed) {
                    REFRESH
                } else {
                    IDLE_REFRESH
                };
                // Dormir num condvar, e não num `sleep`: é o que deixa a troca de aba
                // cortar a espera longa em vez de esperar os vinte segundos inteiros.
                let (lock, condvar) = &*wake;
                if let Ok(flag) = lock.lock()
                    && let Ok((mut flag, _)) = condvar.wait_timeout(flag, interval)
                {
                    *flag = false;
                }
            }
        });
    }

    fn spawn_usage(&self) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let snapshot = self.snapshot.clone();
        let stop = self.stop.clone();
        let watched = self.watched.clone();
        let wake = self.wake.clone();
        let revision = self.revision.clone();
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                // A primeira medição sempre acontece, para a aba já abrir com tamanhos.
                // Depois disso só enquanto alguém está olhando: um número que custa quase
                // um segundo de daemon não vale ser remedido para ninguém.
                let measured = snapshot.lock().is_ok_and(|s| s.measured_at.is_some());
                if (!measured || watched.load(Ordering::Relaxed))
                    && let Ok(usage) = engine.usage()
                {
                    if let Ok(mut current) = snapshot.lock() {
                        current.usage = Some(usage);
                        current.measured_at = Some(Instant::now());
                        apply_sizes(&mut current);
                    }
                    revision.fetch_add(1, Ordering::Relaxed);
                }
                // No mesmo condvar da outra thread: dormir um minuto inteiro em vez de
                // acordar sessenta vezes para conferir uma flag é o que deixa o processo
                // realmente parado entre as medições.
                let (lock, condvar) = &*wake;
                if let Ok(flag) = lock.lock() {
                    let _ = condvar.wait_timeout(flag, USAGE_REFRESH);
                }
            }
        });
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Acordado para reparar que parou: sem isto, trocar o endereço da engine deixaria
        // a thread antiga dormindo até vinte segundos antes de sair.
        let (lock, condvar) = &*self.wake;
        if let Ok(mut flag) = lock.lock() {
            *flag = true;
            condvar.notify_all();
        }
    }
}

/// A chave pela qual uma ação em andamento é achada de volta na linha certa.
pub fn subject_key(subject: &Subject) -> String {
    match subject {
        Subject::Container(c) => c.id.clone(),
        Subject::Volume(v) => v.name.clone(),
        Subject::Image(i) => i.id.clone(),
        Subject::Network(n) => n.id.clone(),
    }
}

/// Uma volta com engine: containers e números ao vivo sempre; volumes, imagens e redes
/// só quando `full` — ver `INVENTORY_EVERY`.
fn collect(engine: &dyn ContainerEngine, reader: &mut cgroup::Reader, full: bool) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let containers = engine.containers();
    match containers {
        Ok(containers) => snapshot.containers = containers,
        Err(error) => {
            snapshot.error = Some(error);
            return snapshot;
        }
    }
    if full {
        snapshot.volumes = engine.volumes().unwrap_or_default();
        snapshot.images = engine.images().unwrap_or_default();
        snapshot.networks = engine.networks().unwrap_or_default();
    }

    let alive: Vec<String> = snapshot.containers.iter().map(|c| c.id.clone()).collect();
    for container in &mut snapshot.containers {
        if !container.state.is_live() {
            continue;
        }
        let reading = match engine.stats_source() {
            StatsSource::Cgroup => reader.read(&container.id, container.pid),
            StatsSource::Api => engine
                .api_stats(&container.id)
                .map(|sample| reader.rate(&container.id, sample)),
        };
        if let Some(reading) = reading {
            apply_reading(container, reading);
        }
    }
    reader.retain(&alive);
    if full {
        cross_reference(&mut snapshot);
    }
    snapshot
}

/// Uma volta sem engine: só o que o cgroup mostra.
///
/// É o modo leitura. Não há nome, imagem nem estado — nada disso está no kernel — mas o
/// consumo está, e um painel que mostra consumo e diz o que não sabe é melhor que uma
/// aba ausente.
fn collect_from_cgroup(reader: &mut cgroup::Reader) -> Snapshot {
    let found = cgroup::list();
    let mut snapshot = Snapshot {
        error: Some(
            "nenhuma engine respondeu — sem nome, imagem nem ações; só o que o cgroup mostra"
                .to_string(),
        ),
        ..Snapshot::default()
    };
    let alive: Vec<String> = found.iter().map(|(id, _)| id.clone()).collect();
    for (id, pid) in found {
        let mut container = Container {
            name: id.chars().take(12).collect(),
            id,
            pid,
            state: super::ContainerState::Running,
            ..Container::default()
        };
        if let Some(reading) = reader.read(&container.id, container.pid) {
            apply_reading(&mut container, reading);
        }
        snapshot.containers.push(container);
    }
    reader.retain(&alive);
    snapshot.containers.sort_by(|a, b| {
        b.cpu_percent
            .unwrap_or(0.0)
            .total_cmp(&a.cpu_percent.unwrap_or(0.0))
    });
    snapshot
}

fn apply_reading(container: &mut Container, reading: cgroup::Reading) {
    let sample = reading.sample;
    container.cpu_percent = reading.cpu_percent;
    container.memory = Some(sample.memory);
    container.memory_limit = sample.memory_limit;
    container.memory_peak = sample.memory_peak;
    container.pids = sample.pids;
    container.cpu_quota = sample.cpu_quota;
    container.throttled = sample.throttled;
    container.oom_kills = sample.oom_kills;
    container.cpu_pressure = sample.cpu_pressure;
    container.memory_pressure = sample.memory_pressure;
    container.io_pressure = sample.io_pressure;
    container.net_rx = Some(sample.net_rx);
    container.net_tx = Some(sample.net_tx);
    container.net_rx_rate = reading.net_rx_rate;
    container.net_tx_rate = reading.net_tx_rate;
}

/// Quem usa o quê.
///
/// A engine não responde isto: a listagem de volumes devolve uma contagem de referências
/// e a de redes devolve `null` no campo dos containers. Quem sabe é a lista de
/// containers, que nomeia as montagens e as redes de cada um — então o cruzamento é
/// feito aqui, uma vez, em vez de uma chamada de inspeção por volume e por rede.
fn cross_reference(snapshot: &mut Snapshot) {
    let mut by_volume: HashMap<&str, Vec<String>> = HashMap::new();
    let mut by_network: HashMap<&str, Vec<String>> = HashMap::new();
    let mut by_image: HashMap<&str, Vec<String>> = HashMap::new();
    for container in &snapshot.containers {
        let name = container.display_name();
        for volume in &container.volumes {
            by_volume.entry(volume).or_default().push(name.clone());
        }
        for network in &container.networks {
            by_network.entry(network).or_default().push(name.clone());
        }
        by_image
            .entry(container.image_id.as_str())
            .or_default()
            .push(name.clone());
    }
    for volume in &mut snapshot.volumes {
        volume.used_by = by_volume
            .get(volume.name.as_str())
            .cloned()
            .unwrap_or_default();
    }
    for network in &mut snapshot.networks {
        network.used_by = by_network
            .get(network.name.as_str())
            .cloned()
            .unwrap_or_default();
    }
    for image in &mut snapshot.images {
        image.used_by = by_image.get(image.id.as_str()).cloned().unwrap_or_default();
    }
    // Órfãos por último, para o painel poder ordenar pelo que se pode limpar.
    snapshot.volumes.sort_by(|a, b| {
        a.orphan()
            .cmp(&b.orphan())
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// Cola os tamanhos medidos nos volumes que a última volta trouxe.
fn apply_sizes(snapshot: &mut Snapshot) {
    let Some(usage) = &snapshot.usage else {
        return;
    };
    let sizes: HashMap<&str, u64> = usage
        .volumes
        .iter()
        .map(|(name, size)| (name.as_str(), *size))
        .collect();
    for volume in &mut snapshot.volumes {
        volume.size = sizes.get(volume.name.as_str()).copied();
    }
}

/// O total ocupado pelos volumes já medidos, e quanto disso está órfão.
pub fn volume_totals(snapshot: &Snapshot) -> (u64, u64, usize) {
    let mut total = 0;
    let mut orphan = 0;
    let mut orphans = 0;
    for volume in &snapshot.volumes {
        let size = volume.size.unwrap_or(0);
        total += size;
        if volume.orphan() {
            orphan += size;
            orphans += 1;
        }
    }
    (total, orphan, orphans)
}

/// Os totais de imagens: quanto ocupam e quantas estão soltas.
pub fn image_totals(images: &[Image]) -> (u64, usize) {
    (
        images.iter().map(|image| image.size).sum(),
        images.iter().filter(|image| image.dangling()).count(),
    )
}

/// Um volume por nome, para quando o menu de ações precisa do sujeito completo.
pub fn volume_named<'a>(snapshot: &'a Snapshot, name: &str) -> Option<&'a Volume> {
    snapshot.volumes.iter().find(|v| v.name == name)
}
