//! Datas, sem dependência nova.
//!
//! A aba precisa saber que dia é hoje (o carimbo do patrimônio), em que mês uma venda
//! caiu (o imposto), e se a bolsa está aberta (a cadência). Nada disso cabe em
//! `std::time`, que só sabe contar segundos.
//!
//! O algoritmo civil↔dias é o de Howard Hinnant, que é o que praticamente toda
//! biblioteca de data usa por baixo. Ele é aritmética pura, vale para o calendário
//! gregoriano proléptico inteiro, e cabe em vinte linhas — o que o torna preferível a
//! trazer uma dependência para responder «que dia é hoje».

/// Fuso de Brasília. Fixo em −3: o horário de verão brasileiro acabou em 2019, e
/// enquanto não voltar um número é mais honesto que uma tabela desatualizada.
pub const BRT_OFFSET: i64 = -3 * 3600;

#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
pub struct Data {
    pub ano: i32,
    pub mes: u32,
    pub dia: u32,
}

/// Dias desde a época a partir de ano/mês/dia. Hinnant, `days_from_civil`.
pub fn dias_de(ano: i32, mes: u32, dia: u32) -> i64 {
    let y = if mes <= 2 { ano - 1 } else { ano } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = mes as i64;
    let d = dia as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// O inverso. Hinnant, `civil_from_days`.
pub fn data_de_dias(dias: i64) -> Data {
    let z = dias + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    Data {
        ano: (if m <= 2 { y + 1 } else { y }) as i32,
        mes: m as u32,
        dia: d as u32,
    }
}

/// O epoch de uma data ISO 8601 — `2026-08-10T03:00:00.000Z`.
///
/// Mora aqui e não no leitor de RSS porque data não é assunto de RSS: a API da Kinvo
/// carimba as séries no mesmo formato, e duas cópias desta função seriam duas chances de
/// só uma delas ser corrigida.
///
/// O fuso do texto é ignorado — o `Z` e um `+03:00` dão o mesmo número. É o suficiente
/// para ordenar notícias e para datar velas diárias, que é tudo o que se pede dela.
pub fn epoch_de_iso(texto: &str) -> Option<u64> {
    let (data, resto) = texto.split_once('T')?;
    let mut d = data.split('-');
    let ano: i32 = d.next()?.parse().ok()?;
    let mes: u32 = d.next()?.parse().ok()?;
    let dia: u32 = d.next()?.parse().ok()?;
    let hora = resto.trim_end_matches('Z');
    let mut h = hora.split(':');
    let hh: i64 = h.next()?.parse().ok()?;
    let mm: i64 = h.next().unwrap_or("0").parse().unwrap_or(0);
    let ss: i64 = h
        .next()
        .unwrap_or("0")
        .split(['.', '+', '-'])
        .next()?
        .parse()
        .unwrap_or(0);
    let dias = dias_de(ano, mes, dia);
    Some((dias * 86400 + hh * 3600 + mm * 60 + ss).max(0) as u64)
}

/// A hora e o minuto de um epoch, no fuso pedido.
///
/// Fica fora de `Data` de propósito: `Data` é um dia do calendário, e a maior parte deste
/// programa quer só o dia. Quem precisa da hora pede a hora.
pub fn hora_minuto(epoch: u64, offset: i64) -> (u32, u32) {
    let s = (epoch as i64 + offset).rem_euclid(86400);
    ((s / 3600) as u32, (s % 3600 / 60) as u32)
}

/// `08/09 21:43` — dia e hora, sem o ano. Para uma lista em que tudo é recente e o ano
/// só ocuparia espaço.
pub fn dia_e_hora(epoch: u64, offset: i64) -> String {
    let d = Data::de_epoch(epoch, offset);
    let (h, m) = hora_minuto(epoch, offset);
    format!("{:02}/{:02} {h:02}:{m:02}", d.dia, d.mes)
}

impl Data {
    /// A data de um epoch, no fuso pedido.
    pub fn de_epoch(epoch: u64, offset: i64) -> Data {
        data_de_dias((epoch as i64 + offset).div_euclid(86400))
    }

    pub fn epoch_inicio(&self, offset: i64) -> u64 {
        (dias_de(self.ano, self.mes, self.dia) * 86400 - offset).max(0) as u64
    }

    /// 0 = domingo. A época caiu numa quinta-feira, daí o `+4`.
    pub fn dia_da_semana(&self) -> u32 {
        (dias_de(self.ano, self.mes, self.dia) + 4).rem_euclid(7) as u32
    }

    pub fn fim_de_semana(&self) -> bool {
        matches!(self.dia_da_semana(), 0 | 6)
    }

    pub fn longa(&self) -> String {
        format!("{:02}/{:02}/{}", self.dia, self.mes, self.ano)
    }

    /// `2026-09` — a chave de um mês, que ordena alfabeticamente na ordem certa.
    pub fn chave_mes(&self) -> String {
        format!("{}-{:02}", self.ano, self.mes)
    }

    pub fn mes_anterior(&self) -> Data {
        match self.mes {
            1 => Data {
                ano: self.ano - 1,
                mes: 12,
                dia: 1,
            },
            m => Data {
                ano: self.ano,
                mes: m - 1,
                dia: 1,
            },
        }
    }

    pub fn mes_seguinte(&self) -> Data {
        match self.mes {
            12 => Data {
                ano: self.ano + 1,
                mes: 1,
                dia: 1,
            },
            m => Data {
                ano: self.ano,
                mes: m + 1,
                dia: 1,
            },
        }
    }
}

/// A semana passada, de **segunda a sexta**.
///
/// Período de calendário fechado, e não uma janela de sete dias para trás: «a performance
/// da semana passada» é uma coisa que já aconteceu e não muda mais, enquanto uma janela
/// móvel dá um número diferente a cada dia e nunca fecha. Sexta e não domingo porque o que
/// se está medindo é pregão — sábado e domingo não têm preço.
pub fn semana_anterior(hoje: &Data) -> (Data, Data) {
    let dias = dias_de(hoje.ano, hoje.mes, hoje.dia);
    // 0 = domingo. A segunda desta semana fica a `(dia_da_semana + 6) % 7` dias atrás.
    //
    // A conta é a da ISO 8601: o domingo pertence à semana que **começou na segunda
    // anterior**, e não abre a semana seguinte. É o que faz «a semana passada», olhada num
    // domingo, ser a de sete dias antes — e não a que acabou na véspera.
    let recuo = (hoje.dia_da_semana() + 6) % 7;
    let segunda_desta = dias - recuo as i64;
    (
        data_de_dias(segunda_desta - 7),
        data_de_dias(segunda_desta - 3),
    )
}

/// O mês passado inteiro, do dia 1 ao último.
pub fn mes_passado(hoje: &Data) -> (Data, Data) {
    let primeiro = hoje.mes_anterior();
    let comeco_deste = Data {
        ano: hoje.ano,
        mes: hoje.mes,
        dia: 1,
    };
    // O último dia do mês passado é a véspera do dia 1 deste — o que dispensa saber
    // quantos dias tem cada mês e acerta fevereiro bissexto sem tabela nenhuma.
    let ultimo = data_de_dias(dias_de(comeco_deste.ano, comeco_deste.mes, 1) - 1);
    (primeiro, ultimo)
}

pub const MESES: [&str; 12] = [
    "janeiro",
    "fevereiro",
    "março",
    "abril",
    "maio",
    "junho",
    "julho",
    "agosto",
    "setembro",
    "outubro",
    "novembro",
    "dezembro",
];

pub fn nome_mes(mes: u32) -> &'static str {
    MESES[((mes.max(1) - 1) as usize).min(11)]
}

/// Hora do dia (0–23) no fuso pedido.
pub fn hora(epoch: u64, offset: i64) -> u32 {
    (((epoch as i64 + offset).rem_euclid(86400)) / 3600) as u32
}

/// Quanto tempo passou, em palavras curtas: «há 4 min», «há 3 d».
///
/// Existe porque toda cotação desta aba aparece com a idade ao lado. Um número de quatro
/// minutos atrás é útil; um número de quatro minutos atrás apresentado como agora não é.
pub fn idade(segundos: u64) -> String {
    match segundos {
        // O corte é em 60 e não em 45: entre os dois, a divisão por 60 dava zero e a
        // tela escrevia «há 0 min», que não é uma quantidade de tempo.
        0..60 => "agora".to_string(),
        s if s < 3600 => format!("há {} min", s / 60),
        s if s < 86400 => format!("há {} h", s / 3600),
        s if s < 86400 * 30 => format!("há {} d", s / 86400),
        s => format!("há {} m", s / (86400 * 30)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_epoca_e_uma_quinta_feira_de_1970() {
        let d = Data::de_epoch(0, 0);
        assert_eq!((d.ano, d.mes, d.dia), (1970, 1, 1));
        assert_eq!(d.dia_da_semana(), 4);
    }

    #[test]
    fn ida_e_volta_bate_em_datas_dificeis() {
        // Bissexto secular, virada de ano, 29 de fevereiro.
        for (a, m, d) in [
            (2000, 2, 29),
            (1900, 3, 1),
            (2024, 2, 29),
            (2026, 12, 31),
            (2027, 1, 1),
            (1970, 1, 1),
        ] {
            let dias = dias_de(a, m, d);
            let volta = data_de_dias(dias);
            assert_eq!((volta.ano, volta.mes, volta.dia), (a, m, d), "{a}-{m}-{d}");
        }
    }

    #[test]
    fn fuso_de_brasilia_muda_o_dia_na_virada() {
        // 2026-09-08T02:00:00Z ainda é dia 7 em Brasília.
        let epoch = (dias_de(2026, 9, 8) * 86400 + 2 * 3600) as u64;
        assert_eq!(Data::de_epoch(epoch, 0).dia, 8);
        assert_eq!(Data::de_epoch(epoch, BRT_OFFSET).dia, 7);
    }

    #[test]
    fn mes_anterior_e_seguinte_viram_o_ano() {
        let jan = Data {
            ano: 2026,
            mes: 1,
            dia: 15,
        };
        assert_eq!(jan.mes_anterior().mes, 12);
        assert_eq!(jan.mes_anterior().ano, 2025);
        let dez = Data {
            ano: 2026,
            mes: 12,
            dia: 15,
        };
        assert_eq!(dez.mes_seguinte().mes, 1);
        assert_eq!(dez.mes_seguinte().ano, 2027);
    }

    #[test]
    fn chave_de_mes_ordena_alfabeticamente_na_ordem_certa() {
        let mut chaves = vec![
            Data {
                ano: 2026,
                mes: 10,
                dia: 1,
            }
            .chave_mes(),
            Data {
                ano: 2026,
                mes: 2,
                dia: 1,
            }
            .chave_mes(),
            Data {
                ano: 2025,
                mes: 12,
                dia: 1,
            }
            .chave_mes(),
        ];
        chaves.sort();
        assert_eq!(chaves, vec!["2025-12", "2026-02", "2026-10"]);
    }

    #[test]
    fn fim_de_semana_e_reconhecido() {
        // 2026-09-05 é sábado, 06 domingo, 07 segunda.
        assert!(
            Data {
                ano: 2026,
                mes: 9,
                dia: 5
            }
            .fim_de_semana()
        );
        assert!(
            Data {
                ano: 2026,
                mes: 9,
                dia: 6
            }
            .fim_de_semana()
        );
        assert!(
            !Data {
                ano: 2026,
                mes: 9,
                dia: 7
            }
            .fim_de_semana()
        );
    }

    #[test]
    fn idade_em_palavras() {
        assert_eq!(idade(10), "agora");
        assert_eq!(idade(240), "há 4 min");
        assert_eq!(idade(7200), "há 2 h");
        assert_eq!(idade(86400 * 3), "há 3 d");
    }

    #[test]
    fn nunca_escreve_uma_quantidade_de_tempo_igual_a_zero() {
        // «há 0 min» não é uma quantidade de tempo. A faixa de 45 a 59 segundos caía nela.
        for s in 0..7200 {
            let texto = idade(s);
            assert!(!texto.starts_with("há 0"), "idade({s}) deu «{texto}»");
        }
    }

    #[test]
    fn a_hora_sai_no_fuso_pedido_e_nao_em_utc() {
        // 2026-09-08T22:00:00Z. Em Brasília isso é dia 8, às 19h — e é exatamente esse
        // deslize (mostrar a hora em UTC) que faria a lista de notícias parecer do futuro.
        let epoch = Data {
            ano: 2026,
            mes: 9,
            dia: 8,
        }
        .epoch_inicio(0)
            + 22 * 3600;
        assert_eq!(hora_minuto(epoch, 0), (22, 0));
        assert_eq!(hora_minuto(epoch, BRT_OFFSET), (19, 0));
        assert_eq!(dia_e_hora(epoch, BRT_OFFSET), "08/09 19:00");
    }

    #[test]
    fn a_virada_do_dia_para_tras_nao_estoura() {
        // 00:30 UTC vira 21:30 do dia anterior em Brasília. `rem_euclid` existe por isso.
        let epoch = Data {
            ano: 2026,
            mes: 9,
            dia: 8,
        }
        .epoch_inicio(0)
            + 30 * 60;
        assert_eq!(dia_e_hora(epoch, BRT_OFFSET), "07/09 21:30");
    }

    #[test]
    fn a_semana_anterior_vai_de_segunda_a_sexta() {
        // 08/09/2026 é uma terça. A semana passada foi 31/08 (seg) a 04/09 (sex).
        let (de, ate) = semana_anterior(&Data {
            ano: 2026,
            mes: 9,
            dia: 8,
        });
        assert_eq!((de.dia, de.mes), (31, 8));
        assert_eq!((ate.dia, ate.mes), (4, 9));
        assert_eq!(de.dia_da_semana(), 1, "começa numa segunda");
        assert_eq!(ate.dia_da_semana(), 5, "termina numa sexta");
    }

    #[test]
    fn na_segunda_a_semana_passada_e_a_que_acabou_de_terminar() {
        // O caso que erra por um: numa segunda, «a semana passada» não pode ser a de hoje.
        let segunda = Data {
            ano: 2026,
            mes: 9,
            dia: 7,
        };
        assert_eq!(segunda.dia_da_semana(), 1);
        let (de, ate) = semana_anterior(&segunda);
        assert_eq!((de.dia, de.mes), (31, 8));
        assert_eq!((ate.dia, ate.mes), (4, 9));
    }

    #[test]
    fn o_domingo_pertence_a_semana_que_comecou_na_segunda_anterior() {
        // Regra da ISO 8601. Domingo 06/09 está na semana que abriu em 31/08, então a
        // semana passada, olhada dali, é 24/08 a 28/08. Sem o `+6 % 7` o domingo abriria
        // uma semana nova e a resposta erraria por sete dias.
        let domingo = Data {
            ano: 2026,
            mes: 9,
            dia: 6,
        };
        assert_eq!(domingo.dia_da_semana(), 0);
        let (de, ate) = semana_anterior(&domingo);
        assert_eq!((de.dia, de.mes), (24, 8));
        assert_eq!((ate.dia, ate.mes), (28, 8));
    }

    #[test]
    fn a_semana_anterior_atravessa_o_ano() {
        let (de, ate) = semana_anterior(&Data {
            ano: 2027,
            mes: 1,
            dia: 5,
        });
        // A semana passada atravessa a virada: 28/12/2026 a 01/01/2027.
        assert_eq!((de.ano, de.mes, de.dia), (2026, 12, 28));
        assert_eq!((ate.ano, ate.mes, ate.dia), (2027, 1, 1));
    }

    #[test]
    fn o_mes_passado_vai_do_dia_um_ao_ultimo() {
        let (de, ate) = mes_passado(&Data {
            ano: 2026,
            mes: 9,
            dia: 8,
        });
        assert_eq!((de.ano, de.mes, de.dia), (2026, 8, 1));
        assert_eq!((ate.ano, ate.mes, ate.dia), (2026, 8, 31));
    }

    #[test]
    fn o_mes_passado_acerta_fevereiro_e_a_virada_do_ano() {
        // Sem tabela de dias por mês: o último dia é a véspera do dia 1 deste.
        let (_, ate) = mes_passado(&Data {
            ano: 2028,
            mes: 3,
            dia: 10,
        });
        assert_eq!((ate.mes, ate.dia), (2, 29), "2028 é bissexto");
        let (de, ate) = mes_passado(&Data {
            ano: 2027,
            mes: 1,
            dia: 15,
        });
        assert_eq!((de.ano, de.mes, de.dia), (2026, 12, 1));
        assert_eq!((ate.ano, ate.mes, ate.dia), (2026, 12, 31));
    }
}
