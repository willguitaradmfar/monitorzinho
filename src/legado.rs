//! Os doze arquivos JSON que existiam antes do banco, trazidos para dentro dele uma vez.
//!
//! Quem já usava o monitorzinho tem um `~/.local/share/monitorzinho/` cheio: o histórico
//! dos gráficos, as ferramentas que estavam rodando, as marcas das tabelas, e — o que
//! importa de verdade — a carteira. Uma atualização que deixasse tudo isso para trás
//! seria uma atualização que apaga anos de uso sem avisar.
//!
//! Então acontece exatamente uma vez, no nascimento do primeiro perfil, e por um caminho
//! só: cada arquivo é lido com o mesmo `serde` de antes e gravado pelas mesmas funções
//! `save` de sempre — que agora escrevem no banco. Não há um segundo entendimento do
//! formato antigo aqui dentro, e por isso não há como os dois discordarem.
//!
//! **Os arquivos não são apagados.** Eles vão para `legado/` ao lado, com o nome que
//! tinham. Custa alguns quilobytes e é a diferença entre uma migração que se pode
//! conferir e uma que só se pode acreditar — e se alguma coisa aqui estiver errada, o
//! `invest.json` de alguém continua lá, inteiro.

use std::fs;
use std::path::{Path, PathBuf};

use crate::db;

/// Todo arquivo que a versão anterior escrevia, na ordem em que são importados.
const ARQUIVOS: &[&str] = &[
    "history.json",
    "tools.json",
    "marks.json",
    "rewrites.json",
    "engine.json",
    "invest.json",
    "invest-mru.json",
    "invest-cache.json",
    "invest-agenda.json",
    "invest-noticias.json",
    "invest-anunciados.json",
    "invest-patrimonio.json",
];

/// O que a importação encontrou e fez, para quem está olhando a tela poder ver.
#[derive(Default)]
pub struct Resumo {
    /// Os arquivos que existiam e foram lidos, pelo nome.
    pub importados: Vec<String>,
    /// Os que existiam mas não deram para ler. Ficam onde estão, intocados.
    pub ilegiveis: Vec<String>,
    /// Para onde os originais foram guardados, quando algum foi.
    pub guardados_em: Option<PathBuf>,
}

impl Resumo {
    pub fn houve_algo(&self) -> bool {
        !self.importados.is_empty() || !self.ilegiveis.is_empty()
    }
}

/// Há algum arquivo da versão anterior esperando?
pub fn existe_algo() -> bool {
    let dir = db::dados_dir();
    ARQUIVOS.iter().any(|nome| dir.join(nome).exists())
}

/// Traz o que houver para o banco que está aberto agora.
///
/// Chamado só quando o arquivo do perfil acabou de nascer: um banco que já existe já
/// passou por aqui, e reimportar por cima do que a pessoa editou desde então seria
/// desfazer o trabalho dela.
pub fn importar() -> Resumo {
    let dir = db::dados_dir();
    let mut resumo = Resumo::default();

    for nome in ARQUIVOS {
        let caminho = dir.join(nome);
        if !caminho.exists() {
            continue;
        }
        match ler(&caminho, nome) {
            true => resumo.importados.push((*nome).to_string()),
            false => resumo.ilegiveis.push((*nome).to_string()),
        }
    }

    if !resumo.importados.is_empty() {
        let guardados = dir.join("legado");
        if fs::create_dir_all(&guardados).is_ok() {
            for nome in &resumo.importados {
                // `rename` e não `remove`: o original continua existindo, com o nome que
                // tinha, num lugar que diz o que ele é.
                let _ = fs::rename(dir.join(nome), guardados.join(nome));
            }
            // O `.bak` da carteira vai junto — ele é a cópia de segurança do arquivo que
            // acabou de ser importado, e sozinho no diretório antigo não seria cópia de
            // segurança de nada.
            let _ = fs::rename(
                dir.join("invest.json.bak"),
                guardados.join("invest.json.bak"),
            );
            resumo.guardados_em = Some(guardados);
        }
    }
    resumo
}

/// Lê um arquivo e o grava pelas funções normais do programa. `false` quando ele existe
/// mas não dá para entender — e aí ele fica onde está, para alguém olhar.
fn ler(caminho: &Path, nome: &str) -> bool {
    let Ok(texto) = fs::read_to_string(caminho) else {
        return false;
    };

    /// Lê um JSON no tipo do arquivo antigo e passa adiante. O tipo é o mesmo de antes,
    /// então o entendimento do formato velho é o `serde` que já estava lá, e não uma
    /// segunda leitura escrita aqui.
    fn aplicar<T: serde::de::DeserializeOwned>(texto: &str, grava: impl FnOnce(T)) -> bool {
        match serde_json::from_str::<T>(texto) {
            Ok(valor) => {
                grava(valor);
                true
            }
            Err(_) => false,
        }
    }

    match nome {
        "history.json" => aplicar(&texto, |m: crate::history::HistoryMap| {
            crate::history::save_all(&m)
        }),
        "tools.json" => aplicar(&texto, |v: Vec<crate::tools::persist::ExecutionSpec>| {
            crate::tools::persist::save(&v)
        }),
        "marks.json" => aplicar(&texto, |v: Vec<crate::monitor::mark::Mark>| {
            crate::monitor::mark::gravar_todas(v)
        }),
        "rewrites.json" => aplicar(&texto, |v: Vec<crate::tools::rewrite::Rule>| {
            // Do mais antigo para o mais novo, para o carimbo que cada um ganha manter a
            // ordem que a lista tinha — ela é «a última que foi útil primeiro».
            for rule in v.iter().rev() {
                crate::tools::rewrite::remember(rule);
            }
        }),
        "engine.json" => aplicar(&texto, |s: EngineJson| {
            crate::container::engine::save_settings(&crate::container::engine::Settings {
                endpoint: s.endpoint,
            })
        }),
        "invest.json" => aplicar(&texto, |p: crate::invest::model::Portfolio| {
            let _ = crate::invest::store::save(&p);
        }),
        "invest-mru.json" => aplicar(&texto, |m: crate::invest::store::Mru| {
            crate::invest::store::save_mru(&m)
        }),
        "invest-cache.json" => aplicar(&texto, |c: crate::invest::store::Cache| {
            crate::invest::store::save_cache(&c)
        }),
        "invest-agenda.json" => aplicar(&texto, |v: Vec<crate::invest::calendario::Evento>| {
            crate::invest::store::save_agenda(&v)
        }),
        "invest-noticias.json" => aplicar(&texto, |v: Vec<(String, crate::invest::rss::Item)>| {
            crate::invest::store::save_noticias(&v)
        }),
        "invest-anunciados.json" => {
            aplicar(&texto, |v: Vec<crate::invest::provento::Anunciado>| {
                crate::invest::store::save_anunciados(&v)
            })
        }
        "invest-patrimonio.json" => aplicar(&texto, |v: Vec<crate::invest::serie::Ponto>| {
            crate::invest::serie::save(&v)
        }),
        _ => false,
    }
}

/// O `engine.json` de antes. Um struct só para esta leitura, porque `engine::Settings`
/// deixou de ser serializável no dia em que virou duas linhas na tabela `config` — e
/// mantê-lo serializável só por causa deste arquivo seria carregar o formato antigo para
/// sempre.
#[derive(serde::Deserialize)]
struct EngineJson {
    #[serde(default)]
    endpoint: String,
}
