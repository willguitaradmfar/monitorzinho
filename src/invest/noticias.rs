//! As manchetes dos feeds, no estado da aba e não dentro da tela que as mostra.
//!
//! Mora aqui pelo mesmo motivo do calendário econômico: **a home também as mostra**. Um
//! cache dentro da vista do módulo só existe enquanto o módulo está aberto, e o cartão da
//! home ficava listando os endereços dos feeds — o que se tem, não o que eles disseram.
//!
//! E é gravado em disco, porque a alternativa era um cartão vazio até alguém entrar no
//! módulo, e vazio de novo no próximo início.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::invest::feed;
use crate::invest::rss::{self, Item};

/// De quanto em quanto tempo os feeds são relidos.
///
/// Cinco minutos. Notícia não é preço: buscar de dois em dois segundos seria gastar banda
/// alheia para receber a mesma lista.
const INTERVALO: u64 = 300;

/// Quantos itens ficam guardados no total. Quatro feeds cheios cabem, e o que passa disso
/// é história antiga que ninguém rola até.
const TETO: usize = rss::MAX_ITENS * 4;

#[derive(Default)]
pub struct Cache {
    itens: Arc<Mutex<Vec<(String, Item)>>>,
    erros: Arc<Mutex<Vec<String>>>,
    buscado_em: AtomicU64,
}

impl Cache {
    /// O que já chegou, **sem buscar nada**. É o que a home usa.
    pub fn ja_tem(&self) -> Vec<(String, Item)> {
        self.itens.lock().map(|i| i.clone()).unwrap_or_default()
    }

    /// O mesmo, pedindo a busca quando ela está vencida. É o que o módulo aberto usa.
    pub fn get(&self, feeds: &[String], agora: u64) -> Vec<(String, Item)> {
        self.buscar(feeds, agora);
        self.ja_tem()
    }

    pub fn erros(&self) -> Vec<String> {
        self.erros.lock().map(|e| e.clone()).unwrap_or_default()
    }

    /// Semeia com o que ficou gravado da sessão passada.
    pub fn semear(&self, itens: Vec<(String, Item)>) {
        if itens.is_empty() {
            return;
        }
        if let Ok(mut i) = self.itens.lock() {
            *i = itens;
        }
    }

    /// Força a próxima leitura a acontecer — usado quando a lista de feeds muda.
    pub fn invalidar(&self) {
        self.buscado_em.store(0, Ordering::Relaxed);
    }

    fn buscar(&self, feeds: &[String], agora: u64) {
        let ultimo = self.buscado_em.load(Ordering::Relaxed);
        if ultimo != 0 && agora.saturating_sub(ultimo) < INTERVALO {
            return;
        }
        self.buscado_em.store(agora, Ordering::Relaxed);
        // Numa thread, como toda leitura de rede desta aba: um feed que não responde não
        // pode congelar o desenho pelo tempo do timeout.
        for url in feeds {
            let url = url.clone();
            let itens = Arc::clone(&self.itens);
            let erros = Arc::clone(&self.erros);
            thread::Builder::new()
                .name("invest-rss".into())
                .spawn(move || {
                    let nome = url.split('/').nth(2).unwrap_or(&url).replace("www.", "");
                    let anotar = |e: &Mutex<Vec<String>>, msg: Option<String>| {
                        if let Ok(mut e) = e.lock() {
                            e.retain(|x| !x.starts_with(&nome));
                            if let Some(msg) = msg {
                                e.push(format!("{nome}: {msg}"));
                            }
                        }
                    };
                    match feed::get(&url) {
                        Ok(r) => {
                            let texto = crate::invest::csv::decodificar(&r.body);
                            let novos = rss::parse(&texto);
                            // Um feed que responde lixo é marcado como quebrado e os
                            // outros seguem — um feed ruim não derruba o módulo.
                            match novos.is_empty() {
                                true => anotar(&erros, Some("respondeu sem itens legíveis".into())),
                                false => anotar(&erros, None),
                            }
                            if let Ok(mut lista) = itens.lock() {
                                for item in novos {
                                    if !lista.iter().any(|(_, i)| i.titulo == item.titulo) {
                                        lista.push((nome.clone(), item));
                                    }
                                }
                                let n = lista.len();
                                if n > TETO {
                                    lista.drain(..n - TETO);
                                }
                            }
                            // Só a volta boa carimba. Um feed que caiu não deixou a
                            // lista mais nova, e dizer «atualizado agora» sobre ela
                            // seria o engano que o carimbo existe para desfazer.
                            crate::invest::store::buscas()
                                .carimbar(crate::invest::store::fonte::NOTICIA);
                        }
                        Err(erro) => anotar(&erros, Some(erro.frase())),
                    }
                })
                .ok();
        }
    }
}

/// Os itens ordenados como a tela os mostra: **do mais recente para o mais antigo, sem
/// separar por feed**.
///
/// Uma ordem por feed põe uma notícia de ontem acima de uma de agora só porque veio de
/// outro endereço. Um item sem data legível vai para o fim, em vez de fingir uma posição.
pub fn recentes(itens: &[(String, Item)]) -> Vec<usize> {
    let mut v: Vec<usize> = (0..itens.len()).collect();
    v.sort_by_key(|&i| std::cmp::Reverse(itens[i].1.em.unwrap_or(0)));
    v
}
