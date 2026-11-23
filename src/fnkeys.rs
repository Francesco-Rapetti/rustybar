//! The mode of the function keys, as Keyboard Settings sets it ("Use F1, F2, etc. keys as
//! standard function keys"): on, F1, F2… are standard keys and fn gives their special features
//! (brightness, music, volume); off, the other way round. Read and set in-process through the
//! parameters of IOHIDSystem, `HIDFKeyMode`, which is what the keyboards follow; setting it also
//! writes the preference Keyboard Settings shows and posts the notification it posts, so that the
//! settings, other apps and the bar itself follow.

use objc2_foundation::{NSDistributedNotificationCenter, NSNumber, NSString};

use crate::iokit::{CFTypeRef, cf, owned};

/// Posted by Keyboard Settings, and by `set`, when the mode changes.
pub const CHANGED: &str = "com.apple.keyboard.fnstatedidchange";

/// The preference in the global domain that Keyboard Settings shows.
const PREFERENCE: &str = "com.apple.keyboard.fnState";
const PARAMETER: &str = "HIDFKeyMode";

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut std::ffi::c_void) -> u32;
    fn IOServiceOpen(service: u32, task: u32, kind: u32, connect: *mut u32) -> i32;
    fn IOServiceClose(connect: u32) -> i32;
    fn IOObjectRelease(object: u32) -> i32;
    fn IOHIDCopyCFTypeParameter(connect: u32, key: CFTypeRef, parameter: *mut CFTypeRef) -> i32;
    fn IOHIDSetCFTypeParameter(connect: u32, key: CFTypeRef, parameter: CFTypeRef) -> i32;
}

unsafe extern "C" {
    /// The task's own port: what the `mach_task_self()` of C reads.
    static mach_task_self_: u32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFPreferencesAnyApplication: CFTypeRef;
    fn CFPreferencesSetAppValue(key: CFTypeRef, value: CFTypeRef, app: CFTypeRef);
    fn CFPreferencesAppSynchronize(app: CFTypeRef) -> u8;
}

/// A connection to IOHIDSystem for its parameters, closed when dropped. It asks for no
/// permission: it's how apps read the key repeat rate, say.
struct Params(u32);

impl Params {
    fn open() -> Result<Params, String> {
        unsafe {
            let service = IOServiceGetMatchingService(0, IOServiceMatching(c"IOHIDSystem".as_ptr()));
            if service == 0 {
                return Err("no IOHIDSystem".into());
            }
            let mut connect = 0;
            // kIOHIDParamConnectType
            let kr = IOServiceOpen(service, mach_task_self_, 1, &mut connect);
            IOObjectRelease(service);
            if kr != 0 {
                return Err(format!("IOServiceOpen: {kr:#x}"));
            }
            Ok(Params(connect))
        }
    }
}

impl Drop for Params {
    fn drop(&mut self) {
        unsafe { IOServiceClose(self.0) };
    }
}

/// True when F1, F2… are standard function keys; none if it can't be read.
pub fn read() -> Option<bool> {
    let params = Params::open().ok()?;
    let key = NSString::from_str(PARAMETER);
    let mut value: CFTypeRef = std::ptr::null();
    let kr = unsafe { IOHIDCopyCFTypeParameter(params.0, cf(&*key), &mut value) };
    let number = unsafe { owned::<NSNumber>(value) }?;
    (kr == 0).then(|| number.intValue() != 0)
}

/// Makes F1, F2… standard function keys (`on`) or the special features: for the keyboards, in
/// the preference and for whoever listens to `CHANGED`.
pub fn set(on: bool) -> Result<(), String> {
    let params = Params::open()?;
    let value = NSNumber::new_i32(on as i32);
    let kr = unsafe { IOHIDSetCFTypeParameter(params.0, cf(&*NSString::from_str(PARAMETER)), cf(&*value)) };
    if kr != 0 {
        return Err(format!("IOHIDSetCFTypeParameter: {kr:#x}"));
    }
    unsafe {
        CFPreferencesSetAppValue(cf(&*NSString::from_str(PREFERENCE)), cf(&*NSNumber::new_bool(on)), kCFPreferencesAnyApplication);
        CFPreferencesAppSynchronize(kCFPreferencesAnyApplication);
    }
    let center = NSDistributedNotificationCenter::defaultCenter();
    unsafe { center.postNotificationName_object_userInfo_deliverImmediately(&NSString::from_str(CHANGED), None, None, true) };
    Ok(())
}
