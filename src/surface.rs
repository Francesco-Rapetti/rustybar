//! The windows: a borderless, non-activating panel per bar and one for the popup, each with a
//! view that hosts a layer tree and forwards the mouse to the app.

use std::cell::Cell;

use objc2::rc::Retained;
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSColor, NSEvent, NSEventPhase, NSPanel,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior,
    NSWindowLevel, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_quartz_core::CALayer;

use crate::app;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceId {
    Bar(u32),
    Popup,
}

#[derive(Clone, Copy, Debug)]
pub enum Mouse {
    Down,
    Up,
    Dragged,
    Moved,
    Exited,
    RightDown,
    Scroll { dy: f64, ended: bool },
}

pub struct ViewIvars {
    id: Cell<SurfaceId>,
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ViewIvars]
    pub struct SurfaceView;

    impl SurfaceView {
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.send(Mouse::Down, event);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.send(Mouse::Up, event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.send(Mouse::Dragged, event);
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.send(Mouse::Moved, event);
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, event: &NSEvent) {
            self.send(Mouse::Exited, event);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.send(Mouse::RightDown, event);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            // Trackpads give points and momentum; wheels give lines
            let dy = if event.hasPreciseScrollingDeltas() {
                -event.scrollingDeltaY()
            } else {
                -event.scrollingDeltaY() * 16.0
            };
            let phase = event.phase();
            let momentum = event.momentumPhase();
            let ended = momentum.contains(NSEventPhase::Ended)
                || (phase.contains(NSEventPhase::Ended) && momentum == NSEventPhase::None)
                || (!event.hasPreciseScrollingDeltas());
            self.send(Mouse::Scroll { dy, ended }, event);
        }
    }
);

impl SurfaceView {
    fn new(mtm: MainThreadMarker, frame: CGRect, id: SurfaceId) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars { id: Cell::new(id) });
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }


    fn send(&self, kind: Mouse, event: &NSEvent) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        let id = self.ivars().id.get();
        app::mouse(id, kind, p);
    }
}

fn panel(mtm: MainThreadMarker, frame: CGRect, level: NSWindowLevel) -> Retained<NSPanel> {
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        frame,
        NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
        NSBackingStoreType::Buffered,
        false,
    );
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setOpaque(false);
    panel.setHasShadow(false);
    panel.setBackgroundColor(Some(&NSColor::clearColor()));
    panel.setLevel(level);
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    panel.setAcceptsMouseMovedEvents(true);
    panel.setHidesOnDeactivate(false);
    panel.setFloatingPanel(true);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel
}

fn track_mouse(view: &NSView) {
    let tracking = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            CGRect::ZERO,
            NSTrackingAreaOptions::MouseMoved
                | NSTrackingAreaOptions::MouseEnteredAndExited
                | NSTrackingAreaOptions::ActiveAlways
                | NSTrackingAreaOptions::InVisibleRect,
            Some(view),
            None,
        )
    };
    view.addTrackingArea(&tracking);
}

pub struct Window {
    pub panel: Retained<NSPanel>,
    pub effect: Option<Retained<NSVisualEffectView>>,
}

impl Window {
    /// A window showing `root`, at `frame` (Cocoa coordinates).
    pub fn new(mtm: MainThreadMarker, id: SurfaceId, frame: CGRect, level: NSWindowLevel, root: &CALayer, blur: Option<f64>) -> Window {
        let panel = panel(mtm, frame, level);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), frame.size);
        let view = SurfaceView::new(mtm, bounds, id);
        view.setLayer(Some(root));
        view.setWantsLayer(true);
        view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
        track_mouse(&view);
        let effect = blur.map(|radius| {
            // The blur of what's behind the popup, clipped to its rounded rect
            let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), bounds);
            effect.setMaterial(NSVisualEffectMaterial::HUDWindow);
            effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            effect.setState(NSVisualEffectState::Active);
            effect.setWantsLayer(true);
            if let Some(layer) = effect.layer() {
                layer.setCornerRadius(radius);
                layer.setMasksToBounds(true);
            }
            effect.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
            effect.addSubview(&view);
            effect
        });
        match &effect {
            Some(e) => panel.setContentView(Some(e)),
            None => panel.setContentView(Some(&view)),
        }
        Window { panel, effect }
    }

    pub fn set_frame(&self, frame: CGRect) {
        if self.panel.frame() != frame {
            self.panel.setFrame_display(frame, true);
        }
    }

    pub fn show(&self) {
        self.panel.orderFrontRegardless();
    }


    pub fn set_corner_radius(&self, radius: f64) {
        if let Some(layer) = self.effect.as_ref().and_then(|e| e.layer()) {
            layer.setCornerRadius(radius);
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
}
