//! Telegram's chats for the `telegram` widget, with Telegram's own API: RustyBar is a client of
//! its own (MTProto, through `grammers`), signed in once with `rustybar telegram login`. The
//! Mac app isn't asked anything: its database is encrypted, in a format of its own.
//!
//! One thread holds the connection: the chats are read at the start (and after a wake), then
//! Telegram itself tells what changes, so nothing is polled. The same thread sends, marks a chat
//! read and downloads what a chat shows (pictures, voice notes, the small picture of a video) into
//! `~/Library/Caches/rustybar/telegram`; a document or a video only when asked.
//!
//! The session (the key Telegram gave this client, and the app id it was asked with) is in
//! `~/Library/Application Support/rustybar/telegram.json`, readable by the user alone:
//! whoever has it reads the account, as with any Telegram client's files. `rustybar telegram
//! logout` ends the session at Telegram and deletes it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use grammers_client::media::{Document, Downloadable, Media, PhotoSize};
use grammers_client::peer::Peer;
use grammers_client::update::Update;
use grammers_client::{Client, InvocationError, SignInError};
use grammers_mtsender::{SenderPool, UpdatesConfiguration};
use grammers_session::types::{ChannelState, DcOption, PeerId, PeerInfo, PeerKind, PeerRef, UpdateState, UpdatesState};
use grammers_session::{Session, SessionData};
use grammers_tl_types as tl;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use super::wa::{Chat, Kind, Message};
use crate::i18n::t;

pub const BUNDLE: &str = "ru.keepcoder.Telegram";

/// How many chats are followed, the latest written to.
const DIALOGS: usize = 40;
/// How many messages of a chat the popup shows.
const MESSAGES: usize = 40;
/// A voice note or a sticker bigger than this isn't downloaded with its chat.
const SMALL: usize = 4 << 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// No session: `rustybar telegram login` was never run, or the session was ended.
    SignedOut,
    Other(String),
}

/// What the widget asks of the connection.
pub enum Cmd {
    /// The chats again, from Telegram: after a wake, or a login.
    Refresh,
    /// A chat is on show: its messages, and what they show.
    Open(String),
    /// No chat is on show any more.
    Close,
    Send { jid: String, text: String, done: Box<dyn FnOnce(Result<(), String>) + Send> },
    /// Marks the chat read at Telegram.
    Read { jid: String, done: Box<dyn FnOnce(Result<(), String>) + Send> },
    /// Downloads the file of a message (a document, a video); `done` gets its path.
    Fetch { jid: String, id: i64, done: Box<dyn FnOnce(Result<String, String>) + Send> },
}

/// The connection, for as long as this lives.
pub struct Handle {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl Handle {
    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

// ---- the session, in a file ---------------------------------------------------------------------

fn session_file() -> PathBuf {
    crate::config::home().join("Library/Application Support/rustybar/telegram.json")
}

fn cache_dir() -> PathBuf {
    crate::config::home().join("Library/Caches/rustybar/telegram")
}

/// What the file keeps: of the peers only oneself, which says the session is signed in; the
/// others come back with the chats.
#[derive(Serialize, Deserialize)]
struct Saved {
    api_id: i32,
    home_dc: i32,
    dcs: Vec<DcOption>,
    peers: Vec<PeerInfo>,
    updates: UpdatesState,
}

/// `grammers`' session in memory, saved to the file when it changes (its own storage is a
/// database with a SQLite of its own).
struct Store {
    api_id: i32,
    data: Mutex<SessionData>,
    dirty: AtomicBool,
}

#[derive(Debug)]
struct Poisoned;

impl std::fmt::Display for Poisoned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the session's lock is poisoned")
    }
}

impl std::error::Error for Poisoned {}

impl Store {
    fn new(api_id: i32) -> Store {
        Store { api_id, data: Mutex::new(SessionData::default()), dirty: AtomicBool::new(false) }
    }

    fn load() -> Option<Store> {
        let saved: Saved = serde_json::from_slice(&std::fs::read(session_file()).ok()?).ok()?;
        let mut data = SessionData { home_dc: saved.home_dc, updates_state: saved.updates, ..Default::default() };
        data.dc_options.extend(saved.dcs.into_iter().map(|dc| (dc.id, dc)));
        data.peer_infos.extend(saved.peers.into_iter().map(|p| (p.id(), p)));
        Some(Store { api_id: saved.api_id, data: Mutex::new(data), dirty: AtomicBool::new(false) })
    }

    fn data(&self) -> Result<MutexGuard<'_, SessionData>, Poisoned> {
        self.data.lock().map_err(|_| Poisoned)
    }

    /// Writes the file, if something changed since the last time: beside it first, then in its
    /// place, never open to others.
    fn save(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let Ok(data) = self.data() else { return };
        let saved = Saved {
            api_id: self.api_id,
            home_dc: data.home_dc,
            dcs: data.dc_options.values().cloned().collect(),
            peers: data.peer_infos.values().filter(|p| matches!(p, PeerInfo::User { is_self: Some(true), .. })).cloned().collect(),
            updates: data.updates_state.clone(),
        };
        drop(data);
        let Ok(json) = serde_json::to_vec(&saved) else { return };
        let file = session_file();
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let beside = file.with_extension("json.new");
        use std::os::unix::fs::OpenOptionsExt;
        let written = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&beside).and_then(|mut f| std::io::Write::write_all(&mut f, &json));
        if written.is_ok() {
            let _ = std::fs::rename(&beside, &file);
        }
    }

    fn touch(&self) {
        self.dirty.store(true, Ordering::Relaxed);
    }
}

type BoxFuture<'a, T> = std::pin::Pin<Box<dyn Future<Output = T> + Send + 'a>>;

impl Session for Store {
    type Error = Poisoned;

    fn home_dc_id(&self) -> Result<i32, Poisoned> {
        Ok(self.data()?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Poisoned>> {
        Box::pin(async move {
            self.data()?.home_dc = dc_id;
            self.touch();
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Poisoned> {
        Ok(self.data()?.dc_options.get(&dc_id).cloned())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Poisoned>> {
        let dc_option = dc_option.clone();
        Box::pin(async move {
            self.data()?.dc_options.insert(dc_option.id, dc_option);
            self.touch();
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Poisoned>> {
        Box::pin(async move { Ok(self.data()?.peer_infos.get(&peer).cloned()) })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Poisoned>> {
        Box::pin(async move {
            let own = matches!(peer, PeerInfo::User { is_self: Some(true), .. });
            self.data()?.peer_infos.entry(peer.id()).or_insert_with(|| peer.clone()).extend_info(&peer);
            if own {
                self.touch();
            }
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Poisoned>> {
        Box::pin(async move { Ok(self.data()?.updates_state.clone()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Poisoned>> {
        Box::pin(async move {
            let mut data = self.data()?;
            match update {
                UpdateState::All(state) => data.updates_state = state,
                UpdateState::Primary { pts, date, seq } => {
                    data.updates_state.pts = pts;
                    data.updates_state.date = date;
                    data.updates_state.seq = seq;
                }
                UpdateState::Secondary { qts } => data.updates_state.qts = qts,
                UpdateState::Channel { id, pts } => {
                    data.updates_state.channels.retain(|c| c.id != id);
                    data.updates_state.channels.push(ChannelState { id, pts });
                }
            }
            Ok(())
        })
    }
}

// ---- the command line ---------------------------------------------------------------------------

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())
}

fn ask(question: &str, hidden: bool) -> Result<String, String> {
    use std::io::Write;
    print!("{question}");
    std::io::stdout().flush().map_err(|e| e.to_string())?;
    // A password isn't shown as it's typed
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let tty = hidden && unsafe { libc::tcgetattr(0, &mut term) } == 0;
    if tty {
        let mut quiet = term;
        quiet.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &quiet) };
    }
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    if tty {
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &term) };
        println!();
    }
    read.map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

/// `rustybar telegram login | logout | status`.
pub fn command(args: &[String]) -> Result<String, String> {
    match args.first().map(String::as_str) {
        Some("login") => runtime()?.block_on(login()),
        Some("logout") => runtime()?.block_on(logout()),
        Some("status") => runtime()?.block_on(status()),
        _ => Err("usage: rustybar telegram login | logout | status".into()),
    }
}

/// A client on `store`, with the task that runs its connection.
fn connect(store: &Arc<Store>) -> (Client, grammers_mtsender::SenderPoolFatHandle, tokio::sync::mpsc::Receiver<grammers_session::updates::UpdatesLike>, tokio::task::JoinHandle<()>) {
    let SenderPool { runner, updates, handle } = SenderPool::new(Arc::clone(store), store.api_id);
    let client = Client::new(handle.clone());
    let task = tokio::spawn(runner.run());
    (client, handle, updates, task)
}

async fn login() -> Result<String, String> {
    println!("{}", t("RustyBar entra in Telegram come un client suo. Servono un api_id e un api_hash tuoi: my.telegram.org, API development tools.", "RustyBar signs in to Telegram as a client of its own. It needs an api_id and an api_hash of yours: my.telegram.org, API development tools."));
    let api_id: i32 = ask("api_id: ", false)?.parse().map_err(|_| t("L'api_id è un numero.", "The api_id is a number.").to_string())?;
    let api_hash = ask("api_hash: ", false)?;
    let phone = ask(t("Numero di telefono, col prefisso (+39…): ", "Phone number, with the country code (+1…): "), false)?;
    let store = Arc::new(Store::new(api_id));
    let (client, handle, _updates, task) = connect(&store);
    let result = async {
        let token = client.request_login_code(&phone, &api_hash).await.map_err(|e| e.to_string())?;
        let code = ask(t("Il codice che Telegram ti ha mandato: ", "The code Telegram sent you: "), false)?;
        match client.sign_in(&token, &code).await {
            Ok(_) => {}
            Err(SignInError::PasswordRequired(token)) => {
                let hint = token.hint().map(|h| format!(" ({h})")).unwrap_or_default();
                let password = ask(&format!("{}{hint}: ", t("La password della verifica in due passaggi", "The two-step verification password")), true)?;
                client.check_password(token, password).await.map_err(|e| e.to_string())?;
            }
            Err(e) => return Err(e.to_string()),
        }
        let me = client.get_me().await.map_err(|e| e.to_string())?;
        Ok(me.full_name())
    }
    .await;
    handle.quit();
    let _ = task.await;
    let name = result?;
    store.touch();
    store.save();
    // The running bar connects with it
    let _ = crate::ipc::send(&["trigger".to_string(), "telegram_login".to_string()]);
    Ok(format!("{} {name}. {}", t("Entrato come", "Signed in as"), t("La sessione è in ~/Library/Application Support/rustybar/telegram.json.", "The session is in ~/Library/Application Support/rustybar/telegram.json.")))
}

async fn logout() -> Result<String, String> {
    let Some(store) = Store::load() else { return Err(t("Nessuna sessione.", "No session.").into()) };
    let store = Arc::new(store);
    let (client, handle, _updates, task) = connect(&store);
    let ended = client.sign_out().await;
    handle.quit();
    let _ = task.await;
    let _ = std::fs::remove_file(session_file());
    let _ = std::fs::remove_dir_all(cache_dir());
    let _ = crate::ipc::send(&["trigger".to_string(), "telegram_login".to_string()]);
    match ended {
        Ok(_) => Ok(t("Sessione chiusa e cancellata.", "Session ended and deleted.").into()),
        Err(e) => Ok(format!("{} ({e})", t("Sessione cancellata dal Mac; Telegram non ha risposto alla chiusura: toglila da Impostazioni, Dispositivi.", "Session deleted from the Mac; Telegram didn't answer its end: remove it in Settings, Devices."))),
    }
}

async fn status() -> Result<String, String> {
    let Some(store) = Store::load() else { return Ok(t("Nessuna sessione: rustybar telegram login.", "No session: rustybar telegram login.").into()) };
    let store = Arc::new(store);
    let (client, handle, _updates, task) = connect(&store);
    let me = match client.is_authorized().await {
        Ok(true) => client.get_me().await.map(|me| format!("{} {}", t("Entrato come", "Signed in as"), me.full_name())).map_err(|e| e.to_string()),
        Ok(false) => Ok(t("La sessione non vale più: rustybar telegram login.", "The session is no longer valid: rustybar telegram login.").into()),
        Err(e) => Err(e.to_string()),
    };
    handle.quit();
    let _ = task.await;
    me
}

// ---- the connection -----------------------------------------------------------------------------

/// A chat as the widget has it, and what it takes to ask Telegram about it.
struct Entry {
    chat: Chat,
    peer: PeerRef,
    /// Its messages were asked for, not only the last one.
    loaded: bool,
    /// The messages as Telegram gave them, by their id there: to download what they carry.
    raw: HashMap<i32, grammers_client::message::Message>,
}

/// What a task brings back to the connection's loop.
enum Done {
    History { jid: String, messages: Vec<grammers_client::message::Message> },
    Picture { jid: String, path: String },
    File { jid: String, id: i64, file: Option<String>, thumb: Option<String> },
}

/// The id the widget knows a message by: when it was sent, then Telegram's id, which is a
/// channel's own count; so a later message has a higher one, across chats too.
fn message_id(date: i64, id: i32) -> i64 {
    (date.max(0) << 32) | (id as u32 as i64)
}

fn telegram_id(id: i64) -> i32 {
    (id & 0xffff_ffff) as i32
}

fn jid(peer: PeerId) -> String {
    peer.bot_api_dialog_id_unchecked().to_string()
}

/// The link that opens the chat `jid` in Telegram's app: a person by its id, a group or a channel
/// by its own (which the dialog's id has after `-`, or `-100`).
pub fn link(jid: &str) -> String {
    match jid.strip_prefix("-100").or(jid.strip_prefix('-')) {
        Some(id) => format!("tg://openmessage?chat_id={id}"),
        None => format!("tg://openmessage?user_id={jid}"),
    }
}

/// The audio and video of a document: whether it's a voice note, a round video.
fn attributes(doc: &Document) -> (bool, bool) {
    let mut found = (false, false);
    if let Some(tl::enums::Document::Document(d)) = &doc.raw.document {
        for a in &d.attributes {
            match a {
                tl::enums::DocumentAttribute::Audio(a) => found.0 = a.voice,
                tl::enums::DocumentAttribute::Video(v) => found.1 = v.round_message,
                _ => {}
            }
        }
    }
    found
}

/// A message of Telegram as the widget's; None for what a chat says itself (who joined).
fn convert(m: &grammers_client::message::Message, group: bool) -> Option<Message> {
    if m.action().is_some() {
        return None;
    }
    let date = m.date().as_second();
    let mut out = Message {
        id: message_id(date, m.id()),
        mine: m.outgoing(),
        sender: if group { m.sender().and_then(|s| s.name()).unwrap_or_default().to_string() } else { String::new() },
        at: date as f64,
        kind: Kind::Text,
        text: m.text().to_string(),
        ..Default::default()
    };
    match m.media() {
        None => {}
        Some(Media::Photo(p)) => out.kind = if p.ttl_seconds().is_some() { Kind::Once } else { Kind::Image },
        Some(Media::Sticker(_)) => out.kind = Kind::Sticker,
        Some(Media::Document(d)) => {
            let mime = d.mime_type().unwrap_or_default();
            let (voice, _round) = attributes(&d);
            out.seconds = d.duration().unwrap_or(0.0);
            out.kind = if d.raw.ttl_seconds.is_some() {
                Kind::Once
            } else if voice || mime.starts_with("audio/") {
                Kind::Voice
            } else if d.is_animated() {
                Kind::Gif
            } else if mime.starts_with("video/") {
                Kind::Video
            } else if mime.starts_with("image/") {
                Kind::Image
            } else {
                Kind::File
            };
            if matches!(out.kind, Kind::File | Kind::Voice) {
                out.title = d.audio_title().or(d.name()).unwrap_or_default().to_string();
            }
        }
        Some(Media::Contact(c)) => {
            out.kind = Kind::Contact;
            out.title = format!("{} {}", c.first_name(), c.last_name()).trim().to_string();
        }
        Some(Media::Poll(p)) => {
            out.kind = Kind::Poll;
            let tl::enums::TextWithEntities::Entities(question) = p.question();
            out.text = question.text.clone();
        }
        Some(Media::Geo(_) | Media::Venue(_) | Media::GeoLive(_)) => out.kind = Kind::Location,
        Some(Media::Dice(d)) => out.text = d.emoji().to_string(),
        Some(Media::WebPage(w)) => {
            if let tl::enums::WebPage::Page(page) = &w.raw.webpage {
                out.kind = Kind::Link;
                out.url = page.url.clone();
                out.title = page.title.clone().or(page.site_name.clone()).unwrap_or_default();
            }
        }
        Some(_) => out.kind = Kind::Other,
    }
    if out.kind == Kind::Other && out.text.is_empty() {
        return None;
    }
    Some(out)
}

/// The file a download of `kind` goes to, in the cache: by chat and message, so it's there the
/// next time.
fn cache_file(jid: &str, id: i32, part: &str, ext: &str) -> PathBuf {
    cache_dir().join(format!("{}_{id}{part}.{ext}", jid.trim_start_matches('-')))
}

fn existing(path: &Path) -> Option<String> {
    path.metadata().ok().filter(|m| m.is_file() && m.len() > 0).map(|_| path.to_string_lossy().into_owned())
}

/// The size of a picture to show in a bubble: the biggest up to 800 points, which Telegram calls
/// `x`; not the stripped ones, which are a blur of a few bytes.
fn showable(sizes: Vec<PhotoSize>) -> Option<PhotoSize> {
    let rank = |s: &PhotoSize| match s.photo_type().as_str() {
        "x" => 5,
        "m" => 4,
        "y" => 3,
        "s" => 2,
        "w" => 1,
        _ => 0,
    };
    sizes.into_iter().filter(|s| matches!(s, PhotoSize::Size(_) | PhotoSize::Progressive(_) | PhotoSize::Cached(_))).max_by_key(rank)
}

async fn download<D: Downloadable>(client: &Client, what: &D, to: &Path) -> Option<String> {
    if let Some(done) = existing(to) {
        return Some(done);
    }
    let part = to.with_extension("part");
    match client.download_media(what, &part).await {
        Ok(()) => {
            std::fs::rename(&part, to).ok()?;
            existing(to)
        }
        Err(_) => {
            let _ = std::fs::remove_file(&part);
            None
        }
    }
}

fn extension(doc: &Document, or: &str) -> String {
    let named = doc.name().and_then(|n| n.rsplit_once('.')).map(|(_, e)| e.to_lowercase()).filter(|e| !e.is_empty() && e.len() <= 5 && e.chars().all(|c| c.is_ascii_alphanumeric()));
    named.unwrap_or_else(|| match doc.mime_type().unwrap_or_default() {
        "audio/ogg" => "ogg".into(),
        "audio/mpeg" => "mp3".into(),
        "video/mp4" => "mp4".into(),
        "image/webp" => "webp".into(),
        "image/jpeg" => "jpg".into(),
        "image/png" => "png".into(),
        "application/pdf" => "pdf".into(),
        _ => or.into(),
    })
}

/// What a chat shows of the message `m` without being asked: a picture, the small picture of a
/// video, a voice note, a sticker that is a picture. (file, thumb).
async fn media(client: &Client, jid: &str, m: &grammers_client::message::Message) -> (Option<String>, Option<String>) {
    let id = m.id();
    match m.media() {
        Some(Media::Photo(p)) if p.ttl_seconds().is_none() => match showable(p.thumbs()) {
            Some(size) => (download(client, &size, &cache_file(jid, id, "", "jpg")).await, None),
            None => (None, None),
        },
        Some(Media::Sticker(s)) => {
            let doc = &s.document;
            if doc.mime_type() == Some("image/webp") && doc.size().is_some_and(|n| n <= SMALL) {
                (download(client, doc, &cache_file(jid, id, "", "webp")).await, None)
            } else {
                (None, None)
            }
        }
        Some(Media::Document(d)) if d.raw.ttl_seconds.is_none() => {
            let mime = d.mime_type().unwrap_or_default().to_string();
            let (voice, _) = attributes(&d);
            let whole = cache_file(jid, id, "", &extension(&d, "bin"));
            if (voice || mime.starts_with("audio/")) && d.size().is_some_and(|n| n <= SMALL) {
                return (download(client, &d, &whole).await, None);
            }
            // A document or a video: its small picture now, the file when asked (or if it's there)
            let thumb = match showable(d.thumbs()) {
                Some(size) => download(client, &size, &cache_file(jid, id, "_t", "jpg")).await,
                None => None,
            };
            let file = existing(&whole);
            if mime.starts_with("image/") && file.is_none() && d.size().is_some_and(|n| n <= SMALL) {
                return (download(client, &d, &whole).await, thumb);
            }
            (file, thumb)
        }
        _ => (None, None),
    }
}

struct Connection {
    client: Client,
    entries: Vec<Entry>,
    channels: bool,
    recent: usize,
    done: mpsc::UnboundedSender<Done>,
    /// The chats that write and aren't followed (a channel, an archived chat), not to ask
    /// Telegram for the chats again each time they do.
    ignored: std::collections::HashSet<String>,
    /// The chat on show.
    open: Option<String>,
}

impl Connection {
    fn entry(&mut self, jid: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().find(|e| e.chat.jid == jid)
    }

    fn snapshot(&self) -> Vec<Chat> {
        self.entries.iter().map(|e| e.chat.clone()).collect()
    }

    /// The chats, from Telegram: the latest written to, without the archived ones.
    async fn load(&mut self) -> Result<(), InvocationError> {
        let now = crate::time::now();
        let mut dialogs = self.client.iter_dialogs();
        let mut entries = Vec::new();
        let mut pictures = Vec::new();
        while let Some(dialog) = dialogs.next().await? {
            if entries.len() >= DIALOGS {
                break;
            }
            let tl::enums::Dialog::Dialog(raw) = &dialog.raw else { continue };
            if raw.folder_id.is_some_and(|f| f != 0) {
                continue;
            }
            let (name, group) = match dialog.peer() {
                Peer::User(u) if u.is_self() => (t("Messaggi salvati", "Saved Messages").to_string(), false),
                Peer::User(u) if u.deleted() => (t("Account eliminato", "Deleted Account").to_string(), false),
                Peer::User(u) => (u.full_name(), false),
                Peer::Group(g) => (g.title().unwrap_or_default().to_string(), true),
                Peer::Channel(c) if self.channels => (c.title().to_string(), true),
                _ => continue,
            };
            let muted = match &raw.notify_settings {
                tl::enums::PeerNotifySettings::Settings(s) => s.mute_until.is_some_and(|until| until as f64 > now),
            };
            let id = jid(dialog.peer_id());
            // What was there before a reload stays: the messages, the pictures
            let before = self.entries.iter().position(|e| e.chat.jid == id).map(|i| self.entries.swap_remove(i));
            let mut entry = before.unwrap_or_else(|| Entry { chat: Chat { jid: id.clone(), ..Default::default() }, peer: dialog.peer_ref(), loaded: false, raw: HashMap::new() });
            entry.peer = dialog.peer_ref();
            entry.chat.name = name;
            entry.chat.group = group;
            entry.chat.unread = raw.unread_count as i64;
            entry.chat.muted = muted;
            if let Some(last) = &dialog.last_message {
                entry.chat.at = last.date().as_second() as f64;
                entry.chat.newest = message_id(last.date().as_second(), last.id());
                if let Some(m) = convert(last, group) {
                    if !entry.chat.messages.iter().any(|old| old.id == m.id) {
                        // A message the chat missed: all of it again
                        entry.loaded = entry.loaded && entry.chat.messages.last().is_none_or(|old| old.id > m.id);
                        if !entry.loaded {
                            entry.chat.messages = vec![m];
                        }
                    }
                    entry.raw.insert(last.id(), last.clone());
                }
            }
            if entry.chat.picture.is_none() {
                pictures.push((id, dialog.peer().clone()));
            }
            entries.push(entry);
        }
        self.entries = entries;
        self.pictures(pictures);
        // The chats the popup shows first have their messages from the start
        let mut quiet = 0;
        let wanted: Vec<String> = self
            .entries
            .iter()
            .filter(|e| !e.loaded)
            .filter(|e| {
                quiet += usize::from(e.chat.unread == 0);
                e.chat.unread > 0 || quiet <= self.recent
            })
            .map(|e| e.chat.jid.clone())
            .collect();
        self.histories(wanted);
        Ok(())
    }

    /// Asks for the small pictures of the chats, on a task, one after the other.
    fn pictures(&self, chats: Vec<(String, Peer)>) {
        let (client, done) = (self.client.clone(), self.done.clone());
        tokio::spawn(async move {
            for (jid, peer) in chats {
                let photo_id = match &peer {
                    Peer::User(u) => u.photo().map(|p| p.photo_id),
                    Peer::Group(g) => g.photo().map(|p| p.photo_id),
                    Peer::Channel(c) => c.photo().map(|p| p.photo_id),
                    _ => None,
                };
                let Some(photo_id) = photo_id else { continue };
                let to = cache_dir().join(format!("p{}_{photo_id}.jpg", jid.trim_start_matches('-')));
                let path = match existing(&to) {
                    Some(p) => Some(p),
                    None => match peer.photo(false).await {
                        Ok(Some(photo)) => download(&client, &photo, &to).await,
                        _ => None,
                    },
                };
                if let Some(path) = path {
                    let _ = done.send(Done::Picture { jid, path });
                }
            }
        });
    }

    /// Asks for the last messages of some chats, on a task, one after the other.
    fn histories(&self, jids: Vec<String>) {
        let peers: Vec<(String, PeerRef)> = jids.into_iter().filter_map(|jid| self.entries.iter().find(|e| e.chat.jid == jid).map(|e| (jid, e.peer))).collect();
        let (client, done) = (self.client.clone(), self.done.clone());
        tokio::spawn(async move {
            for (jid, peer) in peers {
                let mut iter = client.iter_messages(peer).limit(MESSAGES);
                let mut messages = Vec::new();
                while let Ok(Some(m)) = iter.next().await {
                    messages.push(m);
                }
                messages.reverse();
                let _ = done.send(Done::History { jid, messages });
            }
        });
    }

    /// Asks for what the messages of a chat show, those that have nothing yet, on a task: the
    /// latest first.
    fn show(&self, jid: &str) {
        let Some(entry) = self.entries.iter().find(|e| e.chat.jid == jid) else { return };
        let wanted: Vec<(i64, grammers_client::message::Message)> = entry
            .chat
            .messages
            .iter()
            .rev()
            .filter(|m| m.file.is_none() && matches!(m.kind, Kind::Image | Kind::Gif | Kind::Video | Kind::Voice | Kind::Sticker | Kind::File) && (m.thumb.is_none() || m.kind == Kind::Image))
            .filter_map(|m| entry.raw.get(&telegram_id(m.id)).map(|raw| (m.id, raw.clone())))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let (client, done, jid) = (self.client.clone(), self.done.clone(), jid.to_string());
        tokio::spawn(async move {
            for (id, raw) in wanted {
                let (file, thumb) = media(&client, &jid, &raw).await;
                if file.is_some() || thumb.is_some() {
                    let _ = done.send(Done::File { jid: jid.clone(), id, file, thumb });
                }
            }
        });
    }

    /// A message that came, or that changed. False for a chat that isn't followed yet.
    fn message(&mut self, m: grammers_client::message::Message, new: bool) -> bool {
        let id = jid(m.peer_id());
        let Some(entry) = self.entry(&id) else { return false };
        let Some(converted) = convert(&m, entry.chat.group) else { return true };
        entry.raw.insert(m.id(), m.clone());
        match entry.chat.messages.iter_mut().find(|old| old.id == converted.id) {
            Some(old) => {
                // What was downloaded stays
                let (file, thumb) = (old.file.take(), old.thumb.take());
                *old = Message { file, thumb, ..converted };
            }
            None if new => {
                entry.chat.at = converted.at;
                entry.chat.newest = entry.chat.newest.max(converted.id);
                // Written from another device: read there
                entry.chat.unread = if converted.mine { 0 } else { entry.chat.unread + 1 };
                entry.chat.messages.push(converted);
                let extra = entry.chat.messages.len().saturating_sub(MESSAGES);
                for gone in entry.chat.messages.drain(..extra) {
                    entry.raw.remove(&telegram_id(gone.id));
                }
            }
            None => {}
        }
        true
    }

    fn deleted(&mut self, channel: Option<i64>, ids: &[i32]) {
        for entry in &mut self.entries {
            // A channel's ids are its own; the others share one count
            let own = entry.peer.id.kind() == PeerKind::Channel;
            if own != channel.is_some() || channel.is_some_and(|c| entry.peer.id.bare_id() != Some(c)) {
                continue;
            }
            entry.chat.messages.retain(|m| !ids.contains(&telegram_id(m.id)));
            for id in ids {
                entry.raw.remove(id);
            }
        }
    }

    /// Something Telegram tells; true when the chats changed.
    async fn update(&mut self, update: Update) -> Result<bool, InvocationError> {
        match update {
            Update::NewMessage(m) => {
                let m: grammers_client::message::Message = m.into_inner();
                let id = jid(m.peer_id());
                if self.ignored.contains(&id) {
                    return Ok(false);
                }
                if !self.message(m, true) {
                    // A chat that wasn't among the latest: it is now, unless it's one left out
                    self.load().await?;
                    if self.entry(&id).is_none() {
                        self.ignored.insert(id);
                    }
                }
                Ok(true)
            }
            Update::MessageEdited(m) => Ok(self.message(m.into_inner(), false)),
            Update::MessageDeleted(d) => {
                self.deleted(d.channel_id(), d.messages());
                Ok(true)
            }
            Update::Raw(raw) => Ok(match &raw.raw {
                // Read on another device
                tl::enums::Update::ReadHistoryInbox(u) => {
                    let id = jid(PeerId::from(u.peer.clone()));
                    self.entry(&id).map(|e| e.chat.unread = u.still_unread_count as i64).is_some()
                }
                tl::enums::Update::ReadChannelInbox(u) => {
                    let id = jid(PeerId::channel_unchecked(u.channel_id));
                    self.entry(&id).map(|e| e.chat.unread = u.still_unread_count as i64).is_some()
                }
                _ => false,
            }),
            _ => Ok(false),
        }
    }

    fn finished(&mut self, done: Done) {
        match done {
            Done::History { jid, messages } => {
                let Some(entry) = self.entry(&jid) else { return };
                let group = entry.chat.group;
                let old = std::mem::take(&mut entry.chat.messages);
                entry.raw.clear();
                for m in messages {
                    let Some(mut converted) = convert(&m, group) else { continue };
                    if let Some(before) = old.iter().find(|o| o.id == converted.id) {
                        (converted.file, converted.thumb) = (before.file.clone(), before.thumb.clone());
                    }
                    entry.raw.insert(m.id(), m);
                    entry.chat.messages.push(converted);
                }
                // One that came while the answer was on its way
                for m in old {
                    if entry.chat.messages.last().is_none_or(|last| m.id > last.id) {
                        entry.chat.messages.push(m);
                    }
                }
                entry.loaded = true;
                if self.open.as_deref() == Some(jid.as_str()) {
                    self.show(&jid);
                }
            }
            Done::Picture { jid, path } => {
                if let Some(entry) = self.entry(&jid) {
                    entry.chat.picture = Some(path);
                }
            }
            Done::File { jid, id, file, thumb } => {
                if let Some(m) = self.entry(&jid).and_then(|e| e.chat.messages.iter_mut().find(|m| m.id == id)) {
                    m.file = file.or(m.file.take());
                    m.thumb = thumb.or(m.thumb.take());
                }
            }
        }
    }

    /// What the widget asks; true when the chats changed.
    async fn command(&mut self, cmd: Cmd) -> Result<bool, InvocationError> {
        match cmd {
            Cmd::Refresh => {
                self.ignored.clear();
                // After a wake the connection may be gone without a word: no answer is an error
                match tokio::time::timeout(std::time::Duration::from_secs(20), self.load()).await {
                    Ok(loaded) => loaded?,
                    Err(_) => return Err(InvocationError::Dropped("no answer")),
                }
                Ok(true)
            }
            Cmd::Open(jid) => {
                if self.entry(&jid).is_some_and(|e| !e.loaded) {
                    self.histories(vec![jid.clone()]);
                }
                self.show(&jid);
                self.open = Some(jid);
                Ok(false)
            }
            Cmd::Close => {
                self.open = None;
                Ok(false)
            }
            Cmd::Send { jid, text, done } => {
                let Some(peer) = self.entry(&jid).map(|e| e.peer) else {
                    done(Err(t("Questa chat non c'è più.", "This chat is gone.").into()));
                    return Ok(false);
                };
                match self.client.send_message(peer, text.as_str()).await {
                    Ok(sent) => {
                        // An answer reads the chat, as in any Telegram
                        let _ = self.client.mark_as_read(peer).await;
                        self.message(sent, true);
                        if let Some(e) = self.entry(&jid) {
                            e.chat.unread = 0;
                        }
                        done(Ok(()));
                        Ok(true)
                    }
                    Err(e) => {
                        done(Err(format!("{}: {e}", t("Telegram non ha inviato", "Telegram didn't send it"))));
                        Ok(false)
                    }
                }
            }
            Cmd::Read { jid, done } => {
                let Some(peer) = self.entry(&jid).map(|e| e.peer) else {
                    done(Err(t("Questa chat non c'è più.", "This chat is gone.").into()));
                    return Ok(false);
                };
                match self.client.mark_as_read(peer).await {
                    Ok(()) => {
                        if let Some(e) = self.entry(&jid) {
                            e.chat.unread = 0;
                        }
                        done(Ok(()));
                        Ok(true)
                    }
                    Err(e) => {
                        done(Err(format!("{}: {e}", t("Telegram non ha segnato la chat come letta", "Telegram didn't mark the chat read"))));
                        Ok(false)
                    }
                }
            }
            Cmd::Fetch { jid, id, done } => {
                let raw = self.entry(&jid).and_then(|e| e.raw.get(&telegram_id(id)).cloned());
                let (client, finished) = (self.client.clone(), self.done.clone());
                tokio::spawn(async move {
                    let failed = || t("Telegram non ha dato il file.", "Telegram didn't give the file.").to_string();
                    let file = match raw.as_ref().and_then(|m| m.media()) {
                        Some(Media::Document(d)) => download(&client, &d, &cache_file(&jid, telegram_id(id), "", &extension(&d, "bin"))).await,
                        Some(Media::Photo(p)) => download(&client, &p, &cache_file(&jid, telegram_id(id), "_full", "jpg")).await,
                        _ => None,
                    };
                    if let Some(file) = &file {
                        let _ = finished.send(Done::File { jid, id, file: Some(file.clone()), thumb: None });
                    }
                    done(file.ok_or_else(failed));
                });
                Ok(false)
            }
        }
    }
}

/// One connection, until it fails or the widget is gone (`Ok`).
async fn session(store: Arc<Store>, recent: usize, channels: bool, cmds: &mut mpsc::UnboundedReceiver<Cmd>, event: &(impl Fn(Result<Vec<Chat>, Error>) + Send + 'static)) -> Result<(), Error> {
    let other = |e: InvocationError| Error::Other(e.to_string());
    let (client, handle, updates, task) = connect(&store);
    let result = async {
        if !client.is_authorized().await.map_err(other)? {
            return Err(Error::SignedOut);
        }
        let _ = std::fs::create_dir_all(cache_dir());
        let (done, mut finished) = mpsc::unbounded_channel();
        let mut connection = Connection { client: client.clone(), entries: Vec::new(), channels, recent, done, ignored: Default::default(), open: None };
        connection.load().await.map_err(other)?;
        event(Ok(connection.snapshot()));
        let mut updates = client.stream_updates(updates, UpdatesConfiguration { catch_up: false }).await.map_err(|e| Error::Other(e.to_string()))?;
        let mut saved = std::time::Instant::now();
        loop {
            let changed = tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(cmd) => connection.command(cmd).await.map_err(other)?,
                    // The widget is gone
                    None => {
                        let _ = updates.sync_update_state().await;
                        return Ok(());
                    }
                },
                update = updates.next() => connection.update(update.map_err(other)?).await.map_err(other)?,
                Some(done) = finished.recv() => {
                    connection.finished(done);
                    true
                }
            };
            if changed {
                event(Ok(connection.snapshot()));
            }
            // The place in Telegram's updates, now and then: with what happens, never on a timer
            if saved.elapsed().as_secs() > 60 {
                let _ = updates.sync_update_state().await;
                store.touch();
                store.save();
                saved = std::time::Instant::now();
            }
        }
    }
    .await;
    store.save();
    handle.quit();
    let _ = task.await;
    result
}

/// Connects on a thread of its own and calls `event` (there) with the chats each time they
/// change, or with why there are none; until the handle is dropped. Without a session it waits
/// for a `Cmd::Refresh`, which a login sends.
pub fn start(recent: usize, channels: bool, event: impl Fn(Result<Vec<Chat>, Error>) + Send + 'static) -> Handle {
    let (tx, mut cmds) = mpsc::unbounded_channel();
    crate::runtime::spawn("telegram", move || {
        let Ok(runtime) = runtime() else { return };
        runtime.block_on(async move {
            let mut wait = 5;
            loop {
                let failure = match Store::load() {
                    None => Error::SignedOut,
                    Some(store) => {
                        let began = std::time::Instant::now();
                        let ended = session(Arc::new(store), recent, channels, &mut cmds, &event).await;
                        if began.elapsed().as_secs() > 120 {
                            wait = 5;
                        }
                        match ended {
                            Ok(()) => return,
                            Err(e) => e,
                        }
                    }
                };
                if let Error::Other(e) = &failure {
                    crate::log(&format!("telegram: {e}"));
                }
                let signed_out = failure == Error::SignedOut;
                event(Err(failure));
                // Signed out, until something is asked (a login); else again in a while, sooner
                // if asked
                let asked = if signed_out {
                    cmds.recv().await
                } else {
                    tokio::select! {
                        cmd = cmds.recv() => cmd,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(wait)) => Some(Cmd::Refresh),
                    }
                };
                match asked {
                    None => return,
                    // What was asked of a connection that isn't there has no answer but this
                    Some(Cmd::Send { done, .. } | Cmd::Read { done, .. }) => done(Err(t("Telegram non è collegato.", "Telegram isn't connected.").into())),
                    Some(Cmd::Fetch { done, .. }) => done(Err(t("Telegram non è collegato.", "Telegram isn't connected.").into())),
                    Some(_) => {}
                }
                wait = (wait * 2).min(120);
            }
        });
    });
    Handle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_message_has_a_higher_id_across_chats() {
        // A channel counts its own messages: its 7 came after a chat's 5000
        assert!(message_id(1_800_000_100, 7) > message_id(1_800_000_000, 5000));
        assert!(message_id(1_800_000_000, 8) > message_id(1_800_000_000, 7));
        assert_eq!(telegram_id(message_id(1_800_000_000, 5000)), 5000);
        assert_eq!(telegram_id(message_id(1_800_000_000, i32::MAX)), i32::MAX);
    }

    #[test]
    fn a_chat_opens_in_the_app_by_its_id() {
        assert_eq!(link("700100"), "tg://openmessage?user_id=700100");
        assert_eq!(link("-1001700200"), "tg://openmessage?chat_id=1700200");
        assert_eq!(link("-4500"), "tg://openmessage?chat_id=4500");
    }

    #[test]
    fn the_session_is_what_was_saved() {
        let store = Store::new(12345);
        store.data().unwrap().home_dc = 4;
        let data = store.data().unwrap();
        let saved = Saved { api_id: store.api_id, home_dc: data.home_dc, dcs: data.dc_options.values().cloned().collect(), peers: vec![], updates: data.updates_state.clone() };
        let json = serde_json::to_string(&saved).unwrap();
        let back: Saved = serde_json::from_str(&json).unwrap();
        assert_eq!((back.api_id, back.home_dc, back.dcs.len()), (12345, 4, saved.dcs.len()));
    }
}
