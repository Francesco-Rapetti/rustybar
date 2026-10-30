//! The few IOKit calls RustyBar makes, and its CF objects seen as the Foundation objects they are
//! toll-free bridged to.

use std::ffi::{c_char, c_void};

use objc2::Message;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSDictionary, NSNumber, NSString};

pub type CFTypeRef = *const c_void;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main_port: u32, matching: *mut c_void, iterator: *mut u32) -> i32;
    fn IORegistryEntryGetChildIterator(entry: u32, plane: *const c_char, iterator: *mut u32) -> i32;
    fn IOIteratorNext(iterator: u32) -> u32;
    fn IORegistryEntryCreateCFProperty(entry: u32, key: CFTypeRef, allocator: CFTypeRef, options: u32) -> CFTypeRef;
    fn IOObjectRelease(object: u32) -> i32;
}

/// A CF object we own (a Copy or Create function's).
///
/// # Safety
/// `cf` must be null or a +1 reference to an object of class `T`.
pub unsafe fn owned<T: Message>(cf: CFTypeRef) -> Option<Retained<T>> {
    unsafe { Retained::from_raw(cf as *mut T) }
}

/// A CF object we don't own (a Get function's), retained.
///
/// # Safety
/// `cf` must be null or a valid object of class `T`.
pub unsafe fn borrowed<T: Message>(cf: CFTypeRef) -> Option<Retained<T>> {
    unsafe { Retained::retain(cf as *mut T) }
}

pub fn cf<T: Message>(o: &T) -> CFTypeRef {
    o as *const T as CFTypeRef
}

pub type Dict = NSDictionary<NSString, AnyObject>;

pub fn number(d: &Dict, key: &NSString) -> Option<i64> {
    d.objectForKey(key).and_then(|o| o.downcast::<NSNumber>().ok()).map(|n| n.longLongValue())
}

pub fn string(d: &Dict, key: &NSString) -> Option<String> {
    d.objectForKey(key).and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string())
}

/// An IORegistry entry, released when dropped.
pub struct Entry(u32);

impl Entry {
    /// A property of the entry, e.g. a dictionary, a number or a string.
    pub fn property(&self, key: &str) -> Option<Retained<AnyObject>> {
        unsafe { owned(IORegistryEntryCreateCFProperty(self.0, cf(&*NSString::from_str(key)), std::ptr::null(), 0)) }
    }

    pub fn dict(&self, key: &str) -> Option<Retained<Dict>> {
        let o = self.property(key)?;
        o.downcast::<NSDictionary>().ok().map(|d| unsafe { Retained::cast_unchecked(d) })
    }

    pub fn number(&self, key: &str) -> Option<i64> {
        self.property(key)?.downcast::<NSNumber>().ok().map(|n| n.longLongValue())
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.property(key)?.downcast::<NSString>().ok().map(|s| s.to_string())
    }

    /// The children of the entry in the service plane, e.g. the clients of a GPU.
    pub fn children(&self) -> Vec<Entry> {
        let mut it = 0u32;
        if unsafe { IORegistryEntryGetChildIterator(self.0, c"IOService".as_ptr(), &mut it) } != 0 {
            return Vec::new();
        }
        drain(it)
    }
}

impl Drop for Entry {
    fn drop(&mut self) {
        unsafe { IOObjectRelease(self.0) };
    }
}

fn drain(it: u32) -> Vec<Entry> {
    let mut out = Vec::new();
    loop {
        let o = unsafe { IOIteratorNext(it) };
        if o == 0 {
            break;
        }
        out.push(Entry(o));
    }
    unsafe { IOObjectRelease(it) };
    out
}

/// The services of a class, e.g. "IOAccelerator" for the GPUs.
pub fn services(class: &std::ffi::CStr) -> Vec<Entry> {
    let mut it = 0u32;
    // IOServiceGetMatchingServices consumes the matching dictionary
    if unsafe { IOServiceGetMatchingServices(0, IOServiceMatching(class.as_ptr()), &mut it) } != 0 {
        return Vec::new();
    }
    drain(it)
}
