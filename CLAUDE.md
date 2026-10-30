# RustyBar

Barra di stato per macOS in Rust: un solo processo, dati letti in-process e a eventi, Core Animation
per le animazioni, popup costruiti con componenti (anche da Lua). Nasce per sostituire la barra
SketchyBar dei dotfiles (`~/Developer/dotfiles/sketchybar`) con le stesse funzioni, molto più leggera.
Il [README](README.md) è la documentazione per chi la usa; qui ci sono le regole per lavorarci.

## Comandi

Rust viene da Homebrew ed è keg-only: ogni comando `cargo`, anche dentro `scripts/bundle.sh`, vuole
prima `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.

- `cargo build --release`, `cargo test --release`, `cargo clippy --release`. Clippy ha tre avvisi
  vecchi (`ui/layout.rs`, `widgets/aerospace.rs`, `widgets/system.rs`): niente di nuovo oltre a quelli.
- `scripts/bundle.sh` → `target/RustyBar.app`, con firma ad hoc.
- Per provarla accanto a SketchyBar: una config di prova in una cartella a parte, con `[bar] offset = 40`
  (la barra va subito sotto SketchyBar), poi
  `open -n target/RustyBar.app --env RUSTYBAR_CONFIG_DIR=<cartella> --stdout <log> --stderr <log>`.
- `rustybar msg query` (widget, stato, posizione), `msg popup <widget>`, `msg reload`,
  `msg trigger <evento> k=v…`, `msg quit`; `rustybar check` controlla config e widget Lua.
- `RUSTYBAR_DEBUG=1` registra i clic nel log; `RUSTYBAR_MOCK=examples/mock` usa i dati finti.
- `scripts/measure.py [PID] [SECONDI]`: memoria, CPU e processi avviati da RustyBar (e da SketchyBar,
  se gira), per confrontarle.

## Com'è fatta

- `app.rs`: stato dell'app sul main thread (config, widget, una barra per schermo, il popup). Ogni
  chiamata a un widget passa per `catch_unwind`: un widget che va in panico mostra un errore e la
  barra resta su.
- `runtime.rs`: ponte fra thread e main thread via GCD (`runtime::after`, `spawn`), comandi in
  background (`run`, `fire`, `shell`, `open`, `open_app`, `which`).
- `surface.rs`, `ui/` (`node.rs` albero dichiarativo, `layout.rs` flexbox, `render.rs` diff dei
  layer con animazioni implicite), `gfx.rs` (testo, SF Symbols e icone in bitmap, con cache a due
  generazioni ripulita dopo ogni render), `theme.rs`, `config.rs`, `lua.rs` + `prelude.lua`,
  `ipc.rs`, `cache.rs` (ultimo stato su disco per l'avvio), `service.rs` (LaunchAgent), `iokit.rs`,
  `i18n.rs` (`t("italiano", "english")`), `mock.rs`, `net.rs` (WebSocket e richieste HTTP con
  NSURLSession, HTTP/1.1 su socket Unix), `secret.rs` (portachiavi via `security`, e
  `rustybar secret set`), `time.rs` (ora locale, ISO 8601, durate come `2 h 57 min`).
- `widgets/`: un file per widget, registrato in `widgets::create`. Il trait `Widget` ha `start`,
  `bar`, `popup`, `ready`, `action(name, value, cx)` ed `event` (`Wake`, `ScreensChanged`,
  `Trigger`, `PopupOpened`, `PopupClosed`). `Cx` dà `redraw`, `close_popup`, `after`, `every`,
  `background` (lavoro su un thread, risultato sul main thread) e `handle` (per i thread lunghi:
  `send` e `alive`, che diventa falso al reload). `Rx` dà `pill`, `popup`, `section`, `detail`,
  `row_button`, `action`, `toggle_popup` e `compact` (schermi col notch). Le opzioni del TOML
  arrivano come `Options` (`str`, `f64`, `bool`, `strings`).

### Regole del codice

- Niente polling quando macOS ha un evento (CoreAudio, SCDynamicStore, EventKit, IOKit,
  NSWorkspace…) e niente processi lanciati a regime: si legge in-process. Ciò che non ha eventi va
  su un timer in-process e ridisegna solo se il valore cambia. Un processo esterno solo dove non c'è
  alternativa, e documentato nel README (oggi: `brew` ogni ora, `system_profiler` con Bluetooth
  attivo, `curl` all'apertura del popup di rete, `claude` ogni 5 minuti, `security` una volta per
  segreto).
- AppKit e i layer solo sul main thread; il lavoro bloccante in `cx.background` o in un thread con
  `handle.alive()`.
- Ogni widget che legge dati personali ha i dati finti: `crate::mock::load::<T>("<widget>")`, un file
  in `examples/mock/` e azioni che con i dati finti non fanno niente.
- Commenti, README e messaggi di commit in inglese; il commento spiega cosa fa e perché, come nel
  resto del codice. Testi della barra in italiano e inglese con `i18n::t`.
- Dopo un cambiamento che può pesare, misura con `scripts/measure.py`. Obiettivi: < 30 MB,
  ~0% CPU a riposo, 0 processi a regime, primo frame < 100 ms (lo scrive il log).

## Regole di lavoro

- Commit e push solo quando lo chiede l'utente. Commit in stile conventional, in inglese, con un
  corpo che spiega (vedi `git log`).
- **Mai dati veri** in screenshot, mock, test o documentazione: niente nomi, email, dispositivi,
  reti o eventi reali. Gli screenshot del README si fanno con `RUSTYBAR_MOCK`.
- **Non cambiare lo stato reale del Mac** nelle prove senza chiedere: niente join Wi-Fi, connessioni
  Bluetooth, volume, dispositivo audio predefinito, `brew upgrade`, riavvio di AeroSpace.
- **Permessi (TCC)**: macOS li dà al processo responsabile. Il binario lanciato dal terminale li
  chiederebbe a nome del terminale o di Claude: le funzioni con permessi si provano solo sul bundle
  aperto con `open`. Con i dati finti non si chiede nessun permesso.
- La firma ad hoc fa richiedere Calendari a ogni build. `scripts/signing-cert.sh` crea un
  certificato di firma nel portachiavi: **solo con l'OK dell'utente**.
- Un clic altrove chiude il popup (monitor globale): se un `msg popup` sembra non aprirsi, è
  probabilmente quello.
- SketchyBar resta la barra principale finché RustyBar non fa tutto: non toccare la sua config nei
  dotfiles fuori dal passaggio della M3, e quello insieme all'utente.

## Stato

- **M1**: motore, `apple`, `aerospace`, `front_app`, `clock` con calendario scorrevole, `battery`,
  `system`.
- **M2**: `audio` + Bluetooth, `network` (Wi-Fi, VPN, reti vicine), `events` (calendari),
  `notifications` (badge del Dock, crash, Homebrew); dati finti in `examples/mock`. Misurata con
  tutti i widget: ~18 MB, ~0,1% CPU, 1 processo (SketchyBar: ~176 MB, 15 processi, ~1,3%).
- **M3** (in corso): `home` (Home Assistant), `claude` e `docker`, con specifica nel doc del modulo
  e dati finti; `rustybar secret set home-assistant`, `rb.home` in Lua, immagini colorate
  (`tint`) e SF Symbols monocromatici colorati dopo (i buchi dei simboli `.fill` restano).
  Misurata con i dati finti: ~18 MB, ~0,2% CPU, 0 processi in 60 s; coi dati veri (home, claude,
  docker): ~19 MB, ~0% CPU, 0 processi. Provati dal vero, uguali a SketchyBar: `home` (connessione,
  stati in diretta, token importato, `rb.home`, `POST` dei servizi), `claude` (tre account, popup),
  `docker` (avvio, `/events`, stats, si nasconde quando Docker si ferma); con l'utente i riquadri
  di `home` sui dispositivi veri e i pill dei singoli account di `claude`.
- Ancora da provare sul Mac vero, con l'utente: connessione e disconnessione Bluetooth, join di una
  Wi-Fi con password, slider del volume, cambio del dispositivo predefinito, "Aggiorna tutto" di
  Homebrew e riavvio di AeroSpace. I badge del Dock aspettano che RustyBar.app abbia Accessibilità.
  Della M3: la riconnessione di `home` al risveglio, login e logout di `claude`.

## M3

Per ogni widget, prima di scrivere codice, leggi nei dotfiles plugin, helper, sezione del README e
screenshot e scrivi la specifica del comportamento (testi, colori, stati, azioni): RustyBar deve fare
le stesse cose. Fatti 1–3 (da provare dal vero, vedi Stato); restano 4–6.

1. **Home Assistant** (`plugins/home.sh`, `helpers/home_assistant.swift`,
   `helpers/home_assistant_icons.js`): popup a plancia con riquadri per luci e prese (luminosità),
   clima (+/- e modalità), tende, aspirapolvere, stampante 3D (avanzamento), scene come chip;
   disposizione dal TOML o da Lua. WebSocket `subscribe_entities` su un thread, ping ogni 30 s,
   riconnessione dopo 1 s e poi 10 s, e al risveglio; azioni con REST `POST /api/services/…`. Token
   nel portachiavi (`rustybar-home-assistant`), impostato con `rustybar secret set home-assistant`
   (da stdin, comando da aggiungere), importato una volta da `sketchybar-home-assistant`.
2. **Claude** (`plugins/claude.sh`, `helpers/claude_icon.js`): uso del piano di ogni account
   (`claude auth status --json` e `claude -p … get_usage` in stream-json, uno per
   `CLAUDE_CONFIG_DIR`), ogni 5 minuti, al risveglio e al clic, cache di 60 s; azioni
   `claude auth login`/`logout`.
3. **Docker** (`plugins/docker.sh`, `helpers/docker_icons.js`): Engine API sul socket Unix
   (`/containers/json`, `/info`, `/containers/<id>/stats`, `/events` in streaming), niente CLI; clic
   → Docker Desktop.
4. **Screenshot e README**: uno per widget e per popup, con `RUSTYBAR_MOCK`, guardati prima di usarli.
5. **Passaggio dei dotfiles**, con l'utente: config in `~/Developer/dotfiles/rustybar`, avvio da
   `after-startup-command` di AeroSpace o `rustybar service install`, i binding che spostano finestre
   mandano `rustybar msg trigger aerospace_windows`, poi SketchyBar si spegne.
6. **Release**: binario universal arm64 + x86_64, tap Homebrew.
