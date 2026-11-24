//! The messages of Slack and Teams for the `slack` and `teams` widgets, from what the two apps
//! notify on this Mac: Notification Center's database (`db2/db` in the group container of
//! `usernoted`, SQLite), read in-process and read-only each time macOS writes it (`wa::watch_files`).
//!
//! A company's workspace gives no token and no app of one's own, and neither app keeps its
//! messages in a file to read; but each tells macOS of every message meant for the user (a
//! direct message, a mention, a reply), with who wrote and the text, and takes its notifications
//! back when the conversation is read. So a notification that is there is a message that waits.
//! What isn't notified isn't here: no history, no pictures, nothing while the app is closed or
//! its notifications are off.
//!
//! macOS keeps that database from every app without Full Disk Access.

use std::path::{Path, PathBuf};

use super::wa::{Chat, Db, Error, Message};

/// The folder of Notification Center's database.
pub fn database() -> PathBuf {
    crate::config::home().join("Library/Group Containers/group.com.apple.usernoted/db2")
}

/// The database and its log, for the watch.
pub const FILES: [&str; 2] = ["db", "db-wal"];

/// The dates of the database count from 2001.
const EPOCH: f64 = 978_307_200.0;

/// How many messages of a conversation the bar keeps.
const MESSAGES: usize = 40;

/// A notification as an app gave it to macOS.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Note {
    /// The row of the database: higher, later.
    pub id: i64,
    /// When it was delivered, as `time::now`.
    pub at: f64,
    pub title: String,
    pub subtitle: String,
    pub body: String,
    /// The app's own name for it.
    pub ident: String,
    /// What the app groups its notifications by, if it does.
    pub thread: String,
    /// The strings of what the app attached for itself (its `userInfo`), by key.
    pub info: Vec<(String, String)>,
}

/// The notifications of the app `bundle`, and of its helpers (`<bundle>.…`), that Notification
/// Center holds now, oldest first.
pub fn read(dir: &Path, bundle: &str) -> Result<Vec<Note>, Error> {
    let mut notes = Vec::new();
    for (id, at, data) in records(dir, bundle)? {
        let Some(plist) = Plist::parse(&data) else { continue };
        let req = plist.get("req");
        let text = |key: &str| req.and_then(|r| r.get(key)).and_then(Plist::string).unwrap_or_default().to_string();
        let at = if at > 0.0 { at } else { plist.get("date").and_then(Plist::number).unwrap_or(0.0) };
        let mut info = Vec::new();
        if let Some(user) = req.and_then(|r| r.get("usda")) {
            user.strings("", &mut info, 0);
        }
        notes.push(Note { id, at: at + EPOCH, title: text("titl"), subtitle: text("subt"), body: text("body"), ident: text("iden"), thread: text("thre"), info });
    }
    notes.sort_by(|a, b| a.at.total_cmp(&b.at).then(a.id.cmp(&b.id)));
    Ok(notes)
}

/// The rows of `bundle`'s apps: id, delivery date (0 without one) and the archived notification.
fn records(dir: &Path, bundle: &str) -> Result<Vec<(i64, f64, Vec<u8>)>, Error> {
    let file = dir.join(FILES[0]);
    match std::fs::File::open(&file) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::Missing),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Err(Error::Denied),
        Err(e) => return Err(Error::Other(e.to_string())),
    }
    let db = Db::open(&file)?;
    let mut rows = Vec::new();
    // The identifier is one of ours, never the user's: no quote in it
    let bundle: String = bundle.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '.').collect();
    let sql = format!(
        "SELECT r.rec_id, coalesce(r.delivered_date, r.request_date, 0), r.data FROM record r JOIN app a ON a.app_id = r.app_id \
         WHERE lower(a.identifier) = '{0}' OR lower(a.identifier) LIKE '{0}.%' ORDER BY r.rec_id",
        bundle.to_lowercase()
    );
    db.each(&sql, &[], |row| rows.push((row.int(0), row.double(1), row.blob(2))))?;
    Ok(rows)
}

/// What the notifications of `bundle` are made of, for a file to look at (`msg trigger
/// slack_dump`): every key, and of the values only those that look like an identifier or a
/// link; names and texts are only counted.
pub fn dump(dir: &Path, bundle: &str) -> String {
    let rows = match records(dir, bundle) {
        Ok(rows) => rows,
        Err(e) => return format!("{e:?}\n"),
    };
    let mut out = format!("{} notifications of {bundle}*\n", rows.len());
    for (id, at, data) in rows {
        out.push_str(&format!("\nrecord {id} at {at:.0}\n"));
        match Plist::parse(&data) {
            Some(plist) => plist.outline(1, &mut out, 0),
            None => out.push_str(&format!("  not a binary plist: {} bytes\n", data.len())),
        }
    }
    out
}

/// A value that tells of a format and not of a person: no spaces, and something of an identifier.
fn plain(s: &str) -> bool {
    s.len() <= 300 && s.is_ascii() && !s.contains(char::is_whitespace) && s.contains(|c: char| c.is_ascii_digit() || matches!(c, ':' | '/' | '_' | '.' | '-'))
}

/// The shape of a text without what it says: letters and digits as `a`, `A` and `9`, the rest as
/// it is, and of a long one only how it starts.
fn mask(s: &str) -> String {
    s.chars().take(48).map(|c| if c.is_numeric() { '9' } else if c.is_uppercase() { 'A' } else if c.is_alphabetic() { 'a' } else if c == '\n' { '⏎' } else { c }).collect()
}

// ---- conversations -------------------------------------------------------------------------------

/// How an app writes a notification: which of its lines is the conversation and which the sender.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum App {
    Slack,
    Teams,
    /// Any other: the title is the conversation, the line under it who wrote.
    Other,
}

/// A notification as a message of a conversation.
#[derive(Clone, Debug, Default, PartialEq)]
struct Shape {
    /// What tells its conversation from the others.
    key: String,
    name: String,
    group: bool,
    sender: String,
    text: String,
    link: Option<String>,
}

/// How Slack titles the notification of a direct message, and of one in a channel or among
/// many, in English and in Italian; any other title is a conversation of its own.
const FROM: [&str; 2] = ["New message from ", "Nuovo messaggio da "];
const IN: [&str; 2] = ["New message in ", "Nuovo messaggio in "];

fn shape(app: App, note: &Note) -> Shape {
    let title = note.title.trim();
    // Who wrote where many do comes before the text
    let said = |body: &str| match body.split_once(": ") {
        Some((who, rest)) if who.chars().count() <= 60 && !who.contains('\n') => (who.to_string(), rest.to_string()),
        _ => (String::new(), body.to_string()),
    };
    match app {
        App::Slack => {
            let (name, group) = match (FROM.iter().find_map(|p| title.strip_prefix(p)), IN.iter().find_map(|p| title.strip_prefix(p))) {
                (Some(name), _) => (name, false),
                (_, Some(name)) => (name, true),
                _ => (title, false),
            };
            let (sender, text) = if group { said(&note.body) } else { (String::new(), note.body.clone()) };
            // `T…_<when>`: the workspace, which is all the app says of where
            let team = note.ident.split('_').next().filter(|t| t.starts_with('T') && t.len() > 4 && t.chars().all(|c| c.is_ascii_alphanumeric()));
            Shape { key: name.to_string(), name: name.to_string(), group, sender, text, link: team.map(|t| format!("slack://open?team={t}")) }
        }
        App::Teams => {
            // The conversation's id is among what the app keeps for a click: `19:…@…`
            let conversation = note.info.iter().map(|(_, v)| v.as_str()).find(|v| v.starts_with("19:") && v.contains('@') && !v.contains(['{', ' ']));
            // Between two people, or a chat of many or a channel
            let group = conversation.is_some_and(|c| !c.ends_with("@unq.gbl.spaces"));
            let (sender, text) = if group { said(&note.body) } else { (String::new(), note.body.clone()) };
            let link = conversation.map(|c| format!("msteams:/l/chat/{}/conversations", c.replace(':', "%3A").replace('@', "%40")));
            Shape { key: conversation.unwrap_or(title).to_string(), name: title.to_string(), group, sender, text, link }
        }
        App::Other => {
            let subtitle = note.subtitle.trim();
            let key = if note.thread.is_empty() { title } else { note.thread.as_str() };
            Shape { key: key.to_string(), name: title.to_string(), group: !subtitle.is_empty(), sender: subtitle.to_string(), text: note.body.clone(), link: None }
        }
    }
}

/// The conversations of an app, made of its notifications: what was notified stays here as a
/// message after it left Notification Center, for as long as the bar runs.
#[derive(Default)]
pub struct Book {
    chats: Vec<Chat>,
    /// When the first notifications were read.
    started: Option<f64>,
}

/// The conversation of what the Dock counts and no notification tells.
pub const REST: &str = "~rest";

/// An id that grows with time, whatever the app.
fn message_id(at: f64, row: i64) -> i64 {
    ((at.max(0.0) as i64) << 20) | (row & 0xF_FFFF)
}

impl Book {
    /// The conversations with what `notes` say: those where something waits first, then the
    /// `recent` latest of the others. An app leaves its notifications in Notification Center
    /// after their messages are read, so what waits are the last `waiting` of them: the number on
    /// the app's Dock icon. Without that number, those that came since the bar started.
    ///
    /// A number higher than the notifications there are (cleared, never shown, or not readable
    /// at all) is one more conversation with the rest and no message, called `rest`.
    pub fn update(&mut self, app: App, notes: &[Note], waiting: Option<usize>, recent: usize, rest: &str) -> Vec<Chat> {
        self.chats.retain(|c| c.jid != REST);
        for chat in &mut self.chats {
            chat.unread = 0;
        }
        let started = *self.started.get_or_insert_with(crate::time::now);
        let first = match waiting {
            Some(n) => notes.len().saturating_sub(n),
            None => notes.iter().position(|n| n.at >= started).unwrap_or(notes.len()),
        };
        for (i, note) in notes.iter().enumerate() {
            let shape = shape(app, note);
            if shape.key.is_empty() {
                continue;
            }
            let at = match self.chats.iter().position(|c| c.jid == shape.key) {
                Some(at) => at,
                None => {
                    self.chats.push(Chat { jid: shape.key.clone(), ..Default::default() });
                    self.chats.len() - 1
                }
            };
            let chat = &mut self.chats[at];
            chat.name = shape.name;
            chat.group = shape.group;
            chat.unread += (i >= first) as i64;
            if shape.link.is_some() {
                chat.link = shape.link;
            }
            let id = message_id(note.at, note.id);
            if !chat.messages.iter().any(|m| m.id == id) {
                chat.messages.push(Message { id, sender: shape.sender, at: note.at, text: shape.text, ..Default::default() });
            }
        }
        for chat in &mut self.chats {
            chat.messages.sort_by_key(|m| m.id);
            let extra = chat.messages.len().saturating_sub(MESSAGES);
            chat.messages.drain(..extra);
            chat.newest = chat.messages.last().map_or(0, |m| m.id);
            chat.at = chat.messages.last().map_or(0.0, |m| m.at);
        }
        if let Some(more) = waiting.map(|n| n.saturating_sub(notes.len())).filter(|n| *n > 0) {
            let at = self.chats.iter().map(|c| c.at).fold(0.0, f64::max);
            self.chats.push(Chat { jid: REST.into(), name: rest.to_string(), unread: more as i64, at, ..Default::default() });
        }
        // Of those read, only the latest stay
        self.chats.sort_by(|a, b| (b.unread > 0).cmp(&(a.unread > 0)).then(b.at.total_cmp(&a.at)));
        let waiting = self.chats.iter().filter(|c| c.unread > 0).count();
        self.chats.truncate(waiting + recent);
        self.chats.clone()
    }
}

// ---- binary property lists -----------------------------------------------------------------------

/// A value of a binary property list (`bplist00`), which is how macOS archives a notification.
#[derive(Clone, Debug, PartialEq)]
pub enum Plist {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Data(Vec<u8>),
    Array(Vec<Plist>),
    Dict(Vec<(String, Plist)>),
}

impl Plist {
    pub fn parse(data: &[u8]) -> Option<Plist> {
        if data.len() < 40 || !data.starts_with(b"bplist00") {
            return None;
        }
        let trailer = &data[data.len() - 32..];
        let (offset_size, ref_size) = (trailer[6] as usize, trailer[7] as usize);
        let count = be(&trailer[8..16])? as usize;
        let top = be(&trailer[16..24])? as usize;
        let table = be(&trailer[24..32])? as usize;
        if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) || count > data.len() {
            return None;
        }
        Reader { data, offset_size, ref_size, count, table }.object(top, 0)
    }

    pub fn get(&self, key: &str) -> Option<&Plist> {
        match self {
            Plist::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn string(&self) -> Option<&str> {
        match self {
            Plist::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn number(&self) -> Option<f64> {
        match self {
            Plist::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Every string in it with the keys that lead there, a list inside a data too.
    fn strings(&self, path: &str, out: &mut Vec<(String, String)>, depth: usize) {
        if depth > 12 || out.len() > 400 {
            return;
        }
        match self {
            Plist::String(s) => out.push((path.to_string(), s.clone())),
            Plist::Data(d) => {
                if let Some(inner) = Plist::parse(d) {
                    inner.strings(path, out, depth + 1);
                }
            }
            Plist::Array(a) => a.iter().for_each(|v| v.strings(path, out, depth + 1)),
            Plist::Dict(d) => d.iter().for_each(|(k, v)| v.strings(&if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, out, depth + 1)),
            _ => {}
        }
    }

    /// Its keys and kinds, a line each (see `dump`).
    fn outline(&self, indent: usize, out: &mut String, depth: usize) {
        let pad = "  ".repeat(indent);
        if depth > 12 {
            return;
        }
        match self {
            Plist::Null => out.push_str("null\n"),
            Plist::Bool(b) => out.push_str(&format!("{b}\n")),
            Plist::Number(n) => out.push_str(&format!("{n}\n")),
            Plist::String(s) if plain(s) => out.push_str(&format!("\"{s}\"\n")),
            Plist::String(s) => out.push_str(&format!("<text of {}, {} lines: {}>\n", s.chars().count(), s.lines().count(), mask(s))),
            Plist::Data(d) => match Plist::parse(d) {
                Some(inner) => {
                    out.push_str("data, a list:\n");
                    out.push_str(&pad);
                    inner.outline(indent, out, depth + 1);
                }
                None => out.push_str(&format!("<data of {}>\n", d.len())),
            },
            Plist::Array(a) => {
                out.push_str(&format!("array of {}\n", a.len()));
                for (i, v) in a.iter().enumerate().take(60) {
                    out.push_str(&format!("{pad}[{i}] "));
                    v.outline(indent + 1, out, depth + 1);
                }
            }
            Plist::Dict(d) => {
                out.push_str("dict\n");
                for (k, v) in d {
                    out.push_str(&format!("{pad}{k}: "));
                    v.outline(indent + 1, out, depth + 1);
                }
            }
        }
    }
}

fn be(bytes: &[u8]) -> Option<u64> {
    (bytes.len() <= 8).then(|| bytes.iter().fold(0u64, |n, b| (n << 8) | *b as u64))
}

struct Reader<'a> {
    data: &'a [u8],
    offset_size: usize,
    ref_size: usize,
    count: usize,
    table: usize,
}

impl Reader<'_> {
    fn bytes(&self, at: usize, n: usize) -> Option<&[u8]> {
        self.data.get(at..at.checked_add(n)?)
    }

    /// The count of a marker: its low bits, or the integer after it.
    fn count(&self, at: usize, low: u8) -> Option<(usize, usize)> {
        if low != 0x0F {
            return Some((low as usize, at + 1));
        }
        let size = 1usize << (self.data.get(at + 1)? & 0x0F);
        Some((be(self.bytes(at + 2, size)?)? as usize, at + 2 + size))
    }

    fn reference(&self, at: usize) -> Option<usize> {
        Some(be(self.bytes(at, self.ref_size)?)? as usize)
    }

    fn object(&self, index: usize, depth: usize) -> Option<Plist> {
        if index >= self.count || depth > 24 {
            return None;
        }
        let at = be(self.bytes(self.table.checked_add(index.checked_mul(self.offset_size)?)?, self.offset_size)?)? as usize;
        let marker = *self.data.get(at)?;
        let low = marker & 0x0F;
        Some(match marker >> 4 {
            0x0 => match low {
                0x8 => Plist::Bool(false),
                0x9 => Plist::Bool(true),
                _ => Plist::Null,
            },
            0x1 => {
                let size = 1usize << low;
                let bytes = self.bytes(at + 1, size)?;
                Plist::Number(if size > 8 { be(&bytes[size - 8..])? as i64 as f64 } else if size == 8 { be(bytes)? as i64 as f64 } else { be(bytes)? as f64 })
            }
            0x2 => match low {
                2 => Plist::Number(f32::from_be_bytes(self.bytes(at + 1, 4)?.try_into().ok()?) as f64),
                3 => Plist::Number(f64::from_be_bytes(self.bytes(at + 1, 8)?.try_into().ok()?)),
                _ => return None,
            },
            0x3 => Plist::Number(f64::from_be_bytes(self.bytes(at + 1, 8)?.try_into().ok()?)),
            0x4 => {
                let (n, start) = self.count(at, low)?;
                Plist::Data(self.bytes(start, n)?.to_vec())
            }
            0x5 => {
                let (n, start) = self.count(at, low)?;
                Plist::String(String::from_utf8_lossy(self.bytes(start, n)?).into_owned())
            }
            0x6 => {
                let (n, start) = self.count(at, low)?;
                let units: Vec<u16> = self.bytes(start, n.checked_mul(2)?)?.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
                Plist::String(String::from_utf16_lossy(&units))
            }
            0x8 => Plist::Number(be(self.bytes(at + 1, low as usize + 1)?)? as f64),
            0xA | 0xC => {
                let (n, start) = self.count(at, low)?;
                let mut items = Vec::new();
                for i in 0..n.min(self.count) {
                    items.push(self.object(self.reference(start + i * self.ref_size)?, depth + 1)?);
                }
                Plist::Array(items)
            }
            0xD => {
                let (n, start) = self.count(at, low)?;
                let n = n.min(self.count);
                let mut items = Vec::new();
                for i in 0..n {
                    let key = self.object(self.reference(start + i * self.ref_size)?, depth + 1)?;
                    let value = self.object(self.reference(start + (n + i) * self.ref_size)?, depth + 1)?;
                    if let Plist::String(key) = key {
                        items.push((key, value));
                    }
                }
                Plist::Dict(items)
            }
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list as `plutil -convert binary1` writes it, from JSON.
    fn binary(json: &str) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!("rustybar-nc-{}-{}", std::process::id(), json.len()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("list.plist");
        std::fs::write(&file, json).unwrap();
        assert!(std::process::Command::new("plutil").args(["-convert", "binary1"]).arg(&file).status().unwrap().success());
        let data = std::fs::read(&file).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        data
    }

    #[test]
    fn a_binary_list_is_read_as_macos_writes_it() {
        let long = "x".repeat(40);
        let data = binary(&format!(r##"{{"req": {{"titl": "#design", "body": "Però: è così ✓", "long": "{long}", "n": 3, "half": 0.5, "yes": true, "list": ["a", "b"]}}, "date": 800000000.25}}"##));
        let plist = Plist::parse(&data).unwrap();
        let req = plist.get("req").unwrap();
        assert_eq!(req.get("titl").and_then(Plist::string), Some("#design"));
        assert_eq!(req.get("body").and_then(Plist::string), Some("Però: è così ✓"));
        assert_eq!(req.get("long").and_then(Plist::string), Some(long.as_str()));
        assert_eq!(req.get("n").and_then(Plist::number), Some(3.0));
        assert_eq!(req.get("half").and_then(Plist::number), Some(0.5));
        assert_eq!(req.get("yes"), Some(&Plist::Bool(true)));
        assert_eq!(req.get("list"), Some(&Plist::Array(vec![Plist::String("a".into()), Plist::String("b".into())])));
        assert_eq!(plist.get("date").and_then(Plist::number), Some(800000000.25));
    }

    #[test]
    fn what_isnt_a_list_is_none_and_not_a_panic() {
        assert_eq!(Plist::parse(b"bplist00"), None);
        assert_eq!(Plist::parse(&[0xFF; 64]), None);
        let mut data = binary(r#"{"a": {"b": ["c", 1]}}"#);
        let n = data.len();
        // Every offset of the trailer and of the table pointed somewhere else
        for i in 8..n {
            let before = data[i];
            data[i] = 0xEE;
            let _ = Plist::parse(&data);
            data[i] = before;
        }
    }

    fn note(id: i64, title: &str, body: &str) -> Note {
        Note { id, at: 1000.0 + id as f64, title: title.into(), body: body.into(), ident: "T0AB12CD3_1700000000.000100".into(), ..Default::default() }
    }

    #[test]
    fn slack_notifications_become_conversations() {
        let mut book = Book::default();
        let notes = [note(1, "New message from Ada Neri", "Hai un minuto?"), note(2, "New message in #design", "Leo Bruni: nuova bozza: guarda"), note(3, "Nuovo messaggio da Ada Neri", "È urgente"), note(4, "Reminder: stand-up", "In 5 minutes")];
        let chats = book.update(App::Slack, &notes, Some(4), 5, "Slack");
        assert_eq!(chats.len(), 3);
        let ada = chats.iter().find(|c| c.name == "Ada Neri").unwrap();
        assert_eq!((ada.unread, ada.messages.len(), ada.group, ada.link.as_deref()), (2, 2, false, Some("slack://open?team=T0AB12CD3")));
        let design = chats.iter().find(|c| c.name == "#design").unwrap();
        assert_eq!((design.group, design.messages[0].sender.as_str(), design.messages[0].text.as_str()), (true, "Leo Bruni", "nuova bozza: guarda"));
        assert!(chats.iter().any(|c| c.name == "Reminder: stand-up" && c.messages[0].text == "In 5 minutes"));
        // The Dock says two wait: the last two, and the rest is read
        let chats = book.update(App::Slack, &notes, Some(2), 5, "Slack");
        let unread = |name: &str| chats.iter().find(|c| c.name == name).unwrap().unread;
        assert_eq!((unread("Ada Neri"), unread("#design"), unread("Reminder: stand-up")), (1, 0, 1));
        // None: all of it is read, and still there to see
        let chats = book.update(App::Slack, &notes, Some(0), 5, "Slack");
        assert!(chats.iter().all(|c| c.unread == 0) && chats.len() == 3);
        // With no room for the read ones, they go
        assert!(book.update(App::Slack, &notes, Some(0), 0, "Slack").is_empty());
    }

    #[test]
    fn without_the_dock_only_what_came_since_the_start_waits() {
        let mut book = Book::default();
        let old = note(1, "New message from Ada Neri", "ieri");
        assert_eq!(book.update(App::Slack, std::slice::from_ref(&old), None, 5, "Slack")[0].unread, 0);
        let new = Note { at: crate::time::now() + 1.0, ..note(2, "New message from Ada Neri", "ora") };
        assert_eq!(book.update(App::Slack, &[old, new], None, 5, "Slack")[0].unread, 1);
    }

    #[test]
    fn teams_conversations_go_by_their_id() {
        let info = |id: &str| vec![("$objects".to_string(), "default_action".to_string()), ("$objects".to_string(), format!("{{\"actionType\":\"quickReply\",\"convId\":\"{id}\"}}")), ("$objects".to_string(), id.to_string())];
        let one = Note { info: info("19:aa_bb@unq.gbl.spaces"), ..note(1, "Ada Neri", "Ciao: ci sei?") };
        let many = Note { info: info("19:cc@thread.v2"), ..note(2, "Ada Neri", "Leo Bruni: eccomi") };
        let chats = Book::default().update(App::Teams, &[one, many], Some(2), 5, "Slack");
        let direct = chats.iter().find(|c| c.jid == "19:aa_bb@unq.gbl.spaces").unwrap();
        assert_eq!((direct.group, direct.messages[0].text.as_str(), direct.link.as_deref()), (false, "Ciao: ci sei?", Some("msteams:/l/chat/19%3Aaa_bb%40unq.gbl.spaces/conversations")));
        let group = chats.iter().find(|c| c.jid == "19:cc@thread.v2").unwrap();
        assert_eq!((group.group, group.messages[0].sender.as_str()), (true, "Leo Bruni"));
    }

    #[test]
    fn what_the_dock_counts_beyond_the_notifications_is_a_conversation_of_its_own() {
        let mut book = Book::default();
        let notes = [Note { subtitle: "Ada Neri".into(), thread: "t1".into(), ..note(1, "Preventivo", "Ecco la bozza") }];
        let chats = book.update(App::Other, &notes, Some(4), 5, "Mail");
        assert_eq!(chats.len(), 2);
        let mail = chats.iter().find(|c| c.jid == "t1").unwrap();
        assert_eq!((mail.name.as_str(), mail.unread, mail.messages[0].sender.as_str()), ("Preventivo", 1, "Ada Neri"));
        let rest = chats.iter().find(|c| c.jid == REST).unwrap();
        assert_eq!((rest.name.as_str(), rest.unread, rest.messages.len()), ("Mail", 3, 0));
        // Not readable at all: only the number
        let chats = Book::default().update(App::Other, &[], Some(2), 5, "Mail");
        assert_eq!((chats.len(), chats[0].unread), (1, 2));
        // And it goes with the number
        assert_eq!(book.update(App::Other, &notes, Some(1), 5, "Mail").len(), 1);
    }

    #[test]
    fn a_dump_keeps_names_and_texts_out() {
        assert!(plain("C024BE91L"));
        assert!(plain("slack://channel?team=T1&id=C2"));
        assert!(!plain("Ada Neri"));
        assert!(!plain("ok"));
        assert!(!plain("perché"));
    }
}
