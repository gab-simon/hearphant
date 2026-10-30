# HearPhant — extensão GNOME Shell (Ellie)

Indicador da Ellie no painel com popover de três telas, inspirado no mockup do projeto:
principal (anel, tempo de escuta ou dose, volume, dispositivo, sinal ao vivo, proteção),
semana (barras por dia, média, sinal médio, divisão por dispositivo) e configurações. **Só apresenta** o que o serviço `org.hearphant.Service1`
publica via D-Bus; não calcula dose (seção 10.3 da documentação).

- Alvo atual: **GNOME Shell 42** (Ubuntu 22.04). A API de Quick Settings só existe no
  GNOME 43+, por isso aqui é um `PanelMenu.Button`. GNOME 43/44 e 45+ (módulos ES) ainda não suportados.
- Contrato D-Bus: [`dbus/org.hearphant.Service1.xml`](../../dbus/org.hearphant.Service1.xml).

## Salvaguardas na UI

- Sem `measurement_status` `CALIBRATED`/`ESTIMATED`, qualquer `dose_percent` recebido é descartado
  e o anel fica tracejado ("desconhecido"), nunca 0%.
- `NORMAL`/`NOTICE`/`WARNING`/`EXCEEDED` sem dose viram `UNVERIFIED` (verde nunca sem medição).
- Serviço ausente → `UNAVAILABLE`, ícone esmaecido.
- Volume sempre rotulado “valor digital, não é dBA”; escopo/cobertura sempre declarados.
- Redução de volume só é anunciada como feita quando `verified = true`.
- Dados de demonstração exibem o selo **DEMONSTRAÇÃO** e aparecem no nome acessível.
- Animações (inclinação no hover, transição do anel) respeitam “Animações” do sistema e a chave da extensão.
- O medidor de sinal só consulta o serviço (4×/s) enquanto o popover está aberto na tela principal.
- Notificações informativas podem ser desligadas; falhas de proteção e excessos sempre aparecem.
- Margens negativas no CSS quebram a alocação no St (estouro de altura): não usar.

## Uso

```sh
make test       # testes da lógica de apresentação (gjs, fora do Shell)
make install    # empacota e instala em ~/.local/share/gnome-shell/extensions
gnome-extensions enable hearphant@hearphant.org
# X11: Alt+F2, digite r, Enter. Wayland: sair e entrar na sessão.
```

Sem o daemon, use o serviço **simulado** (dados fictícios, `is_demo=true`):

```sh
gjs ../../tools/mock-hearphant-service.js cycle   # alterna todos os estados a cada 5 s
gjs ../../tools/mock-hearphant-service.js exceeded
# cenários: unverified idle normal notice estimated warning exceeded protecting protection-failed error
```

Logs da extensão: `journalctl -f -o cat /usr/bin/gnome-shell | grep -i hearphant`.

## Pendente

- Métodos `GetDeviceProfile`, `SetVolumeCap`, `GetHistory` no contrato.
- Variantes da Ellie por estado e versão detalhada (96–256 px); hoje é um único SVG simbólico.
- Visões "Dia" e "Mês" na tela da semana.
- Integração Quick Settings para GNOME 43+.
