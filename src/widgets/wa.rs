//! WhatsApp's chats for the `whatsapp` widget, from the database its Mac app keeps in its group
//! container (`ChatStorage.sqlite`, Core Data on SQLite): read in-process with the system's
//! SQLite, read-only, each time the app writes it (see `watch`). Nothing is asked of WhatsApp's
//! servers: the app must be installed and linked, and what it hasn't downloaded isn't there.
//! macOS asks once whether RustyBar may read the data of other apps.
//!
//! And the way back: a reply typed in the bar is sent through the app with Accessibility (see
//! `send`), as nothing else writes to a chat.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::i18n::t;

/// What a message is, as the bar draws it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Text,
    Image,
    Video,
    /// A voice note, or an audio file.
    Voice,
    Contact,
    Location,
    /// A text with the preview of its link.
    Link,
    File,
    Gif,
    Sticker,
    Deleted,
    Poll,
    /// A photo, video or voice note to see once: the Mac has none of it, only that it came.
    Once,
    /// One the bar doesn't know: it says so.
    Other,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Message {
    pub id: i64,
    pub mine: bool,
    /// Who wrote it, in a group.
    pub sender: String,
    /// When, as `time::now`.
    pub at: f64,
    pub kind: Kind,
    /// The text, or the caption of a picture.
    pub text: String,
    /// The picture, the voice note, the document, if the app has downloaded it.
    pub file: Option<String>,
    /// A small picture of it, which the app has before the file.
    pub thumb: Option<String>,
    /// How long a voice note or a video is.
    pub seconds: f64,
    /// The title of a link's page, the name of a document or of a contact.
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Chat {
    /// `<number>@s.whatsapp.net`, `<id>@g.us` for a group.
    pub jid: String,
    pub name: String,
    pub group: bool,
    /// As WhatsApp counts them.
    pub unread: i64,
    pub muted: bool,
    /// When its last message came.
    pub at: f64,
    pub picture: Option<String>,
    /// The last ones, oldest first.
    pub messages: Vec<Message>,
    /// The id of its last message, of any kind.
    pub newest: i64,
    /// What opens it in its app, where the app says.
    pub link: Option<String>,
}

impl Chat {
    /// The number to open the chat with a link, for one with a person.
    pub fn phone(&self) -> Option<&str> {
        self.jid.strip_suffix("@s.whatsapp.net").filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// No database: WhatsApp isn't installed, or never opened.
    Missing,
    /// macOS doesn't let RustyBar read it.
    Denied,
    Other(String),
}

pub const BUNDLE: &str = "net.whatsapp.WhatsApp";

/// WhatsApp's group container.
pub fn container() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Group Containers/group.net.whatsapp.WhatsApp.shared")
}

/// How many messages of a chat the bar keeps.
const MESSAGES: i64 = 40;

/// Core Data's dates count from 2001.
const EPOCH: f64 = 978_307_200.0;

/// The chats with something unread, then the `recent` latest of the others, each with its last
/// messages. Archived, hidden and removed chats, broadcasts, statuses and channels are left out.
pub fn read(dir: &Path, recent: usize) -> Result<Vec<Chat>, Error> {
    let file = dir.join("ChatStorage.sqlite");
    // Opening a file that isn't there, or that macOS keeps from us, gives SQLite's generic errors
    match std::fs::File::open(&file) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Error::Missing),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Err(Error::Denied),
        Err(e) => return Err(Error::Other(e.to_string())),
    }
    let db = Db::open(&file)?;
    let mut chats = Vec::new();
    let mut ids = Vec::new();
    let sql = "SELECT s.Z_PK, s.ZCONTACTJID, s.ZPARTNERNAME, s.ZSESSIONTYPE, s.ZUNREADCOUNT, s.ZLASTMESSAGEDATE, \
               (SELECT p.ZPATH FROM ZWAPROFILEPICTUREITEM p WHERE p.ZJID = s.ZCONTACTJID), \
               (SELECT max(c.ZMUTEDATE) FROM ZWACHATPROPERTIES c WHERE c.ZCHATSESSION = s.Z_PK), \
               (SELECT max(m.Z_PK) FROM ZWAMESSAGE m WHERE m.ZCHATSESSION = s.Z_PK) \
               FROM ZWACHATSESSION s \
               WHERE s.ZSESSIONTYPE IN (0, 1) AND s.ZREMOVED = 0 AND s.ZHIDDEN = 0 AND s.ZARCHIVED = 0 AND s.ZLASTMESSAGEDATE IS NOT NULL \
               ORDER BY (s.ZUNREADCOUNT > 0) DESC, s.ZLASTMESSAGEDATE DESC LIMIT ?1";
    let now = crate::time::now();
    db.each(sql, &[Value::Int(60)], |row| {
        let unread = row.int(4).max(0);
        if unread == 0 && chats.iter().filter(|c: &&Chat| c.unread == 0).count() >= recent {
            return;
        }
        ids.push(row.int(0));
        chats.push(Chat {
            jid: row.text(1),
            name: row.text(2),
            group: row.int(3) == 1,
            unread,
            // Muted until a date: for ever is one far away
            muted: row.double(7) + EPOCH > now,
            at: row.double(5) + EPOCH,
            picture: picture(dir, &row.text(6)),
            messages: Vec::new(),
            newest: row.int(8),
            link: None,
        });
    })?;
    // 6 and 10 are what the app says itself (who joined, a missed call); the rest without a text
    // or a file is dropped below
    let sql = "SELECT m.Z_PK, m.ZISFROMME, m.ZMESSAGEDATE, m.ZMESSAGETYPE, m.ZTEXT, \
               coalesce(g.ZCONTACTNAME, (SELECT n.ZPUSHNAME FROM ZWAPROFILEPUSHNAME n WHERE n.ZJID = g.ZMEMBERJID), m.ZPUSHNAME, g.ZMEMBERJID), \
               i.ZMEDIALOCALPATH, i.ZXMPPTHUMBPATH, i.ZMOVIEDURATION, i.ZTITLE, i.ZMEDIAURL, i.ZVCARDNAME \
               FROM ZWAMESSAGE m \
               LEFT JOIN ZWAMEDIAITEM i ON m.ZMEDIAITEM = i.Z_PK \
               LEFT JOIN ZWAGROUPMEMBER g ON m.ZGROUPMEMBER = g.Z_PK \
               WHERE m.ZCHATSESSION = ?1 AND m.ZMESSAGETYPE NOT IN (6, 10) \
               ORDER BY m.ZMESSAGEDATE DESC, m.Z_PK DESC LIMIT ?2";
    for (chat, id) in chats.iter_mut().zip(ids) {
        let group = chat.group;
        let messages = &mut chat.messages;
        db.each(sql, &[Value::Int(id), Value::Int(MESSAGES)], |row| {
            let kind = kind(row.int(3));
            let media = dir.join("Message");
            let existing = |path: String| Some(media.join(path)).filter(|p| !p.as_os_str().is_empty() && p.is_file()).map(|p| p.to_string_lossy().into_owned());
            let mut m = Message {
                id: row.int(0),
                mine: row.int(1) != 0,
                sender: if group { sender(&row.text(5)) } else { String::new() },
                at: row.double(2) + EPOCH,
                kind,
                text: row.text(4),
                file: Some(row.text(6)).filter(|p| !p.is_empty()).and_then(existing),
                thumb: Some(row.text(7)).filter(|p| !p.is_empty()).and_then(existing),
                seconds: row.double(8),
                title: row.text(9),
                url: String::new(),
            };
            match kind {
                // A link's page; a file's own address is WhatsApp's, of no use here
                Kind::Link => m.url = row.text(10),
                Kind::Contact => m.title = row.text(11),
                // A length there is something else
                Kind::Image | Kind::Sticker | Kind::File => m.seconds = 0.0,
                _ => {}
            }
            // A document's name is its text, when it has no caption
            if kind == Kind::File && m.title.is_empty() {
                m.title = std::mem::take(&mut m.text);
            }
            if kind == Kind::Other && m.text.is_empty() {
                return;
            }
            messages.push(m);
        })?;
        messages.reverse();
    }
    Ok(chats)
}

/// The kinds of `ZMESSAGETYPE` the bar knows.
fn kind(n: i64) -> Kind {
    match n {
        0 => Kind::Text,
        1 => Kind::Image,
        2 => Kind::Video,
        3 => Kind::Voice,
        4 => Kind::Contact,
        5 => Kind::Location,
        7 => Kind::Link,
        8 => Kind::File,
        11 => Kind::Gif,
        12 => Kind::Once,
        14 => Kind::Deleted,
        15 => Kind::Sticker,
        46 => Kind::Poll,
        _ => Kind::Other,
    }
}

/// A member with no name is a jid: its number.
fn sender(name: &str) -> String {
    match name.split_once('@') {
        Some((number, _)) if number.bytes().all(|b| b.is_ascii_digit()) => format!("+{number}"),
        Some((id, _)) => id.to_string(),
        None => name.to_string(),
    }
}

/// The picture of a chat: the path the database has, with the file's own ending.
fn picture(dir: &Path, path: &str) -> Option<String> {
    if !path.starts_with("Media/") {
        return None;
    }
    ["jpg", "thumb"].iter().map(|ext| dir.join(format!("{path}.{ext}"))).find(|p| p.is_file()).map(|p| p.to_string_lossy().into_owned())
}

/// A watch on the app's database, for as long as it lives (see `watch`).
pub struct Watch {
    /// Closed with the watch: the thread sees it and ends.
    _stop: std::os::fd::OwnedFd,
}

/// Calls `changed` (on a thread of its own) each time WhatsApp writes its chats, until the watch
/// is dropped.
///
/// With kqueue on the database and its log themselves: the app keeps them open and writes into
/// them, and FSEvents tells of a file only once it's closed, so a watch on the folder saw nothing
/// until the app quit. The folder is watched too, for the log that the app makes and deletes.
pub fn watch(dir: &Path, changed: impl Fn() + Send + 'static) -> Option<Watch> {
    watch_files(dir, ["ChatStorage.sqlite", "ChatStorage.sqlite-wal"], changed)
}

/// `watch` for any database of another process: `names` are the database and its log, in `dir`.
pub fn watch_files(dir: &Path, names: [&'static str; 2], changed: impl Fn() + Send + 'static) -> Option<Watch> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut ends = [0; 2];
    if unsafe { libc::pipe(ends.as_mut_ptr()) } != 0 {
        return None;
    }
    let (stopped, stop) = unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    let dir = dir.to_path_buf();
    // All of it on the thread: opening anything in the folder of another app waits for the
    // user's answer to macOS, the first time
    crate::runtime::spawn("database-watch", move || {
        const GONE: u32 = libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE;
        let event_only = |path: &Path| -> Option<OwnedFd> {
            let path = CString::new(path.to_string_lossy().as_bytes()).ok()?;
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
            (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
        };
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return;
        }
        let queue = unsafe { OwnedFd::from_raw_fd(queue) };
        let add = |fd: &OwnedFd, filter: i16, what: u32| {
            let ev = libc::kevent { ident: fd.as_raw_fd() as usize, filter, flags: libc::EV_ADD | libc::EV_CLEAR, fflags: what, data: 0, udata: std::ptr::null_mut() };
            unsafe { libc::kevent(queue.as_raw_fd(), &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) == 0 }
        };
        let Some(folder) = event_only(&dir) else {
            crate::log(&format!("can't watch {}", dir.display()));
            return;
        };
        if !add(&folder, libc::EVFILT_VNODE, libc::NOTE_WRITE) || !add(&stopped, libc::EVFILT_READ, 0) {
            return;
        }
        // Closing a file takes it off the queue
        let mut files: [Option<OwnedFd>; 2] = [None, None];
        loop {
            // The files that are there now and aren't watched yet: at first, and after the app
            // made its log again
            for (file, name) in files.iter_mut().zip(names) {
                if file.is_none() {
                    *file = event_only(&dir.join(name)).filter(|fd| add(fd, libc::EVFILT_VNODE, libc::NOTE_WRITE | libc::NOTE_EXTEND | GONE));
                }
            }
            let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
            let n = unsafe { libc::kevent(queue.as_raw_fd(), std::ptr::null(), 0, &mut ev, 1, std::ptr::null()) };
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            if n <= 0 || ev.filter == libc::EVFILT_READ {
                return;
            }
            let file = files.iter_mut().find(|f| f.as_ref().is_some_and(|fd| fd.as_raw_fd() as usize == ev.ident));
            match file {
                Some(file) => {
                    if ev.fflags & GONE != 0 {
                        *file = None;
                    }
                    changed();
                }
                // The folder: a file came or went. News only if it's one of the database's
                // that wasn't there
                None => {
                    if files.iter().zip(names).any(|(file, name)| file.is_none() && dir.join(name).exists()) {
                        changed();
                    }
                }
            }
        }
    });
    Some(Watch { _stop: stop })
}

// ---- the app, through Accessibility -------------------------------------------------------------

/// Sends `text` to the chat called `name` through WhatsApp's own window, which must be open: the
/// app comes to the front for a moment, as it takes keys only there (a text set through
/// Accessibility is in its field, but the app doesn't know, and shows no button to send it); the
/// chat is opened there (so the app marks it read), the text typed with keys sent to its process
/// alone, and its send button pressed; then the app that was in front is back. Nothing is sent
/// unless the app shows that very chat, its field was empty and has that very text. A chat with a
/// person (`phone`) that isn't in the app's list is opened by its link. The error says what to
/// do, in words for the popup.
pub fn send(name: &str, phone: Option<&str>, text: &str) -> Result<(), String> {
    let before = front();
    open_app();
    let result = type_and_send(name, phone, text);
    // Back to where the user was, whatever happened
    if let Some(url) = before.filter(|b| !b.contains("WhatsApp.app")) {
        open(&url);
        // Until it is: the bar's own field takes the keyboard again only after that
        wait(2.0, || front().as_deref() == Some(url.as_str()));
    }
    result
}

fn type_and_send(name: &str, phone: Option<&str>, text: &str) -> Result<(), String> {
    let window = reach(name, phone, true)?;
    let missing = || t("Non trovo il campo di testo di WhatsApp: la sua finestra è cambiata.", "Can't find WhatsApp's text field: its window has changed.").to_string();
    let composer = ax::find(&window, COMPOSER).ok_or_else(missing)?;
    if !ax::string(&composer, "AXValue").trim().is_empty() {
        return Err(t("In WhatsApp c'è già un testo scritto in questa chat: invialo o cancellalo lì.", "WhatsApp already has a text typed in this chat: send it or clear it there.").into());
    }
    let pid = ax::pid().ok_or_else(missing)?;
    let not_taken = || t("WhatsApp non ha preso il testo: aprila e scrivi lì.", "WhatsApp didn't take the text: open it and type there.").to_string();
    // The field takes the keys once the app is in front
    if !wait(3.0, || {
        ax::set_bool(&composer, "AXFocused", true);
        ax::bool(&composer, "AXFocused")
    }) {
        return Err(not_taken());
    }
    keys::text(pid, text);
    // What's there is what was typed, and the app shows its button to send it: the one that
    // wasn't there beside the field
    let bar = ax::parent(&composer).unwrap_or_else(|| window.clone());
    let typed = wait(2.0, || ax::string(&composer, "AXValue") == text);
    let button = || ax::buttons(&bar).into_iter().find(|(id, _)| id == SEND).map(|(_, b)| b);
    if !typed || wait_for(1.5, button).is_none() {
        keys::clear(pid);
        return Err(not_taken());
    }
    // A press on the button as it comes in is lost: a moment first, and again while the text is
    // still there (sent, the field is empty: never twice). The second time with Return, which
    // sends too and doesn't depend on the button
    for attempt in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(350));
        // Still that chat and that text, a moment before sending
        if !on_chat(&window, name, phone) || ax::string(&composer, "AXValue") != text {
            keys::clear(pid);
            return Err(t("WhatsApp ha cambiato chat: non ho inviato niente.", "WhatsApp moved to another chat: nothing was sent.").into());
        }
        let Some(button) = button() else { break };
        if attempt == 1 {
            keys::enter(pid);
        } else {
            ax::press(&button);
        }
        if wait(1.2, || ax::string(&composer, "AXValue").is_empty()) {
            return Ok(());
        }
    }
    if ax::string(&composer, "AXValue").is_empty() {
        return Ok(());
    }
    keys::clear(pid);
    Err(t("WhatsApp non ha inviato: riprova, o scrivi lì.", "WhatsApp didn't send it: try again, or type there.").into())
}

/// The app in front, as the path of its bundle, asked on the main thread, which knows.
fn front() -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    crate::runtime::on_main(move || {
        let app = objc2_app_kit::NSWorkspace::sharedWorkspace().frontmostApplication();
        let _ = tx.send(app.and_then(|a| a.bundleURL()).and_then(|u| u.path()).map(|p| p.to_string()));
    });
    rx.recv_timeout(std::time::Duration::from_secs(1)).ok().flatten()
}

/// Keys for one process alone, whatever app is in front.
mod keys {
    use std::ffi::c_void;

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
        fn CGEventKeyboardSetUnicodeString(event: *mut c_void, length: usize, string: *const u16);
        fn CGEventSetFlags(event: *mut c_void, flags: u64);
        fn CGEventPostToPid(pid: i32, event: *mut c_void);
        fn CFRelease(cf: *const c_void);
    }

    const COMMAND: u64 = 1 << 20;

    /// The key `code` down and up, with `flags`; with `units`, it types them instead of its own
    /// letter.
    fn key(pid: i32, code: u16, flags: u64, units: &[u16]) {
        for down in [true, false] {
            unsafe {
                let event = CGEventCreateKeyboardEvent(std::ptr::null(), code, down);
                if event.is_null() {
                    return;
                }
                CGEventSetFlags(event, flags);
                if !units.is_empty() {
                    CGEventKeyboardSetUnicodeString(event, units.len(), units.as_ptr());
                }
                CGEventPostToPid(pid, event);
                CFRelease(event);
            }
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
    }

    /// Types `text`, a few characters a key (an event carries twenty UTF-16 units at most).
    pub fn text(pid: i32, text: &str) {
        let mut units: Vec<u16> = Vec::new();
        for c in text.chars() {
            let mut buffer = [0u16; 2];
            let encoded = c.encode_utf16(&mut buffer);
            if units.len() + encoded.len() > 16 {
                key(pid, 0, 0, &units);
                units.clear();
            }
            units.extend_from_slice(encoded);
        }
        if !units.is_empty() {
            key(pid, 0, 0, &units);
        }
    }

    /// Return, which sends what's in the field.
    pub fn enter(pid: i32) {
        key(pid, 36, 0, &[]);
    }

    /// Empties the field with the keyboard: ⌘A, delete.
    pub fn clear(pid: i32) {
        key(pid, 0, COMMAND, &[]);
        key(pid, 51, 0, &[]);
    }
}

/// Opens the chat called `name` in the app: at the `front` to stay there, or only for the moment
/// it takes the app to mark the chat read.
pub fn show(name: &str, phone: Option<&str>, front: bool) -> Result<(), String> {
    if front {
        // A person's chat has a link, which also opens a closed app; a group's doesn't
        match phone {
            Some(phone) => open(&format!("whatsapp://send?phone={phone}")),
            None => {
                let reached = reach(name, None, false);
                open_app();
                reached?;
            }
        }
        return Ok(());
    }
    // The app marks a chat read only while it is in front: there for a moment, as for a reply,
    // until its list has the chat without a count, then back to the app that was in front
    let before = self::front();
    open_app();
    let result = reach(name, phone, true).and_then(|window| {
        let counted = || {
            ax::find(&window, CHATS).is_some_and(|list| ax::elements(&list, "AXChildren").iter().any(|r| {
                let shown = ax::string(r, "AXDescription");
                row_of(&shown, name, phone) && !same(&shown, name, phone)
            }))
        };
        if wait(3.0, || !counted()) { Ok(()) } else { Err(t("WhatsApp non ha segnato la chat come letta: aprila lì.", "WhatsApp didn't mark the chat read: open it there.").to_string()) }
    });
    if let Some(url) = before.filter(|b| !b.contains("WhatsApp.app")) {
        open(&url);
    }
    result
}

const COMPOSER: &str = "ChatBar_ComposerTextView";
const SEND: &str = "ChatBar_SendButton";
const CHATS: &str = "ChatListView_TableView";
const HEADER: &str = "NavigationBar_HeaderViewButton";

fn open(url: &str) {
    let url = url.to_string();
    crate::runtime::on_main(move || crate::runtime::open(&url));
}

fn open_app() {
    crate::runtime::on_main(|| crate::runtime::open_app(BUNDLE));
}

/// A name as the app's window has it, without the marks of the writing direction around it.
fn plain(name: &str) -> &str {
    name.trim_matches(|c: char| c.is_whitespace() || matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
}

/// Whether the app calls `shown` the chat called `name` here: by that name, or, for a person the
/// app shows by their number (one not in the contacts, or oneself), by the number of `phone`.
fn same(shown: &str, name: &str, phone: Option<&str>) -> bool {
    let shown = plain(shown);
    if shown == plain(name) {
        return true;
    }
    let number = shown.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | ' ' | '-' | '(' | ')' | '\u{a0}' | '\u{202f}'));
    let digits: String = shown.chars().filter(char::is_ascii_digit).collect();
    number && !digits.is_empty() && phone == Some(digits.as_str())
}

/// Whether a row of the app's list, which it calls `shown`, is the chat called `name`: a chat
/// with something unread is called with the count after its name ("Dana, 2 unread messages", in
/// the app's language, with a direction mark before the number).
fn row_of(shown: &str, name: &str, phone: Option<&str>) -> bool {
    if same(shown, name, phone) {
        return true;
    }
    // The count is cut where it starts, the last comma before a number: the name may have commas
    shown.rmatch_indices(',').any(|(at, _)| plain(&shown[at + 1..]).starts_with(|c: char| c.is_ascii_digit()) && same(&shown[..at], name, phone))
}

/// Whether the app's window shows the chat called `name`, or of `phone`.
fn on_chat(window: &ax::El, name: &str, phone: Option<&str>) -> bool {
    ax::find(window, HEADER).is_some_and(|h| same(&ax::string(&h, "AXDescription"), name, phone))
}

/// Until `done`, for `secs` at most.
fn wait(secs: f64, mut done: impl FnMut() -> bool) -> bool {
    wait_for(secs, || done().then_some(())).is_some()
}

fn wait_for<T>(secs: f64, mut get: impl FnMut() -> Option<T>) -> Option<T> {
    let until = std::time::Instant::now() + std::time::Duration::from_secs_f64(secs);
    loop {
        if let Some(v) = get() {
            return Some(v);
        }
        if std::time::Instant::now() >= until {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(60));
    }
}

/// The app's window on the chat called `name`: already there, or after a press on its row in the
/// list; with `link`, for a person's chat that has no row there, after its link (which brings the
/// app to the front).
fn reach(name: &str, phone: Option<&str>, link: bool) -> Result<ax::El, String> {
    if !ax::trusted() {
        return Err(t("RustyBar non ha il permesso Accessibilità, che serve per scrivere in WhatsApp: Impostazioni, Privacy e sicurezza, Accessibilità.", "RustyBar lacks the Accessibility permission, which it needs to type in WhatsApp: Settings, Privacy & Security, Accessibility.").into());
    }
    let closed = || t("WhatsApp non è aperta: aprila, con la sua finestra.", "WhatsApp isn't open: open it, with its window.").to_string();
    // Brought to the front, the app starts if it wasn't running and opens its window again if
    // it was closed: a moment for that
    let window = wait_for(if link { 5.0 } else { 0.0 }, || ax::elements(&ax::app()?, "AXWindows").into_iter().next()).ok_or_else(closed)?;
    if on_chat(&window, name, phone) {
        return Ok(window);
    }
    let rows: Vec<ax::El> = ax::find(&window, CHATS)
        .map(|list| ax::elements(&list, "AXChildren").into_iter().filter(|r| ax::string(r, "AXRole") == "AXButton" && row_of(&ax::string(r, "AXDescription"), name, phone)).collect())
        .unwrap_or_default();
    match (rows.as_slice(), phone.filter(|_| link)) {
        ([row], _) => ax::press(row),
        ([], Some(phone)) => open(&format!("whatsapp://send?phone={phone}")),
        ([], None) => return Err(t("Non trovo questa chat nell'elenco di WhatsApp: aprila lì.", "Can't find this chat in WhatsApp's list: open it there.").into()),
        _ => return Err(t("In WhatsApp più chat hanno questo nome: aprila lì, per non sbagliare.", "More chats have this name in WhatsApp: open it there, to be sure.").into()),
    }
    if wait(3.0, || on_chat(&window, name, phone)) {
        Ok(window)
    } else {
        Err(t("WhatsApp non ha aperto la chat: non ho inviato niente.", "WhatsApp didn't open the chat: nothing was sent.").into())
    }
}

mod ax {
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::{NSArray, NSNumber, NSString};

    use crate::iokit::{CFTypeRef, cf, owned};

    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
        fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
        fn AXUIElementCopyAttributeValue(element: CFTypeRef, attribute: CFTypeRef, value: *mut CFTypeRef) -> i32;
        fn AXUIElementSetAttributeValue(element: CFTypeRef, attribute: CFTypeRef, value: CFTypeRef) -> i32;
        fn AXUIElementPerformAction(element: CFTypeRef, action: CFTypeRef) -> i32;
        fn AXUIElementSetMessagingTimeout(element: CFTypeRef, seconds: f32) -> i32;
    }

    /// An element of the app's window: a plain value that any thread may ask about.
    #[derive(Clone)]
    pub struct El(Retained<AnyObject>);

    pub fn trusted() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// WhatsApp's process, if it runs.
    pub fn pid() -> Option<i32> {
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(super::BUNDLE));
        Some(apps.iter().find(|a| !a.isTerminated())?.processIdentifier())
    }

    /// WhatsApp, if it runs.
    pub fn app() -> Option<El> {
        let pid = pid()?;
        let app: Retained<AnyObject> = unsafe { owned(AXUIElementCreateApplication(pid)) }?;
        unsafe { AXUIElementSetMessagingTimeout(cf(&*app), 1.0) };
        Some(El(app))
    }

    fn attribute(e: &El, name: &str) -> Option<Retained<AnyObject>> {
        let mut value: CFTypeRef = std::ptr::null();
        if unsafe { AXUIElementCopyAttributeValue(cf(&*e.0), cf(&*NSString::from_str(name)), &mut value) } != 0 {
            return None;
        }
        unsafe { owned(value) }
    }

    pub fn string(e: &El, name: &str) -> String {
        attribute(e, name).and_then(|v| v.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default()
    }

    pub fn elements(e: &El, name: &str) -> Vec<El> {
        attribute(e, name).and_then(|v| v.downcast::<NSArray>().ok()).map(|a| a.iter().map(El).collect()).unwrap_or_default()
    }

    pub fn parent(e: &El) -> Option<El> {
        attribute(e, "AXParent").map(El)
    }

    pub fn bool(e: &El, name: &str) -> bool {
        attribute(e, name).and_then(|v| v.downcast::<NSNumber>().ok()).is_some_and(|n| n.boolValue())
    }

    pub fn set_bool(e: &El, name: &str, value: bool) {
        unsafe { AXUIElementSetAttributeValue(cf(&*e.0), cf(&*NSString::from_str(name)), cf(&*NSNumber::new_bool(value))) };
    }

    pub fn press(e: &El) {
        unsafe { AXUIElementPerformAction(cf(&*e.0), cf(&*NSString::from_str("AXPress"))) };
    }

    /// The element with the identifier `id` under `root`, breadth first; not past a few thousand.
    pub fn find(root: &El, id: &str) -> Option<El> {
        let mut queue = std::collections::VecDeque::from([root.clone()]);
        let mut seen = 0;
        while let Some(e) = queue.pop_front() {
            seen += 1;
            if seen > 4000 {
                break;
            }
            if string(&e, "AXIdentifier") == id {
                return Some(e);
            }
            // The messages and the chats are many, and never hold what's looked for
            if !matches!(string(&e, "AXRole").as_str(), "AXStaticText" | "AXButton" | "AXImage" | "AXMenuBar") {
                queue.extend(elements(&e, "AXChildren"));
            }
        }
        None
    }

    /// The buttons right under `root` and one level down, each with its identifier.
    pub fn buttons(root: &El) -> Vec<(String, El)> {
        let mut found = Vec::new();
        for e in elements(root, "AXChildren") {
            let role = string(&e, "AXRole");
            if role == "AXButton" {
                found.push((string(&e, "AXIdentifier"), e));
            } else if role == "AXGroup" && string(&e, "AXIdentifier") != "ChatMessagesTableView" {
                found.extend(elements(&e, "AXChildren").into_iter().filter(|c| string(c, "AXRole") == "AXButton").map(|c| (string(&c, "AXIdentifier"), c)));
            }
        }
        found
    }
}

// ---- SQLite ------------------------------------------------------------------------------------

#[link(name = "sqlite3")]
unsafe extern "C" {
    fn sqlite3_open_v2(filename: *const c_char, db: *mut *mut c_void, flags: c_int, vfs: *const c_char) -> c_int;
    fn sqlite3_close(db: *mut c_void) -> c_int;
    fn sqlite3_busy_timeout(db: *mut c_void, ms: c_int) -> c_int;
    fn sqlite3_errmsg(db: *mut c_void) -> *const c_char;
    fn sqlite3_prepare_v2(db: *mut c_void, sql: *const c_char, n: c_int, stmt: *mut *mut c_void, tail: *mut *const c_char) -> c_int;
    fn sqlite3_bind_int64(stmt: *mut c_void, index: c_int, value: i64) -> c_int;
    fn sqlite3_step(stmt: *mut c_void) -> c_int;
    fn sqlite3_finalize(stmt: *mut c_void) -> c_int;
    fn sqlite3_column_int64(stmt: *mut c_void, col: c_int) -> i64;
    fn sqlite3_column_double(stmt: *mut c_void, col: c_int) -> f64;
    fn sqlite3_column_text(stmt: *mut c_void, col: c_int) -> *const u8;
    fn sqlite3_column_blob(stmt: *mut c_void, col: c_int) -> *const u8;
    fn sqlite3_column_bytes(stmt: *mut c_void, col: c_int) -> c_int;
}

const SQLITE_OPEN_READONLY: c_int = 1;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;

pub(super) enum Value {
    Int(i64),
}

pub(super) struct Db(*mut c_void);

pub(super) struct Row(*mut c_void);

impl Row {
    pub(super) fn int(&self, col: c_int) -> i64 {
        unsafe { sqlite3_column_int64(self.0, col) }
    }

    pub(super) fn double(&self, col: c_int) -> f64 {
        unsafe { sqlite3_column_double(self.0, col) }
    }

    /// Empty for a null.
    pub(super) fn blob(&self, col: c_int) -> Vec<u8> {
        unsafe {
            let p = sqlite3_column_blob(self.0, col);
            if p.is_null() {
                return Vec::new();
            }
            let n = sqlite3_column_bytes(self.0, col) as usize;
            std::slice::from_raw_parts(p, n).to_vec()
        }
    }

    /// Empty for a null.
    fn text(&self, col: c_int) -> String {
        unsafe {
            let p = sqlite3_column_text(self.0, col);
            if p.is_null() {
                return String::new();
            }
            let n = sqlite3_column_bytes(self.0, col) as usize;
            String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned()
        }
    }
}

impl Db {
    pub(super) fn open(file: &Path) -> Result<Db, Error> {
        let path = CString::new(file.to_string_lossy().as_bytes()).map_err(|e| Error::Other(e.to_string()))?;
        let mut db = std::ptr::null_mut();
        let rc = unsafe { sqlite3_open_v2(path.as_ptr(), &mut db, SQLITE_OPEN_READONLY, std::ptr::null()) };
        let db = Db(db);
        if rc != 0 {
            return Err(db.error());
        }
        // The app may be writing: wait a moment for it rather than give up
        unsafe { sqlite3_busy_timeout(db.0, 500) };
        Ok(db)
    }

    fn error(&self) -> Error {
        if self.0.is_null() {
            return Error::Other("out of memory".into());
        }
        Error::Other(unsafe { CStr::from_ptr(sqlite3_errmsg(self.0)) }.to_string_lossy().into_owned())
    }

    pub(super) fn each(&self, sql: &str, args: &[Value], mut row: impl FnMut(&Row)) -> Result<(), Error> {
        let sql = CString::new(sql).map_err(|e| Error::Other(e.to_string()))?;
        let mut stmt = std::ptr::null_mut();
        if unsafe { sqlite3_prepare_v2(self.0, sql.as_ptr(), -1, &mut stmt, std::ptr::null_mut()) } != 0 {
            return Err(self.error());
        }
        for (i, a) in args.iter().enumerate() {
            match a {
                Value::Int(v) => unsafe { sqlite3_bind_int64(stmt, i as c_int + 1, *v) },
            };
        }
        let result = loop {
            match unsafe { sqlite3_step(stmt) } {
                SQLITE_ROW => row(&Row(stmt)),
                SQLITE_DONE => break Ok(()),
                _ => break Err(self.error()),
            }
        };
        unsafe { sqlite3_finalize(stmt) };
        result
    }
}

impl Drop for Db {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { sqlite3_close(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_without_a_name_is_its_number() {
        assert_eq!(sender("Dana"), "Dana");
        assert_eq!(sender("15550100@s.whatsapp.net"), "+15550100");
        assert_eq!(sender("abc123@lid"), "abc123");
    }

    #[test]
    fn a_chat_shown_by_its_number_is_the_one_of_that_number() {
        assert!(same("\u{200e}Dana", "Dana", None));
        assert!(same("+1 (555) 01-00", "You", Some("15550100")));
        assert!(!same("+1 (555) 01-01", "You", Some("15550100")));
        assert!(!same("+1 555 0100", "You", None));
        // A name with digits in it isn't a number
        assert!(!same("Dana 15550100", "You", Some("15550100")));
    }

    #[test]
    fn a_row_with_something_unread_has_the_count_after_the_name() {
        assert!(row_of("Dana", "Dana", None));
        assert!(row_of("Dana, \u{200e}2 messaggi non letti", "Dana", None));
        assert!(row_of("Dana, Sam, 1 unread message", "Dana, Sam", None));
        assert!(row_of("+1 555 0100, \u{200e}3 unread messages", "Someone", Some("15550100")));
        // Another chat whose name starts the same isn't it
        assert!(!row_of("Dana, Sam", "Dana", None));
        assert!(!row_of("Dana, Sam, 1 unread message", "Dana", None));
        assert!(!row_of("Dana 2", "Dana", None));
    }

    #[test]
    fn names_are_compared_without_direction_marks() {
        assert_eq!(plain("\u{200e}Dana \u{202c}"), "Dana");
        assert_eq!(plain("Five-a-side"), "Five-a-side");
    }

    #[test]
    fn a_chat_with_a_person_has_a_number() {
        let chat = |jid: &str| Chat { jid: jid.into(), ..Default::default() };
        assert_eq!(chat("15550100@s.whatsapp.net").phone(), Some("15550100"));
        assert_eq!(chat("1203630@g.us").phone(), None);
        assert_eq!(chat("abc@lid").phone(), None);
    }

    /// A database as the app's, with what `read` asks of it.
    fn database(dir: &Path) {
        let sql = "
            CREATE TABLE ZWACHATSESSION (Z_PK INTEGER PRIMARY KEY, ZCONTACTJID, ZPARTNERNAME, ZSESSIONTYPE, ZUNREADCOUNT, ZLASTMESSAGEDATE, ZREMOVED, ZHIDDEN, ZARCHIVED);
            CREATE TABLE ZWAPROFILEPICTUREITEM (ZJID, ZPATH);
            CREATE TABLE ZWACHATPROPERTIES (ZCHATSESSION, ZMUTEDATE);
            CREATE TABLE ZWAMESSAGE (Z_PK INTEGER PRIMARY KEY, ZCHATSESSION, ZISFROMME, ZMESSAGEDATE, ZMESSAGETYPE, ZTEXT, ZPUSHNAME, ZMEDIAITEM, ZGROUPMEMBER);
            CREATE TABLE ZWAMEDIAITEM (Z_PK INTEGER PRIMARY KEY, ZMEDIALOCALPATH, ZXMPPTHUMBPATH, ZMOVIEDURATION, ZTITLE, ZMEDIAURL, ZVCARDNAME);
            CREATE TABLE ZWAGROUPMEMBER (Z_PK INTEGER PRIMARY KEY, ZCONTACTNAME, ZMEMBERJID);
            CREATE TABLE ZWAPROFILEPUSHNAME (ZJID, ZPUSHNAME);
            INSERT INTO ZWACHATSESSION VALUES (1, '15550100@s.whatsapp.net', 'Dana', 0, 0, 100, 0, 0, 0);
            INSERT INTO ZWACHATSESSION VALUES (2, '1203630@g.us', 'Five-a-side', 1, 2, 50, 0, 0, 0);
            INSERT INTO ZWACHATSESSION VALUES (3, '15550101@s.whatsapp.net', 'Archived', 0, 4, 200, 0, 0, 1);
            INSERT INTO ZWACHATSESSION VALUES (4, 'status@broadcast', 'Status', 3, 9, 300, 0, 0, 0);
            INSERT INTO ZWACHATSESSION VALUES (5, '15550102@s.whatsapp.net', 'Older', 0, 0, 10, 0, 0, 0);
            INSERT INTO ZWACHATPROPERTIES VALUES (2, 9999999999);
            INSERT INTO ZWAGROUPMEMBER VALUES (1, NULL, '15550103@s.whatsapp.net');
            INSERT INTO ZWAPROFILEPUSHNAME VALUES ('15550103@s.whatsapp.net', 'Sam');
            INSERT INTO ZWAMEDIAITEM VALUES (1, 'Media/a/voice.opus', NULL, 14, NULL, 'https://mmg.whatsapp.net/x', NULL);
            INSERT INTO ZWAMEDIAITEM VALUES (2, NULL, NULL, 0, 'A page', 'https://example.com/a', NULL);
            INSERT INTO ZWAMESSAGE VALUES (1, 2, 0, 40, 0, 'first', NULL, NULL, 1);
            INSERT INTO ZWAMESSAGE VALUES (2, 2, 0, 45, 6, NULL, NULL, NULL, NULL);
            INSERT INTO ZWAMESSAGE VALUES (3, 2, 0, 50, 3, NULL, NULL, 1, 1);
            INSERT INTO ZWAMESSAGE VALUES (4, 1, 1, 90, 7, 'look https://example.com/a', NULL, 2, NULL);
            INSERT INTO ZWAMESSAGE VALUES (5, 1, 0, 100, 66, NULL, NULL, NULL, NULL);
            INSERT INTO ZWAMESSAGE VALUES (6, 5, 0, 10, 12, NULL, NULL, NULL, NULL);
        ";
        let file = dir.join("ChatStorage.sqlite");
        let out = std::process::Command::new("/usr/bin/sqlite3").arg(&file).arg(sql).output().expect("sqlite3");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        std::fs::create_dir_all(dir.join("Message/Media/a")).unwrap();
        std::fs::write(dir.join("Message/Media/a/voice.opus"), b"").unwrap();
    }

    #[test]
    fn the_chats_come_from_the_database() {
        let dir = std::env::temp_dir().join(format!("rustybar-wa-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read(&dir, 5), Err(Error::Missing));
        database(&dir);
        // One to see once is an empty row, and still a message
        let older = read(&dir, 5).unwrap().into_iter().find(|c| c.name == "Older").unwrap();
        assert_eq!(older.messages.iter().map(|m| m.kind).collect::<Vec<_>>(), [Kind::Once]);
        let chats = read(&dir, 1).unwrap();
        // The unread one first, then one of the others, the latest: no archived chat, no status
        assert_eq!(chats.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Five-a-side", "Dana"]);
        let group = &chats[0];
        assert!(group.group && group.muted && group.unread == 2 && group.newest == 3);
        // What the app says itself is left out; a member is called as they call themselves
        assert_eq!(group.messages.iter().map(|m| m.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(group.messages[0].sender, "Sam");
        let voice = &group.messages[1];
        assert_eq!((voice.kind, voice.seconds), (Kind::Voice, 14.0));
        assert!(voice.file.as_deref().is_some_and(|f| f.ends_with("Message/Media/a/voice.opus")) && voice.url.is_empty());
        let dana = &chats[1];
        assert!(!dana.group && !dana.muted && dana.newest == 5);
        // One of a kind the bar doesn't know, with nothing to show, isn't there
        assert_eq!(dana.messages.len(), 1);
        let link = &dana.messages[0];
        assert_eq!((link.kind, link.mine, link.title.as_str(), link.url.as_str()), (Kind::Link, true, "A page", "https://example.com/a"));
        assert_eq!(link.at, 90.0 + EPOCH);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The app keeps its database open and writes to its log: that must be seen, without the
    /// file being closed.
    #[test]
    fn a_write_of_an_app_that_keeps_the_database_open_is_seen() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("rustybar-wa-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        database(&dir);
        let mut app = std::process::Command::new("/usr/bin/sqlite3").arg(dir.join("ChatStorage.sqlite")).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).spawn().unwrap();
        let mut stdin = app.stdin.take().unwrap();
        writeln!(stdin, "PRAGMA journal_mode=WAL; UPDATE ZWACHATSESSION SET ZUNREADCOUNT = 1 WHERE Z_PK = 1;").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        let (tx, rx) = std::sync::mpsc::channel();
        let _watcher = watch(&dir, move || drop(tx.send(()))).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1500));
        while rx.try_recv().is_ok() {}
        writeln!(stdin, "UPDATE ZWACHATSESSION SET ZUNREADCOUNT = 3 WHERE Z_PK = 1;").unwrap();
        let seen = rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok();
        let unread = read(&dir, 5).unwrap().iter().find(|c| c.name == "Dana").map(|c| c.unread);
        drop(stdin);
        let _ = app.wait();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(unread, Some(3));
        assert!(seen);
    }
}
