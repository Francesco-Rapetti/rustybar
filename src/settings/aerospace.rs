//! The pages of AeroSpace (general, gaps, bindings, monitors, window rules) and of the borders of
//! JankyBorders, whose options are the `borders …` command that AeroSpace runs at startup. Each
//! change asks AeroSpace to reload its config (see `apply` in the module above); a binding or a
//! rule AeroSpace doesn't understand shows its error under the page.

use std::cell::RefCell;
use std::rc::Rc;

use toml_edit::Table;

use super::form::{self, Form, Num, opts};
use super::rustybar::list_menu;
use super::store::{self, File, Spot, Value, aero};
use crate::gfx::Rgba;
use crate::i18n::t;

fn files() -> Vec<std::path::PathBuf> {
    vec![store::path(File::AeroSpace)]
}

/// A list of AeroSpace commands, which the config may also write as a single string.
fn commands(f: &mut Form, title: &str, help: Option<&str>, spot: &Spot) {
    let list = store::get_strings(spot).unwrap_or_default();
    f.group(Some(title));
    if let Some(h) = help {
        f.note(h);
    }
    let is_array = matches!(store::get(spot), Some(toml::Value::Array(_)));
    for (i, cmd) in list.iter().enumerate() {
        let s = if is_array { spot.i(i) } else { spot.clone() };
        let field = f.field(cmd, "exec-and-forget …", form::INNER - 40.0, move |v| {
            if !v.trim().is_empty() {
                store::set(&s, Value::from(v.trim()));
            }
        });
        let menu = if is_array {
            f.menu_button(list_menu(spot, i, list.len()))
        } else {
            let s = spot.clone();
            f.menu_button(vec![(t("Elimina", "Delete").into(), Box::new(move || {
                store::set(&s, store::strings::<&str>(&[]));
                super::refresh_later();
            }))])
        };
        f.line(&[&field], Some(&menu));
    }
    if list.is_empty() {
        f.note(t("Nessun comando.", "No commands."));
    }
    let s = spot.clone();
    let add = f.button(t("Aggiungi un comando", "Add a command"), move || {
        let mut list = store::get_strings(&s).unwrap_or_default();
        list.push("exec-and-forget ".into());
        store::set(&s, store::strings(&list));
        super::refresh_later();
    });
    f.buttons(&[&add]);
}

pub fn general(f: &mut Form) {
    f.header(
        "AeroSpace",
        t("Il tiling delle finestre: avvio, layout e comportamento. Ogni modifica ricarica la config di AeroSpace.", "The tiling of windows: startup, layout and behavior. Each change reloads AeroSpace's config."),
        &files(),
    );
    f.group(Some(t("Avvio", "Startup")));
    f.toggle_at(t("Apri al login", "Start at login"), None, &aero("start-at-login"), false);
    f.tokens_at(t("Workspace sempre presenti", "Persistent workspaces"), Some(t("Restano anche quando sono vuoti", "They stay even when empty")), &aero("persistent-workspaces"), t("nessuno", "none"));

    f.group(Some("Layout"));
    f.choice_at(t("Layout predefinito", "Default layout"), None, &aero("default-root-container-layout"), opts(&[("tiles", t("Affiancate", "Tiles")), ("accordion", t("A fisarmonica", "Accordion"))]), "tiles");
    f.choice_at(
        t("Orientamento predefinito", "Default orientation"),
        Some(t("Automatico: orizzontale sugli schermi larghi, verticale su quelli alti", "Auto: horizontal on wide screens, vertical on tall ones")),
        &aero("default-root-container-orientation"),
        opts(&[("auto", t("Automatico", "Auto")), ("horizontal", t("Orizzontale", "Horizontal")), ("vertical", t("Verticale", "Vertical"))]),
        "auto",
    );
    f.number_at(t("Margine della fisarmonica", "Accordion padding"), Some(t("0 per toglierlo", "0 turns it off")), &aero("accordion-padding"), 30.0, Num::new(0.0, 300.0, 1.0, 0));
    f.toggle_at(t("Appiattisci i contenitori", "Flatten containers"), Some(t("Un contenitore con una sola finestra sparisce", "A container with a single window goes away")), &aero("enable-normalization-flatten-containers"), true);
    f.toggle_at(t("Orientamento alternato", "Opposite orientation"), Some(t("I contenitori annidati hanno l'orientamento opposto al loro genitore", "Nested containers get the orientation opposite to their parent")), &aero("enable-normalization-opposite-orientation-for-nested-containers"), true);

    f.group(Some(t("Comportamento", "Behavior")));
    f.toggle_at(t("Mostra le app nascoste", "Unhide hidden apps"), Some(t("Annulla il ⌘H di macOS", "Undoes macOS's ⌘H")), &aero("automatically-unhide-macos-hidden-apps"), false);
    f.choice_at(t("Tastiera", "Keyboard"), Some(t("La disposizione dei tasti delle scorciatoie", "The key layout of the bindings")), &aero("key-mapping.preset"), opts(&[("qwerty", "QWERTY"), ("dvorak", "Dvorak"), ("colemak", "Colemak")]), "qwerty");

    commands(f, t("All'avvio", "After startup"), Some(t("Comandi che AeroSpace esegue quando parte. Il comando di borders si modifica nella pagina Bordi.", "Commands AeroSpace runs when it starts. The borders command is edited in the Borders page.")), &aero("after-startup-command"));
    commands(f, t("Quando cambia il monitor attivo", "When the focused monitor changes"), Some(t("Per esempio move-mouse monitor-lazy-center: il mouse segue il focus", "For example move-mouse monitor-lazy-center: the mouse follows the focus")), &aero("on-focused-monitor-changed"));
    commands(f, t("Quando cambia il focus", "When the focus changes"), None, &aero("on-focus-changed"));
}

// ---- gaps --------------------------------------------------------------------------------------

/// A gap: its value for every monitor, and the ones for some (`[{ monitor.main = 16 }, 24]`).
struct Gap {
    default: i64,
    per_monitor: Vec<(String, i64)>,
}

fn read_gap(spot: &Spot) -> Gap {
    match store::get(spot) {
        Some(toml::Value::Integer(n)) => Gap { default: n, per_monitor: vec![] },
        Some(toml::Value::Array(items)) => {
            let mut gap = Gap { default: 0, per_monitor: vec![] };
            for item in items {
                match item {
                    toml::Value::Integer(n) => gap.default = n,
                    toml::Value::Table(t) => {
                        if let Some(toml::Value::Table(m)) = t.get("monitor") {
                            for (pattern, v) in m {
                                gap.per_monitor.push((pattern.clone(), v.as_integer().unwrap_or(0)));
                            }
                        }
                    }
                    _ => {}
                }
            }
            gap
        }
        _ => Gap { default: 0, per_monitor: vec![] },
    }
}

fn write_gap(spot: &Spot, gap: &Gap) {
    if gap.per_monitor.is_empty() {
        store::set(spot, Value::from(gap.default));
        return;
    }
    let mut items: Vec<Value> = gap
        .per_monitor
        .iter()
        .map(|(pattern, n)| {
            let mut inner = toml_edit::InlineTable::new();
            inner.insert(pattern, Value::from(*n));
            inner.set_dotted(true);
            let mut outer = toml_edit::InlineTable::new();
            outer.insert("monitor", Value::InlineTable(inner));
            Value::InlineTable(outer)
        })
        .collect();
    items.push(Value::from(gap.default));
    store::set(spot, Value::Array(items.into_iter().collect()));
}

fn gap_rows(f: &mut Form, title: &str, spot: &Spot) {
    let gap = Rc::new(RefCell::new(read_gap(spot)));
    let px = Num::new(0.0, 500.0, 1.0, 0);
    let (s, g) = (spot.clone(), gap.clone());
    let help = (!gap.borrow().per_monitor.is_empty()).then(|| t("Sugli altri monitor", "On the other monitors"));
    let number = f.number_control(gap.borrow().default as f64, px, move |v| {
        g.borrow_mut().default = v as i64;
        write_gap(&s, &g.borrow());
    });
    let (s, g) = (spot.clone(), gap.clone());
    let add = f.icon_button("plus.circle", t("Un valore per un monitor", "A value for a monitor"), move || {
        let d = g.borrow().default;
        g.borrow_mut().per_monitor.push(("main".into(), d));
        write_gap(&s, &g.borrow());
        super::refresh_later();
    });
    let control = form::stack(f.mtm, false, 6.0);
    control.addArrangedSubview(&number);
    control.addArrangedSubview(&add);
    f.row(title, help, &control);
    let count = gap.borrow().per_monitor.len();
    for i in 0..count {
        let (pattern, value) = gap.borrow().per_monitor[i].clone();
        let (s, g) = (spot.clone(), gap.clone());
        let field = f.field(&pattern, "main", 200.0, move |v| {
            let v = v.trim().to_string();
            if !v.is_empty() {
                g.borrow_mut().per_monitor[i].0 = v;
                write_gap(&s, &g.borrow());
            }
        });
        let (s, g) = (spot.clone(), gap.clone());
        let number = f.number_control(value as f64, px, move |v| {
            g.borrow_mut().per_monitor[i].1 = v as i64;
            write_gap(&s, &g.borrow());
        });
        let (s, g) = (spot.clone(), gap.clone());
        let remove = f.icon_button("minus.circle", t("Togli", "Remove"), move || {
            g.borrow_mut().per_monitor.remove(i);
            write_gap(&s, &g.borrow());
            super::refresh_later();
        });
        let indent = form::label(f.mtm, "↳", 13.0, false, true);
        f.line(&[&indent, &field, &number], Some(&remove));
    }
}

pub fn gaps(f: &mut Form) {
    f.header(t("Spaziature", "Gaps"), t("Lo spazio fra le finestre e dai bordi dello schermo, in punti.", "The space between windows and from the screen edges, in points."), &files());
    f.group(Some(t("Fra le finestre", "Between windows")));
    gap_rows(f, t("In orizzontale", "Horizontal"), &aero("gaps.inner.horizontal"));
    gap_rows(f, t("In verticale", "Vertical"), &aero("gaps.inner.vertical"));
    f.group(Some(t("Dai bordi dello schermo", "From the screen edges")));
    gap_rows(f, t("In alto", "Top"), &aero("gaps.outer.top"));
    gap_rows(f, t("In basso", "Bottom"), &aero("gaps.outer.bottom"));
    gap_rows(f, t("A sinistra", "Left"), &aero("gaps.outer.left"));
    gap_rows(f, t("A destra", "Right"), &aero("gaps.outer.right"));
    f.footer(t(
        "Con ⊕ un valore diverso per un monitor, indicato con main, secondary, built-in o una parte del suo nome. In alto conta la barra: le pillole di RustyBar finiscono a 34 punti dall'alto, e sul display col notch macOS riserva già lo spazio.",
        "With ⊕ a different value for one monitor, named main, secondary, built-in or part of its name. At the top the bar matters: RustyBar's pills end 34 points from the top, and on the display with the notch macOS already reserves the space.",
    ));
}

// ---- bindings ----------------------------------------------------------------------------------

pub fn keys(f: &mut Form) {
    f.header(
        t("Scorciatoie", "Bindings"),
        t("I tasti di ogni modalità e i comandi che eseguono, uno per riga. Modificatori: alt, cmd, ctrl, shift; per esempio alt-shift-a.", "The keys of each mode and the commands they run, one per line. Modifiers: alt, cmd, ctrl, shift; for example alt-shift-a."),
        &files(),
    );
    let mut modes = store::keys(&aero("mode"));
    if let Some(i) = modes.iter().position(|m| m == "main") {
        let main = modes.remove(i);
        modes.insert(0, main);
    }
    for mode in &modes {
        let table = aero("mode").k(mode).k("binding");
        f.group(Some(&format!("{} {mode}", t("Modalità", "Mode"))));
        for key in store::keys(&table) {
            let spot = table.k(&key);
            let list = store::get_strings(&spot).unwrap_or_default();
            let is_array = matches!(store::get(&spot), Some(toml::Value::Array(_)));
            let (tb, old) = (table.clone(), key.clone());
            let key_field = f.field(&key, "alt-x", 130.0, move |new| {
                let new = new.trim().to_string();
                if new.is_empty() {
                    return;
                }
                if !store::rename(&tb, &old, &new) {
                    super::status(t("Questo tasto ha già un comando", "This key already has a command"), true);
                }
                super::refresh_later();
            });
            let col = form::stack(f.mtm, true, 4.0);
            for (i, cmd) in list.iter().enumerate() {
                let s = if is_array { spot.i(i) } else { spot.clone() };
                let field = f.field(cmd, "", form::INNER - 130.0 - 60.0, move |v| {
                    let v = v.trim().to_string();
                    if !v.is_empty() {
                        store::set(&s, Value::from(v));
                    }
                });
                col.addArrangedSubview(&field);
            }
            let mut items: Vec<(String, Box<dyn Fn()>)> = Vec::new();
            let s = spot.clone();
            items.push((t("Aggiungi un comando", "Add a command").into(), Box::new(move || {
                let mut list = store::get_strings(&s).unwrap_or_default();
                list.push("exec-and-forget ".into());
                store::set(&s, store::strings(&list));
                super::refresh_later();
            })));
            if list.len() > 1 {
                let s = spot.clone();
                items.push((t("Togli l'ultimo comando", "Remove the last command").into(), Box::new(move || {
                    let mut list = store::get_strings(&s).unwrap_or_default();
                    list.pop();
                    if list.len() == 1 { store::set(&s, Value::from(list[0].as_str())) } else { store::set(&s, store::strings(&list)) }
                    super::refresh_later();
                })));
            }
            let s = spot.clone();
            items.push((t("Elimina la scorciatoia", "Delete the binding").into(), Box::new(move || {
                store::remove(&s);
                super::refresh_later();
            })));
            let menu = f.menu_button(items);
            f.line(&[&key_field, &col], Some(&menu));
        }
        let key_field = f.field("", t("tasti", "keys"), 130.0, |_| {});
        let cmd_field = f.field("", t("comando", "command"), form::INNER - 130.0 - 120.0, |_| {});
        let (tb, kf, cf) = (table.clone(), key_field.clone(), cmd_field.clone());
        let add = f.button(t("Aggiungi", "Add"), move || {
            let (k, c) = (kf.stringValue().to_string().trim().to_string(), cf.stringValue().to_string().trim().to_string());
            if k.is_empty() || c.is_empty() {
                return;
            }
            if store::keys(&tb).contains(&k) {
                super::status(t("Questo tasto ha già un comando", "This key already has a command"), true);
                return;
            }
            store::set(&tb.k(&k), Value::from(c));
            super::refresh_later();
        });
        f.line(&[&key_field, &cmd_field], Some(&add));
    }
    f.group(Some(t("Nuova modalità", "New mode")));
    let name = f.field("", t("nome, es. resize", "name, e.g. resize"), 200.0, |_| {});
    let nf = name.clone();
    let add = f.button(t("Aggiungi", "Add"), move || {
        let n = nf.stringValue().to_string().trim().to_string();
        if n.is_empty() || store::keys(&aero("mode")).contains(&n) {
            return;
        }
        store::set(&aero("mode").k(&n).k("binding").k("esc"), Value::from("mode main"));
        super::refresh_later();
    });
    f.line(&[&name], Some(&add));
    f.footer(t(
        "Si entra in una modalità con il comando mode <nome>; esc la riporta a main. I comandi sono quelli di AeroSpace (workspace 1, move left, layout floating tiling…) o exec-and-forget con un comando della shell.",
        "A mode is entered with the command mode <name>; esc goes back to main. Commands are AeroSpace's (workspace 1, move left, layout floating tiling…) or exec-and-forget with a shell command.",
    ));
}

// ---- monitors ----------------------------------------------------------------------------------

pub fn monitors(f: &mut Form) {
    f.header(t("Monitor", "Monitors"), t("Su quale monitor va ogni workspace.", "Which monitor each workspace goes to."), &files());
    let table = aero("workspace-to-monitor-force-assignment");
    f.group(None);
    let keys = store::keys(&table);
    for ws in &keys {
        let spot = table.k(ws);
        let monitors = store::get_strings(&spot).unwrap_or_default();
        let (tb, old) = (table.clone(), ws.clone());
        let ws_field = f.field(ws, "1", 70.0, move |new| {
            let new = new.trim().to_string();
            if !new.is_empty() && !store::rename(&tb, &old, &new) {
                super::status(t("Questo workspace ha già un monitor", "This workspace already has a monitor"), true);
            }
            super::refresh_later();
        });
        let arrow = form::label(f.mtm, "→", 13.0, false, true);
        let s = spot.clone();
        let monitor_field = f.field(&monitors.join(", "), "main", 300.0, move |v| {
            let list: Vec<String> = v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            match list.len() {
                0 => {}
                1 => store::set(&s, Value::from(list[0].as_str())),
                _ => store::set(&s, store::strings(&list)),
            }
        });
        let s = spot.clone();
        let menu = f.menu_button(vec![(t("Elimina", "Delete").into(), Box::new(move || {
            store::remove(&s);
            super::refresh_later();
        }))]);
        f.line(&[&ws_field, &arrow, &monitor_field], Some(&menu));
    }
    if keys.is_empty() {
        f.note(t("Ogni workspace va sul monitor dove l'hai aperto.", "Each workspace goes to the monitor where you opened it."));
    }
    let ws_field = f.field("", "workspace", 90.0, |_| {});
    let monitor_field = f.field("", "main", 280.0, |_| {});
    let (tb, wf, mf) = (table.clone(), ws_field.clone(), monitor_field.clone());
    let add = f.button(t("Aggiungi", "Add"), move || {
        let (w, m) = (wf.stringValue().to_string().trim().to_string(), mf.stringValue().to_string().trim().to_string());
        if w.is_empty() || m.is_empty() || store::keys(&tb).contains(&w) {
            return;
        }
        store::set(&tb.k(&w), Value::from(m));
        super::refresh_later();
    });
    f.line(&[&ws_field, &monitor_field], Some(&add));
    f.footer(t(
        "Un monitor è main, secondary, built-in o una parte del suo nome; con più di uno, separati da virgole, vale il primo collegato.",
        "A monitor is main, secondary, built-in or part of its name; with more than one, separated by commas, the first connected one wins.",
    ));
}

// ---- window rules ------------------------------------------------------------------------------

pub fn rules(f: &mut Form) {
    f.header(
        t("Regole delle finestre", "Window rules"),
        t("Cosa fa AeroSpace quando compare una finestra: per esempio spostarla in un workspace o lasciarla fluttuante.", "What AeroSpace does when a window appears: for example move it to a workspace or leave it floating."),
        &files(),
    );
    let list = aero("on-window-detected");
    let count = store::len(&list);
    for i in 0..count {
        let rule = list.i(i);
        let app_id = store::get_str(&rule.k("if").k("app-id")).unwrap_or_default();
        let title = form::app_name(&app_id).unwrap_or_else(|| if app_id.is_empty() { format!("{} {}", t("Regola", "Rule"), i + 1) } else { app_id.clone() });
        f.group(Some(&title));
        let s = rule.k("if").k("app-id");
        let field = f.field(&app_id, "com.example.app", 260.0, move |v| {
            let v = v.trim();
            if v.is_empty() { store::remove(&s) } else { store::set(&s, Value::from(v)) }
            super::refresh_later();
        });
        let s = rule.k("if").k("app-id");
        let pick = f.button(t("Scegli…", "Choose…"), move || {
            if let Some(id) = form::choose_file(Some(std::path::Path::new("/Applications")), true).and_then(|p| form::bundle_id(&p)) {
                store::set(&s, Value::from(id));
                super::refresh_later();
            }
        });
        let menu = f.menu_button(list_menu(&list, i, count));
        f.line(&[&form::label(f.mtm, t("App", "App"), 13.0, false, false), &field, &pick], Some(&menu));
        f.text_at(t("Nome dell'app", "App name"), Some(t("Un'espressione regolare, al posto del bundle id", "A regular expression, in place of the bundle id")), &rule.k("if").k("app-name-regex-substring"), "");
        f.text_at(t("Titolo della finestra", "Window title"), Some(t("Un'espressione regolare", "A regular expression")), &rule.k("if").k("window-title-regex-substring"), "");
        f.text_at(t("Solo nel workspace", "Only in workspace"), None, &rule.k("if").k("workspace"), "");
        let startup = match store::get_bool(&rule.k("if").k("during-aerospace-startup")) {
            Some(true) => "true",
            Some(false) => "false",
            None => "",
        };
        let s = rule.k("if").k("during-aerospace-startup");
        f.choice(
            t("All'avvio di AeroSpace", "At AeroSpace's startup"),
            None,
            opts(&[("", t("Sempre", "Always")), ("true", t("Solo all'avvio", "Only at startup")), ("false", t("Mai all'avvio", "Never at startup"))]),
            startup,
            move |v| match v.as_str() {
                "" => store::remove(&s),
                v => store::set(&s, Value::from(v == "true")),
            },
        );
        f.toggle_at(t("Continua con le altre regole", "Go on with the other rules"), None, &rule.k("check-further-callbacks"), false);
        let run = rule.k("run");
        let cmds = store::get_strings(&run).unwrap_or_default();
        let s = run.clone();
        let field = f.field(&cmds.join(" ; "), "move-node-to-workspace 9", form::INNER - 200.0, move |v| {
            let list: Vec<String> = v.split(" ; ").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            match list.len() {
                0 => {}
                1 => store::set(&s, Value::from(list[0].as_str())),
                _ => store::set(&s, store::strings(&list)),
            }
        });
        f.row(t("Esegui", "Run"), Some(t("Più comandi separati da ;", "More commands separated by ;")), &field);
    }
    if count == 0 {
        f.group(None);
        f.note(t("Nessuna regola.", "No rules."));
    }
    let s = list.clone();
    let add = f.button(t("Aggiungi una regola per un'app…", "Add a rule for an app…"), move || {
        let Some(id) = form::choose_file(Some(std::path::Path::new("/Applications")), true).and_then(|p| form::bundle_id(&p)) else { return };
        let mut cond = Table::new();
        cond.set_dotted(true);
        cond.insert("app-id", toml_edit::value(id));
        let mut rule = Table::new();
        rule.insert("if", toml_edit::Item::Table(cond));
        rule.insert("run", toml_edit::value("layout floating"));
        store::push_table(&s, rule);
        super::refresh_later();
    });
    f.group(None);
    f.buttons(&[&add]);
    f.footer(t(
        "Comandi utili: move-node-to-workspace <n>, layout floating, layout tiling. Una regola nuova parte con layout floating.",
        "Useful commands: move-node-to-workspace <n>, layout floating, layout tiling. A new rule starts with layout floating.",
    ));
}

// ---- borders -----------------------------------------------------------------------------------

/// Where the options of borders are: the `borders …` command among AeroSpace's startup ones.
struct Borders {
    spot: Spot,
    prefix: String,
    options: Vec<(String, String)>,
}

/// borders' defaults, given in full when the running one is updated, so that an option taken
/// away goes back to its default there too.
const BORDER_DEFAULTS: &[(&str, &str)] = &[
    ("style", "round"),
    ("width", "2.0"),
    ("hidpi", "off"),
    ("active_color", "0xffe1e3e4"),
    ("inactive_color", "0xff494d64"),
    ("background_color", "0x00000000"),
    ("blur_radius", "0.0"),
    ("ax_focus", "off"),
    ("order", "below"),
];

/// Splits a command line into words, as the shell does with quotes.
fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quote, mut any) = (None, false);
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '"' | '\'') => {
                quote = Some(c);
                any = true;
            }
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (None, c) if c.is_whitespace() => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            (_, c) => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn quote(v: &str) -> String {
    if v.chars().all(|c| c.is_ascii_alphanumeric() || "._-/:,".contains(c)) && !v.is_empty() {
        v.to_string()
    } else {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn find_borders() -> Option<Borders> {
    let spot = aero("after-startup-command");
    let (list, is_array) = match store::get(&spot)? {
        toml::Value::String(s) => (vec![s], false),
        toml::Value::Array(a) => (a.into_iter().filter_map(|v| v.as_str().map(String::from)).collect(), true),
        _ => return None,
    };
    list.iter().enumerate().find_map(|(i, cmd)| {
        let w = words(cmd);
        let at = w.iter().position(|x| x == "borders" || x.ends_with("/borders"))?;
        let prefix = w[..=at].iter().map(|x| quote(x)).collect::<Vec<_>>().join(" ");
        let options = w[at + 1..].iter().map(|o| o.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())).unwrap_or((o.clone(), String::new()))).collect();
        Some(Borders { spot: if is_array { spot.i(i) } else { spot.clone() }, prefix, options })
    })
}

pub fn borders_available() -> bool {
    store::path(File::AeroSpace).exists() && find_borders().is_some()
}

fn get_opt(b: &Borders, key: &str) -> Option<String> {
    b.options.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
}

fn default_of(key: &str) -> String {
    BORDER_DEFAULTS.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string()).unwrap_or_default()
}

/// Sets (or, with the default or empty, takes away) an option, writes the command and updates the
/// running borders.
fn set_opt(key: &str, value: &str) {
    let Some(mut b) = find_borders() else { return };
    let value = value.trim();
    let had = b.options.iter().position(|(k, _)| k == key);
    if value.is_empty() || value == default_of(key) {
        b.options.retain(|(k, _)| k != key);
    } else if let Some(i) = had {
        // Where it was, as the user wrote it
        b.options[i].1 = value.to_string();
    } else {
        // A new one in the order of the defaults, as borders' README lists them
        let order = |k: &str| BORDER_DEFAULTS.iter().position(|(d, _)| *d == k).unwrap_or(usize::MAX);
        let at = b.options.iter().position(|(k, _)| order(k) > order(key)).unwrap_or(b.options.len());
        b.options.insert(at, (key.to_string(), value.to_string()));
    }
    let mut cmd = b.prefix.clone();
    for (k, v) in &b.options {
        cmd.push(' ');
        cmd.push_str(&if v.is_empty() { k.clone() } else { format!("{k}={}", quote(v)) });
    }
    store::set(&b.spot, Value::from(cmd));
    update_running(&b.options);
}

/// Gives the running borders every option: borders started again with options updates the one
/// that runs. Nothing when it doesn't run, or it would start one of its own.
fn update_running(options: &[(String, String)]) {
    if store::mock_dir().is_some() || !process_runs("borders") {
        return;
    }
    let mut args: Vec<String> = BORDER_DEFAULTS
        .iter()
        .map(|(k, d)| format!("{k}={}", options.iter().find(|(o, _)| o == k).map(|(_, v)| v.as_str()).unwrap_or(d)))
        .collect();
    args.extend(options.iter().filter(|(k, _)| !BORDER_DEFAULTS.iter().any(|(d, _)| d == k)).map(|(k, v)| if v.is_empty() { k.clone() } else { format!("{k}={v}") }));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    crate::runtime::fire("borders", &args);
}

/// Whether a process called `name` runs, from libproc: no `pgrep`.
fn process_runs(name: &str) -> bool {
    let mut pids = vec![0i32; 4096];
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), (pids.len() * 4) as i32) };
    pids.truncate(n.max(0) as usize);
    pids.iter().any(|&pid| {
        let mut buf = [0u8; 256];
        let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
        len > 0 && &buf[..len as usize] == name.as_bytes()
    })
}

/// A color of borders: `0xaarrggbb`, `gradient(top_left=…,bottom_right=…)` or `glow(…)`.
enum BorderColor {
    Solid(Rgba),
    Gradient(String, Rgba, Rgba),
    Glow(Rgba),
}

fn argb(s: &str) -> Rgba {
    Rgba::parse(s.trim()).unwrap_or(Rgba(1.0, 1.0, 1.0, 1.0))
}

fn to_argb(c: Rgba) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("0x{:02x}{:02x}{:02x}{:02x}", b(c.3), b(c.0), b(c.1), b(c.2))
}

fn parse_color(s: &str) -> BorderColor {
    if let Some(inner) = s.strip_prefix("gradient(").and_then(|r| r.strip_suffix(')')) {
        let parts: Vec<(&str, &str)> = inner.split(',').filter_map(|p| p.split_once('=')).collect();
        if let [(corner, a), (_, b)] = parts[..] {
            return BorderColor::Gradient(corner.trim().to_string(), argb(a), argb(b));
        }
    }
    if let Some(inner) = s.strip_prefix("glow(").and_then(|r| r.strip_suffix(')')) {
        return BorderColor::Glow(argb(inner));
    }
    BorderColor::Solid(argb(s))
}

fn opposite(corner: &str) -> &'static str {
    match corner {
        "top_left" => "bottom_right",
        "top_right" => "bottom_left",
        "bottom_left" => "top_right",
        _ => "top_left",
    }
}

fn format_color(c: &BorderColor) -> String {
    match c {
        BorderColor::Solid(c) => to_argb(*c),
        BorderColor::Glow(c) => format!("glow({})", to_argb(*c)),
        BorderColor::Gradient(corner, a, b) => format!("gradient({corner}={},{}={})", to_argb(*a), opposite(corner), to_argb(*b)),
    }
}

fn color_rows(f: &mut Form, title: &str, help: Option<&str>, b: &Borders, key: &'static str) {
    let current = get_opt(b, key).unwrap_or_else(|| default_of(key));
    let color = Rc::new(RefCell::new(parse_color(&current)));
    let mode = match &*color.borrow() {
        BorderColor::Solid(_) => "solid",
        BorderColor::Gradient(..) => "gradient",
        BorderColor::Glow(_) => "glow",
    };
    let c = color.clone();
    let popup = f.popup(opts(&[("solid", t("Tinta unita", "Solid")), ("gradient", t("Sfumatura", "Gradient")), ("glow", t("Bagliore", "Glow"))]), mode, move |v| {
        let first = match &*c.borrow() {
            BorderColor::Solid(x) | BorderColor::Glow(x) | BorderColor::Gradient(_, x, _) => *x,
        };
        let new = match v.as_str() {
            "gradient" => BorderColor::Gradient("top_right".into(), first, Rgba(0.54, 0.86, 0.92, 1.0)),
            "glow" => BorderColor::Glow(first),
            _ => BorderColor::Solid(first),
        };
        set_opt(key, &format_color(&new));
        super::refresh_later();
    });
    let row = form::stack(f.mtm, false, 8.0);
    row.addArrangedSubview(&popup);
    let first = match &*color.borrow() {
        BorderColor::Solid(x) | BorderColor::Glow(x) | BorderColor::Gradient(_, x, _) => *x,
    };
    let c = color.clone();
    let well = f.color_well(Some(first), move |v| {
        match &mut *c.borrow_mut() {
            BorderColor::Solid(x) | BorderColor::Glow(x) | BorderColor::Gradient(_, x, _) => *x = v,
        }
        set_opt(key, &format_color(&c.borrow()));
    });
    row.addArrangedSubview(&well);
    if let BorderColor::Gradient(corner, _, second) = &*color.borrow() {
        let c = color.clone();
        let well = f.color_well(Some(*second), move |v| {
            if let BorderColor::Gradient(_, _, x) = &mut *c.borrow_mut() {
                *x = v;
            }
            set_opt(key, &format_color(&c.borrow()));
        });
        row.addArrangedSubview(&well);
        let c = color.clone();
        let dirs = opts(&[("top_left", "↘"), ("top_right", "↙"), ("bottom_left", "↗"), ("bottom_right", "↖")]);
        let dir = f.popup(dirs, corner, move |v| {
            if let BorderColor::Gradient(corner, _, _) = &mut *c.borrow_mut() {
                *corner = v;
            }
            set_opt(key, &format_color(&c.borrow()));
        });
        dir.setToolTip(Some(&objc2_foundation::NSString::from_str(t("Da dove parte il primo colore", "Where the first color starts"))));
        row.addArrangedSubview(&dir);
    }
    f.row(title, help, &row);
}

pub fn borders(f: &mut Form) {
    f.header(
        t("Bordi", "Borders"),
        t("Il bordo della finestra attiva, disegnato da JankyBorders: le opzioni del comando borders che AeroSpace esegue all'avvio. Si applicano subito.", "The border of the focused window, drawn by JankyBorders: the options of the borders command AeroSpace runs at startup. They apply at once."),
        &files(),
    );
    let Some(b) = find_borders() else {
        f.group(None);
        f.note(t("Fra i comandi All'avvio di AeroSpace non c'è borders.", "There's no borders among AeroSpace's startup commands."));
        return;
    };
    f.group(Some(t("Colori", "Colors")));
    color_rows(f, t("Finestra attiva", "Focused window"), None, &b, "active_color");
    color_rows(f, t("Altre finestre", "Other windows"), Some(t("Trasparente per non disegnarlo", "Transparent to draw none")), &b, "inactive_color");
    color_rows(f, t("Sfondo", "Background"), Some(t("Sotto le finestre, dentro il bordo", "Under the windows, inside the border")), &b, "background_color");

    f.group(Some(t("Forma", "Shape")));
    let width: f64 = get_opt(&b, "width").and_then(|v| v.parse().ok()).unwrap_or(2.0);
    f.number(t("Spessore", "Width"), None, width, Num::new(0.0, 40.0, 0.5, 1), |v| set_opt("width", &format!("{v:.1}")));
    let style = get_opt(&b, "style").unwrap_or_else(|| "round".into());
    f.choice(t("Angoli", "Corners"), None, opts(&[("round", t("Arrotondati", "Round")), ("square", t("Squadrati", "Square"))]), &style, |v| set_opt("style", &v));
    let order = get_opt(&b, "order").unwrap_or_else(|| "below".into());
    f.choice(t("Posizione", "Order"), Some(t("Sotto o sopra la finestra", "Below or above the window")), opts(&[("below", t("Sotto", "Below")), ("above", t("Sopra", "Above"))]), &order, |v| set_opt("order", &v));
    let blur: f64 = get_opt(&b, "blur_radius").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    f.number(t("Sfocatura dello sfondo", "Background blur"), None, blur, Num::new(0.0, 100.0, 1.0, 1), |v| set_opt("blur_radius", &format!("{v:.1}")));
    let on = |k: &str| get_opt(&b, k).as_deref() == Some("on");
    f.toggle(t("Alta risoluzione", "High resolution"), Some(t("Disegna il bordo a risoluzione Retina", "Draws the border at Retina resolution")), on("hidpi"), |v| set_opt("hidpi", if v { "on" } else { "off" }));
    f.toggle(t("Focus con l'Accessibilità", "Focus through Accessibility"), Some(t("Più preciso con alcune app, un po' più lento", "More precise with some apps, a bit slower")), on("ax_focus"), |v| set_opt("ax_focus", if v { "on" } else { "off" }));

    f.group(Some("App"));
    let list = |k: &str| get_opt(&b, k).map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>()).unwrap_or_default();
    f.tokens(t("Senza bordo", "Without a border"), Some(t("I nomi delle app da escludere", "The names of the apps to leave out")), &list("blacklist"), t("nessuna", "none"), |v| set_opt("blacklist", &v.join(",")));
    f.tokens(t("Solo queste", "Only these"), Some(t("Se ce n'è almeno una, il bordo è solo per queste app", "With at least one, only these apps get the border")), &list("whitelist"), t("tutte", "all"), |v| set_opt("whitelist", &v.join(",")));
    let others: Vec<String> = b.options.iter().filter(|(k, _)| !BORDER_DEFAULTS.iter().any(|(d, _)| d == k) && k != "blacklist" && k != "whitelist").map(|(k, v)| format!("{k}={v}")).collect();
    if !others.is_empty() {
        f.note(&format!("{} {}", t("Altre opzioni nel comando:", "Other options in the command:"), others.join(" ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_the_shell() {
        let w = words(r#"exec-and-forget borders active_color="gradient(top_right=0xfff2cdcd,bottom_left=0xff89dceb)" inactive_color=0x00000000 width=14.0"#);
        assert_eq!(w, vec!["exec-and-forget", "borders", "active_color=gradient(top_right=0xfff2cdcd,bottom_left=0xff89dceb)", "inactive_color=0x00000000", "width=14.0"]);
        assert_eq!(quote("gradient(top_right=0xfff2cdcd,bottom_left=0xff89dceb)"), "\"gradient(top_right=0xfff2cdcd,bottom_left=0xff89dceb)\"");
        assert_eq!(quote("0x00000000"), "0x00000000");
    }

    #[test]
    fn border_colors_round_trip() {
        for s in ["0xfff2cdcd", "gradient(top_right=0xfff2cdcd,bottom_left=0xff89dceb)", "glow(0xff89dceb)"] {
            assert_eq!(format_color(&parse_color(s)), s);
        }
    }
}
