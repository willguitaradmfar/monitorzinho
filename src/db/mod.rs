//! Tudo que o monitorzinho guarda, num arquivo só.
//!
//! Antes eram doze arquivos JSON lado a lado — `history.json`, `tools.json`,
//! `marks.json`, `invest.json` e mais oito —, cada um com a sua própria noção de como
//! ler, como gravar e o que fazer quando o conteúdo não presta. Fazer backup era saber
//! quais dos doze importam; levar para outra máquina era copiar os certos; e perguntar
//! qualquer coisa que atravessasse dois deles não era possível sem escrever um programa.
//!
//! Agora é um banco SQLite por perfil, em `~/.local/share/monitorzinho/db/`. Copiar o
//! arquivo é o backup; colocá-lo na pasta de outra máquina é a restauração; e `sqlite3`
//! responde qualquer pergunta sobre o que está lá dentro sem passar por aqui.
//!
//! **Um arquivo mesmo.** O modo de journal é o padrão (`DELETE`) e não o `WAL`, de
//! propósito: o WAL é mais rápido e deixa dois arquivos irmãos ao lado, e um backup que
//! copia só o `.db` de um banco em WAL copia um banco sem as últimas transações. Aqui o
//! processo é um só e a escrita é minúscula — a velocidade do WAL não compra nada, e o
//! arquivo solto compra exatamente o que foi pedido.
//!
//! **Nada aqui derruba o programa.** Uma escrita que falha é ignorada como as gravações
//! antigas eram, e uma leitura que falha devolve o padrão. Um monitor que não abre porque
//! o banco tem um problema é pior que um monitor sem histórico.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub mod migracoes;

/// A extensão que faz um arquivo da pasta ser um perfil. Qualquer outra coisa lá dentro
/// — um `.bak` que alguém deixou, um `.zip` — é ignorada em vez de oferecida.
pub const EXTENSAO: &str = "db";

/// O nome do perfil criado quando não há nenhum.
pub const PADRAO: &str = "padrao";

static CONEXAO: OnceLock<Mutex<Connection>> = OnceLock::new();
static ATUAL: OnceLock<PathBuf> = OnceLock::new();

pub fn agora() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Onde os perfis moram
// ---------------------------------------------------------------------------

/// A pasta de dados do programa — a mesma de sempre, que ainda guarda o `brapi.token` e
/// o que a migração pôs de lado.
pub fn dados_dir() -> PathBuf {
    let mut dir = dirs::data_dir().unwrap_or_else(std::env::temp_dir);
    dir.push("monitorzinho");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// A pasta dos bancos. Uma subpasta só deles para que «tenho mais de um perfil?» seja
/// `ls` e não uma triagem entre arquivos de naturezas diferentes.
pub fn perfis_dir() -> PathBuf {
    let dir = dados_dir().join("db");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Um banco na pasta de perfis.
#[derive(Clone, Debug)]
pub struct Perfil {
    /// O nome do arquivo sem a extensão — o que a tela mostra e o que `--perfil` recebe.
    pub nome: String,
    pub caminho: PathBuf,
    pub bytes: u64,
    /// Epoch da última gravação, para a lista poder dizer qual foi usado por último.
    pub modificado_em: u64,
}

/// Todo perfil que existe, em ordem alfabética.
pub fn perfis() -> Vec<Perfil> {
    let Ok(entradas) = std::fs::read_dir(perfis_dir()) else {
        return Vec::new();
    };
    let mut out: Vec<Perfil> = entradas
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == EXTENSAO))
        .filter_map(|e| {
            let caminho = e.path();
            let nome = caminho.file_stem()?.to_string_lossy().into_owned();
            let meta = e.metadata().ok();
            Some(Perfil {
                nome,
                caminho,
                bytes: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                modificado_em: meta
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
            })
        })
        .collect();
    out.sort_by(|a, b| a.nome.cmp(&b.nome));
    out
}

/// O caminho do perfil de nome `nome`, exista ele ou não.
///
/// O nome é higienizado em vez de recusado: quem digita `carteira/2026` numa caixa que
/// pede um nome quis um nome, e um caractere de caminho ali escreveria o banco em outra
/// pasta — ou em lugar nenhum.
pub fn caminho_de(nome: &str) -> PathBuf {
    perfis_dir().join(format!("{}.{EXTENSAO}", sanear(nome)))
}

/// Só o que cabe num nome de arquivo, e nada que ande pela árvore.
pub fn sanear(nome: &str) -> String {
    let limpo: String = nome
        .trim()
        .chars()
        .map(|c| match c {
            c if c.is_alphanumeric() => c,
            '-' | '_' | '.' | ' ' => c,
            _ => '-',
        })
        .collect();
    // Ponto e traço saem das pontas: sobrando só eles, o que restaria é um nome como
    // `---` ou `..`, que é um arquivo válido e um nome que não diz nada.
    let limpo = limpo.trim().trim_matches(['.', '-', ' ']).to_string();
    match limpo.is_empty() {
        true => PADRAO.to_string(),
        false => limpo,
    }
}

/// Se há mais de um perfil — o que decide se dizer qual está aberto informa alguma
/// coisa. Com um só, o nome dele é ruído.
pub fn varios_perfis() -> bool {
    perfis().len() > 1
}

/// O perfil aberto agora, pelo nome. Vazio antes de `abrir`.
pub fn perfil_atual() -> String {
    ATUAL
        .get()
        .and_then(|p| p.file_stem())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Abrir
// ---------------------------------------------------------------------------

/// Abre `caminho` como o banco deste processo e o deixa pronto para uso.
///
/// Devolve se o arquivo **acabou de nascer** — o que decide se vale procurar os JSON
/// antigos para importar, em `legado::importar`.
pub fn abrir(caminho: &Path) -> Result<bool, String> {
    let novo = !caminho.exists();
    let conn = Connection::open(caminho).map_err(|e| format!("{}: {e}", caminho.display()))?;

    // `synchronous = FULL` porque um destes bancos guarda a carteira de alguém: o que se
    // ganha afrouxando isso é tempo que ninguém sente, e o que se perde numa queda de
    // energia é a última edição. `foreign_keys` porque as tabelas filhas existem, e sem
    // isto o `ON DELETE CASCADE` delas seria decoração.
    let _ = conn.execute_batch(
        "PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = FULL;
         PRAGMA foreign_keys = ON;",
    );

    migracoes::aplicar(&conn)?;

    CONEXAO
        .set(Mutex::new(conn))
        .map_err(|_| "o banco já estava aberto".to_string())?;
    let _ = ATUAL.set(caminho.to_path_buf());
    Ok(novo)
}

// ---------------------------------------------------------------------------
// Ler e gravar
// ---------------------------------------------------------------------------

fn conexao() -> Option<&'static Mutex<Connection>> {
    CONEXAO.get()
}

/// Se o arquivo do perfil não aceita escrita — um pendrive montado somente para
/// leitura, um `chmod` de quem foi fazer backup. A tela da carteira diz isso na hora, em
/// vez de deixar alguém editar por meia hora e descobrir depois que nada foi gravado.
pub fn somente_leitura() -> bool {
    ler(false, |conn| conn.is_readonly(rusqlite::MAIN_DB))
}

/// Lê. Qualquer erro — banco fechado, tabela estranha, coluna que não converte — devolve
/// `padrao`, que é o que os arquivos JSON faziam com um conteúdo ilegível.
pub fn ler<T, F>(padrao: T, f: F) -> T
where
    F: FnOnce(&Connection) -> rusqlite::Result<T>,
{
    let Some(lock) = conexao() else {
        return padrao;
    };
    let Ok(guard) = lock.lock() else {
        return padrao;
    };
    f(&guard).unwrap_or(padrao)
}

/// Grava, tudo dentro de uma transação: ou o conjunto inteiro entrou, ou nada entrou.
///
/// É o que substitui o «temporário ao lado, `fsync`, `rename` por cima» que a carteira
/// fazia à mão — e substitui com vantagem, porque agora a garantia vale para as onze
/// coisas que o arquivo guardava e não só para o arquivo todo de uma vez.
///
/// Devolve se deu certo, para quem precisa dizer na tela que não deu.
pub fn escrever<F>(f: F) -> bool
where
    F: FnOnce(&Connection) -> rusqlite::Result<()>,
{
    let Some(lock) = conexao() else {
        return false;
    };
    let Ok(mut guard) = lock.lock() else {
        return false;
    };
    let Ok(tx) = guard.transaction() else {
        return false;
    };
    if f(&tx).is_err() {
        return false;
    }
    tx.commit().is_ok()
}

/// Apaga uma tabela inteira e a reescreve. É como as coleções pequenas são gravadas:
/// alguns milhares de linhas dentro de uma transação custam milissegundos, e o código
/// que resulta é o mesmo que gravava uma lista inteira em JSON — sem a chance de a
/// diferença entre o que está na memória e o que está no banco divergir com o tempo.
pub fn limpar(conn: &Connection, tabela: &str) -> rusqlite::Result<()> {
    conn.execute(&format!("DELETE FROM {tabela}"), [])?;
    Ok(())
}

// ---------------------------------------------------------------------------
// config: os escalares
// ---------------------------------------------------------------------------

pub fn config_ler(chave: &str) -> Option<String> {
    ler(None, |conn| {
        let mut stmt = conn.prepare("SELECT valor FROM config WHERE chave = ?1")?;
        let mut linhas = stmt.query([chave])?;
        match linhas.next()? {
            Some(linha) => Ok(Some(linha.get::<_, String>(0)?)),
            None => Ok(None),
        }
    })
}

pub fn config_gravar(chave: &str, valor: &str) {
    escrever(|conn| {
        conn.execute(
            "INSERT INTO config (chave, valor) VALUES (?1, ?2)
             ON CONFLICT(chave) DO UPDATE SET valor = excluded.valor",
            (chave, valor),
        )?;
        Ok(())
    });
}

/// O mesmo, dentro de uma transação que já está aberta.
pub fn config_por(conn: &Connection, chave: &str, valor: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO config (chave, valor) VALUES (?1, ?2)
         ON CONFLICT(chave) DO UPDATE SET valor = excluded.valor",
        (chave, valor),
    )?;
    Ok(())
}

pub fn config_de(conn: &Connection, chave: &str) -> Option<String> {
    conn.query_row("SELECT valor FROM config WHERE chave = ?1", [chave], |r| {
        r.get::<_, String>(0)
    })
    .ok()
}

// ---------------------------------------------------------------------------
// Enums em colunas
// ---------------------------------------------------------------------------

/// O código de um enum pequeno, para a coluna guardar `dividendo` e não `2`.
///
/// Pelo `serde`, e não por um `match` novo: esses enums já dizem no `#[serde(rename)]`
/// como se chamam, era assim que apareciam no JSON antigo, e uma segunda tabela de nomes
/// é uma segunda chance de os dois discordarem. O que se ganha é um `SELECT` legível.
pub fn codigo<T: Serialize>(valor: &T) -> String {
    serde_json::to_string(valor)
        .unwrap_or_default()
        .trim_matches('"')
        .to_string()
}

/// A volta. `None` para um código que este binário não conhece — o que acontece ao
/// abrir, com uma versão mais velha, um banco que ganhou uma variante nova.
pub fn do_codigo<T: DeserializeOwned>(texto: &str) -> Option<T> {
    serde_json::from_str(&format!("\"{}\"", texto.replace('"', "\\\""))).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn um_nome_de_perfil_nao_anda_pela_arvore() {
        assert_eq!(sanear("../../etc/passwd"), "etc-passwd");
        // O que importa é a propriedade: nada que o sistema de arquivos leia como um
        // caminho sobrevive.
        for suspeito in ["../../etc/passwd", "a/b", "..", "~/x", "a\\b"] {
            let limpo = sanear(suspeito);
            assert!(!limpo.contains('/'), "{suspeito} virou {limpo}");
            assert!(!limpo.contains('\\'), "{suspeito} virou {limpo}");
            assert_ne!(limpo, "..");
        }
        assert_eq!(sanear("carteira/2026"), "carteira-2026");
        assert_eq!(sanear("  trabalho  "), "trabalho");
        // Um nome que não sobrou nada vira o padrão, e não um arquivo sem nome.
        assert_eq!(sanear("///"), PADRAO);
        assert_eq!(sanear(""), PADRAO);
    }

    #[test]
    fn nomes_normais_passam_intactos() {
        assert_eq!(sanear("padrao"), "padrao");
        assert_eq!(sanear("casa_2"), "casa_2");
        assert_eq!(sanear("minha carteira"), "minha carteira");
    }

    #[test]
    fn o_codigo_de_um_enum_e_o_nome_que_o_serde_ja_dava() {
        use crate::invest::model::{Classe, Moeda};
        assert_eq!(codigo(&Classe::RendaFixa), "renda_fixa");
        assert_eq!(codigo(&Moeda::Brl), "BRL");
        assert_eq!(do_codigo::<Classe>("fii"), Some(Classe::Fii));
        assert_eq!(do_codigo::<Moeda>("USD"), Some(Moeda::Usd));
        // Uma variante que este binário não conhece não vira palpite.
        assert_eq!(do_codigo::<Classe>("criptomoeda-nova"), None);
    }
}
