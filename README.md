# taskbar-bsk

App leve em Rust para Windows 11 que mostra a bateria de mouses, teclados, headsets, fones, controles e celular (Phone Link) sem fio direto na barra de tarefas, ao lado da área de notificação.

- **Leve:** um único executável portátil, sem instalador e sem runtime. Fica ocioso entre as leituras e devolve a memória ao Windows depois de cada uma.
- **Sem software do fabricante:** conversa direto com os dispositivos. G HUB, Synapse, iCUE e similares podem ficar fechados.
- **Privado:** nenhuma requisição de rede, telemetria ou conta.

## Instalação e uso

1. Baixe o `taskbar-bsk.exe` da [última release](https://github.com/bsk-iT/taskbar-bsk/releases/latest).
2. Coloque-o em uma pasta permanente e execute. Não precisa de instalador nem de administrador.
3. Clique com o botão direito no widget ou em um ícone da bandeja para abrir o menu.

O executável não é assinado. O SmartScreen pode avisar: clique em **Mais informações → Executar assim mesmo**, ou compile você mesmo. A versão release ativa **Iniciar com o Windows** por padrão; desative no menu se preferir.

### Leitura do widget

| O que aparece | Significado |
|---|---|
| Texto normal | Nível da bateria |
| Texto vermelho | 15% ou menos |
| ⚡ azul | Carregando |
| Texto esmaecido | Dispositivo dormindo; mostra o último valor |

### Menu

- Cada dispositivo tem um submenu para mostrar/ocultar e mover para a esquerda/direita. Ordem e visibilidade ficam salvas.
- **Mostrar na barra de tarefas** ou **Mostrar como ícones na bandeja** (um ícone com o número por dispositivo). Se a barra estiver na vertical ou o widget não puder ser criado, o app usa a bandeja automaticamente.
- **Ocultar dispositivos dormindo / último valor**.
- **Fundo atrás do texto**, para temas de barra de tarefas transparente.
- **Atualizar agora**, **Alertas de bateria fraca** (notificação em 15%, sem repetir enquanto o nível oscila), **Iniciar com o Windows**, **Copiar diagnóstico** e **Sair**.

No modo bandeja, o Windows 11 pode esconder os ícones no `^`. Arraste-os para a barra ou ative em **Configurações → Personalização → Barra de tarefas → Outros ícones da bandeja do sistema**.

## Dispositivos suportados

A lista resume os protocolos implementados. Nem todo modelo foi testado em hardware; firmware, receptor e modo de conexão fazem diferença.

| Marca / família | Modelos e conexões |
|---|---|
| Logitech / ASTRO | Mouses e teclados em receptores Unifying, Bolt, Lightspeed e Nano via HID++; headsets G; G435 no dongle novo; bases A50 X / A50 Gen 5. |
| Razer | Mouses e teclados sem fio; BlackShark V2/V3; Barracuda X (2022) no receptor. Nem todo Kraken ou Barracuda. |
| SteelSeries | Arctis / Arctis Nova sem fio, GameBuds; Aerox, Prime Wireless, Rival sem fio. |
| Corsair | Void, HS70/HS80; Bragi/Slipstream (Virtuoso, HS80 MAX, Void V2, mouses e teclados). |
| HyperX | Cloud II/III e variantes, Alpha/Alpha 2, Flight/Flight S, Stinger Core; Pulsefire Haste 2 sem fio. |
| ASUS ROG | Scope II, Azoth, Falchion; Keris, Harpe, Gladius, Chakram, Spatha, Pugio, Impact; Omni Receiver e Pelta. |
| Cooler Master | MM712 / MM731 no receptor ou cabo. |
| Glorious / Hitscan | Model O/D sem fio e PRO; Hyperlight / Hyperlight X / N. |
| Kysona / Pulsar / ATK-VXE / G-Wolves | M600 / Mercury; Pulsar incluindo X2 V3/eS; VXE R1/X3, ATK F1 e NK NearLink; Fenrir, HTX/HTS/HSK/HTR, Lycan, WARG, VUK. Headsets ATK N9 Pro/Ultra via porta COM virtual. |
| Keychron | Mouses série M 1K/4K/8K, TurboLink e Nape Pro. Teclados geralmente só informam bateria via Bluetooth. |
| LAMZU / Endgame Gear | Maya, Tachi, Inca, Paro, Thorn, Atlantis, Orcus; OP1w/XM2w 4K v1/v2. |
| WLMouse / VAXEE / Finalmouse | YING e Beast/Sword 8K (não o Beast X 4K); VAXEE 4K/8K e NP-01S V2 no cabo; UltralightX / Starlight X. |
| HECATE / Redragon / Taidu / Areson | G5M PRO; M916/M913/G49 e TSG808; receptores Areson M915/M913. |
| Ninjutso / Sprime | Sora V2, Ten / Ten Air; PM1 no receptor ou cabo. |
| AULA / AJAZZ / MCHOSE (teclados) | F75/F87 Pro/F99, HERO 68 Air; AK820 MAX / AK980 V2 / F108 Pro; K99 V2/G75 Pro/X75. |
| DAREU / ZOWIE / Royal Kludge | EM901X; EC-CW, DW e U2 sem fio; MG6 Pro no receptor ou cabo. |
| Machenike / MCHOSE / MSI | L7 Arctic/Mix; famílias L7/M7/A7/K7, G3 V2 e variantes novas; VERSA 300 ELITE. |
| Rapoo / Darmoshark | VT com status Nordic 54L, VT3 Pro / VT1 Pro Max; Darmoshark M3. Modelos de escritório MT/M não são cobertos. |
| Motospeed / Delux / Ajazz | X6, M900 Pro / M800 Pro, AJ179/AJ159/AJ139 V2. |
| Incott / Wraith / Fantech | G23 V2/G24; W1/Freestyle; Helios Go XD5. |
| Attack Shark / Rongyuan / Xenta | Mouses e teclados com protocolo compartilhado e receptores Xenta (X6/R1/X11/X3 Max/V3 Pro/V6). |
| Sony | DualShock 4, DualSense/Edge; INZONE H9 II/H5 e Buds. |
| Jabra / Fractal Design / MOONDROP / JBL | Headsets em Link 380/390; Scape; GM01 Pro; Quantum 520/650/650X/850/950/950X. |
| GameSir / Flydigi / 8BitDo / Xbox | Nova, G7 Pro (GIP) e Cyclone 2; Vader 5 Pro; Ultimate 2 em modo D-input (firmware 1.03+); controles Xbox e headsets conectados a eles. |
| Samsung / Apple / Beats | Galaxy Buds via Bluetooth; AirPods e Beats via anúncios BLE. Alguns apps do fabricante precisam estar fechados. |
| Windows / genérico | Bateria informada pelo Bluetooth do Windows; HID Battery Strength padrão; celular Android via Phone Link; headsets do [HeadsetControl](https://github.com/Sapd/HeadsetControl) (opcional). |

Para os headsets extras do HeadsetControl, coloque o `headsetcontrol.exe` na mesma pasta do app ou no `PATH`.

## Linha de comando

```text
taskbar-bsk.exe --list    Lista os dispositivos (nome, status, conexão, tensão e id) e sai.
taskbar-bsk.exe --dump    Lista as coleções HID e salva em %TEMP%\taskbar-bsk-dump.txt.
taskbar-bsk.exe --quit    Fecha a instância em execução.
```

O `--dump` ajuda a pedir suporte para um dispositivo novo. Revise antes de compartilhar: nomes e ids de hardware podem identificar o seu equipamento.

O diagnóstico fica em `%LOCALAPPDATA%\taskbar-bsk\diagnostics.log` (use **Copiar diagnóstico** no menu).

## Ignorar um dispositivo

Crie um valor `REG_SZ` chamado `IgnoreDevices` em `HKCU\Software\taskbar-bsk` com pares `VID:PID` em hexadecimal separados por espaço, vírgula ou ponto e vírgula, por exemplo `28BD:0947, 046D:C547`. O app nunca abre esses dispositivos.

## Compilar

Requisitos: Rust (toolchain MSVC) e Visual Studio Build Tools com o Windows SDK.

```sh
cargo build --release
```

O executável fica em `target/release/taskbar-bsk.exe`.

## Licença

[GPL-3.0-or-later](LICENSE). Avisos de terceiros em [NOTICE](NOTICE).
