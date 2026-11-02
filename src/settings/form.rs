//! The pieces of the settings window, in AppKit: a page is a column of groups, rounded boxes of
//! rows as in System Settings, each row a title (and a line of help) on the left and its control
//! on the right. Controls call Rust closures through `Target`, a small Objective-C object the page
//! keeps alive. The `*_at` controls read their value from the config (`store`) and write it back
//! as soon as it changes: there's no Save button, as in System Settings.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSBezierPath, NSBox, NSBoxType, NSButton, NSColor, NSColorSpace, NSColorWell, NSColorWellStyle, NSControl,
    NSControlSize, NSControlStateValueOff, NSControlStateValueOn, NSFont, NSFontWeightSemibold, NSImage,
    NSImageSymbolConfiguration, NSLayoutAttribute, NSLayoutConstraintOrientation, NSLineBreakMode, NSMenu, NSMenuItem,
    NSPopUpButton, NSStackView, NSStackViewGravity, NSStepper, NSTextAlignment, NSTextField,
    NSTokenField, NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{NSArray, NSEdgeInsets, NSFormatter, NSNumberFormatter, NSNumberFormatterStyle, NSObject, NSPoint, NSRect, NSSize, NSString, ns_string};

use super::store::{self, Spot, Value};
use crate::gfx::Rgba;

/// The width of the column of groups, and of what's inside a group.
pub const GROUP_W: f64 = 540.0;
pub const INNER: f64 = GROUP_W - 28.0;

pub struct TargetIvars {
    f: Box<dyn Fn()>,
}

define_class!(
    /// Receives the action of a control (or menu item, or click) and calls its closure.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = TargetIvars]
    pub struct Target;

    impl Target {
        #[unsafe(method(fire:))]
        fn fire(&self, _sender: Option<&AnyObject>) {
            (self.ivars().f)();
        }
    }
);

impl Target {
    pub fn new(mtm: MainThreadMarker, f: impl Fn() + 'static) -> Retained<Target> {
        let this = Self::alloc(mtm).set_ivars(TargetIvars { f: Box::new(f) });
        unsafe { msg_send![super(this), init] }
    }
}

/// Points `control` at `target`.
pub fn wire(control: &NSControl, target: &Target) {
    let obj: &AnyObject = target;
    unsafe {
        control.setTarget(Some(obj));
        control.setAction(Some(sel!(fire:)));
    }
}

pub struct FlippedIvars;

define_class!(
    /// A view whose origin is at the top, for the scroll views: pages start at the top.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = FlippedIvars]
    pub struct Flipped;

    impl Flipped {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl Flipped {
    pub fn new(mtm: MainThreadMarker) -> Retained<Flipped> {
        let this = Self::alloc(mtm).set_ivars(FlippedIvars);
        unsafe { msg_send![super(this), init] }
    }
}

pub struct RoundedIvars {
    fill: RefCell<Option<Retained<NSColor>>>,
    radius: Cell<f64>,
}

define_class!(
    /// A rounded rectangle in a dynamic color, so that it follows light and dark mode: the groups,
    /// and the selected page in the sidebar. A layer's color and corners, not drawRect: a tall
    /// group drawn as a bitmap takes the process past 140 MB for a moment on macOS 26.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = RoundedIvars]
    pub struct Rounded;

    impl Rounded {
        #[unsafe(method(wantsUpdateLayer))]
        fn wants_update_layer(&self) -> bool {
            true
        }

        /// Called again when the appearance changes, with it current: the color resolves to it.
        #[unsafe(method(updateLayer))]
        fn update_layer(&self) {
            if let Some(layer) = self.layer() {
                layer.setCornerRadius(self.ivars().radius.get());
                layer.setBackgroundColor(self.ivars().fill.borrow().as_ref().map(|c| c.CGColor()).as_deref());
            }
        }

        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl Rounded {
    pub fn new(mtm: MainThreadMarker, fill: Option<Retained<NSColor>>, radius: f64) -> Retained<Rounded> {
        let this = Self::alloc(mtm).set_ivars(RoundedIvars { fill: RefCell::new(fill), radius: Cell::new(radius) });
        let this: Retained<Rounded> = unsafe { msg_send![super(this), init] };
        this.setWantsLayer(true);
        this
    }
}

/// Puts `view` inside `parent`, its edges `insets` (top, left, bottom, right) from the parent's.
pub fn pin(view: &NSView, parent: &NSView, insets: (f64, f64, f64, f64)) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    parent.addSubview(view);
    view.topAnchor().constraintEqualToAnchor_constant(&parent.topAnchor(), insets.0).setActive(true);
    view.leadingAnchor().constraintEqualToAnchor_constant(&parent.leadingAnchor(), insets.1).setActive(true);
    parent.bottomAnchor().constraintEqualToAnchor_constant(&view.bottomAnchor(), insets.2).setActive(true);
    parent.trailingAnchor().constraintEqualToAnchor_constant(&view.trailingAnchor(), insets.3).setActive(true);
}

pub fn set_width(view: &NSView, w: f64) {
    view.setTranslatesAutoresizingMaskIntoConstraints(false);
    view.widthAnchor().constraintEqualToConstant(w).setActive(true);
}

pub fn stack(mtm: MainThreadMarker, vertical: bool, spacing: f64) -> Retained<NSStackView> {
    let s = NSStackView::new(mtm);
    if vertical {
        s.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        s.setAlignment(NSLayoutAttribute::Leading);
    } else {
        s.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        s.setAlignment(NSLayoutAttribute::CenterY);
    }
    s.setSpacing(spacing);
    s
}

pub fn label(mtm: MainThreadMarker, text: &str, size: f64, bold: bool, secondary: bool) -> Retained<NSTextField> {
    let l = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    let font = if bold { NSFont::systemFontOfSize_weight(size, unsafe { NSFontWeightSemibold }) } else { NSFont::systemFontOfSize(size) };
    l.setFont(Some(&font));
    if secondary {
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    l
}

/// A label that wraps at `width`.
pub fn paragraph(mtm: MainThreadMarker, text: &str, size: f64, secondary: bool, width: f64) -> Retained<NSTextField> {
    let l = NSTextField::wrappingLabelWithString(&NSString::from_str(text), mtm);
    l.setFont(Some(&NSFont::systemFontOfSize(size)));
    l.setSelectable(false);
    if secondary {
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
    }
    l.setPreferredMaxLayoutWidth(width);
    l
}

pub fn symbol(name: &str, size: f64) -> Option<Retained<NSImage>> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)?;
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(size, 0.0);
    image.imageWithSymbolConfiguration(&config)
}

pub fn ns_color(c: Rgba) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(c.0, c.1, c.2, c.3)
}

pub fn rgba(c: &NSColor) -> Rgba {
    let c = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()).unwrap_or_else(|| objc2::rc::Retained::from(c));
    Rgba(c.redComponent(), c.greenComponent(), c.blueComponent(), c.alphaComponent())
}

/// `#rrggbb`, or `#rrggbbaa` when it isn't opaque.
pub fn hex(c: Rgba) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    if b(c.3) == 255 { format!("#{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2)) } else { format!("#{:02x}{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2), b(c.3)) }
}

/// A small rounded square of `c`, for the colors of the palette in a menu.
pub fn swatch(c: Rgba) -> Retained<NSImage> {
    let block = RcBlock::new(move |rect: NSRect| -> Bool {
        ns_color(c).setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(rect, 3.0, 3.0).fill();
        Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(12.0, 12.0), false, &block)
}

/// Calls `f` with the last value once `secs` pass without another: for sliders and color wells,
/// which change continuously while dragged, so that the file is written once.
pub fn debounce<T: 'static>(secs: f64, f: impl Fn(T) + 'static) -> impl Fn(T) {
    let generation = Rc::new(Cell::new(0u64));
    let f = Rc::new(f);
    move |v: T| {
        let g = generation.get() + 1;
        generation.set(g);
        let (generation, f) = (generation.clone(), f.clone());
        crate::runtime::later(secs, move || {
            if generation.get() == g {
                f(v);
            }
        });
    }
}

#[derive(Clone, Copy)]
pub struct Num {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub decimals: u8,
}

impl Num {
    pub const fn new(min: f64, max: f64, step: f64, decimals: u8) -> Num {
        Num { min, max, step, decimals }
    }
}

/// An entry of a popup button.
pub struct Opt {
    pub value: String,
    pub label: String,
    pub swatch: Option<Rgba>,
}

pub fn opts(items: &[(&str, &str)]) -> Vec<Opt> {
    items.iter().map(|(v, l)| Opt { value: v.to_string(), label: l.to_string(), swatch: None }).collect()
}

/// A page being built.
pub struct Form {
    pub mtm: MainThreadMarker,
    pub column: Retained<NSStackView>,
    rows: Option<Retained<NSStackView>>,
    last_box: Option<Retained<Rounded>>,
    pub keep: Vec<Retained<Target>>,
}

impl Form {
    pub fn new(mtm: MainThreadMarker) -> Form {
        let column = stack(mtm, true, 8.0);
        column.setEdgeInsets(NSEdgeInsets { top: 28.0, left: 20.0, bottom: 28.0, right: 20.0 });
        Form { mtm, column, rows: None, last_box: None, keep: Vec::new() }
    }

    pub fn target(&mut self, f: impl Fn() + 'static) -> Retained<Target> {
        let t = Target::new(self.mtm, f);
        self.keep.push(t.clone());
        t
    }

    pub fn wire(&mut self, control: &NSControl, f: impl Fn() + 'static) {
        let t = self.target(f);
        wire(control, &t);
    }

    fn add(&mut self, view: &NSView, space_after: f64) {
        self.column.addArrangedSubview(view);
        self.column.setCustomSpacing_afterView(space_after, view);
    }

    /// The title of the page, what it's about and the buttons that open its files.
    pub fn header(&mut self, title: &str, subtitle: &str, files: &[std::path::PathBuf]) {
        self.rows = None;
        let t = label(self.mtm, title, 22.0, true, false);
        self.add(&t, 4.0);
        if !subtitle.is_empty() {
            let s = paragraph(self.mtm, subtitle, 12.0, true, GROUP_W);
            self.add(&s, 10.0);
        }
        if !files.is_empty() {
            let row = stack(self.mtm, false, 8.0);
            for path in files {
                let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let p = path.clone();
                let b = self.button(&format!("{} {name}", crate::i18n::t("Apri", "Open")), move || open_file(&p));
                b.setControlSize(NSControlSize::Small);
                row.addArrangedSubview(&b);
            }
            self.add(&row, 18.0);
        }
    }

    /// Starts a group of rows, under its title if it has one.
    pub fn group(&mut self, title: Option<&str>) {
        if let Some(title) = title {
            let l = label(self.mtm, title, 13.0, true, false);
            self.add(&l, 6.0);
        }
        let rows = stack(self.mtm, true, 0.0);
        rows.setAlignment(NSLayoutAttribute::CenterX);
        rows.setEdgeInsets(NSEdgeInsets { top: 3.0, left: 14.0, bottom: 3.0, right: 14.0 });
        let fill = NSColor::labelColor().colorWithAlphaComponent(0.045);
        let bg = Rounded::new(self.mtm, Some(fill), 10.0);
        pin(&rows, &bg, (0.0, 0.0, 0.0, 0.0));
        set_width(&bg, GROUP_W);
        self.add(&bg, 20.0);
        self.rows = Some(rows);
        self.last_box = Some(bg);
    }

    /// A line of help under the group just made.
    pub fn footer(&mut self, text: &str) {
        if let Some(b) = self.last_box.clone() {
            self.column.setCustomSpacing_afterView(6.0, &b);
        }
        let p = paragraph(self.mtm, text, 11.0, true, GROUP_W - 16.0);
        let wrap = stack(self.mtm, false, 0.0);
        wrap.setEdgeInsets(NSEdgeInsets { top: 0.0, left: 8.0, bottom: 0.0, right: 8.0 });
        wrap.addArrangedSubview(&p);
        self.add(&wrap, 20.0);
    }

    pub fn add_view(&mut self, view: &NSView) {
        self.rows = None;
        self.add(view, 20.0);
    }

    fn push_row(&mut self, row: &NSView) {
        if self.rows.is_none() {
            self.group(None);
        }
        let rows = self.rows.clone().expect("group");
        if !rows.arrangedSubviews().is_empty() {
            let sep = NSBox::new(self.mtm);
            sep.setBoxType(NSBoxType::Separator);
            set_width(&sep, INNER);
            rows.addArrangedSubview(&sep);
        }
        set_width(row, INNER);
        rows.addArrangedSubview(row);
    }

    fn labels(&self, title: &str, help: Option<&str>, width: f64) -> Retained<NSStackView> {
        let col = stack(self.mtm, true, 2.0);
        let t = paragraph(self.mtm, title, 13.0, false, width);
        t.setTextColor(Some(&NSColor::labelColor()));
        col.addArrangedSubview(&t);
        if let Some(h) = help.filter(|h| !h.is_empty()) {
            col.addArrangedSubview(&paragraph(self.mtm, h, 11.0, true, width));
        }
        col.setContentCompressionResistancePriority_forOrientation(250.0, NSLayoutConstraintOrientation::Horizontal);
        col
    }

    /// A row: the title and its help on the left, `control` on the right.
    pub fn row(&mut self, title: &str, help: Option<&str>, control: &NSView) {
        let width = (INNER - control.fittingSize().width - 20.0).max(140.0);
        let row = stack(self.mtm, false, 16.0);
        row.setEdgeInsets(NSEdgeInsets { top: 9.0, left: 0.0, bottom: 9.0, right: 0.0 });
        row.addView_inGravity(&self.labels(title, help, width), NSStackViewGravity::Leading);
        row.addView_inGravity(control, NSStackViewGravity::Trailing);
        self.push_row(&row);
    }

    /// A row with the title above and `control` under it, as wide as the group.
    pub fn row_wide(&mut self, title: &str, help: Option<&str>, control: &NSView) {
        let col = stack(self.mtm, true, 6.0);
        col.setEdgeInsets(NSEdgeInsets { top: 9.0, left: 0.0, bottom: 10.0, right: 0.0 });
        col.addArrangedSubview(&self.labels(title, help, INNER));
        set_width(control, INNER);
        col.addArrangedSubview(control);
        self.push_row(&col);
    }

    /// A row of views from the left, and `trailing` at the right end.
    pub fn line(&mut self, views: &[&NSView], trailing: Option<&NSView>) {
        let row = stack(self.mtm, false, 8.0);
        row.setEdgeInsets(NSEdgeInsets { top: 6.0, left: 0.0, bottom: 6.0, right: 0.0 });
        for v in views {
            row.addView_inGravity(v, NSStackViewGravity::Leading);
        }
        if let Some(t) = trailing {
            row.addView_inGravity(t, NSStackViewGravity::Trailing);
        }
        self.push_row(&row);
    }

    /// A row of plain text.
    pub fn note(&mut self, text: &str) {
        let p = paragraph(self.mtm, text, 12.0, true, INNER);
        let row = stack(self.mtm, false, 0.0);
        row.setEdgeInsets(NSEdgeInsets { top: 10.0, left: 0.0, bottom: 10.0, right: 0.0 });
        row.addArrangedSubview(&p);
        self.push_row(&row);
    }

    // ---- controls ------------------------------------------------------------------------------

    pub fn button(&mut self, title: &str, f: impl Fn() + 'static) -> Retained<NSButton> {
        let b = unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), None, None, self.mtm) };
        self.wire(&b, f);
        b
    }

    /// A borderless button with an SF Symbol.
    pub fn icon_button(&mut self, name: &str, tip: &str, f: impl Fn() + 'static) -> Retained<NSButton> {
        let image = symbol(name, 13.0).unwrap_or_default();
        let b = unsafe { NSButton::buttonWithImage_target_action(&image, None, None, self.mtm) };
        b.setBordered(false);
        b.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
        b.setToolTip(Some(&NSString::from_str(tip)));
        self.wire(&b, f);
        b
    }

    /// Buttons at the right end of a row of their own.
    pub fn buttons(&mut self, buttons: &[&NSView]) {
        let row = stack(self.mtm, false, 8.0);
        row.setEdgeInsets(NSEdgeInsets { top: 7.0, left: 0.0, bottom: 7.0, right: 0.0 });
        for b in buttons {
            row.addView_inGravity(b, NSStackViewGravity::Trailing);
        }
        self.push_row(&row);
    }

    /// The `⋯` of a row of a list, with its commands (move up, move down, delete…).
    pub fn menu_button(&mut self, items: Vec<(String, Box<dyn Fn()>)>) -> Retained<NSButton> {
        let menu = NSMenu::new(self.mtm);
        for (title, f) in items {
            let t = self.target(f);
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(self.mtm), &NSString::from_str(&title), Some(sel!(fire:)), ns_string!(""))
            };
            let obj: &AnyObject = &t;
            unsafe { item.setTarget(Some(obj)) };
            menu.addItem(&item);
        }
        let b = self.icon_button("ellipsis.circle", "", || {});
        let (button, m) = (b.clone(), menu);
        let t = self.target(move || {
            let h = button.bounds().size.height;
            m.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.0, h + 4.0), Some(&button));
        });
        wire(&b, &t);
        b
    }

    /// A checkbox. Not an NSSwitch, nor sliders elsewhere: on macOS 26 the first of either takes
    /// the process past 120 MB for a moment, and what it leaves fragmented stays with the bar.
    pub fn switch(&mut self, on: bool, f: impl Fn(bool) + 'static) -> Retained<NSButton> {
        let sw = unsafe { NSButton::checkboxWithTitle_target_action(ns_string!(""), None, None, self.mtm) };
        sw.setState(if on { NSControlStateValueOn } else { NSControlStateValueOff });
        let s = sw.clone();
        self.wire(&sw, move || f(s.state() == NSControlStateValueOn));
        sw
    }

    pub fn toggle(&mut self, title: &str, help: Option<&str>, on: bool, f: impl Fn(bool) + 'static) {
        let sw = self.switch(on, f);
        self.row(title, help, &sw);
    }

    /// A number field with its stepper.
    pub fn number_control(&mut self, value: f64, n: Num, f: impl Fn(f64) + 'static) -> Retained<NSStackView> {
        let field = NSTextField::textFieldWithString(ns_string!(""), self.mtm);
        let fmt = NSNumberFormatter::new();
        fmt.setNumberStyle(NSNumberFormatterStyle::DecimalStyle);
        fmt.setMaximumFractionDigits(n.decimals as usize);
        fmt.setUsesGroupingSeparator(false);
        fmt.setLenient(true);
        let fmt_ref: &NSFormatter = &fmt;
        field.setFormatter(Some(fmt_ref));
        field.setDoubleValue(value);
        field.setAlignment(NSTextAlignment::Right);
        if let Some(cell) = field.cell() {
            cell.setSendsActionOnEndEditing(true);
        }
        set_width(&field, if n.max >= 10000.0 { 64.0 } else { 52.0 });
        let stepper = NSStepper::new(self.mtm);
        stepper.setMinValue(n.min);
        stepper.setMaxValue(n.max);
        stepper.setIncrement(n.step);
        stepper.setValueWraps(false);
        stepper.setDoubleValue(value);
        let f = Rc::new(f);
        let last = Rc::new(Cell::new(value));
        {
            let (fd, st, f, last) = (field.clone(), stepper.clone(), f.clone(), last.clone());
            self.wire(&field, move || {
                let v = fd.doubleValue().clamp(n.min, n.max);
                fd.setDoubleValue(v);
                st.setDoubleValue(v);
                if (v - last.get()).abs() > 1e-9 {
                    last.set(v);
                    f(v);
                }
            });
        }
        {
            let (fd, st) = (field.clone(), stepper.clone());
            self.wire(&stepper, move || {
                let v = st.doubleValue();
                fd.setDoubleValue(v);
                last.set(v);
                f(v);
            });
        }
        let row = stack(self.mtm, false, 4.0);
        row.addArrangedSubview(&field);
        row.addArrangedSubview(&stepper);
        row
    }

    pub fn number(&mut self, title: &str, help: Option<&str>, value: f64, n: Num, f: impl Fn(f64) + 'static) {
        let c = self.number_control(value, n, f);
        self.row(title, help, &c);
    }

    pub fn popup(&mut self, items: Vec<Opt>, selected: &str, f: impl Fn(String) + 'static) -> Retained<NSPopUpButton> {
        let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(self.mtm), NSRect::ZERO, false);
        let menu = p.menu().expect("a popup has a menu");
        let mut values = Vec::new();
        let mut add = |value: &str, title: &str, sw: Option<Rgba>| {
            let item = unsafe { menu.addItemWithTitle_action_keyEquivalent(&NSString::from_str(title), None, ns_string!("")) };
            if let Some(c) = sw {
                item.setImage(Some(&swatch(c)));
            }
            values.push(value.to_string());
        };
        for o in &items {
            add(&o.value, &o.label, o.swatch);
        }
        // A value of the file that isn't among the choices stays, as itself
        if !selected.is_empty() && !items.iter().any(|o| o.value == selected) {
            add(selected, selected, None);
        }
        if let Some(i) = values.iter().position(|v| v == selected) {
            p.selectItemAtIndex(i as isize);
        }
        let pp = p.clone();
        self.wire(&p, move || {
            if let Some(v) = values.get(pp.indexOfSelectedItem().max(0) as usize) {
                f(v.clone());
            }
        });
        p
    }

    pub fn choice(&mut self, title: &str, help: Option<&str>, items: Vec<Opt>, selected: &str, f: impl Fn(String) + 'static) {
        let p = self.popup(items, selected, f);
        self.row(title, help, &p);
    }

    /// A text field, written when Return is pressed or the focus leaves it.
    pub fn field(&mut self, value: &str, placeholder: &str, width: f64, f: impl Fn(String) + 'static) -> Retained<NSTextField> {
        let field = NSTextField::textFieldWithString(&NSString::from_str(value), self.mtm);
        field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
        field.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        if let Some(cell) = field.cell() {
            cell.setSendsActionOnEndEditing(true);
            cell.setScrollable(true);
        }
        set_width(&field, width);
        let fd = field.clone();
        let last = RefCell::new(value.to_string());
        self.wire(&field, move || {
            let v = fd.stringValue().to_string();
            if v != *last.borrow() {
                *last.borrow_mut() = v.clone();
                f(v);
            }
        });
        field
    }

    pub fn text(&mut self, title: &str, help: Option<&str>, value: &str, placeholder: &str, f: impl Fn(String) + 'static) {
        let field = self.field(value, placeholder, 250.0, f);
        self.row(title, help, &field);
    }

    /// A list of words (widget names, bundle ids…) as tokens, which can be dragged to reorder.
    pub fn token_field(&mut self, values: &[String], placeholder: &str, f: impl Fn(Vec<String>) + 'static) -> Retained<NSTokenField> {
        let field = NSTokenField::new(self.mtm);
        field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
        let list: Vec<Retained<NSString>> = values.iter().map(|s| NSString::from_str(s)).collect();
        let array = NSArray::from_retained_slice(&list);
        let obj: &AnyObject = &array;
        unsafe { field.setObjectValue(Some(obj)) };
        if let Some(cell) = field.cell() {
            cell.setSendsActionOnEndEditing(true);
            cell.setWraps(true);
            cell.setScrollable(false);
        }
        field.setPreferredMaxLayoutWidth(INNER);
        let fd = field.clone();
        let last = RefCell::new(values.to_vec());
        self.wire(&field, move || {
            let v: Vec<String> = fd.stringValue().to_string().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if v != *last.borrow() {
                *last.borrow_mut() = v.clone();
                f(v);
            }
        });
        field
    }

    pub fn tokens(&mut self, title: &str, help: Option<&str>, values: &[String], placeholder: &str, f: impl Fn(Vec<String>) + 'static) {
        let field = self.token_field(values, placeholder, f);
        self.row_wide(title, help, &field);
    }

    pub fn color_well(&mut self, value: Option<Rgba>, f: impl Fn(Rgba) + 'static) -> Retained<NSColorWell> {
        let well = NSColorWell::colorWellWithStyle(NSColorWellStyle::Minimal, self.mtm);
        if let Some(c) = value {
            well.setColor(&ns_color(c));
        }
        let write = debounce(0.3, f);
        let w = well.clone();
        self.wire(&well, move || write(rgba(&w.color())));
        well
    }

    // ---- controls bound to the config ----------------------------------------------------------

    pub fn toggle_at(&mut self, title: &str, help: Option<&str>, spot: &Spot, default: bool) {
        let on = store::get_bool(spot).unwrap_or(default);
        let s = spot.clone();
        self.toggle(title, help, on, move |v| store::put(&s, Value::from(v), Some(&toml::Value::Boolean(default))));
    }

    pub fn number_at(&mut self, title: &str, help: Option<&str>, spot: &Spot, default: f64, n: Num) {
        let value = store::get_f64(spot).unwrap_or(default);
        let s = spot.clone();
        self.number(title, help, value, n, move |v| store::put(&s, store::number(v, n.decimals), Some(&toml::Value::Float(default))));
    }

    /// A text; emptied, the key goes.
    pub fn text_at(&mut self, title: &str, help: Option<&str>, spot: &Spot, placeholder: &str) {
        let value = store::get_str(spot).unwrap_or_default();
        let s = spot.clone();
        self.text(title, help, &value, placeholder, move |v| {
            let v = v.trim();
            if v.is_empty() { store::remove(&s) } else { store::set(&s, Value::from(v)) }
        });
    }

    pub fn choice_at(&mut self, title: &str, help: Option<&str>, spot: &Spot, items: Vec<Opt>, default: &str) {
        let value = store::get_str(spot).unwrap_or_else(|| default.to_string());
        let (s, d) = (spot.clone(), toml::Value::String(default.to_string()));
        self.choice(title, help, items, &value, move |v| store::put(&s, Value::from(v), Some(&d)));
    }

    /// A list of words; emptied, the key goes for RustyBar (whose lists are empty by default).
    pub fn tokens_at(&mut self, title: &str, help: Option<&str>, spot: &Spot, placeholder: &str) {
        self.tokens_or_at(title, help, spot, &[], placeholder);
    }

    /// As `tokens_at`, for a list whose default isn't empty.
    pub fn tokens_or_at(&mut self, title: &str, help: Option<&str>, spot: &Spot, default: &[String], placeholder: &str) {
        let values = store::get_strings(spot).unwrap_or_else(|| default.to_vec());
        let s = spot.clone();
        let default = toml::Value::Array(default.iter().map(|v| toml::Value::String(v.clone())).collect());
        self.tokens(title, help, &values, placeholder, move |v| store::put(&s, store::strings(&v), Some(&default)));
    }
}

/// Opens a config file in the app that opens its kind, or TextEdit if none does.
pub fn open_file(path: &std::path::Path) {
    use objc2_app_kit::{NSWorkspace, NSWorkspaceOpenConfiguration};
    use objc2_foundation::NSURL;
    let ws = NSWorkspace::sharedWorkspace();
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.display().to_string()));
    if ws.URLForApplicationToOpenURL(&url).is_some() {
        ws.openURL(&url);
    } else if let Some(app) = ws.URLForApplicationWithBundleIdentifier(ns_string!("com.apple.TextEdit")) {
        let urls = NSArray::from_retained_slice(&[url]);
        ws.openURLs_withApplicationAtURL_configuration_completionHandler(&urls, &app, &NSWorkspaceOpenConfiguration::configuration(), None);
    }
}

/// Asks for a file, starting in `dir`; `apps` asks for an app instead.
pub fn choose_file(dir: Option<&std::path::Path>, apps: bool) -> Option<std::path::PathBuf> {
    use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
    use objc2_foundation::NSURL;
    let mtm = MainThreadMarker::new()?;
    let panel = NSOpenPanel::openPanel(mtm);
    panel.setCanChooseDirectories(false);
    panel.setAllowsMultipleSelection(false);
    if let Some(d) = dir {
        panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(&d.display().to_string()))));
    }
    if apps {
        // allowedContentTypes would need UniformTypeIdentifiers for this one type
        #[allow(deprecated)]
        panel.setAllowedFileTypes(Some(&NSArray::from_retained_slice(&[NSString::from_str("app")])));
    }
    if panel.runModal() != NSModalResponseOK {
        return None;
    }
    panel.URL().and_then(|u| u.path()).map(|p| std::path::PathBuf::from(p.to_string()))
}

/// The bundle id of the app at `path`.
pub fn bundle_id(path: &std::path::Path) -> Option<String> {
    use objc2_foundation::NSBundle;
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(&path.display().to_string()))?;
    bundle.bundleIdentifier().map(|s| s.to_string())
}

/// The name of the app with `bundle_id`, as the Finder shows it.
pub fn app_name(bundle_id: &str) -> Option<String> {
    use objc2_app_kit::NSWorkspace;
    let url = NSWorkspace::sharedWorkspace().URLForApplicationWithBundleIdentifier(&NSString::from_str(bundle_id))?;
    let name = url.lastPathComponent()?.to_string();
    Some(name.trim_end_matches(".app").to_string())
}
