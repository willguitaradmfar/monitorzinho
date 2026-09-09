//! Um leitor de RSS, e nada além disso.
//!
//! RSS é XML sobre HTTP: sem chave, sem cota, sem termo de uso hostil. É a única fonte de
//! informação desta aba com as mesmas garantias do Banco Central.
//!
//! O custo seria precisar de um leitor de XML — mas um leitor de **RSS** não é um leitor
//! de XML geral. Os campos são meia dúzia e a estrutura é rasa, então isto é um extrator
//! de tags conhecidas, com teto de tamanho e **sem entidades externas**. Um analisador de
//! XML genérico não seria auditável em cinquenta linhas; este é.

/// Um item de feed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Item {
    pub titulo: String,
    pub link: String,
    pub data: String,
    pub resumo: String,
    /// A data em epoch, quando deu para lê-la. É por ela que a lista ordena — sem isso, a
    /// ordem era a de chegada das threads, que muda a cada volta e mistura os feeds.
    pub em: Option<u64>,
}

/// Quantos itens se guarda por feed. Um feed com mil itens não pode encher a memória.
pub const MAX_ITENS: usize = 60;

/// Extrai os itens de um documento RSS ou Atom.
pub fn parse(texto: &str) -> Vec<Item> {
    let mut itens = Vec::new();
    // RSS usa `<item>`, Atom usa `<entry>`. Os dois aparecem no mundo real.
    for abertura in ["<item", "<entry"] {
        let mut resto = texto;
        while let Some(inicio) = resto.find(abertura) {
            let bloco_inicio = &resto[inicio..];
            let fim = bloco_inicio
                .find("</item>")
                .or_else(|| bloco_inicio.find("</entry>"))
                .unwrap_or(bloco_inicio.len());
            let bloco = &bloco_inicio[..fim];
            let titulo = tag(bloco, "title").unwrap_or_default();
            if !titulo.is_empty() {
                let data = tag(bloco, "pubDate")
                    .or_else(|| tag(bloco, "updated"))
                    .or_else(|| tag(bloco, "published"))
                    .unwrap_or_default();
                itens.push(Item {
                    titulo,
                    link: tag(bloco, "link").unwrap_or_else(|| atributo(bloco, "href")),
                    em: instante(&data),
                    data,
                    resumo: tag(bloco, "description")
                        .or_else(|| tag(bloco, "summary"))
                        .unwrap_or_default(),
                });
            }
            resto = &bloco_inicio[fim.max(1)..];
            if itens.len() >= MAX_ITENS {
                return itens;
            }
        }
        if !itens.is_empty() {
            break;
        }
    }
    itens
}

/// A data de um item em epoch.
///
/// **O mundo real não usa dois formatos, usa vários.** O plano dizia RFC 822 no RSS e ISO
/// 8601 no Atom; medindo, o investing.com manda `Sep 07, 2026 12:17 GMT` — mês antes do
/// dia, com vírgula, sem segundos e com o fuso por nome. Não é nenhum dos dois, e um
/// leitor posicional devolvia `None` para o feed inteiro.
///
/// Por isso a ISO — que é sem ambiguidade — é tentada primeiro, e o resto cai num leitor
/// que acha cada campo **pela forma**: o mês é o pedaço que se parece com um mês, o ano é
/// o número de quatro dígitos, a hora é o que tem dois-pontos. Assim o próximo formato
/// esquisito tem chance de entrar sozinho.
///
/// `None` numa data que não se entende — e aí o item vai para o fim da lista em vez de
/// fingir uma posição. Um item sem data no topo seria pior que um item sem data no fim.
pub fn instante(texto: &str) -> Option<u64> {
    let texto = texto.trim();
    if texto.is_empty() {
        return None;
    }
    crate::invest::tempo::epoch_de_iso(texto).or_else(|| por_partes(texto))
}

const MESES_EN: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Lê uma data achando cada campo pela forma dele, e não pela posição.
///
/// Serve `Mon, 08 Sep 2026 14:22:00 -0300` e `Sep 07, 2026 12:17 GMT` com o mesmo código.
/// O dia da semana some sozinho: nenhuma abreviação de dia em inglês começa como um mês.
fn por_partes(texto: &str) -> Option<u64> {
    let partes: Vec<&str> = texto
        .split([' ', ',', '\t'])
        .filter(|p| !p.is_empty())
        .collect();

    let mes = partes.iter().find_map(|p| {
        let baixo = p.to_lowercase();
        MESES_EN.iter().position(|m| baixo.starts_with(m))
    })? as u32
        + 1;

    // O ano é o número de quatro dígitos; o dia é o de um ou dois. Separá-los pelo
    // tamanho, e não pela ordem, é o que faz os dois arranjos caírem no mesmo lugar.
    let ano: i32 = partes
        .iter()
        .find(|p| p.len() == 4 && p.chars().all(|c| c.is_ascii_digit()))?
        .parse()
        .ok()?;
    let dia: u32 = partes
        .iter()
        .filter(|p| (1..=2).contains(&p.len()) && p.chars().all(|c| c.is_ascii_digit()))
        .find_map(|p| p.parse().ok().filter(|d| (1..=31).contains(d)))?;

    // A hora é opcional: uma data sem hora vale meia-noite, e é melhor que descartar o
    // item por causa dela.
    let (hh, mm, ss) = partes
        .iter()
        .find(|p| p.contains(':'))
        .map(|hora| {
            let mut h = hora.split(':');
            (
                h.next().unwrap_or("0").parse().unwrap_or(0i64),
                h.next().unwrap_or("0").parse().unwrap_or(0i64),
                h.next().unwrap_or("0").parse().unwrap_or(0i64),
            )
        })
        .unwrap_or((0, 0, 0));

    let dias = crate::invest::tempo::dias_de(ano, mes, dia);
    let deslocamento = fuso(&partes);
    Some((dias * 86400 + hh * 3600 + mm * 60 + ss - deslocamento).max(0) as u64)
}

/// O fuso da data, em segundos. `-0300`, `-03:00`, `Z`, `GMT` e `UTC`.
///
/// Sem isto, itens de feeds em fusos diferentes se ordenam errado por até um dia — que é
/// exatamente a ordenação que esta lista existe para acertar. Um fuso por nome que não
/// seja UTC (`EST`, `BRT`) vale zero: chutar seria pior que assumir o meridiano, porque
/// erraria calado em três horas.
fn fuso(partes: &[&str]) -> i64 {
    for p in partes {
        if matches!(*p, "Z" | "GMT" | "UTC" | "UT") {
            return 0;
        }
        let Some(resto) = p.strip_prefix(['+', '-']) else {
            continue;
        };
        let sinal = if p.starts_with('-') { -1 } else { 1 };
        let digitos: String = resto.chars().filter(|c| c.is_ascii_digit()).collect();
        if digitos.len() == 4
            && let (Ok(horas), Ok(minutos)) =
                (digitos[..2].parse::<i64>(), digitos[2..].parse::<i64>())
        {
            return sinal * (horas * 3600 + minutos * 60);
        }
    }
    0
}

/// O conteúdo de `<nome>…</nome>`, desembrulhado de `CDATA` e com as entidades básicas
/// desfeitas. Nada de entidade externa: o único conjunto reconhecido é este, fixo.
fn tag(bloco: &str, nome: &str) -> Option<String> {
    let abre = format!("<{nome}");
    let fecha = format!("</{nome}>");
    let inicio = bloco.find(&abre)?;
    let depois = &bloco[inicio + abre.len()..];
    // Pula o resto da tag de abertura (atributos), até o `>`.
    let corpo_inicio = depois.find('>')? + 1;
    let corpo = &depois[corpo_inicio..];
    let fim = corpo.find(&fecha)?;
    let bruto = &corpo[..fim];
    let bruto = bruto
        .trim()
        .strip_prefix("<![CDATA[")
        .and_then(|s| s.strip_suffix("]]>"))
        .unwrap_or(bruto);
    Some(desfazer_entidades(bruto.trim()))
}

fn atributo(bloco: &str, nome: &str) -> String {
    let alvo = format!("{nome}=\"");
    match bloco.find(&alvo) {
        Some(i) => {
            let resto = &bloco[i + alvo.len()..];
            resto
                .find('"')
                .map(|f| resto[..f].to_string())
                .unwrap_or_default()
        }
        None => String::new(),
    }
}

fn desfazer_entidades(texto: &str) -> String {
    texto
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        // `&amp;` por último: desfazê-lo antes transformaria `&amp;lt;` em `<`.
        .replace("&amp;", "&")
}

/// Tira as tags HTML de um resumo, que quase todo feed manda cheio delas.
pub fn sem_html(texto: &str) -> String {
    let mut saida = String::new();
    let mut dentro = false;
    for c in texto.chars() {
        match c {
            '<' => dentro = true,
            '>' => dentro = false,
            c if !dentro => saida.push(c),
            _ => {}
        }
    }
    saida.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const RSS: &str = r#"<?xml version="1.0"?><rss><channel>
      <item>
        <title>Petrobras aprova dividendos</title>
        <link>https://exemplo.com/1</link>
        <pubDate>Mon, 08 Sep 2026 14:22:00 -0300</pubDate>
        <description><![CDATA[<p>A empresa &amp; o conselho aprovaram.</p>]]></description>
      </item>
      <item>
        <title>Copom mantém a Selic</title>
        <link>https://exemplo.com/2</link>
        <description>Sem &lt;surpresa&gt;</description>
      </item>
    </channel></rss>"#;

    #[test]
    fn le_os_itens_de_um_rss() {
        let itens = parse(RSS);
        assert_eq!(itens.len(), 2);
        assert_eq!(itens[0].titulo, "Petrobras aprova dividendos");
        assert_eq!(itens[0].link, "https://exemplo.com/1");
        assert!(itens[0].data.contains("2026"));
    }

    #[test]
    fn cdata_e_desembrulhado_e_as_entidades_desfeitas() {
        let itens = parse(RSS);
        assert!(itens[0].resumo.contains("A empresa & o conselho"));
        assert_eq!(itens[1].resumo, "Sem <surpresa>");
    }

    #[test]
    fn amp_e_desfeito_por_ultimo() {
        // `&amp;lt;` tem que virar `&lt;` literal, e não `<`.
        assert_eq!(desfazer_entidades("&amp;lt;"), "&lt;");
    }

    #[test]
    fn atom_tambem_e_lido() {
        let atom = r#"<feed><entry>
            <title>Fed sinaliza cautela</title>
            <link href="https://exemplo.com/3"/>
            <updated>2026-09-08T12:00:00Z</updated>
            <summary>Texto</summary>
        </entry></feed>"#;
        let itens = parse(atom);
        assert_eq!(itens.len(), 1);
        assert_eq!(itens[0].titulo, "Fed sinaliza cautela");
        assert_eq!(itens[0].link, "https://exemplo.com/3");
    }

    #[test]
    fn le_as_duas_formas_de_data_que_os_feeds_usam() {
        // RFC 822 no RSS, ISO 8601 no Atom. Os dois aparecem no mundo real.
        let rss = instante("Mon, 08 Sep 2026 14:22:00 -0300").unwrap();
        let atom = instante("2026-09-08T17:22:00Z").unwrap();
        assert_eq!(rss, atom, "as duas datas são o mesmo instante");
    }

    #[test]
    fn o_fuso_e_respeitado() {
        // Sem ele, itens de feeds em fusos diferentes se ordenariam errado por até um dia.
        let brasilia = instante("Mon, 08 Sep 2026 14:00:00 -0300").unwrap();
        let utc = instante("Mon, 08 Sep 2026 14:00:00 +0000").unwrap();
        assert_eq!(brasilia - utc, 3 * 3600);
    }

    #[test]
    fn data_ilegivel_nao_vira_um_instante_qualquer() {
        assert_eq!(instante("amanhã de manhã"), None);
        assert_eq!(instante(""), None);
        assert_eq!(instante("Mon, 08 Xyz 2026 14:00:00 -0300"), None);
        // Sem ano não dá para datar, e inventar o ano corrente ordenaria errado toda
        // virada de dezembro.
        assert_eq!(instante("Sep 07 12:17 GMT"), None);
    }

    #[test]
    fn le_o_formato_do_investing_com_que_nao_e_nenhum_dos_dois() {
        // `Sep 07, 2026 12:17 GMT` — mês antes do dia, vírgula, sem segundos, fuso por
        // nome. Não é RFC 822 nem ISO 8601, e era o que fazia a coluna «Quando» dizer
        // «sem data» para os dois feeds que o usuário tinha.
        let em = instante("Sep 07, 2026 12:17 GMT").expect("tem que ler");
        let iso = instante("2026-09-07T12:17:00Z").expect("a ISO do mesmo instante");
        assert_eq!(em, iso);
    }

    #[test]
    fn a_ordem_dos_campos_nao_importa_e_o_dia_da_semana_e_ignorado() {
        // O leitor acha cada campo pela forma. Estes três dizem o mesmo instante.
        let a = instante("Mon, 07 Sep 2026 12:17:00 +0000").unwrap();
        let b = instante("Sep 07, 2026 12:17 GMT").unwrap();
        let c = instante("07 Sep 2026 12:17 UTC").unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn o_fuso_e_respeitado_e_um_fuso_por_nome_desconhecido_nao_chuta() {
        // Meio-dia em Brasília é 15:00 UTC. Errar isto ordena um feed brasileiro um dia
        // inteiro fora do lugar em relação a um americano.
        let brt = instante("Mon, 07 Sep 2026 12:00:00 -0300").unwrap();
        let utc = instante("Mon, 07 Sep 2026 15:00:00 +0000").unwrap();
        assert_eq!(brt, utc);
        assert_eq!(instante("07 Sep 2026 12:00:00 -03:00").unwrap(), brt);
        // `EST` não é reconhecido: vale meridiano, e não um chute de três horas.
        assert_eq!(
            instante("07 Sep 2026 15:00:00 EST").unwrap(),
            instante("07 Sep 2026 15:00:00 GMT").unwrap()
        );
    }

    #[test]
    fn uma_data_sem_hora_vale_meia_noite_em_vez_de_ser_descartada() {
        let em = instante("Sep 07, 2026").expect("data sem hora ainda é uma data");
        assert_eq!(em, instante("2026-09-07T00:00:00Z").unwrap());
    }

    #[test]
    fn os_itens_saem_com_a_data_ja_lida() {
        let itens = parse(RSS);
        assert!(itens[0].em.is_some(), "o primeiro tem pubDate");
        assert!(itens[1].em.is_none(), "o segundo não tem, e isso é dito");
    }

    #[test]
    fn html_some_do_resumo() {
        assert_eq!(sem_html("<p>Um <b>texto</b>   aqui</p>"), "Um texto aqui");
    }

    #[test]
    fn documento_sem_item_nao_estoura() {
        assert!(parse("<rss><channel></channel></rss>").is_empty());
        assert!(parse("").is_empty());
        assert!(parse("isto não é XML").is_empty());
    }

    #[test]
    fn o_teto_de_itens_e_respeitado() {
        let muitos: String = (0..200)
            .map(|i| format!("<item><title>n{i}</title></item>"))
            .collect();
        assert_eq!(parse(&muitos).len(), MAX_ITENS);
    }
    #[test]
    #[ignore]
    fn ao_vivo_o_g1_que_responde_gzip_agora_e_lido() {
        // O feed que motivou o descompressor. Antes disto ele contribuía zero itens, e a
        // ordenação por data ordenava uma fonte só.
        let r = crate::invest::feed::get("https://g1.globo.com/rss/g1/economia/")
            .expect("o G1 tem que responder");
        let texto = String::from_utf8_lossy(&r.body);
        assert!(
            texto.starts_with("<?xml"),
            "veio XML e não bytes comprimidos"
        );
        let itens = crate::invest::rss::parse(&texto);
        assert!(itens.len() > 5, "só {} itens", itens.len());
        assert!(
            itens.iter().filter(|i| i.em.is_some()).count() > 5,
            "as datas têm que ser legíveis, senão a ordenação não ordena"
        );
    }

    #[test]
    #[ignore]
    fn ao_vivo_os_feeds_do_investing_saem_com_data() {
        // O feed que motivou o leitor tolerante. Sem ele a coluna «Quando» dizia «sem
        // data» em toda linha, e a ordenação por data ordenava nada.
        for url in [
            "https://br.investing.com/rss/286.rss",
            "https://br.investing.com/rss/290.rss",
        ] {
            let r = crate::invest::feed::get(url).expect("o feed tem que responder");
            let itens = parse(&String::from_utf8_lossy(&r.body));
            assert!(!itens.is_empty(), "{url} veio sem itens");
            let com_data = itens.iter().filter(|i| i.em.is_some()).count();
            assert_eq!(
                com_data,
                itens.len(),
                "{url}: {com_data} de {} com data",
                itens.len()
            );
        }
    }
}
