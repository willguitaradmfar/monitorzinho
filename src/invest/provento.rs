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

/// A partir de que idade a lista de anúncios deixa de estar em dia.
///
/// Um dia. Uma empresa anuncia provento algumas vezes por ano, mas o que importa é o
/// atraso máximo entre o anúncio sair e ele aparecer aqui — e um dia é o que faz a
/// sugestão chegar antes da data-com na esmagadora maioria dos casos.
const VALIDADE: u64 = 24 * 60 * 60;

/// Se a lista guardada ainda vale, pela idade da última conversa com a fonte.
///
/// **Sem registro nenhum é «não vale»**, e essa é a metade que faltava. O cache é semeado
/// do disco na abertura, e sem esta pergunta todo ativo da carteira já era chave do mapa
/// e nunca mais era buscado — nem nesta execução, nem em nenhuma futura.
fn em_dia(idade: Option<u64>) -> bool {
    idade.is_some_and(|i| i < VALIDADE)
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

    /// O mesmo, pedindo o que ainda falta **e o que envelheceu**. Devolve na hora — a
    /// busca é em thread.
    ///
    /// «O que envelheceu» não estava aqui, e a falta congelava a lista para sempre: o
    /// cache é semeado do disco na abertura, todo ativo da carteira já era chave do mapa,
    /// e um ativo que é chave nunca era buscado de novo — nem nesta execução, nem em
    /// nenhuma futura. Quem tivesse rodado o programa uma vez nunca mais veria um anúncio
    /// novo, e nada na tela dizia isso. Foi o selo de idade que denunciou: ele dizia
    /// «nunca buscado» e estava certo.
    pub fn get(&self, providers: &Arc<ProviderSet>, ativos: &[AssetId]) -> Vec<Anunciado> {
        // A semente é **dado**, não recibo: ela diz o que se sabe, e não quando se
        // soube. Quem responde «quando» é o registro de buscas — e sem registro nenhum,
        // como acontece na primeira vez que esta versão roda, o que se sabe é velho por
        // definição.
        let em_dia = em_dia(
            crate::invest::store::buscas()
                .idade(crate::invest::store::fonte::ANUNCIADO, crate::db::agora()),
        );
        for ativo in ativos {
            let ja = em_dia
                && self
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
            // `pedidos` guarda o resto: com a lista vencida, cada ativo é rebuscado
            // **uma vez por execução**, e não a cada desenho da tela.
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
                if achado.is_some() {
                    crate::invest::store::buscas().carimbar(crate::invest::store::fonte::ANUNCIADO);
                }
                if let Ok(mut m) = por_ativo.lock() {
                    // Um ativo sem proventos entra com a lista vazia, e não fica de fora:
                    // é o que impede a busca de ser repetida dentro desta execução.
                    // `insert` e não `extend`: a resposta nova **substitui** a semente do
                    // disco, senão um anúncio que já estava lá entraria duas vezes.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A regra que descongelou a lista. Sem o caso `None`, um cache semeado do disco
    /// nunca era rebuscado: todo ativo da carteira já era chave do mapa, e quem rodasse
    /// o programa uma vez não veria mais nenhum anúncio novo — em nenhuma execução
    /// futura, e sem nada na tela dizendo isso.
    #[test]
    fn sem_registro_de_busca_a_lista_e_velha_por_definicao() {
        assert!(!em_dia(None));
    }

    #[test]
    fn um_dia_e_o_corte() {
        assert!(em_dia(Some(0)));
        assert!(em_dia(Some(VALIDADE - 1)));
        assert!(!em_dia(Some(VALIDADE)));
        assert!(!em_dia(Some(VALIDADE * 30)));
    }
}
