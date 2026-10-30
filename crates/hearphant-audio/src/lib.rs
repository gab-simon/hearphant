//! Descoberta de saídas de áudio pelo PipeWire (seção 10.3, `audio-discovery`).
//!
//! PoC da Fase 1: lê o grafo com `pw-dump` em vez de usar libpipewire. Expõe
//! apenas grandezas digitais (volume do mixer, mudo, streams ligados). Nada
//! aqui é nível acústico: o volume de 22% não diz nada sobre dBA no ouvido.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

/// Categoria aparente da saída. Não equivale a calibração (seção 6.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Headphone,
    Speaker,
    Unknown,
}

impl DeviceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DeviceKind::Headphone => "headphone",
            DeviceKind::Speaker => "speaker",
            DeviceKind::Unknown => "unknown",
        }
    }
}

/// Volume do mixer da saída, como o `wpctl` mostra. Valor digital.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DigitalVolume {
    /// Escala cúbica do `wpctl`/GNOME (0,22 → 22%).
    pub percent: f64,
    /// Ganho linear efetivamente aplicado (produto `volume × channelVolumes` máx.).
    pub linear_gain: f64,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub node_id: u32,
    pub application: Option<String>,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputDevice {
    pub node_id: u32,
    /// `node.name`: estável entre reinícios (inclui serial USB quando há).
    pub stable_name: String,
    pub description: String,
    pub kind: DeviceKind,
    /// Evidência usada para `kind` (ex.: `port.type=headphones`).
    pub kind_evidence: Option<String>,
    pub bus: Option<String>,
    pub state: String,
    pub volume: Option<DigitalVolume>,
    pub streams: Vec<StreamInfo>,
    pub is_default: bool,
}

impl OutputDevice {
    /// Há stream de aplicativo rodando nesta saída. É estado do grafo, não
    /// detecção de sinal: um player pausado com stream aberto ainda conta.
    pub fn has_running_stream(&self) -> bool {
        self.streams.iter().any(|s| s.running)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioSnapshot {
    pub outputs: Vec<OutputDevice>,
}

impl AudioSnapshot {
    pub fn default_output(&self) -> Option<&OutputDevice> {
        self.outputs.iter().find(|o| o.is_default)
    }
}

fn str_prop<'a>(props: &'a Value, key: &str) -> Option<&'a str> {
    props.get(key).and_then(Value::as_str)
}

fn u32_of(v: &Value) -> Option<u32> {
    v.as_u64().and_then(|n| u32::try_from(n).ok())
}

fn object_type(obj: &Value) -> &str {
    obj.get("type").and_then(Value::as_str).unwrap_or("").rsplit(':').next().unwrap_or("")
}

fn default_sink_name(objects: &[Value]) -> Option<String> {
    objects
        .iter()
        .filter(|o| object_type(o) == "Metadata")
        .filter(|o| o.pointer("/props/metadata.name").and_then(Value::as_str) == Some("default"))
        .flat_map(|o| o.get("metadata").and_then(Value::as_array).cloned().unwrap_or_default())
        .find(|m| m.get("key").and_then(Value::as_str) == Some("default.audio.sink"))
        .and_then(|m| m.pointer("/value/name").and_then(Value::as_str).map(str::to_owned))
}

fn parse_volume(info: &Value) -> Option<DigitalVolume> {
    let props = info.pointer("/params/Props")?.as_array()?;
    let p = props.iter().find(|p| p.get("channelVolumes").is_some())?;
    let master = p.get("volume").and_then(Value::as_f64).unwrap_or(1.0);
    let channels = p.get("channelVolumes")?.as_array()?;
    let max_channel = channels.iter().filter_map(Value::as_f64).fold(None, |acc: Option<f64>, v| {
        Some(acc.map_or(v, |a| a.max(v)))
    })?;
    let linear_gain = master * max_channel;
    Some(DigitalVolume {
        percent: linear_gain.max(0.0).cbrt() * 100.0,
        linear_gain,
        muted: p.get("mute").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Tipo pela rota de saída ativa do dispositivo (`port.type`) e, na falta
/// dela, por `device.form-factor`. Sem evidência, `Unknown`.
fn classify(node_props: &Value, device: Option<&Value>) -> (DeviceKind, Option<String>) {
    let profile_device = node_props.get("card.profile.device").and_then(u32_of);
    if let Some(dev) = device {
        let routes = dev.pointer("/info/params/Route").and_then(Value::as_array);
        let route = routes.into_iter().flatten().find(|r| {
            r.get("direction").and_then(Value::as_str) == Some("Output")
                && (profile_device.is_none() || r.get("device").and_then(u32_of) == profile_device)
        });
        // `info` da rota é uma lista plana [n, chave, valor, chave, valor, ...].
        let port_type = route.and_then(|r| r.get("info")).and_then(Value::as_array).and_then(|info| {
            info.iter()
                .skip(1)
                .collect::<Vec<_>>()
                .chunks(2)
                .find(|kv| kv[0].as_str() == Some("port.type"))
                .and_then(|kv| kv.get(1).and_then(|v| v.as_str()))
                .map(str::to_owned)
        });
        match port_type.as_deref() {
            Some("headphones" | "headset") => {
                return (DeviceKind::Headphone, Some(format!("port.type={}", port_type.unwrap())))
            }
            Some("speaker") => return (DeviceKind::Speaker, Some("port.type=speaker".into())),
            _ => {}
        }
        let form = dev.pointer("/info/props/device.form-factor").and_then(Value::as_str);
        match form {
            Some(f @ ("headphone" | "headset" | "hands-free")) => {
                return (DeviceKind::Headphone, Some(format!("device.form-factor={f}")))
            }
            // "internal" só indica alto-falante quando a rota não diz o contrário.
            Some(f @ ("speaker" | "internal")) if port_type.is_none() => {
                return (DeviceKind::Speaker, Some(format!("device.form-factor={f}")));
            }
            _ => {}
        }
        if let Some(pt) = port_type {
            return (DeviceKind::Unknown, Some(format!("port.type={pt}")));
        }
    }
    (DeviceKind::Unknown, None)
}

/// Interpreta a saída JSON de `pw-dump`.
pub fn parse_pw_dump(json: &str) -> Result<AudioSnapshot> {
    let root: Value = serde_json::from_str(json).context("JSON do pw-dump inválido")?;
    let Some(objects) = root.as_array() else {
        bail!("pw-dump não retornou uma lista de objetos");
    };

    let default_name = default_sink_name(objects);
    let find_by_id = |id: u32| objects.iter().find(|o| o.get("id").and_then(u32_of) == Some(id));

    let links: Vec<(u32, u32)> = objects
        .iter()
        .filter(|o| object_type(o) == "Link")
        .filter_map(|o| {
            let info = o.get("info")?;
            Some((info.get("output-node-id").and_then(u32_of)?, info.get("input-node-id").and_then(u32_of)?))
        })
        .collect();

    let mut outputs = Vec::new();
    for obj in objects.iter().filter(|o| object_type(o) == "Node") {
        let Some(info) = obj.get("info") else { continue };
        let Some(props) = info.get("props") else { continue };
        if str_prop(props, "media.class") != Some("Audio/Sink") {
            continue;
        }
        let Some(node_id) = obj.get("id").and_then(u32_of) else { continue };
        let stable_name = str_prop(props, "node.name").unwrap_or_default().to_owned();
        let device = props.get("device.id").and_then(u32_of).and_then(find_by_id);
        let (kind, kind_evidence) = classify(props, device);

        let mut stream_ids: Vec<u32> =
            links.iter().filter(|(_, input)| *input == node_id).map(|(out, _)| *out).collect();
        stream_ids.sort_unstable();
        stream_ids.dedup();
        let streams = stream_ids
            .into_iter()
            .filter_map(find_by_id)
            .filter(|s| s.pointer("/info/props/media.class").and_then(Value::as_str) == Some("Stream/Output/Audio"))
            .map(|s| StreamInfo {
                node_id: s.get("id").and_then(u32_of).unwrap_or(0),
                application: s.pointer("/info/props/application.name").and_then(Value::as_str).map(str::to_owned),
                running: s.pointer("/info/state").and_then(Value::as_str) == Some("running"),
            })
            .collect();

        outputs.push(OutputDevice {
            node_id,
            is_default: default_name.as_deref() == Some(stable_name.as_str()),
            description: str_prop(props, "node.description")
                .or_else(|| str_prop(props, "node.nick"))
                .unwrap_or(&stable_name)
                .to_owned(),
            stable_name,
            kind,
            kind_evidence,
            bus: device.and_then(|d| d.pointer("/info/props/device.bus")).and_then(Value::as_str).map(str::to_owned),
            state: info.get("state").and_then(Value::as_str).unwrap_or("unknown").to_owned(),
            volume: parse_volume(info),
            streams,
        });
    }
    outputs.sort_by_key(|o| o.node_id);
    Ok(AudioSnapshot { outputs })
}

/// Executa `pw-dump` uma vez.
pub async fn snapshot() -> Result<AudioSnapshot> {
    let out = Command::new("pw-dump")
        .arg("--no-colors")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .context("não foi possível executar pw-dump (PipeWire instalado?)")?;
    if !out.status.success() {
        bail!("pw-dump terminou com {}", out.status);
    }
    parse_pw_dump(&String::from_utf8_lossy(&out.stdout))
}

pub mod level;

/// Processos auxiliares morrem com o daemon, mesmo sob SIGKILL, em vez de
/// ficarem órfãos.
fn die_with_parent(cmd: &mut Command) {
    cmd.kill_on_drop(true);
    // SAFETY: prctl é async-signal-safe e só altera o processo filho.
    unsafe {
        cmd.pre_exec(|| {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Tipos de objeto que afetam o estado das saídas.
const RELEVANT_TYPES: [&str; 4] = ["Node", "Link", "Device", "Metadata"];

/// Filtra blocos de `pw-dump --monitor` que não afetam saídas de áudio.
/// Sem isso, os clientes criados pelo próprio `pw-dump` de [`snapshot`]
/// realimentam o monitor indefinidamente.
#[derive(Debug, Default)]
struct MonitorFilter {
    relevant_ids: std::collections::HashSet<u64>,
}

impl MonitorFilter {
    /// `true` se o bloco (uma lista JSON) muda algo relevante.
    fn is_relevant(&mut self, block: &str) -> bool {
        let Ok(Value::Array(objects)) = serde_json::from_str::<Value>(block) else {
            // Bloco ilegível: melhor atualizar do que perder uma mudança.
            return true;
        };
        let mut relevant = false;
        for obj in &objects {
            let Some(id) = obj.get("id").and_then(Value::as_u64) else { continue };
            if obj.get("info").is_some_and(Value::is_null) || obj.get("type").is_none() {
                // Remoção: só importa se o objeto era relevante.
                relevant |= self.relevant_ids.remove(&id);
            } else if RELEVANT_TYPES.contains(&object_type(obj)) {
                self.relevant_ids.insert(id);
                relevant = true;
            }
        }
        relevant
    }
}

/// Notifica mudanças no grafo do PipeWire sem polling (RNF-004): acompanha
/// `pw-dump --monitor` e envia um aviso após `debounce` sem novas mudanças
/// relevantes. O canal fecha se o monitor terminar (PipeWire reiniciado).
pub fn watch_changes(debounce: Duration) -> Result<mpsc::Receiver<()>> {
    let mut cmd = Command::new("pw-dump");
    cmd.args(["--monitor", "--no-colors"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    die_with_parent(&mut cmd);
    let mut child = cmd
        .spawn()
        .context("não foi possível executar pw-dump --monitor")?;
    let stdout = child.stdout.take().context("pw-dump sem stdout")?;

    // Leitor: separa blocos (cada um termina numa linha "]") e repassa só os
    // relevantes. Fica numa tarefa própria para nunca ser cancelado no meio.
    let (relevant_tx, mut relevant_rx) = mpsc::unbounded_channel::<()>();
    tokio::spawn(async move {
        let _child = child;
        let mut lines = BufReader::new(stdout).lines();
        let mut filter = MonitorFilter::default();
        let mut block = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            let end = line == "]";
            block.push_str(&line);
            block.push('\n');
            if end {
                if filter.is_relevant(&block) && relevant_tx.send(()).is_err() {
                    return;
                }
                block.clear();
            }
        }
    });

    // Debounce: agrupa a rajada de blocos de uma mesma mudança.
    let (tx, rx) = mpsc::channel(1);
    tokio::spawn(async move {
        while relevant_rx.recv().await.is_some() {
            loop {
                match tokio::time::timeout(debounce, relevant_rx.recv()).await {
                    Ok(Some(())) => continue,
                    Ok(None) => return,
                    Err(_) => break,
                }
            }
            if tx.send(()).await.is_err() {
                return;
            }
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../../../tests/fixtures/pw-dump-usb-dac.json");

    #[test]
    fn finds_default_sink_and_its_streams() {
        let snap = parse_pw_dump(FIXTURE).unwrap();
        assert_eq!(snap.outputs.len(), 3);
        let out = snap.default_output().unwrap();
        assert_eq!(out.node_id, 70);
        assert_eq!(out.description, "Chu2 DSP Analog Stereo");
        assert_eq!(out.streams.len(), 1, "links duplicados por canal contam um stream");
        assert!(out.has_running_stream());
        assert_eq!(out.streams[0].application.as_deref(), Some("Zen"));
    }

    #[test]
    fn volume_uses_wpctl_cubic_scale() {
        let snap = parse_pw_dump(FIXTURE).unwrap();
        let vol = snap.default_output().unwrap().volume.unwrap();
        assert!((vol.percent - 22.0).abs() < 0.01, "{}", vol.percent);
        assert!(!vol.muted);
    }

    #[test]
    fn analog_port_on_usb_dac_is_unknown_not_headphone() {
        let snap = parse_pw_dump(FIXTURE).unwrap();
        let out = snap.default_output().unwrap();
        assert_eq!(out.kind, DeviceKind::Unknown);
        assert_eq!(out.kind_evidence.as_deref(), Some("port.type=analog"));
    }

    #[test]
    fn classifies_by_route_port_type() {
        let snap = parse_pw_dump(FIXTURE).unwrap();
        let by_id = |id| snap.outputs.iter().find(|o| o.node_id == id).unwrap();
        assert_eq!(by_id(34).kind, DeviceKind::Speaker);
        assert!(by_id(34).volume.unwrap().muted);
        assert_eq!(by_id(80).kind, DeviceKind::Headphone);
        assert!(!by_id(80).is_default);
    }

    #[test]
    fn monitor_ignores_client_churn_from_own_snapshots() {
        let mut f = MonitorFilter::default();
        let client = r#"[{"id": 89, "type": "PipeWire:Interface:Client", "info": {"props": {}}}]"#;
        let client_gone = r#"[{"id": 89, "info": null}]"#;
        assert!(!f.is_relevant(client));
        assert!(!f.is_relevant(client_gone));

        let node = r#"[{"id": 82, "type": "PipeWire:Interface:Node", "info": {"state": "running"}}]"#;
        let node_gone = r#"[{"id": 82, "info": null}]"#;
        assert!(f.is_relevant(node));
        assert!(f.is_relevant(node_gone), "remoção de nó conhecido importa");
        assert!(!f.is_relevant(node_gone), "segunda remoção do mesmo id não");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_pw_dump("{}").is_err());
        assert!(parse_pw_dump("not json").is_err());
        assert_eq!(parse_pw_dump("[]").unwrap().outputs.len(), 0);
    }
}
