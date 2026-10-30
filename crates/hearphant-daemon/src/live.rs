//! Nível estimado "agora": Leq em blocos de 3 s e detecção de volume alto.
//!
//! Blocos fechados (não média móvel) dão um número que muda a cada 3 s, sem
//! oscilar com a dinâmica da música. O aviso de volume alto usa histerese e
//! intervalo mínimo para não virar ruído (seção 12.4).

use std::time::{Duration, Instant};

/// Quadros por bloco (12 × 250 ms = 3 s).
pub const BLOCK_FRAMES: usize = 12;
/// Abaixo disso o nível não é exibido nem conta como som. Baixo de propósito:
/// música baixinha num fone intra-auricular fica na casa de 10–30 dB(A).
pub const FLOOR_DBA: f64 = 10.0;
/// Zona vermelha da UI: a referência semanal adulta chega em ≤ 4 h.
pub const LOUD_ON_DBA: f64 = 90.0;
/// Sai do estado "alto" só abaixo disto (histerese).
pub const LOUD_OFF_DBA: f64 = 85.0;
/// Blocos seguidos acima de `LOUD_ON_DBA` para considerar "alto" (6 s).
pub const LOUD_BLOCKS: u32 = 2;
/// Intervalo mínimo entre avisos de volume alto.
pub const ALERT_COOLDOWN: Duration = Duration::from_secs(10 * 60);
/// Um bloco mais velho que isso não é "agora".
const STALE: Duration = Duration::from_millis(4500);

#[derive(Debug, Default)]
pub struct LiveMeter {
    sum: f64,
    frames: usize,
    last: Option<(Option<f64>, Instant)>,
    loud_blocks: u32,
    loud: bool,
    last_alert: Option<Instant>,
}

/// Resultado de um bloco fechado.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Block {
    pub leq: Option<f64>,
    /// O estado "volume alto" mudou neste bloco.
    pub loud_changed: bool,
    /// Emitir aviso agora, com o nível que o motivou.
    pub alert: Option<f64>,
}

impl LiveMeter {
    /// Acrescenta a energia relativa 10^(L/10) de um quadro (0 em silêncio).
    pub fn push(&mut self, energy: f64, at: Instant) -> Option<Block> {
        self.sum += energy;
        self.frames += 1;
        if self.frames < BLOCK_FRAMES {
            return None;
        }
        let mean = self.sum / self.frames as f64;
        self.sum = 0.0;
        self.frames = 0;
        let leq = (mean > 0.0).then(|| 10.0 * mean.log10()).filter(|l| *l >= FLOOR_DBA);
        self.last = Some((leq, at));

        let was_loud = self.loud;
        if leq.is_some_and(|l| l >= LOUD_ON_DBA) {
            self.loud_blocks += 1;
        } else {
            self.loud_blocks = 0;
        }
        if !self.loud && self.loud_blocks >= LOUD_BLOCKS {
            self.loud = true;
        } else if self.loud && leq.is_none_or(|l| l < LOUD_OFF_DBA) {
            self.loud = false;
        }

        let mut alert = None;
        if self.loud && !was_loud && self.last_alert.is_none_or(|t| at.duration_since(t) >= ALERT_COOLDOWN) {
            self.last_alert = Some(at);
            alert = leq;
        }
        Some(Block { leq, loud_changed: self.loud != was_loud, alert })
    }

    /// Leq do último bloco, se for recente.
    pub fn current(&self, now: Instant) -> Option<f64> {
        self.last.filter(|(_, at)| now.saturating_duration_since(*at) <= STALE).and_then(|(l, _)| l)
    }

    pub fn loud(&self) -> bool {
        self.loud
    }

    /// Captura parou ou mudou de perfil: descarta o bloco em andamento.
    /// O intervalo entre avisos é mantido.
    pub fn reset(&mut self) {
        let last_alert = self.last_alert;
        *self = LiveMeter { last_alert, ..Default::default() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_millis(250);

    fn energy(dba: f64) -> f64 {
        10f64.powf(dba / 10.0)
    }

    /// Alimenta um bloco inteiro no nível dado; devolve o resultado.
    fn block(m: &mut LiveMeter, t: &mut Instant, dba: Option<f64>) -> Block {
        let mut out = None;
        for _ in 0..BLOCK_FRAMES {
            *t += FRAME;
            out = m.push(dba.map_or(0.0, energy), *t);
        }
        out.unwrap()
    }

    #[test]
    fn updates_once_per_block_with_energy_mean() {
        let mut m = LiveMeter::default();
        let mut t = Instant::now();
        for _ in 0..BLOCK_FRAMES - 1 {
            t += FRAME;
            assert!(m.push(energy(70.0), t).is_none());
        }
        assert_eq!(m.current(t), None, "sem bloco fechado ainda");
        t += FRAME;
        let b = m.push(0.0, t).unwrap();
        // 11 quadros a 70 + 1 em silêncio.
        assert!((b.leq.unwrap() - (70.0 + 10.0 * (11.0f64 / 12.0).log10())).abs() < 1e-9);
        assert_eq!(m.current(t + Duration::from_secs(1)), b.leq);
        assert_eq!(m.current(t + Duration::from_secs(5)), None, "bloco velho");
    }

    #[test]
    fn loud_needs_two_blocks_and_has_hysteresis() {
        let mut m = LiveMeter::default();
        let mut t = Instant::now();
        assert!(!block(&mut m, &mut t, Some(92.0)).loud_changed);
        let b = block(&mut m, &mut t, Some(92.0));
        assert!(b.loud_changed && m.loud());
        assert!(b.alert.is_some());
        assert_eq!(b.alert, b.leq);
        assert!(!block(&mut m, &mut t, Some(87.0)).loud_changed, "87 ainda é alto (histerese)");
        let b = block(&mut m, &mut t, Some(80.0));
        assert!(b.loud_changed && !m.loud());
    }

    #[test]
    fn alerts_respect_cooldown() {
        let mut m = LiveMeter::default();
        let mut t = Instant::now();
        block(&mut m, &mut t, Some(95.0));
        assert!(block(&mut m, &mut t, Some(95.0)).alert.is_some());
        block(&mut m, &mut t, None);
        block(&mut m, &mut t, Some(95.0));
        let again = block(&mut m, &mut t, Some(95.0));
        assert!(again.loud_changed && again.alert.is_none(), "dentro de 10 min: sem novo aviso");
        block(&mut m, &mut t, None);
        t += ALERT_COOLDOWN;
        block(&mut m, &mut t, Some(95.0));
        assert!(block(&mut m, &mut t, Some(95.0)).alert.is_some());
    }

    #[test]
    fn silence_and_tiny_energy_are_not_levels() {
        let mut m = LiveMeter::default();
        let mut t = Instant::now();
        assert_eq!(block(&mut m, &mut t, None).leq, None);
        assert_eq!(block(&mut m, &mut t, Some(5.0)).leq, None);
        assert!(block(&mut m, &mut t, Some(15.0)).leq.is_some(), "baixinho, mas audível");
    }

    #[test]
    fn reset_keeps_cooldown() {
        let mut m = LiveMeter::default();
        let mut t = Instant::now();
        block(&mut m, &mut t, Some(95.0));
        block(&mut m, &mut t, Some(95.0));
        m.reset();
        assert!(!m.loud());
        block(&mut m, &mut t, Some(95.0));
        assert!(block(&mut m, &mut t, Some(95.0)).alert.is_none());
    }
}
