//! A small flexbox: rows and columns with gap, padding, alignment, grow and fixed sizes, plus equal
//! column grids and absolutely placed children. Enough for the bar and the popups, and simple
//! enough that a frame can always be explained.

use super::node::{Align, Dir, Justify, Kind, Node};
use crate::gfx;

/// The frame of a node in its parent's coordinates (top-left origin), with its children.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Laid {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    /// The text actually drawn, when a text node had to be cut to fit.
    pub text: Option<String>,
    pub children: Vec<Laid>,
}

pub struct Layout {
    pub scale: f64,
}

impl Layout {
    /// Lays `node` out at its own size, or at `width` when given.
    pub fn run(&self, node: &Node, width: Option<f64>) -> Laid {
        let (w, h) = self.intrinsic(node, width);
        self.place(node, 0.0, 0.0, width.unwrap_or(w), h)
    }

    /// The size a node wants: its content plus padding, then its fixed and min/max sizes.
    /// `avail` is the width it may take at most, for text that can be cut.
    pub fn intrinsic(&self, node: &Node, avail: Option<f64>) -> (f64, f64) {
        let s = &node.style;
        let [pt, pr, pb, pl] = s.padding;
        let (mut w, mut h) = match &node.kind {
            Kind::Box => self.box_content(node, avail.map(|a| a - pl - pr)),
            Kind::Text { text, font, .. } => {
                let m = gfx::measure_text(text, font);
                (m.width, m.height().ceil())
            }
            Kind::Symbol { name, size, weight, color } => {
                match gfx::symbol(name, *size, *weight, *color, self.scale) {
                    Some(b) => (b.width, b.height),
                    None => (*size, *size),
                }
            }
            Kind::AppIcon { size, .. } => (*size, *size),
            // One side 0: the image's proportions decide it
            Kind::Image { path, width, height, tint } if *width == 0.0 || *height == 0.0 => {
                match gfx::image_file(path, *width, *height, *tint, self.scale) {
                    Some(b) => (b.width, b.height),
                    None => (*width, *height),
                }
            }
            Kind::Image { width, height, .. } => (*width, *height),
            Kind::Slider { .. } => (120.0, 20.0),
            Kind::Progress { .. } => (60.0, 6.0),
            Kind::Toggle { .. } => (30.0, 18.0),
            Kind::Skeleton => (40.0, 12.0),
            Kind::Bars { .. } => (super::render::BARS_WIDTH, 14.0),
            Kind::Scroll { max_height, .. } => {
                let (cw, ch) = node
                    .children
                    .first()
                    .map(|c| self.intrinsic(c, avail.map(|a| a - pl - pr)))
                    .unwrap_or_default();
                (cw, ch.min(*max_height))
            }
        };
        w += pl + pr;
        h += pt + pb;
        if let Some(fw) = s.width {
            w = fw;
        }
        if let Some(fh) = s.height {
            h = fh;
        }
        if let Some(min) = s.min_width {
            w = w.max(min);
        }
        if let Some(max) = s.max_width {
            w = w.min(max);
        }
        if let (Some(a), true) = (avail, matches!(node.kind, Kind::Text { .. })) {
            w = w.min(a.max(0.0));
        }
        (w, h)
    }

    fn box_content(&self, node: &Node, avail: Option<f64>) -> (f64, f64) {
        let s = &node.style;
        let flow: Vec<&Node> = node.children.iter().filter(|c| c.style.at.is_none()).collect();
        if flow.is_empty() {
            return (0.0, 0.0);
        }
        if let Some(cols) = s.columns.filter(|c| *c > 0) {
            // Each row as high as its highest cell, as `place` lays them out
            let rows = flow.len().div_ceil(cols);
            let mut cw = 0.0f64;
            let mut row_h = vec![0.0f64; rows];
            for (n, c) in flow.iter().enumerate() {
                let (w, h) = self.intrinsic(c, None);
                cw = cw.max(w);
                row_h[n / cols] = row_h[n / cols].max(h);
            }
            let cols_used = cols.min(flow.len());
            let w = cw * cols_used as f64 + s.gap * (cols_used as f64 - 1.0);
            let h = row_h.iter().sum::<f64>() + s.gap * (rows as f64 - 1.0);
            return (w, h);
        }
        let gaps = s.gap * (flow.len() as f64 - 1.0);
        match s.dir {
            Dir::Row => {
                let mut w = gaps;
                let mut h = 0.0f64;
                for c in &flow {
                    let (cw, ch) = self.intrinsic(c, None);
                    w += cw;
                    h = h.max(ch);
                }
                (w, h)
            }
            Dir::Column => {
                let mut w = 0.0f64;
                let mut h = gaps;
                for c in &flow {
                    let (cw, ch) = self.intrinsic(c, avail);
                    w = w.max(cw);
                    h += ch;
                }
                (w, h)
            }
        }
    }

    fn snap(&self, v: f64) -> f64 {
        (v * self.scale).round() / self.scale
    }

    /// Gives `node` the frame (x, y, w, h) and lays out its children inside it.
    pub fn place(&self, node: &Node, x: f64, y: f64, w: f64, h: f64) -> Laid {
        let mut laid = Laid { x: self.snap(x), y: self.snap(y), w: self.snap(w), h: self.snap(h), ..Default::default() };
        let s = &node.style;
        let [pt, pr, pb, pl] = s.padding;
        let (iw, ih) = ((w - pl - pr).max(0.0), (h - pt - pb).max(0.0));

        match &node.kind {
            Kind::Text { text, font, .. } => {
                let full = gfx::measure_text(text, font).width;
                if full > iw + 0.5 {
                    laid.text = Some(truncate(text, font, iw));
                }
                return laid;
            }
            Kind::Scroll { .. } => {
                if let Some(content) = node.children.first() {
                    let (_, ch) = self.intrinsic(content, Some(iw));
                    laid.children.push(self.place(content, pl, pt, iw, ch));
                }
                return laid;
            }
            Kind::Box => {}
            _ => return laid,
        }

        let flow_idx: Vec<usize> = (0..node.children.len()).filter(|i| node.children[*i].style.at.is_none()).collect();
        let mut frames: Vec<Option<Laid>> = vec![None; node.children.len()];

        if let Some(cols) = s.columns.filter(|c| *c > 0) {
            let cw = (iw - s.gap * (cols as f64 - 1.0)) / cols as f64;
            let rows = flow_idx.len().div_ceil(cols);
            let mut row_h = vec![0.0f64; rows];
            for (n, i) in flow_idx.iter().enumerate() {
                row_h[n / cols] = row_h[n / cols].max(self.intrinsic(&node.children[*i], Some(cw)).1);
            }
            let mut ry = pt;
            for r in 0..rows {
                for c in 0..cols {
                    let n = r * cols + c;
                    let Some(&i) = flow_idx.get(n) else { break };
                    let cx = pl + c as f64 * (cw + s.gap);
                    frames[i] = Some(self.place(&node.children[i], cx, ry, cw, row_h[r]));
                }
                ry += row_h[r] + s.gap;
            }
        } else {
            let row = s.dir == Dir::Row;
            let (main, cross) = if row { (iw, ih) } else { (ih, iw) };
            let sizes: Vec<(f64, f64)> = flow_idx
                .iter()
                .map(|i| {
                    let c = &node.children[*i];
                    let (cw, ch) = self.intrinsic(c, if row { None } else { Some(iw) });
                    if row { (cw, ch) } else { (ch, cw) }
                })
                .collect();
            let gaps = s.gap * (flow_idx.len().saturating_sub(1)) as f64;
            let used: f64 = sizes.iter().map(|s| s.0).sum::<f64>() + gaps;
            let mut free = main - used;
            let grow_total: f64 = flow_idx.iter().map(|i| node.children[*i].style.grow).sum();
            let mut mains: Vec<f64> = sizes.iter().map(|s| s.0).collect();
            if free > 0.0 && grow_total > 0.0 {
                for (n, i) in flow_idx.iter().enumerate() {
                    mains[n] += free * node.children[*i].style.grow / grow_total;
                }
                free = 0.0;
            } else if free < 0.0 && !row {
                free = 0.0;
            }
            let n = flow_idx.len() as f64;
            let (mut pos, between) = match s.justify {
                Justify::Start => (0.0, s.gap),
                Justify::Center => (free.max(0.0) / 2.0, s.gap),
                Justify::End => (free.max(0.0), s.gap),
                Justify::Between if n > 1.0 => (0.0, s.gap + free.max(0.0) / (n - 1.0)),
                Justify::Between => (0.0, s.gap),
            };
            for (k, i) in flow_idx.iter().enumerate() {
                let child = &node.children[*i];
                let (m, c_size) = (mains[k], sizes[k].1);
                let stretch = s.align == Align::Stretch;
                let cs = if stretch { cross } else { c_size.min(cross.max(c_size)) };
                let cpos = match s.align {
                    Align::Start | Align::Stretch => 0.0,
                    Align::Center => (cross - cs) / 2.0,
                    Align::End => cross - cs,
                };
                let f = if row {
                    self.place(child, pl + pos, pt + cpos, m, cs)
                } else {
                    let ch = if stretch { self.intrinsic(child, Some(cross)).1.max(m) } else { m };
                    self.place(child, pl + cpos, pt + pos, cs, ch)
                };
                frames[*i] = Some(f);
                pos += m + between;
            }
        }

        for (i, child) in node.children.iter().enumerate() {
            if let Some((ax, ay)) = child.style.at {
                let (cw, ch) = self.intrinsic(child, None);
                frames[i] = Some(self.place(child, ax, ay, cw, ch));
            }
        }
        laid.children = frames.into_iter().map(|f| f.unwrap_or_default()).collect();
        laid
    }
}

/// The longest prefix of `text` that fits `width` with "…" after it.
pub fn truncate(text: &str, font: &gfx::FontSpec, width: f64) -> String {
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>().trim_end().to_string() + "…";
        if gfx::measure_text(&candidate, font).width <= width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    chars[..lo].iter().collect::<String>().trim_end().to_string() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(w: f64, h: f64) -> Node {
        Node::default().size(w, h)
    }

    #[test]
    fn row_with_gap_and_padding() {
        let n = Node::row(vec![boxed(10.0, 10.0), boxed(20.0, 6.0)]).gap(4.0).pad(2.0, 3.0);
        let l = Layout { scale: 2.0 }.run(&n, None);
        assert_eq!((l.w, l.h), (3.0 + 10.0 + 4.0 + 20.0 + 3.0, 14.0));
        assert_eq!((l.children[0].x, l.children[0].y), (3.0, 2.0));
        assert_eq!((l.children[1].x, l.children[1].y), (17.0, 4.0)); // centered on the cross axis
    }

    #[test]
    fn column_stretches_and_grows() {
        let n = Node::column(vec![boxed(10.0, 10.0), Node::row(vec![Node::spacer(), boxed(5.0, 5.0)]).width(50.0)]);
        let l = Layout { scale: 1.0 }.run(&n, Some(100.0));
        assert_eq!(l.children[0].w, 100.0);
        assert_eq!(l.children[1].w, 100.0);
        assert_eq!(l.children[1].children[1].x, 95.0);
    }

    #[test]
    fn padded_slider_keeps_its_action() {
        let action = super::super::Action::Open("x".into());
        let (color, track) = (gfx::Rgba(1.0, 1.0, 1.0, 1.0), gfx::Rgba(0.0, 0.0, 0.0, 1.0));
        let mut n = Node::new(Kind::Slider { value: 0.5, color, track, font: None, labels: vec![] }).click(action.clone()).padding(4.0, 0.0, 0.0, 0.0);
        n.normalize();
        assert!(matches!(n.kind, Kind::Box) && n.on_click.is_none());
        assert_eq!(n.children[0].on_click, Some(action));
    }

    #[test]
    fn grid_has_equal_columns() {
        let cells: Vec<Node> = (0..10).map(|_| boxed(8.0, 8.0)).collect();
        let n = Node::row(cells).columns(7).gap(2.0);
        let l = Layout { scale: 1.0 }.run(&n, Some(7.0 * 10.0 + 6.0 * 2.0));
        assert_eq!(l.children[7].y, 10.0);
        assert_eq!(l.children[8].x, 12.0);
    }

    #[test]
    fn grid_rows_are_as_high_as_their_cells() {
        let n = Node::row(vec![boxed(10.0, 20.0), boxed(10.0, 20.0), boxed(10.0, 10.0), boxed(10.0, 10.0)]).columns(2).gap(2.0);
        let l = Layout { scale: 1.0 }.run(&n, None);
        assert_eq!(l.children[2].y, 22.0);
        assert_eq!(l.h, 32.0);
    }

    #[test]
    fn justify_between() {
        let n = Node::row(vec![boxed(10.0, 10.0), boxed(10.0, 10.0)]).justify(Justify::Between).width(100.0);
        let l = Layout { scale: 1.0 }.run(&n, None);
        assert_eq!(l.children[1].x, 90.0);
    }
}
