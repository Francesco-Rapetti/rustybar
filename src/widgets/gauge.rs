//! A level drawn on a pill, for the widgets whose pill is its own gauge (`audio`'s volumes,
//! `claude`'s usage): one over all of the pill, or one along its bottom half and another along
//! its top one. `Look` is how it's drawn; all of them are nodes with a `level` (see
//! `ui::Level`) over the pill's content, so they take no room in it.

use super::Rx;
use crate::gfx::Rgba;
use crate::ui::{Action, Align, Justify, Node};

/// How a level is drawn on its part of a pill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Look {
    /// A line along the edge, as long as the level.
    Line,
    /// Filled up to the level with a veil of the color, under the text.
    Veil,
    /// Filled with the color itself; the text over it in a color that reads on it.
    Solid,
    /// A lighter veil that ends with a line, as a slider's knob.
    Mark,
}

impl Look {
    pub fn named(name: &str) -> Option<Look> {
        match name {
            "line" => Some(Look::Line),
            "veil" => Some(Look::Veil),
            "solid" => Some(Look::Solid),
            "mark" => Some(Look::Mark),
            _ => None,
        }
    }
}

/// A level to draw: `value` (0..1) in `color`, fainter with `dim` (the one that matters less of
/// two). `ink`: the color of what's over a solid fill of it. With `slide`, a drag across it
/// sends that with the new value (see `Node::on_slide`).
#[derive(Clone, Debug)]
pub struct Band {
    pub value: f64,
    pub color: Rgba,
    pub ink: Rgba,
    pub dim: bool,
    pub slide: Option<Action>,
}

/// The level over the node it's pushed in: a fill up to it, or a line along the bottom edge (the
/// top one with `top`) that leaves out `round` at the left and at the right, where the pill
/// bends. `copy`: what's under it in the band's ink, where it is in the node, for a solid fill.
pub fn fill(rx: &Rx, look: Look, band: &Band, top: bool, round: [f64; 2], copy: Node) -> Node {
    let th = rx.theme;
    let (v, strength) = (band.value.clamp(0.0, 1.0), if band.dim { 0.55 } else { 1.0 });
    let color = |alpha: f64| band.color.faded(alpha * strength);
    let fill = match look {
        Look::Veil => Node::row(vec![]).background(color(0.3)).level(v),
        Look::Mark => Node::row(vec![]).background(color(0.16)).level(v).level_edge(2.0, color(1.0)),
        Look::Solid => Node::row(vec![copy.key("copy")]).align(Align::Start).background(color(1.0)).level(v),
        // On the straight part of the pill's edge, inside its border
        Look::Line => {
            let inset = if th.glass { 0.0 } else { th.pill_border };
            let line = Node::row(vec![]).background(color(1.0)).radius(1.0).level(v).key("line");
            Node::column(vec![Node::row(vec![line]).height(2.0).key("track")])
                .justify(if top { Justify::Start } else { Justify::End })
                .padding(if top { inset } else { 0.0 }, round[1], if top { 0.0 } else { inset }, round[0])
                .level(1.0)
        }
    };
    fill.key("fill")
}

/// How round a pill is at its ends, for `fill`.
pub fn round(rx: &Rx) -> f64 {
    rx.theme.pill_radius.min(rx.theme.pill_height / 2.0)
}

/// A whole pill with `top` along its top half and `bottom` along its bottom one; with one of
/// them alone, that one over all of it. `row` is what's in the pill, with its padding and as
/// high as the pill, in its own colors or all in the one given. A click sends `click`; a drag
/// across a band that slides, its action.
pub fn pill(rx: &Rx, look: Look, row: &dyn Fn(Option<Rgba>) -> Node, top: Option<Band>, bottom: Option<Band>, click: Action) -> Node {
    let th = rx.theme;
    let r = round(rx);
    let over = |band: &Band, node: Node| match &band.slide {
        Some(action) => node.slide(action.clone(), band.value),
        None => node,
    };
    // The pill's round cuts what's on it: a layer half as tall can't be as round as it is
    let mut inner = row(None).radius(r).clip().click(click.clone()).key("content");
    match (top, bottom) {
        (Some(top), Some(bottom)) => {
            let half = |band: &Band, at_top: bool| {
                // The copy of a solid fill, where the pill has it: the bottom half begins halfway down
                let copy = row(Some(band.ink)).at(0.0, if at_top { 0.0 } else { -th.pill_height / 2.0 });
                let half = Node::row(vec![fill(rx, look, band, at_top, [r; 2], copy)]).grow();
                // One that doesn't slide leaves the mouse to the pill under it; one that does, the
                // highlight: the pill lights up as one, under the text, and not half of it over it
                let half = if band.slide.is_some() { over(band, half.click(click.clone()).hover_below()) } else { half };
                half.key(if at_top { "top" } else { "bottom" })
            };
            inner = inner.push(Node::column(vec![half(&top, true), half(&bottom, false)]).level(1.0).key("levels"));
        }
        (Some(band), None) | (None, Some(band)) => {
            inner = over(&band, inner).push(fill(rx, look, &band, false, [r; 2], row(Some(band.ink))));
        }
        (None, None) => {}
    }
    rx.pill(vec![inner]).padding(0.0, 0.0, 0.0, 0.0).gap(0.0).click(click)
}
