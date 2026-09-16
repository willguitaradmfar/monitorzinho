//! O que perguntar a um cluster de Kubernetes, e o que as respostas querem dizer.
//!
//! O repertório é o de quem recebe um kubeconfig de um cluster que nunca viu e tem uma
//! hora para dizer o que há de errado nele: se os nós estão inteiros, o que não está
//! subindo e por quê, quanto do cluster já está prometido, o que vai cair na primeira
//! manutenção, o que está aberto para o mundo, quem pode tudo, e o que a plataforma tem
//! de quebrado por baixo dos aplicativos.
//!
//! Nada aqui altera o cluster — ver `k8s::CONSULTAS`, que é a lista inteira do que foi
//! perguntado, e `k8s::permitido`, que recusa qualquer outra coisa antes de ela virar um
//! processo. Uma pergunta que o usuário do kubeconfig não tem permissão para fazer
//! simplesmente falha, e uma falha dessas vira «não deu para ler com este acesso» na
//! tela: uma resposta honesta, nunca um atestado de saúde.
//!
//! A divisão é a de sempre: `k8s.rs` colhe, este arquivo conclui.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

use super::{Depth, Plan, Report, bytes, count, duration, k8s, one_line, plural};
use crate::painel::Tone;

/// O `minor` mais novo do Kubernetes que existia quando isto foi escrito.
///
/// O projeto mantém as **três** últimas versões: quando a diferença passa disso, o
/// cluster está fora de suporte e não recebe nem correção de segurança. Este número
/// envelhece de propósito — um cluster mais novo que ele aparece como «mais novo que esta
/// ferramenta conhece», que é verdade e não assusta ninguém.
const MINOR_CONHECIDO: u64 = 35;

/// Acima disto, um contêiner que reinicia não é mais um susto: é um padrão.
const REINICIOS_DEMAIS: u64 = 5;
/// Quanto de um recurso já prometido faz o nó ficar sem folga para um pico.
const PROMETIDO_APERTADO: f64 = 90.0;
/// Quanto tempo um pod pode passar em `Terminating` antes de ser um pod preso.
const PRESO: f64 = 300.0;

/// Uma investigação de um cluster.
///
/// Guarda o retrato anterior porque a pergunta que mais importa num painel aberto não é
/// «quantos reinícios existem», e sim «quantos apareceram desde que eu olhei».
pub struct Session {
    target: k8s::Target,
    depth: Depth,
    passadas: u64,
    antes: Option<Retrato>,
}

/// Os números de uma passada, para a próxima ter contra o que comparar.
#[derive(Clone, Default)]
struct Retrato {
    pods: usize,
    reinicios: u64,
    avisos: usize,
    nos_prontos: usize,
}

/// Tudo que o cluster respondeu nesta passada, e o que ele recusou a responder.
struct Leitura {
    docs: HashMap<&'static str, Value>,
    /// Texto puro das consultas que não são JSON — `auth can-i`, `top`, `/readyz`.
    textos: HashMap<&'static str, String>,
    /// O que não deu para ler, e o motivo. Nunca some: uma seção que falta por falta de
    /// permissão tem que aparecer dita, senão o silêncio dela passa por «está tudo bem».
    falhas: Vec<(&'static str, String)>,
}

impl Leitura {
    fn doc(&self, nome: &str) -> Option<&Value> {
        self.docs.get(nome)
    }

    /// Os objetos de uma listagem. Vazio quando a consulta não foi feita ou falhou — e
    /// quem chama nunca precisa distinguir, porque `falhas` já registrou.
    fn itens(&self, nome: &str) -> &[Value] {
        self.doc(nome)
            .and_then(|d| d.get("items"))
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn texto(&self, nome: &str) -> &str {
        self.textos.get(nome).map(String::as_str).unwrap_or("")
    }

    fn falhou(&self, nome: &str) -> Option<&str> {
        self.falhas
            .iter()
            .find(|(n, _)| *n == nome)
            .map(|(_, motivo)| motivo.as_str())
    }
}

impl Session {
    pub fn open(plan: &Plan) -> Result<Self, String> {
        let target = k8s::Target::parse(plan)?;
        Ok(Self {
            target,
            depth: plan.depth,
            passadas: 0,
            antes: None,
        })
    }

    /// Uma investigação inteira: todas as perguntas da profundidade, e a resposta
    /// repartida em cartões.
    pub fn pass(&mut self, report: &mut Report) -> Result<(), String> {
        self.passadas += 1;
        let leitura = self.ler(report)?;

        self.cluster(report, &leitura);
        nos(report, &leitura);
        capacidade(report, &leitura);
        pods(report, &leitura);
        eventos(report, &leitura);
        if self.depth >= Depth::Medio {
            cargas(report, &leitura);
            consumo(report, &leitura);
            recursos(report, &leitura);
            resiliencia(report, &leitura);
            seguranca(report, &leitura);
            rbac(report, &leitura);
            rede(report, &leitura);
            armazenamento(report, &leitura);
        }
        if self.depth >= Depth::Profundo {
            plataforma(report, &leitura);
            segredos(report, &leitura);
        }
        self.movimento(report, &leitura);
        Ok(())
    }

    /// Faz todas as perguntas da profundidade, uma de cada vez.
    ///
    /// Uma de cada vez, e não todas em paralelo: trinta chamadas simultâneas a um
    /// `kube-apiserver` já ocupado são exatamente o tipo de carga que esta ferramenta
    /// existe para encontrar. O custo é alguns segundos a mais numa leitura que acontece
    /// quando alguém pede.
    ///
    /// Só uma falha é fatal: a primeira pergunta. Se nem a versão do servidor responde,
    /// não há cluster do outro lado — endereço errado, credencial vencida, ou API fora
    /// do ar. Todas as outras viram linha em `falhas`, porque um kubeconfig de
    /// desenvolvedor legitimamente não enxerga metade do cluster.
    fn ler(&self, report: &mut Report) -> Result<Leitura, String> {
        let mut leitura = Leitura {
            docs: HashMap::new(),
            textos: HashMap::new(),
            falhas: Vec::new(),
        };
        for consulta in k8s::consultas(self.depth) {
            if report.stopping() {
                break;
            }
            let json = consulta.args.contains(&"json");
            let resultado = match json {
                true => self.target.json(consulta).map(Saida::Json),
                false => self.target.run(consulta).map(Saida::Texto),
            };
            match (resultado, consulta.nome) {
                (Ok(Saida::Json(v)), _) => {
                    leitura.docs.insert(consulta.nome, v);
                }
                (Ok(Saida::Texto(t)), _) => {
                    leitura.textos.insert(consulta.nome, t);
                }
                (Err(problema), "versao") => return Err(problema),
                (Err(problema), nome) => leitura.falhas.push((nome, problema)),
            }
        }
        Ok(leitura)
    }

    /// Quem é este cluster, e o que respondeu desta vez.
    fn cluster(&self, report: &mut Report, l: &Leitura) {
        report.section("Cluster");
        report.field("acesso", self.target.summary());
        // A linha como seria digitada: é o caminho mais curto entre «não respondeu» e
        // saber por quê, e ela nunca carrega credencial — só o caminho do arquivo.
        if let Some(primeira) = k8s::consultas(self.depth).next() {
            report.line(self.target.command_line(primeira));
        }
        let versao = l.doc("versao").cloned().unwrap_or(Value::Null);
        let servidor = txt(&versao, "serverVersion.gitVersion");
        let cliente = txt(&versao, "clientVersion.gitVersion");
        report.field("servidor", servidor);
        report.field("kubectl", cliente);
        report.field("plataforma", txt(&versao, "serverVersion.platform"));
        report.field("nós", count(l.itens("nos").len() as f64));
        report.field("namespaces", count(l.itens("namespaces").len() as f64));
        report.field("pods", count(l.itens("pods").len() as f64));
        if !self.target.namespace.is_empty() {
            report.line(format!(
                "a investigação está restrita ao namespace {} — o que é do cluster inteiro continua sendo do cluster inteiro",
                self.target.namespace
            ));
        }

        // A versão do servidor decide se este cluster ainda recebe correção de segurança.
        match minor(&versao, "serverVersion") {
            Some(m) if m + 3 < MINOR_CONHECIDO => report.grave(
                format!(
                    "Kubernetes 1.{m} fora de suporte — o projeto mantém as três últimas versões"
                ),
                "planeje o upgrade: um cluster fora de suporte não recebe nem correção de segurança",
            ),
            Some(m) if m + 3 == MINOR_CONHECIDO => report.aviso(
                format!("Kubernetes 1.{m} está no fim do suporte"),
                "agende o upgrade antes de a versão sair da janela das três últimas",
            ),
            Some(m) if m > MINOR_CONHECIDO => report.line(format!(
                "1.{m} é mais novo que o que esta ferramenta conhece (1.{MINOR_CONHECIDO}) — nada a dizer sobre suporte"
            )),
            _ => {}
        }
        // O `kubectl` só é suportado a um minor de distância do servidor, para os dois
        // lados. Fora disso, um campo novo simplesmente some da resposta sem avisar.
        if let (Some(s), Some(c)) = (
            minor(&versao, "serverVersion"),
            minor(&versao, "clientVersion"),
        ) && s.abs_diff(c) > 1
        {
            report.aviso(
                format!("kubectl 1.{c} contra servidor 1.{s} — mais de um minor de diferença"),
                "use um kubectl a no máximo um minor do servidor: fora disso a saída pode vir incompleta sem erro nenhum",
            );
        }

        // O que o próprio control plane diz sobre si. Num cluster gerenciado costuma ser
        // negado, e negado é resposta — não é falha do cluster.
        let readyz = l.texto("readyz");
        if !readyz.trim().is_empty() {
            let ruins: Vec<&str> = readyz
                .lines()
                .filter(|linha| linha.starts_with("[-]"))
                .collect();
            match ruins.is_empty() {
                true => report.ok(format!(
                    "control plane responde pronto em {}",
                    plural(
                        readyz.lines().filter(|l| l.starts_with("[+]")).count() as f64,
                        "verificação",
                        "verificações"
                    )
                )),
                false => report.grave(
                    format!("control plane com {} falhando: {}", ruins.len(), one_line(&ruins.join(" "), 80)),
                    "olhe o componente citado antes de qualquer coisa — com ele fora, o resto do diagnóstico fica duvidoso",
                ),
            }
        } else if let Some(motivo) = l.falhou("readyz") {
            report.line(format!(
                "/readyz não respondeu a este acesso ({})",
                one_line(motivo, 60)
            ));
        }

        // O que não deu para ler, dito de uma vez. Sem isto, um kubeconfig sem permissão
        // produziria um painel quase vazio que pareceria um cluster saudável.
        if !l.falhas.is_empty() {
            report.table(&["Consulta", "Motivo"]);
            for (nome, motivo) in &l.falhas {
                report.cells(vec![nome.to_string(), one_line(motivo, 90)], Tone::Dim);
            }
            // O consumo tem cartão próprio, que explica a ausência do metrics-server
            // melhor do que esta linha explicaria. Contá-lo aqui seria dizer duas vezes
            // a mesma coisa, e uma delas sem o motivo.
            let mudas = l
                .falhas
                .iter()
                .filter(|(nome, _)| !nome.starts_with("consumo_"))
                .count();
            if mudas > 0 {
                report.aviso(
                    format!(
                        "não deu para ler {} com este acesso",
                        plural(mudas as f64, "consulta", "consultas")
                    ),
                    "o que falta aqui não foi avaliado — não confunda com «está tudo bem»",
                );
            }
        }
        report.line(format!(
            "{} perguntas de leitura, nenhuma alteração",
            k8s::consultas(self.depth).count()
        ));
    }

    /// O que mudou desde a passada anterior.
    fn movimento(&mut self, report: &mut Report, l: &Leitura) {
        let agora = Retrato {
            pods: l.itens("pods").len(),
            reinicios: l.itens("pods").iter().map(reinicios_do_pod).sum(),
            avisos: l
                .itens("eventos")
                .iter()
                .filter(|e| txt(e, "type") == "Warning")
                .count(),
            nos_prontos: l.itens("nos").iter().filter(|n| no_pronto(n)).count(),
        };
        report.section("Movimento");
        let Some(antes) = self.antes.replace(agora.clone()) else {
            report.line("primeira leitura — a próxima diz o que mudou desde esta");
            report.field("pods agora", count(agora.pods as f64));
            report.field("reinícios acumulados", count(agora.reinicios as f64));
            return;
        };
        report.field("passadas", count(self.passadas as f64));
        delta(report, "pods", antes.pods as f64, agora.pods as f64);
        delta(
            report,
            "nós prontos",
            antes.nos_prontos as f64,
            agora.nos_prontos as f64,
        );
        delta(
            report,
            "avisos na janela",
            antes.avisos as f64,
            agora.avisos as f64,
        );
        let novos = agora.reinicios.saturating_sub(antes.reinicios);
        report.field("reinícios desde a anterior", count(novos as f64));
        if novos > 0 {
            report.aviso(
                format!("{} reinício(s) entre as duas leituras", novos),
                "um contêiner que reinicia entre duas investigações está reiniciando agora — olhe o cartão de Pods",
            );
        }
        if agora.nos_prontos < antes.nos_prontos {
            report.grave(
                "um nó saiu do ar entre as duas leituras".to_string(),
                "veja o cartão de Nós: um nó que some leva os pods dele junto",
            );
        }
    }
}

enum Saida {
    Json(Value),
    Texto(String),
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Os nós: a máquina de verdade por baixo de tudo.
// ─────────────────────────────────────────────────────────────────────────────────────

fn nos(report: &mut Report, l: &Leitura) {
    report.section("Nós");
    let nos = l.itens("nos");
    if nos.is_empty() {
        report.line("nenhum nó visível com este acesso");
        return;
    }
    // Quanto cada nó já tem prometido. É a soma dos *requests* dos pods que estão nele —
    // o número que o escalonador usa, e não o consumo, que é outra conversa (ver
    // «Consumo»).
    let mut prometido: HashMap<String, (f64, f64, usize)> = HashMap::new();
    for pod in l.itens("pods") {
        if matches!(txt(pod, "status.phase"), "Succeeded" | "Failed") {
            continue;
        }
        let no = txt(pod, "spec.nodeName").to_string();
        if no.is_empty() {
            continue;
        }
        let (cpu, mem) = pedido_do_pod(pod);
        let entrada = prometido.entry(no).or_insert((0.0, 0.0, 0));
        entrada.0 += cpu;
        entrada.1 += mem;
        entrada.2 += 1;
    }

    report.table(&[
        "Nó",
        "Papel",
        "Versão",
        "Estado",
        "Idade",
        "CPU pedida",
        "Memória pedida",
        "Pods",
    ]);
    let mut versoes: HashSet<String> = HashSet::new();
    let mut controle = 0usize;
    for no in nos {
        let nome = txt(no, "metadata.name").to_string();
        let versao = txt(no, "status.nodeInfo.kubeletVersion").to_string();
        versoes.insert(versao.clone());
        let papel = papel_do_no(no);
        if papel.contains("control-plane") || papel.contains("master") {
            controle += 1;
        }
        let (cpu_pedida, mem_pedida, pods_no_no) =
            prometido.get(&nome).copied().unwrap_or((0.0, 0.0, 0));
        let cpu_total = cpu_de(txt(no, "status.allocatable.cpu"));
        let mem_total = mem_de(txt(no, "status.allocatable.memory"));
        let pods_max = txt(no, "status.allocatable.pods")
            .parse::<f64>()
            .unwrap_or(0.0);
        let pronto = no_pronto(no);
        let cordoned = no.pointer("/spec/unschedulable").and_then(Value::as_bool) == Some(true);
        let estado = match (pronto, cordoned) {
            (false, _) => "NotReady".to_string(),
            (true, true) => "Pronto, isolado".to_string(),
            (true, false) => "Pronto".to_string(),
        };
        let tom = match (pronto, cordoned) {
            (false, _) => Tone::Ruim,
            (true, true) => Tone::Aviso,
            _ => Tone::Normal,
        };

        let pressoes: Vec<String> = condicoes_verdadeiras(no)
            .into_iter()
            .filter(|c| c.ends_with("Pressure") || c == "NetworkUnavailable")
            .collect();
        let taints: Vec<String> = no
            .pointer("/spec/taints")
            .and_then(Value::as_array)
            .map(|ts| {
                ts.iter()
                    .map(|t| format!("{}={}:{}", txt(t, "key"), txt(t, "value"), txt(t, "effect")))
                    .collect()
            })
            .unwrap_or_default();

        report.cells_deep(
            vec![
                nome.clone(),
                papel.clone(),
                versao.clone(),
                estado.clone(),
                duration(idade(no)),
                pct_de(cpu_pedida, cpu_total, &|v| format!("{v:.0}m")),
                pct_de(mem_pedida, mem_total, &bytes),
                format!("{pods_no_no}/{pods_max:.0}"),
            ],
            tom,
            vec![
                ("sistema", txt(no, "status.nodeInfo.osImage").to_string()),
                (
                    "kernel",
                    txt(no, "status.nodeInfo.kernelVersion").to_string(),
                ),
                (
                    "runtime",
                    txt(no, "status.nodeInfo.containerRuntimeVersion").to_string(),
                ),
                (
                    "arquitetura",
                    txt(no, "status.nodeInfo.architecture").to_string(),
                ),
                ("CPU total", format!("{:.0}m", cpu_total)),
                ("memória total", bytes(mem_total)),
                ("condições ligadas", condicoes_verdadeiras(no).join(", ")),
                ("taints", taints.join(" · ")),
                ("endereços", enderecos_do_no(no)),
                ("criado", txt(no, "metadata.creationTimestamp").to_string()),
            ],
            Vec::new(),
        );
        report.destacar();

        if !pronto {
            report.grave(
                format!("nó {nome} não está pronto"),
                "olhe o kubelet e a rede desse nó — os pods dele vão ser despejados quando o prazo vencer",
            );
        }
        for pressao in &pressoes {
            report.grave(
                format!("nó {nome} sob {pressao}"),
                "o kubelet já está despejando ou vai despejar pods nesse nó; libere disco, memória ou PIDs",
            );
        }
        if cordoned {
            report.aviso(
                format!("nó {nome} está isolado (cordon) e não recebe pods"),
                "se não é manutenção em andamento, alguém esqueceu de liberar: kubectl uncordon",
            );
        }
        if cpu_total > 0.0 && cpu_pedida / cpu_total * 100.0 > PROMETIDO_APERTADO {
            report.aviso(
                format!(
                    "nó {nome} com {:.0}% da CPU já prometida",
                    cpu_pedida / cpu_total * 100.0
                ),
                "sem folga, o próximo pod não agenda nesse nó — e um pico não tem para onde crescer",
            );
        }
        if mem_total > 0.0 && mem_pedida / mem_total * 100.0 > PROMETIDO_APERTADO {
            report.aviso(
                format!(
                    "nó {nome} com {:.0}% da memória já prometida",
                    mem_pedida / mem_total * 100.0
                ),
                "memória é o recurso que não comprime: o que passar disso é OOM, não lentidão",
            );
        }
        if pods_max > 0.0 && pods_no_no as f64 / pods_max > 0.9 {
            report.aviso(
                format!("nó {nome} em {pods_no_no} de {pods_max:.0} pods"),
                "o limite de pods por nó é do kubelet (--max-pods) e não do recurso: perto dele, nada mais agenda ali",
            );
        }
    }

    if versoes.len() > 1 {
        let mut lista: Vec<String> = versoes.iter().cloned().collect();
        lista.sort();
        report.aviso(
            format!("kubelets em versões diferentes: {}", lista.join(", ")),
            "um upgrade parado no meio: termine a fila de nós antes que a diferença passe de um minor do control plane",
        );
    }
    match controle {
        0 => report.line("nenhum nó de control plane aparece na lista — cluster gerenciado, control plane é do provedor"),
        1 => report.aviso(
            "um único nó de control plane".to_string(),
            "esse nó é o cluster inteiro: perdê-lo é perder a API e o etcd junto. Em produção, três",
        ),
        n if n % 2 == 0 => report.aviso(
            format!("{n} nós de control plane — número par"),
            "o etcd elege por maioria: com número par, um a menos não muda a tolerância e ainda custa uma máquina",
        ),
        _ => {}
    }
}

/// Quanto do cluster já está prometido, e quanto sobra.
fn capacidade(report: &mut Report, l: &Leitura) {
    report.section("Capacidade");
    let nos = l.itens("nos");
    if nos.is_empty() {
        report.line("sem nós visíveis — nada a somar");
        return;
    }
    let mut cpu_total = 0.0;
    let mut mem_total = 0.0;
    let mut pods_max = 0.0;
    for no in nos {
        // Só o que está pronto entra no denominador: um nó fora do ar não é capacidade.
        if !no_pronto(no) {
            continue;
        }
        cpu_total += cpu_de(txt(no, "status.allocatable.cpu"));
        mem_total += mem_de(txt(no, "status.allocatable.memory"));
        pods_max += txt(no, "status.allocatable.pods")
            .parse::<f64>()
            .unwrap_or(0.0);
    }

    let (mut cpu_ped, mut mem_ped, mut cpu_lim, mut mem_lim) = (0.0, 0.0, 0.0, 0.0);
    let mut vivos = 0usize;
    let mut sem_pedido = 0usize;
    let mut esperando = 0usize;
    for pod in l.itens("pods") {
        if matches!(txt(pod, "status.phase"), "Succeeded" | "Failed") {
            continue;
        }
        // Só o que já está **num nó** conta como prometido. Um pod Pending não ocupa
        // lugar nenhum — somar o pedido dele diria que o cluster está 800% cheio porque
        // alguém pediu 64 CPUs que ninguém deu, e o número que importa aqui é o quanto
        // do cluster já foi de fato entregue.
        if txt(pod, "spec.nodeName").is_empty() {
            esperando += 1;
            continue;
        }
        vivos += 1;
        let (cpu, mem) = pedido_do_pod(pod);
        let (lcpu, lmem) = limite_do_pod(pod);
        cpu_ped += cpu;
        mem_ped += mem;
        cpu_lim += lcpu;
        mem_lim += lmem;
        if cpu == 0.0 && mem == 0.0 {
            sem_pedido += 1;
        }
    }

    report.field("CPU alocável", format!("{cpu_total:.0}m"));
    report.field(
        "CPU pedida",
        pct_de(cpu_ped, cpu_total, &|v| format!("{v:.0}m")),
    );
    report.field("memória alocável", bytes(mem_total));
    report.field("memória pedida", pct_de(mem_ped, mem_total, &bytes));
    report.field("pods", format!("{vivos} de {pods_max:.0} possíveis"));
    if esperando > 0 {
        report.field(
            "esperando um nó",
            format!(
                "{} — fora da conta acima, porque ninguém os agendou ainda",
                plural(esperando as f64, "pod", "pods")
            ),
        );
    }
    report.field(
        "limites somados",
        format!("CPU {cpu_lim:.0}m · memória {}", bytes(mem_lim)),
    );

    if cpu_total > 0.0 {
        let uso = cpu_ped / cpu_total * 100.0;
        if uso > PROMETIDO_APERTADO {
            report.grave(
                format!("{uso:.0}% da CPU do cluster já prometida"),
                "não há onde agendar o próximo pod nem para onde escoar um nó em manutenção; adicione nó ou reveja os requests",
            );
        } else if uso > 75.0 {
            report.aviso(
                format!("{uso:.0}% da CPU do cluster prometida"),
                "perto do ponto em que perder um nó deixa de caber no resto — confira se a carga de um nó cabe nos outros",
            );
        }
    }
    if mem_total > 0.0 && mem_lim > mem_total {
        report.aviso(
            format!(
                "limites de memória somam {} para {} de cluster",
                bytes(mem_lim),
                bytes(mem_total)
            ),
            "overcommit de memória: se todo mundo usar o que pode, o kubelet começa a matar pod. Aceitável de propósito, perigoso por acidente",
        );
    }
    // A conta que decide se o cluster sobrevive a perder uma máquina.
    if nos.len() > 1 && cpu_total > 0.0 {
        let maior = nos
            .iter()
            .filter(|n| no_pronto(n))
            .map(|n| cpu_de(txt(n, "status.allocatable.cpu")))
            .fold(0.0, f64::max);
        let sem_ele = cpu_total - maior;
        if sem_ele > 0.0 && cpu_ped > sem_ele {
            report.aviso(
                "a carga pedida não cabe no cluster se o maior nó cair".to_string(),
                "é o teste de N-1: hoje, drenar esse nó deixa pod pendurado em Pending",
            );
        }
    }
    if sem_pedido > 0 {
        report.aviso(
            format!(
                "{} sem requests de CPU e memória",
                plural(sem_pedido as f64, "pod", "pods")
            ),
            "sem request o escalonador acha que o pod não pesa nada, e o nó recebe mais do que aguenta — ver o cartão de Recursos",
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// O que está rodando, e o que não está.
// ─────────────────────────────────────────────────────────────────────────────────────

/// Os pods que não estão bem, com o motivo de cada um.
fn pods(report: &mut Report, l: &Leitura) {
    report.section("Pods");
    let todos = l.itens("pods");
    if todos.is_empty() {
        report.line("nenhum pod visível com este acesso");
        return;
    }
    let mut fases: BTreeMap<&str, usize> = BTreeMap::new();
    for pod in todos {
        *fases.entry(txt(pod, "status.phase")).or_default() += 1;
    }
    report.field(
        "por fase",
        fases
            .iter()
            .map(|(fase, n)| format!("{fase} {n}"))
            .collect::<Vec<_>>()
            .join(" · "),
    );

    let mut problemas: Vec<(&Value, Problema)> = todos
        .iter()
        .filter_map(|pod| problema_do_pod(pod).map(|p| (pod, p)))
        .collect();
    // O pior primeiro: quem está parado antes de quem está só feio.
    problemas.sort_by_key(|(_, p)| p.peso);

    if problemas.is_empty() {
        report.ok(format!(
            "{} sem nada a apontar",
            plural(todos.len() as f64, "pod", "pods")
        ));
        return;
    }

    // Os achados primeiro, a tabela depois: agrupados, porque dez pods do mesmo
    // Deployment em CrashLoopBackOff são um problema e não dez — e porque a primeira
    // linha vermelha de um cartão é a que vira o «o pior agora» do resumo, e ali cabe
    // uma frase, não uma linha de tabela.
    let mut por_motivo: BTreeMap<String, (usize, Tone, String)> = BTreeMap::new();
    for (_, p) in &problemas {
        let entrada =
            por_motivo
                .entry(p.familia.to_string())
                .or_insert((0, p.tom, p.conserto.to_string()));
        entrada.0 += 1;
    }
    for (motivo, (quantos, tom, conserto)) in por_motivo {
        let frase = format!("{} em {motivo}", plural(quantos as f64, "pod", "pods"));
        match tom {
            Tone::Ruim => report.grave(frase, conserto),
            _ => report.aviso(frase, conserto),
        }
    }

    report.table(&["Namespace", "Pod", "Problema", "Reinícios", "Idade", "Nó"]);
    for (pod, p) in &problemas {
        let ns = txt(pod, "metadata.namespace").to_string();
        let nome = txt(pod, "metadata.name").to_string();
        report.cells_deep(
            vec![
                ns.clone(),
                nome.clone(),
                p.rotulo.clone(),
                count(reinicios_do_pod(pod) as f64),
                duration(idade(pod)),
                txt(pod, "spec.nodeName").to_string(),
            ],
            p.tom,
            vec![
                ("fase", txt(pod, "status.phase").to_string()),
                ("motivo", txt(pod, "status.reason").to_string()),
                ("mensagem", one_line(txt(pod, "status.message"), 200)),
                ("qos", txt(pod, "status.qosClass").to_string()),
                ("nó", txt(pod, "spec.nodeName").to_string()),
                (
                    "conta de serviço",
                    txt(pod, "spec.serviceAccountName").to_string(),
                ),
                ("imagens", imagens_do_pod(pod).join(" · ")),
                ("controlador", dono(pod)),
                ("criado", txt(pod, "metadata.creationTimestamp").to_string()),
            ],
            p.detalhe.clone(),
        );
        report.destacar();
    }
}

/// O que há de errado com um pod, se há.
struct Problema {
    rotulo: String,
    familia: &'static str,
    conserto: &'static str,
    tom: Tone,
    /// Quanto o problema pesa na ordenação — menor aparece antes.
    peso: u8,
    detalhe: Vec<String>,
}

fn problema_do_pod(pod: &Value) -> Option<Problema> {
    let fase = txt(pod, "status.phase");
    let estados: Vec<&Value> = pod
        .pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .map(|v| v.iter().collect())
        .unwrap_or_default();
    let iniciais: Vec<&Value> = pod
        .pointer("/status/initContainerStatuses")
        .and_then(Value::as_array)
        .map(|v| v.iter().collect())
        .unwrap_or_default();

    // Esperando: o motivo está no `waiting.reason` de algum contêiner, e é ele que
    // diferencia «não consigo baixar a imagem» de «caí e vou tentar de novo».
    for c in estados.iter().chain(iniciais.iter()) {
        let motivo = txt(c, "state.waiting.reason");
        let mensagem = one_line(txt(c, "state.waiting.message"), 300);
        match motivo {
            "CrashLoopBackOff" => {
                return Some(Problema {
                    rotulo: format!("CrashLoopBackOff ({})", txt(c, "name")),
                    familia: "CrashLoopBackOff",
                    conserto: "o contêiner sobe e morre: veja o último código de saída e os logs da encarnação anterior (kubectl logs --previous)",
                    tom: Tone::Ruim,
                    peso: 1,
                    detalhe: vec![mensagem, saida_anterior(c)],
                });
            }
            "ImagePullBackOff" | "ErrImagePull" | "InvalidImageName" => {
                return Some(Problema {
                    rotulo: format!("{motivo} ({})", txt(c, "name")),
                    familia: "falha ao baixar a imagem",
                    conserto: "nome errado, tag que não existe, ou registro que exige credencial — confira a imagem e o imagePullSecret",
                    tom: Tone::Ruim,
                    peso: 2,
                    detalhe: vec![mensagem],
                });
            }
            "CreateContainerConfigError" | "CreateContainerError" => {
                return Some(Problema {
                    rotulo: format!("{motivo} ({})", txt(c, "name")),
                    familia: "erro de configuração do contêiner",
                    conserto: "quase sempre é um ConfigMap ou Secret citado que não existe — a mensagem diz qual",
                    tom: Tone::Ruim,
                    peso: 2,
                    detalhe: vec![mensagem],
                });
            }
            _ => {}
        }
    }

    // Morto por falta de memória na encarnação anterior: o pod pode estar «Running»
    // agora e ainda assim ser o problema mais caro do cluster.
    for c in &estados {
        if txt(c, "lastState.terminated.reason") == "OOMKilled" {
            return Some(Problema {
                rotulo: format!("OOMKilled antes ({})", txt(c, "name")),
                familia: "morto por falta de memória",
                conserto: "o limite de memória é menor que o que o processo precisa: meça o pico real antes de aumentar",
                tom: Tone::Ruim,
                peso: 1,
                detalhe: vec![saida_anterior(c)],
            });
        }
    }

    match fase {
        "Pending" => {
            return Some(Problema {
                rotulo: "Pending".to_string(),
                familia: "Pending",
                conserto: "ninguém o agendou: falta recurso, o nó tem taint sem toleration, ou o volume não veio — o evento FailedScheduling diz qual",
                tom: Tone::Ruim,
                peso: 3,
                detalhe: vec![one_line(txt(pod, "status.message"), 300)],
            });
        }
        "Failed" => {
            let motivo = txt(pod, "status.reason");
            return Some(Problema {
                rotulo: match motivo.is_empty() {
                    true => "Failed".to_string(),
                    false => format!("Failed ({motivo})"),
                },
                familia: match motivo == "Evicted" {
                    true => "despejado",
                    false => "terminado em falha",
                },
                conserto: "um pod despejado é o nó dizendo que faltou recurso; um Failed comum é o processo tendo saído com erro",
                tom: Tone::Aviso,
                peso: 5,
                detalhe: vec![one_line(txt(pod, "status.message"), 300)],
            });
        }
        _ => {}
    }

    // Preso em Terminating: tem carimbo de remoção e continua na lista.
    if !txt(pod, "metadata.deletionTimestamp").is_empty() {
        let quanto = agora() - epoch(txt(pod, "metadata.deletionTimestamp"));
        if quanto > PRESO {
            return Some(Problema {
                rotulo: format!("Terminating há {}", duration(quanto)),
                familia: "preso em Terminating",
                conserto: "finalizer que não completa, ou kubelet que não responde — veja metadata.finalizers e o nó do pod",
                tom: Tone::Aviso,
                peso: 4,
                detalhe: vec![format!(
                    "finalizers: {}",
                    lista_texto(pod, "/metadata/finalizers").join(", ")
                )],
            });
        }
    }

    let reinicios = reinicios_do_pod(pod);
    if reinicios > REINICIOS_DEMAIS {
        return Some(Problema {
            rotulo: format!("{reinicios} reinícios"),
            familia: "reiniciando demais",
            conserto: "não está caindo agora, mas já caiu várias vezes: procure o motivo antes que vire CrashLoop",
            tom: Tone::Aviso,
            peso: 6,
            detalhe: estados.iter().map(|c| saida_anterior(c)).collect(),
        });
    }

    // Rodando mas não pronto: a sonda de readiness está reprovando, e o serviço não
    // manda tráfego para ele. É o tipo de problema que não aparece em nenhuma contagem
    // de fase.
    if fase == "Running" && !estados.is_empty() {
        let nao_prontos: Vec<String> = estados
            .iter()
            .filter(|c| c.get("ready").and_then(Value::as_bool) != Some(true))
            .map(|c| txt(c, "name").to_string())
            .collect();
        if !nao_prontos.is_empty() && idade(pod) > 120.0 {
            return Some(Problema {
                rotulo: format!("não pronto ({})", nao_prontos.join(", ")),
                familia: "rodando sem ficar pronto",
                conserto: "a sonda de readiness está reprovando: o pod está de pé e fora do balanceamento",
                tom: Tone::Aviso,
                peso: 4,
                detalhe: Vec::new(),
            });
        }
    }
    None
}

/// Um motivo e um objeto, com quantas vezes aconteceu, a mensagem e o último carimbo.
type Grupo = ((String, String), (u64, String, String));

/// Os avisos que o cluster registrou na janela que ele guarda (uma hora, por padrão).
fn eventos(report: &mut Report, l: &Leitura) {
    report.section("Eventos");
    let eventos = l.itens("eventos");
    if eventos.is_empty() {
        match l.falhou("eventos") {
            Some(motivo) => report.line(format!(
                "não deu para ler os eventos: {}",
                one_line(motivo, 80)
            )),
            None => report.ok("nenhum evento na janela que o cluster guarda"),
        }
        return;
    }
    let avisos: Vec<&Value> = eventos
        .iter()
        .filter(|e| txt(e, "type") == "Warning")
        .collect();
    report.field("na janela", count(eventos.len() as f64));
    report.field("avisos", count(avisos.len() as f64));
    if avisos.is_empty() {
        report.ok("nenhum aviso na janela — só eventos normais");
        return;
    }

    // Agrupados por motivo e objeto: o mesmo FailedScheduling repetido trinta vezes é uma
    // linha com trinta, não trinta linhas.
    let mut grupos: HashMap<(String, String), (u64, String, String)> = HashMap::new();
    for e in &avisos {
        let motivo = txt(e, "reason").to_string();
        let alvo = format!(
            "{} {}/{}",
            txt(e, "involvedObject.kind"),
            txt(e, "involvedObject.namespace"),
            txt(e, "involvedObject.name")
        );
        let quantas = e.get("count").and_then(Value::as_u64).unwrap_or(1);
        let quando = ultimo_carimbo(e);
        let entrada = grupos
            .entry((motivo, alvo))
            .or_insert((0, String::new(), String::new()));
        entrada.0 += quantas;
        if entrada.1.is_empty() {
            entrada.1 = one_line(txt(e, "message"), 400);
        }
        if quando > entrada.2 {
            entrada.2 = quando;
        }
    }
    let mut linhas: Vec<Grupo> = grupos.into_iter().collect();
    // O que mais repetiu primeiro: num cluster com problema, a janela de eventos é uma
    // coisa só dita cinquenta vezes.
    linhas.sort_by_key(|(_, (vezes, _, _))| std::cmp::Reverse(*vezes));

    report.table(&["Motivo", "Objeto", "Vezes", "Último", "Mensagem"]);
    for ((motivo, alvo), (vezes, mensagem, quando)) in linhas.iter().take(40) {
        report.cells_deep(
            vec![
                motivo.clone(),
                alvo.clone(),
                count(*vezes as f64),
                curto(quando),
                one_line(mensagem, 70),
            ],
            Tone::Aviso,
            vec![
                ("motivo", motivo.clone()),
                ("objeto", alvo.clone()),
                ("vezes", count(*vezes as f64)),
                ("último", quando.clone()),
            ],
            super::quebrar(mensagem, 100),
        );
        report.destacar();
    }

    // Os motivos que valem um achado por si, porque cada um aponta para uma causa
    // diferente e conhecida.
    let mut por_motivo: BTreeMap<&str, u64> = BTreeMap::new();
    for e in &avisos {
        *por_motivo.entry(txt(e, "reason")).or_default() +=
            e.get("count").and_then(Value::as_u64).unwrap_or(1);
    }
    for (motivo, conserto) in [
        (
            "FailedScheduling",
            "não há nó que caiba o pod: recurso, taint, afinidade ou volume — a mensagem diz qual dos quatro",
        ),
        (
            "FailedMount",
            "o volume não montou: PVC pendente, segredo ausente, ou NFS fora do ar",
        ),
        (
            "Unhealthy",
            "a sonda está reprovando: ou o app está doente, ou a sonda é mais rígida do que o app consegue ser",
        ),
        (
            "FailedCreatePodSandBox",
            "o runtime não criou o sandbox: quase sempre é a CNI do nó",
        ),
        ("NodeNotReady", "um nó saiu do ar e levou os pods dele"),
        (
            "Evicted",
            "o kubelet despejou pods por falta de recurso no nó",
        ),
        (
            "BackOff",
            "contêiner reiniciando em intervalos crescentes — ver o cartão de Pods",
        ),
        (
            "FailedAttachVolume",
            "o disco não desgrudou do nó anterior: comum em troca de nó com volume RWO",
        ),
    ] {
        if let Some(vezes) = por_motivo.get(motivo) {
            report.aviso(format!("{motivo} × {vezes} na janela"), conserto);
        }
    }
}

/// Quanto os nós e os pods estão realmente consumindo — se houver de onde saber.
fn consumo(report: &mut Report, l: &Leitura) {
    report.section("Consumo");
    let nos = l.texto("consumo_nos");
    if nos.trim().is_empty() {
        report.aviso(
            "sem metrics-server: o cluster não sabe dizer o consumo de ninguém".to_string(),
            "sem ele não há kubectl top, não há HPA por CPU e não há como comparar o pedido com o usado — instale o metrics-server",
        );
        if let Some(motivo) = l.falhou("consumo_nos") {
            report.line(one_line(motivo, 100));
        }
        return;
    }
    report.table(&["Nó", "CPU", "CPU%", "Memória", "Memória%"]);
    for linha in nos.lines() {
        let campos: Vec<&str> = linha.split_whitespace().collect();
        if campos.len() < 5 {
            continue;
        }
        let cpu_pct = campos[2]
            .trim_end_matches('%')
            .parse::<f64>()
            .unwrap_or(0.0);
        let mem_pct = campos[4]
            .trim_end_matches('%')
            .parse::<f64>()
            .unwrap_or(0.0);
        let tom = match cpu_pct.max(mem_pct) {
            p if p >= 90.0 => Tone::Ruim,
            p if p >= 75.0 => Tone::Aviso,
            _ => Tone::Normal,
        };
        report.cells(
            vec![
                campos[0].to_string(),
                campos[1].to_string(),
                campos[2].to_string(),
                campos[3].to_string(),
                campos[4].to_string(),
            ],
            tom,
        );
        report.destacar();
        if cpu_pct >= 90.0 {
            report.grave(
                format!("nó {} com CPU em {cpu_pct:.0}%", campos[0]),
                "CPU no teto é throttling em tudo que roda ali — veja quem está consumindo no cartão e redistribua",
            );
        }
        if mem_pct >= 90.0 {
            report.grave(
                format!("nó {} com memória em {mem_pct:.0}%", campos[0]),
                "memória no teto termina em despejo e OOM; o kubelet começa a matar antes de chegar a 100%",
            );
        }
    }

    let pods = l.texto("consumo_pods");
    if pods.trim().is_empty() {
        return;
    }
    // Os dez maiores. A lista inteira num cluster grande é ruído; o que se procura aqui é
    // quem está comendo o nó.
    let mut linhas: Vec<(f64, Vec<String>)> = pods
        .lines()
        .filter_map(|linha| {
            let campos: Vec<&str> = linha.split_whitespace().collect();
            if campos.len() < 4 {
                return None;
            }
            let cpu = cpu_de(campos[2]);
            Some((
                cpu,
                vec![
                    campos[0].to_string(),
                    campos[1].to_string(),
                    campos[2].to_string(),
                    campos[3].to_string(),
                ],
            ))
        })
        .collect();
    linhas.sort_by(|a, b| b.0.total_cmp(&a.0));
    report.line("maiores consumidores de CPU agora:");
    for (_, campos) in linhas.iter().take(10) {
        report.row(format!(
            "{:<20} {:<40} {:>8} {:>10}",
            campos[0], campos[1], campos[2], campos[3]
        ));
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Ajudantes de leitura. Nada aqui conclui nada — só lê JSON sem quebrar quando falta.
// ─────────────────────────────────────────────────────────────────────────────────────

/// Um campo por caminho pontuado: `status.nodeInfo.kubeletVersion`.
///
/// Devolve `""` quando qualquer degrau do caminho falta, que é o caso normal num JSON de
/// API onde metade dos campos é opcional. Um `Option` aqui só encheria de `unwrap_or` as
/// trezentas chamadas deste arquivo.
fn txt<'a>(valor: &'a Value, caminho: &str) -> &'a str {
    let mut atual = valor;
    for degrau in caminho.split('.') {
        match atual.get(degrau) {
            Some(proximo) => atual = proximo,
            None => return "",
        }
    }
    atual.as_str().unwrap_or("")
}

fn lista_texto(valor: &Value, ponteiro: &str) -> Vec<String> {
    valor
        .pointer(ponteiro)
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .map(|x| x.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn agora() -> f64 {
    crate::db::agora() as f64
}

/// Um carimbo da API em segundos desde a época. A API do Kubernetes só escreve RFC 3339
/// com `Z`, que é o formato que `epoch_de_iso` já lê — e uma segunda implementação de
/// leitura de data é uma que diverge da primeira.
fn epoch(carimbo: &str) -> f64 {
    crate::invest::tempo::epoch_de_iso(carimbo).unwrap_or(0) as f64
}

/// Há quantos segundos o objeto existe.
fn idade(objeto: &Value) -> f64 {
    let criado = epoch(txt(objeto, "metadata.creationTimestamp"));
    match criado > 0.0 {
        true => (agora() - criado).max(0.0),
        false => 0.0,
    }
}

/// `1500m`, `2`, `100m` em milicores.
fn cpu_de(texto: &str) -> f64 {
    let texto = texto.trim();
    if texto.is_empty() {
        return 0.0;
    }
    if let Some(milis) = texto.strip_suffix('m') {
        return milis.parse::<f64>().unwrap_or(0.0);
    }
    // Nanocores aparecem no `kubectl top`.
    if let Some(nanos) = texto.strip_suffix('n') {
        return nanos.parse::<f64>().unwrap_or(0.0) / 1_000_000.0;
    }
    if let Some(micros) = texto.strip_suffix('u') {
        return micros.parse::<f64>().unwrap_or(0.0) / 1_000.0;
    }
    texto.parse::<f64>().unwrap_or(0.0) * 1000.0
}

/// `128Mi`, `1Gi`, `1000000`, `2G` em bytes.
fn mem_de(texto: &str) -> f64 {
    let texto = texto.trim();
    if texto.is_empty() {
        return 0.0;
    }
    for (sufixo, fator) in [
        ("Ki", 1024.0),
        ("Mi", 1024.0 * 1024.0),
        ("Gi", 1024.0 * 1024.0 * 1024.0),
        ("Ti", 1024.0 * 1024.0 * 1024.0 * 1024.0),
        ("Pi", 1024f64.powi(5)),
        ("k", 1000.0),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
    ] {
        if let Some(numero) = texto.strip_suffix(sufixo) {
            return numero.parse::<f64>().unwrap_or(0.0) * fator;
        }
    }
    texto.parse::<f64>().unwrap_or(0.0)
}

/// Os contêineres de um pod — os normais e os de inicialização, que também pedem recurso.
fn conteineres(pod: &Value) -> Vec<&Value> {
    let mut v: Vec<&Value> = pod
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .map(|c| c.iter().collect())
        .unwrap_or_default();
    if let Some(iniciais) = pod
        .pointer("/spec/initContainers")
        .and_then(Value::as_array)
    {
        v.extend(iniciais.iter());
    }
    v
}

/// CPU e memória pedidas por um pod: a soma dos contêineres normais.
///
/// Os de inicialização ficam de fora da soma de propósito — o escalonador usa o **maior**
/// entre eles e a soma dos normais, e eles já terminaram quando o pod está de pé.
fn pedido_do_pod(pod: &Value) -> (f64, f64) {
    soma_recursos(pod, "requests")
}

fn limite_do_pod(pod: &Value) -> (f64, f64) {
    soma_recursos(pod, "limits")
}

fn soma_recursos(pod: &Value, qual: &str) -> (f64, f64) {
    let (mut cpu, mut mem) = (0.0, 0.0);
    let normais = pod
        .pointer("/spec/containers")
        .and_then(Value::as_array)
        .map(|c| c.iter().collect::<Vec<_>>())
        .unwrap_or_default();
    for c in normais {
        let recursos = c.pointer(&format!("/resources/{qual}"));
        if let Some(r) = recursos {
            cpu += cpu_de(txt(r, "cpu"));
            mem += mem_de(txt(r, "memory"));
        }
    }
    (cpu, mem)
}

fn reinicios_do_pod(pod: &Value) -> u64 {
    pod.pointer("/status/containerStatuses")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .map(|c| c.get("restartCount").and_then(Value::as_u64).unwrap_or(0))
                .sum()
        })
        .unwrap_or(0)
}

fn imagens_do_pod(pod: &Value) -> Vec<String> {
    conteineres(pod)
        .iter()
        .map(|c| txt(c, "image").to_string())
        .collect()
}

/// Quem criou o objeto, quando alguém criou.
fn dono(objeto: &Value) -> String {
    objeto
        .pointer("/metadata/ownerReferences")
        .and_then(Value::as_array)
        .and_then(|donos| donos.first())
        .map(|d| format!("{} {}", txt(d, "kind"), txt(d, "name")))
        .unwrap_or_else(|| "nenhum — pod avulso".to_string())
}

fn no_pronto(no: &Value) -> bool {
    condicoes_verdadeiras(no).iter().any(|c| c == "Ready")
}

/// As condições do objeto que estão em `True`.
fn condicoes_verdadeiras(objeto: &Value) -> Vec<String> {
    objeto
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .filter(|c| txt(c, "status") == "True")
                .map(|c| txt(c, "type").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn papel_do_no(no: &Value) -> String {
    let vazio = serde_json::Map::new();
    let rotulos = no
        .pointer("/metadata/labels")
        .and_then(Value::as_object)
        .unwrap_or(&vazio);
    let papeis: Vec<String> = rotulos
        .keys()
        .filter_map(|k| k.strip_prefix("node-role.kubernetes.io/"))
        .map(str::to_string)
        .collect();
    match papeis.is_empty() {
        true => "worker".to_string(),
        false => papeis.join(","),
    }
}

fn enderecos_do_no(no: &Value) -> String {
    no.pointer("/status/addresses")
        .and_then(Value::as_array)
        .map(|v| {
            v.iter()
                .map(|a| format!("{}={}", txt(a, "type"), txt(a, "address")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Como terminou a encarnação anterior de um contêiner.
fn saida_anterior(c: &Value) -> String {
    let t = c.pointer("/lastState/terminated");
    let Some(t) = t else {
        return String::new();
    };
    format!(
        "saída anterior: código {} · {} · {}",
        t.get("exitCode").and_then(Value::as_i64).unwrap_or(0),
        txt(t, "reason"),
        txt(t, "finishedAt")
    )
}

fn ultimo_carimbo(evento: &Value) -> String {
    for campo in ["lastTimestamp", "eventTime", "firstTimestamp"] {
        let valor = txt(evento, campo);
        if !valor.is_empty() {
            return valor.to_string();
        }
    }
    txt(evento, "metadata.creationTimestamp").to_string()
}

/// Um carimbo como «há 4 min», que é o que se quer numa coluna estreita.
fn curto(carimbo: &str) -> String {
    let em = epoch(carimbo);
    match em > 0.0 {
        true => format!("há {}", duration((agora() - em).max(0.0))),
        false => carimbo.to_string(),
    }
}

/// `1200m de 4000m (30%)`, ou só o valor quando não há total contra o que medir.
fn pct_de(parte: f64, total: f64, formato: &dyn Fn(f64) -> String) -> String {
    match total > 0.0 {
        true => format!("{} ({:.0}%)", formato(parte), parte / total * 100.0),
        false => formato(parte),
    }
}

/// O `minor` de uma das versões do `kubectl version -o json`.
fn minor(versao: &Value, qual: &str) -> Option<u64> {
    let bruto = txt(versao, &format!("{qual}.minor"));
    // Vem com `+` nos clusters gerenciados: «27+».
    let limpo: String = bruto.chars().take_while(char::is_ascii_digit).collect();
    limpo.parse().ok()
}

/// Uma linha de «era X, agora é Y», só quando mudou.
fn delta(report: &mut Report, rotulo: &str, antes: f64, agora: f64) {
    match (agora - antes).abs() < f64::EPSILON {
        true => report.field(rotulo, count(agora)),
        false => report.field(rotulo, format!("{} (era {})", count(agora), count(antes))),
    }
}

/// O rótulo de um objeto, ou `""`.
fn rotulo<'a>(objeto: &'a Value, chave: &str) -> &'a str {
    objeto
        .pointer("/metadata/labels")
        .and_then(|l| l.get(chave))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// O `spec` do pod que um controlador cria — é ali que mora tudo que importa para
/// segurança e resiliência, e não no objeto de fora.
fn molde(objeto: &Value) -> Option<&Value> {
    objeto.pointer("/spec/template/spec")
}

/// `ns/nome`, o jeito como todo mundo escreve um objeto de namespace.
fn referencia(objeto: &Value) -> String {
    let ns = txt(objeto, "metadata.namespace");
    let nome = txt(objeto, "metadata.name");
    match ns.is_empty() {
        true => nome.to_string(),
        false => format!("{ns}/{nome}"),
    }
}

/// Os namespaces do próprio Kubernetes e dos complementos que todo cluster tem.
///
/// Não é uma lista de coisas a ignorar: é uma lista de coisas a **não cobrar do mesmo
/// jeito**. Um DaemonSet de CNI roda com hostNetwork porque é a rede; cobrar dele o mesmo
/// que de um aplicativo encheria a tela de achados que ninguém vai consertar, e um
/// relatório que se ignora é um relatório inútil.
fn e_do_sistema(ns: &str) -> bool {
    matches!(
        ns,
        "kube-system"
            | "kube-public"
            | "kube-node-lease"
            | "local-path-storage"
            | "gatekeeper-system"
            | "cert-manager"
            | "ingress-nginx"
            | "metallb-system"
            | "monitoring"
            | "istio-system"
            | "linkerd"
            | "calico-system"
            | "tigera-operator"
            | "gmp-system"
            | "kube-flannel"
    )
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Os controladores: quem deveria manter os pods de pé.
// ─────────────────────────────────────────────────────────────────────────────────────

fn cargas(report: &mut Report, l: &Leitura) {
    report.section("Cargas");
    let deploys = l.itens("deployments");
    let stateful = l.itens("statefulsets");
    let daemons = l.itens("daemonsets");
    let jobs = l.itens("jobs");
    let cronjobs = l.itens("cronjobs");
    report.field(
        "objetos",
        format!(
            "{} deployments · {} statefulsets · {} daemonsets · {} jobs · {} cronjobs",
            deploys.len(),
            stateful.len(),
            daemons.len(),
            jobs.len(),
            cronjobs.len()
        ),
    );

    report.table(&["Tipo", "Objeto", "Prontos", "Estado", "Idade"]);
    let mut incompletos = 0usize;

    for d in deploys {
        let desejado = numero(d, "/spec/replicas").unwrap_or(1.0);
        let prontos = numero(d, "/status/readyReplicas").unwrap_or(0.0);
        let atualizados = numero(d, "/status/updatedReplicas").unwrap_or(0.0);
        let pausado = d.pointer("/spec/paused").and_then(Value::as_bool) == Some(true);
        let motivo = condicao(d, "Available")
            .filter(|c| txt(c, "status") != "True")
            .map(|c| format!("{}: {}", txt(c, "reason"), one_line(txt(c, "message"), 120)));
        let estado = match (&motivo, pausado, prontos >= desejado) {
            (_, true, _) => "pausado".to_string(),
            (Some(m), _, _) => one_line(m, 60),
            (None, _, false) => "subindo".to_string(),
            (None, _, true) => "ok".to_string(),
        };
        let tom = match (prontos >= desejado, prontos == 0.0 && desejado > 0.0) {
            (_, true) => Tone::Ruim,
            (false, _) => Tone::Aviso,
            _ => Tone::Normal,
        };
        if prontos < desejado || pausado {
            incompletos += 1;
            report.cells_deep(
                vec![
                    "Deployment".to_string(),
                    referencia(d),
                    format!("{prontos:.0}/{desejado:.0}"),
                    estado,
                    duration(idade(d)),
                ],
                tom,
                vec![
                    ("atualizados", format!("{atualizados:.0}")),
                    ("estratégia", txt(d, "spec.strategy.type").to_string()),
                    ("imagens", imagens_do_molde(d).join(" · ")),
                    ("condições", condicoes_resumidas(d)),
                    ("criado", txt(d, "metadata.creationTimestamp").to_string()),
                ],
                Vec::new(),
            );
            report.destacar();
        }
        if prontos == 0.0 && desejado > 0.0 {
            report.grave(
                format!("Deployment {} sem nenhuma réplica pronta", referencia(d)),
                "está fora do ar: veja os pods dele no cartão de Pods e o evento que explica por quê",
            );
        } else if prontos < desejado && idade(d) > 900.0 {
            report.aviso(
                format!(
                    "Deployment {} em {prontos:.0} de {desejado:.0} réplicas",
                    referencia(d)
                ),
                "um rollout que não termina em quinze minutos não está terminando sozinho",
            );
        }
        if pausado {
            report.aviso(
                format!("Deployment {} está pausado", referencia(d)),
                "rollout pausado não aplica mudança nenhuma — se não é uma pausa proposital, retome",
            );
        }
    }

    for s in stateful {
        let desejado = numero(s, "/spec/replicas").unwrap_or(1.0);
        let prontos = numero(s, "/status/readyReplicas").unwrap_or(0.0);
        if prontos < desejado {
            incompletos += 1;
            report.cells(
                vec![
                    "StatefulSet".to_string(),
                    referencia(s),
                    format!("{prontos:.0}/{desejado:.0}"),
                    "incompleto".to_string(),
                    duration(idade(s)),
                ],
                match prontos == 0.0 {
                    true => Tone::Ruim,
                    false => Tone::Aviso,
                },
            );
            report.destacar();
            report.aviso(
                format!(
                    "StatefulSet {} em {prontos:.0} de {desejado:.0}",
                    referencia(s)
                ),
                "StatefulSet sobe em ordem: o pod travado segura todos os seguintes, então comece pelo de menor índice",
            );
        }
    }

    for d in daemons {
        let desejado = numero(d, "/status/desiredNumberScheduled").unwrap_or(0.0);
        let prontos = numero(d, "/status/numberReady").unwrap_or(0.0);
        let indisponiveis = numero(d, "/status/numberUnavailable").unwrap_or(0.0);
        if prontos < desejado {
            incompletos += 1;
            report.cells(
                vec![
                    "DaemonSet".to_string(),
                    referencia(d),
                    format!("{prontos:.0}/{desejado:.0}"),
                    format!("{indisponiveis:.0} indisponível(is)"),
                    duration(idade(d)),
                ],
                Tone::Aviso,
            );
            report.destacar();
            report.aviso(
                format!(
                    "DaemonSet {} não está em todos os nós ({prontos:.0}/{desejado:.0})",
                    referencia(d)
                ),
                "um DaemonSet incompleto costuma ser um nó doente — e se for de rede ou de log, aquele nó está cego",
            );
        }
    }

    for j in jobs {
        let falhas = numero(j, "/status/failed").unwrap_or(0.0);
        let ativos = numero(j, "/status/active").unwrap_or(0.0);
        if falhas > 0.0 {
            report.cells(
                vec![
                    "Job".to_string(),
                    referencia(j),
                    format!("{falhas:.0} falha(s)"),
                    condicoes_resumidas(j),
                    duration(idade(j)),
                ],
                Tone::Aviso,
            );
            report.destacar();
            report.aviso(
                format!("Job {} falhou {falhas:.0} vez(es)", referencia(j)),
                "um Job que falha fica na lista para ser visto: leia o pod dele antes de apagar",
            );
        } else if ativos > 0.0 && idade(j) > 86_400.0 {
            report.aviso(
                format!("Job {} ativo há {}", referencia(j), duration(idade(j))),
                "um Job de mais de um dia ativo costuma ser um processo travado, não um processo longo",
            );
        }
    }

    for c in cronjobs {
        if c.pointer("/spec/suspend").and_then(Value::as_bool) == Some(true) {
            report.aviso(
                format!("CronJob {} está suspenso", referencia(c)),
                "suspenso não roda: confirme se foi de propósito, porque nada avisa quando deixa de rodar",
            );
        }
        let ultimo = epoch(txt(c, "status.lastScheduleTime"));
        if ultimo > 0.0 && agora() - ultimo > 7.0 * 86_400.0 {
            report.aviso(
                format!(
                    "CronJob {} não roda há {}",
                    referencia(c),
                    duration(agora() - ultimo)
                ),
                "o agendamento pode estar errado, ou o controlador perdeu a janela — confira o schedule",
            );
        }
    }

    if incompletos == 0 {
        report.ok("todos os controladores com as réplicas que pediram");
    }
}

fn imagens_do_molde(objeto: &Value) -> Vec<String> {
    molde(objeto)
        .and_then(|m| m.get("containers"))
        .and_then(Value::as_array)
        .map(|cs| cs.iter().map(|c| txt(c, "image").to_string()).collect())
        .unwrap_or_default()
}

fn condicao<'a>(objeto: &'a Value, tipo: &str) -> Option<&'a Value> {
    objeto
        .pointer("/status/conditions")
        .and_then(Value::as_array)?
        .iter()
        .find(|c| txt(c, "type") == tipo)
}

fn condicoes_resumidas(objeto: &Value) -> String {
    objeto
        .pointer("/status/conditions")
        .and_then(Value::as_array)
        .map(|cs| {
            cs.iter()
                .map(|c| format!("{}={}", txt(c, "type"), txt(c, "status")))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn numero(objeto: &Value, ponteiro: &str) -> Option<f64> {
    objeto.pointer(ponteiro).and_then(Value::as_f64)
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Recursos: o que cada carga pede, e o que ninguém pediu.
// ─────────────────────────────────────────────────────────────────────────────────────

fn recursos(report: &mut Report, l: &Leitura) {
    report.section("Recursos");
    let pods = l.itens("pods");
    if pods.is_empty() {
        report.line("sem pods visíveis");
        return;
    }
    let mut qos: BTreeMap<&str, usize> = BTreeMap::new();
    for pod in pods {
        *qos.entry(txt(pod, "status.qosClass")).or_default() += 1;
    }
    report.field(
        "classes de serviço",
        qos.iter()
            .map(|(q, n)| format!("{q} {n}"))
            .collect::<Vec<_>>()
            .join(" · "),
    );

    // Por carga, e não por pod: vinte pods do mesmo Deployment sem limite são uma linha
    // para corrigir num lugar só.
    let mut faltas: Vec<(String, Vec<&'static str>, String)> = Vec::new();
    for objeto in l
        .itens("deployments")
        .iter()
        .chain(l.itens("statefulsets"))
        .chain(l.itens("daemonsets"))
    {
        let ns = txt(objeto, "metadata.namespace");
        let Some(spec) = molde(objeto) else { continue };
        let vazio = Vec::new();
        let conteineres = spec
            .get("containers")
            .and_then(Value::as_array)
            .unwrap_or(&vazio);
        for c in conteineres {
            let mut faltando = Vec::new();
            if txt(c, "resources.requests.cpu").is_empty() {
                faltando.push("request de CPU");
            }
            if txt(c, "resources.requests.memory").is_empty() {
                faltando.push("request de memória");
            }
            if txt(c, "resources.limits.memory").is_empty() {
                faltando.push("limite de memória");
            }
            if !faltando.is_empty() {
                faltas.push((
                    format!("{}/{}", referencia(objeto), txt(c, "name")),
                    faltando,
                    ns.to_string(),
                ));
            }
        }
    }

    if !faltas.is_empty() {
        report.table(&["Contêiner", "O que falta", "Namespace"]);
        for (quem, o_que, ns) in faltas.iter().take(60) {
            report.cells(
                vec![quem.clone(), o_que.join(", "), ns.clone()],
                match e_do_sistema(ns) {
                    true => Tone::Dim,
                    false => Tone::Aviso,
                },
            );
            report.destacar();
        }
        let fora_do_sistema = faltas.iter().filter(|(_, _, ns)| !e_do_sistema(ns)).count();
        if fora_do_sistema > 0 {
            report.aviso(
                format!(
                    "{} sem request ou limite declarado",
                    plural(fora_do_sistema as f64, "contêiner", "contêineres")
                ),
                "sem request o escalonador não sabe o peso do pod; sem limite de memória um vazamento derruba o nó inteiro, não só o pod",
            );
        }
    } else {
        report.ok("toda carga declara request e limite de memória");
    }

    if let Some(besteffort) = qos.get("BestEffort").filter(|n| **n > 0) {
        report.aviso(
            format!("{besteffort} pod(s) em BestEffort"),
            "BestEffort é o primeiro a ser morto quando o nó aperta — nenhum serviço que responde a usuário deveria estar aí",
        );
    }

    // Cotas e limites padrão por namespace: o que impede um namespace de comer o cluster.
    let cotas: HashSet<String> = l
        .itens("cotas")
        .iter()
        .map(|c| txt(c, "metadata.namespace").to_string())
        .collect();
    let limites: HashSet<String> = l
        .itens("limites_padrao")
        .iter()
        .map(|c| txt(c, "metadata.namespace").to_string())
        .collect();
    let sem_cota: Vec<String> = l
        .itens("namespaces")
        .iter()
        .map(|n| txt(n, "metadata.name").to_string())
        .filter(|n| !e_do_sistema(n) && !cotas.contains(n))
        .collect();
    report.field(
        "namespaces com cota",
        format!("{} de {}", cotas.len(), l.itens("namespaces").len()),
    );
    report.field(
        "namespaces com LimitRange",
        format!("{} de {}", limites.len(), l.itens("namespaces").len()),
    );
    if !sem_cota.is_empty() {
        report.aviso(
            format!(
                "{} sem ResourceQuota: {}",
                plural(sem_cota.len() as f64, "namespace", "namespaces"),
                one_line(&sem_cota.join(", "), 80)
            ),
            "sem cota, um namespace pode pedir o cluster inteiro — e quem paga é o vizinho",
        );
    }

    // Uma cota quase cheia é um deploy que vai falhar sem ninguém entender por quê.
    for cota in l.itens("cotas") {
        let vazio = serde_json::Map::new();
        let duro = cota
            .pointer("/status/hard")
            .and_then(Value::as_object)
            .unwrap_or(&vazio);
        for (recurso, limite) in duro {
            let usado = cota
                .pointer(&format!("/status/used/{}", recurso.replace('/', "~1")))
                .and_then(Value::as_str)
                .unwrap_or("0");
            let (u, t) = match recurso.contains("cpu") {
                true => (cpu_de(usado), cpu_de(limite.as_str().unwrap_or("0"))),
                false => (mem_de(usado), mem_de(limite.as_str().unwrap_or("0"))),
            };
            if t > 0.0 && u / t > 0.9 {
                report.aviso(
                    format!(
                        "cota {} em {:.0}% de {recurso}",
                        referencia(cota),
                        u / t * 100.0
                    ),
                    "cota cheia recusa o próximo pod com um erro que não aparece no Deployment, só no ReplicaSet",
                );
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Resiliência: o que acontece quando um nó cai ou entra em manutenção.
// ─────────────────────────────────────────────────────────────────────────────────────

fn resiliencia(report: &mut Report, l: &Leitura) {
    report.section("Resiliência");
    let deploys = l.itens("deployments");
    let pdbs = l.itens("pdbs");
    report.field("PodDisruptionBudgets", count(pdbs.len() as f64));
    report.field("HPAs", count(l.itens("hpas").len() as f64));

    report.table(&["Objeto", "Réplicas", "O que falta"]);
    let mut frageis = 0usize;
    for d in deploys.iter().chain(l.itens("statefulsets")) {
        let ns = txt(d, "metadata.namespace");
        if e_do_sistema(ns) {
            continue;
        }
        let replicas = numero(d, "/spec/replicas").unwrap_or(1.0);
        let Some(spec) = molde(d) else { continue };
        let vazio = Vec::new();
        let conteineres = spec
            .get("containers")
            .and_then(Value::as_array)
            .unwrap_or(&vazio);

        let mut faltas: Vec<String> = Vec::new();
        if replicas < 2.0 {
            faltas.push("réplica única".to_string());
        }
        if conteineres
            .iter()
            .any(|c| c.get("readinessProbe").is_none())
        {
            faltas.push("sonda de readiness".to_string());
        }
        if conteineres.iter().any(|c| c.get("livenessProbe").is_none()) {
            faltas.push("sonda de liveness".to_string());
        }
        if replicas > 1.0
            && spec.get("topologySpreadConstraints").is_none()
            && spec.pointer("/affinity/podAntiAffinity").is_none()
        {
            faltas.push("espalhamento entre nós".to_string());
        }
        if replicas > 1.0 && !tem_pdb(d, pdbs) {
            faltas.push("PodDisruptionBudget".to_string());
        }
        if faltas.is_empty() {
            continue;
        }
        frageis += 1;
        report.cells_deep(
            vec![referencia(d), format!("{replicas:.0}"), faltas.join(", ")],
            match replicas < 2.0 {
                true => Tone::Aviso,
                false => Tone::Normal,
            },
            vec![
                ("tipo", txt(d, "kind").to_string()),
                ("imagens", imagens_do_molde(d).join(" · ")),
                ("estratégia", txt(d, "spec.strategy.type").to_string()),
                (
                    "classe de prioridade",
                    txt(spec, "priorityClassName").to_string(),
                ),
            ],
            Vec::new(),
        );
        report.destacar();
    }
    if frageis == 0 {
        report.ok("as cargas de aplicativo têm réplica, sonda e espalhamento");
    } else {
        report.aviso(
            format!(
                "{} sem tudo que uma manutenção exige",
                plural(frageis as f64, "carga", "cargas")
            ),
            "réplica única cai junto com o nó; sem readiness o tráfego entra antes da hora; sem PDB um drain derruba tudo de uma vez",
        );
    }

    // Um PDB que nunca permite despejo trava o `drain` para sempre — o oposto do que ele
    // deveria fazer.
    for pdb in pdbs {
        let max_indisponivel = txt(pdb, "spec.maxUnavailable");
        let min_disponivel = txt(pdb, "spec.minAvailable");
        let esperados = numero(pdb, "/status/expectedPods").unwrap_or(0.0);
        let permitidos = numero(pdb, "/status/disruptionsAllowed").unwrap_or(-1.0);
        if max_indisponivel == "0" || (min_disponivel == "100%" && esperados > 0.0) {
            report.grave(
                format!("PDB {} nunca permite despejo", referencia(pdb)),
                "com maxUnavailable=0 um kubectl drain fica preso para sempre — é a causa clássica de manutenção que não termina",
            );
        } else if permitidos == 0.0 && esperados > 0.0 {
            report.aviso(
                format!("PDB {} está com zero despejos permitidos agora", referencia(pdb)),
                "enquanto as réplicas não voltarem ao mínimo, nenhum nó com esses pods pode ser drenado",
            );
        }
    }

    for hpa in l.itens("hpas") {
        let atual = numero(hpa, "/status/currentReplicas").unwrap_or(0.0);
        let max = numero(hpa, "/spec/maxReplicas").unwrap_or(0.0);
        if max > 0.0 && atual >= max {
            report.aviso(
                format!("HPA {} está no teto ({max:.0} réplicas)", referencia(hpa)),
                "no máximo, o autoscaler não tem mais o que fazer: ou sobe o teto, ou a carga não cabe",
            );
        }
        if let Some(c) = condicao(hpa, "ScalingActive")
            && txt(c, "status") == "False"
        {
            report.aviso(
                format!(
                    "HPA {} não consegue escalar: {}",
                    referencia(hpa),
                    txt(c, "reason")
                ),
                "quase sempre falta metrics-server, ou o alvo não declara request do recurso medido",
            );
        }
    }
}

/// Se algum PDB cobre os pods deste controlador, pelo seletor.
fn tem_pdb(carga: &Value, pdbs: &[Value]) -> bool {
    let ns = txt(carga, "metadata.namespace");
    let vazio = serde_json::Map::new();
    let rotulos = carga
        .pointer("/spec/template/metadata/labels")
        .and_then(Value::as_object)
        .unwrap_or(&vazio);
    pdbs.iter()
        .filter(|p| txt(p, "metadata.namespace") == ns)
        .any(|p| {
            let seletor = p
                .pointer("/spec/selector/matchLabels")
                .and_then(Value::as_object);
            match seletor {
                // Um PDB sem matchLabels pega o namespace inteiro.
                None => true,
                Some(s) => s.iter().all(|(k, v)| rotulos.get(k) == Some(v)),
            }
        })
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Segurança das cargas: o que um contêiner comprometido alcançaria daqui.
// ─────────────────────────────────────────────────────────────────────────────────────

fn seguranca(report: &mut Report, l: &Leitura) {
    report.section("Segurança das cargas");
    let pods = l.itens("pods");
    if pods.is_empty() {
        report.line("sem pods visíveis");
        return;
    }

    let mut achados: Vec<(String, Vec<String>, bool)> = Vec::new();
    // Os agregados contam **só aplicativo**. O etcd monta a pasta de dados dele do nó, o
    // kube-proxy é privilegiado e a CNI usa hostNetwork: isso é o que essas peças são, e
    // um «28 volumes hostPath» que é quase todo control plane treina quem lê a ignorar a
    // tela. Os pods do sistema continuam na tabela, em cinza, para quem quiser ver.
    let mut contagem: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut do_sistema: BTreeMap<&'static str, usize> = BTreeMap::new();
    for pod in pods {
        let ns = txt(pod, "metadata.namespace");
        let sistema = e_do_sistema(ns);
        let contagem = match sistema {
            true => &mut do_sistema,
            false => &mut contagem,
        };
        let mut riscos: Vec<String> = Vec::new();
        let spec = pod.get("spec").unwrap_or(&Value::Null);

        if spec.get("hostNetwork").and_then(Value::as_bool) == Some(true) {
            riscos.push("hostNetwork".to_string());
            *contagem.entry("hostNetwork").or_default() += 1;
        }
        if spec.get("hostPID").and_then(Value::as_bool) == Some(true) {
            riscos.push("hostPID".to_string());
            *contagem.entry("hostPID").or_default() += 1;
        }
        if spec.get("hostIPC").and_then(Value::as_bool) == Some(true) {
            riscos.push("hostIPC".to_string());
            *contagem.entry("hostIPC").or_default() += 1;
        }
        // Um hostPath de leitura já é muito; a raiz do nó montada dentro de um contêiner
        // é o nó inteiro nas mãos de quem entrar nele.
        for v in spec
            .get("volumes")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if let Some(caminho) = v.pointer("/hostPath/path").and_then(Value::as_str) {
                riscos.push(format!("hostPath {caminho}"));
                *contagem.entry("hostPath").or_default() += 1;
            }
        }

        for c in conteineres(pod) {
            let sc = c.get("securityContext").unwrap_or(&Value::Null);
            let nome = txt(c, "name");
            if sc.get("privileged").and_then(Value::as_bool) == Some(true) {
                riscos.push(format!("{nome}: privileged"));
                *contagem.entry("privileged").or_default() += 1;
            }
            if sc.get("allowPrivilegeEscalation").and_then(Value::as_bool) == Some(true) {
                riscos.push(format!("{nome}: allowPrivilegeEscalation"));
                *contagem.entry("allowPrivilegeEscalation").or_default() += 1;
            }
            let como_root = sc.get("runAsNonRoot").and_then(Value::as_bool) != Some(true)
                && spec
                    .pointer("/securityContext/runAsNonRoot")
                    .and_then(Value::as_bool)
                    != Some(true)
                && sc.get("runAsUser").and_then(Value::as_i64).unwrap_or(0) == 0;
            if como_root {
                riscos.push(format!("{nome}: pode rodar como root"));
                *contagem.entry("root").or_default() += 1;
            }
            let perigosas = ["SYS_ADMIN", "NET_ADMIN", "SYS_PTRACE", "SYS_MODULE", "ALL"];
            for cap in lista_texto(sc, "/capabilities/add") {
                if perigosas.contains(&cap.as_str()) {
                    riscos.push(format!("{nome}: capability {cap}"));
                    *contagem.entry("capability").or_default() += 1;
                }
            }
            // `:latest` — ou nenhuma tag, que é `:latest` com outro nome. O pod que
            // reiniciar amanhã pode não ser o mesmo software de hoje.
            let imagem = txt(c, "image");
            let sem_versao = imagem.ends_with(":latest")
                || (!imagem.contains('@')
                    && !imagem.rsplit('/').next().unwrap_or("").contains(':'));
            if sem_versao && !sistema {
                riscos.push(format!("{nome}: imagem sem versão fixa ({imagem})"));
                *contagem.entry("imagem sem versão").or_default() += 1;
            }
            for porta in c
                .get("ports")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                if let Some(p) = porta.get("hostPort").and_then(Value::as_i64) {
                    riscos.push(format!("{nome}: hostPort {p}"));
                    *contagem.entry("hostPort").or_default() += 1;
                }
            }
        }

        // O token da conta de serviço montado num pod que não fala com a API é uma
        // credencial de graça para quem invadir o contêiner.
        if spec
            .get("automountServiceAccountToken")
            .and_then(Value::as_bool)
            != Some(false)
            && txt(pod, "spec.serviceAccountName") == "default"
            && !sistema
        {
            riscos.push("token da conta default montado".to_string());
            *contagem.entry("token default").or_default() += 1;
        }

        if !riscos.is_empty() {
            achados.push((referencia(pod), riscos, sistema));
        }
    }

    if achados.is_empty() {
        report.ok("nenhum pod com privilégio, acesso ao host ou imagem sem versão");
    } else {
        report.table(&["Pod", "O que encontrei", "Onde"]);
        for (quem, riscos, sistema) in achados.iter().take(60) {
            report.cells_deep(
                vec![
                    quem.clone(),
                    one_line(&riscos.join(" · "), 90),
                    match sistema {
                        true => "sistema".to_string(),
                        false => "aplicativo".to_string(),
                    },
                ],
                match sistema {
                    true => Tone::Dim,
                    false => Tone::Aviso,
                },
                vec![("pod", quem.clone())],
                riscos.clone(),
            );
            report.destacar();
        }
    }

    if !do_sistema.is_empty() {
        report.line(format!(
            "no sistema (kube-system e afins), por desenho: {}",
            do_sistema
                .iter()
                .map(|(risco, n)| format!("{n} {risco}"))
                .collect::<Vec<_>>()
                .join(" · ")
        ));
    }

    for (risco, quantos) in &contagem {
        let quantos = *quantos as f64;
        let (frase, conserto, grave) = match *risco {
            "privileged" => (
                format!(
                    "{} com privileged",
                    plural(quantos, "contêiner", "contêineres")
                ),
                "privileged é root no nó com quase tudo liberado: quem entra no contêiner sai no host",
                true,
            ),
            "hostPath" => (
                format!("{} hostPath", plural(quantos, "volume", "volumes")),
                "um hostPath escreve no disco do nó e escapa de qualquer isolamento — confira se o caminho justifica o risco",
                true,
            ),
            "hostPID" | "hostIPC" => (
                format!("{} com {risco}", plural(quantos, "pod", "pods")),
                "com o namespace do host, o contêiner enxerga (e sinaliza) os processos da máquina",
                true,
            ),
            "hostNetwork" => (
                format!("{} com hostNetwork", plural(quantos, "pod", "pods")),
                "sem namespace de rede, o pod abre portas direto no nó e ignora NetworkPolicy",
                false,
            ),
            "allowPrivilegeEscalation" => (
                format!(
                    "{} com allowPrivilegeEscalation ligado",
                    plural(quantos, "contêiner", "contêineres")
                ),
                "allowPrivilegeEscalation: false é uma linha no manifesto e fecha o caminho mais curto para root",
                false,
            ),
            "root" => (
                format!(
                    "{} sem runAsNonRoot — podem subir como root",
                    plural(quantos, "contêiner", "contêineres")
                ),
                "runAsNonRoot: true e um runAsUser fixo — a maioria das imagens roda igual sem root",
                false,
            ),
            "capability" => (
                format!(
                    "{} com capability perigosa",
                    plural(quantos, "contêiner", "contêineres")
                ),
                "SYS_ADMIN é praticamente privileged; NET_ADMIN mexe na rede do nó. Só o que a aplicação usa de fato",
                false,
            ),
            "imagem sem versão" => (
                format!(
                    "{} com imagem sem versão fixa",
                    plural(quantos, "contêiner", "contêineres")
                ),
                "latest não é uma versão: o pod que reiniciar amanhã pode rodar outro software, e o rollback não existe",
                false,
            ),
            "hostPort" => (
                format!(
                    "{} com hostPort",
                    plural(quantos, "contêiner", "contêineres")
                ),
                "hostPort prende o pod a um nó e abre a porta na máquina — prefira Service",
                false,
            ),
            "token default" => (
                format!(
                    "{} com o token da conta default montado",
                    plural(quantos, "pod", "pods")
                ),
                "automountServiceAccountToken: false em quem não fala com a API; e uma conta própria para quem fala",
                false,
            ),
            _ => continue,
        };
        match grave {
            true => report.grave(frase, conserto),
            false => report.aviso(frase, conserto),
        }
    }

    // Pod Security Admission: o que o próprio Kubernetes aplica sem precisar de
    // ferramenta nenhuma instalada.
    let sem_psa: Vec<String> = l
        .itens("namespaces")
        .iter()
        .filter(|n| {
            let nome = txt(n, "metadata.name");
            !e_do_sistema(nome) && rotulo(n, "pod-security.kubernetes.io/enforce").is_empty()
        })
        .map(|n| txt(n, "metadata.name").to_string())
        .collect();
    report.field(
        "namespaces com PSA",
        format!(
            "{} de {}",
            l.itens("namespaces")
                .iter()
                .filter(|n| !rotulo(n, "pod-security.kubernetes.io/enforce").is_empty())
                .count(),
            l.itens("namespaces").len()
        ),
    );
    if !sem_psa.is_empty() {
        report.aviso(
            format!(
                "{} sem Pod Security Standards: {}",
                plural(sem_psa.len() as f64, "namespace", "namespaces"),
                one_line(&sem_psa.join(", "), 70)
            ),
            "o rótulo pod-security.kubernetes.io/enforce=baseline (ou restricted) barra pod privilegiado na porta de entrada, sem instalar nada",
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// RBAC: quem pode o quê.
// ─────────────────────────────────────────────────────────────────────────────────────

fn rbac(report: &mut Report, l: &Leitura) {
    report.section("RBAC");
    let vinculos = l.itens("vinculos_de_cluster");
    let papeis = l.itens("papeis_de_cluster");
    report.field("ClusterRoles", count(papeis.len() as f64));
    report.field("ClusterRoleBindings", count(vinculos.len() as f64));
    report.field("RoleBindings", count(l.itens("vinculos").len() as f64));

    // O que o kubeconfig que está sendo usado pode fazer. É a primeira pergunta de quem
    // recebe um arquivo desses e a última que alguém se lembra de fazer.
    let poderes = l.texto("meus_poderes");
    if !poderes.trim().is_empty() {
        let tudo = poderes
            .lines()
            .any(|linha| linha.starts_with("*.*") && linha.contains("[*]"));
        match tudo {
            true => report.aviso(
                "este kubeconfig pode tudo neste cluster (*.* com verbo *)".to_string(),
                "é uma credencial de administrador: guarde-a como tal, e use uma de leitura para investigar",
            ),
            false => report.line(format!(
                "este kubeconfig tem {} regra(s) de permissão",
                poderes.lines().count().saturating_sub(1)
            )),
        }
    }

    // Quem é administrador do cluster, nominalmente.
    report.table(&["Vínculo", "Papel", "Quem"]);
    let mut admins = 0usize;
    for v in vinculos {
        let papel = txt(v, "roleRef.name");
        let sujeitos: Vec<String> = v
            .get("subjects")
            .and_then(Value::as_array)
            .map(|ss| {
                ss.iter()
                    .map(|s| {
                        let ns = txt(s, "namespace");
                        match ns.is_empty() {
                            true => format!("{} {}", txt(s, "kind"), txt(s, "name")),
                            false => format!("{} {ns}/{}", txt(s, "kind"), txt(s, "name")),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let anonimo = sujeitos
            .iter()
            .any(|s| s.contains("system:anonymous") || s.contains("system:unauthenticated"))
            && !VINCULOS_DE_FABRICA.contains(&papel);
        let curinga = papel == "cluster-admin" || papel_e_curinga(papeis, papel);
        if !curinga && !anonimo {
            continue;
        }
        admins += 1;
        report.cells_deep(
            vec![
                txt(v, "metadata.name").to_string(),
                papel.to_string(),
                one_line(&sujeitos.join(", "), 80),
            ],
            match anonimo {
                true => Tone::Ruim,
                false => Tone::Aviso,
            },
            vec![
                ("papel", papel.to_string()),
                ("sujeitos", sujeitos.join("\n")),
                ("criado", txt(v, "metadata.creationTimestamp").to_string()),
            ],
            sujeitos.clone(),
        );
        report.destacar();
        if anonimo {
            report.grave(
                format!(
                    "{} dá {papel} a usuário não autenticado",
                    txt(v, "metadata.name")
                ),
                "qualquer um que alcance a API entra com esse poder, sem credencial nenhuma — remova o vínculo",
            );
        }
    }

    // Contas de serviço com poder de administrador: o caminho mais curto entre um pod
    // comprometido e o cluster inteiro.
    let contas_admin: Vec<String> = vinculos
        .iter()
        .filter(|v| {
            txt(v, "roleRef.name") == "cluster-admin"
                || papel_e_curinga(papeis, txt(v, "roleRef.name"))
        })
        .flat_map(|v| {
            v.get("subjects")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
                .iter()
                .filter(|s| txt(s, "kind") == "ServiceAccount")
                .map(|s| format!("{}/{}", txt(s, "namespace"), txt(s, "name")))
                .collect::<Vec<_>>()
        })
        .collect();
    if !contas_admin.is_empty() {
        report.grave(
            format!(
                "{} com poder total: {}",
                plural(contas_admin.len() as f64, "conta de serviço", "contas de serviço"),
                one_line(&contas_admin.join(", "), 70)
            ),
            "quem entrar num pod dessa conta administra o cluster — troque por um Role com o que a aplicação realmente usa",
        );
    }
    if admins == 0 {
        report.ok("nenhum vínculo de cluster com papel curinga ou sujeito anônimo");
    }

    // Papéis que dizem `*` em tudo, mesmo sem se chamar cluster-admin.
    let curingas: Vec<String> = papeis
        .iter()
        .filter(|p| {
            let nome = txt(p, "metadata.name");
            // Os do próprio Kubernetes são assim por definição.
            !nome.starts_with("system:") && nome != "cluster-admin" && regras_curinga(p)
        })
        .map(|p| txt(p, "metadata.name").to_string())
        .collect();
    if !curingas.is_empty() {
        report.aviso(
            format!(
                "{} com verbo e recurso `*`: {}",
                plural(curingas.len() as f64, "ClusterRole", "ClusterRoles"),
                one_line(&curingas.join(", "), 70)
            ),
            "um papel curinga também dá o que ainda não existe: todo CRD instalado amanhã já vem incluído",
        );
    }

    // Verbos que permitem virar outra pessoa ou conceder poder a si mesmo.
    let perigosos: Vec<String> = papeis
        .iter()
        .filter(|p| {
            let nome = txt(p, "metadata.name");
            !nome.starts_with("system:") && !PAPEIS_DE_FABRICA.contains(&nome)
        })
        .filter(|p| {
            regras(p).iter().any(|r| {
                lista_texto(r, "/verbs")
                    .iter()
                    .any(|v| matches!(v.as_str(), "impersonate" | "escalate" | "bind"))
            })
        })
        .map(|p| txt(p, "metadata.name").to_string())
        .collect();
    if !perigosos.is_empty() {
        report.aviso(
            format!(
                "papéis com impersonate/escalate/bind: {}",
                one_line(&perigosos.join(", "), 70)
            ),
            "esses três verbos contornam o RBAC: quem os tem pode se dar qualquer outro poder",
        );
    }
}

/// Os papéis que o próprio Kubernetes cria e vincula a usuário não autenticado — e que
/// não são achado nenhum.
///
/// `system:public-info-viewer` responde `/healthz` e `/version` a quem chegar, de
/// propósito: é como um balanceador descobre que a API está de pé. Chamar isso de grave
/// em todo cluster do mundo é ensinar quem lê a ignorar a linha vermelha.
const VINCULOS_DE_FABRICA: &[&str] = &[
    "system:public-info-viewer",
    "system:discovery",
    "system:basic-user",
    "system:service-account-issuer-discovery",
];

/// Os papéis embutidos que já vêm com verbo de concessão dentro — `admin` e `edit` têm
/// `bind` e `escalate` por definição, e reclamar deles é reclamar do Kubernetes.
const PAPEIS_DE_FABRICA: &[&str] = &["admin", "edit", "view", "cluster-admin"];

fn regras(papel: &Value) -> Vec<&Value> {
    papel
        .get("rules")
        .and_then(Value::as_array)
        .map(|r| r.iter().collect())
        .unwrap_or_default()
}

fn regras_curinga(papel: &Value) -> bool {
    regras(papel).iter().any(|r| {
        lista_texto(r, "/verbs").iter().any(|v| v == "*")
            && lista_texto(r, "/resources").iter().any(|v| v == "*")
    })
}

fn papel_e_curinga(papeis: &[Value], nome: &str) -> bool {
    papeis
        .iter()
        .find(|p| txt(p, "metadata.name") == nome)
        .is_some_and(regras_curinga)
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Rede: o que entra, o que sai, e o que está aberto.
// ─────────────────────────────────────────────────────────────────────────────────────

fn rede(report: &mut Report, l: &Leitura) {
    report.section("Rede");
    let servicos = l.itens("services");
    let mut por_tipo: BTreeMap<&str, usize> = BTreeMap::new();
    for s in servicos {
        *por_tipo.entry(txt(s, "spec.type")).or_default() += 1;
    }
    report.field(
        "serviços",
        por_tipo
            .iter()
            .map(|(t, n)| format!("{t} {n}"))
            .collect::<Vec<_>>()
            .join(" · "),
    );
    report.field("ingressos", count(l.itens("ingressos").len() as f64));
    report.field(
        "políticas de rede",
        count(l.itens("politicas_de_rede").len() as f64),
    );

    // Quem está publicado para fora, dito em uma tabela: é a resposta a «o que deste
    // cluster está na internet».
    report.table(&["Serviço", "Tipo", "Porta", "Endereço", "Destinos"]);
    let sem_destino: HashSet<String> = l
        .itens("endpoints")
        .iter()
        .filter(|e| {
            e.get("subsets")
                .and_then(Value::as_array)
                .map(|s| {
                    s.iter().all(|x| {
                        x.get("addresses")
                            .and_then(Value::as_array)
                            .is_none_or(|a| a.is_empty())
                    })
                })
                .unwrap_or(true)
        })
        .map(referencia)
        .collect();

    let mut expostos = 0usize;
    for s in servicos {
        let tipo = txt(s, "spec.type");
        let externo = !lista_texto(s, "/spec/externalIPs").is_empty();
        let publicado = matches!(tipo, "LoadBalancer" | "NodePort") || externo;
        let vazio = referencia(s);
        let sem_ninguem = sem_destino.contains(&vazio);
        if !publicado && !sem_ninguem {
            continue;
        }
        let portas: Vec<String> = s
            .pointer("/spec/ports")
            .and_then(Value::as_array)
            .map(|ps| {
                ps.iter()
                    .map(|p| match p.get("nodePort").and_then(Value::as_i64) {
                        Some(np) => format!(
                            "{}→{np}",
                            p.get("port").and_then(Value::as_i64).unwrap_or(0)
                        ),
                        None => format!("{}", p.get("port").and_then(Value::as_i64).unwrap_or(0)),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let endereco = s
            .pointer("/status/loadBalancer/ingress")
            .and_then(Value::as_array)
            .map(|v| {
                v.iter()
                    .map(|x| match txt(x, "ip").is_empty() {
                        true => txt(x, "hostname").to_string(),
                        false => txt(x, "ip").to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        if publicado {
            expostos += 1;
        }
        report.cells_deep(
            vec![
                vazio.clone(),
                tipo.to_string(),
                portas.join(" "),
                match endereco.is_empty() {
                    true => match tipo == "LoadBalancer" {
                        true => "aguardando endereço".to_string(),
                        false => lista_texto(s, "/spec/externalIPs").join(" "),
                    },
                    false => endereco.clone(),
                },
                match sem_ninguem {
                    true => "nenhum".to_string(),
                    false => "ok".to_string(),
                },
            ],
            match (sem_ninguem, publicado) {
                (true, _) => Tone::Aviso,
                (_, true) => Tone::Normal,
                _ => Tone::Dim,
            },
            vec![
                ("seletor", seletor_texto(s)),
                ("cluster IP", txt(s, "spec.clusterIP").to_string()),
                (
                    "política de tráfego",
                    txt(s, "spec.externalTrafficPolicy").to_string(),
                ),
            ],
            Vec::new(),
        );
        report.destacar();
        if sem_ninguem {
            report.aviso(
                format!("serviço {vazio} não aponta para nenhum pod pronto"),
                "seletor que não casa com rótulo nenhum, ou os pods não estão prontos — quem chamar esse serviço recebe recusa",
            );
        }
    }
    if expostos > 0 {
        report.aviso(
            format!(
                "{} publicado(s) para fora do cluster: confira um a um",
                plural(expostos as f64, "serviço", "serviços")
            ),
            "confirme que cada um deveria estar exposto, e que há algo filtrando na frente — NodePort abre a porta em todos os nós",
        );
    }

    // Ingressos sem TLS: tráfego de usuário em texto claro.
    let sem_tls: Vec<String> = l
        .itens("ingressos")
        .iter()
        .filter(|i| {
            i.pointer("/spec/tls")
                .and_then(Value::as_array)
                .is_none_or(|t| t.is_empty())
        })
        .map(referencia)
        .collect();
    if !sem_tls.is_empty() {
        report.aviso(
            format!(
                "{} sem TLS: {}",
                plural(sem_tls.len() as f64, "ingresso", "ingressos"),
                one_line(&sem_tls.join(", "), 70)
            ),
            "sem a seção tls o tráfego chega em texto claro até o controlador — e muitas vezes até o pod",
        );
    }

    // Um namespace sem NetworkPolicy é um namespace onde qualquer pod fala com qualquer
    // pod do cluster.
    let com_politica: HashSet<String> = l
        .itens("politicas_de_rede")
        .iter()
        .map(|p| txt(p, "metadata.namespace").to_string())
        .collect();
    let sem_politica: Vec<String> = l
        .itens("namespaces")
        .iter()
        .map(|n| txt(n, "metadata.name").to_string())
        .filter(|n| !e_do_sistema(n) && !com_politica.contains(n))
        .collect();
    if !sem_politica.is_empty() {
        report.aviso(
            format!(
                "{} sem NetworkPolicy: {}",
                plural(sem_politica.len() as f64, "namespace", "namespaces"),
                one_line(&sem_politica.join(", "), 70)
            ),
            "sem política, a rede do cluster é plana: um pod comprometido alcança o banco de dados do vizinho. Comece com um deny-all de entrada",
        );
    } else if !l.itens("namespaces").is_empty() {
        report.ok("todo namespace de aplicativo tem ao menos uma NetworkPolicy");
    }
}

fn seletor_texto(servico: &Value) -> String {
    servico
        .pointer("/spec/selector")
        .and_then(Value::as_object)
        .map(|s| {
            s.iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

// ─────────────────────────────────────────────────────────────────────────────────────
// Armazenamento.
// ─────────────────────────────────────────────────────────────────────────────────────

fn armazenamento(report: &mut Report, l: &Leitura) {
    report.section("Armazenamento");
    let pvcs = l.itens("pvcs");
    let pvs = l.itens("pvs");
    let classes = l.itens("storageclasses");
    report.field("PVCs", count(pvcs.len() as f64));
    report.field("PVs", count(pvs.len() as f64));
    report.field("StorageClasses", count(classes.len() as f64));

    let padrao: Vec<String> = classes
        .iter()
        .filter(|c| {
            c.pointer("/metadata/annotations/storageclass.kubernetes.io~1is-default-class")
                .and_then(Value::as_str)
                == Some("true")
        })
        .map(|c| txt(c, "metadata.name").to_string())
        .collect();
    report.field(
        "classe padrão",
        match padrao.len() {
            0 => "nenhuma".to_string(),
            _ => padrao.join(", "),
        },
    );
    match padrao.len() {
        0 if !classes.is_empty() => report.aviso(
            "nenhuma StorageClass é a padrão".to_string(),
            "um PVC sem storageClassName fica Pending para sempre — marque uma como padrão",
        ),
        n if n > 1 => report.grave(
            format!("{n} StorageClasses marcadas como padrão"),
            "com mais de uma padrão, o Kubernetes recusa o PVC que não escolher — deixe uma só",
        ),
        _ => {}
    }

    report.table(&["Objeto", "Estado", "Tamanho", "Classe", "Idade"]);
    let mut parados = 0usize;
    for pvc in pvcs {
        let fase = txt(pvc, "status.phase");
        if fase == "Bound" {
            continue;
        }
        parados += 1;
        report.cells_deep(
            vec![
                format!("PVC {}", referencia(pvc)),
                fase.to_string(),
                txt(pvc, "spec.resources.requests.storage").to_string(),
                txt(pvc, "spec.storageClassName").to_string(),
                duration(idade(pvc)),
            ],
            Tone::Ruim,
            vec![
                (
                    "modos de acesso",
                    lista_texto(pvc, "/spec/accessModes").join(", "),
                ),
                ("volume", txt(pvc, "spec.volumeName").to_string()),
                ("condições", condicoes_resumidas(pvc)),
            ],
            Vec::new(),
        );
        report.destacar();
    }
    for pv in pvs {
        let fase = txt(pv, "status.phase");
        if matches!(fase, "Bound" | "Available") {
            continue;
        }
        parados += 1;
        report.cells_deep(
            vec![
                format!("PV {}", txt(pv, "metadata.name")),
                fase.to_string(),
                txt(pv, "spec.capacity.storage").to_string(),
                txt(pv, "spec.storageClassName").to_string(),
                duration(idade(pv)),
            ],
            match fase {
                "Failed" => Tone::Ruim,
                _ => Tone::Aviso,
            },
            vec![
                (
                    "política de reciclagem",
                    txt(pv, "spec.persistentVolumeReclaimPolicy").to_string(),
                ),
                ("motivo", one_line(txt(pv, "status.message"), 200)),
                (
                    "reivindicado por",
                    txt(pv, "spec.claimRef.name").to_string(),
                ),
            ],
            Vec::new(),
        );
        report.destacar();
    }
    if parados == 0 {
        report.ok("todo volume está ligado ao que devia");
    } else {
        report.aviso(
            format!(
                "{} fora de Bound",
                plural(parados as f64, "volume", "volumes")
            ),
            "um PVC Pending trava o pod que o espera; um PV Released ainda ocupa o disco lá atrás",
        );
    }

    // Reciclagem: um `Delete` num volume de banco de dados apaga o disco junto com o PVC.
    let deletam: Vec<String> = pvs
        .iter()
        .filter(|pv| txt(pv, "spec.persistentVolumeReclaimPolicy") == "Delete")
        .map(|pv| txt(pv, "metadata.name").to_string())
        .collect();
    if !deletam.is_empty() {
        report.line(format!(
            "{} com reclaimPolicy=Delete: apagar o PVC apaga o disco",
            plural(deletam.len() as f64, "volume", "volumes")
        ));
    }
}

// ─────────────────────────────────────────────────────────────────────────────────────
// A plataforma por baixo: o que quebra tudo quando quebra.
// ─────────────────────────────────────────────────────────────────────────────────────

fn plataforma(report: &mut Report, l: &Leitura) {
    report.section("Plataforma");
    let apis = l.itens("apiservices");
    let crds = l.itens("crds");
    report.field("APIServices", count(apis.len() as f64));
    report.field("CRDs", count(crds.len() as f64));
    report.field(
        "recursos da API",
        count(l.texto("recursos_da_api").lines().count() as f64),
    );

    // Uma API agregada fora do ar faz `kubectl get` inteiro falhar em alguns clientes, e
    // deixa webhook e métrica sem resposta.
    report.table(&["APIService", "Motivo", "Mensagem"]);
    let mut quebradas = 0usize;
    for api in apis {
        let Some(c) = condicao(api, "Available") else {
            continue;
        };
        if txt(c, "status") == "True" {
            continue;
        }
        quebradas += 1;
        report.cells_deep(
            vec![
                txt(api, "metadata.name").to_string(),
                txt(c, "reason").to_string(),
                one_line(txt(c, "message"), 80),
            ],
            Tone::Ruim,
            vec![
                (
                    "serviço",
                    format!(
                        "{}/{}",
                        txt(api, "spec.service.namespace"),
                        txt(api, "spec.service.name")
                    ),
                ),
                ("mensagem", txt(c, "message").to_string()),
            ],
            Vec::new(),
        );
        report.destacar();
        report.grave(
            format!("APIService {} indisponível", txt(api, "metadata.name")),
            "uma API agregada fora do ar derruba quem depende dela — e, em alguns clientes, faz a listagem inteira falhar",
        );
    }
    if quebradas == 0 && !apis.is_empty() {
        report.ok("todas as APIServices respondem");
    }

    // Webhooks que podem travar o cluster: quem recusa quando o serviço dele não
    // responde, e ainda vale para o namespace do sistema.
    for (consulta, tipo) in [
        ("webhooks_validam", "validação"),
        ("webhooks_mudam", "mutação"),
    ] {
        for w in l.itens(consulta) {
            let nome = txt(w, "metadata.name");
            let duros: Vec<String> = w
                .get("webhooks")
                .and_then(Value::as_array)
                .map(|ws| {
                    ws.iter()
                        .filter(|x| txt(x, "failurePolicy") != "Ignore")
                        .map(|x| txt(x, "name").to_string())
                        .collect()
                })
                .unwrap_or_default();
            if duros.is_empty() {
                continue;
            }
            report.aviso(
                format!("webhook de {tipo} {nome} recusa quando falha (failurePolicy=Fail)"),
                "se o serviço do webhook cair, tudo que ele intercepta para de ser criado — inclusive os pods que o ressuscitariam",
            );
        }
    }

    let pendentes: Vec<String> = l
        .itens("pedidos_de_certificado")
        .iter()
        .filter(|c| condicao(c, "Approved").is_none() && condicao(c, "Denied").is_none())
        .map(|c| txt(c, "metadata.name").to_string())
        .collect();
    if !pendentes.is_empty() {
        report.aviso(
            format!(
                "{} de certificado sem decisão: {}",
                plural(pendentes.len() as f64, "pedido", "pedidos"),
                one_line(&pendentes.join(", "), 60)
            ),
            "um CSR pendente costuma ser um kubelet esperando entrar no cluster — ou alguém pedindo um certificado que ninguém revisou",
        );
    }

    // O que todo cluster de produção precisa ter e às vezes não tem.
    let nomes_crd: Vec<String> = crds
        .iter()
        .map(|c| txt(c, "metadata.name").to_string())
        .collect();
    let tem = |palavra: &str| nomes_crd.iter().any(|n| n.contains(palavra));
    let mut faltando = Vec::new();
    if l.texto("consumo_nos").trim().is_empty() {
        faltando.push("metrics-server");
    }
    if !tem("cert-manager") {
        faltando.push("cert-manager (certificados automáticos)");
    }
    if !tem("monitoring.coreos.com") && !tem("prometheus") {
        faltando.push("Prometheus Operator (métricas e alertas)");
    }
    if !faltando.is_empty() {
        report.line(format!(
            "não vi no cluster: {} — pode ser de propósito, ou pode ser o que falta para operar",
            faltando.join(", ")
        ));
    }
}

/// Os segredos, contados — nunca lidos.
fn segredos(report: &mut Report, l: &Leitura) {
    report.section("Segredos");
    let texto = l.texto("segredos");
    if texto.trim().is_empty() {
        match l.falhou("segredos") {
            Some(motivo) => report.line(format!("não deu para listar: {}", one_line(motivo, 80))),
            None => report.line("nenhum segredo visível com este acesso"),
        }
        return;
    }
    report.line(
        "só nome, tipo e idade — o conteúdo de um segredo nunca é pedido por esta ferramenta",
    );

    let mut por_tipo: BTreeMap<String, usize> = BTreeMap::new();
    let mut por_ns: BTreeMap<String, usize> = BTreeMap::new();
    let mut antigos: Vec<(String, f64)> = Vec::new();
    let mut legados = 0usize;
    let mut quantos = 0usize;
    // `custom-columns` escreve o cabeçalho na primeira linha, e não há flag para tirá-lo
    // sem perder as colunas escolhidas — `--no-headers` só existe no formato de tabela
    // padrão. Contá-lo daria um segredo a mais em todo cluster.
    for linha in texto.lines().skip(1) {
        let campos: Vec<&str> = linha.split_whitespace().collect();
        if campos.len() < 4 {
            continue;
        }
        let (ns, nome, tipo, criado) = (campos[0], campos[1], campos[2], campos[3]);
        quantos += 1;
        *por_tipo.entry(tipo.to_string()).or_default() += 1;
        *por_ns.entry(ns.to_string()).or_default() += 1;
        if tipo == "kubernetes.io/service-account-token" {
            legados += 1;
        }
        let quanto = agora() - epoch(criado);
        if tipo == "kubernetes.io/tls" || tipo.contains("basic-auth") {
            antigos.push((format!("{ns}/{nome} ({tipo})"), quanto));
        }
    }
    report.field("total", count(quantos as f64));
    report.field("namespaces com segredo", count(por_ns.len() as f64));
    report.table(&["Tipo", "Quantos"]);
    for (tipo, quantos) in &por_tipo {
        report.cells(vec![tipo.clone(), count(*quantos as f64)], Tone::Normal);
        report.destacar();
    }

    if legados > 0 {
        report.aviso(
            format!("{legados} token(s) de conta de serviço gravados como Secret"),
            "desde a 1.24 o token é emitido por tempo limitado e montado direto; um Secret desses é uma credencial que não expira",
        );
    }
    antigos.sort_by(|a, b| b.1.total_cmp(&a.1));
    if let Some((quem, quanto)) = antigos.first()
        && *quanto > 365.0 * 86_400.0
    {
        report.aviso(
            format!("segredo de TLS/credencial sem rotação há {}: {quem}", duration(*quanto)),
            "a data aqui é a de criação do objeto, não a validade do certificado — mas um segredo de mais de um ano raramente foi rodado",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_quantidades_do_jeito_que_a_api_escreve() {
        assert_eq!(cpu_de("1500m"), 1500.0);
        assert_eq!(cpu_de("2"), 2000.0);
        assert_eq!(cpu_de("250000000n"), 250.0);
        assert_eq!(cpu_de(""), 0.0);
        assert_eq!(mem_de("128Mi"), 128.0 * 1024.0 * 1024.0);
        assert_eq!(mem_de("1Gi"), 1024.0 * 1024.0 * 1024.0);
        assert_eq!(mem_de("1000000"), 1_000_000.0);
        assert_eq!(mem_de("2G"), 2e9);
    }

    #[test]
    fn um_caminho_que_falta_no_meio_devolve_vazio_em_vez_de_quebrar() {
        let v: Value = serde_json::json!({"status": {"phase": "Running"}});
        assert_eq!(txt(&v, "status.phase"), "Running");
        assert_eq!(txt(&v, "status.conditions.0.type"), "");
        assert_eq!(txt(&v, "spec.nodeName"), "");
    }

    /// O `minor` vem com `+` nos clusters gerenciados, e uma versão que não parseia não
    /// pode virar «fora de suporte» por acidente.
    #[test]
    fn le_o_minor_inclusive_com_mais() {
        let v: Value = serde_json::json!({"serverVersion": {"minor": "29+"}});
        assert_eq!(minor(&v, "serverVersion"), Some(29));
        let vazio: Value = serde_json::json!({});
        assert_eq!(minor(&vazio, "serverVersion"), None);
    }

    #[test]
    fn um_pod_saudavel_nao_vira_problema() {
        let pod: Value = serde_json::json!({
            "metadata": {"name": "ok", "namespace": "app"},
            "status": {
                "phase": "Running",
                "containerStatuses": [{"name": "app", "ready": true, "restartCount": 0, "state": {"running": {}}}]
            }
        });
        assert!(problema_do_pod(&pod).is_none());
    }

    /// Um pod que está «Running» agora mas foi morto por falta de memória antes continua
    /// sendo o achado mais caro do cluster — e some de qualquer contagem por fase.
    #[test]
    fn oomkilled_na_encarnacao_anterior_e_um_achado() {
        let pod: Value = serde_json::json!({
            "metadata": {"name": "comilao", "namespace": "app"},
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "app", "ready": true, "restartCount": 3,
                    "state": {"running": {}},
                    "lastState": {"terminated": {"reason": "OOMKilled", "exitCode": 137}}
                }]
            }
        });
        let p = problema_do_pod(&pod).expect("tinha que achar");
        assert_eq!(p.familia, "morto por falta de memória");
        assert_eq!(p.tom, Tone::Ruim);
    }

    #[test]
    fn crashloop_ganha_o_motivo_do_conteiner() {
        let pod: Value = serde_json::json!({
            "metadata": {"name": "q", "namespace": "app"},
            "status": {
                "phase": "Running",
                "containerStatuses": [{
                    "name": "app", "ready": false, "restartCount": 9,
                    "state": {"waiting": {"reason": "CrashLoopBackOff", "message": "back-off 5m"}}
                }]
            }
        });
        let p = problema_do_pod(&pod).expect("tinha que achar");
        assert_eq!(p.familia, "CrashLoopBackOff");
    }

    /// Só a soma dos contêineres normais — os de inicialização já terminaram quando o pod
    /// está de pé, e somá-los daria um nó mais cheio do que ele está.
    #[test]
    fn soma_o_pedido_dos_conteineres_normais() {
        let pod: Value = serde_json::json!({
            "spec": {
                "containers": [
                    {"name": "a", "resources": {"requests": {"cpu": "100m", "memory": "64Mi"}}},
                    {"name": "b", "resources": {"requests": {"cpu": "1", "memory": "1Gi"}}}
                ],
                "initContainers": [
                    {"name": "i", "resources": {"requests": {"cpu": "4", "memory": "8Gi"}}}
                ]
            }
        });
        let (cpu, mem) = pedido_do_pod(&pod);
        assert_eq!(cpu, 1100.0);
        assert_eq!(mem, 64.0 * 1024.0 * 1024.0 + 1024.0 * 1024.0 * 1024.0);
    }

    #[test]
    fn um_pdb_sem_seletor_cobre_o_namespace_inteiro() {
        let carga: Value = serde_json::json!({
            "metadata": {"namespace": "app"},
            "spec": {"template": {"metadata": {"labels": {"app": "x"}}}}
        });
        let aberto: Value = serde_json::json!({"metadata": {"namespace": "app"}, "spec": {}});
        let outro: Value = serde_json::json!({
            "metadata": {"namespace": "app"},
            "spec": {"selector": {"matchLabels": {"app": "y"}}}
        });
        assert!(tem_pdb(&carga, std::slice::from_ref(&aberto)));
        assert!(!tem_pdb(&carga, std::slice::from_ref(&outro)));
    }

    #[test]
    fn um_papel_com_estrela_em_tudo_e_curinga() {
        let papel: Value = serde_json::json!({
            "metadata": {"name": "tudo"},
            "rules": [{"apiGroups": ["*"], "resources": ["*"], "verbs": ["*"]}]
        });
        let modesto: Value = serde_json::json!({
            "metadata": {"name": "leitor"},
            "rules": [{"apiGroups": [""], "resources": ["pods"], "verbs": ["get", "list"]}]
        });
        assert!(regras_curinga(&papel));
        assert!(!regras_curinga(&modesto));
    }

    /// A investigação inteira contra um cluster de verdade, para os olhos de quem está
    /// escrevendo. Ignorada por padrão: ela precisa de um cluster.
    ///
    /// `SHERLOCK_KUBECONFIG=/caminho cargo test espiar_cluster -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn espiar_cluster() {
        use std::sync::{Arc, Mutex};
        let Ok(kubeconfig) = std::env::var("SHERLOCK_KUBECONFIG") else {
            panic!("defina SHERLOCK_KUBECONFIG");
        };
        let nivel = std::env::var("SHERLOCK_NIVEL").unwrap_or_else(|_| "profundo".to_string());
        let params: HashMap<&'static str, String> = HashMap::from([
            ("motor", super::super::ENGINES[3].to_string()),
            ("kubeconfig", kubeconfig),
            ("nivel", nivel),
            (
                "namespace",
                std::env::var("SHERLOCK_NAMESPACE").unwrap_or_default(),
            ),
        ]);
        let plan = Plan::from(&params).expect("plano");
        let (execution, recorder) = crate::tools::Execution::new(1, "Sherlock", "teste".into());
        let board = Arc::new(Mutex::new(crate::tools::Board::default()));
        let mut report = Report::new(&recorder, Arc::clone(&board));
        let mut sessao = Session::open(&plan).expect("abrir");
        // Duas passadas quando se pede, para ver o cartão de Movimento com o que mudou.
        let passadas: u32 = std::env::var("SHERLOCK_PASSADAS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        for _ in 0..passadas {
            report.begin();
            sessao.pass(&mut report).expect("passada");
            report.end(std::time::Duration::from_secs(1));
        }
        let quadro = crate::tools::lock_board(&board);
        for card in &quadro.cards {
            println!("\n══════ {} ══════", card.title);
            for linha in card.summary.como_texto() {
                println!("{linha}");
            }
            println!("  ── detalhe ──");
            for linha in card.detail.como_texto() {
                println!("  {linha}");
            }
        }
        drop(execution);
    }
}
