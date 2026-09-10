//! O que quase toda tela de módulo repete: uma lista com cursor e busca, e um formulário.
//!
//! Existe para que vinte módulos não tenham vinte jeitos ligeiramente diferentes de andar
//! numa lista. As teclas aqui são as mesmas das tabelas em tela cheia do resto do
//! programa, pelo mesmo motivo de sempre: o mesmo gesto tem que ser a mesma tecla.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::invest::module::{Escape, Field, Row};

/// Quantas linhas um `PgUp`/`PgDn` anda. O mesmo `PAGE_ROWS` do resto do programa: uma
/// página que significa a mesma distância em toda tela é mais fácil de ganhar intuição
/// do que uma que muda com o tamanho do painel.
pub const PAGE: i32 = 10;

/// Cursor e busca sobre uma lista de linhas.
#[derive(Default)]
pub struct Lista {
    pub selecionado: usize,
    /// Busca digitada direto, sem modo a entrar antes — como nas tabelas.
    pub busca: String,
}

impl Lista {
    /// Os índices que a busca deixa passar. Ao contrário das tabelas do monitorzinho, que
    /// só movem o cursor entre acertos, aqui a busca **filtra**: uma lista de módulos ou
    /// de posições é curta, e ver só o que casa é o que se espera dela.
    pub fn visiveis(&self, linhas: &[Row]) -> Vec<usize> {
        let needle = self.busca.to_lowercase();
        linhas
            .iter()
            .enumerate()
            .filter(|(_, r)| r.matches(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    /// O índice **na lista original** da linha sob o cursor.
    ///
    /// Isto não é conveniência: com uma busca ativa, o cursor anda pela lista *filtrada*,
    /// e usar o número dele para indexar a lista original aponta para outra linha. É o
    /// tipo de erro que não aparece sem busca e apaga a coisa errada com busca — então
    /// todo módulo passa por aqui, e nenhum indexa os próprios dados com `selecionado`.
    pub fn atual(&self, linhas: &[Row]) -> Option<usize> {
        self.visiveis(linhas).get(self.selecionado).copied()
    }

    pub fn mover(&mut self, delta: i32, total: usize) {
        if total == 0 {
            self.selecionado = 0;
            return;
        }
        let n = total as i32;
        // Um passo dá a volta; uma página não — um gesto rápido que teleporta do fim para
        // o começo é um gesto rápido que perde o lugar.
        self.selecionado = match delta.abs() {
            1 => (self.selecionado as i32 + delta).rem_euclid(n) as usize,
            _ => (self.selecionado as i32 + delta).clamp(0, n - 1) as usize,
        };
    }

    /// Trata as teclas de navegação e de busca. Devolve `true` quando consumiu.
    pub fn tecla(&mut self, key: KeyEvent, total: usize) -> bool {
        match key.code {
            KeyCode::Up => self.mover(-1, total),
            KeyCode::Down => self.mover(1, total),
            KeyCode::PageUp => self.mover(-PAGE, total),
            KeyCode::PageDown => self.mover(PAGE, total),
            KeyCode::Home => self.selecionado = 0,
            KeyCode::End => self.selecionado = total.saturating_sub(1),
            KeyCode::Backspace => {
                self.busca.pop();
                self.selecionado = 0;
            }
            // Com `Ctrl` a letra é um atalho do módulo, não busca — mesma regra do
            // `Ctrl+E` que marca e do `Ctrl+N` que cria sessão de tmux.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.busca.push(c);
                self.selecionado = 0;
            }
            _ => return false,
        }
        true
    }

    /// `Esc` limpa a busca antes de qualquer outra coisa.
    pub fn escape(&mut self) -> Escape {
        match self.busca.is_empty() {
            true => Escape::Nao,
            false => {
                self.busca.clear();
                self.selecionado = 0;
                Escape::Consumido
            }
        }
    }
}

/// Um formulário: campos com ↑/↓, `Enter` grava, `Esc` fecha.
///
/// A validação acontece **com a caixa ainda aberta**, que é a regra de `Tool::start` e do
/// `SessionEditor`: um formulário que aceita e falha depois é um formulário que perde o
/// que foi digitado.
pub struct Formulario {
    pub titulo: String,
    pub campos: Vec<Field>,
    pub selecionado: usize,
    pub erro: Option<String>,
}

impl Formulario {
    pub fn novo(titulo: impl Into<String>, campos: Vec<Field>) -> Self {
        Self {
            titulo: titulo.into(),
            campos,
            selecionado: 0,
            erro: None,
        }
    }

    pub fn valor(&self, label: &str) -> String {
        self.campos
            .iter()
            .find(|c| c.label == label)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    fn atual(&mut self) -> Option<&mut Field> {
        self.campos.get_mut(self.selecionado)
    }

    /// Anda pelas opções de um campo de escolha. Num campo de texto, ←/→ não fazem nada —
    /// e não fazer nada é melhor que fazer outra coisa.
    fn ciclar(&mut self, delta: i32) {
        let Some(campo) = self.atual() else { return };
        if campo.options.is_empty() {
            return;
        }
        let n = campo.options.len() as i32;
        let atual = campo
            .options
            .iter()
            .position(|o| *o == campo.value)
            .unwrap_or(0) as i32;
        campo.value = campo.options[(atual + delta).rem_euclid(n) as usize].clone();
    }

    /// Trata a tecla. `Enter` devolve `true`, que é o sinal de «tente gravar».
    pub fn tecla(&mut self, key: KeyEvent) -> bool {
        let n = self.campos.len();
        match key.code {
            KeyCode::Up => {
                if n > 0 {
                    self.selecionado = (self.selecionado + n - 1) % n;
                }
            }
            KeyCode::Down => {
                if n > 0 {
                    self.selecionado = (self.selecionado + 1) % n;
                }
            }
            KeyCode::Left => self.ciclar(-1),
            KeyCode::Right => self.ciclar(1),
            KeyCode::Backspace => {
                if let Some(campo) = self.atual()
                    && campo.options.is_empty()
                {
                    campo.value.pop();
                }
            }
            KeyCode::Char(c) => {
                if let Some(campo) = self.atual()
                    && campo.options.is_empty()
                {
                    campo.value.push(c);
                }
            }
            KeyCode::Enter => return true,
            _ => {}
        }
        // Digitar depois de um erro apaga o erro: ele descreve o que estava lá antes.
        if !matches!(key.code, KeyCode::Up | KeyCode::Down | KeyCode::Enter) {
            self.erro = None;
        }
        false
    }
}

/// A linha de rodapé de um módulo, montada de pedaços. Existe para que a ordem e o
/// separador sejam os mesmos em toda tela.
pub fn hint(partes: &[&str]) -> String {
    partes.join(" · ")
}

/// O que escrever no lugar de um preço que ainda não chegou.
///
/// «buscando…» só quando alguém está de fato buscando. Na home nenhum módulo está aberto
/// e a thread está parada — ali a resposta honesta é um traço, porque o número não está a
/// caminho: ninguém foi atrás dele. Escrever «buscando…» naquela tela era prometer uma
/// busca eterna que nunca começou.
pub fn sem_preco(ctx: &crate::invest::module::Ctx) -> String {
    match ctx.providers.buscando() {
        true => "buscando…".into(),
        false => "—".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn linhas() -> Vec<Row> {
        vec![
            Row::new(vec!["PETR4".into(), "ação".into()]),
            Row::new(vec!["VALE3".into(), "ação".into()]),
            Row::new(vec!["HGLG11".into(), "FII".into()]),
        ]
    }

    fn tecla(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    #[test]
    fn busca_filtra_e_recoloca_o_cursor_no_topo() {
        let mut l = Lista {
            selecionado: 2,
            ..Default::default()
        };
        l.tecla(tecla(KeyCode::Char('f')), 3);
        assert_eq!(l.busca, "f");
        assert_eq!(
            l.selecionado, 0,
            "editar a busca volta para o primeiro acerto"
        );
        let v = l.visiveis(&linhas());
        assert_eq!(v, vec![2], "só o FII casa com «f»");
    }

    #[test]
    fn ctrl_mais_letra_nao_e_busca() {
        // Numa tela com busca digitada direto, a ação tem que funcionar *enquanto* se
        // procura — por isso ela é Ctrl, e a Lista não pode engolir a tecla.
        let mut l = Lista::default();
        let consumiu = l.tecla(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL), 3);
        assert!(!consumiu);
        assert!(l.busca.is_empty());
    }

    #[test]
    fn passo_da_a_volta_e_pagina_nao() {
        let mut l = Lista::default();
        l.mover(-1, 3);
        assert_eq!(
            l.selecionado, 2,
            "um passo para trás no topo vai para o fim"
        );
        l.mover(1, 3);
        assert_eq!(l.selecionado, 0);
        l.mover(PAGE, 3);
        assert_eq!(l.selecionado, 2, "uma página para no fim, sem teleportar");
        l.mover(-PAGE, 3);
        assert_eq!(l.selecionado, 0);
    }

    #[test]
    fn o_cursor_filtrado_aponta_para_a_linha_certa_da_lista_original() {
        // Com «f» digitado, só o FII (índice 2) casa. O cursor está em 0 — e 0 na lista
        // filtrada é 2 na original. Indexar os dados com `selecionado` apagaria PETR4.
        let linhas = linhas();
        let mut l = Lista::default();
        l.tecla(tecla(KeyCode::Char('f')), 3);
        assert_eq!(l.selecionado, 0);
        assert_eq!(l.atual(&linhas), Some(2));

        // Sem busca, os dois coincidem — que é por que o erro passa despercebido.
        let mut l = Lista::default();
        l.mover(1, 3);
        assert_eq!(l.atual(&linhas), Some(1));
    }

    #[test]
    fn cursor_sem_acerto_nenhum_nao_aponta_para_nada() {
        // O caso que fazia o Enter navegar para a linha errada: busca sem acerto tem que
        // dar `None`, e não o primeiro item da lista original.
        let linhas = linhas();
        let mut l = Lista::default();
        for c in "zzz".chars() {
            l.tecla(tecla(KeyCode::Char(c)), 3);
        }
        assert!(l.visiveis(&linhas).is_empty());
        assert_eq!(l.atual(&linhas), None);
    }

    #[test]
    fn escape_limpa_a_busca_antes_de_qualquer_coisa() {
        let mut l = Lista {
            busca: "petr".into(),
            ..Default::default()
        };
        assert_eq!(l.escape(), Escape::Consumido);
        assert!(l.busca.is_empty());
        // Sem busca, o Esc não é dele — e é isso que faz o app perguntar se sai.
        assert_eq!(l.escape(), Escape::Nao);
    }

    #[test]
    fn lista_vazia_nao_estoura() {
        let mut l = Lista::default();
        l.mover(1, 0);
        assert_eq!(l.selecionado, 0);
        assert_eq!(l.atual(&[]), None);
    }

    #[test]
    fn campo_de_escolha_cicla_e_campo_de_texto_digita() {
        let mut f = Formulario::novo(
            "t",
            vec![
                Field::text("nome", "", "ajuda"),
                Field::choice("classe", "acao", vec!["acao".into(), "fii".into()], "ajuda"),
            ],
        );
        f.tecla(tecla(KeyCode::Char('x')));
        assert_eq!(f.campos[0].value, "x");
        f.tecla(tecla(KeyCode::Down));
        f.tecla(tecla(KeyCode::Right));
        assert_eq!(f.campos[1].value, "fii", "←/→ andam nas opções");
        // Digitar num campo de escolha não faz nada — ele não é para ser digitado.
        f.tecla(tecla(KeyCode::Char('z')));
        assert_eq!(f.campos[1].value, "fii");
    }

    #[test]
    fn digitar_apaga_o_erro_anterior() {
        let mut f = Formulario::novo("t", vec![Field::text("nome", "", "")]);
        f.erro = Some("quantidade tem que ser maior que zero".into());
        f.tecla(tecla(KeyCode::Char('1')));
        assert!(f.erro.is_none(), "o erro descrevia o que estava lá antes");
    }
}

/// Lê `08/09/2026`, ou hoje quando vazio.
///
/// Mora aqui e não no módulo que a usa porque **dois** a usam, e porque «dd/mm/aaaa»
/// precisa significar a mesma coisa em toda tela. Veio de Lançamentos, que saiu.
pub fn ler_data(texto: &str, agora: u64) -> Result<u64, String> {
    use crate::invest::tempo::{self, Data};
    let texto = texto.trim();
    if texto.is_empty() {
        return Ok(agora);
    }
    let partes: Vec<&str> = texto.split('/').collect();
    if partes.len() != 3 {
        return Err("data no formato dd/mm/aaaa".into());
    }
    let (Ok(dia), Ok(mes), Ok(ano)) = (
        partes[0].parse::<u32>(),
        partes[1].parse::<u32>(),
        partes[2].parse::<i32>(),
    ) else {
        return Err("data no formato dd/mm/aaaa".into());
    };
    if !(1..=31).contains(&dia) || !(1..=12).contains(&mes) {
        return Err("dia ou mês fora da faixa".into());
    }
    Ok(Data { ano, mes, dia }.epoch_inicio(tempo::BRT_OFFSET))
}

#[cfg(test)]
mod tests_data {
    use super::ler_data;

    #[test]
    fn vazio_e_hoje_e_o_resto_e_conferido() {
        assert_eq!(ler_data("", 1_789_000_000), Ok(1_789_000_000));
        assert!(ler_data("08/09/2026", 0).is_ok());
        assert!(ler_data("2026-09-08", 0).is_err());
        assert!(ler_data("32/09/2026", 0).is_err());
        assert!(ler_data("08/13/2026", 0).is_err());
    }
}
