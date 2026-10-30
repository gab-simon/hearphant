//! hearphant-daemon: serviço de sessão `io.github.gab_simon.HearPhant1`.
//!
//! Observa a saída padrão do PipeWire, mede tempo de escuta e nível do sinal
//! digital e guarda agregados em SQLite. Com a análise do sinal ligada e um
//! perfil de estimativa para a saída (`hearphant-calibration`), estima o nível
//! no ouvido e a dose semanal, sempre como `ESTIMATED` e com faixa de
//! incerteza. Não há calibração medida ainda.

mod live;
mod tracker;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{Days, Local, NaiveTime};
use hearphant_audio::level::{self, LevelFrame};
use hearphant_audio::{AudioSnapshot, OutputDevice};
use hearphant_calibration::{self as calibration, KindOverride, Resolution};
use hearphant_dose::ReferenceMode;
use hearphant_storage::{ActivitySource, DeviceKey, Store, UtcMillis, WindowSummary};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use tracker::{Closed, EstimateCtx, Tracker};
use zbus::object_server::SignalContext;
use zbus::zvariant::Value;
use zbus::{fdo, interface};

const BUS_NAME: &str = "io.github.gab_simon.HearPhant1";
const OBJECT_PATH: &str = "/io/github/gab_simon/HearPhant1";
const API_VERSION: u32 = 3;
const WINDOW_HOURS: u32 = 168;
const WINDOW_MS: i64 = WINDOW_HOURS as i64 * 3_600_000;
const DAYS_SHOWN: usize = 7;
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(200);
const MONITOR_RETRY: Duration = Duration::from_secs(3);
/// Recalcula totais e grava segmentos; também limita a taxa de StatusChanged.
const TICK: Duration = Duration::from_secs(15);
const LEVEL_FRAME: Duration = Duration::from_millis(250);
/// Nível mais antigo que isso não é "agora".
const LEVEL_STALE: Duration = Duration::from_secs(1);

const SETTING_PROTECTION: &str = "protection_mode";
const SETTING_SIGNAL: &str = "signal_analysis";
/// Último limiar de dose alertado (0, 80 ou 100), para não repetir alertas.
const SETTING_ALERT: &str = "alert_level";
/// Rearme: o alerta volta a valer quando a dose cai esta margem abaixo.
const ALERT_REARM_MARGIN: f64 = 10.0;
/// Com perfil de estimativa, "há som" é decidido pelo nível no ouvido, não
/// pelo nível digital do conteúdo. Baixo de propósito: com ±5 dB de
/// incerteza, é melhor contar música baixinha do que chamá-la de silêncio.
const AUDIBLE_DBA: f64 = live::FLOOR_DBA;
/// Resolução publicada da dose: 0,0001 ponto percentual.
const DOSE_SCALE: f64 = 10_000.0;
/// Limiares de UX da seção 12.1 (decisões de produto, não marcos médicos).
const NOTICE_PERCENT: f64 = 50.0;
const WARNING_PERCENT: f64 = 80.0;
const EXCEEDED_PERCENT: f64 = 100.0;

fn kind_setting_key(stable_name: &str) -> String {
    format!("device_kind:{stable_name}")
}

type Dict = HashMap<String, Value<'static>>;

fn now_utc() -> UtcMillis {
    chrono::Utc::now().timestamp_millis()
}

/// "Chu2 DSP Analog Stereo" → "Chu2 DSP": o perfil não interessa ao usuário.
fn display_name(out: &OutputDevice) -> String {
    for suffix in [" Analog Stereo", " Digital Stereo (HDMI)", " Analog Surround 5.1", " Mono"] {
        if let Some(stripped) = out.description.strip_suffix(suffix) {
            return stripped.to_owned();
        }
    }
    out.description.clone()
}

fn device_key(out: &OutputDevice) -> DeviceKey {
    DeviceKey { stable_name: out.stable_name.clone(), display_name: display_name(out), kind: out.kind.as_str().into() }
}

/// Totais publicados, arredondados para não emitir sinal por ruído.
#[derive(Debug, Clone, PartialEq, Default)]
struct Totals {
    listening_s: i64,
    monitored_s: i64,
    signal_listening_s: i64,
    mean_signal_dbfs_tenths: Option<i64>,
    monitoring_since: Option<UtcMillis>,
    daily_s: Vec<i64>,
    per_device: Vec<(String, String, i64)>,
    /// Dose estimada (% da referência adulta) em décimos de milésimo, com
    /// faixa: precisão suficiente para "< 1%" no início da semana.
    dose_scaled: Option<i64>,
    dose_low_scaled: Option<i64>,
    dose_high_scaled: Option<i64>,
    uncertainty_db_tenths: Option<i64>,
    estimated_s: i64,
}

impl Totals {
    fn dose_percent(&self) -> Option<f64> {
        self.dose_scaled.map(|t| t as f64 / DOSE_SCALE)
    }
}

/// Situação da estimativa para a saída atual.
#[derive(Debug, Clone, PartialEq)]
struct EstimateStatus {
    profile_label: Option<&'static str>,
    basis: Option<&'static str>,
    reason: Option<&'static str>,
    uncertainty_db: Option<f64>,
    kind_presumed: bool,
    kind_override: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureState {
    Off,
    /// Análise ligada, mas nada tocando: sem captura (e sem ícone de microfone).
    Waiting,
    Capturing,
    Failed,
}

impl CaptureState {
    fn as_str(self) -> &'static str {
        match self {
            CaptureState::Off => "off",
            CaptureState::Waiting => "waiting",
            CaptureState::Capturing => "capturing",
            CaptureState::Failed => "error",
        }
    }
}

/// Estado publicado. Comparável para só emitir sinal quando algo muda.
#[derive(Debug, Clone, PartialEq)]
struct Status {
    mascot_state: &'static str,
    measurement_status: &'static str,
    coverage_status: &'static str,
    device: Option<DeviceStatus>,
    protection_mode: &'static str,
    signal_analysis: bool,
    capture: CaptureState,
    activity_source: ActivitySource,
    totals: Totals,
    estimate: EstimateStatus,
    /// Nível estimado agora na zona vermelha (≥ 90 dB(A), com histerese).
    loud_now: bool,
}

#[derive(Debug, Clone, PartialEq)]
struct DeviceStatus {
    name: String,
    kind: &'static str,
    volume_tenths: Option<i64>,
    muted: bool,
    playing: bool,
    stream_active: bool,
}

impl Status {
    fn to_dict(&self) -> Dict {
        let mut d = Dict::new();
        let t = &self.totals;
        d.insert("api_version".into(), Value::U32(API_VERSION));
        d.insert("mascot_state".into(), Value::from(self.mascot_state));
        d.insert("measurement_status".into(), Value::from(self.measurement_status));
        d.insert("coverage_status".into(), Value::from(self.coverage_status));
        d.insert("window_hours".into(), Value::U32(WINDOW_HOURS));
        d.insert("reference_mode".into(), Value::from("adult"));
        d.insert("protection_mode".into(), Value::from(self.protection_mode));
        d.insert("is_demo".into(), Value::Bool(false));
        // Dose só existe com estimativa acústica na janela; nunca de volume %.
        if let (Some(dose), Some(lo), Some(hi)) = (t.dose_scaled, t.dose_low_scaled, t.dose_high_scaled) {
            d.insert("dose_percent".into(), Value::F64(dose as f64 / DOSE_SCALE));
            d.insert("dose_low_percent".into(), Value::F64(lo as f64 / DOSE_SCALE));
            d.insert("dose_high_percent".into(), Value::F64(hi as f64 / DOSE_SCALE));
            d.insert("coverage_fraction".into(), Value::F64(t.estimated_s as f64 * 1000.0 / WINDOW_MS as f64));
        }
        if let Some(u) = t.uncertainty_db_tenths {
            d.insert("dose_uncertainty_db".into(), Value::F64(u as f64 / 10.0));
        }
        let e = &self.estimate;
        d.insert("estimate_available".into(), Value::Bool(e.profile_label.is_some()));
        if let Some(label) = e.profile_label {
            d.insert("estimate_profile".into(), Value::from(label));
        }
        if let Some(basis) = e.basis {
            d.insert("estimate_basis".into(), Value::from(basis));
        }
        if let Some(reason) = e.reason {
            d.insert("estimate_reason".into(), Value::from(reason));
        }
        if let Some(u) = e.uncertainty_db {
            d.insert("estimate_uncertainty_db".into(), Value::F64(u));
        }
        d.insert("kind_presumed".into(), Value::Bool(e.kind_presumed));
        d.insert("loud_now".into(), Value::Bool(self.loud_now));
        d.insert("device_kind_override".into(), Value::from(e.kind_override));
        d.insert("signal_analysis".into(), Value::Bool(self.signal_analysis));
        d.insert("capture_status".into(), Value::from(self.capture.as_str()));
        d.insert("activity_source".into(), Value::from(self.activity_source.as_str()));
        d.insert("listening_ms".into(), Value::U64(t.listening_s as u64 * 1000));
        d.insert("signal_listening_ms".into(), Value::U64(t.signal_listening_s as u64 * 1000));
        d.insert("monitored_ms".into(), Value::U64(t.monitored_s as u64 * 1000));
        d.insert("monitored_fraction".into(), Value::F64(t.monitored_s as f64 * 1000.0 / WINDOW_MS as f64));
        if let Some(since) = t.monitoring_since {
            d.insert("monitoring_since".into(), Value::I64(since));
        }
        if let Some(db) = t.mean_signal_dbfs_tenths {
            d.insert("signal_mean_dbfs".into(), Value::F64(db as f64 / 10.0));
        }
        d.insert("daily_listening_ms".into(), Value::from(t.daily_s.iter().map(|s| *s as u64 * 1000).collect::<Vec<_>>()));
        d.insert(
            "device_listening".into(),
            Value::from(
                t.per_device
                    .iter()
                    .map(|(name, kind, s)| (name.clone(), kind.clone(), *s as u64 * 1000))
                    .collect::<Vec<_>>(),
            ),
        );
        if let Some(dev) = &self.device {
            d.insert("device_name".into(), Value::from(dev.name.clone()));
            d.insert("device_kind".into(), Value::from(dev.kind));
            if let Some(v) = dev.volume_tenths {
                d.insert("volume_percent".into(), Value::F64(v as f64 / 10.0));
            }
            d.insert("muted".into(), Value::Bool(dev.muted));
            d.insert("playing".into(), Value::Bool(dev.playing));
            d.insert("stream_active".into(), Value::Bool(dev.stream_active));
        }
        d
    }
}

struct Capture {
    sink: String,
    task: JoinHandle<()>,
}

struct Shared {
    store: Store,
    audio: Result<AudioSnapshot>,
    protection_mode: &'static str,
    signal_analysis: bool,
    tracker: Tracker,
    capture: Option<Capture>,
    capture_failed_for: Option<String>,
    last_level: Option<LevelFrame>,
    /// Nível estimado em blocos de 3 s e detecção de volume alto.
    live: live::LiveMeter,
    totals: Totals,
    published: Option<Status>,
    /// Perfil da saída atual e a preferência de tipo usada para escolhê-lo.
    resolution: Option<Resolution>,
    kind_override: KindOverride,
    alert_level: u32,
}

impl Shared {
    fn default_output(&self) -> Option<&OutputDevice> {
        self.audio.as_ref().ok().and_then(AudioSnapshot::default_output)
    }

    fn capture_state(&self) -> CaptureState {
        if !self.signal_analysis {
            CaptureState::Off
        } else if self.capture.is_some() {
            CaptureState::Capturing
        } else if self.capture_failed_for.is_some() {
            CaptureState::Failed
        } else {
            CaptureState::Waiting
        }
    }

    fn activity_source(&self) -> ActivitySource {
        if self.capture.is_some() || self.capture_state() == CaptureState::Waiting {
            ActivitySource::Signal
        } else {
            ActivitySource::Stream
        }
    }

    fn estimate_status(&self) -> EstimateStatus {
        let (profile, kind_presumed, reason) = match &self.resolution {
            Some(Resolution::Estimate { profile, kind_presumed }) => (Some(profile), *kind_presumed, None),
            Some(Resolution::Unavailable { reason }) => (None, false, Some(*reason)),
            None => (None, false, None),
        };
        let reason = reason.or_else(|| {
            (profile.is_some() && !self.signal_analysis)
                .then_some("Ative a análise do sinal para estimar a exposição.")
        });
        EstimateStatus {
            profile_label: profile.map(|p| p.label),
            basis: profile.map(|p| p.basis),
            reason,
            uncertainty_db: profile.map(|p| p.uncertainty_db),
            kind_presumed,
            kind_override: self.kind_override.as_str(),
        }
    }

    /// Contexto de estimativa para o rastreador: só com perfil e captura.
    fn estimate_ctx(&self) -> Option<EstimateCtx> {
        let profile = self.resolution.as_ref()?.profile()?;
        (self.activity_source() == ActivitySource::Signal)
            .then(|| EstimateCtx { profile_id: profile.id.into(), uncertainty_db: profile.uncertainty_db })
    }

    /// O quadro tem som perceptível? Com perfil, pelo nível estimado no
    /// ouvido (o volume do sistema conta); sem perfil, pelo silêncio digital.
    fn frame_audible(&self, frame: &LevelFrame) -> bool {
        if !frame.is_audible() {
            return false;
        }
        match self.resolution.as_ref().and_then(Resolution::profile) {
            Some(_) => self.frame_laeq(frame).is_some_and(|l| l >= AUDIBLE_DBA),
            None => true,
        }
    }

    /// Nível estimado de um quadro na saída atual.
    fn frame_laeq(&self, frame: &LevelFrame) -> Option<f64> {
        let profile = self.resolution.as_ref()?.profile()?;
        let volume = self.default_output()?.volume?;
        calibration::estimate_laeq(profile, frame.mean_square_a, volume.linear_gain, volume.muted)
    }

    fn status(&self) -> Status {
        let capture = self.capture_state();
        let activity_source = self.activity_source();
        let estimate = self.estimate_status();
        let Some(out) = self.default_output() else {
            return Status {
                mascot_state: "ERROR",
                measurement_status: "UNAVAILABLE",
                coverage_status: "missing",
                device: None,
                protection_mode: self.protection_mode,
                signal_analysis: self.signal_analysis,
                capture,
                activity_source,
                totals: self.totals.clone(),
                estimate,
                loud_now: false,
            };
        };
        let playing = self.tracker.active_now();
        let dose = self.totals.dose_percent();
        let mascot_state = match dose {
            Some(d) if d >= EXCEEDED_PERCENT => "EXCEEDED",
            Some(d) if d >= WARNING_PERCENT => "WARNING",
            Some(d) if d >= NOTICE_PERCENT => "NOTICE",
            Some(_) if playing => "NORMAL",
            _ if playing => "UNVERIFIED",
            _ => "IDLE",
        };
        Status {
            mascot_state,
            measurement_status: if dose.is_some() { "ESTIMATED" } else { "UNVERIFIED" },
            coverage_status: if dose.is_some() { "observed" } else { "unverified" },
            device: Some(DeviceStatus {
                name: display_name(out),
                kind: out.kind.as_str(),
                volume_tenths: out.volume.map(|v| (v.percent * 10.0).round() as i64),
                muted: out.volume.is_some_and(|v| v.muted),
                playing,
                stream_active: out.has_running_stream(),
            }),
            protection_mode: self.protection_mode,
            signal_analysis: self.signal_analysis,
            capture,
            activity_source,
            totals: self.totals.clone(),
            estimate,
            loud_now: self.live.loud(),
        }
    }

    fn save(&self, closed: impl IntoIterator<Item = Closed>) {
        for (device, segment) in closed {
            if let Err(e) = self.store.insert_segment(&device, &segment) {
                eprintln!("hearphant: falha ao gravar histórico: {e:#}");
            }
        }
    }

    /// Recalcula os totais da janela a partir do banco + segmento aberto.
    fn recompute_totals(&mut self) {
        let now = now_utc();
        let mut summary = self.store.summary(now - WINDOW_MS, now).unwrap_or_else(|e| {
            eprintln!("hearphant: falha ao ler histórico: {e:#}");
            WindowSummary::default()
        });

        let midnight = Local::now().with_time(NaiveTime::MIN).single().unwrap_or_else(Local::now);
        let mut bounds: Vec<UtcMillis> = (1..DAYS_SHOWN as u64)
            .rev()
            .filter_map(|d| midnight.checked_sub_days(Days::new(d)))
            .map(|t| t.timestamp_millis())
            .collect();
        bounds.push(midnight.timestamp_millis());
        bounds.push(now.max(midnight.timestamp_millis() + 1));
        let mut daily = self.store.active_by_ranges(&bounds).unwrap_or_default();

        if let Some((device, open)) = self.tracker.pending() {
            summary.monitored_ms += open.duration_ms;
            summary.active_ms += open.active_ms;
            if open.source == ActivitySource::Signal {
                summary.signal_active_ms += open.active_ms;
            }
            summary.signal_energy += open.signal_energy.unwrap_or(0.0);
            summary.signal_ms += open.signal_ms;
            if let Some(est) = &open.estimate {
                summary.estimated_energy_pa2h += est.energy_pa2h;
                summary.estimated_ms += est.covered_ms;
                summary.max_uncertainty_db = Some(summary.max_uncertainty_db.map_or(est.uncertainty_db, |u| u.max(est.uncertainty_db)));
            }
            summary.first_start = Some(summary.first_start.map_or(open.start, |s| s.min(open.start)));
            if let Some(today) = daily.last_mut() {
                *today += open.active_ms;
            }
            match summary.per_device.iter_mut().find(|d| d.display_name == device.display_name) {
                Some(d) => d.active_ms += open.active_ms,
                None if open.active_ms > 0 => summary.per_device.push(hearphant_storage::DeviceActivity {
                    display_name: device.display_name.clone(),
                    kind: device.kind.clone(),
                    active_ms: open.active_ms,
                }),
                None => {}
            }
            summary.per_device.sort_by_key(|d| std::cmp::Reverse(d.active_ms));
        }

        let tenths = |x: f64| (x * 10.0).round() as i64;
        let (dose, low, high) = match (summary.estimated_ms > 0, summary.max_uncertainty_db) {
            (true, Some(u)) => {
                let dose = 100.0 * summary.estimated_energy_pa2h / ReferenceMode::Adult.reference_energy_pa2h();
                let f = calibration::dose_range_factor(u);
                let q = |x: f64| (x * DOSE_SCALE).round() as i64;
                (Some(q(dose)), Some(q(dose / f)), Some(q(dose * f)))
            }
            _ => (None, None, None),
        };
        self.totals = Totals {
            dose_scaled: dose,
            dose_low_scaled: low,
            dose_high_scaled: high,
            uncertainty_db_tenths: dose.and(summary.max_uncertainty_db.map(tenths)),
            estimated_s: summary.estimated_ms / 1000,
            listening_s: summary.active_ms / 1000,
            monitored_s: summary.monitored_ms / 1000,
            signal_listening_s: summary.signal_active_ms / 1000,
            mean_signal_dbfs_tenths: summary.mean_signal_dbfs().map(|db| (db * 10.0).round() as i64),
            monitoring_since: summary.first_start,
            daily_s: daily.iter().map(|ms| ms / 1000).collect(),
            per_device: summary
                .per_device
                .iter()
                .map(|d| (d.display_name.clone(), d.kind.clone(), d.active_ms / 1000))
                .collect(),
        };
    }

    /// Alerta quando a dose estimada cruza 80% ou 100% (uma vez por subida;
    /// rearma quando cai `ALERT_REARM_MARGIN` abaixo do limiar).
    fn check_alert(&mut self) -> Option<Dict> {
        let dose = self.totals.dose_percent()?;
        let rearm = |level: f64| dose < level - ALERT_REARM_MARGIN;
        let previous = self.alert_level;
        if previous >= 100 && rearm(EXCEEDED_PERCENT) {
            self.alert_level = if dose >= WARNING_PERCENT { 80 } else { 0 };
        }
        if self.alert_level >= 80 && rearm(WARNING_PERCENT) {
            self.alert_level = 0;
        }
        let (level, severity, title, body) = if dose >= EXCEEDED_PERCENT && self.alert_level < 100 {
            (100, "exceeded", "Referência semanal ultrapassada (estimativa)",
             "Pela estimativa, você passou da referência semanal para adultos. Continuar ouvindo no mesmo nível aumenta o risco; considere reduzir o volume ou fazer uma pausa.")
        } else if dose >= WARNING_PERCENT && self.alert_level < 80 {
            (80, "warning", "Exposição alta nesta semana (estimativa)",
             "Sua exposição estimada passou de 80% da referência semanal. Que tal diminuir um pouco o volume?")
        } else {
            if self.alert_level != previous {
                let _ = self.store.set_setting(SETTING_ALERT, &self.alert_level.to_string());
            }
            return None;
        };
        self.alert_level = level;
        let _ = self.store.set_setting(SETTING_ALERT, &level.to_string());
        let mut d = Dict::new();
        d.insert("alert_id".into(), Value::from(format!("dose-{level}")));
        d.insert("severity".into(), Value::from(severity));
        d.insert("title".into(), Value::from(title));
        d.insert("body".into(), Value::from(body));
        Some(d)
    }


    /// O nível mais recente, se for de agora.
    fn current_level(&self) -> Option<&LevelFrame> {
        self.last_level.as_ref().filter(|l| self.capture.is_some() && l.at.elapsed() < LEVEL_STALE)
    }
}

struct Service {
    shared: Arc<Mutex<Shared>>,
    reconcile: Arc<Notify>,
}

#[interface(name = "io.github.gab_simon.HearPhant1")]
impl Service {
    #[zbus(property)]
    fn api_version(&self) -> u32 {
        API_VERSION
    }

    fn get_status(&self) -> Dict {
        self.shared.lock().unwrap().status().to_dict()
    }

    fn get_exposure(&self, window_hours: u32) -> fdo::Result<Dict> {
        if !(1..=WINDOW_HOURS).contains(&window_hours) {
            return Err(fdo::Error::InvalidArgs(format!("window_hours deve estar entre 1 e {WINDOW_HOURS}")));
        }
        let shared = self.shared.lock().unwrap();
        let status = shared.status();
        let now = now_utc();
        let summary = shared
            .store
            .summary(now - window_hours as i64 * 3_600_000, now)
            .map_err(|e| fdo::Error::Failed(format!("{e:#}")))?;
        let mut d = Dict::new();
        d.insert("window_hours".into(), Value::U32(window_hours));
        d.insert("reference_mode".into(), Value::from("adult"));
        d.insert("measurement_status".into(), Value::from(status.measurement_status));
        d.insert("coverage_status".into(), Value::from(status.coverage_status));
        d.insert("listening_ms".into(), Value::U64(summary.active_ms.max(0) as u64));
        d.insert("monitored_ms".into(), Value::U64(summary.monitored_ms.max(0) as u64));
        d.insert("is_demo".into(), Value::Bool(false));
        Ok(d)
    }

    /// Nível do sinal agora. Pensado para ser consultado só enquanto um
    /// medidor está visível. `capturing = false` quando não há medida atual.
    /// `laeq_estimate` é o nível estimado no ouvido em dB(A) do último bloco
    /// de 3 s (atualiza a cada 3 s), ou −1 sem estimativa (sem perfil,
    /// silêncio ou mudo).
    fn get_level(&self) -> (bool, f64, f64, bool, f64) {
        let shared = self.shared.lock().unwrap();
        match shared.current_level() {
            Some(l) => (
                true,
                l.rms_dbfs().max(-120.0),
                l.peak_dbfs().max(-120.0),
                shared.frame_audible(l),
                shared.live.current(Instant::now()).unwrap_or(-1.0),
            ),
            None => (false, -120.0, -120.0, false, -1.0),
        }
    }

    fn list_devices(&self) -> Vec<Dict> {
        let shared = self.shared.lock().unwrap();
        let Ok(audio) = &shared.audio else { return Vec::new() };
        audio
            .outputs
            .iter()
            .map(|o| {
                let mut d = Dict::new();
                d.insert("id".into(), Value::from(o.stable_name.clone()));
                d.insert("display_name".into(), Value::from(display_name(o)));
                d.insert("kind".into(), Value::from(o.kind.as_str()));
                if let Some(e) = &o.kind_evidence {
                    d.insert("kind_evidence".into(), Value::from(e.clone()));
                }
                d.insert("is_default".into(), Value::Bool(o.is_default));
                d.insert("state".into(), Value::from(o.state.clone()));
                d.insert("measurement_status".into(), Value::from("UNVERIFIED"));
                if let Some(v) = o.volume {
                    d.insert("volume_percent".into(), Value::F64((v.percent * 10.0).round() / 10.0));
                    d.insert("muted".into(), Value::Bool(v.muted));
                }
                d
            })
            .collect()
    }

    async fn set_protection_mode(
        &self,
        mode: String,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> (bool, String) {
        let accepted: &'static str = match mode.as_str() {
            "inform" => "inform",
            "ask" => "ask",
            "auto" => return (false, "Redução automática exige um perfil calibrado para esta saída.".into()),
            "basic_cap" => return (false, "Limite percentual básico ainda não implementado.".into()),
            other => return (false, format!("Modo desconhecido: {other}")),
        };
        {
            let mut s = self.shared.lock().unwrap();
            s.protection_mode = accepted;
            if let Err(e) = s.store.set_setting(SETTING_PROTECTION, accepted) {
                eprintln!("hearphant: falha ao salvar configuração: {e:#}");
            }
        }
        publish_if_changed(&self.shared, &ctxt).await;
        (true, String::new())
    }

    /// Liga/desliga a análise do nível do sinal (consentimento explícito: o
    /// GNOME mostra o ícone de gravação enquanto a captura do monitor roda).
    fn set_signal_analysis(&self, enabled: bool) -> (bool, String) {
        {
            let mut s = self.shared.lock().unwrap();
            s.signal_analysis = enabled;
            s.capture_failed_for = None;
            if let Err(e) = s.store.set_setting(SETTING_SIGNAL, if enabled { "1" } else { "0" }) {
                eprintln!("hearphant: falha ao salvar configuração: {e:#}");
            }
        }
        self.reconcile.notify_one();
        (true, String::new())
    }

    /// Tipo da saída atual: `auto` (detecção), `headphone` ou `speaker`.
    /// Corrige a detecção quando a porta não diz o que está conectado.
    fn set_device_kind(&self, kind: String) -> (bool, String) {
        let Some(choice) = KindOverride::parse(&kind) else {
            return (false, format!("Tipo desconhecido: {kind}"));
        };
        {
            let s = self.shared.lock().unwrap();
            let Some(out) = s.default_output() else {
                return (false, "Nenhuma saída de áudio ativa.".into());
            };
            let key = kind_setting_key(&out.stable_name);
            if let Err(e) = s.store.set_setting(&key, choice.as_str()) {
                return (false, format!("Falha ao salvar: {e:#}"));
            }
        }
        self.reconcile.notify_one();
        (true, String::new())
    }

    /// Apaga o histórico local em `[from_utc_ms, to_utc_ms)` (RF-016).
    async fn delete_history(
        &self,
        from_utc_ms: i64,
        to_utc_ms: i64,
        #[zbus(signal_context)] ctxt: SignalContext<'_>,
    ) -> fdo::Result<u64> {
        let deleted = {
            let mut s = self.shared.lock().unwrap();
            let n = s.store.delete_range(from_utc_ms, to_utc_ms).map_err(|e| fdo::Error::Failed(format!("{e:#}")))?;
            let now = now_utc();
            if from_utc_ms <= now && now < to_utc_ms.saturating_add(WINDOW_MS) {
                s.tracker.discard(Instant::now(), now);
            }
            s.recompute_totals();
            n as u64
        };
        publish_if_changed(&self.shared, &ctxt).await;
        Ok(deleted)
    }

    #[zbus(signal)]
    async fn status_changed(ctxt: &SignalContext<'_>, status: Dict) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn alert_raised(ctxt: &SignalContext<'_>, alert: Dict) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn protection_applied(ctxt: &SignalContext<'_>, result: Dict) -> zbus::Result<()>;
}

async fn publish_if_changed(shared: &Mutex<Shared>, ctxt: &SignalContext<'_>) {
    let dict = {
        let mut s = shared.lock().unwrap();
        let status = s.status();
        if s.published.as_ref() == Some(&status) {
            return;
        }
        let device_changed = s.published.as_ref().map(|p| (&p.device, p.capture, p.mascot_state))
            != Some((&status.device, status.capture, status.mascot_state));
        if device_changed {
            log_status(&status);
        }
        let dict = status.to_dict();
        s.published = Some(status);
        dict
    };
    if let Err(e) = Service::status_changed(ctxt, dict).await {
        eprintln!("hearphant: falha ao emitir StatusChanged: {e}");
    }
}

fn log_status(s: &Status) {
    match &s.device {
        Some(d) => eprintln!(
            "hearphant: {} | {} ({}) vol={} mudo={} áudio={} | análise={} | proteção={}",
            s.mascot_state,
            d.name,
            d.kind,
            d.volume_tenths.map_or("?".into(), |t| format!("{:.1}%", t as f64 / 10.0)),
            d.muted,
            d.playing,
            s.capture.as_str(),
            s.protection_mode
        ),
        None => eprintln!("hearphant: {} | sem saída de áudio", s.mascot_state),
    }
}

struct Daemon {
    shared: Arc<Mutex<Shared>>,
    conn: zbus::Connection,
    reconcile: Arc<Notify>,
}

impl Daemon {
    async fn ctxt(&self) -> Result<SignalContext<'static>> {
        Ok(SignalContext::new(&self.conn, OBJECT_PATH)?.into_owned())
    }

    /// Emite alertas de dose pendentes e o StatusChanged, se algo mudou.
    async fn publish(&self) {
        let Ok(ctxt) = self.ctxt().await else { return };
        let alert = self.shared.lock().unwrap().check_alert();
        if let Some(alert) = alert {
            if let Err(e) = Service::alert_raised(&ctxt, alert).await {
                eprintln!("hearphant: falha ao emitir AlertRaised: {e}");
            }
        }
        publish_if_changed(&self.shared, &ctxt).await;
    }

    /// Alinha rastreador e captura com o estado atual do áudio.
    fn reconcile(&self) {
        let mut s = self.shared.lock().unwrap();
        let output = s.default_output().cloned();
        let stream_active = output.as_ref().is_some_and(OutputDevice::has_running_stream);
        let want_capture = s.signal_analysis && stream_active;
        let sink = output.as_ref().map(|o| o.stable_name.clone());

        // Para a captura se não é mais desejada ou se a saída mudou.
        if s.capture.as_ref().is_some_and(|c| !want_capture || Some(&c.sink) != sink.as_ref()) {
            if let Some(c) = s.capture.take() {
                c.task.abort();
            }
            s.last_level = None;
            s.live.reset();
        }
        if s.capture_failed_for.is_some() && s.capture_failed_for != sink {
            s.capture_failed_for = None;
        }
        if want_capture && s.capture.is_none() && s.capture_failed_for.is_none() {
            let sink = sink.clone().unwrap_or_default();
            match level::capture(&sink, LEVEL_FRAME) {
                Ok(rx) => {
                    let task = tokio::spawn(run_capture(self.shared.clone(), self.conn.clone(), sink.clone(), rx));
                    s.capture = Some(Capture { sink, task });
                }
                Err(e) => {
                    eprintln!("hearphant: {e:#}");
                    s.capture_failed_for = Some(sink);
                }
            }
        }

        // Perfil de estimativa da saída, respeitando o tipo escolhido.
        s.kind_override = output
            .as_ref()
            .and_then(|o| s.store.setting(&kind_setting_key(&o.stable_name)).ok().flatten())
            .and_then(|v| KindOverride::parse(&v))
            .unwrap_or(KindOverride::Auto);
        let resolution = output.as_ref().map(|o| calibration::resolve(o, s.kind_override));
        if resolution != s.resolution {
            s.live.reset();
        }
        s.resolution = resolution;

        let source = s.activity_source();
        // Em modo sinal, a atividade vem dos quadros; sem captura, nada toca.
        let active = match source {
            ActivitySource::Stream => stream_active,
            ActivitySource::Signal => s.tracker.active_now() && s.capture.is_some(),
        };
        let estimate = s.estimate_ctx();
        let closed =
            s.tracker.set_context(output.as_ref().map(device_key), source, estimate, active, Instant::now(), now_utc());
        s.save(closed);
        s.recompute_totals();
    }

    async fn refresh_audio(&self) {
        let audio = hearphant_audio::snapshot().await;
        if let Err(e) = &audio {
            eprintln!("hearphant: {e:#}");
        }
        self.shared.lock().unwrap().audio = audio;
        self.reconcile();
        self.publish().await;
    }

    async fn tick(&self) {
        {
            let mut s = self.shared.lock().unwrap();
            let closed = s.tracker.tick(Instant::now(), now_utc());
            s.save(closed);
            s.recompute_totals();
        }
        self.publish().await;
    }

    async fn run(&self) -> Result<()> {
        let mut ticker = tokio::time::interval(TICK);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            self.refresh_audio().await;
            match hearphant_audio::watch_changes(CHANGE_DEBOUNCE) {
                Ok(mut changes) => loop {
                    tokio::select! {
                        c = changes.recv() => match c {
                            Some(()) => self.refresh_audio().await,
                            None => break,
                        },
                        _ = self.reconcile.notified() => {
                            self.reconcile();
                            self.publish().await;
                        }
                        _ = ticker.tick() => self.tick().await,
                    }
                },
                Err(e) => eprintln!("hearphant: {e:#}"),
            }
            eprintln!("hearphant: monitor do PipeWire encerrou; tentando de novo");
            // Sem monitor não sabemos o estado: lacuna, não um status antigo.
            self.shared.lock().unwrap().audio = Err(anyhow::anyhow!("monitor do PipeWire indisponível"));
            self.reconcile();
            self.publish().await;
            tokio::time::sleep(MONITOR_RETRY).await;
        }
    }

    fn shutdown(&self) {
        let mut s = self.shared.lock().unwrap();
        let _ = s.tracker.tick(Instant::now(), now_utc()).map(|c| s.save([c]));
        let closed = s.tracker.close();
        s.save(closed);
        if let Some(c) = s.capture.take() {
            c.task.abort();
        }
    }
}

/// Aviso de volume alto agora (zona vermelha do medidor).
fn loud_alert(leq: f64) -> Dict {
    let hours = hearphant_dose::time_to_reference_hours(leq, ReferenceMode::Adult);
    let time = if hours >= 1.0 {
        format!("{:.0} h", hours)
    } else {
        format!("{:.0} min", hours * 60.0)
    };
    let mut d = Dict::new();
    d.insert("alert_id".into(), Value::from("loud-now"));
    d.insert("severity".into(), Value::from("loud"));
    d.insert("title".into(), Value::from(format!("Volume muito alto: ≈ {leq:.0} dB(A)")));
    d.insert(
        "body".into(),
        Value::from(format!(
            "A Ellie estima que o som no seu ouvido está muito alto. Nesse nível, a referência semanal              seria atingida em cerca de {time}. Que tal baixar o volume?"
        )),
    );
    d.insert("laeq_estimate".into(), Value::F64(leq));
    d
}

/// Consome quadros de nível da captura atual.
async fn run_capture(
    shared: Arc<Mutex<Shared>>,
    conn: zbus::Connection,
    sink: String,
    mut rx: tokio::sync::mpsc::Receiver<LevelFrame>,
) {
    let Ok(ctxt) = SignalContext::new(&conn, OBJECT_PATH) else { return };
    while let Some(frame) = rx.recv().await {
        let (changed, alert) = {
            let mut s = shared.lock().unwrap();
            let before = s.tracker.active_now();
            let laeq = s.frame_laeq(&frame);
            let audible = s.frame_audible(&frame);
            let closed =
                s.tracker.signal_frame(frame.at, now_utc(), frame.duration, frame.mean_square, audible, laeq);
            s.save(closed);
            s.last_level = Some(frame);
            let mut block = None;
            if s.resolution.as_ref().and_then(Resolution::profile).is_some() {
                // Abaixo do limiar de silêncio conta como energia zero: ruído
                // digital não é "nível no ouvido".
                let energy = laeq.filter(|_| audible).map_or(0.0, |l| 10f64.powf(l / 10.0));
                block = s.live.push(energy, frame.at);
            }
            let alert = block.and_then(|b| b.alert).map(loud_alert);
            (before != s.tracker.active_now() || block.is_some_and(|b| b.loud_changed), alert)
        };
        if let Some(alert) = alert {
            if let Err(e) = Service::alert_raised(&ctxt, alert).await {
                eprintln!("hearphant: falha ao emitir AlertRaised: {e}");
            }
        }
        if changed {
            publish_if_changed(&shared, &ctxt).await;
        }
    }
    // A captura terminou sozinha (saída sumiu, parec falhou).
    {
        let mut s = shared.lock().unwrap();
        if s.capture.as_ref().is_some_and(|c| c.sink == sink) {
            eprintln!("hearphant: captura de nível de {sink} terminou");
            s.capture = None;
            s.capture_failed_for = Some(sink);
            s.last_level = None;
            s.live.reset();
        }
    }
    publish_if_changed(&shared, &ctxt).await;
}

fn database_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("hearphant").join("history.sqlite3")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let db = database_path();
    let store = Store::open(&db)?;
    let protection_mode = match store.setting(SETTING_PROTECTION)?.as_deref() {
        Some("ask") => "ask",
        _ => "inform",
    };
    let signal_analysis = store.setting(SETTING_SIGNAL)?.as_deref() == Some("1");
    let alert_level = store.setting(SETTING_ALERT)?.and_then(|v| v.parse().ok()).unwrap_or(0);

    let shared = Arc::new(Mutex::new(Shared {
        store,
        audio: Err(anyhow::anyhow!("ainda não consultado")),
        protection_mode,
        signal_analysis,
        tracker: Tracker::default(),
        capture: None,
        capture_failed_for: None,
        last_level: None,
        live: live::LiveMeter::default(),
        totals: Totals::default(),
        published: None,
        resolution: None,
        kind_override: KindOverride::Auto,
        alert_level,
    }));
    shared.lock().unwrap().recompute_totals();
    let reconcile = Arc::new(Notify::new());

    let conn = zbus::connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, Service { shared: shared.clone(), reconcile: reconcile.clone() })?
        .build()
        .await
        .with_context(|| format!("não foi possível registrar {BUS_NAME} (outro serviço em execução?)"))?;
    eprintln!("hearphant: {BUS_NAME} ativo em {OBJECT_PATH}; histórico em {}", db.display());

    let daemon = Daemon { shared, conn, reconcile };
    let mut sigterm = signal(SignalKind::terminate())?;
    tokio::select! {
        r = daemon.run() => r?,
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
    daemon.shutdown();
    eprintln!("hearphant: encerrando (histórico salvo)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        Shared {
            store: Store::open_in_memory().unwrap(),
            audio: Err(anyhow::anyhow!("teste")),
            protection_mode: "inform",
            signal_analysis: true,
            tracker: Tracker::default(),
            capture: None,
            capture_failed_for: None,
            last_level: None,
            live: live::LiveMeter::default(),
            totals: Totals::default(),
            published: None,
            resolution: None,
            kind_override: KindOverride::Auto,
            alert_level: 0,
        }
    }

    fn alert_at(s: &mut Shared, dose: f64) -> Option<String> {
        s.totals.dose_scaled = Some((dose * DOSE_SCALE) as i64);
        s.check_alert().map(|d| match d.get("severity") {
            Some(Value::Str(v)) => v.to_string(),
            _ => panic!("sem severidade"),
        })
    }

    #[test]
    fn alerts_fire_once_per_rise_and_rearm() {
        let mut s = shared();
        assert_eq!(alert_at(&mut s, 50.0), None);
        assert_eq!(alert_at(&mut s, 81.0).as_deref(), Some("warning"));
        assert_eq!(alert_at(&mut s, 85.0), None, "não repete");
        assert_eq!(alert_at(&mut s, 101.0).as_deref(), Some("exceeded"));
        assert_eq!(alert_at(&mut s, 95.0), None, "ainda perto do limiar");
        assert_eq!(alert_at(&mut s, 60.0), None, "caiu: rearma");
        assert_eq!(alert_at(&mut s, 82.0).as_deref(), Some("warning"));
        assert_eq!(s.store.setting(SETTING_ALERT).unwrap().as_deref(), Some("80"));
    }

    fn frame_at(dbfs: f64) -> LevelFrame {
        let ms = 10f64.powf(dbfs / 10.0);
        LevelFrame { at: Instant::now(), duration: LEVEL_FRAME, mean_square: ms, mean_square_a: ms, peak: 0.0 }
    }

    fn with_output(s: &mut Shared, volume_cubic: f64) {
        let out = OutputDevice {
            node_id: 1,
            stable_name: "alsa_output.usb-KTMicro_Chu2_DSP-00.analog-stereo".into(),
            description: "Chu2 DSP Analog Stereo".into(),
            kind: hearphant_audio::DeviceKind::Unknown,
            kind_evidence: None,
            bus: None,
            state: "running".into(),
            volume: Some(hearphant_audio::DigitalVolume {
                percent: volume_cubic * 100.0,
                linear_gain: volume_cubic.powi(3),
                muted: false,
            }),
            streams: Vec::new(),
            is_default: true,
        };
        s.resolution = Some(calibration::resolve(&out, KindOverride::Auto));
        s.audio = Ok(AudioSnapshot { outputs: vec![out] });
    }

    #[test]
    fn quiet_app_volume_still_counts_as_audible() {
        // Spotify em ~1% de volume: a música chega a −70 dBFS no monitor.
        let mut s = shared();
        assert!(s.frame_audible(&frame_at(-70.0)), "sem perfil: acima do silêncio digital");
        with_output(&mut s, 0.22);
        // −70 − 39,5 (volume 22%) + 120,2 ≈ 10,7 dB(A): baixinho, mas conta.
        assert!(s.frame_audible(&frame_at(-70.0)));
        // −75 dBFS → ≈ 5,7 dB(A): abaixo do limiar.
        assert!(!s.frame_audible(&frame_at(-75.0)));
        // Ruído digital nunca conta.
        assert!(!s.frame_audible(&frame_at(-95.0)));
    }

    #[test]
    fn no_dose_no_alert() {
        let mut s = shared();
        assert!(s.check_alert().is_none());
    }
}
