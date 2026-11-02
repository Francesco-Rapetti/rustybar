//! The pages of SwipeAeroSpace (its TOML, which it reloads by itself), MiddleClick (its defaults,
//! read when it starts: the page restarts it) and the Brewfile (the packages that `notifications`
//! follows, one line each; the other lines and the comments stay as they are).

use std::cell::RefCell;
use std::rc::Rc;

use objc2::rc::Retained;
use objc2_foundation::{NSString, NSUserDefaults};

use super::form::{Form, Num, opts};
use super::store::{self, File, rb, swipe as sw};
use crate::i18n::t;

pub fn swipe(f: &mut Form) {
    f.header(
        "SwipeAeroSpace",
        t("Gli swipe sul trackpad che cambiano workspace di AeroSpace. Le chiavi scritte qui nelle sue Impostazioni sono in sola lettura; Apri al login si sceglie lì.", "The trackpad swipes that change AeroSpace workspaces. The keys written here are read-only in its Settings; Launch at Login is chosen there."),
        &[store::path(File::Swipe)],
    );
    let fingers = opts(&[("Three", t("Tre dita", "Three fingers")), ("Four", t("Quattro dita", "Four fingers"))]);
    f.group(Some(t("Swipe orizzontale", "Horizontal swipe")));
    f.choice_at(t("Dita", "Fingers"), Some(t("I gesti di macOS con le stesse dita vanno spostati sulle altre", "macOS's gestures with the same fingers must move to the others")), &sw("fingers"), fingers, "Three");
    f.number_at(t("Sensibilità", "Threshold"), Some(t("Quanto devono scorrere le dita per ogni cambio: meno è più sensibile", "How far the fingers travel for each switch: lower is more sensitive")), &sw("threshold"), 1.0, Num::new(0.1, 3.0, 0.05, 2));
    f.toggle_at(t("Verso naturale", "Natural direction"), Some(t("Come lo scorrimento", "As scrolling")), &sw("natural"), true);
    f.toggle_at(t("Ricomincia dal primo", "Wrap around"), Some(t("Dopo l'ultimo workspace si torna al primo", "Past the last workspace back to the first")), &sw("wrap"), false);
    f.toggle_at(t("Salta quelli vuoti", "Skip empty ones"), Some(t("Solo workspace con finestre", "Only workspaces with windows")), &sw("skip-empty"), false);
    f.toggle_at(t("Più workspace per swipe", "Several workspaces per swipe"), Some(t("Uno swipe lungo salta più workspace", "A longer swipe jumps more workspaces")), &sw("multiSwipe"), false);
    f.number_at(t("Al massimo", "At most"), Some(t("Workspace per swipe lungo, da 2 a 9", "Workspaces per long swipe, from 2 to 9")), &sw("maxSteps"), 3.0, Num::new(2.0, 9.0, 1.0, 0));
    f.group(Some(t("Swipe in su", "Swipe up")));
    f.toggle_at(t("Panoramica", "Overview"), Some(t("Ogni workspace con le sue app", "Every workspace with its apps")), &sw("swipeUpOverview"), false);
    let fingers = opts(&[("Three", t("Tre dita", "Three fingers")), ("Four", t("Quattro dita", "Four fingers"))]);
    f.choice_at(t("Dita", "Fingers"), None, &sw("swipeUpFingers"), fingers, "Three");
    f.toggle_at(t("Anche i workspace vuoti", "Empty workspaces too"), None, &sw("show-empty-workspaces"), false);
}

const MIDDLECLICK: &str = "art.ginzburg.MiddleClick";

/// MiddleClick's defaults; none with made-up data.
fn middleclick_defaults() -> Option<Retained<NSUserDefaults>> {
    if store::mock_dir().is_some() {
        return None;
    }
    NSUserDefaults::initWithSuiteName(<NSUserDefaults as objc2::AnyThread>::alloc(), Some(&NSString::from_str(MIDDLECLICK)))
}

pub fn middleclick_available() -> bool {
    store::mock_dir().is_some() || objc2_app_kit::NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&NSString::from_str(MIDDLECLICK)).is_some()
}

pub fn middleclick(f: &mut Form) {
    f.header("MiddleClick", t("Il clic centrale premendo con più dita sul trackpad. Legge queste impostazioni solo quando parte: dopo una modifica va riavviata.", "The middle click, pressing with more fingers on the trackpad. It reads these settings only when it starts: restart it after a change."), &[]);
    let ud = middleclick_defaults();
    let has = |k: &str| ud.as_ref().is_some_and(|u| u.objectForKey(&NSString::from_str(k)).is_some());
    let fingers = if has("fingers") { ud.as_ref().map(|u| u.integerForKey(&NSString::from_str("fingers"))).unwrap_or(3) } else { 3 };
    let bool_of = |k: &str, d: bool| if has(k) { ud.as_ref().map(|u| u.boolForKey(&NSString::from_str(k))).unwrap_or(d) } else { d };
    f.group(None);
    let u = ud.clone();
    f.choice(t("Dita", "Fingers"), None, opts(&[("3", t("Tre", "Three")), ("4", t("Quattro", "Four")), ("5", t("Cinque", "Five"))]), &fingers.to_string(), move |v| {
        if let Some(u) = &u {
            u.setInteger_forKey(v.parse().unwrap_or(3), &NSString::from_str("fingers"));
            super::status(t("Salvato: riavvia MiddleClick per usarlo", "Saved: restart MiddleClick to use it"), false);
        }
    });
    let u = ud.clone();
    f.toggle(t("Anche con più dita", "With more fingers too"), Some(t("Conta anche un clic con più dita di quelle scelte", "Also counts a click with more fingers than those chosen")), bool_of("allowMoreFingers", false), move |v| {
        if let Some(u) = &u {
            u.setBool_forKey(v, &NSString::from_str("allowMoreFingers"));
            super::status(t("Salvato: riavvia MiddleClick per usarlo", "Saved: restart MiddleClick to use it"), false);
        }
    });
    let u = ud.clone();
    f.toggle(t("Anche il tocco", "Tap too"), Some(t("Spento, conta solo il clic premuto; acceso segue il «tocco per fare clic» di macOS", "Off, only a pressed click counts; on, it follows macOS's tap to click")), bool_of("tapToClick", true), move |v| {
        if let Some(u) = &u {
            u.setBool_forKey(v, &NSString::from_str("tapToClick"));
            super::status(t("Salvato: riavvia MiddleClick per usarlo", "Saved: restart MiddleClick to use it"), false);
        }
    });
    let restart = f.button(t("Riavvia MiddleClick", "Restart MiddleClick"), || {
        if store::mock_dir().is_some() {
            return;
        }
        let apps = objc2_app_kit::NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(MIDDLECLICK));
        for app in apps.iter() {
            app.terminate();
        }
        crate::runtime::later(1.5, || crate::runtime::open_app(MIDDLECLICK));
        super::status(t("MiddleClick riparte", "MiddleClick is restarting"), false);
    });
    f.buttons(&[&restart]);
}

/// The Brewfile `notifications` follows, if there is one.
pub fn brewfile() -> Option<std::path::PathBuf> {
    let path = match store::mock_dir() {
        Some(d) => d.join("Brewfile"),
        None => {
            let p = store::get_str(&rb("widgets.notifications.brewfile"))?;
            std::path::PathBuf::from(p.replacen('~', &crate::config::home().display().to_string(), 1))
        }
    };
    path.exists().then_some(path)
}

const KINDS: &[&str] = &["tap", "brew", "cask", "mas"];

/// A package line: its indent, kind, name and what follows the name (options, a comment).
fn parse_line(line: &str) -> Option<(String, String, String, String)> {
    let indent: String = line.chars().take_while(|c| c.is_whitespace()).collect();
    let rest = line.trim_start();
    let (kind, after) = rest.split_once(char::is_whitespace)?;
    if !KINDS.contains(&kind) {
        return None;
    }
    let after = after.trim_start();
    let q = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let end = after[1..].find(q)? + 1;
    Some((indent, kind.to_string(), after[1..end].to_string(), after[end + 1..].to_string()))
}

/// Changes line `n` of the Brewfile, if it's still the package `name`: None removes it.
fn edit_line(n: usize, name: &str, new: Option<(String, String)>) {
    let Some(path) = brewfile() else { return };
    let Ok(text) = std::fs::read_to_string(&path) else { return };
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    match lines.get(n).and_then(|l| parse_line(l)) {
        Some((indent, _, old, rest)) if old == name => match new {
            Some((kind, name)) => lines[n] = format!("{indent}{kind} \"{name}\"{rest}"),
            None => {
                lines.remove(n);
            }
        },
        _ => {
            super::status(t("Il Brewfile è cambiato: ricarico la pagina", "The Brewfile changed: reloading the page"), true);
            super::refresh_later();
            return;
        }
    }
    write(&path, &lines);
}

fn write(path: &std::path::Path, lines: &[String]) {
    let mut text = lines.join("\n");
    text.push('\n');
    match std::fs::write(path, text) {
        Ok(()) => super::status(&format!("{} {}", t("Salvato in", "Saved to"), tilde(path)), false),
        Err(e) => super::status(&e.to_string(), true),
    }
}

fn tilde(path: &std::path::Path) -> String {
    let home = crate::config::home();
    path.strip_prefix(&home).map(|r| format!("~/{}", r.display())).unwrap_or_else(|_| path.display().to_string())
}

pub fn brew(f: &mut Form) {
    let Some(path) = brewfile() else { return };
    f.header(
        "Homebrew",
        t("I pacchetti del Brewfile: RustyBar ne cerca gli aggiornamenti ogni ora. Cambiarli qui non installa né disinstalla niente: per farlo, brew bundle.", "The packages of the Brewfile: RustyBar checks them for updates every hour. Changing them here installs or removes nothing: brew bundle does."),
        std::slice::from_ref(&path),
    );
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let kinds_opts = || opts(&[("tap", "tap"), ("brew", "brew"), ("cask", "cask"), ("mas", "mas")]);
    for kind in KINDS {
        let entries: Vec<(usize, String)> = text.lines().enumerate().filter_map(|(n, l)| parse_line(l).filter(|p| p.1 == *kind).map(|p| (n, p.2))).collect();
        if entries.is_empty() {
            continue;
        }
        f.group(Some(match *kind {
            "tap" => t("Tap", "Taps"),
            "brew" => t("Formule", "Formulae"),
            "cask" => t("App (cask)", "Apps (casks)"),
            _ => "Mac App Store",
        }));
        for (n, name) in entries {
            let current = Rc::new(RefCell::new((kind.to_string(), name.clone())));
            let c = current.clone();
            let popup = f.popup(kinds_opts(), kind, move |v| {
                let (k, nm) = c.borrow().clone();
                if v != k {
                    edit_line(n, &nm, Some((v.clone(), nm.clone())));
                    super::refresh_later();
                }
            });
            let c = current.clone();
            let field = f.field(&name, "", 300.0, move |v| {
                let v = v.trim().to_string();
                let (k, nm) = c.borrow().clone();
                if !v.is_empty() && v != nm {
                    edit_line(n, &nm, Some((k, v.clone())));
                    c.borrow_mut().1 = v;
                }
            });
            let c = current.clone();
            let remove = f.icon_button("minus.circle", t("Togli", "Remove"), move || {
                let (_, nm) = c.borrow().clone();
                edit_line(n, &nm, None);
                super::refresh_later();
            });
            f.line(&[&popup, &field], Some(&remove));
        }
    }
    f.group(Some(t("Aggiungi", "Add")));
    let kind = Rc::new(RefCell::new("brew".to_string()));
    let k = kind.clone();
    let popup = f.popup(kinds_opts(), "brew", move |v| *k.borrow_mut() = v);
    let field = f.field("", t("nome, es. jq", "name, e.g. jq"), 260.0, |_| {});
    let fd = field.clone();
    let add = f.button(t("Aggiungi", "Add"), move || {
        let name = fd.stringValue().to_string().trim().to_string();
        let Some(path) = brewfile() else { return };
        if name.is_empty() {
            return;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut lines: Vec<String> = text.lines().map(String::from).collect();
        // After the last line of its kind, or at the end
        let k = kind.borrow().clone();
        let at = lines.iter().rposition(|l| parse_line(l).is_some_and(|p| p.1 == k)).map(|i| i + 1).unwrap_or(lines.len());
        lines.insert(at, format!("{k} \"{name}\""));
        write(&path, &lines);
        super::refresh_later();
    });
    f.line(&[&popup, &field], Some(&add));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brewfile_lines() {
        assert_eq!(parse_line(r#"brew "felixkratz/formulae/borders""#), Some(("".into(), "brew".into(), "felixkratz/formulae/borders".into(), "".into())));
        assert_eq!(parse_line(r#"  cask "font-sf-pro", greedy: true # fonts"#).map(|p| p.3), Some(", greedy: true # fonts".into()));
        assert_eq!(parse_line("# brew \"x\""), None);
    }
}
