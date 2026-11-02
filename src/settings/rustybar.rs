//! The pages of RustyBar: the bar and the screens, the theme, the layout and one page for each
//! widget, with where it sits in the bar and its options. A Lua widget, or a key no page knows,
//! gets a control for each value it has.

use std::cell::RefCell;
use std::rc::Rc;

use objc2_app_kit::NSFontManager;
use toml_edit::{InlineTable, Table};

use super::form::{self, Form, Num, Opt, hex, opts};
use super::store::{self, Spot, Value, rb};
use crate::config::Config;
use crate::gfx::Rgba;
use crate::i18n::t;
use crate::theme::{self, Theme};

/// The widgets of RustyBar, in the order of the sidebar.
const KINDS: &[&str] = &["apple", "aerospace", "front_app", "home", "system", "docker", "claude", "audio", "network", "battery", "events", "clock", "notifications"];

const SIDES: [&str; 3] = ["left", "center", "right"];

/// The name, SF Symbol and description of a kind of widget.
fn info(kind: &str) -> (&'static str, &'static str, &'static str) {
    match kind {
        "apple" => (t("Menu Apple", "Apple menu"), "apple.logo", t("Il logo Apple, con i comandi del menu Apple e questa finestra.", "The Apple logo, with the commands of the Apple menu and this window.")),
        "aerospace" => ("AeroSpace", "square.grid.2x2.fill", t("I workspace di AeroSpace con le icone delle loro finestre.", "AeroSpace's workspaces with the icons of their windows.")),
        "front_app" => (t("App attiva", "Front app"), "macwindow", t("Il nome dell'app in primo piano.", "The name of the app in front.")),
        "system" => (t("Sistema", "System"), "cpu", t("CPU, GPU e memoria, colorate dal carico.", "CPU, GPU and memory, colored by load.")),
        "battery" => (t("Batteria", "Battery"), "battery.75percent", t("La carica, con un'icona per il suo stato.", "The charge, with an icon for its state.")),
        "clock" => (t("Orologio", "Clock"), "clock.fill", t("Data e ora, con un calendario nel popup.", "Date and time, with a calendar in the popup.")),
        "audio" => ("Audio", "speaker.wave.2.fill", t("L'uscita e l'ingresso in uso, con il volume.", "The output and input in use, with their volume.")),
        "network" => (t("Rete", "Network"), "wifi", t("La VPN, la rete Wi-Fi o Ethernet.", "The VPN, the Wi-Fi network or Ethernet.")),
        "events" => (t("Eventi", "Events"), "calendar", t("Il prossimo evento di oggi dai calendari del Mac.", "Today's next event from the Mac's calendars.")),
        "notifications" => (t("Notifiche", "Notifications"), "bell.badge.fill", t("I badge del Dock delle app scelte, i crash di RustyBar e gli aggiornamenti di Homebrew.", "The Dock badges of the apps you choose, RustyBar's crashes and Homebrew updates.")),
        "home" => ("Home Assistant", "house.fill", t("I dispositivi di casa, in diretta, e una plancia per comandarli.", "The devices of the house, live, and a dashboard to control them.")),
        "claude" => ("Claude", "sparkle", t("Quanto dei limiti del piano ha usato ogni account Claude.", "How much of the plan's limits each Claude account has used.")),
        "docker" => ("Docker", "shippingbox.fill", t("I container in esecuzione, dall'Engine API.", "The running containers, from the Engine API.")),
        _ => ("", "puzzlepiece.extension.fill", t("Un widget scritto in Lua, in widgets/ accanto alla config.", "A widget written in Lua, in widgets/ next to the config.")),
    }
}

fn files() -> Vec<std::path::PathBuf> {
    [store::path(store::File::Bar), store::path(store::File::BarLocal)].into_iter().filter(|p| p.exists()).collect()
}

/// What the bar draws now, for the palette shown in the menus.
fn current_theme() -> Theme {
    Theme::from_config(&crate::config::load().map(|c| c.theme).unwrap_or_default())
}

/// The three sides of `[layout]`, as the bar reads them.
fn sides() -> [Vec<String>; 3] {
    let d = Config::default().layout;
    let get = |key: &str, d: Vec<String>| store::get_strings(&rb(&format!("layout.{key}"))).unwrap_or(d);
    [get("left", d.left), get("center", d.center), get("right", d.right)]
}

/// The kind of the widget called `name`: its `type`, or the name itself.
fn kind_of(name: &str) -> String {
    store::get_str(&rb("widgets").k(name).k("type")).unwrap_or_else(|| name.to_string())
}

/// The widgets for the sidebar: those in the bar first, in its order, then the others. Each with
/// its title, symbol and whether it's in the bar.
pub fn widget_entries(lua: &[String]) -> Vec<(String, String, &'static str, bool)> {
    let used: Vec<String> = sides().concat();
    let mut names = used.clone();
    for name in store::keys(&rb("widgets")).into_iter().chain(KINDS.iter().map(|k| k.to_string())).chain(lua.iter().cloned()) {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .map(|name| {
            let kind = kind_of(&name);
            let (title, symbol, _) = info(&kind);
            let title = if lua.contains(&name) || title.is_empty() {
                name.clone()
            } else if name != kind {
                format!("{title} ({name})")
            } else {
                title.to_string()
            };
            let used = used.contains(&name);
            (name, title, symbol, used)
        })
        .collect()
}

// ---- the bar -----------------------------------------------------------------------------------

pub fn bar(f: &mut Form) {
    let d = Config::default();
    f.header(t("Barra", "Bar"), t("La barra in cima a ogni schermo, e cosa fa quando lo spazio non basta.", "The bar at the top of every screen, and what it does when there isn't room."), &files());
    f.group(None);
    f.number_at(t("Altezza", "Height"), Some(t("Le pillole stanno al centro, in verticale", "The pills are centered in it")), &rb("bar.height"), d.bar.height, Num::new(16.0, 120.0, 1.0, 0));
    f.number_at(t("Margine", "Margin"), Some(t("Dai bordi dello schermo alla prima e all'ultima pillola", "From the screen edges to the first and last pill")), &rb("bar.margin"), d.bar.margin, Num::new(0.0, 1000.0, 1.0, 0));
    f.number_at(t("Distanza dei popup", "Popup gap"), Some(t("Fra le pillole e i loro popup", "Between the pills and their popups")), &rb("bar.popup_gap"), d.bar.popup_gap, Num::new(0.0, 60.0, 1.0, 0));
    f.number_at(t("Spostamento in basso", "Offset"), Some(t("Abbassa la barra, per esempio per provarla sotto un'altra", "Moves the bar down, e.g. to try it under another bar")), &rb("bar.offset"), d.bar.offset, Num::new(0.0, 1000.0, 1.0, 0));
    f.toggle_at(t("Sopra la barra dei menu", "Above the menu bar"), Some(t("Quando la barra dei menu di macOS scende, resta sotto la barra", "When the macOS menu bar slides in, it stays under the bar")), &rb("bar.topmost"), d.bar.topmost);

    f.group(Some(t("Schermi", "Displays")));
    f.toggle_at(t("Tutto compatto col notch", "All compact with a notch"), Some(t("Sugli schermi col notch ogni widget usa la forma compatta, anche se c'è spazio", "On screens with a notch every widget draws its compact form, even with room")), &rb("displays.compact_on_notch"), d.displays.compact_on_notch);
    f.tokens_at(t("Sempre compatti", "Always compact"), Some(t("Widget compatti su ogni schermo", "Widgets compact on every screen")), &rb("displays.always_compact"), t("nessuno", "none"));
    f.tokens_or_at(t("Snelliti per primi", "Slim first"), Some(t("Senza spazio, prima che un widget diventi compatto, in quest'ordine: audio senza gli slider del volume, claude in una pillola", "With no room, before any widget turns compact, in this order: audio without its volume sliders, claude in one pill")), &rb("displays.slim_first"), &d.displays.slim_first, t("nessuno", "none"));
    f.tokens_at(t("Nel ⋯ col notch", "In the ⋯ with a notch"), Some(t("Widget che sugli schermi col notch vanno nel popup della pillola ⋯, anche se c'è spazio", "Widgets that go in the popup of the ⋯ pill on screens with a notch, even with room")), &rb("displays.hide_on_notch"), t("nessuno", "none"));
    f.number_at(t("Schermo grande da", "Large screen from"), Some(t("Da questa larghezza in punti uno schermo è grande: i widget Lua possono mostrare di più (ctx.large)", "From this width in points a screen is large: Lua widgets can show more (ctx.large)")), &rb("displays.large_width"), d.displays.large_width, Num::new(800.0, 10000.0, 10.0, 0));
    f.footer(t(
        "Quando i due lati non ci stanno, prima si snelliscono i widget della lista, poi i widget più lontani dal bordo diventano compatti e passano nel popup di una pillola ⋯, finché non torna lo spazio.",
        "When the two sides don't fit, the widgets of the slim list go slim first, then the widgets farthest from their edge switch to their compact form and move to the popup of a ⋯ pill, until there's room again.",
    ));
}

// ---- the theme ---------------------------------------------------------------------------------

const ACCENTS: &[&str] = &["accent", "rosewater", "flamingo", "pink", "mauve", "red", "maroon", "peach", "yellow", "green", "teal", "sky", "sapphire", "blue", "lavender"];

pub fn theme(f: &mut Form) {
    let d = Config::default().theme;
    let theme = current_theme();
    f.header(t("Tema", "Theme"), t("I colori, i caratteri e le forme della barra e dei popup.", "The colors, fonts and shapes of the bar and the popups."), &files());

    f.group(Some(t("Colori", "Colors")));
    // Each with its own color for the current accent
    let primary = store::get_str(&rb("theme.primary")).unwrap_or_else(|| d.primary.clone());
    let presets = theme::presets()
        .map(|(name, label)| {
            let colors = theme::palette(name).unwrap_or_default();
            let color = |n: &str| colors.iter().find(|(k, _)| *k == n).map(|(_, c)| *c);
            Opt { value: name.into(), label: label.into(), swatch: color(&primary).or(color("accent")) }
        })
        .collect();
    f.choice_at(t("Palette", "Palette"), None, &rb("theme.preset"), presets, &d.preset);
    let accents = ACCENTS.iter().map(|n| Opt { value: n.to_string(), label: n.to_string(), swatch: theme.color(n) }).collect();
    f.choice_at(t("Colore principale", "Accent"), Some(t("Il workspace attivo, oggi, le azioni, i cursori e le barre", "The focused workspace, today, actions, sliders and bars")), &rb("theme.primary"), accents, &d.primary);

    f.group(Some(t("Testo", "Text")));
    let mut families: Vec<String> = NSFontManager::sharedFontManager(f.mtm).availableFontFamilies().iter().map(|s| s.to_string()).filter(|s| !s.starts_with('.')).collect();
    families.sort();
    // First the two whose weights the bar knows (see `gfx::FontSpec`)
    let mut fonts = vec![
        Opt { value: "Helvetica Neue".into(), label: "Helvetica Neue".into(), swatch: None },
        Opt { value: "System".into(), label: t("Sistema (SF Pro)", "System (SF Pro)").into(), swatch: None },
    ];
    fonts.extend(families.iter().filter(|n| !matches!(n.as_str(), "Helvetica Neue" | "SF Pro")).map(|n| Opt { value: n.clone(), label: n.clone(), swatch: None }));
    f.choice_at(t("Carattere", "Font"), Some(t("Grassetto e leggero solo con i primi due", "Bold and light only with the first two")), &rb("theme.font"), fonts, &d.font);
    f.number_at(t("Dimensione", "Size"), None, &rb("theme.font_size"), d.font_size, Num::new(6.0, 40.0, 1.0, 1));
    f.number_at(t("Dimensione piccola", "Small size"), Some(t("Dettagli e testi secondari", "Details and secondary text")), &rb("theme.small_size"), d.small_size, Num::new(6.0, 40.0, 1.0, 1));
    f.number_at(t("Dimensione delle icone", "Icon size"), None, &rb("theme.icon_size"), d.icon_size, Num::new(6.0, 40.0, 1.0, 1));

    f.group(Some(t("Pillole", "Pills")));
    f.number_at(t("Altezza", "Height"), None, &rb("theme.pill_height"), d.pill_height, Num::new(12.0, 100.0, 1.0, 0));
    f.number_at(t("Raggio degli angoli", "Corner radius"), None, &rb("theme.pill_radius"), d.pill_radius, Num::new(0.0, 50.0, 1.0, 0));
    f.number_at(t("Bordo", "Border"), Some(t("Non sul vetro", "Not on glass")), &rb("theme.pill_border"), d.pill_border, Num::new(0.0, 10.0, 0.5, 1));
    f.choice_at(
        t("Vetro", "Glass"),
        Some(t("Con la palette macOS", "With the macOS palette")),
        &rb("theme.pill_glass"),
        opts(&[("clear", t("Trasparente, come il Dock", "Clear, as the Dock")), ("regular", t("Smerigliato, come i menu", "Frosted, as the menus"))]),
        &d.pill_glass,
    );
    f.number_at(t("Spazio interno", "Padding"), None, &rb("theme.pill_padding"), d.pill_padding, Num::new(0.0, 40.0, 1.0, 0));
    f.number_at(t("Spazio fra le pillole", "Gap between pills"), None, &rb("theme.item_gap"), d.item_gap, Num::new(0.0, 40.0, 1.0, 0));

    f.group(Some("Popup"));
    f.number_at(t("Raggio degli angoli", "Corner radius"), None, &rb("theme.popup_radius"), d.popup_radius, Num::new(0.0, 50.0, 1.0, 0));
    f.number_at(t("Bordo", "Border"), None, &rb("theme.popup_border"), d.popup_border, Num::new(0.0, 10.0, 0.5, 1));
    f.number_at(t("Opacità", "Opacity"), Some(t("Senza vetro", "Without glass")), &rb("theme.popup_opacity"), d.popup_opacity, Num::new(0.2, 1.0, 0.05, 2));
    f.toggle_at(t("Sfocatura dello sfondo", "Background blur"), Some(t("Senza vetro", "Without glass")), &rb("theme.popup_blur"), d.popup_blur);
    f.number_at(t("Raggio delle righe", "Row radius"), Some(t("L'evidenziazione delle righe al passaggio del mouse", "The highlight of rows under the mouse")), &rb("theme.row_radius"), d.row_radius, Num::new(0.0, 20.0, 1.0, 0));
    f.toggle_at(t("Animazioni", "Animations"), Some(t("Si fermano anche con Riduci movimento nelle Impostazioni di Sistema", "They also stop with Reduce Motion in System Settings")), &rb("theme.animations"), d.animations);

    f.group(Some(t("Colori della palette", "Palette colors")));
    let table = rb("theme.colors");
    for name in store::keys(&table) {
        let value = store::get_str(&table.k(&name)).unwrap_or_default();
        let (t1, old) = (table.clone(), name.clone());
        let name_field = f.field(&name, t("nome", "name"), 160.0, move |new| {
            let new = new.trim().to_string();
            if new.is_empty() || !store::rename(&t1, &old, &new) {
                super::status(t("C'è già un colore con questo nome", "There's already a color with this name"), true);
            }
            super::refresh_later();
        });
        let s = table.k(&name);
        let well = f.color_well(theme.color(&value), move |c| store::set(&s, Value::from(hex(c))));
        let value_label = form::label(f.mtm, &value, 11.0, false, true);
        let s = table.k(&name);
        let menu = f.menu_button(vec![(
            t("Elimina", "Delete").into(),
            Box::new(move || {
                store::remove(&s);
                super::refresh_later();
            }),
        )]);
        f.line(&[&name_field, &well, &value_label], Some(&menu));
    }
    let t2 = table.clone();
    let add = f.button(t("Aggiungi un colore", "Add a color"), move || {
        let taken = store::keys(&t2);
        let name = (1..).map(|i| format!("color{i}")).find(|n| !taken.contains(n)).expect("a free name");
        store::set(&t2.k(&name), Value::from("#cba6f7"));
        super::refresh_later();
    });
    f.buttons(&[&add]);
    f.footer(t(
        "Un nome della palette (base, text, surface0, mauve…) cambia quel colore ovunque; un nome nuovo si può usare nelle opzioni dei widget e in Lua.",
        "A palette name (base, text, surface0, mauve…) changes that color everywhere; a new name can be used in widget options and in Lua.",
    ));
}

// ---- the layout --------------------------------------------------------------------------------

pub fn layout(f: &mut Form, lua: &[String]) {
    f.header(t("Disposizione", "Layout"), t("Quali widget ci sono nella barra, e in che ordine.", "Which widgets are in the bar, and in what order."), &files());
    let lists = sides();
    f.group(None);
    let titles = [t("A sinistra", "Left"), t("Al centro", "Center"), t("A destra", "Right")];
    for (i, key) in SIDES.iter().enumerate() {
        let s = rb(&format!("layout.{key}"));
        let help = (i == 1).then(|| t("Sugli schermi col notch il centro resta libero", "On screens with a notch the center stays free"));
        f.tokens(titles[i], help, &lists[i], t("nessuno", "none"), move |v| {
            store::set(&s, store::strings(&v));
            super::refresh_sidebar_later();
        });
    }
    f.footer(t(
        "Trascina i widget per riordinarli o spostarli da un lato all'altro; scrivi un nome, seguito da una virgola, per aggiungerlo.",
        "Drag the widgets to reorder them or move them to another side; type a name, followed by a comma, to add it.",
    ));

    let used = lists.concat();
    let mut free: Vec<String> = Vec::new();
    for n in KINDS.iter().map(|k| k.to_string()).chain(lua.iter().cloned()).chain(store::keys(&rb("widgets"))) {
        if !used.contains(&n) && !free.contains(&n) {
            free.push(n);
        }
    }
    f.group(Some(t("Fuori dalla barra", "Not in the bar")));
    f.note(&if free.is_empty() { t("Tutti i widget sono nella barra.", "Every widget is in the bar.").to_string() } else { free.join(", ") });

    f.group(Some(t("Un altro widget dello stesso tipo", "Another widget of the same kind")));
    let name_field = f.field("", t("nome, es. clock_utc", "name, e.g. clock_utc"), 170.0, |_| {});
    let kind = Rc::new(RefCell::new(KINDS[0].to_string()));
    let k = kind.clone();
    let kinds: Vec<Opt> = KINDS.iter().map(|k| Opt { value: k.to_string(), label: info(k).0.to_string(), swatch: None }).collect();
    let popup = f.popup(kinds, KINDS[0], move |v| *k.borrow_mut() = v);
    let nf = name_field.clone();
    let add = f.button(t("Aggiungi a destra", "Add on the right"), move || {
        let name: String = nf.stringValue().to_string().trim().chars().map(|c| if c.is_alphanumeric() || c == '_' { c } else { '_' }).collect();
        if name.is_empty() || sides().concat().contains(&name) || KINDS.contains(&name.as_str()) {
            super::status(t("Scegli un nome che nessun widget ha già", "Pick a name no widget has yet"), true);
            return;
        }
        store::set(&rb("widgets").k(&name).k("type"), Value::from(kind.borrow().as_str()));
        let mut right = sides()[2].clone();
        right.insert(0, name.clone());
        store::set(&rb("layout.right"), store::strings(&right));
        super::select(&format!("widget:{name}"));
    });
    f.line(&[&name_field, &popup], Some(&add));
    f.footer(t(
        "Per esempio un secondo orologio con un altro fuso orario: ha una pagina sua, con le sue opzioni.",
        "For example a second clock with another time zone: it gets a page of its own, with its own options.",
    ));
}

// ---- a widget ----------------------------------------------------------------------------------

pub fn widget(f: &mut Form, name: &str, lua: &[String]) {
    let spot = rb("widgets").k(name);
    let kind = kind_of(name);
    let is_lua = lua.iter().any(|n| n == name);
    let (title, _, about) = info(if is_lua { "" } else { &kind });
    let title = if is_lua || title.is_empty() { name.to_string() } else if name != kind { format!("{title} ({name})") } else { title.to_string() };
    f.header(&title, about, &files());

    let lists = sides();
    let side = SIDES.iter().zip(&lists).find(|(_, l)| l.iter().any(|n| n == name)).map(|(s, _)| *s).unwrap_or("");
    f.group(Some(t("Nella barra", "In the bar")));
    let n = name.to_string();
    f.choice(
        t("Posizione", "Position"),
        None,
        opts(&[("left", t("A sinistra", "Left")), ("center", t("Al centro", "Center")), ("right", t("A destra", "Right")), ("", t("Non nella barra", "Not in the bar"))]),
        side,
        move |v| move_to(&n, &v),
    );
    member(f, t("Sempre compatto", "Always compact"), Some(t("La forma corta su ogni schermo, anche se c'è spazio", "The short form on every screen, even with room")), &rb("displays.always_compact"), name);
    member(f, t("Nel ⋯ col notch", "In the ⋯ with a notch"), Some(t("Sugli schermi col notch va nel popup della pillola ⋯", "On screens with a notch it goes in the popup of the ⋯ pill")), &rb("displays.hide_on_notch"), name);

    let known: &[&str] = match kind.as_str() {
        _ if is_lua => &[],
        "apple" => {
            f.group(Some(t("Opzioni", "Options")));
            f.text_at(t("Simbolo", "Symbol"), Some(t("Un SF Symbol al posto della mela", "An SF Symbol in place of the apple")), &spot.k("icon"), "apple.logo");
            &["icon"]
        }
        "aerospace" => {
            f.group(Some(t("Opzioni", "Options")));
            f.tokens_at(t("Workspace", "Workspaces"), Some(t("Una lista fissa; vuota, quelli che AeroSpace conosce", "A fixed list; empty, the ones AeroSpace knows")), &spot.k("workspaces"), t("quelli di AeroSpace", "AeroSpace's own"));
            f.toggle_at(t("Nascondi quelli vuoti", "Hide the empty ones"), Some(t("Tranne quello attivo", "Except the focused one")), &spot.k("hide_empty"), true);
            &["workspaces", "hide_empty"]
        }
        "clock" => {
            f.group(Some(t("Opzioni", "Options")));
            f.text_at(t("Data", "Date"), Some(t("Uno schema Unicode, come EEE dd/MM; l'ora segue la regione del Mac", "A Unicode pattern, such as EEE dd/MM; the time follows the region of the Mac")), &spot.k("date"), "EEE dd/MM");
            f.text_at(t("Fuso orario", "Time zone"), Some(t("Un nome come UTC o America/New_York; vuoto, quello del Mac", "A name such as UTC or America/New_York; empty, the Mac's")), &spot.k("timezone"), t("quello del Mac", "the Mac's"));
            &["date", "timezone"]
        }
        "audio" => {
            f.group(Some(t("Opzioni", "Options")));
            f.toggle_at(t("Volume nella barra", "Volume in the bar"), Some(t("Un cursore con il volume dell'uscita e dell'ingresso", "A slider with the volume of the output and of the input")), &spot.k("sliders"), true);
            &["sliders"]
        }
        "system" => {
            f.group(Some(t("Opzioni", "Options")));
            f.number_at(t("Gialla da", "Yellow from"), Some(t("CPU e GPU, in percentuale", "CPU and GPU, in percent")), &spot.k("warning"), 75.0, Num::new(0.0, 100.0, 1.0, 0));
            f.number_at(t("Rossa da", "Red from"), None, &spot.k("critical"), 90.0, Num::new(0.0, 100.0, 1.0, 0));
            &["warning", "critical"]
        }
        "events" => {
            f.group(Some(t("Opzioni", "Options")));
            let mins = Num::new(0.0, 600.0, 1.0, 0);
            f.number_at(t("Gialla da", "Yellow from"), Some(t("Minuti prima dell'evento", "Minutes before the event")), &spot.k("soon"), 30.0, mins);
            f.number_at(t("Arancione da", "Peach from"), None, &spot.k("sooner"), 15.0, mins);
            f.number_at(t("Rossa da", "Red from"), None, &spot.k("imminent"), 5.0, mins);
            &["soon", "sooner", "imminent"]
        }
        "notifications" => {
            notifications(f, &spot);
            &["apps", "brewfile", "brew"]
        }
        "home" => {
            home(f, &spot);
            &["server", "dashboard", "watch", "groups", "printer"]
        }
        "claude" => {
            claude(f, &spot);
            &["color", "warning", "critical", "accounts"]
        }
        "front_app" | "battery" | "network" | "docker" => {
            f.group(Some(t("Opzioni", "Options")));
            f.note(t("Questo widget non ha opzioni.", "This widget has no options."));
            &[]
        }
        _ => &[],
    };
    others(f, &spot, known, is_lua || !KINDS.contains(&kind.as_str()));
}

/// Moves `name` to the end of `side` ("" takes it out of the bar).
fn move_to(name: &str, side: &str) {
    let lists = sides();
    for (key, list) in SIDES.iter().zip(lists) {
        let mut new: Vec<String> = list.iter().filter(|n| *n != name).cloned().collect();
        if *key == side {
            new.push(name.to_string());
        }
        if new != list {
            store::set(&rb(&format!("layout.{key}")), store::strings(&new));
        }
    }
    super::refresh_sidebar_later();
}

/// A switch for whether `name` is in the list at `spot`.
fn member(f: &mut Form, title: &str, help: Option<&str>, spot: &Spot, name: &str) {
    let on = store::get_strings(spot).unwrap_or_default().iter().any(|n| n == name);
    let (s, n) = (spot.clone(), name.to_string());
    f.toggle(title, help, on, move |v| {
        let mut list: Vec<String> = store::get_strings(&s).unwrap_or_default().into_iter().filter(|x| *x != n).collect();
        if v {
            list.push(n.clone());
        }
        store::put(&s, store::strings(&list), Some(&toml::Value::Array(vec![])));
    });
}

/// `~/…` for a path in the home folder.
fn tilde(path: &std::path::Path) -> String {
    let home = crate::config::home();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

fn notifications(f: &mut Form, spot: &Spot) {
    f.group(Some(t("Badge del Dock", "Dock badges")));
    f.tokens_at(t("App", "Apps"), Some(t("Bundle id o nomi del Dock, come com.tinyspeck.slackmacgap o Mail; servono i permessi di Accessibilità", "Bundle ids or Dock names, such as com.tinyspeck.slackmacgap or Mail; it needs Accessibility")), &spot.k("apps"), t("nessuna", "none"));
    let s = spot.k("apps");
    let pick = f.button(t("Aggiungi un'app…", "Add an app…"), move || {
        if let Some(id) = form::choose_file(Some(std::path::Path::new("/Applications")), true).and_then(|p| form::bundle_id(&p)) {
            let mut list = store::get_strings(&s).unwrap_or_default();
            if !list.contains(&id) {
                list.push(id);
                store::set(&s, store::strings(&list));
            }
            super::refresh_later();
        }
    });
    f.buttons(&[&pick]);

    f.group(Some("Homebrew"));
    let current = store::get_str(&spot.k("brewfile")).unwrap_or_default();
    let s = spot.k("brewfile");
    let field = f.field(&current, "~/Brewfile", 220.0, move |v| {
        let v = v.trim();
        if v.is_empty() { store::remove(&s) } else { store::set(&s, Value::from(v)) }
        super::refresh_sidebar_later();
    });
    let s = spot.k("brewfile");
    let choose = f.button(t("Scegli…", "Choose…"), move || {
        if let Some(p) = form::choose_file(Some(&crate::config::home()), false) {
            store::set(&s, Value::from(tilde(&p)));
            super::refresh_later();
        }
    });
    let row = form::stack(f.mtm, false, 6.0);
    row.addArrangedSubview(&field);
    row.addArrangedSubview(&choose);
    f.row(t("Brewfile", "Brewfile"), Some(t("I pacchetti di cui cercare gli aggiornamenti, ogni ora", "The packages to check for updates, every hour")), &row);
    f.toggle_at(t("Tutti i pacchetti", "Every package"), Some(t("Senza Brewfile: tutto ciò che è installato", "Without a Brewfile: everything installed")), &spot.k("brew"), false);
}

/// The text of a key of an entity of Home Assistant, which is either a string (its id) or a table.
fn entity_key(v: &toml::Value, key: &str) -> String {
    match v {
        toml::Value::String(s) if key == "id" => s.clone(),
        toml::Value::Table(t) => t.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// Writes `key` of the entity at `spot`: a string becomes a table when it gets a name or an icon,
/// and a table with only its id goes back to a string.
fn set_entity_key(spot: &Spot, key: &str, value: &str) {
    let (key, value) = (key.to_string(), value.trim().to_string());
    store::update(spot, move |v| {
        if let Some(id) = v.as_str().map(String::from) {
            let mut t = InlineTable::new();
            t.insert("id", Value::from(id));
            let decor = v.decor().clone();
            *v = Value::InlineTable(t);
            *v.decor_mut() = decor;
        }
        if let Value::InlineTable(t) = v {
            if value.is_empty() {
                t.remove(&key);
                // Or the spaces around the keys that were there go wrong
                t.fmt();
            } else if let Some(old) = t.get_mut(&key) {
                let decor = old.decor().clone();
                *old = Value::from(value.as_str());
                *old.decor_mut() = decor;
            } else {
                t.insert(&key, Value::from(value.as_str()));
                t.fmt();
            }
            if t.len() == 1
                && let Some(id) = t.get("id").and_then(|i| i.as_str()).map(String::from)
            {
                let decor = v.decor().clone();
                *v = Value::from(id);
                *v.decor_mut() = decor;
            }
        }
    });
}

/// Commands to move or delete element `i` of the list at `spot`, of `n`.
pub(super) fn list_menu(spot: &Spot, i: usize, n: usize) -> Vec<(String, Box<dyn Fn()>)> {
    let mut items: Vec<(String, Box<dyn Fn()>)> = Vec::new();
    if i > 0 {
        let s = spot.clone();
        items.push((t("Sposta su", "Move up").into(), Box::new(move || {
            store::swap(&s, i, i - 1);
            super::refresh_later();
        })));
    }
    if i + 1 < n {
        let s = spot.clone();
        items.push((t("Sposta giù", "Move down").into(), Box::new(move || {
            store::swap(&s, i, i + 1);
            super::refresh_later();
        })));
    }
    let s = spot.i(i);
    items.push((t("Elimina", "Delete").into(), Box::new(move || {
        store::remove(&s);
        super::refresh_later();
    })));
    items
}

fn home(f: &mut Form, spot: &Spot) {
    f.group(Some(t("Server", "Server")));
    f.text_at(t("Indirizzo", "Address"), Some(t("Il server di Home Assistant", "The Home Assistant server")), &spot.k("server"), "http://homeassistant.local:8123");
    f.text_at(t("Plancia", "Dashboard"), Some(t("Cosa apre «Apri Home Assistant»: un percorso sul server o un URL", "What \"Open Home Assistant\" opens: a path on the server or a URL")), &spot.k("dashboard"), "/");
    let token = f.button(t("Imposta il token…", "Set the token…"), ask_token);
    f.row(t("Token", "Token"), Some(t("Un token di accesso a lungo termine, nel Portachiavi come rustybar-home-assistant", "A long-lived access token, in the keychain as rustybar-home-assistant")), &token);
    f.tokens_at(t("Entità per Lua", "Entities for Lua"), Some(t("Quelle che i widget Lua leggono con rb.home.state, oltre ai riquadri", "The ones Lua widgets read with rb.home.state, besides the tiles")), &spot.k("watch"), t("nessuna", "none"));

    let groups = spot.k("groups");
    let count = store::len(&groups);
    for g in 0..count {
        let group = groups.i(g);
        let title = store::get_str(&group.k("title")).unwrap_or_default();
        f.group(Some(&if title.is_empty() { format!("{} {}", t("Gruppo", "Group"), g + 1) } else { title.clone() }));
        let s = group.k("title");
        let title_field = f.field(&title, t("titolo del gruppo", "group title"), 220.0, move |v| {
            store::set(&s, Value::from(v.trim()));
            super::refresh_later();
        });
        let menu = f.menu_button(list_menu(&groups, g, count));
        f.line(&[&form::label(f.mtm, t("Titolo", "Title"), 13.0, false, false), &title_field], Some(&menu));
        let entities = group.k("entities");
        let list = store::get(&entities).and_then(|v| v.as_array().cloned()).unwrap_or_default();
        for (e, v) in list.iter().enumerate() {
            let fields: Vec<_> = [("id", "light.living_room", 190.0), ("name", t("nome", "name"), 150.0), ("icon", t("icona", "icon"), 80.0)]
                .into_iter()
                .map(|(key, placeholder, width)| {
                    let s = entities.i(e);
                    f.field(&entity_key(v, key), placeholder, width, move |value| {
                        if key == "id" && value.trim().is_empty() {
                            return;
                        }
                        set_entity_key(&s, key, &value);
                    })
                })
                .collect();
            let menu = f.menu_button(list_menu(&entities, e, list.len()));
            f.line(&[&fields[0], &fields[1], &fields[2]], Some(&menu));
        }
        let s = entities.clone();
        let add = f.button(t("Aggiungi un'entità", "Add an entity"), move || {
            store::push(&s, Value::from("light.new"));
            super::refresh_later();
        });
        f.buttons(&[&add]);
    }
    let s = groups.clone();
    let add = f.button(t("Aggiungi un gruppo", "Add a group"), move || {
        let mut t = Table::new();
        t.insert("title", toml_edit::value(crate::i18n::t("Nuovo gruppo", "New group")));
        t.insert("entities", toml_edit::value(toml_edit::Array::new()));
        store::push_table(&s, t);
        super::refresh_later();
    });
    f.group(None);
    f.buttons(&[&add]);
    f.footer(t(
        "Ogni gruppo è un titolo con i suoi riquadri. L'icona è un'icona di Home Assistant (lightbulb, fan…); vuota, quella del tipo di entità.",
        "Each group is a title with its tiles. The icon is a Home Assistant icon (lightbulb, fan…); empty, the one of the kind of entity.",
    ));

    f.group(Some(t("Stampante 3D", "3D printer")));
    let printer = spot.k("printer");
    f.text_at(t("Presa", "Plug"), None, &printer.k("plug"), "switch.printer");
    f.text_at(t("Spegnimento", "Turn off"), Some(t("Uno script che non fa niente mentre stampa", "A script that does nothing while it prints")), &printer.k("off"), "script.printer_off");
    f.text_at(t("Stato", "Status"), None, &printer.k("status"), "sensor.printer_status");
    f.text_at(t("Avanzamento", "Progress"), Some("%"), &printer.k("progress"), "sensor.printer_progress");
    f.text_at(t("Tempo rimanente", "Time left"), Some(t("In ore", "In hours")), &printer.k("remaining"), "sensor.printer_remaining");
}

/// Asks for the Home Assistant token and saves it in the keychain, where the widget reads it.
fn ask_token() {
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSSecureTextField};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(t("Token di Home Assistant", "Home Assistant token")));
    alert.setInformativeText(&NSString::from_str(t(
        "Un token di accesso a lungo termine, dal tuo profilo di Home Assistant → Sicurezza. Si salva nel Portachiavi.",
        "A long-lived access token, from your Home Assistant profile → Security. It's saved in the keychain.",
    )));
    alert.addButtonWithTitle(&NSString::from_str(t("Salva", "Save")));
    alert.addButtonWithTitle(&NSString::from_str(t("Annulla", "Cancel")));
    let field = NSSecureTextField::initWithFrame(<NSSecureTextField as objc2::MainThreadOnly>::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 24.0)));
    alert.setAccessoryView(Some(&field));
    alert.window().setInitialFirstResponder(Some(&field));
    if alert.runModal() != NSAlertFirstButtonReturn {
        return;
    }
    let token = field.stringValue().to_string().trim().to_string();
    if token.is_empty() || store::mock_dir().is_some() {
        return;
    }
    crate::runtime::spawn("settings-token", move || {
        let user = std::env::var("USER").unwrap_or_else(|_| "rustybar".into());
        let ok = crate::secret::set("rustybar-home-assistant", &user, &token);
        if ok {
            // The bar's widget connects again with it
            let _ = crate::ipc::send(&["trigger".into(), "home_assistant_token".into()]);
        }
        crate::runtime::on_main(move || {
            if ok {
                super::status(t("Token salvato nel Portachiavi", "Token saved in the keychain"), false);
            } else {
                super::status(t("Il Portachiavi non ha salvato il token", "The keychain didn't save the token"), true);
            }
        });
    });
}

fn claude(f: &mut Form, spot: &Spot) {
    let theme = current_theme();
    f.group(Some(t("Opzioni", "Options")));
    let color = store::get_str(&spot.k("color")).unwrap_or_else(|| "#d97757".into());
    let s = spot.k("color");
    let well = f.color_well(theme.color(&color), move |c| store::put(&s, Value::from(hex(c)), Some(&toml::Value::String("#d97757".into()))));
    f.row(t("Colore di ~/.claude", "Color of ~/.claude"), Some(t("L'account di Claude Code predefinito", "The default Claude Code account")), &well);
    f.number_at(t("Gialla da", "Yellow from"), Some(t("In percentuale del limite", "In percent of the limit")), &spot.k("warning"), 75.0, Num::new(0.0, 100.0, 1.0, 0));
    f.number_at(t("Rossa da", "Red from"), None, &spot.k("critical"), 90.0, Num::new(0.0, 100.0, 1.0, 0));

    f.group(Some(t("Altri account", "More accounts")));
    let accounts = spot.k("accounts");
    let list = store::get(&accounts).and_then(|v| v.as_array().cloned()).unwrap_or_default();
    for (i, a) in list.iter().enumerate() {
        let dir = a.get("dir").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let color = a.get("color").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let s = accounts.i(i).k("dir");
        let field = f.field(&dir, "~/.claude-work", 280.0, move |v| {
            if !v.trim().is_empty() {
                store::set(&s, Value::from(v.trim()));
            }
        });
        let s = accounts.i(i).k("color");
        let well = f.color_well(theme.color(&color), move |c| store::set(&s, Value::from(hex(c))));
        let menu = f.menu_button(list_menu(&accounts, i, list.len()));
        f.line(&[&field, &well], Some(&menu));
    }
    if list.is_empty() {
        f.note(t("Solo l'account di ~/.claude.", "Only the account of ~/.claude."));
    }
    let s = accounts.clone();
    let add = f.button(t("Aggiungi un account", "Add an account"), move || {
        // A folder no account uses yet, to be renamed
        let used: Vec<String> = (0..store::len(&s)).filter_map(|i| store::get_str(&s.i(i).k("dir"))).collect();
        let dir = (2..).map(|n| format!("~/.claude-{n}")).find(|d| !used.contains(d)).unwrap_or_default();
        let mut t = InlineTable::new();
        t.insert("dir", Value::from(dir));
        t.insert("color", Value::from("sky"));
        store::push(&s, Value::InlineTable(t));
        super::refresh_later();
    });
    f.buttons(&[&add]);
    f.footer(t(
        "La cartella di config di Claude Code di ogni account (quella di CLAUDE_CONFIG_DIR=<cartella> claude), e il suo colore.",
        "The Claude Code config folder of each account (the one of CLAUDE_CONFIG_DIR=<folder> claude), and its color.",
    ));
}

/// A control for each key of the widget that no page knows, by the kind of its value; for a Lua
/// widget, also a way to add one.
fn others(f: &mut Form, spot: &Spot, known: &[&str], can_add: bool) {
    let keys: Vec<String> = store::keys(spot).into_iter().filter(|k| k != "type" && !known.contains(&k.as_str())).collect();
    if keys.is_empty() && !can_add {
        return;
    }
    f.group(Some(if known.is_empty() { t("Opzioni", "Options") } else { t("Altre opzioni", "More options") }));
    for k in &keys {
        let s = spot.k(k);
        match store::get(&s) {
            Some(toml::Value::Boolean(b)) => f.toggle(k, None, b, move |v| store::set(&s, Value::from(v))),
            Some(toml::Value::Integer(i)) => f.number(k, None, i as f64, Num::new(-1e6, 1e6, 1.0, 0), move |v| store::set(&s, store::number(v, 0))),
            Some(toml::Value::Float(x)) => f.number(k, None, x, Num::new(-1e6, 1e6, 0.1, 3), move |v| store::set(&s, store::number(v, 3))),
            Some(toml::Value::String(v)) => match Rgba::parse(&v).filter(|_| v.starts_with('#')) {
                Some(c) => {
                    let well = f.color_well(Some(c), move |c| store::set(&s, Value::from(hex(c))));
                    f.row(k, Some(&v), &well);
                }
                None => f.text(k, None, &v, "", move |v| store::set(&s, Value::from(v))),
            },
            Some(toml::Value::Array(a)) if a.iter().all(|v| v.is_str()) => {
                let list: Vec<String> = a.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                f.tokens(k, None, &list, "", move |v| store::set(&s, store::strings(&v)));
            }
            Some(other) => {
                let mut text = other.to_string();
                if text.len() > 300 {
                    text.truncate(300);
                    text.push('…');
                }
                f.row(k, Some(&text), &form::label(f.mtm, t("dal file", "in the file"), 11.0, false, true));
            }
            None => {}
        }
    }
    if keys.is_empty() {
        f.note(t("Nessuna opzione nella config.", "No options in the config."));
    }
    if can_add {
        let name = f.field("", t("nome", "name"), 160.0, |_| {});
        let kind = Rc::new(RefCell::new("text".to_string()));
        let k = kind.clone();
        let popup = f.popup(opts(&[("text", t("Testo", "Text")), ("number", t("Numero", "Number")), ("bool", t("Sì o no", "Yes or no")), ("list", t("Lista", "List"))]), "text", move |v| *k.borrow_mut() = v);
        let (s, nf) = (spot.clone(), name.clone());
        let add = f.button(t("Aggiungi", "Add"), move || {
            let key = nf.stringValue().to_string().trim().to_string();
            if key.is_empty() {
                return;
            }
            let v = match kind.borrow().as_str() {
                "number" => Value::from(0),
                "bool" => Value::from(false),
                "list" => store::strings::<&str>(&[]),
                _ => Value::from(""),
            };
            store::set(&s.k(&key), v);
            super::refresh_later();
        });
        f.line(&[&name, &popup], Some(&add));
    }
}
