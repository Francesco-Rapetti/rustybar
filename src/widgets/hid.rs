//! The batteries behind a receiver or a cable, for `devices`: the mice, keyboards and controllers
//! that macOS doesn't know (its own list, the one of the Batteries widget, has only what tells
//! its battery over Bluetooth, or through Apple's and the game controllers' drivers). None of them
//! has the standard HID battery (usage page 0x06, usage 0x20): each maker has its own way, and all
//! of what's sent here are questions, never a setting. The same as RustyBar for Windows reads
//! them, where they were learned from the devices:
//!
//! - Razer: a 90-byte feature report, as Synapse sends; `07 80` is the battery (0–255), `07 84`
//!   whether it charges. A HyperSpeed receiver without its device answers "timed out".
//! - Logitech: HID++ 2.0 through the receiver (Bolt, Unifying, Lightspeed): the root feature finds
//!   the unified battery (`0x1004`) or the older battery status (`0x1000`). The receiver knows the
//!   name and the kind of what's paired even while it sleeps.
//! - ASUS ROG: the 64-byte vendor reports of the SpeedNova and Omni receivers: `12 01` on the
//!   keyboard's channel (report 2) has its level, the two halves of a split one; `12 07` on the
//!   mouse's (report 3, or report 0 on a mouse's own receiver) as G-Helper reads it. `FF AA` is
//!   "nothing there".
//! - Valve: a Steam Controller (2026) sends its battery (input report 0x43) every ~3.5 s among
//!   its other reports.
//!
//! macOS has one `IOHIDDevice` for each USB interface, with all its collections (a receiver's
//! short and long HID++ reports come through one); the reports here are given with their id
//! first, 0 for a device without ids, as Windows reads them, so the answers are read at the same
//! places. Only the interfaces of these makers are opened, and never one with a keyboard's
//! collection: macOS would ask for Input Monitoring. Everything runs on a worker thread (`pass`),
//! whose run loop waits for the input reports; finding the interfaces only reads the registry
//! (~1 ms), so it's done again on each pass.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{c_char, c_void};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString};

use crate::iokit::{CFTypeRef, Entry};

/// What a device is, as the pill groups them (`pinned`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mouse,
    Keyboard,
    Controller,
    Other,
}

/// How it reaches the Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Bluetooth,
    Receiver,
    Cable,
    Unknown,
}

/// Who made it: a device seen on two links (Bluetooth and a receiver) is told apart by its maker
/// and kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Maker {
    Apple,
    Razer,
    Logitech,
    Asus,
    Valve,
    Sony,
    Microsoft,
    Other,
}

/// The maker of a USB vendor id, or of the one a Bluetooth device gives (Razer's are 0x068E there,
/// Apple's 0x004C).
pub fn maker(vid: u16) -> Maker {
    match vid {
        0x004C | 0x05AC => Maker::Apple,
        0x1532 | 0x068E => Maker::Razer,
        0x046D => Maker::Logitech,
        0x0B05 => Maker::Asus,
        0x28DE => Maker::Valve,
        0x054C => Maker::Sony,
        0x045E => Maker::Microsoft,
        _ => Maker::Other,
    }
}

/// The vendors whose interfaces are read here: Razer, Logitech, ASUS, Valve.
pub const VENDORS: [u16; 4] = [0x1532, 0x046D, 0x0B05, 0x28DE];

/// A device with a battery, as one pass found it.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The same device in the next pass, to keep its last level while it sleeps.
    pub key: String,
    /// Empty when only its receiver is known (a ROG receiver): `devices` names it.
    pub name: String,
    pub kind: Kind,
    pub maker: Maker,
    pub link: Link,
    /// None while it isn't connected (asleep, off, its receiver alone).
    pub level: Option<u8>,
    /// The other half of a split keyboard.
    pub second: Option<u8>,
    pub charging: bool,
}

impl Found {
    fn new(key: String, name: String, kind: Kind, maker: Maker, link: Link) -> Found {
        Found { key, name, kind, maker, link, level: None, second: None, charging: false }
    }
}

/// One pass: every interface of the makers above found in the registry, then every battery read.
/// On a worker thread: it waits on devices for up to a few seconds. Also whether there was any
/// such interface, for `devices` to read again later.
pub fn pass() -> (Vec<Found>, bool) {
    let colls = discover();
    let mut found = Vec::new();
    razer(&colls, &mut found);
    logitech(&colls, &mut found);
    asus(&colls, &mut found);
    valve(&colls, &mut found);
    (merge(found), !colls.is_empty())
}

/// One device per key: the one that's connected, charging first (the cable of a device that has a
/// receiver too).
pub fn merge(found: Vec<Found>) -> Vec<Found> {
    let mut out: Vec<Found> = Vec::new();
    for f in found {
        match out.iter_mut().find(|o| o.key == f.key) {
            Some(o) => {
                let better = |x: &Found| (x.level.is_some(), x.charging);
                if better(&f) > better(o) {
                    *o = f;
                }
            }
            None => out.push(f),
        }
    }
    out
}

// ---- IOKit HID ---------------------------------------------------------------------------------

type DeviceRef = *mut c_void;
type ReportCallback = extern "C" fn(context: *mut c_void, result: i32, sender: *mut c_void, kind: u32, id: u32, report: *mut u8, length: isize);
type MatchingCallback = extern "C" fn(refcon: *mut c_void, iterator: u32);

const OUTPUT: u32 = 1;
const FEATURE: u32 = 2;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOHIDDeviceCreate(allocator: CFTypeRef, service: u32) -> DeviceRef;
    fn IOHIDDeviceOpen(device: DeviceRef, options: u32) -> i32;
    fn IOHIDDeviceClose(device: DeviceRef, options: u32) -> i32;
    fn IOHIDDeviceSetReport(device: DeviceRef, kind: u32, id: isize, report: *const u8, length: isize) -> i32;
    fn IOHIDDeviceGetReport(device: DeviceRef, kind: u32, id: isize, report: *mut u8, length: *mut isize) -> i32;
    fn IOHIDDeviceRegisterInputReportCallback(device: DeviceRef, report: *mut u8, length: isize, callback: Option<ReportCallback>, context: *mut c_void);
    fn IOHIDDeviceScheduleWithRunLoop(device: DeviceRef, run_loop: CFTypeRef, mode: CFTypeRef);
    fn IOHIDDeviceUnscheduleFromRunLoop(device: DeviceRef, run_loop: CFTypeRef, mode: CFTypeRef);
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IONotificationPortCreate(main_port: u32) -> *mut c_void;
    fn IONotificationPortDestroy(port: *mut c_void);
    fn IONotificationPortGetRunLoopSource(port: *mut c_void) -> CFTypeRef;
    fn IOServiceAddMatchingNotification(port: *mut c_void, kind: *const c_char, matching: *mut c_void, callback: MatchingCallback, refcon: *mut c_void, iterator: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IOObjectRelease(object: u32) -> i32;
}

unsafe extern "C" {
    fn CFRunLoopGetCurrent() -> CFTypeRef;
    fn CFRunLoopGetMain() -> CFTypeRef;
    fn CFRunLoopRunInMode(mode: CFTypeRef, seconds: f64, return_after_source_handled: u8) -> i32;
    fn CFRunLoopAddSource(rl: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
    fn CFRunLoopRemoveSource(rl: CFTypeRef, source: CFTypeRef, mode: CFTypeRef);
    fn CFRelease(cf: CFTypeRef);
    static kCFRunLoopDefaultMode: CFTypeRef;
}

/// An output report of an interface, from its report descriptor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Report {
    pub id: u8,
    /// The usage page of the top-level collection it's in.
    pub page: u16,
    /// Its length with the id in front, as Windows counts it (also for id 0).
    pub bytes: usize,
}

/// An interface of one of the makers, as the registry describes it (nothing is opened).
pub struct Collection {
    entry: Entry,
    vid: u16,
    pid: u16,
    product: String,
    transport: String,
    /// The USB device it's an interface of: its other interfaces have the same.
    location: i64,
    /// Its top-level collections, as (usage page, usage).
    usages: Vec<(u16, u16)>,
    input: usize,
    feature: usize,
    outputs: Vec<Report>,
}

impl Collection {
    fn has(&self, page: u16, usage: u16) -> bool {
        self.usages.contains(&(page, usage))
    }

    /// It has a keyboard's (or keypad's) collection: opening it would make macOS ask for Input
    /// Monitoring.
    fn keyboard(&self) -> bool {
        self.has(1, 6) || self.has(1, 7)
    }

    fn receiver(&self) -> bool {
        let p = self.product.to_lowercase();
        p.contains("receiver") || p.contains("dongle")
    }

    fn bluetooth(&self) -> bool {
        self.transport.to_lowercase().contains("bluetooth")
    }
}

/// The interfaces of the makers that `pass` reads.
fn discover() -> Vec<Collection> {
    crate::iokit::services(c"IOHIDDevice")
        .into_iter()
        .filter_map(|entry| {
            let vid = entry.number("VendorID")? as u16;
            if !VENDORS.contains(&vid) {
                return None;
            }
            let mut usages = usage_pairs(&entry);
            if usages.is_empty()
                && let (Some(page), Some(usage)) = (entry.number("PrimaryUsagePage"), entry.number("PrimaryUsage"))
            {
                usages.push((page as u16, usage as u16));
            }
            let outputs = entry.data("ReportDescriptor").map(|d| output_reports(&d)).unwrap_or_default();
            Some(Collection {
                vid,
                pid: entry.number("ProductID").unwrap_or(0) as u16,
                product: entry.string("Product").unwrap_or_default().trim().to_string(),
                transport: entry.string("Transport").unwrap_or_default(),
                location: entry.number("LocationID").unwrap_or(0),
                usages,
                input: entry.number("MaxInputReportSize").unwrap_or(0).max(0) as usize,
                feature: entry.number("MaxFeatureReportSize").unwrap_or(0).max(0) as usize,
                outputs,
                entry,
            })
        })
        .collect()
}

/// `DeviceUsagePairs`: every top-level collection of the interface.
fn usage_pairs(entry: &Entry) -> Vec<(u16, u16)> {
    let Some(list) = entry.property("DeviceUsagePairs").and_then(|o| o.downcast::<NSArray>().ok()) else { return Vec::new() };
    list.iter()
        .filter_map(|o| {
            let d: Retained<crate::iokit::Dict> = o.downcast::<NSDictionary>().ok().map(|d| unsafe { Retained::cast_unchecked(d) })?;
            let n = |k: &str| d.objectForKey(&NSString::from_str(k)).and_then(|v| v.downcast::<NSNumber>().ok()).map(|v| v.longLongValue() as u16);
            Some((n("DeviceUsagePage")?, n("DeviceUsage")?))
        })
        .collect()
}

/// The output reports a report descriptor declares: their ids, the usage page of their top-level
/// collection, and their length.
pub fn output_reports(desc: &[u8]) -> Vec<Report> {
    let mut out: Vec<(u8, u16, u32)> = Vec::new();
    let (mut page, mut size, mut count, mut id) = (0u32, 0u32, 0u32, 0u8);
    let mut stack: Vec<(u32, u32, u32, u8)> = Vec::new();
    let mut usage: Option<u32> = None;
    let mut depth = 0;
    let mut top_page = 0u16;
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        // A long item: skip it
        if prefix == 0xFE {
            let len = desc.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + len;
            continue;
        }
        let len = match prefix & 0x03 {
            3 => 4,
            n => n as usize,
        };
        let Some(bytes) = desc.get(i + 1..i + 1 + len) else { break };
        let value = bytes.iter().rev().fold(0u32, |a, b| (a << 8) | *b as u32);
        match prefix & 0xFC {
            // Global items
            0x04 => page = value,
            0x74 => size = value,
            0x94 => count = value,
            0x84 => id = value as u8,
            0xA4 => stack.push((page, size, count, id)),
            0xB4 => {
                if let Some(s) = stack.pop() {
                    (page, size, count, id) = s;
                }
            }
            // Local: a usage, with its page in the high half when it's 4 bytes long
            0x08 => usage = Some(if len == 4 { value } else { (page << 16) | value }),
            // Main items
            0xA0 => {
                if depth == 0 {
                    top_page = (usage.unwrap_or(page << 16) >> 16) as u16;
                }
                depth += 1;
                usage = None;
            }
            0xC0 => {
                depth = (depth as i32 - 1).max(0) as u32;
                usage = None;
            }
            0x90 => {
                match out.iter_mut().find(|(i, _, _)| *i == id) {
                    Some(r) => r.2 += size * count,
                    None => out.push((id, top_page, size * count)),
                }
                usage = None;
            }
            0x80 | 0xB0 => usage = None,
            _ => {}
        }
        i += 1 + len;
    }
    out.into_iter().map(|(id, page, bits)| Report { id, page, bytes: bits.div_ceil(8) as usize + 1 }).collect()
}

/// The interfaces of each device (same maker, product and USB device), with what picks them.
fn devices(colls: &[Collection], pick: impl Fn(&Collection) -> bool) -> Vec<Vec<&Collection>> {
    let mut groups: Vec<Vec<&Collection>> = Vec::new();
    for c in colls.iter().filter(|c| pick(c)) {
        match groups.iter_mut().find(|g| g[0].location == c.location && g[0].vid == c.vid && g[0].pid == c.pid) {
            Some(g) => g.push(c),
            None => groups.push(vec![c]),
        }
    }
    groups
}

/// The input reports that arrived and haven't been read, the oldest first.
#[derive(Default)]
struct Inbox(RefCell<VecDeque<Vec<u8>>>);

extern "C" fn arrived(context: *mut c_void, result: i32, _sender: *mut c_void, _kind: u32, id: u32, report: *mut u8, length: isize) {
    if result != 0 || context.is_null() || report.is_null() || length <= 0 {
        return;
    }
    let inbox = unsafe { &*(context as *const Inbox) };
    let bytes = unsafe { std::slice::from_raw_parts(report, length as usize) };
    // As Windows reads them: the report's id first, 0 for a device without ids
    let mut r = Vec::with_capacity(bytes.len() + 1);
    if id == 0 {
        r.push(0);
    }
    r.extend_from_slice(bytes);
    let mut reports = inbox.0.borrow_mut();
    // A controller sends hundreds a second: only the latest matter
    if reports.len() >= 512 {
        reports.pop_front();
    }
    reports.push_back(r);
}

/// An interface opened for its reports; with `listen`, its input reports arrive on this thread's
/// run loop while `read` waits. Closed when dropped, on the thread that opened it.
struct Port {
    device: DeviceRef,
    listening: Option<(Box<[u8]>, Box<Inbox>)>,
}

impl Port {
    fn open(c: &Collection, listen: bool) -> Option<Port> {
        if c.keyboard() {
            return None;
        }
        let device = unsafe { IOHIDDeviceCreate(std::ptr::null(), c.entry.object()) };
        if device.is_null() {
            return None;
        }
        // Not seized: the device goes on working as always
        if unsafe { IOHIDDeviceOpen(device, 0) } != 0 {
            unsafe { CFRelease(device) };
            return None;
        }
        let mut port = Port { device, listening: None };
        if listen {
            let mut buffer = vec![0u8; c.input.max(64)].into_boxed_slice();
            let inbox = Box::new(Inbox::default());
            unsafe {
                IOHIDDeviceRegisterInputReportCallback(device, buffer.as_mut_ptr(), buffer.len() as isize, Some(arrived), &*inbox as *const Inbox as *mut c_void);
                IOHIDDeviceScheduleWithRunLoop(device, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
            }
            port.listening = Some((buffer, inbox));
        }
        Some(port)
    }

    /// Sends a report given with its id first (0 for none, which isn't sent).
    fn send(&self, kind: u32, report: &[u8]) -> bool {
        let Some((&id, rest)) = report.split_first() else { return false };
        let data = if id == 0 { rest } else { report };
        unsafe { IOHIDDeviceSetReport(self.device, kind, id as isize, data.as_ptr(), data.len() as isize) == 0 }
    }

    /// A feature report of `len` bytes, with its id first.
    fn feature(&self, id: u8, len: usize) -> Option<Vec<u8>> {
        let mut r = vec![0u8; len + 1];
        r[0] = id;
        let start = if id == 0 { 1 } else { 0 };
        let mut n = (r.len() - start) as isize;
        if unsafe { IOHIDDeviceGetReport(self.device, FEATURE, id as isize, r[start..].as_mut_ptr(), &mut n) } != 0 {
            return None;
        }
        r.truncate(start + n.max(0) as usize);
        Some(r)
    }

    fn take(&self) -> Option<Vec<u8>> {
        self.listening.as_ref()?.1.0.borrow_mut().pop_front()
    }

    /// The next input report, waiting up to `ms` for it.
    fn read(&self, ms: u64) -> Option<Vec<u8>> {
        self.listening.as_ref()?;
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            if let Some(r) = self.take() {
                return Some(r);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, left.as_secs_f64(), 1) };
        }
    }

    /// Lets the input reports arrive for `ms`, then gives them all.
    fn gather(&self, ms: u64) -> Vec<Vec<u8>> {
        if self.listening.is_none() {
            return Vec::new();
        }
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, ms as f64 / 1000.0, 0) };
        std::iter::from_fn(|| self.take()).collect()
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        unsafe {
            if let Some((buffer, _)) = self.listening.as_mut() {
                IOHIDDeviceRegisterInputReportCallback(self.device, buffer.as_mut_ptr(), buffer.len() as isize, None, std::ptr::null_mut());
                IOHIDDeviceUnscheduleFromRunLoop(self.device, CFRunLoopGetCurrent(), kCFRunLoopDefaultMode);
            }
            IOHIDDeviceClose(self.device, 0);
            CFRelease(self.device);
        }
    }
}

/// Tells when an interface of one of the makers comes or goes, on the main run loop. Stops when
/// dropped.
pub struct Watch {
    port: *mut c_void,
    iterators: Vec<u32>,
    context: *mut Box<dyn Fn()>,
}

extern "C" fn matched(refcon: *mut c_void, iterator: u32) {
    if drain(iterator) {
        let f = unsafe { &*(refcon as *const Box<dyn Fn()>) };
        f();
    }
}

/// Empties an iterator of notifications (which arms it again): true if one of the makers' came
/// or went.
fn drain(iterator: u32) -> bool {
    let mut ours = false;
    loop {
        let o = unsafe { IOIteratorNext(iterator) };
        if o == 0 {
            break;
        }
        let entry = Entry::from_object(o);
        ours |= entry.number("VendorID").is_some_and(|v| VENDORS.contains(&(v as u16)));
    }
    ours
}

impl Watch {
    pub fn new(changed: impl Fn() + 'static) -> Option<Watch> {
        unsafe {
            let port = IONotificationPortCreate(0);
            if port.is_null() {
                return None;
            }
            CFRunLoopAddSource(CFRunLoopGetMain(), IONotificationPortGetRunLoopSource(port), kCFRunLoopDefaultMode);
            let context: *mut Box<dyn Fn()> = Box::into_raw(Box::new(Box::new(changed)));
            let mut watch = Watch { port, iterators: Vec::new(), context };
            for kind in [c"IOServiceFirstMatch", c"IOServiceTerminate"] {
                let mut iterator = 0u32;
                // The matching dictionary is consumed
                if IOServiceAddMatchingNotification(port, kind.as_ptr(), IOServiceMatching(c"IOHIDDevice".as_ptr()), matched, context as *mut c_void, &mut iterator) == 0 {
                    // What's there already: the first pass reads it anyway
                    drain(iterator);
                    watch.iterators.push(iterator);
                }
            }
            Some(watch)
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        unsafe {
            for i in &self.iterators {
                IOObjectRelease(*i);
            }
            CFRunLoopRemoveSource(CFRunLoopGetMain(), IONotificationPortGetRunLoopSource(self.port), kCFRunLoopDefaultMode);
            IONotificationPortDestroy(self.port);
            drop(Box::from_raw(self.context));
        }
    }
}

// ---- Razer --------------------------------------------------------------------------------------

/// Razer's statuses: the command went through, the device didn't answer (a receiver alone), it's
/// still working on it.
const RAZER_OK: u8 = 0x02;
const RAZER_TIMEOUT: u8 = 0x04;
const RAZER_BUSY: u8 = 0x01;

/// A Razer command with no arguments: report ID 0, status 0 (new), the transaction, the size of the
/// answer, class and id, then a CRC (the XOR of bytes 3–88 of the report).
pub(super) fn razer_request(transaction: u8, class: u8, id: u8) -> [u8; 91] {
    let mut r = [0u8; 91];
    r[2] = transaction;
    r[6] = 0x02;
    r[7] = class;
    r[8] = id;
    r[89] = r[3..89].iter().fold(0, |a, b| a ^ b);
    r
}

/// The status of an answer to `class`/`id`, and its second argument (the level, or 1 if charging).
pub(super) fn razer_answer(r: &[u8], class: u8, id: u8) -> Option<(u8, u8)> {
    (r.len() >= 11 && r[7] == class && r[8] == id).then(|| (r[1], r[10]))
}

fn razer_ask(port: &Port, transaction: u8, class: u8, id: u8) -> Option<(u8, u8)> {
    if !port.send(FEATURE, &razer_request(transaction, class, id)) {
        return None;
    }
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(5));
        let r = port.feature(0, 90)?;
        // Another program's answer (Synapse asks too) doesn't count
        match razer_answer(&r, class, id) {
            Some((status, _)) if status == RAZER_BUSY || status == 0 => continue,
            other => return other,
        }
    }
    None
}

/// What a Razer product id is: mice under 0x0100, keyboards up to 0x04ff; the rest (headsets,
/// docks, mats, lights, cases) isn't read.
fn razer_kind(pid: u16) -> Option<Kind> {
    match pid {
        0x0001..=0x00ff => Some(Kind::Mouse),
        0x0100..=0x04ff => Some(Kind::Keyboard),
        _ => None,
    }
}

fn razer(colls: &[Collection], found: &mut Vec<Found>) {
    for device in devices(colls, |c| c.vid == 0x1532 && c.feature == 90 && !c.keyboard()) {
        let Some(kind) = razer_kind(device[0].pid) else { continue };
        for c in device {
            let Some(port) = Port::open(c, false) else { continue };
            // Most answer to 0x1F; older ones to 0x3F or 0xFF
            let answer = [0x1F, 0x3F, 0xFF].into_iter().find_map(|t| razer_ask(&port, t, 0x07, 0x80).filter(|(s, _)| *s == RAZER_OK || *s == RAZER_TIMEOUT).map(|a| (t, a)));
            let Some((transaction, (status, level))) = answer else { continue };
            let name = if c.product.is_empty() { "Razer".to_string() } else { c.product.clone() };
            let link = if c.bluetooth() { Link::Bluetooth } else { Link::Unknown };
            let mut f = Found::new(format!("razer:{}", name.to_lowercase()), name, kind, Maker::Razer, link);
            if status == RAZER_OK {
                f.level = Some(((level as u32 * 100 + 127) / 255) as u8);
                f.charging = razer_ask(&port, transaction, 0x07, 0x84).is_some_and(|(s, c)| s == RAZER_OK && c == 1);
                if f.charging {
                    f.link = Link::Cable;
                }
            }
            found.push(f);
            break;
        }
    }
}

// ---- Logitech -----------------------------------------------------------------------------------

/// Our software id in HID++ 2.0 requests (the low nibble of the function byte): answers to another
/// program (Options+, G Hub) carry theirs.
const SWID: u8 = 0x0A;

/// A receiver's HID++ interface, with its short (0x10) and long (0x11) reports.
struct Hidpp {
    port: Port,
}

enum Answer {
    Ok(Vec<u8>),
    /// HID++ 1.0's error (from the receiver: the device didn't connect, nothing paired there), or
    /// 2.0's.
    Error,
    Timeout,
}

impl Hidpp {
    /// Waits for the answer to `matches` (or an error about `sub`/`address`) up to `ms`.
    fn wait(&mut self, device: u8, sub: u8, address: u8, ms: u64, matches: impl Fn(&[u8]) -> bool) -> Answer {
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            let left = deadline.saturating_duration_since(Instant::now()).as_millis() as u64;
            if left == 0 {
                return Answer::Timeout;
            }
            let Some(r) = self.port.read(left) else { return Answer::Timeout };
            if let Some(a) = Self::check(&r, device, sub, address, &matches) {
                return a;
            }
        }
    }

    fn check(r: &[u8], device: u8, sub: u8, address: u8, matches: &impl Fn(&[u8]) -> bool) -> Option<Answer> {
        if r.len() < 6 || r[1] != device {
            return None;
        }
        if (r[2] == 0x8F || r[2] == 0xFF) && r[3] == sub && r[4] == address {
            return Some(Answer::Error);
        }
        matches(r).then(|| Answer::Ok(r[4..].to_vec()))
    }

    /// A HID++ 2.0 call: function `func` of the feature at `index`.
    fn call(&mut self, device: u8, index: u8, func: u8, params: &[u8]) -> Answer {
        let mut req = [0u8; 20];
        req[0] = 0x11;
        req[1] = device;
        req[2] = index;
        req[3] = (func << 4) | SWID;
        req[4..4 + params.len()].copy_from_slice(params);
        if !self.port.send(OUTPUT, &req) {
            return Answer::Timeout;
        }
        self.wait(device, index, req[3], 400, |r| r[0] == 0x11 && r[2] == index && r[3] == req[3])
    }

    /// The index of a feature, if the device has it (`IRoot.getFeature`).
    fn feature(&mut self, device: u8, id: u16) -> Result<Option<u8>, Answer> {
        match self.call(device, 0x00, 0x00, &[(id >> 8) as u8, id as u8]) {
            Answer::Ok(p) => Ok((p[0] != 0).then_some(p[0])),
            other => Err(other),
        }
    }

    /// A long register of the receiver (`0x83`): 16 bytes.
    fn register(&mut self, address: u8, p0: u8, p1: u8) -> Option<Vec<u8>> {
        let req = [0x10u8, 0xFF, 0x83, address, p0, p1, 0];
        if !self.port.send(OUTPUT, &req) {
            return None;
        }
        match self.wait(0xFF, 0x83, address, 300, |r| r[0] == 0x11 && r[2] == 0x83 && r[3] == address && r[4] == p0) {
            Answer::Ok(d) => Some(d),
            _ => None,
        }
    }
}

/// HID++ 1.0's kinds of paired devices (the receiver's pairing information).
pub(super) fn logitech_kind(kind: u8) -> Kind {
    match kind & 0x0F {
        1 | 3 => Kind::Keyboard,
        2 | 8 | 9 => Kind::Mouse,
        11 | 12 => Kind::Controller,
        _ => Kind::Other,
    }
}

/// The name and kind of a receiver's slot from its registers: Bolt keeps them at 0x50 + n and
/// 0x60 + n (with a 1), Unifying and Lightspeed at 0x20 + n and 0x40 + n.
fn logitech_pairing(p: &mut Hidpp, bolt: bool, slot: u8) -> Option<(String, Kind)> {
    if bolt {
        let info = p.register(0xB5, 0x50 + slot, 0)?;
        let name = p.register(0xB5, 0x60 + slot, 1)?;
        let len = (name[2] as usize).min(name.len() - 3);
        Some((String::from_utf8_lossy(&name[3..3 + len]).trim().to_string(), logitech_kind(info[1])))
    } else {
        let info = p.register(0xB5, 0x20 + slot, 0)?;
        let name = p.register(0xB5, 0x40 + slot, 0)?;
        let len = (name[1] as usize).min(name.len() - 2);
        Some((String::from_utf8_lossy(&name[2..2 + len]).trim().to_string(), logitech_kind(info[7])))
    }
}

/// The level and whether it charges, from the unified battery's `getStatus` or the older
/// `getBatteryLevelStatus`.
pub(super) fn logitech_level(unified: bool, p: &[u8]) -> (u8, bool) {
    if unified {
        (p[0].min(100), matches!(p[2], 1..=3))
    } else {
        (p[0].min(100), matches!(p[2], 1 | 2 | 4))
    }
}

fn logitech(colls: &[Collection], found: &mut Vec<Found>) {
    // The interface with HID++'s short and long reports (vendor page 0xFF00, usages 1 and 2)
    for c in colls.iter().filter(|c| c.vid == 0x046D && c.has(0xFF00, 1) && c.has(0xFF00, 2) && c.input >= 20) {
        let Some(port) = Port::open(c, true) else { continue };
        let mut p = Hidpp { port };
        let receiver = c.receiver();
        let bolt = c.pid == 0xC548;
        let slots: Vec<u8> = if receiver { (1..=6).collect() } else { vec![0xFF] };
        for slot in slots {
            let unified = p.feature(slot, 0x1004);
            // A device that isn't a receiver's and doesn't answer isn't one
            if !receiver && unified.is_err() {
                continue;
            }
            // The receiver knows the name and the kind of what's paired even while it sleeps (an
            // empty slot has none)
            let (name, kind) = if receiver { logitech_pairing(&mut p, bolt, slot).unwrap_or((String::new(), Kind::Other)) } else { (c.product.clone(), Kind::Mouse) };
            let level = match unified {
                Ok(Some(i)) => match p.call(slot, i, 1, &[]) {
                    Answer::Ok(r) => Some(logitech_level(true, &r)),
                    _ => None,
                },
                Ok(None) => match p.feature(slot, 0x1000) {
                    Ok(Some(i)) => match p.call(slot, i, 0, &[]) {
                        Answer::Ok(r) => Some(logitech_level(false, &r)),
                        _ => None,
                    },
                    _ => None,
                },
                Err(_) => None,
            };
            // A name is needed: one only the device would say, if it's awake
            let (name, kind) = if name.is_empty() && level.is_some() { device_name(&mut p, slot).unwrap_or(("Logitech".to_string(), Kind::Other)) } else { (name, kind) };
            if name.is_empty() {
                continue;
            }
            let key = format!("logitech:{:x}:{slot}:{}", c.location, name.to_lowercase());
            let link = if receiver { Link::Receiver } else if c.bluetooth() { Link::Bluetooth } else { Link::Cable };
            let mut f = Found::new(key, name, kind, Maker::Logitech, link);
            if let Some((l, charging)) = level {
                f.level = Some(l);
                f.charging = charging;
            }
            found.push(f);
        }
    }
}

/// The name and the kind a HID++ 2.0 device gives (`0x0005`), for one that's no receiver's.
fn device_name(p: &mut Hidpp, slot: u8) -> Option<(String, Kind)> {
    let i = p.feature(slot, 0x0005).ok()??;
    let Answer::Ok(count) = p.call(slot, i, 0, &[]) else { return None };
    let count = count[0] as usize;
    let mut name = Vec::new();
    while name.len() < count {
        let Answer::Ok(part) = p.call(slot, i, 1, &[name.len() as u8]) else { break };
        let take = (count - name.len()).min(part.len());
        if take == 0 {
            break;
        }
        name.extend_from_slice(&part[..take]);
    }
    let kind = match p.call(slot, i, 2, &[]) {
        Answer::Ok(t) => match t[0] {
            0 | 2 => Kind::Keyboard,
            3..=5 => Kind::Mouse,
            11 | 12 => Kind::Controller,
            _ => Kind::Other,
        },
        _ => Kind::Other,
    };
    Some((String::from_utf8_lossy(&name).trim().to_string(), kind))
}

// ---- ASUS ROG -----------------------------------------------------------------------------------

/// The level of a ROG keyboard from its receiver's answer to `12 01`, and the other half's when it
/// has two (byte 7 says 2): `None` if it doesn't answer that (`FF AA`). A level of 0 is a keyboard
/// that sleeps or is off. Read from a split ROG keyboard (both halves at 53%).
pub(super) fn asus_keyboard(r: &[u8]) -> Option<(u8, Option<u8>)> {
    if r.len() < 12 || r[1] != 0x12 || r[2] != 0x01 {
        return None;
    }
    let second = (r[7] == 2 && (1..=100).contains(&r[11])).then_some(r[11]);
    Some((r[6].min(100), second))
}

/// The level of a ROG mouse and whether it charges, from the answer to `12 07` (as G-Helper reads
/// it); a level of 0 without charging is a mouse that sleeps.
pub(super) fn asus_mouse(r: &[u8]) -> Option<(u8, bool)> {
    (r.len() >= 11 && r[1] == 0x12 && r[2] == 0x07).then(|| (r[5].min(100), r[10] > 0))
}

fn asus_ask(c: &Collection, out: &Report, sub: u8) -> Option<Vec<u8>> {
    let port = Port::open(c, true)?;
    let mut req = vec![0u8; out.bytes];
    req[0] = out.id;
    req[1] = 0x12;
    req[2] = sub;
    if !port.send(OUTPUT, &req) {
        return None;
    }
    let deadline = Instant::now() + Duration::from_millis(250);
    while Instant::now() < deadline {
        let r = port.read(50)?;
        if r.len() > 2 && (r[1] == 0x12 && r[2] == sub || r[1] == 0xFF && r[2] == 0xAA) {
            return Some(r);
        }
    }
    None
}

fn asus(colls: &[Collection], found: &mut Vec<Found>) {
    for device in devices(colls, |c| c.vid == 0x0B05) {
        // A ROG keyboard, mouse or their receiver: never the board's lights or anything else of ASUS
        if !device.iter().any(|c| c.has(1, 2) || c.has(1, 6)) {
            continue;
        }
        let receiver = device.iter().any(|c| c.receiver());
        let name = if receiver { String::new() } else { device[0].product.clone() };
        for c in device.iter().filter(|c| c.input >= 64 && !c.keyboard()) {
            for out in c.outputs.iter().filter(|o| o.page >= 0xFF00 && o.bytes >= 64) {
                let id = format!("{:x}", c.location);
                let link = if receiver { Link::Receiver } else if c.bluetooth() { Link::Bluetooth } else { Link::Unknown };
                if out.id == 2 {
                    let Some((level, second)) = asus_ask(c, out, 0x01).as_deref().and_then(asus_keyboard) else { continue };
                    let mut f = Found::new(format!("asus:{id}:keyboard"), name.clone(), Kind::Keyboard, Maker::Asus, link);
                    if level > 0 {
                        f.level = Some(level);
                        f.second = second;
                    }
                    found.push(f);
                } else if out.id == 3 || out.id == 0 && out.bytes == 65 {
                    let Some((level, charging)) = asus_ask(c, out, 0x07).as_deref().and_then(asus_mouse) else { continue };
                    let mut f = Found::new(format!("asus:{id}:mouse"), name.clone(), Kind::Mouse, Maker::Asus, link);
                    if level > 0 || charging {
                        f.level = Some(level);
                        f.charging = charging;
                    }
                    found.push(f);
                }
            }
        }
    }
}

// ---- Valve --------------------------------------------------------------------------------------

/// A Steam Controller's battery report (0x43): the charge state (1 on battery, 2 charging, 4
/// charged) and the level.
pub(super) fn steam_battery(r: &[u8]) -> Option<(u8, bool)> {
    (r.len() >= 15 && r[0] == 0x43).then(|| (r[2].min(100), matches!(r[1], 2 | 4)))
}

fn valve(colls: &[Collection], found: &mut Vec<Found>) {
    // Wired, Bluetooth and the puck (one interface a controller): their vendor collections
    let slots: Vec<&Collection> = colls.iter().filter(|c| c.vid == 0x28DE && matches!(c.pid, 0x1302..=0x1304) && c.has(0xFF00, 1) && c.input >= 15).collect();
    let ports: Vec<(&Collection, Port)> = slots.into_iter().filter_map(|c| Port::open(c, true).map(|p| (c, p))).collect();
    let mut levels: Vec<Option<(u8, bool)>> = vec![None; ports.len()];
    let mut waiting: Vec<bool> = vec![true; ports.len()];
    // The battery comes every ~3.5 s: up to three rounds of 1.5 s, all the controllers at once
    for _ in 0..3 {
        if !waiting.iter().any(|w| *w) {
            break;
        }
        let mut any = vec![false; ports.len()];
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(1500) {
            for (i, (_, port)) in ports.iter().enumerate() {
                for r in port.gather(0) {
                    any[i] = true;
                    if let Some(level) = steam_battery(&r) {
                        levels[i] = Some(level);
                        waiting[i] = false;
                    }
                }
            }
            if !waiting.iter().any(|w| *w) {
                break;
            }
            // The run loop is the same for every port: one wait serves them all
            unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.25, 0) };
        }
        // Nothing came: no controller on that slot
        for i in 0..ports.len() {
            if !any[i] {
                waiting[i] = false;
            }
        }
    }
    let total = levels.iter().filter(|l| l.is_some()).count();
    let mut n = 0;
    for ((c, _), level) in ports.iter().zip(levels) {
        let Some((level, charging)) = level else { continue };
        n += 1;
        let name = if total > 1 { format!("Steam Controller {n}") } else { "Steam Controller".to_string() };
        let link = match c.pid {
            0x1302 => Link::Cable,
            0x1303 => Link::Bluetooth,
            _ => Link::Receiver,
        };
        let mut f = Found::new(format!("valve:{:x}:{n}", c.location), name, Kind::Controller, Maker::Valve, link);
        f.level = Some(level);
        f.charging = charging;
        found.push(f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn razer_requests() {
        let r = razer_request(0x1F, 0x07, 0x80);
        assert_eq!(r.len(), 91);
        assert_eq!((r[0], r[2], r[6], r[7], r[8]), (0, 0x1F, 0x02, 0x07, 0x80));
        assert_eq!(r[89], 0x02 ^ 0x07 ^ 0x80);
        let mut answer = r;
        answer[1] = RAZER_OK;
        answer[10] = 255;
        assert_eq!(razer_answer(&answer, 0x07, 0x80), Some((RAZER_OK, 255)));
        assert_eq!(razer_answer(&answer, 0x07, 0x84), None);
        assert_eq!(razer_kind(0x00A5), Some(Kind::Mouse));
        assert_eq!(razer_kind(0x02B4), Some(Kind::Keyboard));
        assert_eq!(razer_kind(0x0F07), None);
    }

    #[test]
    fn logitech_answers() {
        assert_eq!(logitech_kind(0x42), Kind::Mouse);
        assert_eq!(logitech_kind(0x01), Kind::Keyboard);
        assert_eq!(logitech_level(true, &[64, 0, 0]), (64, false));
        assert_eq!(logitech_level(true, &[90, 0, 2]), (90, true));
        assert_eq!(logitech_level(false, &[30, 0, 1]), (30, true));
    }

    #[test]
    fn asus_answers() {
        let mut r = vec![0u8; 64];
        r[0] = 2;
        r[1] = 0x12;
        r[2] = 0x01;
        r[6] = 53;
        r[7] = 2;
        r[11] = 48;
        assert_eq!(asus_keyboard(&r), Some((53, Some(48))));
        r[7] = 1;
        assert_eq!(asus_keyboard(&r), Some((53, None)));
        r[2] = 0x07;
        r[5] = 70;
        r[10] = 1;
        assert_eq!(asus_mouse(&r), Some((70, true)));
        assert_eq!(asus_keyboard(&[2, 0xFF, 0xAA, 0, 0, 0, 0, 0, 0, 0, 0, 0]), None);
    }

    #[test]
    fn steam_answers() {
        let mut r = vec![0u8; 20];
        r[0] = 0x43;
        r[1] = 2;
        r[2] = 77;
        assert_eq!(steam_battery(&r), Some((77, true)));
        r[0] = 0x45;
        assert_eq!(steam_battery(&r), None);
    }

    #[test]
    fn output_reports_of_a_descriptor() {
        // A vendor collection (0xFF00) with a 63-byte output report 2, then one of 64 bytes
        // without an id in another collection
        let desc = [
            0x06, 0x00, 0xFF, // Usage Page (0xFF00)
            0x09, 0x01, // Usage (1)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x02, // Report ID (2)
            0x75, 0x08, // Report Size (8)
            0x95, 0x3F, // Report Count (63)
            0x09, 0x02, // Usage (2)
            0x91, 0x02, // Output
            0x09, 0x03, // Usage (3)
            0x81, 0x02, // Input
            0xC0, // End Collection
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x02, // Usage (Mouse)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x00, // Report ID (0)
            0x95, 0x40, // Report Count (64)
            0x91, 0x02, // Output
            0xC0, // End Collection
        ];
        assert_eq!(output_reports(&desc), vec![Report { id: 2, page: 0xFF00, bytes: 64 }, Report { id: 0, page: 1, bytes: 65 }]);
    }

    #[test]
    fn one_per_key() {
        let mut asleep = Found::new("k".into(), "Keys".into(), Kind::Keyboard, Maker::Razer, Link::Receiver);
        let mut cable = asleep.clone();
        cable.level = Some(80);
        cable.charging = true;
        asleep.level = None;
        assert_eq!(merge(vec![asleep, cable.clone()]), vec![cable]);
    }
}
