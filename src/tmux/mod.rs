//! Sessões, janelas e painéis do tmux — o vocabulário, e o que dá para fazer com eles.
//!
//! ## Por que aqui existe um `Command`, e em nenhum outro lugar do programa
//!
//! Não há um `std::process::Command` em nenhuma outra parte deste código, e isso é
//! deliberado: `container/exec.rs` recusa o `docker exec` em voz alta, porque chamá-lo
//! traria a exigência de que o binário esteja instalado e de que a versão dele concorde
//! com a nossa. Aqui a conta é outra, por três razões:
//!
//! * O tmux não tem biblioteca nem protocolo de fio estável. A interface de controle
//!   **é** o binário — não existe a alternativa que existia com o Docker.
//! * O binário instalado **é o assunto da aba**. Sem tmux não há sessão, não há o que
//!   mostrar, e a aba não entra na barra — exatamente como a de Containers. A dependência
//!   não é incidental, é o que a aba trata.
//! * O formato da resposta é nosso, não dele: `-F '#{session_name}…'` é a linguagem de
//!   formatação documentada do tmux, e o que volta tem os campos que pedimos, na ordem
//!   que pedimos. Não é raspar a tela de um comando; é pedir um registro.
//!
//! ## Uma chamada por volta
//!
//! `list-panes -a` responde sobre painéis, mas os formatos do tmux dão acesso aos campos
//! da janela e da sessão de cada painel na mesma linha. Como toda sessão tem ao menos uma
//! janela e toda janela ao menos um painel, uma chamada só reconstrói a árvore inteira —
//! ver `cli::snapshot`. É por isso que esta aba não precisa das threads de fundo que a de
//! Containers tem: falar com um socket local custa ~4 ms e não trava, enquanto falar HTTP
//! com um daemon pode travar por segundos.

pub mod cli;

use std::path::PathBuf;

use crate::action::{Action, ActionKey, Gravity};

/// Uma sessão.
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub name: String,
    /// O id que o tmux dá (`$3`) — estável enquanto a sessão vive, ao contrário do nome,
    /// que se renomeia. É por ele que uma linha se reencontra entre uma volta e outra.
    pub id: String,
    /// Criação, em segundos desde a época.
    pub created: i64,
    /// Quantos clientes estão anexados agora. Zero é uma sessão rodando sozinha, que é o
    /// estado normal de uma sessão de trabalho entre um dia e o outro.
    pub attached: usize,
    pub windows: usize,
    /// O diretório em que a sessão foi criada — o `-c` de quem a criou.
    pub path: String,
}

/// Uma janela dentro de uma sessão.
#[derive(Clone, Debug, Default)]
pub struct Window {
    pub session: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub panes: usize,
}

/// Um painel dentro de uma janela: onde de fato há um processo rodando.
#[derive(Clone, Debug, Default)]
pub struct Pane {
    pub session: String,
    pub window_index: u32,
    pub index: u32,
    /// O id do tmux (`%12`).
    pub id: String,
    pub active: bool,
    /// O comando em primeiro plano agora — o que a pessoa quer ver para reconhecer a
    /// janela sem entrar nela.
    pub command: String,
    pub path: String,
    pub pid: u32,
    pub width: u16,
    pub height: u16,
}

/// A árvore inteira, como a última chamada a viu.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub sessions: Vec<Session>,
    pub windows: Vec<Window>,
    pub panes: Vec<Pane>,
}

impl Snapshot {
    pub fn windows_of(&self, session: &str) -> impl Iterator<Item = &Window> {
        self.windows.iter().filter(move |w| w.session == session)
    }

    pub fn panes_of(&self, session: &str, window: u32) -> impl Iterator<Item = &Pane> {
        self.panes
            .iter()
            .filter(move |p| p.session == session && p.window_index == window)
    }

    pub fn session_named(&self, name: &str) -> Option<&Session> {
        self.sessions.iter().find(|s| s.name == name)
    }

    /// Quantos painéis existem ao todo — que é a contagem que diz quantos processos o
    /// servidor está segurando.
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }
}

/// O tmux desta máquina: onde ele está, o que ele respondeu, e onde nós estamos dentro
/// dele.
pub struct Tmux {
    /// O caminho absoluto do binário, resolvido uma vez no arranque. Guardado resolvido
    /// para que uma mudança no `PATH` no meio da execução não troque o programa embaixo
    /// de nós.
    bin: PathBuf,
    pub version: String,
    /// O socket do servidor, quando sabemos qual é — o do `$TMUX` de quem nos abriu.
    /// `None` deixa o tmux escolher o padrão dele, que é o caso de quem roda de fora.
    socket: Option<String>,
    /// A sessão em que o monitorzinho está rodando, quando está dentro de uma.
    ///
    /// É o que decide a metade difícil desta aba. O tmux recusa anexar quando o terminal
    /// do cliente é o de um painel do próprio servidor — a mensagem é «sessions should be
    /// nested with care, unset $TMUX to force» —, então de dentro do tmux «entrar» não
    /// pode ser um `attach` simples. Ver `actions`.
    pub inside: Option<String>,
    /// O painel em que o monitorzinho está desenhando, quando está dentro do tmux.
    ///
    /// Vem do `$TMUX_PANE`, que é o próprio tmux quem exporta para o que roda ali dentro.
    /// A sessão sozinha não bastava assim que passou a haver o que matar em cada nível:
    /// bloquear a sessão inteira porque estamos nela deixaria matar a janela e o painel
    /// que nos seguram, que é a mesma morte por um caminho mais fundo.
    pub inside_pane: Option<String>,
    pub snapshot: Snapshot,
    /// O que o tmux respondeu de errado na última leitura, se respondeu.
    pub error: Option<String>,
    /// O resultado da última operação: a frase, e se deu certo. Fica até a próxima.
    pub outcome: Option<(String, bool)>,
}

/// Sobre o que uma ação desta aba age.
#[derive(Clone, Debug)]
pub enum Subject {
    Session(Session),
    Window(Window),
    Pane(Pane),
}

impl Subject {
    pub fn name(&self) -> String {
        match self {
            Subject::Session(s) => s.name.clone(),
            Subject::Window(w) => format!("{}:{} {}", w.session, w.index, w.name),
            // O id do tmux e o que está rodando: `%12 (nvim)`. O id sozinho não diz nada
            // a quem vai confirmar que quer matá-lo, e o comando sozinho não é único.
            Subject::Pane(p) => match p.command.is_empty() {
                true => p.id.clone(),
                false => format!("{} ({})", p.id, p.command),
            },
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Subject::Session(_) => "sessão",
            Subject::Window(_) => "janela",
            Subject::Pane(_) => "painel",
        }
    }
}

/// O que o `Enter` faz com uma sessão quando o terminal precisa mudar de dono.
#[derive(Clone, Debug)]
pub struct Attach {
    pub session: String,
    /// Se é para forçar o aninhamento — apagar o `$TMUX` do filho, que é o que a própria
    /// mensagem do tmux manda fazer. Falso quando o monitorzinho roda fora do tmux, onde
    /// não há nada para aninhar.
    pub nested: bool,
}

/// O id do tmux por trás de um sujeito de sessão — a identidade que sobrevive a um
/// renomeio, que é exatamente quando o nome deixa de servir para reencontrar a linha.
fn session_id_of(subject: &Subject) -> String {
    match subject {
        Subject::Session(session) => session.id.clone(),
        Subject::Window(_) | Subject::Pane(_) => String::new(),
    }
}

fn action(key: ActionKey, label: &str, gravity: Gravity) -> Action {
    Action {
        key,
        label: label.to_string(),
        gravity,
        consequences: Vec::new(),
        blocked: None,
    }
}

impl Tmux {
    /// Acha o tmux e, achando ou não, decide se há aba.
    ///
    /// A busca é no `PATH` e é só filesystem — nenhum processo é criado para descobrir se
    /// existe um programa. O único `Command` do arranque é o `-V`, que é o que dá a
    /// versão que o resumo mostra.
    pub fn start() -> Option<Self> {
        let bin = cli::find_binary()?;
        let version = cli::version(&bin).unwrap_or_else(|_| "?".to_string());
        // `$TMUX` é `socket,pid,id-da-sessão`. O socket importa numa máquina com mais de
        // um servidor: falar com o padrão enquanto se vive em outro é responder sobre
        // sessões que não são as que estão na tela.
        let env = std::env::var("TMUX").ok();
        let mut socket = None;
        let mut inside_id = None;
        if let Some(value) = &env {
            let mut parts = value.split(',');
            socket = parts.next().map(str::to_string).filter(|s| !s.is_empty());
            let _pid = parts.next();
            inside_id = parts.next().map(|id| format!("${id}"));
        }
        let mut tmux = Self {
            bin,
            version,
            socket,
            inside: None,
            inside_pane: std::env::var("TMUX_PANE").ok().filter(|id| !id.is_empty()),
            snapshot: Snapshot::default(),
            error: None,
            outcome: None,
        };
        tmux.refresh();
        // O nome, e não o id, porque é por nome que toda ação daqui aponta para uma
        // sessão — e é o nome que a tabela mostra.
        tmux.inside = inside_id.and_then(|id| {
            tmux.snapshot
                .sessions
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.clone())
        });
        Some(tmux)
    }

    /// Relê a árvore. Uma chamada, síncrona, no tick da aba — ver o cabeçalho.
    pub fn refresh(&mut self) {
        match cli::snapshot(&self.bin, self.socket.as_deref()) {
            Ok(snapshot) => {
                // O nome pode ter sido trocado por fora enquanto olhávamos; reencontrar
                // pelo id mantém a aba sabendo onde estamos.
                if let Some(current) = &self.inside
                    && snapshot.session_named(current).is_none()
                {
                    self.inside = None;
                }
                self.snapshot = snapshot;
                self.error = None;
            }
            Err(error) => {
                self.snapshot = Snapshot::default();
                self.error = Some(error);
            }
        }
    }

    /// Se o monitorzinho está rodando dentro do tmux. É o que muda o que «entrar»
    /// significa.
    pub fn nested(&self) -> bool {
        self.inside.is_some()
    }

    pub fn socket_label(&self) -> String {
        self.socket.clone().unwrap_or_else(|| "padrão".to_string())
    }

    /// O que dá para fazer com este sujeito, na ordem em que o menu oferece.
    ///
    /// O caso que este método existe para tratar honestamente é o de dentro do tmux. Lá
    /// «entrar» tem duas respostas diferentes e nenhuma é obviamente a certa, então as
    /// duas aparecem, nomeadas pelo que de fato fazem — e a que não dá para fazer fica na
    /// lista, explicada.
    pub fn actions(&self, subject: &Subject) -> Vec<Action> {
        match subject {
            Subject::Session(session) => self.session_actions(session),
            Subject::Window(window) => self.window_actions(window),
            Subject::Pane(pane) => self.pane_actions(pane),
        }
    }

    fn session_actions(&self, session: &Session) -> Vec<Action> {
        let mut actions = Vec::new();
        let is_ours = self.inside.as_deref() == Some(session.name.as_str());
        if self.nested() {
            // Aninhar de propósito, que é o que a mensagem do próprio tmux manda fazer.
            // Desanexar com `prefix prefix d` devolve esta tela, que é o contrato da aba.
            actions.push(Action {
                blocked: is_ours.then(|| "é a sessão onde o monitorzinho está".to_string()),
                ..action(ActionKey::Attach, "entrar aqui (aninhada)", Gravity::Safe)
            });
            // Sem aninhar: o cliente de fora troca de sessão e o monitorzinho continua
            // rodando onde está. Voltar é trocar de volta, não desanexar.
            actions.push(Action {
                blocked: is_ours.then(|| "já é a sessão deste cliente".to_string()),
                ..action(
                    ActionKey::SwitchTo,
                    "trocar o tmux de fora para ela",
                    Gravity::Safe,
                )
            });
        } else {
            actions.push(action(ActionKey::Attach, "entrar", Gravity::Safe));
        }
        actions.push(action(ActionKey::Rename, "renomear", Gravity::Safe));
        actions.push(action(ActionKey::Details, "ver detalhes", Gravity::Safe));

        let panes = self
            .snapshot
            .windows_of(&session.name)
            .map(|w| w.panes)
            .sum::<usize>();
        let mut consequences = vec![
            format!(
                "{} janela(s) e {panes} painel(is) são fechados",
                session.windows
            ),
            "tudo que estiver rodando dentro deles morre junto".to_string(),
        ];
        if session.attached > 0 {
            consequences.push(format!(
                "{} cliente(s) anexado(s) agora são desconectados",
                session.attached
            ));
        }
        actions.push(Action {
            consequences,
            // Matar a sessão onde o monitorzinho vive mata o monitorzinho. Fica na lista,
            // dizendo isso, em vez de sumir.
            blocked: is_ours.then(|| "matá-la mataria o monitorzinho".to_string()),
            ..action(ActionKey::KillSession, "matar sessão", Gravity::Confirm)
        });
        actions
    }

    fn window_actions(&self, window: &Window) -> Vec<Action> {
        let last = self.snapshot.windows_of(&window.session).count() <= 1;
        let mut consequences = vec![format!(
            "{} painel(is) são fechados e o que roda neles morre",
            window.panes
        )];
        if last {
            consequences.push(format!(
                "é a última janela de «{}» — a sessão acaba junto",
                window.session
            ));
        }
        vec![
            action(ActionKey::Rename, "renomear", Gravity::Safe),
            action(ActionKey::Details, "ver detalhes", Gravity::Safe),
            Action {
                consequences,
                // Não «é a janela da nossa sessão»: uma sessão pode ter várias janelas e
                // matar as outras é legítimo. O que não pode é matar a que nos segura.
                blocked: self
                    .holds_us(&window.session, Some(window.index))
                    .then(|| "é a janela onde o monitorzinho está".to_string()),
                ..action(ActionKey::KillWindow, "matar janela", Gravity::Confirm)
            },
        ]
    }

    fn pane_actions(&self, pane: &Pane) -> Vec<Action> {
        let siblings = self
            .snapshot
            .panes_of(&pane.session, pane.window_index)
            .count();
        let last_pane = siblings <= 1;
        let last_window = last_pane && self.snapshot.windows_of(&pane.session).count() <= 1;

        let mut consequences = vec![match pane.command.is_empty() {
            true => "o que estiver rodando no painel morre".to_string(),
            // O comando pelo nome: é o que a pessoa reconhece, e é o que ela perde.
            false => format!("«{}» (pid {}) morre", pane.command, pane.pid),
        }];
        if last_pane {
            consequences.push("é o último painel da janela — a janela fecha junto".to_string());
        }
        if last_window {
            consequences.push(format!(
                "e era a última janela de «{}» — a sessão acaba junto",
                pane.session
            ));
        }

        vec![
            action(ActionKey::Details, "ver detalhes", Gravity::Safe),
            Action {
                consequences,
                blocked: (self.inside_pane.as_deref() == Some(pane.id.as_str()))
                    .then(|| "é o painel onde o monitorzinho está desenhando".to_string()),
                ..action(ActionKey::KillPane, "matar painel", Gravity::Confirm)
            },
        ]
    }

    /// Se este ponto da árvore é o que segura o monitorzinho.
    ///
    /// Com `window` em `None` a pergunta é sobre a sessão inteira. Responder pelo painel
    /// e não pelo nome da sessão é o que deixa matar as outras janelas da sessão em que
    /// estamos — o que é legítimo — e recusar só a que nos segura.
    fn holds_us(&self, session: &str, window: Option<u32>) -> bool {
        if self.inside.as_deref() != Some(session) {
            return false;
        }
        let Some(window) = window else {
            return true;
        };
        let Some(our_pane) = &self.inside_pane else {
            // Sabemos a sessão mas não o painel: sem `$TMUX_PANE` não dá para distinguir
            // as janelas, e recusar todas é o único lado errado seguro.
            return true;
        };
        self.snapshot
            .panes_of(session, window)
            .any(|pane| pane.id == *our_pane)
    }

    /// Executa. A mensagem que volta é a do tmux, sem tradução: quem sabe por que falhou
    /// é ele.
    pub fn perform(&mut self, key: ActionKey, subject: &Subject) -> Result<String, String> {
        let result = match (key, subject) {
            (ActionKey::SwitchTo, Subject::Session(session)) => self
                .run(&["switch-client", "-t", &session.name])
                .map(|_| format!("tmux de fora agora está em «{}»", session.name)),
            (ActionKey::KillSession, Subject::Session(session)) => self
                .run(&["kill-session", "-t", &session.name])
                .map(|_| format!("sessão «{}» morta", session.name)),
            (ActionKey::KillWindow, Subject::Window(window)) => {
                let target = format!("{}:{}", window.session, window.index);
                self.run(&["kill-window", "-t", &target])
                    .map(|_| format!("janela «{target}» morta"))
            }
            // Pelo id do tmux (`%12`) e não pela posição: matar um painel renumera os
            // que sobram, e um alvo posicional guardado antes disso aponta para outro.
            (ActionKey::KillPane, Subject::Pane(pane)) => self
                .run(&["kill-pane", "-t", &pane.id])
                .map(|_| format!("painel «{}» morto", pane.id)),
            _ => Err("operação não existe para este sujeito".to_string()),
        };
        // Relido na hora: uma sessão morta tem que sumir da tabela agora, e não no
        // próximo tick — apertar uma tecla e ver a linha continuar lá é indistinguível de
        // a tecla não ter funcionado.
        self.refresh();
        self.outcome = Some(match &result {
            Ok(message) => (message.clone(), true),
            Err(error) => (error.clone(), false),
        });
        result
    }

    /// Troca o nome de uma sessão ou de uma janela.
    ///
    /// Devolve **o nome que o tmux deu**, pela mesma razão que a criação: ele reescreve
    /// `.` e `:` para `_` sem avisar, e a linha de resultado é o único lugar onde quem
    /// renomeou fica sabendo disso.
    pub fn rename(&mut self, subject: &Subject, novo: &str) -> Result<String, String> {
        let novo = novo.trim();
        if novo.is_empty() {
            return Err("o nome não pode ficar vazio".to_string());
        }
        let result = match subject {
            Subject::Session(session) => self
                .run(&["rename-session", "-t", &session.name, novo])
                .map(|_| session.name.clone()),
            Subject::Window(window) => {
                let target = format!("{}:{}", window.session, window.index);
                self.run(&["rename-window", "-t", &target, novo])
                    .map(|_| target)
            }
            Subject::Pane(_) => Err("um painel não tem nome para trocar".to_string()),
        };
        self.refresh();
        let antes = result?;
        // O que o tmux de fato registrou, lido de volta em vez de suposto.
        let agora = match subject {
            Subject::Session(_) => self
                .snapshot
                .sessions
                .iter()
                .find(|s| s.id == session_id_of(subject))
                .map(|s| s.name.clone()),
            Subject::Window(window) => self
                .snapshot
                .windows_of(&window.session)
                .find(|w| w.index == window.index)
                .map(|w| w.name.clone()),
            Subject::Pane(_) => None,
        }
        .unwrap_or_else(|| novo.to_string());
        self.outcome = Some(if agora == novo {
            (format!("«{antes}» agora se chama «{agora}»"), true)
        } else {
            (
                format!("«{novo}» virou «{agora}» — o tmux não aceita «.» nem «:»"),
                true,
            )
        });
        Ok(agora)
    }

    /// Cria uma sessão destacada e devolve **o nome que o tmux deu a ela**.
    ///
    /// O nome de volta não é frescura: o tmux reescreve `.` e `:` para `_` sem avisar e
    /// sai com zero, de modo que quem criou `deploy.web` fica com `deploy_web` e não é
    /// informado. Anexar depois pelo nome digitado erraria a sessão.
    ///
    /// O diretório é conferido aqui e não pelo tmux, que aceita um caminho inexistente,
    /// cria a sessão em outro lugar e também sai com zero.
    pub fn create(&mut self, name: &str, path: &str) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("a sessão precisa de um nome".to_string());
        }
        let path = path.trim();
        if path.is_empty() {
            return Err("a sessão precisa de uma pasta base".to_string());
        }
        let expanded = expand_home(path);
        match std::fs::metadata(&expanded) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Err(format!("«{path}» não é uma pasta")),
            Err(error) => return Err(format!("«{path}»: {error}")),
        }
        let created = self.run(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-c",
            &expanded,
            "-P",
            "-F",
            "#{session_name}",
        ])?;
        let created = created.trim().to_string();
        self.refresh();
        if created != name {
            // O tmux trocou o nome. Dizer isso é melhor que deixar a pessoa procurar na
            // lista por um nome que não está lá.
            self.outcome = Some((
                format!("«{name}» virou «{created}» — o tmux não aceita «.» nem «:»"),
                true,
            ));
        } else {
            self.outcome = Some((format!("sessão «{created}» criada em {expanded}"), true));
        }
        Ok(created)
    }

    /// Entrega o terminal a uma sessão e só volta quando alguém a desanexar.
    ///
    /// Chamado pelo laço principal, com a tela alternativa já abandonada — daqui até a
    /// volta o terminal é do tmux. Não há relay nenhum, ao contrário do shell de
    /// container: o tmux é um processo filho que herda o terminal de verdade, então ele
    /// mesmo recebe o `SIGWINCH` do redimensionamento e ele mesmo mexe no termios.
    pub fn attach(&mut self, request: &Attach) -> Result<String, String> {
        let status = cli::attach(
            &self.bin,
            self.socket.as_deref(),
            &request.session,
            request.nested,
        )?;
        self.refresh();
        if status {
            Ok(request.session.clone())
        } else {
            // Sair diferente de zero aqui quase sempre é o tmux recusando por um motivo
            // que ele já imprimiu na tela que acabou de ser entregue. Como aquela tela já
            // sumiu, a frase precisa vir de volta com a gente.
            Err(format!(
                "o tmux recusou entrar em «{}» — a sessão pode ter acabado, \
                 ou o terminal não pôde ser aninhado",
                request.session
            ))
        }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        cli::run(&self.bin, self.socket.as_deref(), args)
    }
}

/// `~` e `~/x` viram o diretório do usuário. É o que uma pessoa digita numa caixa que
/// pede uma pasta, e o tmux não expande — quem expande é o shell, que não está aqui.
pub fn expand_home(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    match path {
        "~" => home.to_string_lossy().into_owned(),
        _ => match path.strip_prefix("~/") {
            Some(rest) => home.join(rest).to_string_lossy().into_owned(),
            None => path.to_string(),
        },
    }
}
