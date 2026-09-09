//! O preço informado — e por que ele é um provedor de verdade.
//!
//! Foi decidido cobrir a B3 e a bolsa americana usando **só fontes sem chave**. Essas
//! duas coisas não fecham: o dado de bolsa é vendido, e o que existe de graça ou pede
//! cadastro ou é raspado de página e quebra sem aviso.
//!
//! A saída não é escolher entre as duas respostas. É a mesma já tomada para o preço
//! médio: **onde não há fonte confiável, o número é informado** — e a tela mostra a
//! origem e a idade dele, em vez de fingir que é ao vivo.
//!
//! Este provedor não é um espaço reservado. Ele é o provedor **correto** para uma fonte
//! que não existe. A alternativa — coluna vazia, ou o preço médio no lugar do preço atual
//! — seria pior das duas maneiras possíveis: ou esconde a posição, ou mostra um P&L de
//! zero que parece um fato.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::invest::feed::FeedError;
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::provider::{Grade, Provider, Quote};

pub struct Manual {
    /// Os preços informados vivem no `ProviderSet` e não aqui, porque quem os conhece é a
    /// carteira, que muda enquanto o programa roda.
    precos: Arc<Mutex<HashMap<AssetId, (f64, u64)>>>,
}

impl Manual {
    pub fn new(precos: Arc<Mutex<HashMap<AssetId, (f64, u64)>>>) -> Self {
        Self { precos }
    }
}

impl Provider for Manual {
    fn id(&self) -> &'static str {
        "manual"
    }

    fn name(&self) -> &'static str {
        "informado"
    }

    /// Cobre tudo. É o último da ordem de preferência justamente por isso: ele nunca
    /// deixa um ativo sem resposta, e nunca tira a vez de quem sabe mais.
    fn covers(&self, _ativo: &AssetId) -> bool {
        true
    }

    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
        let Ok(precos) = self.precos.lock() else {
            return Ok(Vec::new());
        };
        Ok(ativos
            .iter()
            .filter_map(|ativo| {
                let (preco, em) = precos.get(ativo)?;
                Some(Quote {
                    fonte: "manual",
                    moeda: moeda_padrao(ativo),
                    ativo: ativo.clone(),
                    preco: *preco,
                    // Nunca: um preço informado tem variação calculável entre dois valores
                    // digitados, e mostrá-la lhe daria a autoridade visual de um ao vivo.
                    anterior: None,
                    grade: Grade::Manual,
                    em: *em,
                    volume: None,
                    max24: None,
                    min24: None,
                })
            })
            .collect())
    }

    /// Não faz I/O, então nunca conta como «uma fonte que caiu» nem participa do
    /// limitador.
    fn remoto(&self) -> bool {
        false
    }

    fn intervalo(&self) -> Duration {
        Duration::from_secs(2)
    }
}

fn moeda_padrao(ativo: &AssetId) -> Moeda {
    ativo.market.moeda().unwrap_or(match ativo.market {
        Market::Binance => super::binance::moeda_do_par(&ativo.symbol),
        _ => Moeda::Brl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devolve_so_o_que_foi_informado() {
        let precos = Arc::new(Mutex::new(HashMap::new()));
        let petr = AssetId::new(Market::B3, "PETR4");
        let vale = AssetId::new(Market::B3, "VALE3");
        precos.lock().unwrap().insert(petr.clone(), (38.42, 1000));

        let m = Manual::new(Arc::clone(&precos));
        let q = m.quotes(&[petr.clone(), vale]).unwrap();
        assert_eq!(q.len(), 1, "o que não foi informado não vira preço");
        assert_eq!(q[0].preco, 38.42);
        assert_eq!(q[0].grade, Grade::Manual);
        assert_eq!(q[0].anterior, None, "informado nunca tem variação");
        assert_eq!(q[0].moeda, Moeda::Brl);
    }

    #[test]
    fn cobre_tudo_para_nunca_deixar_ativo_sem_resposta() {
        let m = Manual::new(Arc::new(Mutex::new(HashMap::new())));
        assert!(m.covers(&AssetId::new(Market::B3, "PETR4")));
        assert!(m.covers(&AssetId::new(Market::Us, "AAPL")));
        assert!(m.covers(&AssetId::new(Market::Outro, "MEU-CDB")));
    }

    #[test]
    fn nao_e_remoto_e_por_isso_nao_entra_no_limitador() {
        let m = Manual::new(Arc::new(Mutex::new(HashMap::new())));
        assert!(!m.remoto());
    }

    #[test]
    fn moeda_vem_do_mercado_quando_ele_a_define() {
        assert_eq!(moeda_padrao(&AssetId::new(Market::B3, "PETR4")), Moeda::Brl);
        assert_eq!(moeda_padrao(&AssetId::new(Market::Us, "AAPL")), Moeda::Usd);
        assert_eq!(
            moeda_padrao(&AssetId::new(Market::Binance, "BTCUSDT")),
            Moeda::Usdt
        );
    }
}
