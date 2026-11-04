//! Turns a laid-out node tree into a tree of CALayers, reusing and animating the layers of the
//! previous render. Core Animation runs the animations in the window server, so a bar that isn't
//! changing costs nothing, and one that is costs a few property sets.

use std::collections::{HashMap, HashSet};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFRetained, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGMutablePath;
use objc2_foundation::{NSArray, NSNumber, NSObjectNSKeyValueCoding, ns_string};
use objc2_quartz_core::{
    CAAnimation, CABasicAnimation, CAFrameRateRange, CAGradientLayer, CAKeyframeAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction,
    CAShapeLayer, CASpringAnimation, CATransaction, kCACornerCurveContinuous, kCAGravityCenter, kCAGravityLeft,
};

use super::layout::Laid;
use super::node::{Action, Kind, Node, Notch};
use crate::gfx::{self, FontSpec, Rgba};

/// An interactive region of a surface, in its coordinates (top-left origin).
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub rect: CGRect,
    pub action: Option<Action>,
    pub hover: Option<Rgba>,
    pub kind: HitKind,
}

/// Where Liquid Glass goes behind a glass node, in surface coordinates (top-left origin).
#[derive(Clone, Debug, PartialEq)]
pub struct Glass {
    pub path: String,
    pub rect: CGRect,
    pub radius: f64,
    pub opacity: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HitKind {
    Click,
    Slider,
    Scroll,
}

struct Entry {
    layer: Retained<CALayer>,
    /// What the contents show, to skip rasterizing when it didn't change.
    sig: Option<String>,
    /// Slider track, fill, labels and knob, progress fill, toggle knob, shimmer, scroll content,
    /// selection.
    parts: HashMap<&'static str, Retained<CALayer>>,
    background: Option<Rgba>,
    frame: CGRect,
    generation: u64,
    /// A slider's colors and font, to draw it again while it's dragged.
    slider: Option<SliderLook>,
    /// The mask of a node whose corners aren't all as round, with the corners and size it has.
    corners: Option<(Retained<CAShapeLayer>, Corners, (f64, f64))>,
    /// A text that scrolls (see `Style::marquee`).
    marquee: Option<Marquee>,
    /// The mask of a node shaped as the notch (see `Notch`), with what its path was made of.
    notch: Option<(Retained<CAShapeLayer>, [f64; 7])>,
}

/// The layers of a text that scrolls: the strip with the text and a copy of it after a gap, which
/// moves, and the mask that fades the text where the box cuts it.
struct Marquee {
    strip: Retained<CALayer>,
    fade: Retained<CAGradientLayer>,
    size: (f64, f64),
}

/// Between the end of a scrolling text and its copy.
const MARQUEE_GAP: f64 = 32.0;
/// Points a second.
const MARQUEE_SPEED: f64 = 30.0;
/// Seconds at the start, before each turn.
const MARQUEE_WAIT: f64 = 3.0;
/// How wide the fade is where the box cuts the text.
const MARQUEE_FADE: f64 = 12.0;

/// Corner radii: top left, top right, bottom right, bottom left.
type Corners = [f64; 4];

/// The bars of `Kind::Bars`: how many, how wide, how far apart.
const BARS: usize = 4;
const BAR_W: f64 = 3.0;
const BAR_GAP: f64 = 2.0;
pub const BARS_WIDTH: f64 = BARS as f64 * BAR_W + (BARS - 1) as f64 * BAR_GAP;
/// Each bar's lowest height (a share of the node's), the seconds it takes to rise, and where it
/// starts in its cycle: out of step with one another, as the sound would have them.
const BAR_LOW: [f64; BARS] = [0.3, 0.25, 0.35, 0.2];
const BAR_TIME: [f64; BARS] = [0.47, 0.71, 0.38, 0.59];
const BAR_PHASE: [f64; BARS] = [0.1, 0.5, 0.25, 0.4];
/// Their heights when they stand still.
const BAR_STILL: [f64; BARS] = [0.55, 0.9, 0.45, 0.7];

/// The rounded node that holds the ones inside it, in surface coordinates.
#[derive(Clone, Copy)]
struct Container {
    rect: CGRect,
    corners: Corners,
}

#[derive(Clone)]
struct SliderLook {
    color: Rgba,
    track: Rgba,
    font: Option<FontSpec>,
    labels: Vec<String>,
    value: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ScrollState {
    pub offset: f64,
    /// Where the content starts in the viewport when not scrolled (the scroll's padding).
    pub top: f64,
    pub content_height: f64,
    pub viewport: f64,
    /// The content was laid out already once (so `start` isn't applied again).
    pub started: bool,
}

pub struct Surface {
    pub root: Retained<CALayer>,
    pub scale: f64,
    pub animate: bool,
    /// Motion that goes on by itself (a text that scrolls); off when the Mac asks for less motion.
    pub motion: bool,
    pub hits: Vec<Hit>,
    pub scrolls: HashMap<String, ScrollState>,
    pub hovered: Option<String>,
    /// The highlight the hovered node shows.
    hover_color: Option<Rgba>,
    pub highlight: Rgba,
    /// The glass nodes of the last render, for the window to put its glass views behind them.
    pub glass: Vec<Glass>,
    /// Draws the glass nodes with this fill instead (a popup, which is glass already).
    pub glass_fill: Option<Rgba>,
    /// The stiffness and damping of the spring that changes of a notch's shape follow (see
    /// `Notch`), instead of the ease of the rest.
    pub spring: Option<(f64, f64)>,
    entries: HashMap<String, Entry>,
    generation: u64,
    /// Sliders being dragged keep their value until released.
    pub dragging: Option<(String, f64)>,
    /// The slider under the mouse, which shows its knob.
    pub slider_hovered: Option<String>,
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

fn cg_obj(c: &objc2_core_foundation::CFRetained<objc2_core_graphics::CGColor>) -> &AnyObject {
    let t: &CFType = c;
    t.as_ref()
}

/// The ease of every change: quick at first, settling gently (an ease-out quint).
pub fn ease() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(0.22, 1.0, 0.36, 1.0)
}

pub fn transaction<R>(animate: bool, duration: f64, f: impl FnOnce() -> R) -> R {
    CATransaction::begin();
    CATransaction::setDisableActions(!animate);
    CATransaction::setAnimationDuration(duration);
    CATransaction::setAnimationTimingFunction(Some(&ease()));
    let r = f();
    CATransaction::commit();
    r
}

impl Surface {
    pub fn new(scale: f64, highlight: Rgba) -> Surface {
        let root = CALayer::new();
        root.setGeometryFlipped(true);
        root.setContentsScale(scale);
        Surface {
            root,
            scale,
            animate: true,
            motion: true,
            hits: Vec::new(),
            scrolls: HashMap::new(),
            hovered: None,
            hover_color: None,
            highlight,
            glass: Vec::new(),
            glass_fill: None,
            spring: None,
            entries: HashMap::new(),
            generation: 0,
            dragging: None,
            slider_hovered: None,
        }
    }

    pub fn set_scale(&mut self, scale: f64) {
        if (scale - self.scale).abs() > f64::EPSILON {
            self.scale = scale;
            // Every bitmap was drawn for the old scale: draw them again
            for e in self.entries.values_mut() {
                e.sig = None;
                e.layer.setContentsScale(scale);
            }
        }
    }

    /// Updates the layers to show `node` laid out as `laid`.
    pub fn render(&mut self, node: &Node, laid: &Laid) {
        self.generation += 1;
        self.hits.clear();
        self.glass.clear();
        let animate = self.animate;
        CATransaction::begin();
        CATransaction::setDisableActions(!animate);
        CATransaction::setAnimationDuration(0.32);
        CATransaction::setAnimationTimingFunction(Some(&ease()));
        let root = self.root.clone();
        let path = self.child_path("", node);
        self.walk(node, laid, &root, path, 0.0, 0.0, None, None);
        self.sweep();
        self.refresh_hover();
        CATransaction::commit();
    }

    /// Gives the node under the mouse the highlight it has now, which a render can change without
    /// the mouse moving: a tab clicked takes the one of the selected tab, instead of keeping the
    /// opaque one it had over the selection sliding in. One that can't be clicked anymore loses it.
    fn refresh_hover(&mut self) {
        let Some(path) = self.hovered.clone() else { return };
        let color = self.hits.iter().rev().find(|h| h.kind == HitKind::Click && h.path == path).and_then(|h| h.hover);
        if color == self.hover_color {
            return;
        }
        let Some(e) = self.entries.get(&path) else { return };
        match color {
            Some(c) => e.layer.setBackgroundColor(Some(&c.cg())),
            None => {
                e.layer.setBackgroundColor(e.background.map(|c| c.cg()).as_deref());
                self.hovered = None;
            }
        }
        self.hover_color = color;
    }

    /// Fades out and removes the layers of the nodes that are gone.
    fn sweep(&mut self) {
        let generation = self.generation;
        let gone: Vec<String> = self.entries.iter().filter(|(_, e)| e.generation != generation).map(|(k, _)| k.clone()).collect();
        let gone_set: HashSet<&String> = gone.iter().collect();
        for path in &gone {
            let Some(e) = self.entries.remove(path) else { continue };
            // A child of a removed node goes away with its parent's layer
            let parent_gone = path.rfind('/').map(|i| gone_set.contains(&path[..i].to_string())).unwrap_or(false);
            if parent_gone {
                continue;
            }
            if self.animate {
                let layer = e.layer.clone();
                CATransaction::begin();
                CATransaction::setAnimationDuration(0.2);
                let block = block2::RcBlock::new(move || layer.removeFromSuperlayer());
                unsafe { CATransaction::setCompletionBlock(Some(&block)) };
                e.layer.setOpacity(0.0);
                CATransaction::commit();
            } else {
                e.layer.removeFromSuperlayer();
            }
        }
        self.scrolls.retain(|k, _| self.entries.contains_key(k));
        if self.hovered.as_ref().is_some_and(|h| !self.entries.contains_key(h)) {
            self.hovered = None;
            self.hover_color = None;
        }
        if self.slider_hovered.as_ref().is_some_and(|h| !self.entries.contains_key(h)) {
            self.slider_hovered = None;
        }
    }

    /// The path of `node` under `parent_path`: its key, or its kind and its order among the
    /// unkeyed siblings of the same kind already walked in this render.
    fn child_path(&self, parent_path: &str, node: &Node) -> String {
        let id = match &node.key {
            Some(k) => format!("k:{k}"),
            None => kind_tag(&node.kind).to_string(),
        };
        let mut path = format!("{parent_path}/{id}");
        let mut n = 1;
        while self.entries.get(&path).is_some_and(|e| e.generation == self.generation) {
            n += 1;
            path = format!("{parent_path}/{id}#{n}");
        }
        path
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        &mut self,
        node: &Node,
        laid: &Laid,
        parent: &CALayer,
        path: String,
        ox: f64,
        oy: f64,
        clip: Option<CGRect>,
        container: Option<Container>,
    ) {
        let frame = rect(laid.x, laid.y, laid.w, laid.h);
        let abs = rect(ox + laid.x, oy + laid.y, laid.w, laid.h);
        let created = !self.entries.contains_key(&path);
        if created {
            let layer = CALayer::new();
            layer.setContentsScale(self.scale);
            layer.setCornerCurve(unsafe { kCACornerCurveContinuous });
            transaction(false, 0.0, || {
                layer.setFrame(frame);
                if self.animate {
                    layer.setOpacity(0.0);
                }
            });
            parent.addSublayer(&layer);
            self.entries.insert(
                path.clone(),
                Entry {
                    layer,
                    sig: None,
                    parts: HashMap::new(),
                    background: None,
                    frame,
                    generation: self.generation,
                    slider: None,
                    corners: None,
                    marquee: None,
                    notch: None,
                },
            );
        }
        let scale = self.scale;
        let motion = self.motion;
        let spring = self.spring.filter(|_| self.animate);
        let hovered = self.hovered.as_deref() == Some(path.as_str());
        let highlight = self.highlight;
        let glass_fill = self.glass_fill;
        let dragging = self.dragging.clone();
        let knob = self.slider_hovered.as_deref() == Some(path.as_str()) || dragging.as_ref().is_some_and(|(p, _)| *p == path);
        let entry = self.entries.get_mut(&path).unwrap();
        entry.generation = self.generation;
        let layer = entry.layer.clone();

        let s = &node.style;
        if entry.frame != frame || created {
            // A notch's shape moves with its mask alone: bounds that moved too would cut its
            // background short of the mask, and not what's in it
            if s.notch.is_some() {
                transaction(false, 0.0, || layer.setFrame(frame));
            } else {
                layer.setFrame(frame);
            }
            entry.frame = frame;
        }
        let opacity = s.opacity as f32;
        if (layer.opacity() - opacity).abs() > f32::EPSILON {
            layer.setOpacity(opacity);
        }
        let background = if s.glass { glass_fill.or(s.background) } else { s.background };
        if entry.background != background || created {
            entry.background = background;
            if !hovered {
                layer.setBackgroundColor(background.map(|c| c.cg()).as_deref());
            }
        }
        // A fill (the background, or the hover highlight of a click) follows the corners of the
        // node it's in: a corner near one of the container's is concentric with it
        let fills = matches!(node.kind, Kind::Box) && s.border.is_none() && !s.glass && (s.background.is_some() || node.on_click.is_some());
        let corners = if fills { concentric(abs, s.radius, container) } else { [s.radius; 4] };
        // What's in a notch is in its shape, which is round at the bottom only
        let notched = s.notch.map(|n| {
            set_notch(entry, &layer, n, s.radius, laid.w, laid.h, spring);
            let (x, w, h) = n.area.map_or((0.0, laid.w, laid.h), |[x, w, h]| (x, w, h));
            Container { rect: rect(abs.origin.x + x + n.ear, abs.origin.y, (w - 2.0 * n.ear).max(0.0), h), corners: [0.0, 0.0, s.radius, s.radius] }
        });
        if notched.is_none() {
            if entry.notch.take().is_some() {
                unsafe { layer.setMask(None) };
            }
            set_corners(entry, &layer, corners, laid.w, laid.h);
        }
        match s.border {
            Some((w, c)) => {
                layer.setBorderWidth(w);
                layer.setBorderColor(Some(&c.cg()));
            }
            None => layer.setBorderWidth(0.0),
        }

        // What the layer itself draws
        let scrolls = s.marquee && motion && laid.text.is_some() && matches!(node.kind, Kind::Text { .. });
        if !scrolls && entry.marquee.is_some() {
            stop_marquee(entry, &layer);
        }
        match &node.kind {
            Kind::Text { text, font, color } if scrolls => {
                let sig = format!("m|{text}|{font:?}|{color:?}|{scale}");
                if entry.sig.as_deref() != Some(&sig) {
                    start_marquee(entry, &layer, text, font, *color, laid.h, scale);
                    entry.sig = Some(sig);
                }
                if let Some(m) = entry.marquee.as_mut() {
                    fit_fade(m, laid.w, laid.h);
                }
            }
            Kind::Text { text, font, color } => {
                let shown = laid.text.as_deref().unwrap_or(text);
                let sig = format!("t|{shown}|{font:?}|{color:?}|{scale}");
                if entry.sig.as_deref() != Some(&sig) {
                    if let Some(b) = gfx::text(shown, font, *color, scale) {
                        unsafe { layer.setContents(Some(b.as_contents())) };
                        layer.setContentsGravity(unsafe { kCAGravityLeft });
                    }
                    entry.sig = Some(sig);
                }
            }
            Kind::Symbol { name, size, weight, color } => {
                let sig = format!("s|{name}|{size}|{weight:?}|{color:?}|{scale}");
                if entry.sig.as_deref() != Some(&sig) {
                    if let Some(b) = gfx::symbol(name, *size, *weight, *color, scale) {
                        unsafe { layer.setContents(Some(b.as_contents())) };
                        layer.setContentsGravity(unsafe { kCAGravityCenter });
                    }
                    entry.sig = Some(sig);
                }
            }
            Kind::AppIcon { bundle_id, size } => {
                let sig = format!("a|{bundle_id}|{size}|{scale}");
                if entry.sig.as_deref() != Some(&sig) {
                    match gfx::app_icon(bundle_id, *size, scale) {
                        Some(b) => unsafe { layer.setContents(Some(b.as_contents())) },
                        None => unsafe { layer.setContents(None) },
                    }
                    layer.setContentsGravity(unsafe { kCAGravityCenter });
                    entry.sig = Some(sig);
                }
            }
            Kind::Image { path: file, width, height, tint } => {
                // A radius rounds the picture too (a cover), not only a background
                layer.setMasksToBounds(s.radius > 0.0);
                let sig = format!("i|{file}|{width}|{height}|{tint:?}|{scale}");
                if entry.sig.as_deref() != Some(&sig) {
                    match gfx::image_file(file, *width, *height, *tint, scale) {
                        Some(b) => unsafe { layer.setContents(Some(b.as_contents())) },
                        None => unsafe { layer.setContents(None) },
                    }
                    layer.setContentsGravity(unsafe { kCAGravityCenter });
                    entry.sig = Some(sig);
                }
            }
            Kind::Progress { value, color, track } => {
                let h = laid.h;
                layer.setBackgroundColor(Some(&track.cg()));
                layer.setCornerRadius(h / 2.0);
                let fill = part(entry, "fill", &layer, scale);
                fill.setCornerRadius(h / 2.0);
                fill.setBackgroundColor(Some(&color.cg()));
                let w = (laid.w * value.clamp(0.0, 1.0)).max(if *value > 0.0 { h } else { 0.0 });
                fill.setFrame(rect(0.0, 0.0, w, h));
            }
            Kind::Slider { value, color, track, font, labels } => {
                let v = match &dragging {
                    Some((p, v)) if *p == path => *v,
                    _ => snap(value.clamp(0.0, 1.0), labels.len()),
                };
                entry.slider = Some(SliderLook { color: *color, track: *track, font: font.clone(), labels: labels.clone(), value: v });
                layout_slider(entry, &layer, laid.w, laid.h, v, knob, scale);
            }
            Kind::Toggle { on, color, track } => {
                let h = laid.h;
                layer.setCornerRadius(h / 2.0);
                layer.setBackgroundColor(Some(&if *on { *color } else { *track }.cg()));
                let knob = part(entry, "knob", &layer, scale);
                let k = h - 4.0;
                knob.setCornerRadius(k / 2.0);
                knob.setBackgroundColor(Some(&Rgba(1.0, 1.0, 1.0, 1.0).cg()));
                knob.setFrame(rect(if *on { laid.w - k - 2.0 } else { 2.0 }, 2.0, k, k));
            }
            Kind::Skeleton => {
                if !entry.parts.contains_key("shimmer") {
                    let shimmer = CAGradientLayer::new();
                    let base = s.background.unwrap_or(highlight);
                    let shine = base.mix(Rgba(1.0, 1.0, 1.0, base.3), 0.12);
                    let colors = [base.cg(), shine.cg(), base.cg()];
                    let objs: Vec<&AnyObject> = colors.iter().map(cg_obj).collect();
                    unsafe { shimmer.setColors(Some(&NSArray::from_slice(&objs))) };
                    shimmer.setStartPoint(CGPoint::new(0.0, 0.5));
                    shimmer.setEndPoint(CGPoint::new(1.0, 0.5));
                    let locs = |a: f64| NSArray::from_retained_slice(&[NSNumber::new_f64(a), NSNumber::new_f64(a + 0.5), NSNumber::new_f64(a + 1.0)]);
                    shimmer.setLocations(Some(&locs(-1.0)));
                    let anim = CABasicAnimation::animationWithKeyPath(Some(ns_string!("locations")));
                    unsafe {
                        anim.setFromValue(Some(&locs(-1.0)));
                        anim.setToValue(Some(&locs(1.0)));
                    }
                    anim.setDuration(1.4);
                    anim.setRepeatCount(f32::INFINITY);
                    shimmer.addAnimation_forKey(&anim, Some(ns_string!("shimmer")));
                    shimmer.setCornerRadius(s.radius);
                    layer.addSublayer(&shimmer);
                    entry.parts.insert("shimmer", Retained::into_super(shimmer));
                }
                let sh = &entry.parts["shimmer"];
                sh.setFrame(rect(0.0, 0.0, laid.w, laid.h));
                sh.setCornerRadius(s.radius);
            }
            Kind::Bars { color } => layout_bars(entry, &layer, *color, laid.w, laid.h, motion, scale),
            Kind::Box | Kind::Scroll { .. } => {
                if entry.sig.is_some() {
                    unsafe { layer.setContents(None) };
                    entry.sig = None;
                }
                if matches!(node.kind, Kind::Box) && layer.masksToBounds() != s.clip {
                    layer.setMasksToBounds(s.clip);
                }
            }
        }

        // The sliding highlight behind the selected child
        if let (Some(sel), Some(color)) = (&node.selected, node.selected_color) {
            let idx = node.children.iter().position(|c| c.key.as_deref() == Some(sel.as_str()));
            let ind_new = !entry.parts.contains_key("selection");
            let ind = part(entry, "selection", &layer, scale);
            if ind_new {
                ind.setCornerCurve(unsafe { kCACornerCurveContinuous });
                layer.insertSublayer_atIndex(&ind, 0);
            }
            match idx.and_then(|i| laid.children.get(i).map(|l| (i, l))) {
                Some((i, l)) => {
                    let r = node.children[i].style.radius.max(s.radius - 2.0).max(0.0);
                    if ind_new {
                        transaction(false, 0.0, || ind.setFrame(rect(l.x, l.y, l.w, l.h)));
                    }
                    ind.setFrame(rect(l.x, l.y, l.w, l.h));
                    ind.setCornerRadius(r);
                    ind.setBackgroundColor(Some(&color.cg()));
                    ind.setOpacity(1.0);
                }
                None => ind.setOpacity(0.0),
            }
        } else if let Some(ind) = entry.parts.remove("selection") {
            ind.removeFromSuperlayer();
        }

        // Interactions and glass, in surface coordinates
        if s.glass && glass_fill.is_none() {
            self.glass.push(Glass { path: path.clone(), rect: abs, radius: s.radius, opacity: s.opacity });
        }
        let visible = match clip {
            Some(c) => intersect(abs, c),
            None => Some(abs),
        };
        if let Some(r) = visible {
            let kind = match node.kind {
                // One with nothing to tell when released stays where it is
                Kind::Slider { .. } if node.on_click.is_some() => Some(HitKind::Slider),
                Kind::Slider { .. } => None,
                Kind::Scroll { .. } => Some(HitKind::Scroll),
                _ if node.on_click.is_some() => Some(HitKind::Click),
                _ => None,
            };
            if let Some(kind) = kind {
                let hover = match (kind, node.hover) {
                    // A clear one: none at all, not even the theme's
                    (_, Some(c)) if c.3 == 0.0 => None,
                    (HitKind::Click, hover) => Some(hover.unwrap_or(highlight)),
                    (_, hover) => hover,
                };
                self.hits.push(Hit { path: path.clone(), rect: r, action: node.on_click.clone(), hover, kind });
            }
        }

        // Children, in this node if it's rounded
        let container = match notched {
            Some(n) => Some(n),
            None if corners.iter().any(|&r| r > 0.0) => Some(Container { rect: abs, corners }),
            None => container,
        };
        if let Kind::Scroll { start, .. } = &node.kind {
            let (Some(content), Some(content_laid)) = (node.children.first(), laid.children.first()) else { return };
            let content_path = format!("{path}/k:content");
            let old = self.scrolls.get(&path).copied().unwrap_or_default();
            let mut state = old;
            state.viewport = laid.h;
            state.content_height = content_laid.h;
            state.top = content_laid.y;
            if !old.started {
                state.offset = start
                    .as_ref()
                    .and_then(|k| content.children.iter().position(|c| c.key.as_deref() == Some(k.as_str())))
                    .and_then(|i| content_laid.children.get(i))
                    .map(|l| l.y)
                    .unwrap_or(0.0);
                state.started = true;
            } else if let Some((key, delta)) = self.anchor(&content_path, content, old.offset) {
                // Keep the child at the top where it was, if the content grew above it
                let i = content.children.iter().position(|c| c.key.as_deref() == Some(key.as_str()));
                if let Some(l) = i.and_then(|i| content_laid.children.get(i)) {
                    state.offset = l.y - delta;
                }
            }
            state.offset = state.offset.clamp(0.0, (state.content_height - state.viewport).max(0.0));
            self.scrolls.insert(path.clone(), state);
            layer.setMasksToBounds(true);
            let mut shifted = content_laid.clone();
            shifted.y -= state.offset;
            let viewport = rect(ox + laid.x, oy + laid.y, laid.w, laid.h);
            let clip = Some(match clip {
                Some(c) => intersect(viewport, c).unwrap_or(rect(0.0, 0.0, 0.0, 0.0)),
                None => viewport,
            });
            // The corners of what's inside as when it isn't scrolled, so they don't change as it
            // scrolls: the container goes up with the content
            let container = container.map(|mut c| {
                c.rect.origin.y -= state.offset;
                c
            });
            self.walk(content, &shifted, &layer, content_path, ox + laid.x, oy + laid.y, clip, container);
            return;
        }
        // What a clipping node doesn't show can't be clicked either
        let clip = match (s.clip, clip) {
            (true, Some(c)) => Some(intersect(abs, c).unwrap_or(rect(0.0, 0.0, 0.0, 0.0))),
            (true, None) => Some(abs),
            (false, c) => c,
        };
        for (child, child_laid) in node.children.iter().zip(&laid.children) {
            let child_path = self.child_path(&path, child);
            self.walk(child, child_laid, &layer, child_path, ox + laid.x, oy + laid.y, clip, container);
        }
    }

    /// The first child of a scroll's content that shows at the top, with how far below the top of
    /// the viewport it starts (negative when partly above).
    fn anchor(&self, content_path: &str, content: &Node, offset: f64) -> Option<(String, f64)> {
        let mut best: Option<(String, f64)> = None;
        for c in &content.children {
            let Some(key) = &c.key else { continue };
            let Some(e) = self.entries.get(&format!("{content_path}/k:{key}")) else { continue };
            let bottom = e.frame.origin.y + e.frame.size.height;
            if bottom > offset {
                let delta = e.frame.origin.y - offset;
                if best.as_ref().is_none_or(|(_, d)| delta < *d) {
                    best = Some((key.clone(), delta));
                }
            }
        }
        best
    }

    pub fn hit(&self, p: CGPoint, kinds: &[HitKind]) -> Option<&Hit> {
        // The last one drawn is on top
        self.hits.iter().rev().find(|h| kinds.contains(&h.kind) && contains(h.rect, p))
    }

    /// Highlights the hovered node, restoring the previous one.
    pub fn set_hover(&mut self, path: Option<String>, color: Option<Rgba>) {
        if self.hovered == path {
            return;
        }
        let animate = self.animate;
        transaction(animate, 0.18, || {
            if let Some(old) = self.hovered.take()
                && let Some(e) = self.entries.get(&old) {
                    e.layer.setBackgroundColor(e.background.map(|c| c.cg()).as_deref());
                }
            if let (Some(p), Some(c)) = (&path, color)
                && let Some(e) = self.entries.get(p) {
                    e.layer.setBackgroundColor(Some(&c.cg()));
                }
        });
        self.hover_color = if path.is_some() { color } else { None };
        self.hovered = path;
    }

    /// The spring of a press: the node shrinks a little and bounces back.
    pub fn press(&self, path: &str) {
        if !self.animate {
            return;
        }
        let Some(e) = self.entries.get(path) else { return };
        let anim = CASpringAnimation::animationWithKeyPath(Some(ns_string!("transform.scale")));
        anim.setMass(1.0);
        anim.setStiffness(500.0);
        anim.setDamping(15.0);
        anim.setInitialVelocity(0.0);
        unsafe {
            anim.setFromValue(Some(&NSNumber::new_f64(0.92)));
            anim.setToValue(Some(&NSNumber::new_f64(1.0)));
        }
        anim.setDuration(anim.settlingDuration());
        e.layer.addAnimation_forKey(&anim, Some(ns_string!("press")));
    }

    /// Moves a slider while it's dragged, and its percentage with it, without a full render. One
    /// with steps jumps to the nearest, quickly animated.
    pub fn drag_slider(&mut self, path: &str, value: f64) {
        let steps = self.entries.get(path).and_then(|e| e.slider.as_ref()).map_or(0, |s| s.labels.len());
        let value = snap(value, steps);
        self.dragging = Some((path.to_string(), value));
        let (scale, animate) = (self.scale, self.animate && steps >= 2);
        let Some(e) = self.entries.get_mut(path) else { return };
        let (w, h) = (e.frame.size.width, e.frame.size.height);
        let layer = e.layer.clone();
        transaction(animate, 0.12, || layout_slider(e, &layer, w, h, value, true, scale));
    }

    /// Shows the knob of the slider under the mouse, and moves its percentage out of the way;
    /// hides the previous one's, unless it's being dragged.
    pub fn set_slider_hover(&mut self, path: Option<String>) {
        if self.slider_hovered == path {
            return;
        }
        let dragged = self.dragging.as_ref().map(|(p, _)| p.clone());
        let hidden = self.slider_hovered.take().filter(|p| dragged.as_ref() != Some(p));
        let changes: Vec<(String, bool)> = hidden.map(|p| (p, false)).into_iter().chain(path.clone().map(|p| (p, true))).collect();
        let scale = self.scale;
        transaction(self.animate, 0.18, || {
            for (p, shown) in changes {
                let Some(e) = self.entries.get_mut(&p) else { continue };
                let Some(value) = e.slider.as_ref().map(|s| s.value) else { continue };
                let (w, h, layer) = (e.frame.size.width, e.frame.size.height, e.layer.clone());
                layout_slider(e, &layer, w, h, value, shown, scale);
            }
        });
        self.slider_hovered = path;
    }

    /// Scrolls a scroll node by `dy` points. Past the ends it moves with resistance and `settle`
    /// brings it back. Returns "top"/"bottom" when an end is within a viewport.
    pub fn scroll_by(&mut self, path: &str, dy: f64, elastic: bool) -> Option<&'static str> {
        let state = self.scrolls.get_mut(path)?;
        let max = (state.content_height - state.viewport).max(0.0);
        let mut next = state.offset + dy;
        if next < 0.0 || next > max {
            if elastic {
                let over = if next < 0.0 { next } else { next - max };
                let resisted = over / (1.0 + over.abs() / 60.0);
                next = if next < 0.0 { resisted } else { max + resisted };
            } else {
                next = next.clamp(0.0, max);
            }
        }
        state.offset = next;
        let state = *state;
        self.apply_scroll(path, state.offset, false);
        if state.offset < state.viewport {
            Some("top")
        } else if state.offset > max - state.viewport {
            Some("bottom")
        } else {
            None
        }
    }

    /// Brings a scroll that went past an end back inside, animated.
    pub fn settle(&mut self, path: &str) {
        let Some(state) = self.scrolls.get_mut(path) else { return };
        let max = (state.content_height - state.viewport).max(0.0);
        let clamped = state.offset.clamp(0.0, max);
        if (clamped - state.offset).abs() > 0.1 {
            state.offset = clamped;
            self.apply_scroll(path, clamped, true);
        }
    }

    /// Moves the content of a scroll to `offset`. The hits inside it keep their old place until
    /// the next render, which the caller asks for when the gesture ends.
    fn apply_scroll(&mut self, path: &str, offset: f64, animate: bool) {
        let top = self.scrolls.get(path).map(|s| s.top).unwrap_or(0.0);
        let Some(e) = self.entries.get_mut(&format!("{path}/k:content")) else { return };
        e.frame.origin.y = top - offset;
        let (layer, frame) = (e.layer.clone(), e.frame);
        let animate = animate && self.animate;
        transaction(animate, 0.35, || layer.setFrame(frame));
    }
}

fn part(entry: &mut Entry, name: &'static str, layer: &CALayer, scale: f64) -> Retained<CALayer> {
    entry
        .parts
        .entry(name)
        .or_insert_with(|| {
            let p = CALayer::new();
            p.setContentsScale(scale);
            layer.addSublayer(&p);
            p
        })
        .clone()
}

/// Makes the text layer `layer`, `h` high, scroll `text`: it waits at its start, then goes left
/// until a copy of it, `MARQUEE_GAP` after it, is where it started, and again. Faded where the box
/// cuts it: on the right, and on the left while it moves. Core Animation runs it in the window
/// server, so RustyBar does nothing while it scrolls.
fn start_marquee(entry: &mut Entry, layer: &CALayer, text: &str, font: &FontSpec, color: Rgba, h: f64, scale: f64) {
    stop_marquee(entry, layer);
    let Some(b) = gfx::text(text, font, color, scale) else { return };
    unsafe { layer.setContents(None) };
    let step = b.width.ceil() + MARQUEE_GAP;
    let (strip, copy) = (CALayer::new(), CALayer::new());
    let fade = CAGradientLayer::new();
    transaction(false, 0.0, || {
        for (l, x) in [(&strip, 0.0), (&copy, step)] {
            l.setContentsScale(scale);
            unsafe { l.setContents(Some(b.as_contents())) };
            l.setContentsGravity(unsafe { kCAGravityLeft });
            l.setFrame(rect(x, 0.0, b.width, h));
        }
        strip.addSublayer(&copy);
        layer.addSublayer(&strip);
        fade.setStartPoint(CGPoint::new(0.0, 0.5));
        fade.setEndPoint(CGPoint::new(1.0, 0.5));
        // A new layer with no superlayer, as a mask has to be
        unsafe { layer.setMask(Some(&fade)) };
    });

    let (wait, total) = (MARQUEE_WAIT, MARQUEE_WAIT + step / MARQUEE_SPEED);
    let times = |t: &[f64]| NSArray::from_retained_slice(&t.iter().map(|t| NSNumber::new_f64(t / total)).collect::<Vec<_>>());
    let scroll = CAKeyframeAnimation::animationWithKeyPath(Some(ns_string!("transform.translation.x")));
    let values = [0.0, 0.0, -step].map(NSNumber::new_f64);
    let values: Vec<&AnyObject> = values.iter().map(|v| -> &AnyObject { v }).collect();
    let still = CAMediaTimingFunction::functionWithControlPoints(0.0, 0.0, 1.0, 1.0);
    let smooth = CAMediaTimingFunction::functionWithControlPoints(0.42, 0.0, 0.58, 1.0);
    unsafe { scroll.setValues(Some(&NSArray::from_slice(&values))) };
    scroll.setKeyTimes(Some(&times(&[0.0, wait, total])));
    scroll.setTimingFunctions(Some(&NSArray::from_retained_slice(&[still, smooth])));

    // The left edge fades in as it starts and out as it stops
    let edges = |left: f32| {
        let colors = [Rgba(0.0, 0.0, 0.0, left as f64), Rgba(0.0, 0.0, 0.0, 1.0), Rgba(0.0, 0.0, 0.0, 1.0), Rgba(0.0, 0.0, 0.0, 0.0)].map(|c| c.cg());
        let objs: Vec<&AnyObject> = colors.iter().map(cg_obj).collect();
        NSArray::from_slice(&objs)
    };
    let (rest, moving) = (edges(1.0), edges(0.0));
    unsafe { fade.setColors(Some(&rest)) };
    let ramp = 0.3;
    let colors = CAKeyframeAnimation::animationWithKeyPath(Some(ns_string!("colors")));
    let values: Vec<&AnyObject> = [&rest, &rest, &moving, &moving, &rest].into_iter().map(|a| -> &AnyObject { a }).collect();
    unsafe { colors.setValues(Some(&NSArray::from_slice(&values))) };
    colors.setKeyTimes(Some(&times(&[0.0, wait, wait + ramp, total - ramp, total])));

    // Added in the same transaction, they start together. At most 60 frames a second, a pixel at
    // a time on a Retina screen: a ProMotion one would draw the bar 120 times a second for it
    let run = |anim: &CAAnimation, on: &CALayer| {
        anim.setDuration(total);
        anim.setRepeatCount(f32::INFINITY);
        anim.setPreferredFrameRateRange(CAFrameRateRange::new(30.0, 60.0, 60.0));
        on.addAnimation_forKey(anim, Some(ns_string!("marquee")));
    };
    run(&scroll, &strip);
    run(&colors, &fade);
    entry.marquee = Some(Marquee { strip, fade, size: (0.0, 0.0) });
}

/// Back to a text that stays still.
fn stop_marquee(entry: &mut Entry, layer: &CALayer) {
    if let Some(m) = entry.marquee.take() {
        m.strip.removeFromSuperlayer();
        unsafe { layer.setMask(None) };
    }
}

/// The fade of a scrolling text over its box, `w`×`h`: the box can change without the text
/// starting again.
fn fit_fade(m: &mut Marquee, w: f64, h: f64) {
    if m.size == (w, h) {
        return;
    }
    // A new one is at once as big as its text, a box that changes animates with it
    let first = m.size == (0.0, 0.0);
    m.size = (w, h);
    let edge = (MARQUEE_FADE / w.max(1.0)).min(0.3);
    let locations = NSArray::from_retained_slice(&[0.0, edge, 1.0 - edge, 1.0].map(NSNumber::new_f64));
    let set = || {
        m.fade.setFrame(rect(0.0, 0.0, w, h));
        m.fade.setLocations(Some(&locations));
    };
    if first { transaction(false, 0.0, set) } else { set() }
}

/// The corners of a node at `abs` (surface coordinates) with radius `own`, inside `container`.
/// Each corner near one of the container's is concentric with it, as macOS draws them: the
/// container's radius less the farther of the two insets from that corner. Never less round than
/// `own`, and no rounder than half the node's shorter side.
fn concentric(abs: CGRect, own: f64, container: Option<Container>) -> Corners {
    let mut corners = [own; 4];
    let Some(c) = container else { return corners };
    let left = abs.origin.x - c.rect.origin.x;
    let top = abs.origin.y - c.rect.origin.y;
    let right = (c.rect.origin.x + c.rect.size.width) - (abs.origin.x + abs.size.width);
    let bottom = (c.rect.origin.y + c.rect.size.height) - (abs.origin.y + abs.size.height);
    let insets = [left.max(top), right.max(top), right.max(bottom), left.max(bottom)];
    let half = abs.size.width.min(abs.size.height) / 2.0;
    for (corner, (radius, inset)) in corners.iter_mut().zip(c.corners.iter().zip(insets)) {
        let r = (radius - inset.max(0.0)).min(half);
        if r > *corner {
            *corner = r;
        }
    }
    corners
}

/// Rounds the layer's corners: with its radius when they're all the same, else with a mask.
fn set_corners(entry: &mut Entry, layer: &CALayer, corners: Corners, w: f64, h: f64) {
    if corners.iter().all(|&r| r == corners[0]) {
        layer.setCornerRadius(corners[0]);
        if entry.corners.take().is_some() {
            unsafe { layer.setMask(None) };
        }
        return;
    }
    layer.setCornerRadius(0.0);
    if entry.corners.as_ref().is_some_and(|(_, c, size)| *c == corners && *size == (w, h)) {
        return;
    }
    let mask = match entry.corners.take() {
        Some((mask, _, _)) => mask,
        None => {
            let mask = CAShapeLayer::new();
            mask.setContentsScale(layer.contentsScale());
            // A new layer with no superlayer, as a mask has to be
            unsafe { layer.setMask(Some(&mask)) };
            mask
        }
    };
    mask.setFrame(rect(0.0, 0.0, w, h));
    mask.setPath(Some(&rounded_rect(w, h, corners)));
    entry.corners = Some((mask, corners, (w, h)));
}

/// Gives the layer the shape of the notch, `w`×`h`, as a mask (see `Notch`). A new shape follows
/// `spring` from the one on screen, if there's one; else the transaction's animation, as the
/// layer's frame does.
fn set_notch(entry: &mut Entry, layer: &CALayer, n: Notch, radius: f64, w: f64, h: f64, spring: Option<(f64, f64)>) {
    if entry.corners.take().is_some() {
        unsafe { layer.setMask(None) };
    }
    layer.setCornerRadius(0.0);
    let (x, sw, sh) = n.area.map_or((0.0, w, h), |[x, sw, sh]| (x, sw, sh));
    let key = [x, sw, sh, n.ear, radius, w, h];
    if entry.notch.as_ref().is_some_and(|(_, k)| *k == key) {
        return;
    }
    let path = notch_path(x, sw, sh, n.ear, radius);
    let Some((mask, _)) = entry.notch.take() else {
        let mask = CAShapeLayer::new();
        mask.setContentsScale(layer.contentsScale());
        transaction(false, 0.0, || {
            mask.setFrame(rect(0.0, 0.0, w, h));
            mask.setPath(Some(&path));
        });
        // A new layer with no superlayer, as a mask has to be
        unsafe { layer.setMask(Some(&mask)) };
        entry.notch = Some((mask, key));
        return;
    };
    match spring {
        Some((stiffness, damping)) => {
            // From where it is on screen, also in the middle of another spring
            let shown = unsafe { mask.presentationLayer() }.unwrap_or_else(|| Retained::into_super(mask.clone()));
            let from = shown.valueForKey(ns_string!("path"));
            transaction(false, 0.0, || {
                mask.setFrame(rect(0.0, 0.0, w, h));
                mask.setPath(Some(&path));
            });
            let anim = CASpringAnimation::animationWithKeyPath(Some(ns_string!("path")));
            anim.setMass(1.0);
            anim.setStiffness(stiffness);
            anim.setDamping(damping);
            let to: &CFType = &path;
            unsafe {
                anim.setFromValue(from.as_deref());
                anim.setToValue(Some(to.as_ref()));
            }
            anim.setDuration(anim.settlingDuration());
            mask.addAnimation_forKey(&anim, Some(ns_string!("path")));
        }
        None => {
            mask.setFrame(rect(0.0, 0.0, w, h));
            mask.setPath(Some(&path));
        }
    }
    entry.notch = Some((mask, key));
}

/// The notch's shape from `x`, `w`×`h` with its ears (see `Notch`): from the top left corner a
/// curve of `ear` in to the side, down it, round at the bottom with `radius`, and out again to the
/// top right corner. The same points for any size, so that one shape can turn into another.
fn notch_path(x: f64, w: f64, h: f64, ear: f64, radius: f64) -> CFRetained<CGMutablePath> {
    let ear = ear.clamp(0.0, (w / 2.0).max(0.0));
    let r = radius.clamp(0.0, ((w - 2.0 * ear) / 2.0).min(h - ear).max(0.0));
    let (left, right) = (x + ear, x + w - ear);
    let path = CGMutablePath::new();
    let p = Some(&*path);
    let m = std::ptr::null();
    unsafe {
        CGMutablePath::move_to_point(p, m, x, 0.0);
        CGMutablePath::add_quad_curve_to_point(p, m, left, 0.0, left, ear);
        CGMutablePath::add_line_to_point(p, m, left, h - r);
        CGMutablePath::add_quad_curve_to_point(p, m, left, h, left + r, h);
        CGMutablePath::add_line_to_point(p, m, right - r, h);
        CGMutablePath::add_quad_curve_to_point(p, m, right, h, right, h - r);
        CGMutablePath::add_line_to_point(p, m, right, ear);
        CGMutablePath::add_quad_curve_to_point(p, m, right, 0.0, x + w, 0.0);
    }
    CGMutablePath::close_subpath(p);
    path
}

/// How long a spring of `stiffness` and `damping` takes to come to rest, in seconds.
pub fn settling(stiffness: f64, damping: f64) -> f64 {
    let anim = CASpringAnimation::new();
    anim.setMass(1.0);
    anim.setStiffness(stiffness);
    anim.setDamping(damping);
    anim.settlingDuration()
}

/// The bars of `Kind::Bars`, `w`×`h`: each rises to the top and falls to its lowest by itself, out
/// of step with the others, at 30 frames a second at most; with `motion` off they stand still at
/// heights of their own.
fn layout_bars(entry: &mut Entry, layer: &CALayer, color: Rgba, w: f64, h: f64, motion: bool, scale: f64) {
    const NAMES: [&str; BARS] = ["bar0", "bar1", "bar2", "bar3"];
    let x0 = ((w - BARS_WIDTH) / 2.0).max(0.0);
    for (i, name) in NAMES.iter().enumerate() {
        let bar = part(entry, name, layer, scale);
        bar.setFrame(rect(x0 + i as f64 * (BAR_W + BAR_GAP), 0.0, BAR_W, h));
        bar.setCornerRadius(BAR_W / 2.0);
        bar.setBackgroundColor(Some(&color.cg()));
        let moving = unsafe { bar.animationForKey(ns_string!("bars")) }.is_some();
        if motion && !moving {
            let anim = CABasicAnimation::animationWithKeyPath(Some(ns_string!("transform.scale.y")));
            unsafe {
                anim.setFromValue(Some(&NSNumber::new_f64(BAR_LOW[i])));
                anim.setToValue(Some(&NSNumber::new_f64(1.0)));
            }
            anim.setDuration(BAR_TIME[i]);
            anim.setAutoreverses(true);
            anim.setRepeatCount(f32::INFINITY);
            anim.setTimeOffset(BAR_PHASE[i] * BAR_TIME[i] * 2.0);
            anim.setTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(0.45, 0.0, 0.55, 1.0)));
            anim.setPreferredFrameRateRange(CAFrameRateRange::new(15.0, 30.0, 30.0));
            transaction(false, 0.0, || unsafe { bar.setValue_forKeyPath(Some(&NSNumber::new_f64(1.0)), ns_string!("transform.scale.y")) });
            bar.addAnimation_forKey(&anim, Some(ns_string!("bars")));
        } else if !motion {
            if moving {
                bar.removeAnimationForKey(ns_string!("bars"));
            }
            transaction(false, 0.0, || unsafe { bar.setValue_forKeyPath(Some(&NSNumber::new_f64(BAR_STILL[i])), ns_string!("transform.scale.y")) });
        }
    }
}

/// A `w`×`h` rectangle with each corner as round as `corners` says.
fn rounded_rect(w: f64, h: f64, corners: Corners) -> CFRetained<CGMutablePath> {
    let [tl, tr, br, bl] = corners;
    let path = CGMutablePath::new();
    let p = Some(&*path);
    let m = std::ptr::null();
    unsafe {
        CGMutablePath::move_to_point(p, m, tl, 0.0);
        CGMutablePath::add_arc_to_point(p, m, w, 0.0, w, h, tr);
        CGMutablePath::add_arc_to_point(p, m, w, h, 0.0, h, br);
        CGMutablePath::add_arc_to_point(p, m, 0.0, h, 0.0, 0.0, bl);
        CGMutablePath::add_arc_to_point(p, m, 0.0, 0.0, w, 0.0, tl);
    }
    CGMutablePath::close_subpath(p);
    path
}

/// A thick track and a fill as round as it, gone at 0 without steps, and the percentage (or the
/// step's label): in the fill before its end when it fits there, else on the track after it, else
/// in the middle. The percentage is drawn
/// twice, in a color that reads on the track and, clipped to the fill, in one that reads on the
/// fill, so it reads wherever it falls. The knob, a white disc in the fill's round end, shows only
/// with `knob`, and the percentage then keeps clear of it.
fn layout_slider(entry: &mut Entry, layer: &CALayer, w: f64, h: f64, v: f64, knob: bool, scale: f64) {
    let Some(look) = entry.slider.clone() else { return };
    if let Some(s) = entry.slider.as_mut() {
        s.value = v;
    }
    let track = part(entry, "track", layer, scale);
    track.setFrame(rect(0.0, 0.0, w, h));
    track.setCornerRadius(h / 2.0);
    track.setBackgroundColor(Some(&look.track.cg()));
    // Under the label and the fill: a dot for each step not reached, where the knob would stop
    let ticks = part(entry, "ticks", &track, scale);
    ticks.setFrame(rect(0.0, 0.0, w, h));
    layout_ticks(&ticks, &look, w, h, v, scale);
    let under = part(entry, "under", &track, scale);
    let fill = part(entry, "fill", &track, scale);
    // The knob's center is `slider_value`'s: from h/2 to w - h/2
    let travel = (w - h).max(0.0);
    // Gone at 0, unless 0 is a step like the others
    let edge = if v > 0.0 || look.labels.len() >= 2 { h + v * travel } else { 0.0 };
    fill.setFrame(rect(0.0, 0.0, edge, h));
    fill.setCornerRadius(h / 2.0);
    fill.setMasksToBounds(true);
    fill.setBackgroundColor(Some(&look.color.cg()));
    // The fill starts where the track does: the label's frame is the same in both
    let over = part(entry, "over", &fill, scale);
    if let Some(font) = &look.font {
        let text = match look.labels.len() {
            0 | 1 => format!("{:.0}%", v * 100.0),
            n => look.labels[step(v, n)].clone(),
        };
        let m = gfx::measure_text(&text, font);
        let (tw, th) = (m.width, m.height().ceil());
        // Clear of the round ends, and of the knob when it shows
        let (inset, inside, outside) = (8.0, if knob { h + 2.0 } else { 8.0 }, 6.0);
        let x = if edge - inside - tw >= inset {
            edge - inside - tw
        } else if (edge + outside).max(inset) + tw <= w - inset {
            (edge + outside).max(inset)
        } else {
            (w - tw) / 2.0
        };
        let sig = format!("l|{text}|{font:?}|{:?}|{:?}|{scale}", look.color, look.track);
        if entry.sig.as_deref() != Some(&sig) {
            for (label, background) in [(&under, look.track), (&over, look.color)] {
                if let Some(b) = gfx::text(&text, font, ink(background), scale) {
                    unsafe { label.setContents(Some(b.as_contents())) };
                    label.setContentsGravity(unsafe { kCAGravityLeft });
                }
            }
            entry.sig = Some(sig);
        }
        let frame = rect(x, (h - th) / 2.0, tw, th);
        under.setFrame(frame);
        over.setFrame(frame);
    }
    let disc = part(entry, "knob", layer, scale);
    let k = (h - 4.0).max(0.0);
    disc.setBackgroundColor(Some(&Rgba(1.0, 1.0, 1.0, 1.0).cg()));
    disc.setCornerRadius(k / 2.0);
    disc.setShadowOpacity(0.3);
    disc.setShadowRadius(2.0);
    disc.setShadowOffset(CGSize::new(0.0, 1.0));
    disc.setFrame(rect(v * travel + 2.0, 2.0, k, k));
    disc.setOpacity(if knob { 1.0 } else { 0.0 });
}

/// The step (0..n) of a slider with `n` steps at `v`.
fn step(v: f64, n: usize) -> usize {
    ((v * (n - 1) as f64).round() as usize).min(n - 1)
}

/// `v` at the nearest of `n` evenly spaced steps, or as it is without steps.
fn snap(v: f64, n: usize) -> f64 {
    if n < 2 { v } else { step(v, n) as f64 / (n - 1) as f64 }
}

/// The dots of a slider's steps after `v`, on the track; none without steps.
fn layout_ticks(ticks: &CALayer, look: &SliderLook, w: f64, h: f64, v: f64, scale: f64) {
    let n = if look.labels.len() >= 2 { look.labels.len() } else { 0 };
    let dots: Vec<Retained<CALayer>> = unsafe { ticks.sublayers() }.map(|a| a.to_vec()).unwrap_or_default();
    for extra in dots.iter().skip(n) {
        extra.removeFromSuperlayer();
    }
    let at = if n > 0 { step(v, n) } else { 0 };
    let (d, travel) = ((h * 0.22).round().max(3.0), (w - h).max(0.0));
    let color = ink(look.track).faded(0.4).cg();
    for i in 0..n {
        let dot = match dots.get(i) {
            Some(d) => d.clone(),
            None => {
                let d = CALayer::new();
                d.setContentsScale(scale);
                ticks.addSublayer(&d);
                d
            }
        };
        let x = h / 2.0 + travel * i as f64 / (n - 1) as f64;
        dot.setFrame(rect(x - d / 2.0, (h - d) / 2.0, d, d));
        dot.setCornerRadius(d / 2.0);
        dot.setBackgroundColor(Some(&color));
        dot.setOpacity(if i > at { 1.0 } else { 0.0 });
    }
}

/// The value (0..1) of a slider in `rect` with the knob's center at `x`.
pub fn slider_value(rect: CGRect, x: f64) -> f64 {
    let (w, h) = (rect.size.width, rect.size.height);
    ((x - rect.origin.x - h / 2.0) / (w - h).max(1.0)).clamp(0.0, 1.0)
}

/// Text that reads on `background`: dark on light colors, light on dark ones.
fn ink(background: Rgba) -> Rgba {
    let luminance = 0.2126 * background.0 + 0.7152 * background.1 + 0.0722 * background.2;
    if luminance > 0.6 { Rgba(0.0, 0.0, 0.0, 0.75) } else { Rgba(1.0, 1.0, 1.0, 0.92) }
}

fn kind_tag(kind: &Kind) -> &'static str {
    match kind {
        Kind::Box => "box",
        Kind::Text { .. } => "text",
        Kind::Symbol { .. } => "sym",
        Kind::AppIcon { .. } => "app",
        Kind::Image { .. } => "img",
        Kind::Slider { .. } => "slider",
        Kind::Progress { .. } => "progress",
        Kind::Toggle { .. } => "toggle",
        Kind::Scroll { .. } => "scroll",
        Kind::Skeleton => "skeleton",
        Kind::Bars { .. } => "bars",
    }
}

pub fn contains(r: CGRect, p: CGPoint) -> bool {
    p.x >= r.origin.x && p.x < r.origin.x + r.size.width && p.y >= r.origin.y && p.y < r.origin.y + r.size.height
}

fn intersect(a: CGRect, b: CGRect) -> Option<CGRect> {
    let x0 = a.origin.x.max(b.origin.x);
    let y0 = a.origin.y.max(b.origin.y);
    let x1 = (a.origin.x + a.size.width).min(b.origin.x + b.size.width);
    let y1 = (a.origin.y + a.size.height).min(b.origin.y + b.size.height);
    (x1 > x0 && y1 > y0).then(|| rect(x0, y0, x1 - x0, y1 - y0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slider_value_follows_the_knob() {
        // 100 wide and 20 high: the knob's center goes from 10 to 90
        let r = rect(50.0, 0.0, 100.0, 20.0);
        assert_eq!(slider_value(r, 60.0), 0.0);
        assert_eq!(slider_value(r, 100.0), 0.5);
        assert_eq!(slider_value(r, 140.0), 1.0);
        assert_eq!(slider_value(r, 10.0), 0.0);
        assert_eq!(slider_value(r, 200.0), 1.0);
        assert_eq!(ink(Rgba(0.8, 0.65, 0.97, 1.0)), Rgba(0.0, 0.0, 0.0, 0.75));
        assert_eq!(ink(Rgba(0.2, 0.2, 0.27, 1.0)), Rgba(1.0, 1.0, 1.0, 0.92));
    }

    #[test]
    fn steps_snap_to_the_nearest() {
        // 5 steps: 0, 0.25, 0.5, 0.75, 1
        assert_eq!(snap(0.1, 5), 0.0);
        assert_eq!(snap(0.13, 5), 0.25);
        assert_eq!(snap(0.9, 5), 1.0);
        assert_eq!(step(1.0, 5), 4);
        assert_eq!(step(0.5, 5), 2);
        // No steps: as it is
        assert_eq!(snap(0.37, 0), 0.37);
    }

    #[test]
    fn corners_follow_the_container() {
        let popup = |h: f64| Some(Container { rect: rect(0.0, 0.0, 200.0, h), corners: [16.0; 4] });
        // One row 4 points in: concentric all around, a capsule
        assert_eq!(concentric(rect(4.0, 4.0, 192.0, 24.0), 8.0, popup(32.0)), [12.0; 4]);
        // The first and last of three: round on the popup's side only
        assert_eq!(concentric(rect(4.0, 4.0, 192.0, 24.0), 8.0, popup(80.0)), [12.0, 12.0, 8.0, 8.0]);
        assert_eq!(concentric(rect(4.0, 52.0, 192.0, 24.0), 8.0, popup(80.0)), [8.0, 8.0, 12.0, 12.0]);
        // 8 points in, the row's own radius is concentric already
        assert_eq!(concentric(rect(8.0, 8.0, 184.0, 24.0), 8.0, popup(40.0)), [8.0; 4]);
        // Never rounder than half the shorter side, nor less round than its own
        assert_eq!(concentric(rect(1.0, 1.0, 198.0, 10.0), 2.0, popup(12.0)), [5.0; 4]);
        assert_eq!(concentric(rect(4.0, 4.0, 192.0, 24.0), 14.0, popup(32.0)), [14.0; 4]);
        assert_eq!(concentric(rect(4.0, 4.0, 192.0, 24.0), 8.0, None), [8.0; 4]);
    }

    #[test]
    fn hover_follows_a_tab_selected_under_the_mouse() {
        let (primary, veil, highlight) = (Rgba(0.8, 0.65, 0.97, 1.0), Rgba(0.0, 0.0, 0.0, 0.12), Rgba(0.3, 0.3, 0.4, 1.0));
        // Two tabs as the widgets draw them: the selected one has a veil as its hover
        let switch = |on: &str| {
            let tab = |key: &str| {
                let n = Node::default().size(40.0, 20.0).click(Action::Widget { widget: "w".into(), name: key.into() }).key(key);
                if key == on { n.hover(veil) } else { n }
            };
            Node::row(vec![tab("a"), tab("b")]).selected(Some(on.into()), primary).key("switch")
        };
        let mut surface = Surface::new(2.0, highlight);
        surface.animate = false;
        let shown = |s: &Surface, path: &str| {
            let c = s.entries[path].layer.backgroundColor();
            c.map(|c| (1..=4).map(|i| unsafe { *objc2_core_graphics::CGColor::components(Some(&c)).add(i - 1) }).collect::<Vec<_>>())
        };
        let rgba = |c: Rgba| Some(vec![c.0, c.1, c.2, c.3]);
        let render = |s: &mut Surface, on: &str| {
            let node = switch(on);
            let laid = super::super::layout::Layout { scale: 2.0 }.run(&node, None);
            s.render(&node, &laid);
        };

        render(&mut surface, "a");
        let b = surface.hit(CGPoint::new(60.0, 10.0), &[HitKind::Click]).map(|h| (h.path.clone(), h.hover)).unwrap();
        surface.set_hover(Some(b.0.clone()), b.1);
        assert_eq!(shown(&surface, &b.0), rgba(highlight));
        // Clicked, the mouse where it was: the veil over the selection, not the opaque highlight
        render(&mut surface, "b");
        assert_eq!(shown(&surface, &b.0), rgba(veil));
        // The mouse leaves: nothing over the selection
        surface.set_hover(None, None);
        assert_eq!(shown(&surface, &b.0), None);
    }

    #[test]
    fn a_clear_hover_is_no_highlight() {
        let click = Action::Widget { widget: "w".into(), name: "a".into() };
        let node = Node::row(vec![Node::default().size(40.0, 20.0).click(click.clone()).key("plain"), Node::default().size(40.0, 20.0).click(click).hover(Rgba(0.0, 0.0, 0.0, 0.0)).key("clear")]);
        let mut surface = Surface::new(2.0, Rgba(0.3, 0.3, 0.4, 1.0));
        surface.animate = false;
        let laid = super::super::layout::Layout { scale: 2.0 }.run(&node, None);
        surface.render(&node, &laid);
        let hover = |x: f64| surface.hit(CGPoint::new(x, 10.0), &[HitKind::Click]).map(|h| h.hover).expect("a click");
        assert_eq!(hover(20.0), Some(Rgba(0.3, 0.3, 0.4, 1.0)));
        // Still a click, without a highlight
        assert_eq!(hover(60.0), None);
    }
}
