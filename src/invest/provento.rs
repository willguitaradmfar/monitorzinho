//! Os proventos **anunciados** — o que a empresa declarou por cota, e quando.
//!
//! É diferente de `model::Provento`, que é o que **você recebeu**. A distinção não é
//! preciosismo: o valor recebido depende da quantidade que se tinha na data-com, e
//! nenhuma fonte de mercado sabe isso. Calcular com a quantidade de hoje e gravar como
//! fato seria inventar histórico — o mesmo erro que a regra do preço médio informado
//! existe para evitar.
//!
//! Então isto entra como **sugestão**: a tela mostra o anúncio, estima com a quantidade
//! de hoje, e quem confirma é quem investe. Some a digitação, fica a decisão.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::invest::model::AssetId;
use crate::invest::provider::ProviderSet;

/// Um provento declarado por uma empresa.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Anunciado {
    pub ativo: AssetId,
    /// A data-com, em epoch — o dia em que era preciso ter o papel.
    pub em: u64,
    /// Quanto por cota. **Não** é o valor recebido: falta a quantidade.
    pub por_cota: f64,
    /// O dividend yield que a fonte publicou junto, em porcento.
    pub dy: f64,
}

#[derive(Default)]
pub struct Cache {
    /// Por ativo, para não refazer a busca de quem já respondeu.
    por_ativo: Arc<Mutex<HashMap<AssetId, Vec<Anunciado>>>>,
    pedidos: Arc<Mutex<Vec<AssetId>>>,
    pendentes: Arc<AtomicUsize>,
}

impl Cache {
    /// O que já chegou, sem buscar nada.
    pub fn ja_tem(&self) -> Vec<Anunciado> {
        let mut v: Vec<Anunciado> = self
            .por_ativo
            .lock()
            .map(|m| m.values().flatten().cloned().collect())
            .unwrap_or_default();
        v.sort_by_key(|a| std::cmp::Reverse(a.em));
        v
    }

    /// O mesmo, pedindo o que ainda falta. Devolve na hora — a busca é em thread.
    pub fn get(&self, providers: &Arc<ProviderSet>, ativos: &[AssetId]) -> Vec<Anunciado> {
        for ativo in ativos {
            let ja = self
                .por_ativo
                .lock()
                .map(|m| m.contains_key(ativo))
                .unwrap_or(true);
            let pedido = self
                .pedidos
                .lock()
                .map(|p| p.contains(ativo))
                .unwrap_or(true);
            if ja || pedido {
                continue;
            }
            if let Ok(mut p) = self.pedidos.lock() {
                p.push(ativo.clone());
            }
            self.buscar(providers, ativo.clone());
        }
        self.ja_tem()
    }

    pub fn pendentes(&self) -> usize {
        self.pendentes.load(Ordering::Relaxed)
    }

    pub fn semear(&self, anunciados: Vec<Anunciado>) {
        if let Ok(mut m) = self.por_ativo.lock() {
            for a in anunciados {
                m.entry(a.ativo.clone()).or_default().push(a);
            }
        }
    }

    fn buscar(&self, providers: &Arc<ProviderSet>, ativo: AssetId) {
        let por_ativo = Arc::clone(&self.por_ativo);
        let pendentes = Arc::clone(&self.pendentes);
        let providers = Arc::clone(providers);
        pendentes.fetch_add(1, Ordering::Relaxed);
        thread::Builder::new()
            .name("invest-proventos".into())
            .spawn(move || {
                let achado = providers.proventos(&ativo);
                if let Ok(mut m) = por_ativo.lock() {
                    // Um ativo sem proventos entra com a lista vazia, e não fica de fora:
                    // é o que impede a busca de ser repetida para sempre.
                    m.insert(ativo, achado.unwrap_or_default());
                }
                pendentes.fetch_sub(1, Ordering::Relaxed);
            })
            .ok();
    }
}

/// O valor que **teria sido** recebido, com a quantidade de hoje.
///
/// Uma estimativa, e a tela que a mostra tem que dizer isso: quem comprou depois da
/// data-com não recebeu nada, e quem tinha o dobro recebeu o dobro.
pub fn estimado(anunciado: &Anunciado, quantidade: f64) -> f64 {
    anunciado.por_cota * quantidade
}
