//! The files the settings window edits. toml_edit keeps their comments and layout: only the value
//! that changes is written again. Each edit reads the file again first if it changed on disk, so
//! an edit made elsewhere while the window is open isn't lost, and writes it in place: a rename
//! would break the watchers that follow the file itself, as SwipeAeroSpace's.
//!
//! RustyBar's keys are read from `rustybar.local.toml` over `rustybar.toml`, as the bar merges
//! them, and written where they are: a key, or a table, of the local file stays there; anything
//! new goes to `rustybar.toml`. A key set back to its default goes away, so that the file keeps
//! only what differs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

pub use toml_edit::Value;
use toml_edit::{ArrayOfTables, DocumentMut, InlineTable, Item, Table};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum File {
    Bar,
    BarLocal,
    AeroSpace,
    Swipe,
}

/// Whose a value is: RustyBar's live in either of its two files.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Tool {
    RustyBar,
    AeroSpace,
    Swipe,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seg {
    K(String),
    I(usize),
}

/// A place in a tool's config: `widgets.home.groups[2].title`.
#[derive(Clone, Debug)]
pub struct Spot {
    pub tool: Tool,
    pub path: Vec<Seg>,
}

impl Spot {
    /// Keys split at the dots: for names without one.
    pub fn new(tool: Tool, path: &str) -> Spot {
        Spot { tool, path: path.split('.').filter(|s| !s.is_empty()).map(|s| Seg::K(s.to_string())).collect() }
    }

    pub fn k(&self, key: &str) -> Spot {
        let mut s = self.clone();
        s.path.push(Seg::K(key.to_string()));
        s
    }

    pub fn i(&self, index: usize) -> Spot {
        let mut s = self.clone();
        s.path.push(Seg::I(index));
        s
    }
}

pub fn rb(path: &str) -> Spot {
    Spot::new(Tool::RustyBar, path)
}

pub fn aero(path: &str) -> Spot {
    Spot::new(Tool::AeroSpace, path)
}

pub fn swipe(path: &str) -> Spot {
    Spot::new(Tool::Swipe, path)
}

/// The folder of made-up configs with `RUSTYBAR_MOCK` (see `mock`): the window edits those, and
/// applies nothing.
pub fn mock_dir() -> Option<PathBuf> {
    std::env::var_os("RUSTYBAR_MOCK").map(|d| PathBuf::from(d).join("settings"))
}

pub fn path(file: File) -> PathBuf {
    let home = crate::config::home();
    let xdg = std::env::var("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|_| home.join(".config"));
    match file {
        File::Bar => crate::config::config_path(),
        File::BarLocal => crate::config::local_path(),
        File::AeroSpace => match mock_dir() {
            Some(d) => d.join("aerospace.toml"),
            // AeroSpace refuses to start with both
            None if home.join(".aerospace.toml").exists() => home.join(".aerospace.toml"),
            None => xdg.join("aerospace/aerospace.toml"),
        },
        // "swipeareospace" (sic) is the folder the app reads
        File::Swipe => match mock_dir() {
            Some(d) => d.join("swipeaerospace.toml"),
            None => home.join(".config/swipeareospace/config.toml"),
        },
    }
}

struct Doc {
    path: PathBuf,
    text: String,
    doc: DocumentMut,
    mtime: Option<SystemTime>,
    /// A file that doesn't parse is never written.
    error: Option<String>,
}

#[derive(Default)]
struct Store {
    docs: HashMap<File, Doc>,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::new(Store::default());
}

fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    STORE.with(|s| f(&mut s.borrow_mut()))
}

/// Forgets every file: the window reads them again when it opens.
pub fn clear() {
    with(|s| s.docs.clear());
}

impl Store {
    fn load(&mut self, file: File) -> &mut Doc {
        let path = path(file);
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let stale = self.docs.get(&file).is_none_or(|d| d.mtime != mtime || d.path != path);
        if stale {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let (doc, error) = match text.parse::<DocumentMut>() {
                Ok(d) => (d, None),
                Err(e) => (DocumentMut::new(), Some(format!("{}: {}", short(&path), e.to_string().trim()))),
            };
            self.docs.insert(file, Doc { path, text, doc, mtime, error });
        }
        self.docs.get_mut(&file).expect("loaded")
    }

    fn find(&mut self, file: File, path: &[Seg]) -> Option<toml::Value> {
        find(self.load(file).doc.as_table(), path).and_then(owned)
    }

    fn exists(&mut self, file: File, path: &[Seg]) -> bool {
        find(self.load(file).doc.as_table(), path).is_some()
    }

    /// The file an edit of `spot` goes to (see the top of the module).
    fn target(&mut self, spot: &Spot) -> File {
        match spot.tool {
            Tool::AeroSpace => File::AeroSpace,
            Tool::Swipe => File::Swipe,
            Tool::RustyBar => {
                let p = &spot.path;
                if self.exists(File::BarLocal, p) {
                    return File::BarLocal;
                }
                if self.exists(File::Bar, p) {
                    return File::Bar;
                }
                // A table of the local file, such as [widgets.home], keeps what's new in it
                for n in (2..p.len()).rev() {
                    if self.exists(File::BarLocal, &p[..n]) {
                        return File::BarLocal;
                    }
                }
                File::Bar
            }
        }
    }

    fn edit<R>(&mut self, spot: &Spot, f: impl FnOnce(&mut DocumentMut, usize) -> R) -> Result<R, String> {
        let file = self.target(spot);
        let headers = headers(spot);
        let d = self.load(file);
        if let Some(e) = &d.error {
            return Err(e.clone());
        }
        let r = f(&mut d.doc, headers);
        let text = d.doc.to_string();
        if text != d.text {
            if let Some(dir) = d.path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(&d.path, &text).map_err(|e| format!("{}: {e}", short(&d.path)))?;
            d.text = text;
            d.mtime = std::fs::metadata(&d.path).and_then(|m| m.modified()).ok();
            super::saved(spot.tool, file);
        }
        Ok(r)
    }
}

/// How many of the first keys of a path are `[headers]` when they have to be made; deeper ones are
/// dotted keys (`outer.top = 8`, `if.app-id = '…'`).
fn headers(spot: &Spot) -> usize {
    let key = |i: usize| match spot.path.get(i) {
        Some(Seg::K(k)) => k.as_str(),
        _ => "",
    };
    match (spot.tool, key(0)) {
        (Tool::RustyBar, "widgets") if key(2) == "printer" => 3,
        (Tool::RustyBar, "widgets") => 2,
        (Tool::RustyBar, "theme") if key(1) == "colors" => 2,
        (Tool::RustyBar, _) => 1,
        (Tool::AeroSpace, "mode") => 3,
        (Tool::AeroSpace, "gaps" | "key-mapping" | "workspace-to-monitor-force-assignment") => 1,
        _ => 0,
    }
}

fn short(path: &std::path::Path) -> String {
    let home = crate::config::home();
    match path.strip_prefix(&home) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn short_path(file: File) -> String {
    short(&path(file))
}

// ---- reading -----------------------------------------------------------------------------------

/// A step through the document: tables hold items, arrays and inline tables hold values.
enum Got<'a> {
    I(&'a Item),
    V(&'a Value),
    T(&'a Table),
}

fn step<'a>(g: Got<'a>, seg: &Seg) -> Option<Got<'a>> {
    match (g, seg) {
        (Got::I(Item::Table(t)), _) => step(Got::T(t), seg),
        (Got::I(Item::Value(v)), _) => step(Got::V(v), seg),
        (Got::I(Item::ArrayOfTables(a)), Seg::I(i)) => a.get(*i).map(Got::T),
        (Got::T(t), Seg::K(k)) => t.get(k).filter(|i| !i.is_none()).map(Got::I),
        (Got::V(Value::InlineTable(t)), Seg::K(k)) => t.get(k).map(Got::V),
        (Got::V(Value::Array(a)), Seg::I(i)) => a.get(*i).map(Got::V),
        _ => None,
    }
}

fn find<'a>(root: &'a Table, path: &[Seg]) -> Option<Got<'a>> {
    let mut g = Got::T(root);
    for seg in path {
        g = step(g, seg)?;
    }
    Some(g)
}

fn owned(g: Got) -> Option<toml::Value> {
    match g {
        Got::I(i) => owned_item(i),
        Got::V(v) => Some(owned_value(v)),
        Got::T(t) => Some(owned_table(t)),
    }
}

fn owned_item(i: &Item) -> Option<toml::Value> {
    match i {
        Item::None => None,
        Item::Value(v) => Some(owned_value(v)),
        Item::Table(t) => Some(owned_table(t)),
        Item::ArrayOfTables(a) => Some(toml::Value::Array(a.iter().map(owned_table).collect())),
    }
}

fn owned_table(t: &Table) -> toml::Value {
    toml::Value::Table(t.iter().filter_map(|(k, i)| owned_item(i).map(|v| (k.to_string(), v))).collect())
}

fn owned_value(v: &Value) -> toml::Value {
    match v {
        Value::String(s) => toml::Value::String(s.value().clone()),
        Value::Integer(i) => toml::Value::Integer(*i.value()),
        Value::Float(f) => toml::Value::Float(*f.value()),
        Value::Boolean(b) => toml::Value::Boolean(*b.value()),
        Value::Datetime(d) => toml::Value::String(d.value().to_string()),
        Value::Array(a) => toml::Value::Array(a.iter().map(owned_value).collect()),
        Value::InlineTable(t) => toml::Value::Table(t.iter().map(|(k, v)| (k.to_string(), owned_value(v))).collect()),
    }
}

/// The value at `spot`. RustyBar's come from the local file over the main one, tables merged as
/// the bar merges them.
pub fn get(spot: &Spot) -> Option<toml::Value> {
    with(|s| match spot.tool {
        Tool::RustyBar => {
            let main = s.find(File::Bar, &spot.path);
            let local = s.find(File::BarLocal, &spot.path);
            match (main, local) {
                (Some(toml::Value::Table(mut m)), Some(toml::Value::Table(l))) => {
                    crate::config::merge(&mut m, l);
                    Some(toml::Value::Table(m))
                }
                (m, l) => l.or(m),
            }
        }
        tool => s.find(if tool == Tool::AeroSpace { File::AeroSpace } else { File::Swipe }, &spot.path),
    })
}

pub fn get_str(spot: &Spot) -> Option<String> {
    get(spot).and_then(|v| v.as_str().map(String::from))
}

pub fn get_f64(spot: &Spot) -> Option<f64> {
    get(spot).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
}

pub fn get_bool(spot: &Spot) -> Option<bool> {
    get(spot).and_then(|v| v.as_bool())
}

/// A list of strings; a single string counts as a list of one.
pub fn get_strings(spot: &Spot) -> Option<Vec<String>> {
    match get(spot)? {
        toml::Value::String(s) => Some(vec![s]),
        toml::Value::Array(a) => Some(a.iter().filter_map(|v| v.as_str().map(String::from)).collect()),
        _ => None,
    }
}

/// How many elements an array (or array of tables) has.
pub fn len(spot: &Spot) -> usize {
    get(spot).and_then(|v| v.as_array().map(Vec::len)).unwrap_or(0)
}

/// The keys of a table, in the file's order.
pub fn keys(spot: &Spot) -> Vec<String> {
    let file = match spot.tool {
        Tool::AeroSpace => File::AeroSpace,
        Tool::Swipe => File::Swipe,
        Tool::RustyBar => File::Bar,
    };
    with(|s| {
        let mut out: Vec<String> = Vec::new();
        let files: &[File] = if spot.tool == Tool::RustyBar { &[File::Bar, File::BarLocal] } else { std::slice::from_ref(&file) };
        for &f in files {
            let keys: Vec<String> = match find(s.load(f).doc.as_table(), &spot.path) {
                Some(Got::I(Item::Table(t))) | Some(Got::T(t)) => t.iter().filter(|(_, i)| !i.is_none()).map(|(k, _)| k.to_string()).collect(),
                Some(Got::I(Item::Value(Value::InlineTable(t)))) | Some(Got::V(Value::InlineTable(t))) => t.iter().map(|(k, _)| k.to_string()).collect(),
                _ => Vec::new(),
            };
            for k in keys {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        out
    })
}

/// The error of a file that doesn't parse, which the window won't write.
pub fn error(tool: Tool) -> Option<String> {
    let files: &[File] = match tool {
        Tool::RustyBar => &[File::Bar, File::BarLocal],
        Tool::AeroSpace => &[File::AeroSpace],
        Tool::Swipe => &[File::Swipe],
    };
    with(|s| files.iter().find_map(|&f| s.load(f).error.clone()))
}

// ---- writing -----------------------------------------------------------------------------------

enum GotMut<'a> {
    I(&'a mut Item),
    V(&'a mut Value),
    T(&'a mut Table),
}

/// One step down; with `make`, a missing table is made: a `[header]` while `header`, else dotted
/// (or inline, inside an inline table).
fn step_mut<'a>(g: GotMut<'a>, seg: &Seg, make: bool, header: bool) -> Option<GotMut<'a>> {
    match g {
        GotMut::I(Item::Table(t)) => step_mut(GotMut::T(t), seg, make, header),
        GotMut::I(Item::Value(v)) => step_mut(GotMut::V(v), seg, make, header),
        GotMut::I(Item::ArrayOfTables(a)) => match seg {
            Seg::I(i) => a.get_mut(*i).map(GotMut::T),
            _ => None,
        },
        GotMut::I(Item::None) => None,
        GotMut::T(t) => {
            let Seg::K(k) = seg else { return None };
            if t.get(k).is_none_or(Item::is_none) {
                if !make {
                    return None;
                }
                let mut new = Table::new();
                if header {
                    new.set_implicit(true);
                } else {
                    new.set_dotted(true);
                }
                t.insert(k, Item::Table(new));
            }
            t.get_mut(k).map(GotMut::I)
        }
        GotMut::V(Value::InlineTable(t)) => {
            let Seg::K(k) = seg else { return None };
            if !t.contains_key(k) {
                if !make {
                    return None;
                }
                t.insert(k, Value::InlineTable(InlineTable::new()));
            }
            t.get_mut(k).map(GotMut::V)
        }
        GotMut::V(Value::Array(a)) => match seg {
            Seg::I(i) => a.get_mut(*i).map(GotMut::V),
            _ => None,
        },
        GotMut::V(_) => None,
    }
}

fn find_mut<'a>(doc: &'a mut DocumentMut, path: &[Seg], make: bool, headers: usize) -> Option<GotMut<'a>> {
    let mut g = GotMut::T(doc.as_table_mut());
    for (n, seg) in path.iter().enumerate() {
        g = step_mut(g, seg, make, n < headers)?;
    }
    Some(g)
}

/// Puts `v` in place of `slot`, keeping its comments and spacing.
fn replace(slot: &mut Value, mut v: Value) {
    // A 'literal' string stays one, when the new text can be written that way
    if let (Value::String(old), Value::String(new)) = (&*slot, &v) {
        let literal = old.as_repr().and_then(|r| r.as_raw().as_str()).is_some_and(|r| r.starts_with('\'') && !r.starts_with("'''"));
        if literal
            && !new.value().contains('\'')
            && let Ok(l) = format!("'{}'", new.value()).parse::<Value>()
        {
            v = l;
        }
    }
    *v.decor_mut() = slot.decor().clone();
    *slot = v;
}

fn set_in(doc: &mut DocumentMut, path: &[Seg], headers: usize, v: Value) {
    let Some((last, parent)) = path.split_last() else { return };
    let Some(g) = find_mut(doc, parent, true, headers) else { return };
    match (g, last) {
        (GotMut::I(Item::Table(t)) | GotMut::T(t), Seg::K(k)) => match t.get_mut(k) {
            Some(Item::Value(old)) => replace(old, v),
            _ => {
                t.insert(k, Item::Value(v));
            }
        },
        (GotMut::I(Item::Value(Value::InlineTable(t))) | GotMut::V(Value::InlineTable(t)), Seg::K(k)) => match t.get_mut(k) {
            Some(old) => replace(old, v),
            None => {
                t.insert(k, v);
                // The spaces of `{ a = 1 }` around the new key
                t.fmt();
            }
        },
        (GotMut::I(Item::Value(Value::Array(a))) | GotMut::V(Value::Array(a)), Seg::I(i)) => {
            if let Some(old) = a.get_mut(*i) {
                replace(old, v);
            }
        }
        _ => {}
    }
}

/// Removes the value at `path`, then the tables it leaves empty (unless they have a comment).
fn remove_in(doc: &mut DocumentMut, path: &[Seg]) {
    for n in (1..=path.len()).rev() {
        let (last, parent) = path[..n].split_last().expect("not empty");
        if n < path.len() {
            // Only an empty table goes, and never an element of an array
            let Seg::K(_) = last else { return };
            let empty = match find_mut(doc, &path[..n], false, 0) {
                Some(GotMut::I(Item::Table(t))) => t.is_empty() && !t.decor().prefix().and_then(|p| p.as_str()).unwrap_or("").contains('#'),
                Some(GotMut::I(Item::Value(Value::InlineTable(t))) | GotMut::V(Value::InlineTable(t))) => t.is_empty(),
                _ => false,
            };
            if !empty {
                return;
            }
        }
        match (find_mut(doc, parent, false, 0), last) {
            (Some(GotMut::I(Item::Table(t)) | GotMut::T(t)), Seg::K(k)) => {
                t.remove(k);
            }
            (Some(GotMut::I(Item::Value(Value::InlineTable(t))) | GotMut::V(Value::InlineTable(t))), Seg::K(k)) => {
                t.remove(k);
                t.fmt();
            }
            (Some(GotMut::I(Item::Value(Value::Array(a))) | GotMut::V(Value::Array(a))), Seg::I(i)) if *i < a.len() => {
                a.remove(*i);
                tidy(a);
            }
            (Some(GotMut::I(Item::ArrayOfTables(a))), Seg::I(i)) if *i < a.len() => {
                a.remove(*i);
            }
            _ => return,
        }
    }
}

/// An array on one line is written again as one after an element goes, so that no stray space or
/// comma stays behind.
fn tidy(a: &mut toml_edit::Array) {
    if !a.to_string().contains('\n') {
        a.fmt();
    }
}

fn report<R>(r: Result<R, String>) -> Option<R> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            super::status(&e, true);
            None
        }
    }
}

/// Writes `v` at `spot`, making the tables on the way.
pub fn set(spot: &Spot, v: Value) {
    report(with(|s| s.edit(spot, |doc, h| set_in(doc, &spot.path, h, v))));
}

/// Writes `v` at `spot`, or, for RustyBar, removes it when it's the default and no other file has
/// it, so that the file keeps only what differs.
pub fn put(spot: &Spot, v: Value, default: Option<&toml::Value>) {
    if spot.tool == Tool::RustyBar
        && let Some(d) = default
        && same(&owned_value(&v), d)
    {
        let other = with(|s| {
            let file = s.target(spot);
            let other = if file == File::Bar { File::BarLocal } else { File::Bar };
            s.exists(other, &spot.path)
        });
        if !other {
            remove(spot);
            return;
        }
    }
    set(spot, v);
}

fn same(a: &toml::Value, b: &toml::Value) -> bool {
    let num = |v: &toml::Value| v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
    match (num(a), num(b)) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

pub fn remove(spot: &Spot) {
    report(with(|s| s.edit(spot, |doc, _| remove_in(doc, &spot.path))));
}

/// Changes the value at `spot` in place; nothing happens if there's none.
pub fn update(spot: &Spot, f: impl FnOnce(&mut Value)) {
    report(with(|s| {
        s.edit(spot, |doc, _| {
            if let Some(GotMut::I(Item::Value(v)) | GotMut::V(v)) = find_mut(doc, &spot.path, false, 0) {
                f(v);
            }
        })
    }));
}

/// Adds `v` at the end of the array at `spot`, which is made if missing.
pub fn push(spot: &Spot, v: Value) {
    report(with(|s| {
        s.edit(spot, |doc, h| {
            if find_mut(doc, &spot.path, false, 0).is_none() {
                set_in(doc, &spot.path, h, Value::Array(toml_edit::Array::new()));
            }
            if let Some(GotMut::I(Item::Value(Value::Array(a))) | GotMut::V(Value::Array(a))) = find_mut(doc, &spot.path, false, 0) {
                // A list written one per line gets one more line
                let multiline = a.iter().last().map(|l| l.decor().prefix().and_then(|p| p.as_str()).unwrap_or("").contains('\n'));
                let mut v = v;
                if multiline == Some(true) {
                    let last = a.iter().last().expect("not empty");
                    *v.decor_mut() = last.decor().clone();
                }
                a.push_formatted(v);
            }
        })
    }));
}

/// Adds a table at the end of the array of tables at `spot` (`[[…]]`), which is made if missing.
pub fn push_table(spot: &Spot, t: Table) {
    report(with(|s| {
        s.edit(spot, |doc, h| {
            let Some((Seg::K(k), parent)) = spot.path.split_last() else { return };
            let Some(GotMut::I(Item::Table(p)) | GotMut::T(p)) = find_mut(doc, parent, true, h) else { return };
            if !matches!(p.get(k), Some(Item::ArrayOfTables(_))) {
                p.insert(k, Item::ArrayOfTables(ArrayOfTables::new()));
            }
            if let Some(Item::ArrayOfTables(a)) = p.get_mut(k) {
                a.push(t);
            }
        })
    }));
}

/// Swaps two elements of the array (or array of tables) at `spot`; the tables keep their place in
/// the file, only what's in them moves.
pub fn swap(spot: &Spot, i: usize, j: usize) {
    report(with(|s| {
        s.edit(spot, |doc, _| match find_mut(doc, &spot.path, false, 0) {
            Some(GotMut::I(Item::Value(Value::Array(a))) | GotMut::V(Value::Array(a))) if i < a.len() && j < a.len() => {
                let (mut vi, mut vj) = (a.get(i).cloned().expect("in range"), a.get(j).cloned().expect("in range"));
                let (di, dj) = (vi.decor().clone(), vj.decor().clone());
                *vi.decor_mut() = dj;
                *vj.decor_mut() = di;
                a.replace_formatted(i, vj);
                a.replace_formatted(j, vi);
            }
            Some(GotMut::I(Item::ArrayOfTables(a))) if i < a.len() && j < a.len() => {
                let (ti, tj) = (a.get(i).cloned().expect("in range"), a.get(j).cloned().expect("in range"));
                let (pi, pj) = (ti.position(), tj.position());
                let (di, dj) = (ti.decor().clone(), tj.decor().clone());
                for (n, mut t, pos, decor) in [(i, tj, pi, di), (j, ti, pj, dj)] {
                    t.set_position(pos);
                    *t.decor_mut() = decor;
                    *a.get_mut(n).expect("in range") = t;
                }
            }
            _ => {}
        })
    }));
}

/// Renames a key of the table at `spot`, keeping its place and its comments. False if the new
/// name is taken.
pub fn rename(spot: &Spot, old: &str, new: &str) -> bool {
    if old == new {
        return true;
    }
    let done = report(with(|s| {
        s.edit(spot, |doc, _| {
            let Some(GotMut::I(Item::Table(t)) | GotMut::T(t)) = find_mut(doc, &spot.path, false, 0) else { return false };
            if t.contains_key(new) || !t.contains_key(old) {
                return false;
            }
            let entries: Vec<(toml_edit::Key, Item)> =
                t.iter().filter_map(|(k, _)| t.get_key_value(k).map(|(k, v)| (k.clone(), v.clone()))).collect();
            t.clear();
            for (key, item) in entries {
                if key.get() == old {
                    let mut renamed = toml_edit::Key::new(new);
                    *renamed.leaf_decor_mut() = key.leaf_decor().clone();
                    t.insert_formatted(&renamed, item);
                } else {
                    t.insert_formatted(&key, item);
                }
            }
            true
        })
    }));
    done == Some(true)
}

pub fn strings<S: AsRef<str>>(list: &[S]) -> Value {
    Value::Array(list.iter().map(|s| s.as_ref()).collect())
}

/// A number as AeroSpace and the bar expect it: whole when it has no decimals.
pub fn number(v: f64, decimals: u8) -> Value {
    if decimals == 0 { Value::from(v.round() as i64) } else { Value::from((v * 10f64.powi(decimals as i32)).round() / 10f64.powi(decimals as i32)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &str) -> DocumentMut {
        s.parse().unwrap()
    }

    fn path(s: &str) -> Vec<Seg> {
        s.split('.').map(|p| p.parse::<usize>().map(Seg::I).unwrap_or_else(|_| Seg::K(p.into()))).collect()
    }

    #[test]
    fn set_keeps_comments() {
        let mut d = doc("# top\n[bar]\nheight = 40   # the bar\n");
        set_in(&mut d, &path("bar.height"), 1, Value::from(44));
        assert_eq!(d.to_string(), "# top\n[bar]\nheight = 44   # the bar\n");
        // 'Literal' strings stay literal, unless the new text has a quote
        let mut d = doc("cmd = ['a', 'b']\n");
        set_in(&mut d, &[Seg::K("cmd".into()), Seg::I(0)], 0, Value::from("it's"));
        set_in(&mut d, &[Seg::K("cmd".into()), Seg::I(1)], 0, Value::from("c d"));
        assert_eq!(d.to_string(), "cmd = [\"it's\", 'c d']\n");
        // Keys added to and removed from an inline table keep its spaces
        let mut d = doc("e = [{ id = \"a\", name = \"A\" }]\n");
        set_in(&mut d, &[Seg::K("e".into()), Seg::I(0), Seg::K("icon".into())], 0, Value::from("i"));
        assert_eq!(d.to_string(), "e = [{ id = \"a\", name = \"A\", icon = \"i\" }]\n");
        remove_in(&mut d, &[Seg::K("e".into()), Seg::I(0), Seg::K("icon".into())]);
        assert_eq!(d.to_string(), "e = [{ id = \"a\", name = \"A\" }]\n");
    }

    #[test]
    fn set_makes_headers_and_dotted_keys() {
        let mut d = doc("[layout]\nleft = []\n");
        set_in(&mut d, &path("widgets.audio.sliders"), 2, Value::from(false));
        set_in(&mut d, &path("widgets.home.printer.plug"), 3, Value::from("switch.p"));
        assert_eq!(d.to_string(), "[layout]\nleft = []\n\n[widgets.audio]\nsliders = false\n\n[widgets.home.printer]\nplug = \"switch.p\"\n");
        let mut d = doc("[gaps]\ninner.horizontal = 14\n");
        set_in(&mut d, &path("gaps.outer.top"), 1, Value::from(8));
        assert_eq!(d.to_string(), "[gaps]\ninner.horizontal = 14\nouter.top = 8\n");
    }

    #[test]
    fn remove_prunes_empty_tables() {
        let mut d = doc("[layout]\nleft = []\n\n[widgets.audio]\nsliders = false\n");
        remove_in(&mut d, &path("widgets.audio.sliders"));
        assert_eq!(d.to_string(), "[layout]\nleft = []\n");
        let mut d = doc("x = [\"a\", \"b\", \"c\"]\n");
        remove_in(&mut d, &path("x.0"));
        assert_eq!(d.to_string(), "x = [\"b\", \"c\"]\n");
    }

    #[test]
    fn reads_through_arrays_of_tables() {
        let d = doc("[[on-window-detected]]\nif.app-id = 'a'\nrun = 'x'\n\n[[on-window-detected]]\nif.app-id = 'b'\nrun = ['y', 'z']\n");
        let v = find(d.as_table(), &path("on-window-detected.1.if.app-id")).and_then(owned);
        assert_eq!(v, Some(toml::Value::String("b".into())));
        let v = find(d.as_table(), &path("on-window-detected.1.run.1")).and_then(owned);
        assert_eq!(v, Some(toml::Value::String("z".into())));
    }

    #[test]
    fn gap_per_monitor_prints_dotted() {
        let mut inner = InlineTable::new();
        inner.insert("built-in", Value::from(4));
        inner.set_dotted(true);
        let mut outer = InlineTable::new();
        outer.insert("monitor", Value::InlineTable(inner));
        let mut quoted = InlineTable::new();
        quoted.insert("Monitor 27", Value::from(40));
        quoted.set_dotted(true);
        let mut outer2 = InlineTable::new();
        outer2.insert("monitor", Value::InlineTable(quoted));
        let a: toml_edit::Array = [Value::InlineTable(outer), Value::InlineTable(outer2), Value::from(42)].into_iter().collect();
        let mut d = doc("[gaps]\nouter.top = 42\n");
        set_in(&mut d, &path("gaps.outer.top"), 1, Value::Array(a));
        assert_eq!(d.to_string(), "[gaps]\nouter.top = [{ monitor.built-in = 4 }, { monitor.\"Monitor 27\" = 40 }, 42]\n");
    }
}
