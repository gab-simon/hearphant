//! Histórico local do HearPhant (seção 11).
//!
//! Persiste apenas agregados de atividade por segmento curto (≤ 1 min):
//! tempo monitorado, tempo com áudio e energia digital do sinal. Nunca PCM,
//! nomes de faixas ou conteúdo (RNF-002). Tudo é `UNVERIFIED`: são medidas
//! digitais, não exposição acústica.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub type UtcMillis = i64;

/// Como o tempo "com áudio" de um segmento foi determinado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivitySource {
    /// Nível do sinal do monitor da saída acima do limiar de silêncio.
    Signal,
    /// Aproximação: havia stream de aplicativo rodando na saída.
    Stream,
}

impl ActivitySource {
    pub fn as_str(self) -> &'static str {
        match self {
            ActivitySource::Signal => "signal",
            ActivitySource::Stream => "stream",
        }
    }
}

/// Saída de áudio como identificada pelo PipeWire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceKey {
    pub stable_name: String,
    pub display_name: String,
    pub kind: String,
}

/// Segmento contínuo de monitoramento em uma saída.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivitySegment {
    pub start: UtcMillis,
    pub duration_ms: i64,
    /// Parte de `duration_ms` com áudio.
    pub active_ms: i64,
    /// Σ (média quadrática × ms) dos trechos ativos medidos pelo sinal.
    pub signal_energy: Option<f64>,
    /// Milissegundos ativos que contribuíram para `signal_energy`.
    pub signal_ms: i64,
    pub source: ActivitySource,
    /// Estimativa acústica do segmento, quando havia perfil (seção 9.3, B).
    pub estimate: Option<SegmentEstimate>,
}

/// Energia acústica estimada de um segmento. Nunca é medição calibrada.
#[derive(Debug, Clone, PartialEq)]
pub struct SegmentEstimate {
    pub energy_pa2h: f64,
    /// Tempo do segmento coberto pela estimativa (inclui silêncio).
    pub covered_ms: i64,
    pub uncertainty_db: f64,
    pub profile_id: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DeviceActivity {
    pub display_name: String,
    pub kind: String,
    pub active_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct WindowSummary {
    /// Tempo em que o serviço estava observando (cobertura).
    pub monitored_ms: i64,
    pub active_ms: i64,
    /// Parte de `active_ms` medida pelo sinal (o resto é aproximação por stream).
    pub signal_active_ms: i64,
    pub signal_energy: f64,
    pub signal_ms: i64,
    pub first_start: Option<UtcMillis>,
    pub per_device: Vec<DeviceActivity>,
    /// Energia acústica estimada na janela, Pa²·h.
    pub estimated_energy_pa2h: f64,
    pub estimated_ms: i64,
    /// Maior incerteza entre os segmentos estimados da janela.
    pub max_uncertainty_db: Option<f64>,
}

impl WindowSummary {
    /// Nível RMS médio do sinal digital nos trechos com áudio, em dBFS.
    pub fn mean_signal_dbfs(&self) -> Option<f64> {
        (self.signal_ms > 0 && self.signal_energy > 0.0)
            .then(|| 10.0 * (self.signal_energy / self.signal_ms as f64).log10())
    }
}

pub struct Store {
    conn: Connection,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS devices (
    id            INTEGER PRIMARY KEY,
    stable_name   TEXT NOT NULL UNIQUE,
    display_name  TEXT NOT NULL,
    kind          TEXT NOT NULL,
    last_seen_utc INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS activity_segments (
    id               INTEGER PRIMARY KEY,
    device_id        INTEGER NOT NULL REFERENCES devices(id),
    start_utc        INTEGER NOT NULL,
    duration_ms      INTEGER NOT NULL CHECK (duration_ms > 0),
    active_ms        INTEGER NOT NULL CHECK (active_ms BETWEEN 0 AND duration_ms),
    signal_energy    REAL,
    signal_ms        INTEGER NOT NULL DEFAULT 0,
    activity_source  TEXT NOT NULL,
    confidence_class TEXT NOT NULL DEFAULT 'UNVERIFIED',
    coverage_status  TEXT NOT NULL DEFAULT 'unverified'
);
CREATE INDEX IF NOT EXISTS activity_segments_start ON activity_segments(start_utc);
-- Colunas de estimativa (v2) são adicionadas por `migrate`.
CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Fração de um segmento dentro de `[from, to)`, em SQL. Segmentos que cruzam
/// a borda contam proporcionalmente (seção 8.2).
const OVERLAP: &str = "(MAX(0, MIN(s.start_utc + s.duration_ms, ?2) - MAX(s.start_utc, ?1)) * 1.0 / s.duration_ms)";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("criando {}", dir.display()))?;
        }
        let conn = Connection::open(path).with_context(|| format!("abrindo {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    /// Acrescenta colunas novas a bancos criados por versões anteriores.
    fn migrate(conn: &Connection) -> Result<()> {
        let existing: Vec<String> = conn
            .prepare("SELECT name FROM pragma_table_info('activity_segments')")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for (column, ddl) in [
            ("est_energy_pa2h", "REAL"),
            ("est_ms", "INTEGER NOT NULL DEFAULT 0"),
            ("est_uncertainty_db", "REAL"),
            ("profile_id", "TEXT"),
        ] {
            if !existing.iter().any(|c| c == column) {
                conn.execute_batch(&format!("ALTER TABLE activity_segments ADD COLUMN {column} {ddl}"))?;
            }
        }
        Ok(())
    }

    fn device_id(&self, device: &DeviceKey, now: UtcMillis) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO devices (stable_name, display_name, kind, last_seen_utc) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(stable_name) DO UPDATE SET display_name = ?2, kind = ?3, last_seen_utc = ?4",
            params![device.stable_name, device.display_name, device.kind, now],
        )?;
        Ok(self.conn.query_row(
            "SELECT id FROM devices WHERE stable_name = ?1",
            [&device.stable_name],
            |r| r.get(0),
        )?)
    }

    pub fn insert_segment(&self, device: &DeviceKey, seg: &ActivitySegment) -> Result<()> {
        if seg.duration_ms <= 0 {
            return Ok(());
        }
        let id = self.device_id(device, seg.start + seg.duration_ms)?;
        self.conn.execute(
            "INSERT INTO activity_segments
                (device_id, start_utc, duration_ms, active_ms, signal_energy, signal_ms, activity_source,
                 est_energy_pa2h, est_ms, est_uncertainty_db, profile_id, confidence_class)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id,
                seg.start,
                seg.duration_ms,
                seg.active_ms.clamp(0, seg.duration_ms),
                seg.signal_energy,
                seg.signal_ms,
                seg.source.as_str(),
                seg.estimate.as_ref().map(|e| e.energy_pa2h),
                seg.estimate.as_ref().map_or(0, |e| e.covered_ms.clamp(0, seg.duration_ms)),
                seg.estimate.as_ref().map(|e| e.uncertainty_db),
                seg.estimate.as_ref().map(|e| e.profile_id.as_str()),
                if seg.estimate.is_some() { "ESTIMATED" } else { "UNVERIFIED" },
            ],
        )?;
        Ok(())
    }

    /// Atividade em `[from, to)`.
    pub fn summary(&self, from: UtcMillis, to: UtcMillis) -> Result<WindowSummary> {
        let mut out = self.conn.query_row(
            &format!(
                "SELECT
                    COALESCE(SUM(s.duration_ms * {OVERLAP}), 0),
                    COALESCE(SUM(s.active_ms * {OVERLAP}), 0),
                    COALESCE(SUM(CASE WHEN s.activity_source = 'signal' THEN s.active_ms * {OVERLAP} END), 0),
                    COALESCE(SUM(s.signal_energy * {OVERLAP}), 0),
                    COALESCE(SUM(s.signal_ms * {OVERLAP}), 0),
                    MIN(s.start_utc),
                    COALESCE(SUM(s.est_energy_pa2h * {OVERLAP}), 0),
                    COALESCE(SUM(s.est_ms * {OVERLAP}), 0),
                    MAX(CASE WHEN s.est_ms > 0 THEN s.est_uncertainty_db END)
                 FROM activity_segments s
                 WHERE s.start_utc < ?2 AND s.start_utc + s.duration_ms > ?1"
            ),
            params![from, to],
            |r| {
                Ok(WindowSummary {
                    monitored_ms: r.get::<_, f64>(0)?.round() as i64,
                    active_ms: r.get::<_, f64>(1)?.round() as i64,
                    signal_active_ms: r.get::<_, f64>(2)?.round() as i64,
                    signal_energy: r.get(3)?,
                    signal_ms: r.get::<_, f64>(4)?.round() as i64,
                    first_start: r.get(5)?,
                    per_device: Vec::new(),
                    estimated_energy_pa2h: r.get(6)?,
                    estimated_ms: r.get::<_, f64>(7)?.round() as i64,
                    max_uncertainty_db: r.get(8)?,
                })
            },
        )?;

        let mut stmt = self.conn.prepare(&format!(
            "SELECT d.display_name, d.kind, SUM(s.active_ms * {OVERLAP}) AS active
             FROM activity_segments s JOIN devices d ON d.id = s.device_id
             WHERE s.start_utc < ?2 AND s.start_utc + s.duration_ms > ?1
             GROUP BY d.id HAVING active > 0 ORDER BY active DESC"
        ))?;
        out.per_device = stmt
            .query_map(params![from, to], |r| {
                Ok(DeviceActivity {
                    display_name: r.get(0)?,
                    kind: r.get(1)?,
                    active_ms: r.get::<_, f64>(2)?.round() as i64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        Ok(out)
    }

    /// Tempo com áudio em cada intervalo `[bounds[i], bounds[i+1])`.
    pub fn active_by_ranges(&self, bounds: &[UtcMillis]) -> Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT COALESCE(SUM(s.active_ms * {OVERLAP}), 0) FROM activity_segments s
             WHERE s.start_utc < ?2 AND s.start_utc + s.duration_ms > ?1"
        ))?;
        bounds
            .windows(2)
            .map(|w| Ok(stmt.query_row(params![w[0], w[1]], |r| r.get::<_, f64>(0))?.round() as i64))
            .collect()
    }

    /// Apaga segmentos que começam em `[from, to)` (RF-016). Devolve quantos.
    pub fn delete_range(&self, from: UtcMillis, to: UtcMillis) -> Result<usize> {
        let n = self.conn.execute(
            "DELETE FROM activity_segments WHERE start_utc >= ?1 AND start_utc < ?2",
            params![from, to],
        )?;
        self.conn.execute(
            "DELETE FROM devices WHERE id NOT IN (SELECT DISTINCT device_id FROM activity_segments)",
            [],
        )?;
        Ok(n)
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    fn dev(name: &str) -> DeviceKey {
        DeviceKey { stable_name: name.into(), display_name: format!("{name} bonito"), kind: "unknown".into() }
    }

    fn seg(start: i64, active_ms: i64) -> ActivitySegment {
        ActivitySegment {
            start,
            duration_ms: MIN,
            active_ms,
            signal_energy: Some(0.01 * active_ms as f64),
            signal_ms: active_ms,
            source: ActivitySource::Signal,
            estimate: None,
        }
    }

    #[test]
    fn summary_sums_and_splits_by_device() {
        let s = Store::open_in_memory().unwrap();
        s.insert_segment(&dev("a"), &seg(0, 30_000)).unwrap();
        s.insert_segment(&dev("a"), &seg(MIN, 60_000)).unwrap();
        s.insert_segment(&dev("b"), &seg(2 * MIN, 15_000)).unwrap();

        let sum = s.summary(0, 10 * MIN).unwrap();
        assert_eq!(sum.monitored_ms, 3 * MIN);
        assert_eq!(sum.active_ms, 105_000);
        assert_eq!(sum.first_start, Some(0));
        assert_eq!(sum.per_device.len(), 2);
        assert_eq!(sum.per_device[0].display_name, "a bonito");
        assert_eq!(sum.per_device[0].active_ms, 90_000);
        // energia média 0,01 → −20 dBFS
        assert!((sum.mean_signal_dbfs().unwrap() + 20.0).abs() < 1e-9);
    }

    #[test]
    fn window_edge_counts_proportionally() {
        let s = Store::open_in_memory().unwrap();
        s.insert_segment(&dev("a"), &seg(0, 60_000)).unwrap();
        let sum = s.summary(30_000, 10 * MIN).unwrap();
        assert_eq!(sum.monitored_ms, 30_000);
        assert_eq!(sum.active_ms, 30_000);
    }

    #[test]
    fn stream_approximation_is_kept_apart() {
        let s = Store::open_in_memory().unwrap();
        let approx = ActivitySegment {
            start: 0,
            duration_ms: MIN,
            active_ms: MIN,
            signal_energy: None,
            signal_ms: 0,
            source: ActivitySource::Stream,
            estimate: None,
        };
        s.insert_segment(&dev("a"), &approx).unwrap();
        let sum = s.summary(0, MIN).unwrap();
        assert_eq!(sum.active_ms, MIN);
        assert_eq!(sum.signal_active_ms, 0);
        assert_eq!(sum.mean_signal_dbfs(), None);
    }

    #[test]
    fn ranges_and_delete() {
        let s = Store::open_in_memory().unwrap();
        for i in 0..4 {
            s.insert_segment(&dev("a"), &seg(i * MIN, 10_000)).unwrap();
        }
        assert_eq!(s.active_by_ranges(&[0, 2 * MIN, 4 * MIN]).unwrap(), vec![20_000, 20_000]);
        assert_eq!(s.delete_range(0, i64::MAX).unwrap(), 4);
        assert_eq!(s.summary(0, 10 * MIN).unwrap(), WindowSummary::default());
    }

    #[test]
    fn estimates_are_summed_with_max_uncertainty() {
        let s = Store::open_in_memory().unwrap();
        let est = |e: f64, u: f64| ActivitySegment {
            estimate: Some(SegmentEstimate { energy_pa2h: e, covered_ms: MIN, uncertainty_db: u, profile_id: "p".into() }),
            ..seg(0, MIN)
        };
        s.insert_segment(&dev("a"), &ActivitySegment { start: 0, ..est(0.2, 5.0) }).unwrap();
        s.insert_segment(&dev("a"), &ActivitySegment { start: MIN, ..est(0.3, 10.0) }).unwrap();
        s.insert_segment(&dev("a"), &seg(2 * MIN, MIN)).unwrap();
        let sum = s.summary(0, 10 * MIN).unwrap();
        assert!((sum.estimated_energy_pa2h - 0.5).abs() < 1e-12);
        assert_eq!(sum.estimated_ms, 2 * MIN);
        assert_eq!(sum.max_uncertainty_db, Some(10.0));
    }

    #[test]
    fn migrates_v1_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE activity_segments (id INTEGER PRIMARY KEY, device_id INTEGER, start_utc INTEGER,
             duration_ms INTEGER, active_ms INTEGER, signal_energy REAL, signal_ms INTEGER, activity_source TEXT,
             confidence_class TEXT, coverage_status TEXT)",
        )
        .unwrap();
        let s = Store::init(conn).unwrap();
        s.insert_segment(&dev("a"), &seg(0, 1000)).unwrap();
        assert_eq!(s.summary(0, MIN).unwrap().estimated_ms, 0);
    }

    #[test]
    fn settings_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.setting("x").unwrap(), None);
        s.set_setting("x", "1").unwrap();
        s.set_setting("x", "2").unwrap();
        assert_eq!(s.setting("x").unwrap().as_deref(), Some("2"));
    }
}
