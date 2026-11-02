//! Keeps the two sides of a bar apart. When the widgets on the left and those on the right don't
//! fit in the room the screen gives them, between its edges and the notch (or each other), first
//! the widgets of `slim_first` leave out what the bar can do without, in that order (the volume
//! sliders of `audio`, then the pills of the single Claude accounts); then the ones farthest from
//! their edge give way first: they switch to their compact form one at a time, and when every one
//! is compact they move, one at a time, the outermost last, to the popup of a `⋯` pill that takes
//! their place (see `App::bar_tree`), so that every widget stays at hand. A side takes its bigger
//! forms back only with some room to spare, so that a widget whose width changes doesn't flip back
//! and forth at the limit.

/// How a widget is drawn in a bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Form {
    Full,
    /// The full form without what the bar can do without: see `Item::slim`.
    Slim,
    Compact,
    /// Not in the bar: in the popup of the `⋯` pill of its side.
    Hidden,
}

/// A widget of the left or right side.
#[derive(Clone, Debug)]
pub struct Item {
    pub left: bool,
    /// Its place counted from its edge: 0 is the leftmost of the left side, the rightmost of the
    /// right side.
    pub rank: usize,
    /// The form the config gives it: full, or compact or hidden whether or not there's room.
    pub form: Form,
    /// Its place in `slim_first`: it has a slim form, which goes before any widget turns compact,
    /// in this order.
    pub slim: Option<usize>,
}

/// The room the sides have, without the margins and the gaps around them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Room {
    /// Nothing between them: together they take at most this.
    Shared(f64),
    /// The notch or the center group between them: each side takes at most its own.
    Split { left: f64, right: f64 },
}

/// The room a side must have to spare before it takes a bigger form back.
pub const SLACK: f64 = 24.0;

/// One widget giving way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step {
    item: usize,
    form: Form,
}

/// The order in which the widgets of `lane` give way: first those that have one switch to their
/// slim form, in their order; then all of them to their compact one, then they go, the farthest
/// from their edge first and, at the same distance, the right side first.
fn steps(items: &[Item], lane: impl Fn(&Item) -> bool) -> Vec<Step> {
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| lane(&items[i])).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(items[i].rank), items[i].left));
    let mut slim: Vec<usize> = order.iter().copied().filter(|&i| items[i].slim.is_some() && items[i].form == Form::Full).collect();
    slim.sort_by_key(|&i| items[i].slim);
    let slim = slim.into_iter().map(|item| Step { item, form: Form::Slim });
    let compact = order.iter().filter(|&&i| items[i].form == Form::Full).map(|&item| Step { item, form: Form::Compact });
    let hide = order.iter().filter(|&&i| items[i].form != Form::Hidden).map(|&item| Step { item, form: Form::Hidden });
    slim.chain(compact).chain(hide).collect()
}

fn forms(items: &[Item], lanes: &[Vec<Step>], levels: &[usize]) -> Vec<Form> {
    let mut forms: Vec<Form> = items.iter().map(|i| i.form).collect();
    for (steps, &level) in lanes.iter().zip(levels) {
        for s in &steps[..level] {
            forms[s.item] = s.form;
        }
    }
    forms
}

/// Picks the form of every item. `prev` is what the last call returned for this bar (empty the
/// first time); `measure` gives the width of the left and right sides with the given forms, `⋯`
/// pills included. Returns the levels to pass as `prev` next time, and the forms.
pub fn fit(items: &[Item], room: Room, prev: &[usize], mut measure: impl FnMut(&[Form]) -> (f64, f64)) -> (Vec<usize>, Vec<Form>) {
    // A lane is a set of widgets that share their room: both sides, or each on its own
    let lanes: Vec<Vec<Step>> = match room {
        Room::Shared(_) => vec![steps(items, |_| true)],
        Room::Split { .. } => vec![steps(items, |i| i.left), steps(items, |i| !i.left)],
    };
    let fits = |lane: usize, (l, r): (f64, f64), slack: f64| match room {
        Room::Shared(w) => l + r <= w - slack,
        Room::Split { left, .. } if lane == 0 => l <= left - slack,
        Room::Split { right, .. } => r <= right - slack,
    };
    let mut levels: Vec<usize> = if prev.len() == lanes.len() { prev.to_vec() } else { vec![0; lanes.len()] };
    for lane in 0..lanes.len() {
        let max = lanes[lane].len();
        let mut fits_at = |levels: &mut Vec<usize>, level: usize, slack: f64| {
            levels[lane] = level;
            fits(lane, measure(&forms(items, &lanes, levels)), slack)
        };
        let mut level = levels[lane].min(max);
        if fits_at(&mut levels, level, 0.0) {
            // Bigger forms back, while they leave some room
            while level > 0 && fits_at(&mut levels, level - 1, SLACK) {
                level -= 1;
            }
        } else {
            // Smaller ones until they fit, or nothing left to give
            while level < max && !fits_at(&mut levels, level + 1, 0.0) {
                level += 1;
            }
            level = (level + 1).min(max);
        }
        levels[lane] = level;
    }
    let forms = forms(items, &lanes, &levels);
    (levels, forms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use Form::*;

    /// A bar: (left, width, compact width) of each widget, in the order of the config.
    fn bar(widgets: &[(bool, f64, f64)]) -> (Vec<Item>, impl Fn(&[Form]) -> (f64, f64) + Clone) {
        bar_slim(&widgets.iter().map(|&(left, full, compact)| (left, full, None, compact)).collect::<Vec<_>>())
    }

    /// As `bar`, with the place in `slim_first` and the slim width of the widgets that have a slim
    /// form.
    fn bar_slim(widgets: &[(bool, f64, Option<(usize, f64)>, f64)]) -> (Vec<Item>, impl Fn(&[Form]) -> (f64, f64) + Clone + use<>) {
        let rights = widgets.iter().filter(|w| !w.0).count();
        let mut seen = [0, 0];
        let items = widgets
            .iter()
            .map(|&(left, _, slim, _)| {
                let k = &mut seen[left as usize];
                *k += 1;
                Item { left, rank: if left { *k - 1 } else { rights - *k }, form: Full, slim: slim.map(|s| s.0) }
            })
            .collect();
        let widths: Vec<(bool, f64, Option<(usize, f64)>, f64)> = widgets.to_vec();
        let measure = move |forms: &[Form]| {
            let mut sides = (0.0, 0.0);
            for (&(left, full, slim, compact), form) in widths.iter().zip(forms) {
                let w = match form {
                    Form::Full => full,
                    Form::Slim => slim.map_or(full, |s| s.1),
                    Form::Compact => compact,
                    Form::Hidden => 0.0,
                };
                if left { sides.0 += w } else { sides.1 += w }
            }
            sides
        };
        (items, measure)
    }

    #[test]
    fn room_enough() {
        let (items, measure) = bar(&[(true, 100.0, 50.0), (false, 100.0, 50.0)]);
        let (levels, forms) = fit(&items, Room::Shared(300.0), &[], measure);
        assert_eq!(levels, vec![0]);
        assert_eq!(forms, vec![Full, Full]);
    }

    #[test]
    fn farthest_from_the_edge_first() {
        // apple, aerospace | home, audio, clock
        let widgets = [(true, 30.0, 30.0), (true, 300.0, 150.0), (false, 50.0, 50.0), (false, 200.0, 60.0), (false, 120.0, 60.0)];
        let (items, measure) = bar(&widgets);
        // 700 wide in 570: home and audio compact (2nd and 1st from the right edge), before
        // aerospace (1st from the left one)
        let (_, forms) = fit(&items, Room::Shared(570.0), &[], measure.clone());
        assert_eq!(forms, vec![Full, Full, Compact, Compact, Full]);
        // Less room: aerospace too, then the clock
        let (_, forms) = fit(&items, Room::Shared(400.0), &[], measure.clone());
        assert_eq!(forms, vec![Full, Compact, Compact, Compact, Compact]);
        // Every one compact isn't enough: home goes first
        let (_, forms) = fit(&items, Room::Shared(330.0), &[], measure.clone());
        assert_eq!(forms, vec![Compact, Compact, Hidden, Compact, Compact]);
        // Nothing fits: nothing stays
        let (_, forms) = fit(&items, Room::Shared(10.0), &[], measure);
        assert_eq!(forms, vec![Hidden; 5]);
    }

    #[test]
    fn slim_forms_first_in_their_order() {
        // apple, aerospace | home, claude (2nd in slim_first), audio (1st), clock
        let widgets = [
            (true, 30.0, None, 30.0),
            (true, 300.0, None, 150.0),
            (false, 50.0, None, 50.0),
            (false, 470.0, Some((1, 110.0)), 110.0),
            (false, 400.0, Some((0, 200.0)), 60.0),
            (false, 120.0, None, 60.0),
        ];
        let (items, measure) = bar_slim(&widgets);
        // 1370 wide in 1200: audio leaves its sliders out, though home and claude are farther from
        // the edge
        let (levels, forms) = fit(&items, Room::Shared(1200.0), &[], measure.clone());
        assert_eq!((levels, forms), (vec![1], vec![Full, Full, Full, Full, Slim, Full]));
        // Less room: claude next
        let (levels, forms) = fit(&items, Room::Shared(1000.0), &[], measure.clone());
        assert_eq!((levels, forms), (vec![2], vec![Full, Full, Full, Slim, Slim, Full]));
        // Less still: then the others, the farthest first, as without them
        let (_, forms) = fit(&items, Room::Shared(700.0), &[], measure.clone());
        assert_eq!(forms, vec![Full, Full, Compact, Compact, Compact, Full]);
        // Compact from the config: no slim step
        let mut items = items;
        items[4].form = Compact;
        let (levels, forms) = fit(&items, Room::Shared(2000.0), &[], measure);
        assert_eq!((levels, forms), (vec![0], vec![Full, Full, Full, Full, Compact, Full]));
    }

    #[test]
    fn each_side_of_the_notch_on_its_own() {
        let widgets = [(true, 30.0, 30.0), (true, 300.0, 150.0), (false, 200.0, 60.0), (false, 120.0, 60.0)];
        let (items, measure) = bar(&widgets);
        // The right side fits, the left one doesn't: only the left one gives way
        let (levels, forms) = fit(&items, Room::Split { left: 200.0, right: 400.0 }, &[], measure);
        assert_eq!(levels.len(), 2);
        assert_eq!(forms, vec![Full, Compact, Full, Full]);
    }

    #[test]
    fn the_config_forms_have_no_step() {
        let (mut items, measure) = bar(&[(true, 100.0, 50.0), (false, 100.0, 50.0), (false, 100.0, 50.0), (false, 100.0, 50.0)]);
        items[1].form = Compact;
        items[3].form = Hidden;
        let (_, forms) = fit(&items, Room::Shared(1000.0), &[], measure.clone());
        assert_eq!(forms, vec![Full, Compact, Full, Hidden]);
        let (levels, forms) = fit(&items, Room::Shared(170.0), &[], measure);
        assert_eq!(forms, vec![Compact, Compact, Compact, Hidden]);
        assert_eq!(levels, vec![2]);
    }

    #[test]
    fn bigger_forms_back_only_with_room_to_spare() {
        let widgets = [(true, 100.0, 50.0), (false, 100.0, 50.0)];
        let (items, measure) = bar(&widgets);
        let (levels, _) = fit(&items, Room::Shared(190.0), &[], measure.clone());
        assert_eq!(levels, vec![1]);
        // Room for both full forms, but not much more: the right one stays compact
        let (levels, forms) = fit(&items, Room::Shared(200.0 + SLACK / 2.0), &levels, measure.clone());
        assert_eq!((levels.clone(), forms), (vec![1], vec![Full, Compact]));
        // With room to spare it takes its full form back
        let (levels, forms) = fit(&items, Room::Shared(200.0 + SLACK), &levels, measure);
        assert_eq!((levels, forms), (vec![0], vec![Full, Full]));
    }
}
