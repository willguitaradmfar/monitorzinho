//! A tabela da lista de módulos da aba Invest.
//!
//! É uma `TableMonitor` comum, e isso não é detalhe de implementação: fazê-la ser uma
//! tabela ganha **de graça e sem uma linha de código de interface nova** as cinco coisas
//! que a lista precisa ter — a moldura e o badge do painel, o atalho por número, a tela
//! cheia, a busca digitada direto, e o `Enter`. Uma tela própria significaria
//! reimplementar as cinco, e significaria que elas se comportariam *quase* como no resto
//! do programa.
//!
//! Ela não amostra nada por conta própria. Quem monta as linhas é a `App`, que é quem tem
//! o `InvestState` — ver `TableMonitor::feed`.

use std::sync::{Arc, Mutex};

use crate::app::Tab;
use crate::monitor::{SystemState, TableMonitor, TableRow};

/// O identificador da tabela. Estável para sempre: o `App::open_row` reconhece a lista
/// por ele para saber que o `Enter` abre um módulo em vez de um detalhe.
pub const ID: &str = "invest-modulos";

pub struct ModulesMonitor {
    linhas: Arc<Mutex<Vec<TableRow>>>,
}

impl Default for ModulesMonitor {
    fn default() -> Self {
        Self {
            linhas: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

impl TableMonitor for ModulesMonitor {
    fn id(&self) -> &'static str {
        ID
    }

    fn title(&self) -> &'static str {
        "Módulos"
    }

    fn tab(&self) -> Tab {
        Tab::Invest
    }

    fn headers(&self) -> &'static [&'static str] {
        &["Módulo", "Resumo", "Assunto"]
    }

    /// O painel compacto mostra o nome e o que o módulo diz de si. O grupo é o que menos
    /// se lê de relance, e é a coluna que cai primeiro.
    ///
    /// A ordem das células importa: o painel compacto pega as **primeiras** colunas, então
    /// o resumo tem que vir antes do grupo mesmo na tela cheia.
    fn compact_headers(&self) -> &'static [&'static str] {
        &["Módulo", "Resumo"]
    }

    /// Sem I/O e sem cálculo: as linhas já vieram prontas da `App`. É o que mantém a
    /// promessa de que amostrar esta aba custa uma cópia de vetor.
    fn sample(&mut self, _state: &SystemState, limit: Option<usize>) -> Vec<TableRow> {
        let linhas = match self.linhas.lock() {
            Ok(l) => l.clone(),
            Err(envenenado) => envenenado.into_inner().clone(),
        };
        match limit {
            Some(n) => linhas.into_iter().take(n).collect(),
            None => linhas,
        }
    }

    fn feed(&self) -> Option<Arc<Mutex<Vec<TableRow>>>> {
        Some(Arc::clone(&self.linhas))
    }

    /// Marcar um módulo não quer dizer nada — favoritar é outra coisa, e ainda não existe.
    fn mark_kinds(&self) -> &'static [crate::monitor::mark::MarkKind] {
        &[]
    }

    fn note(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_linhas_vem_de_fora_e_o_limite_e_respeitado() {
        let m = ModulesMonitor::default();
        let canal = m.feed().expect("a lista tem que ter canal");
        *canal.lock().unwrap() = (0..5)
            .map(|i| TableRow::leaf(vec![format!("m{i}"), String::new()], 0))
            .collect();

        let mut m = m;
        let state = SystemState::new();
        assert_eq!(m.sample(&state, None).len(), 5);
        assert_eq!(m.sample(&state, Some(2)).len(), 2);
    }

    #[test]
    fn mora_na_aba_invest() {
        assert!(ModulesMonitor::default().tab() == Tab::Invest);
    }
}
