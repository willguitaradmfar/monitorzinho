//! Os provedores concretos, na ordem de preferência em que o despacho os consulta.

pub mod bcb;
pub mod binance;
pub mod brapi;
pub mod cambio;
pub mod fred;
pub mod kinvo;
pub mod manual;
pub mod tesouro;
pub mod yahoo;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::invest::model::AssetId;
use crate::invest::provider::Provider;

/// A ordem importa, e ela é uma cadeia de reserva: o despacho tenta o primeiro que cobre
/// o ativo e, **se ele não entregar**, passa a vez ao seguinte — ver `ProviderSet::rodar`.
///
/// * `kinvo` primeiro para a B3, por uma razão medida: ela responde **em lote**. Vinte
///   tickers numa chamada contra um pedido por ativo da brapi sem token — e é a única
///   fonte sem chave que serve o IBOV.
/// * `brapi` logo atrás, e é ela quem tem contrato: se o endereço da Kinvo mudar de forma,
///   a aba troca de fonte sozinha em vez de ficar sem preço. Ela também é a única com
///   fundamento, que a Kinvo não serve.
/// * `yahoo` como terceira: não-oficial, sem contrato, e é onde se cai quando a brapi
///   sem token esbarra no limite anônimo.
/// * `manual` por último, porque cobre tudo e nunca pode tirar a vez de quem sabe mais.
///   Ele é a reserva final: onde nenhuma fonte responde, vale o preço informado.
pub fn todos(manuais: Arc<Mutex<HashMap<AssetId, (f64, u64)>>>) -> Vec<Box<dyn Provider>> {
    vec![
        Box::new(kinvo::Kinvo),
        Box::new(brapi::Brapi::default()),
        Box::new(yahoo::Yahoo::default()),
        Box::new(binance::Binance::default()),
        Box::new(cambio::Cambio),
        Box::new(bcb::Bcb),
        Box::new(fred::Fred),
        Box::new(tesouro::Tesouro),
        Box::new(manual::Manual::new(manuais)),
    ]
}
