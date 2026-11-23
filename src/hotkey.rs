//! Global shortcuts, with Carbon's hot keys: macOS tells the app when its combination is pressed,
//! whatever app is in front, on the main thread. They ask for no permission, unlike reading the
//! keyboard (Input Monitoring), as only that combination ever reaches the app.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::Rc;

type OsStatus = i32;

#[repr(C)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

type Handler = extern "C" fn(call: *mut c_void, event: *mut c_void, data: *mut c_void) -> OsStatus;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(target: *mut c_void, handler: Handler, count: u32, types: *const EventTypeSpec, data: *mut c_void, out: *mut *mut c_void) -> OsStatus;
    fn RegisterEventHotKey(code: u32, modifiers: u32, id: EventHotKeyId, target: *mut c_void, options: u32, out: *mut *mut c_void) -> OsStatus;
    fn UnregisterEventHotKey(key: *mut c_void) -> OsStatus;
    fn GetEventParameter(event: *mut c_void, name: u32, kind: u32, out_kind: *mut u32, size: usize, out_size: *mut usize, data: *mut c_void) -> OsStatus;
}

const fn code(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}

/// `kEventClassKeyboard`, `kEventHotKeyPressed`, `kEventParamDirectObject`, `typeEventHotKeyID`.
const KEYBOARD: u32 = code(b"keyb");
const PRESSED: u32 = 5;
const DIRECT: u32 = code(b"----");
const HOT_KEY_ID: u32 = code(b"hkid");
/// Ours, in the ids we give macOS.
const SIGNATURE: u32 = code(b"RsBr");

/// `cmdKey`, `shiftKey`, `optionKey`, `controlKey`.
const COMMAND: u32 = 1 << 8;
const SHIFT: u32 = 1 << 9;
const OPTION: u32 = 1 << 11;
const CONTROL: u32 = 1 << 12;

/// The hot keys registered, on the main thread.
#[derive(Default)]
struct Registry {
    /// What each does, by its id.
    actions: HashMap<u32, Rc<dyn Fn()>>,
    /// The last id given.
    last: u32,
    /// The handler is installed.
    handler: bool,
}

thread_local! {
    static KEYS: RefCell<Registry> = RefCell::new(Registry::default());
}

extern "C" fn pressed(_call: *mut c_void, event: *mut c_void, _data: *mut c_void) -> OsStatus {
    let mut id = EventHotKeyId::default();
    let status = unsafe {
        GetEventParameter(event, DIRECT, HOT_KEY_ID, std::ptr::null_mut(), size_of::<EventHotKeyId>(), std::ptr::null_mut(), (&mut id as *mut EventHotKeyId).cast())
    };
    if status != 0 || id.signature != SIGNATURE {
        // eventNotHandledErr: someone else's
        return -9874;
    }
    // Out of the borrow, as what it does may register or drop a key
    let f = KEYS.with(|k| k.borrow().actions.get(&id.id).cloned());
    if let Some(f) = f {
        f();
    }
    0
}

/// A combination such as `ctrl-alt-f`, as AeroSpace writes them: modifiers (`cmd`, `ctrl`, `alt`,
/// `shift`) and a key, by its place on a US keyboard (letters, digits, `f1`…`f20`, `space`,
/// arrows…), with `-` between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combo {
    code: u32,
    modifiers: u32,
    /// What the key shows, for `label`.
    name: &'static str,
}

impl Combo {
    pub fn parse(text: &str) -> Result<Combo, String> {
        let text = text.trim().to_lowercase();
        let parts: Vec<&str> = text.split('-').collect();
        let Some((&key, mods)) = parts.split_last() else { return Err("empty".into()) };
        let mut modifiers = 0;
        for m in mods {
            modifiers |= match *m {
                "cmd" | "command" => COMMAND,
                "ctrl" | "control" => CONTROL,
                "alt" | "opt" | "option" => OPTION,
                "shift" => SHIFT,
                other => return Err(format!("unknown modifier {other:?} in {text:?}")),
            };
        }
        let Some(&(name, code)) = KEYS_BY_NAME.iter().find(|(n, _)| *n == key) else { return Err(format!("unknown key {key:?} in {text:?}")) };
        Ok(Combo { code, modifiers, name })
    }

    /// As macOS menus write it: ⌃⌥⇧⌘ and the key.
    pub fn label(&self) -> String {
        let mut s = String::new();
        for (m, sign) in [(CONTROL, "⌃"), (OPTION, "⌥"), (SHIFT, "⇧"), (COMMAND, "⌘")] {
            if self.modifiers & m != 0 {
                s.push_str(sign);
            }
        }
        let key = match self.name {
            "space" => "Space".to_string(),
            "left" => "←".into(),
            "right" => "→".into(),
            "up" => "↑".into(),
            "down" => "↓".into(),
            "enter" => "↩".into(),
            "tab" => "⇥".into(),
            "esc" => "⎋".into(),
            "backspace" => "⌫".into(),
            n => n.to_uppercase(),
        };
        s + &key
    }
}

/// A hot key, registered while it lives.
pub struct HotKey {
    id: u32,
    key: *mut c_void,
}

/// Calls `f` on the main thread whenever `combo` is pressed, until the result is dropped. An
/// error when another app has the combination already.
pub fn register(combo: Combo, f: impl Fn() + 'static) -> Result<HotKey, String> {
    let id = KEYS.with(|k| {
        let mut k = k.borrow_mut();
        if !k.handler {
            let spec = EventTypeSpec { class: KEYBOARD, kind: PRESSED };
            let status = unsafe { InstallEventHandler(GetApplicationEventTarget(), pressed, 1, &spec, std::ptr::null_mut(), std::ptr::null_mut()) };
            if status != 0 {
                return Err(format!("InstallEventHandler: {status}"));
            }
            k.handler = true;
        }
        k.last += 1;
        Ok(k.last)
    })?;
    let mut key = std::ptr::null_mut();
    let status = unsafe { RegisterEventHotKey(combo.code, combo.modifiers, EventHotKeyId { signature: SIGNATURE, id }, GetApplicationEventTarget(), 0, &mut key) };
    if status != 0 {
        // eventHotKeyExistsErr
        return Err(if status == -9878 { format!("{} is taken by another app", combo.label()) } else { format!("RegisterEventHotKey: {status}") });
    }
    KEYS.with(|k| k.borrow_mut().actions.insert(id, Rc::new(f)));
    Ok(HotKey { id, key })
}

impl Drop for HotKey {
    fn drop(&mut self) {
        unsafe { UnregisterEventHotKey(self.key) };
        // Also when the thread locals are already going, after exit
        let _ = KEYS.try_with(|k| k.borrow_mut().actions.remove(&self.id));
    }
}

/// The virtual key codes of `Events.h` (`kVK_…`), which name a key by its place on a US
/// keyboard.
const KEYS_BY_NAME: &[(&str, u32)] = &[
    ("a", 0x00), ("s", 0x01), ("d", 0x02), ("f", 0x03), ("h", 0x04), ("g", 0x05), ("z", 0x06), ("x", 0x07), ("c", 0x08), ("v", 0x09),
    ("b", 0x0B), ("q", 0x0C), ("w", 0x0D), ("e", 0x0E), ("r", 0x0F), ("y", 0x10), ("t", 0x11), ("1", 0x12), ("2", 0x13), ("3", 0x14),
    ("4", 0x15), ("6", 0x16), ("5", 0x17), ("equal", 0x18), ("9", 0x19), ("7", 0x1A), ("minus", 0x1B), ("8", 0x1C), ("0", 0x1D),
    ("rightsquarebracket", 0x1E), ("o", 0x1F), ("u", 0x20), ("leftsquarebracket", 0x21), ("i", 0x22), ("p", 0x23), ("enter", 0x24),
    ("l", 0x25), ("j", 0x26), ("quote", 0x27), ("k", 0x28), ("semicolon", 0x29), ("backslash", 0x2A), ("comma", 0x2B), ("slash", 0x2C),
    ("n", 0x2D), ("m", 0x2E), ("period", 0x2F), ("tab", 0x30), ("space", 0x31), ("backtick", 0x32), ("backspace", 0x33), ("esc", 0x35),
    ("f1", 0x7A), ("f2", 0x78), ("f3", 0x63), ("f4", 0x76), ("f5", 0x60), ("f6", 0x61), ("f7", 0x62), ("f8", 0x64), ("f9", 0x65),
    ("f10", 0x6D), ("f11", 0x67), ("f12", 0x6F), ("f13", 0x69), ("f14", 0x6B), ("f15", 0x71), ("f16", 0x6A), ("f17", 0x40),
    ("f18", 0x4F), ("f19", 0x50), ("f20", 0x5A), ("left", 0x7B), ("right", 0x7C), ("down", 0x7D), ("up", 0x7E),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combos() {
        let c = Combo::parse("ctrl-alt-f").unwrap();
        assert_eq!((c.code, c.modifiers), (0x03, CONTROL | OPTION));
        assert_eq!(c.label(), "⌃⌥F");
        assert_eq!(Combo::parse("Cmd-Shift-F12").unwrap().label(), "⇧⌘F12");
        assert_eq!(Combo::parse("alt-space").unwrap().label(), "⌥Space");
        assert!(Combo::parse("hyper-f").is_err());
        assert!(Combo::parse("ctrl-ü").is_err());
        assert!(Combo::parse("").is_err());
    }
}
