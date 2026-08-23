//! Just enough X.509 to say what a certificate is: who it names, who signed it, and
//! when it stops being valid.
//!
//! `rustls` hands over the peer's certificate as raw DER and takes no position on what
//! is inside it — it only had to decide whether to trust it. Everything a person wants
//! to read off a certificate lives in that DER, and pulling four fields out of it is a
//! short walk through a well-specified encoding, so this walks it rather than adding a
//! parsing crate for the purpose. Same trade as the hand-rolled DNS wire format next
//! door.
//!
//! Deliberately partial: no signature checking (rustls already did or didn't), no
//! extension beyond subjectAltName, no attribute beyond CN and O. Anything unparseable
//! comes back as `None` and the caller shows what it did get — a certificate whose
//! validity dates we can't read is still a certificate whose name we can.

use std::time::{SystemTime, UNIX_EPOCH};

/// What we read out of a certificate. Every field is optional or empty-able because a
/// malformed or merely unusual certificate should cost the caller one missing line, not
/// the whole reading.
#[derive(Default)]
pub struct Cert {
    /// X.509 version as a number: 3 for anything issued this century.
    pub version: u8,
    /// Serial as the issuer wrote it, in the colon-separated hex everyone else prints.
    pub serial: String,
    /// Common Name of the subject — the name the certificate is *for*.
    pub subject: Option<String>,
    /// Common Name of the issuer, falling back to its Organization: "R11" says less
    /// than "Let's Encrypt", and some CAs put the recognisable half in only one of them.
    pub issuer: Option<String>,
    /// Every attribute of the subject and issuer names, in the order they appear, so a
    /// full report can show the distinguished name rather than one field of it.
    pub subject_parts: Vec<(String, String)>,
    pub issuer_parts: Vec<(String, String)>,
    pub not_before: Option<u64>,
    pub not_after: Option<u64>,
    /// dNSName entries from subjectAltName. Modern verifiers ignore CN entirely and
    /// match against these, so a certificate's real coverage is here.
    pub dns_names: Vec<String>,
    /// iPAddress entries from the same extension — what a certificate issued to a bare
    /// address is valid for.
    pub ip_addresses: Vec<String>,
    /// How the issuer signed it, e.g. `ecdsa-with-SHA256`. A SHA-1 signature here is
    /// the certificate telling you how old it is.
    pub signature_algorithm: Option<String>,
    /// Public key algorithm and strength, e.g. `RSA 2048 bits` or `ECDSA P-256`.
    pub public_key: Option<String>,
    /// `keyUsage`, spelled out.
    pub key_usage: Vec<String>,
    /// `extendedKeyUsage`, spelled out: what the key is allowed to be used *for*.
    pub extended_key_usage: Vec<String>,
    /// From `basicConstraints`: whether this certificate may sign others, and how deep.
    pub is_ca: bool,
    pub path_len: Option<u64>,
    /// Subject and authority key identifiers, in hex — what links a certificate to the
    /// one above it when several share a name.
    pub subject_key_id: Option<String>,
    pub authority_key_id: Option<String>,
    /// Where to ask whether it has been revoked, and where to fetch the issuer.
    pub ocsp: Vec<String>,
    pub ca_issuers: Vec<String>,
    pub crl: Vec<String>,
    /// Whether it carries embedded Certificate Transparency proofs. Public CAs have
    /// been required to for years; a certificate without them is internal or old.
    pub has_sct: bool,
    /// SHA-256 over the whole DER — the fingerprint every other tool prints.
    pub fingerprint: String,
}

impl Cert {
    /// Whole days from now until it expires; negative once it already has.
    pub fn days_left(&self) -> Option<i64> {
        let not_after = self.not_after?;
        Some((not_after as i64 - now() as i64) / 86_400)
    }

    /// Whether the certificate signed itself — the definition of self-signed, and what
    /// separates a root or a home-made certificate from one somebody vouched for.
    pub fn self_signed(&self) -> bool {
        !self.subject_parts.is_empty() && self.subject_parts == self.issuer_parts
    }

    /// The distinguished name as `CN=x, O=y`, which is how every other tool prints it.
    pub fn subject_dn(&self) -> String {
        distinguished(&self.subject_parts)
    }

    pub fn issuer_dn(&self) -> String {
        distinguished(&self.issuer_parts)
    }

    /// Whether `host` is one of the names this certificate is valid for, by the rule
    /// browsers use: subjectAltName only, with a wildcard matching exactly one label.
    pub fn covers(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.dns_names.iter().any(|name| matches_name(name, &host))
            || self.ip_addresses.iter().any(|ip| ip == &host)
    }

    /// One line for a scan row: what it's for, who signed it, how long it has left.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(subject) = &self.subject {
            parts.push(subject.clone());
        }
        if let Some(issuer) = &self.issuer {
            parts.push(format!("por {issuer}"));
        }
        match self.days_left() {
            // Expiry is the one fact about a certificate that becomes an outage, so it
            // is phrased as the reader would say it rather than as a date to subtract.
            Some(days) if days < 0 => parts.push(format!("VENCIDO há {} dias", -days)),
            Some(0) => parts.push("vence hoje".to_string()),
            Some(days) => parts.push(format!("vence em {days} dias")),
            None => {}
        }
        // A certificate covering half a dozen names is worth flagging as such without
        // printing the list into a table column.
        match self.dns_names.len() {
            0 | 1 => {}
            n => parts.push(format!("{n} nomes")),
        }
        parts.join(", ")
    }
}

/// An epoch second as a UTC date, spelled out. Certificates are UTC by specification —
/// the trailing `Z` — so this is exact without a timezone database, and it says "UTC"
/// so nobody has to wonder whether it was converted.
pub fn utc(epoch: u64) -> String {
    let days = (epoch / 86_400) as i64;
    let seconds = epoch % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

/// The inverse of `days_from_civil`, by the same algorithm read backwards.
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u64;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u64;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// --- DER ------------------------------------------------------------------------------

const SEQUENCE: u8 = 0x30;
const SET: u8 = 0x31;
const OID: u8 = 0x06;
const UTC_TIME: u8 = 0x17;
const GENERALIZED_TIME: u8 = 0x18;
const OCTET_STRING: u8 = 0x04;
/// `[0]` and `[3]` in the TBSCertificate, which are constructed and context-specific.
const CONTEXT_0: u8 = 0xA0;
const CONTEXT_3: u8 = 0xA3;
/// `dNSName` inside a GeneralNames — context tag 2, primitive.
const DNS_NAME: u8 = 0x82;

/// OIDs, as the bytes they're encoded with rather than as dotted strings: comparing two
/// byte slices is the whole of it, and decoding the arcs first would only make it
/// slower and longer.
const OID_CN: &[u8] = &[0x55, 0x04, 0x03]; // 2.5.4.3
const OID_ORG: &[u8] = &[0x55, 0x04, 0x0A]; // 2.5.4.10
const OID_SAN: &[u8] = &[0x55, 0x1D, 0x11]; // 2.5.29.17

/// Name attributes, by their short form — the one that appears in a distinguished
/// name. Anything not here is printed by its dotted OID rather than dropped.
const ATTRIBUTES: &[(&[u8], &str)] = &[
    (&[0x55, 0x04, 0x03], "CN"),
    (&[0x55, 0x04, 0x06], "C"),
    (&[0x55, 0x04, 0x07], "L"),
    (&[0x55, 0x04, 0x08], "ST"),
    (&[0x55, 0x04, 0x0A], "O"),
    (&[0x55, 0x04, 0x0B], "OU"),
    (&[0x55, 0x04, 0x05], "serialNumber"),
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x01],
        "emailAddress",
    ),
];

/// Signature and public-key algorithms. The names are the ones OpenSSL prints, since
/// that's what anyone comparing two readings will have in front of them.
const ALGORITHMS: &[(&[u8], &str)] = &[
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01],
        "RSA",
    ),
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x05],
        "sha1WithRSA",
    ),
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0B],
        "sha256WithRSA",
    ),
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0C],
        "sha384WithRSA",
    ),
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0D],
        "sha512WithRSA",
    ),
    (
        &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x0A],
        "RSASSA-PSS",
    ),
    (&[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01], "ECDSA"),
    (
        &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02],
        "ecdsa-with-SHA256",
    ),
    (
        &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03],
        "ecdsa-with-SHA384",
    ),
    (
        &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x04],
        "ecdsa-with-SHA512",
    ),
    (&[0x2B, 0x65, 0x70], "Ed25519"),
];

/// Named curves, by the size people call them.
const CURVES: &[(&[u8], &str)] = &[
    (&[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07], "P-256"),
    (&[0x2B, 0x81, 0x04, 0x00, 0x22], "P-384"),
    (&[0x2B, 0x81, 0x04, 0x00, 0x23], "P-521"),
];

/// What an extendedKeyUsage OID permits.
const PURPOSES: &[(&[u8], &str)] = &[
    (
        &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01],
        "servidor TLS",
    ),
    (
        &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x02],
        "cliente TLS",
    ),
    (
        &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x03],
        "assinatura de código",
    ),
    (
        &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x04],
        "proteção de e-mail",
    ),
    (
        &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x08],
        "carimbo de tempo",
    ),
    (
        &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x09],
        "assinatura OCSP",
    ),
];

/// `keyUsage` is a BIT STRING whose bits are named in this order by RFC 5280.
const KEY_USAGES: &[&str] = &[
    "assinatura digital",
    "não repúdio",
    "cifrar chave",
    "cifrar dados",
    "acordo de chaves",
    "assinar certificados",
    "assinar CRL",
    "só cifrar",
    "só decifrar",
];

const OID_KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x0F]; // 2.5.29.15
const OID_EKU: &[u8] = &[0x55, 0x1D, 0x25]; // 2.5.29.37
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1D, 0x13]; // 2.5.29.19
const OID_SKI: &[u8] = &[0x55, 0x1D, 0x0E]; // 2.5.29.14
const OID_AKI: &[u8] = &[0x55, 0x1D, 0x23]; // 2.5.29.35
const OID_CRL: &[u8] = &[0x55, 0x1D, 0x1F]; // 2.5.29.31
const OID_AIA: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x01]; // 1.3.6.1.5.5.7.1.1
const OID_OCSP: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x01];
const OID_CA_ISSUERS: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x30, 0x02];
/// Embedded signed certificate timestamps — 1.3.6.1.4.1.11129.2.4.2.
const OID_SCT: &[u8] = &[0x2B, 0x06, 0x01, 0x04, 0x01, 0xD6, 0x79, 0x02, 0x04, 0x02];

const BIT_STRING: u8 = 0x03;
const INTEGER: u8 = 0x02;
const BOOLEAN: u8 = 0x01;
/// `uniformResourceIdentifier` in a GeneralName, and `iPAddress`.
const URI: u8 = 0x86;
const IP_ADDRESS: u8 = 0x87;

/// A cursor over DER, handing back one tag-length-value at a time.
struct Der<'a> {
    bytes: &'a [u8],
}

impl<'a> Der<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// The next element: its tag and its contents, with the header consumed.
    ///
    /// Lengths come in two forms — one byte under 128, or a byte saying how many bytes
    /// the length itself takes. Four length bytes is already a 4 GB certificate, so
    /// anything longer is refused rather than accommodated.
    fn next(&mut self) -> Option<(u8, &'a [u8])> {
        let tag = *self.bytes.first()?;
        let first_len = *self.bytes.get(1)? as usize;
        let (len, header) = if first_len < 0x80 {
            (first_len, 2)
        } else {
            let count = first_len & 0x7F;
            if count == 0 || count > 4 {
                return None;
            }
            let mut len = 0usize;
            for i in 0..count {
                len = (len << 8) | *self.bytes.get(2 + i)? as usize;
            }
            (len, 2 + count)
        };
        let end = header.checked_add(len)?;
        if end > self.bytes.len() {
            return None;
        }
        let value = &self.bytes[header..end];
        self.bytes = &self.bytes[end..];
        Some((tag, value))
    }

    /// The next element, required to carry `tag`.
    fn expect(&mut self, tag: u8) -> Option<&'a [u8]> {
        let (found, value) = self.next()?;
        (found == tag).then_some(value)
    }
}

/// Reads a certificate out of its DER — everything this app knows how to say about one.
pub fn parse(der: &[u8]) -> Option<Cert> {
    let certificate = Der::new(der).expect(SEQUENCE)?;
    let mut top = Der::new(certificate);
    let tbs = top.expect(SEQUENCE)?;
    let mut fields = Der::new(tbs);

    let mut cert = Cert {
        version: 1,
        fingerprint: fingerprint(der),
        ..Default::default()
    };

    // `version` is `[0] EXPLICIT` and defaults to v1 by being absent, so it's peeked at
    // rather than required — a v1 certificate starts straight at the serial number.
    let mut rest = Der::new(fields.bytes);
    if let Some((CONTEXT_0, value)) = rest.next() {
        fields.next();
        // Stored zero-based in the encoding: the byte 2 means v3.
        cert.version = Der::new(value)
            .expect(INTEGER)
            .and_then(|v| v.first().copied())
            .map(|v| v + 1)
            .unwrap_or(1);
    }
    cert.serial = fields
        .next()
        .map(|(_, value)| hex(value))
        .unwrap_or_default();
    cert.signature_algorithm = fields.expect(SEQUENCE).and_then(algorithm_name);
    let issuer = fields.expect(SEQUENCE)?;
    let validity = fields.expect(SEQUENCE)?;
    let subject = fields.expect(SEQUENCE)?;
    let spki = fields.expect(SEQUENCE)?;

    cert.subject_parts = name_parts(subject);
    cert.issuer_parts = name_parts(issuer);
    cert.subject = name_attribute(subject, OID_CN);
    cert.issuer = name_attribute(issuer, OID_CN).or_else(|| name_attribute(issuer, OID_ORG));
    cert.public_key = public_key(spki);

    let mut times = Der::new(validity);
    cert.not_before = times.next().and_then(|(tag, value)| time(tag, value));
    cert.not_after = times.next().and_then(|(tag, value)| time(tag, value));

    // Extensions are `[3] EXPLICIT`, after two optional fields we never asked about, so
    // the remainder is scanned for them rather than positioned at.
    while !fields.is_empty() {
        match fields.next() {
            Some((CONTEXT_3, value)) => {
                extensions(&mut cert, value);
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    Some(cert)
}

/// Walks the extension list once, filling in whatever it recognises. An extension we
/// don't know is skipped in silence — there are hundreds, and a certificate carrying an
/// exotic one is not a certificate we failed to read.
fn extensions(cert: &mut Cert, extensions: &[u8]) {
    let Some(list) = Der::new(extensions).expect(SEQUENCE) else {
        return;
    };
    let mut entries = Der::new(list);
    while let Some((SEQUENCE, extension)) = entries.next() {
        let mut parts = Der::new(extension);
        let Some(oid) = parts.expect(OID) else {
            continue;
        };
        // `critical` is a BOOLEAN with a default, so it may or may not be there; the
        // OCTET STRING that follows is what matters either way.
        let mut contents = None;
        while let Some((tag, value)) = parts.next() {
            match tag {
                BOOLEAN => continue,
                OCTET_STRING => {
                    contents = Some(value);
                    break;
                }
                _ => break,
            }
        }
        let Some(contents) = contents else { continue };

        match oid {
            _ if oid == OID_SAN => {
                let (names, ips) = alt_names(contents);
                cert.dns_names = names;
                cert.ip_addresses = ips;
            }
            _ if oid == OID_KEY_USAGE => cert.key_usage = key_usage(contents),
            _ if oid == OID_EKU => cert.extended_key_usage = purposes(contents),
            _ if oid == OID_BASIC_CONSTRAINTS => {
                if let Some(inner) = Der::new(contents).expect(SEQUENCE) {
                    let mut parts = Der::new(inner);
                    while let Some((tag, value)) = parts.next() {
                        match tag {
                            BOOLEAN => cert.is_ca = value.first().copied().unwrap_or(0) != 0,
                            INTEGER => cert.path_len = Some(be_number(value)),
                            _ => {}
                        }
                    }
                }
            }
            _ if oid == OID_SKI => {
                cert.subject_key_id = Der::new(contents).expect(OCTET_STRING).map(hex);
            }
            _ if oid == OID_AKI => {
                // The identifier is `[0]` inside a SEQUENCE, alongside optional issuer
                // name and serial that nothing here needs.
                if let Some(inner) = Der::new(contents).expect(SEQUENCE) {
                    let mut parts = Der::new(inner);
                    while let Some((tag, value)) = parts.next() {
                        if tag == 0x80 {
                            cert.authority_key_id = Some(hex(value));
                            break;
                        }
                    }
                }
            }
            _ if oid == OID_AIA => {
                let (ocsp, issuers) = access_descriptions(contents);
                cert.ocsp = ocsp;
                cert.ca_issuers = issuers;
            }
            _ if oid == OID_CRL => cert.crl = uris(contents),
            _ if oid == OID_SCT => cert.has_sct = true,
            _ => {}
        }
    }
}

/// Every attribute of a Name, in order, as (short name, value).
fn name_parts(name: &[u8]) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let mut rdns = Der::new(name);
    while let Some((SET, rdn)) = rdns.next() {
        let mut attributes = Der::new(rdn);
        while let Some((SEQUENCE, attribute)) = attributes.next() {
            let mut fields = Der::new(attribute);
            let Some(oid) = fields.expect(OID) else {
                continue;
            };
            let Some((_, value)) = fields.next() else {
                continue;
            };
            let label = ATTRIBUTES
                .iter()
                .find(|(known, _)| *known == oid)
                .map(|(_, name)| (*name).to_string())
                .unwrap_or_else(|| dotted(oid));
            parts.push((label, printable(value)));
        }
    }
    parts
}

fn distinguished(parts: &[(String, String)]) -> String {
    parts
        .iter()
        .map(|(label, value)| format!("{label}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Algorithm name from an AlgorithmIdentifier — the OID it starts with.
fn algorithm_name(algorithm: &[u8]) -> Option<String> {
    let oid = Der::new(algorithm).expect(OID)?;
    Some(
        ALGORITHMS
            .iter()
            .find(|(known, _)| *known == oid)
            .map(|(_, name)| (*name).to_string())
            .unwrap_or_else(|| dotted(oid)),
    )
}

/// Algorithm and strength of the public key: the modulus length for RSA, the curve for
/// ECDSA. Both are what someone means when they ask how strong a certificate is.
fn public_key(spki: &[u8]) -> Option<String> {
    let mut parts = Der::new(spki);
    let algorithm = parts.expect(SEQUENCE)?;
    let key = parts.expect(BIT_STRING)?;
    let mut identifier = Der::new(algorithm);
    let oid = identifier.expect(OID)?;
    let name = ALGORITHMS
        .iter()
        .find(|(known, _)| *known == oid)
        .map(|(_, name)| *name)
        .unwrap_or("chave");

    if name == "ECDSA" {
        let curve = identifier
            .expect(OID)
            .and_then(|oid| {
                CURVES
                    .iter()
                    .find(|(known, _)| *known == oid)
                    .map(|(_, name)| (*name).to_string())
            })
            .unwrap_or_else(|| "curva desconhecida".to_string());
        return Some(format!("ECDSA {curve}"));
    }
    if name == "RSA" {
        // A BIT STRING's first byte counts the unused bits at the end; the RSAPublicKey
        // SEQUENCE starts after it.
        let inner = Der::new(key.get(1..)?).expect(SEQUENCE)?;
        let modulus = Der::new(inner).expect(INTEGER)?;
        // DER integers are signed, so a modulus with its top bit set carries a leading
        // zero byte that is padding rather than key material.
        let bytes = modulus.iter().skip_while(|b| **b == 0).count();
        return Some(format!("RSA {} bits", bytes * 8));
    }
    Some(name.to_string())
}

/// The named bits of `keyUsage`, in RFC order.
fn key_usage(contents: &[u8]) -> Vec<String> {
    let Some(bits) = Der::new(contents).expect(BIT_STRING) else {
        return Vec::new();
    };
    let Some((unused, bytes)) = bits.split_first() else {
        return Vec::new();
    };
    let total = bytes.len() * 8 - *unused as usize;
    (0..total.min(KEY_USAGES.len()))
        .filter(|bit| bytes[bit / 8] & (0x80 >> (bit % 8)) != 0)
        .map(|bit| KEY_USAGES[bit].to_string())
        .collect()
}

/// What extendedKeyUsage permits, named.
fn purposes(contents: &[u8]) -> Vec<String> {
    let Some(list) = Der::new(contents).expect(SEQUENCE) else {
        return Vec::new();
    };
    let mut entries = Der::new(list);
    let mut found = Vec::new();
    while let Some((OID, oid)) = entries.next() {
        found.push(
            PURPOSES
                .iter()
                .find(|(known, _)| *known == oid)
                .map(|(_, name)| (*name).to_string())
                .unwrap_or_else(|| dotted(oid)),
        );
    }
    found
}

/// dNSName and iPAddress entries of a subjectAltName.
fn alt_names(contents: &[u8]) -> (Vec<String>, Vec<String>) {
    let Some(list) = Der::new(contents).expect(SEQUENCE) else {
        return (Vec::new(), Vec::new());
    };
    let mut names = Der::new(list);
    let (mut dns, mut ips) = (Vec::new(), Vec::new());
    while let Some((tag, value)) = names.next() {
        match tag {
            DNS_NAME => dns.push(printable(value)),
            // Four bytes for IPv4, sixteen for IPv6 — the address as the kernel would
            // hold it, not as text.
            IP_ADDRESS => ips.push(match value.len() {
                4 => std::net::Ipv4Addr::from([value[0], value[1], value[2], value[3]]).to_string(),
                16 => {
                    let mut octets = [0u8; 16];
                    octets.copy_from_slice(value);
                    std::net::Ipv6Addr::from(octets).to_string()
                }
                _ => hex(value),
            }),
            _ => {}
        }
    }
    (dns, ips)
}

/// OCSP responders and issuer locations out of authorityInfoAccess.
fn access_descriptions(contents: &[u8]) -> (Vec<String>, Vec<String>) {
    let Some(list) = Der::new(contents).expect(SEQUENCE) else {
        return (Vec::new(), Vec::new());
    };
    let mut entries = Der::new(list);
    let (mut ocsp, mut issuers) = (Vec::new(), Vec::new());
    while let Some((SEQUENCE, description)) = entries.next() {
        let mut parts = Der::new(description);
        let Some(method) = parts.expect(OID) else {
            continue;
        };
        let Some((URI, location)) = parts.next() else {
            continue;
        };
        let url = printable(location);
        if method == OID_OCSP {
            ocsp.push(url);
        } else if method == OID_CA_ISSUERS {
            issuers.push(url);
        }
    }
    (ocsp, issuers)
}

/// Every URI buried anywhere in a structure of nested SEQUENCEs and context tags —
/// which is what a CRL distribution point is, and its shape varies enough that walking
/// for the URIs beats spelling out the grammar.
fn uris(contents: &[u8]) -> Vec<String> {
    let mut found = Vec::new();
    collect_uris(contents, 0, &mut found);
    found
}

fn collect_uris(bytes: &[u8], depth: usize, found: &mut Vec<String>) {
    if depth > 6 {
        return;
    }
    let mut reader = Der::new(bytes);
    while let Some((tag, value)) = reader.next() {
        if tag == URI {
            found.push(printable(value));
        } else if tag & 0x20 != 0 {
            // Constructed: something is nested inside it.
            collect_uris(value, depth + 1, found);
        }
    }
}

/// Whether a certificate name matches a host, by the browser rule: exact, or a leading
/// `*` standing for exactly one label.
fn matches_name(name: &str, host: &str) -> bool {
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    if let Some(suffix) = name.strip_prefix("*.") {
        return host.split_once('.').is_some_and(|(_, rest)| rest == suffix);
    }
    name == host
}

/// SHA-256 of the whole certificate, in the colon-separated hex every other tool prints.
fn fingerprint(der: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, der).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// An OID back as dotted arcs, for the ones no table here names. The first byte packs
/// two arcs; the rest are base-128 with a continuation bit.
fn dotted(oid: &[u8]) -> String {
    let Some((&first, rest)) = oid.split_first() else {
        return String::new();
    };
    let mut arcs = vec![(first / 40).to_string(), (first % 40).to_string()];
    let mut value: u64 = 0;
    for byte in rest {
        value = (value << 7) | (byte & 0x7F) as u64;
        if byte & 0x80 == 0 {
            arcs.push(value.to_string());
            value = 0;
        }
    }
    arcs.join(".")
}

/// A DER INTEGER's value, for the small ones (path lengths, versions).
fn be_number(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, b| (acc << 8) | *b as u64)
}

/// The first value of `wanted` in a Name — walking RDNSequence → RDN → AttributeTypeAndValue.
/// A Name with several CNs is pathological; the first is the one every reader means.
fn name_attribute(name: &[u8], wanted: &[u8]) -> Option<String> {
    let mut rdns = Der::new(name);
    while let Some((SET, rdn)) = rdns.next() {
        let mut attributes = Der::new(rdn);
        while let Some((SEQUENCE, attribute)) = attributes.next() {
            let mut parts = Der::new(attribute);
            let Some(oid) = parts.expect(OID) else {
                continue;
            };
            let Some((_, value)) = parts.next() else {
                continue;
            };
            if oid == wanted {
                return Some(printable(value));
            }
        }
    }
    None
}

/// A UTCTime or GeneralizedTime as seconds since the epoch. Both are UTC by
/// specification — the trailing `Z` — which is why no timezone ever enters into it.
fn time(tag: u8, value: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(value).ok()?;
    let digits: Vec<u8> = text.bytes().filter(|b| b.is_ascii_digit()).collect();
    let read = |from: usize, len: usize| -> Option<i64> {
        std::str::from_utf8(digits.get(from..from + len)?)
            .ok()?
            .parse()
            .ok()
    };
    let (year, rest) = match tag {
        // Two-digit years, split at 50 by RFC 5280: 49 is 2049, 50 is 1950.
        UTC_TIME => {
            let short = read(0, 2)?;
            (
                if short < 50 {
                    2000 + short
                } else {
                    1900 + short
                },
                2,
            )
        }
        GENERALIZED_TIME => (read(0, 4)?, 4),
        _ => return None,
    };
    let month = read(rest, 2)?;
    let day = read(rest + 2, 2)?;
    let hour = read(rest + 4, 2).unwrap_or(0);
    let minute = read(rest + 6, 2).unwrap_or(0);
    let second = read(rest + 8, 2).unwrap_or(0);
    let seconds = days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second;
    // A date before 1970 can't be a validity anyone cares about — it's a misread or a
    // certificate from a machine with a broken clock — and refusing it here is what
    // keeps every downstream figure an honest unsigned second count.
    u64::try_from(seconds).ok()
}

/// Days from 1970-01-01 to a civil date, by Howard Hinnant's algorithm: shift the year
/// to start in March so the leap day lands at the end of it, and every month length
/// becomes a straight line. No lookup tables, no leap-year branches.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// A DER string as something safe to print. Certificates carry UTF8String, PrintableString
/// and, in older ones, BMPString; the first two are read as-is and anything else has its
/// unprintable bytes dropped rather than being refused.
fn printable(value: &[u8]) -> String {
    String::from_utf8_lossy(value)
        .chars()
        .filter(|c| !c.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real certificate, fetched from example.com and kept as it arrived. Parsing is
    /// checked against fields rather than against anything time-relative, so this test
    /// keeps meaning something after the certificate itself expires.
    const EXAMPLE_COM: &[u8] = include_bytes!("testdata/example-com.der");

    #[test]
    fn reads_a_real_certificate() {
        let cert = parse(EXAMPLE_COM).expect("certificado válido não foi lido");
        assert_eq!(cert.subject.as_deref(), Some("example.com"));
        assert_eq!(
            cert.issuer.as_deref(),
            Some("Cloudflare TLS Issuing ECC CA 3")
        );
        assert_eq!(cert.dns_names, vec!["example.com", "*.example.com"]);
        // Oct 27 22:17:21 2026 GMT, as openssl reads it.
        assert_eq!(cert.not_after, Some(1_793_139_441));
        assert!(cert.not_before.unwrap() < cert.not_after.unwrap());
    }

    #[test]
    fn a_truncated_certificate_is_refused_rather_than_guessed() {
        assert!(parse(&EXAMPLE_COM[..EXAMPLE_COM.len() / 2]).is_none());
        assert!(parse(b"").is_none());
        assert!(parse(b"\x30\x82\xff\xff").is_none());
    }

    #[test]
    fn utc_round_trips_through_the_civil_calendar() {
        assert_eq!(utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc(1_793_139_441), "2026-10-27 22:17:21 UTC");
        // A leap day, which is where a calendar that cuts corners goes wrong.
        assert_eq!(utc(1_709_164_800), "2024-02-29 00:00:00 UTC");
    }

    #[test]
    fn wildcards_cover_one_label_and_no_more() {
        assert!(matches_name("*.example.com", "api.example.com"));
        assert!(!matches_name("*.example.com", "a.b.example.com"));
        assert!(!matches_name("*.example.com", "example.com"));
        assert!(matches_name("Example.COM", "example.com"));
    }

    #[test]
    fn civil_days_match_known_epochs() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11017);
        assert_eq!(days_from_civil(2026, 8, 23), 20688);
    }

    #[test]
    fn utc_time_splits_the_century_at_fifty() {
        assert_eq!(time(UTC_TIME, b"260823120000Z"), Some(1_787_486_400));
        // 49 is 2049 and 50 is 1950, per RFC 5280 — and 1950, being before the epoch,
        // is refused rather than wrapped into some enormous positive second count.
        assert_eq!(
            time(UTC_TIME, b"490101000000Z"),
            time(GENERALIZED_TIME, b"20490101000000Z")
        );
        assert_eq!(time(UTC_TIME, b"500101000000Z"), None);
    }

    #[test]
    fn generalized_time_carries_a_four_digit_year() {
        assert_eq!(
            time(GENERALIZED_TIME, b"20260823120000Z"),
            time(UTC_TIME, b"260823120000Z")
        );
    }
}
