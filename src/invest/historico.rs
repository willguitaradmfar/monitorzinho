//! Séries históricas buscadas em segundo plano.
//!
//! Uma série é uma requisição que pode demorar segundos. Buscá-la no caminho do desenho
//! congelaria a interface pelo tempo do timeout, então ela vai para uma thread e a tela
//! mostra o que já tem — a mesma disciplina de todo o resto do programa.
//!
//! O cache é por `(ativo, janela)` e vive enquanto o módulo está aberto. Andar entre
//! períodos já visitados não gera requisição; fechar o módulo descarta tudo, porque uma
//! série inteira não é o que o cache de disco guarda (aquele guarda o último ponto, para
//! a aba abrir com números).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::invest::model::AssetId;
use crate::invest::provider::{Candle, ProviderSet, Span};

/// O que se sabe sobre uma série agora.
#[derive(Clone)]
pub enum Estado {
    Buscando,
    Pronta(Vec<Candle>),
    Falhou(String),
    /// A fonte não tem histórico deste ativo — o caso de todo preço informado. É
    /// diferente de «falhou»: não adianta tentar de novo.
    SemFonte,
}

/// O cache de séries de um módulo aberto.
pub struct Cache {
    series: Arc<Mutex<HashMap<(AssetId, Span), Estado>>>,
    /// Quantas buscas estão em voo. A tela mostra «carregando 3 de 8» em vez de esperar
    /// tudo — a matriz de correlação aparece parcial e vai preenchendo.
    pendentes: Arc<AtomicUsize>,
}

impl Default for Cache {
    fn default() -> Self {
        Self {
            series: Arc::new(Mutex::new(HashMap::new())),
            pendentes: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Cache {
    /// O estado de uma série, pedindo a busca se ela ainda não foi pedida.
    pub fn get(&self, providers: &Arc<ProviderSet>, ativo: &AssetId, span: Span) -> Estado {
        let chave = (ativo.clone(), span);
        {
            let Ok(mapa) = self.series.lock() else {
                return Estado::Buscando;
            };
            if let Some(estado) = mapa.get(&chave) {
                return estado.clone();
            }
        }
        self.buscar(providers, chave);
        Estado::Buscando
    }

    /// Só olha, sem pedir. Para quem quer desenhar o que já existe sem disparar rede.
    pub fn peek(&self, ativo: &AssetId, span: Span) -> Option<Estado> {
        self.series
            .lock()
            .ok()
            .and_then(|m| m.get(&(ativo.clone(), span)).cloned())
    }

    pub fn pendentes(&self) -> usize {
        self.pendentes.load(Ordering::Relaxed)
    }

    fn buscar(&self, providers: &Arc<ProviderSet>, chave: (AssetId, Span)) {
        if let Ok(mut mapa) = self.series.lock() {
            // Marca antes de soltar o mutex, senão dois desenhos seguidos disparam duas
            // buscas da mesma série.
            if mapa.contains_key(&chave) {
                return;
            }
            mapa.insert(chave.clone(), Estado::Buscando);
        }
        let series = Arc::clone(&self.series);
        let pendentes = Arc::clone(&self.pendentes);
        let providers = Arc::clone(providers);
        pendentes.fetch_add(1, Ordering::Relaxed);
        thread::Builder::new()
            .name("invest-historico".into())
            .spawn(move || {
                let (ativo, span) = chave.clone();
                let resultado = match providers.history(&ativo, span) {
                    Some(Ok(v)) if v.is_empty() => {
                        Estado::Falhou("a fonte respondeu sem nenhum ponto".into())
                    }
                    Some(Ok(v)) => Estado::Pronta(v),
                    Some(Err(e)) => Estado::Falhou(e.frase()),
                    None => Estado::SemFonte,
                };
                if matches!(resultado, Estado::Pronta(_)) {
                    crate::invest::store::buscas().carimbar(crate::invest::store::fonte::HISTORICO);
                }
                if let Ok(mut mapa) = series.lock() {
                    mapa.insert(chave, resultado);
                }
                pendentes.fetch_sub(1, Ordering::Relaxed);
            })
            .ok();
    }
}

/// Os fechamentos de uma série, que é o que quase todo cálculo quer.
/// Os rótulos do eixo do tempo de uma série de velas.
///
/// `dd/mm` numa janela curta e `mm/aa` numa longa: num gráfico de cinco anos a data exata
/// de cada ponto não cabe nem interessa, e num de uma semana o mês sozinho não distingue
/// nada. Quem desenha decide quantos deles cabem — ver `render_pane_chart`.
pub fn rotulos(candles: &[Candle], span: Span) -> Vec<String> {
    use crate::invest::tempo::{self, Data};
    let curto = matches!(span, Span::Dia | Span::Semana | Span::Mes);
    candles
        .iter()
        .map(|c| {
            let d = Data::de_epoch(c.em, tempo::BRT_OFFSET);
            match curto {
                true => format!("{:02}/{:02}", d.dia, d.mes),
                false => format!("{:02}/{:02}", d.mes, d.ano % 100),
            }
        })
        .collect()
}

pub fn fechamentos(candles: &[Candle]) -> Vec<f64> {
    candles.iter().map(|c| c.fechamento).collect()
}

/// A frase que a tela mostra quando não há série. Diz **por que**, e o que fazer.
pub fn explicar(estado: &Estado, ativo: &AssetId) -> Option<String> {
    match estado {
        Estado::Pronta(_) => None,
        Estado::Buscando => Some("buscando a série…".into()),
        Estado::Falhou(e) => Some(format!("a fonte não respondeu: {e}")),
        Estado::SemFonte => Some(format!(
            "{ativo} não tem histórico: o preço dele é informado, e um preço informado \
             não tem série.\n\nTêm histórico: pares da Binance, câmbio (FX/USDBRL) e as \
             séries do Banco Central."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::Market;

    #[test]
    fn fechamentos_extrai_a_coluna_certa() {
        let candles = vec![
            Candle {
                em: 1,
                fechamento: 10.0,
            },
            Candle {
                em: 2,
                fechamento: 12.0,
            },
        ];
        assert_eq!(fechamentos(&candles), vec![10.0, 12.0]);
    }

    #[test]
    fn sem_fonte_e_diferente_de_falhou() {
        let ativo = AssetId::new(Market::B3, "PETR4");
        // «Sem fonte» explica e não convida a tentar de novo; «falhou» diz o erro.
        let sem = explicar(&Estado::SemFonte, &ativo).unwrap();
        assert!(sem.contains("informado"));
        let falhou = explicar(&Estado::Falhou("tempo".into()), &ativo).unwrap();
        assert!(falhou.contains("tempo"));
        // Pronta não explica nada — não há o que explicar.
        assert!(explicar(&Estado::Pronta(Vec::new()), &ativo).is_none());
    }

    #[test]
    fn peek_nao_dispara_busca() {
        let c = Cache::default();
        let ativo = AssetId::new(Market::Binance, "BTCBRL");
        assert!(c.peek(&ativo, Span::Mes).is_none());
        assert_eq!(c.pendentes(), 0, "olhar não pode custar uma requisição");
    }
}
