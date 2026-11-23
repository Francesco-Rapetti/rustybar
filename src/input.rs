//! The text field of a popup (see `Kind::Input`): a field of macOS in a small panel of its own
//! over the popup, so that typing, the selection, paste and the accents are AppKit's. The panel
//! may take the keyboard without RustyBar becoming the active app, as Spotlight's does; the
//! popup's own windows never do. What's typed goes to the app (`app::typed`), Return and Esc too.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, Sel};
use objc2::{MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSColor, NSControl, NSControlTextEditingDelegate, NSEvent, NSEventModifierFlags, NSFocusRingType, NSLineBreakMode, NSPanel, NSTextField, NSTextFieldDelegate, NSTextView, NSWindow, NSWindowAnimationBehavior,
    NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSNotification, NSString};

use crate::app;
use crate::gfx::{self, Rgba};
use crate::ui::{Input, Kind};

define_class!(
    /// A borderless panel that takes the keyboard, which AppKit gives only to one with a title.
    #[unsafe(super(NSPanel, NSWindow))]
    #[thread_kind = MainThreadOnly]
    struct KeyPanel;

    impl KeyPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }

        // The bar has no Edit menu, where these shortcuts live
        #[unsafe(method(performKeyEquivalent:))]
        fn key_equivalent(&self, event: &NSEvent) -> bool {
            let flags = event.modifierFlags() & NSEventModifierFlags::DeviceIndependentFlagsMask;
            let key = event.charactersIgnoringModifiers().map(|c| c.to_string().to_lowercase()).unwrap_or_default();
            let action = match (flags == NSEventModifierFlags::Command, key.as_str()) {
                (true, "v") => Some(sel!(paste:)),
                (true, "c") => Some(sel!(copy:)),
                (true, "x") => Some(sel!(cut:)),
                (true, "a") => Some(sel!(selectAll:)),
                _ => None,
            };
            match action {
                Some(action) => unsafe { NSApplication::sharedApplication(self.mtm()).sendAction_to_from(action, None, Some(self)) },
                None => unsafe { msg_send![super(self), performKeyEquivalent: event] },
            }
        }
    }
);

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct Typing;

    unsafe impl NSObjectProtocol for Typing {}

    unsafe impl NSControlTextEditingDelegate for Typing {
        #[unsafe(method(controlTextDidChange:))]
        fn changed(&self, notification: &NSNotification) {
            if let Some(field) = notification.object().and_then(|o| o.downcast::<NSTextField>().ok()) {
                app::typed(field.stringValue().to_string(), false);
            }
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn command(&self, control: &NSControl, _view: &NSTextView, command: Sel) -> bool {
            if command == sel!(insertNewline:) {
                app::typed(control.stringValue().to_string(), true);
                true
            } else if command == sel!(cancelOperation:) {
                app::with(|app| app.close_popup());
                true
            } else {
                false
            }
        }
    }

    unsafe impl NSTextFieldDelegate for Typing {}
);

fn color(c: Rgba) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.0, c.1, c.2, c.3)
}

pub struct Field {
    panel: Retained<NSPanel>,
    field: Retained<NSTextField>,
    _typing: Retained<Typing>,
    parent: Retained<NSWindow>,
    /// The text the widget and the field agree on: what was typed last, or what the widget set.
    text: String,
    /// What the field looks like, to set it again only when it changes.
    look: Option<(gfx::FontSpec, Rgba)>,
    frame: CGRect,
}

impl Field {
    /// A field over `parent`, at `frame` (Cocoa coordinates), with the keyboard.
    pub fn new(mtm: MainThreadMarker, parent: &NSWindow, frame: CGRect) -> Field {
        let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
        let panel: Retained<KeyPanel> = unsafe {
            msg_send![KeyPanel::alloc(mtm), initWithContentRect: frame, styleMask: style, backing: NSBackingStoreType::Buffered, defer: false]
        };
        let panel: Retained<NSPanel> = Retained::into_super(panel);
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setOpaque(false);
        panel.setHasShadow(false);
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setHidesOnDeactivate(false);
        let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), CGRect::new(Default::default(), frame.size));
        field.setBordered(false);
        field.setBezeled(false);
        field.setDrawsBackground(false);
        field.setFocusRingType(NSFocusRingType::None);
        field.setLineBreakMode(NSLineBreakMode::ByClipping);
        field.setUsesSingleLineMode(true);
        if let Some(cell) = field.cell() {
            cell.setScrollable(true);
            cell.setWraps(false);
        }
        let typing: Retained<Typing> = unsafe { msg_send![Typing::alloc(mtm), init] };
        unsafe { field.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*typing))) };
        panel.setContentView(Some(&field));
        unsafe { parent.addChildWindow_ordered(&panel, NSWindowOrderingMode::Above) };
        panel.makeKeyWindow();
        panel.makeFirstResponder(Some(&field));
        Field { panel, field, _typing: typing, parent: parent.retain(), text: String::new(), look: None, frame }
    }

    /// The field over `parent`? One made for another window (a popup closed, another opened)
    /// isn't.
    pub fn over(&self, parent: &NSWindow) -> bool {
        std::ptr::eq(&*self.parent, parent)
    }

    /// Puts the field at `frame` (Cocoa coordinates), as `input` draws it; its text only when the
    /// widget's isn't what was typed.
    pub fn place(&mut self, frame: CGRect, input: &Input) {
        if self.frame != frame {
            self.panel.setFrame_display(frame, true);
            self.frame = frame;
        }
        let Kind::Input { text, font, color: ink, .. } = &input.node.kind else { return };
        if *text != self.text {
            self.field.setStringValue(&NSString::from_str(text));
            self.text = text.clone();
        }
        // The placeholder is the popup's, under the field (see `Kind::Input`)
        let look = (font.clone(), *ink);
        if self.look.as_ref() != Some(&look) {
            self.field.setFont(Some(&gfx::ns_font(font)));
            self.field.setTextColor(Some(&color(*ink)));
            self.look = Some(look);
        }
    }

    /// What was typed: the field has it already.
    pub fn typed(&mut self, text: &str) {
        self.text = text.to_string();
    }
}

impl Drop for Field {
    fn drop(&mut self) {
        self.parent.removeChildWindow(&self.panel);
        self.panel.orderOut(None);
    }
}
