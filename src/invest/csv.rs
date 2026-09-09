//! Ler o CSV que a corretora exporta.
//!
//! Não existe «um CSV de corretora». Cada uma tem colunas com nomes diferentes, em ordens
//! diferentes, com decimal em vírgula ou ponto, com ou sem cabeçalho. Exigir um formato
//! fixo empurraria o trabalho de conversão para fora do programa, que é onde ele mais dói.
//!
//! Então: o programa lê o cabeçalho e **propõe** um mapeamento, o usuário corrige o que
//! ele errou, e **o mapeamento fica salvo por fonte**. Da segunda importação em diante é
//! só escolher o arquivo — que é o que transforma isto numa coisa que se usa todo mês.

use std::collections::BTreeMap;

/// O que uma coluna do arquivo significa aqui dentro.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Campo {
    Ativo,
    /// A corretora da linha. Existe porque o extrato consolidado da B3 traz **várias**
    /// numa mesma exportação — ver `invest/b3.rs`. Num CSV de corretora única ela não
    /// aparece, e aí a fonte continua vindo do nome do arquivo.
    Fonte,
    Quantidade,
    PrecoMedio,
    /// Cotação atual, quando o arquivo traz — vira `preco_manual`, e nunca preço médio.
    PrecoAtual,
    Classe,
    Conta,
    Moeda,
    /// A coluna não é usada. É uma resposta legítima e tem que ser escolhível: um arquivo
    /// tem colunas que não interessam.
    Ignorar,
}

impl Campo {
    pub const ALL: [Campo; 9] = [
        Campo::Ativo,
        Campo::Fonte,
        Campo::Quantidade,
        Campo::PrecoMedio,
        Campo::PrecoAtual,
        Campo::Classe,
        Campo::Conta,
        Campo::Moeda,
        Campo::Ignorar,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Campo::Ativo => "ativo",
            Campo::Fonte => "corretora",
            Campo::Quantidade => "quantidade",
            Campo::PrecoMedio => "preço médio",
            Campo::PrecoAtual => "preço atual",
            Campo::Classe => "classe",
            Campo::Conta => "conta",
            Campo::Moeda => "moeda",
            Campo::Ignorar => "(ignorar)",
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Campo::Ativo => "ativo",
            Campo::Fonte => "fonte",
            Campo::Quantidade => "quantidade",
            Campo::PrecoMedio => "preco_medio",
            Campo::PrecoAtual => "preco_atual",
            Campo::Classe => "classe",
            Campo::Conta => "conta",
            Campo::Moeda => "moeda",
            Campo::Ignorar => "ignorar",
        }
    }

    pub fn de_code(code: &str) -> Campo {
        Campo::ALL
            .into_iter()
            .find(|c| c.code() == code)
            .unwrap_or(Campo::Ignorar)
    }

    /// Os nomes de coluna que denunciam este campo. É o palpite inicial, e o usuário
    /// corrige o que ele errar — por isso pode ser generoso sem ser perigoso.
    fn pistas(&self) -> &'static [&'static str] {
        match self {
            Campo::Ativo => &[
                "ativo", "ticker", "codigo", "código", "papel", "symbol", "produto",
            ],
            Campo::Fonte => &[
                "corretora",
                "instituicao",
                "instituição",
                "broker",
                "custodiante",
            ],
            Campo::Quantidade => &["quantidade", "qtd", "qtde", "quantity", "saldo"],
            Campo::PrecoMedio => &[
                "preco medio",
                "preço médio",
                "pm",
                "custo",
                "preco_medio",
                "avg",
                "average",
            ],
            Campo::PrecoAtual => &[
                "preco atual",
                "preço atual",
                "ultimo",
                "último",
                "cotacao",
                "cotação",
                "last",
                "fechamento",
            ],
            Campo::Classe => &["classe", "tipo", "categoria", "class"],
            Campo::Conta => &["conta", "account", "carteira"],
            Campo::Moeda => &["moeda", "currency"],
            Campo::Ignorar => &[],
        }
    }
}

/// O que o arquivo parece ser: separador, e se tem cabeçalho.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dialeto {
    pub separador: char,
    pub tem_cabecalho: bool,
}

/// Descobre o separador **pela consistência entre linhas**, e não pela contagem numa só.
///
/// Contar na primeira linha erra quando um nome de coluna tem vírgula dentro. O que
/// identifica o separador de verdade é ele produzir o **mesmo** número de campos em todas
/// as linhas — que é a propriedade que um separador tem e um caractere qualquer não.
pub fn detectar_dialeto(texto: &str) -> Dialeto {
    let linhas: Vec<&str> = texto
        .lines()
        .filter(|l| !l.trim().is_empty())
        .take(20)
        .collect();
    let candidatos = [';', ',', '\t', '|'];
    let mut melhor = (',', 0usize);
    for sep in candidatos {
        let contagens: Vec<usize> = linhas.iter().map(|l| dividir(l, sep).len()).collect();
        let Some(&primeira) = contagens.first() else {
            continue;
        };
        if primeira < 2 {
            continue;
        }
        let consistentes = contagens.iter().filter(|c| **c == primeira).count();
        // Consistência primeiro; entre dois igualmente consistentes, mais colunas ganha.
        let nota = consistentes * 100 + primeira;
        if nota > melhor.1 {
            melhor = (sep, nota);
        }
    }

    // Cabeçalho é a primeira linha cujos campos não são quase todos números.
    let tem_cabecalho = linhas
        .first()
        .map(|l| {
            let campos = dividir(l, melhor.0);
            let numericos = campos
                .iter()
                .filter(|c| super::calc::ler_numero(c).is_some())
                .count();
            numericos * 2 < campos.len()
        })
        .unwrap_or(true);

    Dialeto {
        separador: melhor.0,
        tem_cabecalho,
    }
}

/// Divide uma linha respeitando aspas — um separador dentro de aspas é texto, não divisão.
pub fn dividir(linha: &str, separador: char) -> Vec<String> {
    let mut campos = Vec::new();
    let mut atual = String::new();
    let mut dentro = false;
    let mut chars = linha.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if dentro && chars.peek() == Some(&'"') => {
                // Aspas duplicadas dentro de aspas são uma aspa literal.
                atual.push('"');
                chars.next();
            }
            '"' => dentro = !dentro,
            c if c == separador && !dentro => campos.push(std::mem::take(&mut atual)),
            c => atual.push(c),
        }
    }
    campos.push(atual);
    campos.into_iter().map(|c| c.trim().to_string()).collect()
}

/// Adivinha o que cada coluna é, pelo nome dela.
pub fn propor(cabecalho: &[String]) -> Vec<Campo> {
    cabecalho
        .iter()
        .map(|nome| {
            let n = normalizar(nome);
            Campo::ALL
                .into_iter()
                .filter(|c| *c != Campo::Ignorar)
                .find(|campo| {
                    campo
                        .pistas()
                        .iter()
                        .any(|p| n == normalizar(p) || n.contains(&normalizar(p)))
                })
                .unwrap_or(Campo::Ignorar)
        })
        .collect()
}

/// Tira acento e caixa, para que «Preço Médio» e «preco medio» sejam a mesma coisa.
fn normalizar(texto: &str) -> String {
    texto
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'Á' | 'À' | 'Â' | 'Ã' => 'a',
            'é' | 'ê' | 'è' | 'É' | 'Ê' => 'e',
            'í' | 'ì' | 'î' | 'Í' => 'i',
            'ó' | 'ô' | 'õ' | 'ò' | 'Ó' | 'Ô' | 'Õ' => 'o',
            'ú' | 'ù' | 'û' | 'Ú' => 'u',
            'ç' | 'Ç' => 'c',
            c => c.to_ascii_lowercase(),
        })
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .trim()
        .to_string()
}

/// Um arquivo lido: as linhas cruas, e o que se decidiu sobre elas.
/// O mapeamento de colunas: o salvo, quando há, e o palpite quando não.
///
/// Um mapeamento já aprendido para esta fonte ganha do palpite: ele foi corrigido por
/// gente uma vez, e o palpite não.
fn mapear(cabecalho: &[String], salvo: Option<&BTreeMap<String, String>>) -> Vec<Campo> {
    match salvo {
        Some(salvo) if !salvo.is_empty() => cabecalho
            .iter()
            .map(|nome| {
                salvo
                    .get(nome)
                    .map(|c| Campo::de_code(c))
                    .unwrap_or(Campo::Ignorar)
            })
            .collect(),
        _ => propor(cabecalho),
    }
}

pub struct Arquivo {
    pub dialeto: Dialeto,
    pub cabecalho: Vec<String>,
    pub linhas: Vec<Vec<String>>,
    pub mapa: Vec<Campo>,
}

impl Arquivo {
    /// Lê o conteúdo bruto de um arquivo. `bytes` e não `String` porque a codificação é
    /// parte do problema — ver `decodificar`.
    pub fn ler(bytes: &[u8], salvo: Option<&BTreeMap<String, String>>) -> Arquivo {
        // O extrato da B3 é JSON, não CSV. Ele entra aqui achatado em linhas e colunas —
        // e daí em diante é indistinguível de um arquivo de corretora, o que significa
        // que ele passa pela **mesma** conferência antes de gravar. Ver `invest/b3.rs`.
        if crate::invest::b3::parece(bytes)
            && let Some((cabecalho, linhas)) = crate::invest::b3::ler(bytes)
        {
            let mapa = mapear(&cabecalho, salvo);
            return Arquivo {
                dialeto: Dialeto {
                    separador: ';',
                    tem_cabecalho: true,
                },
                cabecalho,
                linhas,
                mapa,
            };
        }
        let texto = decodificar(bytes);
        let dialeto = detectar_dialeto(&texto);
        let mut linhas: Vec<Vec<String>> = texto
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| dividir(l, dialeto.separador))
            .collect();

        let cabecalho = match dialeto.tem_cabecalho && !linhas.is_empty() {
            true => linhas.remove(0),
            false => (0..linhas.first().map(|l| l.len()).unwrap_or(0))
                .map(|i| format!("coluna {}", i + 1))
                .collect(),
        };

        // Um mapeamento já aprendido para esta fonte ganha do palpite: ele foi corrigido
        // por gente uma vez, e o palpite não.
        let mapa = mapear(&cabecalho, salvo);

        Arquivo {
            dialeto,
            cabecalho,
            linhas,
            mapa,
        }
    }

    pub fn coluna(&self, linha: &[String], campo: Campo) -> Option<String> {
        let i = self.mapa.iter().position(|c| *c == campo)?;
        linha.get(i).filter(|v| !v.is_empty()).cloned()
    }

    /// Lê um número de uma coluna, com a convenção decimal **da coluna inteira**.
    ///
    /// Existe porque olhar um valor de cada vez não resolve: «55.665» é cinquenta e cinco
    /// mil numa planilha brasileira e cinquenta e cinco vírgula seiscentos e sessenta e
    /// cinco numa americana, e a heurística das três casas errava — num CSV com ponto
    /// decimal, ela gravou um preço médio mil vezes maior. A coluna toda desempata.
    pub fn numero(&self, linha: &[String], campo: Campo) -> Option<f64> {
        let i = self.mapa.iter().position(|c| *c == campo)?;
        let bruto = linha.get(i).filter(|v| !v.is_empty())?;
        let ponto = crate::invest::calc::Ponto::da_coluna(
            self.linhas
                .iter()
                .filter_map(|l| l.get(i))
                .map(|s| s.as_str()),
        );
        crate::invest::calc::ler_numero_com(bruto, ponto)
    }

    /// O mapeamento a guardar, para a próxima importação desta fonte não perguntar nada.
    pub fn para_salvar(&self) -> BTreeMap<String, String> {
        self.cabecalho
            .iter()
            .zip(&self.mapa)
            .filter(|(_, c)| **c != Campo::Ignorar)
            .map(|(nome, campo)| (nome.clone(), campo.code().to_string()))
            .collect()
    }

    /// Se dá para importar: sem saber qual coluna é o ativo e qual é a quantidade, não há
    /// posição nenhuma a montar.
    pub fn falta(&self) -> Option<&'static str> {
        if !self.mapa.contains(&Campo::Ativo) {
            return Some("falta dizer qual coluna é o ativo");
        }
        if !self.mapa.contains(&Campo::Quantidade) {
            return Some("falta dizer qual coluna é a quantidade");
        }
        if !self.mapa.contains(&Campo::PrecoMedio) {
            return Some("falta dizer qual coluna é o preço médio");
        }
        None
    }
}

/// UTF-8 quando o arquivo é UTF-8 válido, Latin-1 quando não é.
///
/// Não é preciosismo: um `ç` mal lido num nome de ativo vira um ativo diferente, e uma
/// importação que cria `POSIÃ‡ÃƒO` ao lado de `POSIÇÃO` é uma carteira duplicada.
pub fn decodificar(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(t) => t.to_string(),
        // Latin-1 é a única outra codificação que aparece de verdade em exportação de
        // corretora brasileira, e ela mapeia byte a byte para os primeiros 256 pontos
        // Unicode — o que torna a conversão exata e sem tabela.
        Err(_) => bytes.iter().map(|b| *b as char).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separador_e_achado_pela_consistencia() {
        // Ponto-e-vírgula, com uma vírgula decimal dentro dos campos: contar vírgulas na
        // primeira linha daria a resposta errada.
        let texto = "ativo;quantidade;preco medio\nPETR4;300;32,10\nVALE3;150;61,20";
        let d = detectar_dialeto(texto);
        assert_eq!(d.separador, ';');
        assert!(d.tem_cabecalho);
    }

    #[test]
    fn arquivo_sem_cabecalho_e_reconhecido() {
        let texto = "PETR4,300,32.10\nVALE3,150,61.20";
        let d = detectar_dialeto(texto);
        assert_eq!(d.separador, ',');
        assert!(!d.tem_cabecalho, "linha só de dados não é cabeçalho");
    }

    #[test]
    fn aspas_protegem_o_separador() {
        let campos = dividir(r#"PETR4,"Petróleo, S.A.",300"#, ',');
        assert_eq!(campos.len(), 3);
        assert_eq!(campos[1], "Petróleo, S.A.");
    }

    #[test]
    fn aspas_duplicadas_viram_uma() {
        let campos = dividir(r#"a,"diz ""oi""",b"#, ',');
        assert_eq!(campos[1], r#"diz "oi""#);
    }

    #[test]
    fn proposta_acerta_nomes_com_acento_e_caixa() {
        let cab: Vec<String> = ["Ativo", "Qtde", "Preço Médio", "Saldo Bruto"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let m = propor(&cab);
        assert_eq!(m[0], Campo::Ativo);
        assert_eq!(m[1], Campo::Quantidade);
        assert_eq!(m[2], Campo::PrecoMedio);
    }

    #[test]
    fn latin1_e_lido_sem_estragar_acento() {
        // "POSIÇÃO" em Latin-1.
        let bytes = b"POSI\xC7\xC3O";
        let texto = decodificar(bytes);
        assert_eq!(texto, "POSIÇÃO");
        // E UTF-8 continua sendo lido como UTF-8.
        assert_eq!(decodificar("POSIÇÃO".as_bytes()), "POSIÇÃO");
    }

    #[test]
    fn mapeamento_salvo_ganha_do_palpite() {
        let bytes = b"coisa;numero;valor\nPETR4;300;32,10";
        let mut salvo = BTreeMap::new();
        salvo.insert("coisa".to_string(), "ativo".to_string());
        salvo.insert("numero".to_string(), "quantidade".to_string());
        salvo.insert("valor".to_string(), "preco_medio".to_string());
        let a = Arquivo::ler(bytes, Some(&salvo));
        assert_eq!(a.mapa[0], Campo::Ativo);
        assert_eq!(a.mapa[2], Campo::PrecoMedio);
        assert!(a.falta().is_none());
        // Sem o mapeamento salvo, o palpite não teria como acertar «coisa».
        let sem = Arquivo::ler(bytes, None);
        assert!(sem.falta().is_some());
    }

    #[test]
    fn arquivo_incompleto_diz_o_que_falta() {
        let a = Arquivo::ler(b"ativo;coisa\nPETR4;x", None);
        assert_eq!(a.falta(), Some("falta dizer qual coluna é a quantidade"));
    }
}
