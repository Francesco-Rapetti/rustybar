//! The compose key, as on Linux: a tap of a modifier (the right ⌘ by default), then a sequence of
//! keys, and in their place comes a character: `e` gives è, `'` then `e` gives é. `[compose]` in
//! the config turns it on and adds, changes or removes sequences (`sequences`, keys → text; an
//! empty text removes one RustyBar comes with).
//!
//! A tap is the key pressed and released within `tap` seconds, and what's typed meanwhile belongs
//! to it: typing fast, the first key of the sequence goes down before the compose key is up, and
//! must not be lost nor taken for a shortcut. So a key that could begin a sequence, pressed while
//! the compose key is down and may still be a tap, is kept back until one of the two is known: the
//! compose key comes up in time, and the keys kept are the sequence; or the time passes, and they
//! go on as they came, the shortcuts of a modifier held down. A key that begins no sequence is a
//! shortcut at once, and so is everything once the key has been down for longer than a tap. The
//! key itself always goes on to the apps: only what it means for the keys after it changes.
//!
//! An event tap reads the keyboard, on a thread of its own so that typing never waits for the
//! bar; it takes Accessibility, and with none it starts as soon as that is granted. The keys of a
//! sequence are swallowed and its text typed with a key event that carries it. A sequence that
//! doesn't exist is typed as it is, so that nothing is lost; Esc, a click or a shortcut drop it,
//! Backspace takes its last key back. Nothing typed is kept or logged: only the keys of the
//! sequence in progress, until it ends, and how long the compose key was down the last few times
//! (`rustybar msg compose`), which says why a tap wasn't one.
//!
//! The keys are read as characters, by the keyboard layout in use, so a sequence is the same
//! whatever the layout: `'` is wherever that layout has it. The text an event carries isn't
//! used: the one of a key made by another app ignores Shift.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::config::ComposeConfig;
use crate::gfx::Weight;
use crate::theme::Theme;
use crate::ui::{Justify, Node, ink};

/// The sequences RustyBar comes with: the accents of Italian on the vowel alone (grave) and after
/// `'` (acute), then those of Linux's compose key that don't begin with a vowel.
pub const DEFAULTS: &[(&str, &str)] = &[
    ("a", "à"), ("e", "è"), ("i", "ì"), ("o", "ò"), ("u", "ù"),
    ("A", "À"), ("E", "È"), ("I", "Ì"), ("O", "Ò"), ("U", "Ù"),
    ("'a", "á"), ("'e", "é"), ("'i", "í"), ("'o", "ó"), ("'u", "ú"),
    ("'A", "Á"), ("'E", "É"), ("'I", "Í"), ("'O", "Ó"), ("'U", "Ú"),
    ("\"a", "ä"), ("\"e", "ë"), ("\"i", "ï"), ("\"o", "ö"), ("\"u", "ü"),
    ("\"A", "Ä"), ("\"E", "Ë"), ("\"I", "Ï"), ("\"O", "Ö"), ("\"U", "Ü"),
    ("^a", "â"), ("^e", "ê"), ("^i", "î"), ("^o", "ô"), ("^u", "û"),
    ("^A", "Â"), ("^E", "Ê"), ("^I", "Î"), ("^O", "Ô"), ("^U", "Û"),
    ("~a", "ã"), ("~n", "ñ"), ("~o", "õ"), ("~A", "Ã"), ("~N", "Ñ"), ("~O", "Õ"),
    (",c", "ç"), (",C", "Ç"), ("ss", "ß"),
    ("=e", "€"), ("<<", "«"), (">>", "»"), ("..", "…"), ("--.", "–"), ("---", "—"),
    ("??", "¿"), ("!!", "¡"),
];

/// A sequence as the settings show it: one of the file, or one RustyBar comes with (`default`,
/// its text there). An empty `text` is a default the file removes.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub keys: String,
    pub text: String,
    pub default: Option<&'static str>,
}

/// Every sequence: first those only the file has, in its order, then those RustyBar comes with,
/// as the file changes them.
pub fn listing(custom: &[(String, String)]) -> Vec<Entry> {
    let default_of = |keys: &str| DEFAULTS.iter().find(|(k, _)| *k == keys).map(|(_, text)| *text);
    let mut out: Vec<Entry> = custom
        .iter()
        .filter(|(keys, text)| !keys.is_empty() && !text.is_empty() && default_of(keys).is_none())
        .map(|(keys, text)| Entry { keys: keys.clone(), text: text.clone(), default: None })
        .collect();
    for (keys, text) in DEFAULTS {
        let text = custom.iter().find(|(k, _)| k == keys).map(|(_, t)| t.as_str()).unwrap_or(text);
        out.push(Entry { keys: keys.to_string(), text: text.to_string(), default: default_of(keys) });
    }
    out
}

/// The shortest sequence of `active` that `keys` begins with: it ends as soon as it's typed, so
/// `keys` is never reached.
pub fn shadow<'a>(keys: &str, active: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    active.filter(|a| !a.is_empty() && *a != keys && keys.starts_with(a)).min_by_key(|a| a.len())
}

// `CGEventFlags`: the modifiers down, whichever of the two…
const SHIFT: u64 = 1 << 17;
const CONTROL: u64 = 1 << 18;
const ALT: u64 = 1 << 19;
const COMMAND: u64 = 1 << 20;
const CAPS_LOCK: u64 = 1 << 16;
/// …and which, in the low bits a keyboard sets (`NX_DEVICE…KEYMASK`).
const DEVICE: u64 = 0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20 | 0x40 | 0x2000;
const DEVICE_SHIFTS: u64 = 0x02 | 0x04;
/// The modifiers that are part of typing, not of a shortcut: the two Shift, Caps Lock and fn.
const TYPING: [u16; 4] = [0x38, 0x3C, 0x39, 0x3F];

/// A modifier that can be the compose key: its virtual key code and its bits in the flags.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub name: &'static str,
    code: u16,
    device: u64,
    mask: u64,
}

pub const KEYS: &[Key] = &[
    Key { name: "right_cmd", code: 0x36, device: 0x10, mask: COMMAND },
    Key { name: "right_alt", code: 0x3D, device: 0x40, mask: ALT },
    Key { name: "right_ctrl", code: 0x3E, device: 0x2000, mask: CONTROL },
    Key { name: "right_shift", code: 0x3C, device: 0x04, mask: SHIFT },
    Key { name: "left_cmd", code: 0x37, device: 0x08, mask: COMMAND },
    Key { name: "left_alt", code: 0x3A, device: 0x20, mask: ALT },
    Key { name: "left_ctrl", code: 0x3B, device: 0x01, mask: CONTROL },
    Key { name: "left_shift", code: 0x38, device: 0x02, mask: SHIFT },
];

/// `right_cmd`, or as one may write it: `right-command`, `Right_Option`.
pub fn key(name: &str) -> Option<Key> {
    let name = name.trim().to_lowercase().replace('-', "_").replace("command", "cmd").replace("option", "alt").replace("opt", "alt").replace("control", "ctrl");
    KEYS.iter().find(|k| k.name == name).copied()
}

/// What's wrong with `[compose]`, for `rustybar check`.
pub fn check(config: &ComposeConfig) -> Vec<String> {
    let mut problems = Vec::new();
    if key(&config.key).is_none() {
        let names: Vec<&str> = KEYS.iter().map(|k| k.name).collect();
        problems.push(format!("compose: unknown key {:?}: one of {}", config.key, names.join(", ")));
    }
    if config.tap <= 0.0 {
        problems.push("compose: tap must be more than 0 seconds".to_string());
    }
    problems
}

/// What the keyboard thread goes by: the key, the times in nanoseconds and the sequences.
pub struct Rules {
    key: Key,
    tap: u64,
    /// 0: a sequence waits for its next key for ever.
    timeout: u64,
    sequences: HashMap<String, String>,
    /// What every sequence begins with, short of itself.
    prefixes: HashSet<String>,
}

impl Rules {
    pub fn new(config: &ComposeConfig) -> Rules {
        let custom: Vec<(String, String)> = config.sequences.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let sequences: HashMap<String, String> = listing(&custom).into_iter().filter(|e| !e.text.is_empty()).map(|e| (e.keys, e.text)).collect();
        let mut prefixes = HashSet::new();
        for keys in sequences.keys() {
            for (at, _) in keys.char_indices().skip(1) {
                prefixes.insert(keys[..at].to_string());
            }
        }
        let nanos = |secs: f64| (secs.max(0.0) * 1e9) as u64;
        Rules { key: key(&config.key).unwrap_or(KEYS[0]), tap: nanos(config.tap), timeout: nanos(config.timeout), sequences, prefixes }
    }

    pub fn len(&self) -> usize {
        self.sequences.len()
    }

    /// `keys` is a sequence, or the start of one.
    fn leads(&self, keys: &str) -> bool {
        self.sequences.contains_key(keys) || self.prefixes.contains(keys)
    }
}

/// What the keyboard does, as far as a sequence goes.
pub enum Input<'a> {
    /// A modifier went down or up: its key code and the flags after it.
    Modifier { code: u16, flags: u64 },
    /// A key went down, `text` being what it types without ⌘ and ⌃ (asked only during a sequence
    /// and while the compose key is down).
    Down { code: u16, flags: u64, repeat: bool, text: &'a str },
    Up { code: u16 },
    /// A mouse button went down.
    Click,
    /// The time `Engine::deadline` gave has come.
    Tick,
}

/// What becomes of the event.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Fate {
    /// It goes on to the app in front.
    #[default]
    Pass,
    /// Nobody sees it.
    Swallow,
    /// Nobody sees it yet: it's kept, after those kept before it, until a verdict says what
    /// becomes of them all.
    Keep,
}

/// What becomes of the events kept.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Kept {
    /// Nothing yet.
    #[default]
    Wait,
    /// They go on as they came, in their order, before the event of this verdict.
    Replay,
    /// Nobody sees them: they were the keys of a sequence.
    Drop,
}

#[derive(Debug, Default, PartialEq)]
pub struct Verdict {
    pub fate: Fate,
    pub kept: Kept,
    /// To type, in place of what was swallowed.
    pub text: Option<String>,
    /// The compose key came up: how that press went.
    pub press: Option<Press>,
}

/// What a press of the compose key turned out to be.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Outcome {
    /// A tap: a sequence started, or the one that was on was dropped.
    Tap,
    /// Down for longer than a tap, with nothing else.
    Long,
    /// A modifier: a key that begins no sequence, a key held past the time of a tap, or a click.
    Chord,
    /// A modifier among others: ⌃ or ⌥ too.
    Mixed,
}

/// A press of the compose key, in times alone: for `rustybar msg compose`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Press {
    /// Nanoseconds from down to up.
    pub held: u64,
    pub outcome: Outcome,
    /// How many keys went down meanwhile.
    pub keys: u8,
    /// Nanoseconds from the press to the first of them.
    pub first: Option<u64>,
    /// That first key came up before the compose key did.
    pub nested: bool,
}

const ESC: u16 = 0x35;
const BACKSPACE: u16 = 0x33;

/// A key event kept back while the compose key is down.
struct KeptKey {
    code: u16,
    down: bool,
    text: String,
}

/// The compose key, while it's down.
struct Hold {
    since: u64,
    /// It may still be a tap: nothing has made it a modifier yet.
    alone: bool,
    /// What it is if not a tap.
    outcome: Outcome,
    /// The keys kept back until that's known, oldest first.
    kept: Vec<KeptKey>,
    keys: u8,
    first: Option<(u64, u16)>,
    nested: bool,
}

/// Follows the keyboard from the compose key to the end of a sequence. No AppKit in here: the
/// tests drive it.
#[derive(Default)]
pub struct Engine {
    held: Option<Hold>,
    /// A sequence is on: its keys so far and when the last came.
    typed: Option<(String, u64)>,
    /// The keys swallowed while down: their release, and their repeating, are swallowed too.
    swallowed: Vec<u16>,
}

impl Engine {
    /// The next key down must come with what it types.
    pub fn wants_text(&self) -> bool {
        self.typed.is_some() || self.held.as_ref().is_some_and(|h| h.alone)
    }

    /// A sequence is on, waiting for its keys.
    pub fn composing(&self) -> bool {
        self.typed.is_some()
    }

    /// When what waits stops waiting, whichever comes first: the keys kept, for the compose key to
    /// come up; a sequence, for its next key. At that time, a `Tick`.
    pub fn deadline(&self, rules: &Rules) -> Option<u64> {
        let kept = self.held.as_ref().filter(|h| h.alone && !h.kept.is_empty()).map(|h| h.since + rules.tap);
        let left = self.typed.as_ref().filter(|_| rules.timeout > 0).map(|(_, last)| last + rules.timeout);
        match (kept, left) {
            (Some(kept), Some(left)) => Some(kept.min(left)),
            (kept, left) => kept.or(left),
        }
    }

    /// Forgets everything: the rules changed, or the tap missed some events. What was kept is the
    /// caller's to let go.
    pub fn reset(&mut self) {
        *self = Engine::default();
    }

    /// A key that types `text` joins the sequence: what to type in its place, if anything yet.
    fn feed(&mut self, rules: &Rules, text: &str, now: u64) -> String {
        let Some((typed, last)) = self.typed.as_mut() else { return text.to_string() };
        typed.push_str(text);
        *last = now;
        if let Some(out) = rules.sequences.get(typed.as_str()) {
            let out = out.clone();
            self.typed = None;
            return out;
        }
        if rules.prefixes.contains(typed.as_str()) {
            return String::new();
        }
        // No such sequence: what was typed, as it is
        let out = std::mem::take(typed);
        self.typed = None;
        out
    }

    pub fn handle(&mut self, rules: &Rules, input: Input, now: u64) -> Verdict {
        let mut verdict = Verdict::default();
        // What began at `start` has lasted `span`: for a `Tick`, from the very time it was asked for
        let tick = matches!(input, Input::Tick);
        let over = |start: u64, span: u64| if tick { now >= start.saturating_add(span) } else { now.saturating_sub(start) > span };
        // A sequence left there too long is over
        if rules.timeout > 0 && self.typed.as_ref().is_some_and(|(_, last)| over(*last, rules.timeout)) {
            self.typed = None;
        }
        // Down for longer than a tap: a modifier, and the keys kept meanwhile were its shortcuts
        if let Some(hold) = self.held.as_mut()
            && hold.alone
            && over(hold.since, rules.tap)
        {
            hold.alone = false;
            if !hold.kept.is_empty() {
                hold.kept.clear();
                hold.outcome = Outcome::Chord;
                verdict.kept = Kept::Replay;
                self.typed = None;
            }
        }
        match input {
            Input::Tick => {}
            Input::Modifier { code, flags } if code == rules.key.code => {
                let down = if flags & DEVICE != 0 {
                    flags & rules.key.device != 0
                } else {
                    // A keyboard that doesn't say which of the two: down and up take turns
                    flags & rules.key.mask != 0 && self.held.is_none()
                };
                if down {
                    if self.held.is_none() {
                        // Shift may be down already, for a capital
                        let others = flags & DEVICE & !(rules.key.device | DEVICE_SHIFTS) != 0 || flags & (CONTROL | ALT | COMMAND) & !rules.key.mask != 0;
                        let outcome = if others { Outcome::Mixed } else { Outcome::Long };
                        self.held = Some(Hold { since: now, alone: !others, outcome, kept: Vec::new(), keys: 0, first: None, nested: false });
                    }
                } else if let Some(hold) = self.held.take() {
                    let held = now.saturating_sub(hold.since);
                    let tap = hold.alone && held <= rules.tap;
                    if tap {
                        // A sequence starts, or the one that was on is dropped; then the keys
                        // that came before the release, in their order
                        self.typed = if self.typed.is_some() { None } else { Some((String::new(), now)) };
                        let mut out = String::new();
                        for (i, key) in hold.kept.iter().enumerate().filter(|(_, k)| k.down) {
                            if !hold.kept[i + 1..].iter().any(|k| !k.down && k.code == key.code) {
                                self.swallowed.push(key.code);
                            }
                            out.push_str(&self.feed(rules, &key.text, now));
                        }
                        if !hold.kept.is_empty() {
                            verdict.kept = Kept::Drop;
                        }
                        if !out.is_empty() {
                            verdict.text = Some(out);
                        }
                    }
                    let outcome = if tap { Outcome::Tap } else { hold.outcome };
                    verdict.press = Some(Press { held, outcome, keys: hold.keys, first: hold.first.map(|(after, _)| after), nested: hold.nested });
                }
            }
            Input::Modifier { code, .. } => {
                // With ⌃ or ⌥ the compose key is a modifier itself
                if !TYPING.contains(&code)
                    && let Some(hold) = self.held.as_mut()
                    && hold.alone
                {
                    hold.alone = false;
                    hold.outcome = Outcome::Mixed;
                    if !hold.kept.is_empty() {
                        hold.kept.clear();
                        verdict.kept = Kept::Replay;
                    }
                }
            }
            Input::Click => {
                if let Some(hold) = self.held.as_mut()
                    && hold.alone
                {
                    hold.alone = false;
                    hold.outcome = Outcome::Chord;
                    if !hold.kept.is_empty() {
                        hold.kept.clear();
                        verdict.kept = Kept::Replay;
                    }
                }
                self.typed = None;
            }
            Input::Up { code } => {
                if let Some(hold) = self.held.as_mut()
                    && hold.alone
                {
                    // The release of a key kept waits with it
                    let count = |down: bool| hold.kept.iter().filter(|k| k.down == down && k.code == code).count();
                    if count(true) > count(false) {
                        if hold.first.is_some_and(|(_, first)| first == code) {
                            hold.nested = true;
                        }
                        hold.kept.push(KeptKey { code, down: false, text: String::new() });
                        verdict.fate = Fate::Keep;
                        return verdict;
                    }
                }
                if let Some(i) = self.swallowed.iter().position(|c| *c == code) {
                    self.swallowed.swap_remove(i);
                    verdict.fate = Fate::Swallow;
                }
            }
            Input::Down { code, flags, repeat, text } => {
                if repeat && self.swallowed.contains(&code) {
                    verdict.fate = Fate::Swallow;
                    return verdict;
                }
                if let Some(hold) = self.held.as_mut() {
                    if repeat {
                        // Of a key kept, which goes down once; or of one down from before
                        if hold.kept.iter().any(|k| k.code == code) {
                            verdict.fate = Fate::Swallow;
                        }
                        return verdict;
                    }
                    self.swallowed.retain(|c| *c != code);
                    if hold.alone {
                        hold.keys = hold.keys.saturating_add(1);
                        if hold.first.is_none() {
                            hold.first = Some((now.saturating_sub(hold.since), code));
                        }
                        let types = !text.is_empty() && text.chars().all(printable);
                        // The first is kept if it could begin a sequence; after it, whatever types
                        if types && (!hold.kept.is_empty() || rules.leads(text)) {
                            hold.kept.push(KeptKey { code, down: true, text: text.to_string() });
                            verdict.fate = Fate::Keep;
                            return verdict;
                        }
                        // A shortcut with the compose key
                        hold.alone = false;
                        hold.outcome = Outcome::Chord;
                        if !hold.kept.is_empty() {
                            hold.kept.clear();
                            verdict.kept = Kept::Replay;
                        }
                    }
                    self.typed = None;
                    return verdict;
                }
                // Pressed again without the release having been seen
                self.swallowed.retain(|c| *c != code);
                let Some((typed, last)) = self.typed.as_mut() else { return verdict };
                if repeat {
                    return verdict;
                }
                if flags & (COMMAND | CONTROL) != 0 {
                    self.typed = None;
                    return verdict;
                }
                if code == ESC || code == BACKSPACE {
                    if code == ESC || typed.pop().is_none() {
                        self.typed = None;
                    } else {
                        *last = now;
                    }
                    self.swallowed.push(code);
                    verdict.fate = Fate::Swallow;
                    return verdict;
                }
                if text.is_empty() || !text.chars().all(printable) {
                    // Return, an arrow, a function key
                    self.typed = None;
                    return verdict;
                }
                self.swallowed.push(code);
                verdict.fate = Fate::Swallow;
                let out = self.feed(rules, text, now);
                if !out.is_empty() {
                    verdict.text = Some(out);
                }
            }
        }
        verdict
    }
}

/// Not a control character, nor one of those AppKit gives the arrows and the function keys.
fn printable(c: char) -> bool {
    !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(&c)
}

// ---- the keyboard ------------------------------------------------------------------------------

type Callback = unsafe extern "C" fn(proxy: *mut c_void, kind: u32, event: *mut c_void, info: *mut c_void) -> *mut c_void;
type TimerCallback = unsafe extern "C" fn(timer: *mut c_void, info: *mut c_void);

/// `CFRunLoopTimerContext`.
#[repr(C)]
struct TimerContext {
    version: isize,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    describe: *const c_void,
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> u8;
    fn CGEventTapCreate(tap: u32, place: u32, options: u32, events: u64, callback: Callback, info: *mut c_void) -> *mut c_void;
    fn CGEventTapEnable(tap: *mut c_void, enable: bool);
    fn CGEventTapPostEvent(proxy: *mut c_void, event: *mut c_void);
    fn CGEventPost(tap: u32, event: *mut c_void);
    fn CGEventCreateCopy(event: *mut c_void) -> *mut c_void;
    fn CGEventGetIntegerValueField(event: *mut c_void, field: u32) -> i64;
    fn CGEventSetIntegerValueField(event: *mut c_void, field: u32, value: i64);
    fn CGEventGetFlags(event: *mut c_void) -> u64;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventGetTimestamp(event: *mut c_void) -> u64;
    fn CGEventCreateKeyboardEvent(source: *const c_void, key: u16, down: bool) -> *mut c_void;
    fn CGEventKeyboardSetUnicodeString(event: *mut c_void, length: usize, string: *const u16);
    fn CFMachPortCreateRunLoopSource(allocator: *const c_void, port: *mut c_void, order: isize) -> *const c_void;
    fn CFRunLoopGetCurrent() -> *const c_void;
    fn CFRunLoopAddSource(run_loop: *const c_void, source: *const c_void, mode: *const c_void);
    fn CFRunLoopTimerCreate(allocator: *const c_void, fire: f64, interval: f64, flags: usize, order: isize, callback: TimerCallback, context: *mut TimerContext) -> *mut c_void;
    fn CFRunLoopAddTimer(run_loop: *const c_void, timer: *mut c_void, mode: *const c_void);
    fn CFRunLoopTimerSetNextFireDate(timer: *mut c_void, fire: f64);
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRunLoopRun();
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFRelease(cf: *const c_void);
    fn CFDataGetBytePtr(data: *const c_void) -> *const u8;
    static kCFRunLoopCommonModes: *const c_void;
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardLayoutInputSource() -> *const c_void;
    fn TISGetInputSourceProperty(source: *const c_void, key: *const c_void) -> *const c_void;
    fn LMGetKbdType() -> u8;
    static kTISPropertyUnicodeKeyLayoutData: *const c_void;
}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn UCKeyTranslate(layout: *const u8, code: u16, action: u16, modifiers: u32, keyboard: u32, options: u32, dead: *mut u32, max: usize, length: *mut usize, string: *mut u16) -> i32;
}

unsafe extern "C" {
    fn clock_gettime_nsec_np(clock: u32) -> u64;
}

const CLOCK_UPTIME_RAW: u32 = 8;

/// `kCGEventKeyDown`, `kCGEventKeyUp`, `kCGEventFlagsChanged` and the mouse buttons going down;
/// what macOS sends in place of an event when it turns the tap off.
const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const FLAGS_CHANGED: u32 = 12;
const MOUSE_DOWN: [u32; 3] = [1, 3, 25];
const DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
const DISABLED_BY_USER: u32 = 0xFFFF_FFFF;
/// `kCGKeyboardEventAutorepeat`, `kCGKeyboardEventKeycode`, `kCGKeyboardEventKeyboardType`,
/// `kCGEventSourceUserData`.
const FIELD_REPEAT: u32 = 8;
const FIELD_CODE: u32 = 9;
const FIELD_KEYBOARD: u32 = 10;
const FIELD_USER_DATA: u32 = 42;
/// In the user data of the events posted from outside the tap's callback, which come back
/// through the tap: they're let through as they are ("RsBrCmps").
const OURS: i64 = 0x5273_4272_436d_7073;
/// A time the timer never gets to, in seconds: it's set again when there's something to wait for.
const NEVER: f64 = 1e10;

/// The keyboard layout in use: its `uchr` data, which `UCKeyTranslate` reads on any thread.
struct Layout(*const c_void);

unsafe impl Send for Layout {}
unsafe impl Sync for Layout {}

impl Layout {
    /// On the main thread, as all of Text Input Sources.
    fn current() -> Option<Layout> {
        unsafe {
            let source = TISCopyCurrentKeyboardLayoutInputSource();
            if source.is_null() {
                return None;
            }
            let data = TISGetInputSourceProperty(source, kTISPropertyUnicodeKeyLayoutData);
            let layout = (!data.is_null()).then(|| Layout(CFRetain(data)));
            CFRelease(source);
            layout
        }
    }

    /// What the key `code` types with the Shift, Caps Lock and ⌥ of `flags`, on a keyboard of the
    /// kind `keyboard`; a dead key gives its own sign.
    fn text(&self, code: u16, flags: u64, keyboard: u32) -> String {
        // As Carbon's modifiers, shifted down a byte: shiftKey, alphaLock, optionKey
        let mut modifiers = 0;
        for (flag, bit) in [(SHIFT, 2), (CAPS_LOCK, 4), (ALT, 8)] {
            if flags & flag != 0 {
                modifiers |= bit;
            }
        }
        let mut units = [0u16; 8];
        let (mut length, mut dead) = (0usize, 0u32);
        // kUCKeyActionDown, kUCKeyTranslateNoDeadKeysMask
        let status = unsafe { UCKeyTranslate(CFDataGetBytePtr(self.0), code, 0, modifiers, keyboard, 1, &mut dead, units.len(), &mut length, units.as_mut_ptr()) };
        if status != 0 {
            return String::new();
        }
        String::from_utf16_lossy(&units[..length.min(units.len())])
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) };
    }
}

/// What the main thread hands the keyboard thread, which takes it when `GENERATION` moves.
static RULES: Mutex<Option<Arc<Rules>>> = Mutex::new(None);
static LAYOUT: Mutex<Option<Arc<Layout>>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The config wants the compose key.
static WANTED: AtomicBool = AtomicBool::new(false);
/// The tap, once made: it stays for the life of the process, on or off.
static TAP: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());
static STARTING: AtomicBool = AtomicBool::new(false);
/// What the log said last, not to say it at every reload.
static SAID: Mutex<String> = Mutex::new(String::new());
/// The bar shows that a sequence waits for its keys (see `edge_tab`), if the config wants it to.
static MARK: AtomicBool = AtomicBool::new(false);
static MARK_WANTED: AtomicBool = AtomicBool::new(true);
/// Counts the sequences starting and ending: a mark asked for one that's over doesn't show.
static MARK_TICKET: AtomicU64 = AtomicU64::new(0);
/// How long a sequence waits before the bar says so, in seconds: typed fast it's over before, and
/// nothing flashes at every accent.
const MARK_AFTER: f64 = 0.15;
/// The last presses of the compose key and when they ended, oldest first.
static PRESSES: Mutex<VecDeque<(Instant, Press)>> = Mutex::new(VecDeque::new());
const REMEMBERED: usize = 20;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn say(text: String) {
    let mut said = lock(&SAID);
    if *said != text {
        crate::log(&text);
        *said = text;
    }
}

pub fn trusted() -> bool {
    unsafe { AXIsProcessTrusted() != 0 }
}

/// A sequence waits for its keys, and has for a moment: the bars draw `edge_tab`.
pub fn mark() -> bool {
    MARK.load(Ordering::SeqCst)
}

/// The tab at the left edge of the screen while a sequence waits, `height` tall: the one of
/// `fn_keys`, which it covers for that while, in a color of its own and with the sign of the
/// compose key (⎄, drawn in `gfx`: no font has it readable that small). It's drawn over the bar,
/// whose pills stay where they are. A click on it drops the sequence, as a click anywhere does.
pub fn edge_tab(th: &Theme, height: f64) -> Node {
    let width = crate::widgets::fn_keys::EDGE_WIDTH;
    let r = (width / 2.0).floor();
    Node::row(vec![Node::symbol("compose", 14.0, Weight::Bold, ink(th.peach)).key("sign")])
        .key("compose_edge")
        .size(width + r, height)
        .padding(0.0, 0.0, 0.0, r)
        .justify(Justify::Center)
        .radius(r)
        .background(th.peach)
}

/// From the keyboard thread: a sequence started waiting for its keys, or stopped. The bars are
/// told on the main thread, of a start only once it has lasted `MARK_AFTER`.
fn show_mark(on: bool) {
    let ticket = MARK_TICKET.fetch_add(1, Ordering::SeqCst) + 1;
    crate::runtime::post(move |app| {
        if !on {
            if MARK.swap(false, Ordering::SeqCst) {
                app.mark_dirty();
            }
        } else if MARK_WANTED.load(Ordering::SeqCst) {
            crate::runtime::after(MARK_AFTER, move |app| {
                if MARK_TICKET.load(Ordering::SeqCst) == ticket && !MARK.swap(true, Ordering::SeqCst) {
                    app.mark_dirty();
                }
            });
        }
    });
}

/// For `rustybar msg compose`: whether it's on, and how the last presses of the key went, in
/// times alone, to see why a tap wasn't taken for one.
pub fn report() -> String {
    let state = if !WANTED.load(Ordering::SeqCst) {
        "off"
    } else if !TAP.load(Ordering::SeqCst).is_null() {
        "on"
    } else if trusted() {
        "starting"
    } else {
        "waiting for Accessibility"
    };
    let mut out = match lock(&RULES).as_ref() {
        Some(r) => format!("compose: {state} ({}, a tap within {:.2} s, {} sequences)\n", r.key.name, r.tap as f64 / 1e9, r.len()),
        None => format!("compose: {state}\n"),
    };
    let presses = lock(&PRESSES);
    if presses.is_empty() {
        out.push_str("the key hasn't been pressed since the bar started\n");
        return out;
    }
    out.push_str("the last presses of the key, oldest first:\n");
    let ms = |nanos: u64| (nanos as f64 / 1e6).round() as u64;
    for (when, press) in presses.iter() {
        let what = match press.outcome {
            Outcome::Tap => "a tap",
            Outcome::Long => "not a tap: held too long",
            Outcome::Chord => "a modifier: with a key that begins no sequence, a key held past the tap, or a click",
            Outcome::Mixed => "a modifier: with ⌃, ⌥ or the other ⌘",
        };
        let keys = match (press.keys, press.first) {
            (0, _) | (_, None) => String::new(),
            (n, Some(first)) => format!("; {n} key(s) down meanwhile, the first after {} ms{}", ms(first), if press.nested { " and up before it" } else { "" }),
        };
        out.push_str(&format!("  {:>4} s ago  down for {:>4} ms  {what}{keys}\n", when.elapsed().as_secs(), ms(press.held)));
    }
    out
}

thread_local! {
    /// The keyboard layout changing, and Accessibility being granted: followed on the main thread.
    static WATCH: std::cell::RefCell<Vec<crate::runtime::Listener>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Follows `[compose]`: at the start of the bar and at every reload, on the main thread. Never
/// with made-up data, which asks for no permission and leaves the Mac as it is.
pub fn apply(config: &ComposeConfig) {
    if cfg!(test) || std::env::var_os("RUSTYBAR_MOCK").is_some() {
        return;
    }
    let rules = Rules::new(config);
    let about = format!("{}, {} sequences", rules.key.name, rules.len());
    *lock(&RULES) = Some(Arc::new(rules));
    GENERATION.fetch_add(1, Ordering::SeqCst);
    WANTED.store(config.enabled, Ordering::SeqCst);
    MARK_WANTED.store(config.mark, Ordering::SeqCst);
    if !config.enabled || !config.mark {
        // The bars are drawn again after this
        MARK.store(false, Ordering::SeqCst);
    }
    let tap = TAP.load(Ordering::SeqCst);
    if !config.enabled {
        if !tap.is_null() {
            unsafe { CGEventTapEnable(tap, false) };
            say("compose: off".into());
        }
        return;
    }
    watch();
    read_layout();
    if !tap.is_null() {
        unsafe { CGEventTapEnable(tap, true) };
        say(format!("compose: on ({about})"));
    } else {
        start(true);
    }
}

fn read_layout() {
    *lock(&LAYOUT) = Layout::current().map(Arc::new);
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

fn watch() {
    let Some(mtm) = objc2::MainThreadMarker::new() else { return };
    WATCH.with(|w| {
        let mut w = w.borrow_mut();
        if !w.is_empty() {
            return;
        }
        w.push(crate::runtime::Listener::new(mtm, "com.apple.Carbon.TISNotifySelectedKeyboardInputSourceChanged", |_| read_layout()));
        // Posted when the list of Accessibility changes: a moment later the answer has too
        w.push(crate::runtime::Listener::new(mtm, "com.apple.accessibility.api", |_| {
            crate::runtime::later(0.5, || {
                if WANTED.load(Ordering::SeqCst) && TAP.load(Ordering::SeqCst).is_null() {
                    start(false);
                }
            });
        }));
    });
}

/// Makes the tap, on its thread. Without Accessibility macOS is asked for it, once (`ask`).
fn start(ask: bool) {
    if !trusted() {
        static ASKED: AtomicBool = AtomicBool::new(false);
        say("compose: needs Accessibility (System Settings → Privacy & Security → Accessibility → RustyBar)".into());
        if ask && !ASKED.swap(true, Ordering::SeqCst) {
            crate::widgets::notifications::prompt_accessibility();
        }
        return;
    }
    if STARTING.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::runtime::spawn("compose", || unsafe {
        let state = Box::into_raw(Box::new(State {
            tap: std::ptr::null_mut(),
            timer: std::ptr::null_mut(),
            engine: Engine::default(),
            rules: None,
            layout: None,
            generation: 0,
            kept: Vec::new(),
            armed: None,
            composing: false,
        }));
        let events = [KEY_DOWN, KEY_UP, FLAGS_CHANGED].iter().chain(&MOUSE_DOWN).fold(0u64, |mask, kind| mask | 1 << kind);
        // kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault: it may swallow
        let tap = CGEventTapCreate(1, 0, 0, events, callback, state.cast());
        if tap.is_null() {
            drop(Box::from_raw(state));
            STARTING.store(false, Ordering::SeqCst);
            say("compose: macOS refused the event tap".into());
            return;
        }
        (*state).tap = tap;
        let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
        CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
        CFRelease(source);
        // One timer for the keys kept, set each time there are some: repeating, however rarely,
        // as one that fires once can't be set again
        let mut context = TimerContext { version: 0, info: state.cast(), retain: std::ptr::null(), release: std::ptr::null(), describe: std::ptr::null() };
        let timer = CFRunLoopTimerCreate(std::ptr::null(), CFAbsoluteTimeGetCurrent() + NEVER, NEVER, 0, 0, tick, &mut context);
        CFRunLoopAddTimer(CFRunLoopGetCurrent(), timer, kCFRunLoopCommonModes);
        (*state).timer = timer;
        TAP.store(tap, Ordering::SeqCst);
        // The config may have changed its mind meanwhile
        let wanted = WANTED.load(Ordering::SeqCst);
        CGEventTapEnable(tap, wanted);
        let about = lock(&RULES).as_ref().map(|r| format!("{}, {} sequences", r.key.name, r.len())).unwrap_or_default();
        say(if wanted { format!("compose: on ({about})") } else { "compose: off".into() });
        CFRunLoopRun();
    });
}

/// The keyboard thread's own.
struct State {
    tap: *mut c_void,
    timer: *mut c_void,
    engine: Engine,
    rules: Option<Arc<Rules>>,
    layout: Option<Arc<Layout>>,
    generation: u64,
    /// Copies of the events the engine keeps back, oldest first.
    kept: Vec<*mut c_void>,
    /// What the timer is set for, in the time of the events.
    armed: Option<u64>,
    /// What the bars were last told: a sequence waits for its keys.
    composing: bool,
}

unsafe extern "C" fn callback(proxy: *mut c_void, kind: u32, event: *mut c_void, info: *mut c_void) -> *mut c_void {
    // A panic must not cross into CoreGraphics: the event goes on as it came
    let pass = std::panic::catch_unwind(|| unsafe { (*info.cast::<State>()).event(proxy, kind, event) }).unwrap_or(true);
    if pass { event } else { std::ptr::null_mut() }
}

unsafe extern "C" fn tick(_timer: *mut c_void, info: *mut c_void) {
    let _ = std::panic::catch_unwind(|| unsafe { (*info.cast::<State>()).tick() });
}

impl State {
    /// An event of the tap: true if it goes on.
    unsafe fn event(&mut self, proxy: *mut c_void, kind: u32, event: *mut c_void) -> bool {
        if kind == DISABLED_BY_TIMEOUT || kind == DISABLED_BY_USER {
            // macOS turned the tap off: what went by meanwhile is unknown
            unsafe { self.forget() };
            if WANTED.load(Ordering::SeqCst) {
                unsafe { CGEventTapEnable(self.tap, true) };
            }
            return true;
        }
        if unsafe { CGEventGetIntegerValueField(event, FIELD_USER_DATA) } == OURS {
            return true;
        }
        let generation = GENERATION.load(Ordering::SeqCst);
        if generation != self.generation {
            self.generation = generation;
            self.rules = lock(&RULES).clone();
            self.layout = lock(&LAYOUT).clone();
            unsafe { self.forget() };
        }
        let Some(rules) = self.rules.clone() else { return true };
        let (code, flags, stamp) = unsafe { (CGEventGetIntegerValueField(event, FIELD_CODE) as u16, CGEventGetFlags(event), CGEventGetTimestamp(event)) };
        // When the key moved, in nanoseconds since the Mac started; now, for an event made without it
        let now = if stamp != 0 { stamp } else { unsafe { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) } };
        let text;
        let input = match kind {
            FLAGS_CHANGED => Input::Modifier { code, flags },
            KEY_DOWN => {
                text = match &self.layout {
                    Some(layout) if self.engine.wants_text() => {
                        let keyboard = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYBOARD) };
                        layout.text(code, flags, if keyboard > 0 { keyboard as u32 } else { unsafe { LMGetKbdType() as u32 } })
                    }
                    _ => String::new(),
                };
                Input::Down { code, flags, repeat: unsafe { CGEventGetIntegerValueField(event, FIELD_REPEAT) } != 0, text: &text }
            }
            KEY_UP => Input::Up { code },
            _ => Input::Click,
        };
        let verdict = self.engine.handle(&rules, input, now);
        let mut pass = verdict.fate == Fate::Pass;
        unsafe {
            if verdict.fate == Fate::Keep {
                let copy = CGEventCreateCopy(event);
                if !copy.is_null() {
                    self.kept.push(copy);
                }
            }
            // A key that goes on after those kept is posted after them, to arrive in that order.
            // Never a modifier: one that doesn't pass the tap as itself stays down for macOS
            let after = verdict.kept == Kept::Replay && pass && (kind == KEY_DOWN || kind == KEY_UP);
            self.act(&verdict, proxy);
            if after {
                CGEventTapPostEvent(proxy, event);
                pass = false;
            }
            self.set_timer(&rules, now);
        }
        self.tell();
        pass
    }

    /// The time of a tap has passed with keys kept, or a sequence was left there.
    unsafe fn tick(&mut self) {
        let Some(deadline) = self.armed.take() else { return };
        let Some(rules) = self.rules.clone() else { return };
        let verdict = self.engine.handle(&rules, Input::Tick, deadline);
        unsafe {
            self.act(&verdict, std::ptr::null_mut());
            self.set_timer(&rules, deadline);
        }
        self.tell();
    }

    /// Tells the bars when a sequence starts waiting for its keys, and when it stops. One that
    /// starts and ends with the same event, its keys typed before the compose key came up, never
    /// waited.
    fn tell(&mut self) {
        let composing = self.engine.composing();
        if composing != self.composing {
            self.composing = composing;
            show_mark(composing);
        }
    }

    /// Does what a verdict says of the events kept and of the text; `proxy` is the tap's inside
    /// its callback, null outside.
    unsafe fn act(&mut self, verdict: &Verdict, proxy: *mut c_void) {
        for event in self.kept.drain(..if verdict.kept == Kept::Wait { 0 } else { self.kept.len() }) {
            unsafe {
                if verdict.kept == Kept::Replay {
                    post(proxy, event);
                }
                CFRelease(event);
            }
        }
        if let Some(text) = &verdict.text {
            unsafe { type_text(proxy, text) };
        }
        if let Some(press) = verdict.press {
            let mut presses = lock(&PRESSES);
            if presses.len() == REMEMBERED {
                presses.pop_front();
            }
            presses.push_back((Instant::now(), press));
        }
    }

    /// Sets the timer for when the engine wants a `Tick`, or for never.
    unsafe fn set_timer(&mut self, rules: &Rules, now: u64) {
        let deadline = self.engine.deadline(rules);
        if deadline == self.armed {
            return;
        }
        self.armed = deadline;
        let wait = deadline.map(|d| d.saturating_sub(now) as f64 / 1e9).unwrap_or(NEVER);
        unsafe { CFRunLoopTimerSetNextFireDate(self.timer, CFAbsoluteTimeGetCurrent() + wait) };
    }

    /// Starts again from nothing; the keys kept go on as they came, not to be lost.
    unsafe fn forget(&mut self) {
        self.engine.reset();
        for event in self.kept.drain(..) {
            unsafe {
                post(std::ptr::null_mut(), event);
                CFRelease(event);
            }
        }
        if self.armed.take().is_some() && !self.timer.is_null() {
            unsafe { CFRunLoopTimerSetNextFireDate(self.timer, CFAbsoluteTimeGetCurrent() + NEVER) };
        }
        self.tell();
    }
}

/// Sends `event` on: after the tap from inside its callback (`proxy`), where it isn't seen again;
/// from outside through the session, marked so that the tap lets it by.
unsafe fn post(proxy: *mut c_void, event: *mut c_void) {
    unsafe {
        if proxy.is_null() {
            CGEventSetIntegerValueField(event, FIELD_USER_DATA, OURS);
            // kCGSessionEventTap
            CGEventPost(1, event);
        } else {
            CGEventTapPostEvent(proxy, event);
        }
    }
}

/// Types `text`: key events that carry it, twenty UTF-16 units each at most.
unsafe fn type_text(proxy: *mut c_void, text: &str) {
    let mut units: Vec<u16> = Vec::new();
    let flush = |units: &mut Vec<u16>| {
        for down in [true, false] {
            unsafe {
                let event = CGEventCreateKeyboardEvent(std::ptr::null(), 0, down);
                if event.is_null() {
                    return;
                }
                // Not the Shift of an È, nor the ⌘ of the compose key, still down
                CGEventSetFlags(event, 0);
                CGEventKeyboardSetUnicodeString(event, units.len(), units.as_ptr());
                post(proxy, event);
                CFRelease(event);
            }
        }
        units.clear();
    };
    for c in text.chars() {
        let mut buffer = [0u16; 2];
        let encoded = c.encode_utf16(&mut buffer);
        if units.len() + encoded.len() > 16 {
            flush(&mut units);
        }
        units.extend_from_slice(encoded);
    }
    if !units.is_empty() {
        flush(&mut units);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;
    /// The right ⌘ down, by itself, as a keyboard says it.
    const RCMD: u64 = COMMAND | 0x10 | 0x100;
    const NONE: u64 = 0x100;
    const LSHIFT: u64 = SHIFT | 0x02;

    fn rules(extra: &[(&str, &str)]) -> Rules {
        let mut config = ComposeConfig::default();
        for (k, v) in extra {
            config.sequences.insert(k.to_string(), v.to_string());
        }
        Rules::new(&config)
    }

    /// An engine and its clock.
    struct Typist {
        engine: Engine,
        rules: Rules,
        now: u64,
    }

    impl Typist {
        fn new(extra: &[(&str, &str)]) -> Typist {
            Typist { engine: Engine::default(), rules: rules(extra), now: 1_000 * MS }
        }

        fn send(&mut self, input: Input) -> Verdict {
            self.now += 10 * MS;
            self.engine.handle(&self.rules, input, self.now)
        }

        fn down(&mut self, code: u16, flags: u64, text: &str) -> Verdict {
            self.send(Input::Down { code, flags, repeat: false, text })
        }

        fn up(&mut self, code: u16) -> Fate {
            self.send(Input::Up { code }).fate
        }

        /// The compose key down, or up.
        fn key_down(&mut self) {
            assert_eq!(self.send(Input::Modifier { code: 0x36, flags: RCMD }), Verdict::default());
        }

        fn key_up(&mut self) -> Verdict {
            let verdict = self.send(Input::Modifier { code: 0x36, flags: NONE });
            assert_eq!(verdict.fate, Fate::Pass, "the compose key always goes on");
            verdict
        }

        /// The compose key down and, `held` ms later, up, with nothing in between.
        fn compose(&mut self, held: u64) -> Outcome {
            self.key_down();
            self.now += held * MS;
            let verdict = self.key_up();
            assert_eq!((verdict.kept, &verdict.text), (Kept::Wait, &None));
            verdict.press.expect("a press").outcome
        }

        /// A key typing `text` down, then up, after the compose key: what it types in its place
        /// (`Some("")`: swallowed, nothing yet), or None if it goes on.
        fn key(&mut self, code: u16, text: &str) -> Option<String> {
            let verdict = self.down(code, NONE, text);
            assert_eq!(verdict.kept, Kept::Wait);
            let up = self.up(code);
            match verdict.fate {
                Fate::Pass => {
                    assert_eq!((up, &verdict.text), (Fate::Pass, &None));
                    None
                }
                fate => {
                    assert_eq!((fate, up), (Fate::Swallow, Fate::Swallow), "a key swallowed is swallowed going up too");
                    Some(verdict.text.unwrap_or_default())
                }
            }
        }
    }

    fn types(text: &str) -> Option<String> {
        Some(text.to_string())
    }

    #[test]
    fn accents() {
        let mut t = Typist::new(&[]);
        assert_eq!(t.compose(80), Outcome::Tap);
        assert_eq!(t.key(14, "e"), types("è"));
        // Over: the next key is itself
        assert_eq!(t.key(14, "e"), None);
        t.compose(80);
        assert_eq!(t.key(14, "E"), types("È"));
        t.compose(80);
        assert_eq!(t.key(39, "'"), types(""));
        assert_eq!(t.key(14, "e"), types("é"));
        t.compose(80);
        assert_eq!(t.key(39, "'"), types(""));
        assert_eq!(t.key(32, "U"), types("Ú"));
        for (keys, text) in [("a", "à"), ("i", "ì"), ("o", "ò"), ("u", "ù"), ("A", "À"), ("I", "Ì"), ("O", "Ò"), ("U", "Ù")] {
            t.compose(60);
            assert_eq!(t.key(0, keys), types(text));
        }
    }

    #[test]
    fn typed_fast_the_letter_comes_before_the_release() {
        let mut t = Typist::new(&[]);
        // The compose key still down when `e` goes down: kept, then the sequence
        t.key_down();
        assert_eq!(t.down(14, RCMD, "e"), Verdict { fate: Fate::Keep, ..Default::default() });
        assert_eq!(t.engine.deadline(&t.rules), Some(1_010 * MS + 300 * MS));
        let up = t.key_up();
        assert_eq!((up.kept, up.text.as_deref()), (Kept::Drop, Some("è")));
        let press = up.press.unwrap();
        assert_eq!((press.outcome, press.keys, press.first, press.nested), (Outcome::Tap, 1, Some(10 * MS), false));
        assert_eq!(t.engine.deadline(&t.rules), None);
        assert_eq!(t.up(14), Fate::Swallow);
        assert_eq!(t.key(14, "e"), None);
        // The letter up before the compose key too, all within a tap
        t.key_down();
        assert_eq!(t.down(14, RCMD, "e").fate, Fate::Keep);
        assert_eq!(t.up(14), Fate::Keep);
        let up = t.key_up();
        assert_eq!((up.kept, up.text.as_deref(), up.press.unwrap().nested), (Kept::Drop, Some("è"), true));
        assert_eq!(t.key(14, "e"), None);
        // Two keys before the release
        t.key_down();
        assert_eq!(t.down(39, RCMD, "'").fate, Fate::Keep);
        assert_eq!(t.down(14, RCMD, "e").fate, Fate::Keep);
        assert_eq!(t.up(39), Fate::Keep);
        let up = t.key_up();
        assert_eq!((up.kept, up.text.as_deref()), (Kept::Drop, Some("é")));
        assert_eq!(t.up(14), Fate::Swallow);
        // The first of a longer one, the rest after the release
        t.key_down();
        assert_eq!(t.down(39, RCMD, "'").fate, Fate::Keep);
        let up = t.key_up();
        assert_eq!((up.kept, up.text), (Kept::Drop, None));
        assert_eq!(t.up(39), Fate::Swallow);
        assert_eq!(t.key(14, "e"), types("é"));
        // A capital: Shift goes down while the compose key still is, or was down before it
        t.key_down();
        assert_eq!(t.send(Input::Modifier { code: 0x38, flags: RCMD | LSHIFT }), Verdict::default());
        assert_eq!(t.down(14, RCMD | LSHIFT, "E").fate, Fate::Keep);
        let up = t.send(Input::Modifier { code: 0x36, flags: LSHIFT | 0x100 });
        assert_eq!((up.kept, up.text.as_deref()), (Kept::Drop, Some("È")));
        t.up(14);
        t.send(Input::Modifier { code: 0x38, flags: NONE });
        t.send(Input::Modifier { code: 0x38, flags: LSHIFT | 0x100 });
        t.send(Input::Modifier { code: 0x36, flags: RCMD | LSHIFT });
        assert_eq!(t.send(Input::Modifier { code: 0x36, flags: LSHIFT | 0x100 }).press.unwrap().outcome, Outcome::Tap);
        assert_eq!(t.key(14, "E"), types("È"));
    }

    #[test]
    fn held_is_a_modifier() {
        let mut t = Typist::new(&[]);
        // Too long for a tap
        assert_eq!(t.compose(400), Outcome::Long);
        assert_eq!(t.key(14, "e"), None);
        // A key that begins no sequence is a shortcut at once
        t.key_down();
        assert_eq!(t.down(8, RCMD, "c"), Verdict::default());
        assert_eq!(t.up(8), Fate::Pass);
        assert_eq!(t.key_up().press.unwrap().outcome, Outcome::Chord);
        assert_eq!(t.key(14, "e"), None);
        // One that could begin a sequence waits: the time of a tap passes, and it was a shortcut
        t.key_down();
        assert_eq!(t.down(0, RCMD, "a").fate, Fate::Keep);
        let deadline = t.engine.deadline(&t.rules).unwrap();
        assert_eq!(t.engine.handle(&t.rules, Input::Tick, deadline), Verdict { kept: Kept::Replay, ..Default::default() });
        assert_eq!(t.engine.deadline(&t.rules), None);
        t.now = deadline + 50 * MS;
        assert_eq!(t.up(0), Fate::Pass);
        let up = t.key_up();
        assert_eq!((up.kept, up.text, up.press.unwrap().outcome), (Kept::Wait, None, Outcome::Chord));
        assert_eq!(t.key(14, "e"), None);
        // The same when the next event comes before the timer does
        t.key_down();
        assert_eq!(t.down(0, RCMD, "a").fate, Fate::Keep);
        t.now += 400 * MS;
        assert_eq!(t.send(Input::Up { code: 0 }), Verdict { kept: Kept::Replay, ..Default::default() });
        assert_eq!(t.key_up().press.unwrap().outcome, Outcome::Chord);
        // Down for a while, then a key: a shortcut, with no wait
        t.key_down();
        t.now += 400 * MS;
        assert_eq!(t.down(0, RCMD, "a"), Verdict::default());
        t.up(0);
        t.key_up();
        assert_eq!(t.key(14, "e"), None);
        // A key that types nothing after one kept: both go on, in their order
        t.key_down();
        assert_eq!(t.down(0, RCMD, "a").fate, Fate::Keep);
        assert_eq!(t.down(36, RCMD, "\r"), Verdict { kept: Kept::Replay, ..Default::default() });
        t.up(0);
        t.up(36);
        t.key_up();
        // A click
        t.key_down();
        assert_eq!(t.down(0, RCMD, "a").fate, Fate::Keep);
        assert_eq!(t.send(Input::Click), Verdict { kept: Kept::Replay, ..Default::default() });
        assert_eq!(t.key_up().press.unwrap().outcome, Outcome::Chord);
        assert_eq!(t.key(14, "e"), None);
        // With ⌥, going down after it or down already
        t.key_down();
        t.send(Input::Modifier { code: 0x3A, flags: RCMD | ALT | 0x20 });
        t.send(Input::Modifier { code: 0x3A, flags: RCMD });
        assert_eq!(t.key_up().press.unwrap().outcome, Outcome::Mixed);
        assert_eq!(t.key(14, "e"), None);
        t.send(Input::Modifier { code: 0x3A, flags: ALT | 0x20 | 0x100 });
        t.send(Input::Modifier { code: 0x36, flags: RCMD | ALT | 0x20 });
        assert_eq!(t.send(Input::Modifier { code: 0x36, flags: ALT | 0x20 | 0x100 }).press.unwrap().outcome, Outcome::Mixed);
        t.send(Input::Modifier { code: 0x3A, flags: NONE });
        assert_eq!(t.key(14, "e"), None);
        // The other ⌘ is not the compose key
        t.send(Input::Modifier { code: 0x37, flags: COMMAND | 0x08 | 0x100 });
        assert_eq!(t.send(Input::Modifier { code: 0x37, flags: NONE }).press, None);
        assert_eq!(t.key(14, "e"), None);
    }

    #[test]
    fn no_such_sequence_is_typed_as_it_is() {
        let mut t = Typist::new(&[]);
        t.compose(80);
        assert_eq!(t.key(4, "h"), types("h"));
        assert_eq!(t.key(14, "e"), None);
        t.compose(80);
        assert_eq!(t.key(39, "'"), types(""));
        assert_eq!(t.key(7, "x"), types("'x"));
        // Kept before the release, too
        t.key_down();
        assert_eq!(t.down(1, RCMD, "s").fate, Fate::Keep);
        assert_eq!(t.down(2, RCMD, "d").fate, Fate::Keep);
        assert_eq!(t.key_up().text.as_deref(), Some("sd"));
        // And what follows the end of a sequence is itself
        t.key_down();
        assert_eq!(t.down(14, RCMD, "e").fate, Fate::Keep);
        assert_eq!(t.down(7, RCMD, "x").fate, Fate::Keep);
        assert_eq!(t.key_up().text.as_deref(), Some("èx"));
    }

    #[test]
    fn ways_out() {
        let mut t = Typist::new(&[]);
        // Esc drops it, and isn't seen
        t.compose(80);
        assert_eq!(t.key(39, "'"), types(""));
        assert_eq!(t.key(ESC, "\u{1b}"), types(""));
        assert_eq!(t.key(14, "e"), None);
        // Backspace takes the last key back, then drops it
        t.compose(80);
        assert_eq!(t.key(39, "\""), types(""));
        assert_eq!(t.key(BACKSPACE, "\u{8}"), types(""));
        assert_eq!(t.key(39, "'"), types(""));
        assert_eq!(t.key(14, "e"), types("é"));
        t.compose(80);
        assert_eq!(t.key(BACKSPACE, "\u{8}"), types(""));
        assert_eq!(t.key(14, "e"), None);
        // A second tap, a click, Return, a shortcut: dropped, and they go on
        t.compose(80);
        t.compose(80);
        assert_eq!(t.key(14, "e"), None);
        t.compose(80);
        assert_eq!(t.send(Input::Click), Verdict::default());
        assert_eq!(t.key(14, "e"), None);
        t.compose(80);
        assert_eq!(t.key(39, "'"), types(""));
        assert_eq!(t.key(36, "\r"), None);
        assert_eq!(t.key(14, "e"), None);
        t.compose(80);
        assert_eq!(t.down(9, COMMAND | 0x08 | 0x100, "v"), Verdict::default());
        t.up(9);
        assert_eq!(t.key(14, "e"), None);
        // Left there past the timeout
        t.compose(80);
        assert_eq!(t.key(39, "'"), types(""));
        t.now += 6_000 * MS;
        assert_eq!(t.key(14, "e"), None);
        // The same with no key after it: the timer, set from the last key
        assert_eq!(t.engine.deadline(&t.rules), None);
        t.compose(80);
        assert_eq!(t.engine.deadline(&t.rules), Some(t.now + 5_000 * MS));
        assert_eq!(t.key(39, "'"), types(""));
        let deadline = t.engine.deadline(&t.rules).unwrap();
        assert_eq!(deadline, t.now - 10 * MS + 5_000 * MS);
        assert_eq!(t.engine.handle(&t.rules, Input::Tick, deadline - MS), Verdict::default());
        assert!(t.engine.composing());
        assert_eq!(t.engine.handle(&t.rules, Input::Tick, deadline), Verdict::default());
        assert!(!t.engine.composing());
        assert_eq!(t.engine.deadline(&t.rules), None);
    }

    #[test]
    fn a_key_held_down_repeats_nothing() {
        let mut t = Typist::new(&[]);
        t.compose(80);
        assert_eq!(t.down(14, NONE, "e").text.as_deref(), Some("è"));
        assert_eq!(t.send(Input::Down { code: 14, flags: NONE, repeat: true, text: "" }).fate, Fate::Swallow);
        assert_eq!(t.up(14), Fate::Swallow);
        assert_eq!(t.down(14, NONE, ""), Verdict::default());
        assert_eq!(t.up(14), Fate::Pass);
        // Nor does one kept, nor does one down from before change what the compose key is
        t.key_down();
        assert_eq!(t.send(Input::Down { code: 4, flags: RCMD, repeat: true, text: "h" }), Verdict::default());
        assert_eq!(t.down(14, RCMD, "e").fate, Fate::Keep);
        assert_eq!(t.send(Input::Down { code: 14, flags: RCMD, repeat: true, text: "e" }).fate, Fate::Swallow);
        assert_eq!(t.key_up().text.as_deref(), Some("è"));
    }

    #[test]
    fn the_file_adds_changes_and_removes() {
        let mut t = Typist::new(&[("->", "→"), ("e", "ę"), ("'e", "")]);
        t.compose(80);
        assert_eq!(t.key(27, "-"), types(""));
        assert_eq!(t.key(47, ">"), types("→"));
        t.compose(80);
        assert_eq!(t.key(14, "e"), types("ę"));
        // Removed: no sequence any more
        t.compose(80);
        assert_eq!(t.key(39, "'"), types(""));
        assert_eq!(t.key(14, "e"), types("'e"));
    }

    #[test]
    fn a_keyboard_that_does_not_say_which() {
        let mut t = Typist::new(&[]);
        t.send(Input::Modifier { code: 0x36, flags: COMMAND });
        t.send(Input::Modifier { code: 0x36, flags: 0 });
        assert_eq!(t.key(14, "e"), types("è"));
    }

    #[test]
    fn listings() {
        let custom = vec![("->".to_string(), "→".to_string()), ("e".to_string(), "ę".to_string()), ("'e".to_string(), String::new())];
        let list = listing(&custom);
        assert_eq!(list[0], Entry { keys: "->".into(), text: "→".into(), default: None });
        assert_eq!(list.len(), DEFAULTS.len() + 1);
        assert!(list.contains(&Entry { keys: "e".into(), text: "ę".into(), default: Some("è") }));
        assert!(list.contains(&Entry { keys: "'e".into(), text: "".into(), default: Some("é") }));
        // No default hides another
        let keys: Vec<&str> = DEFAULTS.iter().map(|(k, _)| *k).collect();
        for k in &keys {
            assert_eq!(shadow(k, keys.iter().copied()), None, "{k}");
        }
        assert_eq!(shadow("e=", keys.iter().copied()), Some("e"));
        assert_eq!(keys.len(), keys.iter().collect::<HashSet<_>>().len());
    }

    #[test]
    fn keys_by_name() {
        assert_eq!(key("right_cmd").map(|k| k.code), Some(0x36));
        assert_eq!(key("Right-Option").map(|k| k.name), Some("right_alt"));
        assert_eq!(key("left_control").map(|k| k.name), Some("left_ctrl"));
        assert!(key("fn").is_none());
        let mut config = ComposeConfig::default();
        assert!(check(&config).is_empty());
        config.key = "hyper".into();
        assert_eq!(check(&config).len(), 1);
    }
}
