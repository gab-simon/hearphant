//! Nível do sinal digital da saída (seção 10.3, `audio-observer`).
//!
//! Captura o monitor da saída com `parec`, calcula RMS e pico por quadro e
//! descarta as amostras: nada é gravado (RNF-002). O monitor do PipeWire não
//! aplica o volume da saída (`monitorVolumes = 1`), então o resultado é o
//! nível do **conteúdo** em dBFS, não o que chega ao ouvido e nunca dBA.

use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

use super::die_with_parent;

/// Abaixo disso o quadro conta como silêncio digital (RF-002) quando não há
/// perfil para estimar o nível no ouvido. Fica perto do silêncio de fato
/// (players pausados mandam zeros; dither fica em torno de −96 dBFS): o
/// monitor vem depois do volume de cada aplicativo, então música com o app
/// em 1% já chega a −60/−75 dBFS e ainda é audível.
pub const SILENCE_DBFS: f64 = -80.0;

const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LevelFrame {
    pub at: Instant,
    pub duration: Duration,
    /// Média quadrática de todas as amostras de todos os canais (energia
    /// média por canal, nunca média de dB).
    pub mean_square: f64,
    /// Igual a `mean_square`, após ponderação A (IEC 61672) no domínio digital.
    pub mean_square_a: f64,
    pub peak: f64,
}

impl LevelFrame {
    pub fn rms_dbfs(&self) -> f64 {
        to_dbfs(self.mean_square)
    }

    pub fn peak_dbfs(&self) -> f64 {
        to_dbfs(self.peak * self.peak)
    }

    pub fn is_audible(&self) -> bool {
        self.rms_dbfs() > SILENCE_DBFS
    }

    /// RMS ponderado A em dBFS (mesma convenção de [`Self::rms_dbfs`]).
    pub fn rms_dbfs_a(&self) -> f64 {
        to_dbfs(self.mean_square_a)
    }
}

/// Biquad em forma direta II transposta.
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b: [f64; 3],
    a: [f64; 2],
    z: [f64; 2],
}

impl Biquad {
    /// Transformação bilinear de `(b2 s² + b1 s + b0) / (s² + a1 s + a0)`.
    fn bilinear(b: [f64; 3], a: [f64; 2], rate: f64) -> Self {
        let k = 2.0 * rate;
        let k2 = k * k;
        let [b0, b1, b2] = b;
        let [a0, a1] = a;
        let n = k2 + a1 * k + a0;
        Biquad {
            b: [(b2 * k2 + b1 * k + b0) / n, (2.0 * b0 - 2.0 * b2 * k2) / n, (b2 * k2 - b1 * k + b0) / n],
            a: [(2.0 * a0 - 2.0 * k2) / n, (k2 - a1 * k + a0) / n],
            z: [0.0; 2],
        }
    }

    fn process(&mut self, x: f64) -> f64 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }

    fn response(&self, w: f64) -> num::Complex {
        let z1 = num::Complex::expi(-w);
        let z2 = num::Complex::expi(-2.0 * w);
        let num = num::Complex::real(self.b[0]) + z1.scale(self.b[1]) + z2.scale(self.b[2]);
        let den = num::Complex::real(1.0) + z1.scale(self.a[0]) + z2.scale(self.a[1]);
        num.div(den)
    }
}

mod num {
    #[derive(Debug, Clone, Copy)]
    pub struct Complex(pub f64, pub f64);
    impl Complex {
        pub fn real(x: f64) -> Self {
            Complex(x, 0.0)
        }
        pub fn expi(w: f64) -> Self {
            Complex(w.cos(), w.sin())
        }
        pub fn scale(self, k: f64) -> Self {
            Complex(self.0 * k, self.1 * k)
        }
        pub fn div(self, o: Self) -> Self {
            let d = o.0 * o.0 + o.1 * o.1;
            Complex((self.0 * o.0 + self.1 * o.1) / d, (self.1 * o.0 - self.0 * o.1) / d)
        }
        pub fn abs(self) -> f64 {
            self.0.hypot(self.1)
        }
    }
    impl std::ops::Add for Complex {
        type Output = Self;
        fn add(self, o: Self) -> Self {
            Complex(self.0 + o.0, self.1 + o.1)
        }
    }
}

/// Filtro de ponderação A (IEC 61672-1) por transformação bilinear,
/// normalizado para 0 dB em 1 kHz. Erro ≤ 0,7 dB até 12,5 kHz a 48 kHz;
/// irrelevante diante da incerteza do perfil (±5 a ±10 dB).
#[derive(Debug, Clone)]
pub struct AWeighting {
    sections: [Biquad; 3],
    gain: f64,
}

impl AWeighting {
    pub fn new(rate: f64) -> Self {
        // Polos pré-distorcidos (prewarping) para a bilinear acertar a
        // frequência de cada polo; sem isso o erro em 10 kHz passa de 1 dB.
        let warp = |hz: f64| 2.0 * rate * (std::f64::consts::PI * hz / rate).tan();
        let w1 = warp(20.598_997);
        let w2 = warp(107.652_65);
        let w3 = warp(737.862_23);
        let w4 = warp(12_194.217);
        let sections = [
            Biquad::bilinear([0.0, 0.0, 1.0], [w1 * w1, 2.0 * w1], rate),
            Biquad::bilinear([0.0, 0.0, 1.0], [w2 * w3, w2 + w3], rate),
            Biquad::bilinear([1.0, 0.0, 0.0], [w4 * w4, 2.0 * w4], rate),
        ];
        let mut f = AWeighting { sections, gain: 1.0 };
        f.gain = 1.0 / f.magnitude(1000.0, rate);
        f
    }

    fn magnitude(&self, freq: f64, rate: f64) -> f64 {
        let w = 2.0 * std::f64::consts::PI * freq / rate;
        self.sections.iter().map(|s| s.response(w).abs()).product::<f64>() * self.gain
    }

    pub fn process(&mut self, x: f64) -> f64 {
        self.sections.iter_mut().fold(x * self.gain, |acc, s| s.process(acc))
    }
}

fn to_dbfs(mean_square: f64) -> f64 {
    if mean_square > 0.0 {
        10.0 * mean_square.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// Estatísticas de um bloco de amostras f32 intercaladas: (média quadrática,
/// média quadrática ponderada A, pico). Um filtro por canal, com estado
/// mantido entre blocos.
fn analyze(samples: &[f32], filters: &mut [AWeighting]) -> (f64, f64, f64) {
    let mut sum = 0.0f64;
    let mut sum_a = 0.0f64;
    let mut peak = 0.0f64;
    for (i, &x) in samples.iter().enumerate() {
        let x = f64::from(x);
        sum += x * x;
        let y = filters[i % filters.len()].process(x);
        sum_a += y * y;
        peak = peak.max(x.abs());
    }
    let n = samples.len().max(1) as f64;
    (sum / n, sum_a / n, peak)
}

/// Captura o monitor de `sink_name` e envia um [`LevelFrame`] a cada `frame`.
/// O canal fecha quando a captura termina (saída removida, PipeWire caiu).
/// Soltar o receptor encerra o `parec`.
pub fn capture(sink_name: &str, frame: Duration) -> Result<mpsc::Receiver<LevelFrame>> {
    let mut cmd = Command::new("parec");
    cmd.arg(format!("--device={sink_name}.monitor"))
        .args(["--raw", "--format=float32le"])
        .arg(format!("--rate={RATE}"))
        .arg(format!("--channels={CHANNELS}"))
        .args(["--latency-msec=100", "--client-name=HearPhant", "--stream-name=Análise de nível (sem gravação)"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    die_with_parent(&mut cmd);
    let mut child = cmd.spawn().context("não foi possível executar parec (pipewire-pulse instalado?)")?;
    let mut stdout = child.stdout.take().context("parec sem stdout")?;

    let samples_per_frame = (RATE as f64 * frame.as_secs_f64()) as usize * CHANNELS as usize;
    let (tx, rx) = mpsc::channel(8);
    tokio::spawn(async move {
        let _child = child;
        let mut bytes = vec![0u8; samples_per_frame * 4];
        let mut samples = vec![0f32; samples_per_frame];
        let mut filters = vec![AWeighting::new(f64::from(RATE)); CHANNELS as usize];
        loop {
            // Encerra o parec assim que ninguém mais escuta, sem esperar o
            // próximo quadro.
            tokio::select! {
                r = stdout.read_exact(&mut bytes) => if r.is_err() { return },
                _ = tx.closed() => return,
            }
            for (s, b) in samples.iter_mut().zip(bytes.as_chunks::<4>().0) {
                *s = f32::from_le_bytes(*b);
            }
            let (mean_square, mean_square_a, peak) = analyze(&samples, &mut filters);
            let level = LevelFrame { at: Instant::now(), duration: frame, mean_square, mean_square_a, peak };
            if tx.send(level).await.is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(mean_square: f64) -> LevelFrame {
        LevelFrame { at: Instant::now(), duration: Duration::from_millis(250), mean_square, mean_square_a: mean_square, peak: 0.0 }
    }

    fn sine(freq: f64) -> Vec<f32> {
        (0..48_000).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / 48_000.0).sin() as f32).collect()
    }

    /// Atenuação medida do filtro A para um seno, descartando o transitório.
    fn a_weighting_db(freq: f64) -> f64 {
        let mut f = vec![AWeighting::new(48_000.0)];
        let s = sine(freq);
        analyze(&s, &mut f);
        let (ms, ms_a, _) = analyze(&s, &mut f);
        10.0 * (ms_a / ms).log10()
    }

    #[test]
    fn a_weighting_matches_iec_61672_table() {
        // Valores nominais da IEC 61672-1; tolerância folgada para a bilinear.
        for (freq, expected, tol) in
            [(31.5, -39.4, 0.5), (100.0, -19.1, 0.3), (1000.0, 0.0, 0.05), (4000.0, 1.0, 0.4), (10_000.0, -2.5, 0.7)]
        {
            let got = a_weighting_db(freq);
            assert!((got - expected).abs() <= tol, "{freq} Hz: esperado {expected} dB, obtido {got:.2}");
        }
    }

    #[test]
    fn full_scale_sine_is_minus_3_dbfs() {
        let (ms, _, peak) = analyze(&sine(1000.0), &mut [AWeighting::new(48_000.0)]);
        assert!((to_dbfs(ms) + 3.0103).abs() < 0.01, "{}", to_dbfs(ms));
        assert!((peak - 1.0).abs() < 1e-3);
    }

    #[test]
    fn silence_threshold() {
        assert!(!frame(0.0).is_audible());
        assert_eq!(frame(0.0).rms_dbfs(), f64::NEG_INFINITY);
        assert!(!frame(1e-9).is_audible()); // −90 dBFS
        assert!(frame(1e-7).is_audible()); // −70 dBFS: app com volume baixo
        assert!(frame(1e-4).is_audible()); // −40 dBFS
    }
}
