//! O registro de módulos. Espelha `tools::all_tools()`, pelo mesmo motivo que aquele
//! existe: uma lista num lugar só, e um módulo novo é uma linha nela.

pub mod agenda;
pub mod alertas;
pub mod alocacao;
pub mod book;
pub mod cambio;
pub mod comparador;
pub mod comum;
pub mod correlacao;
pub mod corretoras;
pub mod cotacoes;
pub mod cripto;
pub mod fita;
pub mod fundamentos;
pub mod grafico;
pub mod heatmap;
pub mod importacao;
pub mod imposto;
pub mod indicadores;
pub mod indices;
pub mod lancamentos;
pub mod mercado;
pub mod metas;
pub mod noticias;
pub mod patrimonio;
pub mod posicoes;
pub mod proventos;
pub mod rebalanceamento;
pub mod renda_fixa;
pub mod risco;
pub mod simulador;

use crate::invest::module::InvestModule;

/// A leitura de uma data escrita por gente, compartilhada por quem pede uma. Uma cópia só
/// para que «dd/mm/aaaa» signifique a mesma coisa em toda tela.
pub use lancamentos::ler_data as lancamentos_data;

/// A ordem aqui é a ordem em que os módulos aparecem para quem nunca abriu nenhum — e é
/// a ordem em que alguém constrói o uso: primeiro o que é meu, depois o que está
/// acontecendo, depois o que isso quer dizer.
pub fn todos() -> Vec<Box<dyn InvestModule>> {
    vec![
        // Carteira
        Box::new(posicoes::Posicoes),
        Box::new(alocacao::Alocacao),
        Box::new(rebalanceamento::Rebalanceamento),
        Box::new(patrimonio::Patrimonio),
        Box::new(corretoras::Corretoras),
        Box::new(proventos::Proventos),
        Box::new(lancamentos::Lancamentos),
        // Mercado
        Box::new(cotacoes::Cotacoes),
        Box::new(cambio::Cambio),
        Box::new(renda_fixa::RendaFixa),
        Box::new(cripto::Cripto),
        Box::new(grafico::Grafico),
        Box::new(indices::Indices),
        Box::new(heatmap::Heatmap),
        Box::new(fita::Fita),
        Box::new(book::Book),
        // Análise
        Box::new(risco::Risco),
        Box::new(correlacao::Correlacao),
        Box::new(comparador::Comparador),
        Box::new(indicadores::Indicadores),
        Box::new(simulador::Simulador),
        Box::new(fundamentos::Fundamentos),
        // Operação
        Box::new(importacao::Importacao),
        Box::new(imposto::Imposto),
        Box::new(metas::Metas),
        // Informação
        Box::new(alertas::Alertas),
        Box::new(noticias::Noticias),
        Box::new(agenda::Agenda),
    ]
}
