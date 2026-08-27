//! Os painéis da aba Containers.
//!
//! Cinco tabelas sobre os mesmos dados: os containers, o que eles montam, de que imagem
//! saíram, em que rede estão, e um resumo de tudo. Nenhuma delas sabe qual engine
//! respondeu — leem o retrato que `container::Store` publica, e o `Store` é quem fala
//! com a engine, numa thread de fundo, para que amostrar esta aba não custe I/O nenhum.
//!
//! Os ids são genéricos de propósito (`containers`, `volumes`, `images`, `networks`):
//! são o que vai para o `marks.json`, e trocar de engine não pode invalidar as marcas de
//! quem já usa.

use std::sync::Arc;

use crate::container::{self, Container, Store, Subject};
use crate::format;
use crate::tools::Handoff;

use super::{Detail, DetailSection, Rates, SystemState, TableMonitor, TableRow, mark::MarkKind};
use crate::app::Tab;

/// A vaga que o pai de um projeto ocupa no espaço de identidades das linhas.
///
/// `TableFocus` guarda o que está expandido por `pid`, e uma linha de projeto não tem
/// processo nenhum. Numerar do topo para baixo mantém cada projeto distinto sem chegar
/// perto de um pid de verdade, que nunca passa de alguns milhões.
fn project_key(index: usize) -> u32 {
    u32::MAX - index as u32
}

fn store(state: &SystemState) -> Option<&Arc<Store>> {
    state.containers.as_ref()
}

/// Uma taxa de rede como `1.2 MB/s ↓ 300 KB/s ↑`, ou um traço quando não foi medida.
///
/// Um traço diz «não medido aqui»; um zero diria «nada passou», e num container remoto
/// ou recém-visto a diferença é a única coisa honesta a dizer.
fn net_cell(container: &Container) -> String {
    match (container.net_rx_rate, container.net_tx_rate) {
        (Some(rx), Some(tx)) => format!(
            "{} ↓ {} ↑",
            format::human_bytes_per_sec(rx),
            format::human_bytes_per_sec(tx)
        ),
        _ => "-".to_string(),
    }
}

fn cpu_cell(container: &Container) -> String {
    match container.cpu_percent {
        Some(percent) => format!("{percent:.1}%"),
        None => "-".to_string(),
    }
}

fn memory_cell(container: &Container) -> String {
    let Some(used) = container.memory else {
        return "-".to_string();
    };
    match container.memory_limit {
        Some(limit) if limit > 0 => format!(
            "{} / {}",
            format::human_bytes(used as f64),
            format::human_bytes(limit as f64)
        ),
        _ => format::human_bytes(used as f64),
    }
}

/// O estado, ou o que está acontecendo com ele agora. Uma ação leva segundos — parar
/// espera o processo sair sozinho — e a linha diz isso enquanto acontece, em vez de
/// ficar parada mostrando o estado antigo como se nada tivesse sido pedido.
fn state_cell(container: &Container, snapshot: &container::store::Snapshot) -> String {
    match snapshot.running.get(&container.id) {
        Some(verb) => format!("{verb}…"),
        None => container.state_label(),
    }
}

/// As portas publicadas, como uma pessoa as compara: `5432→5432`, e o endereço do host
/// só quando ele não é «qualquer um».
///
/// Uma porta que o container abre mas ninguém publica não entra: a coluna responde «por
/// onde eu chego nisto daqui», e uma porta fechada para o host não é resposta para essa
/// pergunta. Quem quiser todas continua tendo a seção de rede no detalhe.
fn ports_cell(container: &Container) -> String {
    let published: Vec<String> = container
        .ports
        .iter()
        .filter(|port| port.host_port.is_some())
        .map(|port| port.label())
        .collect();
    if published.is_empty() {
        return "-".to_string();
    }
    published.join(" ")
}

fn container_cells(container: &Container, snapshot: &container::store::Snapshot) -> Vec<String> {
    vec![
        container.display_name(),
        container.image.clone(),
        state_cell(container, snapshot),
        ports_cell(container),
        cpu_cell(container),
        memory_cell(container),
        net_cell(container),
    ]
}

/// Quanto tempo faz, a partir de um carimbo em segundos desde a época.
fn age(created: i64) -> String {
    if created <= 0 {
        return "-".to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if now <= created {
        return "agora".to_string();
    }
    format::human_duration((now - created) as u64)
}

// --- containers ----------------------------------------------------------------------

const CONTAINER_HEADERS: [&str; 7] = [
    "Nome", "Imagem", "Estado", "Portas", "CPU%", "Memória", "Rede E/S",
];

const CONTAINER_MARKS: [MarkKind; 4] = [
    MarkKind {
        name: "container",
        column: 0,
        numeric: false,
        help: "Segue um container pelo nome. Um projeto inteiro do compose é o prefixo dele.",
    },
    MarkKind {
        name: "imagem",
        column: 1,
        numeric: false,
        help: "Segue tudo que roda uma imagem — «postgres» pega todas as versões dela.",
    },
    MarkKind {
        name: "estado",
        column: 2,
        numeric: false,
        help: "Segue por estado: «parado» acende toda vez que algum container cair.",
    },
    MarkKind {
        name: "porta",
        column: 3,
        numeric: true,
        help: "Segue quem publica uma porta — acende se outro container passar a expô-la.",
    },
];

#[derive(Default)]
pub struct ContainersMonitor {
    /// A nota da borda, montada durante a amostragem: `note()` não recebe estado, e o
    /// que a tabela tem a dizer sobre si mesma só se sabe depois de olhar o retrato.
    note: Option<String>,
}

impl TableMonitor for ContainersMonitor {
    fn id(&self) -> &'static str {
        "containers"
    }

    fn actions_on_enter(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Containers"
    }

    fn tab(&self) -> Tab {
        Tab::Containers
    }

    fn headers(&self) -> &'static [&'static str] {
        &CONTAINER_HEADERS
    }

    fn mark_kinds(&self) -> &'static [MarkKind] {
        &CONTAINER_MARKS
    }

    fn tree(&self) -> bool {
        true
    }

    fn has_detail(&self) -> bool {
        true
    }

    /// Plano no painel compacto, em árvore por projeto do compose em tela cheia.
    ///
    /// A mesma escolha que as duas tabelas de processos fazem, pelo mesmo motivo: no
    /// painel pequeno cabem dez linhas e gastar metade delas com cabeçalhos de grupo é
    /// gastar a tabela; em tela cheia há espaço, e aí o agrupamento é o que responde
    /// «este projeto inteiro está de pé?» sem ler linha por linha.
    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let Some(store) = store(state) else {
            return Vec::new();
        };
        let rows = store.read(|snapshot| match limit {
            Some(limit) => snapshot
                .containers
                .iter()
                .take(limit)
                .map(|container| {
                    let mut row =
                        TableRow::leaf(container_cells(container, snapshot), container.pid);
                    row.key = container.id.clone();
                    row
                })
                .collect(),
            None => grouped_rows(snapshot),
        });
        self.note = store.read(|snapshot| {
            let mut parts = vec![store.engine_label()];
            let (running, stopped, _) = snapshot.counts();
            if !snapshot.containers.is_empty() {
                parts.push(format!("{running} em execução, {stopped} parado(s)"));
            }
            // O que o painel não consegue ver, dito onde o painel está: uma tabela que
            // mostra parte do quadro e não avisa é lida como o quadro inteiro.
            if let Some(error) = &snapshot.error {
                parts.push(error.clone());
            }
            Some(parts.join(" · "))
        });
        rows
    }

    /// Em tela cheia a forma da tabela fica congelada e só os números se mexem — a lista
    /// não pode reordenar debaixo de quem está lendo. Casadas pelo id do container, que
    /// é a identidade que sobrevive a um reinício da linha.
    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let Some(store) = store(state) else {
            return;
        };
        store.read(|snapshot| {
            for row in rows.iter_mut() {
                if row.key.is_empty() {
                    continue;
                }
                let Some(container) = snapshot.containers.iter().find(|c| c.id == row.key) else {
                    continue;
                };
                row.cells = container_cells(container, snapshot);
            }
        });
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let store = store(state)?;
        store.read(|snapshot| {
            let container = snapshot.containers.iter().find(|c| c.id == row.key)?;
            Some(container_detail(container, snapshot))
        })
    }
}

/// As linhas em árvore: um pai por projeto do compose, os avulsos na raiz.
fn grouped_rows(snapshot: &container::store::Snapshot) -> Vec<TableRow> {
    // Ordem de projetos pela ordem em que seus containers aparecem, que já vem ordenada
    // por estado — assim um projeto inteiro no ar não fica abaixo de um parado.
    let mut projects: Vec<&str> = Vec::new();
    for container in &snapshot.containers {
        if let Some(project) = &container.project
            && !projects.contains(&project.as_str())
        {
            projects.push(project);
        }
    }

    let mut rows = Vec::new();
    for (index, project) in projects.iter().enumerate() {
        let members: Vec<&Container> = snapshot
            .containers
            .iter()
            .filter(|c| c.project.as_deref() == Some(*project))
            .collect();
        let running = members.iter().filter(|c| c.state.is_live()).count();
        let key = project_key(index);
        rows.push(TableRow {
            cells: vec![
                (*project).to_string(),
                format!("{} serviço(s)", members.len()),
                format!("{running}/{} no ar", members.len()),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ],
            pid: key,
            depth: 0,
            is_last_sibling: false,
            guides: Vec::new(),
            mark: None,
            child_count: members.len(),
            descendant_pids: members.iter().map(|c| c.pid).collect(),
            key: String::new(),
        });
        let last = members.len().saturating_sub(1);
        for (position, container) in members.iter().enumerate() {
            // O serviço, não o nome inteiro: dentro de «lexminer» a linha que diz
            // «lexminer-postgres» repete o que o pai já disse.
            let name = container
                .service
                .clone()
                .unwrap_or_else(|| container.display_name());
            let mut cells = container_cells(container, snapshot);
            cells[0] = name;
            rows.push(TableRow {
                cells,
                pid: container.pid,
                depth: 1,
                is_last_sibling: position == last,
                guides: vec![false],
                mark: None,
                child_count: 0,
                descendant_pids: Vec::new(),
                key: container.id.clone(),
            });
        }
    }
    for container in snapshot.containers.iter().filter(|c| c.project.is_none()) {
        let mut row = TableRow::leaf(container_cells(container, snapshot), container.pid);
        row.key = container.id.clone();
        rows.push(row);
    }
    rows
}

fn container_detail(container: &Container, snapshot: &container::store::Snapshot) -> Detail {
    let mut sections = Vec::new();

    let mut identity = DetailSection::new("Identidade");
    identity.push("Nome", container.display_name());
    identity.push("Id", container.short_id());
    identity.push("Imagem", container.image.clone());
    identity.push("Comando", container.command.clone());
    if let Some(project) = &container.project {
        identity.push("Projeto", project.clone());
    }
    if let Some(service) = &container.service {
        identity.push("Serviço", service.clone());
    }
    identity.push("Criado há", age(container.created));
    sections.push(identity);

    let mut status = DetailSection::new("Estado");
    status.push("Situação", container.state_label());
    status.push("Por extenso", container.status.clone());
    if !container.started_at.is_empty() {
        status.push("Subiu em", container.started_at.clone());
    }
    if !container.finished_at.is_empty() && !container.state.is_live() {
        status.push("Terminou em", container.finished_at.clone());
    }
    if container.restart_count > 0 {
        status.push("Reinícios", container.restart_count.to_string());
    }
    if container.oom_killed {
        status.push("Morto por falta de memória", "sim".to_string());
    }
    if container.pid != 0 {
        status.push("Pid no host", container.pid.to_string());
    }
    sections.push(status);

    // Só existe no modo local: são arquivos do kernel desta máquina. Num endpoint remoto
    // a seção some inteira, em vez de aparecer zerada dizendo que nada acontece.
    let mut limits = DetailSection::new("Limites e pressão");
    if let Some(quota) = &container.cpu_quota {
        limits.push("Teto de CPU", quota.clone());
    }
    if let Some(limit) = container.memory_limit {
        limits.push("Teto de memória", format::human_bytes(limit as f64));
    }
    if let Some(peak) = container.memory_peak {
        limits.push("Pico de memória", format::human_bytes(peak as f64));
    }
    if let Some((count, usec)) = container.throttled
        && count > 0
    {
        // O número que responde «por que está lento» quando a CPU parece ociosa.
        limits.push(
            "Freado no teto de CPU",
            format!("{count}x, {:.1}s parado", usec as f64 / 1_000_000.0),
        );
    }
    if let Some(kills) = container.oom_kills
        && kills > 0
    {
        limits.push("Processos mortos por memória", kills.to_string());
    }
    if let Some(pressure) = container.cpu_pressure {
        limits.push("Pressão de CPU", pressure_text(pressure));
    }
    if let Some(pressure) = container.memory_pressure {
        limits.push("Pressão de memória", pressure_text(pressure));
    }
    if let Some(pressure) = container.io_pressure {
        limits.push("Pressão de disco", pressure_text(pressure));
    }
    if !limits.fields.is_empty() {
        sections.push(limits);
    }

    let mut network = DetailSection::new("Rede");
    if let Some(ip) = &container.ip {
        network.push("Endereço", ip.clone());
    }
    if !container.networks.is_empty() {
        network.push("Redes", container.networks.join(", "));
    }
    if !container.ports.is_empty() {
        network.push(
            "Portas",
            container
                .ports
                .iter()
                .map(|p| p.label())
                .collect::<Vec<_>>()
                .join("  "),
        );
    }
    if !network.fields.is_empty() {
        sections.push(network);
    }

    let mut storage = DetailSection::new("Armazenamento e log");
    if !container.volumes.is_empty() {
        storage.push("Volumes", container.volumes.join(", "));
    }
    if let Some(path) = &container.log_path {
        storage.push("Log", path.clone());
        // Um log que cresce sem rotação é um problema de verdade, e o tamanho é a única
        // coisa que avisa antes de o disco acabar.
        if let Ok(meta) = std::fs::metadata(path) {
            storage.push("Tamanho do log", format::human_bytes(meta.len() as f64));
        }
    }
    if !storage.fields.is_empty() {
        sections.push(storage);
    }

    if let Some((message, ok)) = &snapshot.outcome {
        let mut last = DetailSection::new("Última ação");
        last.push(if *ok { "Resultado" } else { "Falhou" }, message.clone());
        sections.push(last);
    }

    Detail {
        title: format!("{} — {}", container.display_name(), container.image),
        gone_note: "removido",
        sections,
        rates: match (container.net_rx_rate, container.net_tx_rate) {
            (Some(rx), Some(tx)) => Some(Rates {
                labels: ("Recebido", "Enviado"),
                values: (rx, tx),
            }),
            _ => None,
        },
        handoffs: container_handoffs(container),
        handoff_title: "O que apontar para este container",
    }
}

fn pressure_text(pressure: container::Pressure) -> String {
    // Três janelas, porque uma sozinha não distingue um pico de agora de um problema que
    // dura cinco minutos.
    format!(
        "{:.2}% (10s) · {:.2}% (1min) · {:.2}% (5min)",
        pressure.avg10, pressure.avg60, pressure.avg300
    )
}

/// O que outra ferramenta do app pode fazer com este container.
///
/// Um container é um endereço com um log, e o programa já sabe o que fazer com os dois.
fn container_handoffs(container: &Container) -> Vec<Handoff> {
    let mut offers = Vec::new();
    if let Some(path) = &container.log_path {
        offers.push(Handoff {
            label: format!("seguir os logs de {}", container.display_name()),
            tool: "tail",
            params: vec![
                ("caminho", path.clone()),
                ("inicio", "fim do arquivo".to_string()),
                ("formato", "JSON por linha".to_string()),
            ],
        });
    }
    // Portas publicadas viram achados do tipo «porta», e aí ganham varredura,
    // certificado e túnel de graça — pela mesma tabela que decide o que um achado vale.
    for port in &container.ports {
        if let Some(host_port) = port.host_port {
            offers.extend(crate::tools::offers_for(
                "porta",
                &format!("127.0.0.1:{host_port}"),
            ));
        }
    }
    if let Some(ip) = &container.ip {
        offers.extend(crate::tools::offers_for("ip", ip));
    }
    offers
}

// --- volumes -------------------------------------------------------------------------

const VOLUME_HEADERS: [&str; 4] = ["Volume", "Usado por", "Tamanho", "Criado"];

const VOLUME_MARKS: [MarkKind; 2] = [
    MarkKind {
        name: "volume",
        column: 0,
        numeric: false,
        help: "Segue um volume pelo nome, ou um projeto inteiro pelo prefixo dele.",
    },
    MarkKind {
        name: "container",
        column: 1,
        numeric: false,
        help: "Segue os volumes de um container — e acende quando ele deixa de usá-los.",
    },
];

#[derive(Default)]
pub struct VolumesMonitor {
    note: Option<String>,
}

impl TableMonitor for VolumesMonitor {
    fn id(&self) -> &'static str {
        "volumes"
    }

    fn actions_on_enter(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Volumes"
    }

    fn tab(&self) -> Tab {
        Tab::Containers
    }

    fn headers(&self) -> &'static [&'static str] {
        &VOLUME_HEADERS
    }

    fn mark_kinds(&self) -> &'static [MarkKind] {
        &VOLUME_MARKS
    }

    fn has_detail(&self) -> bool {
        true
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let Some(store) = store(state) else {
            return Vec::new();
        };
        let rows = store.read(|snapshot| {
            snapshot
                .volumes
                .iter()
                .take(limit.unwrap_or(usize::MAX))
                .map(|volume| {
                    let mut row = TableRow::leaf(
                        vec![
                            volume.name.clone(),
                            if volume.used_by.is_empty() {
                                "— órfão".to_string()
                            } else {
                                volume.used_by.join(", ")
                            },
                            match volume.size {
                                Some(size) => format::human_bytes(size as f64),
                                // A primeira medição custa quase um segundo; um zero no
                                // lugar dela seria mentira, e um vazio não explicaria.
                                None => "medindo…".to_string(),
                            },
                            age(volume.created),
                        ],
                        0,
                    );
                    row.key = volume.name.clone();
                    row
                })
                .collect()
        });
        self.note = store.read(|snapshot| {
            let (total, orphan_size, orphans) = container::store::volume_totals(snapshot);
            let mut note = format!("{} volumes", snapshot.volumes.len());
            if let Some(at) = snapshot.measured_at {
                note.push_str(&format!(
                    " · {} · {orphans} órfão(s) ({} recuperáveis) · medidos há {}",
                    format::human_bytes(total as f64),
                    format::human_bytes(orphan_size as f64),
                    // A idade da medição, porque um número de um minuto atrás é útil e um
                    // número de um minuto atrás apresentado como atual não é.
                    format::human_duration(at.elapsed().as_secs())
                ));
            } else {
                note.push_str(" · medindo tamanhos…");
            }
            Some(note)
        });
        rows
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let fresh = self.sample(state, None);
        for row in rows.iter_mut() {
            if let Some(current) = fresh.iter().find(|r| r.key == row.key) {
                row.cells = current.cells.clone();
            }
        }
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let store = store(state)?;
        store.read(|snapshot| {
            let volume = container::store::volume_named(snapshot, &row.key)?;
            let mut about = DetailSection::new("Volume");
            about.push("Nome", volume.name.clone());
            about.push("Driver", volume.driver.clone());
            about.push("Caminho no disco", volume.mountpoint.clone());
            about.push("Criado há", age(volume.created));
            if let Some(project) = &volume.project {
                about.push("Projeto", project.clone());
            }
            if let Some(size) = volume.size {
                about.push("Tamanho", format::human_bytes(size as f64));
            }
            let mut users = DetailSection::new("Quem usa");
            if volume.used_by.is_empty() {
                users.push("Nenhum container", "órfão — nada o monta agora".to_string());
            } else {
                for name in &volume.used_by {
                    users.push("Container", name.clone());
                }
            }
            Some(Detail {
                title: format!("volume {}", volume.name),
                gone_note: "removido",
                sections: vec![about, users],
                rates: None,
                handoffs: Vec::new(),
                handoff_title: "O que apontar para este volume",
            })
        })
    }

    /// O resumo na borda: total, órfãos e a idade da medição.
    fn note(&self) -> Option<String> {
        self.note.clone()
    }
}

// --- imagens -------------------------------------------------------------------------

const IMAGE_HEADERS: [&str; 4] = ["Imagem", "Tamanho", "Criada", "Em uso"];

const IMAGE_MARKS: [MarkKind; 1] = [MarkKind {
    name: "imagem",
    column: 0,
    numeric: false,
    help: "Segue uma imagem pelo nome — «postgres» pega todas as versões dela.",
}];

#[derive(Default)]
pub struct ImagesMonitor {
    note: Option<String>,
}

impl TableMonitor for ImagesMonitor {
    fn id(&self) -> &'static str {
        "images"
    }

    fn actions_on_enter(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Imagens"
    }

    fn tab(&self) -> Tab {
        Tab::Containers
    }

    fn headers(&self) -> &'static [&'static str] {
        &IMAGE_HEADERS
    }

    fn mark_kinds(&self) -> &'static [MarkKind] {
        &IMAGE_MARKS
    }

    fn has_detail(&self) -> bool {
        true
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let Some(store) = store(state) else {
            return Vec::new();
        };
        let rows = store.read(|snapshot| {
            snapshot
                .images
                .iter()
                .take(limit.unwrap_or(usize::MAX))
                .map(|image| {
                    let mut row = TableRow::leaf(
                        vec![
                            image.display_name(),
                            format::human_bytes(image.size as f64),
                            age(image.created),
                            match (image.used_by.len(), image.dangling()) {
                                (0, true) => "— solta".to_string(),
                                (0, false) => "— sem uso".to_string(),
                                (_, _) => image.used_by.join(", "),
                            },
                        ],
                        0,
                    );
                    row.key = image.id.clone();
                    row
                })
                .collect()
        });
        self.note = store.read(|snapshot| {
            let (size, dangling) = container::store::image_totals(&snapshot.images);
            Some(format!(
                "{} imagens · {} · {dangling} solta(s)",
                snapshot.images.len(),
                format::human_bytes(size as f64)
            ))
        });
        rows
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let fresh = self.sample(state, None);
        for row in rows.iter_mut() {
            if let Some(current) = fresh.iter().find(|r| r.key == row.key) {
                row.cells = current.cells.clone();
            }
        }
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let store = store(state)?;
        store.read(|snapshot| {
            let image = snapshot.images.iter().find(|i| i.id == row.key)?;
            let mut about = DetailSection::new("Imagem");
            about.push("Nome", image.display_name());
            about.push("Id", image.short_id());
            about.push("Tamanho", format::human_bytes(image.size as f64));
            about.push("Criada há", age(image.created));
            if image.tags.len() > 1 {
                about.push("Outras tags", image.tags[1..].join(", "));
            }
            let mut users = DetailSection::new("Quem usa");
            if image.used_by.is_empty() {
                users.push(
                    "Nenhum container",
                    if image.dangling() {
                        "solta — sem tag nenhuma".to_string()
                    } else {
                        "nada a usa agora, mas ela tem tag".to_string()
                    },
                );
            } else {
                for name in &image.used_by {
                    users.push("Container", name.clone());
                }
            }
            Some(Detail {
                title: format!("imagem {}", image.display_name()),
                gone_note: "removida",
                sections: vec![about, users],
                rates: None,
                handoffs: Vec::new(),
                handoff_title: "O que apontar para esta imagem",
            })
        })
    }
}

// --- redes ---------------------------------------------------------------------------

const NETWORK_HEADERS: [&str; 4] = ["Rede", "Driver", "Sub-rede", "Containers"];

const NETWORK_MARKS: [MarkKind; 1] = [MarkKind {
    name: "rede",
    column: 0,
    numeric: false,
    help: "Segue uma rede pelo nome, ou um projeto inteiro pelo prefixo dele.",
}];

#[derive(Default)]
pub struct NetworksMonitor {
    note: Option<String>,
}

impl TableMonitor for NetworksMonitor {
    fn id(&self) -> &'static str {
        "networks"
    }

    fn actions_on_enter(&self) -> bool {
        true
    }

    fn title(&self) -> &'static str {
        "Redes"
    }

    fn tab(&self) -> Tab {
        Tab::Containers
    }

    fn headers(&self) -> &'static [&'static str] {
        &NETWORK_HEADERS
    }

    fn mark_kinds(&self) -> &'static [MarkKind] {
        &NETWORK_MARKS
    }

    fn has_detail(&self) -> bool {
        true
    }

    fn sample(&mut self, state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let Some(store) = store(state) else {
            return Vec::new();
        };
        let rows = store.read(|snapshot| {
            snapshot
                .networks
                .iter()
                .take(limit.unwrap_or(usize::MAX))
                .map(|network| {
                    let mut row = TableRow::leaf(
                        vec![
                            network.name.clone(),
                            // Uma rede embutida não é lixo, e oferecer removê-la seria
                            // oferecer um erro — a coluna diz isso onde se lê a linha.
                            if network.builtin() {
                                format!("{} (embutida)", network.driver)
                            } else {
                                network.driver.clone()
                            },
                            network.subnet.clone().unwrap_or_else(|| "-".to_string()),
                            if network.used_by.is_empty() {
                                "— vazia".to_string()
                            } else {
                                network.used_by.join(", ")
                            },
                        ],
                        0,
                    );
                    row.key = network.id.clone();
                    row
                })
                .collect()
        });
        self.note = store.read(|snapshot| {
            let builtin = snapshot.networks.iter().filter(|n| n.builtin()).count();
            let empty = snapshot
                .networks
                .iter()
                .filter(|n| n.used_by.is_empty() && !n.builtin())
                .count();
            Some(format!(
                "{} redes · {builtin} embutida(s) · {empty} vazia(s)",
                snapshot.networks.len()
            ))
        });
        rows
    }

    fn note(&self) -> Option<String> {
        self.note.clone()
    }

    fn refresh_values(&mut self, state: &SystemState, rows: &mut [TableRow]) {
        let fresh = self.sample(state, None);
        for row in rows.iter_mut() {
            if let Some(current) = fresh.iter().find(|r| r.key == row.key) {
                row.cells = current.cells.clone();
            }
        }
    }

    fn detail(&mut self, state: &SystemState, row: &TableRow) -> Option<Detail> {
        let store = store(state)?;
        store.read(|snapshot| {
            let network = snapshot.networks.iter().find(|n| n.id == row.key)?;
            let mut about = DetailSection::new("Rede");
            about.push("Nome", network.name.clone());
            about.push("Driver", network.driver.clone());
            about.push("Escopo", network.scope.clone());
            if let Some(subnet) = &network.subnet {
                about.push("Sub-rede", subnet.clone());
            }
            if network.internal {
                about.push("Interna", "sim — sem saída para fora".to_string());
            }
            if network.builtin() {
                about.push("Embutida", "criada pela própria engine".to_string());
            }
            if let Some(project) = &network.project {
                about.push("Projeto", project.clone());
            }
            about.push("Criada há", age(network.created));
            let mut users = DetailSection::new("Quem está nela");
            if network.used_by.is_empty() {
                users.push("Nenhum container", "vazia".to_string());
            } else {
                for name in &network.used_by {
                    users.push("Container", name.clone());
                }
            }
            Some(Detail {
                title: format!("rede {}", network.name),
                gone_note: "removida",
                sections: vec![about, users],
                rates: None,
                handoffs: Vec::new(),
                handoff_title: "O que apontar para esta rede",
            })
        })
    }
}

// --- resumo --------------------------------------------------------------------------

const SUMMARY_HEADERS: [&str; 2] = ["Item", "Situação"];

/// Quantidades, não bytes. Responde de longe «o que existe nesta máquina», que é a
/// pergunta que se faz sem chegar perto de nenhum dos outros painéis.
#[derive(Default)]
pub struct ContainerSummaryMonitor;

impl TableMonitor for ContainerSummaryMonitor {
    fn id(&self) -> &'static str {
        "resumo-containers"
    }

    fn title(&self) -> &'static str {
        "Resumo"
    }

    fn tab(&self) -> Tab {
        Tab::Containers
    }

    fn headers(&self) -> &'static [&'static str] {
        &SUMMARY_HEADERS
    }

    fn sample(&mut self, state: &SystemState, _limit: Option<usize>) -> Vec<TableRow> {
        let Some(store) = store(state) else {
            return Vec::new();
        };
        let mut rows = Vec::new();
        let mut push = |item: &str, value: String| {
            let mut row = TableRow::leaf(vec![item.to_string(), value], 0);
            // Só a linha da engine tem chave, porque só ela responde ao Enter: é onde se
            // escolhe o endereço em que a engine atende.
            if item == "Engine" {
                row.key = "engine".to_string();
            }
            rows.push(row);
        };
        store.read(|snapshot| {
            let (running, stopped, orphan_volumes) = snapshot.counts();
            let paused = snapshot
                .containers
                .iter()
                .filter(|c| c.state == container::ContainerState::Paused)
                .count();
            push(
                "Containers",
                format!("{running} em execução · {stopped} parado(s) · {paused} pausado(s)"),
            );
            let (images_size, dangling) = container::store::image_totals(&snapshot.images);
            push(
                "Imagens",
                match snapshot.usage.as_ref().map(|u| u.images_size) {
                    // O tamanho das camadas, e não a soma dos tamanhos das imagens: as
                    // camadas são compartilhadas, e somar imagem por imagem conta a mesma
                    // camada várias vezes. Aqui a soma é o que aparece entre parênteses,
                    // porque é ela que bate com o que cada linha do painel diz.
                    Some(layers) if layers > 0 => format!(
                        "{} · {} em camadas ({} somando) · {dangling} solta(s)",
                        snapshot.images.len(),
                        format::human_bytes(layers as f64),
                        format::human_bytes(images_size as f64)
                    ),
                    _ => format!(
                        "{} · {} · {dangling} solta(s)",
                        snapshot.images.len(),
                        format::human_bytes(images_size as f64)
                    ),
                },
            );
            let (volumes_size, orphan_size, _) = container::store::volume_totals(snapshot);
            push(
                "Volumes",
                match snapshot.measured_at {
                    Some(_) => format!(
                        "{} · {} · {orphan_volumes} órfão(s) ({})",
                        snapshot.volumes.len(),
                        format::human_bytes(volumes_size as f64),
                        format::human_bytes(orphan_size as f64)
                    ),
                    None => format!("{} · medindo tamanhos…", snapshot.volumes.len()),
                },
            );
            let empty = snapshot
                .networks
                .iter()
                .filter(|n| n.used_by.is_empty() && !n.builtin())
                .count();
            push(
                "Redes",
                format!("{} · {empty} vazia(s)", snapshot.networks.len()),
            );
            if let Some(usage) = &snapshot.usage {
                let mut extras = Vec::new();
                if usage.containers_size > 0 {
                    extras.push(format!(
                        "{} escritos pelos containers",
                        format::human_bytes(usage.containers_size as f64)
                    ));
                }
                if usage.build_cache > 0 {
                    extras.push(format!(
                        "{} de cache de build",
                        format::human_bytes(usage.build_cache as f64)
                    ));
                }
                if !extras.is_empty() {
                    push("Em disco", extras.join(" · "));
                }
            }
            push("Engine", store.engine_detail());
            if let Some((message, ok)) = &snapshot.outcome {
                push(
                    if *ok {
                        "Última ação"
                    } else {
                        "Última ação falhou"
                    },
                    message.clone(),
                );
            }
        });
        rows
    }
}

// --- o sujeito de uma linha ----------------------------------------------------------

/// De que coisa uma linha de uma destas tabelas fala.
///
/// É o que o menu de ações precisa saber para perguntar à engine o que se pode fazer.
/// Devolvido pela tabela porque só ela sabe o que a sua própria chave significa.
pub fn subject_of(store: &Store, table: &str, key: &str) -> Option<Subject> {
    store.read(|snapshot| match table {
        "containers" => snapshot
            .containers
            .iter()
            .find(|c| c.id == key)
            .map(|c| Subject::Container(Box::new(c.clone()))),
        "volumes" => container::store::volume_named(snapshot, key)
            .cloned()
            .map(Subject::Volume),
        "images" => snapshot
            .images
            .iter()
            .find(|i| i.id == key)
            .cloned()
            .map(Subject::Image),
        "networks" => snapshot
            .networks
            .iter()
            .find(|n| n.id == key)
            .cloned()
            .map(Subject::Network),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_rows_never_collide_with_a_real_pid() {
        // Pids reais não passam de alguns milhões; as chaves de projeto começam no topo
        // do espaço e descem. Se colidissem, expandir um projeto expandiria outro.
        assert!(project_key(0) > 10_000_000);
        assert_ne!(project_key(0), project_key(1));
    }

    #[test]
    fn unmeasured_rates_read_as_a_dash_not_a_zero() {
        let container = Container::default();
        assert_eq!(net_cell(&container), "-");
        assert_eq!(cpu_cell(&container), "-");
        assert_eq!(memory_cell(&container), "-");
    }

    #[test]
    fn only_published_ports_reach_the_column() {
        use container::PortMap;
        let container = Container {
            ports: vec![
                PortMap {
                    container_port: 5432,
                    protocol: "tcp".to_string(),
                    host_ip: Some("0.0.0.0".to_string()),
                    host_port: Some(5433),
                },
                // Aberta dentro do container e fechada para o host: não é por onde se
                // chega nele daqui, então não é resposta para o que a coluna pergunta.
                PortMap {
                    container_port: 9000,
                    protocol: "tcp".to_string(),
                    host_ip: None,
                    host_port: None,
                },
            ],
            ..Container::default()
        };
        assert_eq!(ports_cell(&container), "5433→5432/tcp");
        assert_eq!(ports_cell(&Container::default()), "-");
    }

    #[test]
    fn memory_shows_the_limit_when_there_is_one() {
        let limited = Container {
            memory: Some(1024 * 1024),
            memory_limit: Some(10 * 1024 * 1024),
            ..Container::default()
        };
        assert_eq!(memory_cell(&limited), "1.0 MB / 10.0 MB");
        let unlimited = Container {
            memory_limit: None,
            ..limited
        };
        assert_eq!(memory_cell(&unlimited), "1.0 MB");
    }
}
