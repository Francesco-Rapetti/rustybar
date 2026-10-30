//! Turns a laid-out node tree into a tree of CALayers, reusing and animating the layers of the
//! previous render. Core Animation runs the animations in the window server, so a bar that isn't
//! changing costs nothing, and one that is costs a few property sets.

use std::collections::{HashMap, HashSet};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFType, CGPoint, CGRect, CGSize};
use objc2_foundation::{NSArray, NSNumber, ns_string};
use objc2_quartz_core::{
    CABasicAnimation, CAGradientLayer, CALayer, CAMediaTiming, CAMediaTimingFunction,
    CASpringAnimation, CATransaction, kCACornerCurveContinuous, kCAGravityCenter, kCAGravityLeft,
};

use super::layout::Laid;
use super::node::{Action, Kind, Node};
use crate::gfx::{self, Rgba};

/// An interactive region of a surface, in its coordinates (top-left origin).
#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub rect: CGRect,
    pub action: Option<Action>,
    pub hover: Option<Rgba>,
    pub kind: HitKind,
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
    /// Slider fill and knob, progress fill, toggle knob, shimmer, scroll content, selection.
    parts: HashMap<&'static str, Retained<CALayer>>,
    background: Option<Rgba>,
    frame: CGRect,
    generation: u64,
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
    entries: HashMap<String, Entry>,
    generation: u64,
    /// Sliders being dragged keep their value until released.
    pub dragging: Option<(String, f64)>,
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
            entries: HashMap::new(),
            generation: 0,
            dragging: None,
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
        let animate = self.animate;
        CATransaction::begin();
        CATransaction::setDisableActions(!animate);
        CATransaction::setAnimationDuration(0.32);
        CATransaction::setAnimationTimingFunction(Some(&ease()));
        let root = self.root.clone();
        let path = self.child_path("", node);
        self.walk(node, laid, &root, path, 0.0, 0.0, None);
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
    ) {
        let frame = rect(laid.x, laid.y, laid.w, laid.h);
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
                Entry { layer, sig: None, parts: HashMap::new(), background: None, frame, generation: self.generation },
            );
        }
        let scale = self.scale;
        let hovered = self.hovered.as_deref() == Some(path.as_str());
        let highlight = self.highlight;
        let dragging = self.dragging.clone();
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
        if entry.background != s.background || created {
            entry.background = s.background;
            if !hovered {
                layer.setBackgroundColor(s.background.map(|c| c.cg()).as_deref());
            }
        }
        layer.setCornerRadius(s.radius);
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
            Kind::Image { path: file, width, height } => {
                let sig = format!("i|{file}|{width}|{height}|{scale}");
                if entry.sig.as_deref() != Some(&sig) {
                    match gfx::image_file(file, *width, *height, scale) {
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
            Kind::Slider { value, color, track } => {
                let v = match &dragging {
                    Some((p, v)) if *p == path => *v,
                    _ => value.clamp(0.0, 1.0),
                };
                layout_slider(entry, &layer, laid.w, laid.h, v, *color, *track, scale);
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

        // Interactions, in surface coordinates
        let abs = rect(ox + laid.x, oy + laid.y, laid.w, laid.h);
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

        // Children
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
            self.walk(content, &shifted, &layer, content_path, ox + laid.x, oy + laid.y, clip);
            return;
        }
        for (child, child_laid) in node.children.iter().zip(&laid.children) {
            let child_path = self.child_path(&path, child);
            self.walk(child, child_laid, &layer, child_path, ox + laid.x, oy + laid.y, clip);
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

    /// Moves a slider while it's dragged, without a full render.
    pub fn drag_slider(&mut self, path: &str, value: f64) {
        self.dragging = Some((path.to_string(), value));
        let Some(e) = self.entries.get(path) else { return };
        let (w, h) = (e.frame.size.width, e.frame.size.height);
        transaction(false, 0.0, || {
            if let (Some(fill), Some(knob)) = (e.parts.get("fill"), e.parts.get("knob")) {
                let k = h.min(16.0);
                let x = (w - k) * value;
                fill.setFrame(rect(0.0, (h - 6.0) / 2.0, x + k / 2.0, 6.0));
                knob.setFrame(rect(x, (h - k) / 2.0, k, k));
            }
        });
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

#[allow(clippy::too_many_arguments)]
fn layout_slider(entry: &mut Entry, layer: &CALayer, w: f64, h: f64, v: f64, color: Rgba, track: Rgba, scale: f64) {
    let bar = part(entry, "track", layer, scale);
    bar.setBackgroundColor(Some(&track.cg()));
    bar.setCornerRadius(3.0);
    bar.setFrame(rect(0.0, (h - 6.0) / 2.0, w, 6.0));
    let fill = part(entry, "fill", layer, scale);
    fill.setBackgroundColor(Some(&color.cg()));
    fill.setCornerRadius(3.0);
    let k = h.min(16.0);
    let x = (w - k) * v;
    fill.setFrame(rect(0.0, (h - 6.0) / 2.0, x + k / 2.0, 6.0));
    let knob = part(entry, "knob", layer, scale);
    knob.setBackgroundColor(Some(&color.cg()));
    knob.setCornerRadius(k / 2.0);
    knob.setBorderWidth(2.0);
    knob.setBorderColor(Some(&Rgba(1.0, 1.0, 1.0, 0.9).cg()));
    knob.setShadowOpacity(0.3);
    knob.setShadowRadius(2.0);
    knob.setShadowOffset(CGSize::new(0.0, 1.0));
    knob.setFrame(rect(x, (h - k) / 2.0, k, k));
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
