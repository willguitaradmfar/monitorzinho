//! Um shell dentro de um container, no terminal que já está aqui.
//!
//! O protocolo é o de sempre: pede-se à engine que crie uma execução com terminal, a
//! requisição que a inicia deixa de ser HTTP no meio (`101 Switching Protocols`), e a
//! partir dali os bytes são do terminal nos dois sentidos, sem envelope. Com terminal
//! ligado o fluxo **não** é multiplexado — saída e erro vêm misturados, que é
//! exatamente o que um terminal é —, então o relay é uma cópia de bytes e nada mais.
//!
//! Nada disto passa por um binário externo. É a mesma decisão do resto do programa:
//! `docker exec` faria isto em uma linha e traria consigo a exigência de que o binário
//! esteja instalado e que a versão dele concorde com a nossa.
//!
//! Enquanto o shell está aberto o laço da interface não roda — ela saiu da tela
//! alternativa e o terminal é do shell. É a única tela do programa que funciona assim, e
//! é por isso que ela é a única que não pode ser desenhada por cima de nada.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use super::http::{self, Conn, Endpoint};

/// Quanto uma leitura do fluxo espera antes de devolver o controle ao laço. Não é
/// latência: uma resposta que chega em 5 ms é lida em 5 ms. É só o quanto o laço demora
/// para reparar que o teclado tem algo a dizer quando o shell está calado.
const READ_SLICE: Duration = Duration::from_millis(50);
/// Quanto o laço espera pelo teclado antes de tentar ler o fluxo de novo.
const KEY_SLICE_MS: i32 = 20;
/// De quanto em quanto tempo o tamanho do terminal é reconferido, para o shell não ficar
/// desenhando numa janela do tamanho errado depois que alguém redimensiona.
const RESIZE_CHECK: Duration = Duration::from_millis(400);

const BUFFER: usize = 16 * 1024;

/// Um shell aberto: o fluxo por onde ele fala, e o que é preciso para redimensioná-lo.
pub struct Session {
    pub stream: Conn,
    pub endpoint: Endpoint,
    /// O id que a engine deu a esta execução, para avisá-la de um redimensionamento.
    pub id: String,
    /// O shell que de fato abriu, para a linha que anuncia a sessão dizer qual foi.
    pub shell: String,
    /// O que o shell já disse antes de a sessão começar — o prompt, tipicamente.
    ///
    /// Foi preciso lê-lo para saber se o shell subiu (ver `DockerEngine::start_shell`), e
    /// o que foi lido não volta para o fluxo. Sem guardá-lo, toda sessão abriria sem o
    /// primeiro prompt.
    pub greeting: Vec<u8>,
}

/// Como a sessão terminou. Nenhuma delas é erro do programa: um shell fecha porque
/// alguém saiu dele.
pub enum Ended {
    /// O shell terminou por conta própria — `exit`, ou o processo morreu.
    Closed,
    /// O terminal deu erro de leitura ou escrita. Traz o que aconteceu.
    Broken(String),
}

/// Copia bytes entre o terminal e o shell até um dos dois acabar.
///
/// Um laço só, sem thread: uma thread lendo o teclado sobreviveria ao fim da sessão
/// bloqueada num `read`, e a primeira tecla que a pessoa apertasse depois de voltar para
/// a interface seria engolida por ela em vez de chegar à tela. Esperar no descritor com
/// `poll(2)` é como todo relay deste programa espera, e não deixa nada para trás.
pub fn relay(session: &mut Session) -> Ended {
    let _ = session.stream.set_read_timeout(READ_SLICE);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // O prompt que já tinha sido lido para decidir se o shell subiu.
    if !session.greeting.is_empty() {
        let _ = out.write_all(&session.greeting);
        let _ = out.flush();
    }
    let mut buffer = [0u8; BUFFER];
    let mut size = crossterm::terminal::size().unwrap_or((80, 24));
    let mut checked = Instant::now();

    loop {
        // Teclado → shell.
        if crate::tools::poll::readable(0, KEY_SLICE_MS) {
            let read = {
                let mut lock = stdin.lock();
                lock.read(&mut buffer)
            };
            match read {
                // O terminal fechou por baixo de nós. Nada mais a enviar.
                Ok(0) => return Ended::Closed,
                Ok(n) => {
                    if let Err(e) = session.stream.write_all(&buffer[..n]) {
                        return Ended::Broken(format!("não consegui enviar ao shell: {e}"));
                    }
                    let _ = session.stream.flush();
                }
                // Redimensionar a janela manda um SIGWINCH, e um sinal interrompe a
                // leitura bloqueada com `EINTR`. Não é falha nenhuma: a próxima volta
                // lê de novo. Tratá-lo como erro fechava o shell toda vez que alguém
                // mexia no tamanho do terminal. O `poll` deste projeto já trata o
                // mesmo caso do mesmo jeito, pela mesma razão.
                Err(e) if interrupted_or_idle(&e) => {}
                Err(e) => return Ended::Broken(format!("não consegui ler o teclado: {e}")),
            }
        }

        // Shell → tela.
        match session.stream.read(&mut buffer) {
            // Fim do fluxo: o shell saiu. É como uma sessão termina, não como ela falha.
            Ok(0) => return Ended::Closed,
            Ok(n) => {
                if out.write_all(&buffer[..n]).is_err() {
                    return Ended::Closed;
                }
                let _ = out.flush();
            }
            // O prazo da fatia expirou sem nada para ler — o shell está calado —, ou um
            // sinal interrompeu a leitura. Nos dois casos o laço volta a olhar o teclado.
            Err(e) if interrupted_or_idle(&e) => {}
            Err(e) => return Ended::Broken(format!("o shell fechou: {e}")),
        }

        // Redimensionamento. Sem sinal e sem handler: conferir o tamanho é um ioctl, e
        // duas vezes por segundo custa menos que instalar um tratador de sinal num
        // programa que já tem outro dono para o terminal.
        if checked.elapsed() >= RESIZE_CHECK {
            checked = Instant::now();
            if let Ok(current) = crossterm::terminal::size()
                && current != size
            {
                size = current;
                resize(session, size);
            }
        }
    }
}

/// Se um erro de leitura quer dizer «ainda não» em vez de «acabou».
///
/// `WouldBlock` e `TimedOut` são o prazo da fatia expirando, e as duas formas dependem de
/// o fluxo ser um socket ou uma sessão TLS por cima de um. `Interrupted` é um sinal —
/// `SIGWINCH`, tipicamente, que é o que chega quando alguém redimensiona a janela.
fn interrupted_or_idle(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    )
}

/// Avisa a engine do novo tamanho da janela. Numa conexão à parte, porque a desta sessão
/// já não fala HTTP. Falhar aqui não derruba nada: o shell continua, desenhando numa
/// janela do tamanho antigo até a próxima tentativa.
pub fn resize(session: &Session, (cols, rows): (u16, u16)) {
    let _ = http::request(
        &session.endpoint,
        "POST",
        &format!("/v1.44/exec/{}/resize?h={rows}&w={cols}", session.id),
        Some("{}"),
        Duration::from_secs(2),
    );
}
