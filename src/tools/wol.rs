//! Waking a machine that is switched off, by the one thing its network card still
//! listens for.
//!
//! A powered-down machine with Wake-on-LAN enabled keeps its card alive watching for a
//! single pattern: six bytes of `FF` followed by its own MAC address repeated sixteen
//! times, anywhere inside any packet. That is the whole protocol. It is sent to the
//! broadcast address because the machine has no IP while it is asleep — there is nobody
//! to address it as.
//!
//! The network scanner already knows the MAC of everything it has seen, which is why
//! waking one is offered straight from its findings.

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use super::{EventKind, Execution, ParamSpec, Recorder, Tool};

/// Sent more than once because it cannot be acknowledged: nothing answers a magic
/// packet, and a card that misses one while negotiating link has no way to ask again.
const REPEATS: usize = 3;
const GAP: Duration = Duration::from_millis(200);

pub struct WolTool;

impl Tool for WolTool {
    fn id(&self) -> &'static str {
        "wol"
    }

    fn name(&self) -> &'static str {
        "Acordar máquina (Wake-on-LAN)"
    }

    fn description(&self) -> &'static str {
        "Manda o pacote mágico para o MAC de uma máquina desligada — o único pacote que a placa dela ainda escuta"
    }

    fn params(&self) -> Vec<ParamSpec> {
        vec![
            ParamSpec::text(
                "mac",
                "MAC",
                "",
                "Endereço da placa, com : ou -. O scanner de rede mostra o de tudo que ele vê",
            ),
            ParamSpec::text(
                "destino",
                "Enviar para",
                "255.255.255.255:9",
                "Broadcast da rede onde a máquina está. Ela não tem IP enquanto dorme, então não há como endereçá-la",
            ),
        ]
    }

    fn summarize(&self, params: &HashMap<&'static str, String>) -> String {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        format!("{} via {}", get("mac"), get("destino"))
    }

    fn on_demand(&self, _params: &HashMap<&'static str, String>) -> bool {
        true
    }

    fn start(&self, id: u64, params: &HashMap<&'static str, String>) -> Result<Execution, String> {
        let plan = Plan::from(params)?;
        let (execution, recorder) = Execution::new(id, self.name(), self.summarize(params));
        recorder.record(
            0,
            EventKind::Note(format!(
                "pronto para acordar {} via {}. Nada é enviado até você abrir",
                plan.mac_text, plan.destination
            )),
        );
        Ok(execution.on_demand())
    }

    fn open(&self, execution: &Execution, params: &HashMap<&'static str, String>) {
        if execution.has_result() || execution.is_working() {
            return;
        }
        self.rerun(execution, params);
    }

    fn rerun(&self, execution: &Execution, params: &HashMap<&'static str, String>) {
        if execution.is_working() {
            return;
        }
        let Ok(plan) = Plan::from(params) else {
            return;
        };
        let recorder = execution.recorder();
        let finished = execution.finish_flag();
        finished.store(false, Ordering::Relaxed);
        thread::spawn(move || {
            wake(plan, &recorder);
            recorder.ran();
            finished.store(true, Ordering::Relaxed);
        });
    }

    fn columns(&self, execution: &Execution) -> (String, String) {
        execution.outcome()
    }
}

struct Plan {
    mac: [u8; 6],
    mac_text: String,
    destination: SocketAddr,
}

impl Plan {
    fn from(params: &HashMap<&'static str, String>) -> Result<Self, String> {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("").trim();
        let mac_text = get("mac").to_string();
        let mac = parse_mac(&mac_text)?;
        let destination = match get("destino") {
            "" => "255.255.255.255:9".to_string(),
            text if text.contains(':') => text.to_string(),
            // Port 9 (discard) is where magic packets conventionally go; 7 also works,
            // and neither is ever listened on — the card matches the pattern, not the port.
            text => format!("{text}:9"),
        };
        let destination = destination
            .to_socket_addrs()
            .map_err(|e| format!("destino inválido ({destination}): {e}"))?
            .next()
            .ok_or_else(|| format!("{destination} não resolveu"))?;
        Ok(Self {
            mac,
            mac_text,
            destination,
        })
    }
}

/// `a1:b2:c3:d4:e5:f6`, or with dashes, or bare hex.
fn parse_mac(text: &str) -> Result<[u8; 6], String> {
    let cleaned: String = text
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    if cleaned.len() != 12 {
        return Err(format!(
            "«{text}» não é um MAC: precisa de 12 dígitos hexadecimais"
        ));
    }
    let mut mac = [0u8; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("«{text}» não é um MAC"))?;
    }
    Ok(mac)
}

fn wake(plan: Plan, rec: &Recorder) {
    let Ok(socket) = UdpSocket::bind(("0.0.0.0", 0)) else {
        rec.record(
            0,
            EventKind::Error("não consegui abrir um socket UDP".to_string()),
        );
        rec.report("falhou", "socket UDP recusado");
        return;
    };
    // Broadcast has to be asked for explicitly; without it the send fails with
    // "permission denied" and the reason is anything but obvious.
    if let Err(e) = socket.set_broadcast(true) {
        rec.record(
            0,
            EventKind::Error(format!("não consegui habilitar broadcast: {e}")),
        );
        rec.report("falhou", "broadcast recusado");
        return;
    }

    // Six 0xFF, then the MAC sixteen times. The card matches this anywhere in any
    // packet, which is why it can be carried by a datagram to a port nobody listens on.
    let mut packet = vec![0xFFu8; 6];
    for _ in 0..16 {
        packet.extend_from_slice(&plan.mac);
    }

    let mut sent = 0;
    for attempt in 1..=REPEATS {
        match socket.send_to(&packet, plan.destination) {
            Ok(bytes) => {
                sent += 1;
                rec.record(
                    0,
                    EventKind::Note(format!(
                        "pacote {attempt}/{REPEATS} enviado para {} ({bytes} bytes)",
                        plan.destination
                    )),
                );
            }
            Err(e) => rec.record(0, EventKind::Error(format!("envio {attempt} falhou: {e}"))),
        }
        if attempt < REPEATS {
            thread::sleep(GAP);
        }
    }

    // Nothing answers a magic packet — there is no acknowledgement in the protocol and
    // the machine is not on the network yet — so this says what it did, not what
    // happened. The scanner next door is how you find out whether it worked.
    rec.record(
        0,
        EventKind::Note(
            "nada responde a um pacote mágico: espere uns segundos e procure a máquina com o scanner de rede"
                .to_string(),
        ),
    );
    rec.report(
        format!("{sent} de {REPEATS} enviados"),
        format!("{} · {}", plan.mac_text, plan.destination),
    );
}
