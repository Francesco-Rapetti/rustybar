# RustyBar

Barra di stato per macOS in Rust: un solo processo, dati letti in-process e a eventi, Core Animation
per le animazioni, popup costruiti con componenti (anche da Lua). Nasce per sostituire la barra
SketchyBar dei dotfiles (`~/Developer/dotfiles/sketchybar`) con le stesse funzioni, molto più leggera.
Il [README](README.md) è la documentazione per chi la usa; qui ci sono le regole per lavorarci.

## Comandi

Rust viene da Homebrew ed è keg-only: ogni comando `cargo`, anche dentro `scripts/bundle.sh`, vuole
prima `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.

- `cargo build --release`, `cargo test --release`, `cargo clippy --release`. Clippy ha due avvisi
  vecchi (`ui/layout.rs`, `widgets/aerospace.rs`): niente di nuovo oltre a quelli.
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
- `rustybar msg query` (widget, stato, posizione, peek, isola), `msg popup <widget>`, `msg peek <widget>`
  (la pill cresce come sotto il mouse; resta finché il mouse non passa sulla barra),
  `msg notice <widget>` (l'isola cresce da sola per 3 s, come a un cambio di traccia), `msg reload`,
  `msg trigger <evento> k=v…`, `msg settings [pagina]`, `msg quit`; `rustybar check` controlla
  config, widget Lua e nomi del layout; `rustybar settings [pagina]` apre la finestra delle
  impostazioni senza barra.
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
  `system` con la sola memoria, poi `claude` in una pill), poi cedono i widget più lontani dal loro
  bordo, prima compatti e poi nel popup di una pill `⋯`, con un margine prima di tornare grandi.
  L'ordine dice cosa conta meno: quando un passo serve comunque, quelli prima tornano indietro se
  lo spazio basta, dal più importante (`Given`: livello e passi risparmiati) (lo usa
  `App::bar_tree`, che disegna una forma solo quando serve misurarla).
- `surface.rs`, `ui/` (`node.rs` albero dichiarativo, `layout.rs` flexbox, `render.rs` diff dei
  layer con animazioni implicite), `gfx.rs` (testo, SF Symbols e icone in bitmap, più i simboli
  che SF Symbols non ha, disegnati lì su pixel interi: `magictrackpad.fill`, `magickeyboard.fill`;
  con cache a due
  generazioni ripulita dopo ogni render, e del tutto quando un popup si chiude, insieme alle pagine
  libere di malloc), `theme.rs`, `palette.rs` (i colori di un'immagine in Oklab e un tema fatto
  con quelli), `config.rs` (`rustybar.local.toml` unito sopra `rustybar.toml`),
  `lua.rs` + `prelude.lua`,
  `ipc.rs`, `cache.rs` (ultimo stato su disco per l'avvio), `fnkeys.rs` (modo dei tasti F:
  `HIDFKeyMode` coi parametri di IOHIDSystem, preferenza e notifica di Impostazioni Tastiera),
  `hotkey.rs` (scorciatoie globali con gli hot key di Carbon, senza permessi), `service.rs` (LaunchAgent), `iokit.rs`,
  `input.rs` (il campo di testo dei popup: un `NSTextField` in un pannello figlio che prende la
  tastiera senza attivare l'app), `i18n.rs` (`t("italiano", "english")`), `mock.rs`, `net.rs` (WebSocket e richieste HTTP con
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
- `widgets/`: un file per widget, registrato in `widgets::create` (più `hid.rs`, i protocolli HID
  dei ricevitori per `devices`, `call.rs` e `meet.rs`, le chiamate per `now_playing`, con
  l'estensione del browser in `extension/`, e `wa.rs`, il database e l'invio per `whatsapp`). Il trait `Widget` ha `start`,
  `bar`, `popup`, `peek`, `notice`, `ready`, `action(name, value, cx)`, `input(name, text, submit, cx)` (il testo di un
  `Kind::Input`) ed `event` (`Wake`,
  `ScreensChanged`, `Trigger`, `PopupOpened`, `PopupClosed`, `PeekOpened`, `PeekClosed`). `Cx` dà
  `redraw`, `close_popup`, `notice`, `after`, `every`,
  `background` (lavoro su un thread, risultato sul main thread) e `handle` (per i thread lunghi:
  `send` e `alive`, che diventa falso al reload). `Rx` dà `pill`, `popup`, `section`, `detail`,
  `row_button`, `action`, `toggle_popup`, `compact` (la forma intera non ci sta, vedi `fit.rs`),
  `slim` (senza ciò di cui può fare a meno, per i widget di `slim_first`; vero anche da compatto) e
  `island` (il primo del gruppo `center` su uno schermo col notch: `Rx::island(left, right)` lo
  disegna attorno al notch, e peek, popup e notice crescono dall'isola).
  Le opzioni del TOML arrivano come `Options` (`str`, `f64`, `bool`, `strings`).

### Regole del codice

- Niente polling quando macOS ha un evento (CoreAudio, SCDynamicStore, EventKit, IOKit,
  NSWorkspace…) e niente processi lanciati a regime: si legge in-process. Ciò che non ha eventi va
  su un timer in-process e ridisegna solo se il valore cambia. Un processo esterno solo dove non c'è
  alternativa, e documentato nel README (oggi: `brew` ogni ora, `system_profiler` con Bluetooth
  attivo, `curl` a ipinfo.io all'apertura dei dettagli del popup di rete, `claude` ogni 5 minuti,
  `security` una volta per segreto, la finestra delle impostazioni finché è aperta, `borders` quando
  se ne cambia un'opzione; sempre acceso solo `perl` con `other_apps` di `now_playing`, spento di
  default).
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
  La pagina Disposizione non fa scrivere nomi: anteprima della barra, per lato un elenco con
  icona, nome del TOML, ↑ ↓ e ⋯ (opzioni, sposta di lato, togli), `Aggiungi un widget` con quelli
  che esistono e non sono già nella barra; un nome sconosciuto in rosso con `Usa <nome>` per un
  errore di battitura (`widgets::closest`, lo stesso di `rustybar check` e del log). Provata coi
  dati finti via AX per pid (AXPress, senza mouse): ogni controllo scrive il file giusto.
  System Events sbaglia `frontmost` (dice sempre Claude) e con due processi `rbtest` confonde le
  finestre: per le prove `NSWorkspace` e AX per pid. Da provare dal vero con l'utente: ricarica di
  AeroSpace, `borders` aggiornato al volo, riavvio di MiddleClick, token di Home Assistant.
- Temi: 24 palette in `theme::PRESETS` (Catppuccin ×4, Dracula, Everforest, Gruvbox, Kanagawa,
  Nord, One, Rosé Pine, Solarized, Tokyo Night), prese dai repository dei temi e messe sui 26 nomi
  di Catppuccin (colori mancanti ripetuti, grigi mancanti mescolati; un test controlla l'ordine dal
  testo allo sfondo e il contrasto). Provate tutte coi dati finti sulla barra di prova (barra e
  popup di `clock`, `audio`, `system`); un nome sbagliato va nel log e `rustybar check` lo rifiuta.
  Predefinito `macos`: colori di sistema (chiaro/scuro e accento seguiti con KVO su
  `effectiveAppearance` e `NSSystemColorsDidChangeNotification`), SF Pro, pill a capsula su
  Liquid Glass (`NSGlassEffectView` sotto la `SurfaceView`, uno per pill; `pill_glass` `clear` con
  un velo di `base` a 0,3, o `regular`), popup di vetro `regular`; sotto macOS 26 una sfocatura. Il
  vetro è disegnato da SwiftUI, che con l'`animator` di AppKit arriva un frame dopo: posizione e
  dissolvenze vanno con animazioni CA sul layer della vista, nella transazione dei layer, e solo la
  dimensione con l'`animator`. Provato coi dati finti su sfondi colorati, chiaro (con
  `-NSRequiresAquaSystemAppearance YES`) e scuro, e a metà animazione: ~24 MB, ~0,1% CPU,
  0 processi.
  Vetro per ogni tema: `glass` (non impostata: solo con `macos`; nelle impostazioni un
  interruttore sotto la palette, che ricarica la pagina). Su una palette il vetro prende chiaro o
  scuro dalla palette (`NSAppearance` del pannello, `Theme::glass_dark`), con un velo di `base` a
  0,3 sulle pill e a 0,5 sui popup. Il vetro già disegnato non segue un cambio di stile o di
  aspetto: `Window` lo rifà da capo. Provato coi dati finti sulla barra di prova, davanti a una
  finestra colorata finta (bianco, nero, righe), con Mac scuro: tutte le palette, `clear` e
  `regular`, popup di `audio` e `system`, cambio di tema al volo, interruttore via AX:
  ~18 MB, ~0,1% CPU, 0 processi. Sulla barra vera, Mocha col vetro dal 4/11/2026.
  Costo del vetro (anche di `macos`): la misura animata con l'`animator` fa rifare a ogni frame
  il layout della finestra e il vetro a SwiftUI, ~35 ms per animazione (le viste del vetro sono
  `CASDFLayer` privati di SwiftUI: niente animazione CA della misura). Coi dati veri le pill
  cambiano di 1–2 pt ogni 2–4 s (`system`, cifre proporzionali; `clock` ogni minuto): fino a
  `SNAP` (4 pt) la misura del vetro cambia subito, sopra resta animata; `setCornerRadius` solo se
  cambia. Prova con una pill Lua che cambia ogni secondo, 12 pill: 2 pt da ~4,75% a 0,5% CPU
  (senza vetro 0,13%); 16 pt ancora ~3,5–4,75%. Barra vera: da ~0,7% a ~0,2% CPU, ~21 MB,
  0 processi.
- `docker` a plancia: tutti i container (anche fermi, `?all=1`) per progetto Compose, con ▶ ■ ↻ e
  cestino (secondo clic) per container e ▶ ■ ↻ per progetto, le porte come link a localhost; lo
  spazio su disco (`/system/df`, all'apertura e dopo ogni azione) con la pulizia a due clic di
  immagini (`-a`), container, volumi anonimi e cache di build, più reti e `system prune -a`.
  Azioni con `net::unix_request` (POST/DELETE) in `cx.background`, poi rilettura subito. Provato
  coi dati finti (stati, conferme, note): ~16 MB, ~0% CPU, 0 processi. Da provare con l'utente sul
  Docker vero: le azioni e le pulizie (cambiano lo stato reale).
  Niente più secondo clic (l'utente ha eliminato tutti i container senza volerlo): cestino, riquadri
  del disco, `Pulisci reti…` e `Pulisci tutto…` mostrano al posto della plancia una domanda
  (`Ask`, `Sheet`) con cosa va via per nome e misura, cosa resta, cosa si perde e come riaverlo
  (`docker compose up -d` nella cartella del progetto, da `com.docker.compose.project.working_dir`;
  `docker run`; pull o build delle immagini), `Annulla` e un tasto rosso che dice cosa fa; un clic
  sul tasto entro 0,5 s dalla domanda non vale. Per le pulizie prima rilegge `/system/df` e
  `/networks` (tasto in attesa fino ad allora). Calcolato come Docker: `system prune` toglie anche
  le immagini dei container fermi; `network prune` toglie le reti senza endpoint, anche se un
  container fermo ci sta, che poi non riparte (verificato nel sorgente di moby): la domanda lo
  nomina. Riquadro Container: `N fermi`, non i MB. Tip su ogni tasto a icona (`Ferma db-1`,
  `Riavvia api-1, db-1 e web-1 di shop`).
  Tip nel motore (`Node::tip`, in Lua `tip`): col mouse fermo 0,6 s su un nodo di un popup,
  un'etichetta sotto (sopra se non c'è posto), da una all'altra subito; via a un clic (finché il
  mouse non va altrove), allo scroll e all'uscita. È un figlio `at` della radice del popup, aggiunto
  da `render_popup` dopo la misura, quindi non cambia la misura del popup; testo a capo con
  `ui::wrap`. Solo nei popup, non nella barra né nel peek. Provato coi dati finti sulla barra di
  prova (it/en), con un comando temporaneo che mandava eventi del mouse al popup (tolto): domande,
  annulla, conferma subito ignorata e dopo accettata, tip con ritardo e passaggio: ~22 MB, 0,13% CPU,
  0 processi. Sulla barra vera dal 17/11/2026 (insieme a `devices`): popup coi container veri,
  stats e disco letti (una cache di build vuota diceva `-0 MB`: la somma di niente è -0.0). Da
  provare con l'utente: il mouse vero sui tip, e una pulizia vera.
- `network` a plancia: in cima Wi-Fi (↻ e interruttore) con le reti vicine a riquadri e i servizi
  cablati con l'interruttore, sotto le metriche: la connessione col segnale (o la velocità di
  Ethernet), download e upload con gli ultimi 30 s a barre (contatori a 64 bit di `NET_RT_IFLIST2`),
  la VPN, poi i Dettagli, chiusi finché non si cliccano e poi aperti fino al reload
  (`Rx::disclosure`, in Lua `rb.ui.disclosure`): il link Wi-Fi (velocità e generazione, canale con
  banda e larghezza, SNR, sicurezza: CoreWLAN, senza la posizione), gli indirizzi che un clic copia
  (IP locale col prefisso, router col ping ICMP senza privilegi, IP pubblico con la latenza e il
  provider da ipinfo.io, DNS, MAC privato o no, IPv6). Contatori e link solo a popup aperto, ogni
  secondo; ping (ogni 5 s) e `curl` (una volta per popup) solo coi Dettagli aperti. Dettagli e
  `rb.ui.disclosure` provati coi dati finti sulla barra di prova (clic, riapertura). Provata coi
  dati finti (it/en) e dal vero, sulla barra di prova e su quella vera, su Ethernet; il ping al
  router lì non risponde (`EHOSTUNREACH` sulla barra di prova): quasi certamente il permesso Rete
  locale, che RustyBar non ha (`home` va in HTTPS, non lo prova). Il ping è provato su loopback nei
  test. Con tutti i widget: a riposo ~20 MB, ~0,1% CPU; popup aperto ~24 MB, ~0,6%
  CPU; 0 processi (un `curl` all'apertura dei Dettagli). Da provare dal vero: il link Wi-Fi con la
  Wi-Fi come connessione, il ping sulla barra vera, la copia negli appunti.
- `battery` a plancia: in cima lo stato con la barra della carica (colore dell'icona, segno al
  limite), il limite di carica come slider a scatti (`Rx::steps`, `labels` di `Kind::Slider`, anche
  da Lua: si ferma ai passi, pallini sui passi dopo, etichette 80–100% sotto; disattivato mentre
  Completa carica ora lo sospende) con Completa carica ora sotto; riquadri capacità e condizione,
  cicli su `DesignCycleCount9C`, temperatura e consumo (`PowerTelemetryData`, V×A della batteria)
  letti da IOKit solo a popup aperto, ogni 2 s; l'alimentatore (W, USB-C/MagSafe, V, potenza in
  ingresso). La griglia ora misura ogni riga alla sua altezza. Provata coi dati finti (quattro
  stati, it/en) e coi dati veri sulla barra di prova (lettura sola): ~15 MB, ~0,1% CPU a popup
  aperto, 0 processi. Da provare dal vero con l'utente: trascinare lo slider (cambia il limite).
- `system` a plancia: CPU e GPU affiancate con l'ultimo minuto a barre (30 letture tenute anche a
  popup chiuso, così il grafico è pieno all'apertura), memoria con la barra divisa in app, wired e
  compressa, un riquadro per disco (avvio prima, poi i volumi locali scrivibili visibili, niente
  immagini disco né condivisioni di rete: `getfsstat` con `MNT_NOWAIT`, poi `NSURL` per usato,
  disponibile ed eliminabile come il Finder, ~8 ms; all'avvio, all'apertura e ogni 10 s a popup
  aperto; clic → Finder; `storage_warning`/`storage_critical` 90/95), un solo elenco di app con
  il selettore CPU/GPU/Memoria (`selected`), Monitoraggio Attività e Impostazioni Archiviazione.
  Provato coi dati finti (it/en, scheda Memoria) e coi dati veri sulla barra di prova: uguale alla
  build di prima (14 MB all'avvio, 20 a popup aperto, 19 dopo; stessa CPU), 0 processi a riposo.
  Da provare: un disco esterno vero, il clic su un disco e su Impostazioni Archiviazione.
- `now_playing` (al centro di default): Music e Spotify, perché MediaRemote da macOS 15.4 risponde
  solo ai processi Apple. Stato e traccia dalle notifiche distribuite (`com.apple.Music.playerInfo`,
  `com.spotify.client.PlaybackStateChanged`, registrate con `DeliverImmediately`: RustyBar non è
  mai l'app attiva), il resto e i comandi con Apple event per pid (Automazione; senza prompt finché
  non c'è un clic o il popup). Copertine ridotte con ImageIO a PNG in `~/Library/Caches/rustybar`.
  Popup a player: copertina, avanzamento con seek, shuffle, ⏮ ⏯ ⏭, repeat, volume dell'app,
  preferito (Music), scelta fra le due app. Col notch il gruppo `center` va accanto al notch:
  a sinistra se c'è spazio, se no a destra, se no compatto, se no non si vede (`place_center`,
  con `SLACK` per tornare a un posto migliore). Provato coi dati finti sulla barra di prova col
  notch (sinistra, destra, compatto, nascosto): ~18 MB, ~0,07% CPU a riposo, 0,3% a popup aperto,
  0 processi. Da provare dal vero con l'utente: Music e Spotify veri, il permesso Automazione, i
  comandi; i clic sul popup (i clic sintetici con `postToPid` non arrivano alla barra).
  `other_apps` (spento di default, scelto dall'utente): qualunque app da MediaRemote, dentro
  `/usr/bin/perl` (firmato da Apple, quindi risponde; un nostro binario riceve niente). Un helper
  in Objective-C (`now_playing.m`, compilato da `build.rs` con `xcrun clang`, universal, dentro il
  binario con `include_bytes!`, scritto in `~/Library/Caches/rustybar/now_playing-<hash>.dylib`),
  chiamato da perl come XSUB: `MRNowPlayingRequest` (`localNowPlayingItem`, `localIsPlaying`,
  `localNowPlayingPlayerPath`, `localSupportedCommands`), copertina con
  `MRMediaRemoteRequestNowPlayingPlaybackQueueForPlayer` (320 px, JPEG, base64 in una riga sua),
  comandi con `MRMediaRemoteSendCommand` e `MRMediaRemoteSetElapsedTime`, eventi con
  `MRMediaRemoteRegisterForNowPlayingNotifications` (un cambio di traccia ne manda 6, niente in
  mezzo). `MRMediaRemoteGetNowPlayingInfo` su macOS 26 non risponde mai, neanche in perl. JSON su
  stdout, comandi su stdin; finisce con stdin (reload, quit, anche `kill -9` della barra); se muore
  riparte dopo 5 s, poi sempre più tardi. Terzo player `App::Other`, ignorato quando l'app è Music
  o Spotify. Provato sulla barra di prova: dati finti (solo l'altra app; tre sorgenti), dati veri
  (Spotify trattato per prova come altra app: copertina, testi, ⏮ ⏭ presenti), tre reload (un
  helper solo), `kill -9` dell'helper (riparte), quit. Barra + helper ~23 MB, ~0,2% CPU, helper
  ~4 MB e 0,02 s di CPU, 0 processi nuovi in 60 s. Dal vero con l'utente su Arc (YouTube):
  copertina della pagina, posizione che avanza (`elapsedTimeTimestamp` è un CFAbsoluteTime, non
  una data), solo ⏯ (Arc non dà ⏮ ⏭), pausa, play e seek mandati all'helper (la barra di prova
  vede la pausa e si attenua). Da provare: il clic sui controlli del popup (i clic sintetici non
  arrivano). Sulla barra vera dal 4/11/2026, con `center = ["now_playing"]` e `other_apps = true`
  nei dotfiles: col notch va a sinistra, compatto. A riposo 32 MB in 2 processi (helper ~4 MB),
  0,23% CPU, 0 processi nuovi. Nei primi minuti dopo l'installazione 2,55% a picchi (0,5–4%), con
  l'utente che passava ad Arc per il video; non si ripete: con Spotify, dopo un riavvio, e con
  Arc e Spotify insieme (barra vera e due barre di prova, solo `now_playing` e solo `system`)
  0,1–0,3%, helper 0. Quasi certamente le pill col vetro che cambiano misura al cambio di
  finestra (~35 ms l'una, vedi sopra), non la riproduzione. Con due app che suonano MediaRemote
  ne dà una sola (lì Spotify): per questo l'helper ora riporta tutti i client
  (`MRMediaRemoteGetNowPlayingClients`, per ognuno `MRPlayerPath` con `MROrigin.localOrigin`,
  `GetPlaybackStateForPlayer`, `GetSupportedCommandsForPlayer` e la coda), in una riga
  `{"players": […]}`; il widget tiene l'app che suona, se no quella che mostrava. I comandi però
  macOS li manda solo all'app che chiama in riproduzione, anche con un `MRPlayerPath` completo
  (`GetNowPlayingPlayerForClient`) e `SendCommandToPlayer`: provato, una pausa per Arc fermava
  Spotify. Quindi ogni player ha `active`, l'helper scarta i comandi per un'app non attiva e il
  popup al posto dei tasti dice di comandarla dalla sua app ("macOS manda i comandi a Spotify").
  Provato dal vero con Arc e Spotify insieme (scheda di Arc che resta, pausa per Arc scartata).
  Titolo lungo: il gruppo `center` riceve lo spazio che gli resta accanto (`Rx::spare`, in Lua
  `ctx.spare`; fino al notch, o fino ai lati quando è in mezzo) e `now_playing` lo dà al titolo,
  poi all'artista. `App::widen` ridisegna il gruppo con lo spazio, che segue subito un lato che
  cresce e uno che cala solo oltre 8 pt (`steady_spare`: le cifre di `system`). Se il titolo
  ancora non ci sta e suona, scorre (`Style::marquee`, in Lua `marquee = true`): striscia col
  testo e una copia dopo 32 pt, `CAKeyframeAnimation` su `transform.translation.x` (3 s fermo, poi
  30 pt/s), maschera a gradiente che sfuma a destra e, mentre scorre, a sinistra; tutto nel window
  server, al massimo 60 fps (`preferredFrameRateRange`); niente con Riduci movimento. Il riquadro
  che cambia non lo fa ripartire. Provato coi dati finti sulla barra di prova col notch: la pill
  arriva al notch, scorre, in pausa si ferma coi puntini, da ferma a scorrere e ritorno coi
  reload: ~18 MB, 0,05% CPU, 0 processi. Il costo nel WindowServer non si misura qui (oscilla
  33–43% per altre app); a coppie sembrava ~3 punti mentre scorre, a 60 come senza limite.
  Popup nei colori della copertina, come il player di Android Auto (`palette.rs`, `cover_colors`,
  di default acceso): copia a 64 px con ImageIO in `cx.background` una volta per copertina (pochi
  ms), pixel in Oklab, median cut poi k-means, colori vicini uniti; sfondo dal colore più esteso,
  accento col punteggio di Material (quota della tinta + croma) più la distanza di tinta dallo
  sfondo, e su uno sfondo colorato prima una tinta sua col 5%. Lo schema tiene la tinta e mette la
  luminosità del tema (scuro o chiaro, `Theme::dark`); un test controlla i contrasti. Hover
  espliciti (`th.highlight`), perché quello di default del popup è del tema globale. Sfondo del
  popup a 0,85 sul vetro. Provato coi dati finti sulla barra di prova: Mocha col vetro (Neon Harbor
  blu notte e oro), macOS scuro (Glass Orchard verde), Latte (oro che diventa ocra, per il
  contrasto del bianco sul play). Popup aperto 40 s: ~20 MB, 0,2% CPU, 0 processi (senza i colori
  19 MB, 0,25%). Da provare dal vero: copertine di Music, Spotify e di un'altra app.
- Peek (`Widget::peek`, per ora solo `now_playing`, `hover = true` di default): col mouse fermo
  0,1 s su una pill (0,25 s era lento per l'utente), la pill cresce in giù con quello che dà `peek`
  (qui avanzamento con i tempi e ⏮ ⏯ ⏭ come l'app li prende, o "Si comanda da X"); un clic sulla
  cima apre il popup. Uscendo (anche sulla barra) torna pill dopo 0,2 s, se il mouse non rientra;
  il mouse che torna sulla pill mentre si richiude la fa ricrescere subito. Niente bagliore
  dell'hover sulla pill né sulla cima cresciuta (l'ha chiesto l'utente): un `hover` trasparente
  vuol dire nessuno, nemmeno quello del tema; con `hover = false` torna quello solito. È una
  finestra sua (`SurfaceId::Peek`, al livello della barra) grande già da cresciuta: dentro una card
  con l'aspetto della pill (vetro, velo e bordo della pill anche da cresciuta: l'utente la vuole
  di vetro come la pill, il velo del popup la faceva piatta) che si allarga coi layer e ritaglia
  (`Style::clip`), il contenuto della pill fermo al suo posto (preso dal nodo che la barra ha
  disegnato, `Bar::tree`/`peek_pill`, anche allargato da `spare`). La pill nella barra intanto ha
  opacità 0, senza animazione (`App::instant`). Il vetro, imparato a fotogrammi su righe colorate:
  una `NSGlassEffectView` ad alpha 0 si vede ancora (sotto il peek c'erano due vetri, più scuri),
  e una tolta e rifatta compare scura e si schiarisce in ~0,3 s; quindi il vetro di una pill
  nascosta all'istante va fuori dalla finestra (`PARKED`) e torna senza essere rifatto. Il vetro
  prende una misura nuova dall'`animator` 2–3 fotogrammi dopo i layer: il contenitore dei vetri è
  capovolto come la superficie (`GlassHolder`), così uno che cresce in giù cambia solo misura; il
  vetro del peek ha la misura da cresciuto fin dal primo fotogramma (la card cresce dentro, mai
  fuori dal vetro) e si stringe dietro la card tornando. Alla fine la pill riappare e il peek si
  nasconde nello stesso render (card a opacità 0 e senza vetro, che va via subito); la finestra va
  0,1 s dopo, poi `trim_cache` e `release_memory` come per i popup. Pannelli senza animazione di
  finestra (`NSWindowAnimationBehavior::None`). Cresce dal lato del popup; il gruppo centrale
  accanto al notch verso lo spazio che ha trovato, non sulle pill del suo lato.
  Eventi `PeekOpened`/`PeekClosed` (Lua `peek_opened`/`peek_closed`), `Cx::peek_open`: il tick di
  1 s solo da cresciuta. Provato coi dati finti sulla barra di prova (Mocha col vetro, col notch,
  forma piena e compatta) con `msg peek`, a fotogrammi davanti a righe colorate: nessun fotogramma
  senza pill né con due vetri, il ritorno è un passo solo. A riposo ~16 MB, ~0,07% CPU, 0 processi
  (anche dopo 20 crescite); cresciuta ~18 MB, 0,2%. Una crescita e un ritorno ~125 ms di CPU (il
  popup ~64). Provato dall'utente col mouse vero sulla barra di prova il 4/11/2026: crescita,
  aspetto di vetro, ritorno senza sfarfallio, uscita e rientro veloci. Da provare: clic sui tasti
  e sulla cima, trascinare l'avanzamento.
- Isola del notch (`now_playing`, `notch = true` di default; decisioni di design dell'utente,
  prese con i mockup prima di scrivere codice): sugli schermi col notch il primo widget del
  gruppo `center` può essere un'estensione del notch (`Rx::island`, `Island` con la misura del
  notch, `notch_height` da `safeAreaInsets`): nero, alto quanto il notch (38 pt), con le
  "orecchie" concave in cima (`Style::notch`, `Notch { ear, area }`: maschera `CAShapeLayer` col
  percorso del notch, `notch_path`, stessi punti per ogni misura). `bar_tree` misura prima l'isola
  al minimo (copertina 26 pt + 36 pt di titolo a sinistra, barre a destra) e i lati le fanno
  posto, poi le dà quello che i lati lasciano accanto (`island_spare`, con `steady_spare`); a
  sinistra copertina, titolo e artista (titolo che scorre), a destra le barre (`Kind::Bars`,
  animate nel window server) nel colore più vivo della copertina (`Palette::dark_accent`), in
  pausa ⏸ e copertina attenuata; tema `Theme::notch` (nero anche coi temi chiari). Sotto il mouse
  cresce col peek (anche shuffle e repeat, ora anche nel peek della pill), al clic nel player
  (`Popup { view: None }`: il popup dell'isola è il peek allo stadio `Stage::Popup`), da sola per
  3 s a un cambio di traccia (`Widget::notice`, `Cx::notice`, `watch_track`: non la prima, non
  una pausa, non col mouse sopra, non col popup o il peek aperti). Si muove solo la maschera, con
  una molla (`Surface::spring`, `CASpringAnimation` su `path` dal percorso a schermo; `GROW` con un
  po' di rimbalzo, `SHRINK` senza); il contenuto resta al suo posto e il contenuto di ogni stadio
  ha la stessa chiave, così avanzamento e controlli scivolano dal peek al player. Imparato a
  fotogrammi (video di `screencapture -v`, fotogrammi con AVAssetImageGenerator): una finestra
  che cambia misura mostra i layer già arrivati, senza animazioni; quindi la finestra dell'isola
  ha da subito il posto per tutti gli stadi più il rimbalzo (`island_room`, `OVERSHOOT`) ed è
  larga solo attorno al notch (`Peek::room`), e un layer col notch cambia frame senza animazione
  (se no il nero restava indietro rispetto al contenuto). Nella finestra il mouse fuori dalla
  forma va alla barra sotto (`on_mouse`); un clic sotto la barra chiude solo il player. Esc non
  c'è: i popup non hanno tasti (servirebbe Monitoraggio input). Coi dati finti ⏮ ⏭ passano per la
  `queue` di `now_playing.json`. Provata coi dati finti sulla barra di prova (col notch vero,
  40 pt sotto): riposo, peek, player, notice, ritorno, a fotogrammi: a riposo ~18 MB, 0,07% CPU;
  peek 19 MB, 0,25%; player 20 MB, 0,2%; dopo 10 crescite 19 MB, 0%; 0 processi. Sulla barra
  vera dal 4/11/2026: attaccata al notch vero (orecchie comprese), a destra tutti compatti tranne
  `notifications`; a riposo (con l'helper di `other_apps`) 25 MB in 2 processi, 0,17% CPU,
  0 processi nuovi (il primo minuto dopo l'avvio 1,5%). Da provare con l'utente: il mouse vero
  (crescita, clic sulla cima, clic fuori, passaggio alla barra sotto), il notice a un cambio di
  traccia vero, le barre col colore di una copertina vera.
- `now_playing` è il primo a compattarsi (l'ha chiesto l'utente: `aerospace` era compatto e
  l'isola no), ma solo se serve: ciò che sta fra i lati, l'isola o il gruppo al centro senza
  notch, è compatto quando così un lato a cui lascia più posto cede meno (`fit` fatto due volte,
  livelli confrontati per lato: l'isola compatta aiuta solo il lato dove si stringe un'ala),
  prima degli `slim_first`; da compatto non prende altro spazio (niente `spare` né `widen`).
  Se i lati cedono lo stesso (con le finestre vere `aerospace` intero non ci sta neanche senza
  isola), resta intero e prende lo spazio che avanza: l'ha chiesto l'utente dopo averlo visto
  compatto per niente sulla barra vera. Torna intero solo con `SLACK` di scarto: da compatto la
  forma intera si misura con ogni lato più stretto di `SLACK` (`Bar::center_compact`). Isola
  compatta scelta dall'utente fra tre mockup: solo la copertina a sinistra (46 pt invece di 89 al
  minimo), barre a destra; titolo e artista nel peek. Il gruppo accanto al notch senza isola
  (`place_center`) è com'era: prende già solo lo spazio che avanza. Provato coi dati finti sulla
  barra di prova col notch (margini 250–360: isola compatta con `aerospace` intero, poi
  `aerospace` compatto e isola di nuovo intera; peek col titolo), senza notch con un ritocco
  temporaneo di `display.rs` (tolto), e al limite con una pill Lua che cambia di 10 pt ogni
  secondo (forme ferme da tutti e due i lati; un reload riparte da capo, `center_compact`
  compreso). Coi dati finti ~17 MB, 0,02% CPU, 0 processi. Sulla barra vera dal 6/11/2026:
  `aerospace` compatto, isola intera con 387 pt a sinistra; 27 MB in 2 processi, 0,35% CPU dopo
  qualche minuto.
- Avanzamento a riposo di `now_playing` (opzione D dei mockup, scelta dall'utente): sulla pill una
  linea dritta sulla parte piatta del bordo in basso (non segue la curva: l'utente la vuole dritta),
  nell'accento del tema, `overlay` in pausa; sull'isola un anello attorno alle barre (più piccole,
  13×10, `Kind::Bars` si stringe in un nodo più stretto) nel colore della copertina, su una traccia
  `surface`. È `Style::trace` (`Trace`: `value` a `at`, `rate` al secondo, `Along::Bottom` o
  `Around`; in Lua `trace = {…}`): un `CAShapeLayer` sopra i figli con `strokeEnd` animato da Core
  Animation fino alla fine della traccia, lineare, a pochi fps (mezzo pixel per fotogramma); un
  render lo rifà solo a pausa, seek, traccia nuova o se è indietro di più di 1 s secondo l'orologio
  di CA (`CACurrentMediaTime`, che si ferma col Mac in stop come l'animazione). Imparato: la
  transazione del render impone il suo ease alle animazioni esplicite aggiunte dentro (la linea
  andava ~4× troppo veloce all'inizio); va aggiunta in una transazione sua, lineare. Nella pill
  cresciuta (peek) la linea sparisce: c'è la barra del peek. Niente per uno stream (durata 0).
  Provato coi dati finti sulla barra di prova (pill in riproduzione e in pausa, misurata la
  velocità; peek), l'isola con un notch finto temporaneo (lo schermo col notch non c'era):
  ~12 MB, 0,07% CPU, 0 processi. Da provare col notch vero e coi player veri (pausa, seek, cambio
  di traccia, risveglio).
- `events` a plancia: in cima l'evento della barra (`featured`: quello in corso che finisce prima,
  finché non comincia il prossimo, se no il prossimo) in un riquadro col colore del conto alla
  rovescia, con orari, calendario (pallino del suo colore) e luogo, invitati che hanno accettato,
  avanzamento se è in corso e `Partecipa a Google Meet`; clic sul riquadro → Calendario. Sotto gli
  eventi di tutto il giorno come chip, poi tutti quelli con l'orario: barretta del colore del
  calendario, inizio sopra la fine, titolo sopra calendario · luogo, i passati attenuati e una riga
  rossa all'ora di adesso dopo quelli cominciati; `Apri Calendario…` in fondo. Da EventKit anche
  nome e colore del calendario (`NSColor` in sRGB, feature `objc2-app-kit` di `objc2-event-kit`),
  luogo (non un link) e partecipanti. Con i dati finti i clic non fanno niente. Provato coi dati
  finti (it/en, prossimo e in corso, titoli lunghi, chip su due righe): ~16 MB, ~0,1% CPU, 0
  processi, popup aperto o chiuso. Da provare dal vero con l'utente: colori dei calendari veri,
  i clic (riquadro, Partecipa, righe, chip).
- `notifications`, ignorare senza aprire: una ✕ in fondo a ogni riga di app e di crash (il resto
  della riga apre, come prima) e `Ignora tutte` sotto quando le righe sono almeno due. Un badge
  ignorato resta nascosto (dal conto verde, dal popup e dai pallini di `aerospace`) finché non sale:
  il widget tiene l'etichetta che aveva e, a ogni cambio del Dock, la abbassa se scende e la
  dimentica se il badge sparisce o sale (il pallino di Slack conta meno di ogni numero). Nei default
  (`badges.dismissed`, bundle → etichetta), non coi dati finti; i crash come all'apertura
  (`crashes.seen`). Popup rimasto vuoto: si chiude. Azioni per bundle, non per indice (`app:<bundle>`,
  `dismiss:<bundle>`); coi dati finti apertura dell'app e di Console non fanno niente. Provato coi
  dati finti sulla barra di prova, con clic veri: ✕ di Slack e del crash, `Ignora tutte` con e
  senza Homebrew (popup chiuso, widget via dalla barra). Da provare dal vero: un badge vero che
  torna, e che resta ignorato dopo un reload.
- `claude` in una pill sola (slim o compatto): l'account i cui numeri sono saliti per ultimi
  (quello in uso: sessione o settimana più alte, o una finestra nuova cominciata con qualcosa
  dentro; non i numeri vecchi di una lettura fallita né un altro login), ricordato nei default
  (`claude.latest`, per config dir) così resta dopo un reload; la sessione più impegnata finché
  nessuno sale, o se quell'account non ha numeri. Provato coi dati finti (`latest` in
  `claude.json`, e senza) e, coi dati veri sulla barra di prova, col valore scritto a mano nei
  default (poi tolto). Da provare dal vero: il passaggio da un account all'altro usandoli.
  Il piano viene dal profilo che Claude Code tiene in `.claude.json` (`oauthAccount.organizationType`,
  `claude_max` → `Max`, più il multiplo dal rate limit tier: `Max 5×`), non da `auth status`, che
  tiene il piano del login (diceva `pro` dopo il passaggio a Max). Provato coi dati veri sulla barra
  di prova: Pro, Team, Max 5×.
- `system` cede prima di `claude` (l'ha chiesto l'utente: `claude` conta di più): `slim_first`
  di default `audio`, `system`, `claude`, e la forma slim di `system` è quella compatta (solo la
  memoria). L'ordine di `fit` dice cosa conta meno, non cosa deve cedere: quando un passo serve
  comunque, quelli prima tornano indietro se lo spazio basta, dal più importante, con `SLACK` se
  non erano già tornati (`Given`, ordinato su quanto ha ceduto il lato: il confronto del gruppo
  centrale compatto lo usa). Vale per tutti i passi: anche un widget lontano dal bordo torna
  intero quando uno più vicino è diventato compatto e lascia lo spazio. Provato coi dati finti
  sulla barra di prova sullo schermo esterno (1920 pt): `system` solo memoria con `claude`
  intero, poi `claude` in una pill con `system` intero, poi entrambi; stati fermi su tutti e due
  gli schermi: ~23 MB, 0,03% CPU, 0 processi. Sulla barra vera dal 10/11/2026 (build fatta da
  HEAD più solo questo cambiamento): sullo schermo esterno `system` e `claude` interi (solo
  `audio` slim; prima `claude` era in una pill); sul MacBook `system` e `claude` slim, `audio`
  compatto (prima compatti anche `home` e `docker`). Forme ferme per minuti (cambiano solo le
  larghezze di `system` e `front_app`); primo frame 33 ms; 27 MB nel primo minuto, 0,63% CPU;
  poi, con l'utente che cambiava app, 34 MB e 1,23% (il vetro di `front_app`), ma 10 s di
  `sample` ~0,3%; 0 processi.
- `devices` (chiesto dall'utente, come il widget `devices` di rustybar-windows, non di
  SketchyBar; non nel layout di default): le batterie di mouse, tastiere, trackpad, controller e
  cuffie. Pill: i collegati sotto `low` (20), i tipi di `pinned`, o tutti con `always`; per
  ognuno tipo, nome corto (`short_name`: senza proprietario, marca e parole generiche; `names` per
  i propri), barretta e numero col colore del livello, fulmine in carica, righe sottili fra uno e
  l'altro; compatta tipi, barrette e fulmini; quelli che dormono attenuati. Popup: un riquadro per
  collegato (badge del tipo, nome, marca · collegamento · stato, livello grande, una barra per
  parte: metà di una tastiera divisa, auricolari e custodia; tinta quando è basso), poi
  `Non collegati`, poi Impostazioni Bluetooth. Due fonti: l'elenco degli accessori di macOS
  (`IOPSCopyPowerSourcesByType(4)`, lo stesso di `pmset -g accps`: Bluetooth, BLE col servizio
  batteria, cuffie HFP, AirPods per parte) letto sul main thread (~0,1 ms) alle notifiche di
  powerd (`notify_register_dispatch` su `accpowersources.attach/.source/.timeremaining` e
  `powersources`, che su un MacBook arriva ogni minuto); e `hid.rs`, i protocolli di Windows
  portati su IOKit HID (Razer a feature report da 90 byte, Logitech HID++ 2.0 con i registri di
  Bolt e Unifying, ROG `12 01`/`12 07`, Steam Controller report 0x43) in `cx.background`:
  all'avvio, 1 s dopo che IOKit dice che un'interfaccia di quei produttori va o viene, al
  risveglio, all'apertura del popup e ogni 5 minuti se ce n'è una. Solo le interfacce di quei
  quattro produttori, aperte senza esclusiva, mai una con una collezione tastiera (1,6)/(1,7):
  così niente richiesta di Monitoraggio input. Sony e Xbox li lascia a macOS (gamecontrollerd),
  non provato. Un dispositivo visto da tutte e due le fonti (stesso produttore e tipo) compare
  una volta sola. Provato coi dati finti (it/en, screenshot del README) e coi dati veri sulla barra
  di prova (`always = true`): trackpad Apple, occhiali audio BLE e una tastiera ROG in Bluetooth
  da macOS, nessuna richiesta di permessi; dietro un ricevitore o un cavo una tastiera Razer è
  trovata (14 ms) ma risponde 4 (timeout) e va in `Non collegati`. ~15 MB, 0,02% CPU, 0 processi
  in 60 s; apertura e chiusura del popup ~50 ms di CPU; primo frame 36–43 ms. In 30 minuti nessun
  livello è cambiato: da provare che `accpowersources.*` arrivino a un cambio di livello. Da
  provare con l'utente: la tastiera Razer (cavo o HyperSpeed, accesa), il ricevitore ROG,
  ricevitori Logitech, controller in macOS, AirPods (parti raggruppate), la pagina delle
  impostazioni. Sulla barra vera dal 17/11/2026 (nei dotfiles prima di `battery`, opzioni di
  default; build HEAD + docker + devices): pronto su tutti e due gli schermi, lettura dei
  ricevitori 49–252 ms sul thread; 31 MB in 2 processi (con l'helper di `now_playing`), 0,32% CPU
  nel primo minuto, che ha 2 processi brevi all'avvio; poi 70 s senza nessuno.
  Icone dei modelli (chieste dall'utente): Magic Mouse `magicmouse.fill`, Magic Trackpad e Magic
  Keyboard disegnati in `gfx` nello stile di `magicmouse.fill` (SF Symbols non li ha, nemmeno fra
  quelli privati), riconosciuti dal vendor Apple o dal nome; occhiali (per macOS un `Headset`) dal
  nome, `eyeglasses`. Gli schermi dell'utente sono a 1x: l'unità è `size / 14` arrotondata ai
  pixel, tasti da 2 unità a 1 di distanza (la prima versione, 7 colonne in 22 pt, a 1x era
  sfocata). Provati coi dati finti sulla barra di prova (pill a 1x ingrandita, badge del popup) e
  sulla barra vera dal 17/11/2026 (trackpad e occhiali; la ROG resta `keyboard.fill`).
  In carica senza che lo dica: via BLE (la ROG col cavo a un caricatore, non al Mac) macOS ha solo
  il livello, senza `Is Charging` e con `Power Source State` = `Battery Power`. Quindi `rising`:
  un dispositivo che non lo dice (`says_charging`) è in carica dal secondo passo in su, ognuno
  entro `STEADY` (15 minuti) dal precedente, finché un passo non arriva in tempo, il livello
  scende o è al 100%; senza la chiave, `AC Power` vale in carica. Prima bastavano 2 punti sopra il
  minimo, fino alla discesa: un mouse Logitech a pila AA via BLE (mai in carica) era a 100% e in
  carica, per un solo salto in su (una pila a riposo si riprende) che a 100% non scendeva più.
  Sulla barra vera dal 17/11/2026: il mouse a 100% senza fulmine. Da riprovare: la ROG al
  caricatore (il fulmine ora dopo due passi in su, ~2 minuti).
  Il fulmine resta anche nella forma compatta (senza, da compatta la carica non si vedeva). Provato
  sulla barra vera dal 17/11/2026 con la ROG al caricatore (sale di 1–3 punti al minuto): dopo un
  riavvio, che azzera la memoria, fulmine in pill e popup ("in carica") entro 5 minuti.
- Senza notch il gruppo `center` non è più fisso a metà (l'utente vedeva pill compatte e la `⋯`
  a destra con ~190 pt liberi a sinistra, sul monitor da 1920 pt): i lati si dividono lo spazio
  che lascia (`Room::Shared`, una corsia sola come senza gruppo centrale; prima `Split`, metà
  ciascuno), e il gruppo resta in mezzo finché può, poi si sposta verso il lato che ha posto,
  quanto basta (`center_x`). Per il titolo prende tutto lo spazio fra i lati (`spare`), e il peek
  cresce lontano dal lato a cui si appoggia (`Bar::lean`). Col notch non cambia niente. Provato
  coi dati finti sulla barra di prova accanto alla build di prima, stessa config dei dotfiles:
  sul MQ16AHE (1920 pt) nessun widget nella `⋯` (prima cinque), `now_playing` compatto contro
  `home`; sull'Odyssey (5120 pt) `now_playing` intero con l'artista, `system` e `claude` interi
  (prima compatto, con `claude` e `audio` slim); peek della pill compatta verso sinistra. ~21 MB,
  0,03% CPU, 0 processi in 60 s; primo frame 36 ms. Sulla barra vera dal 17/11/2026 (build del
  working tree, con dentro il lavoro su `gfx.rs` che un'altra sessione stava facendo): sul MQ16AHE
  niente `⋯` e `home`, `network` e `clock` di nuovo interi; sull'Odyssey tutto intero,
  `now_playing` di 511 pt; 27 MB in 2 processi, 0,33% CPU nel primo minuto, 0 processi.
- Chiamate in `now_playing` (chieste dall'utente; scelte con i mockup: a riposo nome, videocamera e
  microfono; mic, videocamera, mano, mostra la chiamata, Esci; l'isola cresce da sola solo
  all'inizio di una chiamata): `call.rs` (Slack, Teams) e `meet.rs` (Meet).
  Meet con un'estensione Chromium di RustyBar (`extension/`, MV3, nell'app in
  `Contents/Resources/extension`; caricata non pacchettizzata dall'utente): lo script nella pagina
  di Meet trova i tasti da `data-is-muted` e dai nomi delle icone Material (`call_end`,
  `back_hand`), indipendenti dalla lingua, e guarda la pagina con un `MutationObserver` (al più ogni
  200 ms), quindi niente timer; la mano solo nella barra dei controlli (l'antenato di Esci con i
  `data-is-muted`): fuori c'è un'altra `back_hand` che apre i partecipanti. La mano alzata Meet la
  dice solo con l'etichetta (`Alza`/`Abbassa la mano`, nella lingua della pagina; niente
  `aria-pressed`): alzata, il tasto ha altre classi al posto di alcune di quelle con cui è fatto
  (`data-idom-class`), come il microfono da muto; visto a confronto di due dump, alzata e no.
  Installata o ricaricata (↻ sulla scheda o Aggiorna in `chrome://extensions`), l'estensione si
  mette da sola nelle schede di Meet aperte (`scripting`), senza ricaricare la riunione; la copia
  vecchia si ferma (per versione: il worker ascolta la più nuova di una scheda). Ogni cambiamento
  dell'estensione alza `version` nel manifest: alla prima linea (`hello`) la barra confronta la
  versione con quella dell'app e, se è più vecchia, le chiede di ricaricarsi dalla sua cartella
  (`chrome.runtime.reload`), una volta per versione; `rustybar msg trigger meet_reload` la ricarica
  senza cambiare versione. Dia ricarica davvero solo con la ↻ della scheda dell'estensione, non con
  quella della pagina. Nessun permesso per 127.0.0.1: Local Network Access di Chromium 158 (Dia)
  lascia passare il WebSocket del worker anche senza. Lo script parla col worker con una porta solo
  durante una chiamata, il worker tiene un WebSocket con la barra (`ws://127.0.0.1:47823/meet`,
  riprova 1→10 s, un messaggio ogni 20 s perché Chrome non lo fermi) solo finché una scheda è in
  chiamata. Nella barra `meet.rs`: un server avviato una volta per processo, un thread per linea che
  aspetta il messaggio dopo (niente in mezzo), handshake RFC 6455 con `CC_SHA1`, solo con l'`Origin`
  dell'estensione (id `palkoaepeohmdiehbgmiglbndcebomlk`, fisso per la `key` del manifest); il
  browser dall'altra parte come fa `lsof` (`proc_pidfdinfo` di ogni processo, il socket TCP da
  quella porta a 47823: ~10 ms una volta per linea), poi `call::app_of`. Un reload chiede di nuovo
  lo stato alle linee aperte (`resend`). Una seconda barra (di prova) non ha la porta e lo scrive
  nel log. Prima era con Apple event, lasciati perché Dia esegue JavaScript solo se avviato con
  `--enable-applescript-javascript` (visto nel binario: il suo `execute` guarda
  `CommandLine.arguments`, non la preferenza `browser.allow_javascript_apple_events` che ArcCore ha)
  e l'utente non vuole avviarlo così. Nome: Meet non ne ha per una riunione fatta al momento (titolo
  `Meet - <codice>`): quello dell'evento di oggi col link (`events::meet_title`, EventKit in
  background, una volta per codice), se no il codice. La chiamata della barra viene da `settle`: le
  schede di Meet più quella di Slack o Teams letta (`polled`), tenuta quella mostrata. Una linea che
  si chiude tiene le sue chiamate fino a 20 s (`LINE_GONE`) se il browser c'è ancora (Dia ha chiuso
  da sé la linea due volte in una chiamata, tornata dopo ~16 s), 1 s se è uscito. Controlli di Meet
  con la grafica di Meet (chiesto dall'utente; `Look`): i colori della sua barra dei controlli presi
  da uno screenshot (scura in ogni tema, muto rosa `#f9dedc`/`#601410`, mano alzata blu
  `#a8c7fa`/`#062e6f`, neutri `#333537`, Esci una pillola rossa `#dc362e` con la sola icona), angoli
  di 12; Slack e Teams restano nei colori del tema. Provati coi dati finti sulla barra di prova
  (isola: peek e popup).
  Slack e Teams: Accessibilità, tasti per nome (dicono cosa fa il clic: "Mute" col microfono
  acceso), AXPress, `AXManualAccessibility` per Electron; elementi tenuti fra una lettura e l'altra.
  L'API locale di Teams (8124) è stata ritirata da Microsoft il 30/06/2026 (MC1266901, da una sola
  fonte); nei settaggi di Teams c'è ancora `enable_third_party_devices_service = false`. Senza
  permessi l'app che cattura compare con cosa manca (`Blocked`). `rustybar msg trigger call_dump`
  scrive l'albero AX di Slack e Teams in `~/Library/Caches/rustybar/call-<bundle>.txt`, e i tasti
  che l'estensione vede in ogni scheda di Meet in `call-meet-<scheda>.txt`. Dati finti:
  `examples/mock/call.json`, `msg trigger call service=meet|slack|teams|end`. Provato coi dati finti
  sulla barra di prova (isola, notice, peek, popup, pill, compatto): ~20 MB, 0,03% CPU, 0 processi.
  Da provare con l'utente, tutto dal vero: un huddle di Slack e una riunione di Teams (Accessibilità
  a RustyBar, poi `call_dump` per scrivere i nomi veri dei tasti: quelli di `ax::classify` sono
  ipotesi), il costo del polling durante una chiamata.
  Chiamata e musica insieme (chiesto dall'utente): la barra mostra l'ultimo dei due che cambia
  (`Focus`, `follow`): la chiamata quando comincia, la musica quando parte, di nuovo la chiamata
  alla pausa o quando il microfono si accende; schede in cima a peek e popup (la chiamata e ogni app
  con qualcosa, `source:call`/`source:<app>`), che cambiano anche la barra; peek e popup aperti
  restano sulla loro scheda (`view`) anche se la barra cambia. Microfono acceso mentre suona
  qualcosa: microfono giallo nella barra, avviso in peek e popup, l'isola cresce 3 s
  (`warning_notice`). Provato coi dati finti sulla barra di prova (isola: notice, peek e popup con
  le schede; pill e peek), con test per le regole. Dal vero, ancora con gli Apple event (Meet in Dia
  aperto con l'argomento): stato del microfono e della videocamera giusto. Con l'estensione in Dia,
  dal vero con l'utente: la mano alzata dalla barra, il suo stato letto dalle classi (tasto blu),
  l'aggiornamento da sé da 1.4 in poi. Da provare: microfono, videocamera, mostra ed Esci dalla
  barra; il nome dal calendario con una riunione in calendario. Il ponte provato con un client finto
  (`Origin` sbagliato → 403, quello dell'estensione → 101, chiamata in barra e via, ping → pong) e
  con un test che trova il processo dall'altra parte (`nc`). Sulla barra vera dal 23/11/2026: 25 MB
  in 2 processi, 0,8% CPU nei primi 30 s dopo l'avvio, 0 processi. Dal vero con l'utente in Dia:
  chiamata trovata (codice, microfono e videocamera giusti), mano alzata dalla barra, dump con la
  mano alzata e no; durante la chiamata 28 MB in 2 processi, 0,22% CPU in 60 s, 0 processi (con gli
  Apple event la lettura costava 20–50 ms ogni secondo).
- `fn_keys` (chiesto dall'utente, segni scelti dai mockup: tasto fn, pallino, mela, isola,
  AeroSpace, linguetta; uno o più, a scelta in `show`; spenti, spariscono): mentre F1, F2… sono
  tasti standard un segno lo dice. Modo letto e cambiato in-process (`fnkeys.rs`:
  `IOHIDCopyCFTypeParameter`/`IOHIDSetCFTypeParameter` di `HIDFKeyMode` su IOHIDSystem, poi
  `com.apple.keyboard.fnState` nel dominio globale e la notifica `com.apple.keyboard.fnstatedidchange`
  di Impostazioni Tastiera, che la barra ascolta: niente timer). Stato condiviso con gli altri
  widget per generazione (`fn_keys::mark`, `fn_keys::switch`): `apple` (fn in alto accanto al logo,
  interruttore nel menu), `aerospace` (F cerchiata dopo i workspace), `now_playing` (fn nell'ala
  destra dell'isola), `App::bar_tree` (linguetta al bordo sinistro, il lato sinistro le fa posto).
  Per cambiarlo: clic su tasto, pallino, F e linguetta; peek con l'interruttore (un nodo senza
  pill, il pallino, cresce con l'aspetto della pill); menu della mela; `hotkey` (Carbon,
  `RegisterEventHotKey`); `msg trigger fn_keys [mode=on|off]`; interruttore nella pagina delle
  impostazioni (passa dalla barra); `apps`/`media_apps` col modo per app, il resto quello scelto a
  mano (`fn_keys.base` nei default). Provato coi dati finti sulla barra di prova col notch: tutti i
  segni, spento e riacceso (via trigger e dall'interruttore delle impostazioni via AX), peek di
  tasto e pallino, menu della mela, una casella dei segni che scrive il TOML: 17 MB, 0,03% CPU,
  0 processi. Dal vero sulla barra di prova, con l'OK dell'utente (e il modo rimesso com'era):
  cambio col trigger (preferenza e tutti i 232 servizi con `HIDFKeyMode`, senza permessi), un
  cambio da fuori seguito, la scorciatoia (un F19 sintetico vale solo col flag fn, che i tasti F
  veri hanno), `media_apps` con l'app davanti all'avvio, al reload e all'uscita: andando via il
  widget rimette il modo scelto a mano. Per questo i widget si chiudono in
  `applicationWillTerminate` (`App::stop`): nei thread local dopo exit il drop di un hot key
  mandava in abort. Sulla barra vera dal 23/11/2026 (build HEAD + `fn_keys`), con le scelte
  dell'utente nei dotfiles: `show = ["island", "edge"]` (nessuna pill), `ctrl-alt-f`, `apps` VS
  Code, Xcode e Android Studio, `media_apps` Spotify e Keynote; linguetta e "fn" nell'isola visti
  sullo schermo; 28 MB in 2 processi, 0,23% CPU, 0 processi in 60 s. Poi, chiesto dall'utente
  lo stesso giorno: dove l'isola mostra "fn" gli altri segni non compaiono (`shows`,
  `Rx::notch_island`: l'isola si disegna prima dei lati); servono agli schermi senza isola, e
  al notch quando non suona niente. "fn" nell'isola si clicca (anche da cresciuta), come gli altri
  segni: l'utente non aveva modo di spegnerli da lì. La linguetta sotto il mouse spariva: il
  colore di `hover` prende il posto dello sfondo, quindi su un nodo pieno dev'essere un colore
  pieno (`primary.mix(on_primary, 0.18)`, come il play). Provato coi dati finti sulla barra di
  prova col notch, con clic veri: con l'isola solo il suo "fn", senza (`center = []`) linguetta,
  mela, tasto e F di AeroSpace; clic su "fn" nell'isola e nell'isola cresciuta; linguetta sotto
  il mouse. Sulla barra vera: 27 MB in 2 processi, 0,22% CPU, 0 processi in 60 s. Da provare con
  l'utente: tastiere e Impostazioni Tastiera che seguono, il modo per app al cambio d'app.
- `whatsapp` (chiesto dall'utente; scelte sue: fonte l'app del Mac, leggere dalla barra non manda
  le spunte blu, nel notch chi scrive e l'anteprima, notch A e popup 1 dei mockup; non nel layout
  di default, va dopo `now_playing` in `center`): i non letti di WhatsApp, da leggere e a cui
  rispondere. `wa.rs` legge `ChatStorage.sqlite` nel group container dell'app
  (`group.net.whatsapp.WhatsApp.shared`, Core Data: `ZWACHATSESSION`, `ZWAMESSAGE`,
  `ZWAMEDIAITEM`, date dal 2001) con la SQLite di sistema via FFI, in sola lettura, in
  `cx.background`, a ogni scrittura dell'app (`notify` sulla cartella, solo `ChatStorage.sqlite*`,
  dopo 0,4 s di quiete): niente polling, niente processi. Solo chat e gruppi (tipi 0 e 1), non
  archiviati; tipi di messaggio 0 testo, 1 foto, 2 video, 3 vocale, 4 contatto, 5 posizione,
  7 link, 8 documento, 11 GIF, 14 eliminato, 15 sticker, 46 sondaggio; 6 e 10 (di sistema) fuori.
  I media recenti sono già sul disco (negli ultimi 3000 messaggi dell'utente 80 foto su 82 e 97
  vocali su 98): niente download né decifratura; per gli altri la miniatura e `Apri in WhatsApp`.
  I vocali (Opus in Ogg) li apre `AVAudioPlayer`, con AVFAudio caricato con `dlopen` al primo
  ascolto. Letto qui: `whatsapp.seen` nei default (jid → id dell'ultimo messaggio), fuori dal
  conto finché non ne arriva un altro; WhatsApp non ne sa niente.
  Notch: `bar_tree` ora prende come isola il primo del gruppo `center` che ne disegna una, quindi
  `whatsapp` è l'isola quando `now_playing` non ha niente (foto e nome di chi ha scritto per
  ultimo, a destra il conto), e quando c'è musica è un segno nell'ala destra della sua isola
  (`whatsapp::island_mark`, come `fn_keys`; stato condiviso per generazione); il suo `cx.notice()`
  senza isola propria fa crescere quella dell'altro (`App::notice`), che mostra
  `whatsapp::guest_notice`; il popup di un widget senza pill parte dall'ala destra dell'isola.
  Campo di testo nel motore: `Kind::Input` (`Node::input`), `Surface::inputs`, `App::sync_field`
  a ogni render mette `input::Field` sopra il popup, anche quello dell'isola; prende la tastiera
  alla creazione; ⌘V ⌘C ⌘X ⌘A a mano (la barra non ha il menu Composizione); Esc chiude.
  Invio (`wa::send`, non c'è altro modo di scrivere in una chat): Accessibilità sulla finestra
  dell'app, con gli identificatori visti nella 26.46 (`ChatListView_TableView`, righe `AXButton`
  con `AXDescription` = nome, `NavigationBar_HeaderViewButton`, `ChatBar_ComposerTextView`,
  `ChatBar_SendButton`, che c'è solo con un testo). Imparato dal vero: `AXValue` scritto resta nel
  campo ma l'app non se ne accorge (niente tasto Invia); i tasti con `CGEventPostToPid` arrivano
  solo con WhatsApp davanti (da dietro `AXFocused` non prende); un `AXPress` sul tasto appena
  comparso si perde. Quindi: si ricorda l'app davanti, WhatsApp davanti (`open_app`, fino a 5 s
  per la finestra), riga della chat (o link `whatsapp://send?phone=` per una persona fuori
  elenco), intestazione uguale al nome o al numero (la chat con se stessi e i non in rubrica
  l'app li mostra col numero: `same`), campo vuoto, `AXFocused`, testo a tasti (16 unità UTF-16
  per evento), `AXValue` uguale al testo, 0,35 s, `AXPress` (fino a tre volte, solo finché il
  testo è ancora lì), poi di nuovo l'app di prima; se non va, ⌘A e canc tolgono il testo. Gli
  errori vanno nel log senza il testo. Nel database 11 righe su 14 avevano il nome di
  `ZPARTNERNAME` (le altre il nome della rubrica).
  Provato coi dati finti sulla barra di prova col notch vero (`examples/mock/whatsapp.json`,
  immagini disegnate da uno script; `msg trigger whatsapp_message`, `whatsapp_chat chat=…`):
  segno e notice nell'isola della musica, isola propria a riposo, peek, notice, popup con elenco
  e chat (bolle, link, video, documento, vocale), testo scritto con tasti mandati al solo
  processo (`CGEvent.postToPid`) e Invio: 13 MB a riposo, 19 con la chat aperta, 17 dopo, 0% CPU,
  0 processi. Dal vero sulla barra di prova il 23/11/2026, con l'utente: la richiesta di macOS
  "accedere ai dati di altre app" all'avvio (la lettura aspetta la risposta, widget in
  `loading`), dopo il consenso `ready` e isola, 13 MB, 0% CPU, 0 processi in 30 s; un invio alla
  chat con se stesso scritto nel campo della barra (tasti al solo processo) e arrivato nel
  database dell'app. WhatsApp si è aggiornata da sola a metà prova (26.46.21, stessi
  identificatori). Da provare con l'utente: un invio a una persona e a un gruppo (la riga
  nell'elenco), se il popup resta aperto dopo l'invio, `Segna come letta`, un vocale vero, i clic
  col mouse, un messaggio che arriva, la risposta "no" a macOS; manca lo screenshot del README.
- `whatsapp`, letto solo nella barra (l'utente non vedeva niente nel notch mentre l'app diceva
  una notifica: l'unica chat non letta era già stata aperta nel popup): una chat letta qui che
  WhatsApp conta ancora (`held`) resta nella barra attenuata, segno e numero di WhatsApp in grigio
  (`tally`), badge grigio nell'elenco in cima con le altre, isola propria attenuata, nessun
  notice; verde solo il nuovo. Ala destra dell'isola con tutto insieme (chiesto dall'utente):
  `island_marks` (fn, WhatsApp con un'onda mentre suona un vocale), i segni della chiamata anche
  quando a sinistra c'è la musica, e `music_sign` (barre o pausa nell'anello) anche quando a
  sinistra c'è la chiamata. Provato coi dati finti sulla barra di prova col notch: musica,
  chiamata Meet finta e WhatsApp insieme, verde poi grigio dopo aver aperto le chat. Da provare:
  l'onda del vocale (serve un clic), la larghezza dell'ala con tutto su uno schermo stretto.
- Le stesse cose senza notch (chiesto dall'utente): `Widget::notice` vale anche per una pill, che
  cresce da sola come col peek (`App::notice` sceglie lo schermo del mouse fra quelli dove il
  widget è isola o pill; `pill_tree` prende lo stadio: peek o notice; `peek_tree` lo riceve anche
  da chiusa, per la misura della finestra). Un `cx.notice()` con la barra da ridisegnare aspetta
  0,05 s: il primo messaggio non letto è anche ciò che mette `whatsapp` nella barra (prima, senza
  isola già sua, il notice si perdeva). `now_playing` in pill: i segni della chiamata nella pill
  della musica, barre o pausa in quella della chiamata; l'onda del vocale nella pill di
  `whatsapp`. I notice di `now_playing` (traccia nuova, chiamata) restano solo dell'isola.
  Provato coi dati finti sulla barra di prova con un ritocco temporaneo di `display.rs` che
  toglieva il notch (tolto): pill che cresce col messaggio, pill della chiamata con la musica.
  Da provare su uno schermo vero senza notch.
- Ancora da provare sul Mac vero, con l'utente: connessione e disconnessione Bluetooth, join di una
  Wi-Fi con password, slider del volume, cambio del dispositivo predefinito, "Aggiorna tutto" di
  Homebrew e riavvio di AeroSpace. I badge del Dock aspettano che l'utente dia Accessibilità a
  RustyBar.app: macOS la chiede da sé una volta sola, poi la chiedono il `!` giallo di
  `notifications` e la riga nel suo popup (provati sulla barra di prova, il clic no).
  Della M3: la riconnessione di `home` al risveglio, login e logout di `claude`. Di `fit.rs`: un
  clic dentro il popup della `⋯`.
- Notch vero (MacBook, 1800×1169): AppKit teneva la barra sotto la riga del notch anche col menu
  nascosto; il pannello ora rifiuta `constrainFrameRect` e la barra sta nella riga, a livello 20, quello del
  Dock (prima `setFloatingPanel` rimetteva ogni pannello al livello 3; a 23 copriva i banner delle
  notifiche, che col menu nascosto scendono nella riga e stanno a 21). Lati e forme compatte provati
  lì sulla barra vera; i banner sopra la barra provati con `display notification`.

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
