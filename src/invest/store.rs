//! O que fica no banco, e as duas regras que não se negociam.
//!
//! **A gravação é atômica.** Um histórico de gráfico truncado por queda de energia é um
//! gráfico feio; uma carteira truncada é o registro do patrimônio de alguém. Antes isso
//! era temporário ao lado, `fsync` e `rename` por cima, à mão. Agora é uma transação:
//! ou as onze tabelas da carteira mudaram juntas, ou nenhuma mudou — e a garantia passou
//! a valer para cada uma delas, e não só para o arquivo inteiro de uma vez.
//!
//! **A estrutura é estrutura, e não um JSON numa coluna.** Uma posição tem colunas de
//! posição, um provento tem colunas de provento. O que isso compra é a pergunta: quanto
//! entrou de provento por ativo neste ano, que linhas estão sem preço médio informado,
//! quais alertas já dispararam — tudo `SELECT`, sem passar por este arquivo.
//!
//! **O preço médio continua informado, nunca calculado.** Não há nada aqui que o derive
//! de lançamentos, e a coluna `preco_medio` só recebe o que a importação ou a edição
//! puseram nela. Ver `docs/invest/03`.

use std::collections::BTreeMap;
use std::sync::Mutex;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db;
use crate::invest::model::{
    Alerta, AlvoCarteira, AssetId, Carteira, Lancamento, Moeda, Portfolio, Position, Provento,
};

pub use crate::db::agora;

/// A carteira lida, e em que condições ela foi lida.
pub struct Loaded {
    pub portfolio: Portfolio,
    /// Quando `true`, gravar está proibido porque o arquivo do perfil não aceita escrita
    /// — um pendrive montado somente para leitura, um `chmod` de quem foi fazer backup.
    /// A tela diz isso em vez de deixar alguém editar durante meia hora e descobrir
    /// depois que nada foi gravado.
    pub somente_leitura: bool,
}

const CHAVE_MOEDA_BASE: &str = "invest.moeda_base";

pub fn load() -> Loaded {
    let portfolio = db::ler(Portfolio::default(), ler_portfolio);
    Loaded {
        portfolio,
        somente_leitura: db::somente_leitura(),
    }
}

fn ler_portfolio(conn: &Connection) -> rusqlite::Result<Portfolio> {
    let mut p = Portfolio {
        moeda_base: db::config_de(conn, CHAVE_MOEDA_BASE)
            .and_then(|m| Moeda::parse(&m))
            .unwrap_or(Moeda::Brl),
        ..Portfolio::default()
    };

    // Uma linha cujo ativo este binário não sabe ler — um mercado que só existe numa
    // versão mais nova — é pulada, e não derruba a leitura das outras. É a mesma regra
    // que o JSON seguia com um campo desconhecido: uma atualização nunca rejeita o
    // arquivo antigo por inteiro, e um downgrade não pode custar a carteira.
    let mut posicoes = conn.prepare(
        "SELECT fonte, conta, ativo, classe, quantidade, preco_medio, moeda,
                preco_manual, preco_manual_em, atualizado_em, carteira
         FROM posicao ORDER BY id",
    )?;
    p.posicoes = posicoes
        .query_map([], |row| {
            let Some(ativo) = AssetId::parse(&row.get::<_, String>(2)?).ok() else {
                return Ok(None);
            };
            let (Some(classe), Some(moeda)) = (
                db::do_codigo(&row.get::<_, String>(3)?),
                db::do_codigo(&row.get::<_, String>(6)?),
            ) else {
                return Ok(None);
            };
            Ok(Some(Position {
                fonte: row.get(0)?,
                conta: row.get(1)?,
                ativo,
                classe,
                quantidade: row.get(4)?,
                preco_medio: row.get(5)?,
                moeda,
                preco_manual: row.get(7)?,
                preco_manual_em: row.get::<_, Option<i64>>(8)?.map(|v| v as u64),
                atualizado_em: row.get::<_, i64>(9)? as u64,
                carteira: row.get(10)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    let mut watchlist = conn.prepare("SELECT ativo FROM watchlist ORDER BY ordem")?;
    p.watchlist = watchlist
        .query_map([], |row| Ok(AssetId::parse(&row.get::<_, String>(0)?).ok()))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    let mut alvos = conn.prepare("SELECT classe, percentual FROM alvo")?;
    p.alvos = alvos
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut setores = conn.prepare("SELECT ativo, setor FROM setor")?;
    p.setores = setores
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let mut feeds = conn.prepare("SELECT url FROM feed ORDER BY ordem")?;
    p.feeds = feeds
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut mapeamentos = conn.prepare("SELECT fonte, campo, coluna FROM mapeamento")?;
    for linha in mapeamentos.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })? {
        let (fonte, campo, coluna) = linha?;
        p.mapeamentos
            .entry(fonte)
            .or_default()
            .insert(campo, coluna);
    }

    let mut proventos = conn.prepare(
        "SELECT ativo, tipo, pago_em, bruto, retido, moeda, quantidade
         FROM provento ORDER BY id",
    )?;
    p.proventos = proventos
        .query_map([], |row| {
            let (Ok(ativo), Some(tipo), Some(moeda)) = (
                AssetId::parse(&row.get::<_, String>(0)?),
                db::do_codigo(&row.get::<_, String>(1)?),
                db::do_codigo(&row.get::<_, String>(5)?),
            ) else {
                return Ok(None);
            };
            Ok(Some(Provento {
                ativo,
                tipo,
                pago_em: row.get::<_, i64>(2)? as u64,
                bruto: row.get(3)?,
                retido: row.get(4)?,
                moeda,
                quantidade: row.get(6)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    let mut lancamentos = conn.prepare(
        "SELECT em, tipo, ativo, quantidade, preco, taxas, valor, moeda, nota
         FROM lancamento ORDER BY id",
    )?;
    p.lancamentos = lancamentos
        .query_map([], |row| {
            let Some(tipo) = db::do_codigo(&row.get::<_, String>(1)?) else {
                return Ok(None);
            };
            Ok(Some(Lancamento {
                em: row.get::<_, i64>(0)? as u64,
                tipo,
                ativo: row
                    .get::<_, Option<String>>(2)?
                    .and_then(|t| AssetId::parse(&t).ok()),
                quantidade: row.get(3)?,
                preco: row.get(4)?,
                taxas: row.get(5)?,
                valor: row.get(6)?,
                moeda: row
                    .get::<_, Option<String>>(7)?
                    .and_then(|m| db::do_codigo(&m)),
                nota: row.get(8)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    let mut alertas = conn.prepare(
        "SELECT ativo, regra, valor, ligado, armado, ultimo_disparo FROM alerta ORDER BY id",
    )?;
    p.alertas = alertas
        .query_map([], |row| {
            let (Ok(ativo), Some(regra)) = (
                AssetId::parse(&row.get::<_, String>(0)?),
                db::do_codigo(&row.get::<_, String>(1)?),
            ) else {
                return Ok(None);
            };
            Ok(Some(Alerta {
                ativo,
                regra,
                valor: row.get(2)?,
                ligado: row.get::<_, i64>(3)? != 0,
                armado: row.get::<_, i64>(4)? != 0,
                ultimo_disparo: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect();

    let mut carteiras = conn.prepare("SELECT nome FROM carteira ORDER BY ordem")?;
    let nomes: Vec<String> = carteiras
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut itens = conn.prepare(
        "SELECT ativo, ordem, percentual, teto FROM carteira_alvo
         WHERE carteira = ?1 ORDER BY ordem, id",
    )?;
    for nome in nomes {
        let alvos: Vec<AlvoCarteira> = itens
            .query_map([&nome], |row| {
                let Ok(ativo) = AssetId::parse(&row.get::<_, String>(0)?) else {
                    return Ok(None);
                };
                Ok(Some(AlvoCarteira {
                    ativo,
                    ordem: row.get::<_, i64>(1)? as u32,
                    percentual: row.get(2)?,
                    teto: row.get(3)?,
                }))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        p.carteiras.push(Carteira { nome, alvos });
    }

    Ok(p)
}

/// Grava a carteira inteira, numa transação só.
///
/// As tabelas são reescritas do zero em vez de comparadas linha a linha. São dezenas ou
/// centenas de linhas dentro de uma transação — microssegundos —, e o que se ganha é que
/// não existe caminho pelo qual o banco fique diferente do que está na memória: o que
/// está na memória é o que está no banco, sempre, e não «o que está na memória mais as
/// diferenças que alguém lembrou de aplicar».
pub fn save(portfolio: &Portfolio) -> Result<(), String> {
    match db::escrever(|conn| gravar_em(conn, portfolio)) {
        true => Ok(()),
        false => Err("não deu para gravar a carteira no banco do perfil".to_string()),
    }
}

/// O corpo da gravação, contra uma conexão qualquer — é o que deixa o teste de ida e
/// volta rodar contra um banco em memória em vez de contra o do usuário.
fn gravar_em(conn: &Connection, portfolio: &Portfolio) -> rusqlite::Result<()> {
    {
        db::config_por(conn, CHAVE_MOEDA_BASE, portfolio.moeda_base.code())?;

        for tabela in [
            "posicao",
            "watchlist",
            "alvo",
            "setor",
            "feed",
            "mapeamento",
            "provento",
            "lancamento",
            "alerta",
            "carteira_alvo",
            "carteira",
        ] {
            db::limpar(conn, tabela)?;
        }

        let mut posicao = conn.prepare(
            "INSERT INTO posicao (id, fonte, conta, ativo, classe, quantidade, preco_medio,
                                  moeda, preco_manual, preco_manual_em, atualizado_em, carteira)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        )?;
        for (i, p) in portfolio.posicoes.iter().enumerate() {
            posicao.execute((
                i as i64 + 1,
                &p.fonte,
                &p.conta,
                p.ativo.to_string(),
                db::codigo(&p.classe),
                p.quantidade,
                p.preco_medio,
                db::codigo(&p.moeda),
                p.preco_manual,
                p.preco_manual_em.map(|v| v as i64),
                p.atualizado_em as i64,
                p.carteira.as_deref(),
            ))?;
        }

        let mut watchlist =
            conn.prepare("INSERT OR REPLACE INTO watchlist (ativo, ordem) VALUES (?1, ?2)")?;
        for (i, ativo) in portfolio.watchlist.iter().enumerate() {
            watchlist.execute((ativo.to_string(), i as i64))?;
        }

        let mut alvo = conn.prepare("INSERT INTO alvo (classe, percentual) VALUES (?1, ?2)")?;
        for (classe, pct) in &portfolio.alvos {
            alvo.execute((classe, pct))?;
        }

        let mut setor = conn.prepare("INSERT INTO setor (ativo, setor) VALUES (?1, ?2)")?;
        for (ativo, nome) in &portfolio.setores {
            setor.execute((ativo, nome))?;
        }

        let mut feed = conn.prepare("INSERT OR REPLACE INTO feed (url, ordem) VALUES (?1, ?2)")?;
        for (i, url) in portfolio.feeds.iter().enumerate() {
            feed.execute((url, i as i64))?;
        }

        let mut mapeamento =
            conn.prepare("INSERT INTO mapeamento (fonte, campo, coluna) VALUES (?1, ?2, ?3)")?;
        for (fonte, campos) in &portfolio.mapeamentos {
            for (campo, coluna) in campos {
                mapeamento.execute((fonte, campo, coluna))?;
            }
        }

        let mut provento = conn.prepare(
            "INSERT INTO provento (id, ativo, tipo, pago_em, bruto, retido, moeda, quantidade)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (i, v) in portfolio.proventos.iter().enumerate() {
            provento.execute((
                i as i64 + 1,
                v.ativo.to_string(),
                db::codigo(&v.tipo),
                v.pago_em as i64,
                v.bruto,
                v.retido,
                db::codigo(&v.moeda),
                v.quantidade,
            ))?;
        }

        let mut lancamento = conn.prepare(
            "INSERT INTO lancamento (id, em, tipo, ativo, quantidade, preco, taxas, valor,
                                     moeda, nota)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for (i, l) in portfolio.lancamentos.iter().enumerate() {
            lancamento.execute((
                i as i64 + 1,
                l.em as i64,
                db::codigo(&l.tipo),
                l.ativo.as_ref().map(|a| a.to_string()),
                l.quantidade,
                l.preco,
                l.taxas,
                l.valor,
                l.moeda.as_ref().map(db::codigo),
                &l.nota,
            ))?;
        }

        let mut alerta = conn.prepare(
            "INSERT INTO alerta (id, ativo, regra, valor, ligado, armado, ultimo_disparo)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (i, a) in portfolio.alertas.iter().enumerate() {
            alerta.execute((
                i as i64 + 1,
                a.ativo.to_string(),
                db::codigo(&a.regra),
                a.valor,
                a.ligado as i64,
                a.armado as i64,
                a.ultimo_disparo.map(|v| v as i64),
            ))?;
        }

        let mut carteira =
            conn.prepare("INSERT OR REPLACE INTO carteira (nome, ordem) VALUES (?1, ?2)")?;
        let mut item = conn.prepare(
            "INSERT INTO carteira_alvo (id, carteira, ativo, ordem, percentual, teto)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        let mut proximo_alvo = 0i64;
        for (i, c) in portfolio.carteiras.iter().enumerate() {
            carteira.execute((&c.nome, i as i64))?;
            for a in &c.alvos {
                proximo_alvo += 1;
                item.execute((
                    proximo_alvo,
                    &c.nome,
                    a.ativo.to_string(),
                    a.ordem as i64,
                    a.percentual,
                    a.teto,
                ))?;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// A ordem da lista de módulos: id → epoch da última abertura.
// ---------------------------------------------------------------------------

pub type Mru = BTreeMap<String, u64>;

/// Perder isto custa a ordem da lista até o próximo uso, e nada mais.
pub fn load_mru() -> Mru {
    db::ler(Mru::new(), |conn| {
        let mut stmt = conn.prepare("SELECT id, aberto_em FROM modulo_mru")?;
        let linhas = stmt.query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?;
        linhas.collect()
    })
}

pub fn save_mru(mru: &Mru) {
    db::escrever(|conn| {
        let mut stmt = conn.prepare(
            "INSERT INTO modulo_mru (id, aberto_em) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET aberto_em = excluded.aberto_em",
        )?;
        for (id, em) in mru {
            stmt.execute((id, *em as i64))?;
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// O cache de mercado: abrir a aba mostrando números, e não traços.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedQuote {
    pub preco: f64,
    pub em: u64,
    /// Quando esta leitura foi **gravada**, que é outra coisa que `em`. `em` é a hora do
    /// dado — o dia de referência de uma série do Banco Central é do mês passado, e está
    /// certo. Isto é a hora em que ela entrou, e é o que responde «este cache é de
    /// quando?» antes de a busca dar a primeira volta.
    ///
    /// `None` no que foi gravado por uma versão que ainda não anotava isso.
    #[serde(default)]
    pub gravado_em: Option<u64>,
    #[serde(default)]
    pub anterior: Option<f64>,
    #[serde(default)]
    pub grade: String,
    #[serde(default)]
    pub moeda: String,
}

pub type Cache = BTreeMap<String, CachedQuote>;

/// A partir de que idade um **preço** em cache deixa de informar e passa a enganar.
pub const CACHE_MAX_IDADE: u64 = 24 * 60 * 60;

/// O mesmo, para uma **série publicada**. Cento e vinte dias.
///
/// Uma série do Banco Central não envelhece como um preço: o IPCA de julho é o IPCA
/// corrente até o de agosto sair, e ele nasce com semanas de idade porque o carimbo é o
/// **primeiro dia do mês de referência**, não o da busca.
///
/// O número não é redondo por acaso. Medido em 08/09/2026, o IPCA mais recente que o SGS
/// servia estava carimbado em 01/07 — **setenta dias**, e ele ainda era o corrente. Um
/// teto de sessenta dias descartava a leitura válida; um mensal pode chegar a uns setenta
/// e cinco dias de carimbo pouco antes de o próximo sair. Cento e vinte dá folga para
/// isso e ainda descarta uma série que a fonte tenha parado de publicar.
pub const CACHE_MAX_IDADE_SERIE: u64 = 120 * 24 * 60 * 60;

/// Quanto tempo um dado em cache continua valendo, pelo que ele é.
///
/// A regra ser única era um bug: as quatro linhas do Banco Central eram descartadas em
/// **toda** abertura do programa, e voltavam a «buscando…» até a thread chegar nelas —
/// dezenas de segundos depois, porque o provedor do BCB é o sexto da fila. O usuário via
/// «buscando eternamente» e estava certo em achar estranho.
pub fn validade(grade: &str) -> u64 {
    match grade {
        // Publicado: vale até o próximo sair.
        "fechamento" => CACHE_MAX_IDADE_SERIE,
        // Preço. Um de ontem numa tela de mercado não é um dado antigo: é um dado errado
        // com cara de atual.
        _ => CACHE_MAX_IDADE,
    }
}

/// Lê o cache, **descartando o que está velho demais** — pelo critério de cada tipo de
/// dado, ver `validade`. O descarte é no `WHERE`, e não numa passada depois: uma linha
/// vencida não chega a virar objeto.
pub fn load_cache() -> Cache {
    let agora = agora() as i64;
    db::ler(Cache::new(), |conn| {
        let mut stmt = conn.prepare(
            "SELECT ativo, preco, anterior, em, grade, moeda, gravado_em FROM cotacao
             WHERE ?1 - em <= CASE grade WHEN 'fechamento' THEN ?2 ELSE ?3 END",
        )?;
        // Os dois tetos vêm de `validade`, e não repetidos aqui: o `CASE` do SQL escolhe
        // qual dos dois se aplica, mas quanto vale cada um continua sendo decidido num
        // lugar só.
        let linhas = stmt.query_map(
            (
                agora,
                validade("fechamento") as i64,
                validade("ao_vivo") as i64,
            ),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    CachedQuote {
                        preco: row.get(1)?,
                        anterior: row.get(2)?,
                        em: row.get::<_, i64>(3)? as u64,
                        grade: row.get(4)?,
                        moeda: row.get(5)?,
                        gravado_em: row.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                    },
                ))
            },
        )?;
        linhas.collect()
    })
}

pub fn save_cache(cache: &Cache) {
    db::escrever(|conn| {
        let mut stmt = conn.prepare(
            "INSERT INTO cotacao (ativo, preco, anterior, em, grade, moeda, gravado_em)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(ativo) DO UPDATE SET
                 preco = excluded.preco, anterior = excluded.anterior,
                 em = excluded.em, grade = excluded.grade, moeda = excluded.moeda,
                 gravado_em = excluded.gravado_em",
        )?;
        for (ativo, q) in cache {
            if !q.preco.is_finite() {
                continue;
            }
            stmt.execute((
                ativo,
                q.preco,
                q.anterior.filter(|v| v.is_finite()),
                q.em as i64,
                &q.grade,
                &q.moeda,
                q.gravado_em.map(|v| v as i64),
            ))?;
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// Os caches que existem porque a home mostra e a home não busca nada
// ---------------------------------------------------------------------------

/// O calendário econômico já buscado.
///
/// Gravado porque a home mostra a agenda e **a home não busca nada**: sem isto ela
/// ficaria vazia até alguém abrir o módulo, e voltaria a ficar vazia no próximo início.
/// Um calendário econômico muda uma vez por mês; relê-lo do banco é grátis.
pub fn load_agenda() -> Vec<crate::invest::calendario::Evento> {
    use crate::invest::calendario::Evento;
    use crate::invest::tempo::Data;
    db::ler(Vec::new(), |conn| {
        let mut stmt = conn
            .prepare("SELECT ano, mes, dia, pais, titulo, peso, origem FROM agenda ORDER BY id")?;
        let linhas = stmt.query_map([], |row| {
            let Some(origem) = db::do_codigo(&row.get::<_, String>(6)?) else {
                return Ok(None);
            };
            Ok(Some(Evento {
                data: Data {
                    ano: row.get::<_, i64>(0)? as i32,
                    mes: row.get::<_, i64>(1)? as u32,
                    dia: row.get::<_, i64>(2)? as u32,
                },
                pais: row.get(3)?,
                titulo: row.get(4)?,
                peso: row.get::<_, i64>(5)? as u8,
                origem,
            }))
        })?;
        Ok(linhas
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    })
}

pub fn save_agenda(eventos: &[crate::invest::calendario::Evento]) {
    // Vazio não apaga o que já está lá: uma busca que não trouxe nada é uma busca que
    // falhou, e ela não pode custar o calendário que a home mostra.
    if eventos.is_empty() {
        return;
    }
    db::escrever(|conn| {
        db::limpar(conn, "agenda")?;
        let mut stmt = conn.prepare(
            "INSERT INTO agenda (id, ano, mes, dia, pais, titulo, peso, origem)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for (i, e) in eventos.iter().enumerate() {
            stmt.execute((
                i as i64 + 1,
                e.data.ano as i64,
                e.data.mes as i64,
                e.data.dia as i64,
                &e.pais,
                &e.titulo,
                e.peso as i64,
                db::codigo(&e.origem),
            ))?;
        }
        Ok(())
    });
}

/// As manchetes já lidas. Gravadas pelo mesmo motivo do calendário: o cartão da home as
/// mostra e nasceria vazio a cada início.
pub fn load_noticias() -> Vec<(String, crate::invest::rss::Item)> {
    use crate::invest::rss::Item;
    db::ler(Vec::new(), |conn| {
        let mut stmt =
            conn.prepare("SELECT fonte, titulo, link, data, resumo, em FROM noticia ORDER BY id")?;
        let linhas = stmt.query_map([], |row| {
            let data: String = row.get(3)?;
            // O instante é **relido do texto cru** quando o que está gravado é nulo.
            //
            // Não é redundância: o texto é o que o feed disse, e o número é o que a
            // versão daquele dia conseguiu entender dele. Quando o leitor aprende um
            // formato novo — e aprendeu, o `2026-09-10 12:08:07` sem `T` que 90% das
            // linhas usavam —, as já gravadas voltam a ter data sozinhas, sem esperar o
            // feed republicar nada.
            let em = row
                .get::<_, Option<i64>>(5)?
                .map(|v| v as u64)
                .or_else(|| crate::invest::rss::instante(&data));
            Ok((
                row.get::<_, String>(0)?,
                Item {
                    titulo: row.get(1)?,
                    link: row.get(2)?,
                    data,
                    resumo: row.get(4)?,
                    em,
                },
            ))
        })?;
        linhas.collect()
    })
}

pub fn save_noticias(itens: &[(String, crate::invest::rss::Item)]) {
    if itens.is_empty() {
        return;
    }
    db::escrever(|conn| {
        db::limpar(conn, "noticia")?;
        let mut stmt = conn.prepare(
            "INSERT INTO noticia (id, fonte, titulo, link, data, resumo, em)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for (i, (fonte, item)) in itens.iter().enumerate() {
            stmt.execute((
                i as i64 + 1,
                fonte,
                &item.titulo,
                &item.link,
                &item.data,
                &item.resumo,
                item.em.map(|v| v as i64),
            ))?;
        }
        Ok(())
    });
}

/// Os proventos anunciados já buscados. Gravados pelo mesmo motivo dos outros caches: a
/// lista de sugestões nasce cheia em vez de esperar dez buscas.
pub fn load_anunciados() -> Vec<crate::invest::provento::Anunciado> {
    use crate::invest::provento::Anunciado;
    db::ler(Vec::new(), |conn| {
        let mut stmt = conn.prepare("SELECT ativo, em, por_cota, dy FROM anunciado ORDER BY id")?;
        let linhas = stmt.query_map([], |row| {
            let Ok(ativo) = AssetId::parse(&row.get::<_, String>(0)?) else {
                return Ok(None);
            };
            Ok(Some(Anunciado {
                ativo,
                em: row.get::<_, i64>(1)? as u64,
                por_cota: row.get(2)?,
                dy: row.get(3)?,
            }))
        })?;
        Ok(linhas
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect())
    })
}

pub fn save_anunciados(v: &[crate::invest::provento::Anunciado]) {
    if v.is_empty() {
        return;
    }
    db::escrever(|conn| {
        db::limpar(conn, "anunciado")?;
        let mut stmt = conn.prepare(
            "INSERT INTO anunciado (id, ativo, em, por_cota, dy) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (i, a) in v.iter().enumerate() {
            stmt.execute((
                i as i64 + 1,
                a.ativo.to_string(),
                a.em as i64,
                a.por_cota,
                a.dy,
            ))?;
        }
        Ok(())
    });
}

// ---------------------------------------------------------------------------
// Há quanto tempo cada fonte externa foi consultada.
// ---------------------------------------------------------------------------

/// O nome de uma fonte externa no registro de buscas. Constantes e não literais soltos:
/// quem grava e quem lê têm que dizer a mesma palavra, e um erro de digitação aqui não
/// dá erro nenhum — só faz a tela dizer «nunca» para sempre.
pub mod fonte {
    pub const COTACAO: &str = "cotacao";
    pub const NOTICIA: &str = "noticia";
    pub const AGENDA: &str = "agenda";
    pub const ANUNCIADO: &str = "anunciado";
    pub const HISTORICO: &str = "historico";
    pub const FUNDAMENTO: &str = "fundamento";
}

/// Quando cada fonte externa respondeu pela última vez.
///
/// A pergunta que isto existe para responder é «este número é de agora ou de ontem?», e
/// ela não tinha resposta em lugar nenhum da tela: um preço em cache, uma manchete
/// guardada e um calendário de duas semanas atrás apareciam com exatamente a mesma cara
/// de dado fresco.
///
/// Mora atrás de um `Mutex` global e escreve direto no banco porque quem carimba são as
/// **threads de busca**, que não têm — e não devem ter — uma referência para o estado da
/// aba. É a mesma forma de `save_mru`.
#[derive(Default)]
pub struct Buscas {
    por_fonte: Mutex<BTreeMap<String, u64>>,
}

/// O registro desta execução, semeado do banco na primeira vez que alguém pergunta.
///
/// Global e não um campo de `InvestState` porque quem **carimba** são as threads de
/// busca, que não têm — e não devem ter — uma referência para o estado da aba. É a mesma
/// forma de `save_mru`, e o banco já é um por processo de qualquer jeito.
pub fn buscas() -> &'static Buscas {
    static BUSCAS: std::sync::OnceLock<Buscas> = std::sync::OnceLock::new();
    BUSCAS.get_or_init(Buscas::carregar)
}

impl Buscas {
    /// Semeada com o que ficou do último uso, para a primeira tela já saber a idade em
    /// vez de dizer «nunca» até a primeira volta da thread.
    pub fn carregar() -> Self {
        Self {
            por_fonte: Mutex::new(load_buscas()),
        }
    }

    /// Quando a fonte respondeu pela última vez. `None` = nunca, nem nesta execução nem
    /// em nenhuma anterior.
    pub fn quando(&self, chave: &str) -> Option<u64> {
        self.por_fonte.lock().ok()?.get(chave).copied()
    }

    /// Há quantos segundos, para quem só quer a idade.
    pub fn idade(&self, chave: &str, agora: u64) -> Option<u64> {
        self.quando(chave).map(|em| agora.saturating_sub(em))
    }

    /// Carimba uma resposta. **Só o sucesso carimba** — uma busca que falhou não deixou
    /// o dado mais novo, e mover o carimbo nela faria a tela dizer «agora» sobre um
    /// número de ontem, que é exatamente o engano que este registro existe para desfazer.
    pub fn carimbar(&self, chave: &str) {
        let em = crate::db::agora();
        if let Ok(mut mapa) = self.por_fonte.lock() {
            mapa.insert(chave.to_string(), em);
        }
        save_busca(chave, em);
    }
}

pub fn load_buscas() -> BTreeMap<String, u64> {
    db::ler(BTreeMap::new(), |conn| {
        let mut stmt = conn.prepare("SELECT chave, em FROM busca")?;
        let linhas = stmt.query_map([], |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64)))?;
        linhas.collect()
    })
}

fn save_busca(chave: &str, em: u64) {
    db::escrever(|conn| {
        conn.execute(
            "INSERT INTO busca (chave, em) VALUES (?1, ?2)
             ON CONFLICT(chave) DO UPDATE SET em = excluded.em",
            (chave, em as i64),
        )?;
        Ok(())
    });
}

#[cfg(test)]
mod tests_noticias {
    /// Uma notícia gravada por uma versão que não sabia ler o formato dela volta a ter
    /// data quando a versão nova a lê — o texto cru continua no banco, e é dele que o
    /// instante sai.
    #[test]
    fn a_data_e_relida_do_texto_cru() {
        // O formato que 215 das 240 linhas gravadas usavam, e que a versão que as gravou
        // não entendia.
        let bruto = "2026-09-10 12:08:07";
        assert!(
            crate::invest::rss::instante(bruto).is_some(),
            "o leitor precisa entender o formato para a cura funcionar"
        );
    }
}

#[cfg(test)]
mod tests_buscas {
    use super::*;

    /// Sem perfil aberto — que é o caso num teste — o banco não responde, e o registro
    /// tem que continuar sendo um mapa em memória que funciona. Um `carimbar` que
    /// estourasse aqui derrubaria toda thread de busca de quem roda a suíte.
    #[test]
    fn o_registro_funciona_em_memoria_sem_banco() {
        let b = Buscas::default();
        assert_eq!(b.quando(fonte::NOTICIA), None);
        assert_eq!(b.idade(fonte::NOTICIA, 1_000), None);
        b.carimbar(fonte::NOTICIA);
        let quando = b.quando(fonte::NOTICIA).expect("carimbou");
        assert_eq!(b.idade(fonte::NOTICIA, quando + 300), Some(300));
        // Uma fonte não carimba a outra.
        assert_eq!(b.quando(fonte::AGENDA), None);
    }

    /// «Nunca» e «agora» são respostas diferentes, e a tela as escreve diferente. Um
    /// `idade` que devolvesse zero para o que nunca foi buscado apagaria a distinção.
    #[test]
    fn nunca_buscado_nao_e_o_mesmo_que_buscado_agora() {
        let b = Buscas::default();
        assert!(b.idade(fonte::HISTORICO, 1_000).is_none());
        b.carimbar(fonte::HISTORICO);
        assert!(b.idade(fonte::HISTORICO, crate::db::agora()).is_some());
    }

    /// Os nomes vão para dentro do banco e são lidos de lá na execução seguinte: dois
    /// iguais fariam duas fontes compartilharem um carimbo calado.
    #[test]
    fn cada_fonte_tem_um_nome_so() {
        let todas = [
            fonte::COTACAO,
            fonte::NOTICIA,
            fonte::AGENDA,
            fonte::ANUNCIADO,
            fonte::HISTORICO,
            fonte::FUNDAMENTO,
        ];
        let mut vistos: Vec<&str> = todas.to_vec();
        vistos.sort_unstable();
        vistos.dedup();
        assert_eq!(vistos.len(), todas.len(), "há nome de fonte repetido");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{Classe, RegraAlerta, TipoLancamento, TipoProvento};

    fn portfolio_cheio() -> Portfolio {
        let mut p = Portfolio::default();
        p.posicoes.push(Position {
            fonte: "corretora-x".into(),
            conta: "12345-6".into(),
            ativo: AssetId::parse("B3/PETR4").unwrap(),
            classe: Classe::Acao,
            quantidade: 100.0,
            preco_medio: Some(31.4),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 7,
            carteira: Some("dividendos".into()),
        });
        // Sem preço médio: uma posição legítima, e o `None` tem que sobreviver à ida e
        // volta — um zero no lugar dele seria um custo inventado.
        p.posicoes.push(Position {
            fonte: "manual".into(),
            conta: String::new(),
            ativo: AssetId::parse("BINANCE/BTCBRL").unwrap(),
            classe: Classe::Cripto,
            quantidade: 0.5,
            preco_medio: None,
            moeda: Moeda::Brl,
            preco_manual: Some(512340.0),
            preco_manual_em: Some(99),
            atualizado_em: 8,
            carteira: None,
        });
        p.watchlist = vec![AssetId::parse("B3/IBOV").unwrap()];
        p.alvos.insert("acao".into(), 40.0);
        p.setores.insert("B3/PETR4".into(), "petróleo e gás".into());
        p.feeds = vec!["https://exemplo/rss".into()];
        p.mapeamentos.insert(
            "corretora-x".into(),
            BTreeMap::from([("ativo".to_string(), "Papel".to_string())]),
        );
        p.proventos.push(Provento {
            ativo: AssetId::parse("B3/PETR4").unwrap(),
            tipo: TipoProvento::Jcp,
            pago_em: 1000,
            bruto: 120.0,
            retido: 18.0,
            moeda: Moeda::Brl,
            quantidade: Some(100.0),
        });
        p.lancamentos.push(Lancamento {
            em: 2000,
            tipo: TipoLancamento::Compra,
            ativo: Some(AssetId::parse("B3/PETR4").unwrap()),
            quantidade: 100.0,
            preco: 31.4,
            taxas: 2.5,
            valor: 0.0,
            moeda: Some(Moeda::Brl),
            nota: "primeira".into(),
        });
        p.alertas.push(Alerta {
            ativo: AssetId::parse("B3/PETR4").unwrap(),
            regra: RegraAlerta::Acima,
            valor: 40.0,
            ligado: false,
            armado: true,
            ultimo_disparo: Some(3000),
        });
        p.carteiras.push(Carteira {
            nome: "dividendos".into(),
            alvos: vec![AlvoCarteira {
                ordem: 1,
                ativo: AssetId::parse("B3/BBAS3").unwrap(),
                percentual: 12.5,
                teto: Some(28.0),
            }],
        });
        p
    }

    /// A carteira inteira dá a volta pelo banco sem perder nada — que é a única coisa
    /// que importa numa mudança de formato de armazenamento.
    #[test]
    fn a_carteira_inteira_sobrevive_a_ida_e_volta() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migracoes::aplicar(&conn).unwrap();
        let original = portfolio_cheio();

        // Grava pelo mesmo caminho de `save`, mas nesta conexão de teste.
        gravar_em(&conn, &original).unwrap();
        let lido = ler_portfolio(&conn).unwrap();

        assert_eq!(lido.moeda_base, original.moeda_base);
        assert_eq!(lido.posicoes.len(), 2);
        assert_eq!(lido.posicoes[0].ativo.to_string(), "B3/PETR4");
        assert_eq!(lido.posicoes[0].preco_medio, Some(31.4));
        assert_eq!(lido.posicoes[0].conta, "12345-6");
        assert_eq!(lido.posicoes[0].carteira.as_deref(), Some("dividendos"));
        // O `None` continua `None`, e não virou zero.
        assert_eq!(lido.posicoes[1].preco_medio, None);
        assert_eq!(lido.posicoes[1].preco_manual, Some(512340.0));
        assert_eq!(lido.watchlist.len(), 1);
        assert_eq!(lido.alvos.get("acao"), Some(&40.0));
        assert_eq!(
            lido.setores.get("B3/PETR4").map(String::as_str),
            Some("petróleo e gás")
        );
        assert_eq!(lido.feeds, original.feeds);
        assert_eq!(lido.mapeamentos["corretora-x"]["ativo"], "Papel");
        assert_eq!(lido.proventos.len(), 1);
        assert_eq!(lido.proventos[0].tipo, TipoProvento::Jcp);
        assert_eq!(lido.proventos[0].retido, 18.0);
        assert_eq!(lido.lancamentos.len(), 1);
        assert_eq!(lido.lancamentos[0].nota, "primeira");
        assert_eq!(lido.lancamentos[0].moeda, Some(Moeda::Brl));
        assert_eq!(lido.alertas.len(), 1);
        // Desligado continua desligado depois de reiniciar.
        assert!(!lido.alertas[0].ligado);
        assert!(lido.alertas[0].armado);
        assert_eq!(lido.alertas[0].ultimo_disparo, Some(3000));
        assert_eq!(lido.carteiras.len(), 1);
        assert_eq!(lido.carteiras[0].alvos[0].teto, Some(28.0));
        assert_eq!(lido.carteiras[0].alvos[0].ordem, 1);
    }

    /// Gravar duas vezes não duplica: as tabelas são reescritas, não acrescentadas.
    #[test]
    fn gravar_de_novo_nao_duplica_linha_nenhuma() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migracoes::aplicar(&conn).unwrap();
        let p = portfolio_cheio();
        gravar_em(&conn, &p).unwrap();
        gravar_em(&conn, &p).unwrap();
        let lido = ler_portfolio(&conn).unwrap();
        assert_eq!(lido.posicoes.len(), 2);
        assert_eq!(lido.proventos.len(), 1);
        assert_eq!(lido.carteiras.len(), 1);
        assert_eq!(lido.carteiras[0].alvos.len(), 1);
    }

    /// Uma linha que este binário não sabe ler é pulada, e as outras continuam vindo.
    /// É a regra que o JSON já seguia: uma atualização — ou um downgrade — nunca custa a
    /// carteira inteira por causa de um campo.
    #[test]
    fn uma_linha_ilegivel_nao_derruba_a_leitura_das_outras() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migracoes::aplicar(&conn).unwrap();
        gravar_em(&conn, &portfolio_cheio()).unwrap();
        conn.execute(
            "INSERT INTO posicao (id, fonte, conta, ativo, classe, quantidade, moeda)
             VALUES (99, 'x', '', 'MARTE/XPTO3', 'acao', 1, 'BRL')",
            [],
        )
        .unwrap();
        let lido = ler_portfolio(&conn).unwrap();
        assert_eq!(
            lido.posicoes.len(),
            2,
            "a de mercado desconhecido foi pulada"
        );
    }

    /// Dois anúncios do mesmo papel com a mesma data-com são duas linhas.
    ///
    /// Não é hipótese: uma carteira de verdade tinha 445 anúncios com 399 pares
    /// `(ativo, data-com)` distintos — um papel anuncia dividendo **e** JCP com a mesma
    /// data, com valores por cota diferentes. Uma chave composta teria comido 46 deles
    /// sem dizer nada, e o que sumiria era dinheiro.
    #[test]
    fn dois_anuncios_do_mesmo_papel_no_mesmo_dia_sao_duas_linhas() {
        use crate::invest::provento::Anunciado;
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migracoes::aplicar(&conn).unwrap();
        let ativo = AssetId::parse("B3/VALE3").unwrap();
        let anuncios = [
            Anunciado {
                ativo: ativo.clone(),
                em: 1786406400,
                por_cota: 0.46,
                dy: 0.6384,
            },
            Anunciado {
                ativo,
                em: 1786406400,
                por_cota: 1.57,
                dy: 2.1676,
            },
        ];
        let mut stmt = conn
            .prepare("INSERT INTO anunciado (id, ativo, em, por_cota, dy) VALUES (?1,?2,?3,?4,?5)")
            .unwrap();
        for (i, a) in anuncios.iter().enumerate() {
            stmt.execute((
                i as i64 + 1,
                a.ativo.to_string(),
                a.em as i64,
                a.por_cota,
                a.dy,
            ))
            .unwrap();
        }
        let n: i64 = conn
            .query_row("SELECT count(*) FROM anunciado", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            n, 2,
            "o segundo anúncio do dia não pode sobrescrever o primeiro"
        );
    }

    #[test]
    fn cache_velho_e_descartado_na_leitura() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migracoes::aplicar(&conn).unwrap();
        let agora = agora() as i64;
        conn.execute(
            "INSERT INTO cotacao (ativo, preco, em, grade, moeda) VALUES
                ('B3/PETR4', 38.0, ?1, 'manual', 'BRL'),
                ('B3/VALE3', 61.0, ?2, 'ao_vivo', 'BRL'),
                ('BCB/IPCA', 4.5,  ?3, 'fechamento', 'BRL')",
            (
                agora - CACHE_MAX_IDADE as i64 - 10,
                agora,
                // Trinta dias: velho para um preço, corrente para uma série publicada.
                agora - 30 * 24 * 60 * 60,
            ),
        )
        .unwrap();
        let vivos: Cache = {
            let mut stmt = conn
                .prepare(
                    "SELECT ativo, preco, anterior, em, grade, moeda FROM cotacao
                     WHERE ?1 - em <= CASE grade WHEN 'fechamento' THEN ?2 ELSE ?3 END",
                )
                .unwrap();
            stmt.query_map(
                (agora, CACHE_MAX_IDADE_SERIE as i64, CACHE_MAX_IDADE as i64),
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        CachedQuote {
                            preco: row.get(1)?,
                            anterior: row.get(2)?,
                            em: row.get::<_, i64>(3)? as u64,
                            grade: row.get(4)?,
                            moeda: row.get(5)?,
                            gravado_em: None,
                        },
                    ))
                },
            )
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
        };
        assert_eq!(vivos.len(), 2);
        assert!(vivos.contains_key("B3/VALE3"));
        assert!(
            vivos.contains_key("BCB/IPCA"),
            "uma série publicada não expira como um preço"
        );
        assert!(!vivos.contains_key("B3/PETR4"));
    }
}

#[cfg(test)]
mod cache_tests {
    use super::{CACHE_MAX_IDADE, CACHE_MAX_IDADE_SERIE, validade};

    #[test]
    fn uma_serie_publicada_nao_expira_como_um_preco() {
        // O bug que isto trava: as quatro linhas do Banco Central carimbam `em` com o
        // **mês de referência**, não com a hora da busca — o IPCA nasce com semanas de
        // idade. Com a regra única de 24 h elas eram descartadas em toda abertura do
        // programa e voltavam a «buscando…» até a thread chegar nelas, dezenas de
        // segundos depois. Um IPCA de trinta dias é o IPCA corrente, não um dado velho.
        let trinta_dias: u64 = 30 * 24 * 60 * 60;
        assert!(trinta_dias > validade("atrasado"), "um preço assim é velho");
        assert!(trinta_dias < validade("fechamento"), "a série ainda vale");
    }

    #[test]
    fn um_preco_de_ontem_continua_sendo_descartado() {
        // A regra antiga estava certa para o que ela foi feita: preço de mercado.
        assert_eq!(validade("ao_vivo"), CACHE_MAX_IDADE);
        assert_eq!(validade("atrasado"), CACHE_MAX_IDADE);
        assert_eq!(validade("nao_oficial"), CACHE_MAX_IDADE);
        assert_eq!(validade("fechamento"), CACHE_MAX_IDADE_SERIE);
    }

    #[test]
    fn o_ipca_corrente_cabe_e_uma_serie_abandonada_nao() {
        // Os dois lados do teto, com o número medido: em 08/09/2026 o IPCA corrente do
        // SGS estava carimbado em 01/07 — setenta dias, e válido.
        let setenta_dias = 70 * 24 * 60 * 60;
        let meio_ano = 180 * 24 * 60 * 60;
        assert!(
            setenta_dias < validade("fechamento"),
            "o IPCA corrente tem que caber"
        );
        assert!(
            meio_ano > validade("fechamento"),
            "meio ano é série abandonada"
        );
    }
}
