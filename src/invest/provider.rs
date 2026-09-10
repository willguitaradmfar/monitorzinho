//! De onde vêm os números que não são do usuário.
//!
//! O `trait Provider` existe já na v1, com um provedor que não faz rede nenhuma. Ele não
//! é abstração especulativa: é o que permite que a promessa de `docs/invest/02` seja
//! verdade — cripto e renda fixa ao vivo, ação da B3 e dos EUA com preço informado, e
//! **nenhum módulo mudando** no dia em que entrar um provedor profissional.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::invest::feed::FeedError;
use crate::invest::model::{AssetId, Market, Moeda};
use crate::invest::store::{self, Cache, CachedQuote};
use crate::invest::tempo;

/// O quanto se pode confiar num preço, e o que a tela mostra ao lado dele. Não é enfeite:
/// é a diferença entre confiar num P&L e conferi-lo.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Grade {
    AoVivo,
    /// Fonte oficial, com atraso conhecido, em segundos.
    Atrasado(u64),
    Fechamento,
    /// Informado pelo usuário — ver `docs/invest/02 §3.4`.
    Manual,
    /// Fonte sem contrato nenhum. Nenhum provedor da v1 é assim; a variante existe para
    /// que ligar uma um dia já chegue estampada em toda linha que vier dela.
    NaoOficial,
}

impl Grade {
    /// O quanto esta origem vale contra outra, para decidir quem sobrescreve quem.
    ///
    /// Existe por causa de um sintoma exato: o preço ao vivo aparecia e **virava
    /// «manual»** um instante depois. O provedor manual cobre todo ativo e tem intervalo
    /// de dois segundos, então nas voltas em que a brapi e o Yahoo não estavam na vez ele
    /// respondia — e publicava o preço informado por cima do preço de mercado.
    ///
    /// A ordem não é gosto: um preço de mercado, mesmo atrasado, diz mais sobre quanto a
    /// coisa vale agora do que um número que alguém digitou semana passada.
    fn peso(&self) -> u8 {
        match self {
            Grade::AoVivo => 4,
            Grade::Atrasado(_) => 3,
            Grade::NaoOficial | Grade::Fechamento => 2,
            Grade::Manual => 1,
        }
    }

    /// A marca curta na coluna. `AoVivo` não escreve nada — o silêncio é o bom estado.
    pub fn marca(&self) -> &'static str {
        match self {
            Grade::AoVivo => "",
            Grade::Atrasado(_) => "atrasado",
            Grade::Fechamento => "fech.",
            Grade::Manual => "manual",
            Grade::NaoOficial => "não-oficial",
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Grade::AoVivo => "ao_vivo",
            Grade::Atrasado(_) => "atrasado",
            Grade::Fechamento => "fechamento",
            Grade::Manual => "manual",
            Grade::NaoOficial => "nao_oficial",
        }
    }

    /// Se um preço desta qualidade pode ser colorido por variação e desenhado num
    /// gráfico. Um preço informado tem variação calculável entre dois valores digitados,
    /// e colori-la lhe daria a mesma autoridade visual de um preço ao vivo.
    pub fn ao_vivo(&self) -> bool {
        matches!(self, Grade::AoVivo | Grade::Atrasado(_))
    }

    fn de_code(code: &str) -> Grade {
        match code {
            "ao_vivo" => Grade::AoVivo,
            "fechamento" => Grade::Fechamento,
            "nao_oficial" => Grade::NaoOficial,
            "atrasado" => Grade::Atrasado(900),
            _ => Grade::Manual,
        }
    }
}

/// Uma cotação.
#[derive(Clone, Debug)]
pub struct Quote {
    pub ativo: AssetId,
    /// Qual provedor deu este número — `Provider::id`.
    ///
    /// A qualidade (`grade`) e a origem são coisas diferentes, e as duas importam: «ao
    /// vivo» diz o quanto se pode confiar, «brapi» diz a quem reclamar. Numa aba em que
    /// metade dos preços vem de um agregador de terceiros e a outra metade de uma
    /// exchange, esconder a fonte é esconder a diferença.
    pub fonte: &'static str,
    pub preco: f64,
    /// O fechamento anterior, quando a fonte dá. É o que permite calcular a variação do
    /// dia — sem ele a coluna fica vazia em vez de zerada.
    pub anterior: Option<f64>,
    pub moeda: Moeda,
    pub grade: Grade,
    pub em: u64,
    pub volume: Option<f64>,
    pub max24: Option<f64>,
    pub min24: Option<f64>,
}

impl Quote {
    pub fn variacao(&self) -> Option<f64> {
        let anterior = self.anterior?;
        (anterior > 0.0).then(|| (self.preco / anterior - 1.0) * 100.0)
    }

    pub fn idade(&self, agora: u64) -> u64 {
        agora.saturating_sub(self.em)
    }
}

/// Um ponto de série histórica.
#[derive(Clone, Copy, Debug)]
pub struct Candle {
    pub em: u64,
    pub fechamento: f64,
}

/// Que janela de histórico se está pedindo.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Span {
    Dia,
    Semana,
    Mes,
    SeisMeses,
    Ano,
    CincoAnos,
}

impl Span {
    pub const ALL: [Span; 6] = [
        Span::Dia,
        Span::Semana,
        Span::Mes,
        Span::SeisMeses,
        Span::Ano,
        Span::CincoAnos,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            Span::Dia => "1 dia",
            Span::Semana => "5 dias",
            Span::Mes => "1 mês",
            Span::SeisMeses => "6 meses",
            Span::Ano => "1 ano",
            Span::CincoAnos => "5 anos",
        }
    }

    pub fn dias(&self) -> u32 {
        match self {
            Span::Dia => 1,
            Span::Semana => 5,
            Span::Mes => 30,
            Span::SeisMeses => 182,
            Span::Ano => 365,
            Span::CincoAnos => 1825,
        }
    }
}

pub trait Provider: Send + Sync {
    /// Chave estável, gravada na configuração — nunca muda depois de publicada.
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    /// Que ativos esta fonte sabe responder. É como o despacho escolhe quem pergunta.
    fn covers(&self, ativo: &AssetId) -> bool;

    /// Se o mercado deste ativo está aberto agora. Decide a cadência — e é do provedor e
    /// não do relógio local, porque a Binance nunca fecha, a B3 fecha às 18h de Brasília,
    /// e a NYSE tem feriado próprio.
    fn is_open(&self, _ativo: &AssetId, _agora: u64) -> bool {
        true
    }

    /// Cotação de vários ativos **de uma vez**. Em lote porque quase toda API cobra por
    /// requisição e não por símbolo: pedir trinta preços em trinta chamadas é trinta
    /// vezes a cota pelo mesmo dado.
    fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError>;

    /// Série histórica. `None` numa fonte que não tem — e aí o gráfico simplesmente não
    /// oferece aquele ativo, em vez de mostrar uma tela vazia.
    fn history(&self, _ativo: &AssetId, _span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        None
    }

    /// Os números da empresa por trás do papel. `None` numa fonte que não os serve — e
    /// numa fonte que os serve, para um ativo que não é empresa.
    fn fundamentos(
        &self,
        _ativo: &AssetId,
    ) -> Option<Result<crate::invest::fundamento::Fundamentos, FeedError>> {
        None
    }

    /// Os proventos que a empresa anunciou. `None` numa fonte que não os serve.
    fn proventos(
        &self,
        _ativo: &AssetId,
    ) -> Option<Result<Vec<crate::invest::provento::Anunciado>, FeedError>> {
        None
    }

    /// Se esta fonte serve fundamento, para os módulos perguntarem sem buscar.
    fn tem_fundamento(&self) -> bool {
        false
    }

    /// Se esta fonte serve série histórica **sem precisar buscá-la para descobrir**.
    ///
    /// É o que substituiu uma lista de mercados escrita à mão em cinco módulos. Aquela
    /// lista foi feita quando só cripto e câmbio tinham série, e ficou para trás no dia em
    /// que a B3 ganhou uma fonte — sem que nada apontasse o erro. Perguntar ao provedor é
    /// a única forma de a resposta continuar certa quando as fontes mudam.
    fn tem_historico(&self) -> bool {
        false
    }

    /// O mínimo entre duas voltas desta fonte. Nunca abaixo do piso de 2 s, mesmo que a
    /// cota permita.
    fn intervalo(&self) -> Duration {
        Duration::from_secs(2)
    }

    /// Quanto da cota da janela já foi gasto, quando a API se auto-reporta. `None` nas
    /// fontes que não dizem — e aí o limitador só tem o intervalo para se guiar.
    fn peso_usado(&self) -> Option<u64> {
        None
    }

    /// Se esta fonte faz I/O. O provedor manual não faz, e por isso nunca conta como
    /// «uma fonte que caiu» nem participa do limitador.
    fn remoto(&self) -> bool {
        true
    }
}

/// O estado de uma fonte, para a linha do rodapé que responde «por que este número não
/// mexe».
#[derive(Clone, Debug)]
pub struct StatusFonte {
    pub id: &'static str,
    pub nome: &'static str,
    pub ok: bool,
    pub erro: Option<String>,
    pub ultima_volta: Option<u64>,
    /// Para onde o recuo levou o intervalo, quando ele está apertando.
    pub espera: Option<Duration>,
    pub peso: Option<u64>,
}

/// Se uma volta de um provedor conta como «falei com a fonte», para o carimbo de idade.
///
/// **Um provedor local não conta, por mais preços que responda.** Foi por não perguntar
/// isto que o «há quanto tempo» mentia sem internet: o provedor `informado` não faz I/O
/// nenhum, roda de dois em dois segundos, cobre todo ativo e sempre devolve `Ok` — então
/// com a rede caída ele carimbava a cotação como recém-buscada a cada duas voltas de
/// relógio, e a tela dizia «agora» sobre números que ninguém confirmava havia horas.
///
/// Resposta vazia também não conta: uma fonte que respondeu sem nenhum papel não deixou
/// nada mais novo do que já havia.
fn conta_como_busca(remoto: bool, quantas: usize) -> bool {
    remoto && quantas > 0
}

/// Volume, máxima e mínima de quem os tiver, quando o dono do preço não os tem.
///
/// O preço vem sempre da melhor fonte — essa disputa é resolvida acima e não muda. Mas
/// «melhor para o preço» não é «tem todos os campos»: quem serve a B3 aqui não publica
/// volume em endereço nenhum, e por isso a coluna aparecia vazia em quase toda linha da
/// tela de Cotações, mesmo quando outra fonte tinha respondido o número na mesma volta.
///
/// **Só dentro do mesmo pregão.** O volume é acumulado do dia; carregá-lo para o dia
/// seguinte mostraria o giro de ontem como o de hoje, que é o tipo de número velho com
/// cara de novo que esta aba passa o tempo todo evitando.
fn herdar_acessorios(mut q: Quote, anterior: &Quote) -> Quote {
    let mesmo_dia = tempo::Data::de_epoch(q.em, tempo::BRT_OFFSET)
        == tempo::Data::de_epoch(anterior.em, tempo::BRT_OFFSET);
    if !mesmo_dia {
        return q;
    }
    q.volume = q.volume.or(anterior.volume);
    q.max24 = q.max24.or(anterior.max24);
    q.min24 = q.min24.or(anterior.min24);
    q
}

/// O retrato que a UI lê. Trocado inteiro a cada volta, nunca editado no lugar — mesma
/// mecânica do `container::store::Snapshot`, e pelo mesmo motivo: a interface nunca pode
/// esperar por um socket.
#[derive(Clone, Default)]
pub struct MarketSnapshot {
    pub quotes: HashMap<AssetId, Quote>,
    pub fontes: Vec<StatusFonte>,
}

impl MarketSnapshot {
    pub fn quote(&self, ativo: &AssetId) -> Option<&Quote> {
        self.quotes.get(ativo)
    }

    /// Converte para BRL. `None` quando não há como — e aí a linha fica de fora do total
    /// e o total diz que ficou, em vez de somar um número inventado.
    pub fn para_brl(&self, valor: f64, moeda: Moeda) -> Option<f64> {
        match moeda {
            Moeda::Brl => Some(valor),
            Moeda::Usd | Moeda::Usdt => self.taxa(Moeda::Usd).map(|t| valor * t),
            Moeda::Eur => self.taxa(Moeda::Eur).map(|t| valor * t),
        }
    }

    pub fn taxa(&self, moeda: Moeda) -> Option<f64> {
        if moeda == Moeda::Brl {
            return Some(1.0);
        }
        let par = AssetId::new(Market::Fx, format!("{}BRL", moeda.code()));
        self.quotes.get(&par).map(|q| q.preco)
    }
}

/// Os provedores, na ordem de preferência, e a thread que os faz trabalhar.
pub struct ProviderSet {
    provedores: Vec<Box<dyn Provider>>,
    snapshot: Arc<Mutex<MarketSnapshot>>,
    /// O que a thread tem que buscar. Trocado pela UI quando a carteira muda.
    pedido: Arc<Mutex<Vec<AssetId>>>,
    /// Preços informados, para o provedor manual. Vive aqui e não dentro dele porque quem
    /// os conhece é a carteira, que muda enquanto o programa roda.
    ///
    /// **É o mesmo `Arc` que o provedor manual segura** — e tem que ser, recebido de fora
    /// em vez de criado aqui. Criar um segundo mapa fazia `set_manuais` escrever num lugar
    /// que ninguém lia, e todo preço informado sumia da tela de Cotações.
    manuais: Arc<Mutex<HashMap<AssetId, (f64, u64)>>>,
    /// Se alguma tela está olhando. Falso para a thread inteira: bater numa API de
    /// mercado quando ninguém está olhando gasta cota que alguém tem em quantidade
    /// finita — ver `docs/invest/00 §3`, regra 3.
    ativo: Arc<AtomicBool>,
    parar: Arc<AtomicBool>,
    acordar: Arc<(Mutex<bool>, Condvar)>,
    revisao: Arc<AtomicU64>,
    /// Se a thread já foi criada. Atômico e não `Option<JoinHandle>` porque o conjunto
    /// vive dentro de um `Arc` e ninguém tem `&mut` para guardar o handle.
    iniciada: AtomicBool,
    /// O intervalo mínimo entre voltas, em segundos — ver `set_piso`.
    piso: AtomicU64,
}

impl ProviderSet {
    pub fn new(
        provedores: Vec<Box<dyn Provider>>,
        manuais: Arc<Mutex<HashMap<AssetId, (f64, u64)>>>,
        cache: &Cache,
    ) -> Arc<Self> {
        // O cache entra no retrato antes de a thread começar: é o que faz a aba abrir
        // mostrando números e não traços.
        let mut inicial = MarketSnapshot::default();
        for (chave, c) in cache {
            if let Ok(ativo) = AssetId::parse(chave) {
                let moeda = Moeda::parse(&c.moeda).unwrap_or(Moeda::Brl);
                inicial.quotes.insert(
                    ativo.clone(),
                    Quote {
                        // O cache não guarda a fonte: ele existe para a aba abrir com
                        // números, e o provedor que os deu pode nem estar neste build.
                        fonte: "cache",
                        ativo,
                        preco: c.preco,
                        anterior: c.anterior,
                        moeda,
                        grade: Grade::de_code(&c.grade),
                        em: c.em,
                        volume: None,
                        max24: None,
                        min24: None,
                    },
                );
            }
        }

        Arc::new(Self {
            provedores,
            snapshot: Arc::new(Mutex::new(inicial)),
            pedido: Arc::new(Mutex::new(Vec::new())),
            manuais,
            ativo: Arc::new(AtomicBool::new(false)),
            parar: Arc::new(AtomicBool::new(false)),
            acordar: Arc::new((Mutex::new(false), Condvar::new())),
            revisao: Arc::new(AtomicU64::new(0)),
            iniciada: AtomicBool::new(false),
            piso: AtomicU64::new(0),
        })
    }

    /// Começa a trabalhar. Chamado quando o **primeiro módulo abre**, nunca ao entrar na
    /// aba: é a regra 3 de `docs/invest/00 §3`.
    pub fn start(self: &Arc<Self>) {
        if self.provedores.iter().all(|p| !p.remoto())
            || self.iniciada.swap(true, Ordering::Relaxed)
        {
            return;
        }
        let eu = Arc::clone(self);
        thread::Builder::new()
            .name("invest-feed".into())
            .spawn(move || eu.rodar())
            .ok();
    }

    /// O intervalo mínimo entre voltas de uma mesma fonte.
    ///
    /// Existe para a home: ali a aba está à vista mas ninguém está operando, e um preço
    /// de trinta em trinta segundos é tudo o que um relance precisa. Sem este piso, a
    /// Binance sozinha faria trinta chamadas por minuto para encher um cartão que se olha
    /// de passagem — gastar cota de terceiro por isso é o tipo de custo que a aba inteira
    /// foi desenhada para não ter.
    ///
    /// Dentro de um módulo o piso cai a zero e cada fonte volta ao ritmo dela.
    pub fn set_piso(&self, piso: Duration) {
        self.piso.store(piso.as_secs(), Ordering::Relaxed);
        self.cutucar();
    }

    fn piso(&self) -> Duration {
        Duration::from_secs(self.piso.load(Ordering::Relaxed))
    }

    /// Se alguém está de fato buscando agora.
    ///
    /// A tela precisa disto para não mentir. Um painel que escreve «buscando…» num ativo
    /// sem cotação está certo **enquanto a thread roda** — e é uma promessa falsa na home,
    /// onde nada é buscado até um módulo abrir. Ali o certo é um traço: não é que o número
    /// esteja a caminho, é que ninguém foi atrás dele.
    pub fn buscando(&self) -> bool {
        self.ativo.load(Ordering::Relaxed)
    }

    /// Liga e desliga a busca. Ligar acorda a thread na hora — chegar numa tela não pode
    /// custar uma volta inteira de espera.
    pub fn set_ativo(&self, ativo: bool) {
        if self.ativo.swap(ativo, Ordering::Relaxed) == ativo {
            return;
        }
        if ativo {
            self.cutucar();
        }
    }

    fn cutucar(&self) {
        let (lock, cv) = &*self.acordar;
        if let Ok(mut flag) = lock.lock() {
            *flag = true;
            cv.notify_all();
        }
    }

    pub fn parar(&self) {
        self.parar.store(true, Ordering::Relaxed);
        self.cutucar();
    }

    pub fn revisao(&self) -> u64 {
        self.revisao.load(Ordering::Relaxed)
    }

    /// O que buscar. Trocado quando a carteira ou a watchlist mudam.
    pub fn pedir(&self, ativos: Vec<AssetId>) {
        if let Ok(mut p) = self.pedido.lock() {
            if *p == ativos {
                return;
            }
            *p = ativos;
        }
        self.cutucar();
    }

    pub fn set_manuais(&self, manuais: HashMap<AssetId, (f64, u64)>) {
        if let Ok(mut m) = self.manuais.lock() {
            *m = manuais;
        }
    }

    /// Lê o retrato. Nunca bloqueia por I/O — no pior caso espera a thread soltar o
    /// mutex, que é o tempo de trocar um mapa.
    pub fn snapshot(&self) -> MarketSnapshot {
        match self.snapshot.lock() {
            Ok(s) => s.clone(),
            // Um mutex envenenado é uma thread que morreu no meio da publicação. O
            // retrato de dentro dele ainda é o último bom.
            Err(envenenado) => envenenado.into_inner().clone(),
        }
    }

    /// Busca histórico. Síncrono e chamado de dentro de uma thread do módulo, nunca do
    /// tick — é uma requisição que pode demorar segundos.
    /// A série de um ativo, **percorrendo a cadeia de reserva** como as cotações fazem.
    ///
    /// Sem isto, a primeira fonte que cobre o ativo decidia sozinha: a brapi devolve 401
    /// para o histórico de um papel que ela só libera com token, e o gráfico ficava vazio
    /// mesmo havendo o Yahoo logo atrás sabendo respondê-lo.
    pub fn history(&self, ativo: &AssetId, span: Span) -> Option<Result<Vec<Candle>, FeedError>> {
        let mut ultimo_erro = None;
        for p in self.provedores.iter().filter(|p| p.covers(ativo)) {
            match p.history(ativo, span) {
                Some(Ok(serie)) => return Some(Ok(serie)),
                Some(Err(e)) => ultimo_erro = Some(e),
                // Esta fonte não serve série; a próxima que cobre o ativo pode servir.
                None => {}
            }
        }
        ultimo_erro.map(Err)
    }

    /// Os fundamentos de um ativo, percorrendo a cadeia de reserva como o histórico.
    pub fn fundamentos(
        &self,
        ativo: &AssetId,
    ) -> Option<Result<crate::invest::fundamento::Fundamentos, FeedError>> {
        let mut ultimo_erro = None;
        for p in self.provedores.iter().filter(|p| p.covers(ativo)) {
            match p.fundamentos(ativo) {
                Some(Ok(f)) => return Some(Ok(f)),
                Some(Err(e)) => ultimo_erro = Some(e),
                None => {}
            }
        }
        ultimo_erro.map(Err)
    }

    /// Os proventos anunciados de um ativo, percorrendo a cadeia como o histórico.
    ///
    /// `Vec` vazio quando ninguém sabe responder — e vazio é uma resposta: um ativo sem
    /// provento anunciado e um sem fonte que os sirva ficam iguais aqui de propósito, e a
    /// tela não promete a diferença.
    pub fn proventos(&self, ativo: &AssetId) -> Option<Vec<crate::invest::provento::Anunciado>> {
        for p in self.provedores.iter().filter(|p| p.covers(ativo)) {
            if let Some(Ok(v)) = p.proventos(ativo) {
                return Some(v);
            }
        }
        None
    }

    /// Se algum provedor sabe dar fundamento deste ativo — o que separa uma empresa de
    /// um par de moedas.
    pub fn tem_fundamento(&self, ativo: &AssetId) -> bool {
        self.provedores
            .iter()
            .any(|p| p.covers(ativo) && p.tem_fundamento())
    }

    /// Se algum provedor sabe dar série histórica deste ativo. É o que os módulos que
    /// desenham no tempo perguntam — em vez de trazerem a própria lista de mercados.
    pub fn tem_historico(&self, ativo: &AssetId) -> bool {
        self.provedores
            .iter()
            .any(|p| p.covers(ativo) && p.tem_historico())
    }

    /// Quem responderia por este ativo, para a tela poder dizer a origem antes mesmo de
    /// haver preço.
    /// Quem **busca** este ativo de verdade, ignorando o provedor manual.
    ///
    /// `quem_cobre` responde «sim» para qualquer ativo, porque o manual cobre todos — é
    /// a reserva final. Para a pergunta «isto tem fonte, ou é um número que alguém
    /// digitou?», é esta que serve.
    pub fn quem_busca(&self, ativo: &AssetId) -> Option<&dyn Provider> {
        self.provedores
            .iter()
            .find(|p| p.remoto() && p.covers(ativo))
            .map(|p| p.as_ref())
    }

    pub fn quem_cobre(&self, ativo: &AssetId) -> Option<&dyn Provider> {
        self.provedores
            .iter()
            .find(|p| p.covers(ativo))
            .map(|p| p.as_ref())
    }

    /// O laço da thread.
    fn rodar(self: Arc<Self>) {
        let mut proxima: HashMap<&'static str, Instant> = HashMap::new();
        let mut recuo: HashMap<&'static str, u32> = HashMap::new();

        while !self.parar.load(Ordering::Relaxed) {
            if !self.ativo.load(Ordering::Relaxed) {
                self.dormir(Duration::from_secs(3600));
                continue;
            }

            let ativos = self.pedido.lock().map(|p| p.clone()).unwrap_or_default();
            let agora_epoch = store::agora();
            let mut espera = Duration::from_secs(2);
            let mut fontes: Vec<StatusFonte> = Vec::new();
            let mut novos: Vec<Quote> = Vec::new();
            // O que ainda ninguém entregou nesta volta. É isto que faz a cadeia de
            // reserva funcionar: quem responde tira o ativo da lista, e quem falha o
            // deixa para o próximo que o cobre. Sem isso, uma fonte que cai leva o ativo
            // junto, mesmo havendo outra logo atrás que sabe respondê-lo.
            let mut pendentes: Vec<AssetId> = ativos.clone();

            for provedor in &self.provedores {
                let meus: Vec<AssetId> = pendentes
                    .iter()
                    .filter(|a| provedor.covers(a))
                    .cloned()
                    .collect();
                if meus.is_empty() {
                    continue;
                }

                // Mercado fechado custa uma volta por minuto e não uma a cada dois
                // segundos: o preço não muda, e insistir é gastar cota à toa.
                let aberto = meus.iter().any(|a| provedor.is_open(a, agora_epoch));
                let base = match aberto {
                    true => provedor.intervalo(),
                    false => Duration::from_secs(60),
                }
                // O piso da home. Dentro de um módulo ele é zero e não muda nada.
                .max(self.piso());
                let passos = recuo.get(provedor.id()).copied().unwrap_or(0);
                let intervalo = aplicar_recuo(base, passos);

                let devido = proxima
                    .get(provedor.id())
                    .is_none_or(|t| Instant::now() >= *t);
                if !devido {
                    if let Some(t) = proxima.get(provedor.id()) {
                        espera = espera.min(t.saturating_duration_since(Instant::now()));
                    }
                    continue;
                }

                let resultado = provedor.quotes(&meus);
                proxima.insert(provedor.id(), Instant::now() + intervalo);
                espera = espera.min(intervalo);

                match resultado {
                    Ok(quotes) => {
                        recuo.insert(provedor.id(), 0);
                        // Só o que ele de fato entregou sai da lista. Um provedor que
                        // devolve metade dos ativos deixa a outra metade para o seguinte.
                        pendentes.retain(|a| !quotes.iter().any(|q| q.ativo == *a));
                        if conta_como_busca(provedor.remoto(), quotes.len()) {
                            crate::invest::store::buscas()
                                .carimbar(crate::invest::store::fonte::COTACAO);
                        }
                        novos.extend(quotes);
                        if provedor.remoto() {
                            fontes.push(StatusFonte {
                                id: provedor.id(),
                                nome: provedor.name(),
                                ok: true,
                                erro: None,
                                ultima_volta: Some(agora_epoch),
                                espera: None,
                                peso: provedor.peso_usado(),
                            });
                        }
                    }
                    Err(erro) => {
                        // `Retry-After` manda. Se o servidor disse quanto esperar, é esse
                        // o tempo — tratar 429 como 500 é como se perde acesso a uma API.
                        if let FeedError::Cota {
                            retry_after: Some(s),
                        } = &erro
                        {
                            proxima.insert(provedor.id(), Instant::now() + Duration::from_secs(*s));
                        }
                        // Um erro que não adianta repetir — cota estourada, ou um corpo
                        // que não é o esperado — pula direto para um recuo largo: tentar
                        // de novo em dois segundos devolve exatamente a mesma resposta.
                        let passos = match erro.desiste() {
                            true => RECUO_MAX,
                            false => (passos + 1).min(RECUO_MAX),
                        };
                        recuo.insert(provedor.id(), passos);
                        if provedor.remoto() {
                            fontes.push(StatusFonte {
                                id: provedor.id(),
                                nome: provedor.name(),
                                ok: false,
                                erro: Some(erro.frase()),
                                ultima_volta: None,
                                espera: Some(aplicar_recuo(base, passos)),
                                peso: None,
                            });
                        }
                    }
                }
            }

            if !novos.is_empty() || !fontes.is_empty() {
                self.publicar(novos, fontes);
            }
            self.dormir(espera.max(Duration::from_millis(500)));
        }
    }

    fn publicar(&self, novos: Vec<Quote>, fontes: Vec<StatusFonte>) {
        if let Ok(mut s) = self.snapshot.lock() {
            let agora = store::agora();
            for q in novos {
                // Uma origem pior não toma o lugar de uma melhor **enquanto esta ainda
                // vale**. E o que faz uma cotação de mercado deixar de valer é o mercado
                // seguir andando sem ela: uma fonte que morreu no meio do pregão prende a
                // tela num número que ninguém mais confirma, e passado o prazo o informado
                // assume.
                //
                // **Com o pregão fechado isso não acontece**, e é onde a regra antiga
                // errava: o último negócio do dia é o preço de agora e vale a noite
                // inteira. Medindo só o relógio, todo fim de tarde o preço ao vivo era
                // trocado pelo informado — o valor aparecia e sumia, virando «manual».
                let andando = self
                    .quem_busca(&q.ativo)
                    .is_some_and(|p| p.is_open(&q.ativo, agora));
                if let Some(atual) = s.quotes.get(&q.ativo)
                    && q.grade.peso() < atual.grade.peso()
                    && (!andando || q.em.saturating_sub(atual.em) < VALIDADE_DA_ORIGEM)
                {
                    continue;
                }
                let q = match s.quotes.get(&q.ativo) {
                    Some(anterior) => herdar_acessorios(q, anterior),
                    None => q,
                };
                s.quotes.insert(q.ativo.clone(), q);
            }
            // Uma fonte que não falou nesta volta mantém o que disse na anterior: sumir
            // com a linha dela faria a tela piscar entre «ok» e nada.
            for f in fontes {
                match s.fontes.iter_mut().find(|x| x.id == f.id) {
                    Some(slot) => *slot = f,
                    None => s.fontes.push(f),
                }
            }
        }
        self.revisao.fetch_add(1, Ordering::Relaxed);
    }

    fn dormir(&self, quanto: Duration) {
        let (lock, cv) = &*self.acordar;
        let Ok(flag) = lock.lock() else { return };
        let (mut flag, _) = cv
            .wait_timeout(flag, quanto)
            .unwrap_or_else(|e| e.into_inner());
        *flag = false;
    }

    /// O que gravar no cache, para a próxima abertura mostrar números.
    pub fn para_cache(&self) -> Cache {
        let s = self.snapshot();
        s.quotes
            .values()
            .map(|q| {
                (
                    q.ativo.to_string(),
                    CachedQuote {
                        preco: q.preco,
                        em: q.em,
                        anterior: q.anterior,
                        grade: q.grade.code().to_string(),
                        moeda: q.moeda.code().to_string(),
                    },
                )
            })
            .collect()
    }
}

/// Por quanto tempo uma origem melhor segura o lugar contra uma pior.
///
/// Meia hora: passado isso, um preço de mercado deixou de ser mais informativo que o
/// informado, e prendê-lo na tela seria mostrar um número que já não descreve nada.
const VALIDADE_DA_ORIGEM: u64 = 30 * 60;

/// Quantas vezes o recuo pode dobrar. A partir de 2 s, oito passos chegam a ~8 min.
const RECUO_MAX: u32 = 8;

/// Recuo exponencial com teto. Sem isto, uma fonte fora do ar vira uma tempestade de
/// tentativas — que é a forma mais rápida de ser bloqueado por uma API pública.
fn aplicar_recuo(base: Duration, passos: u32) -> Duration {
    let fator = 1u32 << passos.min(RECUO_MAX);
    (base * fator).min(Duration::from_secs(300))
}

/// A janela de pregão da B3, em horário de Brasília: dia útil mais a hora do dia.
pub fn b3_aberta(agora: u64) -> bool {
    dia_util_br(agora) && (10..18).contains(&tempo::hora(agora, tempo::BRT_OFFSET))
}

/// Se hoje é dia útil no Brasil: nem fim de semana, nem feriado de mercado.
///
/// É o que decide a cadência das fontes que só publicam em dia útil — o Banco Central e a
/// PTAX. Sem isto, o programa passa o sábado inteiro pedindo um número que não vai mudar.
///
/// A tabela de feriados vive no módulo Agenda, que é onde se vê o que ela tem.
pub fn dia_util_br(agora: u64) -> bool {
    let data = tempo::Data::de_epoch(agora, tempo::BRT_OFFSET);
    !data.fim_de_semana() && crate::invest::modules::agenda::feriado_b3(&data).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A regressão que a falta de internet revelou: o provedor `informado` não faz I/O,
    /// roda de dois em dois segundos, cobre todo ativo e sempre devolve `Ok`. Com a rede
    /// caída ele carimbava a cotação como recém-buscada a cada duas voltas de relógio, e
    /// a tela dizia «agora» sobre números que ninguém confirmava havia horas.
    #[test]
    fn provedor_local_nao_conta_como_ida_a_fonte() {
        assert!(
            !conta_como_busca(false, 21),
            "informado não fala com fonte nenhuma"
        );
        assert!(conta_como_busca(true, 1));
    }

    /// Resposta vazia de fonte remota também não conta: ela não deixou nada mais novo do
    /// que já havia.
    #[test]
    fn resposta_vazia_nao_renova_o_carimbo() {
        assert!(!conta_como_busca(true, 0));
        assert!(!conta_como_busca(false, 0));
    }

    /// A regra amarrada ao conjunto de verdade: nenhum provedor que não faz rede pode
    /// mexer no relógio, e um provedor local novo entra coberto sem que ninguém lembre.
    #[test]
    fn nenhum_provedor_local_mexe_no_relogio() {
        let manuais = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        let todos = crate::invest::providers::todos(std::sync::Arc::clone(&manuais));
        let locais: Vec<&'static str> = todos
            .iter()
            .filter(|p| !p.remoto())
            .map(|p| p.id())
            .collect();
        assert!(
            !locais.is_empty(),
            "o teste perde o sentido se não houver provedor local nenhum"
        );
        for id in locais {
            assert!(
                !conta_como_busca(false, 99),
                "{id} é local e não pode carimbar"
            );
        }
    }

    fn q(em: u64, volume: Option<f64>, max24: Option<f64>) -> Quote {
        Quote {
            ativo: AssetId::new(Market::B3, "PETR4"),
            fonte: "teste",
            preco: 10.0,
            anterior: None,
            moeda: Moeda::Brl,
            grade: Grade::AoVivo,
            em,
            volume,
            max24,
            min24: None,
        }
    }

    /// Quem ganha o preço nem sempre tem todos os campos: a fonte da B3 não publica
    /// volume, e a coluna ficava vazia mesmo quando outra fonte tinha respondido o
    /// número na mesma volta.
    #[test]
    fn os_acessorios_vem_de_quem_os_tem() {
        let dia = 1_789_000_000;
        let novo = herdar_acessorios(q(dia + 60, None, None), &q(dia, Some(1e6), Some(11.0)));
        assert_eq!(novo.volume, Some(1e6));
        assert_eq!(novo.max24, Some(11.0));
        // E o que a fonte nova traz manda: herdar é preencher buraco, não sobrescrever.
        let novo = herdar_acessorios(q(dia + 60, Some(2e6), None), &q(dia, Some(1e6), None));
        assert_eq!(novo.volume, Some(2e6));
    }

    /// O volume é acumulado do dia. Carregá-lo para o pregão seguinte mostraria o giro
    /// de ontem como o de hoje.
    #[test]
    fn o_acessorio_nao_atravessa_o_pregao() {
        let ontem = 1_789_000_000;
        let hoje = ontem + 86_400;
        let novo = herdar_acessorios(q(hoje, None, None), &q(ontem, Some(1e6), Some(11.0)));
        assert_eq!(novo.volume, None);
        assert_eq!(novo.max24, None);
    }

    #[test]
    fn recuo_dobra_e_para_no_teto() {
        let base = Duration::from_secs(2);
        assert_eq!(aplicar_recuo(base, 0), Duration::from_secs(2));
        assert_eq!(aplicar_recuo(base, 1), Duration::from_secs(4));
        assert_eq!(aplicar_recuo(base, 3), Duration::from_secs(16));
        assert_eq!(aplicar_recuo(base, 99), Duration::from_secs(300));
    }

    #[test]
    fn grade_manual_nao_e_ao_vivo() {
        assert!(Grade::AoVivo.ao_vivo());
        assert!(Grade::Atrasado(900).ao_vivo());
        assert!(!Grade::Manual.ao_vivo());
        assert!(!Grade::NaoOficial.ao_vivo());
        assert!(!Grade::Fechamento.ao_vivo());
    }

    #[test]
    fn dia_util_exclui_fim_de_semana_e_feriado() {
        let em = |dia: u32| {
            tempo::Data {
                ano: 2026,
                mes: 9,
                dia,
            }
            .epoch_inicio(tempo::BRT_OFFSET)
        };
        assert!(!dia_util_br(em(5)), "sábado");
        assert!(!dia_util_br(em(6)), "domingo");
        assert!(!dia_util_br(em(7)), "7 de setembro é feriado");
        assert!(dia_util_br(em(8)), "terça comum");
    }

    #[test]
    fn conversao_para_brl_sem_cambio_e_none() {
        let mut s = MarketSnapshot::default();
        assert_eq!(s.para_brl(100.0, Moeda::Brl), Some(100.0));
        // Sem o par na mesa, converter é inventar.
        assert_eq!(s.para_brl(100.0, Moeda::Usd), None);

        let par = AssetId::new(Market::Fx, "USDBRL");
        s.quotes.insert(
            par.clone(),
            Quote {
                fonte: "teste",
                ativo: par,
                preco: 5.42,
                anterior: None,
                moeda: Moeda::Brl,
                grade: Grade::AoVivo,
                em: 0,
                volume: None,
                max24: None,
                min24: None,
            },
        );
        assert_eq!(s.para_brl(100.0, Moeda::Usd), Some(542.0));
        // USDT vale por dólar para conversão.
        assert_eq!(s.para_brl(100.0, Moeda::Usdt), Some(542.0));
    }

    #[test]
    fn o_preco_informado_chega_ao_provedor_manual() {
        // O conjunto e o provedor têm que compartilhar o **mesmo** mapa. Com dois mapas,
        // `set_manuais` escrevia num lugar que ninguém lia e todo preço informado sumia
        // da tela de Cotações — enquanto Posições, que tem um caminho alternativo, o
        // mostrava. O sintoma era «sem provedor» ao lado de um preço visível noutra tela.
        use crate::invest::providers::manual::Manual;
        let manuais = Arc::new(Mutex::new(HashMap::new()));
        // Só o provedor manual: com a lista completa, `quem_cobre` devolveria a brapi —
        // que é o comportamento certo e faria este teste bater na rede.
        let set = ProviderSet::new(
            vec![Box::new(Manual::new(Arc::clone(&manuais)))],
            Arc::clone(&manuais),
            &Cache::new(),
        );
        let petr = AssetId::new(Market::B3, "PETR4");
        set.set_manuais(HashMap::from([(petr.clone(), (38.42, 1000))]));

        let quotes = set
            .quem_cobre(&petr)
            .expect("o manual cobre tudo")
            .quotes(std::slice::from_ref(&petr))
            .expect("o manual não faz I/O e não falha");
        assert_eq!(
            quotes.len(),
            1,
            "o preço informado tem que sair do provedor"
        );
        assert_eq!(quotes[0].preco, 38.42);
        assert_eq!(quotes[0].grade, Grade::Manual);
    }

    #[test]
    fn variacao_precisa_do_fechamento_anterior() {
        let mut q = Quote {
            fonte: "teste",
            ativo: AssetId::new(Market::B3, "PETR4"),
            preco: 38.42,
            anterior: None,
            moeda: Moeda::Brl,
            grade: Grade::Manual,
            em: 0,
            volume: None,
            max24: None,
            min24: None,
        };
        assert_eq!(
            q.variacao(),
            None,
            "sem anterior a coluna fica vazia, não zerada"
        );
        q.anterior = Some(38.0);
        let v = q.variacao().unwrap();
        assert!((v - 1.105).abs() < 0.01);
    }
}

#[cfg(test)]
mod cadeia_tests {
    use super::*;
    use crate::invest::model::Market;

    /// Um provedor que cobre o que se pedir e responde o que se mandar responder.
    struct Falso {
        id: &'static str,
        mercado: Market,
        falha: bool,
    }

    impl Provider for Falso {
        fn id(&self) -> &'static str {
            self.id
        }
        fn name(&self) -> &'static str {
            self.id
        }
        fn covers(&self, ativo: &AssetId) -> bool {
            ativo.market == self.mercado
        }
        fn quotes(&self, ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
            if self.falha {
                return Err(FeedError::Tempo);
            }
            Ok(ativos
                .iter()
                .map(|a| Quote {
                    fonte: self.id,
                    ativo: a.clone(),
                    preco: 1.0,
                    anterior: None,
                    moeda: Moeda::Brl,
                    grade: Grade::AoVivo,
                    em: 0,
                    volume: None,
                    max24: None,
                    min24: None,
                })
                .collect())
        }
    }

    /// A simulação de uma volta do laço, com a mesma regra de `rodar`: quem entrega tira
    /// o ativo da lista de pendentes, e quem falha o deixa para o próximo que o cobre.
    fn uma_volta(provedores: &[Falso], ativos: &[AssetId]) -> Vec<Quote> {
        let mut pendentes: Vec<AssetId> = ativos.to_vec();
        let mut novos = Vec::new();
        for p in provedores {
            let meus: Vec<AssetId> = pendentes.iter().filter(|a| p.covers(a)).cloned().collect();
            if meus.is_empty() {
                continue;
            }
            if let Ok(quotes) = p.quotes(&meus) {
                pendentes.retain(|a| !quotes.iter().any(|q| q.ativo == *a));
                novos.extend(quotes);
            }
        }
        novos
    }

    #[test]
    fn a_primeira_fonte_que_entrega_ganha() {
        let petr = AssetId::new(Market::B3, "PETR4");
        let quotes = uma_volta(
            &[
                Falso {
                    id: "brapi",
                    mercado: Market::B3,
                    falha: false,
                },
                Falso {
                    id: "yahoo",
                    mercado: Market::B3,
                    falha: false,
                },
            ],
            std::slice::from_ref(&petr),
        );
        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].fonte, "brapi", "a reserva não pode tomar a vez");
    }

    #[test]
    fn a_reserva_entra_quando_a_primeira_falha() {
        // O caso que motiva a cadeia: brapi sem token esbarra no limite, e o preço não
        // pode sumir da tela por causa disso.
        let petr = AssetId::new(Market::B3, "PETR4");
        let quotes = uma_volta(
            &[
                Falso {
                    id: "brapi",
                    mercado: Market::B3,
                    falha: true,
                },
                Falso {
                    id: "yahoo",
                    mercado: Market::B3,
                    falha: false,
                },
            ],
            std::slice::from_ref(&petr),
        );
        assert_eq!(quotes.len(), 1);
        assert_eq!(quotes[0].fonte, "yahoo");
    }

    #[test]
    fn todas_falhando_nao_inventa_preco() {
        let petr = AssetId::new(Market::B3, "PETR4");
        let quotes = uma_volta(
            &[
                Falso {
                    id: "brapi",
                    mercado: Market::B3,
                    falha: true,
                },
                Falso {
                    id: "yahoo",
                    mercado: Market::B3,
                    falha: true,
                },
            ],
            std::slice::from_ref(&petr),
        );
        assert!(
            quotes.is_empty(),
            "sem fonte, a linha fica sem preço — nunca inventada"
        );
    }

    #[test]
    fn cada_ativo_segue_a_propria_cadeia() {
        let petr = AssetId::new(Market::B3, "PETR4");
        let btc = AssetId::new(Market::Binance, "BTCBRL");
        let quotes = uma_volta(
            &[
                Falso {
                    id: "brapi",
                    mercado: Market::B3,
                    falha: true,
                },
                Falso {
                    id: "yahoo",
                    mercado: Market::B3,
                    falha: false,
                },
                Falso {
                    id: "binance",
                    mercado: Market::Binance,
                    falha: false,
                },
            ],
            &[petr, btc],
        );
        assert_eq!(quotes.len(), 2);
        let de = |s: &str| quotes.iter().find(|q| q.fonte == s).is_some();
        assert!(de("yahoo") && de("binance"));
        assert!(!de("brapi"));
    }
}

#[cfg(test)]
mod historico_tests {
    use super::*;
    use crate::invest::model::Market;

    /// Um provedor que cobre B3 e responde o que se mandar responder sobre série.
    struct Falso {
        id: &'static str,
        serie: Option<Result<Vec<Candle>, FeedError>>,
    }

    impl Provider for Falso {
        fn id(&self) -> &'static str {
            self.id
        }
        fn name(&self) -> &'static str {
            self.id
        }
        fn covers(&self, ativo: &AssetId) -> bool {
            ativo.market == Market::B3
        }
        fn quotes(&self, _: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
            Ok(Vec::new())
        }
        fn history(&self, _: &AssetId, _: Span) -> Option<Result<Vec<Candle>, FeedError>> {
            match &self.serie {
                Some(Ok(v)) => Some(Ok(v.clone())),
                Some(Err(_)) => Some(Err(FeedError::Status {
                    code: 401,
                    detalhe: None,
                })),
                None => None,
            }
        }
    }

    fn conjunto(provedores: Vec<Box<dyn Provider>>) -> Arc<ProviderSet> {
        ProviderSet::new(
            provedores,
            Arc::new(Mutex::new(HashMap::new())),
            &Cache::new(),
        )
    }

    fn ponto() -> Vec<Candle> {
        vec![Candle {
            em: 1,
            fechamento: 10.0,
        }]
    }

    #[test]
    fn a_serie_cai_para_a_reserva_quando_a_primeira_recusa() {
        // O caso medido: a brapi devolve 401 para o histórico de um papel que ela só
        // libera com token, e o gráfico ficava vazio com o Yahoo logo atrás.
        let set = conjunto(vec![
            Box::new(Falso {
                id: "brapi",
                serie: Some(Err(FeedError::Status {
                    code: 401,
                    detalhe: None,
                })),
            }),
            Box::new(Falso {
                id: "yahoo",
                serie: Some(Ok(ponto())),
            }),
        ]);
        let serie = set
            .history(&AssetId::new(Market::B3, "PRIO3"), Span::Mes)
            .expect("alguém tem que responder")
            .expect("a reserva respondeu");
        assert_eq!(serie.len(), 1);
    }

    #[test]
    fn uma_fonte_que_nao_serve_serie_nao_bloqueia_a_seguinte() {
        // `None` é «não sirvo série», e não «falhei». Ele tem que passar a vez.
        let set = conjunto(vec![
            Box::new(Falso {
                id: "so-cotacao",
                serie: None,
            }),
            Box::new(Falso {
                id: "com-serie",
                serie: Some(Ok(ponto())),
            }),
        ]);
        assert!(
            set.history(&AssetId::new(Market::B3, "PETR4"), Span::Mes)
                .is_some_and(|r| r.is_ok())
        );
    }

    #[test]
    fn todas_recusando_devolve_o_erro_e_nao_uma_serie_vazia() {
        let set = conjunto(vec![
            Box::new(Falso {
                id: "a",
                serie: Some(Err(FeedError::Status {
                    code: 401,
                    detalhe: None,
                })),
            }),
            Box::new(Falso {
                id: "b",
                serie: Some(Err(FeedError::Status {
                    code: 500,
                    detalhe: None,
                })),
            }),
        ]);
        assert!(
            set.history(&AssetId::new(Market::B3, "PETR4"), Span::Mes)
                .is_some_and(|r| r.is_err())
        );
    }

    #[test]
    fn sem_ninguem_cobrindo_nao_ha_resposta() {
        // `None` no topo significa «nenhuma fonte conhece este ativo», que é diferente de
        // «todas falharam» — e a tela diz coisas diferentes para os dois.
        let set = conjunto(vec![Box::new(Falso {
            id: "a",
            serie: Some(Ok(ponto())),
        })]);
        assert!(
            set.history(&AssetId::new(Market::Us, "AAPL"), Span::Mes)
                .is_none()
        );
    }
}

#[cfg(test)]
mod prioridade_tests {
    use super::*;
    use crate::invest::model::Market;

    fn quote(grade: Grade, preco: f64, em: u64) -> Quote {
        Quote {
            fonte: "teste",
            ativo: AssetId::new(Market::B3, "PETR4"),
            preco,
            anterior: None,
            moeda: Moeda::Brl,
            grade,
            em,
            volume: None,
            max24: None,
            min24: None,
        }
    }

    fn conjunto() -> Arc<ProviderSet> {
        ProviderSet::new(
            Vec::new(),
            Arc::new(Mutex::new(HashMap::new())),
            &Cache::new(),
        )
    }

    /// Um provedor que cobre a B3 e diz se o pregão está aberto. Sem ele o conjunto de
    /// teste não tem mercado nenhum, e «mercado andando» é sempre falso.
    struct Pregao(bool);

    impl Provider for Pregao {
        fn id(&self) -> &'static str {
            "pregao"
        }
        fn name(&self) -> &'static str {
            "pregão de teste"
        }
        fn covers(&self, ativo: &AssetId) -> bool {
            ativo.market == Market::B3
        }
        fn is_open(&self, _ativo: &AssetId, _agora: u64) -> bool {
            self.0
        }
        fn quotes(&self, _ativos: &[AssetId]) -> Result<Vec<Quote>, FeedError> {
            Ok(Vec::new())
        }
    }

    fn conjunto_com_pregao(aberto: bool) -> Arc<ProviderSet> {
        ProviderSet::new(
            vec![Box::new(Pregao(aberto))],
            Arc::new(Mutex::new(HashMap::new())),
            &Cache::new(),
        )
    }

    #[test]
    fn o_preco_informado_nao_apaga_o_preco_de_mercado() {
        // O sintoma exato relatado: o valor aparecia na coluna e virava «manual» um
        // instante depois. O provedor manual cobre todo ativo e responde a cada dois
        // segundos, então nas voltas em que a brapi não estava na vez ele publicava por
        // cima.
        let set = conjunto();
        let ativo = AssetId::new(Market::B3, "PETR4");
        set.publicar(vec![quote(Grade::AoVivo, 48.09, 1000)], Vec::new());
        set.publicar(vec![quote(Grade::Manual, 38.42, 1001)], Vec::new());

        let q = set
            .snapshot()
            .quote(&ativo)
            .cloned()
            .expect("tem que haver preço");
        assert_eq!(q.preco, 48.09, "o informado tomou o lugar do de mercado");
        assert_eq!(q.grade, Grade::AoVivo);
    }

    #[test]
    fn uma_origem_melhor_sempre_entra() {
        let set = conjunto();
        let ativo = AssetId::new(Market::B3, "PETR4");
        set.publicar(vec![quote(Grade::Manual, 38.42, 1000)], Vec::new());
        set.publicar(vec![quote(Grade::AoVivo, 48.09, 1001)], Vec::new());
        assert_eq!(set.snapshot().quote(&ativo).unwrap().preco, 48.09);
    }

    #[test]
    fn a_mesma_origem_sempre_atualiza() {
        // Segurar o lugar contra igual congelaria o preço ao vivo.
        let set = conjunto();
        let ativo = AssetId::new(Market::B3, "PETR4");
        set.publicar(vec![quote(Grade::AoVivo, 48.09, 1000)], Vec::new());
        set.publicar(vec![quote(Grade::AoVivo, 48.50, 1001)], Vec::new());
        assert_eq!(set.snapshot().quote(&ativo).unwrap().preco, 48.50);
    }

    #[test]
    fn passada_a_validade_o_informado_assume_com_o_pregao_aberto() {
        // A fonte morreu **no meio do pregão**: o mercado seguiu andando sem ela, e um
        // preço de meia hora atrás já não descreve nada. Só neste caso o informado assume.
        let set = conjunto_com_pregao(true);
        let ativo = AssetId::new(Market::B3, "PETR4");
        set.publicar(vec![quote(Grade::AoVivo, 48.09, 1000)], Vec::new());
        set.publicar(
            vec![quote(Grade::Manual, 38.42, 1000 + VALIDADE_DA_ORIGEM + 1)],
            Vec::new(),
        );
        assert_eq!(set.snapshot().quote(&ativo).unwrap().preco, 38.42);
    }

    #[test]
    fn com_o_pregao_fechado_o_informado_nao_toma_o_lugar_do_fechamento() {
        // O sintoma que isto trava: à noite, a cotação aparecia com variação e coluna e
        // um instante depois virava «manual». O fechamento do dia é velho **por
        // definição** — a bolsa fechou —, e medir a validade só pelo relógio fazia o
        // preço informado ganhar dele todo fim de tarde.
        let set = conjunto();
        let ativo = AssetId::new(Market::B3, "PETR4");
        set.publicar(vec![quote(Grade::AoVivo, 48.09, 1000)], Vec::new());
        set.publicar(
            vec![quote(Grade::Manual, 38.42, 1000 + VALIDADE_DA_ORIGEM + 1)],
            Vec::new(),
        );
        // O conjunto de teste não tem provedor da B3, então `quem_busca` diz «ninguém» e
        // o mercado não está andando — o mesmo que uma bolsa fechada.
        assert_eq!(
            set.snapshot().quote(&ativo).unwrap().preco,
            48.09,
            "sem mercado andando, o fechamento fica"
        );
    }

    #[test]
    fn nao_oficial_ganha_do_informado_e_perde_do_ao_vivo() {
        assert!(Grade::NaoOficial.peso() > Grade::Manual.peso());
        assert!(Grade::NaoOficial.peso() < Grade::AoVivo.peso());
    }
}
