//! A engine de migrations: como a estrutura do banco anda para a frente sem levar dado
//! junto.
//!
//! A regra é a de qualquer sistema que versiona esquema, e ela vale aqui pela mesma
//! razão: o banco não é do programa, é de quem usa o programa. Ele já existe, já tem a
//! carteira de alguém dentro, e a versão nova precisa alcançá-lo onde ele está.
//!
//! **Todo início do monitorzinho passa por aqui.** Abrir um perfil lê a tabela `migracao`
//! — o registro do que já rodou naquele arquivo —, compara com [`TODAS`], e aplica o que
//! faltar, na ordem. Num banco em dia isso é uma consulta e nenhuma escrita; num banco
//! que ficou para trás porque a máquina passou meses sem atualizar, é a fila inteira de
//! uma vez, cada uma na sua transação.
//!
//! **A tabela é a fonte da verdade, e não um contador.** O que decide se a migration 7
//! roda é ela não estar registrada naquele banco — não «o banco está na versão 6». A
//! diferença aparece quando duas migrations nascem em paralelo e a de número menor é
//! publicada depois: com um contador ela seria pulada para sempre no banco de quem já
//! tinha passado da maior; pelo registro, ela roda na próxima abertura, que é o que se
//! espera.
//!
//! As regras de quem escreve uma:
//!
//! * Cada mudança de estrutura é uma [`Migracao`] nova no fim de [`TODAS`], com um número
//!   maior que o da última. **Uma migration publicada nunca é editada** — o banco de
//!   alguém já a registrou como aplicada, e mudar o SQL só mudaria o que acontece na
//!   máquina de quem ainda não a rodou, criando duas estruturas diferentes com o mesmo
//!   número.
//! * O que fazer em vez disso é sempre uma migration nova: `ALTER TABLE ... ADD COLUMN`,
//!   uma tabela nova, um índice novo. Corrigir a de ontem é escrever a de hoje.
//! * Cada uma roda dentro da própria transação. Uma que falha no meio não deixa metade da
//!   estrutura aplicada nem se registra como aplicada: o programa recusa abrir aquele
//!   perfil e diz qual falhou, em vez de seguir com um banco pela metade.
//!
//! `PRAGMA user_version` continua sendo escrito, espelhando a maior versão aplicada. Ele
//! não decide nada — serve para `sqlite3 arquivo.db "PRAGMA user_version"` responder de
//! fora sem precisar saber o nome da tabela.
//!
//! Não há *down*. Um `down` que ninguém roda é código morto, e um que alguém roda apaga a
//! coluna com o dado dentro. Para voltar de versão existe a cópia do arquivo — que é um
//! arquivo só, e é o motivo de tudo isto estar em SQLite.

use rusqlite::Connection;

/// Um degrau da estrutura.
pub struct Migracao {
    /// Estritamente crescente e imutável depois de publicado.
    pub versao: u32,
    /// O nome do arquivo, sem a extensão. Vai para a tabela `migracao` e para a mensagem
    /// de erro quando ela falha — que é quando alguém precisa achá-la no código.
    pub nome: &'static str,
    pub sql: &'static str,
}

/// Toda migration que existe, em ordem. Acrescentar é no fim, sempre.
///
/// Uma linha aqui e um `.sql` ao lado, em `src/db/migracoes/`. O `include_str!` lê o
/// arquivo **em tempo de compilação**: o SQL fica dentro do binário como qualquer outra
/// constante, e continua sendo um binário único que não procura nada no disco para subir.
/// O arquivo separado é só para o SQL ser lido como SQL — com destaque de sintaxe, e com
/// um `diff` que mostra a estrutura mudando em vez de uma string mudando.
pub const TODAS: &[Migracao] = &[Migracao {
    versao: 1,
    nome: "0001_estrutura_inicial",
    sql: include_str!("migracoes/0001_estrutura_inicial.sql"),
}];

/// A maior versão que este binário sabe alcançar.
pub fn alvo() -> u32 {
    TODAS.iter().map(|m| m.versao).max().unwrap_or(0)
}

/// A maior versão registrada neste banco. `0` num banco que nunca rodou nenhuma.
pub fn versao_de(conn: &Connection) -> rusqlite::Result<u32> {
    registrar_tabela(conn)?;
    conn.query_row("SELECT COALESCE(MAX(versao), 0) FROM migracao", [], |r| {
        r.get(0)
    })
}

/// Que migrations este banco já rodou, do mais antigo ao mais novo — versão, nome e
/// quando. É o que a tela de perfis mostra e o que responde «por que este banco está
/// diferente daquele».
pub fn aplicadas(conn: &Connection) -> rusqlite::Result<Vec<(u32, String, u64)>> {
    registrar_tabela(conn)?;
    let mut stmt =
        conn.prepare("SELECT versao, nome, aplicada_em FROM migracao ORDER BY versao")?;
    let linhas = stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? as u64))
    })?;
    linhas.collect()
}

/// O que ainda falta rodar neste banco, na ordem em que vai rodar.
pub fn pendentes(conn: &Connection) -> rusqlite::Result<Vec<&'static Migracao>> {
    let ja: Vec<u32> = aplicadas(conn)?.into_iter().map(|(v, _, _)| v).collect();
    let mut falta: Vec<&Migracao> = TODAS.iter().filter(|m| !ja.contains(&m.versao)).collect();
    falta.sort_by_key(|m| m.versao);
    Ok(falta)
}

/// O livro de registro. Fora de qualquer migration e antes de todas, porque ele precisa
/// existir para a primeira poder ser registrada nele — e `IF NOT EXISTS` porque toda
/// abertura passa por aqui.
fn registrar_tabela(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS migracao (
            versao      INTEGER PRIMARY KEY,
            nome        TEXT NOT NULL,
            aplicada_em INTEGER NOT NULL
         ) WITHOUT ROWID;",
    )
}

/// Leva `conn` até [`alvo`], uma migration por vez. É isto que todo início do programa
/// chama, para todo perfil que abre.
///
/// Devolve quantas rodaram — zero é o caso normal de um banco já em dia, e é o que faz
/// abrir não custar nada.
pub fn aplicar(conn: &Connection) -> Result<usize, String> {
    registrar_tabela(conn)
        .map_err(|e| format!("não deu para criar a tabela de migrations: {e}"))?;

    let registrada = versao_de(conn).map_err(|e| format!("não deu para ler o registro: {e}"))?;
    let alvo = alvo();
    if registrada > alvo {
        // A trava que o `invest.json` já tinha, agora para o banco inteiro: um arquivo
        // gravado por uma versão futura não é tocado por esta, que não sabe o que há
        // dentro dele.
        return Err(format!(
            "este perfil já rodou a migration {registrada} e esta instalação só conhece \
             até a {alvo} — atualize o monitorzinho, ou abra outro perfil"
        ));
    }

    let falta = pendentes(conn).map_err(|e| format!("não deu para ler o registro: {e}"))?;
    let rodadas = falta.len();
    for migracao in falta {
        aplicar_uma(conn, migracao)?;
    }
    Ok(rodadas)
}

/// Uma migration, inteira ou nenhuma: o SQL dela, o registro na tabela e o espelho na
/// `user_version` entram juntos ou não entra nada.
///
/// `BEGIN`/`COMMIT` à mão em vez de `Connection::transaction` porque os três têm que
/// estar na mesma transação — e `PRAGMA user_version = N` não aceita parâmetro, então o
/// número é formatado a partir de um `u32` que veio de uma constante do código, nunca de
/// fora.
fn aplicar_uma(conn: &Connection, migracao: &Migracao) -> Result<(), String> {
    let falha = |etapa: &str, e: rusqlite::Error| {
        format!(
            "migration {} ({}) falhou {etapa}: {e}",
            migracao.versao, migracao.nome
        )
    };

    conn.execute_batch("BEGIN")
        .map_err(|e| falha("ao começar", e))?;

    let resultado = (|| -> rusqlite::Result<()> {
        conn.execute_batch(migracao.sql)?;
        conn.execute(
            "INSERT INTO migracao (versao, nome, aplicada_em) VALUES (?1, ?2, ?3)",
            (migracao.versao, migracao.nome, super::agora() as i64),
        )?;
        // O espelho: a maior versão aplicada, para quem olhar o arquivo de fora com o
        // `sqlite3` ver um número sem precisar saber o nome da tabela. Um `MAX` e não a
        // versão desta, para uma migration antiga chegando atrasada não fazer o número
        // andar para trás.
        let maior: u32 =
            conn.query_row("SELECT COALESCE(MAX(versao), 0) FROM migracao", [], |r| {
                r.get(0)
            })?;
        conn.execute_batch(&format!("PRAGMA user_version = {maior}"))
    })();

    match resultado {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .map_err(|e| falha("ao confirmar", e)),
        Err(e) => {
            // O `rollback` desfaz o SQL *e* o registro, então a próxima abertura tenta
            // esta mesma migration de novo em vez de pular por cima de uma estrutura que
            // não chegou a existir.
            let _ = conn.execute_batch("ROLLBACK");
            Err(falha("ao aplicar", e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn banco() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    #[test]
    fn as_versoes_sao_crescentes_e_unicas() {
        // O registro é por versão: duas migrations com o mesmo número seriam «a mesma»
        // para a engine, e a segunda nunca rodaria em banco nenhum.
        let mut anterior = 0;
        for m in TODAS {
            assert!(
                m.versao > anterior,
                "migration {} ({}) não vem depois da anterior",
                m.versao,
                m.nome
            );
            anterior = m.versao;
            assert!(!m.nome.trim().is_empty(), "toda migration tem nome");
            assert!(!m.sql.trim().is_empty(), "toda migration tem SQL");
        }
    }

    #[test]
    fn um_banco_novo_roda_a_fila_inteira() {
        let conn = banco();
        assert_eq!(aplicar(&conn).unwrap(), TODAS.len());
        assert_eq!(versao_de(&conn).unwrap(), alvo());
        // E o registro diz o que rodou, com nome e carimbo.
        let feitas = aplicadas(&conn).unwrap();
        assert_eq!(feitas.len(), TODAS.len());
        assert_eq!(feitas[0].1, TODAS[0].nome);
    }

    #[test]
    fn todo_inicio_consulta_o_registro_e_nao_roda_nada_de_novo() {
        let conn = banco();
        aplicar(&conn).unwrap();
        // O caso normal, e o que faz abrir um perfil não custar uma escrita sequer.
        assert!(pendentes(&conn).unwrap().is_empty());
        assert_eq!(aplicar(&conn).unwrap(), 0);
    }

    #[test]
    fn uma_migration_nao_registrada_roda_mesmo_com_numero_menor_que_o_maior() {
        // É a diferença entre um registro e um contador: duas migrations nascidas em
        // paralelo, e a de número menor publicada depois. Um contador a pularia para
        // sempre; o registro a roda na próxima abertura.
        let conn = banco();
        aplicar(&conn).unwrap();
        conn.execute(
            "INSERT INTO migracao (versao, nome, aplicada_em) VALUES (99, 'de outro ramo', 0)",
            [],
        )
        .unwrap();
        // A 1 continua registrada, então nada roda; mas se ela sumisse do registro, ela
        // voltaria a estar pendente mesmo com a 99 lá dentro.
        conn.execute("DELETE FROM migracao WHERE versao = 1", [])
            .unwrap();
        let falta = pendentes(&conn).unwrap();
        assert_eq!(falta.len(), 1);
        assert_eq!(falta[0].versao, 1);
    }

    #[test]
    fn um_banco_do_futuro_e_recusado_em_vez_de_mexido() {
        let conn = banco();
        aplicar(&conn).unwrap();
        conn.execute(
            "INSERT INTO migracao (versao, nome, aplicada_em) VALUES (9999, 'do futuro', 0)",
            [],
        )
        .unwrap();
        let erro = aplicar(&conn).unwrap_err();
        assert!(
            erro.contains("9999"),
            "o erro diz até onde o banco já foi: {erro}"
        );
        // E não mexeu em nada: o registro continua o que era.
        assert_eq!(versao_de(&conn).unwrap(), 9999);
    }

    #[test]
    fn uma_migration_que_falha_nao_deixa_meia_estrutura_nem_se_registra() {
        let conn = banco();
        registrar_tabela(&conn).unwrap();
        let quebrada = Migracao {
            versao: 1,
            nome: "0001_meia",
            sql: "CREATE TABLE a (x INTEGER); ISTO NAO E SQL;",
        };
        let erro = aplicar_uma(&conn, &quebrada).unwrap_err();
        assert!(erro.contains("0001_meia"), "o erro diz qual falhou: {erro}");

        let tabela_a: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'a'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tabela_a, 0, "o rollback desfaz o que ela já tinha feito");
        // E ela continua pendente, para a próxima abertura tentar de novo em vez de
        // pular por cima de uma estrutura que não chegou a existir.
        assert_eq!(versao_de(&conn).unwrap(), 0);
    }

    #[test]
    fn a_estrutura_inicial_tem_as_tabelas_que_o_programa_usa() {
        let conn = banco();
        aplicar(&conn).unwrap();
        for tabela in [
            "config",
            "historico",
            "marca",
            "execucao",
            "execucao_param",
            "regra",
            "posicao",
            "watchlist",
            "alvo",
            "setor",
            "feed",
            "mapeamento",
            "provento",
            "lancamento",
            "alerta",
            "carteira",
            "carteira_alvo",
            "patrimonio",
            "cotacao",
            "agenda",
            "noticia",
            "anunciado",
            "modulo_mru",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name = ?1",
                    [tabela],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "falta a tabela {tabela}");
        }
    }
}
