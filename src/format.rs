const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

fn scale(mut value: f64, suffix: &str) -> String {
    let mut unit_idx = 0;
    while value.abs() >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }
    format!("{:.1} {}{}", value, UNITS[unit_idx], suffix)
}

/// Formats a byte count with an auto-scaled unit (B/KB/MB/GB/TB), e.g. "482.0 MB".
pub fn human_bytes(bytes: f64) -> String {
    scale(bytes, "")
}

/// Formats a byte rate with an auto-scaled unit, e.g. "1.3 MB/s".
pub fn human_bytes_per_sec(bytes_per_sec: f64) -> String {
    scale(bytes_per_sec, "/s")
}

/// Formats a duration in seconds as a compact human-readable string, e.g. "2h15m".
pub fn human_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let mins = (seconds % 3_600) / 60;
    let secs = seconds % 60;

    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins}m")
    } else if mins > 0 {
        format!("{mins}m{secs}s")
    } else {
        format!("{secs}s")
    }
}

/// Case-insensitive substring search over ASCII, returning a byte offset. Comparing
/// raw bytes keeps the returned index valid for slicing: an ASCII byte can never match
/// a UTF-8 continuation byte, so a match can only ever start and end on a character
/// boundary. What gets searched here is addresses, commands and protocol text rather
/// than prose, so folding by byte is both correct enough and free — a Unicode-aware
/// fold would allocate on every line of a log that redraws several times a second.
pub fn find_ci(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    let (hay, need) = (haystack.as_bytes(), needle.as_bytes());
    if need.is_empty() || hay.len() < need.len() || from > hay.len() - need.len() {
        return None;
    }
    (from..=hay.len() - need.len()).find(|&i| hay[i..i + need.len()].eq_ignore_ascii_case(need))
}

/// Whether `needle` appears anywhere in `haystack`, ignoring case.
pub fn contains_ci(haystack: &str, needle: &str) -> bool {
    find_ci(haystack, needle, 0).is_some()
}

/// Minúsculas e sem acento, para uma busca digitada por gente.
///
/// Separado de `find_ci`, que dobra por byte de propósito: aquele percorre logs que
/// redesenham várias vezes por segundo e não pode alocar. Este roda sobre células de
/// tabela quando alguém digita, onde uma alocação por linha não é medida por ninguém — e
/// onde acertar «sessao» → «sessão» é a diferença entre achar e não achar.
pub fn fold(text: &str) -> String {
    // A caixa cai **antes** da dobra do acento. Na ordem inversa, «Ã» não casaria com
    // nenhum braço (a tabela é minúscula), passaria intacto, e só então viraria «ã» — o
    // que deixaria «SESSÃO» sem dobrar enquanto «sessão» dobrava.
    text.chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            'ñ' => 'n',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod fold_tests {
    use super::fold;

    #[test]
    fn acento_e_caixa_somem() {
        assert_eq!(fold("Câmbio"), "cambio");
        assert_eq!(
            fold("SESSÃO"),
            "sessao",
            "maiúscula acentuada tem que dobrar igual"
        );
        assert_eq!(fold("Ação"), "acao");
        assert_eq!(fold("Imposto de renda"), "imposto de renda");
    }

    #[test]
    fn o_que_casava_antes_continua_casando() {
        // A dobra é estritamente mais permissiva: nada que casava deixa de casar.
        for (texto, busca) in [("PETR4", "petr"), ("docker-proxy", "proxy")] {
            assert!(fold(texto).contains(&fold(busca)));
        }
        // E agora também casa sem acento.
        assert!(fold("Câmbio").contains(&fold("camb")));
        assert!(fold("Posições").contains(&fold("posicoes")));
    }
}
