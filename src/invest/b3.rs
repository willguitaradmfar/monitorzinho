//! O JSON de posição consolidada da B3, achatado para a mesma grade de um CSV.
//!
//! É o extrato que a área do investidor da B3 exporta: todas as corretoras, todos os
//! produtos, numa estrutura aninhada por categoria. Ele não vira um caminho de importação
//! próprio — vira **linhas e colunas**, e daí em diante segue o mesmo fluxo de conferência
//! do CSV. Um segundo pipeline seria um segundo lugar para a conferência não acontecer.
//!
//! Duas coisas que este arquivo obriga e que o CSV de corretora não obrigava:
//!
//! * **Ele traz várias corretoras.** É o extrato da B3, não o de uma casa. Por isso existe
//!   `Campo::Fonte`: a corretora sai de uma coluna, e a importação grava uma substituição
//!   por corretora — importar este arquivo não pode apagar uma fonte que ele não menciona.
//! * **Ele não traz preço médio.** Traz o valor de mercado. A única exceção é o Tesouro
//!   Direto, que informa `valorAplicado` — o custo, dito pela fonte. Dividi-lo pela
//!   quantidade não é reconstruir preço médio de histórico nenhum: é a mesma informação
//!   por unidade. Onde a fonte não diz o custo, a coluna sai **vazia**, e a posição fica
//!   sem preço médio em vez de ganhar um inventado.

use serde_json::Value;

/// Os nomes das colunas. Ficam aqui e não espalhados porque o palpite de mapeamento do
/// `csv.rs` casa por nome — mudar um destes é mudar o mapeamento.
pub const COLUNAS: [&str; 8] = [
    "corretora",
    "conta",
    "ativo",
    "quantidade",
    "preço médio",
    "preço atual",
    "classe",
    "moeda",
];

/// Se estes bytes são o JSON de posição da B3.
pub fn parece(bytes: &[u8]) -> bool {
    let inicio: String = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]).to_string();
    inicio.trim_start().starts_with('{')
        && inicio.contains("\"itens\"")
        && (inicio.contains("\"categoriaProduto\"") || inicio.contains("\"posicoes\""))
}

/// Achata o JSON em cabeçalho e linhas. `None` quando não é este formato.
pub fn ler(bytes: &[u8]) -> Option<(Vec<String>, Vec<Vec<String>>)> {
    let raiz: Value = serde_json::from_slice(bytes).ok()?;
    let grupos = raiz.get("itens")?.as_array()?;
    let mut linhas = Vec::new();
    for grupo in grupos {
        let tipo = grupo
            .get("tipoProduto")
            .and_then(Value::as_str)
            .unwrap_or("");
        for p in grupo
            .get("posicoes")
            .and_then(Value::as_array)
            .map(|v| v.as_slice())
            .unwrap_or_default()
        {
            if let Some(linha) = linha_de(p, tipo) {
                linhas.push(linha);
            }
        }
    }
    match linhas.is_empty() {
        true => None,
        false => Some((COLUNAS.iter().map(|c| c.to_string()).collect(), linhas)),
    }
}

fn linha_de(p: &Value, tipo: &str) -> Option<Vec<String>> {
    let quantidade = numero(p, "quantidade")?;
    if quantidade == 0.0 {
        return None;
    }
    let codigo = texto(p, "codigoNegociacao")
        .or_else(|| texto(p, "produto"))
        .unwrap_or_default();
    if codigo.is_empty() {
        return None;
    }

    // O custo, **só quando a fonte o diz**. Renda variável neste extrato não traz nenhum,
    // e é por isso que a coluna sai vazia: uma posição sem preço médio é honesta, uma com
    // preço médio inventado contamina P&L, imposto e metas de uma vez.
    let preco_medio = numero(p, "valorAplicado").map(|custo| custo / quantidade);
    // O valor de mercado por unidade — mas **só para o que não tem fonte de cotação**.
    //
    // O extrato traz o fechamento de alguns dias atrás. Num papel da B3 isso não acrescenta
    // nada (a Kinvo e a brapi dão o preço de hoje) e atrapalha: gravado como preço
    // informado, ele carrega o carimbo da **importação**, não o da data de referência —
    // então um preço de quatro dias atrás se apresentava como sendo deste minuto e ganhava
    // do preço ao vivo. O sintoma era a cotação aparecer e virar «manual».
    //
    // No CDB e no Tesouro é o contrário: ninguém os cota, e sem este número eles ficariam
    // sem valor de mercado nenhum.
    let preco_atual = match tem_fonte(tipo) {
        true => None,
        false => numero(p, "precoUnitarioAtualizado")
            .or_else(|| numero(p, "valorAtualizado").map(|v| v / quantidade)),
    };

    Some(vec![
        texto(p, "instituicao").unwrap_or_else(|| "b3".into()),
        texto(p, "codigoConta").unwrap_or_default(),
        ativo(&codigo, tipo),
        virgula(quantidade),
        preco_medio.map(virgula).unwrap_or_default(),
        preco_atual.map(virgula).unwrap_or_default(),
        classe(tipo).into(),
        "BRL".into(),
    ])
}

/// Se este tipo de produto tem fonte de cotação neste programa.
///
/// É a mesma divisão de `ativo`: o que vira `B3/` é cotado, o que vira `OUTRO/` não.
fn tem_fonte(tipo: &str) -> bool {
    !ativo("x", tipo).starts_with("OUTRO/")
}

/// O ativo com o mercado na frente.
///
/// Ação, BDR e ETF são papéis da B3 e ganham `B3/`, que é o que faz uma fonte de cotação
/// reconhecê-los. CDB e Tesouro **não são**: não há ticker nem fonte que os cote, e
/// chamá-los de B3 faria a aba passar a vida pedindo um preço que ninguém tem.
fn ativo(codigo: &str, tipo: &str) -> String {
    let simbolo = codigo.trim().replace(' ', "-");
    match tipo {
        "CDB" | "TesouroDireto" | "LCI" | "LCA" | "CRI" | "CRA" | "Debenture" => {
            format!("OUTRO/{simbolo}")
        }
        _ => format!("B3/{simbolo}"),
    }
}

/// A classe, pelo tipo de produto do extrato.
///
/// BDR vira `ação` porque é o que ele é — um recibo de ação. O programa não tem uma classe
/// «internacional», e inventar uma aqui, escondida num importador, seria decidir por fora
/// uma coisa que é do módulo de Alocação.
fn classe(tipo: &str) -> &'static str {
    match tipo {
        "Acao" | "BDR" => "acao",
        "FundoImobiliario" | "FII" => "fii",
        t if t.starts_with("ETF") => "etf",
        "CDB" | "TesouroDireto" | "LCI" | "LCA" | "CRI" | "CRA" | "Debenture" => "renda_fixa",
        _ => "outro",
    }
}

fn texto(p: &Value, campo: &str) -> Option<String> {
    let t = p.get(campo)?.as_str()?.trim().to_string();
    (!t.is_empty()).then_some(t)
}

fn numero(p: &Value, campo: &str) -> Option<f64> {
    p.get(campo)?.as_f64()
}

/// Um número como o resto do programa o escreve: vírgula decimal, sem separador de
/// milhar. Passar por texto parece um rodeio, mas é o que faz este arquivo entrar pela
/// **mesma** porta do CSV — e a conferência que existe naquela porta é o ponto todo.
fn virgula(v: f64) -> String {
    format!("{v:.8}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .replace('.', ",")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Um extrato de mentira, com a forma do de verdade.
    ///
    /// Número de conta e valores são inventados de propósito: um extrato de posição é
    /// documento pessoal, e uma amostra de teste não precisa de nenhum dado real para
    /// exercitar o formato.
    const AMOSTRA: &str = r#"{"itens":[
      {"tipoProduto":"Acao","posicoes":[
        {"instituicao":"CORRETORA A","codigoConta":"000000","quantidade":400,
         "valorAtualizado":22112,"precoFechamento":55.28,"codigoNegociacao":"AXIA3"}]},
      {"tipoProduto":"TesouroDireto","posicoes":[
        {"instituicao":"CORRETORA B","quantidade":1000.0,
         "valorAtualizado":565000.0,"valorAplicado":476780.0,
         "codigoNegociacao":"Tesouro Prefixado 2031"}]}]}"#;

    #[test]
    fn reconhece_o_extrato_e_recusa_o_resto() {
        assert!(parece(AMOSTRA.as_bytes()));
        assert!(!parece(b"ativo;quantidade\nPETR4;100"));
        assert!(!parece(b"{\"outra\":\"coisa\"}"));
    }

    #[test]
    fn renda_variavel_entra_sem_preco_medio() {
        // A regra que este importador existe para respeitar: o extrato da B3 **não traz**
        // preço médio de ação, e inventá-lo a partir do preço de fechamento daria um P&L
        // de zero com cara de fato.
        let (cab, linhas) = ler(AMOSTRA.as_bytes()).unwrap();
        let pm = cab.iter().position(|c| c == "preço médio").unwrap();
        let atual = cab.iter().position(|c| c == "preço atual").unwrap();
        let acao = &linhas[0];
        assert_eq!(acao[pm], "", "ação tem que entrar sem preço médio");
        // E sem preço atual também: quem cota a ação é a fonte de cotação. O fechamento
        // do extrato é de dias atrás, e gravado como informado ele ganhava do preço de
        // hoje — a cotação aparecia na tela e virava «manual».
        assert_eq!(acao[atual], "", "papel cotado não recebe preço informado");
    }

    #[test]
    fn o_que_ninguem_cota_recebe_o_valor_do_extrato() {
        // O outro lado: sem este número, o Tesouro e o CDB ficariam sem valor de mercado.
        let (cab, linhas) = ler(AMOSTRA.as_bytes()).unwrap();
        let atual = cab.iter().position(|c| c == "preço atual").unwrap();
        let tesouro: f64 = linhas[1][atual].replace(',', ".").parse().unwrap();
        assert!((tesouro - 565000.0 / 1000.0).abs() < 1e-6);
    }

    #[test]
    fn o_tesouro_traz_o_custo_e_ele_vira_preco_medio_por_unidade() {
        // Aqui a fonte **diz** o custo: `valorAplicado`. Dividir pela quantidade é a mesma
        // informação por unidade, e não uma reconstrução a partir de histórico.
        let (cab, linhas) = ler(AMOSTRA.as_bytes()).unwrap();
        let pm = cab.iter().position(|c| c == "preço médio").unwrap();
        let pm: f64 = linhas[1][pm].replace(',', ".").parse().unwrap();
        assert!((pm - 476780.0 / 1000.0).abs() < 1e-6);
    }

    #[test]
    fn cada_linha_traz_a_corretora_dela() {
        // O extrato é da B3 e não de uma casa: sem esta coluna, importar juntaria duas
        // corretoras numa fonte só e o módulo de Corretoras deixaria de responder
        // «quanto está onde».
        let (cab, linhas) = ler(AMOSTRA.as_bytes()).unwrap();
        let f = cab.iter().position(|c| c == "corretora").unwrap();
        assert_eq!(linhas[0][f], "CORRETORA A");
        assert_eq!(linhas[1][f], "CORRETORA B");
    }

    #[test]
    fn renda_fixa_nao_vira_papel_da_b3() {
        // Um Tesouro com `B3/` na frente faria a aba pedir cotação dele para sempre.
        let (cab, linhas) = ler(AMOSTRA.as_bytes()).unwrap();
        let a = cab.iter().position(|c| c == "ativo").unwrap();
        assert_eq!(linhas[0][a], "B3/AXIA3");
        assert_eq!(linhas[1][a], "OUTRO/Tesouro-Prefixado-2031");
    }

    #[test]
    fn posicao_zerada_nao_entra() {
        let vazio = r#"{"itens":[{"tipoProduto":"Acao","posicoes":[
            {"instituicao":"X","quantidade":0,"codigoNegociacao":"PETR4"}]}]}"#;
        assert!(ler(vazio.as_bytes()).is_none());
    }

    #[test]
    fn numeros_saem_na_forma_que_o_resto_do_programa_le() {
        assert_eq!(virgula(55.28), "55,28");
        assert_eq!(virgula(400.0), "400");
        assert_eq!(virgula(0.0100155), "0,0100155");
    }
}
