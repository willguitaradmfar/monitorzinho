//! A temperatura do processador, quando a máquina tem uma para dar.
//!
//! O painel é opcional pelo mesmo motivo que o da GPU: numa máquina sem sensor ele
//! simplesmente não existe, em vez de existir mostrando zero. E «sem sensor» é o caso
//! comum onde este programa mais roda — uma VM na nuvem não recebe os sensores do
//! hospedeiro, porque o driver que os lê fala com registradores do processador físico
//! que o hipervisor não repassa ao convidado.
//!
//! Só sensores que são **comprovadamente** do processador entram: `coretemp` (Intel),
//! `k10temp` e `zenpower` (AMD), `cpu_thermal` (ARM), e a zona `x86_pkg_temp`. O
//! `acpitz`, que é o que costuma sobrar numa VM, fica de fora de propósito: ele é uma
//! zona térmica do ACPI, que às vezes é o processador e às vezes é o gabinete, e um
//! painel chamado «Temperatura CPU» mostrando a temperatura de outra coisa é pior que
//! painel nenhum — é a diferença entre não responder e responder errado com confiança.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Monitor, SystemState};

/// Os chips cujo sensor é o do processador, e nada mais.
const CPU_CHIPS: [&str; 5] = [
    "coretemp",
    "k10temp",
    "zenpower",
    "cpu_thermal",
    "soc_thermal",
];

/// Os rótulos que nomeiam o sensor do pacote inteiro, que é o número que se quer — a
/// temperatura do processador, e não a de um núcleo específico dele.
const PACKAGE_LABELS: [&str; 4] = ["package id 0", "tctl", "tdie", "cpu"];

/// Os tipos de zona térmica que são o processador. Fora desta lista não se adivinha.
const CPU_ZONES: [&str; 3] = ["x86_pkg_temp", "cpu-thermal", "cpu_thermal"];

pub struct TemperatureMonitor {
    input: PathBuf,
    /// Como o sensor se chama, para o painel dizer de onde veio o número.
    source: String,
    /// A partir de quanto o próprio chip se considera em risco. É o teto do gráfico e o
    /// que acende o sinal de cor — melhor que um número escolhido por nós, porque cada
    /// processador tem o seu.
    critical: Option<f64>,
    /// A última leitura boa. Um sensor que para de responder deixa a linha onde ela
    /// estava em vez de despencar para zero, que seria uma temperatura que ninguém tem.
    last: f64,
}

impl TemperatureMonitor {
    /// Procura um sensor de processador. `None` numa máquina que não tem — VM na nuvem,
    /// tipicamente — e aí o painel não é registrado.
    pub fn probe() -> Option<Self> {
        hwmon_sensor().or_else(thermal_zone_sensor)
    }
}

impl Monitor for TemperatureMonitor {
    fn id(&self) -> &str {
        "cpu-temp"
    }

    fn title(&self) -> &str {
        "CPU temp"
    }

    fn sample(&mut self, _state: &SystemState) -> f64 {
        if let Some(value) = millidegrees(&self.input) {
            self.last = value;
        }
        self.last
    }

    fn format(&self, value: f64) -> String {
        format!("{value:.0} °C")
    }

    fn limit(&self) -> Option<f64> {
        self.critical
    }

    /// Junto do CPU e da memória, e registrado logo depois do CPU — é ao lado do uso que
    /// a temperatura quer dizer alguma coisa.
    fn group(&self) -> &'static str {
        "System"
    }

    fn extra(&self, _state: &SystemState) -> Option<String> {
        Some(match self.critical {
            Some(critical) => format!("{} · crítico a {critical:.0} °C", self.source),
            None => self.source.clone(),
        })
    }
}

/// Milésimos de grau, que é como o kernel escreve, em graus.
fn millidegrees(path: &Path) -> Option<f64> {
    let value: f64 = fs::read_to_string(path).ok()?.trim().parse().ok()?;
    // Um sensor desligado responde zero, e zero grau não é uma leitura — é a ausência de
    // uma. Descartar aqui evita um mergulho no gráfico toda vez que isso acontece.
    (value > 0.0).then_some(value / 1000.0)
}

/// O sensor do processador em `/sys/class/hwmon`, que é onde ele está quando existe.
fn hwmon_sensor() -> Option<TemperatureMonitor> {
    for entry in fs::read_dir("/sys/class/hwmon").ok()?.flatten() {
        let chip = entry.path();
        let name = fs::read_to_string(chip.join("name")).unwrap_or_default();
        let name = name.trim().to_ascii_lowercase();
        if !CPU_CHIPS.contains(&name.as_str()) {
            continue;
        }
        if let Some(sensor) = best_sensor(&chip, &name) {
            return Some(sensor);
        }
    }
    None
}

/// Dentro de um chip, o sensor do pacote — e o de menor número quando ele não se
/// identifica, que nos processadores que rotulam é sempre o do pacote.
fn best_sensor(chip: &Path, name: &str) -> Option<TemperatureMonitor> {
    let mut inputs: Vec<PathBuf> = fs::read_dir(chip)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("temp") && n.ends_with("_input"))
        })
        .collect();
    inputs.sort();

    let labelled = inputs.iter().find(|input| {
        let Some(label) = read_label(input) else {
            return false;
        };
        let label = label.to_ascii_lowercase();
        PACKAGE_LABELS
            .iter()
            .any(|wanted| label.starts_with(wanted))
    });
    let input = labelled.or_else(|| inputs.first())?.clone();
    // Confere que ele responde antes de prometer um painel: um arquivo que existe e não
    // lê é um gráfico que nasce vazio.
    millidegrees(&input)?;

    let source = match read_label(&input) {
        Some(label) => format!("{name} · {label}"),
        None => name.to_string(),
    };
    Some(TemperatureMonitor {
        critical: sibling(&input, "crit").or_else(|| sibling(&input, "max")),
        last: millidegrees(&input).unwrap_or(0.0),
        input,
        source,
    })
}

fn read_label(input: &Path) -> Option<String> {
    let label = with_suffix(input, "label")?;
    let text = fs::read_to_string(label).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// O limiar irmão de um `tempN_input`: `tempN_crit`, `tempN_max`.
fn sibling(input: &Path, suffix: &str) -> Option<f64> {
    millidegrees(&with_suffix(input, suffix)?)
}

/// `…/temp1_input` + `crit` → `…/temp1_crit`.
fn with_suffix(input: &Path, suffix: &str) -> Option<PathBuf> {
    let name = input.file_name()?.to_str()?;
    let stem = name.strip_suffix("_input")?;
    Some(input.with_file_name(format!("{stem}_{suffix}")))
}

/// A zona térmica do processador, para as máquinas que não expõem `hwmon` — placas ARM,
/// sobretudo.
fn thermal_zone_sensor() -> Option<TemperatureMonitor> {
    for entry in fs::read_dir("/sys/class/thermal").ok()?.flatten() {
        let zone = entry.path();
        let kind = fs::read_to_string(zone.join("type")).unwrap_or_default();
        let kind = kind.trim().to_ascii_lowercase();
        if !CPU_ZONES.contains(&kind.as_str()) {
            continue;
        }
        let input = zone.join("temp");
        let Some(value) = millidegrees(&input) else {
            continue;
        };
        return Some(TemperatureMonitor {
            input,
            source: kind,
            // Uma zona térmica declara seus limiares em arquivos numerados por tipo; o
            // de desligamento é o que corresponde ao «crítico» do hwmon.
            critical: zone_critical(&zone),
            last: value,
        });
    }
    None
}

/// O ponto de desligamento que a zona declara, se declarar algum.
fn zone_critical(zone: &Path) -> Option<f64> {
    for index in 0..8 {
        // `continue`, e não `?`: um ponto que não existe é um número faltando na
        // sequência, não o fim dela. Sair aqui deixaria de achar o ponto crítico numa
        // zona cuja numeração tem buraco.
        let Ok(kind) = fs::read_to_string(zone.join(format!("trip_point_{index}_type"))) else {
            continue;
        };
        if kind.trim() == "critical" {
            return millidegrees(&zone.join(format!("trip_point_{index}_temp")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("monitorzinho-temp-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, content: &str) {
        let mut file = fs::File::create(dir.join(name)).unwrap();
        file.write_all(content.as_bytes()).unwrap();
    }

    #[test]
    fn prefers_the_package_sensor_over_a_single_core() {
        let dir = scratch("pacote");
        write(&dir, "temp1_input", "45000\n");
        write(&dir, "temp1_label", "Core 0\n");
        write(&dir, "temp2_input", "73000\n");
        write(&dir, "temp2_label", "Package id 0\n");
        write(&dir, "temp2_crit", "100000\n");
        let sensor = best_sensor(&dir, "coretemp").unwrap();
        assert!(sensor.source.contains("Package id 0"));
        assert_eq!(sensor.critical, Some(100.0));
        assert_eq!(sensor.last, 73.0);
    }

    #[test]
    fn falls_back_to_the_first_sensor_when_nothing_is_labelled() {
        let dir = scratch("sem-rotulo");
        write(&dir, "temp1_input", "51000\n");
        write(&dir, "temp2_input", "49000\n");
        let sensor = best_sensor(&dir, "k10temp").unwrap();
        assert_eq!(sensor.last, 51.0);
        assert_eq!(sensor.source, "k10temp");
        // Sem limiar declarado não se inventa um: o gráfico fica sem teto em vez de com
        // um teto errado.
        assert_eq!(sensor.critical, None);
    }

    #[test]
    fn a_sensor_that_reads_zero_is_not_a_sensor() {
        // Zero grau não é uma leitura, é a ausência de uma — e prometer um painel a
        // partir dela seria prometer um gráfico que nasce mentindo.
        let dir = scratch("zero");
        write(&dir, "temp1_input", "0\n");
        assert!(best_sensor(&dir, "coretemp").is_none());
    }

    #[test]
    fn a_gap_in_the_trip_points_does_not_hide_the_critical_one() {
        let dir = scratch("trip");
        // Sem `trip_point_0_*`: a numeração começa no 1, o que acontece de verdade.
        write(&dir, "trip_point_1_type", "passive\n");
        write(&dir, "trip_point_1_temp", "80000\n");
        write(&dir, "trip_point_2_type", "critical\n");
        write(&dir, "trip_point_2_temp", "105000\n");
        assert_eq!(zone_critical(&dir), Some(105.0));
    }

    #[test]
    fn the_critical_threshold_comes_from_the_matching_sensor() {
        let dir = scratch("limiar");
        write(&dir, "temp3_input", "60000\n");
        write(&dir, "temp3_max", "95000\n");
        let sensor = best_sensor(&dir, "coretemp").unwrap();
        // `crit` é preferido, mas na falta dele o `max` do mesmo sensor serve — e não o
        // de um vizinho.
        assert_eq!(sensor.critical, Some(95.0));
    }
}
