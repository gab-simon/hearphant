# HearPhant

Acompanhamento de exposição sonora no Linux, com a Ellie como interface.
Especificação completa: [`SafeListen_Documentacao_Tecnica.md`](SafeListen_Documentacao_Tecnica.md) (escrita com o nome provisório anterior).

> Não é dispositivo médico. A dose exibida é uma **estimativa automática** com faixa de
> incerteza, não uma medição calibrada. Volume percentual nunca é convertido em dBA sozinho:
> a estimativa usa o sinal real (ponderado A), o ganho do volume e um perfil do dispositivo.
>
> **Divergência da especificação:** a RF-008 original proibia dose sem calibração. Por decisão
> de produto, o HearPhant mostra dose `ESTIMATED` com faixa (seção 9.2 permite estimativas
> com incerteza explícita); a UI sempre rotula "estimativa" e mostra a faixa provável.

## Estado atual

| Componente | Caminho | Situação |
|---|---|---|
| Motor de dose (Pa²·h, janela de 168 h, cobertura) | `crates/hearphant-dose` | Pronto, com os testes T-DOS da seção 16.1. Ainda sem fonte acústica para alimentá-lo |
| Descoberta e nível do sinal (PipeWire) | `crates/hearphant-audio` | Saída padrão, volume, mudo, tipo, streams (eventos via `pw-dump`); RMS/pico do monitor via `parec`, em memória |
| Perfis de estimativa | `crates/hearphant-calibration` | Detecção automática do perfil; ver "Estimativa automática" abaixo |
| Histórico local | `crates/hearphant-storage` | SQLite em `~/.local/share/hearphant/`; só agregados por minuto, nunca áudio |
| Serviço D-Bus `org.hearphant.Service1` | `crates/hearphant-daemon` | Tempo de escuta em 7 dias, por dia e por dispositivo; tudo `UNVERIFIED` |
| CLI de diagnóstico | `crates/hearphant-cli` | `devices`, `status`, `reference` |
| Extensão GNOME 42 | `integrations/gnome-shell-extension` | Popover com telas principal, semana e configurações |
| Contrato D-Bus (v2) | `dbus/org.hearphant.Service1.xml` | Fonte única para daemon e extensão |
| Serviço simulado | `tools/mock-hearphant-service.js` | Dados fictícios para desenvolver a UI |

**Análise do nível do sinal** é opcional (Configurações na Ellie). No GNOME 42, qualquer
captura de áudio, inclusive do monitor da saída, mostra o ícone de gravação no painel; o
microfone não é usado e nada é gravado. Desligada, o tempo de escuta é aproximado pelos
aplicativos com áudio aberto e marcado como "aproximado". A captura só roda enquanto há
áudio tocando.

**Ainda não existe:** calibração medida (assistente com multímetro ou acoplador), proteção/redução
de volume, app GTK.

## Estimativa automática

`L_A ≈ RMS_A do sinal (dBFS) + 20·log10(ganho do volume) + K do perfil`, integrado em Pa²·h na
janela de 168 h e comparado com a referência adulta OMS/UIT (1,6 Pa²·h). A dose aparece como valor
central e faixa provável (`× 10^(±incerteza/10)`).

| Perfil | Detecção | K | Incerteza | Base |
|---|---|---|---|---|
| Moondrop Chu II DSP | nome USB `Chu2_DSP` | 120,2 dB | ±5 dB | 119 dB/Vrms (fabricante); cabo DSP −0,43 dBV em 0 dBFS, 3 Ω ([Reference Audio Analyzer](https://reference-audio-analyzer.pro/en/report/amp/moondrop-chu2-flat.php)) |
| Fone genérico | demais saídas (tipo fone ou desconhecido) | 113,0 dB | ±10 dB | Ponto de projeto da EN 50332: 150 mV / 75 mV→94 dB(A) ⇒ 100 dB(A) no máximo |
| Caixa de som | `port.type=speaker` ou escolha do usuário | — | — | Sem estimativa: depende da distância e da sala |

O tipo pode ser corrigido em Configurações → "Tipo desta saída". Para adicionar um dispositivo,
inclua um `Profile` em `crates/hearphant-calibration/src/lib.rs` com as fontes dos números.
Alertas: 80% e 100% da dose estimada, uma vez por subida (rearmam 10 pontos abaixo).
**Volume alto agora:** o nível ao vivo é o Leq de blocos de 3 s. Com ≥ 90 dB(A) por dois blocos
seguidos (zona vermelha: a referência semanal chegaria em ≤ 4 h), a Ellie fica vermelha, balança no
painel e envia uma notificação que não pode ser silenciada; volta ao normal abaixo de 85 dB(A).
Intervalo mínimo de 10 min entre avisos.



## Compilar e testar

Requer Rust (rustup), PipeWire e `pw-dump`.

```sh
cargo test
cargo build --release
./target/release/hearphant devices
```

## Executar o serviço

Manual (Ctrl+C encerra): `./target/release/hearphant-daemon`

Como serviço de usuário, iniciado no login:

```sh
install -Dm755 target/release/hearphant-daemon target/release/hearphant -t ~/.local/bin
install -Dm644 packaging/systemd/hearphant.service -t ~/.config/systemd/user
systemctl --user daemon-reload && systemctl --user enable --now hearphant.service
```

Logs: `journalctl --user -u hearphant -f`. Só um provedor de `org.hearphant.Service1`
roda por vez: pare o daemon antes de usar o serviço simulado.

## Verificar a ausência de dosimetria falsa

- Sem análise do sinal ou em caixa de som, `hearphant status` mostra `dose_percent: indisponível`.
- Com estimativa, o status é `ESTIMATED` e sempre traz `dose_low_percent`/`dose_high_percent`;
  a extensão descarta dose estimada sem faixa.
- `hearphant devices` rotula o volume como “valor digital do mixer, não é dBA”.
- `SetProtectionMode auto` é recusado: exige perfil calibrado.
- No motor, só `Measurement::Acoustic` carrega nível em dBA; `Digital` nunca gera dose
  (teste `t_dos_08_uncalibrated_has_no_dose`).
- A extensão descarta `dose_percent` vindo com fonte não calibrada (testes em
  `integrations/gnome-shell-extension/tests`).

## Limitações conhecidas

- Sem a análise do sinal, `playing` vem do estado dos streams: player pausado com stream
  aberto conta como tocando.
- O nível em dBFS é do conteúdo, antes do volume da saída (o monitor do PipeWire não
  aplica o volume). Não representa o que chega ao ouvido.
- Tipo do dispositivo vem do `port.type` da rota. DACs USB com porta `analog` (ex.: Chu2 DSP)
  ficam `unknown`, pois não há como saber o que está conectado.
- Saídas virtuais (EasyEffects etc.) não são resolvidas até o dispositivo físico.
- Volume por aplicativo e ganhos fora do Linux (amplificador, controle no fone) não são vistos.
- Testado só em Ubuntu 22.04, PipeWire 0.3.48, GNOME 42.9.
