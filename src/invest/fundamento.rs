//! Os números que descrevem a empresa por trás do papel.
//!
//! O documento de planejamento dizia que isto não existiria sem chave. Estava errado: a
//! brapi serve os módulos `defaultKeyStatistics`, `financialData` e `summaryProfile`
//! mesmo com um token grátis — conferido em 08/09/2026.
//!
//! Como o histórico, a busca é em segundo plano e o cache vive enquanto o módulo está
//! aberto. Diferente do histórico, a cadência é folgada: balanço muda por trimestre, e
//! pedi-lo duas vezes no mesmo dia é gastar cota para receber a mesma resposta.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::invest::model::AssetId;
use crate::invest::provider::ProviderSet;

/// O que se sabe sobre uma empresa. Todo campo é opcional porque toda fonte tem buracos —
/// e um buraco vira `—` na tela, nunca um zero que se leria como fato.
#[derive(Clone, Debug, Default)]
pub struct Fundamentos {
    pub setor: Option<String>,
    pub industria: Option<String>,
    /// Preço sobre lucro.
    pub p_l: Option<f64>,
    /// Lucro por ação.
    pub lpa: Option<f64>,
    pub valor_de_mercado: Option<f64>,
    pub margem_liquida: Option<f64>,
    pub ebitda: Option<f64>,
    pub divida_total: Option<f64>,
    pub caixa: Option<f64>,
    pub liquidez_corrente: Option<f64>,
    pub acoes_emitidas: Option<f64>,
}

impl Fundamentos {
    /// Dívida líquida sobre EBITDA — o quanto a empresa deve, medido no que ela gera.
    ///
    /// `None` com EBITDA nulo ou negativo: a divisão existiria, e o número não
    /// significaria nada. Uma empresa que não gera caixa não tem «dívida em anos de
    /// EBITDA» — ela tem outro problema, e mostrar um número aqui esconderia isso.
    pub fn divida_sobre_ebitda(&self) -> Option<f64> {
        let (divida, caixa, ebitda) = (self.divida_total?, self.caixa.unwrap_or(0.0), self.ebitda?);
        (ebitda > 0.0).then(|| (divida - caixa) / ebitda)
    }

    /// Se há o suficiente para valer a pena desenhar uma tela.
    pub fn tem_algo(&self) -> bool {
        self.p_l.is_some()
            || self.margem_liquida.is_some()
            || self.ebitda.is_some()
            || self.setor.is_some()
    }
}

/// O estado de uma busca de fundamentos.
#[derive(Clone)]
pub enum Estado {
    Buscando,
    Pronto(Box<Fundamentos>),
    Falhou(String),
    /// Nenhuma fonte serve fundamento deste ativo — cripto e câmbio não têm balanço.
    SemFonte,
}

/// O cache de um módulo aberto.
/// Quanto tempo uma falha fica de pé antes de valer a pena tentar de novo.
///
/// Uma recusa por cota é passageira — o token grátis solta alguns por minuto —, e guardá-la
/// para sempre deixaria metade da carteira com «respondeu 403» até fechar o módulo. Já
/// tentar de novo a cada desenho seria martelar a fonte que acabou de pedir calma.
const TENTAR_DE_NOVO: Duration = Duration::from_secs(45);

#[derive(Default)]
pub struct Cache {
    dados: Arc<Mutex<HashMap<AssetId, (Estado, Instant)>>>,
    pendentes: Arc<AtomicUsize>,
}

impl Cache {
    pub fn get(&self, providers: &Arc<ProviderSet>, ativo: &AssetId) -> Estado {
        {
            let Ok(mapa) = self.dados.lock() else {
                return Estado::Buscando;
            };
            if let Some((estado, desde)) = mapa.get(ativo) {
                // Uma falha vencida deixa de valer, e a próxima olhada tenta de novo.
                let vencida =
                    matches!(estado, Estado::Falhou(_)) && desde.elapsed() >= TENTAR_DE_NOVO;
                if !vencida {
                    return estado.clone();
                }
            }
        }
        self.buscar(providers, ativo.clone());
        Estado::Buscando
    }

    pub fn pendentes(&self) -> usize {
        self.pendentes.load(Ordering::Relaxed)
    }

    fn buscar(&self, providers: &Arc<ProviderSet>, ativo: AssetId) {
        if let Ok(mut mapa) = self.dados.lock() {
            // Marca antes de soltar o mutex: dois desenhos seguidos disparariam duas
            // buscas do mesmo ativo.
            if matches!(mapa.get(&ativo), Some((Estado::Buscando, _))) {
                return;
            }
            mapa.insert(ativo.clone(), (Estado::Buscando, Instant::now()));
        }
        let dados = Arc::clone(&self.dados);
        let pendentes = Arc::clone(&self.pendentes);
        let providers = Arc::clone(providers);
        pendentes.fetch_add(1, Ordering::Relaxed);
        thread::Builder::new()
            .name("invest-fundamento".into())
            .spawn(move || {
                let resultado = match providers.fundamentos(&ativo) {
                    Some(Ok(f)) if f.tem_algo() => Estado::Pronto(Box::new(f)),
                    Some(Ok(_)) => Estado::Falhou("a fonte respondeu sem nenhum número".into()),
                    Some(Err(e)) => Estado::Falhou(e.frase()),
                    None => Estado::SemFonte,
                };
                if matches!(resultado, Estado::Pronto(_)) {
                    crate::invest::store::buscas()
                        .carimbar(crate::invest::store::fonte::FUNDAMENTO);
                }
                if let Ok(mut mapa) = dados.lock() {
                    mapa.insert(ativo, (resultado, Instant::now()));
                }
                pendentes.fetch_sub(1, Ordering::Relaxed);
            })
            .ok();
    }
}

/// A frase que a tela mostra quando não há o que mostrar.
pub fn explicar(estado: &Estado, ativo: &AssetId) -> Option<String> {
    match estado {
        Estado::Pronto(_) => None,
        Estado::Buscando => Some("buscando…".into()),
        // Um 401/403 aqui não é a fonte fora do ar: é o plano recusando **este ticker**.
        // Medido: com token grátis, PETR4 responde 200 e BBAS3 responde 403. Chamar isso
        // de «não respondeu» manda a pessoa procurar um problema de rede que não existe.
        Estado::Falhou(e) if e.contains("401") || e.contains("403") => Some(format!(
            "o seu plano na fonte não libera fundamento de {} ({e})",
            ativo.short()
        )),
        Estado::Falhou(e) => Some(format!("a fonte não respondeu: {e}")),
        Estado::SemFonte => Some(format!(
            "{ativo} não tem fundamento a mostrar: cripto, câmbio e séries do Banco \
             Central não são empresas, e não têm balanço."
        )),
    }
}

/// O mesmo motivo, em duas ou três palavras, para caber numa célula de tabela.
///
/// A explicação inteira ia na coluna «Setor», truncada em sessenta caracteres — e uma
/// frase cortada onde se espera o nome de um setor não se lê como um aviso, se lê como
/// dado errado. A frase inteira continua no painel de detalhe, que é onde há espaço.
pub fn motivo_curto(estado: &Estado) -> &'static str {
    match estado {
        Estado::Pronto(_) => "",
        Estado::Buscando => "buscando…",
        Estado::Falhou(e) if e.contains("401") || e.contains("403") => "o plano não libera",
        Estado::Falhou(_) => "a fonte não respondeu",
        Estado::SemFonte => "não é empresa",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::Market;

    #[test]
    fn o_motivo_curto_cabe_numa_celula_e_o_longo_explica() {
        // Os dois existem para lugares diferentes, e o curto tem que continuar curto:
        // numa coluna de tabela, uma frase cortada no meio parece dado, não aviso.
        let recusado = Estado::Falhou("HTTP 403".into());
        assert!(motivo_curto(&recusado).chars().count() <= 24);
        let longo = explicar(&recusado, &AssetId::new(Market::B3, "BBAS3")).unwrap();
        assert!(longo.contains("BBAS3"), "o longo diz de quem se trata");
        assert!(longo.len() > motivo_curto(&recusado).len());
        assert_eq!(motivo_curto(&Estado::Buscando), "buscando…");
    }

    #[test]
    fn divida_sobre_ebitda_desconta_o_caixa() {
        let f = Fundamentos {
            divida_total: Some(676_283_000_000.0),
            caixa: Some(53_764_000_000.0),
            ebitda: Some(273_323_000_000.0),
            ..Default::default()
        };
        let d = f.divida_sobre_ebitda().unwrap();
        assert!((d - 2.277).abs() < 0.01, "deu {d}");
    }

    #[test]
    fn sem_ebitda_positivo_nao_ha_multiplo() {
        // A divisão existiria, e o número não significaria nada. Uma empresa que não gera
        // caixa não tem «dívida em anos de EBITDA» — ela tem outro problema.
        let base = Fundamentos {
            divida_total: Some(100.0),
            caixa: Some(10.0),
            ..Default::default()
        };
        assert_eq!(base.divida_sobre_ebitda(), None);
        let zero = Fundamentos {
            ebitda: Some(0.0),
            ..base.clone()
        };
        assert_eq!(zero.divida_sobre_ebitda(), None);
        let negativo = Fundamentos {
            ebitda: Some(-50.0),
            ..base
        };
        assert_eq!(negativo.divida_sobre_ebitda(), None);
    }

    #[test]
    fn tudo_vazio_nao_vira_tela() {
        assert!(!Fundamentos::default().tem_algo());
        assert!(
            Fundamentos {
                p_l: Some(4.6),
                ..Default::default()
            }
            .tem_algo()
        );
    }

    #[test]
    fn sem_fonte_explica_em_vez_de_ficar_em_branco() {
        let btc = AssetId::new(Market::Binance, "BTCBRL");
        let frase = explicar(&Estado::SemFonte, &btc).unwrap();
        assert!(frase.contains("não são empresas"));
        assert!(explicar(&Estado::Pronto(Box::default()), &btc).is_none());
    }
}
