//! Containers, volumes, imagens e redes — o vocabulário, sem engine nenhuma dentro.
//!
//! Nada aqui menciona Docker. Docker é a primeira engine a ter suporte, não a única
//! prevista: `docker.rs` implementa `ContainerEngine` e é o único arquivo que sabe
//! como uma API de container se parece. Os painéis conhecem só estes tipos, e os ids
//! que vão para o disco (`containers`, `volumes`, `images`, `networks`) são genéricos
//! de propósito — trocar de engine não pode invalidar as marcas de quem já usa.
//!
//! A metade que *mede* já era agnóstica antes disto existir: cgroup, PSI e
//! `/proc/<pid>/net/dev` são do kernel, e `monitor::netns` já reconhecia
//! `docker-<id>.scope`, `libpod-<id>` e `crio-<id>`. Só o inventário e as ações
//! precisavam de um trait.

pub mod cgroup;
pub mod docker;
pub mod engine;
pub mod exec;
pub mod http;
pub mod store;

pub use engine::{ContainerEngine, EngineInfo};
pub use store::Store;

/// Em que ponto da vida um container está.
///
/// `Other` guarda o que a engine disse sem tentar traduzir: uma engine futura com um
/// estado que não existe aqui aparece com o nome dela em vez de virar `Unknown`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContainerState {
    Running,
    Paused,
    Restarting,
    Created,
    Exited,
    Dead,
    Removing,
    Other(String),
}

impl ContainerState {
    pub fn from_str(text: &str) -> Self {
        match text.trim().to_ascii_lowercase().as_str() {
            "running" => Self::Running,
            "paused" => Self::Paused,
            "restarting" => Self::Restarting,
            "created" => Self::Created,
            "exited" | "stopped" => Self::Exited,
            "dead" => Self::Dead,
            "removing" => Self::Removing,
            other => Self::Other(other.to_string()),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Running => "em execução",
            Self::Paused => "pausado",
            Self::Restarting => "reiniciando",
            Self::Created => "criado",
            Self::Exited => "parado",
            Self::Dead => "morto",
            Self::Removing => "removendo",
            Self::Other(text) => text,
        }
    }

    /// Se o container tem processos rodando agora — o que decide se há cgroup para ler
    /// e quais ações fazem sentido.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Running | Self::Paused | Self::Restarting)
    }

    /// Ordem de exibição: o que está no ar primeiro, o que morreu por último. Um painel
    /// de containers é lido de cima, e o que está rodando é o que se olha.
    pub fn rank(&self) -> u8 {
        match self {
            Self::Running => 0,
            Self::Restarting => 1,
            Self::Paused => 2,
            Self::Created => 3,
            Self::Exited => 4,
            Self::Removing => 5,
            Self::Dead => 6,
            Self::Other(_) => 7,
        }
    }
}

/// Uma porta publicada: dentro do container, e onde ela aparece no host.
#[derive(Clone, Debug)]
pub struct PortMap {
    pub container_port: u16,
    pub protocol: String,
    pub host_ip: Option<String>,
    pub host_port: Option<u16>,
}

impl PortMap {
    /// Como a porta se lê numa célula: `0.0.0.0:5432→5432/tcp`, ou só `5432/tcp` quando
    /// nada a publica no host.
    pub fn label(&self) -> String {
        match (&self.host_ip, self.host_port) {
            (Some(ip), Some(port)) => {
                // `::` e `0.0.0.0` dizem a mesma coisa e ocupam espaço que a porta quer.
                let ip = if ip == "0.0.0.0" || ip == "::" {
                    String::new()
                } else {
                    format!("{ip}:")
                };
                format!("{ip}{port}→{}/{}", self.container_port, self.protocol)
            }
            _ => format!("{}/{}", self.container_port, self.protocol),
        }
    }
}

/// Pressão do cgroup (PSI): quanto tempo o container passou parado esperando um recurso.
///
/// Existe só no modo local — é um arquivo do kernel desta máquina. Responde *por que* um
/// container está lento, que é a pergunta que uso de CPU sozinho nunca responde.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pressure {
    pub avg10: f64,
    pub avg60: f64,
    pub avg300: f64,
}

/// Um container, do jeito que os painéis o conhecem.
///
/// Os campos `Option` são todos a mesma decisão: um traço diz «não medido aqui», um zero
/// diria «não aconteceu». Num endpoint remoto não há cgroup, e as linhas que só o cgroup
/// responde somem em vez de aparecerem zeradas.
#[derive(Clone, Debug, Default)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image: String,
    /// O id da imagem, que é como o cruzamento com o painel de imagens é feito.
    ///
    /// Pelo id e não pelo nome: a engine escreve o nome como o container foi criado
    /// (`docker.io/chromedp/headless-shell:latest`) e a imagem como a tag dela
    /// (`chromedp/headless-shell:latest`), e casar essas duas strings exigiria conhecer
    /// as regras de nome de todo registro que existe. O id é o mesmo dos dois lados.
    pub image_id: String,
    pub command: String,
    pub state: ContainerState,
    /// O que a engine escreve como estado por extenso — `Up 5 days (healthy)`.
    pub status: String,
    pub health: Option<String>,
    /// Criação, em segundos desde a época.
    pub created: i64,
    pub started_at: String,
    /// Quando subiu, em segundos desde a época — `started_at` já resolvido, para o painel
    /// não ter que reparsear uma data por linha a cada volta. `0` quando nunca subiu.
    pub started: i64,
    pub finished_at: String,
    pub restart_count: u64,
    pub exit_code: i64,
    pub oom_killed: bool,
    /// Projeto e serviço do compose, quando os rótulos os carregam. É o que agrupa a
    /// tabela em árvore.
    pub project: Option<String>,
    pub service: Option<String>,
    pub ports: Vec<PortMap>,
    /// Nomes dos volumes montados (montagens de caminho do host não entram: elas não
    /// são volumes e o painel de volumes não as conhece).
    pub volumes: Vec<String>,
    pub networks: Vec<String>,
    pub ip: Option<String>,
    /// Pid do processo principal no host, quando a engine é local. `0` quando não há.
    pub pid: u32,
    pub log_path: Option<String>,

    // --- ao vivo -------------------------------------------------------------------
    pub cpu_percent: Option<f64>,
    pub memory: Option<u64>,
    pub memory_limit: Option<u64>,
    pub memory_peak: Option<u64>,
    pub pids: Option<u64>,
    pub cpu_quota: Option<String>,
    pub net_rx: Option<u64>,
    pub net_tx: Option<u64>,
    pub net_rx_rate: Option<f64>,
    pub net_tx_rate: Option<f64>,
    /// `(nr_throttled, throttled_usec)` — quantas vezes o cgroup bateu no teto de CPU.
    pub throttled: Option<(u64, u64)>,
    pub oom_kills: Option<u64>,
    pub cpu_pressure: Option<Pressure>,
    pub memory_pressure: Option<Pressure>,
    pub io_pressure: Option<Pressure>,
}

impl Default for ContainerState {
    fn default() -> Self {
        Self::Other(String::new())
    }
}

impl Container {
    /// Os doze caracteres que toda engine imprime como identidade curta, e que é o que
    /// uma pessoa compara.
    pub fn short_id(&self) -> String {
        self.id.chars().take(12).collect()
    }

    /// Como o container se chama numa célula: o nome quando há, o id curto quando não.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.short_id()
        } else {
            self.name.clone()
        }
    }

    /// Estado mais saúde, que é como uma pessoa lê a coluna: `em execução (healthy)`.
    pub fn state_label(&self) -> String {
        match (&self.health, &self.state) {
            (Some(health), _) if !health.is_empty() => {
                format!("{} ({health})", self.state.label())
            }
            (_, ContainerState::Exited) if self.oom_killed => "parado (OOM)".to_string(),
            (_, ContainerState::Exited) => format!("parado ({})", self.exit_code),
            _ => self.state.label().to_string(),
        }
    }
}

/// Um volume, com o que o cruzamento com os containers descobriu sobre ele.
#[derive(Clone, Debug, Default)]
pub struct Volume {
    pub name: String,
    pub driver: String,
    pub mountpoint: String,
    /// Segundos desde a época, ou 0 quando a engine não datou o volume.
    pub created: i64,
    pub project: Option<String>,
    /// Nomes dos containers que o montam. Vazio é um volume órfão — a engine devolve
    /// só uma contagem, e uma contagem não diz quem.
    pub used_by: Vec<String>,
    /// Bytes em disco, quando a medição já chegou. `None` enquanto não chegou: a
    /// primeira medição custa quase um segundo e um zero no lugar dela seria mentira.
    pub size: Option<u64>,
}

impl Volume {
    pub fn orphan(&self) -> bool {
        self.used_by.is_empty()
    }
}

/// Uma imagem.
#[derive(Clone, Debug, Default)]
pub struct Image {
    pub id: String,
    /// `repositório:tag`, ou `<none>:<none>` para uma imagem solta.
    pub tags: Vec<String>,
    pub size: u64,
    pub created: i64,
    /// Nomes dos containers que a usam, do cruzamento por id — a engine responde `-1`
    /// nesse campo, que quer dizer «não contei», e uma contagem que não existe não pode
    /// virar zero.
    pub used_by: Vec<String>,
}

impl Image {
    pub fn display_name(&self) -> String {
        match self.tags.first() {
            Some(tag) if tag != "<none>:<none>" => tag.clone(),
            _ => format!("<sem tag> {}", self.short_id()),
        }
    }

    pub fn short_id(&self) -> String {
        self.id
            .strip_prefix("sha256:")
            .unwrap_or(&self.id)
            .chars()
            .take(12)
            .collect()
    }

    /// Uma imagem sem tag nenhuma é lixo de build acumulado. Ter tag e não estar em uso
    /// é outra coisa — e as duas não podem se chamar do mesmo jeito na tela.
    pub fn dangling(&self) -> bool {
        self.tags.is_empty() || self.tags.iter().all(|tag| tag == "<none>:<none>")
    }
}

/// Uma rede.
#[derive(Clone, Debug, Default)]
pub struct Network {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub scope: String,
    pub internal: bool,
    pub subnet: Option<String>,
    pub created: i64,
    pub project: Option<String>,
    pub used_by: Vec<String>,
}

impl Network {
    /// As redes que toda engine cria sozinha. Não são lixo, e oferecer removê-las seria
    /// oferecer um erro.
    pub fn builtin(&self) -> bool {
        matches!(self.name.as_str(), "bridge" | "host" | "none" | "podman")
    }
}

/// O que os tamanhos custam caro para responder, medido por fora do inventário.
#[derive(Clone, Debug, Default)]
pub struct Usage {
    pub volumes: Vec<(String, u64)>,
    pub images_size: u64,
    pub containers_size: u64,
    pub build_cache: u64,
}

/// Sobre o que uma ação age. O menu do `Enter` é montado a partir disto.
#[derive(Clone, Debug)]
pub enum Subject {
    Container(Box<Container>),
    Volume(Volume),
    Image(Image),
    Network(Network),
}

impl Subject {
    /// Como o sujeito se chama numa caixa de confirmação.
    pub fn name(&self) -> String {
        match self {
            Subject::Container(c) => c.display_name(),
            Subject::Volume(v) => v.name.clone(),
            Subject::Image(i) => i.display_name(),
            Subject::Network(n) => n.name.clone(),
        }
    }

    /// O substantivo, para um título que precisa dizer de que tipo de coisa se trata.
    pub fn kind(&self) -> &'static str {
        match self {
            Subject::Container(_) => "container",
            Subject::Volume(_) => "volume",
            Subject::Image(_) => "imagem",
            Subject::Network(_) => "rede",
        }
    }
}

/// Quanto atrito uma ação exige antes de acontecer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gravity {
    /// Executa direto. Reversível: iniciar, parar, reiniciar, pausar.
    Safe,
    /// Uma caixa dizendo o que se perde, confirmada com Enter.
    Confirm,
    /// Exige digitar o nome. Perda de dado irreversível: remover volume, limpar órfãos.
    Typed,
}

/// O que uma engine sabe fazer com um sujeito.
///
/// A UI não conhece nenhuma ação por nome: monta o menu com o que `actions()` devolver.
/// Uma engine que não saiba pausar simplesmente não devolve a entrada, e nada na tela
/// precisa mudar por causa disso.
#[derive(Clone, Debug)]
pub struct Action {
    /// Chave estável da operação, para o código decidir o que fazer sem ler rótulo.
    pub key: ActionKey,
    pub label: String,
    pub gravity: Gravity,
    /// O que a caixa de confirmação diz que vai acontecer, uma consequência por linha.
    /// Vazio para as ações que não param para perguntar.
    pub consequences: Vec<String>,
    /// Por que não dá agora, quando não dá. Um item explicado vale mais que um item
    /// ausente: «remover — 2 containers ainda usam» ensina, some não ensina nada.
    pub blocked: Option<String>,
}

/// As operações que existem. Uma engine oferece as que souber; nenhuma precisa oferecer
/// todas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKey {
    // Container
    Logs,
    Start,
    Stop,
    Restart,
    Pause,
    Unpause,
    Kill,
    RemoveContainer,
    Details,
    Inspect,
    /// Abrir um shell dentro do container, no terminal que já está aqui.
    Shell,
    // Volume
    RemoveVolume,
    PruneVolumes,
    // Imagem
    RemoveImage,
    ForceRemoveImage,
    PruneImages,
    // Rede
    RemoveNetwork,
    PruneNetworks,
}

/// De onde sai o log de um container.
pub enum LogSource {
    /// O arquivo que a engine escreve. Seguir o arquivo dá busca, filtro, hex e rolagem
    /// de horas — tudo que o seguidor de arquivo já faz, sem uma linha nova.
    File(String),
    /// Nenhum arquivo legível daqui: driver de log que não escreve em disco, ou engine
    /// remota.
    Unavailable(String),
}

/// De onde saem os números que se mexem.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatsSource {
    /// cgroup e `/proc` desta máquina: tudo, inclusive PSI e throttling, por ~1 ms.
    Cgroup,
    /// A API da engine, uma amostra por container. Sem PSI e sem throttling.
    Api,
}
