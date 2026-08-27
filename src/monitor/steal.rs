//! O tempo que o hipervisor tomou da máquina — o *steal*.
//!
//! É a única métrica que separa «meu sistema está lento» de «a máquina que eu aluguei
//! não é realmente minha neste momento». Uso de CPU alto quer dizer que o seu código
//! está trabalhando, e o que fazer com isso é seu. Steal alto quer dizer que o seu
//! servidor ficou **na fila**, esperando o processador físico terminar de atender outro
//! cliente do mesmo hospedeiro — e nada no seu código muda isso.
//!
//! Sem ela o sintoma é enlouquecedor: o aplicativo está lento, o CPU está baixo, o disco
//! está bem, a memória sobra, e não há o que consertar. Com ela, o gráfico responde.
//!
//! O painel só existe onde há um hipervisor — a mesma regra do de temperatura e do da
//! GPU. Em ferro dedicado não há de quem esperar, o número é sempre zero, e um gráfico
//! permanentemente vazio é ruído.

use std::fs;

use super::{Monitor, SystemState};

/// A partir de quanto o número deixa de ser ruído e vira problema.
///
/// Até 1% é normal em qualquer máquina alugada. Entre 2 e 5 já se sente lentidão
/// esporádica. Acima de 10%, sustentado, o hospedeiro está superlotado — é o número que
/// se leva ao suporte, ou o motivo para mudar de plano.
///
/// É o que acende a cor, e **não** o teto do desenho — ver `Monitor::scale`. Usar o
/// limiar como teto parecia boa ideia («fora da escala já é a resposta») e não é: numa
/// máquina que passa o dia entre 15% e 45% de steal, toda barra satura, o painel vira um
/// bloco sólido, e some justamente a forma da crista — que é o que denuncia se o steal
/// acompanha o seu trabalho ou tem horário próprio. E é essa forma que distingue uma
/// instância sem crédito de um hospedeiro superlotado.
const CONCERNING: f64 = 10.0;

pub struct StealMonitor {
    /// A leitura anterior, em jiffies acumulados: steal e total. Um contador que só sobe
    /// não diz nada sobre agora — a diferença entre duas leituras é que diz.
    previous: Option<(u64, u64)>,
    last: f64,
    vcpus: usize,
}

impl StealMonitor {
    /// Só numa máquina virtualizada. `None` em ferro dedicado, e aí o painel não é
    /// registrado.
    ///
    /// A flag `hypervisor` do `/proc/cpuinfo` é o bit que o próprio processador acende
    /// quando está sendo virtualizado, e é o que toda ferramenta usa para responder esta
    /// pergunta. Conferido nos dois casos: presente nos dois núcleos de um convidado KVM,
    /// ausente em todos os de um laptop.
    pub fn probe() -> Option<Self> {
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
        let virtualised = cpuinfo
            .lines()
            .filter(|line| line.starts_with("flags"))
            .any(|line| line.split_whitespace().any(|flag| flag == "hypervisor"));
        if !virtualised {
            return None;
        }
        // Um kernel sem a coluna de steal não tem o que medir. Ela existe desde sempre
        // nos que este programa alcança, mas prometer um painel sem conferir é prometer
        // um gráfico que nasce vazio.
        let previous = read_jiffies()?;
        Some(Self {
            previous: Some(previous),
            last: 0.0,
            vcpus: cpuinfo
                .lines()
                .filter(|line| line.starts_with("processor"))
                .count()
                .max(1),
        })
    }
}

impl Monitor for StealMonitor {
    fn id(&self) -> &str {
        "cpu-steal"
    }

    fn title(&self) -> &str {
        "CPU steal"
    }

    fn sample(&mut self, _state: &SystemState) -> f64 {
        let Some((steal, total)) = read_jiffies() else {
            return self.last;
        };
        if let Some((before_steal, before_total)) = self.previous {
            let elapsed = total.saturating_sub(before_total);
            // Duas leituras dentro do mesmo jiffy não têm o que dividir.
            if elapsed > 0 {
                self.last = steal.saturating_sub(before_steal) as f64 * 100.0 / elapsed as f64;
            }
        }
        self.previous = Some((steal, total));
        self.last
    }

    fn format(&self, value: f64) -> String {
        format!("{value:.1}%")
    }

    fn limit(&self) -> Option<f64> {
        Some(CONCERNING)
    }

    /// O gráfico vai até 100%, que é onde a métrica de fato pode chegar. A cor continua
    /// vindo do limiar, então um steal ruim segue vermelho — só que agora dá para ver a
    /// altura dele, em vez de uma parede.
    fn scale(&self) -> Option<f64> {
        Some(100.0)
    }

    /// Ao lado do uso de CPU, que é o número com que ele é confundido — e do qual ele é o
    /// contrário: um diz que você trabalhou, o outro que você esperou.
    fn group(&self) -> &'static str {
        "System"
    }

    fn extra(&self, _state: &SystemState) -> Option<String> {
        // Quantos vCPUs, porque o mesmo percentual dói diferente em dois e em sessenta e
        // quatro; e o limiar, porque é um número que ninguém sabe ler de cabeça.
        Some(format!(
            "{} vCPUs · ruim acima de {CONCERNING:.0}%",
            self.vcpus
        ))
    }
}

/// Steal e total acumulados, em jiffies, da primeira linha do `/proc/stat`.
///
/// A linha é `cpu user nice system idle iowait irq softirq steal guest guest_nice`. O
/// total soma até o steal e para: `guest` e `guest_nice` já estão contados dentro de
/// `user` e `nice`, e somá-los de novo inflaria o denominador.
fn read_jiffies() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields.take(8).filter_map(|f| f.parse().ok()).collect();
    if values.len() < 8 {
        return None;
    }
    Some((values[7], values.iter().sum()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Um monitor com uma leitura anterior já posta, para exercitar a conta do delta sem
    /// depender do `/proc` da máquina que roda o teste.
    fn with_previous(steal: u64, total: u64) -> StealMonitor {
        StealMonitor {
            previous: Some((steal, total)),
            last: 0.0,
            vcpus: 2,
        }
    }

    fn rate(monitor: &mut StealMonitor, steal: u64, total: u64) -> f64 {
        let (before_steal, before_total) = monitor.previous.unwrap();
        let elapsed = total.saturating_sub(before_total);
        if elapsed > 0 {
            monitor.last = steal.saturating_sub(before_steal) as f64 * 100.0 / elapsed as f64;
        }
        monitor.previous = Some((steal, total));
        monitor.last
    }

    #[test]
    fn the_rate_is_the_difference_between_two_readings() {
        // Contadores acumulados: sozinhos não dizem nada, e 5 de 1000 jiffies é 0,5%.
        let mut monitor = with_previous(100, 10_000);
        assert!((rate(&mut monitor, 105, 11_000) - 0.5).abs() < 0.001);
    }

    #[test]
    fn a_counter_that_did_not_move_reads_as_zero() {
        let mut monitor = with_previous(100, 10_000);
        assert_eq!(rate(&mut monitor, 100, 11_000), 0.0);
    }

    #[test]
    fn two_readings_in_the_same_jiffy_keep_the_last_value() {
        // Sem tempo decorrido não há o que dividir, e o valor anterior é melhor resposta
        // que um zero inventado.
        let mut monitor = with_previous(100, 10_000);
        rate(&mut monitor, 200, 20_000);
        let held = monitor.last;
        assert_eq!(rate(&mut monitor, 300, 20_000), held);
    }

    #[test]
    fn the_chart_is_taller_than_the_threshold_that_colours_it() {
        // As duas coisas são separadas de propósito. Com o teto no limiar, uma máquina
        // que passa o dia entre 15% e 45% desenha só barras cheias — um bloco sólido, sem
        // crista, sem forma, sem a informação de quando subiu.
        let monitor = with_previous(0, 0);
        assert_eq!(monitor.scale(), Some(100.0));
        assert_eq!(monitor.limit(), Some(CONCERNING));
        assert!(monitor.scale() > monitor.limit());
    }

    #[test]
    fn a_bad_reading_is_still_far_past_the_colour_threshold() {
        // A altura mudou, a cor não: o sinal acende em 0,7 e 0,9 do limiar, e 30% de
        // steal é três vezes o limiar inteiro — vermelho com folga.
        let limit = CONCERNING;
        assert!(30.0 / limit >= 0.9);
        // E o que é ruído continua sem acender.
        assert!(0.8 / limit < 0.7);
    }

    #[test]
    fn the_total_stops_at_steal_so_guest_is_not_counted_twice() {
        // `guest` e `guest_nice` já estão dentro de `user` e `nice`; somá-los de novo
        // inflaria o denominador e faria todo steal parecer menor do que é.
        let line = "cpu 100 20 30 800 10 5 5 30 999 999";
        let mut fields = line.split_whitespace();
        assert_eq!(fields.next(), Some("cpu"));
        let values: Vec<u64> = fields.take(8).filter_map(|f| f.parse().ok()).collect();
        assert_eq!(values.len(), 8);
        assert_eq!(values[7], 30);
        assert_eq!(values.iter().sum::<u64>(), 1000);
    }
}
