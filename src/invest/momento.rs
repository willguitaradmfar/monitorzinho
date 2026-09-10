//! Para que lado **cada número** andou desde a leitura anterior.
//!
//! É o sinalizador de momento das telas de mercado: a seta verde ou vermelha colada no
//! número. Ela responde outra pergunta que a variação do dia — «está andando agora, e
//! para que lado» — e as duas juntas mostram o desencontro que interessa: um papel em
//! alta no dia e caindo neste minuto.
//!
//! **Um registro por número, e não um por papel.** A primeira versão guardava a direção
//! do *preço* e repetia essa mesma seta em toda coluna da linha — a variação, a máxima, a
//! mínima, o P&L. Estava errado e era visível: a máxima do dia pode subir numa volta em
//! que o preço caiu, e a seta dizia o contrário. Cada grandeza tem a própria memória.
//!
//! **Anotado na hora de desenhar, e não na volta da busca.** Quem sabe o que é «o P&L do
//! PETR4» é o módulo que o calcula, e centralizar isto obrigaria a refazer a conta de
//! cada módulo num lugar só. Desenhar com o mesmo retrato duas vezes não move seta
//! nenhuma: valor igual mantém o que estava, então a seta só muda quando o número muda.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// O registro do processo.
///
/// Global pelo mesmo motivo de `store::buscas`: é a memória de «o que eu vi da última
/// vez», e quem consulta são vinte telas que não têm — nem deviam ter — um caminho até
/// um estado compartilhado só para isto.
pub fn momento() -> &'static Momento {
    static M: OnceLock<Momento> = OnceLock::new();
    M.get_or_init(Momento::default)
}

#[derive(Default)]
pub struct Momento {
    /// Chave → (último valor visto, para que lado ele veio).
    visto: Mutex<HashMap<String, (f64, Direcao)>>,
}

/// Para que lado, ou nenhum — que é o estado de quem ainda não mudou desde que o
/// programa abriu.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Direcao {
    #[default]
    Parado,
    Subiu,
    Desceu,
}

impl Direcao {
    /// A seta, com o espaço que a separa do número. Vazio quando não há o que dizer.
    ///
    /// Só o texto: a cor é decidida no desenho, porque a seta **não pode** herdar a cor
    /// da célula — ela é vermelha ao lado de uma variação verde justamente quando isso
    /// importa. Ver `ui::com_seta`.
    pub fn seta(self) -> &'static str {
        match self {
            Direcao::Subiu => "▲ ",
            Direcao::Desceu => "▼ ",
            Direcao::Parado => "",
        }
    }
}

impl Momento {
    /// Anota o valor de `chave` e devolve para que lado ele andou desde a anotação
    /// anterior.
    ///
    /// **Valor igual mantém a direção anterior.** Uma seta que aparece numa volta e some
    /// na seguinte não chega a ser lida, e a pergunta é «para que lado foi o último
    /// movimento», não «mexeu exatamente agora».
    pub fn direcao(&self, chave: &str, valor: f64) -> Direcao {
        if !valor.is_finite() {
            return Direcao::Parado;
        }
        let Ok(mut visto) = self.visto.lock() else {
            return Direcao::Parado;
        };
        match visto.get_mut(chave) {
            Some((anterior, direcao)) => {
                match valor.total_cmp(anterior) {
                    std::cmp::Ordering::Greater => *direcao = Direcao::Subiu,
                    std::cmp::Ordering::Less => *direcao = Direcao::Desceu,
                    std::cmp::Ordering::Equal => {}
                }
                *anterior = valor;
                *direcao
            }
            None => {
                // A primeira vez não é movimento nenhum: não havia com o que comparar.
                visto.insert(chave.to_string(), (valor, Direcao::Parado));
                Direcao::Parado
            }
        }
    }

    /// O mesmo, direto na seta.
    pub fn seta(&self, chave: &str, valor: f64) -> &'static str {
        self.direcao(chave, valor).seta()
    }

    /// E para o número que pode não existir. Um número ausente **não apaga** a direção
    /// que já havia: a fonte pode ter falhado nesta volta, e isso não é um movimento.
    pub fn seta_de(&self, chave: &str, valor: Option<f64>) -> &'static str {
        match valor {
            Some(v) => self.seta(chave, v),
            None => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_primeira_leitura_nao_e_movimento() {
        let m = Momento::default();
        assert_eq!(m.direcao("x", 10.0), Direcao::Parado);
    }

    #[test]
    fn sobe_desce_e_fica() {
        let m = Momento::default();
        m.direcao("x", 10.0);
        assert_eq!(m.direcao("x", 10.5), Direcao::Subiu);
        // Parado mantém a última direção, em vez de apagá-la.
        assert_eq!(m.direcao("x", 10.5), Direcao::Subiu);
        assert_eq!(m.direcao("x", 9.0), Direcao::Desceu);
        assert_eq!(m.direcao("x", 9.0), Direcao::Desceu);
    }

    /// A correção que motivou o registro: a máxima do dia pode subir numa volta em que
    /// o preço caiu. Com uma direção por papel, as duas setas diziam a mesma coisa — e
    /// uma delas mentia.
    #[test]
    fn cada_numero_tem_a_propria_direcao() {
        let m = Momento::default();
        m.direcao("PETR4:preco", 30.0);
        m.direcao("PETR4:max", 30.0);
        assert_eq!(m.direcao("PETR4:preco", 29.0), Direcao::Desceu);
        assert_eq!(m.direcao("PETR4:max", 31.0), Direcao::Subiu);
    }

    #[test]
    fn numero_que_nao_existe_nao_apaga_a_seta() {
        let m = Momento::default();
        m.direcao("x", 10.0);
        m.direcao("x", 11.0);
        assert_eq!(m.seta_de("x", None), "");
        // E a direção guardada continua lá para a próxima leitura de verdade.
        assert_eq!(m.direcao("x", 11.0), Direcao::Subiu);
    }

    #[test]
    fn valor_nao_finito_nao_vira_movimento() {
        let m = Momento::default();
        assert_eq!(m.direcao("x", f64::NAN), Direcao::Parado);
        assert_eq!(m.direcao("x", f64::INFINITY), Direcao::Parado);
    }
}
