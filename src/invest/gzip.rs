//! Descompressão de gzip — DEFLATE, RFC 1951 e RFC 1952.
//!
//! Existe porque o G1 responde comprimido mesmo com `Accept-Encoding: identity`, e um
//! feed que não se lê é um feed que não existe. Sem isso, metade das notícias da carteira
//! ficava de fora e a ordenação por data que o usuário pediu ordenava uma fonte só.
//!
//! É pequeno de propósito: só o que um corpo HTTP precisa. Não escreve arquivo, não lê
//! múltiplos membros concatenados, não confere o CRC — o TLS já garante que os bytes
//! chegaram como saíram, e um CRC aqui só repetiria essa garantia mais devagar.
//!
//! A alternativa era uma dependência. Duzentas linhas testadas contra o `gzip` do sistema
//! custam menos que uma árvore de crates para ler um RSS.

/// O teto de saída. Um feed RSS tem dezenas de kilobytes; um megabyte já é um exagero
/// confortável, e o limite existe para que um corpo malicioso não vire um travamento —
/// DEFLATE comprime um gigabyte de zeros em um megabyte.
const MAX_SAIDA: usize = 8 << 20;

#[derive(Debug, PartialEq)]
pub enum Erro {
    /// Não começa com a assinatura do gzip.
    NaoEGzip,
    /// Acabaram os bytes no meio de um bloco.
    Truncado,
    /// Os bytes não descrevem um fluxo DEFLATE válido.
    Corrompido(&'static str),
    /// A saída passou de `MAX_SAIDA`.
    GrandeDemais,
}

impl std::fmt::Display for Erro {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Erro::NaoEGzip => write!(f, "não é gzip"),
            Erro::Truncado => write!(f, "a resposta comprimida acabou no meio"),
            Erro::Corrompido(o) => write!(f, "a resposta comprimida está corrompida: {o}"),
            Erro::GrandeDemais => write!(f, "a resposta comprimida é grande demais"),
        }
    }
}

/// Se estes bytes parecem um membro gzip. Dois bytes de assinatura e o método de
/// compressão — é o que o RFC 1952 dá para reconhecer, e basta.
pub fn parece_gzip(dados: &[u8]) -> bool {
    dados.len() > 3 && dados[0] == 0x1f && dados[1] == 0x8b && dados[2] == 8
}

/// Descomprime um membro gzip inteiro.
pub fn inflar(dados: &[u8]) -> Result<Vec<u8>, Erro> {
    if !parece_gzip(dados) {
        return Err(Erro::NaoEGzip);
    }
    let flg = dados[3];
    let mut i = 10;
    // FEXTRA: dois bytes de tamanho e o campo.
    if flg & 0b0000_0100 != 0 {
        let n = *pegar(dados, i)? as usize | (*pegar(dados, i + 1)? as usize) << 8;
        i += 2 + n;
    }
    // FNAME e FCOMMENT: cadeias terminadas em zero.
    for bit in [0b0000_1000, 0b0001_0000] {
        if flg & bit != 0 {
            while *pegar(dados, i)? != 0 {
                i += 1;
            }
            i += 1;
        }
    }
    // FHCRC: dois bytes que não conferimos, pelo motivo no cabeçalho do módulo.
    if flg & 0b0000_0010 != 0 {
        i += 2;
    }
    if i > dados.len() {
        return Err(Erro::Truncado);
    }
    inflar_deflate(&dados[i..])
}

fn pegar(dados: &[u8], i: usize) -> Result<&u8, Erro> {
    dados.get(i).ok_or(Erro::Truncado)
}

/// Um fluxo de bits lido do menos significativo para o mais, que é a ordem do DEFLATE.
struct Bits<'a> {
    dados: &'a [u8],
    /// Em bits desde o começo, e não em bytes: um bloco DEFLATE não respeita fronteira de
    /// byte, e guardar a posição em bits é o que dispensa fazer essa conta em cada leitura.
    pos: usize,
}

impl<'a> Bits<'a> {
    fn novo(dados: &'a [u8]) -> Bits<'a> {
        Bits { dados, pos: 0 }
    }

    fn bit(&mut self) -> Result<u32, Erro> {
        let byte = *self.dados.get(self.pos / 8).ok_or(Erro::Truncado)?;
        let b = (byte >> (self.pos % 8)) & 1;
        self.pos += 1;
        Ok(b as u32)
    }

    fn ler(&mut self, quantos: u32) -> Result<u32, Erro> {
        let mut valor = 0;
        for i in 0..quantos {
            valor |= self.bit()? << i;
        }
        Ok(valor)
    }

    /// Descarta o resto do byte atual. O bloco não-comprimido começa alinhado.
    fn alinhar(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
}

/// Uma tabela de Huffman canônica, guardada como o `puff` da zlib guarda: quantos códigos
/// existem de cada comprimento, e os símbolos em ordem canônica.
///
/// Não é a forma mais rápida — a rápida é uma tabela de consulta direta. É a forma que
/// cabe em trinta linhas e que dá para conferir lendo, e o volume aqui é um RSS.
struct Huffman {
    quantos: [u16; 16],
    simbolos: Vec<u16>,
}

impl Huffman {
    fn nova(comprimentos: &[u8]) -> Result<Huffman, Erro> {
        let mut quantos = [0u16; 16];
        for &c in comprimentos {
            quantos[c as usize] += 1;
        }
        quantos[0] = 0;
        // Um código de Huffman completo esgota exatamente o espaço. Sobrar é aceitável
        // (o RFC permite tabelas incompletas); faltar significa bytes inválidos.
        let mut sobra = 1i32;
        for &quantos in quantos.iter().skip(1) {
            sobra = (sobra << 1) - quantos as i32;
            if sobra < 0 {
                return Err(Erro::Corrompido("código de Huffman inválido"));
            }
        }
        let mut deslocamento = [0u16; 16];
        for c in 1..15 {
            deslocamento[c + 1] = deslocamento[c] + quantos[c];
        }
        let mut simbolos = vec![0u16; comprimentos.len()];
        for (simbolo, &c) in comprimentos.iter().enumerate() {
            if c != 0 {
                simbolos[deslocamento[c as usize] as usize] = simbolo as u16;
                deslocamento[c as usize] += 1;
            }
        }
        Ok(Huffman { quantos, simbolos })
    }

    fn decodificar(&self, bits: &mut Bits) -> Result<u16, Erro> {
        let (mut codigo, mut primeiro, mut indice) = (0i32, 0i32, 0i32);
        for c in 1..16 {
            codigo |= bits.bit()? as i32;
            let quantos = self.quantos[c] as i32;
            if codigo - quantos < primeiro {
                let em = (indice + codigo - primeiro) as usize;
                return self
                    .simbolos
                    .get(em)
                    .copied()
                    .ok_or(Erro::Corrompido("símbolo fora da tabela"));
            }
            indice += quantos;
            primeiro = (primeiro + quantos) << 1;
            codigo <<= 1;
        }
        Err(Erro::Corrompido("código sem símbolo"))
    }
}

/// RFC 1951 §3.2.5: o comprimento da cópia, por símbolo a partir do 257.
const COMP_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const COMP_EXTRA: [u32; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u32; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// A ordem em que os comprimentos do alfabeto de comprimentos aparecem. RFC 1951 §3.2.7.
const ORDEM: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

fn inflar_deflate(dados: &[u8]) -> Result<Vec<u8>, Erro> {
    let mut bits = Bits::novo(dados);
    let mut saida: Vec<u8> = Vec::new();
    loop {
        let ultimo = bits.bit()? == 1;
        match bits.ler(2)? {
            0 => bloco_cru(&mut bits, &mut saida)?,
            1 => {
                let (lit, dist) = tabelas_fixas()?;
                bloco_comprimido(&mut bits, &mut saida, &lit, &dist)?;
            }
            2 => {
                let (lit, dist) = tabelas_dinamicas(&mut bits)?;
                bloco_comprimido(&mut bits, &mut saida, &lit, &dist)?;
            }
            _ => return Err(Erro::Corrompido("tipo de bloco reservado")),
        }
        if ultimo {
            return Ok(saida);
        }
    }
}

fn bloco_cru(bits: &mut Bits, saida: &mut Vec<u8>) -> Result<(), Erro> {
    bits.alinhar();
    let inicio = bits.pos / 8;
    let comprimento =
        *pegar(bits.dados, inicio)? as usize | (*pegar(bits.dados, inicio + 1)? as usize) << 8;
    // Os dois bytes seguintes são o complemento de um do comprimento. Conferir é a única
    // defesa que um bloco cru tem contra bytes trocados, e custa uma comparação.
    let complemento =
        *pegar(bits.dados, inicio + 2)? as usize | (*pegar(bits.dados, inicio + 3)? as usize) << 8;
    if comprimento != !complemento & 0xffff {
        return Err(Erro::Corrompido(
            "bloco não-comprimido com tamanho incoerente",
        ));
    }
    let corpo = inicio + 4;
    let fim = corpo + comprimento;
    if fim > bits.dados.len() {
        return Err(Erro::Truncado);
    }
    if saida.len() + comprimento > MAX_SAIDA {
        return Err(Erro::GrandeDemais);
    }
    saida.extend_from_slice(&bits.dados[corpo..fim]);
    bits.pos = fim * 8;
    Ok(())
}

fn tabelas_fixas() -> Result<(Huffman, Huffman), Erro> {
    let mut lit = [0u8; 288];
    for (simbolo, c) in lit.iter_mut().enumerate() {
        *c = match simbolo {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((Huffman::nova(&lit)?, Huffman::nova(&[5u8; 30])?))
}

fn tabelas_dinamicas(bits: &mut Bits) -> Result<(Huffman, Huffman), Erro> {
    let n_lit = bits.ler(5)? as usize + 257;
    let n_dist = bits.ler(5)? as usize + 1;
    let n_codigo = bits.ler(4)? as usize + 4;
    if n_lit > 288 || n_dist > 30 {
        return Err(Erro::Corrompido("alfabeto grande demais"));
    }
    let mut comprimentos = [0u8; 19];
    for &pos in ORDEM.iter().take(n_codigo) {
        comprimentos[pos] = bits.ler(3)? as u8;
    }
    let tabela = Huffman::nova(&comprimentos)?;

    // Os comprimentos dos dois alfabetos vêm num fluxo só, comprimidos por sua vez.
    let mut todos = vec![0u8; n_lit + n_dist];
    let mut i = 0;
    while i < todos.len() {
        let simbolo = tabela.decodificar(bits)?;
        let (valor, repete) = match simbolo {
            0..=15 => (simbolo as u8, 1),
            // 16 repete o **anterior**; sem anterior não há o que repetir.
            16 => {
                let anterior = *todos
                    .get(i.wrapping_sub(1))
                    .ok_or(Erro::Corrompido("repetição sem nada antes"))?;
                (anterior, 3 + bits.ler(2)? as usize)
            }
            17 => (0, 3 + bits.ler(3)? as usize),
            18 => (0, 11 + bits.ler(7)? as usize),
            _ => return Err(Erro::Corrompido("símbolo de comprimento inválido")),
        };
        if i + repete > todos.len() {
            return Err(Erro::Corrompido("repetição passa do alfabeto"));
        }
        todos[i..i + repete].fill(valor);
        i += repete;
    }
    Ok((
        Huffman::nova(&todos[..n_lit])?,
        Huffman::nova(&todos[n_lit..])?,
    ))
}

fn bloco_comprimido(
    bits: &mut Bits,
    saida: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), Erro> {
    loop {
        let simbolo = lit.decodificar(bits)?;
        match simbolo {
            256 => return Ok(()),
            0..=255 => {
                if saida.len() >= MAX_SAIDA {
                    return Err(Erro::GrandeDemais);
                }
                saida.push(simbolo as u8);
            }
            257..=285 => {
                let i = simbolo as usize - 257;
                let comprimento = COMP_BASE[i] as usize + bits.ler(COMP_EXTRA[i])? as usize;
                let d = dist.decodificar(bits)? as usize;
                if d >= DIST_BASE.len() {
                    return Err(Erro::Corrompido("distância fora da tabela"));
                }
                let distancia = DIST_BASE[d] as usize + bits.ler(DIST_EXTRA[d])? as usize;
                if distancia > saida.len() {
                    return Err(Erro::Corrompido("cópia de antes do começo"));
                }
                if saida.len() + comprimento > MAX_SAIDA {
                    return Err(Erro::GrandeDemais);
                }
                // Byte a byte, e não `extend_from_slice`: a cópia pode se sobrepor a si
                // mesma — é assim que o DEFLATE escreve uma sequência repetida — e copiar
                // em bloco leria bytes que ainda não foram escritos.
                let inicio = saida.len() - distancia;
                for k in 0..comprimento {
                    saida.push(saida[inicio + k]);
                }
            }
            _ => return Err(Erro::Corrompido("símbolo literal inválido")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Comprime com o `gzip` do sistema. É a única referência que vale: um teste que
    /// comprimisse com o nosso próprio código só provaria que ele concorda consigo mesmo.
    fn comprimir(dados: &[u8], nivel: &str) -> Vec<u8> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut p = Command::new("gzip")
            .arg(nivel)
            .arg("-c")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("gzip precisa existir para este teste");
        p.stdin.take().unwrap().write_all(dados).unwrap();
        let saida = p.wait_with_output().unwrap();
        assert!(saida.status.success());
        saida.stdout
    }

    fn ida_e_volta(dados: &[u8]) {
        for nivel in ["-1", "-6", "-9"] {
            let comprimido = comprimir(dados, nivel);
            let voltou = inflar(&comprimido)
                .unwrap_or_else(|e| panic!("{nivel} com {} bytes: {e}", dados.len()));
            assert_eq!(voltou, dados, "{nivel} com {} bytes", dados.len());
        }
    }

    #[test]
    fn bate_com_o_gzip_do_sistema_em_texto_de_verdade() {
        // Um pedaço de RSS, que é para o que isto existe: muita repetição de tags, que é
        // onde o DEFLATE usa as cópias com distância.
        let mut xml = String::from("<?xml version=\"1.0\"?><rss><channel>");
        for i in 0..400 {
            xml.push_str(&format!(
                "<item><title>Notícia número {i} sobre a bolsa</title>\
                 <link>https://exemplo.com.br/noticia/{i}</link>\
                 <pubDate>Tue, 08 Sep 2026 21:4{} +0000</pubDate></item>",
                i % 10
            ));
        }
        xml.push_str("</channel></rss>");
        ida_e_volta(xml.as_bytes());
    }

    #[test]
    fn bate_em_bytes_sem_padrao_nenhum() {
        // Dados incompressíveis viram blocos crus, que é o caminho que o texto nunca toma.
        let ruido: Vec<u8> = (0..70_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        ida_e_volta(&ruido);
    }

    #[test]
    fn bate_nos_tamanhos_de_beirada() {
        for n in [0usize, 1, 2, 3, 255, 256, 257, 32_767, 32_768, 32_769] {
            let dados: Vec<u8> = (0..n).map(|i| b"abcdefgh"[i % 8]).collect();
            ida_e_volta(&dados);
        }
    }

    #[test]
    fn uma_sequencia_que_se_copia_de_si_mesma_sai_inteira() {
        // O caso em que `extend_from_slice` estaria errado: distância 1 e comprimento
        // grande é o DEFLATE dizendo «repita este byte 258 vezes».
        ida_e_volta(&vec![b'z'; 100_000]);
    }

    #[test]
    fn o_que_nao_e_gzip_e_recusado_em_vez_de_adivinhado() {
        assert_eq!(inflar(b"<?xml version=\"1.0\"?>"), Err(Erro::NaoEGzip));
        assert_eq!(inflar(b""), Err(Erro::NaoEGzip));
        assert!(!parece_gzip(b"<rss>"));
        assert!(parece_gzip(&comprimir(b"oi", "-6")));
    }

    #[test]
    fn um_corpo_cortado_no_meio_diz_que_foi_cortado_em_vez_de_travar() {
        let inteiro = comprimir(&vec![b'a'; 50_000], "-9");
        for corte in [11, inteiro.len() / 3, inteiro.len() - 9] {
            // O rabo do gzip são oito bytes de CRC e tamanho que não lemos, então cortar
            // só eles ainda dá um fluxo válido — o que importa é nunca entrar em laço.
            let _ = inflar(&inteiro[..corte]);
        }
        assert!(matches!(
            inflar(&inteiro[..15]),
            Err(Erro::Truncado) | Err(Erro::Corrompido(_))
        ));
    }

    #[test]
    fn bytes_embaralhados_nao_travam_nem_estouram() {
        // Nenhum destes precisa descomprimir. Todos precisam **terminar**, e sem pânico:
        // o corpo vem da rede, e um feed hostil não pode derrubar o programa.
        let base = comprimir(&vec![b'q'; 20_000], "-6");
        for semente in 0..300u32 {
            let mut ruim = base.clone();
            let em = 10 + (semente as usize * 7) % (ruim.len() - 10);
            ruim[em] ^= (semente % 251 + 1) as u8;
            let _ = inflar(&ruim);
        }
    }
}
