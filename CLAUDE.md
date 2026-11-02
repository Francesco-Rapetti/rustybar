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
- `scripts/bundle.sh` → `target/RustyBar.app`, firmata con il certificato "RustyBar Local" del
  portachiavi (se manca, ad hoc).
- La barra vera è `/Applications/RustyBar.app`, avviata dal LaunchAgent
  `io.github.francesco-rapetti.rustybar` (log in `~/Library/Logs/rustybar.log`) con la config dei
  dotfiles (`~/.config/rustybar` → `~/Developer/dotfiles/rustybar`; server e account in
  `rustybar.local.toml`, fuori da git). Per metterci una build: `scripts/bundle.sh`, sostituire
  l'app in `/Applications`, poi `launchctl kickstart -k gui/$(id -u)/io.github.francesco-rapetti.rustybar`.
- Per una prova a parte: una config in un'altra cartella, con `[bar] offset = 40` (la barra va
  subito sotto quella vera), poi
  `open -n target/RustyBar.app --env RUSTYBAR_CONFIG_DIR=<cartella> --stdout <log> --stderr <log>`.
  Con un suo `RUSTYBAR_CONFIG_DIR` ha socket e cache suoi, quindi gira accanto alla barra vera senza
  toccarla; `RUSTYBAR_CONFIG_DIR=<cartella> rustybar msg …` parla con lei. Per uno schermo stretto
  basta un `margin` grande; per il notch, un gruppo `center` (stesso codice: ogni lato il suo spazio).
- `rustybar msg query` (widget, stato, posizione), `msg popup <widget>`, `msg reload`,
  `msg trigger <evento> k=v…`, `msg settings [pagina]`, `msg quit`; `rustybar check` controlla
  config e widget Lua; `rustybar settings [pagina]` apre la finestra delle impostazioni senza barra.
- `RUSTYBAR_DEBUG=1` registra nel log i clic, cosa c'è sotto (e se ha un'azione) e le azioni
  mandate; `RUSTYBAR_MOCK=examples/mock` usa i dati finti (tutti i widget tranne `clock`);
  `-AppleLanguages '(en)' -AppleLocale en_US` dopo il binario, o dopo `--args` con `open`, la fa in
  inglese.
- `scripts/screenshots.py`: gli screenshot del README in `docs/screenshots`, finestra per finestra
  (niente di quello che c'è dietro) su uno sfondo neutro, dalla barra coi dati finti in inglese e
  ogni widget nel layout, più la finestra delle impostazioni (`settings` da solo la rifà); il mouse
  sullo schermo principale. Prende solo `target/RustyBar.app`, mai la barra vera. Guardali tutti
  prima di usarli.
- `scripts/measure.py [PID] [SECONDI]`: memoria, CPU e processi avviati da RustyBar (e da SketchyBar,
  se gira), per confrontarle.

## Com'è fatta

- `app.rs`: stato dell'app sul main thread (config, widget, una barra per schermo, il popup). Ogni
  chiamata a un widget passa per `catch_unwind`: un widget che va in panico mostra un errore e la
  barra resta su.
- `runtime.rs`: ponte fra thread e main thread via GCD (`runtime::after`, `spawn`), comandi in
  background (`run`, `fire`, `shell`, `open`, `open_app`, `which`).
- `fit.rs`: tiene separati i lati sinistro e destro; prima va ciò di cui la barra può fare a meno
  (forma `Slim`, per i widget di `[displays] slim_first` e nel loro ordine: gli slider di `audio`,
  poi `claude` in una pill), poi cedono i widget più lontani dal loro bordo, prima compatti e poi
  nel popup di una pill `⋯`, con un margine prima di tornare grandi (lo usa `App::bar_tree`, che
  disegna una forma solo quando serve misurarla).
- `surface.rs`, `ui/` (`node.rs` albero dichiarativo, `layout.rs` flexbox, `render.rs` diff dei
  layer con animazioni implicite), `gfx.rs` (testo, SF Symbols e icone in bitmap, con cache a due
  generazioni ripulita dopo ogni render, e del tutto quando un popup si chiude, insieme alle pagine
  libere di malloc), `theme.rs`, `config.rs` (`rustybar.local.toml` unito sopra `rustybar.toml`),
  `lua.rs` + `prelude.lua`,
  `ipc.rs`, `cache.rs` (ultimo stato su disco per l'avvio), `service.rs` (LaunchAgent), `iokit.rs`,
  `i18n.rs` (`t("italiano", "english")`), `mock.rs`, `net.rs` (WebSocket e richieste HTTP con
  NSURLSession, HTTP/1.1 su socket Unix), `secret.rs` (portachiavi via `security`, e
  `rustybar secret set`), `time.rs` (ora locale, ISO 8601, durate come `2 h 57 min`).
- `settings/`: la finestra delle impostazioni dei dotfiles (dalla mela o `msg settings`), in un
  processo suo (`rustybar settings`, lanciato con Launch Services, uno solo per cartella di config:
  il secondo passa la pagina al primo su un socket) che finisce quando si chiude, perché AppKit
  lascia ~35 MB anche dopo la finestra. `store.rs` legge e scrive i TOML con `toml_edit` (commenti
  e ordine restano, valore di default = chiave tolta, RustyBar nel file che ha già la chiave),
  `form.rs` i controlli (checkbox e campi numerici: `NSSwitch` e `NSSlider` portano il processo
  oltre 120 MB su macOS 26; sfondi con un layer, non `drawRect`), poi una pagina per file
  (`rustybar.rs`, `aerospace.rs` con i bordi, `other.rs`). Dati finti: `<mock>/settings/`.
- `widgets/`: un file per widget, registrato in `widgets::create`. Il trait `Widget` ha `start`,
  `bar`, `popup`, `ready`, `action(name, value, cx)` ed `event` (`Wake`, `ScreensChanged`,
  `Trigger`, `PopupOpened`, `PopupClosed`). `Cx` dà `redraw`, `close_popup`, `after`, `every`,
  `background` (lavoro su un thread, risultato sul main thread) e `handle` (per i thread lunghi:
  `send` e `alive`, che diventa falso al reload). `Rx` dà `pill`, `popup`, `section`, `detail`,
  `row_button`, `action`, `toggle_popup`, `compact` (la forma intera non ci sta, vedi `fit.rs`) e
  `slim` (senza ciò di cui può fare a meno, per i widget di `slim_first`; vero anche da compatto).
  Le opzioni del TOML arrivano come `Options` (`str`, `f64`, `bool`, `strings`).

### Regole del codice

- Niente polling quando macOS ha un evento (CoreAudio, SCDynamicStore, EventKit, IOKit,
  NSWorkspace…) e niente processi lanciati a regime: si legge in-process. Ciò che non ha eventi va
  su un timer in-process e ridisegna solo se il valore cambia. Un processo esterno solo dove non c'è
  alternativa, e documentato nel README (oggi: `brew` ogni ora, `system_profiler` con Bluetooth
  attivo, `curl` all'apertura del popup di rete, `claude` ogni 5 minuti, `security` una volta per
  segreto, la finestra delle impostazioni finché è aperta, `borders` quando se ne cambia un'opzione).
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
- La firma ad hoc fa richiedere Calendari a ogni build: per questo c'è il certificato
  "RustyBar Local" nel portachiavi, fatto con `scripts/signing-cert.sh` e l'OK dell'utente. Non
  rifarlo né toglierlo senza chiedere.
- Un clic altrove chiude il popup (monitor globale): se un `msg popup` sembra non aprirsi, è
  probabilmente quello.
- SketchyBar è spenta, e la sua config resta nei dotfiles come riserva: non toccarla e non
  riaccenderla senza l'utente.

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
  di `home` sui dispositivi veri e i pill dei singoli account di `claude`. Screenshot del README:
  dati finti anche per `apple`, `aerospace`, `front_app`, `system` e `battery`. Passaggio fatto il
  31/10/2026: RustyBar è la barra, dal LaunchAgent, con la config dei dotfiles e i binding di
  AeroSpace; SketchyBar spenta. Misurata dal vero con tutti i widget: ~20 MB all'avvio, ~25 MB dopo
  aver aperto ogni popup (prima di ripulire alla chiusura erano 31 MB e crescevano), ~0,2% CPU,
  0 processi in 60 s.
- Lati che non si sovrappongono mai (`fit.rs`), anche col notch: forme compatte e pill `⋯` con i
  widget che non ci stanno; forme compatte nuove per `aerospace` (icone solo nel workspace attivo),
  `front_app` e `claude` (una pill); `compact_on_notch` ora è `false`. Prima di tutto cedono gli
  slider di `audio`, poi `claude` va in una pill (forma `Slim`, `slim_first`; `audio` è senza
  slider anche da compatto, `claude` ha una pill per account su ogni schermo dove ci stanno).
  Provati con la barra di prova stretta e col gruppo centrale; coi dati finti in uno stato
  stretto: ~20 MB, ~0% CPU, 0 processi.
- Finestra delle impostazioni (dalla mela): tutte le pagine provate coi dati finti sulla barra di
  prova (scrittura, rinomina sul posto, ⋯ sposta/elimina, nuova istanza di widget, Brewfile, tema
  chiaro, voce della mela, fuoco e ritorno all'app di prima). Barra a riposo ~16 MB, ~0% CPU,
  0 processi; la finestra ~55–60 MB nel suo processo, la barra resta a 16 MB anche dopo.
  System Events sbaglia `frontmost` (dice sempre Claude) e con due processi `rbtest` confonde le
  finestre: per le prove `NSWorkspace` e AX per pid. Da provare dal vero con l'utente: ricarica di
  AeroSpace, `borders` aggiornato al volo, riavvio di MiddleClick, token di Home Assistant.
- Temi: 24 palette in `theme::PRESETS` (Catppuccin ×4, Dracula, Everforest, Gruvbox, Kanagawa,
  Nord, One, Rosé Pine, Solarized, Tokyo Night), prese dai repository dei temi e messe sui 26 nomi
  di Catppuccin (colori mancanti ripetuti, grigi mancanti mescolati; un test controlla l'ordine dal
  testo allo sfondo e il contrasto). Provate tutte coi dati finti sulla barra di prova (barra e
  popup di `clock`, `audio`, `system`); un nome sbagliato va nel log e `rustybar check` lo rifiuta.
- Ancora da provare sul Mac vero, con l'utente: connessione e disconnessione Bluetooth, join di una
  Wi-Fi con password, slider del volume, cambio del dispositivo predefinito, "Aggiorna tutto" di
  Homebrew e riavvio di AeroSpace. I badge del Dock aspettano che l'utente dia Accessibilità a
  RustyBar.app: macOS la chiede da sé una volta sola, poi la chiedono il `!` giallo di
  `notifications` e la riga nel suo popup (provati sulla barra di prova, il clic no).
  Della M3: la riconnessione di `home` al risveglio, login e logout di `claude`. Di `fit.rs`: uno
  schermo col notch vero e un clic dentro il popup della `⋯`.

## M3

Per ogni widget, prima di scrivere codice, leggi nei dotfiles plugin, helper, sezione del README e
screenshot e scrivi la specifica del comportamento (testi, colori, stati, azioni): RustyBar deve fare
le stesse cose. Fatti 1–5; resta 6, con l'utente.

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
