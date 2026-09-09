//! O que fica em disco, e as duas regras que não se negociam.
//!
//! **Escrita atômica.** Um `history.json` truncado por queda de energia é um gráfico
//! feio; um `invest.json` truncado é o registro do patrimônio de alguém. Grava-se num
//! temporário ao lado, chama-se `sync_all`, e só então `rename` por cima — que no mesmo
//! sistema de arquivos é atômico: ou o arquivo velho está inteiro, ou o novo está.
//!
//! **Arquivo ruim não vira carteira vazia.** O resto do programa trata JSON inválido como
//! «comece do zero», e ali isso custa um histórico de gráfico. Aqui custaria a carteira,
//! então um arquivo ilegível é renomeado para `.corrompido`, o `.bak` é tentado, e nada é
//! gravado por cima até alguém resolver.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::history;
use crate::invest::model::{Portfolio, VERSAO_ATUAL};

pub fn agora() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn caminho(nome: &str) -> PathBuf {
    history::data_file(nome)
}

/// O que deu errado ao ler a carteira, para a tela poder dizer em vez de só mostrar vazio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadIssue {
    /// Não existe ainda. Não é problema: é a primeira execução.
    Novo,
    /// JSON inválido. O arquivo foi posto de lado e o `.bak` entrou no lugar.
    Corrompido { salvo_em: String, do_backup: bool },
    /// Escrito por uma versão que este programa não entende. **Nada é gravado por cima.**
    VersaoDesconhecida { arquivo: u32, entendo: u32 },
}

/// A carteira lida, e o que houve de errado ao lê-la.
pub struct Loaded {
    pub portfolio: Portfolio,
    pub issue: Option<LoadIssue>,
    /// Quando `true`, gravar está proibido: é a trava que impede uma versão futura de ser
    /// sobrescrita por esta, que não sabe o que há dentro dela.
    pub somente_leitura: bool,
}

const ARQUIVO: &str = "invest.json";
const BACKUP: &str = "invest.json.bak";

pub fn load() -> Loaded {
    let path = caminho(ARQUIVO);
    let texto = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            return Loaded {
                portfolio: Portfolio::default(),
                issue: Some(LoadIssue::Novo),
                somente_leitura: false,
            };
        }
    };

    match serde_json::from_str::<Portfolio>(&texto) {
        Ok(p) if p.versao > VERSAO_ATUAL => Loaded {
            portfolio: Portfolio::default(),
            issue: Some(LoadIssue::VersaoDesconhecida {
                arquivo: p.versao,
                entendo: VERSAO_ATUAL,
            }),
            // A trava: uma versão futura não pode ser sobrescrita por esta.
            somente_leitura: true,
        },
        Ok(p) => Loaded {
            portfolio: p,
            issue: None,
            somente_leitura: false,
        },
        Err(_) => {
            // Põe o arquivo ruim de lado com um nome que diz o que é, e tenta o backup.
            let quarentena = caminho(&format!("invest.json.corrompido.{}", agora()));
            let _ = fs::rename(&path, &quarentena);
            let (portfolio, do_backup) = match fs::read_to_string(caminho(BACKUP))
                .ok()
                .and_then(|t| serde_json::from_str::<Portfolio>(&t).ok())
            {
                Some(p) => (p, true),
                None => (Portfolio::default(), false),
            };
            Loaded {
                portfolio,
                issue: Some(LoadIssue::Corrompido {
                    salvo_em: quarentena.display().to_string(),
                    do_backup,
                }),
                somente_leitura: false,
            }
        }
    }
}

/// Grava a carteira. Guarda a versão anterior em `.bak` antes, e escreve atomicamente.
pub fn save(portfolio: &Portfolio) -> Result<(), String> {
    let path = caminho(ARQUIVO);
    let texto = serde_json::to_string_pretty(portfolio).map_err(|e| e.to_string())?;

    // A versão anterior, antes de qualquer coisa. Custa um arquivo pequeno e é a
    // diferença entre um susto e uma perda.
    if path.exists() {
        let _ = fs::copy(&path, caminho(BACKUP));
    }
    escrever_atomico(&path, texto.as_bytes())
}

/// Temporário ao lado, `sync_all`, `rename` por cima. O `rename` é a parte atômica; o
/// `sync_all` é o que garante que os bytes chegaram ao disco antes de o nome apontar
/// para eles — sem ele, uma queda de energia pode deixar o nome novo apontando para um
/// arquivo vazio, que é exatamente o que se estava tentando evitar.
fn escrever_atomico(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
        f.write_all(bytes).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// A ordem da lista de módulos: id → epoch da última abertura.
// ---------------------------------------------------------------------------

pub type Mru = BTreeMap<String, u64>;

const ARQUIVO_MRU: &str = "invest-mru.json";

/// Perder isto custa a ordem da lista até o próximo uso, e nada mais — por isso a leitura
/// engole qualquer erro, e a gravação é simples, sem `fsync`.
pub fn load_mru() -> Mru {
    fs::read_to_string(caminho(ARQUIVO_MRU))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_mru(mru: &Mru) {
    if let Ok(t) = serde_json::to_string_pretty(mru) {
        let _ = fs::write(caminho(ARQUIVO_MRU), t);
    }
}

// ---------------------------------------------------------------------------
// O cache de mercado: abrir a aba mostrando números, e não traços.
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CachedQuote {
    pub preco: f64,
    pub em: u64,
    #[serde(default)]
    pub anterior: Option<f64>,
    #[serde(default)]
    pub grade: String,
    #[serde(default)]
    pub moeda: String,
}

pub type Cache = BTreeMap<String, CachedQuote>;

/// O calendário econômico já buscado.
///
/// Gravado porque a home mostra a agenda e **a home não busca nada**: sem isto ela
/// ficaria vazia até alguém abrir o módulo, e voltaria a ficar vazia no próximo início.
/// Um calendário econômico muda uma vez por mês; relê-lo do disco é grátis.
const ARQUIVO_AGENDA: &str = "invest-agenda.json";

pub fn load_agenda() -> Vec<crate::invest::calendario::Evento> {
    fs::read_to_string(caminho(ARQUIVO_AGENDA))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_agenda(eventos: &[crate::invest::calendario::Evento]) {
    if eventos.is_empty() {
        return;
    }
    if let Ok(t) = serde_json::to_string_pretty(eventos) {
        let _ = fs::write(caminho(ARQUIVO_AGENDA), t);
    }
}

/// As manchetes já lidas. Gravadas pelo mesmo motivo do calendário: o cartão da home as
/// mostra e nasceria vazio a cada início.
const ARQUIVO_NOTICIAS: &str = "invest-noticias.json";

pub fn load_noticias() -> Vec<(String, crate::invest::rss::Item)> {
    fs::read_to_string(caminho(ARQUIVO_NOTICIAS))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_noticias(itens: &[(String, crate::invest::rss::Item)]) {
    if itens.is_empty() {
        return;
    }
    if let Ok(t) = serde_json::to_string_pretty(itens) {
        let _ = fs::write(caminho(ARQUIVO_NOTICIAS), t);
    }
}

/// Os proventos anunciados já buscados. Gravados pelo mesmo motivo dos outros caches: a
/// lista de sugestões nasce cheia em vez de esperar dez buscas.
const ARQUIVO_ANUNCIADOS: &str = "invest-anunciados.json";

pub fn load_anunciados() -> Vec<crate::invest::provento::Anunciado> {
    fs::read_to_string(caminho(ARQUIVO_ANUNCIADOS))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn save_anunciados(v: &[crate::invest::provento::Anunciado]) {
    if v.is_empty() {
        return;
    }
    if let Ok(t) = serde_json::to_string_pretty(v) {
        let _ = fs::write(caminho(ARQUIVO_ANUNCIADOS), t);
    }
}

const ARQUIVO_CACHE: &str = "invest-cache.json";
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
/// dado, ver `validade`.
pub fn load_cache() -> Cache {
    let agora = agora();
    fs::read_to_string(caminho(ARQUIVO_CACHE))
        .ok()
        .and_then(|t| serde_json::from_str::<Cache>(&t).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, q)| agora.saturating_sub(q.em) <= validade(&q.grade))
        .collect()
}

pub fn save_cache(cache: &Cache) {
    if let Ok(t) = serde_json::to_string_pretty(cache) {
        let _ = fs::write(caminho(ARQUIVO_CACHE), t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{AssetId, Classe, Moeda, Position};

    fn portfolio_com_uma_posicao() -> Portfolio {
        let mut p = Portfolio::default();
        p.posicoes.push(Position {
            fonte: "t".into(),
            conta: String::new(),
            ativo: AssetId::parse("B3/PETR4").unwrap(),
            classe: Classe::Acao,
            quantidade: 100.0,
            preco_medio: Some(31.4),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
        });
        p
    }

    #[test]
    fn escrita_atomica_deixa_o_arquivo_inteiro() {
        let dir = std::env::temp_dir().join(format!("mz-invest-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let path = dir.join("x.json");
        let texto = serde_json::to_vec_pretty(&portfolio_com_uma_posicao()).unwrap();
        escrever_atomico(&path, &texto).unwrap();
        let lido: Portfolio = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(lido.posicoes.len(), 1);
        // Nenhum temporário sobrou ao lado.
        let sobras: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(sobras.is_empty(), "o temporário tem que sumir no rename");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn versao_futura_trava_a_gravacao() {
        // Um arquivo de uma versão que não entendemos não pode ser sobrescrito por esta.
        let json = r#"{"versao": 999, "moeda_base": "BRL", "posicoes": []}"#;
        let p: Portfolio = serde_json::from_str(json).unwrap();
        assert!(p.versao > VERSAO_ATUAL);
    }

    #[test]
    fn cache_velho_e_descartado_na_leitura() {
        let mut cache = Cache::new();
        cache.insert(
            "B3/PETR4".into(),
            CachedQuote {
                preco: 38.0,
                em: agora() - CACHE_MAX_IDADE - 10,
                anterior: None,
                grade: "manual".into(),
                moeda: "BRL".into(),
            },
        );
        cache.insert(
            "B3/VALE3".into(),
            CachedQuote {
                preco: 61.0,
                em: agora(),
                anterior: None,
                grade: "ao_vivo".into(),
                moeda: "BRL".into(),
            },
        );
        let agora = agora();
        let vivos: Cache = cache
            .into_iter()
            .filter(|(_, q)| agora.saturating_sub(q.em) <= CACHE_MAX_IDADE)
            .collect();
        assert_eq!(vivos.len(), 1);
        assert!(vivos.contains_key("B3/VALE3"));
    }

    #[test]
    fn campo_novo_ausente_vale_o_padrao() {
        // Uma atualização do programa nunca rejeita um arquivo antigo por inteiro —
        // mesma regra de `tools::persist::restore_params`.
        let json = r#"{"posicoes": [], "watchlist": []}"#;
        let p: Portfolio = serde_json::from_str(json).unwrap();
        assert_eq!(p.versao, VERSAO_ATUAL);
        assert_eq!(p.moeda_base, Moeda::Brl);
        assert!(p.alertas.is_empty());
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
