//! Motor de dosimetria do HearPhant (seção 8 da documentação).
//!
//! Biblioteca pura: não conhece áudio, D-Bus, disco nem interface. Recebe
//! segmentos com a medição já classificada e integra energia ponderada A em
//! Pa²·h sobre uma janela móvel.
//!
//! A garantia central (RF-008) está nos tipos: só [`Measurement::Acoustic`]
//! carrega um nível em dBA, e só ele pode produzir dose. Volume percentual,
//! dBFS ou LUFS entram como [`Measurement::Digital`] e nunca viram exposição.

use std::fmt;

/// Pressão sonora de referência, 20 µPa.
pub const P0_PA: f64 = 20e-6;

/// Janela móvel padrão: 7 × 24 h (seção 8.2).
pub const WINDOW_MS: i64 = 168 * HOUR_MS;

pub const HOUR_MS: i64 = 3_600_000;

/// Instante UTC em milissegundos desde a época Unix.
pub type UtcMillis = i64;

/// Perfis de referência OMS/UIT (seção 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceMode {
    /// Modo 1: 80 dBA por 40 h em 7 dias (≈1,6 Pa²·h).
    Adult,
    /// Modo 2: 75 dBA por 40 h em 7 dias (≈0,51 Pa²·h).
    Conservative,
}

impl ReferenceMode {
    pub fn reference_level_dba(self) -> f64 {
        match self {
            ReferenceMode::Adult => 80.0,
            ReferenceMode::Conservative => 75.0,
        }
    }

    /// Energia de referência semanal em Pa²·h, derivada do nível × 40 h
    /// (sem o arredondamento de apresentação de 1,6 / 0,51).
    pub fn reference_energy_pa2h(self) -> f64 {
        energy_pa2h(self.reference_level_dba(), 40.0)
    }
}

/// Como um nível acústico foi obtido (seção 9.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AcousticSource {
    /// Cadeia calibrada com perfil válido.
    Calibrated,
    /// Modelo validado em condições limitadas, com incerteza em dB.
    Estimated { uncertainty_db: f64 },
}

/// O que foi observado durante um segmento.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Measurement {
    /// Nível equivalente ponderado A no ouvido, fundamentado acusticamente.
    Acoustic { laeq_dba: f64, source: AcousticSource },
    /// Apenas grandezas digitais; não permite dose física.
    Digital { rms_dbfs: Option<f64> },
    /// Captura ausente (falha, daemon parado). Não é silêncio.
    Missing,
    /// Dado descartado por inconsistência.
    Invalid,
}

/// Situação de cobertura de um segmento (seção 8.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageStatus {
    Observed,
    Missing,
    Unverified,
    Invalid,
}

impl CoverageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CoverageStatus::Observed => "observed",
            CoverageStatus::Missing => "missing",
            CoverageStatus::Unverified => "unverified",
            CoverageStatus::Invalid => "invalid",
        }
    }
}

impl Measurement {
    pub fn coverage(&self) -> CoverageStatus {
        match self {
            Measurement::Acoustic { .. } => CoverageStatus::Observed,
            Measurement::Digital { .. } => CoverageStatus::Unverified,
            Measurement::Missing => CoverageStatus::Missing,
            Measurement::Invalid => CoverageStatus::Invalid,
        }
    }
}

/// Intervalo `[start, start + duration)` com nível constante.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub start: UtcMillis,
    pub duration_ms: i64,
    pub measurement: Measurement,
}

impl Segment {
    pub fn end(&self) -> UtcMillis {
        self.start + self.duration_ms
    }

    /// Milissegundos deste segmento dentro de `[from, to)`.
    fn overlap_ms(&self, from: UtcMillis, to: UtcMillis) -> i64 {
        (self.end().min(to) - self.start.max(from)).max(0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TimelineError {
    /// Duração não positiva.
    EmptySegment,
    /// Nível não finito ou fora de uma faixa fisicamente plausível.
    ImplausibleLevel(f64),
    /// O segmento começa antes do fim do anterior: aceitar contaria a mesma
    /// exposição duas vezes (seção 10.4, T-DOS-07).
    Overlap { previous_end: UtcMillis, start: UtcMillis },
}

impl fmt::Display for TimelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimelineError::EmptySegment => write!(f, "segmento com duração não positiva"),
            TimelineError::ImplausibleLevel(l) => write!(f, "nível acústico implausível: {l} dBA"),
            TimelineError::Overlap { previous_end, start } => write!(
                f,
                "segmento inicia em {start} antes do fim do anterior ({previous_end})"
            ),
        }
    }
}

impl std::error::Error for TimelineError {}

/// Sequência ordenada e sem sobreposição de segmentos.
#[derive(Debug, Clone, Default)]
pub struct Timeline {
    segments: Vec<Segment>,
}

impl Timeline {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    pub fn push(&mut self, segment: Segment) -> Result<(), TimelineError> {
        if segment.duration_ms <= 0 {
            return Err(TimelineError::EmptySegment);
        }
        if let Measurement::Acoustic { laeq_dba, .. } = segment.measurement {
            if !laeq_dba.is_finite() || !(-20.0..=200.0).contains(&laeq_dba) {
                return Err(TimelineError::ImplausibleLevel(laeq_dba));
            }
        }
        if let Some(last) = self.segments.last() {
            if segment.start < last.end() {
                return Err(TimelineError::Overlap { previous_end: last.end(), start: segment.start });
            }
        }
        self.segments.push(segment);
        Ok(())
    }

    /// Remove segmentos que terminaram antes de `cutoff` (retenção).
    pub fn prune_before(&mut self, cutoff: UtcMillis) {
        self.segments.retain(|s| s.end() > cutoff);
    }

    /// Dose na janela `[now - window_ms, now)`.
    pub fn dose(&self, now: UtcMillis, window_ms: i64, mode: ReferenceMode) -> DoseReport {
        let from = now - window_ms;
        let mut report = DoseReport {
            window_ms,
            reference_mode: mode,
            energy_pa2h: None,
            dose_percent: None,
            max_uncertainty_db: None,
            observed_ms: 0,
            unverified_ms: 0,
            invalid_ms: 0,
            missing_ms: 0,
        };

        let mut energy = 0.0;
        let mut covered_ms = 0;
        for seg in &self.segments {
            // Só a parcela dentro da janela conta (T-DOS-06).
            let ms = seg.overlap_ms(from, now);
            if ms == 0 {
                continue;
            }
            covered_ms += ms;
            match seg.measurement {
                Measurement::Acoustic { laeq_dba, source } => {
                    report.observed_ms += ms;
                    energy += energy_pa2h(laeq_dba, ms as f64 / HOUR_MS as f64);
                    if let AcousticSource::Estimated { uncertainty_db } = source {
                        let max = report.max_uncertainty_db.get_or_insert(0.0);
                        *max = max.max(uncertainty_db);
                    }
                }
                Measurement::Digital { .. } => report.unverified_ms += ms,
                Measurement::Invalid => report.invalid_ms += ms,
                Measurement::Missing => report.missing_ms += ms,
            }
        }
        // Tempo sem nenhum segmento também é lacuna, nunca silêncio (UC-08).
        report.missing_ms += window_ms - covered_ms;

        if report.observed_ms > 0 {
            report.energy_pa2h = Some(energy);
            report.dose_percent = Some(100.0 * energy / mode.reference_energy_pa2h());
        }
        report
    }
}

/// Resultado de [`Timeline::dose`]. `dose_percent` é `None` quando não houve
/// nenhum tempo acusticamente fundamentado na janela (T-DOS-08).
#[derive(Debug, Clone, PartialEq)]
pub struct DoseReport {
    pub window_ms: i64,
    pub reference_mode: ReferenceMode,
    pub energy_pa2h: Option<f64>,
    pub dose_percent: Option<f64>,
    /// Maior incerteza entre os segmentos estimados, se houver.
    pub max_uncertainty_db: Option<f64>,
    pub observed_ms: i64,
    pub unverified_ms: i64,
    pub invalid_ms: i64,
    pub missing_ms: i64,
}

impl DoseReport {
    /// Fração da janela com medição acústica fundamentada.
    pub fn coverage_fraction(&self) -> f64 {
        self.observed_ms as f64 / self.window_ms as f64
    }

    /// A dose cobre parte da janela; o restante não é exposição zero.
    pub fn is_partial(&self) -> bool {
        self.observed_ms < self.window_ms
    }
}

/// Energia `p0² · 10^(L/10) · Δt` em Pa²·h (seção 8.1).
pub fn energy_pa2h(laeq_dba: f64, hours: f64) -> f64 {
    P0_PA * P0_PA * 10f64.powf(laeq_dba / 10.0) * hours
}

/// Tempo até 100% da referência a nível constante: `40 · 10^((Lref − L)/10)` h.
pub fn time_to_reference_hours(laeq_dba: f64, mode: ReferenceMode) -> f64 {
    40.0 * 10f64.powf((mode.reference_level_dba() - laeq_dba) / 10.0)
}

/// Média energética de níveis em dB com o mesmo peso (nunca média aritmética).
pub fn energy_mean_db(levels: &[f64]) -> Option<f64> {
    if levels.is_empty() {
        return None;
    }
    let mean = levels.iter().map(|l| 10f64.powf(l / 10.0)).sum::<f64>() / levels.len() as f64;
    Some(10.0 * mean.log10())
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: UtcMillis = 1_800_000_000_000;

    fn calibrated(laeq_dba: f64) -> Measurement {
        Measurement::Acoustic { laeq_dba, source: AcousticSource::Calibrated }
    }

    fn hours(h: f64) -> i64 {
        (h * HOUR_MS as f64).round() as i64
    }

    /// Timeline com segmentos consecutivos começando em T0; devolve o fim.
    fn build(parts: &[(f64, Measurement)]) -> (Timeline, UtcMillis) {
        let mut t = Timeline::new();
        let mut at = T0;
        for &(h, m) in parts {
            let duration_ms = hours(h);
            t.push(Segment { start: at, duration_ms, measurement: m }).unwrap();
            at += duration_ms;
        }
        (t, at)
    }

    fn dose_at_end(parts: &[(f64, Measurement)], mode: ReferenceMode) -> DoseReport {
        let (t, end) = build(parts);
        t.dose(end, WINDOW_MS, mode)
    }

    fn approx(actual: f64, expected: f64, tol: f64) {
        assert!((actual - expected).abs() <= tol, "esperado {expected} ± {tol}, obtido {actual}");
    }

    #[test]
    fn reference_energies_match_documentation() {
        approx(ReferenceMode::Adult.reference_energy_pa2h(), 1.6, 1e-12);
        approx(ReferenceMode::Conservative.reference_energy_pa2h(), 0.51, 0.005);
    }

    #[test]
    fn equivalent_times_match_table_3_2() {
        approx(time_to_reference_hours(80.0, ReferenceMode::Adult), 40.0, 1e-9);
        approx(time_to_reference_hours(85.0, ReferenceMode::Adult), 12.649, 0.001);
        approx(time_to_reference_hours(90.0, ReferenceMode::Adult), 4.0, 1e-9);
        approx(time_to_reference_hours(100.0, ReferenceMode::Adult) * 60.0, 24.0, 1e-9);
    }

    #[test]
    fn t_dos_01_80_dba_for_40_h_is_100_percent() {
        let r = dose_at_end(&[(40.0, calibrated(80.0))], ReferenceMode::Adult);
        approx(r.dose_percent.unwrap(), 100.0, 1e-9);
    }

    #[test]
    fn t_dos_02_90_dba_for_4_h_is_100_percent() {
        let r = dose_at_end(&[(4.0, calibrated(90.0))], ReferenceMode::Adult);
        approx(r.dose_percent.unwrap(), 100.0, 1e-9);
    }

    #[test]
    fn t_dos_03_mixed_segments_about_75_percent() {
        let r = dose_at_end(&[(2.0, calibrated(90.0)), (10.0, calibrated(80.0))], ReferenceMode::Adult);
        approx(r.dose_percent.unwrap(), 75.0, 1e-9);
    }

    #[test]
    fn t_dos_04_50_dba_for_4_h_is_tiny() {
        let r = dose_at_end(&[(4.0, calibrated(50.0))], ReferenceMode::Adult);
        let dose = r.dose_percent.unwrap();
        approx(dose, 0.01, 1e-9);
        assert!(dose < 1.0);
    }

    #[test]
    fn t_dos_05_energy_sum_not_arithmetic_db_mean() {
        // 1 h a 70 + 1 h a 90: média aritmética seria 80 dBA por 2 h = 5%.
        let r = dose_at_end(&[(1.0, calibrated(70.0)), (1.0, calibrated(90.0))], ReferenceMode::Adult);
        approx(r.dose_percent.unwrap(), 25.25, 1e-9);
        approx(energy_mean_db(&[70.0, 90.0]).unwrap(), 87.0329, 1e-4);
    }

    #[test]
    fn t_dos_06_only_portion_inside_window_counts() {
        // 10 h a 90 dBA; a janela começa na metade do segmento.
        let (t, end) = build(&[(10.0, calibrated(90.0))]);
        let now = end + WINDOW_MS - hours(5.0);
        let r = t.dose(now, WINDOW_MS, ReferenceMode::Adult);
        approx(r.dose_percent.unwrap(), 125.0, 1e-9);
        assert_eq!(r.observed_ms, hours(5.0));
    }

    #[test]
    fn t_dos_06_segment_fully_outside_window_is_ignored() {
        let (t, end) = build(&[(1.0, calibrated(100.0))]);
        let r = t.dose(end + WINDOW_MS, WINDOW_MS, ReferenceMode::Adult);
        assert_eq!(r.dose_percent, None);
        assert_eq!(r.missing_ms, WINDOW_MS);
    }

    #[test]
    fn t_dos_07_replayed_segment_is_rejected_not_double_counted() {
        let (mut t, _) = build(&[(1.0, calibrated(90.0))]);
        let again = t.segments()[0];
        assert!(matches!(t.push(again), Err(TimelineError::Overlap { .. })));
        assert_eq!(t.segments().len(), 1);
    }

    #[test]
    fn t_dos_08_uncalibrated_has_no_dose() {
        let r = dose_at_end(&[(40.0, Measurement::Digital { rms_dbfs: Some(-12.0) })], ReferenceMode::Adult);
        assert_eq!(r.dose_percent, None);
        assert_eq!(r.energy_pa2h, None);
        assert_eq!(r.unverified_ms, hours(40.0));
    }

    #[test]
    fn t_dos_09_missing_capture_is_a_gap_not_silence() {
        let r = dose_at_end(
            &[(2.0, calibrated(90.0)), (3.0, Measurement::Missing), (1.0, calibrated(90.0))],
            ReferenceMode::Adult,
        );
        approx(r.dose_percent.unwrap(), 75.0, 1e-9);
        assert!(r.is_partial());
        assert_eq!(r.observed_ms, hours(3.0));
        assert_eq!(r.missing_ms, WINDOW_MS - hours(6.0) + hours(3.0));
        approx(r.coverage_fraction(), 3.0 / 168.0, 1e-12);
    }

    #[test]
    fn t_dos_10_conservative_mode_75_dba_for_40_h() {
        let r = dose_at_end(&[(40.0, calibrated(75.0))], ReferenceMode::Conservative);
        approx(r.dose_percent.unwrap(), 100.0, 1e-9);
    }

    #[test]
    fn mixed_calibrated_and_digital_reports_partial_coverage() {
        let r = dose_at_end(
            &[(4.0, calibrated(90.0)), (4.0, Measurement::Digital { rms_dbfs: None })],
            ReferenceMode::Adult,
        );
        approx(r.dose_percent.unwrap(), 100.0, 1e-9);
        assert_eq!(r.unverified_ms, hours(4.0));
        assert!(r.is_partial());
    }

    #[test]
    fn estimated_segments_report_uncertainty() {
        let est = |u| Measurement::Acoustic { laeq_dba: 85.0, source: AcousticSource::Estimated { uncertainty_db: u } };
        let r = dose_at_end(&[(1.0, est(2.0)), (1.0, est(4.5))], ReferenceMode::Adult);
        assert_eq!(r.max_uncertainty_db, Some(4.5));
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        let mut t = Timeline::new();
        assert_eq!(
            t.push(Segment { start: T0, duration_ms: 0, measurement: calibrated(80.0) }),
            Err(TimelineError::EmptySegment)
        );
        assert!(matches!(
            t.push(Segment { start: T0, duration_ms: 1000, measurement: calibrated(f64::NAN) }),
            Err(TimelineError::ImplausibleLevel(_))
        ));
    }

    #[test]
    fn prune_keeps_segments_crossing_cutoff() {
        let (mut t, _) = build(&[(1.0, calibrated(80.0)), (1.0, calibrated(80.0))]);
        t.prune_before(T0 + hours(1.5));
        assert_eq!(t.segments().len(), 1);
    }
}
