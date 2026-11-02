//! The windows: a borderless, non-activating panel per bar and one for the popup, each with a
//! view that hosts a layer tree and forwards the mouse to the app. On the glass theme a bar has a
//! Liquid Glass view behind each pill, and a popup is one.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSColor, NSEvent, NSEventPhase, NSGlassEffectContainerView, NSGlassEffectView, NSGlassEffectViewStyle, NSPanel,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior,
    NSWindowLevel, NSWindowOrderingMode, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGRect};
use objc2_foundation::{NSNumber, NSString, NSValue, ns_string};
use objc2_quartz_core::{CABasicAnimation, CALayer, CAMediaTiming, CATransaction};

use crate::app;
use crate::ui::{Glass, ease};

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

/// What a popup shows behind its content, clipped to its rounded rect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Backdrop {
    None,
    /// A dark blur of what's behind it.
    Blur,
    /// Liquid Glass, light or dark as macOS is.
    Glass,
}

/// Liquid Glass is in macOS 26 and later.
fn has_glass() -> bool {
    AnyClass::get(c"NSGlassEffectView").is_some()
}

fn fill(view: &NSView) {
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
}

/// A blur of what's behind the window, clipped to a rounded rect.
fn blur(mtm: MainThreadMarker, frame: CGRect, radius: f64, material: NSVisualEffectMaterial) -> Retained<NSView> {
    let effect = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), frame);
    effect.setMaterial(material);
    effect.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    effect.setState(NSVisualEffectState::Active);
    effect.setWantsLayer(true);
    if let Some(layer) = effect.layer() {
        layer.setCornerRadius(radius);
        layer.setMasksToBounds(true);
    }
    Retained::into_super(effect)
}

/// A piece of Liquid Glass; before macOS 26, a blur that follows light and dark mode.
fn glass(mtm: MainThreadMarker, frame: CGRect, radius: f64, clear: bool) -> Retained<NSView> {
    if !has_glass() {
        return blur(mtm, frame, radius, NSVisualEffectMaterial::Popover);
    }
    let glass = NSGlassEffectView::initWithFrame(NSGlassEffectView::alloc(mtm), frame);
    glass.setCornerRadius(radius);
    let glass = Retained::into_super(glass);
    set_style(&glass, clear);
    glass
}

/// Clear glass, which bends the light at its edges and shows what's behind it almost as it is (the
/// Dock's), or regular, frosted and darker or lighter as macOS is (the menus').
fn set_style(view: &NSView, clear: bool) {
    if view.class().name() == c"NSGlassEffectView" {
        let style = if clear { NSGlassEffectViewStyle::Clear } else { NSGlassEffectViewStyle::Regular };
        let current: NSGlassEffectViewStyle = unsafe { msg_send![view, style] };
        if current != style {
            let _: () = unsafe { msg_send![view, setStyle: style] };
        }
    }
}

/// Animates `key` of `layer` from `from` to the value it has now, with the ease of the surface's
/// layers.
fn animate_from(layer: &CALayer, key: &NSString, from: &AnyObject, duration: f64) {
    let anim = CABasicAnimation::animationWithKeyPath(Some(key));
    unsafe { anim.setFromValue(Some(from)) };
    anim.setDuration(duration);
    anim.setTimingFunction(Some(&ease()));
    layer.addAnimation_forKey(&anim, Some(key));
}

/// The corner radius of a view made by `glass` or `blur`.
fn set_radius(view: &NSView, radius: f64) {
    if view.class().name() == c"NSGlassEffectView" {
        let _: () = unsafe { msg_send![view, setCornerRadius: radius] };
    } else if let Some(layer) = view.layer() {
        layer.setCornerRadius(radius);
    }
}

pub struct Window {
    pub panel: Retained<NSPanel>,
    view: Retained<SurfaceView>,
    /// A popup's blur or glass, the size of the window.
    backdrop: Option<Retained<NSView>>,
    /// Where a bar's glass views go, under the surface; made with the first.
    glass: Option<Retained<NSView>>,
    /// A bar's glass views, by the path of their node.
    pills: HashMap<String, Retained<NSView>>,
}

impl Window {
    /// A window showing `root`, at `frame` (Cocoa coordinates).
    pub fn new(mtm: MainThreadMarker, id: SurfaceId, frame: CGRect, level: NSWindowLevel, root: &CALayer, backdrop: Backdrop, radius: f64) -> Window {
        let panel = panel(mtm, frame, level);
        let bounds = CGRect::new(CGPoint::new(0.0, 0.0), frame.size);
        let view = SurfaceView::new(mtm, bounds, id);
        view.setLayer(Some(root));
        view.setWantsLayer(true);
        fill(&view);
        track_mouse(&view);
        let backdrop = match backdrop {
            Backdrop::None => None,
            Backdrop::Blur => Some(blur(mtm, bounds, radius, NSVisualEffectMaterial::HUDWindow)),
            Backdrop::Glass => Some(glass(mtm, bounds, radius, false)),
        };
        // The content of a glass view is its content view, which it draws on the glass
        let content = match &backdrop {
            Some(b) if b.class().name() == c"NSGlassEffectView" => {
                let _: () = unsafe { msg_send![&**b, setContentView: &*view] };
                b.clone()
            }
            Some(b) => {
                b.addSubview(&view);
                b.clone()
            }
            None => {
                let content = NSView::initWithFrame(NSView::alloc(mtm), bounds);
                content.addSubview(&view);
                content
            }
        };
        fill(&content);
        panel.setContentView(Some(&content));
        Window { panel, view, backdrop, glass: None, pills: HashMap::new() }
    }

    /// Puts a glass view behind each of `pills` (surface coordinates), moving, resizing and fading
    /// them as the layers do, so that each pill and its glass go together.
    pub fn set_glass(&mut self, pills: &[Glass], clear: bool, animate: bool) {
        if pills.is_empty() && self.pills.is_empty() {
            return;
        }
        let mtm = self.panel.mtm();
        let parent = self.glass_parent(mtm);
        let height = self.view.frame().size.height;
        let frame = |g: &Glass| CGRect::new(CGPoint::new(g.rect.origin.x, height - g.rect.origin.y - g.rect.size.height), g.rect.size);
        // The glass is drawn by SwiftUI, which takes a frame animated through AppKit a frame late,
        // behind the pill it's under: the move and the fades go on the view's layer, in the
        // transaction of the surface's layers, and only a new size through AppKit.
        let mut sizes = Vec::new();
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for g in pills {
            let to = frame(g);
            let Some(v) = self.pills.get(&g.path) else {
                let v = glass(mtm, to, g.radius, clear);
                parent.addSubview(&v);
                v.setAlphaValue(g.opacity);
                if animate && let Some(layer) = v.layer() {
                    animate_from(&layer, ns_string!("opacity"), &NSNumber::new_f64(0.0), 0.32);
                }
                self.pills.insert(g.path.clone(), v);
                continue;
            };
            set_radius(v, g.radius);
            set_style(v, clear);
            let from = v.frame();
            if from.origin != to.origin {
                let shown = v.layer().map(|l| unsafe { l.presentationLayer() }.unwrap_or(l).position());
                v.setFrameOrigin(to.origin);
                if animate && let (Some(layer), Some(shown)) = (v.layer(), shown) {
                    animate_from(&layer, ns_string!("position"), &NSValue::new(shown), 0.32);
                }
            }
            if from.size != to.size {
                sizes.push((v.clone(), to.size));
            }
            if (v.alphaValue() - g.opacity).abs() > f64::EPSILON {
                let shown = v.layer().map(|l| unsafe { l.presentationLayer() }.unwrap_or(l).opacity());
                v.setAlphaValue(g.opacity);
                if animate && let (Some(layer), Some(shown)) = (v.layer(), shown) {
                    animate_from(&layer, ns_string!("opacity"), &NSNumber::new_f32(shown), 0.32);
                }
            }
        }
        let live: HashSet<&str> = pills.iter().map(|g| g.path.as_str()).collect();
        for (_, v) in self.pills.extract_if(|path, _| !live.contains(path.as_str())) {
            // Gone with its pill, which fades out in 0.2 s
            let Some(layer) = v.layer().filter(|_| animate) else {
                v.removeFromSuperview();
                continue;
            };
            let shown = unsafe { layer.presentationLayer() }.unwrap_or(layer.clone()).opacity();
            let done = RcBlock::new(move || v.removeFromSuperview());
            CATransaction::begin();
            unsafe { CATransaction::setCompletionBlock(Some(&done)) };
            layer.setOpacity(0.0);
            animate_from(&layer, ns_string!("opacity"), &NSNumber::new_f32(shown), 0.2);
            CATransaction::commit();
        }
        CATransaction::commit();
        if sizes.is_empty() {
            return;
        }
        let group = RcBlock::new(move |ctx: NonNull<NSAnimationContext>| {
            let ctx = unsafe { ctx.as_ref() };
            ctx.setDuration(if animate { 0.32 } else { 0.0 });
            ctx.setTimingFunction(Some(&ease()));
            for (v, size) in &sizes {
                v.animator().setFrameSize(*size);
            }
        });
        NSAnimationContext::runAnimationGroup(&group);
    }

    /// The view the glass views go in: a glass container, which draws them together, under the
    /// surface.
    fn glass_parent(&mut self, mtm: MainThreadMarker) -> Retained<NSView> {
        if let Some(g) = &self.glass {
            return g.clone();
        }
        let bounds = self.view.frame();
        let parent = NSView::initWithFrame(NSView::alloc(mtm), bounds);
        fill(&parent);
        let holder = if has_glass() {
            let container = NSGlassEffectContainerView::initWithFrame(NSGlassEffectContainerView::alloc(mtm), bounds);
            container.setContentView(Some(&parent));
            Retained::into_super(container)
        } else {
            parent.clone()
        };
        fill(&holder);
        if let Some(content) = unsafe { self.view.superview() } {
            content.addSubview_positioned_relativeTo(&holder, NSWindowOrderingMode::Below, Some(&self.view));
        }
        self.glass = Some(parent.clone());
        parent
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
        if let Some(b) = &self.backdrop {
            set_radius(b, radius);
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        self.panel.orderOut(None);
        self.panel.close();
    }
}
