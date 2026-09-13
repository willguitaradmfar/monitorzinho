//! O que perguntar a uma máquina Linux, e o que as respostas querem dizer.
//!
//! O repertório é o de quem recebe um servidor que nunca viu e tem uma hora para dizer o
//! que há de errado nele: quanto trabalho ele está aguentando, o que está comendo a CPU e
//! a memória, quanto falta no disco, o que está travado esperando I/O, que serviço morreu,
//! que erro o kernel registrou, quem entra ali e com que poder, o que está aberto para o
//! mundo, e o que está velho o bastante para ser uma vulnerabilidade.
//!
//! Nada aqui altera a máquina — ver `ssh::SCRIPT`, que é o que foi executado, inteiro e
//! em texto. Um comando que precisa de root e não tem simplesmente devolve vazio, e vazio
//! aqui vira «não deu para saber com este usuário», que é uma resposta honesta e não um
//! atestado de saúde.

use std::collections::HashMap;

use super::{Depth, Plan, Report, bytes, count, duration, one_line, plural, ssh};
use crate::painel::Tone;

/// Uma investigação de uma máquina.
///
/// Guarda a resposta anterior porque os contadores do Linux são acumulados desde o boot:
/// `/proc/stat` diz quanto tempo a CPU passou ociosa desde que a máquina ligou, e o que
/// alguém quer saber é quanto ela passou ociosa **desde a última vez que se olhou**.
pub struct Session {
    target: ssh::Target,
    depth: Depth,
    /// Os tempos de CPU da passada anterior, para a diferença.
    cpu_antes: Option<Cpu>,
    passadas: u64,
    ultima: std::time::Instant,
}

impl Session {
    pub fn open(plan: &Plan) -> Result<Self, String> {
        let target = ssh::Target::parse(plan)?;
        Ok(Self {
            target,
            depth: plan.depth,
            cpu_antes: None,
            passadas: 0,
            ultima: std::time::Instant::now(),
        })
    }

    /// Uma investigação inteira: um `ssh`, um script, e a resposta repartida em cartões.
    pub fn pass(&mut self, report: &mut Report) -> Result<(), String> {
        let janela = self.ultima.elapsed().as_secs_f64().max(0.1);
        self.ultima = std::time::Instant::now();
        self.passadas += 1;

        let saida = self.target.run(&ssh::script(self.depth))?;
        let s = ssh::sections(&saida);

        self.machine(report, &s);
        self.cpu(report, &s, janela);
        memory(report, &s);
        disk(report, &s);
        pressure(report, &s);
        network(report, &s);
        if self.depth >= Depth::Medio {
            processes(report, &s);
            services(report, &s);
            errors(report, &s);
            access(report, &s);
            updates(report, &s);
        }
        if self.depth >= Depth::Profundo {
            security(report, &s);
            inventory(report, &s);
        }
        Ok(())
    }

    fn machine(&self, report: &mut Report, s: &Secoes) {
        report.section("Máquina");
        report.field("endereço", self.target.summary());
        // A linha como seria digitada: é o caminho mais curto entre «não conectou» e
        // saber por quê, e ela nunca carrega a senha.
        report.line(self.target.command_line());
        let sistema = campo(s, "sistema");
        report.field("sistema", aspas(&valor_de(&sistema, "PRETTY_NAME")));
        // `uname -sr` sai como «Linux 6.8.0-88», e é a única linha da seção que começa
        // assim: o resto é o os-release, que é tudo `CHAVE=valor`.
        report.field(
            "kernel",
            sistema
                .lines()
                .find(|linha| linha.starts_with("Linux "))
                .unwrap_or(""),
        );
        report.field("virtualização", campo(s, "virtualizacao").trim());
        let identidade = campo(s, "identidade");
        report.field("conectado como", identidade.lines().next().unwrap_or(""));
        report.field("nome", identidade.lines().nth(1).unwrap_or(""));
        let ligada = campo(s, "uptime")
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<f64>().ok())
            .unwrap_or(0.0);
        report.field("ligada há", duration(ligada));
        // Uma máquina que subiu agora não tem contador nenhum com história dentro, e
        // várias conclusões abaixo dependem de história.
        if ligada > 0.0 && ligada < 3600.0 {
            report.line("subiu há pouco — os contadores acumulados ainda dizem pouco");
        }
        if identidade
            .lines()
            .next()
            .is_some_and(|id| id.contains("uid=0("))
        {
            report.line("este usuário é root: tudo que o diagnóstico pede, ele alcança");
        } else {
            report.line(
                "usuário comum: o que precisar de root aparece abaixo como não alcançado, nunca como ausente",
            );
        }
    }

    /// Onde o tempo da CPU está indo.
    fn cpu(&mut self, report: &mut Report, s: &Secoes, janela: f64) {
        report.section("CPU");
        let nucleos = campo(s, "carga")
            .lines()
            .nth(1)
            .and_then(|n| n.trim().parse::<f64>().ok())
            .unwrap_or(1.0)
            .max(1.0);
        report.field(
            "modelo",
            campo(s, "cpu")
                .lines()
                .next()
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
                .unwrap_or_default(),
        );
        report.field("núcleos", count(nucleos));

        let carga: Vec<f64> = campo(s, "carga")
            .split_whitespace()
            .take(3)
            .filter_map(|n| n.parse().ok())
            .collect();
        if carga.len() == 3 {
            report.field(
                "carga (1/5/15 min)",
                format!("{:.2} · {:.2} · {:.2}", carga[0], carga[1], carga[2]),
            );
            // Carga é fila, não porcentagem: acima do número de núcleos quer dizer que há
            // mais gente querendo CPU do que CPU existe, e a partir daí todo mundo espera.
            let por_nucleo = carga[0] / nucleos;
            if por_nucleo >= 2.0 {
                report.grave(
                    format!(
                        "carga de {:.2} para {} núcleos — {:.1}× mais trabalho do que cabe",
                        carga[0],
                        count(nucleos),
                        por_nucleo
                    ),
                    "com a fila nesse tamanho, o tempo de resposta de tudo já está sendo decidido pela espera. A lista de processos abaixo diz quem está na frente",
                );
            } else if por_nucleo >= 1.0 {
                report.aviso(
                    format!("carga de {:.2} para {} núcleos", carga[0], count(nucleos)),
                    "a máquina está no limite do que consegue processar: qualquer pico a partir daqui vira fila",
                );
            } else {
                report.ok(format!(
                    "carga de {:.2} para {} núcleos — folga",
                    carga[0],
                    count(nucleos)
                ));
            }
        }

        let agora = Cpu::parse(&campo(s, "cpu_tempos"));
        if let (Some(agora), Some(antes)) = (agora, self.cpu_antes) {
            let delta = agora.menos(&antes);
            if delta.total() > 0.0 {
                report.field(
                    format!("desde a leitura anterior ({janela:.0}s)"),
                    format!(
                        "{:.0}% usuário · {:.0}% sistema · {:.0}% espera de I/O · {:.0}% ocioso",
                        delta.pct(delta.user),
                        delta.pct(delta.system),
                        delta.pct(delta.iowait),
                        delta.pct(delta.idle)
                    ),
                );
                if delta.pct(delta.iowait) > 20.0 {
                    report.grave(
                        format!(
                            "{:.0}% do tempo de CPU é espera de disco",
                            delta.pct(delta.iowait)
                        ),
                        "não é falta de CPU, é o disco não acompanhando. Os processos em D na lista abaixo são os que estão esperando",
                    );
                }
                if delta.pct(delta.steal) > 5.0 {
                    report.grave(
                        format!(
                            "{:.0}% do tempo de CPU foi roubado pelo hipervisor",
                            delta.pct(delta.steal)
                        ),
                        "a máquina virtual está dividindo CPU física com vizinhos barulhentos. Isso não se resolve aqui dentro — se resolve mudando de instância ou falando com quem hospeda",
                    );
                }
            }
        } else if self.passadas <= 1 {
            report.line("Ctrl+R lê de novo e mostra onde o tempo de CPU foi no intervalo");
        }
        self.cpu_antes = agora;
    }
}

/// As seções da resposta, pelo nome que o script deu a cada uma.
type Secoes = HashMap<String, String>;

fn campo(s: &Secoes, nome: &str) -> String {
    s.get(nome).cloned().unwrap_or_default()
}

/// `PRETTY_NAME="Ubuntu 24.04"` → `Ubuntu 24.04`.
fn valor_de(texto: &str, chave: &str) -> String {
    texto
        .lines()
        .find_map(|linha| linha.strip_prefix(&format!("{chave}=")))
        .unwrap_or("")
        .to_string()
}

fn aspas(texto: &str) -> String {
    texto.trim_matches('"').to_string()
}

/// Os tempos de CPU de `/proc/stat`, em jiffies desde o boot.
#[derive(Clone, Copy)]
struct Cpu {
    user: f64,
    nice: f64,
    system: f64,
    idle: f64,
    iowait: f64,
    irq: f64,
    softirq: f64,
    steal: f64,
}

impl Cpu {
    fn parse(linha: &str) -> Option<Self> {
        let n: Vec<f64> = linha
            .split_whitespace()
            .skip(1)
            .filter_map(|v| v.parse().ok())
            .collect();
        if n.len() < 8 {
            return None;
        }
        Some(Self {
            user: n[0],
            nice: n[1],
            system: n[2],
            idle: n[3],
            iowait: n[4],
            irq: n[5],
            softirq: n[6],
            steal: n[7],
        })
    }

    fn menos(&self, antes: &Cpu) -> Cpu {
        Cpu {
            user: (self.user - antes.user).max(0.0),
            nice: (self.nice - antes.nice).max(0.0),
            system: (self.system - antes.system).max(0.0),
            idle: (self.idle - antes.idle).max(0.0),
            iowait: (self.iowait - antes.iowait).max(0.0),
            irq: (self.irq - antes.irq).max(0.0),
            softirq: (self.softirq - antes.softirq).max(0.0),
            steal: (self.steal - antes.steal).max(0.0),
        }
    }

    fn total(&self) -> f64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }

    fn pct(&self, parte: f64) -> f64 {
        match self.total() {
            0.0 => 0.0,
            total => 100.0 * parte / total,
        }
    }
}

fn memory(report: &mut Report, s: &Secoes) {
    report.section("Memória");
    let mem: HashMap<String, f64> = campo(s, "memoria")
        .lines()
        .filter_map(|linha| {
            let (chave, resto) = linha.split_once(':')?;
            let kb: f64 = resto.split_whitespace().next()?.parse().ok()?;
            Some((chave.to_string(), kb * 1024.0))
        })
        .collect();
    let total = mem.get("MemTotal").copied().unwrap_or(0.0);
    let disponivel = mem.get("MemAvailable").copied().unwrap_or(0.0);
    if total <= 0.0 {
        report.line("não deu para ler /proc/meminfo");
        return;
    }
    report.field("total", bytes(total));
    report.field(
        "disponível",
        format!("{} ({:.0}%)", bytes(disponivel), 100.0 * disponivel / total),
    );
    report.field(
        "cache e buffers",
        bytes(
            mem.get("Cached").copied().unwrap_or(0.0) + mem.get("Buffers").copied().unwrap_or(0.0),
        ),
    );
    // «Disponível» é o número que importa: o Linux usa toda a RAM livre como cache, e
    // «usada» sozinha assusta sem motivo em toda máquina saudável.
    let livre = 100.0 * disponivel / total;
    if livre < 5.0 {
        report.grave(
            format!("só {livre:.0}% de memória disponível"),
            "a partir daqui o kernel passa a escolher quem morre. Ou é vazamento, ou é dimensionamento — a lista de processos por memória diz qual",
        );
    } else if livre < 15.0 {
        report.aviso(
            format!("{livre:.0}% de memória disponível"),
            "pouca folga para um pico. Vale olhar quem cresceu na lista de processos por memória",
        );
    } else {
        report.ok(format!("{livre:.0}% de memória disponível"));
    }

    let swap_total = mem.get("SwapTotal").copied().unwrap_or(0.0);
    let swap_livre = mem.get("SwapFree").copied().unwrap_or(0.0);
    if swap_total > 0.0 {
        let usada = swap_total - swap_livre;
        report.field(
            "swap",
            format!(
                "{} de {} em uso ({:.0}%)",
                bytes(usada),
                bytes(swap_total),
                100.0 * usada / swap_total
            ),
        );
        if usada / swap_total > 0.5 {
            report.aviso(
                format!("{:.0}% da swap em uso", 100.0 * usada / swap_total),
                "swap em uso pesado é memória de disco fingindo ser RAM: tudo que tocar aquela página espera o disco",
            );
        }
    } else {
        report.line("sem swap configurada");
    }
}

fn disk(report: &mut Report, s: &Secoes) {
    report.section("Disco");
    report.table(&["sistema de arquivos", "ponto", "tamanho", "usado", "livre"]);
    let inodes = inode_uso(&campo(s, "inodes"));
    let mut apertados: Vec<(String, f64)> = Vec::new();
    let mut vistos: std::collections::HashSet<String> = std::collections::HashSet::new();
    for linha in campo(s, "disco").lines().skip(1) {
        let colunas: Vec<&str> = linha.split_whitespace().collect();
        if colunas.len() < 6 {
            continue;
        }
        let (dispositivo, ponto) = (colunas[0], colunas[5]);
        // Um bind mount repete o mesmo dispositivo em vários pontos — dentro de um
        // container isso é a regra — e a mesma partição listada seis vezes não diz nada
        // seis vezes.
        if !vistos.insert(dispositivo.to_string()) {
            continue;
        }
        // Os sistemas de arquivos virtuais não são disco de ninguém, e 100% em `/dev` é a
        // coisa mais normal do mundo.
        if !dispositivo.starts_with('/') || ponto.starts_with("/snap") {
            continue;
        }
        let tamanho: f64 = colunas[1].parse().unwrap_or(0.0) * 1024.0;
        let usado: f64 = colunas[2].parse().unwrap_or(0.0) * 1024.0;
        let livre: f64 = colunas[3].parse().unwrap_or(0.0) * 1024.0;
        let pct = colunas[4]
            .trim_end_matches('%')
            .parse::<f64>()
            .unwrap_or(0.0);
        report.cells_headline(
            vec![
                dispositivo.to_string(),
                ponto.to_string(),
                bytes(tamanho),
                format!("{} ({pct:.0}%)", bytes(usado)),
                bytes(livre),
            ],
            match pct {
                cheio if cheio >= 90.0 => Tone::Ruim,
                apertado if apertado >= 80.0 => Tone::Aviso,
                _ => Tone::Normal,
            },
        );
        if pct >= 90.0 {
            apertados.push((ponto.to_string(), pct));
        }
    }
    for (ponto, pct) in &apertados {
        report.grave(
            format!("{ponto} está {pct:.0}% cheio"),
            "disco cheio derruba banco, log, build e tudo que escreve — e costuma acontecer de madrugada. Os maiores diretórios de /var/log aparecem no cartão de inventário",
        );
    }
    if apertados.is_empty() {
        report.ok("nenhum sistema de arquivos acima de 90%");
    }
    for (ponto, pct) in inodes {
        if pct >= 85.0 {
            report.grave(
                format!("{ponto} usou {pct:.0}% dos inodes"),
                "acabar inode é acabar disco com espaço sobrando: nenhum arquivo novo é criado, e a mensagem de erro fala de espaço. Quase sempre é um diretório com milhões de arquivos pequenos",
            );
        }
    }
    // Um sistema de arquivos que o kernel remontou como somente-leitura fez isso porque
    // achou erro de I/O. É um dos poucos sinais que não têm interpretação benigna.
    for linha in campo(s, "montagens").lines() {
        let colunas: Vec<&str> = linha.split_whitespace().collect();
        if colunas.len() >= 4
            && colunas[0].starts_with("/dev/")
            && colunas[3].split(',').any(|opcao| opcao == "ro")
        {
            report.grave(
                format!("{} está montado somente para leitura em {}", colunas[0], colunas[1]),
                "o kernel remonta assim quando encontra erro de I/O. Olhe o registro do kernel antes de qualquer outra coisa",
            );
        }
    }
}

/// Os pontos de montagem com uso de inode alto.
fn inode_uso(texto: &str) -> Vec<(String, f64)> {
    texto
        .lines()
        .skip(1)
        .filter_map(|linha| {
            let colunas: Vec<&str> = linha.split_whitespace().collect();
            if colunas.len() < 6 || !colunas[0].starts_with('/') {
                return None;
            }
            let pct = colunas[4].trim_end_matches('%').parse::<f64>().ok()?;
            Some((colunas[5].to_string(), pct))
        })
        .collect()
}

/// A pressão: quanto do tempo alguém ficou parado esperando CPU, memória ou disco.
///
/// É a métrica que o kernel ganhou justamente porque «uso de CPU em 100%» não distingue
/// uma máquina trabalhando de uma máquina sofrendo. Aqui a diferença está escrita.
fn pressure(report: &mut Report, s: &Secoes) {
    let texto = campo(s, "pressao");
    if texto.trim().is_empty() {
        return;
    }
    report.section("Pressão");
    let mut recurso = String::new();
    for linha in texto.lines() {
        if let Some(nome) = linha.strip_prefix("== ") {
            recurso = nome.to_string();
            continue;
        }
        let Some(resto) = linha.strip_prefix("some ") else {
            continue;
        };
        let avg60: f64 = resto
            .split_whitespace()
            .find_map(|par| par.strip_prefix("avg60=")?.parse().ok())
            .unwrap_or(0.0);
        let nome = match recurso.as_str() {
            "cpu" => "CPU",
            "memory" => "memória",
            "io" => "disco",
            outro => outro,
        };
        report.field(
            format!("espera por {nome}"),
            format!("{avg60:.1}% do último minuto"),
        );
        if avg60 >= 20.0 {
            report.grave(
                format!("{avg60:.0}% do tempo alguém esteve parado esperando {nome}"),
                "pressão é espera medida, não uso: um número desses quer dizer que a máquina não está dando conta do que pedem dela",
            );
        } else if avg60 >= 5.0 {
            report.aviso(
                format!("{avg60:.0}% do tempo alguém esperou por {nome}"),
                "ainda dá, mas é o começo da fila — vale saber de onde vem antes de virar incidente",
            );
        }
    }
}

fn network(report: &mut Report, s: &Secoes) {
    report.section("Rede");
    let escutando = campo(s, "escutando");
    let mut abertas: Vec<(String, String, String)> = Vec::new();
    for linha in escutando.lines() {
        let colunas: Vec<&str> = linha.split_whitespace().collect();
        // `ss -H -ltnup`: Estado Recv-Q Send-Q Local Peer Processo
        let Some(local) = colunas.get(4).or(colunas.get(3)) else {
            continue;
        };
        if !local.contains(':') {
            continue;
        }
        let processo = colunas
            .get(6)
            .map(|p| {
                p.split("((")
                    .nth(1)
                    .unwrap_or(p)
                    .trim_matches(|c| c == '"' || c == ')' || c == '(')
                    .split(',')
                    .next()
                    .unwrap_or(p)
                    .to_string()
            })
            .unwrap_or_default();
        let endereco = local
            .rsplit_once(':')
            .map(|(host, _)| host.to_string())
            .unwrap_or_default();
        abertas.push((local.to_string(), endereco, processo));
    }
    // Zero porta e nenhuma ferramenta para olhar são coisas diferentes, e a segunda não
    // pode virar um «nada escutando» tranquilizador.
    if escutando.trim().is_empty() {
        report.line("não deu para listar as portas: nem ss nem netstat responderam nesta máquina");
    } else {
        report.field("portas escutando", count(abertas.len() as f64));
    }
    if !abertas.is_empty() {
        report.table(&["endereço", "alcance", "processo"]);
        for (local, endereco, processo) in &abertas {
            let mundo =
                endereco.contains("0.0.0.0") || endereco == "*" || endereco.contains("[::]");
            report.cells(
                vec![
                    local.clone(),
                    match mundo {
                        true => "qualquer endereço".to_string(),
                        false => "local".to_string(),
                    },
                    processo.clone(),
                ],
                match mundo {
                    true => Tone::Aviso,
                    false => Tone::Normal,
                },
            );
        }
        let mundo = abertas
            .iter()
            .filter(|(_, endereco, _)| {
                endereco.contains("0.0.0.0") || endereco == "*" || endereco.contains("[::]")
            })
            .count();
        if mundo > 0 {
            report.aviso(
                format!("{} porta(s) escutando em qualquer endereço", count(mundo as f64)),
                "cada uma delas é alcançável de fora se o firewall deixar. Vale conferir uma por uma se é para ser assim",
            );
        }
    }

    let conexoes = campo(s, "conexoes");
    if !conexoes.trim().is_empty() {
        let resumo: Vec<String> = conexoes
            .lines()
            .map(|linha| linha.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        report.field("conexões TCP", resumo.join(" · "));
        // Um monte de conexão em TIME-WAIT é normal; um monte em SYN-SENT ou CLOSE-WAIT
        // não é — a primeira é alguém que não responde, a segunda é software que não
        // fecha o que abriu.
        for (estado, aviso) in [
            (
                "CLOSE-WAIT",
                "conexões que a aplicação não fechou depois que o outro lado foi embora — vazamento de descritor, e o limite de arquivos abertos é o teto",
            ),
            (
                "SYN-SENT",
                "conexões saindo para algo que não responde: firewall no caminho, ou serviço fora do ar",
            ),
        ] {
            let quantas: f64 = conexoes
                .lines()
                .find(|linha| linha.contains(estado))
                .and_then(|linha| linha.split_whitespace().next()?.parse().ok())
                .unwrap_or(0.0);
            if quantas > 50.0 {
                report.aviso(format!("{} conexões em {estado}", count(quantas)), aviso);
            }
        }
    }
}

fn processes(report: &mut Report, s: &Secoes) {
    report.section("Processos");
    report.table(&[
        "pid",
        "usuário",
        "%cpu",
        "%mem",
        "residente",
        "há",
        "comando",
    ]);
    let mut quentes: Vec<String> = Vec::new();
    for linha in campo(s, "processos_cpu").lines().skip(1).take(10) {
        let c: Vec<&str> = linha.split_whitespace().collect();
        if c.len() < 8 {
            continue;
        }
        let pcpu: f64 = c[2].parse().unwrap_or(0.0);
        let segundos: f64 = c[5].parse().unwrap_or(0.0);
        // Um processo colado no teto de um núcleo por minutos não é um pico: ou está num
        // laço, ou está fazendo algo que ninguém pediu.
        if pcpu >= 90.0 && segundos > 120.0 {
            quentes.push(format!(
                "pid {} ({}) em {pcpu:.0}% de CPU há {}: {}",
                c[0],
                c[1],
                duration(segundos),
                c[7..].join(" ")
            ));
        }
        report.cells_headline(
            vec![
                c[0].to_string(),
                c[1].to_string(),
                format!("{pcpu:.1}%"),
                format!("{}%", c[3]),
                bytes(c[4].parse::<f64>().unwrap_or(0.0) * 1024.0),
                duration(c[5].parse().unwrap_or(0.0)),
                c[7..].join(" "),
            ],
            match pcpu {
                muito if muito >= 90.0 => Tone::Aviso,
                _ => Tone::Normal,
            },
        );
    }
    for quente in &quentes {
        report.aviso(
            quente.clone(),
            "um núcleo inteiro ocupado por tanto tempo aparece na carga e na conta do provedor. Vale saber se é trabalho ou laço",
        );
    }
    report.line("os maiores por memória:");
    for linha in campo(s, "processos_memoria").lines().skip(1).take(5) {
        let c: Vec<&str> = linha.split_whitespace().collect();
        if c.len() < 8 {
            continue;
        }
        report.row(format!(
            "{:>10}  pid {} ({}) {}",
            bytes(c[4].parse::<f64>().unwrap_or(0.0) * 1024.0),
            c[0],
            c[1],
            c[7..].join(" ")
        ));
    }

    let estados = campo(s, "processos_estado");
    let conta = |letra: &str| -> f64 {
        estados
            .lines()
            .find(|linha| linha.trim().ends_with(letra))
            .and_then(|linha| linha.split_whitespace().next()?.parse().ok())
            .unwrap_or(0.0)
    };
    let zumbis = conta("Z");
    let presos = conta("D");
    report.field(
        "estados",
        estados
            .lines()
            .map(|linha| linha.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(" · "),
    );
    if zumbis > 20.0 {
        report.aviso(
            format!("{} processos zumbis", count(zumbis)),
            "zumbi é filho que morreu e o pai não recolheu. Poucos não custam nada; muitos são um pai que não faz wait, e cada um ocupa uma entrada na tabela de processos",
        );
    }
    if presos > 0.0 {
        let lista = campo(s, "processos_presos");
        report.aviso(
            format!(
                "{} processo(s) parados em espera ininterrupta",
                count(presos)
            ),
            "estado D é espera de I/O que nem sinal interrompe: quase sempre disco ou rede (NFS) que não responde. Nem kill -9 tira daí",
        );
        for linha in lista.lines().skip(1).take(5) {
            report.row(one_line(linha, 110));
        }
    }
}

fn services(report: &mut Report, s: &Secoes) {
    report.section("Serviços");
    let falhos = campo(s, "servicos_falhos");
    let rodando = campo(s, "servicos").trim().to_string();
    if !rodando.is_empty() {
        report.field("rodando", format!("{rodando} serviços"));
    }
    if falhos.trim().is_empty() {
        report.ok("nenhuma unidade do systemd em estado de falha");
        return;
    }
    report.table(&["unidade", "estado"]);
    let mut quantos = 0;
    for linha in falhos.lines() {
        let c: Vec<&str> = linha.split_whitespace().collect();
        if c.is_empty() {
            continue;
        }
        quantos += 1;
        report.cells_headline(vec![c[0].to_string(), c[1..].join(" ")], Tone::Ruim);
    }
    if quantos > 0 {
        report.grave(
            format!("{} unidade(s) do systemd falharam", count(quantos as f64)),
            "`systemctl status <unidade>` diz por quê, e `journalctl -u <unidade>` diz desde quando. Uma unidade falha que ninguém viu costuma ser a causa do problema que alguém está procurando em outro lugar",
        );
    }
}

fn errors(report: &mut Report, s: &Secoes) {
    report.section("Erros registrados");
    let oom = campo(s, "oom");
    if !oom.trim().is_empty() {
        report.grave(
            "o kernel matou processo por falta de memória (OOM killer)",
            "quando a memória acaba, o kernel escolhe uma vítima e a mata sem aviso. O que morreu aparece nas linhas abaixo, e ele não voltou sozinho a menos que alguém tenha configurado para voltar",
        );
        for linha in oom.lines().take(5) {
            report.row(one_line(linha, 130));
        }
    }
    let erros = campo(s, "erros_recentes");
    if erros.trim().is_empty() {
        if oom.trim().is_empty() {
            report.ok("nada de nível erro no registro recente — ou este usuário não lê o journal");
        }
        return;
    }
    let linhas: Vec<&str> = erros.lines().collect();
    report.field(
        "erros recentes",
        format!("{} linhas", count(linhas.len() as f64)),
    );
    report.table(&["quando", "o quê"]);
    for linha in linhas.iter().take(15) {
        let (quando, resto) = linha.split_at(linha.len().min(15));
        report.cells(
            vec![quando.trim().to_string(), one_line(resto, 140)],
            Tone::Aviso,
        );
    }
    if linhas.len() >= 10 {
        report.aviso(
            format!("{} erros no registro recente", count(linhas.len() as f64)),
            "vale ler: um erro que se repete é um serviço tentando e falhando em laço, e isso custa CPU e disco sem aparecer em lugar nenhum",
        );
    }
}

fn access(report: &mut Report, s: &Secoes) {
    report.section("Quem entra");
    let sessoes = campo(s, "sessoes");
    let (agora, antes) = sessoes.split_once("--").unwrap_or((sessoes.as_str(), ""));
    let conectados: Vec<&str> = agora.lines().filter(|l| !l.trim().is_empty()).collect();
    report.field("conectados agora", count(conectados.len() as f64));
    for linha in conectados.iter().take(6) {
        report.row(one_line(linha, 110));
    }
    if !antes.trim().is_empty() {
        report.line("últimos logins:");
        for linha in antes.lines().filter(|l| !l.trim().is_empty()).take(6) {
            report.row(one_line(linha, 110));
        }
    }

    let contas = campo(s, "contas");
    let mut humanas: Vec<String> = Vec::new();
    let mut roots: Vec<String> = Vec::new();
    for linha in contas.lines() {
        let c: Vec<&str> = linha.split(':').collect();
        if c.len() < 7 {
            continue;
        }
        let (nome, uid, shell) = (c[0], c[2], c[6]);
        if shell.contains("nologin") || shell.contains("/false") {
            continue;
        }
        if uid == "0" {
            roots.push(nome.to_string());
        } else {
            humanas.push(nome.to_string());
        }
    }
    report.field("contas com shell", humanas.join(", "));
    report.field("contas com uid 0", roots.join(", "));
    // Mais de um uid 0 é uma porta dos fundos com nome de usuário: o sistema trata os dois
    // como root, e só um deles costuma estar na cabeça de quem administra.
    if roots.len() > 1 {
        report.grave(
            format!("{} contas diferentes têm uid 0: {}", roots.len(), roots.join(", ")),
            "uid 0 é root, com qualquer nome. Uma conta assim que ninguém criou de propósito é a definição de porta dos fundos",
        );
    }
    let admins = campo(s, "administradores");
    if !admins.trim().is_empty() {
        for linha in admins.lines() {
            if let Some((grupo, membros)) = linha.rsplit_once(':') {
                let grupo = grupo.split(':').next().unwrap_or(grupo);
                report.field(format!("grupo {grupo}"), membros);
            }
        }
    }
}

fn updates(report: &mut Report, s: &Secoes) {
    report.section("Atualizações");
    let kernel = campo(s, "kernel_atual");
    let rodando = kernel.lines().next().unwrap_or("").to_string();
    let instalado = kernel
        .lines()
        .nth(1)
        .and_then(|caminho| caminho.rsplit('/').next())
        .map(|arquivo| arquivo.trim_start_matches("vmlinuz-").to_string())
        .unwrap_or_default();
    report.field("kernel rodando", &rodando);
    if !instalado.is_empty() && instalado != rodando {
        report.aviso(
            format!("o kernel instalado é {instalado}, mas quem está rodando é {rodando}"),
            "a máquina está com correção de kernel esperando um reinício — inclusive as de segurança",
        );
    }
    if kernel.contains("/var/run/reboot-required") {
        report.aviso(
            "a máquina está pedindo reinício desde a última atualização",
            "enquanto não reiniciar, parte do que foi atualizado continua rodando na versão antiga",
        );
    }
    let pendentes: f64 = campo(s, "atualizacoes")
        .lines()
        .filter_map(|linha| linha.trim().parse::<f64>().ok())
        .fold(0.0, f64::max);
    if pendentes > 0.0 {
        report.field("pacotes a atualizar", count(pendentes));
        if pendentes > 50.0 {
            report.aviso(
                format!("{} pacotes esperando atualização", count(pendentes)),
                "uma máquina muito atrás costuma estar atrás também nas correções de segurança. `apt list --upgradable` diz quais",
            );
        }
    }
}

fn security(report: &mut Report, s: &Secoes) {
    report.section("Segurança");
    let sshd = campo(s, "sshd");
    let tem = |chave: &str, valor: &str| -> bool {
        sshd.lines().any(|linha| {
            let linha = linha.trim().to_lowercase();
            linha.starts_with(chave) && linha.contains(valor)
        })
    };
    if sshd.trim().is_empty() {
        report.line("não deu para ler a configuração do sshd com este usuário");
    } else {
        for linha in sshd.lines().take(10) {
            report.row(linha.trim().to_string());
        }
        if tem("permitrootlogin", "yes") {
            report.grave(
                "o sshd aceita login direto como root",
                "todo ataque de força bruta tenta root primeiro, e um acesso como root não deixa rastro de quem era a pessoa. PermitRootLogin prohibit-password, ou no",
            );
        }
        if tem("passwordauthentication", "yes") {
            report.aviso(
                "o sshd aceita senha",
                "senha é adivinhável e reutilizável; chave não é. Com chave configurada para todo mundo que precisa entrar, PasswordAuthentication no fecha a porta da força bruta",
            );
        }
        if tem("permitemptypasswords", "yes") {
            report.grave(
                "o sshd aceita senha vazia",
                "não há situação em que isto seja o que se queria",
            );
        }
    }

    let firewall = campo(s, "firewall");
    if firewall.trim().is_empty() {
        report.line("não deu para ver o firewall com este usuário (precisa de root)");
    } else if firewall.to_lowercase().contains("inactive") {
        report.aviso(
            "o firewall está desligado",
            "toda porta que estiver escutando em qualquer endereço está alcançável de onde a rede alcançar. A lista está no cartão de Rede",
        );
    } else {
        report.field("firewall", one_line(&firewall, 120));
    }

    let chaves = campo(s, "chaves_autorizadas");
    if let Ok(quantas) = chaves.trim().parse::<f64>()
        && quantas > 0.0
    {
        report.field("chaves autorizadas deste usuário", count(quantas));
        if quantas > 5.0 {
            report.aviso(
                format!("{} chaves podem entrar como este usuário", count(quantas)),
                "cada linha do authorized_keys é uma pessoa ou um robô que entra sem senha. Uma lista que só cresce é uma lista que ninguém revisa",
            );
        }
    }
}

fn inventory(report: &mut Report, s: &Secoes) {
    report.section("Inventário");
    let pacotes: f64 = campo(s, "pacotes")
        .lines()
        .filter_map(|linha| linha.trim().parse::<f64>().ok())
        .fold(0.0, f64::max);
    if pacotes > 0.0 {
        report.field("pacotes instalados", count(pacotes));
    }
    let containers = campo(s, "containers");
    if !containers.trim().is_empty() {
        let linhas: Vec<&str> = containers
            .lines()
            .filter(|l| !l.trim().is_empty())
            .collect();
        report.field(
            "containers",
            plural(linhas.len() as f64, "rodando", "rodando"),
        );
        report.table(&["container", "estado", "imagem"]);
        for linha in linhas.iter().take(12) {
            let partes: Vec<&str> = linha.split('\t').collect();
            report.cells(
                vec![
                    partes.first().unwrap_or(&"").to_string(),
                    partes.get(1).unwrap_or(&"").to_string(),
                    partes.get(2).unwrap_or(&"").to_string(),
                ],
                Tone::Normal,
            );
        }
    }
    let ajustes = campo(s, "ajustes_kernel");
    if !ajustes.trim().is_empty() {
        report.field("ajustes do kernel", ajustes.replace('\n', " · "));
    }
    let descritores = campo(s, "descritores");
    if let Some((abertos, _, maximo)) = descritores
        .split_whitespace()
        .collect::<Vec<_>>()
        .split_first()
        .and_then(|(a, resto)| Some((*a, resto.first()?, resto.get(1)?)))
    {
        let (abertos, maximo): (f64, f64) = (
            abertos.parse().unwrap_or(0.0),
            maximo.parse().unwrap_or(0.0),
        );
        if maximo > 0.0 {
            report.field(
                "arquivos abertos",
                format!(
                    "{} de {} ({:.1}%)",
                    count(abertos),
                    count(maximo),
                    100.0 * abertos / maximo
                ),
            );
            if abertos / maximo > 0.7 {
                report.aviso(
                    format!("{:.0}% do limite de arquivos abertos em uso", 100.0 * abertos / maximo),
                    "quando acabar, todo accept e todo open falham ao mesmo tempo, em tudo que roda na máquina",
                );
            }
        }
    }
    let logs = campo(s, "logs");
    if let Some(kb) = logs
        .split_whitespace()
        .next()
        .and_then(|n| n.parse::<f64>().ok())
    {
        report.field("/var/log", bytes(kb * 1024.0));
    }
    let agendados = campo(s, "agendados");
    if !agendados.trim().is_empty() {
        report.line("tarefas agendadas:");
        for linha in agendados.lines().filter(|l| !l.trim().is_empty()).take(10) {
            report.row(one_line(linha, 120));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_tempos_de_cpu_viram_porcentagem_do_intervalo() {
        // Dois retratos de /proc/stat: entre eles, 100 tiques de usuário e 100 de espera
        // de I/O, e mais nada.
        let antes = Cpu::parse("cpu  1000 0 500 8000 200 0 0 0").unwrap();
        let agora = Cpu::parse("cpu  1100 0 500 8000 300 0 0 0").unwrap();
        let delta = agora.menos(&antes);
        assert_eq!(delta.total(), 200.0);
        assert!((delta.pct(delta.user) - 50.0).abs() < 0.01);
        assert!((delta.pct(delta.iowait) - 50.0).abs() < 0.01);
        assert_eq!(delta.pct(delta.idle), 0.0);
    }

    /// Contador que volta para trás é reinício da máquina, não trabalho negativo.
    #[test]
    fn contador_que_regride_nao_vira_numero_negativo() {
        let antes = Cpu::parse("cpu  5000 0 5000 5000 5000 0 0 0").unwrap();
        let agora = Cpu::parse("cpu  10 0 10 10 10 0 0 0").unwrap();
        let delta = agora.menos(&antes);
        assert_eq!(delta.user, 0.0);
        assert_eq!(delta.total(), 0.0);
        assert_eq!(delta.pct(delta.user), 0.0, "sem divisão por zero");
    }

    #[test]
    fn linha_curta_do_proc_stat_nao_vira_pane() {
        assert!(Cpu::parse("cpu 1 2 3").is_none());
        assert!(Cpu::parse("").is_none());
    }

    #[test]
    fn o_os_release_entrega_o_nome_bonito() {
        let texto = "NAME=\"Ubuntu\"\nVERSION=\"24.04.1 LTS\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nLinux 6.8.0-88-generic";
        assert_eq!(aspas(&valor_de(texto, "PRETTY_NAME")), "Ubuntu 24.04.1 LTS");
        assert_eq!(valor_de(texto, "NAO_EXISTE"), "");
        assert_eq!(
            texto.lines().find(|l| l.starts_with("Linux ")),
            Some("Linux 6.8.0-88-generic"),
            "o kernel é a linha do uname, não uma do os-release"
        );
    }

    #[test]
    fn os_inodes_saem_do_df_i() {
        let texto = "Filesystem Inodes IUsed IFree IUse% Mounted on\n\
                     /dev/sda1 1000 900 100 90% /\n\
                     tmpfs 500 1 499 1% /run";
        let uso = inode_uso(texto);
        assert_eq!(uso.len(), 1, "só o que é disco de verdade");
        assert_eq!(uso[0].0, "/");
        assert!((uso[0].1 - 90.0).abs() < 0.01);
    }
}
