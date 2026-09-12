//! BSON, the binary document format every MongoDB command is written in.
//!
//! Only as much of it as a conversation with a server needs: commands are small
//! documents of numbers and strings, and replies are arbitrarily deep documents of
//! everything else. So encoding covers the handful of types a command uses and decoding
//! covers the whole type byte range, because a reply that contains one timestamp this
//! parser has never seen would otherwise take the rest of the reply down with it.

use std::time::{SystemTime, UNIX_EPOCH};

/// One BSON value. The variants that carry no useful structure for an inspection —
/// regular expressions, code, pointers — arrive as `Other` with their type named, which
/// is enough to print and enough to skip.
#[derive(Clone, Debug)]
pub enum Value {
    Double(f64),
    Text(String),
    Doc(Doc),
    List(Vec<Value>),
    Binary(u8, Vec<u8>),
    Bool(bool),
    Null,
    Int32(i32),
    Int64(i64),
    /// Milliseconds since the epoch.
    Time(i64),
    ObjectId([u8; 12]),
    Other(&'static str),
}

impl Value {
    /// Any of the four numeric shapes as one number. A server that answers `1` where it
    /// answered `1.0` yesterday is not a case worth handling twice.
    pub fn as_num(&self) -> Option<f64> {
        match self {
            Value::Double(value) => Some(*value),
            Value::Int32(value) => Some(f64::from(*value)),
            Value::Int64(value) => Some(*value as f64),
            Value::Time(value) => Some(*value as f64),
            Value::Bool(value) => Some(f64::from(*value)),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Value::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_doc(&self) -> Option<&Doc> {
        match self {
            Value::Doc(doc) => Some(doc),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(list) => Some(list),
            _ => None,
        }
    }

    /// How the value reads in a report line.
    ///
    /// Um documento vira a **forma** dele — as chaves, e no lugar de cada valor uma marca
    /// do tipo: `{status: "…", total: {$gt: 1}}`. Ver `Doc::render_forma`.
    pub fn render(&self) -> String {
        match self {
            Value::Double(value) => format!("{value}"),
            Value::Text(text) => text.clone(),
            Value::Doc(doc) => doc.render_forma(),
            Value::List(list) => {
                let itens: Vec<String> = list.iter().take(3).map(Value::render).collect();
                match list.len() > 3 {
                    true => format!("[{}, …+{}]", itens.join(", "), list.len() - 3),
                    false => format!("[{}]", itens.join(", ")),
                }
            }
            Value::Binary(_, bytes) => format!("{} bytes", bytes.len()),
            Value::Bool(value) => (if *value { "sim" } else { "não" }).to_string(),
            Value::Null => String::new(),
            Value::Int32(value) => value.to_string(),
            Value::Int64(value) => value.to_string(),
            Value::Time(ms) => ago(*ms),
            Value::ObjectId(bytes) => bytes.iter().map(|b| format!("{b:02x}")).collect(),
            Value::Other(name) => format!("<{name}>"),
        }
    }

    /// O valor como uma marca do tipo dele, sem o conteúdo.
    ///
    /// `"…"` para texto, o número para número — um número num filtro é quase sempre um
    /// limite, e um limite é a própria pergunta («mais de mil o quê»), não um dado de
    /// ninguém. O que some é o que identifica alguém: nome, e-mail, documento, id.
    fn forma(&self) -> String {
        match self {
            Value::Text(_) => "\"…\"".to_string(),
            Value::Doc(doc) => doc.render_forma(),
            Value::List(list) => format!("[…{}]", list.len()),
            Value::ObjectId(_) => "ObjectId(…)".to_string(),
            Value::Time(_) => "data".to_string(),
            Value::Binary(..) => "bin".to_string(),
            outro => outro.render(),
        }
    }
}

/// A timestamp as the distance from now, which is the only thing a report ever wants
/// from one and needs no calendar to compute.
fn ago(ms: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let seconds = (now - ms) as f64 / 1000.0;
    if seconds < 0.0 {
        return "no futuro".to_string();
    }
    if seconds < 90.0 {
        return format!("há {seconds:.0}s");
    }
    if seconds < 5400.0 {
        return format!("há {:.0} min", seconds / 60.0);
    }
    if seconds < 172_800.0 {
        return format!("há {:.1}h", seconds / 3600.0);
    }
    format!("há {:.0} dias", seconds / 86_400.0)
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Value::Int32(value)
    }
}
impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Value::Int64(value)
    }
}
impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Value::Double(value)
    }
}
impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Value::Bool(value)
    }
}
impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Value::Text(value.to_string())
    }
}
impl From<String> for Value {
    fn from(value: String) -> Self {
        Value::Text(value)
    }
}
impl From<Doc> for Value {
    fn from(value: Doc) -> Self {
        Value::Doc(value)
    }
}
impl From<Vec<Value>> for Value {
    fn from(value: Vec<Value>) -> Self {
        Value::List(value)
    }
}

/// Campos que todo comando carrega e que não dizem nada sobre ele: o id da sessão, o
/// relógio do cluster, a preferência de leitura. Numa linha de relatório eles empurram
/// para fora justamente o que se queria ler.
const RUIDO: &[&str] = &[
    "lsid",
    "$clusterTime",
    "$readPreference",
    "$audit",
    "$client",
    "$configServerState",
    "txnNumber",
    "autocommit",
    "maxTimeMS",
    "cursor",
    "apiVersion",
    "apiStrict",
    "signature",
    "readConcern",
    "writeConcern",
    "shardVersion",
    "databaseVersion",
    "clientOperationKey",
    "mayBypassWriteBlocking",
];

/// A document: ordered pairs, because BSON is ordered and MongoDB cares — the first key
/// of a command document *is* the command's name.
#[derive(Clone, Debug, Default)]
pub struct Doc(pub Vec<(String, Value)>);

impl Doc {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Adds a field, chainable, so a command reads as one expression.
    pub fn with(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.0.push((key.to_string(), value.into()));
        self
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    /// A value down a dotted path — `wiredTiger.cache` — since every interesting number
    /// in `serverStatus` is three documents deep.
    pub fn path(&self, path: &str) -> Option<&Value> {
        let mut current = self;
        let mut parts = path.split('.').peekable();
        while let Some(part) = parts.next() {
            let value = current.get(part)?;
            if parts.peek().is_none() {
                return Some(value);
            }
            current = value.as_doc()?;
        }
        None
    }

    pub fn num(&self, path: &str) -> f64 {
        self.path(path).and_then(Value::as_num).unwrap_or(0.0)
    }

    pub fn int(&self, path: &str) -> i64 {
        self.num(path) as i64
    }

    pub fn text(&self, path: &str) -> String {
        self.path(path).map(Value::render).unwrap_or_default()
    }

    pub fn flag(&self, path: &str) -> bool {
        matches!(self.path(path), Some(Value::Bool(true)))
    }

    pub fn doc(&self, path: &str) -> Option<&Doc> {
        self.path(path).and_then(Value::as_doc)
    }

    /// A list field, empty when it is absent or is something else — a caller iterating
    /// over "the collections" should not have to care which.
    pub fn list(&self, path: &str) -> &[Value] {
        self.path(path).and_then(Value::as_list).unwrap_or(&[])
    }

    /// O documento como a **forma** dele: as chaves por inteiro, os valores trocados por
    /// uma marca do tipo.
    ///
    /// É a mesma decisão que o Postgres já toma sozinho — `pg_stat_statements` guarda
    /// `WHERE email = $1`, nunca o e-mail — e ela vale mais ainda aqui, onde o filtro
    /// chega inteiro do log ou do profiler. Uma tela de investigação de banco de produção
    /// é olhada por quem está ao lado, fotografada e colada em chamado; o que responde
    /// «por que está lento» é a forma da query, e o dado de um cliente nunca é parte da
    /// resposta.
    ///
    /// As chaves ficam porque são o assunto: são elas que dizem qual índice está
    /// faltando. Os nomes que identificam a operação — a coleção de um `find`, o banco de
    /// um `$db` — também ficam, porque não são dado de ninguém.
    pub fn render_forma(&self) -> String {
        /// Chaves cujo valor é o nome de uma coisa, e não um dado.
        const NOMES: &[&str] = &[
            "find",
            "aggregate",
            "count",
            "distinct",
            "update",
            "insert",
            "delete",
            "collection",
            "listIndexes",
            "collStats",
            "$db",
            "ns",
            "name",
            "planSummary",
            "op",
        ];
        let dentro: Vec<String> = self
            .0
            .iter()
            .filter(|(chave, _)| !RUIDO.contains(&chave.as_str()))
            .take(6)
            .map(|(chave, valor)| match NOMES.contains(&chave.as_str()) {
                true => format!("{chave}: {}", valor.render()),
                false => format!("{chave}: {}", valor.forma()),
            })
            .collect();
        format!("{{{}}}", dentro.join(", "))
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(key, _)| key.as_str())
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for (key, value) in &self.0 {
            encode_value(&mut body, key, value);
        }
        let mut out = ((body.len() + 5) as i32).to_le_bytes().to_vec();
        out.extend_from_slice(&body);
        out.push(0);
        out
    }
}

fn encode_value(out: &mut Vec<u8>, key: &str, value: &Value) {
    let tag = match value {
        Value::Double(_) => 0x01,
        Value::Text(_) => 0x02,
        Value::Doc(_) => 0x03,
        Value::List(_) => 0x04,
        Value::Binary(..) => 0x05,
        Value::Bool(_) => 0x08,
        Value::Null | Value::Other(_) => 0x0a,
        Value::Int32(_) => 0x10,
        Value::Int64(_) => 0x12,
        // A date, not a number: a filter comparing against a `ts` field has to send one
        // the server will compare as a date.
        Value::Time(_) => 0x09,
        Value::ObjectId(_) => 0x07,
    };
    out.push(tag);
    out.extend_from_slice(key.as_bytes());
    out.push(0);
    match value {
        Value::Double(number) => out.extend_from_slice(&number.to_le_bytes()),
        Value::Text(text) => {
            out.extend_from_slice(&((text.len() + 1) as i32).to_le_bytes());
            out.extend_from_slice(text.as_bytes());
            out.push(0);
        }
        Value::Doc(doc) => out.extend_from_slice(&doc.encode()),
        Value::List(list) => {
            // An array is a document whose keys are "0", "1", "2".
            let mut doc = Doc::new();
            for (index, item) in list.iter().enumerate() {
                doc.0.push((index.to_string(), item.clone()));
            }
            out.extend_from_slice(&doc.encode());
        }
        Value::Binary(subtype, bytes) => {
            out.extend_from_slice(&(bytes.len() as i32).to_le_bytes());
            out.push(*subtype);
            out.extend_from_slice(bytes);
        }
        Value::Bool(flag) => out.push(u8::from(*flag)),
        Value::Null | Value::Other(_) => {}
        Value::Int32(number) => out.extend_from_slice(&number.to_le_bytes()),
        Value::Int64(number) => out.extend_from_slice(&number.to_le_bytes()),
        Value::Time(number) => out.extend_from_slice(&number.to_le_bytes()),
        Value::ObjectId(bytes) => out.extend_from_slice(bytes),
    }
}

/// Reads one document from the front of `bytes`.
pub fn decode(bytes: &[u8]) -> Result<Doc, String> {
    let (doc, _) = read_doc(bytes, 0)?;
    Ok(doc)
}

fn read_doc(bytes: &[u8], at: usize) -> Result<(Doc, usize), String> {
    let length = read_i32(bytes, at)? as usize;
    let end = at
        .checked_add(length)
        .filter(|end| *end <= bytes.len() && length >= 5)
        .ok_or("documento BSON com tamanho impossível")?;
    let mut doc = Doc::new();
    let mut cursor = at + 4;
    while cursor < end - 1 {
        let tag = bytes[cursor];
        cursor += 1;
        if tag == 0 {
            break;
        }
        let (key, next) = read_cstring(bytes, cursor)?;
        cursor = next;
        let (value, next) = read_value(bytes, cursor, tag)?;
        cursor = next;
        doc.0.push((key, value));
    }
    Ok((doc, end))
}

fn read_value(bytes: &[u8], at: usize, tag: u8) -> Result<(Value, usize), String> {
    Ok(match tag {
        0x01 => (Value::Double(read_f64(bytes, at)?), at + 8),
        0x02 | 0x0e => {
            let length = read_i32(bytes, at)? as usize;
            let end = at + 4 + length.max(1) - 1;
            let text = slice(bytes, at + 4, end)?;
            (
                Value::Text(String::from_utf8_lossy(text).into_owned()),
                at + 4 + length,
            )
        }
        0x03 => {
            let (doc, end) = read_doc(bytes, at)?;
            (Value::Doc(doc), end)
        }
        0x04 => {
            let (doc, end) = read_doc(bytes, at)?;
            (
                Value::List(doc.0.into_iter().map(|(_, value)| value).collect()),
                end,
            )
        }
        0x05 => {
            let length = read_i32(bytes, at)? as usize;
            let subtype = *bytes.get(at + 4).ok_or("binário truncado")?;
            let end = at + 5 + length;
            (
                Value::Binary(subtype, slice(bytes, at + 5, end)?.to_vec()),
                end,
            )
        }
        0x07 => {
            let mut id = [0u8; 12];
            id.copy_from_slice(slice(bytes, at, at + 12)?);
            (Value::ObjectId(id), at + 12)
        }
        0x08 => (
            Value::Bool(*bytes.get(at).ok_or("booleano truncado")? != 0),
            at + 1,
        ),
        0x09 => (Value::Time(read_i64(bytes, at)?), at + 8),
        0x0a => (Value::Null, at),
        0x06 | 0xff | 0x7f => (Value::Null, at),
        0x0b => {
            let (_, next) = read_cstring(bytes, at)?;
            let (_, next) = read_cstring(bytes, next)?;
            (Value::Other("regex"), next)
        }
        0x0c => {
            let length = read_i32(bytes, at)? as usize;
            (Value::Other("dbpointer"), at + 4 + length + 12)
        }
        0x0d => {
            let length = read_i32(bytes, at)? as usize;
            (Value::Other("javascript"), at + 4 + length)
        }
        0x0f => {
            let length = read_i32(bytes, at)? as usize;
            (Value::Other("código com escopo"), at + length)
        }
        0x10 => (Value::Int32(read_i32(bytes, at)?), at + 4),
        // A BSON "timestamp" is an internal replication counter, not a date: the high
        // half is seconds since the epoch, and it stays a number rather than becoming a
        // `Time`, which would then be rendered as if it were milliseconds.
        0x11 => (Value::Int64(read_i64(bytes, at)? >> 32), at + 8),
        0x12 => (Value::Int64(read_i64(bytes, at)?), at + 8),
        0x13 => (Value::Other("decimal128"), at + 16),
        other => return Err(format!("tipo BSON desconhecido: 0x{other:02x}")),
    })
}

fn slice(bytes: &[u8], from: usize, to: usize) -> Result<&[u8], String> {
    bytes
        .get(from..to)
        .ok_or_else(|| "documento BSON truncado".to_string())
}

fn read_cstring(bytes: &[u8], at: usize) -> Result<(String, usize), String> {
    let end = bytes[at.min(bytes.len())..]
        .iter()
        .position(|byte| *byte == 0)
        .map(|offset| at + offset)
        .ok_or("chave BSON sem terminador")?;
    Ok((
        String::from_utf8_lossy(&bytes[at..end]).into_owned(),
        end + 1,
    ))
}

fn read_i32(bytes: &[u8], at: usize) -> Result<i32, String> {
    Ok(i32::from_le_bytes(
        slice(bytes, at, at + 4)?.try_into().unwrap_or([0; 4]),
    ))
}

fn read_i64(bytes: &[u8], at: usize) -> Result<i64, String> {
    Ok(i64::from_le_bytes(
        slice(bytes, at, at + 8)?.try_into().unwrap_or([0; 8]),
    ))
}

fn read_f64(bytes: &[u8], at: usize) -> Result<f64, String> {
    Ok(f64::from_le_bytes(
        slice(bytes, at, at + 8)?.try_into().unwrap_or([0; 8]),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documento_vai_e_volta() {
        let doc = Doc::new()
            .with("find", "pedido")
            .with("limit", 10)
            .with("batch", 200i64)
            .with("taxa", 1.5)
            .with("ok", true)
            .with("filtro", Doc::new().with("status", "pago"))
            .with("chaves", vec![Value::Text("a".into()), Value::Int32(2)])
            .with("carga", Value::Binary(0, b"n,,user".to_vec()));
        let lido = decode(&doc.encode()).unwrap();

        assert_eq!(lido.text("find"), "pedido");
        assert_eq!(lido.int("limit"), 10);
        assert_eq!(lido.int("batch"), 200);
        assert!((lido.num("taxa") - 1.5).abs() < f64::EPSILON);
        assert!(lido.flag("ok"));
        assert_eq!(lido.text("filtro.status"), "pago");
        assert_eq!(lido.list("chaves").len(), 2);
        assert!(matches!(lido.path("carga"), Some(Value::Binary(0, _))));
        // A ordem é parte do formato: o nome do comando é a primeira chave, e um servidor
        // que recebesse outra coisa ali recusaria o documento inteiro.
        assert_eq!(lido.keys().next(), Some("find"));
    }

    #[test]
    fn o_que_nao_existe_le_como_vazio() {
        let doc = decode(&Doc::new().with("a", 1).encode()).unwrap();
        assert_eq!(doc.num("nao.existe"), 0.0);
        assert_eq!(doc.text("nem.isso"), "");
        assert!(doc.list("nada").is_empty());
        assert!(!doc.flag("tampouco"));
    }

    /// Um documento cortado no meio não pode virar pânico: ele chega pela rede, e a rede
    /// corta.
    #[test]
    fn documento_truncado_devolve_erro() {
        let inteiro = Doc::new().with("find", "pedido").with("limit", 10).encode();
        for corte in 1..inteiro.len() {
            let _ = decode(&inteiro[..corte]);
        }
        assert!(decode(&[]).is_err());
        assert!(decode(&[255, 255, 255, 127, 0]).is_err());
    }
}
