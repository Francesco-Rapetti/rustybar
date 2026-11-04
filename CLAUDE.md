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
- `rustybar msg query` (widget, stato, posizione, peek), `msg popup <widget>`, `msg peek <widget>`
  (la pill cresce come sotto il mouse; resta finché il mouse non passa sulla barra), `msg reload`,
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
  poi `claude` in una pill), poi cedono i widget più lontani dal loro bordo, prima compatti e poi
  nel popup di una pill `⋯`, con un margine prima di tornare grandi (lo usa `App::bar_tree`, che
  disegna una forma solo quando serve misurarla).
- `surface.rs`, `ui/` (`node.rs` albero dichiarativo, `layout.rs` flexbox, `render.rs` diff dei
  layer con animazioni implicite), `gfx.rs` (testo, SF Symbols e icone in bitmap, con cache a due
  generazioni ripulita dopo ogni render, e del tutto quando un popup si chiude, insieme alle pagine
  libere di malloc), `theme.rs`, `palette.rs` (i colori di un'immagine in Oklab e un tema fatto
  con quelli), `config.rs` (`rustybar.local.toml` unito sopra `rustybar.toml`),
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
  cima apre il popup, uscendo torna pill dopo 0,15 s. Niente bagliore dell'hover sulla pill né
  sulla cima cresciuta (l'ha chiesto l'utente): un `hover` trasparente vuol dire nessuno, nemmeno
  quello del tema; con `hover = false` torna quello solito. È una finestra sua (`SurfaceId::Peek`, livello dei popup) grande già da
  cresciuta: dentro una card con l'aspetto della pill (vetro, velo, bordo; velo del popup da
  cresciuta) che si allarga coi layer e ritaglia (`Style::clip`), il contenuto della pill fermo al
  suo posto (preso dal nodo che la barra ha disegnato, `Bar::tree`/`peek_pill`, anche allargato da
  `spare`). La pill nella barra intanto ha opacità 0, senza animazione (`App::instant`); al
  ritorno riappare sotto il peek e il peek va 0,04 s dopo (due vetri sovrapposti hanno il bordo più
  chiaro), poi `trim_cache` e `release_memory` come per i popup. Cresce dal lato del popup; il
  gruppo centrale accanto al notch verso lo spazio che ha trovato, non sulle pill del suo lato.
  Eventi `PeekOpened`/`PeekClosed` (Lua `peek_opened`/`peek_closed`), `Cx::peek_open`: il tick di
  1 s solo da cresciuta. Provato coi dati finti sulla barra di prova (Mocha col vetro, col notch,
  forma piena e compatta) con `msg peek`, a fotogrammi: cresce come un cassetto, torna identica alla
  pill. A riposo ~15–16 MB, ~0,07% CPU, 0 processi (anche dopo 20 crescite); cresciuta ~18 MB,
  0,2%. Una crescita e un ritorno ~160 ms di CPU (il popup ~64): il vetro misurato due volte da
  SwiftUI. Da provare con l'utente: il passaggio del mouse vero (crescita, uscita, clic sui tasti e
  sulla cima, trascinare l'avanzamento).
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
