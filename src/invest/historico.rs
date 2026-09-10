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
    /// Sem mensagem: nenhuma tela a mostra desde que Gráfico e Risco saíram, e um
    /// campo que ninguém lê é um campo que mente sobre existir.
    Falhou,
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
                    Some(Ok(v)) if v.is_empty() => Estado::Falhou,
                    Some(Ok(v)) => Estado::Pronta(v),
                    Some(Err(_)) => Estado::Falhou,
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
