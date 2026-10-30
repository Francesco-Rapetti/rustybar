//! The screens: their frames, scale, notch and size class.

use objc2::MainThreadMarker;
use objc2_app_kit::NSScreen;
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSNumber, NSString};

#[derive(Clone, Debug, PartialEq)]
pub struct DisplayInfo {
    /// The CGDirectDisplayID.
    pub id: u32,
    pub name: String,
    /// In Cocoa coordinates (origin at the bottom left of the main screen).
    pub frame: CGRect,
    pub scale: f64,
    /// The notch, from its left to its right edge in points from the screen's left edge.
    pub notch: Option<(f64, f64)>,
    /// At least `large_width` wide.
    pub large: bool,
}

pub fn displays(mtm: MainThreadMarker, large_width: f64) -> Vec<DisplayInfo> {
    NSScreen::screens(mtm)
        .iter()
        .map(|screen| {
            let frame = screen.frame();
            let description = screen.deviceDescription();
            let id = description
                .objectForKey(&NSString::from_str("NSScreenNumber"))
                .and_then(|o| o.downcast::<NSNumber>().ok())
                .map(|n| n.unsignedIntValue())
                .unwrap_or(0);
            // The menu bar areas left and right of the notch; zero-sized without one
            let notch = (screen.safeAreaInsets().top > 0.0).then(|| {
                let left = screen.auxiliaryTopLeftArea();
                let right = screen.auxiliaryTopRightArea();
                (
                    left.origin.x + left.size.width - frame.origin.x,
                    right.origin.x - frame.origin.x,
                )
            });
            DisplayInfo {
                id,
                name: screen.localizedName().to_string(),
                frame,
                scale: screen.backingScaleFactor(),
                notch,
                large: frame.size.width >= large_width,
            }
        })
        .collect()
}
