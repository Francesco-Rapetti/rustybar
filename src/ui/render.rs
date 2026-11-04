//! Turns a laid-out node tree into a tree of CALayers, reusing and animating the layers of the
//! previous render. Core Animation runs the animations in the window server, so a bar that isn't
//! changing costs nothing, and one that is costs a few property sets.

use std::collections::{HashMap, HashSet};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFRetained, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGMutablePath;
use objc2_foundation::{NSArray, NSNumber, ns_string};
use objc2_quartz_core::{
    CABasicAnimation, CAGradientLayer, CALayer, CAMediaTiming, CAMediaTimingFunction, CAShapeLayer,
    CASpringAnimation, CATransaction, kCACornerCurveContinuous, kCAGravityCenter, kCAGravityLeft,
};

use super::layout::Laid;
use super::node::{Action, Kind, Node};
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
}

/// Corner radii: top left, top right, bottom right, bottom left.
type Corners = [f64; 4];

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
    pub hits: Vec<Hit>,
    pub scrolls: HashMap<String, ScrollState>,
    pub hovered: Option<String>,
    pub highlight: Rgba,
    /// The glass nodes of the last render, for the window to put its glass views behind them.
    pub glass: Vec<Glass>,
    /// Draws the glass nodes with this fill instead (a popup, which is glass already).
    pub glass_fill: Option<Rgba>,
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
            hits: Vec::new(),
            scrolls: HashMap::new(),
            hovered: None,
            highlight,
            glass: Vec::new(),
            glass_fill: None,
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
        CATransaction::commit();
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
                Entry { layer, sig: None, parts: HashMap::new(), background: None, frame, generation: self.generation, slider: None, corners: None },
            );
        }
        let scale = self.scale;
        let hovered = self.hovered.as_deref() == Some(path.as_str());
        let highlight = self.highlight;
        let glass_fill = self.glass_fill;
        let dragging = self.dragging.clone();
        let knob = self.slider_hovered.as_deref() == Some(path.as_str()) || dragging.as_ref().is_some_and(|(p, _)| *p == path);
        let entry = self.entries.get_mut(&path).unwrap();
        entry.generation = self.generation;
        let layer = entry.layer.clone();

        if entry.frame != frame || created {
            layer.setFrame(frame);
            entry.frame = frame;
        }
        let s = &node.style;
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
        set_corners(entry, &layer, corners, laid.w, laid.h);
        match s.border {
            Some((w, c)) => {
                layer.setBorderWidth(w);
                layer.setBorderColor(Some(&c.cg()));
            }
            None => layer.setBorderWidth(0.0),
        }

        // What the layer itself draws
        match &node.kind {
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
            Kind::Slider { value, color, track, font } => {
                let v = match &dragging {
                    Some((p, v)) if *p == path => *v,
                    _ => value.clamp(0.0, 1.0),
                };
                entry.slider = Some(SliderLook { color: *color, track: *track, font: font.clone(), value: v });
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
            Kind::Box | Kind::Scroll { .. } => {
                if entry.sig.is_some() {
                    unsafe { layer.setContents(None) };
                    entry.sig = None;
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
                Kind::Slider { .. } => Some(HitKind::Slider),
                Kind::Scroll { .. } => Some(HitKind::Scroll),
                _ if node.on_click.is_some() => Some(HitKind::Click),
                _ => None,
            };
            if let Some(kind) = kind {
                let hover = match kind {
                    HitKind::Click => Some(node.hover.unwrap_or(highlight)),
                    _ => node.hover,
                };
                self.hits.push(Hit { path: path.clone(), rect: r, action: node.on_click.clone(), hover, kind });
            }
        }

        // Children, in this node if it's rounded
        let container = if corners.iter().any(|&r| r > 0.0) { Some(Container { rect: abs, corners }) } else { container };
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

    /// Moves a slider while it's dragged, and its percentage with it, without a full render.
    pub fn drag_slider(&mut self, path: &str, value: f64) {
        self.dragging = Some((path.to_string(), value));
        let scale = self.scale;
        let Some(e) = self.entries.get_mut(path) else { return };
        let (w, h) = (e.frame.size.width, e.frame.size.height);
        let layer = e.layer.clone();
        transaction(false, 0.0, || layout_slider(e, &layer, w, h, value, true, scale));
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

/// A thick track and a fill as round as it, gone at 0, and the percentage: in the fill before its
/// end when it fits there, else on the track after it, else in the middle. The percentage is drawn
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
    let under = part(entry, "under", &track, scale);
    let fill = part(entry, "fill", &track, scale);
    // The knob's center is `slider_value`'s: from h/2 to w - h/2
    let travel = (w - h).max(0.0);
    let edge = if v > 0.0 { h + v * travel } else { 0.0 };
    fill.setFrame(rect(0.0, 0.0, edge, h));
    fill.setCornerRadius(h / 2.0);
    fill.setMasksToBounds(true);
    fill.setBackgroundColor(Some(&look.color.cg()));
    // The fill starts where the track does: the label's frame is the same in both
    let over = part(entry, "over", &fill, scale);
    if let Some(font) = &look.font {
        let text = format!("{:.0}%", v * 100.0);
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
}
