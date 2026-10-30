//! Online ICMP history and conservative paired trials; the predictor never decides a promotion.
#[path = "flow.rs"]
pub(crate) mod flow;
use crate::{process_ping::{EchoOutcome, ProcessPing}, routes::Observation};
use std::time::{Duration, Instant};

const WINDOW: usize = 3;
const PHASE_LIMIT: Duration = Duration::from_secs(20);
const HOLD: Duration = Duration::from_secs(60);
const REST: Duration = Duration::from_secs(30);

struct History {
    route: Observation,
    model: flow::Flow,
    mean: f64,
    deviation: f64,
    residual: f64,
    last: f64,
    previous: f64,
    at: Instant,
}
impl History {
    fn input(&self) -> [f64; 9] {
        [self.mean / 250.0, self.deviation / 250.0, self.last / 250.0, self.previous / 250.0,
            (self.last - self.previous) / 250.0, (self.model.samples.min(32) as f64) / 32.0,
            self.residual / 250.0, if self.route.gateway.contains(':') { 1.0 } else { 0.0 }, 1.0]
    }
    fn add(&mut self, ms: f64, now: Instant) {
        let input = self.input();
        let predicted = self.model.predict(&input).clamp(0.0, 1.0) * 250.0;
        self.model.train(&input, (ms / 250.0).min(1.0));
        self.residual = 0.8 * self.residual + 0.2 * (ms - predicted).abs();
        self.deviation = 0.8 * self.deviation + 0.2 * (ms - self.mean).abs();
        self.mean = 0.8 * self.mean + 0.2 * ms;
        self.previous = self.last; self.last = ms; self.at = now;
    }
}
#[derive(Clone, Copy, Default)]
struct Window { sum: f64, min: f64, max: f64, count: usize }
impl Window {
    fn add(&mut self, value: f64) {
        if self.count == 0 { self.min = value; self.max = value; }
        self.sum += value; self.min = self.min.min(value); self.max = self.max.max(value); self.count += 1;
    }
    fn mean(self) -> f64 { self.sum / self.count.max(1) as f64 }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase { Before, Trial, After, Holding, Resting }
#[derive(Debug, PartialEq, Eq)]
pub enum Action { None, Apply(String), Restore }

pub struct Session {
    pub destination: String,
    pub message: String,
    pids: Vec<u32>,
    baseline: Observation,
    candidates: Vec<String>,
    candidate: usize,
    phase: Phase,
    since: Instant,
    last_valid: Instant,
    last_sample: u64,
    window: Window,
    before: Window,
    trial: Window,
    confirmations: usize,
    trial_route: Option<String>,
    history: Vec<History>,
}
impl Session {
    pub fn start(destination: String, mut candidates: Vec<String>, sample: &ProcessPing, now: Instant) -> Result<Self, String> {
        crate::routes::destination(&destination)?;
        if sample.sampled_at.is_none_or(|at| now.saturating_duration_since(at) > Duration::from_secs(10)) || sample.pids.is_empty() {
            return Err("Automático exige uma amostra TCP/ICMP atual do processo selecionado.".into());
        }
        let endpoint = sample.endpoints.iter().find(|e| e.destination == destination).ok_or("Escolha um IP TCP observado atualmente; isso não identifica servidor de partida.")?;
        if !matches!(endpoint.outcome, EchoOutcome::Reply(_)) { return Err("Automático exige resposta ICMP válida, não timeout/indisponibilidade.".into()); }
        let baseline = endpoint.route.clone().ok_or("Rota efetiva da sonda não foi confirmada; não iniciar testes.")?;
        candidates.sort(); candidates.dedup();
        if candidates.len() < 2 || !candidates.contains(&baseline.candidate) {
            return Err("Sem alternativa medida: selecione a saída atual e ao menos outro gateway disponível da mesma família. Nenhuma rota alterada.".into());
        }
        candidates.retain(|id| *id != baseline.candidate);
        Ok(Self { destination, message: "Aprendendo referência atual; sem previsão para gateways nunca medidos.".into(), pids: sample.pids.clone(), baseline,
            candidates, candidate: 0, phase: Phase::Before, since: now, last_valid: now, last_sample: sample.sample_id,
            window: Window::default(), before: Window::default(), trial: Window::default(), confirmations: 0, trial_route: None, history: Vec::new() })
    }
    fn transition(&mut self, phase: Phase, now: Instant) {
        self.phase = phase; self.since = now; self.window = Window::default(); self.trial_route = None;
    }
    pub fn barrier(&mut self, now: Instant) {
        self.since = now;
        self.last_valid = now;
    }
    pub fn expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_valid) > PHASE_LIMIT
            || (!matches!(self.phase, Phase::Holding | Phase::Resting) && now.saturating_duration_since(self.since) > PHASE_LIMIT)
    }
    fn remember(&mut self, route: &Observation, value: f64, now: Instant) {
        let index = self.history.iter().position(|h| h.route.id == route.id).unwrap_or_else(|| {
            self.history.push(History { route: route.clone(), model: flow::Flow::new(0x5241504f), mean: value,
                deviation: 0.0, residual: 0.0, last: value, previous: value, at: now }); self.history.len() - 1
        });
        self.history[index].add(value, now);
    }
    pub fn history(&self, now: Instant) -> Vec<String> {
        self.history.iter().map(|h| {
            let prediction = if h.model.samples >= 6 {
                format!("previsão neural próxima ICMP {:.1} ms; incerteza descritiva ≥{:.1} ms", h.model.predict(&h.input()).clamp(0.0, 1.0) * 250.0, h.residual.max(h.deviation).max(1.0))
            } else { "previsão indisponível: poucas medições reais".into() };
            format!("{} · if {} · origem {} · {} amostras · média móvel {:.1} ms · {} · há {} s", h.route.gateway, h.route.interface, h.route.source, h.model.samples, h.mean, prediction, now.saturating_duration_since(h.at).as_secs())
        }).collect()
    }
    pub fn observe(&mut self, sample: &ProcessPing, now: Instant) -> Result<Action, String> {
        if sample.pids != self.pids { return Err("Alvo/PIDs mudaram; automático suspenso e referência solicitada.".into()); }
        if sample.sample_id <= self.last_sample { return Ok(Action::None); }
        self.last_sample = sample.sample_id;
        let at = sample.sampled_at.ok_or("Amostra sem timestamp")?;
        if now.saturating_duration_since(at) > Duration::from_secs(10) { return Err("Amostra ICMP obsoleta; automático suspenso.".into()); }
        if at < self.since { return Ok(Action::None); }
        let endpoint = sample.endpoints.iter().find(|e| e.destination == self.destination).ok_or("Destino deixou de ser observado no processo; automático suspenso.")?;
        let route = endpoint.route.as_ref().ok_or("Rota mudou durante a sonda ou consulta falhou; janela inválida.")?;
        let baseline_phase = matches!(self.phase, Phase::Before | Phase::After | Phase::Resting);
        if baseline_phase && route.id != self.baseline.id { return Err("Referência de rota mudou externamente; não atribuir ganho nem substituir terceiros.".into()); }
        if !baseline_phase {
            if route.candidate != self.candidates[self.candidate] { return Ok(Action::None); }
            if self.trial_route.as_ref().is_some_and(|id| *id != route.id) { return Err("Rota/origem do candidato mudou; janela inválida.".into()); }
            self.trial_route = Some(route.id.clone());
        }
        let ms = match endpoint.outcome { EchoOutcome::Reply(ms) => ms as f64,
            _ => return Err("Timeout/ICMP indisponível: não é 0 ms; restaurando referência, sem promoção.".into()) };
        self.last_valid = now;
        self.remember(route, ms, now);
        if self.phase == Phase::Resting {
            if now.saturating_duration_since(self.since) < REST { return Ok(Action::None); }
            // Predictions order only candidates with recent real measurements, never certify a gain.
            let history = &self.history;
            self.candidates.sort_by(|a, b| {
                let score = |id: &String| history.iter().find(|h| &h.route.candidate == id && h.model.samples >= 6 && now.saturating_duration_since(h.at) < Duration::from_secs(120))
                    .map(|h| h.model.predict(&h.input()));
                match (score(a), score(b)) {
                    (Some(a_score), Some(b_score)) => a_score.total_cmp(&b_score).then_with(|| a.cmp(b)),
                    (None, Some(_)) => std::cmp::Ordering::Less,
                    (Some(_), None) => std::cmp::Ordering::Greater,
                    (None, None) => a.cmp(b),
                }
            });
            self.candidate = 0; self.transition(Phase::Before, now);
            self.message = "Revalidando referência contemporânea; aprendizado contínuo.".into(); return Ok(Action::None);
        }
        self.window.add(ms);
        if self.window.count < WINDOW { return Ok(Action::None); }
        match self.phase {
            Phase::Before => {
                self.before = self.window; self.transition(Phase::Trial, now);
                self.message = "Teste consentido de gateway; pode desconectar. Ainda sem ganho confirmado.".into();
                Ok(Action::Apply(self.candidates[self.candidate].clone()))
            }
            Phase::Trial => {
                self.trial = self.window; self.transition(Phase::After, now);
                self.message = "Restaurando referência para comparação pareada; não há vitória declarada.".into(); Ok(Action::Restore)
            }
            Phase::After => {
                let after = self.window;
                let threshold = (self.before.mean().min(after.mean()) * 0.10).max(3.0);
                let stable = (self.before.mean() - after.mean()).abs() <= threshold;
                let improved = self.trial.max + threshold < self.before.min.min(after.min);
                if stable && improved {
                    self.confirmations += 1;
                    if self.confirmations >= 2 {
                        self.transition(Phase::Holding, now);
                        self.message = "Candidato confirmado em 2 pares ICMP locais; aplicado temporariamente, revalidação em 60 s. Não prova causalidade nem ping do jogo.".into();
                        return Ok(Action::Apply(self.candidates[self.candidate].clone()));
                    }
                    self.transition(Phase::Before, now);
                    self.message = "Uma comparação favorável; falta confirmação contemporânea independente.".into();
                } else {
                    self.confirmations = 0; self.candidate += 1;
                    if self.candidate == self.candidates.len() {
                        self.candidate = 0; self.transition(Phase::Resting, now);
                        self.message = "Sem ganho ICMP consistente; referência mantida. Aguardando 30 s para reaprender; variação não prova causalidade.".into();
                    } else { self.transition(Phase::Before, now); self.message = "Candidato não confirmado; referência restaurada antes do próximo teste consentido.".into(); }
                }
                Ok(Action::None)
            }
            Phase::Holding => {
                let worse = self.window.mean() > self.before.mean() + (self.before.mean() * 0.10).max(3.0);
                if worse || now.saturating_duration_since(self.since) >= HOLD {
                    self.confirmations = 0; self.transition(Phase::Resting, now);
                    self.message = "Referência restaurada para revalidação contínua ou regressão ICMP; nenhuma garantia de ganho.".into();
                    Ok(Action::Restore)
                } else { self.window = Window::default(); Ok(Action::None) }
            }
            Phase::Resting => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_ping::EndpointPing;
    fn sample(id: u64, at: Instant, gateway: &str, ms: Option<u32>) -> ProcessPing {
        ProcessPing { sample_id: id, sampled_at: Some(at), pids: vec![42], summary: String::new(), endpoints: vec![EndpointPing {
            destination: "192.0.2.42".into(), outcome: ms.map_or(EchoOutcome::Timeout, EchoOutcome::Reply),
            route: Some(Observation { id: gateway.into(), candidate: gateway.into(), interface: 2, gateway: gateway.into(), source: "192.0.2.2".into() }),
        }] }
    }
    #[test]
    fn no_alternative_no_action_and_requires_observed_reply() {
        let now = Instant::now(); let first = sample(1, now, "a", Some(40));
        assert!(Session::start("192.0.2.42".into(), vec!["a".into()], &first, now).is_err());
        assert!(Session::start("192.0.2.43".into(), vec!["a".into(), "b".into()], &first, now).is_err());
        assert!(Session::start("192.0.2.42".into(), vec!["a".into(), "b".into()], &sample(1, now, "a", None), now).is_err());
    }
    #[test]
    fn deduplicates_cached_samples_and_rejects_stale_pid_and_timeout() {
        let now = Instant::now(); let first = sample(1, now, "a", Some(40));
        let mut session = Session::start("192.0.2.42".into(), vec!["a".into(), "b".into()], &first, now).unwrap();
        for _ in 0..10 { assert_eq!(session.observe(&first, now).unwrap(), Action::None); }
        assert!(session.history.is_empty());
        let mut changed = sample(2, now, "a", Some(40)); changed.pids = vec![99];
        assert!(session.observe(&changed, now).is_err());
        assert!(session.observe(&sample(3, now, "a", None), now).is_err());
        assert!(session.observe(&sample(4, now, "a", Some(40)), now + Duration::from_secs(11)).is_err());
    }
    #[test]
    fn two_paired_confirmations_precede_promotion_and_revalidate_continuously() {
        let start = Instant::now(); let mut now = start; let mut id = 1;
        let mut s = Session::start("192.0.2.42".into(), vec!["a".into(), "b".into()], &sample(id, now, "a", Some(60)), now).unwrap();
        for round in 0..2 {
            for (route, ms, action) in [("a", 60, Action::Apply("b".into())), ("b", 20, Action::Restore), ("a", 60, if round == 0 { Action::None } else { Action::Apply("b".into()) })] {
                for index in 0..3 { now += Duration::from_secs(3); id += 1;
                    let result = s.observe(&sample(id, now, route, Some(ms)), now).unwrap();
                    assert_eq!(result, if index == 2 { action.clone_for_test() } else { Action::None });
                }
            }
        }
        assert!(s.phase == Phase::Holding);
        now += HOLD;
        for index in 0..3 { id += 1; now += Duration::from_secs(1);
            let action = s.observe(&sample(id, now, "b", Some(20)), now).unwrap();
            if index == 2 { assert_eq!(action, Action::Restore); }
        }
        assert!(s.phase == Phase::Resting);
    }
    #[test]
    fn noise_is_not_a_win_and_neural_history_adapts_without_inventing_other_routes() {
        let now = Instant::now(); let first = sample(1, now, "a", Some(40));
        let mut s = Session::start("192.0.2.42".into(), vec!["a".into(), "b".into()], &first, now).unwrap();
        let route = first.endpoints[0].route.as_ref().unwrap();
        for _ in 0..1000 { s.remember(route, 20.0, now); }
        let low = s.history[0].model.predict(&s.history[0].input());
        for _ in 0..1000 { s.remember(route, 100.0, now); }
        assert!(s.history[0].model.predict(&s.history[0].input()) > low + 0.1);
        assert_eq!(s.history.len(), 1); assert!(s.history(now)[0].contains("previsão neural"));
        let before = Window { sum: 120.0, min: 39.0, max: 41.0, count: 3 };
        let trial = Window { sum: 117.0, min: 38.0, max: 40.0, count: 3 };
        assert!(!(trial.max + (before.mean() * 0.1).max(3.0) < before.min));
    }
    impl Action {
        fn clone_for_test(&self) -> Self { match self { Self::None => Self::None, Self::Restore => Self::Restore, Self::Apply(id) => Self::Apply(id.clone()) } }
    }
}
