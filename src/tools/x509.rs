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

/// What we read out of a certificate. Every field is optional because a malformed or
/// merely unusual certificate should cost the caller one missing line, not the whole
/// reading.
#[derive(Default)]
pub struct Cert {
    /// Common Name of the subject — the name the certificate is *for*.
    pub subject: Option<String>,
    /// Common Name of the issuer, falling back to its Organization: "R11" says less
    /// than "Let's Encrypt", and some CAs put the recognisable half in only one of them.
    pub issuer: Option<String>,
    pub not_before: Option<u64>,
    pub not_after: Option<u64>,
    /// dNSName entries from subjectAltName. Modern verifiers ignore CN entirely and
    /// match against these, so a certificate's real coverage is here.
    pub dns_names: Vec<String>,
}

impl Cert {
    /// Whole days from now until it expires; negative once it already has.
    pub fn days_left(&self) -> Option<i64> {
        let not_after = self.not_after?;
        Some((not_after as i64 - now() as i64) / 86_400)
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

/// Reads the certificate's four interesting fields out of its DER.
pub fn parse(der: &[u8]) -> Option<Cert> {
    let certificate = Der::new(der).expect(SEQUENCE)?;
    let mut top = Der::new(certificate);
    let tbs = top.expect(SEQUENCE)?;
    let mut fields = Der::new(tbs);

    // `version` is `[0] EXPLICIT` and defaults to v1 by being absent, so it's peeked at
    // rather than required — a v1 certificate starts straight at the serial number.
    let mut rest = Der::new(fields.bytes);
    if let Some((CONTEXT_0, _)) = rest.next() {
        fields.next();
    }
    fields.next()?; // serialNumber
    fields.expect(SEQUENCE)?; // signature algorithm
    let issuer = fields.expect(SEQUENCE)?;
    let validity = fields.expect(SEQUENCE)?;
    let subject = fields.expect(SEQUENCE)?;
    fields.expect(SEQUENCE)?; // subjectPublicKeyInfo

    let mut cert = Cert {
        subject: name_attribute(subject, OID_CN),
        issuer: name_attribute(issuer, OID_CN).or_else(|| name_attribute(issuer, OID_ORG)),
        ..Default::default()
    };

    let mut times = Der::new(validity);
    cert.not_before = times.next().and_then(|(tag, value)| time(tag, value));
    cert.not_after = times.next().and_then(|(tag, value)| time(tag, value));

    // Extensions are `[3] EXPLICIT`, after two optional fields we never asked about, so
    // the remainder is scanned for them rather than positioned at.
    while !fields.is_empty() {
        match fields.next() {
            Some((CONTEXT_3, value)) => {
                cert.dns_names = subject_alt_names(value).unwrap_or_default();
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    Some(cert)
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

/// dNSName entries of the subjectAltName extension, if it has one.
fn subject_alt_names(extensions: &[u8]) -> Option<Vec<String>> {
    let mut wrapper = Der::new(extensions);
    let list = wrapper.expect(SEQUENCE)?;
    let mut entries = Der::new(list);
    while let Some((SEQUENCE, extension)) = entries.next() {
        let mut parts = Der::new(extension);
        let Some(oid) = parts.expect(OID) else {
            continue;
        };
        if oid != OID_SAN {
            continue;
        }
        // `critical` is a BOOLEAN with a default, so it may or may not be there; the
        // OCTET STRING that follows is what matters either way.
        let contents = loop {
            match parts.next() {
                Some((OCTET_STRING, value)) => break value,
                Some(_) => continue,
                None => return None,
            }
        };
        let mut names = Der::new(Der::new(contents).expect(SEQUENCE)?);
        let mut found = Vec::new();
        while let Some((tag, value)) = names.next() {
            if tag == DNS_NAME {
                found.push(printable(value));
            }
        }
        return Some(found);
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
