//! The declarative tree that widgets (native or Lua) return for the bar and for their popups.
//! The renderer diffs it against the previous one by key and animates what changed, so widgets
//! just describe the current state.

use serde::{Deserialize, Serialize};

use crate::gfx::{FontSpec, Rgba, Weight};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Node {
    /// Identity among siblings: a node keeps its layer (and animates) while its key stays the
    /// same, even if it moves. Without one, its position and kind are its identity.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub key: Option<String>,
    pub kind: Kind,
    #[serde(default)]
    pub style: Style,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub children: Vec<Node>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub on_click: Option<Action>,
    /// The background while the mouse is over the node; nodes with `on_click` get the theme's. A
    /// clear one (alpha 0): none, the background stays as it is.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub hover: Option<Rgba>,
    /// The key of the child that gets a sliding highlight behind it (the focused workspace…).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub selected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub selected_color: Option<Rgba>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum Kind {
    #[default]
    Box,
    Text {
        text: String,
        font: FontSpec,
        color: Rgba,
    },
    Symbol {
        name: String,
        size: f64,
        weight: Weight,
        color: Rgba,
    },
    AppIcon {
        bundle_id: String,
        size: f64,
    },
    /// Drawn in `tint` where it isn't transparent, when given: one-color logos (a menu bar icon,
    /// a mask icon) in any color.
    Image {
        path: String,
        width: f64,
        height: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tint: Option<Rgba>,
    },
    /// 0..1; dragging moves it, releasing sends `on_click` with the value. A thick track with the
    /// percentage inside, in `font` (none without one), and a knob while the mouse is over it.
    /// With `labels` it stops at as many evenly spaced steps (`value` is then `step / (n - 1)`),
    /// shows the label of its step instead of the percentage and a dot for each step after it.
    /// Without `on_click` it can't be moved.
    Slider {
        value: f64,
        color: Rgba,
        track: Rgba,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        font: Option<FontSpec>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        labels: Vec<String>,
    },
    Progress {
        value: f64,
        color: Rgba,
        track: Rgba,
    },
    Toggle {
        on: bool,
        color: Rgba,
        track: Rgba,
    },
    /// Shows its single child (the content) through a viewport of at most `max_height`, scrolled
    /// with the trackpad or the wheel. `start` is the key of the content child to show at the top
    /// the first time. `on_click` is sent with "top"/"bottom" when the end comes near, so the
    /// content can grow (the calendar adds months), and the visible child stays where it is.
    Scroll {
        max_height: f64,
        #[serde(default)]
        start: Option<String>,
    },
    /// A shimmering placeholder, while a widget has no data yet.
    Skeleton,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum Dir {
    #[default]
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum Align {
    Start,
    #[default]
    Center,
    End,
    Stretch,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum Justify {
    #[default]
    Start,
    Center,
    End,
    Between,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Style {
    pub dir: Dir,
    pub gap: f64,
    /// Top, right, bottom, left.
    pub padding: [f64; 4],
    /// Cross axis.
    pub align: Align,
    /// Main axis.
    pub justify: Justify,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub min_width: Option<f64>,
    pub max_width: Option<f64>,
    /// Share of the free space on the main axis.
    pub grow: f64,
    /// Lays the children out in this many equal columns.
    pub columns: Option<usize>,
    pub background: Option<Rgba>,
    pub border: Option<(f64, Rgba)>,
    pub radius: f64,
    pub opacity: f64,
    /// Placed at this position in its parent instead of in the flow.
    pub at: Option<(f64, f64)>,
    /// Liquid Glass behind it, with its radius (a pill on the glass theme).
    pub glass: bool,
    /// Text that doesn't fit scrolls instead of being cut: all of it, then a pause, and again.
    /// Not when the Mac asks for less motion.
    pub marquee: bool,
    /// Its children show only inside it, rounded as it is: what's under a pill that grows down
    /// shows as it grows.
    pub clip: bool,
}

impl Default for Style {
    fn default() -> Self {
        Style {
            dir: Dir::Row,
            gap: 0.0,
            padding: [0.0; 4],
            align: Align::Center,
            justify: Justify::Start,
            width: None,
            height: None,
            min_width: None,
            max_width: None,
            grow: 0.0,
            columns: None,
            background: None,
            border: None,
            radius: 0.0,
            opacity: 1.0,
            at: None,
            glass: false,
            marquee: false,
            clip: false,
        }
    }
}

/// What a click (or a slider release, or a scroll reaching an end) asks for.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Action {
    /// Sent to the widget that drew the node, with a name it chose.
    Widget { widget: String, name: String },
    /// A Lua function of the widget, by its index in the widget's last render.
    Lua { widget: String, index: usize },
    /// Opens the widget's popup (or closes it if open).
    Popup { widget: String },
    /// A shell command run in the background.
    Shell(String),
    /// A URL or a file opened with `open`.
    Open(String),
}

// Builders, so that widgets read like the tree they make.
impl Node {
    pub fn new(kind: Kind) -> Node {
        Node { kind, ..Default::default() }
    }

    pub fn row(children: Vec<Node>) -> Node {
        Node { children, ..Default::default() }
    }

    pub fn column(children: Vec<Node>) -> Node {
        let mut n = Node { children, ..Default::default() };
        n.style.dir = Dir::Column;
        n.style.align = Align::Stretch;
        n
    }

    pub fn text(text: impl Into<String>, font: FontSpec, color: Rgba) -> Node {
        Node::new(Kind::Text { text: text.into(), font, color })
    }

    pub fn symbol(name: impl Into<String>, size: f64, weight: Weight, color: Rgba) -> Node {
        Node::new(Kind::Symbol { name: name.into(), size, weight, color })
    }

    pub fn app_icon(bundle_id: impl Into<String>, size: f64) -> Node {
        Node::new(Kind::AppIcon { bundle_id: bundle_id.into(), size })
    }

    pub fn image(path: impl Into<String>, width: f64, height: f64, tint: Option<Rgba>) -> Node {
        Node::new(Kind::Image { path: path.into(), width, height, tint })
    }

    pub fn spacer() -> Node {
        let mut n = Node::default();
        n.style.grow = 1.0;
        n
    }

    pub fn key(mut self, key: impl Into<String>) -> Node {
        self.key = Some(key.into());
        self
    }

    pub fn click(mut self, action: Action) -> Node {
        self.on_click = Some(action);
        self
    }

    pub fn hover(mut self, color: Rgba) -> Node {
        self.hover = Some(color);
        self
    }

    pub fn gap(mut self, gap: f64) -> Node {
        self.style.gap = gap;
        self
    }

    pub fn padding(mut self, top: f64, right: f64, bottom: f64, left: f64) -> Node {
        self.style.padding = [top, right, bottom, left];
        self
    }

    pub fn pad(self, v: f64, h: f64) -> Node {
        self.padding(v, h, v, h)
    }

    pub fn align(mut self, align: Align) -> Node {
        self.style.align = align;
        self
    }

    pub fn justify(mut self, justify: Justify) -> Node {
        self.style.justify = justify;
        self
    }

    pub fn width(mut self, w: f64) -> Node {
        self.style.width = Some(w);
        self
    }

    pub fn height(mut self, h: f64) -> Node {
        self.style.height = Some(h);
        self
    }

    pub fn size(self, w: f64, h: f64) -> Node {
        self.width(w).height(h)
    }

    pub fn min_width(mut self, w: f64) -> Node {
        self.style.min_width = Some(w);
        self
    }

    pub fn max_width(mut self, w: f64) -> Node {
        self.style.max_width = Some(w);
        self
    }

    pub fn grow(mut self) -> Node {
        self.style.grow = 1.0;
        self
    }

    pub fn marquee(mut self, on: bool) -> Node {
        self.style.marquee = on;
        self
    }

    pub fn columns(mut self, n: usize) -> Node {
        self.style.columns = Some(n);
        self
    }

    pub fn background(mut self, color: Rgba) -> Node {
        self.style.background = Some(color);
        self
    }

    pub fn border(mut self, width: f64, color: Rgba) -> Node {
        self.style.border = Some((width, color));
        self
    }

    pub fn radius(mut self, r: f64) -> Node {
        self.style.radius = r;
        self
    }

    pub fn opacity(mut self, o: f64) -> Node {
        self.style.opacity = o;
        self
    }

    pub fn glass(mut self) -> Node {
        self.style.glass = true;
        self
    }

    pub fn clip(mut self) -> Node {
        self.style.clip = true;
        self
    }

    pub fn at(mut self, x: f64, y: f64) -> Node {
        self.style.at = Some((x, y));
        self
    }

    pub fn selected(mut self, key: Option<String>, color: Rgba) -> Node {
        self.selected = key;
        self.selected_color = Some(color);
        self
    }

    pub fn push(mut self, child: Node) -> Node {
        self.children.push(child);
        self
    }

    /// Wraps each content node (text, symbol, bar…) that has padding in a box with that padding
    /// and its frame (size, background, border, click), since the content of a layer fills it.
    /// Called once on each tree before its layout.
    pub fn normalize(&mut self) {
        for c in &mut self.children {
            c.normalize();
        }
        let leaf = !matches!(self.kind, Kind::Box | Kind::Scroll { .. });
        if !leaf || self.style.padding == [0.0; 4] {
            return;
        }
        let [_, pr, _, pl] = self.style.padding;
        let mut inner = Node::new(std::mem::take(&mut self.kind)).key("c");
        inner.style.grow = 1.0;
        inner.style.max_width = self.style.max_width.map(|m| (m - pl - pr).max(0.0));
        let mut outer = std::mem::replace(self, inner);
        // A slider takes its value from where it's pressed, so its action stays with it: on the
        // box, the drag would end with no action and a click on the padding would send none
        if matches!(self.kind, Kind::Slider { .. }) {
            self.on_click = outer.on_click.take();
            self.hover = outer.hover.take();
        }
        outer.kind = Kind::Box;
        outer.style.dir = Dir::Row;
        outer.style.align = Align::Stretch;
        outer.children = vec![std::mem::take(self)];
        *self = outer;
    }
}
