//! Base64, MD5 and SCRAM: the arithmetic standing between a connection string and a
//! database willing to talk.
//!
//! Both engines here authenticate the same way — a challenge-response handshake defined
//! by RFC 5802 — and neither the Postgres nor the MongoDB side of this tool can say a
//! word until it is done. There is no driver in this tree to borrow it from, so it lives
//! here once and is used by both: Postgres does SCRAM-SHA-256, MongoDB does whichever of
//! SHA-1 and SHA-256 the server admits to, and the only difference between the two is
//! which hash goes into the same sequence of steps.

use std::num::NonZeroU32;

use ring::rand::SecureRandom;

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let packed = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(packed >> (18 - i * 6)) as usize & 0x3f] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn b64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        let value = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// MD5, RFC 1321, hex-encoded. Here for exactly one reason: a Postgres role created
/// before v14 still authenticates with `md5`, and a server full of them is precisely the
/// kind of server this tool is pointed at. Nothing new is being protected with it.
pub fn md5_hex(data: &[u8]) -> String {
    let digest = md5(data);
    let mut out = String::with_capacity(32);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn md5(data: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    // K[i] = floor(2^32 × |sin(i + 1)|), the table straight out of the RFC.
    let k: Vec<u32> = (0..64)
        .map(|i| ((f64::from(i + 1).sin().abs()) * 4_294_967_296.0) as u32)
        .collect();

    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in message.chunks(64) {
        let words: Vec<u32> = block
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let temp = d;
            d = c;
            c = b;
            let sum = a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(words[g]);
            b = b.wrapping_add(sum.rotate_left(S[i]));
            a = temp;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }

    let mut out = [0u8; 16];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    out
}

/// Which hash a SCRAM conversation is built on. The mechanism name is the one place the
/// wire spells it out, and it's what the server offered in its mechanism list.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    Sha1,
    Sha256,
}

impl Hash {
    pub fn mechanism(self) -> &'static str {
        match self {
            Hash::Sha1 => "SCRAM-SHA-1",
            Hash::Sha256 => "SCRAM-SHA-256",
        }
    }

    fn len(self) -> usize {
        match self {
            Hash::Sha1 => 20,
            Hash::Sha256 => 32,
        }
    }

    fn digest(self, data: &[u8]) -> Vec<u8> {
        let algorithm = match self {
            // SHA-1 is the hash MongoDB's original SCRAM mechanism specifies. The name
            // ring gives it is a warning, not a mistake.
            Hash::Sha1 => &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
            Hash::Sha256 => &ring::digest::SHA256,
        };
        ring::digest::digest(algorithm, data).as_ref().to_vec()
    }

    fn hmac(self, key: &[u8], data: &[u8]) -> Vec<u8> {
        let algorithm = match self {
            Hash::Sha1 => ring::hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
            Hash::Sha256 => ring::hmac::HMAC_SHA256,
        };
        ring::hmac::sign(&ring::hmac::Key::new(algorithm, key), data)
            .as_ref()
            .to_vec()
    }

    fn pbkdf2(self, password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
        let algorithm = match self {
            Hash::Sha1 => ring::pbkdf2::PBKDF2_HMAC_SHA1,
            Hash::Sha256 => ring::pbkdf2::PBKDF2_HMAC_SHA256,
        };
        let mut out = vec![0u8; self.len()];
        let rounds = NonZeroU32::new(iterations.max(1)).unwrap_or(NonZeroU32::MIN);
        ring::pbkdf2::derive(algorithm, rounds, salt, password, &mut out);
        out
    }
}

/// One SCRAM conversation, from the client's side.
///
/// Three messages, in this order: `first` goes out, the server's reply goes into
/// `respond`, and the server's last word goes into `verify`. The verification at the end
/// is not a formality — it is the half of the handshake that proves the *server* knew
/// the password, which is what stops a machine in the middle from collecting one.
pub struct Scram {
    hash: Hash,
    password: String,
    nonce: String,
    first_bare: String,
    salted: Vec<u8>,
    auth_message: String,
}

impl Scram {
    /// Starts a conversation, returning the client's first message.
    ///
    /// `password` is what goes into the key derivation, which is not always what the
    /// user typed: MongoDB's SHA-1 mechanism hashes it with the username first. That
    /// transformation belongs to the engine, so it happens before this is called.
    pub fn start(hash: Hash, user: &str, password: &str) -> (Self, String) {
        let nonce = nonce();
        // `,` and `=` are the separators of every SCRAM message, so a username carrying
        // one has to be spelled out rather than sent raw.
        let escaped = user.replace('=', "=3D").replace(',', "=2C");
        let first_bare = format!("n={escaped},r={nonce}");
        let first = format!("n,,{first_bare}");
        (
            Self {
                hash,
                password: password.to_string(),
                nonce,
                first_bare,
                salted: Vec::new(),
                auth_message: String::new(),
            },
            first,
        )
    }

    /// The server's first message in, the client's final message out — the one carrying
    /// the proof.
    pub fn respond(&mut self, server_first: &str) -> Result<String, String> {
        let server_nonce = field(server_first, 'r').ok_or("SCRAM: resposta sem nonce")?;
        let salt = b64_decode(field(server_first, 's').ok_or("SCRAM: resposta sem sal")?)
            .ok_or("SCRAM: sal ilegível")?;
        let iterations: u32 = field(server_first, 'i')
            .ok_or("SCRAM: resposta sem contagem de iterações")?
            .parse()
            .map_err(|_| "SCRAM: contagem de iterações ilegível")?;
        // The server's nonce must extend ours. Anything else means the reply belongs to
        // a different conversation than the one we started.
        if !server_nonce.starts_with(&self.nonce) {
            return Err("SCRAM: o servidor devolveu outro nonce".to_string());
        }

        self.salted = self
            .hash
            .pbkdf2(self.password.as_bytes(), &salt, iterations);
        let client_key = self.hash.hmac(&self.salted, b"Client Key");
        let stored_key = self.hash.digest(&client_key);
        // `biws` is base64 of the `n,,` header sent in the first message: no channel
        // binding, said the same way twice so the server can tell nobody rewrote it.
        let without_proof = format!("c=biws,r={server_nonce}");
        self.auth_message = format!("{},{server_first},{without_proof}", self.first_bare);
        let signature = self.hash.hmac(&stored_key, self.auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(signature.iter())
            .map(|(key, sig)| key ^ sig)
            .collect();
        Ok(format!("{without_proof},p={}", b64_encode(&proof)))
    }

    /// Checks the server's closing message. Its `v=` is a signature only something
    /// holding the stored credential could have produced.
    pub fn verify(&self, server_final: &str) -> Result<(), String> {
        if let Some(error) = field(server_final, 'e') {
            return Err(format!("SCRAM recusado pelo servidor: {error}"));
        }
        let expected = self.hash.hmac(
            &self.hash.hmac(&self.salted, b"Server Key"),
            self.auth_message.as_bytes(),
        );
        match field(server_final, 'v') {
            Some(signature) if signature == b64_encode(&expected) => Ok(()),
            _ => Err("SCRAM: o servidor não provou conhecer a senha".to_string()),
        }
    }
}

/// One `k=value` out of a comma-separated SCRAM message.
fn field(message: &str, key: char) -> Option<&str> {
    message
        .split(',')
        .find_map(|part| part.strip_prefix(key)?.strip_prefix('='))
}

/// A fresh client nonce. Printable, comma-free, and unpredictable — the last of which is
/// the only one that matters, since a repeated nonce is what makes a replay possible.
fn nonce() -> String {
    let mut bytes = [0u8; 18];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        // Falling back to the clock is worse, and still better than a constant.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        bytes[..16].copy_from_slice(&now.to_le_bytes());
    }
    b64_encode(&bytes).replace(['+', '/', '='], "x")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_ida_e_volta() {
        for caso in [
            "",
            "a",
            "ab",
            "abc",
            "abcd",
            "qualquer coisa mais longa aqui",
        ] {
            let codificado = b64_encode(caso.as_bytes());
            assert_eq!(b64_decode(&codificado).unwrap(), caso.as_bytes(), "{caso}");
        }
        assert_eq!(b64_encode(b"n,,"), "biws");
    }

    /// Os vetores do RFC 1321. Um MD5 errado só apareceria como «senha incorreta» contra
    /// um servidor Postgres antigo, que é o pior lugar para descobrir.
    #[test]
    fn md5_bate_com_o_rfc() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    /// O exemplo do RFC 5802, com o nonce e o sal de lá: se a conta bater com ele, bate
    /// com qualquer servidor que implemente o mesmo documento.
    #[test]
    fn scram_sha1_reproduz_o_exemplo_do_rfc() {
        let (mut scram, first) = Scram::start(Hash::Sha1, "user", "pencil");
        assert!(first.starts_with("n,,n=user,r="));
        // O nonce é sorteado, então a conversa é refeita com o do RFC no lugar dele.
        scram.nonce = "fyko+d2lbbFgONRv9qkxdawL".to_string();
        scram.first_bare = "n=user,r=fyko+d2lbbFgONRv9qkxdawL".to_string();
        let final_message = scram
            .respond("r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,s=QSXCR+Q6sek8bf92,i=4096")
            .unwrap();
        assert_eq!(
            final_message,
            "c=biws,r=fyko+d2lbbFgONRv9qkxdawL3rfcNHYJY1ZVvWVs7j,p=v0X8v3Bz2T0CJGbJQyF0X+HI4Ts="
        );
        assert!(scram.verify("v=rmF9pqV8S7suAoZWja4dJRkFsKQ=").is_ok());
        assert!(scram.verify("v=outracoisa").is_err());
    }

    #[test]
    fn um_servidor_que_nao_prova_nada_e_recusado() {
        let (mut scram, _) = Scram::start(Hash::Sha256, "u", "senha");
        let nonce = scram.nonce.clone();
        assert!(
            scram
                .respond(&format!("r={nonce}mais,s=QSXCR+Q6sek8bf92,i=4096"))
                .is_ok()
        );
        // Sem `v=`, o servidor não demonstrou conhecer a senha.
        assert!(scram.verify("").is_err());
    }

    #[test]
    fn nonce_trocado_pelo_servidor_interrompe_a_conversa() {
        let (mut scram, _) = Scram::start(Hash::Sha256, "u", "senha");
        assert!(
            scram
                .respond("r=outrononcequalquer,s=QSXCR+Q6sek8bf92,i=4096")
                .is_err()
        );
    }
}
