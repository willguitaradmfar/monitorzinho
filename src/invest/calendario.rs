//! O calendário econômico: quando saem os números que movem o mercado.
//!
//! **O que é real e o que não é**, medido em 08/09/2026:
//!
//! * **Brasil** — o IBGE publica um calendário de divulgação em API pública, sem chave
//!   (`servicodados.ibge.gov.br/api/v3/calendario`), com data e hora exatas de IPCA, PIB,
//!   desemprego, produção industrial e comércio. O Banco Central publica as reuniões do
//!   COPOM como tabela anual. Os dois são fonte de verdade.
//! * **Exterior** — não há fonte gratuita e sem chave. A TradingEconomics encerrou a conta
//!   de convidado (HTTP 410), e as demais pedem cadastro. O que dá para fazer sem
//!   inventar data é **regra determinística** — o payroll americano sai na primeira
//!   sexta-feira do mês, e isso é definição, não estimativa — e deixar o resto para quem
//!   quiser digitar.
//!
//! Nada aqui é adivinhado. Um evento sem data conhecida não aparece com data aproximada
//! disfarçada de exata: ou tem regra, ou vem do IBGE, ou foi digitado.

use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::Value;

use crate::invest::feed;
use crate::invest::tempo::{self, Data};

/// De onde um evento veio — e é o que decide se a data pode ser levada a sério.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Origem {
    /// Calendário publicado: IBGE ou a tabela do COPOM. Data e hora exatas.
    #[serde(rename = "publicado")]
    Publicado,
    /// Regra determinística, como «primeira sexta-feira do mês». Exata por definição.
    #[serde(rename = "regra")]
    Regra,
    /// Digitado por quem usa. Ainda não há tela que crie um — a variante existe porque a
    /// origem é a informação que decide se a data pode ser levada a sério, e um evento
    /// digitado precisa poder dizer que é digitado no dia em que essa tela existir.
    #[allow(dead_code)]
    #[serde(rename = "informado")]
    Informado,
}

impl Origem {
    pub fn marca(&self) -> &'static str {
        match self {
            Origem::Publicado => "",
            Origem::Regra => "regra",
            Origem::Informado => "informado",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evento {
    pub data: Data,
    /// `String` e não `&'static str` porque o calendário é gravado em disco: a home
    /// mostra o que já foi buscado, e ela não busca nada — ver `Cache::ja_tem`.
    pub pais: String,
    pub titulo: String,
    /// Quanto o mercado costuma reagir, de 1 a 3 — a mesma ideia das «estrelinhas» de um
    /// calendário econômico. Aqui é uma classificação nossa, dos indicadores que a lista
    /// conhece, e não um número que alguma fonte publique.
    pub peso: u8,
    pub origem: Origem,
}

/// Os produtos do IBGE que movem preço, e o peso de cada um.
///
/// É uma lista curada e é preciso que seja: o calendário do IBGE traz mais de duas mil
/// divulgações, a esmagadora maioria sem efeito nenhum sobre mercado — mapa de relevo,
/// abate de animais, custos da construção. Sem o filtro, o que importa some no meio.
const RELEVANTES: &[(&str, u8)] = &[
    ("Índice Nacional de Preços ao Consumidor Amplo", 3),
    ("Sistema de Contas Nacionais Trimestrais", 3),
    ("Pesquisa Nacional por Amostra de Domicílios Contínua", 3),
    ("Pesquisa Industrial Mensal", 2),
    ("Pesquisa Mensal de Comércio", 2),
    ("Pesquisa Mensal de Serviços", 2),
    ("Índice Nacional de Preços ao Consumidor Amplo 15", 2),
];

/// O peso de uma divulgação do IBGE, ou `None` quando ela não é das que interessam.
fn peso_ibge(titulo: &str) -> Option<u8> {
    RELEVANTES
        .iter()
        // Do mais específico para o menos: «IPCA-15» tem que ganhar de «IPCA», senão os
        // dois casariam com a mesma entrada e o peso sairia errado.
        .filter(|(nome, _)| titulo.starts_with(nome))
        .max_by_key(|(nome, _)| nome.len())
        .map(|(_, peso)| *peso)
}

/// `29/12/2026 12:00:00` → data.
fn data_ibge(texto: &str) -> Option<Data> {
    let mut p = texto.split(['/', ' ']);
    let dia: u32 = p.next()?.parse().ok()?;
    let mes: u32 = p.next()?.parse().ok()?;
    let ano: i32 = p.next()?.parse().ok()?;
    Some(Data { ano, mes, dia })
}

/// A primeira sexta-feira do mês — quando sai o payroll americano. É definição, e por isso
/// entra como `Origem::Regra` em vez de estimativa.
pub fn primeira_sexta(ano: i32, mes: u32) -> Data {
    let primeiro = Data { ano, mes, dia: 1 };
    // 5 = sexta-feira, com 0 = domingo.
    let avanca = (5 + 7 - primeiro.dia_da_semana()) % 7;
    Data {
        ano,
        mes,
        dia: 1 + avanca,
    }
}

/// Os eventos que se conhecem por regra, sem consultar ninguém.
pub fn por_regra(de: &Data, meses: u32) -> Vec<Evento> {
    let mut saida = Vec::new();
    let mut mes = *de;
    for _ in 0..meses.max(1) {
        let sexta = primeira_sexta(mes.ano, mes.mes);
        saida.push(Evento {
            data: sexta,
            pais: "EUA".into(),
            titulo: "Payroll — relatório de emprego".to_string(),
            peso: 3,
            origem: Origem::Regra,
        });
        mes = mes.mes_seguinte();
    }
    saida
}

/// O calendário buscado em segundo plano.
#[derive(Default)]
pub struct Cache {
    eventos: Arc<Mutex<Vec<Evento>>>,
    erro: Arc<Mutex<Option<String>>>,
    buscando: Arc<AtomicBool>,
    pedido: Arc<AtomicBool>,
}

impl Cache {
    /// Os eventos publicados que já chegaram, pedindo a busca na primeira vez.
    pub fn get(&self, de: &Data, ate: &Data) -> Vec<Evento> {
        if !self.pedido.swap(true, Ordering::Relaxed) {
            self.buscar(*de, *ate);
        }
        self.eventos.lock().map(|e| e.clone()).unwrap_or_default()
    }

    /// O que já está em memória, **sem disparar busca nenhuma**.
    ///
    /// É o que a home usa. `get` dispara a busca na primeira chamada, e a home não pode
    /// disparar busca: a regra da aba é que nada vai à rede até um módulo abrir.
    pub fn ja_tem(&self) -> Vec<Evento> {
        self.eventos.lock().map(|e| e.clone()).unwrap_or_default()
    }

    /// Semeia o cache com o que ficou gravado da sessão passada.
    ///
    /// Sem isto, a agenda da home ficaria vazia até alguém abrir o módulo — e voltaria a
    /// ficar vazia no próximo início. Um calendário econômico muda uma vez por mês.
    pub fn semear(&self, eventos: Vec<Evento>) {
        if eventos.is_empty() {
            return;
        }
        if let Ok(mut e) = self.eventos.lock() {
            *e = eventos;
        }
    }

    pub fn buscando(&self) -> bool {
        self.buscando.load(Ordering::Relaxed)
    }

    pub fn erro(&self) -> Option<String> {
        self.erro.lock().ok().and_then(|e| e.clone())
    }

    fn buscar(&self, de: Data, ate: Data) {
        let eventos = Arc::clone(&self.eventos);
        let erro = Arc::clone(&self.erro);
        let buscando = Arc::clone(&self.buscando);
        buscando.store(true, Ordering::Relaxed);
        thread::Builder::new()
            .name("invest-calendario".into())
            .spawn(move || {
                let url = format!(
                    "https://servicodados.ibge.gov.br/api/v3/calendario/?de={:04}-{:02}-{:02}&ate={:04}-{:02}-{:02}&qtd=200",
                    de.ano, de.mes, de.dia, ate.ano, ate.mes, ate.dia
                );
                match feed::get_json(&url) {
                    Ok(valor) => {
                        let lidos = ler_ibge(&valor);
                        if let Ok(mut e) = eventos.lock() {
                            *e = lidos;
                        }
                        if let Ok(mut x) = erro.lock() {
                            *x = None;
                        }
                    }
                    Err(e) => {
                        if let Ok(mut x) = erro.lock() {
                            *x = Some(format!("IBGE: {}", e.frase()));
                        }
                    }
                }
                buscando.store(false, Ordering::Relaxed);
            })
            .ok();
    }
}

/// Lê a resposta do IBGE, ficando só com as divulgações que movem preço.
pub fn ler_ibge(valor: &Value) -> Vec<Evento> {
    let Some(itens) = valor.get("items").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut saida: Vec<Evento> = itens
        .iter()
        .filter_map(|i| {
            let titulo = i.get("titulo")?.as_str()?;
            let peso = peso_ibge(titulo)?;
            Some(Evento {
                data: data_ibge(i.get("data_divulgacao")?.as_str()?)?,
                pais: "Brasil".into(),
                titulo: titulo.to_string(),
                peso,
                origem: Origem::Publicado,
            })
        })
        .collect();
    // O IBGE repete a mesma divulgação em entradas diferentes do mesmo produto.
    saida.sort_by(|a, b| a.data.cmp(&b.data).then_with(|| a.titulo.cmp(&b.titulo)));
    saida.dedup_by(|a, b| a.data == b.data && a.titulo == b.titulo);
    saida
}

/// A janela padrão que a agenda mostra.
pub fn janela(hoje: &Data, dias: i64) -> (Data, Data) {
    let fim = tempo::data_de_dias(tempo::dias_de(hoje.ano, hoje.mes, hoje.dia) + dias);
    (*hoje, fim)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_primeira_sexta_e_calculada_e_nao_estimada() {
        // Setembro de 2026 começa numa terça; a primeira sexta é dia 4.
        let s = primeira_sexta(2026, 9);
        assert_eq!(s.dia, 4);
        assert_eq!(s.dia_da_semana(), 5);
        // E quando o mês já começa numa sexta, é o dia 1.
        for (ano, mes) in [(2026, 1), (2026, 5), (2027, 10)] {
            let s = primeira_sexta(ano, mes);
            assert_eq!(s.dia_da_semana(), 5, "{ano}-{mes} não deu numa sexta");
            assert!(s.dia <= 7, "a primeira sexta não pode passar do dia 7");
        }
    }

    #[test]
    fn so_o_que_move_preco_entra() {
        // O calendário do IBGE traz mais de duas mil divulgações; sem o filtro, o IPCA
        // some no meio de mapa de relevo e abate de animais.
        assert_eq!(
            peso_ibge("Índice Nacional de Preços ao Consumidor Amplo"),
            Some(3)
        );
        assert_eq!(
            peso_ibge("Sistema de Contas Nacionais Trimestrais"),
            Some(3)
        );
        assert_eq!(
            peso_ibge("Mapa dos Macrocompartimentos de Relevo do Brasil"),
            None
        );
        assert_eq!(
            peso_ibge("Pesquisas Trimestrais do Abate de Animais, do Leite"),
            None
        );
    }

    #[test]
    fn o_mais_especifico_ganha() {
        // «IPCA-15» tem que ganhar de «IPCA», senão os dois casam e o peso sai errado.
        assert_eq!(
            peso_ibge("Índice Nacional de Preços ao Consumidor Amplo 15"),
            Some(2)
        );
    }

    #[test]
    fn a_data_do_ibge_e_lida() {
        let d = data_ibge("29/12/2026 12:00:00").unwrap();
        assert_eq!((d.ano, d.mes, d.dia), (2026, 12, 29));
        assert_eq!(data_ibge("não é data"), None);
    }

    #[test]
    fn divulgacoes_repetidas_aparecem_uma_vez() {
        let bruto: Value = serde_json::from_str(
            r#"{"items":[
                {"titulo":"Índice Nacional de Preços ao Consumidor Amplo","data_divulgacao":"10/10/2026 09:00:00"},
                {"titulo":"Índice Nacional de Preços ao Consumidor Amplo","data_divulgacao":"10/10/2026 09:00:00"},
                {"titulo":"Mapa de Relevo","data_divulgacao":"11/10/2026 09:00:00"}
            ]}"#,
        )
        .unwrap();
        let e = ler_ibge(&bruto);
        assert_eq!(e.len(), 1, "a repetição some e o irrelevante não entra");
        assert_eq!(e[0].peso, 3);
        assert_eq!(e[0].origem, Origem::Publicado);
    }
}
