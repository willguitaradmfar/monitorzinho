//! A aba Invest: um terminal de mercado dentro do monitorzinho.
//!
//! É a primeira aba que não fala sobre a máquina em que o programa roda. Todas as outras
//! respondem «o que está acontecendo aqui»; esta responde «o que está acontecendo com o
//! meu dinheiro». A diferença decide três coisas: a fonte é a rede e não o `/proc`, o
//! dado é do usuário e tem que sobreviver a reinstalação, e a aba tem estado próprio.
//!
//! **Custo zero fora de foco.** O comentário do próprio `Tab` já diz que cada aba só é
//! amostrada enquanto está na frente. Aqui a regra aperta, porque o recurso gasto não é
//! só CPU local:
//!
//! 1. `App::new()` não constrói nada daqui — o campo nasce `None`.
//! 2. Entrar na aba lê dois arquivos pequenos. **Nada de rede acontece.**
//! 3. Thread de rede só nasce quando um módulo que precisa dela é **aberto**, e morre
//!    quando ele fecha. Mais rígido que o `container::Store`, que desacelera para 20 s:
//!    ler um socket local de graça é aceitável, gastar cota de uma API de mercado quando
//!    ninguém está olhando não é — a cota é um recurso finito do usuário.
//! 4. `sample()` nunca faz I/O de rede. Toda leitura é em thread de fundo, e a UI lê um
//!    retrato sob mutex.

use std::collections::HashMap;
use std::sync::Arc;

use crate::invest::model::{AssetId, Portfolio};
use crate::invest::module::{Ctx, Estado, Group, InvestModule, Need};
use crate::invest::provider::{MarketSnapshot, ProviderSet};
use crate::invest::store::{Cache, LoadIssue, Mru};

pub mod b3;
pub mod calc;
pub mod calendario;
pub mod carteira;
pub mod csv;
pub mod feed;
pub mod fundamento;
pub mod gzip;
pub mod historico;
pub mod model;
pub mod module;
pub mod modules;
pub mod noticias;
pub mod provento;
pub mod provider;
pub mod providers;
pub mod rss;
pub mod serie;
pub mod store;
pub mod tempo;

/// Tudo que a aba tem. Nasce na primeira vez que alguém entra nela, e não antes.
pub struct InvestState {
    pub portfolio: Portfolio,
    pub issue: Option<LoadIssue>,
    pub somente_leitura: bool,
    pub mru: Mru,
    pub cache: Cache,
    pub providers: Arc<ProviderSet>,
    pub market: MarketSnapshot,
    /// O calendário econômico. **Aqui e não dentro da vista de Agenda** porque a home
    /// mostra a agenda também, e um cache dentro da vista só existe enquanto o módulo
    /// está aberto — era isso que fazia o cartão da home mostrar uma agenda diferente da
    /// tela cheia.
    pub calendario: crate::invest::calendario::Cache,
    /// As manchetes dos feeds. Aqui e não dentro da vista de Notícias, pelo mesmo motivo
    /// do calendário: a home também as mostra.
    pub noticias: crate::invest::noticias::Cache,
    /// As séries históricas, e os fundamentos das empresas.
    ///
    /// **No estado da aba, e não dentro de cada vista.** Sete módulos guardavam cada um o
    /// seu, então abrir o Gráfico e depois o Risco buscava a mesma série duas vezes — e o
    /// painel da home não tinha como mostrar nada, porque o cache nascia e morria com o
    /// módulo aberto.
    pub historico: crate::invest::historico::Cache,
    pub fundamentos: crate::invest::fundamento::Cache,
    /// Os proventos **anunciados** pelas empresas — sugestões, não registros. Ver
    /// `invest::provento`.
    pub anunciados: crate::invest::provento::Cache,
    modulos: Vec<Box<dyn InvestModule>>,
    /// A curva do patrimônio, um ponto por dia — ver `serie`.
    pub patrimonio: Vec<serie::Ponto>,
    /// Quando a série foi gravada pela última vez. Ela muda a cada tick com preço ao
    /// vivo, e gravar um arquivo a cada dois segundos por um número que se lê em meses
    /// seria escrever no disco à toa.
    serie_gravada_em: u64,
    /// Os disparos de alerta desde que a aba abriu, do mais recente para o mais antigo.
    /// É o que a barra de abas conta e o que o módulo mostra no histórico.
    pub disparos: Vec<(u64, String)>,
    /// Quantos disparos ainda não foram vistos — o contador ao lado de «Invest».
    pub disparos_novos: usize,
    /// Se há sujeira ainda não gravada.
    sujo: bool,
    /// O que a última gravação disse que deu errado, para a tela poder mostrar.
    pub erro_gravacao: Option<String>,
}

impl InvestState {
    /// Lê o disco e monta o registro. Chamado na primeira entrada na aba — dois arquivos
    /// pequenos e alocação, dezenas de microssegundos. Nenhuma rede.
    pub fn load() -> Self {
        let carregado = store::load();
        let cache = store::load_cache();
        let manuais = Arc::new(std::sync::Mutex::new(HashMap::new()));
        // O mesmo `Arc` vai para os provedores e para o conjunto: é o que faz
        // `set_manuais` chegar ao provedor manual.
        let providers = ProviderSet::new(
            providers::todos(Arc::clone(&manuais)),
            Arc::clone(&manuais),
            &cache,
        );
        let market = providers.snapshot();
        // Semeado do disco: o calendário da sessão passada aparece na home antes de
        // qualquer rede, e é o que faz o cartão de Agenda nascer preenchido.
        let calendario = crate::invest::calendario::Cache::default();
        calendario.semear(store::load_agenda());
        let noticias = crate::invest::noticias::Cache::default();
        noticias.semear(store::load_noticias());
        let anunciados = crate::invest::provento::Cache::default();
        anunciados.semear(store::load_anunciados());

        let state = Self {
            portfolio: carregado.portfolio,
            issue: carregado.issue,
            somente_leitura: carregado.somente_leitura,
            mru: store::load_mru(),
            cache,
            providers,
            market,
            calendario,
            noticias,
            historico: Default::default(),
            fundamentos: Default::default(),
            anunciados,
            modulos: modules::todos(),
            patrimonio: serie::load(),
            serie_gravada_em: 0,
            disparos: Vec::new(),
            disparos_novos: 0,
            sujo: false,
            erro_gravacao: None,
        };
        state.sincronizar_manuais();
        state
    }

    pub fn ctx(&self) -> Ctx<'_> {
        Ctx {
            portfolio: &self.portfolio,
            market: &self.market,
            providers: &self.providers,
            patrimonio: &self.patrimonio,
            noticias: &self.noticias,
            historico: &self.historico,
            fundamentos: &self.fundamentos,
            anunciados: &self.anunciados,
            calendario: &self.calendario,
            disparos: &self.disparos,
            agora: store::agora(),
            somente_leitura: self.somente_leitura,
        }
    }

    pub fn modulo(&self, id: &str) -> Option<&dyn InvestModule> {
        self.modulos
            .iter()
            .find(|m| m.id() == id)
            .map(|m| m.as_ref())
    }

    /// Os módulos na ordem da lista: os já abertos, do mais recente para o mais antigo, e
    /// depois os nunca abertos, na ordem de registro agrupada por família.
    /// Os módulos, sempre na mesma ordem: a do registro, que é a dos grupos —
    /// Carteira, Mercado, Análise, Informação, Operação.
    ///
    /// **Fixa de propósito.** Uma home que se reordena a cada acesso obriga a reler a
    /// tela antes de cada tecla, e a tecla de um módulo deixa de ser decorável. Quem
    /// procura um módulo específico tem a busca da lista completa; quem olha a home quer
    /// encontrar ontem onde encontrou hoje.
    pub fn ordem(&self) -> Vec<&dyn InvestModule> {
        let mut v: Vec<&dyn InvestModule> = self.modulos.iter().map(|m| m.as_ref()).collect();
        // Estável: dentro de um grupo vale a ordem em que os módulos foram registrados.
        v.sort_by_key(|m| Group::ALL.iter().position(|g| *g == m.group()).unwrap_or(9));
        v
    }

    /// Carimba a abertura. **No momento em que o módulo abre**, e não quando fecha: abrir
    /// é o gesto que expressa interesse, e reordenar por tempo de permanência seria
    /// adivinhar intenção.
    pub fn marcar_aberto(&mut self, id: &str) {
        self.mru.insert(id.to_string(), store::agora());
        store::save_mru(&self.mru);
    }

    /// O que falta para um módulo funcionar. Conferido contra o estado atual, e nunca
    /// impede de abrir: a tela do módulo é onde se ensina o que fazer.
    pub fn estado(&self, m: &dyn InvestModule) -> Estado {
        let ctx = self.ctx();
        for need in m.needs() {
            let falta = match need {
                Need::Posicoes if ctx.portfolio.posicoes.is_empty() => Some("sem posições"),
                Need::Lancamentos if ctx.portfolio.lancamentos.is_empty() => {
                    Some("sem lançamentos")
                }
                Need::Cotacao if ctx.market.quotes.is_empty() => Some("sem cotação"),
                Need::Cambio if ctx.market.taxa(model::Moeda::Usd).is_none() => Some("sem câmbio"),
                Need::Historico if !self.tem_historico() => Some("sem histórico"),
                Need::Provedor(nome) => Some(*nome),
                _ => None,
            };
            if let Some(falta) = falta {
                return Estado::Falta(match need {
                    Need::Provedor(nome) => format!("precisa de {nome}"),
                    _ => falta.to_string(),
                });
            }
        }
        // Um aviso e não um impedimento: dá para usar tudo com preço informado, e a tela
        // diz a origem de cada linha.
        if self.tudo_manual() && !ctx.portfolio.posicoes.is_empty() {
            return Estado::Falta("preço manual".to_string());
        }
        Estado::Pronto
    }

    fn tem_historico(&self) -> bool {
        self.portfolio.ativos_de_interesse().iter().any(|a| {
            matches!(
                a.market,
                model::Market::Binance | model::Market::Bcb | model::Market::Fx
            )
        })
    }

    fn tudo_manual(&self) -> bool {
        self.portfolio.posicoes.iter().all(|p| {
            self.market
                .quote(&p.ativo)
                .is_none_or(|q| !q.grade.ao_vivo())
        })
    }

    /// Repassa ao provedor manual os preços informados que estão na carteira. Chamado a
    /// cada edição, porque quem os conhece é a carteira e ela muda enquanto o app roda.
    pub fn sincronizar_manuais(&self) {
        let manuais: HashMap<AssetId, (f64, u64)> = self
            .portfolio
            .posicoes
            .iter()
            .filter_map(|p| {
                let preco = p.preco_manual?;
                Some((p.ativo.clone(), (preco, p.preco_manual_em.unwrap_or(0))))
            })
            .collect();
        self.providers.set_manuais(manuais);
    }

    /// Diz à thread o que buscar: o que se tem, o que se acompanha, mais os pares de
    /// câmbio, que a âncora em BRL torna obrigatórios.
    pub fn sincronizar_pedido(&self) {
        self.sincronizar_pedido_com(&[]);
    }

    /// O mesmo, mais o que o módulo aberto pediu — ver `ModuleView::ativos`.
    pub fn sincronizar_pedido_com(&self, extras: &[AssetId]) {
        let mut ativos = self.portfolio.ativos_de_interesse();
        for par in providers::cambio::pares_essenciais() {
            if !ativos.contains(&par) {
                ativos.push(par);
            }
        }
        for a in extras {
            if !ativos.contains(a) {
                ativos.push(a.clone());
            }
        }
        self.providers.pedir(ativos);
    }

    /// Lê o retrato publicado pela thread. Uma cópia de um mapa de dezenas de entradas —
    /// e nunca segura o mutex enquanto a tela desenha.
    pub fn atualizar_market(&mut self) {
        self.market = self.providers.snapshot();
        self.registrar_patrimonio();
        self.avaliar_alertas();
    }

    /// Confere as regras de alerta contra o retrato novo.
    ///
    /// Aqui e não dentro do módulo: um alerta que só funcionasse com a tela dele aberta
    /// seria inútil — é justamente para avisar de longe que ele existe.
    fn avaliar_alertas(&mut self) {
        if self.portfolio.alertas.is_empty() {
            return;
        }
        let agora = store::agora();
        let disparados =
            modules::alertas::avaliar(&mut self.portfolio.alertas, &self.market, agora);
        for i in disparados {
            let Some(a) = self.portfolio.alertas.get(i) else {
                continue;
            };
            let preco = self
                .market
                .quote(&a.ativo)
                .map(|q| self::calc::preco(q.preco))
                .unwrap_or_default();
            self.disparos.insert(
                0,
                (
                    agora,
                    format!(
                        "{} {} {} · agora {preco}",
                        a.ativo.short(),
                        a.regra.label(),
                        match a.regra.e_percentual() {
                            true => self::calc::pct_casas(a.valor, 2),
                            false => self::calc::preco(a.valor),
                        }
                    ),
                ),
            );
            self.disparos_novos += 1;
            // O estado «armado» é do arquivo: sem gravá-lo, reiniciar o programa faria
            // toda regra já satisfeita disparar de novo.
            self.sujo = true;
        }
        // O histórico tem teto: um alerta de variação numa semana volátil encheria a
        // memória com linhas que ninguém vai rolar até o fim.
        self.disparos.truncate(200);
    }

    /// Chamado enquanto a tela dos alertas está na frente: zera o contador da barra. A
    /// cada volta e não só ao abrir, senão um disparo que acontece **com a tela aberta**
    /// deixaria um aviso que ninguém tem como dispensar.
    pub fn marcar_disparos_vistos(&mut self) {
        self.disparos_novos = 0;
    }

    /// Carimba o patrimônio de hoje. Um ponto por dia; dentro do dia, o mais recente vale.
    ///
    /// Só grava em disco de minuto em minuto: o número muda a cada tick com preço ao vivo,
    /// e escrever um arquivo a cada dois segundos por uma grandeza que se lê em meses
    /// seria gastar disco à toa.
    fn registrar_patrimonio(&mut self) {
        // Sem posição não há patrimônio a registrar, e um ponto de zero criaria uma queda
        // que nunca aconteceu na curva de quem começou a usar hoje.
        if self.portfolio.posicoes.is_empty() {
            return;
        }
        let agora = store::agora();
        let linhas = self::carteira::linhas(&self.portfolio, &self.market, agora);
        let totais = self::carteira::totais(&linhas);
        // Um total incompleto não entra: metade da carteira registrada como se fosse ela
        // inteira criaria um degrau na curva no dia em que uma fonte caiu.
        if !totais.completo() || totais.mercado <= 0.0 {
            return;
        }
        let (aporte, retirada, proventos) = serie::fluxos_do_dia(&self.portfolio, agora);
        serie::registrar(
            &mut self.patrimonio,
            agora,
            totais.mercado,
            aporte,
            retirada,
            proventos,
        );
        if agora.saturating_sub(self.serie_gravada_em) >= 60 {
            serie::save(&self.patrimonio);
            self.serie_gravada_em = agora;
        }
    }

    /// Grava se houver o que gravar. Uma carteira que perde a última edição por um `kill`
    /// é uma carteira em que não se confia, então isto é chamado logo depois de cada
    /// edição — e não só no ritmo do tick.
    pub fn persist(&mut self) {
        if !self.sujo {
            return;
        }
        if self.somente_leitura {
            self.erro_gravacao = Some(
                "o arquivo é de uma versão mais nova do monitorzinho — nada foi gravado".into(),
            );
            self.sujo = false;
            return;
        }
        match store::save(&self.portfolio) {
            Ok(()) => {
                self.erro_gravacao = None;
                self.sujo = false;
            }
            Err(e) => self.erro_gravacao = Some(e),
        }
    }

    /// Grava o cache de mercado, para a próxima abertura mostrar números e não traços.
    /// Grava a série do patrimônio agora, sem esperar o minuto. Chamado ao sair da aba e
    /// ao fechar o programa.
    pub fn persist_serie(&mut self) {
        if !self.patrimonio.is_empty() {
            serie::save(&self.patrimonio);
            self.serie_gravada_em = store::agora();
        }
    }

    /// Grava o calendário buscado. Chamado com os outros, ao sair da aba.
    pub fn persist_agenda(&self) {
        store::save_agenda(&self.calendario.ja_tem());
        store::save_noticias(&self.noticias.ja_tem());
        store::save_anunciados(&self.anunciados.ja_tem());
    }

    pub fn persist_cache(&mut self) {
        let cache = self.providers.para_cache();
        if !cache.is_empty() {
            store::save_cache(&cache);
            self.cache = cache;
        }
    }

    /// Aplica uma edição pedida por um módulo. Um lugar só que escreve na carteira.
    pub fn aplicar(&mut self, edit: module::Edit) {
        use module::Edit as E;
        let agora = store::agora();
        match edit {
            E::UpsertPosicao(mut p) => {
                p.atualizado_em = agora;
                // O carimbo do preço informado é escrito aqui e não no formulário: é o
                // momento em que ele passa a valer, e é ele que a tela mostra como idade.
                if p.preco_manual.is_some() && p.preco_manual_em.unwrap_or(0) == 0 {
                    p.preco_manual_em = Some(agora);
                }
                self.portfolio.upsert(*p);
            }
            E::RemoverPosicao(k) => self.portfolio.remove(&k),
            E::PrecoManual(ativo, preco) => {
                for p in self
                    .portfolio
                    .posicoes
                    .iter_mut()
                    .filter(|p| p.ativo == ativo)
                {
                    p.preco_manual = Some(preco);
                    p.preco_manual_em = Some(agora);
                }
            }
            E::PrecoMedio(ativo, pm) => {
                for p in self
                    .portfolio
                    .posicoes
                    .iter_mut()
                    .filter(|p| p.ativo == ativo)
                {
                    p.preco_medio = Some(pm);
                    p.atualizado_em = agora;
                }
            }
            E::AddWatch(a) => {
                if !self.portfolio.watchlist.contains(&a) {
                    self.portfolio.watchlist.push(a);
                }
            }
            E::RemoveWatch(a) => self.portfolio.watchlist.retain(|x| *x != a),
            E::SetAlvos(alvos) => self.portfolio.alvos = alvos,
            E::AddProvento(p) => self.portfolio.proventos.push(*p),
            E::RemoverProvento(i) => {
                if i < self.portfolio.proventos.len() {
                    self.portfolio.proventos.remove(i);
                }
            }
            E::AddLancamento(l) => {
                self.portfolio.lancamentos.push(*l);
                // Ordem cronológica inversa é como o módulo os mostra, e ordenar aqui
                // significa que nenhuma tela precisa ordenar de novo.
                self.portfolio
                    .lancamentos
                    .sort_by_key(|l| std::cmp::Reverse(l.em));
            }
            E::RemoverLancamento(i) => {
                if i < self.portfolio.lancamentos.len() {
                    self.portfolio.lancamentos.remove(i);
                }
            }
            E::SetMetas(m) => self.portfolio.metas = m,
            E::SetAlertas(a) => self.portfolio.alertas = a,
            E::SetPrejuizoAbertura(cat, v) => {
                self.portfolio.prejuizo_abertura.insert(cat, v);
            }
            E::SetSetor(a, setor) => {
                self.portfolio.setores.insert(a.to_string(), setor);
            }
            E::SetMapeamento(fonte, mapa) => {
                self.portfolio.mapeamentos.insert(fonte, mapa);
            }
            E::AddFeed(url) => {
                if !self.portfolio.feeds.contains(&url) {
                    self.portfolio.feeds.push(url);
                }
            }
            E::RemoveFeed(url) => self.portfolio.feeds.retain(|f| *f != url),
            E::SetFita(f) => self.portfolio.fita = f,
            E::SubstituirFonte { fonte, posicoes } => {
                // O preço médio informado **sobrevive** à substituição.
                //
                // Um extrato de corretora diz quanto se tem e onde; ele não tem nada a
                // dizer sobre quanto custou. Apagar o custo por causa de um arquivo que
                // não fala dele obrigaria a reinformar a carteira inteira toda vez que o
                // extrato do mês entrasse — e o preço médio é justamente a informação que
                // este programa não sabe reconstruir sozinho.
                //
                // Um arquivo que **traz** preço médio continua mandando: só o que vem
                // vazio herda o que já estava lá.
                let anteriores: std::collections::HashMap<model::PosKey, f64> = self
                    .portfolio
                    .posicoes
                    .iter()
                    .filter(|p| p.fonte == fonte)
                    .filter_map(|p| Some((p.key(), p.preco_medio?)))
                    .collect();
                // Atômica de propósito: metade de uma importação aplicada é uma carteira
                // que não corresponde a nada.
                self.portfolio.posicoes.retain(|p| p.fonte != fonte);
                for mut p in posicoes {
                    p.atualizado_em = agora;
                    if p.preco_medio.is_none() {
                        p.preco_medio = anteriores.get(&p.key()).copied();
                    }
                    // O preço que veio no arquivo passa a valer agora — sem este carimbo,
                    // a tela mostraria a idade contada desde a época e escreveria «há 690 m».
                    if p.preco_manual.is_some() && p.preco_manual_em.unwrap_or(0) == 0 {
                        p.preco_manual_em = Some(agora);
                    }
                    self.portfolio.posicoes.push(p);
                }
            }
        }
        self.sujo = true;
        self.sincronizar_manuais();
        self.sincronizar_pedido();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invest::model::{Classe, Market, Moeda, Position};

    fn estado_de_teste() -> InvestState {
        let manuais = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let cache = Cache::new();
        // O mesmo `Arc` vai para os provedores e para o conjunto: é o que faz
        // `set_manuais` chegar ao provedor manual.
        let providers = ProviderSet::new(
            providers::todos(Arc::clone(&manuais)),
            Arc::clone(&manuais),
            &cache,
        );
        InvestState {
            calendario: Default::default(),
            noticias: Default::default(),
            historico: Default::default(),
            fundamentos: Default::default(),
            anunciados: Default::default(),
            portfolio: Portfolio::default(),
            issue: None,
            somente_leitura: false,
            mru: Mru::new(),
            cache,
            market: providers.snapshot(),
            providers,
            modulos: modules::todos(),
            patrimonio: Vec::new(),
            serie_gravada_em: 0,
            disparos: Vec::new(),
            disparos_novos: 0,
            sujo: false,
            erro_gravacao: None,
        }
    }

    #[test]
    fn a_ordem_nao_muda_por_acesso() {
        // A home é decorável de propósito: a tecla de um módulo tem que ser a mesma
        // amanhã. Uma ordem por último acesso obriga a reler a tela antes de cada tecla.
        let mut s = estado_de_teste();
        let antes: Vec<&str> = s.ordem().iter().map(|m| m.id()).collect();
        s.mru.insert(antes[5].to_string(), 200);
        s.mru.insert(antes[2].to_string(), 100);
        let depois: Vec<&str> = s.ordem().iter().map(|m| m.id()).collect();
        assert_eq!(antes, depois, "abrir um módulo não pode reordenar a home");
        assert_eq!(depois.len(), s.modulos.len(), "nenhum módulo some da lista");
    }

    #[test]
    fn sem_nenhum_carimbo_a_ordem_e_a_de_registro() {
        let s = estado_de_teste();
        let ordem: Vec<&str> = s.ordem().iter().map(|m| m.id()).collect();
        assert_eq!(
            ordem[0], "posicoes",
            "numa instalação nova, começa-se por aqui"
        );
    }

    #[test]
    fn todo_id_de_modulo_e_unico() {
        // Um id repetido faria dois módulos brigarem pelo mesmo carimbo de MRU.
        let s = estado_de_teste();
        let mut ids: Vec<&str> = s.modulos.iter().map(|m| m.id()).collect();
        ids.sort_unstable();
        let antes = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), antes, "há id de módulo repetido");
    }

    #[test]
    fn edicao_marca_sujo_e_nunca_toca_o_preco_medio() {
        let mut s = estado_de_teste();
        let ativo = AssetId::new(Market::B3, "PETR4");
        s.aplicar(module::Edit::UpsertPosicao(Box::new(Position {
            fonte: "t".into(),
            conta: String::new(),
            ativo: ativo.clone(),
            classe: Classe::Acao,
            quantidade: 100.0,
            preco_medio: Some(31.4),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
        })));
        assert!(s.sujo);
        // Informar preço de mercado não pode mexer no custo.
        s.aplicar(module::Edit::PrecoManual(ativo, 38.42));
        let p = &s.portfolio.posicoes[0];
        assert_eq!(
            p.preco_medio,
            Some(31.4),
            "o PM não pode ser tocado por preço de mercado"
        );
        assert_eq!(p.preco_manual, Some(38.42));
    }

    #[test]
    fn substituir_fonte_e_atomico_por_fonte() {
        let mut s = estado_de_teste();
        let nova = |fonte: &str, simbolo: &str| Position {
            fonte: fonte.into(),
            conta: String::new(),
            ativo: AssetId::new(Market::B3, simbolo),
            classe: Classe::Acao,
            quantidade: 10.0,
            preco_medio: Some(1.0),
            moeda: Moeda::Brl,
            preco_manual: None,
            preco_manual_em: None,
            atualizado_em: 0,
        };
        s.aplicar(module::Edit::UpsertPosicao(Box::new(nova("a", "PETR4"))));
        s.aplicar(module::Edit::UpsertPosicao(Box::new(nova("b", "VALE3"))));
        s.aplicar(module::Edit::SubstituirFonte {
            fonte: "a".into(),
            posicoes: vec![nova("a", "BBAS3")],
        });
        let fontes: Vec<&str> = s
            .portfolio
            .posicoes
            .iter()
            .map(|p| p.fonte.as_str())
            .collect();
        assert_eq!(fontes.iter().filter(|f| **f == "a").count(), 1);
        assert!(
            s.portfolio.posicoes.iter().any(|p| p.fonte == "b"),
            "reimportar uma fonte não pode tocar noutra"
        );
    }

    #[test]
    fn somente_leitura_recusa_gravar_e_diz_por_que() {
        let mut s = estado_de_teste();
        s.somente_leitura = true;
        s.sujo = true;
        s.persist();
        assert!(s.erro_gravacao.is_some(), "tem que dizer que não gravou");
    }

    #[cfg(test)]
    mod substituir_tests {
        use super::*;
        use crate::invest::model::{AssetId, Classe, Moeda, Position};

        fn pos(fonte: &str, ativo: &str, qtd: f64, pm: Option<f64>) -> Position {
            Position {
                fonte: fonte.into(),
                conta: String::new(),
                ativo: AssetId::parse(ativo).unwrap(),
                classe: Classe::Acao,
                quantidade: qtd,
                preco_medio: pm,
                moeda: Moeda::Brl,
                preco_manual: None,
                preco_manual_em: None,
                atualizado_em: 0,
            }
        }

        fn pm_de(s: &InvestState, ativo: &str) -> Option<f64> {
            s.portfolio
                .posicoes
                .iter()
                .find(|p| p.ativo.to_string() == ativo)
                .and_then(|p| p.preco_medio)
        }

        #[test]
        fn reimportar_o_extrato_nao_apaga_o_preco_medio_informado() {
            // O caso mensal: o extrato da B3 chega sem custo nenhum, e o custo veio de uma
            // planilha à parte. Sem esta herança, cada extrato do mês zerava os preços médios
            // e obrigava a reinformar a carteira inteira — justamente a informação que este
            // programa não sabe reconstruir sozinho.
            let mut s = estado_de_teste();
            s.aplicar(module::Edit::SubstituirFonte {
                fonte: "xp".into(),
                posicoes: vec![pos("xp", "B3/PETR4", 300.0, None)],
            });
            s.aplicar(module::Edit::PrecoMedio(
                AssetId::parse("B3/PETR4").unwrap(),
                32.10,
            ));
            assert_eq!(pm_de(&s, "B3/PETR4"), Some(32.10));

            // O extrato do mês seguinte: mesma posição, quantidade nova, sem custo.
            s.aplicar(module::Edit::SubstituirFonte {
                fonte: "xp".into(),
                posicoes: vec![pos("xp", "B3/PETR4", 400.0, None)],
            });
            assert_eq!(
                pm_de(&s, "B3/PETR4"),
                Some(32.10),
                "o custo tinha que ficar"
            );
            assert_eq!(
                s.portfolio.posicoes[0].quantidade, 400.0,
                "a quantidade é a nova"
            );
        }

        #[test]
        fn um_arquivo_que_traz_preco_medio_continua_mandando() {
            // A herança preenche o que veio vazio; ela não sobrepõe o que o arquivo diz.
            let mut s = estado_de_teste();
            s.aplicar(module::Edit::SubstituirFonte {
                fonte: "xp".into(),
                posicoes: vec![pos("xp", "B3/PETR4", 300.0, Some(20.0))],
            });
            s.aplicar(module::Edit::SubstituirFonte {
                fonte: "xp".into(),
                posicoes: vec![pos("xp", "B3/PETR4", 300.0, Some(25.0))],
            });
            assert_eq!(pm_de(&s, "B3/PETR4"), Some(25.0));
        }

        #[test]
        fn a_heranca_e_por_chave_e_nao_por_ativo_solto() {
            // A chave é `(fonte, conta, ativo)`. Um papel que existe noutra corretora não
            // empresta o custo dele para esta.
            let mut s = estado_de_teste();
            s.aplicar(module::Edit::SubstituirFonte {
                fonte: "itau".into(),
                posicoes: vec![pos("itau", "B3/PETR4", 100.0, Some(50.0))],
            });
            s.aplicar(module::Edit::SubstituirFonte {
                fonte: "xp".into(),
                posicoes: vec![pos("xp", "B3/PETR4", 300.0, None)],
            });
            let xp = s
                .portfolio
                .posicoes
                .iter()
                .find(|p| p.fonte == "xp")
                .unwrap();
            assert_eq!(
                xp.preco_medio, None,
                "o custo da outra corretora não vale aqui"
            );
        }
    }
}
