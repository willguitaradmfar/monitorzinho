//! Alcançando uma máquina pelo `ssh` que já está instalado aqui.
//!
//! Não existe cliente SSH escrito neste projeto, e não deve existir: o `ssh` lê o
//! `~/.ssh/config`, conversa com o agente, confere o `known_hosts`, atravessa um
//! `ProxyJump` e conhece a chave que a máquina espera. Um cliente escrito aqui seria um
//! segundo `ssh`, pior, que ignoraria tudo isso — é a mesma decisão que o `sshfwd` tomou,
//! pela mesma razão.
//!
//! **Uma conexão por investigação, e um comando só.** O diagnóstico inteiro é um script de
//! shell — `SCRIPT`, logo abaixo, que dá para ler de ponta a ponta — mandado de uma vez e
//! respondido de uma vez. Trinta comandos em trinta `ssh` seriam trinta handshakes e trinta
//! registros de login no servidor de alguém.
//!
//! **Nada ali escreve.** Nenhum `>`, nenhum `rm`, nenhum `systemctl` que não seja
//! `list`/`status`, nenhum gerenciador de pacote que não seja consulta. O script é a
//! prova: está inteiro num lugar só, em texto, e pode ser lido antes de rodar.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::Plan;

/// O separador entre as seções da saída. Um caractere que nenhum comando devolve por
/// conta própria — `\x1e` é literalmente «separador de registro» no ASCII, e existe desde
/// 1963 para isto.
const MARCA: char = '\u{1e}';

/// A variável que carrega a senha até o `ssh`, e o sinal de que este processo foi
/// chamado para ser o programa que digita a senha. Ver `Target::run`.
pub const SENHA_ENV: &str = "MONITORZINHO_SSH_ASKPASS";

/// Quanto tempo a máquina tem para responder o diagnóstico inteiro.
const TIMEOUT: Duration = Duration::from_secs(90);

/// Onde conectar e como se identificar.
pub struct Target {
    pub host: String,
    pub user: String,
    pub port: String,
    pub key: Option<PathBuf>,
    /// Vazia quer dizer «use o agente e as chaves do ~/.ssh», que é o caminho normal.
    pub password: String,
    ssh: PathBuf,
}

impl Target {
    pub fn parse(plan: &Plan) -> Result<Self, String> {
        let bruto = plan.url.trim();
        if bruto.is_empty() {
            return Err("informe a máquina".to_string());
        }
        // `usuario@host` no campo do host é como todo mundo escreve, e recusar seria
        // pedir para a pessoa desmontar o que ela já tem na mão.
        let (user, host) = match bruto.split_once('@') {
            Some((user, host)) => (user.to_string(), host.to_string()),
            None => (plan.usuario.trim().to_string(), bruto.to_string()),
        };
        if host.is_empty() {
            return Err("informe a máquina".to_string());
        }
        if !plan.porta.trim().is_empty() && plan.porta.trim().parse::<u16>().is_err() {
            return Err(format!("porta do SSH inválida: {}", plan.porta));
        }
        let key = match plan.chave.trim() {
            "" => None,
            caminho => {
                let caminho = expandir(caminho);
                if !caminho.exists() {
                    return Err(format!("não achei a chave {}", caminho.display()));
                }
                Some(caminho)
            }
        };
        Ok(Self {
            host,
            user,
            port: plan.porta.trim().to_string(),
            key,
            password: plan.senha.clone(),
            ssh: ssh_binary().ok_or_else(|| "não achei o comando ssh no PATH".to_string())?,
        })
    }

    /// Quem e onde, nunca a senha.
    pub fn summary(&self) -> String {
        let porta = match self.port.as_str() {
            "" => String::new(),
            porta => format!(":{porta}"),
        };
        match self.user.as_str() {
            "" => format!("{}{porta}", self.host),
            user => format!("{user}@{}{porta}", self.host),
        }
    }

    fn argv(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![
            "-o".to_string(),
            "ConnectTimeout=10".to_string(),
            // Não perguntar nada é o padrão aqui: um prompt atrás da tela cheia é um
            // travamento sem causa visível. Com senha, quem responde é o askpass — ver
            // `run` — e continua sem prompt nenhum na tela.
            "-o".to_string(),
            match self.password.is_empty() {
                true => "BatchMode=yes".to_string(),
                false => "NumberOfPasswordPrompts=1".to_string(),
            },
            // Uma máquina nunca visitada é o caso normal de quem investiga um servidor
            // pela primeira vez; uma chave que *mudou* continua sendo recusada, que é o
            // aviso que importa.
            "-o".to_string(),
            "StrictHostKeyChecking=accept-new".to_string(),
            // Sem terminal: o script não pede nada e não desenha nada.
            "-T".to_string(),
        ];
        if let Some(key) = &self.key {
            args.push("-i".to_string());
            args.push(key.display().to_string());
            // Pedir uma chave e deixar o agente oferecer as dele primeiro é como um
            // servidor que conta tentativas fecha a porta antes da certa ser tentada.
            args.push("-o".to_string());
            args.push("IdentitiesOnly=yes".to_string());
        }
        if !self.port.is_empty() {
            args.push("-p".to_string());
            args.push(self.port.clone());
        }
        if !self.user.is_empty() {
            args.push("-l".to_string());
            args.push(self.user.clone());
        }
        args.push(self.host.clone());
        // O script vai como argumento, e o shell do outro lado o executa. `sh` e não
        // `bash`: nem toda máquina tem bash, e nada aqui precisa dele.
        args.push("LC_ALL=C sh".to_string());
        args
    }

    /// A linha como seria digitada, para o log. O jeito mais rápido de descobrir por que
    /// uma conexão não sobe é rodar a mesma linha na mão.
    pub fn command_line(&self) -> String {
        let mut linha = self.ssh.display().to_string();
        for arg in self.argv().iter().take(self.argv().len() - 1) {
            linha.push(' ');
            linha.push_str(arg);
        }
        format!("{linha} 'sh'  ← e o script pela entrada padrão")
    }

    /// Manda o script e devolve o que a máquina respondeu.
    ///
    /// A senha, quando existe, chega ao `ssh` por uma variável de ambiente **só do
    /// processo filho**, e quem a digita é este mesmo binário chamado como `SSH_ASKPASS`
    /// — ver `SENHA_ENV` e o começo do `main`. É melhor que `sshpass`, que a coloca na
    /// linha de comando, onde qualquer `ps` da máquina a lê; aqui ela fica no ambiente do
    /// `ssh`, visível só para o próprio usuário e para o root.
    pub fn run(&self, script: &str) -> Result<String, String> {
        let mut command = Command::new(&self.ssh);
        command
            .args(self.argv())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if !self.password.is_empty() {
            let eu = std::env::current_exe()
                .map_err(|e| format!("não consegui encontrar o próprio binário: {e}"))?;
            command
                .env(SENHA_ENV, &self.password)
                .env("SSH_ASKPASS", eu)
                // `force` é o que faz o ssh preferir o askpass mesmo havendo terminal.
                // Existe desde o OpenSSH 8.4; sem ele, uma versão antiga tentaria o
                // terminal e a conexão morreria esperando alguém digitar.
                .env("SSH_ASKPASS_REQUIRE", "force")
                .env("DISPLAY", ":0");
        }
        let mut filho = command
            .spawn()
            .map_err(|e| format!("não consegui executar o ssh: {e}"))?;

        // O script vai pela entrada padrão em vez de virar argumento: assim ele pode ter
        // o tamanho que precisar, sem esbarrar no limite da linha de comando nem exigir
        // aspas dentro de aspas.
        if let Some(entrada) = filho.stdin.take() {
            let script = script.to_string();
            std::thread::spawn(move || {
                use std::io::Write;
                let mut entrada = entrada;
                let _ = entrada.write_all(script.as_bytes());
            });
        }

        let (envia, recebe) = std::sync::mpsc::channel();
        let mut saida = filho.stdout.take();
        let mut erro = filho.stderr.take();
        std::thread::spawn(move || {
            let mut texto = String::new();
            if let Some(saida) = saida.as_mut() {
                let _ = saida.read_to_string(&mut texto);
            }
            let mut queixa = String::new();
            if let Some(erro) = erro.as_mut() {
                let _ = erro.read_to_string(&mut queixa);
            }
            let _ = envia.send((texto, queixa));
        });

        let (texto, queixa) = match recebe.recv_timeout(TIMEOUT) {
            Ok(par) => par,
            Err(_) => {
                // Uma máquina que não respondeu em um minuto e meio não vai responder.
                let _ = filho.kill();
                return Err(format!("a máquina não respondeu em {}s", TIMEOUT.as_secs()));
            }
        };
        let _ = filho.wait();
        if texto.trim().is_empty() {
            return Err(match queixa.trim() {
                "" => "o ssh não disse nada e não trouxe nada".to_string(),
                queixa => queixa.lines().last().unwrap_or(queixa).to_string(),
            });
        }
        Ok(texto)
    }
}

/// Quebra a resposta nas seções que o script marcou.
pub fn sections(saida: &str) -> HashMap<String, String> {
    let mut mapa = HashMap::new();
    for bloco in saida.split(MARCA) {
        let Some((nome, conteudo)) = bloco.split_once('\n') else {
            continue;
        };
        let nome = nome.trim();
        if !nome.is_empty() {
            mapa.insert(nome.to_string(), conteudo.trim_end().to_string());
        }
    }
    mapa
}

/// `~/.ssh/id_rsa` com o `~` resolvido.
fn expandir(caminho: &str) -> PathBuf {
    match caminho.strip_prefix("~/") {
        Some(resto) => dirs::home_dir().unwrap_or_default().join(resto),
        None => PathBuf::from(caminho),
    }
}

fn ssh_binary() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("ssh"))
        .find(|candidato| executavel(candidato))
}

fn executavel(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// O diagnóstico inteiro, em POSIX sh, mandado de uma vez.
///
/// **Leia antes de rodar — é para isso que ele está aqui inteiro.** Não há um `>`, um
/// `rm`, um `kill`, um `systemctl start`, um `apt install`. Tudo é `cat`, `ps`, `df`,
/// `ss`, `getent`, `list`, `status`, `-s` (simulação) e `grep`. Um comando que não existir
/// na máquina falha sozinho e deixa a seção vazia, e uma seção vazia vira «não deu para
/// saber» na tela, nunca um susto.
///
/// Cada bloco começa com uma marca e um nome. O que vem depois é a saída crua, que quem
/// interpreta é `ssh_checks` — a divisão de sempre: quem colhe não conclui.
const BASE: &str = r#"
s() { printf '\036%s\n' "$1"; }
s identidade
id 2>/dev/null; hostname 2>/dev/null; hostname -I 2>/dev/null
s sistema
cat /etc/os-release 2>/dev/null; uname -sr 2>/dev/null; uname -m 2>/dev/null
s virtualizacao
systemd-detect-virt 2>/dev/null || echo desconhecido
s uptime
cat /proc/uptime 2>/dev/null
s carga
cat /proc/loadavg 2>/dev/null; nproc 2>/dev/null
s cpu
grep -m1 'model name' /proc/cpuinfo 2>/dev/null; grep -c '^processor' /proc/cpuinfo 2>/dev/null
s cpu_tempos
head -1 /proc/stat 2>/dev/null
s memoria
head -30 /proc/meminfo 2>/dev/null
s swap
cat /proc/swaps 2>/dev/null
s disco
df -P -k 2>/dev/null
s inodes
df -P -i 2>/dev/null
s montagens
cat /proc/mounts 2>/dev/null
s pressao
for f in cpu memory io; do echo "== $f"; cat /proc/pressure/$f 2>/dev/null; done
s escutando
ss -H -ltnup 2>/dev/null || netstat -ltnup 2>/dev/null
s servicos_falhos
systemctl --failed --no-legend --plain 2>/dev/null
"#;

/// O que exige estatística acumulada, e o que custa uma varredura de processos.
const MEDIO: &str = r#"
s processos_cpu
ps -eo pid,user,pcpu,pmem,rss,etimes,stat,comm --sort=-pcpu 2>/dev/null | head -12
s processos_memoria
ps -eo pid,user,pcpu,pmem,rss,etimes,stat,comm --sort=-rss 2>/dev/null | head -12
s processos_estado
ps -eo stat= 2>/dev/null | cut -c1 | sort | uniq -c
s processos_presos
ps -eo pid,stat,wchan:20,comm 2>/dev/null | awk 'NR==1 || $2 ~ /^D/' | head -10
s conexoes
ss -H -tan 2>/dev/null | awk '{print $1}' | sort | uniq -c
s interfaces
ip -o -4 addr show 2>/dev/null; echo '--'; cat /proc/net/dev 2>/dev/null
s sessoes
who 2>/dev/null; echo '--'; last -n 8 2>/dev/null | head -9
s contas
getent passwd 2>/dev/null | awk -F: '($3>=1000 && $3<65000) || $3==0'
s administradores
getent group sudo 2>/dev/null; getent group wheel 2>/dev/null; getent group admin 2>/dev/null
s erros_recentes
journalctl -p err -n 25 --no-pager -q 2>/dev/null
s oom
journalctl -k --no-pager -q 2>/dev/null | grep -i 'out of memory\|oom-killer' | tail -5
s servicos
systemctl list-units --type=service --state=running --no-legend --plain 2>/dev/null | wc -l
s relogio
timedatectl 2>/dev/null
s descritores
cat /proc/sys/fs/file-nr 2>/dev/null
s kernel_atual
uname -r 2>/dev/null; ls -1 /boot/vmlinuz-* 2>/dev/null | tail -1; ls /var/run/reboot-required 2>/dev/null
"#;

/// O inventário e a configuração: o que custa mais e muda menos.
const PROFUNDO: &str = r#"
s pacotes
dpkg-query -f '.\n' -W 2>/dev/null | wc -l; rpm -qa 2>/dev/null | wc -l; apk info 2>/dev/null | wc -l
s atualizacoes
apt-get -s -q upgrade 2>/dev/null | grep -c '^Inst'; dnf -q --refresh check-update 2>/dev/null | grep -c '^[a-zA-Z0-9]'
s sshd
grep -RhEi '^[[:space:]]*(permitrootlogin|passwordauthentication|permitemptypasswords|port|x11forwarding|maxauthtries|allowtcpforwarding)' /etc/ssh/sshd_config /etc/ssh/sshd_config.d 2>/dev/null
s firewall
ufw status 2>/dev/null; iptables -S 2>/dev/null | head -12; nft list ruleset 2>/dev/null | head -12
s agendados
crontab -l 2>/dev/null | grep -v '^#'; echo '--'; ls -1 /etc/cron.d /etc/cron.daily 2>/dev/null
s containers
docker ps --format '{{.Names}}\t{{.Status}}\t{{.Image}}' 2>/dev/null; podman ps 2>/dev/null | tail -n +2
s ajustes_kernel
for k in vm.swappiness vm.overcommit_memory net.core.somaxconn fs.file-max net.ipv4.tcp_syncookies kernel.panic; do printf '%s=' $k; sysctl -n $k 2>/dev/null || echo '?'; done
s limites
ulimit -n; ulimit -u
s logs
du -sk /var/log 2>/dev/null
s chaves_autorizadas
wc -l < "$HOME/.ssh/authorized_keys" 2>/dev/null
s servicos_habilitados
systemctl list-unit-files --state=enabled --no-legend --plain --type=service 2>/dev/null | wc -l
"#;

/// O script da profundidade pedida.
pub fn script(depth: super::Depth) -> String {
    let mut texto = BASE.to_string();
    if depth >= super::Depth::Medio {
        texto.push_str(MEDIO);
    }
    if depth >= super::Depth::Profundo {
        texto.push_str(PROFUNDO);
    }
    texto
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separa_as_secoes_pela_marca() {
        let saida = format!("{MARCA}host\nservidor\nlinux\n{MARCA}disco\n/dev/sda 50%\n");
        let mapa = sections(&saida);
        assert_eq!(mapa.get("host").unwrap(), "servidor\nlinux");
        assert_eq!(mapa.get("disco").unwrap(), "/dev/sda 50%");
        assert!(!mapa.contains_key("nao_existe"));
    }

    #[test]
    fn uma_secao_vazia_existe_e_e_vazia() {
        let mapa = sections(&format!("{MARCA}nada\n{MARCA}algo\nx"));
        assert_eq!(mapa.get("nada").map(String::as_str), Some(""));
        assert_eq!(mapa.get("algo").map(String::as_str), Some("x"));
    }
}
