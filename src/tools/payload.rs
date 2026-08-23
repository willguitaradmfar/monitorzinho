//! Writing captured bytes on one line, so a request can live in a text field.
//!
//! A request is bytes: CRLFs, sometimes a body that isn't text at all. The wizard edits
//! single-line strings, and that is worth keeping — being able to *change* the path or a
//! header before repeating a request is most of why anyone repeats one. So the bytes are
//! escaped the way a shell or a C string would: `\r`, `\n`, `\t`, `\\`, and `\xNN` for
//! anything else that can't be shown.
//!
//! Round-tripping is exact, including for a binary body. Text that is valid UTF-8 keeps
//! its accents rather than turning into a wall of `\xc3\xa1` — the field is meant to be
//! read.

/// Bytes as a single editable line.
pub fn encode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.chars().map(escape_char).collect(),
        // Not text: every byte spelled out, since there are no characters to preserve.
        Err(_) => bytes.iter().map(|&b| escape_byte(b)).collect(),
    }
}

fn escape_char(c: char) -> String {
    match c {
        '\\' => "\\\\".to_string(),
        '\r' => "\\r".to_string(),
        '\n' => "\\n".to_string(),
        '\t' => "\\t".to_string(),
        c if (c as u32) < 0x20 || c as u32 == 0x7f => escape_byte(c as u8),
        c => c.to_string(),
    }
}

fn escape_byte(b: u8) -> String {
    match b {
        b'\\' => "\\\\".to_string(),
        b'\r' => "\\r".to_string(),
        b'\n' => "\\n".to_string(),
        b'\t' => "\\t".to_string(),
        0x20..=0x7e => (b as char).to_string(),
        other => format!("\\x{other:02x}"),
    }
}

/// The bytes back. Anything that isn't a recognised escape is taken literally, including
/// a lone backslash — a field somebody typed into by hand shouldn't refuse to be sent
/// because of a stray character.
pub fn decode(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('0') => out.push(0),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let mut hex = String::new();
                while hex.len() < 2 {
                    match chars.peek() {
                        Some(c) if c.is_ascii_hexdigit() => {
                            hex.push(*c);
                            chars.next();
                        }
                        _ => break,
                    }
                }
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) if hex.len() == 2 => out.push(byte),
                    // Not a hex pair: it was a literal "\x", and saying so beats
                    // swallowing characters somebody meant to send.
                    _ => {
                        out.extend_from_slice(b"\\x");
                        out.extend_from_slice(hex.as_bytes());
                    }
                }
            }
            Some(other) => {
                out.push(b'\\');
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

/// The first line of an encoded payload, for a label. For an HTTP request that is the
/// request line, which is exactly what identifies it.
pub fn first_line(encoded: &str, limit: usize) -> String {
    let line = encoded.split("\\r\\n").next().unwrap_or(encoded);
    let line = line.split("\\n").next().unwrap_or(line);
    if line.chars().count() <= limit {
        return line.to_string();
    }
    let kept: String = line.chars().take(limit.saturating_sub(1)).collect();
    format!("{kept}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_survives_the_round_trip_and_stays_readable() {
        let request = b"GET /caf\xc3\xa9 HTTP/1.1\r\nHost: exemplo\r\n\r\n";
        let encoded = encode(request);
        assert_eq!(encoded, "GET /café HTTP/1.1\\r\\nHost: exemplo\\r\\n\\r\\n");
        assert_eq!(decode(&encoded), request);
    }

    #[test]
    fn binary_survives_the_round_trip() {
        let body: Vec<u8> = (0u8..=255).collect();
        assert_eq!(decode(&encode(&body)), body);
    }

    #[test]
    fn a_typed_backslash_is_sent_as_typed() {
        assert_eq!(decode("C:\\pasta"), b"C:\\pasta");
        assert_eq!(decode("\\xzz"), b"\\xzz");
    }

    #[test]
    fn first_line_is_the_request_line() {
        let encoded = encode(b"POST /pedidos HTTP/1.1\r\nHost: x\r\n\r\n{}");
        assert_eq!(first_line(&encoded, 60), "POST /pedidos HTTP/1.1");
    }
}
