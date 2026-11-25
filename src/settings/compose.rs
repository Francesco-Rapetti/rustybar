//! The page of the compose key (see `crate::compose`): its switch, the key and its times, and the
//! sequences, each a row to change or remove. Those RustyBar comes with aren't in the file: one
//! that changes is written under `[compose.sequences]`, one removed is written empty, and one set
//! back to what it was goes away from the file, which keeps only what differs.

use objc2_app_kit::NSControlSize;

use super::form::{self, Form, Num, opts};
use super::store::{self, Spot, Value, rb};
use crate::compose as engine;
use crate::config::ComposeConfig;
use crate::gfx::Rgba;
use crate::i18n::t;

fn files() -> Vec<std::path::PathBuf> {
    [store::path(store::File::Bar), store::path(store::File::BarLocal)].into_iter().filter(|p| p.exists()).collect()
}

fn spot(keys: &str) -> Spot {
    rb("compose.sequences").k(keys)
}

/// The sequences of the file, in its order: keys → text.
fn custom() -> Vec<(String, String)> {
    let table = rb("compose.sequences");
    store::keys(&table).into_iter().filter_map(|k| store::get_str(&table.k(&k)).map(|v| (k, v))).collect()
}

fn default_of(keys: &str) -> Option<&'static str> {
    engine::DEFAULTS.iter().find(|(k, _)| *k == keys).map(|(_, text)| *text)
}

/// The keys as a field shows them, a space as `␣`, and back.
fn show(keys: &str) -> String {
    keys.replace(' ', "␣")
}

fn read(field: &str) -> String {
    field.replace('␣', " ")
}

/// `keys` gives `text`: written, unless RustyBar comes with just that.
fn put(keys: &str, text: &str) {
    if default_of(keys) == Some(text) { store::remove(&spot(keys)) } else { store::set(&spot(keys), Value::from(text)) }
}

/// `keys` gives nothing any more: one RustyBar comes with is written empty.
fn take(keys: &str) {
    if default_of(keys).is_some() { store::set(&spot(keys), Value::from("")) } else { store::remove(&spot(keys)) }
}

fn taken(keys: &str) -> bool {
    engine::listing(&custom()).iter().any(|e| e.keys == keys && !e.text.is_empty())
}

fn rename(old: &str, new: &str, text: &str) {
    if new.is_empty() || new == old {
        return;
    }
    if taken(new) {
        super::status(&format!("{} {}", t("C'è già la sequenza", "There's already the sequence"), show(new)), true);
        return;
    }
    take(old);
    put(new, text);
}

pub fn page(f: &mut Form) {
    let d = ComposeConfig::default();
    f.header(
        t("Tasto Compose", "Compose key"),
        t(
            "Come su Linux: un tocco sul tasto, poi una sequenza di tasti, e al loro posto arriva un carattere. Con ⌘ destro: e dà è, ' poi e dà é. Tenuto premuto, il tasto resta il modificatore di sempre.",
            "As on Linux: a tap of the key, then a sequence of keys, and in their place comes a character. With the right ⌘: e gives è, ' then e gives é. Held down, the key stays the modifier it always was.",
        ),
        &files(),
    );
    f.group(None);
    let on = store::get_bool(&rb("compose.enabled")).unwrap_or(d.enabled);
    let default = toml::Value::Boolean(d.enabled);
    f.toggle(
        t("Tasto Compose", "Compose key"),
        Some(t(
            "Per riconoscere le sequenze RustyBar legge la tastiera: serve Accessibilità. Niente di ciò che scrivi è tenuto o registrato",
            "To tell the sequences RustyBar reads the keyboard: it takes Accessibility. Nothing you type is kept or logged",
        )),
        on,
        move |v| {
            store::put(&rb("compose.enabled"), Value::from(v), Some(&default));
            // The row of the permission comes and goes with it
            super::refresh_later();
        },
    );
    if on && store::mock_dir().is_none() && !engine::trusted() {
        let open = f.button(t("Apri Impostazioni…", "Open Settings…"), || {
            crate::widgets::notifications::prompt_accessibility();
            crate::runtime::open("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility");
        });
        let icon = form::icon_tile(f.mtm, "exclamationmark.triangle.fill", Rgba(0.92, 0.3, 0.27, 1.0));
        f.item(
            &icon,
            t("Manca il permesso Accessibilità", "Accessibility isn't granted"),
            t("Privacy e sicurezza → Accessibilità → RustyBar: il tasto parte appena lo accendi", "Privacy & Security → Accessibility → RustyBar: the key starts as soon as it's on"),
            true,
            &[&open],
        );
    }
    let keys = opts(&[
        ("right_cmd", t("⌘ destro", "Right ⌘")),
        ("right_alt", t("⌥ destro", "Right ⌥")),
        ("right_ctrl", t("⌃ destro", "Right ⌃")),
        ("right_shift", t("⇧ destro", "Right ⇧")),
        ("left_cmd", t("⌘ sinistro", "Left ⌘")),
        ("left_alt", t("⌥ sinistro", "Left ⌥")),
        ("left_ctrl", t("⌃ sinistro", "Left ⌃")),
        ("left_shift", t("⇧ sinistro", "Left ⇧")),
    ]);
    f.choice_at(
        t("Tasto", "Key"),
        Some(t("Un tocco avvia una sequenza, anche se la lettera arriva prima del rilascio; tenuto premuto è il modificatore", "A tap starts a sequence, even when the letter comes before the release; held down it's the modifier")),
        &rb("compose.key"),
        keys,
        &d.key,
    );
    f.number_at(
        t("Tocco", "Tap"),
        Some(t(
            "Secondi al massimo fra la pressione e il rilascio, perché sia un tocco. Oltre, è il modificatore: una scorciatoia veloce con una lettera che apre una sequenza (a, e, s…) vuole il tasto giù almeno così",
            "Seconds between the press and the release, at most, for a tap. Past them it's the modifier: a quick shortcut with a letter that begins a sequence (a, e, s…) wants the key down at least this long",
        )),
        &rb("compose.tap"),
        d.tap,
        Num::new(0.1, 1.0, 0.05, 2),
    );
    f.number_at(
        t("Attesa", "Timeout"),
        Some(t("Secondi senza tasti dopo i quali la sequenza è lasciata; 0, mai", "Seconds without a key after which the sequence is dropped; 0, never")),
        &rb("compose.timeout"),
        d.timeout,
        Num::new(0.0, 60.0, 1.0, 0),
    );
    f.toggle_at(
        t("Segno nella barra", "Mark in the bar"),
        Some(t(
            "Una linguetta col segno ⎄ al bordo sinistro di ogni schermo mentre una sequenza aspetta i suoi tasti; scrivendo veloce non fa in tempo a comparire",
            "A tab with the ⎄ sign at the left edge of every screen while a sequence waits for its keys; typing fast, it has no time to show",
        )),
        &rb("compose.mark"),
        d.mark,
    );
    let try_it = f.field("", t("tocca il tasto, poi e", "tap the key, then e"), 250.0, |_| {});
    f.row(t("Prova qui", "Try it here"), None, &try_it);
    f.footer(t(
        "Una sequenza che non esiste è scritta com'è. Esc, un clic o una scorciatoia la lasciano, ⌫ toglie l'ultimo tasto, un secondo tocco la annulla. Non arriva dove macOS protegge la tastiera, come nei campi delle password.",
        "A sequence that doesn't exist is typed as it is. Esc, a click or a shortcut drop it, ⌫ takes its last key back, a second tap cancels it. It doesn't reach where macOS guards the keyboard, as in password fields.",
    ));

    f.group(Some(t("Nuova sequenza", "New sequence")));
    let keys = f.field("", t("tasti, es. ->", "keys, e.g. ->"), 130.0, |_| {});
    let text = f.field("", t("testo, es. →", "text, e.g. →"), 130.0, |_| {});
    let (k, v) = (keys.clone(), text.clone());
    let add = f.button(t("Aggiungi", "Add"), move || {
        let (keys, text) = (read(&k.stringValue().to_string()), v.stringValue().to_string());
        if keys.is_empty() || text.is_empty() {
            return;
        }
        if taken(&keys) {
            super::status(&format!("{} {}", t("C'è già la sequenza", "There's already the sequence"), show(&keys)), true);
            return;
        }
        put(&keys, &text);
        super::refresh_later();
    });
    let arrow = form::label(f.mtm, "→", 13.0, false, true);
    f.line(&[&keys, &arrow, &text], Some(&add));

    let list = engine::listing(&custom());
    let active: Vec<String> = list.iter().filter(|e| !e.text.is_empty()).map(|e| e.keys.clone()).collect();
    f.group(Some(t("Sequenze", "Sequences")));
    for e in list.iter().filter(|e| !e.text.is_empty()) {
        let (old, text) = (e.keys.clone(), e.text.clone());
        let keys = f.field(&show(&e.keys), "", 130.0, move |new| {
            rename(&old, &read(&new), &text);
            super::refresh_later();
        });
        let k = e.keys.clone();
        let text = f.field(&e.text, "", 130.0, move |new| {
            // Emptied, it stays what it was: the − removes it
            if !new.is_empty() {
                put(&k, &new);
            }
            super::refresh_later();
        });
        // What it is: hidden by a shorter one, one RustyBar comes with (changed or not), or the user's
        let (note, hidden) = match (engine::shadow(&e.keys, active.iter().map(String::as_str)), e.default) {
            (Some(by), _) => (format!("{} {}", t("mai: la ferma prima", "never: stopped first by"), show(by)), true),
            (None, Some(was)) if was != e.text => (format!("{} {was}", t("di serie era", "it came as")), false),
            (None, Some(_)) => (t("di serie", "built in").to_string(), false),
            (None, None) => (String::new(), false),
        };
        let note = form::label(f.mtm, &note, 11.0, false, true);
        if hidden {
            note.setTextColor(Some(&objc2_app_kit::NSColor::systemRedColor()));
        }
        let k = e.keys.clone();
        let remove = f.icon_button("minus.circle", t("Togli", "Remove"), move || {
            take(&k);
            super::refresh_later();
        });
        let arrow = form::label(f.mtm, "→", 13.0, false, true);
        f.line(&[&keys, &arrow, &text, &note], Some(&remove));
    }
    f.footer(t(
        "A sinistra i tasti, uno dopo l'altro (␣ è lo spazio), a destra ciò che scrivono, anche più caratteri. Una sequenza finisce appena è completa: e ed e= non possono stare insieme. Quelle di serie non sono nel file: ci vanno solo le tue, quelle che cambi e quelle che togli.",
        "On the left the keys, one after the other (␣ is the space), on the right what they type, several characters too. A sequence ends as soon as it's complete: e and e= can't live together. The built-in ones aren't in the file: only yours, those you change and those you remove go there.",
    ));

    let removed: Vec<&engine::Entry> = list.iter().filter(|e| e.text.is_empty()).collect();
    if !removed.is_empty() {
        f.group(Some(t("Tolte", "Removed")));
        for e in removed {
            let label = form::label(f.mtm, &format!("{}  →  {}", show(&e.keys), e.default.unwrap_or_default()), 13.0, false, true);
            let k = e.keys.clone();
            let back = f.button(t("Rimetti", "Put back"), move || {
                store::remove(&spot(&k));
                super::refresh_later();
            });
            back.setControlSize(NSControlSize::Small);
            f.line(&[&label], Some(&back));
        }
    }
}
