//! As contas que mais de um módulo faz, num lugar só — e a formatação de dinheiro.
//!
//! Cada função aqui tem teste, e várias têm teste **por causa de uma armadilha
//! conhecida**: acumular juro por soma, anualizar volatilidade pelo fator errado, semear
//! uma média exponencial no ponto errado. São erros que produzem números plausíveis, que
//! é o pior tipo.

// ---------------------------------------------------------------------------
// Dinheiro e porcentagem
// ---------------------------------------------------------------------------

/// `1234567.8` → `1.234.567,80`. Ponto para milhar e vírgula para decimal, que é como se
/// escreve dinheiro em português — e como o usuário vai digitar de volta.
pub fn moeda(valor: f64) -> String {
    if !valor.is_finite() {
        return "—".to_string();
    }
    let negativo = valor < 0.0;
    let bruto = format!("{:.2}", valor.abs());
    let (inteiro, decimal) = bruto.split_once('.').unwrap_or((bruto.as_str(), "00"));
    let mut grupos = String::new();
    for (i, c) in inteiro.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grupos.push('.');
        }
        grupos.push(c);
    }
    let inteiro: String = grupos.chars().rev().collect();
    format!("{}{inteiro},{decimal}", if negativo { "-" } else { "" })
}

/// Preço, que precisa de mais casas que dinheiro: um câmbio de 5,4210 e uma cripto de
/// 0,00004120 perdem o que interessa com duas casas.
/// Um valor em reais **curto o bastante para caber numa caixa**: `1,2 mi`, `747,9 mil`,
/// `930`.
///
/// Existe por causa de um perigo concreto no heatmap: cortar `R$ 30.291,00` na largura da
/// caixa dava `R$ 30.29`, que não é um número truncado — é **outro número**, e se lê como
/// trinta reais e vinte e nove centavos. Uma grandeza abreviada diz menos; uma grandeza
/// cortada mente.
pub fn moeda_curta(valor: f64) -> String {
    if !valor.is_finite() {
        return "—".to_string();
    }
    let (sinal, v) = match valor < 0.0 {
        true => ("-", -valor),
        false => ("", valor),
    };
    match v {
        v if v >= 1e9 => format!("{sinal}{} bi", virgula(v / 1e9, 1)),
        v if v >= 1e6 => format!("{sinal}{} mi", virgula(v / 1e6, 1)),
        v if v >= 1e3 => format!("{sinal}{} mil", virgula(v / 1e3, 1)),
        v => format!("{sinal}{}", virgula(v, 0)),
    }
}

pub fn preco(valor: f64) -> String {
    if !valor.is_finite() {
        return "—".to_string();
    }
    let abs = valor.abs();
    let casas = match abs {
        a if a >= 1000.0 => 2,
        a if a >= 1.0 => 2,
        a if a >= 0.01 => 4,
        a if a > 0.0 => 8,
        _ => 2,
    };
    let bruto = format!("{:.*}", casas, abs);
    let (inteiro, decimal) = bruto.split_once('.').unwrap_or((bruto.as_str(), ""));
    let mut grupos = String::new();
    for (i, c) in inteiro.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grupos.push('.');
        }
        grupos.push(c);
    }
    let inteiro: String = grupos.chars().rev().collect();
    format!(
        "{}{inteiro}{}",
        if valor < 0.0 { "-" } else { "" },
        match decimal.is_empty() {
            true => String::new(),
            false => format!(",{decimal}"),
        }
    )
}

/// `+2,14%`. O sinal é sempre explícito: numa coluna de variação, `2,14%` sem sinal se lê
/// como alta por hábito, e uma queda que se lê como alta é o pior erro possível ali.
pub fn pct(valor: f64) -> String {
    match valor.is_finite() {
        true => format!(
            "{}{}%",
            if valor >= 0.0 { "+" } else { "" },
            virgula(valor, 2)
        ),
        false => "—".to_string(),
    }
}

/// `{:.n}` com vírgula no lugar do ponto. Existe porque o Rust formata com ponto e o
/// resto da tela escreve dinheiro com vírgula — misturar os dois na mesma coluna é como
/// se lê 2.14 como dois mil e cento e quarenta.
fn virgula(valor: f64, casas: usize) -> String {
    // Zero negativo nunca vai para a tela. `-0.0 == 0.0` é verdadeiro, mas o formatador
    // imprime «-0,0» — e um peso de «-0,0%» num ativo que simplesmente não se tem parece
    // um erro de conta. Somar zero normaliza o sinal.
    let valor = valor + 0.0;
    let valor = match valor == 0.0 {
        true => 0.0,
        false => valor,
    };
    format!("{:.*}", casas, valor).replace('.', ",")
}

/// Porcentagem sem sinal, para peso e alocação, onde não há alta nem queda.
pub fn pct_simples(valor: f64) -> String {
    pct_casas(valor, 1)
}

/// Porcentagem com o número de casas que a grandeza exige.
///
/// Uma casa serve para peso e alocação. **Não serve para taxa diária**: o CDI de
/// 0,051660% ao dia vira «0,1%» com uma casa, o que perde justamente o que se veio ler.
pub fn pct_casas(valor: f64, casas: usize) -> String {
    match valor.is_finite() {
        true => format!("{}%", virgula(valor, casas)),
        false => "—".to_string(),
    }
}

/// Pontos percentuais. `+4,2 p.p.` — e a unidade escrita por extenso é o ponto: «cripto
/// subiu 4,2%» e «cripto está 4,2 p.p. acima do alvo» são frases diferentes, e confundi-las
/// é como se toma a decisão errada de aporte.
pub fn pp(valor: f64) -> String {
    match valor.is_finite() {
        true => format!(
            "{}{} p.p.",
            if valor >= 0.0 { "+" } else { "" },
            virgula(valor, 1)
        ),
        false => "—".to_string(),
    }
}

/// Lê um número escrito por gente: `1.234,56`, `1234.56`, `1,5`, `R$ 38,42`.
///
/// A ambiguidade real é `1.200`: mil e duzentos, ou um vírgula dois? A regra usada é a que
/// acerta na esmagadora maioria: **se há vírgula em algum lugar, o ponto é milhar**; sem
/// vírgula, um ponto com exatamente três dígitos depois é milhar — a menos que o que vem
/// antes seja `0`, porque ninguém escreve zero mil.
pub fn ler_numero(texto: &str) -> Option<f64> {
    ler_numero_com(texto, Ponto::Adivinhar)
}

/// O que o ponto significa num número — quando se sabe.
///
/// «1.200» é mil e duzentos numa planilha brasileira e um vírgula dois numa americana, e
/// **nenhuma regra olhando só esse valor acerta os dois**. Adivinhando pelo número de
/// casas, «55.665» virava cinquenta e cinco mil — num CSV que usava ponto decimal em toda
/// linha, isso gravou um preço médio mil vezes maior.
///
/// Por isso quem lê uma coluna inteira decide antes, olhando a coluna: ver `Ponto::da_coluna`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ponto {
    /// Nada se sabe: vale a heurística das três casas.
    Adivinhar,
    /// O ponto separa decimais — `55.665` é cinquenta e cinco vírgula seiscentos e
    /// sessenta e cinco.
    Decimal,
    /// O ponto separa milhares — `55.665` é cinquenta e cinco mil.
    Milhar,
}

impl Ponto {
    /// O que o ponto significa nesta coluna, olhando todos os valores dela.
    ///
    /// Uma vírgula em qualquer valor resolve na hora: quem escreve vírgula decimal usa o
    /// ponto para milhar. Sem vírgula nenhuma, um único valor com um número de casas
    /// **diferente de três** depois do ponto prova que o ponto é decimal — nenhum
    /// separador de milhar produz `37.0538` nem `53.00`.
    pub fn da_coluna<'a>(valores: impl Iterator<Item = &'a str>) -> Ponto {
        let mut viu_ponto = false;
        for v in valores {
            if v.contains(',') {
                return Ponto::Milhar;
            }
            if let Some((_, depois)) = v.trim().split_once('.') {
                viu_ponto = true;
                let casas = depois.chars().filter(|c| c.is_ascii_digit()).count();
                if casas != 3 && !depois.contains('.') {
                    return Ponto::Decimal;
                }
            }
        }
        match viu_ponto {
            // Só valores de três casas, e nenhuma vírgula: genuinamente ambíguo.
            true => Ponto::Adivinhar,
            false => Ponto::Decimal,
        }
    }
}

pub fn ler_numero_com(texto: &str, ponto: Ponto) -> Option<f64> {
    let limpo: String = texto
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == ',' || *c == '.' || *c == '-')
        .collect();
    if limpo.is_empty() {
        return None;
    }
    let normalizado = if limpo.contains(',') {
        limpo.replace('.', "").replace(',', ".")
    } else {
        match ponto {
            Ponto::Decimal => limpo,
            Ponto::Milhar => limpo.replace('.', ""),
            Ponto::Adivinhar => match limpo.split_once('.') {
                Some((antes, depois))
                    if depois.len() == 3
                        && !depois.contains('.')
                        && !antes.is_empty()
                        && antes.trim_start_matches('-') != "0" =>
                {
                    limpo.replace('.', "")
                }
                _ => limpo,
            },
        }
    };
    normalizado.parse().ok().filter(|v: &f64| v.is_finite())
}

#[cfg(test)]
mod tests_moeda_curta {
    use super::moeda_curta;

    /// O perigo que ela existe para evitar: `R$ 30.291,00` cortado na largura da caixa
    /// virava `R$ 30.29`, que se lê como trinta reais. Abreviar diz menos; cortar mente.
    #[test]
    fn cabe_em_dez_caracteres_em_qualquer_ordem_de_grandeza() {
        for v in [0.0, 9.5, 930.0, 30_291.0, 747_860.78, 1_983_599.89, 4.2e9] {
            let texto = moeda_curta(v);
            assert!(
                texto.chars().count() <= 10,
                "{v} deu «{texto}», que não cabe"
            );
        }
    }

    #[test]
    fn a_ordem_de_grandeza_vai_junto() {
        assert_eq!(moeda_curta(930.0), "930");
        assert_eq!(moeda_curta(30_291.0), "30,3 mil");
        assert_eq!(moeda_curta(747_860.78), "747,9 mil");
        assert_eq!(moeda_curta(1_983_599.89), "2,0 mi");
    }

    #[test]
    fn o_sinal_sobrevive() {
        assert_eq!(moeda_curta(-30_291.0), "-30,3 mil");
        assert_eq!(moeda_curta(-0.0), "0");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dinheiro_agrupa_milhar_com_ponto() {
        assert_eq!(moeda(1234567.8), "1.234.567,80");
        assert_eq!(moeda(0.0), "0,00");
        assert_eq!(moeda(-1234.5), "-1.234,50");
        assert_eq!(moeda(100.0), "100,00");
        assert_eq!(moeda(f64::NAN), "—");
    }

    #[test]
    fn preco_ganha_casas_quando_o_valor_e_pequeno() {
        assert_eq!(preco(512340.0), "512.340,00");
        assert_eq!(preco(5.4210), "5,42");
        assert_eq!(preco(0.0412), "0,0412");
        assert!(preco(0.00004120).starts_with("0,0000412"));
    }

    #[test]
    fn taxa_diaria_precisa_de_casas() {
        // Com uma casa, o CDI diário desaparece — e é ele que se veio ler.
        assert_eq!(pct_simples(0.051660), "0,1%");
        assert_eq!(pct_casas(0.051660, 4), "0,0517%");
    }

    #[test]
    fn variacao_sempre_tem_sinal() {
        // Numa coluna de variação, um número sem sinal se lê como alta por hábito.
        assert_eq!(pct(2.14), "+2,14%");
        assert_eq!(pct(-0.42), "-0,42%");
        assert_eq!(pct(0.0), "+0,00%");
    }

    #[test]
    fn ler_numero_resolve_a_ambiguidade_do_ponto() {
        assert_eq!(ler_numero("1.234,56"), Some(1234.56));
        assert_eq!(ler_numero("1234.56"), Some(1234.56));
        assert_eq!(ler_numero("R$ 38,42"), Some(38.42));
        assert_eq!(ler_numero("1,5"), Some(1.5));
        // Com vírgula presente, o ponto é milhar.
        assert_eq!(ler_numero("1.200,50"), Some(1200.5));
        // Sem vírgula, três dígitos depois do ponto: milhar.
        assert_eq!(ler_numero("1.200"), Some(1200.0));
        assert_eq!(ler_numero("12.500"), Some(12500.0));
        // Mas 0.412 é decimal, porque ninguém escreve zero mil.
        assert_eq!(ler_numero("0.412"), Some(0.412));
        // E um ponto com menos de três dígitos depois é sempre decimal.
        assert_eq!(ler_numero("1.5"), Some(1.5));
        assert_eq!(ler_numero("abc"), None);
        assert_eq!(ler_numero(""), None);
    }
}

/// `2 fontes`, `1 fonte`. Existe porque «1 fontes» aparece em toda tela que conta coisas,
/// e concordar o plural em cada uma delas à mão é como se esquece de uma.
pub fn plural(n: usize, singular: &str, plural: &str) -> String {
    match n {
        1 => format!("1 {singular}"),
        n => format!("{n} {plural}"),
    }
}

#[cfg(test)]
mod plural_tests {
    use super::plural;

    #[test]
    fn concorda_o_numero() {
        assert_eq!(plural(1, "fonte", "fontes"), "1 fonte");
        assert_eq!(plural(2, "fonte", "fontes"), "2 fontes");
        assert_eq!(plural(0, "fonte", "fontes"), "0 fontes");
    }
}

#[cfg(test)]
mod ponto_tests {
    use super::{Ponto, ler_numero, ler_numero_com};

    fn coluna(v: &[&str]) -> Ponto {
        Ponto::da_coluna(v.iter().copied())
    }

    #[test]
    fn uma_planilha_com_ponto_decimal_e_reconhecida_pela_coluna() {
        // O caso real que motivou isto: um CSV com `53.00`, `37.0538` e `55.665` na mesma
        // coluna. Olhando `55.665` sozinho, a heurística diz «milhar» e grava cinquenta e
        // cinco mil como preço médio da VALE3 — mil vezes o valor certo. As outras linhas
        // da coluna provam que o ponto é decimal.
        let c = coluna(&["53.00", "37.0538", "55.665", "259.4367"]);
        assert_eq!(c, Ponto::Decimal);
        assert_eq!(ler_numero_com("55.665", c), Some(55.665));
        assert_eq!(ler_numero_com("112.414", c), Some(112.414));
    }

    #[test]
    fn uma_planilha_brasileira_continua_lendo_milhar() {
        // Vírgula em qualquer valor resolve na hora: quem escreve vírgula decimal usa o
        // ponto para milhar.
        let c = coluna(&["1.200", "55,66", "112.414"]);
        assert_eq!(c, Ponto::Milhar);
        assert_eq!(ler_numero_com("1.200", c), Some(1200.0));
        assert_eq!(ler_numero_com("112.414", c), Some(112414.0));
    }

    #[test]
    fn sem_ponto_nenhum_a_coluna_nao_precisa_desempatar() {
        assert_eq!(coluna(&["400", "1100", "9850"]), Ponto::Decimal);
    }

    #[test]
    fn tres_casas_em_toda_a_coluna_continua_ambiguo_e_cai_na_heuristica() {
        // Aqui não há informação para decidir, e inventar uma seria pior que manter a
        // regra antiga — que ao menos é a mesma de sempre.
        let c = coluna(&["1.200", "3.500"]);
        assert_eq!(c, Ponto::Adivinhar);
        assert_eq!(ler_numero_com("1.200", c), ler_numero("1.200"));
    }

    #[test]
    fn a_leitura_de_um_valor_solto_nao_mudou() {
        // `ler_numero` continua sendo o que era para quem não tem uma coluna inteira.
        assert_eq!(ler_numero("1.200"), Some(1200.0));
        assert_eq!(ler_numero("0.567"), Some(0.567));
        assert_eq!(ler_numero("12,34"), Some(12.34));
        assert_eq!(ler_numero("-5.5"), Some(-5.5));
    }
}
