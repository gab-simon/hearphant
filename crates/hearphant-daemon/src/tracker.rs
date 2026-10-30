//! Converte o estado observado em segmentos de atividade (≤ 1 min).
//!
//! Durações vêm do relógio monotônico; só o início do segmento usa o relógio
//! de parede (seção 11.2), então ajustes de hora não criam nem apagam tempo.

use std::time::{Duration, Instant};

use hearphant_dose::energy_pa2h;
use hearphant_storage::{ActivitySegment, ActivitySource, DeviceKey, SegmentEstimate, UtcMillis};

pub const SEGMENT_MAX: Duration = Duration::from_secs(60);

/// Perfil de estimativa ativo no segmento.
#[derive(Debug, Clone, PartialEq)]
pub struct EstimateCtx {
    pub profile_id: String,
    pub uncertainty_db: f64,
}

#[derive(Debug)]
struct Open {
    device: DeviceKey,
    source: ActivitySource,
    start_utc: UtcMillis,
    last: Instant,
    duration: Duration,
    active: Duration,
    signal_energy: f64,
    signal: Duration,
    estimate: Option<EstimateCtx>,
    est_energy: f64,
    est_covered: Duration,
}

impl Open {
    fn fresh(device: DeviceKey, source: ActivitySource, estimate: Option<EstimateCtx>, now: Instant, now_utc: UtcMillis) -> Self {
        Open {
            device,
            source,
            start_utc: now_utc,
            last: now,
            duration: Duration::ZERO,
            active: Duration::ZERO,
            signal_energy: 0.0,
            signal: Duration::ZERO,
            estimate,
            est_energy: 0.0,
            est_covered: Duration::ZERO,
        }
    }

    fn to_segment(&self) -> ActivitySegment {
        let signal_ms = self.signal.as_millis() as i64;
        ActivitySegment {
            start: self.start_utc,
            duration_ms: self.duration.as_millis() as i64,
            active_ms: self.active.as_millis() as i64,
            signal_energy: (self.source == ActivitySource::Signal).then_some(self.signal_energy),
            signal_ms,
            source: self.source,
            estimate: self.estimate.as_ref().filter(|_| self.est_covered > Duration::ZERO).map(|e| SegmentEstimate {
                energy_pa2h: self.est_energy,
                covered_ms: self.est_covered.as_millis() as i64,
                uncertainty_db: e.uncertainty_db,
                profile_id: e.profile_id.clone(),
            }),
        }
    }
}

/// Segmento encerrado, pronto para gravar.
pub type Closed = (DeviceKey, ActivitySegment);

#[derive(Debug, Default)]
pub struct Tracker {
    open: Option<Open>,
    /// Estado de atividade aplicado ao tempo até o próximo evento.
    active_now: bool,
}

impl Tracker {
    pub fn active_now(&self) -> bool {
        self.active_now
    }

    /// Atribui o tempo desde o último evento ao estado `active` e fecha o
    /// segmento se atingiu [`SEGMENT_MAX`].
    fn advance_with(&mut self, now: Instant, now_utc: UtcMillis, active: bool) -> Option<Closed> {
        let open = self.open.as_mut()?;
        let elapsed = now.saturating_duration_since(open.last);
        open.last = now;
        open.duration += elapsed;
        if active {
            open.active += elapsed;
        }
        if open.duration >= SEGMENT_MAX {
            let closed = (open.device.clone(), open.to_segment());
            *open = Open::fresh(open.device.clone(), open.source, open.estimate.clone(), now, now_utc);
            return Some(closed);
        }
        None
    }

    /// Avança o relógio mantendo o estado atual (timer periódico).
    pub fn tick(&mut self, now: Instant, now_utc: UtcMillis) -> Option<Closed> {
        self.advance_with(now, now_utc, self.active_now)
    }

    /// Muda o contexto observado. `device = None` encerra o monitoramento
    /// (o intervalo seguinte é lacuna, não silêncio).
    pub fn set_context(
        &mut self,
        device: Option<DeviceKey>,
        source: ActivitySource,
        estimate: Option<EstimateCtx>,
        active: bool,
        now: Instant,
        now_utc: UtcMillis,
    ) -> Vec<Closed> {
        let mut closed: Vec<Closed> = self.tick(now, now_utc).into_iter().collect();
        let same = match (&self.open, &device) {
            (Some(o), Some(d)) => o.device == *d && o.source == source && o.estimate == estimate,
            (None, None) => true,
            _ => false,
        };
        if !same {
            closed.extend(self.close());
            self.open = device.map(|device| Open::fresh(device, source, estimate, now, now_utc));
        }
        self.active_now = active && self.open.is_some();
        closed
    }

    /// Quadro de nível do sinal: descreve o intervalo que acabou de passar.
    /// `laeq` é o nível estimado do quadro (`None` = silêncio/mudo), usado
    /// só se o segmento tem perfil de estimativa.
    pub fn signal_frame(
        &mut self,
        at: Instant,
        now_utc: UtcMillis,
        frame: Duration,
        mean_square: f64,
        audible: bool,
        laeq: Option<f64>,
    ) -> Option<Closed> {
        if self.open.as_ref().is_none_or(|o| o.source != ActivitySource::Signal) {
            return None;
        }
        let closed = self.advance_with(at, now_utc, audible);
        if let Some(open) = self.open.as_mut() {
            if audible {
                open.signal_energy += mean_square * frame.as_millis() as f64;
                open.signal += frame;
            }
            if open.estimate.is_some() {
                open.est_covered += frame;
                if let Some(l) = laeq {
                    open.est_energy += energy_pa2h(l, frame.as_secs_f64() / 3600.0);
                }
            }
        }
        self.active_now = audible;
        closed
    }

    /// Encerra o segmento aberto (desligamento, troca de contexto).
    pub fn close(&mut self) -> Option<Closed> {
        let open = self.open.take()?;
        self.active_now = false;
        (open.duration > Duration::ZERO).then(|| (open.device.clone(), open.to_segment()))
    }

    /// Descarta o segmento aberto sem gravar (histórico apagado).
    pub fn discard(&mut self, now: Instant, now_utc: UtcMillis) {
        if let Some(open) = self.open.as_mut() {
            open.start_utc = now_utc;
            open.last = now;
            open.duration = Duration::ZERO;
            open.active = Duration::ZERO;
            open.signal_energy = 0.0;
            open.signal = Duration::ZERO;
            open.est_energy = 0.0;
            open.est_covered = Duration::ZERO;
        }
    }

    /// Segmento aberto como está agora, para somar aos totais publicados.
    pub fn pending(&self) -> Option<(&DeviceKey, ActivitySegment)> {
        self.open.as_ref().map(|o| (&o.device, o.to_segment()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(n: &str) -> DeviceKey {
        DeviceKey { stable_name: n.into(), display_name: n.into(), kind: "unknown".into() }
    }

    const S: Duration = Duration::from_secs(1);

    #[test]
    fn stream_mode_counts_active_time_between_events() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        assert!(t.set_context(Some(dev("a")), ActivitySource::Stream, None, true, t0, 0).is_empty());
        t.set_context(Some(dev("a")), ActivitySource::Stream, None, false, t0 + 10 * S, 10_000);
        t.tick(t0 + 15 * S, 15_000);
        let (_, seg) = t.close().unwrap();
        assert_eq!(seg.duration_ms, 15_000);
        assert_eq!(seg.active_ms, 10_000);
        assert_eq!(seg.signal_energy, None);
    }

    #[test]
    fn device_change_closes_segment() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.set_context(Some(dev("a")), ActivitySource::Stream, None, true, t0, 0);
        let closed = t.set_context(Some(dev("b")), ActivitySource::Stream, None, true, t0 + 5 * S, 5_000);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].0, dev("a"));
        assert_eq!(closed[0].1.active_ms, 5_000);
        assert_eq!(t.pending().unwrap().0, &dev("b"));
    }

    #[test]
    fn segments_are_split_every_minute() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.set_context(Some(dev("a")), ActivitySource::Stream, None, true, t0, 0);
        let (_, seg) = t.tick(t0 + 61 * S, 61_000).unwrap();
        assert_eq!(seg.duration_ms, 61_000);
        assert!(t.tick(t0 + 70 * S, 70_000).is_none());
        let (_, rest) = t.close().unwrap();
        assert_eq!(rest.start, 61_000);
        assert_eq!(rest.duration_ms, 9_000);
    }

    #[test]
    fn signal_frames_drive_activity_and_energy() {
        let t0 = Instant::now();
        let frame = Duration::from_millis(250);
        let mut t = Tracker::default();
        t.set_context(Some(dev("a")), ActivitySource::Signal, None, false, t0, 0);
        t.signal_frame(t0 + frame, 250, frame, 0.01, true, None);
        t.signal_frame(t0 + 2 * frame, 500, frame, 0.0, false, None);
        let (_, seg) = t.close().unwrap();
        assert_eq!(seg.duration_ms, 500);
        assert_eq!(seg.active_ms, 250);
        assert_eq!(seg.signal_ms, 250);
        assert!((seg.signal_energy.unwrap() - 2.5).abs() < 1e-12);
    }

    #[test]
    fn estimated_frames_accumulate_acoustic_energy() {
        let t0 = Instant::now();
        let ctx = EstimateCtx { profile_id: "p".into(), uncertainty_db: 5.0 };
        let mut t = Tracker::default();
        t.set_context(Some(dev("a")), ActivitySource::Signal, Some(ctx), false, t0, 0);
        // 1 h a 80 dBA em quadros de 1 s, seguida de 10 s de silêncio.
        let mut closed = Vec::new();
        for i in 1..=3610u32 {
            let loud = i <= 3600;
            closed.extend(t.signal_frame(t0 + S * i, i as i64 * 1000, S, 0.01, loud, loud.then_some(80.0)));
        }
        closed.extend(t.close());
        let estimates: Vec<_> = closed.iter().map(|(_, seg)| seg.estimate.clone().unwrap()).collect();
        let energy: f64 = estimates.iter().map(|e| e.energy_pa2h).sum();
        let covered: i64 = estimates.iter().map(|e| e.covered_ms).sum();
        // 80 dBA por 1 h = 1/40 da referência adulta (1,6 Pa²·h).
        assert!((energy - 0.04).abs() < 1e-9, "{energy}");
        assert_eq!(covered, 3_610_000, "silêncio também é tempo coberto");
        assert!(estimates.iter().all(|e| e.profile_id == "p"));
    }

    #[test]
    fn profile_change_closes_segment() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        let a = EstimateCtx { profile_id: "a".into(), uncertainty_db: 5.0 };
        let b = EstimateCtx { profile_id: "b".into(), uncertainty_db: 10.0 };
        t.set_context(Some(dev("x")), ActivitySource::Signal, Some(a), false, t0, 0);
        let closed = t.set_context(Some(dev("x")), ActivitySource::Signal, Some(b), false, t0 + S, 1000);
        assert_eq!(closed.len(), 1);
    }

    #[test]
    fn no_device_means_gap_not_silence() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.set_context(Some(dev("a")), ActivitySource::Stream, None, false, t0, 0);
        let closed = t.set_context(None, ActivitySource::Stream, None, false, t0 + 2 * S, 2_000);
        assert_eq!(closed.len(), 1);
        assert!(t.tick(t0 + 100 * S, 100_000).is_none());
        assert!(t.pending().is_none());
    }

    #[test]
    fn signal_frames_ignored_in_stream_mode() {
        let t0 = Instant::now();
        let mut t = Tracker::default();
        t.set_context(Some(dev("a")), ActivitySource::Stream, None, false, t0, 0);
        assert!(t.signal_frame(t0 + S, 1_000, S, 0.5, true, Some(90.0)).is_none());
        assert!(!t.active_now());
    }
}
