//! What Music or Spotify is playing. In the bar: the cover, the title and the artist (the title
//! only, when compact), dimmed with a pause sign while paused; hidden when nothing plays, or while
//! paused with `paused = false`. The popup is a player: the cover, the title, artist and album, a
//! bar to seek with the time gone and left, shuffle, previous, play/pause, next and repeat, the
//! app's own volume, favorite (Music) and the app; a switch between the two when both have
//! something.
//!
//! macOS has no public way to know what other apps play, and MediaRemote, the private framework of
//! Control Center, answers only Apple's own processes since macOS 15.4. So the widget reads the two
//! players that tell: Music and Spotify post a distributed notification at every change of state or
//! track, which needs no permission. What it doesn't say (the position in Music, the cover,
//! shuffle, repeat, the volume) and the controls go through Apple events to the app, in-process,
//! with the Automation permission: only to an app that runs, addressed by its pid so that an event
//! never launches it, and without asking for the permission (an event that would ask fails
//! instead) until a click or the popup. Music's cover comes in the reply; Spotify's is downloaded
//! from the URL it gives (its CDN). Each is scaled down to a small PNG in the cache folder, one per
//! app, for the image nodes.
//!
//! The popup takes its colors from the cover, as Android's media player does (see `palette`): the
//! background from the color the cover has most of, the play button, the bars and what's on from
//! its liveliest color. Worked out on a background thread when a cover comes, from a copy of it
//! 64 pixels wide; with `cover_colors = false`, or without a cover, the theme's.
//!
//! With `other_apps` (off by default), any other app too: a browser, Podcasts… Only MediaRemote
//! knows, so a helper asks it from inside /usr/bin/perl, which Apple signs (`now_playing.m`, built
//! by build.rs and carried in the binary): one process for as long as the option is on, idle until
//! MediaRemote says something changed. It's a third player, for an app with a player that is
//! neither Music nor Spotify (those two say more on their own): the one playing, else the one it
//! showed, paused, so a video paused under Spotify stays. It has play/pause and, if the app takes
//! them, previous, next and seek, sent to that app; no permission is needed. A trick that a
//! macOS update may end: then the helper says so in the log, or just never sees anything playing,
//! and the widget has Music and Spotify as before.
//!
//! On a screen with a notch, as the first of the center group, it's an extension of the notch with
//! `notch` (on by default; see `Rx::island`): black as the notch and as tall, curving into the top
//! of the screen as it does. Left of it the cover, the title and the artist, which take the room
//! the bar leaves there; right of it bars that move to the music in the cover's liveliest color,
//! or the pause sign. Under the mouse it grows down with the peek, on a click into the player, in
//! the cover's colors on the notch's black, and by itself for a moment when the track changes,
//! with the cover bigger (see `notice`): not for the first one, nor with the mouse on it.
//!
//! With made-up data (see `mock`), the players of `now_playing.json`, whose `artwork` is relative
//! to its folder (the `other` one with `other_apps` only); the controls change them in memory only,
//! and ⏮ ⏭ go through the tracks of its `queue`.

use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey, NSWorkspaceDidLaunchApplicationNotification,
    NSWorkspaceDidTerminateApplicationNotification,
};
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo, kCGColorSpaceSRGB};
use objc2_foundation::{NSDistributedNotificationCenter, NSNotification, NSNumber, NSObjectProtocol, NSString};
use serde::Deserialize;

use super::{Cx, Event, Island, Rx, Widget};
use crate::config::Options;
use crate::gfx::Weight;
use crate::i18n::t;
use crate::palette::Palette;
use crate::theme::Theme;
use crate::time;
use crate::ui::{Align, Justify, Kind, Node};

const WIDTH: f64 = 300.0;
/// In the island: the least the title takes, and how wide the peek and the notice are at least.
const ISLAND_TEXT: f64 = 36.0;
const ISLAND_WIDTH: f64 = 300.0;
const COVER: f64 = 76.0;
/// The side of the covers kept on disk, in pixels: the popup's at 2x, with room to spare.
const COVER_PIXELS: i64 = 320;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum App {
    #[default]
    Music,
    Spotify,
    /// Whichever other app MediaRemote says plays, with `other_apps`.
    Other,
}

/// The apps read on their own, with notifications and Apple events.
const APPS: [App; 2] = [App::Music, App::Spotify];
const ALL: [App; 3] = [App::Music, App::Spotify, App::Other];

impl App {
    /// Empty for `Other`: its player has the app's.
    fn bundle(self) -> &'static str {
        match self {
            App::Music => "com.apple.Music",
            App::Spotify => "com.spotify.client",
            App::Other => "",
        }
    }

    fn name(self) -> &'static str {
        match self {
            App::Music => "Music",
            App::Spotify => "Spotify",
            App::Other => "",
        }
    }

    fn key(self) -> &'static str {
        match self {
            App::Music => "music",
            App::Spotify => "spotify",
            App::Other => "other",
        }
    }

    /// The distributed notification it posts at every change.
    fn notification(self) -> &'static str {
        match self {
            App::Music => "com.apple.Music.playerInfo",
            App::Spotify => "com.spotify.client.PlaybackStateChanged",
            App::Other => unreachable!("the other apps are the helper's"),
        }
    }

    /// The event class of its playback commands.
    fn suite(self) -> u32 {
        ae::fc(match self {
            App::Music => b"hook",
            App::Spotify => b"spfy",
            App::Other => unreachable!("the other apps are the helper's"),
        })
    }

    fn pid(self) -> Option<i32> {
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(self.bundle()));
        apps.iter().find(|a| !a.isTerminated()).map(|a| a.processIdentifier())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum State {
    #[default]
    Stopped,
    Playing,
    Paused,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Repeat {
    #[default]
    Off,
    All,
    One,
}

/// The Automation permission to send the app Apple events.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Access {
    /// Not known yet: no event sent.
    #[default]
    Unknown,
    Granted,
    /// macOS would ask.
    Ask,
    Denied,
}

/// What an app of `other_apps` takes besides play/pause, as MediaRemote says.
#[derive(Clone, Copy, Debug, Deserialize)]
struct Controls {
    next: bool,
    previous: bool,
    seek: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Player {
    app: App,
    /// The app's: Music's and Spotify's, or the other one's.
    bundle: String,
    name: String,
    /// None: all of them (Music and Spotify).
    controls: Option<Controls>,
    /// The other app, while macOS sends the controls to another one (the one it calls playing,
    /// named here, empty if none): they would reach that one instead, so there are none.
    controls_go_to: Option<String>,
    state: State,
    title: String,
    artist: String,
    album: String,
    /// Seconds; 0 when not known (a stream).
    duration: f64,
    /// Seconds, at `at`.
    position: f64,
    shuffle: Option<bool>,
    repeat: Option<Repeat>,
    /// The app's own volume, 0–100.
    volume: Option<f64>,
    /// Favorite, in Music.
    favorited: Option<bool>,
    /// A file with the cover.
    artwork: Option<String>,
    #[serde(skip)]
    at: f64,
    /// While the app runs.
    #[serde(skip)]
    pid: Option<i32>,
    #[serde(skip)]
    access: Access,
    /// When it last changed state or track: of two players, the bar shows the latest.
    #[serde(skip)]
    changed: f64,
    /// The track (see `track_key`) whose cover `artwork` is, or is on its way.
    #[serde(skip)]
    cover_key: String,
    /// The colors of the cover at this path, for the popup: None while they're worked out, or of
    /// a picture without any that shows.
    #[serde(skip)]
    colors: Option<(String, Option<Palette>)>,
    /// The last read asked for: the replies of the ones before are dropped.
    #[serde(skip)]
    reads: u64,
}

impl Player {
    fn new(app: App) -> Player {
        Player { app, bundle: app.bundle().into(), name: app.name().into(), ..Default::default() }
    }

    fn can_skip(&self) -> (bool, bool) {
        self.controls.map_or((true, true), |c| (c.previous, c.next))
    }

    fn can_seek(&self) -> bool {
        self.duration > 0.0 && self.controls_go_to.is_none() && self.controls.is_none_or(|c| c.seek)
    }

    fn position_at(&self, now: f64) -> f64 {
        let p = if self.state == State::Playing { self.position + (now - self.at).max(0.0) } else { self.position };
        if self.duration > 0.0 { p.clamp(0.0, self.duration) } else { p.max(0.0) }
    }

    /// Something to show: a track, playing or paused, of an app that runs.
    fn active(&self) -> bool {
        self.pid.is_some() && !self.title.is_empty() && self.state != State::Stopped
    }

    fn reset(&mut self) {
        *self = Player { access: self.access, ..Player::new(self.app) };
    }
}

/// What tells a track apart: its title, artist and album. The same in the notifications and in the
/// replies to Apple events, unlike the ids, which the two give in different forms.
fn track_key(title: &str, artist: &str, album: &str) -> String {
    format!("{title}\u{1f}{artist}\u{1f}{album}")
}

/// Made-up data (see `mock`).
#[derive(Default, Deserialize)]
#[serde(default)]
struct Mock {
    players: Vec<Player>,
    /// The tracks ⏮ ⏭ go through: title, artist, album, duration and artwork.
    queue: Vec<Player>,
}

pub struct ObserverIvars {
    f: Box<dyn Fn(&NSNotification)>,
}

define_class!(
    /// Gets the distributed notifications of a player and calls its closure. Registered to have
    /// them delivered at once: by default they wait while the app isn't active, and RustyBar never
    /// is.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ObserverIvars]
    pub struct NowPlayingObserver;

    impl NowPlayingObserver {
        #[unsafe(method(notified:))]
        fn notified(&self, n: &NSNotification) {
            (self.ivars().f)(n);
        }
    }
);

impl NowPlayingObserver {
    fn new(mtm: MainThreadMarker, f: impl Fn(&NSNotification) + 'static) -> Retained<NowPlayingObserver> {
        let this = Self::alloc(mtm).set_ivars(ObserverIvars { f: Box::new(f) });
        unsafe { msg_send![super(this), init] }
    }
}

pub struct NowPlaying {
    /// Also while paused, in the bar.
    paused: bool,
    /// The popup in the colors of the cover.
    cover_colors: bool,
    /// Any app, through the helper in perl.
    other_apps: bool,
    /// Under the mouse the pill grows down with the quick controls (see `peek`).
    hover: bool,
    /// On a screen with a notch, an extension of it (see the top).
    notch: bool,
    mock: bool,
    /// The made-up tracks of ⏮ ⏭, after the one playing.
    queue: Vec<Player>,
    /// The track the bar played last (see `watch_track`), none before the first.
    noticed: Option<String>,
    /// Music, Spotify and the other app, by `App`.
    players: [Player; 3],
    /// The player the popup shows, picked in it; else the bar's.
    pick: Option<App>,
    /// Counts the popup's openings and closings: the timer of an older one stops.
    ticking: u64,
    observers: Vec<Retained<NowPlayingObserver>>,
    workspace: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
    /// The helper's commands, while it runs. Dropped with the widget (a reload, a quit), it ends
    /// the helper.
    helper: Option<ChildStdin>,
    /// Its starts since it last said something: each next one waits longer.
    helper_starts: u32,
    /// It said MediaRemote isn't what it knows: not started again.
    helper_broken: bool,
}

impl NowPlaying {
    pub fn new(options: &Options) -> NowPlaying {
        NowPlaying {
            paused: options.bool("paused").unwrap_or(true),
            cover_colors: options.bool("cover_colors").unwrap_or(true),
            other_apps: options.bool("other_apps").unwrap_or(false),
            hover: options.bool("hover").unwrap_or(true),
            notch: options.bool("notch").unwrap_or(true),
            mock: false,
            queue: Vec::new(),
            noticed: None,
            players: ALL.map(Player::new),
            pick: None,
            ticking: 0,
            observers: Vec::new(),
            workspace: Vec::new(),
            helper: None,
            helper_starts: 0,
            helper_broken: false,
        }
    }

    /// The player of the bar: the one playing, or else the one paused last (with `paused`).
    fn current(&self) -> Option<&Player> {
        self.players
            .iter()
            .filter(|p| p.active() && (self.paused || p.state == State::Playing))
            .max_by(|a, b| (a.state == State::Playing, a.changed).partial_cmp(&(b.state == State::Playing, b.changed)).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// The player of the popup: the one picked in it, or the latest, paused too.
    fn shown(&self) -> Option<&Player> {
        let picked = self.pick.map(|a| &self.players[a as usize]).filter(|p| p.active());
        picked.or_else(|| self.current()).or_else(|| self.players.iter().filter(|p| p.active()).max_by(|a, b| a.changed.total_cmp(&b.changed)))
    }

    /// The colors of the cover `p` shows, for the popup.
    fn palette(&self, p: &Player) -> Option<Palette> {
        let (of, palette) = p.colors.as_ref()?;
        if self.cover_colors && p.artwork.as_ref() == Some(of) { *palette } else { None }
    }

    fn load_mock(&mut self) -> bool {
        let Some(mock) = crate::mock::load::<Mock>("now_playing") else { return false };
        self.mock = true;
        let dir = PathBuf::from(std::env::var_os("RUSTYBAR_MOCK").unwrap_or_default());
        let now = time::now();
        for (i, mut p) in mock.players.into_iter().enumerate() {
            if p.app == App::Other && !self.other_apps {
                continue;
            }
            if p.bundle.is_empty() {
                p.bundle = p.app.bundle().into();
                p.name = p.app.name().into();
            }
            p.pid = Some(0);
            p.access = Access::Granted;
            p.at = now;
            p.changed = now - i as f64;
            p.artwork = p.artwork.map(|a| dir.join(a).to_string_lossy().into_owned());
            let app = p.app as usize;
            self.players[app] = p;
        }
        self.queue = mock.queue;
        for track in &mut self.queue {
            track.artwork = track.artwork.take().map(|a| dir.join(a).to_string_lossy().into_owned());
        }
        true
    }

    /// After a change of what plays: on a track that follows another one, the island shows it
    /// for a moment (see `notice`). Not for the first after the start, nor for a pause.
    fn watch_track(&mut self, cx: &mut Cx) {
        let Some(key) = self.current().filter(|p| p.state == State::Playing).map(|p| format!("{}\u{1f}{}", p.bundle, track_key(&p.title, &p.artist, &p.album))) else {
            return;
        };
        if self.noticed.as_ref().is_some_and(|k| *k != key) {
            cx.notice();
        }
        self.noticed = Some(key);
    }

    /// Listens to the players' notifications and to their launches and quits.
    fn observe(&mut self, cx: &mut Cx) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        let center = NSDistributedNotificationCenter::defaultCenter();
        for app in APPS {
            let handle = cx.handle::<NowPlaying>();
            let observer = NowPlayingObserver::new(mtm, move |n| {
                let info = info(app, n);
                handle.send(move |w, cx| w.notified(app, info, cx));
            });
            unsafe {
                let _: () = msg_send![
                    &*center,
                    addObserver: &*observer,
                    selector: sel!(notified:),
                    name: &*NSString::from_str(app.notification()),
                    object: Option::<&NSString>::None,
                    suspensionBehavior: 4usize // NSNotificationSuspensionBehaviorDeliverImmediately
                ];
            }
            self.observers.push(observer);
        }
        let ws = NSWorkspace::sharedWorkspace();
        for (name, launched) in unsafe { [(NSWorkspaceDidLaunchApplicationNotification, true), (NSWorkspaceDidTerminateApplicationNotification, false)] } {
            let handle = cx.handle::<NowPlaying>();
            let block = RcBlock::new(move |n: NonNull<NSNotification>| {
                let n = unsafe { n.as_ref() };
                let running = n
                    .userInfo()
                    .and_then(|info| info.objectForKey(unsafe { NSWorkspaceApplicationKey }))
                    .and_then(|o| o.downcast::<NSRunningApplication>().ok());
                let Some(running) = running else { return };
                let bundle = running.bundleIdentifier().map(|b| b.to_string()).unwrap_or_default();
                let Some(app) = APPS.into_iter().find(|a| a.bundle() == bundle) else { return };
                let pid = running.processIdentifier();
                handle.send(move |w, cx| {
                    let p = &mut w.players[app as usize];
                    if launched {
                        p.pid = Some(pid);
                    } else if p.pid == Some(pid) {
                        p.reset();
                        cx.redraw();
                    }
                });
            });
            self.workspace.push(unsafe { ws.notificationCenter().addObserverForName_object_queue_usingBlock(Some(name), None, None, &block) });
        }
    }

    /// A notification of `app`: the state and the track at once, then the rest from the app.
    fn notified(&mut self, app: App, info: Info, cx: &mut Cx) {
        let now = time::now();
        let p = &mut self.players[app as usize];
        if p.pid.is_none() {
            p.pid = app.pid();
        }
        let same = track_key(&info.title, &info.artist, &info.album) == track_key(&p.title, &p.artist, &p.album);
        // Spotify tells the position; in Music it goes on from where it was, or from the start
        p.position = info.position.unwrap_or_else(|| if same { p.position_at(now) } else { 0.0 });
        p.at = now;
        p.state = info.state;
        p.changed = now;
        if !same {
            p.title = info.title;
            p.artist = info.artist;
            p.album = info.album;
            p.favorited = None;
            // Without the permission no cover will come: not the last track's
            if p.access != Access::Granted {
                p.artwork = None;
                p.cover_key.clear();
            }
        }
        if info.duration > 0.0 || !same {
            p.duration = info.duration;
        }
        if p.state == State::Stopped {
            p.title.clear();
        }
        cx.redraw();
        if matches!(p.access, Access::Granted | Access::Unknown) {
            self.refresh(app, false, cx);
        }
        self.watch_track(cx);
    }

    /// Reads all of the player from its app, in the background; `ask` lets macOS ask for the
    /// permission.
    fn refresh(&mut self, app: App, ask: bool, cx: &mut Cx) {
        let p = &mut self.players[app as usize];
        let Some(pid) = p.pid.filter(|_| !self.mock && app != App::Other) else { return };
        p.reads += 1;
        let (reads, have) = (p.reads, p.cover_key.clone());
        cx.background::<NowPlaying, _>(move || read(app, pid, ask, &have), move |w, result, cx| w.read_done(app, reads, result, cx));
    }

    fn read_done(&mut self, app: App, reads: u64, result: Result<Read, i32>, cx: &mut Cx) {
        let now = time::now();
        let p = &mut self.players[app as usize];
        if reads != p.reads {
            return;
        }
        let r = match result {
            Ok(r) => r,
            Err(e) => {
                match e {
                    ae::DENIED => p.access = Access::Denied,
                    ae::WOULD_ASK => p.access = Access::Ask,
                    ae::NOT_RUNNING | ae::NO_CONNECTION => p.reset(),
                    _ => crate::log(&format!("now_playing: {} replied {e}", app.name())),
                }
                cx.redraw();
                return;
            }
        };
        p.access = Access::Granted;
        if r.state != p.state || track_key(&r.title, &r.artist, &r.album) != track_key(&p.title, &p.artist, &p.album) {
            p.changed = now;
        }
        p.state = if r.title.is_empty() { State::Stopped } else { r.state };
        p.title = r.title;
        p.artist = r.artist;
        p.album = r.album;
        p.duration = r.duration;
        p.position = r.position;
        p.at = now;
        p.shuffle = r.shuffle;
        p.repeat = r.repeat;
        p.volume = r.volume;
        p.favorited = r.favorited;
        if let Some(cover) = r.cover {
            p.cover_key = r.key.clone();
            match cover {
                Cover::File(path) => p.artwork = Some(path),
                Cover::Missing => p.artwork = None,
                Cover::Url(url) => download(app, r.key, url, cx.handle::<NowPlaying>()),
            }
        }
        self.colorize(cx);
        cx.redraw();
        self.watch_track(cx);
    }

    /// Works out, in the background, the colors of the covers that have none yet.
    fn colorize(&mut self, cx: &mut Cx) {
        if !self.cover_colors {
            return;
        }
        for app in ALL {
            let p = &mut self.players[app as usize];
            let Some(path) = p.artwork.clone() else { continue };
            if p.colors.as_ref().is_some_and(|(of, _)| *of == path) {
                continue;
            }
            p.colors = Some((path.clone(), None));
            let of = path.clone();
            cx.background::<NowPlaying, _>(
                move || colors(&of),
                move |w, palette, cx| {
                    let p = &mut w.players[app as usize];
                    if p.colors.as_ref().is_some_and(|(of, _)| *of == path) {
                        p.colors = Some((path, palette));
                        if cx.popup_open {
                            cx.redraw();
                        }
                    }
                },
            );
        }
    }

    /// Sends the app an event that changes something, then reads it all again: what the app made
    /// of it, and a note if it isn't allowed.
    fn send(&mut self, app: App, op: impl FnOnce(i32) -> Result<(), i32> + Send + 'static, cx: &mut Cx) {
        let p = &self.players[app as usize];
        let Some(pid) = p.pid.filter(|_| !self.mock) else { return };
        if p.access == Access::Denied {
            return;
        }
        cx.background::<NowPlaying, _>(
            move || op(pid),
            move |w, result, cx| {
                let p = &mut w.players[app as usize];
                match result {
                    Err(ae::DENIED) => p.access = Access::Denied,
                    Err(ae::WOULD_ASK) => p.access = Access::Ask,
                    Err(e) if e != ae::NO_OBJECT => crate::log(&format!("now_playing: {} replied {e}", app.name())),
                    _ => p.access = Access::Granted,
                }
                if p.access == Access::Granted {
                    w.refresh(app, false, cx);
                }
                cx.redraw();
            },
        );
    }

    /// The cover, or the app's icon without one.
    fn cover(&self, rx: &Rx, p: &Player, size: f64, radius: f64) -> Node {
        match &p.artwork {
            Some(path) => Node::image(path.as_str(), size, size, None).radius(radius),
            None if size <= 32.0 => Node::app_icon(&p.bundle, size),
            None => Node::row(vec![Node::app_icon(&p.bundle, size * 0.6).key("icon")])
                .size(size, size)
                .justify(Justify::Center)
                .radius(radius)
                .background(rx.theme.tile(rx.theme.primary, false)),
        }
    }

    /// The room of the island, when the widget is one (see `Rx::island`).
    fn island(&self, rx: &Rx) -> Option<Island> {
        rx.island.filter(|_| self.notch)
    }

    /// The notch's black, with the liveliest color of the cover `p` shows for an accent.
    fn notch_theme(&self, theme: &Theme, p: &Player) -> Theme {
        theme.notch(self.palette(p).map(|palette| palette.dark_accent()))
    }

    /// The bar's player as an extension of the notch (see the top): left of it the cover, the
    /// title and the artist, as in the pill; right of it bars that move to the music, in the
    /// cover's liveliest color, or the pause sign, the cover dimmed. The cover and the bars always
    /// have their room; the title and the artist take what the bar leaves beside them.
    fn island_bar(&self, rx: &Rx, p: &Player, island: Island) -> Node {
        let theme = self.notch_theme(rx.theme, p);
        let rx = &Rx { theme: &theme, ..*rx };
        let th = rx.theme;
        let playing = p.state == State::Playing;
        let size = island_cover(island);
        // The cover as far from the island's edge as from its top and bottom
        let inset = ((island.height - size) / 2.0).round();
        let (title_w, artist_w) = island_text(th, p, island.left);
        let mut left = vec![
            self.cover(rx, p, size, 6.0).opacity(if playing { 1.0 } else { 0.6 }).key("cover"),
            Node::text(&p.title, th.font(), if playing { th.text } else { th.subtext }).max_width(title_w).marquee(playing).key("title"),
        ];
        if artist_w > 0.0 {
            left.push(Node::text(&p.artist, th.font(), th.subtext).max_width(artist_w).key("artist"));
        }
        let left = Node::row(left).gap(8.0).align(Align::Center).height(island.height).padding(0.0, 8.0, 0.0, inset).key("wing");
        let sign = match playing {
            true => Node::new(Kind::Bars { color: th.primary }).key("bars"),
            false => Node::symbol("pause.fill", 12.0, Weight::Bold, th.subtext).key("paused"),
        };
        let right = Node::row(vec![Node::row(vec![sign]).size(size, size).justify(Justify::Center).align(Align::Center).key("sign")])
            .align(Align::Center)
            .height(island.height)
            .padding(0.0, inset, 0.0, 8.0)
            .key("wing");
        // Under the mouse it grows (see `peek`): no highlight of its own
        rx.island(vec![left], vec![right]).click(rx.toggle_popup()).hover(crate::gfx::Rgba(0.0, 0.0, 0.0, 0.0))
    }

    /// The popup with nothing to play: the apps to open.
    fn idle_popup(&self, rx: &Rx) -> Node {
        let th = rx.theme;
        let mut rows = vec![
            Node::row(vec![Node::symbol("music.note", 15.0, Weight::Semibold, th.subtext), Node::text(t("Niente in riproduzione", "Nothing playing"), th.font(), th.subtext)])
                .gap(8.0)
                .padding(6.0, 8.0, 6.0, 8.0)
                .key("idle"),
            rx.divider().key("divider"),
        ];
        for app in APPS {
            rows.push(open_row(rx, &self.players[app as usize]));
        }
        rx.popup(WIDTH, rows).gap(4.0)
    }
}

/// A row that opens the app of `p`.
fn open_row(rx: &Rx, p: &Player) -> Node {
    let th = rx.theme;
    let key = format!("open:{}", p.app.key());
    rx.row_button(
        vec![Node::app_icon(&p.bundle, 16.0).key("icon"), Node::text(format!("{} {}", t("Apri", "Open"), p.name), th.font(), th.text).key("name")],
        vec![],
        rx.action(&key),
    )
    .hover(th.highlight)
    .key(key)
}

/// How big the cover is in the island: as far from its top as from its bottom, as in the pill.
fn island_cover(island: Island) -> f64 {
    (island.height - 12.0).clamp(12.0, 28.0)
}

/// How wide the title and the artist are in the island's left wing, with `room` beside it: the
/// title the least it takes, and then the room, to the title first and then to the artist, as
/// far as they need it; no artist (0) without room for some of it.
fn island_text(th: &Theme, p: &Player, room: f64) -> (f64, f64) {
    let font = th.font();
    let width = |text: &str| crate::gfx::measure_text(text, &font).width.ceil();
    let whole = width(&p.title);
    let least = whole.min(ISLAND_TEXT);
    let mut spare = room.max(0.0);
    let more = (whole - least).clamp(0.0, spare);
    spare -= more;
    // The artist after a gap, and only if at least the least of it fits
    let artist = if p.artist.is_empty() { 0.0 } else { width(&p.artist) };
    let artist_w = if artist > 0.0 && spare - 8.0 >= artist.min(ISLAND_TEXT) { artist.min(spare - 8.0) } else { 0.0 };
    (least + more, artist_w)
}

/// `secs` as 3:07, or 1:02:03.
fn clock(secs: f64) -> String {
    let s = secs.max(0.0).floor() as u64;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// A round button of the player with a symbol: `lit` in the accent, as shuffle and repeat when on.
fn control(rx: &Rx, symbol: &str, size: f64, lit: bool, action: &str) -> Node {
    let th = rx.theme;
    let mut b = Node::row(vec![Node::symbol(symbol, size, Weight::Semibold, if lit { th.primary } else { th.text }).key("symbol")])
        .size(40.0, 36.0)
        .justify(Justify::Center)
        .radius(18.0)
        .click(rx.action(action))
        .hover(th.highlight)
        .key(action);
    if lit {
        b = b.background(th.tile(th.primary, true));
    }
    b
}

impl Widget for NowPlaying {
    fn start(&mut self, cx: &mut Cx) {
        if self.load_mock() {
            self.colorize(cx);
            return;
        }
        self.observe(cx);
        // What already plays: only from an app that lets RustyBar ask, without a prompt at login
        for app in APPS {
            self.players[app as usize].pid = app.pid();
            self.refresh(app, false, cx);
        }
        if self.other_apps {
            self.spawn_helper(cx);
        }
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        let th = rx.theme;
        let p = self.current()?;
        if let Some(island) = self.island(rx) {
            return Some(self.island_bar(rx, p, island));
        }
        let playing = p.state == State::Playing;
        let size = (th.pill_height - 8.0).max(12.0);
        let mut children = vec![self.cover(rx, p, size, 5.0).opacity(if playing { 1.0 } else { 0.6 }).key("cover")];
        if !playing {
            children.push(Node::symbol("pause.fill", 10.0, Weight::Bold, th.subtext).key("paused"));
        }
        // The room the bar has beside the pill goes to the title, then to the artist, as far as
        // they need it; a title still cut scrolls while it plays
        let font = th.font();
        let mut spare = rx.spare;
        let mut widen = |text: &str, max: f64| {
            let more = (crate::gfx::measure_text(text, &font).width.ceil() - max).clamp(0.0, spare.max(0.0));
            spare -= more;
            max + more
        };
        let max = widen(&p.title, if rx.compact { 110.0 } else { 220.0 });
        children.push(Node::text(&p.title, font.clone(), if playing { th.text } else { th.subtext }).max_width(max).marquee(playing).key("title"));
        if !rx.compact && !p.artist.is_empty() {
            let max = widen(&p.artist, 150.0);
            children.push(Node::text(&p.artist, font.clone(), th.subtext).max_width(max).key("artist"));
        }
        // The cover as far from the pill's edge as from its top and bottom
        let inset = ((th.pill_height - size) / 2.0).max(2.0);
        let pill = rx.pill(children).padding(0.0, th.pill_padding, 0.0, inset).click(rx.toggle_popup());
        // Under the mouse it grows (see `peek`): no highlight too, on it or on the grown one's top
        Some(if self.hover { pill.hover(crate::gfx::Rgba(0.0, 0.0, 0.0, 0.0)) } else { pill })
    }

    /// With `hover`, under the pill as it grows: where the track is, with the time gone and left,
    /// to seek if the app takes it (nothing for a stream), then previous, play/pause and next as
    /// the app takes them; while macOS sends the controls to another app, where to control it
    /// instead. The player is the bar's; the popup is a click away, on the pill's top.
    fn peek(&self, rx: &Rx) -> Option<Node> {
        if !self.hover {
            return None;
        }
        let p = self.current()?;
        // On the island, on the notch's black in the colors of the cover
        let island = self.island(rx);
        let theme = island.map(|_| self.notch_theme(rx.theme, p));
        let own = theme.as_ref().map(|theme| Rx { theme, ..*rx });
        let rx = own.as_ref().unwrap_or(rx);
        let th = rx.theme;
        let mut rows = Vec::new();
        // What the island had no room for: the title, if it's cut there, and the artist
        if let Some(i) = island {
            let (title_w, artist_w) = island_text(th, p, i.left);
            let cut = title_w < crate::gfx::measure_text(&p.title, &th.font()).width.ceil();
            if cut || (artist_w == 0.0 && !p.artist.is_empty()) {
                let max = ISLAND_WIDTH - 24.0;
                let mut lines = vec![Node::text(&p.title, th.font_with(Weight::Semibold, th.font_size), th.text).max_width(max).marquee(p.state == State::Playing).key("title")];
                if !p.artist.is_empty() {
                    lines.push(Node::text(&p.artist, th.small_font(), th.subtext).max_width(max).key("artist"));
                }
                rows.push(Node::column(lines).gap(1.0).padding(2.0, 0.0, 2.0, 0.0).key("track"));
            }
        }
        if p.duration > 0.0 {
            let pos = p.position_at(time::now());
            let mut slider = Node::new(Kind::Slider { value: pos / p.duration, color: th.primary, track: th.surface, font: None, labels: vec![] }).height(10.0).grow();
            if p.can_seek() {
                slider = slider.click(rx.action("seek"));
            }
            // As wide as the longest the times get, so that the bar doesn't move as they change
            let longest = if p.duration >= 3600.0 { "-0:00:00" } else { "-00:00" };
            let width = crate::gfx::measure_text(longest, &th.small_font()).width.ceil();
            let time = |text: String, justify: Justify| Node::row(vec![Node::text(text, th.small_font(), th.subtext).key("text")]).width(width).justify(justify);
            rows.push(
                Node::row(vec![
                    time(clock(pos), Justify::Start).key("gone"),
                    slider.key("slider"),
                    time(format!("-{}", clock(p.duration.floor() - pos.floor())), Justify::End).key("left"),
                ])
                .gap(8.0)
                .align(Align::Center)
                .key("seek"),
            );
        }
        match &p.controls_go_to {
            None => {
                let (previous, next) = p.can_skip();
                let playing = p.state == State::Playing;
                let play = Node::row(vec![Node::symbol(if playing { "pause.fill" } else { "play.fill" }, 15.0, Weight::Semibold, th.on_primary).key("symbol")])
                    .size(36.0, 36.0)
                    .justify(Justify::Center)
                    .radius(18.0)
                    .background(th.primary)
                    .hover(th.primary.mix(th.on_primary, 0.18))
                    .click(rx.action("playpause"))
                    .key("playpause");
                // Shuffle and repeat for Music and Spotify, at the ends
                let full = p.app != App::Other;
                let repeat = p.repeat.unwrap_or_default();
                let mut controls = Vec::new();
                if full {
                    controls.push(control(rx, "shuffle", 13.0, p.shuffle == Some(true), "shuffle"));
                }
                if previous {
                    controls.push(control(rx, "backward.fill", 16.0, false, "previous"));
                }
                controls.push(play);
                if next {
                    controls.push(control(rx, "forward.fill", 16.0, false, "next"));
                }
                if full {
                    controls.push(control(rx, if repeat == Repeat::One { "repeat.1" } else { "repeat" }, 13.0, repeat != Repeat::Off, "repeat"));
                }
                let row = Node::row(controls).align(Align::Center).key("controls");
                rows.push(if full { row.justify(Justify::Between) } else { row.gap(14.0).justify(Justify::Center) });
            }
            Some(_) => rows.push(
                Node::row(vec![
                    Node::symbol("arrow.up.forward.app.fill", 12.0, Weight::Semibold, th.primary).key("symbol"),
                    Node::text(format!("{} {}", t("Si comanda da", "Control it in"), p.name), th.small_font(), th.text).key("text"),
                ])
                .gap(6.0)
                .justify(Justify::Center)
                .height(30.0)
                .radius(th.row_radius)
                .click(rx.action("open"))
                .key("elsewhere"),
            ),
        }
        let peek = Node::column(rows).gap(6.0).padding(2.0, 12.0, 10.0, 12.0);
        Some(if island.is_some() { peek.padding(4.0, 14.0, 12.0, 14.0).min_width(ISLAND_WIDTH) } else { peek.min_width(240.0) })
    }

    /// On the island, for a moment when the track changes (see `watch_track`): its cover
    /// bigger, the title, the artist and the album.
    fn notice(&self, rx: &Rx) -> Option<Node> {
        self.island(rx)?;
        let p = self.current()?;
        let theme = self.notch_theme(rx.theme, p);
        let rx = &Rx { theme: &theme, ..*rx };
        let th = rx.theme;
        let max = ISLAND_WIDTH - 28.0 - 56.0 - 12.0;
        let mut lines = vec![Node::text(&p.title, th.font_with(Weight::Semibold, th.font_size + 1.0), th.text).max_width(max).marquee(p.state == State::Playing).key("title")];
        if !p.artist.is_empty() {
            lines.push(Node::text(&p.artist, th.font(), th.subtext).max_width(max).key("artist"));
        }
        if !p.album.is_empty() {
            lines.push(Node::text(&p.album, th.small_font(), th.subtext).max_width(max).key("album"));
        }
        Some(
            Node::row(vec![self.cover(rx, p, 56.0, 10.0).key("cover"), Node::column(lines).gap(2.0).grow().key("lines")])
                .gap(12.0)
                .align(Align::Center)
                .padding(6.0, 14.0, 14.0, 14.0)
                .min_width(ISLAND_WIDTH),
        )
    }

    fn popup(&self, rx: &Rx) -> Option<Node> {
        let Some(p) = self.shown() else { return Some(self.idle_popup(rx)) };
        // In the colors of the cover, once they're there; the hovers too, as the popup's own
        // highlight is the theme's. On the island, on the notch's black
        let island = self.island(rx);
        let tinted = match island {
            Some(_) => Some(self.notch_theme(rx.theme, p)),
            None => self.palette(p).map(|palette| palette.theme(rx.theme)),
        };
        let own = tinted.as_ref().map(|theme| Rx { theme, ..*rx });
        let rx = own.as_ref().unwrap_or(rx);
        let th = rx.theme;
        let now = time::now();
        let playing = p.state == State::Playing;
        let mut rows = Vec::new();

        // More than one app has something: which one the player is for
        let active: Vec<&Player> = self.players.iter().filter(|q| q.active()).collect();
        if active.len() > 1 {
            let segments = active
                .iter()
                .map(|q| {
                    let on = q.app == p.app;
                    let mut s = Node::row(vec![
                        Node::app_icon(&q.bundle, 14.0).key("icon"),
                        Node::text(&q.name, th.small_font(), if on { th.on_primary } else { th.text }).max_width(WIDTH / 3.0 - 34.0).key("name"),
                    ])
                    .gap(5.0)
                    .justify(Justify::Center)
                    .height(22.0)
                    .grow()
                    .radius(6.0)
                    .click(rx.action(&format!("source:{}", q.app.key())))
                    .hover(th.highlight)
                    .key(q.app.key());
                    if on {
                        s = s.hover(th.on_primary.faded(0.12));
                    }
                    s
                })
                .collect();
            rows.push(Node::row(segments).gap(2.0).padding(2.0, 2.0, 2.0, 2.0).radius(8.0).background(th.surface).selected(Some(p.app.key().to_string()), th.primary).key("sources"));
        }

        // The cover and what plays
        let text_w = WIDTH - 16.0 - COVER - 12.0 - 8.0 - if p.favorited.is_some() { 32.0 } else { 0.0 };
        let mut lines = vec![Node::text(&p.title, th.font_with(Weight::Semibold, th.font_size + 1.0), th.text).max_width(text_w).key("title")];
        if !p.artist.is_empty() {
            lines.push(Node::text(&p.artist, th.font(), th.subtext).max_width(text_w).key("artist"));
        }
        if !p.album.is_empty() {
            lines.push(Node::text(&p.album, th.small_font(), th.subtext).max_width(text_w).key("album"));
        }
        let mut header = vec![
            self.cover(rx, p, COVER, 10.0).click(rx.action("open")).key("cover"),
            Node::column(lines).gap(2.0).grow().key("lines"),
        ];
        if let Some(on) = p.favorited {
            let heart = Node::symbol(if on { "heart.fill" } else { "heart" }, 14.0, Weight::Semibold, if on { th.red } else { th.subtext }).key("symbol");
            header.push(Node::row(vec![heart]).size(28.0, 28.0).justify(Justify::Center).radius(14.0).click(rx.action("favorite")).hover(th.highlight).key("favorite"));
        }
        rows.push(Node::row(header).gap(12.0).align(Align::Center).padding(4.0, 4.0, 2.0, 4.0).key("header"));

        // Where it is, to seek if the app takes it; nothing for a stream
        if p.duration > 0.0 {
            let pos = p.position_at(now);
            let mut slider = Node::new(Kind::Slider { value: pos / p.duration, color: th.primary, track: th.surface, font: None, labels: vec![] }).height(12.0);
            if p.can_seek() {
                slider = slider.click(rx.action("seek"));
            }
            let slider = slider.key("slider");
            let times = Node::row(vec![
                Node::text(clock(pos), th.small_font(), th.subtext).key("gone"),
                Node::spacer(),
                Node::text(format!("-{}", clock(p.duration.floor() - pos.floor())), th.small_font(), th.subtext).key("left"),
            ])
            .key("times");
            rows.push(Node::column(vec![slider, times]).gap(4.0).padding(4.0, 4.0, 0.0, 4.0).key("seek"));
        }

        // The controls: shuffle and repeat for Music and Spotify, the rest as the app takes them;
        // none while macOS sends them to another app
        let repeat = p.repeat.unwrap_or_default();
        let play = Node::row(vec![Node::symbol(if playing { "pause.fill" } else { "play.fill" }, 20.0, Weight::Semibold, th.on_primary).key("symbol")])
            .size(48.0, 48.0)
            .justify(Justify::Center)
            .radius(24.0)
            .background(th.primary)
            .hover(th.primary.mix(th.on_primary, 0.18))
            .click(rx.action("playpause"))
            .key("playpause");
        let full = p.app != App::Other;
        let (previous, next) = p.can_skip();
        let mut controls = Vec::new();
        if full {
            controls.push(control(rx, "shuffle", 14.0, p.shuffle == Some(true), "shuffle"));
        }
        if previous {
            controls.push(control(rx, "backward.fill", 18.0, false, "previous"));
        }
        controls.push(play);
        if next {
            controls.push(control(rx, "forward.fill", 18.0, false, "next"));
        }
        if full {
            controls.push(control(rx, if repeat == Repeat::One { "repeat.1" } else { "repeat" }, 14.0, repeat != Repeat::Off, "repeat"));
        }
        if p.controls_go_to.is_none() {
            rows.push(
                Node::row(controls)
                    .justify(if full { Justify::Between } else { Justify::Center })
                    .gap(if full { 0.0 } else { 20.0 })
                    .align(Align::Center)
                    .padding(0.0, 8.0, 0.0, 8.0)
                    .key("controls"),
            );
        }

        // The app's own volume
        if let Some(v) = p.volume {
            rows.push(
                Node::row(vec![
                    Node::symbol("speaker.fill", 11.0, Weight::Semibold, th.subtext).key("low"),
                    rx.slider(v / 100.0, rx.action("volume")).height(22.0).grow().key("slider"),
                    Node::symbol("speaker.wave.3.fill", 11.0, Weight::Semibold, th.subtext).key("high"),
                ])
                .gap(8.0)
                .padding(0.0, 4.0, 0.0, 4.0)
                .key("volume"),
            );
        }

        // Without the controls, where they are; without the permission, what's missing and how to
        // give it
        let note = match (&p.controls_go_to, p.access) {
            (Some(to), _) => {
                let detail = if to.is_empty() { t("macOS non gli manda i comandi", "macOS doesn't send it the controls").to_string() } else { format!("{} {to}", t("macOS manda i comandi a", "macOS sends the controls to")) };
                Some(("arrow.up.forward.app.fill", th.primary, format!("{} {}", t("Si comanda da", "Control it in"), p.name), detail, "open"))
            }
            // Made-up players need no permission
            _ if self.mock => None,
            (None, Access::Ask | Access::Unknown) => {
                Some(("hand.raised.fill", th.primary, format!("{}{}", t("Consenti di comandare ", "Allow control of "), p.name), t("Posizione, copertina e comandi", "Position, cover and controls").to_string(), "allow"))
            }
            (None, Access::Denied) => Some((
                "exclamationmark.triangle.fill",
                th.yellow,
                format!("{}{}", t("Non è consentito comandare ", "Not allowed to control "), p.name),
                t("Privacy e sicurezza → Automazione", "Privacy & Security → Automation").to_string(),
                "allow",
            )),
            (None, Access::Granted) => None,
        };
        if let Some((symbol, color, text, detail, action)) = note {
            let lines = Node::column(vec![
                Node::text(text, th.font(), th.text).key("text"),
                Node::text(detail, th.small_font(), th.subtext).key("detail"),
            ])
            .gap(1.0)
            .key("lines");
            rows.push(
                Node::row(vec![Node::symbol(symbol, 13.0, Weight::Semibold, color).key("symbol"), lines])
                    .gap(10.0)
                    .padding(8.0, 10.0, 8.0, 10.0)
                    .radius(10.0)
                    .background(th.tile(color, true))
                    .click(rx.action(action))
                    .hover(th.highlight)
                    .key("access"),
            );
        }

        rows.push(rx.divider().key("divider"));
        rows.push(open_row(rx, p));
        let popup = rx.popup(WIDTH, rows).gap(10.0);
        Some(match &tinted {
            // The island's black is its own
            _ if island.is_some() => popup.padding(4.0, 14.0, 14.0, 14.0),
            // Over the theme's background, or its glass, which shows a little
            Some(t) => {
                let alpha = if t.glass { 0.85 } else if t.popup_blur { t.popup_opacity } else { 1.0 };
                popup.background(t.base.faded(alpha)).radius(t.popup_radius)
            }
            None => popup,
        })
    }

    fn action(&mut self, name: &str, value: Option<f64>, cx: &mut Cx) {
        if let Some(key) = name.strip_prefix("source:") {
            self.pick = ALL.into_iter().find(|a| a.key() == key);
            cx.redraw();
            return;
        }
        if let Some(key) = name.strip_prefix("open:") {
            if let Some(p) = self.players.iter().find(|p| p.app.key() == key && !p.bundle.is_empty())
                && !self.mock
            {
                crate::runtime::open_app(&p.bundle);
            }
            cx.close_popup();
            return;
        }
        let Some(app) = self.shown().map(|p| p.app) else { return };
        if app == App::Other {
            self.other_action(name, value, cx);
            return;
        }
        let now = time::now();
        let suite = app.suite();
        let p = &mut self.players[app as usize];
        // The change shows at once; the app's reply corrects it
        match name {
            "playpause" => {
                p.position = p.position_at(now);
                p.at = now;
                p.state = if p.state == State::Playing { State::Paused } else { State::Playing };
                p.changed = now;
                self.send(app, move |pid| ae::command(pid, suite, ae::fc(b"PlPs")), cx);
            }
            "next" | "previous" => {
                if self.mock {
                    p.position = 0.0;
                    p.at = now;
                    // The next of the made-up tracks, or the one before: the one playing goes to
                    // the other end of the queue
                    let other = match name {
                        "next" if !self.queue.is_empty() => Some(self.queue.remove(0)),
                        _ => self.queue.pop(),
                    };
                    if let Some(mut track) = other {
                        std::mem::swap(&mut p.title, &mut track.title);
                        std::mem::swap(&mut p.artist, &mut track.artist);
                        std::mem::swap(&mut p.album, &mut track.album);
                        std::mem::swap(&mut p.duration, &mut track.duration);
                        std::mem::swap(&mut p.artwork, &mut track.artwork);
                        p.changed = now;
                        if name == "next" {
                            self.queue.push(track);
                        } else {
                            self.queue.insert(0, track);
                        }
                        self.colorize(cx);
                        self.watch_track(cx);
                    }
                    cx.redraw();
                    return;
                }
                let id = match (name, app) {
                    ("next", _) => b"Next",
                    // Music's ⏮: back to the start of the track, or to the one before near it
                    (_, App::Music) => b"Back",
                    _ => b"Prev",
                };
                self.send(app, move |pid| ae::command(pid, suite, ae::fc(id)), cx);
            }
            "shuffle" => {
                let on = !p.shuffle.unwrap_or(false);
                p.shuffle = Some(on);
                let code = if app == App::Music { b"pShE" } else { b"pShu" };
                self.send(app, move |pid| ae::set(pid, &ae::property(ae::fc(code), None), &ae::boolean(on)), cx);
            }
            "repeat" => {
                // Spotify repeats the context only
                let next = match (p.repeat.unwrap_or_default(), app) {
                    (Repeat::Off, _) => Repeat::All,
                    (Repeat::All, App::Music) => Repeat::One,
                    _ => Repeat::Off,
                };
                p.repeat = Some(next);
                self.send(
                    app,
                    move |pid| match app {
                        App::Music => {
                            let code = match next {
                                Repeat::Off => b"kRpO",
                                Repeat::All => b"kAll",
                                Repeat::One => b"kRp1",
                            };
                            ae::set(pid, &ae::property(ae::fc(b"pRpt"), None), &ae::enumerated(ae::fc(code)))
                        }
                        _ => ae::set(pid, &ae::property(ae::fc(b"pRep"), None), &ae::boolean(next != Repeat::Off)),
                    },
                    cx,
                );
            }
            "favorite" => {
                let Some(on) = p.favorited.map(|f| !f) else { return };
                p.favorited = Some(on);
                self.send(
                    app,
                    move |pid| {
                        let track = ae::property(ae::fc(b"pTrk"), None);
                        ae::set(pid, &ae::property(ae::fc(b"pLov"), Some(&track)), &ae::boolean(on))
                    },
                    cx,
                );
            }
            "seek" => {
                let Some(v) = value.filter(|_| p.duration > 0.0) else { return };
                let to = v.clamp(0.0, 1.0) * p.duration;
                p.position = to;
                p.at = now;
                self.send(app, move |pid| ae::set(pid, &ae::property(ae::fc(b"pPos"), None), &ae::real(to)), cx);
            }
            "volume" => {
                let Some(v) = value else { return };
                let level = (v.clamp(0.0, 1.0) * 100.0).round();
                p.volume = Some(level);
                self.send(app, move |pid| ae::set(pid, &ae::property(ae::fc(b"pVol"), None), &ae::integer(level as i32)), cx);
            }
            "open" => {
                if !self.mock {
                    crate::runtime::open_app(app.bundle());
                }
                cx.close_popup();
            }
            "allow" => match p.access {
                Access::Denied => crate::runtime::open("x-apple.systempreferences:com.apple.preference.security?Privacy_Automation"),
                _ => self.refresh(app, true, cx),
            },
            _ => {}
        }
        cx.redraw();
    }

    fn event(&mut self, event: &Event, cx: &mut Cx) {
        match event {
            Event::Wake => {
                for app in APPS {
                    if self.players[app as usize].access == Access::Granted {
                        self.refresh(app, false, cx);
                    }
                }
            }
            Event::PopupOpened => {
                self.ticking += 1;
                self.tick(self.ticking, cx);
                // The popup is a click: macOS may ask for the permission now
                if let Some(app) = self.shown().map(|p| p.app) {
                    self.refresh(app, true, cx);
                }
            }
            Event::PopupClosed => {
                self.ticking += 1;
                self.pick = None;
            }
            // Not a click: the permission isn't asked for, only what's allowed is read again.
            // Its closing stops the timer by itself, which the popup opening from it needs
            Event::PeekOpened => {
                self.ticking += 1;
                self.tick(self.ticking, cx);
                if let Some(app) = self.current().map(|p| p.app)
                    && self.players[app as usize].access == Access::Granted
                {
                    self.refresh(app, false, cx);
                }
            }
            _ => {}
        }
    }

    crate::any_widget!();
}

impl NowPlaying {
    /// Every second while the popup is open, or the pill grown, and it plays: the time moves on.
    fn tick(&mut self, ticking: u64, cx: &mut Cx) {
        if ticking != self.ticking || !(cx.popup_open || cx.peek_open) {
            return;
        }
        if self.shown().is_some_and(|p| p.state == State::Playing) {
            cx.redraw();
        }
        cx.after::<NowPlaying>(1.0, move |w, cx| w.tick(ticking, cx));
    }

    /// Starts the helper of `other_apps` (see the top): perl with `now_playing.m` loaded. Its
    /// lines come to `helped` from a thread, and its end to `helper_ended`.
    fn spawn_helper(&mut self, cx: &mut Cx) {
        let child = helper_file().and_then(|path| {
            Command::new("/usr/bin/perl").arg("-e").arg(PERL).arg(path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn()
        });
        let mut child = match child {
            Ok(child) => child,
            Err(e) => return crate::log(&format!("now_playing: the helper for other apps didn't start: {e}")),
        };
        self.helper = child.stdin.take();
        let Some(stdout) = child.stdout.take() else { return };
        let handle = cx.handle::<NowPlaying>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match serde_json::from_str::<Helped>(&line) {
                    Ok(m) => handle.send(move |w, cx| w.helped(m, cx)),
                    Err(e) => crate::log(&format!("now_playing: the helper said something unknown: {e}")),
                }
            }
            let _ = child.wait();
            handle.send(|w, cx| w.helper_ended(cx));
        });
    }

    /// A line of the helper: the players of all the apps, or the cover of one's artwork.
    fn helped(&mut self, m: Helped, cx: &mut Cx) {
        if let Some(e) = m.error {
            crate::log(&format!("now_playing: other apps: {e}"));
            self.helper_broken = true;
            return;
        }
        self.helper_starts = 0;
        let now = time::now();
        let p = &mut self.players[App::Other as usize];
        if let Some(cover) = m.cover {
            if p.bundle == m.bundle && p.cover_key == m.artwork && !cover.is_empty() {
                let key = m.artwork;
                let wanted = key.clone();
                cx.background::<NowPlaying, _>(
                    move || decode_cover(&cover, &key),
                    move |w, path, cx| {
                        let p = &mut w.players[App::Other as usize];
                        if p.cover_key == wanted {
                            p.artwork = path;
                            w.colorize(cx);
                            cx.redraw();
                        }
                    },
                );
            }
            return;
        }
        // The apps with a track, but Music and Spotify, whose players say more. Of them, the one
        // playing (the one shown already, if it is), else the one shown, paused, else the first:
        // a video paused while Spotify plays stays
        let players = m.players.unwrap_or_default();
        let active = players.iter().find(|q| q.active).map(|q| if q.name.is_empty() { q.bundle.clone() } else { q.name.clone() });
        let mut others: Vec<Played> = players
            .into_iter()
            .filter(|q| !q.title.is_empty() && q.state != State::Stopped && !APPS.iter().any(|a| a.bundle() == q.bundle))
            .collect();
        let shown = others.iter().position(|q| p.active() && q.bundle == p.bundle);
        let playing = others.iter().position(|q| q.state == State::Playing);
        let pick = shown.filter(|&i| others[i].state == State::Playing).or(playing).or(shown).or((!others.is_empty()).then_some(0));
        let Some(m) = pick.map(|i| others.swap_remove(i)) else {
            if p.active() {
                p.reset();
                cx.redraw();
            }
            return;
        };
        if p.bundle != m.bundle || p.state != m.state || track_key(&m.title, &m.artist, &m.album) != track_key(&p.title, &p.artist, &p.album) {
            p.changed = now;
        }
        p.name = if m.name.is_empty() { m.bundle.clone() } else { m.name };
        p.bundle = m.bundle;
        p.pid = Some(m.pid);
        p.access = Access::Granted;
        p.controls = Some(Controls { next: m.next, previous: m.previous, seek: m.seek });
        p.controls_go_to = if m.active { None } else { Some(active.unwrap_or_default()) };
        p.state = m.state;
        p.title = m.title;
        p.artist = m.artist;
        p.album = m.album;
        p.duration = m.duration;
        p.position = m.position;
        p.at = now;
        // The cover comes in the next line; the app's icon till then, or without one
        if p.cover_key != m.artwork {
            p.cover_key = m.artwork;
            p.artwork = None;
        }
        cx.redraw();
        self.watch_track(cx);
    }

    /// The helper ended: a crash, or perl gone. It starts again, later at each try.
    fn helper_ended(&mut self, cx: &mut Cx) {
        self.helper = None;
        let p = &mut self.players[App::Other as usize];
        if p.active() {
            p.reset();
            cx.redraw();
        }
        if self.helper_broken {
            return;
        }
        let delay = (5.0 * 2f64.powi(self.helper_starts.min(6) as i32)).min(300.0);
        self.helper_starts += 1;
        crate::log(&format!("now_playing: the helper for other apps ended, again in {delay} s"));
        cx.after::<NowPlaying>(delay, |w, cx| w.spawn_helper(cx));
    }

    /// A command to the helper, which sends it to the app it names.
    fn tell(&mut self, line: &str) {
        if let Some(stdin) = &mut self.helper
            && writeln!(stdin, "{line}").is_err()
        {
            self.helper = None;
        }
    }

    /// The controls of the other app's player: through the helper, as the app takes them.
    fn other_action(&mut self, name: &str, value: Option<f64>, cx: &mut Cx) {
        let now = time::now();
        let p = &mut self.players[App::Other as usize];
        let (previous, next) = p.can_skip();
        if p.controls_go_to.is_some() && name != "open" {
            return;
        }
        let line = match name {
            "playpause" => {
                p.position = p.position_at(now);
                p.at = now;
                p.state = if p.state == State::Playing { State::Paused } else { State::Playing };
                p.changed = now;
                format!("toggle {}", p.bundle)
            }
            "previous" if previous => format!("previous {}", p.bundle),
            "next" if next => format!("next {}", p.bundle),
            "seek" if p.can_seek() => {
                let Some(v) = value else { return };
                let to = v.clamp(0.0, 1.0) * p.duration;
                p.position = to;
                p.at = now;
                format!("seek {} {to:.2}", p.bundle)
            }
            "open" => {
                if !self.mock {
                    crate::runtime::open_app(&p.bundle);
                }
                cx.close_popup();
                return;
            }
            _ => return,
        };
        if !self.mock {
            self.tell(&line);
        }
        cx.redraw();
    }
}

/// The helper (see `now_playing.m`), compiled by build.rs.
const HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/now_playing.dylib"));

/// What perl runs: it loads the helper and calls it as one of its own functions (an XSUB). Named
/// in `ps` after RustyBar.
const PERL: &str = r#"$0 = "rustybar now_playing helper"; require DynaLoader;
my $lib = DynaLoader::dl_load_file($ARGV[0], 0) or die DynaLoader::dl_error();
my $run = DynaLoader::dl_find_symbol($lib, "rustybar_now_playing") or die DynaLoader::dl_error();
DynaLoader::dl_install_xsub("main::run", $run); run();"#;

/// The helper on disk, for perl: in the cache folder, named after its bytes, so a new build has a
/// new file and the old one goes.
fn helper_file() -> std::io::Result<PathBuf> {
    let dir = crate::config::home().join("Library/Caches/rustybar");
    std::fs::create_dir_all(&dir)?;
    let hash = HELPER.iter().fold(0x811c_9dc5u32, |h, b| (h ^ *b as u32).wrapping_mul(0x0100_0193));
    let name = format!("now_playing-{hash:08x}.dylib");
    let path = dir.join(&name);
    if !path.exists() {
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let other = entry.file_name().to_string_lossy().into_owned();
            if other.starts_with("now_playing-") && other.ends_with(".dylib") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        // Whole or not at all: another RustyBar (a test bar) may be writing it too
        let part = dir.join(format!("{name}.{}", std::process::id()));
        std::fs::write(&part, HELPER)?;
        std::fs::rename(&part, &path)?;
    }
    Ok(path)
}

/// A line of the helper (see `now_playing.m`): an error, the players, or a cover.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Helped {
    /// It can't work: MediaRemote changed.
    error: Option<String>,
    /// Every app with a player.
    players: Option<Vec<Played>>,
    /// The cover of the artwork `artwork` of the app `bundle`, base64.
    cover: Option<String>,
    bundle: String,
    artwork: String,
}

/// An app with a player, as the helper says.
#[derive(Default, Deserialize)]
#[serde(default)]
struct Played {
    bundle: String,
    name: String,
    pid: i32,
    state: State,
    title: String,
    artist: String,
    album: String,
    duration: f64,
    position: f64,
    next: bool,
    previous: bool,
    seek: bool,
    /// The artwork's id, empty without one.
    artwork: String,
    /// It's the app MediaRemote calls playing: the only one its commands reach.
    active: bool,
}

/// The helper's cover, base64, as a small PNG in the cache folder.
fn decode_cover(base64: &str, key: &str) -> Option<String> {
    use objc2::AnyThread;
    use objc2_foundation::{NSData, NSDataBase64DecodingOptions};
    let data = NSData::initWithBase64EncodedString_options(NSData::alloc(), &NSString::from_str(base64), NSDataBase64DecodingOptions::empty())?;
    save_cover(App::Other, key, &data.to_vec())
}

impl Drop for NowPlaying {
    fn drop(&mut self) {
        let center = NSDistributedNotificationCenter::defaultCenter();
        for o in self.observers.drain(..) {
            let o: &NSObject = &o;
            unsafe { center.removeObserver(o) };
        }
        let ws = NSWorkspace::sharedWorkspace().notificationCenter();
        for o in self.workspace.drain(..) {
            unsafe { ws.removeObserver(o.as_ref()) };
        }
    }
}

/// What a player's notification says.
struct Info {
    state: State,
    title: String,
    artist: String,
    album: String,
    /// Seconds, 0 if not said.
    duration: f64,
    /// Spotify's only.
    position: Option<f64>,
}

/// What the notification of `app` (Music or Spotify) says.
fn info(app: App, n: &NSNotification) -> Info {
    let user = n.userInfo();
    let get = |key: &str| user.as_ref().and_then(|u| u.objectForKey(&NSString::from_str(key)));
    let string = |key: &str| get(key).and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default();
    let number = |key: &str| get(key).and_then(|o| o.downcast::<NSNumber>().ok()).map(|n| n.doubleValue());
    let state = match string("Player State").as_str() {
        "Playing" => State::Playing,
        "Paused" => State::Paused,
        _ => State::Stopped,
    };
    let (duration, position) = match app {
        App::Music => (number("Total Time").unwrap_or(0.0) / 1000.0, None),
        _ => (number("Duration").unwrap_or(0.0) / 1000.0, number("Playback Position")),
    };
    Info { state, title: string("Name"), artist: string("Artist"), album: string("Album"), duration, position }
}

/// A cover from the app: a file already, a URL to download, or none for this track.
enum Cover {
    File(String),
    Url(String),
    Missing,
}

/// All of a player, as its app says.
#[derive(Default)]
struct Read {
    state: State,
    position: f64,
    title: String,
    artist: String,
    album: String,
    duration: f64,
    shuffle: Option<bool>,
    repeat: Option<Repeat>,
    volume: Option<f64>,
    favorited: Option<bool>,
    key: String,
    /// None when the track is the one of `have`: its cover is there.
    cover: Option<Cover>,
}

/// Reads the player of `app` (Music or Spotify) with Apple events: the state first, whose error (no permission, not
/// running) is the read's; then the rest, each on its own, as not every one is always there.
fn read(app: App, pid: i32, ask: bool, have: &str) -> Result<Read, i32> {
    use ae::{fc, property};
    let get = |spec: &ae::Desc| ae::get(pid, spec, ask);
    let of_app = |code: &[u8; 4]| get(&property(fc(code), None));
    let state = of_app(b"pPlS")?.enumCodeValue();
    let mut r = Read {
        state: match &state.to_be_bytes() {
            b"kPSP" | b"kPSF" | b"kPSR" => State::Playing,
            b"kPSp" => State::Paused,
            _ => State::Stopped,
        },
        ..Default::default()
    };
    r.position = of_app(b"pPos").map(|d| d.doubleValue()).unwrap_or(0.0);
    r.volume = of_app(b"pVol").ok().map(|d| d.int32Value() as f64);
    match app {
        App::Music => {
            r.shuffle = of_app(b"pShE").ok().map(|d| ae::bool_value(&d));
            r.repeat = of_app(b"pRpt").ok().map(|d| match &d.enumCodeValue().to_be_bytes() {
                b"kAll" => Repeat::All,
                b"kRp1" => Repeat::One,
                _ => Repeat::Off,
            });
        }
        _ => {
            r.shuffle = of_app(b"pShu").ok().map(|d| ae::bool_value(&d));
            r.repeat = of_app(b"pRep").ok().map(|d| if ae::bool_value(&d) { Repeat::All } else { Repeat::Off });
        }
    }
    let track = property(fc(b"pTrk"), None);
    let of_track = |code: &[u8; 4]| get(&property(fc(code), Some(&track)));
    // No current track: stopped
    let Ok(title) = of_track(b"pnam") else { return Ok(Read { state: State::Stopped, ..r }) };
    r.title = ae::string(&title);
    r.artist = of_track(b"pArt").map(|d| ae::string(&d)).unwrap_or_default();
    r.album = of_track(b"pAlb").map(|d| ae::string(&d)).unwrap_or_default();
    r.duration = match app {
        App::Music => of_track(b"pDur").map(|d| d.doubleValue()).unwrap_or(0.0),
        _ => of_track(b"pDur").map(|d| d.doubleValue() / 1000.0).unwrap_or(0.0),
    };
    if app == App::Music {
        r.favorited = of_track(b"pLov").ok().map(|d| ae::bool_value(&d));
    }
    r.key = track_key(&r.title, &r.artist, &r.album);
    if r.key != have {
        r.cover = Some(match app {
            App::Music => {
                let artwork = ae::item(fc(b"cArt"), 1, Some(&track));
                let data = get(&property(fc(b"pRaw"), Some(&artwork))).map(|d| d.data().to_vec()).unwrap_or_default();
                save_cover(app, &r.key, &data).map(Cover::File).unwrap_or(Cover::Missing)
            }
            _ => match of_track(b"aUrl").map(|d| ae::string(&d)) {
                Ok(url) if url.starts_with("https://") => Cover::Url(url),
                _ => Cover::Missing,
            },
        });
    }
    Ok(r)
}

/// Downloads Spotify's cover, for the track of `key` if it's still the one playing then.
fn download(app: App, key: String, url: String, handle: super::Handle<NowPlaying>) {
    crate::net::request("GET", &url, &[], None, 15.0, move |status, body| {
        let path = if status == 200 { save_cover(app, &key, &body) } else { None };
        let key = key.clone();
        handle.send(move |w, cx| {
            let p = &mut w.players[app as usize];
            if p.cover_key == key {
                p.artwork = path;
                w.colorize(cx);
                cx.redraw();
            }
        });
    });
}

/// Scales `bytes` (a JPEG, a PNG…) down to a PNG of `COVER_PIXELS` and writes it to the cache
/// folder, in place of the app's last cover: a file of its own for each track, as the images are
/// cached by path.
fn save_cover(app: App, key: &str, bytes: &[u8]) -> Option<String> {
    let png = thumbnail(bytes, COVER_PIXELS)?;
    let dir = crate::config::home().join(format!("Library/Caches/rustybar/covers{}", crate::config::instance()));
    std::fs::create_dir_all(&dir).ok()?;
    let hash = key.bytes().fold(0x811c_9dc5u32, |h, b| (h ^ b as u32).wrapping_mul(0x0100_0193));
    let name = format!("{}-{hash:08x}.png", app.key());
    let prefix = format!("{}-", app.key());
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let other = entry.file_name().to_string_lossy().into_owned();
        if other.starts_with(&prefix) && other != name {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    let path = dir.join(name);
    std::fs::write(&path, png).ok()?;
    Some(path.to_string_lossy().into_owned())
}

#[link(name = "ImageIO", kind = "framework")]
unsafe extern "C" {
    fn CGImageSourceCreateWithData(data: *const c_void, options: *const c_void) -> *mut c_void;
    fn CGImageSourceCreateThumbnailAtIndex(source: *mut c_void, index: usize, options: *const c_void) -> *mut c_void;
    fn CGImageDestinationCreateWithData(data: *const c_void, kind: *const c_void, count: usize, options: *const c_void) -> *mut c_void;
    fn CGImageDestinationAddImage(destination: *mut c_void, image: *mut c_void, properties: *const c_void);
    fn CGImageDestinationFinalize(destination: *mut c_void) -> bool;
    static kCGImageSourceCreateThumbnailFromImageAlways: *const c_void;
    static kCGImageSourceCreateThumbnailWithTransform: *const c_void;
    static kCGImageSourceThumbnailMaxPixelSize: *const c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// `bytes` (a JPEG, a PNG…) decoded at most `pixels` on its longer side. ImageIO decodes a JPEG
/// straight at the smaller size, so a big cover never takes its full size in memory.
fn decode(bytes: &[u8], pixels: i64) -> Option<CFRetained<CGImage>> {
    use objc2_foundation::{NSData, NSDictionary};
    if bytes.is_empty() {
        return None;
    }
    unsafe {
        let data = NSData::with_bytes(bytes);
        let source = CGImageSourceCreateWithData(Retained::as_ptr(&data).cast(), std::ptr::null());
        if source.is_null() {
            return None;
        }
        let key = |k: *const c_void| &*(k as *const NSString);
        let keys = [key(kCGImageSourceCreateThumbnailFromImageAlways), key(kCGImageSourceCreateThumbnailWithTransform), key(kCGImageSourceThumbnailMaxPixelSize)];
        let (yes, size) = (NSNumber::new_bool(true), NSNumber::new_i64(pixels));
        let options = NSDictionary::from_slices(&keys, &[&*yes, &*yes, &*size]);
        let image = CGImageSourceCreateThumbnailAtIndex(source, 0, Retained::as_ptr(&options).cast());
        CFRelease(source);
        NonNull::new(image.cast::<CGImage>()).map(|image| CFRetained::from_raw(image))
    }
}

/// An image at most `pixels` on its longer side, as a PNG.
fn thumbnail(bytes: &[u8], pixels: i64) -> Option<Vec<u8>> {
    use objc2_foundation::NSMutableData;
    let image = decode(bytes, pixels)?;
    unsafe {
        let out = NSMutableData::new();
        let kind = NSString::from_str("public.png");
        let destination = CGImageDestinationCreateWithData(Retained::as_ptr(&out).cast(), Retained::as_ptr(&kind).cast(), 1, std::ptr::null());
        if destination.is_null() {
            return None;
        }
        CGImageDestinationAddImage(destination, CFRetained::as_ptr(&image).as_ptr().cast(), std::ptr::null());
        let ok = CGImageDestinationFinalize(destination);
        CFRelease(destination);
        ok.then(|| out.to_vec())
    }
}

/// The colors of the cover at `path` (see `palette`), from a copy at most 64 pixels wide: a few
/// thousand pixels, enough for its colors and quick to group.
fn colors(path: &str) -> Option<Palette> {
    let image = decode(&std::fs::read(path).ok()?, 64)?;
    let (w, h) = (CGImage::width(Some(&image)), CGImage::height(Some(&image)));
    let mut pixels = vec![0u8; w * h * 4];
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))?;
    // RGBA in this order, in sRGB, whatever the picture's own space
    let info = CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0;
    let ctx = unsafe { CGBitmapContextCreate(pixels.as_mut_ptr().cast(), w, h, 8, w * 4, Some(&space), info)? };
    CGContext::draw_image(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w as f64, h as f64)), Some(&image));
    drop(ctx);
    Palette::of_pixels(&pixels)
}

/// Apple events, as AppleScript sends them, built by hand: an object specifier for each property,
/// `get` and `set`, and the playback commands.
mod ae {
    use objc2::rc::Retained;
    use objc2::{ClassType, msg_send};
    use objc2_foundation::{NSAppleEventDescriptor, NSAppleEventSendOptions};

    pub type Desc = NSAppleEventDescriptor;

    pub const fn fc(s: &[u8; 4]) -> u32 {
        u32::from_be_bytes(*s)
    }

    /// errAENoSuchObject: no current track, or no cover.
    pub const NO_OBJECT: i32 = -1728;
    /// errAEEventNotPermitted: Automation denied.
    pub const DENIED: i32 = -1743;
    /// errAEEventWouldRequireUserConsent: macOS would ask.
    pub const WOULD_ASK: i32 = -1744;
    /// procNotFound.
    pub const NOT_RUNNING: i32 = -600;
    /// connectionInvalid: the app quit.
    pub const NO_CONNECTION: i32 = -609;

    /// kAEDoNotPromptForUserConsent: fail with `WOULD_ASK` rather than ask.
    const NO_PROMPT: usize = 0x0002_0000;

    fn new_event(pid: i32, class: u32, id: u32) -> Retained<Desc> {
        let target = Desc::descriptorWithProcessIdentifier(pid);
        unsafe {
            msg_send![
                Desc::class(),
                appleEventWithEventClass: class,
                eventID: id,
                targetDescriptor: &*target,
                returnID: -1i16, // kAutoGenerateReturnID
                transactionID: 0i32 // kAnyTransactionID
            ]
        }
    }

    fn set_param(event: &Desc, key: u32, value: &Desc) {
        unsafe {
            let _: () = msg_send![event, setParamDescriptor: value, forKeyword: key];
        }
    }

    fn param(d: &Desc, key: u32) -> Option<Retained<Desc>> {
        unsafe { msg_send![d, paramDescriptorForKeyword: key] }
    }

    fn type_code(code: u32) -> Retained<Desc> {
        unsafe { msg_send![Desc::class(), descriptorWithTypeCode: code] }
    }

    pub fn enumerated(code: u32) -> Retained<Desc> {
        unsafe { msg_send![Desc::class(), descriptorWithEnumCode: code] }
    }

    pub fn boolean(on: bool) -> Retained<Desc> {
        unsafe { msg_send![Desc::class(), descriptorWithBoolean: on as u8] }
    }

    pub fn real(v: f64) -> Retained<Desc> {
        Desc::descriptorWithDouble(v)
    }

    pub fn integer(v: i32) -> Retained<Desc> {
        Desc::descriptorWithInt32(v)
    }

    pub fn bool_value(d: &Desc) -> bool {
        let b: u8 = unsafe { msg_send![d, booleanValue] };
        b != 0
    }

    pub fn string(d: &Desc) -> String {
        d.stringValue().map(|s| s.to_string()).unwrap_or_default()
    }

    /// An object specifier: the `want` of `from` (the app without one), picked by `form`.
    fn specifier(want: u32, form: u32, seld: &Desc, from: Option<&Desc>) -> Retained<Desc> {
        let record = Desc::recordDescriptor();
        let null = Desc::nullDescriptor();
        for (key, value) in [(fc(b"want"), &*type_code(want)), (fc(b"form"), &*enumerated(form)), (fc(b"seld"), seld), (fc(b"from"), from.unwrap_or(&null))] {
            unsafe {
                let _: () = msg_send![&*record, setDescriptor: value, forKeyword: key];
            }
        }
        let spec: Option<Retained<Desc>> = unsafe { msg_send![&*record, coerceToDescriptorType: fc(b"obj ")] };
        spec.unwrap_or(record)
    }

    /// A property of `from`, or of the app.
    pub fn property(code: u32, from: Option<&Desc>) -> Retained<Desc> {
        specifier(fc(b"prop"), fc(b"prop"), &type_code(code), from)
    }

    /// The element of class `class` at `index` (from 1) of `from`.
    pub fn item(class: u32, index: i32, from: Option<&Desc>) -> Retained<Desc> {
        specifier(class, fc(b"indx"), &integer(index), from)
    }

    /// Sends `event` and waits for the reply: its result, or the error number. `ask` lets macOS
    /// ask for the Automation permission, and waits for the answer.
    fn send(event: &Desc, ask: bool) -> Result<Option<Retained<Desc>>, i32> {
        let mut options = NSAppleEventSendOptions::WaitForReply | NSAppleEventSendOptions::NeverInteract;
        if !ask {
            options |= NSAppleEventSendOptions::from_bits_retain(NO_PROMPT);
        }
        match event.sendEventWithOptions_timeout_error(options, if ask { 120.0 } else { 3.0 }) {
            Ok(reply) => match param(&reply, fc(b"errn")).map(|e| e.int32Value()) {
                Some(e) if e != 0 => Err(e),
                _ => Ok(param(&reply, fc(b"----"))),
            },
            Err(e) => Err(e.code() as i32),
        }
    }

    pub fn get(pid: i32, spec: &Desc, ask: bool) -> Result<Retained<Desc>, i32> {
        let event = new_event(pid, fc(b"core"), fc(b"getd"));
        set_param(&event, fc(b"----"), spec);
        send(&event, ask)?.ok_or(NO_OBJECT)
    }

    /// Sets the property of `spec`: from a click, so macOS may ask.
    pub fn set(pid: i32, spec: &Desc, value: &Desc) -> Result<(), i32> {
        let event = new_event(pid, fc(b"core"), fc(b"setd"));
        set_param(&event, fc(b"----"), spec);
        set_param(&event, fc(b"data"), value);
        send(&event, true).map(|_| ())
    }

    /// A command without parameters (play/pause, next…): from a click, so macOS may ask.
    pub fn command(pid: i32, class: u32, id: u32) -> Result<(), i32> {
        send(&new_event(pid, class, id), true).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(clock(0.0), "0:00");
        assert_eq!(clock(187.9), "3:07");
        assert_eq!(clock(3723.0), "1:02:03");
    }

    #[test]
    fn the_position_goes_on_while_playing() {
        let mut p = Player { state: State::Playing, title: "A".into(), duration: 100.0, position: 10.0, at: 50.0, pid: Some(1), ..Default::default() };
        assert_eq!(p.position_at(55.0), 15.0);
        assert_eq!(p.position_at(500.0), 100.0);
        p.state = State::Paused;
        assert_eq!(p.position_at(55.0), 10.0);
    }

    #[test]
    fn the_bar_shows_the_one_playing_then_the_latest() {
        let mut w = NowPlaying::new(&Options::default());
        let player = |app, state, changed| Player { app, state, title: "A".into(), pid: Some(1), changed, ..Default::default() };
        w.players = [player(App::Music, State::Paused, 2.0), player(App::Spotify, State::Playing, 1.0), Player::new(App::Other)];
        assert_eq!(w.current().map(|p| p.app), Some(App::Spotify));
        w.players[1].state = State::Paused;
        assert_eq!(w.current().map(|p| p.app), Some(App::Music));
        w.paused = false;
        assert!(w.current().is_none());
        // The popup still shows it, paused
        assert_eq!(w.shown().map(|p| p.app), Some(App::Music));
        w.pick = Some(App::Spotify);
        assert_eq!(w.shown().map(|p| p.app), Some(App::Spotify));
    }

    #[test]
    fn a_long_title_takes_the_room_beside_it_and_scrolls_while_playing() {
        let theme = crate::theme::Theme::from_config(&crate::config::ThemeConfig::default());
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1512.0, 982.0));
        let display = crate::display::DisplayInfo { id: 1, name: "Test".into(), frame, scale: 2.0, notch: None, notch_height: 0.0, large: false };
        let title = "A made-up title much longer than the pill has room for, on and on and on";
        let mut w = NowPlaying::new(&Options::default());
        w.players = [
            Player { app: App::Music, state: State::Playing, title: title.into(), pid: Some(1), ..Default::default() },
            Player::new(App::Spotify),
            Player::new(App::Other),
        ];
        let shown = |w: &NowPlaying, spare: f64| {
            let mut rx = Rx::new("now_playing", &theme, &display, crate::fit::Form::Compact, false);
            rx.spare = spare;
            let pill = w.bar(&rx).expect("a pill");
            let n = pill.children.iter().find(|c| c.key.as_deref() == Some("title")).expect("a title").clone();
            (n.style.max_width.expect("a max width"), n.style.marquee)
        };
        assert_eq!(shown(&w, 0.0), (110.0, true));
        assert_eq!(shown(&w, 50.0), (160.0, true));
        // Never wider than the title itself
        let whole = crate::gfx::measure_text(title, &theme.font()).width.ceil();
        assert_eq!(shown(&w, 2000.0).0, whole);
        // Paused, it stays still
        w.players[0].state = State::Paused;
        assert_eq!(shown(&w, 0.0), (110.0, false));
    }

    #[test]
    fn the_peek_has_the_controls_the_app_takes() {
        let theme = crate::theme::Theme::from_config(&crate::config::ThemeConfig::default());
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1512.0, 982.0));
        let display = crate::display::DisplayInfo { id: 1, name: "Test".into(), frame, scale: 2.0, notch: None, notch_height: 0.0, large: false };
        let rx = Rx::new("now_playing", &theme, &display, crate::fit::Form::Full, false);
        let keys = |n: &Node| n.children.iter().filter_map(|c| c.key.clone()).collect::<Vec<_>>();
        let mut w = NowPlaying::new(&Options::default());
        let music = Player { app: App::Music, state: State::Playing, title: "A".into(), duration: 200.0, position: 50.0, at: time::now(), pid: Some(1), ..Default::default() };
        w.players = [music, Player::new(App::Spotify), Player::new(App::Other)];
        let peek = w.peek(&rx).expect("a peek");
        assert_eq!(keys(&peek), ["seek", "controls"]);
        // The pill that grows has no highlight of its own
        let clear = Some(crate::gfx::Rgba(0.0, 0.0, 0.0, 0.0));
        assert_eq!(w.bar(&rx).expect("a pill").hover, clear);
        assert_eq!(keys(&peek.children[1]), ["shuffle", "previous", "playpause", "next", "repeat"]);
        assert!(peek.children[0].children[1].on_click.is_some(), "Music seeks");
        // A stream has no time to seek in
        w.players[0].duration = 0.0;
        assert_eq!(keys(&w.peek(&rx).expect("a peek")), ["controls"]);
        // Another app: only what it takes, or where to control it while macOS sends the controls
        // elsewhere
        w.players[0] = Player::new(App::Music);
        w.players[2] = Player {
            app: App::Other,
            state: State::Playing,
            title: "A".into(),
            pid: Some(2),
            controls: Some(Controls { next: false, previous: false, seek: false }),
            ..Default::default()
        };
        assert_eq!(keys(&w.peek(&rx).expect("a peek").children[0]), ["playpause"]);
        w.players[2].controls_go_to = Some("Spotify".into());
        assert_eq!(keys(&w.peek(&rx).expect("a peek")), ["elsewhere"]);
        // None without the option, or with nothing playing
        w.hover = false;
        assert!(w.peek(&rx).is_none());
        // Without it, the pill has the theme's highlight again
        assert_eq!(w.bar(&rx).expect("a pill").hover, None);
        w.hover = true;
        w.players[2].state = State::Stopped;
        assert!(w.peek(&rx).is_none());
    }

    #[test]
    fn around_the_notch_the_cover_and_the_bars_keep_their_room() {
        let theme = crate::theme::Theme::from_config(&crate::config::ThemeConfig::default());
        let frame = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(1512.0, 982.0));
        let display = crate::display::DisplayInfo { id: 1, name: "Test".into(), frame, scale: 2.0, notch: Some((646.0, 866.0)), notch_height: 38.0, large: false };
        let keys = |n: &Node| n.children.iter().filter_map(|c| c.key.clone()).collect::<Vec<_>>();
        let mut w = NowPlaying::new(&Options::default());
        let title = "A made-up title much longer than the island has room for";
        w.players = [
            Player { app: App::Music, state: State::Playing, title: title.into(), artist: "A Made-up Band".into(), pid: Some(1), ..Default::default() },
            Player::new(App::Spotify),
            Player::new(App::Other),
        ];
        let island = Island { notch: 220.0, height: 38.0, ear: 6.0, radius: 10.0, left: 0.0, right: 0.0 };
        let drawn = |w: &NowPlaying, left: f64| {
            let mut rx = Rx::new("now_playing", &theme, &display, crate::fit::Form::Full, false);
            rx.island = Some(Island { left, ..island });
            w.bar(&rx).expect("an island")
        };
        // The least: the cover, some of the title and the bars
        let n = drawn(&w, 0.0);
        assert!(n.style.notch.is_some());
        assert_eq!(keys(&n), ["left", "notch", "right"]);
        let wing = &n.children[0].children[0];
        assert_eq!(keys(wing), ["cover", "title"]);
        assert_eq!(wing.children[1].style.max_width, Some(ISLAND_TEXT));
        assert!(wing.children[1].style.marquee);
        assert_eq!(keys(&n.children[2].children[0].children[0]), ["bars"]);
        // With room, the title first, then the artist
        let whole = crate::gfx::measure_text(title, &theme.font()).width.ceil();
        let wing = drawn(&w, whole - ISLAND_TEXT).children[0].children[0].clone();
        assert_eq!((keys(&wing), wing.children[1].style.max_width), (vec!["cover".to_string(), "title".to_string()], Some(whole)));
        assert_eq!(keys(&drawn(&w, 2000.0).children[0].children[0]), ["cover", "title", "artist"]);
        // Paused: the pause sign, and a peek with the title the island cuts
        w.players[0].state = State::Paused;
        assert_eq!(keys(&drawn(&w, 0.0).children[2].children[0].children[0]), ["paused"]);
        let mut rx = Rx::new("now_playing", &theme, &display, crate::fit::Form::Full, false);
        rx.island = Some(island);
        assert_eq!(keys(&w.peek(&rx).expect("a peek")), ["track", "controls"]);
        assert!(w.notice(&rx).is_some());
        // Without the option, the pill
        w.notch = false;
        assert!(drawn(&w, 0.0).style.notch.is_none());
        assert!(w.notice(&rx).is_none());
    }

    #[test]
    fn a_track_after_another_shows_for_a_moment() {
        let mut w = NowPlaying::new(&Options::default());
        w.mock = true;
        w.players = [
            Player { app: App::Music, state: State::Playing, title: "A".into(), pid: Some(0), ..Default::default() },
            Player::new(App::Spotify),
            Player::new(App::Other),
        ];
        w.queue = vec![Player { title: "B".into(), ..Default::default() }];
        let mut timers = Vec::new();
        let mut cx = Cx { name: "now_playing", generation: 0, popup_open: false, peek_open: false, timers: &mut timers, effects: Vec::new() };
        let notices = |cx: &Cx| cx.effects.iter().filter(|e| matches!(e, super::super::Effect::Notice)).count();
        // Not the first one
        w.watch_track(&mut cx);
        assert_eq!(notices(&cx), 0);
        w.action("next", None, &mut cx);
        assert_eq!((w.players[0].title.as_str(), notices(&cx)), ("B", 1));
        // Nor a pause and play again
        for _ in 0..2 {
            w.action("playpause", None, &mut cx);
            w.watch_track(&mut cx);
        }
        assert_eq!(notices(&cx), 1);
        w.action("previous", None, &mut cx);
        assert_eq!((w.players[0].title.as_str(), notices(&cx)), ("A", 2));
    }

    #[test]
    fn the_controls_change_the_made_up_players() {
        let mut w = NowPlaying::new(&Options::default());
        w.mock = true;
        let player = |app, state| Player { app, state, title: "A".into(), duration: 200.0, position: 50.0, at: time::now(), pid: Some(0), repeat: Some(Repeat::Off), favorited: Some(false), ..Default::default() };
        w.players = [player(App::Music, State::Playing), player(App::Spotify, State::Paused), Player::new(App::Other)];
        let mut timers = Vec::new();
        let mut cx = Cx { name: "now_playing", generation: 0, popup_open: true, peek_open: false, timers: &mut timers, effects: Vec::new() };
        let music = |w: &NowPlaying| w.players[0].clone();
        w.action("playpause", None, &mut cx);
        assert_eq!(music(&w).state, State::Paused);
        w.action("shuffle", None, &mut cx);
        assert_eq!(music(&w).shuffle, Some(true));
        for expected in [Repeat::All, Repeat::One, Repeat::Off] {
            w.action("repeat", None, &mut cx);
            assert_eq!(music(&w).repeat, Some(expected));
        }
        w.action("favorite", None, &mut cx);
        assert_eq!(music(&w).favorited, Some(true));
        w.action("seek", Some(0.25), &mut cx);
        assert_eq!(music(&w).position, 50.0);
        w.action("volume", Some(0.333), &mut cx);
        assert_eq!(music(&w).volume, Some(33.0));
        // Spotify, picked in the popup, repeats all or nothing
        w.action("source:spotify", None, &mut cx);
        assert_eq!(w.shown().map(|p| p.app), Some(App::Spotify));
        w.action("repeat", None, &mut cx);
        w.action("repeat", None, &mut cx);
        assert_eq!(w.players[1].repeat, Some(Repeat::Off));
        assert_eq!(music(&w).repeat, Some(Repeat::Off));
    }

    fn helped(line: &str) -> Helped {
        serde_json::from_str(line).expect("a line of the helper")
    }

    #[test]
    fn the_helper_tells_the_other_app() {
        let mut w = NowPlaying::new(&Options::default());
        let mut timers = Vec::new();
        let mut cx = Cx { name: "now_playing", generation: 0, popup_open: false, peek_open: false, timers: &mut timers, effects: Vec::new() };
        // The first one playing is the active one, as MediaRemote has one
        let app = |bundle: &str, state: &str| {
            let name = if bundle == "com.spotify.client" { "Spotify" } else { "Player" };
            format!(r#"{{"bundle":"{bundle}","name":"{name}","pid":42,"state":"{state}","title":"Tide Tables","artist":"Harbor Notes","album":"","duration":120.5,"position":30,"next":false,"previous":true,"seek":true,"artwork":"a1","active":ACTIVE}}"#)
        };
        let players = |apps: &[String]| {
            let mut first = true;
            let apps: Vec<String> = apps
                .iter()
                .map(|a| {
                    let active = first && a.contains(r#""state":"playing""#);
                    first &= !active;
                    a.replace("ACTIVE", if active { "true" } else { "false" })
                })
                .collect();
            helped(&format!(r#"{{"players":[{}]}}"#, apps.join(",")))
        };
        let other = |w: &NowPlaying| w.players[2].bundle.clone();
        w.helped(players(&[app("com.spotify.client", "paused"), app("com.example.player", "playing")]), &mut cx);
        let p = w.current().expect("the other app, playing");
        assert_eq!((p.app, p.name.as_str(), p.pid, p.cover_key.as_str()), (App::Other, "Player", Some(42), "a1"));
        assert_eq!(p.can_skip(), (true, false));
        assert!(p.can_seek());
        // A cover for another artwork, or of another app, is too late: nothing to do
        cx.effects.clear();
        w.helped(helped(r#"{"bundle":"com.example.player","artwork":"b2","cover":"AAAA"}"#), &mut cx);
        w.helped(helped(r#"{"bundle":"com.example.radio","artwork":"a1","cover":"AAAA"}"#), &mut cx);
        assert!(cx.effects.is_empty());
        // Spotify plays over the video: Spotify's own player has it, the video stays, and as the
        // controls would reach Spotify it has none
        w.helped(players(&[app("com.spotify.client", "playing"), app("com.example.player", "playing")]), &mut cx);
        assert_eq!((other(&w).as_str(), w.players[2].state), ("com.example.player", State::Playing));
        assert_eq!(w.players[2].controls_go_to.as_deref(), Some("Spotify"));
        assert!(!w.players[2].can_seek());
        w.action("source:other", None, &mut cx);
        w.action("playpause", None, &mut cx);
        assert_eq!(w.players[2].state, State::Playing);
        w.helped(players(&[app("com.spotify.client", "playing"), app("com.example.player", "paused")]), &mut cx);
        assert_eq!((other(&w).as_str(), w.players[2].state), ("com.example.player", State::Paused));
        // Another app plays: it takes the place of the paused one, not of one playing
        w.helped(players(&[app("com.example.player", "paused"), app("com.example.radio", "playing")]), &mut cx);
        assert_eq!(other(&w), "com.example.radio");
        w.helped(players(&[app("com.example.player", "playing"), app("com.example.radio", "playing")]), &mut cx);
        assert_eq!(other(&w), "com.example.radio");
        // Gone, or only Spotify left
        w.helped(players(&[app("com.spotify.client", "playing")]), &mut cx);
        assert!(!w.players[2].active());
        w.helped(helped(r#"{"error":"MediaRemote changed"}"#), &mut cx);
        assert!(w.helper_broken);
    }

    #[test]
    fn the_other_app_takes_only_its_controls() {
        let mut w = NowPlaying::new(&Options::default());
        w.mock = true;
        let controls = Some(Controls { next: false, previous: true, seek: false });
        w.players[2] = Player { app: App::Other, bundle: "com.example.player".into(), name: "Player".into(), controls, state: State::Playing, title: "A".into(), duration: 100.0, position: 10.0, at: time::now(), pid: Some(0), ..Default::default() };
        let mut timers = Vec::new();
        let mut cx = Cx { name: "now_playing", generation: 0, popup_open: true, peek_open: false, timers: &mut timers, effects: Vec::new() };
        w.action("seek", Some(0.5), &mut cx);
        assert!(w.players[2].position < 50.0);
        w.action("playpause", None, &mut cx);
        assert_eq!(w.players[2].state, State::Paused);
        // Music's controls aren't there
        w.action("shuffle", None, &mut cx);
        assert_eq!(w.players[2].shuffle, None);
    }

    /// The helper in perl, on this Mac: its first line, whatever plays, then its end with stdin.
    #[test]
    fn the_helper_runs_in_perl() {
        let path = std::env::temp_dir().join(format!("rustybar-helper-{}.dylib", std::process::id()));
        std::fs::write(&path, HELPER).expect("the helper on disk");
        let mut child = Command::new("/usr/bin/perl").arg("-e").arg(PERL).arg(&path).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("perl");
        let mut line = String::new();
        BufReader::new(child.stdout.take().expect("stdout")).read_line(&mut line).expect("a line");
        drop(child.stdin.take());
        let status = child.wait().expect("its end");
        let _ = std::fs::remove_file(&path);
        let m = helped(&line);
        assert_eq!(m.error, None);
        assert!(m.players.is_some_and(|players| players.iter().all(|p| !p.bundle.is_empty())));
        assert!(status.success());
    }

    #[test]
    fn covers_are_scaled_down_to_a_png() {
        let cover = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/mock/now_playing/harbor.jpg")).expect("the made-up cover");
        let png = thumbnail(&cover, 64).expect("a PNG");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        assert!(png.len() < cover.len());
        assert!(thumbnail(b"not an image", 64).is_none());
    }

    /// The made-up covers in their colors: the sunset's navy sea under its golden sun, the
    /// orchard's pale green under a deeper one.
    #[test]
    fn covers_have_their_colors() {
        let hue = |p: crate::palette::Lch| p.h.to_degrees().rem_euclid(360.0);
        let cover = |f: &str| colors(&format!("{}/examples/mock/now_playing/{f}", env!("CARGO_MANIFEST_DIR"))).expect("its colors");
        let harbor = cover("harbor.jpg");
        assert!((250.0..300.0).contains(&hue(harbor.dominant)) && harbor.dominant.l < 0.4, "{harbor:?}");
        assert!((60.0..100.0).contains(&hue(harbor.accent)) && harbor.accent.c > 0.1, "{harbor:?}");
        let orchard = cover("orchard.png");
        assert!((110.0..160.0).contains(&hue(orchard.dominant)) && orchard.dominant.l > 0.8, "{orchard:?}");
        assert!((120.0..170.0).contains(&hue(orchard.accent)) && orchard.accent.c > orchard.dominant.c, "{orchard:?}");
        assert!(colors("/nonexistent.png").is_none());
    }
}
