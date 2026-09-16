//! Alcançando um cluster pelo `kubectl` que já está instalado aqui.
//!
//! Não existe cliente de Kubernetes escrito neste projeto, e não deve existir — a mesma
//! decisão do `ssh`, pela mesma razão. O `kubectl` lê o kubeconfig, escolhe o contexto,
//! apresenta o certificado de cliente, renova o token, chama o `aws eks get-token` ou o
//! `gke-gcloud-auth-plugin` quando o arquivo manda chamar, respeita o `proxy-url` e
//! confere a CA do cluster. Um cliente escrito aqui seria um segundo `kubectl`, pior, que
//! ignoraria tudo isso — e que não conseguiria falar com os clusters gerenciados, que são
//! quase todos.
//!
//! **Só leitura, e a prova está numa lista.** Tudo que a investigação pergunta está em
//! `CONSULTAS`, que dá para ler de ponta a ponta, e todo comando passa por `permitido`
//! antes de existir como processo: `get`, `version`, `api-resources`, `auth can-i`,
//! `config view`, `cluster-info` e `top`. Não há `apply`, `create`, `delete`, `patch`,
//! `edit`, `scale`, `drain`, `cordon`, `rollout`, `exec`, `port-forward` nem `cp` — e um
//! deles não *deixa* de ser chamado por disciplina de quem escreve: ele é recusado aqui,
//! antes de virar um processo, e um teste confere a lista inteira.
//!
//! **Nem os segredos.** O conteúdo de um `Secret` nunca é pedido: o que se pergunta é o
//! nome, o tipo e a idade dele, por colunas escolhidas à mão. Uma ferramenta que puxa o
//! `data` de todo segredo do cluster para dentro de um relatório que alguém vai colar num
//! chamado é uma ferramenta que vaza — por mais somente-leitura que ela seja.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;

use super::{Depth, Plan};

/// Quanto tempo uma pergunta inteira pode levar aqui, incluindo o processo.
const TIMEOUT: Duration = Duration::from_secs(60);
/// Quanto tempo o servidor tem para responder cada uma. Vai como argumento para o
/// `kubectl` porque é ele quem fala com a API — e um cluster doente é justamente onde a
/// diferença entre esperar e desistir importa.
const PEDIDO: &str = "--request-timeout=25s";
/// Teto do que se lê de uma resposta. Um `get pods -A -o json` de um cluster grande são
/// dezenas de megabytes; acima disto a investigação prefere dizer que não coube.
const MAX_RESPOSTA: u64 = 64 * 1024 * 1024;

/// Os primeiros verbos que esta engine sabe pronunciar. Ver `permitido`.
const VERBOS: &[&str] = &[
    "get",
    "version",
    "api-resources",
    "api-versions",
    "auth",
    "config",
    "cluster-info",
    "top",
];

/// Se um comando é um que esta ferramenta pode executar.
///
/// Dois deles têm subcomando que escreve — `kubectl auth reconcile` cria papéis, e
/// `kubectl config set-context` reescreve o arquivo de quem está rodando — então o verbo
/// sozinho não basta: o segundo argumento é conferido junto. `cluster-info dump` também
/// fica de fora, por escrever arquivos quando recebe `--output-directory`.
pub fn permitido(args: &[String]) -> bool {
    let Some(verbo) = args.first().map(String::as_str) else {
        return false;
    };
    if !VERBOS.contains(&verbo) {
        return false;
    }
    let segundo = args.get(1).map(String::as_str).unwrap_or("");
    match verbo {
        "auth" => segundo == "can-i",
        "config" => segundo == "view",
        "cluster-info" => segundo != "dump",
        _ => true,
    }
}

/// De onde veio a configuração do cluster.
enum Config {
    /// O arquivo que já estava na máquina: o `$KUBECONFIG`, ou o `~/.kube/config`.
    Detectado(PathBuf),
    /// Um arquivo apontado no formulário.
    Arquivo(PathBuf),
    /// O texto colado no formulário, gravado num arquivo temporário só desta
    /// investigação e apagado quando ela termina — ver `Temporario`.
    Colado(Temporario),
}

/// Um kubeconfig colado, enquanto a investigação dura.
///
/// O `kubectl` só lê configuração de arquivo, então um texto colado precisa virar um. Ele
/// nasce com permissão 0600 — é credencial —, mora na pasta temporária do usuário e é
/// apagado no `Drop`, inclusive quando a investigação falha no meio.
struct Temporario {
    caminho: PathBuf,
}

impl Temporario {
    fn novo(conteudo: &str) -> Result<Self, String> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let nome = format!(
            "monitorzinho-kubeconfig-{}-{}",
            std::process::id(),
            crate::db::agora()
        );
        let caminho = std::env::temp_dir().join(nome);
        let mut arquivo = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&caminho)
            .map_err(|e| format!("não consegui guardar o kubeconfig colado: {e}"))?;
        arquivo
            .write_all(conteudo.as_bytes())
            .map_err(|e| format!("não consegui guardar o kubeconfig colado: {e}"))?;
        Ok(Self { caminho })
    }
}

impl Drop for Temporario {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.caminho);
    }
}

/// Onde perguntar, e com que identidade.
pub struct Target {
    kubectl: PathBuf,
    config: Config,
    /// Vazio quer dizer «o contexto atual do arquivo», que é o que todo mundo espera.
    pub context: String,
    /// Vazio quer dizer o cluster inteiro — `-A`. Preenchido, a investigação se restringe
    /// a um namespace, e as seções que são do cluster dizem que são.
    pub namespace: String,
}

impl Target {
    pub fn parse(plan: &Plan) -> Result<Self, String> {
        let kubectl =
            kubectl_binary().ok_or_else(|| "não achei o comando kubectl no PATH".to_string())?;
        let bruto = plan.kubeconfig.trim();
        let config = match bruto {
            // Vazio é o caminho normal: o arquivo que a máquina já usa.
            "" => Config::Detectado(padrao().ok_or_else(|| {
                "não achei kubeconfig nenhum — informe o arquivo, ou cole o conteúdo".to_string()
            })?),
            texto if parece_kubeconfig(texto) => Config::Colado(Temporario::novo(texto)?),
            caminho => {
                let caminho = expandir(caminho);
                if !caminho.exists() {
                    return Err(format!("não achei o kubeconfig {}", caminho.display()));
                }
                Config::Arquivo(caminho)
            }
        };
        Ok(Self {
            kubectl,
            config,
            context: plan.contexto.trim().to_string(),
            namespace: plan.namespace.trim().to_string(),
        })
    }

    fn caminho(&self) -> &Path {
        match &self.config {
            Config::Detectado(p) | Config::Arquivo(p) => p,
            Config::Colado(temp) => &temp.caminho,
        }
    }

    /// Quem e onde, sem nada que seja credencial.
    ///
    /// O nome do arquivo e o do contexto, nunca o token nem o certificado — esta linha
    /// fica na listagem de execuções, à vista de quem passar pela tela.
    pub fn summary(&self) -> String {
        let onde = match &self.config {
            Config::Colado(_) => "kubeconfig colado".to_string(),
            Config::Detectado(p) | Config::Arquivo(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.display().to_string()),
        };
        let contexto = match self.context.is_empty() {
            true => "contexto atual".to_string(),
            false => self.context.clone(),
        };
        let escopo = match self.namespace.is_empty() {
            true => String::new(),
            false => format!(" · ns {}", self.namespace),
        };
        format!("{onde} · {contexto}{escopo}")
    }

    /// A linha como seria digitada, para quem quiser conferir o que foi perguntado.
    pub fn command_line(&self, consulta: &Consulta) -> String {
        let mut linha = self.kubectl.display().to_string();
        for arg in self.argv(consulta) {
            linha.push(' ');
            linha.push_str(&arg);
        }
        linha
    }

    fn argv(&self, consulta: &Consulta) -> Vec<String> {
        let mut args: Vec<String> = consulta.args.iter().map(|a| a.to_string()).collect();
        if consulta.escopo {
            match self.namespace.is_empty() {
                true => args.push("--all-namespaces".to_string()),
                false => {
                    args.push("--namespace".to_string());
                    args.push(self.namespace.clone());
                }
            }
        }
        args.push(PEDIDO.to_string());
        args.push(format!("--kubeconfig={}", self.caminho().display()));
        if !self.context.is_empty() {
            args.push(format!("--context={}", self.context));
        }
        args
    }

    /// Faz uma pergunta e devolve a resposta crua.
    ///
    /// O erro devolvido é a última linha do que o `kubectl` reclamou, que é onde ele põe
    /// o motivo — «Unauthorized», «connection refused», «the server doesn't have a
    /// resource type "x"». Quem chama decide se aquilo é fatal ou só uma seção que não vai
    /// existir nesta investigação.
    pub fn run(&self, consulta: &Consulta) -> Result<String, String> {
        let args = self.argv(consulta);
        // A recusa acontece **antes** do processo existir. Esta é a linha que sustenta a
        // promessa do módulo inteiro.
        if !permitido(&args) {
            return Err(format!(
                "comando recusado por esta ferramenta: kubectl {}",
                args.first().cloned().unwrap_or_default()
            ));
        }
        let mut filho = Command::new(&self.kubectl)
            .args(&args)
            // Sem entrada: nada aqui lê de stdin, e um kubectl que espera por um arquivo
            // que não vem seria uma tela travada sem causa visível.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // Um plugin de credencial (`aws`, `gcloud`, `kubelogin`) pode querer abrir o
            // navegador ou perguntar algo; aqui ninguém pode responder.
            .env("KUBECTL_INTERACTIVE", "false")
            .spawn()
            .map_err(|e| format!("não consegui executar o kubectl: {e}"))?;

        let (envia, recebe) = std::sync::mpsc::channel();
        let mut saida = filho.stdout.take();
        let mut erro = filho.stderr.take();
        std::thread::spawn(move || {
            let mut texto = String::new();
            if let Some(saida) = saida.as_mut() {
                let _ = saida.take(MAX_RESPOSTA).read_to_string(&mut texto);
            }
            let mut queixa = String::new();
            if let Some(erro) = erro.as_mut() {
                let _ = erro.take(64 * 1024).read_to_string(&mut queixa);
            }
            let _ = envia.send((texto, queixa));
        });

        let (texto, queixa) = match recebe.recv_timeout(TIMEOUT) {
            Ok(par) => par,
            Err(_) => {
                let _ = filho.kill();
                return Err(format!(
                    "o cluster não respondeu «{}» em {}s",
                    consulta.nome,
                    TIMEOUT.as_secs()
                ));
            }
        };
        let _ = filho.wait();
        if texto.trim().is_empty() && !queixa.trim().is_empty() {
            return Err(limpar(&queixa));
        }
        Ok(texto)
    }

    /// A mesma pergunta, já em JSON.
    pub fn json(&self, consulta: &Consulta) -> Result<Value, String> {
        let texto = self.run(consulta)?;
        serde_json::from_str(&texto)
            .map_err(|e| format!("não entendi a resposta de «{}»: {e}", consulta.nome))
    }
}

/// Uma pergunta da investigação.
///
/// Todas moram em `CONSULTAS`, e é de lá que sai a lista do que esta ferramenta é capaz
/// de fazer. Cada uma declara a profundidade em que entra, porque um `get pods -A` num
/// cluster de dez mil pods não é de graça para o servidor da API.
pub struct Consulta {
    /// A chave pela qual `k8s_checks` procura a resposta.
    pub nome: &'static str,
    pub args: &'static [&'static str],
    pub depth: Depth,
    /// Se o objeto é de namespace — e portanto a pergunta ganha `-A` ou `-n`.
    pub escopo: bool,
}

const fn c(
    nome: &'static str,
    args: &'static [&'static str],
    depth: Depth,
    escopo: bool,
) -> Consulta {
    Consulta {
        nome,
        args,
        depth,
        escopo,
    }
}

/// **Tudo** que a investigação pergunta, num lugar só.
///
/// É o equivalente ao `SCRIPT` da engine de SSH: dá para ler antes de rodar e conferir
/// que não há nada ali que escreva. A ordem é a da leitura — o que é mais barato e mais
/// esclarecedor primeiro.
pub const CONSULTAS: &[Consulta] = &[
    // ── raso: o que a API já sabe de cor ──────────────────────────────────────────
    c("versao", &["version", "-o", "json"], Depth::Raso, false),
    c("nos", &["get", "nodes", "-o", "json"], Depth::Raso, false),
    c(
        "namespaces",
        &["get", "namespaces", "-o", "json"],
        Depth::Raso,
        false,
    ),
    c("pods", &["get", "pods", "-o", "json"], Depth::Raso, true),
    c(
        "eventos",
        &["get", "events", "-o", "json"],
        Depth::Raso,
        true,
    ),
    // O `/readyz` verboso é o que o próprio control plane responde sobre si: etcd,
    // scheduler, controller-manager, cada um com uma linha. Num cluster gerenciado ele
    // costuma ser negado — e negado vira «não deu para saber», nunca um susto.
    c(
        "readyz",
        &["get", "--raw", "/readyz?verbose"],
        Depth::Raso,
        false,
    ),
    // ── médio: o que exige listar os controladores e a plataforma ─────────────────
    c(
        "deployments",
        &["get", "deployments", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "statefulsets",
        &["get", "statefulsets", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "daemonsets",
        &["get", "daemonsets", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c("jobs", &["get", "jobs", "-o", "json"], Depth::Medio, true),
    c(
        "cronjobs",
        &["get", "cronjobs", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "services",
        &["get", "services", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "endpoints",
        &["get", "endpoints", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "ingressos",
        &["get", "ingresses", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "politicas_de_rede",
        &["get", "networkpolicies", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "pdbs",
        &["get", "poddisruptionbudgets", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "hpas",
        &["get", "horizontalpodautoscalers", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c("pvcs", &["get", "pvc", "-o", "json"], Depth::Medio, true),
    c("pvs", &["get", "pv", "-o", "json"], Depth::Medio, false),
    c(
        "storageclasses",
        &["get", "storageclasses", "-o", "json"],
        Depth::Medio,
        false,
    ),
    c(
        "cotas",
        &["get", "resourcequotas", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "limites_padrao",
        &["get", "limitranges", "-o", "json"],
        Depth::Medio,
        true,
    ),
    c(
        "papeis_de_cluster",
        &["get", "clusterroles", "-o", "json"],
        Depth::Medio,
        false,
    ),
    c(
        "vinculos_de_cluster",
        &["get", "clusterrolebindings", "-o", "json"],
        Depth::Medio,
        false,
    ),
    c(
        "vinculos",
        &["get", "rolebindings", "-o", "json"],
        Depth::Medio,
        true,
    ),
    // O que **eu** posso fazer neste cluster. É a primeira pergunta de quem recebe um
    // kubeconfig e não sabe o que ele carrega — e a resposta aparece na tela em vez de
    // ficar só na cabeça de quem entregou o arquivo.
    c(
        "meus_poderes",
        &["auth", "can-i", "--list"],
        Depth::Medio,
        false,
    ),
    c(
        "apiservices",
        &["get", "apiservices", "-o", "json"],
        Depth::Medio,
        false,
    ),
    // Métricas: só existem com metrics-server instalado. Sem ele o comando falha, e a
    // seção diz que o cluster não tem de onde tirar consumo — que já é um achado.
    c(
        "consumo_nos",
        &["top", "nodes", "--no-headers"],
        Depth::Medio,
        false,
    ),
    c(
        "consumo_pods",
        &["top", "pods", "--no-headers"],
        Depth::Medio,
        true,
    ),
    // ── profundo: o que é caro de listar e muda pouco ─────────────────────────────
    c(
        "papeis",
        &["get", "roles", "-o", "json"],
        Depth::Profundo,
        true,
    ),
    c(
        "contas",
        &["get", "serviceaccounts", "-o", "json"],
        Depth::Profundo,
        true,
    ),
    // Nome, tipo e idade — nunca o conteúdo. Ver o cabeçalho deste arquivo.
    c(
        "segredos",
        &[
            "get",
            "secrets",
            "-o",
            "custom-columns=NS:.metadata.namespace,NOME:.metadata.name,TIPO:.type,CRIADO:.metadata.creationTimestamp",
        ],
        Depth::Profundo,
        true,
    ),
    c(
        "crds",
        &["get", "crds", "-o", "json"],
        Depth::Profundo,
        false,
    ),
    c(
        "webhooks_validam",
        &["get", "validatingwebhookconfigurations", "-o", "json"],
        Depth::Profundo,
        false,
    ),
    c(
        "webhooks_mudam",
        &["get", "mutatingwebhookconfigurations", "-o", "json"],
        Depth::Profundo,
        false,
    ),
    c(
        "pedidos_de_certificado",
        &["get", "certificatesigningrequests", "-o", "json"],
        Depth::Profundo,
        false,
    ),
    c(
        "classes_de_prioridade",
        &["get", "priorityclasses", "-o", "json"],
        Depth::Profundo,
        false,
    ),
    c(
        "recursos_da_api",
        &["api-resources", "--no-headers"],
        Depth::Profundo,
        false,
    ),
];

/// As consultas de uma profundidade.
pub fn consultas(depth: Depth) -> impl Iterator<Item = &'static Consulta> {
    CONSULTAS.iter().filter(move |c| c.depth <= depth)
}

/// O kubeconfig que esta máquina usaria sozinha: o `$KUBECONFIG` — que pode listar
/// vários, separados por `:`, e aí vale o primeiro que existe — ou o `~/.kube/config`.
pub fn padrao() -> Option<PathBuf> {
    if let Some(lista) = std::env::var_os("KUBECONFIG") {
        for caminho in std::env::split_paths(&lista) {
            if caminho.is_file() {
                return Some(caminho);
            }
        }
    }
    let padrao = dirs::home_dir()?.join(".kube").join("config");
    padrao.is_file().then_some(padrao)
}

/// Os kubeconfigs que estão em `~/.kube`, para o formulário oferecer o que já existe.
///
/// Quem tem mais de um cluster tem mais de um arquivo ali, com nomes que só a pessoa
/// conhece — oferecê-los é a diferença entre escolher e lembrar.
pub fn candidatos() -> Vec<PathBuf> {
    let Some(pasta) = dirs::home_dir().map(|home| home.join(".kube")) else {
        return Vec::new();
    };
    let Ok(leitura) = std::fs::read_dir(&pasta) else {
        return Vec::new();
    };
    let mut achados: Vec<PathBuf> = leitura
        .flatten()
        .map(|entrada| entrada.path())
        .filter(|caminho| caminho.is_file())
        .filter(|caminho| {
            let nome = caminho
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            // Os `.bak` de quem já mexeu nisso ficam de fora: eles apontam para clusters
            // que podem nem existir mais, e um erro de conexão por escolher o arquivo
            // errado é o tipo de confusão que a sugestão deveria evitar.
            nome.starts_with("config") && !nome.contains(".bak")
        })
        .collect();
    achados.sort();
    achados
}

/// Se o que veio no campo é o conteúdo de um kubeconfig, e não um caminho.
///
/// Colado, ele tem sempre as duas palavras que o formato exige. Um caminho não tem
/// nenhuma das duas — nem um caminho com espaço, nem um `~/.kube/config-de-produção`.
pub fn parece_kubeconfig(texto: &str) -> bool {
    let texto = texto.trim_start();
    (texto.contains("apiVersion") || texto.contains("clusters:"))
        && texto.contains(':')
        && (texto.contains('\n') || texto.len() > 200)
}

/// `~/.kube/config` com o `~` resolvido.
fn expandir(caminho: &str) -> PathBuf {
    match caminho.strip_prefix("~/") {
        Some(resto) => dirs::home_dir().unwrap_or_default().join(resto),
        None => PathBuf::from(caminho),
    }
}

fn kubectl_binary() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("kubectl"))
        .find(|candidato| executavel(candidato))
}

fn executavel(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// A reclamação do `kubectl` reduzida ao que importa.
///
/// Ele escreve avisos de plugin e de versão obsoleta na mesma saída de erro em que põe o
/// motivo da falha, e o motivo é a última linha. As linhas de aviso sozinhas não são
/// falha nenhuma.
fn limpar(queixa: &str) -> String {
    let util = queixa
        .lines()
        .map(str::trim)
        .rfind(|linha| {
            !linha.is_empty() && !linha.starts_with("W0") && !linha.starts_with("Warning:")
        })
        .unwrap_or("");
    match util {
        "" => "o kubectl falhou sem dizer por quê".to_string(),
        texto => texto.trim_start_matches("error: ").to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A promessa do módulo, conferida comando a comando: nada em `CONSULTAS` escreve.
    #[test]
    fn toda_consulta_e_de_leitura() {
        for consulta in CONSULTAS {
            let args: Vec<String> = consulta.args.iter().map(|a| a.to_string()).collect();
            assert!(
                permitido(&args),
                "a consulta «{}» não é de leitura: {:?}",
                consulta.nome,
                consulta.args
            );
        }
    }

    /// E o contrário: o que muda o cluster é recusado antes de virar processo.
    #[test]
    fn o_que_escreve_e_recusado() {
        for comando in [
            vec!["apply", "-f", "x.yaml"],
            vec!["create", "ns", "x"],
            vec!["delete", "pod", "x"],
            vec!["patch", "deploy", "x"],
            vec!["edit", "deploy", "x"],
            vec!["scale", "--replicas=0", "deploy/x"],
            vec!["drain", "no1"],
            vec!["cordon", "no1"],
            vec!["uncordon", "no1"],
            vec!["rollout", "restart", "deploy/x"],
            vec!["exec", "pod", "--", "sh"],
            vec!["port-forward", "pod", "8080"],
            vec!["cp", "a", "b"],
            vec!["label", "node", "x=y"],
            vec!["annotate", "pod", "x=y"],
            vec!["taint", "node", "x=y:NoSchedule"],
            vec!["replace", "-f", "x.yaml"],
            vec!["run", "x", "--image=nginx"],
            vec!["debug", "pod"],
            vec!["attach", "pod"],
            // Os dois que se disfarçam de leitura pelo primeiro argumento.
            vec!["auth", "reconcile", "-f", "x.yaml"],
            vec!["config", "set-context", "x"],
            vec!["cluster-info", "dump"],
        ] {
            let args: Vec<String> = comando.iter().map(|a| a.to_string()).collect();
            assert!(!permitido(&args), "deixou passar: {comando:?}");
        }
    }

    #[test]
    fn nenhuma_consulta_repete_o_nome() {
        let mut nomes: Vec<&str> = CONSULTAS.iter().map(|c| c.nome).collect();
        nomes.sort_unstable();
        let antes = nomes.len();
        nomes.dedup();
        assert_eq!(antes, nomes.len(), "duas consultas com o mesmo nome");
    }

    #[test]
    fn a_profundidade_filtra() {
        let raso = consultas(Depth::Raso).count();
        let medio = consultas(Depth::Medio).count();
        let profundo = consultas(Depth::Profundo).count();
        assert!(raso > 0 && raso < medio && medio < profundo);
    }

    #[test]
    fn distingue_um_caminho_de_um_arquivo_colado() {
        assert!(!parece_kubeconfig("~/.kube/config"));
        assert!(!parece_kubeconfig("/home/alguem/.kube/config-producao"));
        assert!(parece_kubeconfig(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: x\n"
        ));
    }

    #[test]
    fn a_reclamacao_vira_uma_linha_so() {
        let queixa = "W0916 10:00:00.000 warning: plugin obsoleto\nerror: You must be logged in to the server (Unauthorized)\n";
        assert_eq!(
            limpar(queixa),
            "You must be logged in to the server (Unauthorized)"
        );
    }
}
