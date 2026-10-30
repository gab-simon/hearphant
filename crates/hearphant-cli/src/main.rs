//! `hearphant`: CLI de diagnóstico.
//!
//!   hearphant devices      saídas detectadas no PipeWire (direto, sem daemon)
//!   hearphant status       estado publicado pelo serviço via D-Bus
//!   hearphant reference    tempos equivalentes à referência semanal (teoria)

use std::collections::BTreeMap;

use anyhow::{bail, Result};
use hearphant_dose::{time_to_reference_hours, ReferenceMode};
use zbus::zvariant::OwnedValue;

const BUS_NAME: &str = "io.github.gab_simon.HearPhant1";
const OBJECT_PATH: &str = "/io/github/gab_simon/HearPhant1";

const NOT_DBA: &str = "volume é valor digital do mixer, não é dBA";

async fn devices() -> Result<()> {
    let snap = hearphant_audio::snapshot().await?;
    if snap.outputs.is_empty() {
        println!("Nenhuma saída de áudio encontrada.");
        return Ok(());
    }
    for o in &snap.outputs {
        println!("{} {} [{}]", if o.is_default { "*" } else { " " }, o.description, o.node_id);
        println!("    id estável : {}", o.stable_name);
        let evidence = o.kind_evidence.as_deref().unwrap_or("sem evidência");
        println!("    tipo       : {} ({evidence})", o.kind.as_str());
        if let Some(bus) = &o.bus {
            println!("    barramento : {bus}");
        }
        println!("    estado     : {}", o.state);
        match o.volume {
            Some(v) => println!(
                "    volume     : {:.0}%{} ({NOT_DBA})",
                v.percent,
                if v.muted { " · mudo" } else { "" }
            ),
            None => println!("    volume     : indisponível"),
        }
        let apps: Vec<String> = o
            .streams
            .iter()
            .map(|s| {
                let name = s.application.as_deref().unwrap_or("?");
                if s.running { name.to_owned() } else { format!("{name} (parado)") }
            })
            .collect();
        if !apps.is_empty() {
            println!("    streams    : {}", apps.join(", "));
        }
        println!("    medição    : UNVERIFIED (sem perfil de calibração; dose indisponível)");
    }
    Ok(())
}

async fn status() -> Result<()> {
    let conn = zbus::Connection::session().await?;
    let proxy = zbus::Proxy::new(&conn, BUS_NAME, OBJECT_PATH, BUS_NAME).await?;
    let status: BTreeMap<String, OwnedValue> = match proxy.call("GetStatus", &()).await {
        Ok(s) => s,
        Err(zbus::Error::MethodError(name, _, _)) if name.as_str() == "org.freedesktop.DBus.Error.ServiceUnknown" => {
            bail!("o serviço {BUS_NAME} não está em execução (inicie hearphant-daemon)")
        }
        Err(e) => return Err(e.into()),
    };
    for (key, value) in &status {
        println!("{key:20} {}", **value);
    }
    if !status.contains_key("dose_percent") {
        println!("{:20} indisponível (sem medição acústica calibrada)", "dose_percent");
    }
    if status.contains_key("volume_percent") {
        println!("\nObs.: {NOT_DBA}.");
    }
    Ok(())
}

fn reference() {
    println!("Tempo até 100% da referência semanal a nível constante (OMS/UIT, teórico):\n");
    println!("  nível   adulto (80 dBA/40 h)   conservador (75 dBA/40 h)");
    for level in [70.0, 75.0, 80.0, 85.0, 90.0, 95.0, 100.0, 105.0] {
        let fmt = |h: f64| {
            if h >= 1.0 { format!("{h:.1} h") } else { format!("{:.1} min", h * 60.0) }
        };
        println!(
            "  {level:>3} dBA  {:>20}   {:>24}",
            fmt(time_to_reference_hours(level, ReferenceMode::Adult)),
            fmt(time_to_reference_hours(level, ReferenceMode::Conservative))
        );
    }
    println!("\nSão níveis acústicos no ouvido (dBA SPL), não porcentagens de volume.");
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("devices") => devices().await,
        Some("status") => status().await,
        Some("reference") => {
            reference();
            Ok(())
        }
        _ => {
            eprintln!("uso: hearphant <devices|status|reference>");
            std::process::exit(2);
        }
    }
}
