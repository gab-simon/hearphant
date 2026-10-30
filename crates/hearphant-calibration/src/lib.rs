//! Estimativa automática de nível acústico por dispositivo (seção 9.3, B).
//!
//! Modelo: `L_A ≈ RMS_A(dBFS) + 20·log10(ganho do volume) + K`, onde `K` é o
//! nível no ouvido (dB SPL, ponderado A) para um sinal digital de 0 dB na
//! convenção de [`hearphant_audio::level`] (média quadrática = 1, ou seja,
//! um seno de escala cheia +3,01 dB) com o volume no máximo.
//!
//! Nenhum perfil aqui é calibração: todos são `ESTIMATED`, com incerteza
//! explícita. A decisão de mostrar dose estimada sem calibração é do produto
//! e diverge da RF-008 original da especificação; a UI sempre rotula
//! "estimativa" e mostra a faixa.

use hearphant_audio::{DeviceKind, OutputDevice};

/// Preferência do usuário para o tipo da saída (detecção automática erra em
/// DACs USB com porta "analog", por exemplo).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KindOverride {
    Auto,
    Headphone,
    Speaker,
}

impl KindOverride {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(Self::Auto),
            "headphone" => Some(Self::Headphone),
            "speaker" => Some(Self::Speaker),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Headphone => "headphone",
            Self::Speaker => "speaker",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// Identificador estável, gravado junto com cada segmento estimado.
    pub id: &'static str,
    pub label: &'static str,
    /// dB SPL(A) para média quadrática digital ponderada A = 1, volume máximo.
    pub k_db: f64,
    /// Meia largura da faixa provável, em dB.
    pub uncertainty_db: f64,
    /// De onde vieram os números (exibido na UI e no README).
    pub basis: &'static str,
}

/// Moondrop Chu II com cabo DSP (DAC KTMicro no cabo, cadeia fechada).
///
/// - Sensibilidade 119 dB/Vrms a 1 kHz, 18 Ω (especificação do fabricante).
/// - Cabo: −0,43 dBV em 0 dBFS (seno) no volume máximo, impedância de saída
///   3 Ω (Reference Audio Analyzer, relatório "Moondrop Chu 2 DSP Cable").
/// - Divisor 18/(18+3) Ω: −1,34 dB. Convenção do medidor: +3,01 dB.
///
/// K = 119 − 0,43 − 1,34 + 3,01 ≈ 120,2 dB. Incerteza ±5 dB: tolerância de
/// sensibilidade, vedação no ouvido, resposta não plana do fone versus a
/// ponderação A digital e a carga do teste de tensão não declarada.
pub const MOONDROP_CHU2_DSP: Profile = Profile {
    id: "moondrop-chu2-dsp-v1",
    label: "Moondrop Chu II DSP",
    k_db: 120.2,
    uncertainty_db: 5.0,
    basis: "Sensibilidade do fabricante (119 dB/Vrms) e tensão medida do cabo DSP (Reference Audio Analyzer).",
};

/// Fone genérico no ponto de projeto da EN 50332: tocador no limite (150 mV
/// com ruído de programa a −10 dBFS re. seno) e fone de referência
/// (94 dB(A) com 75 mV) dão 100 dB(A) no volume máximo.
///
/// −10 dBFS re. seno = −13,01 dB na convenção do medidor → K = 113,01 dB.
/// Incerteza ±10 dB: PCs não seguem a EN 50332 e fones variam muito.
pub const GENERIC_HEADPHONE: Profile = Profile {
    id: "generic-headphone-en50332-v1",
    label: "Fone genérico (EN 50332)",
    k_db: 113.01,
    uncertainty_db: 10.0,
    basis: "Ponto de projeto da EN 50332 (100 dB(A) no volume máximo); dispositivo sem perfil próprio.",
};

#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    Estimate {
        profile: Profile,
        /// O tipo "fone" foi presumido (saída sem evidência de tipo).
        kind_presumed: bool,
    },
    Unavailable {
        reason: &'static str,
    },
}

impl Resolution {
    pub fn profile(&self) -> Option<&Profile> {
        match self {
            Resolution::Estimate { profile, .. } => Some(profile),
            Resolution::Unavailable { .. } => None,
        }
    }
}

fn is_chu2_dsp(out: &OutputDevice) -> bool {
    out.stable_name.contains("Chu2_DSP") || out.description.starts_with("Chu2 DSP")
}

/// Escolhe o perfil de estimativa para a saída.
pub fn resolve(out: &OutputDevice, kind: KindOverride) -> Resolution {
    let effective = match kind {
        KindOverride::Headphone => DeviceKind::Headphone,
        KindOverride::Speaker => DeviceKind::Speaker,
        KindOverride::Auto => out.kind,
    };
    if effective == DeviceKind::Speaker {
        return Resolution::Unavailable {
            reason: "Caixas de som dependem da distância e da sala; não há estimativa sem medição no local.",
        };
    }
    if is_chu2_dsp(out) {
        return Resolution::Estimate { profile: MOONDROP_CHU2_DSP, kind_presumed: false };
    }
    Resolution::Estimate { profile: GENERIC_HEADPHONE, kind_presumed: effective == DeviceKind::Unknown }
}

/// Nível estimado no ouvido para um quadro, em dB(A). `None` em silêncio
/// digital ou mudo (energia zero).
pub fn estimate_laeq(profile: &Profile, mean_square_a: f64, linear_gain: f64, muted: bool) -> Option<f64> {
    if muted || mean_square_a <= 0.0 || linear_gain <= 0.0 {
        return None;
    }
    Some(10.0 * mean_square_a.log10() + 20.0 * linear_gain.log10() + profile.k_db)
}

/// Fator multiplicativo da faixa de dose para uma incerteza em dB.
pub fn dose_range_factor(uncertainty_db: f64) -> f64 {
    10f64.powf(uncertainty_db / 10.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, desc: &str, kind: DeviceKind) -> OutputDevice {
        OutputDevice {
            node_id: 1,
            stable_name: name.into(),
            description: desc.into(),
            kind,
            kind_evidence: None,
            bus: None,
            state: "running".into(),
            volume: None,
            streams: Vec::new(),
            is_default: true,
        }
    }

    #[test]
    fn chu2_dsp_is_detected_by_usb_name() {
        let dev = out("alsa_output.usb-KTMicro_Chu2_DSP_2020-02-20-0000-0000-0000-00.analog-stereo", "Chu2 DSP Analog Stereo", DeviceKind::Unknown);
        assert_eq!(resolve(&dev, KindOverride::Auto).profile(), Some(&MOONDROP_CHU2_DSP));
    }

    #[test]
    fn unknown_output_presumes_headphone_with_generic_profile() {
        let dev = out("alsa_output.pci-0000.hdmi-stereo", "HDMI", DeviceKind::Unknown);
        assert_eq!(resolve(&dev, KindOverride::Auto), Resolution::Estimate { profile: GENERIC_HEADPHONE, kind_presumed: true });
    }

    #[test]
    fn speakers_have_no_estimate_even_if_overridden_later() {
        let spk = out("x", "Built-in Audio", DeviceKind::Speaker);
        assert!(matches!(resolve(&spk, KindOverride::Auto), Resolution::Unavailable { .. }));
        assert!(resolve(&spk, KindOverride::Headphone).profile().is_some());
        let chu = out("KTMicro_Chu2_DSP", "Chu2 DSP", DeviceKind::Unknown);
        assert!(matches!(resolve(&chu, KindOverride::Speaker), Resolution::Unavailable { .. }));
    }

    #[test]
    fn generic_profile_hits_en50332_design_point() {
        // Ruído a −10 dBFS re. seno (−13,01 dB na convenção) no volume máximo → 100 dB(A).
        let ms = 10f64.powf(-13.0103 / 10.0);
        let l = estimate_laeq(&GENERIC_HEADPHONE, ms, 1.0, false).unwrap();
        assert!((l - 100.0).abs() < 0.01, "{l}");
    }

    #[test]
    fn volume_and_mute_are_applied() {
        let l_full = estimate_laeq(&MOONDROP_CHU2_DSP, 0.01, 1.0, false).unwrap();
        let l_vol = estimate_laeq(&MOONDROP_CHU2_DSP, 0.01, 0.010653, false).unwrap();
        assert!((l_full - l_vol - 39.45).abs() < 0.01);
        assert_eq!(estimate_laeq(&MOONDROP_CHU2_DSP, 0.01, 1.0, true), None);
        assert_eq!(estimate_laeq(&MOONDROP_CHU2_DSP, 0.0, 1.0, false), None);
    }

    #[test]
    fn range_factor() {
        assert!((dose_range_factor(10.0) - 10.0).abs() < 1e-12);
        assert!((dose_range_factor(5.0) - 3.1623).abs() < 1e-4);
    }
}
