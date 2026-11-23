//! The dictation key (the 🎤 on F5 of Apple's keyboards since 2021) as a key of RustyBar's, while
//! taken. Without Input Monitoring or Accessibility the only keys an app can catch are Carbon's
//! hot keys (see `hotkey`), and the dictation key isn't one: it's a consumer control (usage 0xCF
//! of page 0x0C), which macOS turns into dictation itself. So while taken, every keyboard maps it
//! to a function key nothing else has (F19, else F18…), as `hidutil property --set
//! '{"UserKeyMapping": …}'` would, and a hot key catches that one: the key reaches RustyBar alone,
//! and dictation doesn't start. Given back, the keyboards map it as before.
//!
//! The mapping is a property of each keyboard's service in the HID event system
//! (`UserKeyMapping`), which any process may set without a permission and which lasts until a
//! restart. What else is in it stays (Raycast's Hyper Key maps Caps Lock there), and a keyboard
//! that maps the dictation key already, to anything else, is left alone. A keyboard that comes
//! while taken gets it too, a second after IOKit says so. While taken it's in the defaults, so that
//! a bar that crashed gives the key back at its next start (`tidy`).

use std::cell::RefCell;
use std::ffi::{c_char, c_void};
use std::rc::Rc;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUserDefaults, ns_string};

use crate::hotkey::{self, Combo, HotKey};
use crate::iokit::{self, CFTypeRef, Dict, cf, owned};

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDEventSystemClientCreateSimpleClient(allocator: CFTypeRef) -> CFTypeRef;
    fn IOHIDEventSystemClientCopyServices(client: CFTypeRef) -> CFTypeRef;
    fn IOHIDServiceClientConformsTo(service: CFTypeRef, page: u32, usage: u32) -> u32;
    fn IOHIDServiceClientCopyProperty(service: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
    fn IOHIDServiceClientSetProperty(service: CFTypeRef, key: CFTypeRef, value: CFTypeRef) -> u8;
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IONotificationPortCreate(main_port: u32) -> *mut c_void;
    fn IONotificationPortDestroy(port: *mut c_void);
    fn IONotificationPortGetRunLoopSource(port: *mut c_void) -> CFTypeRef;
    fn IOServiceAddMatchingNotification(port: *mut c_void, kind: *const c_char, matching: *mut c_void, callback: extern "C" fn(*mut c_void, u32), refcon: *mut c_void, iterator: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
}

unsafe extern "C" {
    fn CFRunLoopGetMain() -> CFTypeRef;
    fn CFRunLoopAddSource(rl: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
    fn CFRunLoopRemoveSource(rl: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
    static kCFRunLoopDefaultMode: CFTypeRef;
}

/// Usages as `UserKeyMapping` writes them: the page, then the usage in the low 32 bits.
const DICTATION: u64 = 0x0C_0000_00CF;
const KEYBOARD_PAGE: u64 = 0x07_0000_0000;

/// The keys it may become, by their name for `hotkey` and their usage: function keys that no
/// Mac has any more (F13–F15 change the brightness, F20 is Raycast's Hyper Key), the first a hot
/// key can have.
const TARGETS: [(&str, u64); 4] = [("f19", 0x6E), ("f18", 0x6D), ("f17", 0x6C), ("f16", 0x6B)];

const MAPPING: &str = "UserKeyMapping";
const SRC: &str = "HIDKeyboardModifierMappingSrc";
const DST: &str = "HIDKeyboardModifierMappingDst";
/// In the defaults while taken: the usage it became.
const TAKEN: &str = "dictation.taken";

/// The key the dictation key is now, while taken.
struct State {
    target: u64,
    /// IOKit's notifications of new keyboards.
    port: *mut c_void,
    iterator: u32,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// The dictation key, RustyBar's until dropped.
pub struct Taken {
    _key: HotKey,
}

/// Takes the dictation key: `pressed` runs on the main thread at each press (not while it's
/// held). An error if every key it could become is another app's hot key, or it's taken already.
pub fn take(pressed: impl Fn() + 'static) -> Result<Taken, String> {
    // Never the Mac's keyboards from the tests
    if cfg!(test) {
        return Err("not in the tests".into());
    }
    if STATE.with(|s| s.borrow().is_some()) {
        return Err("taken already".into());
    }
    let pressed = Rc::new(pressed);
    let mut error = String::new();
    for (name, usage) in TARGETS {
        let f = pressed.clone();
        let key = match hotkey::register(Combo::parse(name).expect("a key hotkey knows"), move || f()) {
            Ok(key) => key,
            Err(e) => {
                error = e;
                continue;
            }
        };
        let target = KEYBOARD_PAGE | usage;
        let (port, iterator) = watch();
        STATE.with(|s| *s.borrow_mut() = Some(State { target, port, iterator }));
        mark(Some(target));
        let keyboards = apply(Some(target));
        crate::log(&format!("keymap: the dictation key is {name} on {keyboards} keyboards"));
        return Ok(Taken { _key: key });
    }
    Err(error)
}

impl Drop for Taken {
    fn drop(&mut self) {
        // Also after exit, when the thread locals may be going (see `App::stop`)
        if let Ok(Some(state)) = STATE.try_with(|s| s.borrow_mut().take()) {
            unsafe {
                IOObjectRelease(state.iterator);
                CFRunLoopRemoveSource(CFRunLoopGetMain(), IONotificationPortGetRunLoopSource(state.port), kCFRunLoopDefaultMode);
                IONotificationPortDestroy(state.port);
            }
        }
        apply(None);
        mark(None);
    }
}

/// At the start: gives the key back if a bar took it and didn't (it crashed, or was killed).
pub fn tidy() {
    if NSUserDefaults::standardUserDefaults().objectForKey(ns_string!(TAKEN)).is_some() {
        let keyboards = apply(None);
        mark(None);
        crate::log(&format!("keymap: the dictation key given back to {keyboards} keyboards, after a bar that kept it"));
    }
}

fn mark(target: Option<u64>) {
    let d = NSUserDefaults::standardUserDefaults();
    match target {
        Some(t) => d.setInteger_forKey(t as isize, ns_string!(TAKEN)),
        None => d.removeObjectForKey(ns_string!(TAKEN)),
    }
}

/// Notifications of every new HID event service (keyboards, but also mice and the rest), on the
/// main run loop: `arrived` maps it again on all of them, a second after.
fn watch() -> (*mut c_void, u32) {
    unsafe {
        let port = IONotificationPortCreate(0);
        if port.is_null() {
            return (port, 0);
        }
        CFRunLoopAddSource(CFRunLoopGetMain(), IONotificationPortGetRunLoopSource(port), kCFRunLoopDefaultMode);
        let mut iterator = 0;
        // The matching dictionary is consumed; what's there already is mapped by `take`
        if IOServiceAddMatchingNotification(port, c"IOServiceFirstMatch".as_ptr(), IOServiceMatching(c"IOHIDEventService".as_ptr()), arrived, std::ptr::null_mut(), &mut iterator) == 0 {
            drain(iterator);
        }
        (port, iterator)
    }
}

/// Empties an iterator of notifications, which arms it again: true if something came.
fn drain(iterator: u32) -> bool {
    let mut any = false;
    loop {
        let o = unsafe { IOIteratorNext(iterator) };
        if o == 0 {
            return any;
        }
        unsafe { IOObjectRelease(o) };
        any = true;
    }
}

extern "C" fn arrived(_refcon: *mut c_void, iterator: u32) {
    if drain(iterator) {
        // When the HID event system has it as a service too
        crate::runtime::after(1.0, |_| {
            if let Some(target) = STATE.with(|s| s.borrow().as_ref().map(|s| s.target)) {
                apply(Some(target));
            }
        });
    }
}

/// Maps the dictation key to `target` on every keyboard that doesn't map it already, or with none
/// takes RustyBar's mapping of it away from every keyboard: how many keyboards have it then, or
/// how many gave it back.
fn apply(target: Option<u64>) -> usize {
    let key = NSString::from_str(MAPPING);
    let mut count = 0;
    // The services are the client's: it lives as long as they're used
    let (_client, keyboards) = keyboards();
    for service in keyboards {
        let now = unsafe { owned::<NSArray<Dict>>(IOHIDServiceClientCopyProperty(cf(&*service), cf(&*key))) };
        let pairs: Vec<(u64, u64)> = now.iter().flat_map(|a| a.iter()).filter_map(|d| pair(&d)).collect();
        let next = match target {
            Some(t) => with_ours(&pairs, t),
            None => without_ours(&pairs),
        };
        let Some(next) = next else {
            count += target.is_some_and(|t| pairs.contains(&(DICTATION, t))) as usize;
            continue;
        };
        let value = NSArray::from_retained_slice(&next.iter().map(|&(src, dst)| entry(src, dst)).collect::<Vec<_>>());
        if unsafe { IOHIDServiceClientSetProperty(cf(&*service), cf(&*key), cf(&*value)) } != 0 {
            count += 1;
        } else {
            crate::log("keymap: a keyboard didn't take its new mapping");
        }
    }
    count
}

/// The services of the HID event system that may send the dictation key: keyboards, and consumer
/// controls (the media keys of keyboards that have them apart). With the client they're of, which
/// must outlive them.
fn keyboards() -> (Option<Retained<AnyObject>>, Vec<Retained<AnyObject>>) {
    unsafe {
        let Some(client) = owned::<AnyObject>(IOHIDEventSystemClientCreateSimpleClient(std::ptr::null())) else { return (None, Vec::new()) };
        let Some(services) = owned::<NSArray<AnyObject>>(IOHIDEventSystemClientCopyServices(cf(&*client))) else { return (Some(client), Vec::new()) };
        let keyboards = services.iter().filter(|s| IOHIDServiceClientConformsTo(cf(&**s), 0x01, 0x06) != 0 || IOHIDServiceClientConformsTo(cf(&**s), 0x0C, 0x01) != 0).collect();
        (Some(client), keyboards)
    }
}

fn pair(d: &Dict) -> Option<(u64, u64)> {
    Some((iokit::number(d, &NSString::from_str(SRC))? as u64, iokit::number(d, &NSString::from_str(DST))? as u64))
}

fn entry(src: u64, dst: u64) -> Retained<NSDictionary<NSString, NSNumber>> {
    let keys = [&*NSString::from_str(SRC), &*NSString::from_str(DST)];
    let values = [&*NSNumber::new_u64(src), &*NSNumber::new_u64(dst)];
    NSDictionary::from_slices(&keys, &values)
}

fn ours(&(src, dst): &(u64, u64)) -> bool {
    src == DICTATION && TARGETS.iter().any(|&(_, usage)| dst == KEYBOARD_PAGE | usage)
}

/// A keyboard's mapping with the dictation key to `target`: none when there's nothing to change,
/// or the keyboard maps it to something of someone else's.
fn with_ours(pairs: &[(u64, u64)], target: u64) -> Option<Vec<(u64, u64)>> {
    if pairs.contains(&(DICTATION, target)) || pairs.iter().any(|p| p.0 == DICTATION && !ours(p)) {
        return None;
    }
    // One of RustyBar's to another key, from a bar that crashed: this one in its place
    let mut next: Vec<(u64, u64)> = pairs.iter().copied().filter(|p| !ours(p)).collect();
    next.push((DICTATION, target));
    Some(next)
}

/// A keyboard's mapping without RustyBar's: none when it has none.
fn without_ours(pairs: &[(u64, u64)]) -> Option<Vec<(u64, u64)>> {
    pairs.iter().any(ours).then(|| pairs.iter().copied().filter(|p| !ours(p)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPS_TO_F20: (u64, u64) = (0x07_0000_0039, 0x07_0000_006F);
    const F19: u64 = KEYBOARD_PAGE | 0x6E;
    const F18: u64 = KEYBOARD_PAGE | 0x6D;

    #[test]
    fn the_dictation_key_joins_what_a_keyboard_maps_and_leaves_alone() {
        assert_eq!(with_ours(&[], F19), Some(vec![(DICTATION, F19)]));
        // Raycast's stays, before and after
        let taken = with_ours(&[CAPS_TO_F20], F19).unwrap();
        assert_eq!(taken, vec![CAPS_TO_F20, (DICTATION, F19)]);
        assert_eq!(without_ours(&taken), Some(vec![CAPS_TO_F20]));
        assert_eq!(without_ours(&[CAPS_TO_F20]), None);
        // Already so: nothing to set
        assert_eq!(with_ours(&taken, F19), None);
        // Left by a bar that crashed, to another key: replaced
        assert_eq!(with_ours(&[(DICTATION, F18), CAPS_TO_F20], F19), Some(vec![CAPS_TO_F20, (DICTATION, F19)]));
        // Someone else's mapping of the dictation key (to F5): theirs
        let theirs = (DICTATION, KEYBOARD_PAGE | 0x3E);
        assert_eq!(with_ours(&[theirs], F19), None);
        assert_eq!(without_ours(&[theirs]), None);
    }

    #[test]
    fn its_keys_are_hot_keys() {
        for (name, _) in TARGETS {
            assert!(Combo::parse(name).is_ok());
        }
    }
}
