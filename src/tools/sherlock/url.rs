//! Taking a connection string apart.
//!
//! Both engines here are configured by a URL, and both URLs are the same shape with
//! different defaults: a scheme, credentials, one or more hosts, a path that names the
//! database, and options after a `?`. The standard library has no URL parser and the one
//! thing a hand-written parser has to get right is the password — it is the field most
//! likely to contain an `@`, a `/` or a `%`, and the field where being wrong means an
//! authentication failure nobody can explain.

/// A connection string, in pieces. Credentials are already percent-decoded; everything
/// else is as written.
pub struct Parts {
    pub user: String,
    pub password: String,
    /// Host and, when the URL said so, port. More than one for a replica set.
    pub hosts: Vec<(String, Option<u16>)>,
    /// What came after the host and before the `?`, with its slash removed — the
    /// database name, for both engines.
    pub path: String,
    pub options: Vec<(String, String)>,
}

impl Parts {
    /// An option by name, compared without case — `sslMode` and `sslmode` are the same
    /// option, and which spelling a given tutorial used is not worth caring about.
    pub fn option(&self, key: &str) -> &str {
        self.options
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(key))
            .map(|(_, value)| value.as_str())
            .unwrap_or("")
    }
}

/// Splits a URL whose scheme is one of `schemes`. `None` when it is something else —
/// the caller says so in its own words, since it knows which engine was expected.
pub fn split(text: &str, schemes: &[&str]) -> Option<Parts> {
    let text = text.trim();
    let (scheme, rest) = text.split_once("://")?;
    if !schemes.iter().any(|s| scheme.eq_ignore_ascii_case(s)) {
        return None;
    }

    let (rest, query) = match rest.split_once('?') {
        Some((rest, query)) => (rest, query),
        None => (rest, ""),
    };
    // The authority ends at the first `/`, and the credentials end at the *last* `@`
    // inside it: a password may legitimately contain an unencoded `@`, and taking the
    // first one would cut it in half. A `/` inside the password is a different matter —
    // it has to arrive percent-encoded, here as in every driver that reads these URLs,
    // because raw it is indistinguishable from the start of the path.
    let (authority, path) = match rest.split_once('/') {
        Some((authority, path)) => (authority, path),
        None => (rest, ""),
    };
    let (credentials, hosts) = match authority.rsplit_once('@') {
        Some((credentials, hosts)) => (credentials, hosts),
        None => ("", authority),
    };
    let (user, password) = match credentials.split_once(':') {
        Some((user, password)) => (decode(user), decode(password)),
        None => (decode(credentials), String::new()),
    };

    let hosts = hosts
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| {
            // An IPv6 literal wears brackets precisely so its colons aren't read as a
            // port separator.
            if let Some(rest) = host.strip_prefix('[') {
                let (address, tail) = rest.split_once(']').unwrap_or((rest, ""));
                let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
                return (address.to_string(), port);
            }
            match host.rsplit_once(':') {
                Some((address, port)) => (address.to_string(), port.parse().ok()),
                None => (host.to_string(), None),
            }
        })
        .collect();

    let options = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (decode(key), decode(value)),
            None => (decode(pair), String::new()),
        })
        .collect();

    Some(Parts {
        user,
        password,
        hosts,
        path: decode(path.split('?').next().unwrap_or(path)),
        options,
    })
}

/// `%40` back into `@`. A stray `%` that isn't followed by two hex digits is kept as
/// written — it is far more likely to be a password character than a broken escape.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(hex) = bytes.get(i + 1..i + 3)
            && let Ok(text) = std::str::from_utf8(hex)
            && let Ok(byte) = u8::from_str_radix(text, 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_senha_pode_ter_arroba() {
        let parts = split("postgres://u:p@ss@host:5432/base", &["postgres"]).unwrap();
        assert_eq!(parts.user, "u");
        assert_eq!(parts.password, "p@ss");
        assert_eq!(parts.hosts, vec![("host".to_string(), Some(5432))]);
        assert_eq!(parts.path, "base");
    }

    /// Uma barra na senha **precisa** vir escapada, e é assim em toda biblioteca que lê
    /// esta URL: crua, ela é indistinguível do começo do caminho. O teste existe para
    /// dizer qual das duas leituras é a nossa.
    #[test]
    fn a_barra_na_senha_vem_escapada() {
        let parts = split("postgres://u:p%2Fw@host/base", &["postgres"]).unwrap();
        assert_eq!(parts.password, "p/w");
        assert_eq!(parts.path, "base");
    }

    #[test]
    fn percent_vira_o_caractere_e_o_percent_solto_fica() {
        let parts = split(
            "mongodb://u:se%40nha%2F1@h/b?authSource=admin",
            &["mongodb"],
        )
        .unwrap();
        assert_eq!(parts.password, "se@nha/1");
        assert_eq!(parts.option("authsource"), "admin");
        let parts = split("mongodb://u:100%pu.ro@h/b", &["mongodb"]).unwrap();
        assert_eq!(parts.password, "100%pu.ro");
    }

    #[test]
    fn varios_hosts_e_ipv6() {
        let parts = split("mongodb://a:1,b:2,[::1]:3/base", &["mongodb"]).unwrap();
        assert_eq!(
            parts.hosts,
            vec![
                ("a".to_string(), Some(1)),
                ("b".to_string(), Some(2)),
                ("::1".to_string(), Some(3)),
            ]
        );
        assert!(parts.user.is_empty());
    }

    #[test]
    fn esquema_errado_nao_passa() {
        assert!(split("mysql://h/b", &["postgres", "postgresql"]).is_none());
        assert!(split("postgresql://h/b", &["postgres", "postgresql"]).is_some());
    }
}
