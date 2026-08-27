//! A primeira engine com suporte.
//!
//! É o único arquivo do projeto que sabe como uma API de container se parece. Tudo
//! acima dele fala em `Container`, `Volume`, `Image`, `Network` — trocar de engine é
//! escrever outro destes, não mexer nos painéis.
//!
//! Fala HTTP à mão no socket (ver `http.rs`), com a versão de API fixada: perseguir a
//! mais nova é trocar uma dependência de crate por uma dependência de daemon. A `v1.44`
//! é aceita por tudo que ainda recebe correção, e nada que esta aba pede é novo.
//!
//! Podman atende uma API compatível noutro socket, e por isso a descoberta já o procura:
//! a segunda engine é trocar o caminho, não escrever outro cliente.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::cgroup;
use super::http::{self, Endpoint};
use super::{
    Action, ActionKey, Container, ContainerState, Gravity, Image, LogSource, Network, PortMap,
    StatsSource, Subject, Usage, Volume,
};
use super::{ContainerEngine, EngineInfo};

/// Versão de API pedida em toda chamada. Fixada de propósito — ver o cabeçalho.
const API: &str = "/v1.44";

/// Prazo de uma chamada. Generoso para um socket local e curto o bastante para que um
/// daemon remoto que parou de responder não segure a thread que o consultou.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Prazo de uma sondagem, que é bem mais curto que o de uma chamada de verdade.
///
/// A sondagem acontece em dois lugares onde alguém está esperando: no arranque, antes do
/// primeiro quadro, e na caixa do endereço, com a pessoa olhando para ela. Dez segundos
/// de tela parada porque um endereço não responde é tempo demais para uma pergunta cuja
/// resposta certa chega em milissegundos.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Parar um container espera o processo sair sozinho antes de o matar, e essa espera é
/// da engine, não nossa — mas o prazo da requisição tem que caber nela.
const ACTION_TIMEOUT: Duration = Duration::from_secs(40);

/// Quanto se espera pela primeira palavra do fluxo de um shell recém-iniciado.
///
/// Medido, ela chega em ~60 ms — o prompt de um shell que subiu, ou a mensagem de erro de
/// um que não subiu. Um segundo é folga para uma máquina carregada, e só é gasto por
/// inteiro por um shell que sobe calado, que é raro num terminal.
const FIRST_WORD: Duration = Duration::from_secs(1);

/// Os shells tentados, na ordem em que se prefere um.
///
/// `bash` é o mais confortável e falta na maioria das imagens enxutas, que é o que mais
/// roda em produção; `sh` existe em quase tudo; `/bin/sh` pelo caminho absoluto cobre a
/// imagem cujo `PATH` está vazio, onde procurar pelo nome não acha nada.
const SHELLS: [&str; 3] = ["bash", "sh", "/bin/sh"];

/// De quanto em quanto tempo os campos que só a inspeção responde são relidos.
///
/// A listagem já traz estado, saúde, portas, montagens e redes de uma vez e custa quase
/// nada. Reinícios, código de saída, pid e caminho do log só vêm da inspeção, uma
/// chamada por container — então ficam em cache e são relidos quando o estado muda ou
/// quando envelhecem. Em regime, isso é praticamente nenhuma chamada.
const INSPECT_TTL: Duration = Duration::from_secs(30);

struct Inspected {
    at: Instant,
    state: ContainerState,
    pid: u32,
    restart_count: u64,
    exit_code: i64,
    oom_killed: bool,
    started_at: String,
    finished_at: String,
    log_path: Option<String>,
    log_driver: String,
}

pub struct DockerEngine {
    info: EngineInfo,
    inspected: Mutex<HashMap<String, Inspected>>,
}

impl DockerEngine {
    /// Pergunta ao endereço se há alguém ali. Só volta uma engine se houver.
    pub fn probe(endpoint: Endpoint) -> Option<Self> {
        let response = http::request(
            &endpoint,
            "GET",
            &format!("{API}/version"),
            None,
            PROBE_TIMEOUT,
        )
        .ok()
        .filter(|r| r.status == 200)?;
        let version: Value = serde_json::from_slice(&response.body).ok()?;

        // O produto se apresenta; não escrevemos o nome dele aqui. Podman responde
        // "Podman Engine" no mesmo campo, e é assim que a nota do painel fica certa
        // sozinha quando a engine muda.
        let product = text(&version, "Platform")
            .and_then(|p| {
                serde_json::from_str::<Value>(&p)
                    .ok()
                    .and_then(|v| text(&v, "Name"))
            })
            .or_else(|| {
                version
                    .get("Platform")
                    .and_then(|p| p.get("Name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "Engine".to_string());

        let rootless = match &endpoint {
            // Um socket dentro do runtime dir do usuário é, por construção, do usuário.
            Endpoint::Unix(path) => path.to_string_lossy().contains("/run/user/"),
            Endpoint::Tcp { .. } => false,
        };

        Some(Self {
            info: EngineInfo {
                product: product
                    .split_whitespace()
                    .next()
                    .unwrap_or("Engine")
                    .to_string(),
                version: text(&version, "Version").unwrap_or_default(),
                api_version: text(&version, "ApiVersion").unwrap_or_default(),
                endpoint,
                rootless,
            },
            inspected: Mutex::new(HashMap::new()),
        })
    }

    fn get(&self, path: &str) -> Result<Value, String> {
        let response = http::request(
            &self.info.endpoint,
            "GET",
            &format!("{API}{path}"),
            None,
            TIMEOUT,
        )?;
        if response.status != 200 {
            return Err(response.error_message());
        }
        serde_json::from_slice(&response.body)
            .map_err(|e| format!("resposta não é JSON válido: {e}"))
    }

    /// Uma chamada que muda alguma coisa. Devolve o texto da engine quando ela diz algo,
    /// ou uma confirmação curta quando responde só um código.
    fn act(&self, method: &str, path: &str) -> Result<String, String> {
        let response = http::request(
            &self.info.endpoint,
            method,
            &format!("{API}{path}"),
            None,
            ACTION_TIMEOUT,
        )?;
        match response.status {
            200..=299 => Ok(response.text().trim().to_string()),
            // 304 é a engine dizendo que já estava assim. Não é erro, e tratá-lo como um
            // faria «parar» falhar num container já parado.
            304 => Ok("já estava nesse estado".to_string()),
            _ => Err(response.error_message()),
        }
    }

    /// Os campos que só a inspeção responde, relidos quando envelhecem ou quando o
    /// estado do container mudou.
    fn inspect_cached(&self, id: &str, state: &ContainerState) -> Option<()> {
        let fresh = {
            let cache = self.inspected.lock().ok()?;
            cache
                .get(id)
                .is_some_and(|entry| entry.at.elapsed() < INSPECT_TTL && &entry.state == state)
        };
        if fresh {
            return Some(());
        }
        let value = self.get(&format!("/containers/{id}/json")).ok()?;
        let state_value = value.get("State");
        let entry = Inspected {
            at: Instant::now(),
            state: state.clone(),
            pid: state_value
                .and_then(|s| s.get("Pid"))
                .and_then(|p| p.as_u64())
                .unwrap_or(0) as u32,
            restart_count: value
                .get("RestartCount")
                .and_then(|r| r.as_u64())
                .unwrap_or(0),
            exit_code: state_value
                .and_then(|s| s.get("ExitCode"))
                .and_then(|c| c.as_i64())
                .unwrap_or(0),
            oom_killed: state_value
                .and_then(|s| s.get("OOMKilled"))
                .and_then(|o| o.as_bool())
                .unwrap_or(false),
            started_at: state_value
                .and_then(|s| text(s, "StartedAt"))
                .unwrap_or_default(),
            finished_at: state_value
                .and_then(|s| text(s, "FinishedAt"))
                .unwrap_or_default(),
            log_path: text(&value, "LogPath").filter(|p| !p.is_empty()),
            log_driver: value
                .get("HostConfig")
                .and_then(|h| h.get("LogConfig"))
                .and_then(|l| text(l, "Type"))
                .unwrap_or_default(),
        };
        self.inspected.lock().ok()?.insert(id.to_string(), entry);
        Some(())
    }

    /// Uma tentativa de shell: cria a execução, inicia trocando de protocolo, e devolve
    /// o fluxo cru por onde o terminal vai falar.
    fn start_shell(
        &self,
        container: &Container,
        shell: &str,
        (cols, rows): (u16, u16),
    ) -> Result<super::exec::Session, String> {
        // `Tty: true` é o que faz o fluxo vir sem multiplexação: saída e erro misturados,
        // que é o que um terminal é. Sem isso cada bloco viria com oito bytes de cabeçalho
        // na frente e a tela encheria de lixo.
        //
        // `ConsoleSize` é o tamanho da janela **na criação**, e é a única forma de o
        // primeiro prompt sair certo. A chamada de redimensionar só funciona depois que a
        // execução começou: pedida antes disso, a engine simplesmente não responde e
        // segura a conexão até o prazo estourar — dez segundos entre apertar Enter e ver
        // o shell, que era exatamente o sintoma.
        let create = format!(
            r#"{{"AttachStdin":true,"AttachStdout":true,"AttachStderr":true,"Tty":true,"ConsoleSize":[{rows},{cols}],"Cmd":["{shell}"]}}"#
        );
        let response = http::request(
            &self.info.endpoint,
            "POST",
            &format!("{API}/containers/{}/exec", container.id),
            Some(&create),
            TIMEOUT,
        )?;
        if !(200..300).contains(&response.status) {
            return Err(response.error_message());
        }
        let value: Value = serde_json::from_slice(&response.body)
            .map_err(|e| format!("resposta não é JSON válido: {e}"))?;
        let id = text(&value, "Id").ok_or("a engine não devolveu o id da execução")?;

        let mut stream = http::upgrade(
            &self.info.endpoint,
            &format!("{API}/exec/{id}/start"),
            r#"{"Detach":false,"Tty":true}"#,
            TIMEOUT,
        )?;

        // O `101` não quer dizer que o shell subiu.
        //
        // Um comando que não existe no container é aceito na criação — a engine não
        // confere — e o *upgrade* também dá certo: ela responde `101 UPGRADED`, escreve
        // «executable file not found in $PATH» no fluxo e fecha. Quem só olha o código de
        // status vê sucesso, entrega um fluxo já morto, e a tela volta no mesmo instante
        // sem dizer por quê. Era exatamente esse o sintoma numa imagem sem `bash`.
        //
        // A inspeção da execução responde na hora e sem corrida: `Running: false` com
        // código 127 é «não achei o programa».
        // A primeira palavra do fluxo é o que separa um shell que subiu de um que não
        // subiu, e é preciso esperar por ela — ver `exec_failed`.
        let mut greeting = Vec::new();
        let _ = stream.set_read_timeout(FIRST_WORD);
        let mut chunk = [0u8; 4096];
        let closed = match std::io::Read::read(&mut stream, &mut chunk) {
            // Fechou sem dizer nada: não subiu.
            Ok(0) => true,
            Ok(n) => {
                greeting.extend_from_slice(&chunk[..n]);
                false
            }
            // Calado até aqui. Não é veredito: quem decide é a inspeção, logo abaixo.
            Err(_) => false,
        };
        if let Some(failure) = self.exec_failed(&id, closed, &greeting) {
            return Err(failure);
        }

        Ok(super::exec::Session {
            stream,
            endpoint: self.info.endpoint.clone(),
            id,
            shell: shell.to_string(),
            greeting,
        })
    }

    /// Por que a execução não está de pé, ou `None` se estiver.
    ///
    /// Perguntar isto **logo depois** do `101` não funciona: o runtime ainda não decidiu,
    /// a inspeção responde «rodando», e um shell que não existe passa por bom. A corrida
    /// se fecha sozinha esperando a primeira palavra do fluxo — medido, ela chega em
    /// ~60 ms nos dois casos, e nesse ponto o runtime já reportou o que aconteceu.
    ///
    /// Um fluxo que fechou sem dizer nada já é resposta suficiente. Quando disse algo, o
    /// que ele disse é a explicação boa — «executable file not found in $PATH» vale muito
    /// mais que um número — e é ela que volta.
    fn exec_failed(&self, id: &str, closed: bool, greeting: &[u8]) -> Option<String> {
        if !closed {
            // Uma inspeção que não responde não pode passar por «está tudo bem»: se não
            // dá para saber, o shell segue, e o relay descobre — em vez de o programa
            // decidir sozinho que falhou.
            let state = self.get(&format!("/exec/{id}/json")).ok()?;
            if state.get("Running").and_then(|r| r.as_bool()) != Some(false) {
                return None;
            }
        }
        let said = String::from_utf8_lossy(greeting);
        let said = said.trim();
        Some(if said.is_empty() {
            "o processo saiu sem dizer nada".to_string()
        } else {
            said.to_string()
        })
    }

    fn apply_inspected(&self, container: &mut Container) {
        self.inspect_cached(&container.id, &container.state);
        let Ok(cache) = self.inspected.lock() else {
            return;
        };
        let Some(entry) = cache.get(&container.id) else {
            return;
        };
        container.pid = entry.pid;
        container.restart_count = entry.restart_count;
        container.exit_code = entry.exit_code;
        container.oom_killed = entry.oom_killed;
        container.started_at = entry.started_at.clone();
        container.finished_at = entry.finished_at.clone();
        container.log_path = entry.log_path.clone();
    }
}

impl ContainerEngine for DockerEngine {
    fn info(&self) -> &EngineInfo {
        &self.info
    }

    fn containers(&self) -> Result<Vec<Container>, String> {
        let value = self.get("/containers/json?all=1")?;
        let list = value
            .as_array()
            .ok_or("listagem de containers inesperada")?;
        let mut containers: Vec<Container> = list.iter().map(read_container).collect();
        for container in &mut containers {
            self.apply_inspected(container);
        }
        // Os que estão no ar primeiro, depois os que consomem mais. Um painel de
        // containers é lido de cima, e o que está rodando é o que se olha.
        containers.sort_by(|a, b| {
            a.state
                .rank()
                .cmp(&b.state.rank())
                .then_with(|| b.created.cmp(&a.created))
                .then_with(|| a.name.cmp(&b.name))
        });
        // Um cache que nunca esquece é um vazamento lento numa máquina onde containers
        // vão e vêm.
        if let Ok(mut cache) = self.inspected.lock() {
            cache.retain(|id, _| containers.iter().any(|c| &c.id == id));
        }
        Ok(containers)
    }

    fn volumes(&self) -> Result<Vec<Volume>, String> {
        let value = self.get("/volumes")?;
        let list = value
            .get("Volumes")
            .and_then(|v| v.as_array())
            .ok_or("listagem de volumes inesperada")?;
        let mut volumes: Vec<Volume> = list
            .iter()
            .map(|item| Volume {
                name: text(item, "Name").unwrap_or_default(),
                driver: text(item, "Driver").unwrap_or_default(),
                mountpoint: text(item, "Mountpoint").unwrap_or_default(),
                created: text(item, "CreatedAt")
                    .and_then(|t| parse_time(&t))
                    .unwrap_or(0),
                project: label(item, "com.docker.compose.project"),
                used_by: Vec::new(),
                size: None,
            })
            .collect();
        volumes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(volumes)
    }

    fn images(&self) -> Result<Vec<Image>, String> {
        let value = self.get("/images/json")?;
        let list = value.as_array().ok_or("listagem de imagens inesperada")?;
        let mut images: Vec<Image> = list
            .iter()
            .map(|item| Image {
                id: text(item, "Id").unwrap_or_default(),
                tags: item
                    .get("RepoTags")
                    .and_then(|t| t.as_array())
                    .map(|tags| {
                        tags.iter()
                            .filter_map(|t| t.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                size: item.get("Size").and_then(|s| s.as_u64()).unwrap_or(0),
                created: item.get("Created").and_then(|c| c.as_i64()).unwrap_or(0),
                // Sem ler `Containers`: a engine responde -1 ali, que quer dizer «não
                // contei». Quem sabe quem usa o quê é a lista de containers, e o
                // cruzamento é feito uma vez em `store::cross_reference`.
                used_by: Vec::new(),
            })
            .collect();
        images.sort_by_key(|image| std::cmp::Reverse(image.size));
        Ok(images)
    }

    fn networks(&self) -> Result<Vec<Network>, String> {
        let value = self.get("/networks")?;
        let list = value.as_array().ok_or("listagem de redes inesperada")?;
        let mut networks: Vec<Network> = list
            .iter()
            .map(|item| Network {
                id: text(item, "Id").unwrap_or_default(),
                name: text(item, "Name").unwrap_or_default(),
                driver: text(item, "Driver").unwrap_or_default(),
                scope: text(item, "Scope").unwrap_or_default(),
                internal: item
                    .get("Internal")
                    .and_then(|i| i.as_bool())
                    .unwrap_or(false),
                subnet: item
                    .get("IPAM")
                    .and_then(|i| i.get("Config"))
                    .and_then(|c| c.as_array())
                    .and_then(|c| c.first())
                    .and_then(|c| text(c, "Subnet")),
                created: text(item, "Created")
                    .and_then(|t| parse_time(&t))
                    .unwrap_or(0),
                project: label(item, "com.docker.compose.project"),
                used_by: Vec::new(),
            })
            .collect();
        // As embutidas por último: existem em toda máquina e nunca são a resposta.
        networks.sort_by(|a, b| {
            a.builtin()
                .cmp(&b.builtin())
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(networks)
    }

    fn usage(&self) -> Result<Usage, String> {
        let value = self.get("/system/df")?;
        let volumes = value
            .get("Volumes")
            .and_then(|v| v.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|item| {
                        let name = text(item, "Name")?;
                        let size = item
                            .get("UsageData")
                            .and_then(|u| u.get("Size"))
                            .and_then(|s| s.as_i64())
                            .unwrap_or(0)
                            .max(0) as u64;
                        Some((name, size))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let sum = |key: &str, field: &str| -> u64 {
            value
                .get(key)
                .and_then(|v| v.as_array())
                .map(|list| {
                    list.iter()
                        .filter_map(|item| item.get(field).and_then(|s| s.as_i64()))
                        .map(|n| n.max(0) as u64)
                        .sum()
                })
                .unwrap_or(0)
        };
        let layers = value
            .get("LayersSize")
            .and_then(|s| s.as_i64())
            .unwrap_or(0)
            .max(0) as u64;
        Ok(Usage {
            images_size: layers,
            containers_size: sum("Containers", "SizeRw"),
            build_cache: sum("BuildCache", "Size"),
            volumes,
        })
    }

    fn api_stats(&self, id: &str) -> Option<cgroup::Sample> {
        // `one-shot=true` é obrigatório: sem ele o daemon coleta *duas* amostras para
        // calcular o CPU% no nosso lugar e a chamada custa um segundo inteiro por
        // container. Medido. Com ele custa 7 ms, e o delta é nosso — como o do painel de
        // conexões.
        let value = self
            .get(&format!(
                "/containers/{id}/stats?stream=false&one-shot=true"
            ))
            .ok()?;
        let cpu = value.get("cpu_stats")?.get("cpu_usage")?;
        let memory = value.get("memory_stats");
        let inactive = memory
            .and_then(|m| m.get("stats"))
            .and_then(|s| s.get("inactive_file"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let (rx, tx) = value
            .get("networks")
            .and_then(|n| n.as_object())
            .map(|nets| {
                nets.values().fold((0u64, 0u64), |(rx, tx), net| {
                    (
                        rx + net.get("rx_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                        tx + net.get("tx_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
                    )
                })
            })
            .unwrap_or((0, 0));
        Some(cgroup::Sample {
            // A API conta nanossegundos; o cgroup conta microssegundos. O resto do
            // programa fala microssegundo.
            usage_usec: cpu.get("total_usage").and_then(|v| v.as_u64()).unwrap_or(0) / 1_000,
            memory: memory
                .and_then(|m| m.get("usage"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
                .saturating_sub(inactive),
            memory_limit: memory
                .and_then(|m| m.get("limit"))
                .and_then(|v| v.as_u64())
                .filter(|&limit| limit > 0),
            pids: value
                .get("pids_stats")
                .and_then(|p| p.get("current"))
                .and_then(|v| v.as_u64()),
            net_rx: rx,
            net_tx: tx,
            // PSI, throttling e pico de memória são arquivos do kernel desta máquina. Um
            // daemon remoto administra containers que não estão aqui, e inventar zeros
            // para eles seria dizer «não aconteceu» onde a verdade é «não medido».
            ..cgroup::Sample::default()
        })
    }

    fn stats_source(&self) -> StatsSource {
        if self.info.endpoint.is_local() {
            StatsSource::Cgroup
        } else {
            StatsSource::Api
        }
    }

    fn log_source(&self, container: &Container) -> LogSource {
        let driver = self
            .inspected
            .lock()
            .ok()
            .and_then(|cache| cache.get(&container.id).map(|e| e.log_driver.clone()))
            .unwrap_or_default();
        match &container.log_path {
            // Seguir o arquivo em vez do fluxo da API dá busca, filtro, hex e rolagem de
            // horas — tudo que o seguidor de arquivo já faz, sem uma linha nova.
            Some(path) if !path.is_empty() && std::path::Path::new(path).exists() => {
                LogSource::File(path.clone())
            }
            _ if driver.is_empty() => {
                LogSource::Unavailable("o log deste container não está em arquivo".to_string())
            }
            _ => LogSource::Unavailable(format!(
                "o driver de log «{driver}» não escreve um arquivo que dê para seguir"
            )),
        }
    }

    /// Abre um shell, tentando o mais confortável primeiro.
    ///
    /// Em cascata porque nem toda imagem tem `bash` — as enxutas, que é o que mais roda
    /// em produção, têm só `sh`. `/bin/sh` no fim cobre a imagem cujo `PATH` está vazio.
    fn open_shell(
        &self,
        container: &Container,
        size: (u16, u16),
    ) -> Result<super::exec::Session, String> {
        let mut reasons: Vec<String> = Vec::new();
        for shell in SHELLS {
            match self.start_shell(container, shell, size) {
                Ok(session) => return Ok(session),
                Err(error) => reasons.push(format!("{shell}: {error}")),
            }
        }
        Err(format!(
            "nenhum shell abriu neste container.\n\n{}",
            reasons.join("\n")
        ))
    }

    fn inspect(&self, subject: &Subject) -> Result<String, String> {
        let path = match subject {
            Subject::Container(c) => format!("/containers/{}/json", c.id),
            Subject::Volume(v) => format!("/volumes/{}", v.name),
            Subject::Image(i) => format!("/images/{}/json", i.id),
            Subject::Network(n) => format!("/networks/{}", n.id),
        };
        let value = self.get(&path)?;
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
    }

    fn actions(&self, subject: &Subject) -> Vec<Action> {
        match subject {
            Subject::Container(c) => container_actions(c),
            Subject::Volume(v) => volume_actions(v),
            Subject::Image(i) => image_actions(i),
            Subject::Network(n) => network_actions(n),
        }
    }

    fn perform(&self, action: ActionKey, subject: &Subject) -> Result<String, String> {
        match (action, subject) {
            (ActionKey::Start, Subject::Container(c)) => {
                self.act("POST", &format!("/containers/{}/start", c.id))?;
                Ok(format!("«{}» iniciado", c.display_name()))
            }
            (ActionKey::Stop, Subject::Container(c)) => {
                self.act("POST", &format!("/containers/{}/stop", c.id))?;
                Ok(format!("«{}» parado", c.display_name()))
            }
            (ActionKey::Restart, Subject::Container(c)) => {
                self.act("POST", &format!("/containers/{}/restart", c.id))?;
                Ok(format!("«{}» reiniciado", c.display_name()))
            }
            (ActionKey::Pause, Subject::Container(c)) => {
                self.act("POST", &format!("/containers/{}/pause", c.id))?;
                Ok(format!("«{}» pausado", c.display_name()))
            }
            (ActionKey::Unpause, Subject::Container(c)) => {
                self.act("POST", &format!("/containers/{}/unpause", c.id))?;
                Ok(format!("«{}» retomado", c.display_name()))
            }
            (ActionKey::Kill, Subject::Container(c)) => {
                self.act("POST", &format!("/containers/{}/kill", c.id))?;
                Ok(format!("«{}» morto", c.display_name()))
            }
            (ActionKey::RemoveContainer, Subject::Container(c)) => {
                self.act("DELETE", &format!("/containers/{}", c.id))?;
                Ok(format!("«{}» removido", c.display_name()))
            }
            (ActionKey::RemoveVolume, Subject::Volume(v)) => {
                self.act("DELETE", &format!("/volumes/{}", v.name))?;
                Ok(format!("volume «{}» removido", v.name))
            }
            (ActionKey::PruneVolumes, _) => {
                // `all=true` porque sem ele a engine só considera volumes anônimos, e
                // «limpar os órfãos» que deixa órfãos para trás é uma promessa quebrada.
                let body = self.act(
                    "POST",
                    "/volumes/prune?filters=%7B%22all%22%3A%5B%22true%22%5D%7D",
                )?;
                Ok(pruned(&body, "volume"))
            }
            (ActionKey::RemoveImage, Subject::Image(i)) => {
                self.act("DELETE", &format!("/images/{}", i.id))?;
                Ok(format!("imagem «{}» removida", i.display_name()))
            }
            (ActionKey::ForceRemoveImage, Subject::Image(i)) => {
                self.act("DELETE", &format!("/images/{}?force=true", i.id))?;
                Ok(format!("imagem «{}» removida à força", i.display_name()))
            }
            (ActionKey::PruneImages, _) => {
                let body = self.act("POST", "/images/prune")?;
                Ok(pruned(&body, "imagem"))
            }
            (ActionKey::RemoveNetwork, Subject::Network(n)) => {
                self.act("DELETE", &format!("/networks/{}", n.id))?;
                Ok(format!("rede «{}» removida", n.name))
            }
            (ActionKey::PruneNetworks, _) => {
                let body = self.act("POST", "/networks/prune")?;
                Ok(pruned(&body, "rede"))
            }
            // As locais nunca chegam aqui: são resolvidas no app, não na engine.
            (action, subject) => Err(format!("«{action:?}» não se aplica a {}", subject.kind())),
        }
    }
}

/// O que uma limpeza devolveu, dito em números em vez de em JSON.
fn pruned(body: &str, noun: &str) -> String {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return format!("{noun}s limpos");
    };
    let count = ["VolumesDeleted", "ImagesDeleted", "NetworksDeleted"]
        .iter()
        .filter_map(|key| value.get(key))
        .filter_map(|v| v.as_array())
        .map(|list| list.len())
        .sum::<usize>();
    let freed = value
        .get("SpaceReclaimed")
        .and_then(|s| s.as_u64())
        .unwrap_or(0);
    if freed > 0 {
        format!(
            "{count} {noun}(s) removido(s), {} liberados",
            crate::format::human_bytes(freed as f64)
        )
    } else {
        format!("{count} {noun}(s) removido(s)")
    }
}

// --- o que se pode fazer com cada coisa ----------------------------------------------

fn action(key: ActionKey, label: &str, gravity: Gravity) -> Action {
    Action {
        key,
        label: label.to_string(),
        gravity,
        consequences: Vec::new(),
        blocked: None,
    }
}

fn container_actions(c: &Container) -> Vec<Action> {
    let name = c.display_name();
    let mut actions = vec![action(
        ActionKey::Details,
        "ver detalhes completos",
        Gravity::Safe,
    )];

    match c.state {
        ContainerState::Running | ContainerState::Restarting => {
            actions.push(action(ActionKey::Stop, "parar", Gravity::Safe));
            actions.push(action(ActionKey::Restart, "reiniciar", Gravity::Safe));
            actions.push(action(ActionKey::Pause, "pausar", Gravity::Safe));
            actions.push(Action {
                consequences: vec![
                    format!("«{name}» recebe SIGKILL, sem chance de encerrar direito."),
                    "Trabalho em andamento e dados não gravados se perdem.".to_string(),
                ],
                ..action(ActionKey::Kill, "matar (SIGKILL)", Gravity::Confirm)
            });
        }
        ContainerState::Paused => {
            actions.push(action(ActionKey::Unpause, "retomar", Gravity::Safe));
            actions.push(action(ActionKey::Stop, "parar", Gravity::Safe));
        }
        _ => {
            actions.push(action(ActionKey::Start, "iniciar", Gravity::Safe));
        }
    }

    actions.push(match &c.log_path {
        Some(_) => action(ActionKey::Logs, "ver logs", Gravity::Safe),
        None => Action {
            blocked: Some("sem arquivo de log legível daqui".to_string()),
            ..action(ActionKey::Logs, "ver logs", Gravity::Safe)
        },
    });
    if c.state == ContainerState::Running {
        actions.push(action(
            ActionKey::Shell,
            "abrir um shell dentro dele",
            Gravity::Safe,
        ));
    }
    actions.push(action(
        ActionKey::Inspect,
        "inspecionar (JSON)",
        Gravity::Safe,
    ));

    let mut remove = Action {
        consequences: vec![
            format!("O container «{name}» deixa de existir."),
            "Volumes nomeados não são apagados; o que estava só no sistema de arquivos dele, sim."
                .to_string(),
        ],
        ..action(ActionKey::RemoveContainer, "remover", Gravity::Confirm)
    };
    // Removível à força existiria, mas «pare antes» é mais honesto que uma remoção que
    // derruba o que está no ar sem dizer que era isso que ia acontecer.
    if c.state.is_live() {
        remove.blocked = Some("está em execução — pare antes".to_string());
    }
    actions.push(remove);
    actions
}

fn volume_actions(v: &Volume) -> Vec<Action> {
    let mut remove = Action {
        consequences: vec![
            format!("Os dados dentro de «{}» são apagados para sempre.", v.name),
            "Não há como desfazer.".to_string(),
        ],
        ..action(ActionKey::RemoveVolume, "remover volume", Gravity::Typed)
    };
    if !v.used_by.is_empty() {
        remove.blocked = Some(format!(
            "{} container(s) ainda usam: {}",
            v.used_by.len(),
            v.used_by.join(", ")
        ));
    }
    vec![
        action(ActionKey::Inspect, "inspecionar (JSON)", Gravity::Safe),
        remove,
        Action {
            consequences: vec![
                "Todo volume que nenhum container usa é apagado, com o conteúdo.".to_string(),
                "Inclui volumes de projetos que estão só parados, não removidos.".to_string(),
                "Não há como desfazer.".to_string(),
            ],
            ..action(
                ActionKey::PruneVolumes,
                "limpar volumes órfãos",
                Gravity::Typed,
            )
        },
    ]
}

fn image_actions(i: &Image) -> Vec<Action> {
    let name = i.display_name();
    let mut remove = Action {
        consequences: vec![format!("A imagem «{name}» sai do disco.")],
        ..action(ActionKey::RemoveImage, "remover imagem", Gravity::Confirm)
    };
    if !i.used_by.is_empty() {
        remove.blocked = Some(format!(
            "{} container(s) a usam: {}",
            i.used_by.len(),
            i.used_by.join(", ")
        ));
    }
    vec![
        action(ActionKey::Inspect, "inspecionar (JSON)", Gravity::Safe),
        remove,
        Action {
            consequences: vec![
                format!("A imagem «{name}» sai do disco mesmo com containers a usando."),
                "Containers que dependem dela não sobem de novo sem baixá-la outra vez."
                    .to_string(),
            ],
            ..action(
                ActionKey::ForceRemoveImage,
                "remover à força",
                Gravity::Typed,
            )
        },
        Action {
            consequences: vec![
                "Toda imagem sem tag e sem container é apagada.".to_string(),
                "Camadas de build acumuladas vão junto.".to_string(),
            ],
            ..action(
                ActionKey::PruneImages,
                "limpar imagens soltas",
                Gravity::Typed,
            )
        },
    ]
}

fn network_actions(n: &Network) -> Vec<Action> {
    let mut remove = Action {
        consequences: vec![format!("A rede «{}» deixa de existir.", n.name)],
        ..action(ActionKey::RemoveNetwork, "remover rede", Gravity::Confirm)
    };
    if n.builtin() {
        remove.blocked = Some("é uma rede embutida da engine".to_string());
    } else if !n.used_by.is_empty() {
        remove.blocked = Some(format!("{} container(s) conectados", n.used_by.len()));
    }
    vec![
        action(ActionKey::Inspect, "inspecionar (JSON)", Gravity::Safe),
        remove,
        Action {
            consequences: vec!["Toda rede sem container conectado é removida.".to_string()],
            ..action(
                ActionKey::PruneNetworks,
                "limpar redes vazias",
                Gravity::Typed,
            )
        },
    ]
}

// --- leitura do JSON da engine -------------------------------------------------------

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_string)
}

fn label(value: &Value, name: &str) -> Option<String> {
    let text = value.get("Labels")?.get(name)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn read_container(item: &Value) -> Container {
    let state = ContainerState::from_str(&text(item, "State").unwrap_or_default());
    let networks = item
        .get("NetworkSettings")
        .and_then(|n| n.get("Networks"))
        .and_then(|n| n.as_object());
    Container {
        id: text(item, "Id").unwrap_or_default(),
        // A engine guarda o nome com uma barra na frente, que nada mais mostra.
        name: item
            .get("Names")
            .and_then(|n| n.as_array())
            .and_then(|n| n.first())
            .and_then(|n| n.as_str())
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string(),
        image: text(item, "Image").unwrap_or_default(),
        image_id: text(item, "ImageID").unwrap_or_default(),
        command: text(item, "Command").unwrap_or_default(),
        status: text(item, "Status").unwrap_or_default(),
        // A saúde vem separada na listagem e entre parênteses no estado por extenso;
        // preferimos o campo, que não precisa ser lido de dentro de uma frase.
        health: item
            .get("Health")
            .and_then(|h| h.as_str())
            .map(str::to_string)
            .or_else(|| health_from_status(&text(item, "Status").unwrap_or_default()))
            .filter(|h| !h.is_empty()),
        created: item.get("Created").and_then(|c| c.as_i64()).unwrap_or(0),
        project: label(item, "com.docker.compose.project"),
        service: label(item, "com.docker.compose.service"),
        ports: read_ports(item),
        // Só volumes nomeados: uma montagem de caminho do host não é um volume e o
        // painel de volumes não a conhece.
        volumes: item
            .get("Mounts")
            .and_then(|m| m.as_array())
            .map(|mounts| {
                mounts
                    .iter()
                    .filter(|m| text(m, "Type").as_deref() == Some("volume"))
                    .filter_map(|m| text(m, "Name"))
                    .collect()
            })
            .unwrap_or_default(),
        networks: networks
            .map(|n| n.keys().cloned().collect())
            .unwrap_or_default(),
        ip: networks.and_then(|n| {
            n.values()
                .filter_map(|net| text(net, "IPAddress"))
                .find(|ip| !ip.is_empty())
        }),
        state,
        ..Container::default()
    }
}

/// A saúde escondida dentro de `Up 5 days (healthy)`, para as engines que não a mandam
/// como campo próprio.
fn health_from_status(status: &str) -> Option<String> {
    let start = status.find('(')?;
    let end = status[start..].find(')')? + start;
    let inside = status[start + 1..end].trim();
    matches!(
        inside,
        "healthy" | "unhealthy" | "health: starting" | "starting"
    )
    .then(|| inside.replace("health: ", ""))
}

fn read_ports(item: &Value) -> Vec<PortMap> {
    let Some(list) = item.get("Ports").and_then(|p| p.as_array()) else {
        return Vec::new();
    };
    let mut ports: Vec<PortMap> = list
        .iter()
        .filter_map(|port| {
            Some(PortMap {
                container_port: port.get("PrivatePort")?.as_u64()? as u16,
                protocol: text(port, "Type").unwrap_or_else(|| "tcp".to_string()),
                host_ip: text(port, "IP").filter(|ip| !ip.is_empty()),
                host_port: port
                    .get("PublicPort")
                    .and_then(|p| p.as_u64())
                    .map(|p| p as u16),
            })
        })
        .collect();
    // A mesma porta aparece uma vez por família de endereço; a lista fica o dobro do
    // tamanho dizendo a mesma coisa duas vezes.
    ports.sort_by_key(|p| (p.container_port, p.host_port));
    ports.dedup_by_key(|p| (p.container_port, p.host_port, p.protocol.clone()));
    ports
}

/// Um instante em RFC 3339 como segundos desde a época.
///
/// Escrito à mão porque é a única data que este programa precisa entender, e uma crate
/// de calendário para converter um carimbo por container é uma dependência inteira pelo
/// que cabe em vinte linhas. Fuso é respeitado; frações de segundo são descartadas,
/// porque nada aqui mostra menos que um segundo.
fn parse_time(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() || text.starts_with("0001-01-01") {
        return None;
    }
    let bytes = text.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |range: std::ops::Range<usize>| -> Option<i64> { text.get(range)?.parse().ok() };
    let (year, month, day) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (hour, minute, second) = (num(11..13)?, num(14..16)?, num(17..19)?);

    // Dias desde a época pelo algoritmo de calendário civil de Howard Hinnant: a
    // aritmética de ano bissexto sem tabela e sem laço.
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    let mut stamp = days * 86_400 + hour * 3_600 + minute * 60 + second;

    // O fuso, quando não é `Z`: o deslocamento é o que se soma para chegar ao UTC, então
    // entra com o sinal trocado.
    let tail = &text[19..];
    if let Some(sign) = tail.rfind(['+', '-']) {
        let offset = &tail[sign..];
        if offset.len() >= 6
            && let (Some(hours), Some(minutes)) = (
                offset.get(1..3).and_then(|h| h.parse::<i64>().ok()),
                offset.get(4..6).and_then(|m| m.parse::<i64>().ok()),
            )
        {
            let delta = hours * 3_600 + minutes * 60;
            stamp += if offset.starts_with('-') {
                delta
            } else {
                -delta
            };
        }
    }
    Some(stamp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rfc3339_with_and_without_offset() {
        // 2026-08-27T13:12:13Z, com a fração de segundo descartada.
        assert_eq!(
            parse_time("2026-08-27T13:12:13.728861681Z"),
            Some(1_787_836_333)
        );
        // Mesmo instante escrito em -03:00 é três horas mais tarde em UTC.
        assert_eq!(
            parse_time("2026-08-27T10:12:13-03:00"),
            parse_time("2026-08-27T13:12:13Z")
        );
        // Um carimbo real de volume, conferido contra o calendário.
        assert_eq!(parse_time("2026-06-10T10:19:14-03:00"), Some(1_781_097_554));
        // A data zero do Go é «nunca», não 1º de janeiro do ano 1.
        assert_eq!(parse_time("0001-01-01T00:00:00Z"), None);
        assert_eq!(parse_time(""), None);
    }

    #[test]
    fn reads_health_out_of_the_status_line() {
        assert_eq!(
            health_from_status("Up 3 minutes (healthy)"),
            Some("healthy".to_string())
        );
        assert_eq!(
            health_from_status("Up 2 seconds (health: starting)"),
            Some("starting".to_string())
        );
        // Um parêntese que não é saúde não vira saúde.
        assert_eq!(health_from_status("Exited (0) 9 days ago"), None);
        assert_eq!(health_from_status("Up 5 days"), None);
    }

    #[test]
    fn a_published_port_reads_as_host_to_container() {
        let port = PortMap {
            container_port: 5432,
            protocol: "tcp".to_string(),
            host_ip: Some("0.0.0.0".to_string()),
            host_port: Some(5432),
        };
        assert_eq!(port.label(), "5432→5432/tcp");
        let closed = PortMap {
            host_ip: None,
            host_port: None,
            ..port
        };
        assert_eq!(closed.label(), "5432/tcp");
    }

    #[test]
    fn a_dead_exec_is_recognised_by_what_it_said() {
        // A engine responde `101 UPGRADED` mesmo para um comando que não existe: o
        // upgrade dá certo, ela escreve o erro no fluxo e fecha. Quem olhar só o código
        // de status entrega um fluxo já morto — e a tela volta sem dizer por quê, que era
        // exatamente o sintoma numa imagem sem `bash`.
        let engine = DockerEngine {
            info: EngineInfo {
                product: "Teste".to_string(),
                version: String::new(),
                api_version: String::new(),
                endpoint: Endpoint::Unix(std::path::PathBuf::from("/nao/existe")),
                rootless: false,
            },
            inspected: Mutex::new(HashMap::new()),
        };
        let oci = b"OCI runtime exec failed: exec: \"bash\": executable file not found";
        // Fluxo fechado: veredito na hora, sem precisar perguntar nada à engine — que
        // aqui nem existe, e é justamente por isso que este caso tem que se bastar.
        assert_eq!(
            engine.exec_failed("qualquer", true, oci).as_deref(),
            Some("OCI runtime exec failed: exec: \"bash\": executable file not found")
        );
        // Fechado e calado ainda é falha, e a frase diz isso em vez de ficar vazia.
        assert_eq!(
            engine.exec_failed("qualquer", true, b"").as_deref(),
            Some("o processo saiu sem dizer nada")
        );
    }

    #[test]
    fn a_running_container_offers_no_removal() {
        let running = Container {
            name: "x".to_string(),
            state: ContainerState::Running,
            ..Container::default()
        };
        let remove = container_actions(&running)
            .into_iter()
            .find(|a| a.key == ActionKey::RemoveContainer)
            .unwrap();
        assert!(remove.blocked.is_some());

        let stopped = Container {
            state: ContainerState::Exited,
            ..running
        };
        let remove = container_actions(&stopped)
            .into_iter()
            .find(|a| a.key == ActionKey::RemoveContainer)
            .unwrap();
        assert!(remove.blocked.is_none());
    }

    #[test]
    fn a_volume_in_use_says_who_uses_it() {
        let volume = Volume {
            name: "dados".to_string(),
            used_by: vec!["postgres".to_string()],
            ..Volume::default()
        };
        let remove = volume_actions(&volume)
            .into_iter()
            .find(|a| a.key == ActionKey::RemoveVolume)
            .unwrap();
        assert!(remove.blocked.unwrap().contains("postgres"));
        assert_eq!(remove.gravity, Gravity::Typed);
    }
}
