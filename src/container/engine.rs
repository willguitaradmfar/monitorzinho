//! O trait que uma engine de container implementa, e como se acha uma nesta máquina.
//!
//! O ponto central é `actions()`: a engine declara o que sabe fazer com um sujeito, e a
//! UI monta o menu com o que voltar. Nenhuma tela conhece uma operação pelo nome. É o
//! mesmo idioma que o resto do programa já usa — `Tool::params()` declara o que o
//! formulário deve perguntar, `tools::offers_for` decide o que um achado vale pelo tipo
//! dele. Uma engine que não saiba pausar simplesmente não devolve a entrada, e nada
//! precisa mudar por causa disso.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::cgroup;
use super::http::Endpoint;
use super::{
    Action, ActionKey, Container, Image, LogSource, Network, StatsSource, Subject, Usage, Volume,
};
use crate::history;

/// Quem respondeu, e por onde.
#[derive(Clone, Debug)]
pub struct EngineInfo {
    /// Como o produto se chama — vindo dele mesmo, não escrito aqui.
    pub product: String,
    pub version: String,
    pub api_version: String,
    pub endpoint: Endpoint,
    /// Se atende num socket do usuário em vez do socket do sistema. Numa máquina com as
    /// duas coisas instaladas — e elas existem — saber qual respondeu é a diferença
    /// entre uma resposta e uma confusão.
    pub rootless: bool,
}

impl EngineInfo {
    /// A única linha da UI em que o nome de uma engine aparece.
    pub fn label(&self) -> String {
        let mut label = format!("{} {}", self.product, self.version);
        if self.rootless {
            label.push_str(" (rootless)");
        }
        if !self.endpoint.is_local() {
            label.push_str(&format!(" · {}", self.endpoint.to_url()));
        }
        label
    }

    /// A versão longa, para o resumo: também por onde se fala e que versão de API o
    /// daemon oferece.
    ///
    /// A versão dela importa porque a nossa é fixada: um daemon cuja versão mínima já
    /// passou da que pedimos falha em tudo, e aí este número é a primeira coisa a olhar.
    pub fn detail(&self) -> String {
        format!(
            "{} · API {} · {}",
            self.label(),
            self.api_version,
            self.endpoint.to_url()
        )
    }
}

pub trait ContainerEngine: Send + Sync {
    fn info(&self) -> &EngineInfo;

    fn containers(&self) -> Result<Vec<Container>, String>;
    fn volumes(&self) -> Result<Vec<Volume>, String>;
    fn images(&self) -> Result<Vec<Image>, String>;
    fn networks(&self) -> Result<Vec<Network>, String>;
    /// Tamanhos, que custam caro o bastante para nunca entrarem no caminho do inventário
    /// — ver `store`.
    fn usage(&self) -> Result<Usage, String>;

    /// Uma amostra dos números ao vivo pela API, para quando não há cgroup para ler.
    /// `None` na engine cujo `stats_source()` é o cgroup, que não precisa disto.
    fn api_stats(&self, _id: &str) -> Option<cgroup::Sample> {
        None
    }

    /// O que esta engine sabe fazer com este sujeito, na ordem em que o menu oferece.
    fn actions(&self, subject: &Subject) -> Vec<Action>;
    /// Executa. A mensagem de erro que volta é a da engine, sem tradução: quem sabe por
    /// que falhou é ela.
    fn perform(&self, action: ActionKey, subject: &Subject) -> Result<String, String>;
    /// Tudo que a engine sabe sobre o sujeito, como ela mesma escreve.
    fn inspect(&self, subject: &Subject) -> Result<String, String>;

    fn log_source(&self, container: &Container) -> LogSource;
    fn stats_source(&self) -> StatsSource;

    /// Abre um shell dentro do container, com terminal, no tamanho da janela de agora.
    ///
    /// O padrão recusa: uma engine que não saiba fazer isto não oferece a ação, e a
    /// mensagem daqui só apareceria se alguém contornasse o menu.
    fn open_shell(
        &self,
        _container: &Container,
        _size: (u16, u16),
    ) -> Result<super::exec::Session, String> {
        Err("esta engine não abre shell".to_string())
    }
}

// --- descoberta ----------------------------------------------------------------------

/// O endereço escolhido à mão, quando há um.
///
/// Fica ao lado de `tools.json`, pelo mesmo mecanismo e pela mesma razão: é
/// configuração, e configuração sobrevive ao fechamento do app.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Vazio significa «descubra sozinho», que é o padrão.
    #[serde(default)]
    pub endpoint: String,
}

fn settings_path() -> PathBuf {
    history::data_file("engine.json")
}

pub fn load_settings() -> Settings {
    match fs::read_to_string(settings_path()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Settings::default(),
    }
}

pub fn save_settings(settings: &Settings) {
    if let Ok(text) = serde_json::to_string_pretty(settings) {
        let _ = fs::write(settings_path(), text);
    }
}

/// Todo endereço que vale a pena tentar, na ordem em que vale.
///
/// O primeiro que responder vence. A ordem é a mesma que as ferramentas de linha de
/// comando usam, para que o app e elas concordem sobre com qual daemon se está falando.
pub fn candidates() -> Vec<Endpoint> {
    let mut list: Vec<Endpoint> = Vec::new();
    let mut push = |endpoint: Endpoint| {
        if !list.contains(&endpoint) {
            list.push(endpoint);
        }
    };

    let configured = load_settings().endpoint;
    if !configured.trim().is_empty()
        && let Ok(endpoint) = Endpoint::parse(&configured)
    {
        push(endpoint);
    }
    if let Some(host) = std::env::var_os("DOCKER_HOST")
        && let Ok(endpoint) = Endpoint::parse(&host.to_string_lossy())
    {
        push(endpoint);
    }
    if let Some(endpoint) = current_context() {
        push(endpoint);
    }

    // O socket do usuário mora no runtime dir, que o ambiente já nomeia — nenhuma
    // chamada de sistema para descobrir um uid que só serviria para remontar esse mesmo
    // caminho. Podman entra na lista porque atende uma API compatível: a segunda engine
    // é trocar o caminho do socket, não escrever outro cliente.
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        let runtime = PathBuf::from(runtime);
        paths.push(runtime.join("docker.sock"));
        paths.push(runtime.join("podman/podman.sock"));
    }
    paths.push(PathBuf::from("/var/run/docker.sock"));
    paths.push(PathBuf::from("/run/podman/podman.sock"));
    for path in paths {
        if path.exists() {
            push(Endpoint::Unix(path));
        }
    }
    list
}

/// O endereço do contexto ativo, do arquivo de configuração das ferramentas de linha de
/// comando.
///
/// **Só a chave do contexto é lida.** Esse mesmo arquivo guarda credenciais de registry
/// em texto claro, e nada aqui toca nelas, escreve-as em log ou as mostra na tela.
fn current_context() -> Option<Endpoint> {
    let home = std::env::var_os("HOME")?;
    let root = PathBuf::from(home).join(".docker");
    let config = fs::read_to_string(root.join("config.json")).ok()?;
    let config: serde_json::Value = serde_json::from_str(&config).ok()?;
    let name = config.get("currentContext")?.as_str()?;
    if name == "default" {
        return None;
    }
    // O diretório de cada contexto é o SHA-256 do nome, e procurar o `meta.json` que se
    // apresenta com o nome certo evita ter que calcular hash nenhum.
    let meta_root = root.join("contexts/meta");
    for entry in fs::read_dir(meta_root).ok()?.flatten() {
        let Ok(text) = fs::read_to_string(entry.path().join("meta.json")) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if meta.get("Name").and_then(|n| n.as_str()) != Some(name) {
            continue;
        }
        let host = meta
            .get("Endpoints")
            .and_then(|e| e.get("docker"))
            .and_then(|d| d.get("Host"))
            .and_then(|h| h.as_str())?;
        return Endpoint::parse(host).ok();
    }
    None
}

/// A primeira engine que responder, ou nada.
///
/// Nada não quer dizer «sem containers»: uma máquina pode ter cgroups de container
/// legíveis e o socket fechado — container de root visto de usuário fora do grupo. Quem
/// decide se a aba existe é `available()`, que pergunta as duas coisas.
pub fn discover() -> Option<Box<dyn ContainerEngine>> {
    for endpoint in candidates() {
        if let Some(engine) = super::docker::DockerEngine::probe(endpoint) {
            return Some(Box::new(engine));
        }
    }
    None
}

/// Containers vistos pelo cgroup, sem engine nenhuma para perguntar.
///
/// É o modo leitura: numa máquina onde o daemon é do root e quem roda o app não está no
/// grupo dele, o socket é negado mas os cgroups são legíveis — e uma aba que mostra o
/// consumo e diz por que não pode agir é melhor que uma aba ausente numa máquina que
/// está claramente rodando containers.
pub fn any_local_containers() -> bool {
    cgroup::any_containers()
}
