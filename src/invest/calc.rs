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

// ---------------------------------------------------------------------------
// Séries
// ---------------------------------------------------------------------------

/// Retornos simples de uma série de preços: `p[i]/p[i-1] − 1`.
pub fn retornos(serie: &[f64]) -> Vec<f64> {
    serie
        .windows(2)
        .filter(|w| w[0] > 0.0)
        .map(|w| w[1] / w[0] - 1.0)
        .collect()
}

pub fn media(v: &[f64]) -> f64 {
    match v.is_empty() {
        true => 0.0,
        false => v.iter().sum::<f64>() / v.len() as f64,
    }
}

/// Desvio padrão **amostral** (divide por n−1). Amostral e não populacional porque uma
/// série de retornos é uma amostra do comportamento do ativo, não a população inteira.
pub fn desvio(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let m = media(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

/// Pregões num ano. O fator de anualização é `√252`, e **não** 252: variância escala com
/// o tempo, desvio padrão escala com a raiz dele. Anualizar por 252 infla a volatilidade
/// por um fator de quase 16.
pub const PREGOES_ANO: f64 = 252.0;

pub fn volatilidade_anual(retornos: &[f64]) -> f64 {
    desvio(retornos) * PREGOES_ANO.sqrt() * 100.0
}

/// A maior queda de um topo até o fundo seguinte, em porcentagem (negativa).
pub fn drawdown_maximo(serie: &[f64]) -> (f64, usize, usize) {
    let (mut pior, mut inicio, mut fim) = (0.0, 0usize, 0usize);
    let (mut topo, mut topo_em) = (f64::NEG_INFINITY, 0usize);
    for (i, &v) in serie.iter().enumerate() {
        if v > topo {
            topo = v;
            topo_em = i;
        }
        if topo > 0.0 {
            let queda = (v / topo - 1.0) * 100.0;
            if queda < pior {
                pior = queda;
                inicio = topo_em;
                fim = i;
            }
        }
    }
    (pior, inicio, fim)
}

/// Quanto abaixo do topo histórico a série está agora.
pub fn drawdown_atual(serie: &[f64]) -> f64 {
    let Some(&ultimo) = serie.last() else {
        return 0.0;
    };
    let topo = serie.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    match topo > 0.0 {
        true => (ultimo / topo - 1.0) * 100.0,
        false => 0.0,
    }
}

/// Sharpe anualizado. O «sem risco» é o CDI, e não zero: no Brasil, comparar contra zero
/// dá um número que elogia qualquer coisa.
/// Abaixo desta volatilidade anualizada não há Sharpe que signifique alguma coisa.
///
/// `vol > 0.0` não basta: uma série de retornos constantes tem desvio de ~1e-19 por erro
/// de ponto flutuante, e não zero — o que produziria um Sharpe da ordem de 10¹⁶. Um
/// número desses seria desenhado na tela como se fosse um resultado.
const VOL_MINIMA: f64 = 1e-4;

pub fn sharpe(retornos: &[f64], cdi_anual_pct: f64) -> Option<f64> {
    if retornos.len() < 2 {
        return None;
    }
    let vol = desvio(retornos) * PREGOES_ANO.sqrt();
    if vol < VOL_MINIMA {
        return None;
    }
    let retorno_anual = media(retornos) * PREGOES_ANO;
    Some((retorno_anual - cdi_anual_pct / 100.0) / vol)
}

/// VaR **histórico** no percentil dado (5 para 95% de confiança), em porcentagem.
///
/// Histórico e não paramétrico de propósito: o paramétrico supõe que os retornos são
/// normais, e eles não são — têm cauda gorda, e é exatamente na cauda que o VaR vive.
/// Supor normalidade num número que existe para medir o extremo é errar onde importa.
pub fn var_historico(retornos: &[f64], percentil: f64) -> Option<f64> {
    if retornos.len() < 20 {
        return None;
    }
    let mut ordenados = retornos.to_vec();
    ordenados.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let posicao = (percentil / 100.0 * ordenados.len() as f64).floor() as usize;
    ordenados
        .get(posicao.min(ordenados.len() - 1))
        .map(|v| v * 100.0)
}

/// Média móvel simples. Devolve `None` nos pontos em que ela ainda não existe, para que o
/// gráfico não desenhe uma linha errada nos primeiros períodos.
pub fn sma(serie: &[f64], periodo: usize) -> Vec<Option<f64>> {
    if periodo == 0 {
        return vec![None; serie.len()];
    }
    serie
        .iter()
        .enumerate()
        .map(|(i, _)| (i + 1 >= periodo).then(|| media(&serie[i + 1 - periodo..=i])))
        .collect()
}

/// Média móvel exponencial, **semeada com a SMA do primeiro período**.
///
/// Semear com o primeiro preço é o erro comum e desloca a curva inteira nos primeiros
/// períodos. A semente com a SMA é o padrão da indústria, e é o que faz esta curva bater
/// com a de qualquer outra plataforma.
pub fn ema(serie: &[f64], periodo: usize) -> Vec<Option<f64>> {
    let mut saida = vec![None; serie.len()];
    if periodo == 0 || serie.len() < periodo {
        return saida;
    }
    let k = 2.0 / (periodo as f64 + 1.0);
    let mut atual = media(&serie[..periodo]);
    saida[periodo - 1] = Some(atual);
    for i in periodo..serie.len() {
        atual = serie[i] * k + atual * (1.0 - k);
        saida[i] = Some(atual);
    }
    saida
}

/// RSI com a **suavização de Wilder**, e não média simples de ganhos e perdas.
///
/// As duas dão números diferentes, e a de Wilder é a que todo mundo mostra. Usar a simples
/// daria um RSI que não bate com nenhuma outra tela do mundo.
pub fn rsi(serie: &[f64], periodo: usize) -> Vec<Option<f64>> {
    let mut saida = vec![None; serie.len()];
    if periodo == 0 || serie.len() <= periodo {
        return saida;
    }
    let (mut ganho, mut perda) = (0.0, 0.0);
    for i in 1..=periodo {
        let d = serie[i] - serie[i - 1];
        if d >= 0.0 {
            ganho += d;
        } else {
            perda -= d;
        }
    }
    let (mut mg, mut mp) = (ganho / periodo as f64, perda / periodo as f64);
    saida[periodo] = Some(rsi_de(mg, mp));
    for i in periodo + 1..serie.len() {
        let d = serie[i] - serie[i - 1];
        let (g, p) = if d >= 0.0 { (d, 0.0) } else { (0.0, -d) };
        mg = (mg * (periodo as f64 - 1.0) + g) / periodo as f64;
        mp = (mp * (periodo as f64 - 1.0) + p) / periodo as f64;
        saida[i] = Some(rsi_de(mg, mp));
    }
    saida
}

fn rsi_de(mg: f64, mp: f64) -> f64 {
    match mp <= 0.0 {
        true => 100.0,
        false => 100.0 - 100.0 / (1.0 + mg / mp),
    }
}

/// MACD: linha, sinal e histograma.
pub fn macd(
    serie: &[f64],
    rapida: usize,
    lenta: usize,
    sinal: usize,
) -> Vec<Option<(f64, f64, f64)>> {
    let r = ema(serie, rapida);
    let l = ema(serie, lenta);
    let linha: Vec<f64> = r
        .iter()
        .zip(&l)
        .map(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => a - b,
            _ => 0.0,
        })
        .collect();
    let primeiro = lenta.saturating_sub(1);
    let s = ema(&linha[primeiro.min(linha.len())..], sinal);
    (0..serie.len())
        .map(|i| {
            if i < primeiro {
                return None;
            }
            let sig = (*s.get(i - primeiro)?)?;
            Some((linha[i], sig, linha[i] - sig))
        })
        .collect()
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

    #[test]
    fn volatilidade_anualiza_pela_raiz_e_nao_pelo_fator() {
        // Uma série com desvio diário de 1% dá ~15,87% ao ano (1 × √252), e não 252%.
        let r: Vec<f64> = (0..100)
            .map(|i| if i % 2 == 0 { 0.01 } else { -0.01 })
            .collect();
        let v = volatilidade_anual(&r);
        assert!(
            v > 10.0 && v < 25.0,
            "deu {v} — o fator de anualização está errado"
        );
    }

    #[test]
    fn drawdown_encontra_a_maior_queda() {
        let serie = vec![100.0, 120.0, 90.0, 110.0, 60.0, 80.0];
        let (pior, inicio, fim) = drawdown_maximo(&serie);
        // Topo em 120 (índice 1), fundo em 60 (índice 4): −50%.
        assert!((pior - -50.0).abs() < 1e-9, "deu {pior}");
        assert_eq!((inicio, fim), (1, 4));
        // Atual: 80 contra topo de 120.
        assert!((drawdown_atual(&serie) - -33.3333).abs() < 0.01);
    }

    #[test]
    fn sma_nao_existe_antes_do_periodo() {
        let s = sma(&[1.0, 2.0, 3.0, 4.0], 3);
        assert_eq!(s[0], None);
        assert_eq!(s[1], None);
        assert_eq!(s[2], Some(2.0));
        assert_eq!(s[3], Some(3.0));
    }

    #[test]
    fn ema_e_semeada_com_a_sma_do_primeiro_periodo() {
        // Semear com o primeiro preço é o erro comum. Com [1,2,3], período 3, a semente
        // tem que ser 2 (a média), e não 1.
        let e = ema(&[1.0, 2.0, 3.0, 4.0], 3);
        assert_eq!(e[2], Some(2.0));
        let esperado = 4.0 * 0.5 + 2.0 * 0.5;
        assert!((e[3].unwrap() - esperado).abs() < 1e-9);
    }

    #[test]
    fn rsi_de_serie_so_de_alta_e_cem() {
        let subindo: Vec<f64> = (1..40).map(|i| i as f64).collect();
        let r = rsi(&subindo, 14);
        assert!((r.last().unwrap().unwrap() - 100.0).abs() < 1e-6);

        // E de uma série só de queda é zero.
        let caindo: Vec<f64> = (1..40).rev().map(|i| i as f64).collect();
        let r = rsi(&caindo, 14);
        assert!(r.last().unwrap().unwrap() < 1e-6);
    }

    #[test]
    fn rsi_fica_entre_zero_e_cem_sempre() {
        let serie: Vec<f64> = (0..200)
            .map(|i| 100.0 + (i as f64 * 0.7).sin() * 20.0 + (i as f64) * 0.1)
            .collect();
        for v in rsi(&serie, 14).into_iter().flatten() {
            assert!((0.0..=100.0).contains(&v), "RSI fora da escala: {v}");
        }
    }

    #[test]
    fn var_precisa_de_amostra() {
        assert_eq!(var_historico(&[0.01, -0.02], 5.0), None);
        let r: Vec<f64> = (0..100).map(|i| (i as f64 - 50.0) / 1000.0).collect();
        let v = var_historico(&r, 5.0).unwrap();
        assert!(v < 0.0, "o VaR de 5% tem que estar na cauda de perda");
    }

    #[test]
    fn sharpe_usa_o_cdi_como_piso() {
        // Retorno constante não tem volatilidade, então não há Sharpe. O desvio dessa
        // série é ~1e-19 e não zero, e sem um piso isto devolveria ~1e16.
        let r = vec![0.001; 60];
        assert_eq!(sharpe(&r, 10.0), None);
        let mut r: Vec<f64> = (0..60)
            .map(|i| if i % 2 == 0 { 0.004 } else { -0.001 })
            .collect();
        r.push(0.002);
        let com_cdi_alto = sharpe(&r, 20.0).unwrap();
        let com_cdi_baixo = sharpe(&r, 1.0).unwrap();
        assert!(
            com_cdi_baixo > com_cdi_alto,
            "CDI maior tem que baixar o Sharpe"
        );
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
