//! O que dá para fazer com uma linha, e quanto atrito isso exige — sem saber de quem.
//!
//! Isto morava dentro de `container/`, porque containers foram a primeira coisa deste
//! programa sobre a qual havia o que *fazer*. Saiu de lá quando a segunda apareceu: uma
//! sessão de tmux também é uma linha com operações, e o menu do `Enter` não pode
//! pertencer a um dos dois assuntos. `container` continua reexportando estes tipos, de
//! modo que nada do lado da engine mudou de nome ao se mudarem de arquivo.
//!
//! A regra que os três tipos aqui existem para sustentar é a mesma de sempre: **a UI não
//! conhece nenhuma operação pelo nome**. Ela desenha o que `actions()` devolver, e quem
//! não sabe fazer alguma coisa simplesmente não devolve a entrada.

use crate::container;
use crate::tmux;

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

/// O que se pode fazer com um sujeito.
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

/// As operações que existem, de todos os assuntos que têm operações.
///
/// Um enum só, e não um por assunto: `ActionMenu` guarda uma lista de `Action` e
/// `run_chosen_action` decide o que fazer num lugar só. Dois enums exigiriam dois menus
/// e dois despachos que fariam a mesma coisa com nomes diferentes — que é exatamente o
/// que este arquivo existe para não deixar acontecer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKey {
    // --- container ---
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
    // --- volume ---
    RemoveVolume,
    PruneVolumes,
    // --- imagem ---
    RemoveImage,
    ForceRemoveImage,
    PruneImages,
    // --- rede ---
    RemoveNetwork,
    PruneNetworks,
    // --- tmux ---
    /// Entregar o terminal a esta sessão até que alguém a desanexe.
    Attach,
    /// Mandar o tmux de fora trocar de sessão, sem aninhar nada — ver `tmux::attach`.
    SwitchTo,
    /// Trocar o nome de uma sessão ou de uma janela.
    Rename,
    /// Matar a sessão inteira, com tudo que está rodando dentro.
    KillSession,
    /// Matar uma janela só.
    KillWindow,
    /// Matar um painel só — o nível mais fundo em que ainda há o que matar.
    KillPane,
}

/// Sobre o que uma ação age.
///
/// Os dois assuntos que têm menu. Cada um guarda o seu próprio sujeito inteiro: o menu
/// foi montado sobre uma linha, e a linha pode ter sumido quando a tecla for apertada.
#[derive(Clone, Debug)]
pub enum Target {
    Container(container::Subject),
    Tmux(tmux::Subject),
}

impl Target {
    /// Como o sujeito se chama numa caixa de confirmação.
    pub fn name(&self) -> String {
        match self {
            Target::Container(subject) => subject.name(),
            Target::Tmux(subject) => subject.name(),
        }
    }

    /// O substantivo, para um título que precisa dizer de que tipo de coisa se trata.
    pub fn kind(&self) -> &'static str {
        match self {
            Target::Container(subject) => subject.kind(),
            Target::Tmux(subject) => subject.kind(),
        }
    }

    /// Por que o menu pode estar vazio, dito no vocabulário de quem está vazio. Um menu
    /// de sessão que se explicasse falando de engine seria uma resposta sobre outra
    /// coisa.
    pub fn empty_reason(&self) -> &'static str {
        match self {
            Target::Container(_) => {
                "Nenhuma operação disponível — nenhuma engine respondeu nesta máquina."
            }
            Target::Tmux(_) => "Nenhuma operação disponível para esta linha.",
        }
    }
}
