//! The settings window: what the dotfiles configure, edited in place. It opens from the Apple
//! menu (`apple`) or with `rustybar msg settings [page]`. On the left the pages: RustyBar (the bar,
//! the theme, the layout and each widget), AeroSpace (general, gaps, bindings, monitors, window
//! rules), the borders of JankyBorders, SwipeAeroSpace, MiddleClick and the Brewfile; on the right
//! the page. A change is written at once (see `store`) and applied: the bar reloads by itself when
//! its files change, AeroSpace is asked to reload its config, `borders` gets its new options,
//! SwipeAeroSpace reloads by itself, MiddleClick when restarted from its page.
//!
//! It runs in a process of its own, `rustybar settings [page]`, which ends when the window closes:
//! AppKit's controls leave some 35 MB in a process after their window has gone, which the bar
//! shouldn't carry. With made-up data (see `mock`) it edits the configs in `<mock>/settings/` and
//! applies nothing.

mod aerospace;
mod form;
mod other;
mod rustybar;
mod store;

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSClickGestureRecognizer, NSColor, NSImageView,
    NSMenu, NSMenuItem, NSRunningApplication, NSScrollView, NSStackView, NSTextField, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowCollectionBehavior,
    NSWindowDelegate, NSWindowStyleMask, NSWindowTitleVisibility, NSWorkspace, NSWorkspaceOpenConfiguration,
};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSEdgeInsets, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString, ns_string,
};

use self::form::{Flipped, Form, Rounded, Target, label, pin, set_width, stack};
use self::store::{File, Tool};
use crate::gfx::Rgba;
use crate::i18n::t;

const SIDEBAR_W: f64 = 230.0;
const DETAIL_W: f64 = form::GROUP_W + 40.0;
const STATUS_H: f64 = 30.0;

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, sender: &NSWindow) -> bool {
            // The field being edited writes its value before the window goes
            sender.makeFirstResponder(None);
            true
        }

        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _notification: &NSNotification) {
            crate::runtime::later(0.0, close_now);
        }
    }
);

struct Settings {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    _delegate: Retained<WindowDelegate>,
    sidebar: Retained<NSStackView>,
    scroll: Retained<NSScrollView>,
    status: Retained<NSTextField>,
    page: String,
    lua: Vec<String>,
    sidebar_keep: Vec<Retained<Target>>,
    page_keep: Vec<Retained<Target>>,
}

thread_local! {
    static SETTINGS: RefCell<Option<Settings>> = const { RefCell::new(None) };
    static APPLY: Cell<[u64; 3]> = const { Cell::new([0; 3]) };
}

/// Starts the window in a process of its own, on `page` if given (`bar`, `theme`, `layout`,
/// `widget:<name>`, `aerospace`, `gaps`, `keys`, `monitors`, `rules`, `borders`, `swipe`,
/// `middleclick`, `brew`): from the bar, with its config folder, made-up data and language.
pub fn launch(page: Option<&str>) {
    let mut args = vec!["settings".to_string()];
    args.extend(page.map(String::from));
    // AppKit's own flags the bar started with, e.g. `-AppleLanguages '(en)'`
    let argv: Vec<String> = std::env::args().collect();
    for pair in argv.windows(2) {
        if pair[0].starts_with("-Apple") {
            args.extend(pair.iter().cloned());
        }
    }
    let bundle = NSBundle::mainBundle().bundleURL();
    if !bundle.path().is_some_and(|p| p.to_string().ends_with(".app")) {
        // A binary out of its bundle, as cargo builds it
        if let Ok(exe) = std::env::current_exe() {
            crate::runtime::fire(&exe.to_string_lossy(), &args.iter().map(String::as_str).collect::<Vec<_>>());
        }
        return;
    }
    // Through Launch Services, as a click on an app: the new process comes to the front
    let config = NSWorkspaceOpenConfiguration::configuration();
    config.setCreatesNewApplicationInstance(true);
    let args: Vec<Retained<NSString>> = args.iter().map(|a| NSString::from_str(a)).collect();
    config.setArguments(&NSArray::from_retained_slice(&args));
    let env: Vec<(Retained<NSString>, Retained<NSString>)> =
        std::env::vars().filter(|(k, _)| k.starts_with("RUSTYBAR_")).map(|(k, v)| (NSString::from_str(&k), NSString::from_str(&v))).collect();
    let keys: Vec<&NSString> = env.iter().map(|(k, _)| &**k).collect();
    let values: Vec<&NSString> = env.iter().map(|(_, v)| &**v).collect();
    config.setEnvironment(&NSDictionary::from_slices(&keys, &values));
    NSWorkspace::sharedWorkspace().openApplicationAtURL_configuration_completionHandler(&bundle, &config, None);
}

/// `rustybar settings [page]`: the window, in this process, which ends when it closes. With one
/// already open for the same config folder, that one shows `page` and comes to the front instead.
pub fn run(page: Option<String>) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let app = NSApplication::sharedApplication(mtm);
    let path = crate::ipc::settings_socket_path();
    if let Ok(mut stream) = UnixStream::connect(&path) {
        // It says its pid, gets the focus this process was launched with, then the page
        let mut line = String::new();
        let _ = BufReader::new(&stream).read_line(&mut line);
        if let Some(other) = line.trim().parse().ok().and_then(NSRunningApplication::runningApplicationWithProcessIdentifier) {
            app.yieldActivationToApplication(&other);
        }
        let _ = stream.write_all(format!("{}\n", page.unwrap_or_default()).as_bytes());
        let _ = stream.read_to_end(&mut Vec::new());
        return;
    }
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    serve(&path);
    create(mtm, lua_names());
    match page {
        Some(p) => select(&p),
        None => refresh(false),
    }
    show();
    app.run();
}

/// Answers the `rustybar settings` started while this one is open (see `run`).
fn serve(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let Ok(listener) = UnixListener::bind(path) else { return };
    crate::runtime::spawn("settings-socket", move || {
        for mut stream in listener.incoming().flatten() {
            let _ = stream.write_all(format!("{}\n", std::process::id()).as_bytes());
            let mut page = String::new();
            let _ = BufReader::new(&stream).read_line(&mut page);
            let page = page.trim().to_string();
            crate::runtime::on_main(move || {
                if !page.is_empty() {
                    select(&page);
                }
                show();
            });
        }
    });
}

/// The widgets the Lua files define, read here as `rustybar check` reads them.
fn lua_names() -> Vec<String> {
    let Ok(config) = crate::config::load() else { return Vec::new() };
    let theme = crate::theme::Theme::from_config(&config.theme);
    crate::lua::Host::load(&config, &theme).map(|(host, _)| host.names()).unwrap_or_default()
}

/// Brings the window to the front, with no field in edit until one is clicked (AppKit would pick
/// the first).
fn show() {
    SETTINGS.with(|s| {
        let s = s.borrow();
        let Some(s) = s.as_ref() else { return };
        // Not `activate()`: it asks, and macOS says no to a process the user hasn't clicked yet,
        // as this one is when the bar starts it
        #[allow(deprecated)]
        NSApplication::sharedApplication(s.mtm).activateIgnoringOtherApps(true);
        s.window.makeKeyAndOrderFront(None);
        s.window.makeFirstResponder(None);
    });
}

fn create(mtm: MainThreadMarker, lua: Vec<String>) {
    store::clear();
    install_menu(mtm);
    let width = SIDEBAR_W + DETAIL_W;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(width, 680.0)),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(t("Impostazioni dotfiles", "Dotfiles Settings")));
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    // As wide as the pages, as tall as one likes
    window.setContentMinSize(NSSize::new(width, 420.0));
    window.setContentMaxSize(NSSize::new(width, 10000.0));
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenNone | NSWindowCollectionBehavior::MoveToActiveSpace);
    window.setFrameAutosaveName(ns_string!("RustyBarSettings"));
    if window.setFrameUsingName(ns_string!("RustyBarSettings")) {
        // The saved frame keeps its place and height, but not a width of another version
        let mut content = window.contentRectForFrameRect(window.frame());
        content.size.width = width;
        window.setFrame_display(window.frameRectForContentRect(content), false);
    } else {
        window.center();
    }
    let delegate: Retained<WindowDelegate> = unsafe { msg_send![WindowDelegate::alloc(mtm), init] };
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

    let root = NSView::new(mtm);
    window.setContentView(Some(&root));

    // The sidebar, under the title bar as in System Settings
    let side = NSVisualEffectView::new(mtm);
    side.setMaterial(NSVisualEffectMaterial::Sidebar);
    side.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    side.setState(NSVisualEffectState::FollowsWindowActiveState);
    side.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&side);
    side.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    side.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);
    side.leadingAnchor().constraintEqualToAnchor(&root.leadingAnchor()).setActive(true);
    side.widthAnchor().constraintEqualToConstant(SIDEBAR_W).setActive(true);
    let side_scroll = scroll_view(mtm);
    pin(&side_scroll, &side, (0.0, 0.0, 0.0, 0.0));
    let sidebar = stack(mtm, true, 1.0);
    sidebar.setEdgeInsets(NSEdgeInsets { top: 46.0, left: 10.0, bottom: 16.0, right: 10.0 });
    document(&side_scroll, &sidebar);

    // The page
    let scroll = scroll_view(mtm);
    scroll.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&scroll);
    scroll.topAnchor().constraintEqualToAnchor(&root.topAnchor()).setActive(true);
    scroll.leadingAnchor().constraintEqualToAnchor(&side.trailingAnchor()).setActive(true);
    scroll.trailingAnchor().constraintEqualToAnchor(&root.trailingAnchor()).setActive(true);

    // What happened to the last change, under the page
    let bar = NSView::new(mtm);
    bar.setTranslatesAutoresizingMaskIntoConstraints(false);
    root.addSubview(&bar);
    bar.topAnchor().constraintEqualToAnchor(&scroll.bottomAnchor()).setActive(true);
    bar.leadingAnchor().constraintEqualToAnchor(&side.trailingAnchor()).setActive(true);
    bar.trailingAnchor().constraintEqualToAnchor(&root.trailingAnchor()).setActive(true);
    bar.bottomAnchor().constraintEqualToAnchor(&root.bottomAnchor()).setActive(true);
    bar.heightAnchor().constraintEqualToConstant(STATUS_H).setActive(true);
    let sep = objc2_app_kit::NSBox::new(mtm);
    sep.setBoxType(objc2_app_kit::NSBoxType::Separator);
    sep.setTranslatesAutoresizingMaskIntoConstraints(false);
    bar.addSubview(&sep);
    sep.topAnchor().constraintEqualToAnchor(&bar.topAnchor()).setActive(true);
    sep.leadingAnchor().constraintEqualToAnchor(&bar.leadingAnchor()).setActive(true);
    sep.trailingAnchor().constraintEqualToAnchor(&bar.trailingAnchor()).setActive(true);
    let status = label(mtm, "", 11.0, false, true);
    status.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    // Below the window's own size (500): a long path gets cut, the window doesn't grow
    status.setContentCompressionResistancePriority_forOrientation(250.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
    status.setTranslatesAutoresizingMaskIntoConstraints(false);
    bar.addSubview(&status);
    status.leadingAnchor().constraintEqualToAnchor_constant(&bar.leadingAnchor(), 20.0).setActive(true);
    bar.trailingAnchor().constraintEqualToAnchor_constant(&status.trailingAnchor(), 20.0).setActive(true);
    status.centerYAnchor().constraintEqualToAnchor(&bar.centerYAnchor()).setActive(true);

    let settings = Settings {
        mtm,
        window,
        _delegate: delegate,
        sidebar,
        scroll,
        status,
        page: "bar".into(),
        lua,
        sidebar_keep: Vec::new(),
        page_keep: Vec::new(),
    };
    SETTINGS.with(|s| *s.borrow_mut() = Some(settings));
}

fn scroll_view(mtm: MainThreadMarker) -> Retained<NSScrollView> {
    let s = NSScrollView::new(mtm);
    s.setHasVerticalScroller(true);
    s.setDrawsBackground(false);
    s.setAutohidesScrollers(true);
    s
}

/// Makes `content` the document of `scroll`, as wide as it and starting at the top.
fn document(scroll: &NSScrollView, content: &NSView) {
    let mtm = scroll.mtm();
    let doc = Flipped::new(mtm);
    pin(content, &doc, (0.0, 0.0, 0.0, 0.0));
    doc.setTranslatesAutoresizingMaskIntoConstraints(false);
    scroll.setDocumentView(Some(&doc));
    let clip = scroll.contentView();
    doc.topAnchor().constraintEqualToAnchor(&clip.topAnchor()).setActive(true);
    doc.leadingAnchor().constraintEqualToAnchor(&clip.leadingAnchor()).setActive(true);
    doc.widthAnchor().constraintEqualToAnchor(&clip.widthAnchor()).setActive(true);
}

/// The Edit menu, never shown (the window has no menu bar) but whose shortcuts make ⌘C, ⌘V, ⌘X,
/// ⌘A and ⌘Z work in the text fields, and ⌘W close the window.
fn install_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if app.mainMenu().is_some() {
        return;
    }
    let main = NSMenu::new(mtm);
    let add_menu = |items: &[(&str, &str, objc2::runtime::Sel)]| {
        let menu = NSMenu::new(mtm);
        for (title, key, action) in items {
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(*action), &NSString::from_str(key))
            };
            menu.addItem(&item);
        }
        let holder = NSMenuItem::new(mtm);
        holder.setSubmenu(Some(&menu));
        main.addItem(&holder);
    };
    add_menu(&[("Close", "w", sel!(performClose:))]);
    add_menu(&[
        ("Undo", "z", sel!(undo:)),
        ("Redo", "Z", sel!(redo:)),
        ("Cut", "x", sel!(cut:)),
        ("Copy", "c", sel!(copy:)),
        ("Paste", "v", sel!(paste:)),
        ("Select All", "a", sel!(selectAll:)),
    ]);
    app.setMainMenu(Some(&main));
}

/// After the window has closed: no new `rustybar settings` comes to this one, and the process
/// ends once what was still to write or apply has gone (a color well waits 0.3 s, `apply` 0.5 s).
fn close_now() {
    let _ = std::fs::remove_file(crate::ipc::settings_socket_path());
    crate::runtime::later(1.5, || std::process::exit(0));
}

/// Shows `page`, after the field being edited has written its value.
pub fn select(page: &str) {
    SETTINGS.with(|s| {
        if let Some(s) = s.borrow_mut().as_mut() {
            s.window.makeFirstResponder(None);
        }
    });
    let page = page.to_string();
    crate::runtime::later(0.0, move || {
        let changed = SETTINGS.with(|s| {
            let mut s = s.borrow_mut();
            let Some(s) = s.as_mut() else { return false };
            let changed = s.page != page;
            s.page = page;
            changed
        });
        refresh(!changed);
    });
}

/// Builds the sidebar and the page again, after what changed the config, keeping the scroll when
/// `keep_scroll`.
pub fn refresh(keep_scroll: bool) {
    SETTINGS.with(|cell| {
        let mut guard = cell.borrow_mut();
        let Some(s) = guard.as_mut() else { return };
        build_sidebar(s);
        let clip = s.scroll.contentView();
        let y = if keep_scroll { clip.bounds().origin.y } else { 0.0 };
        let mut f = Form::new(s.mtm);
        if let Some(tool) = tool_of(&s.page)
            && let Some(e) = store::error(tool)
        {
            let warning = format!("{} {e}", t("Il file non si legge, e finché non è corretto qui non si salva niente:", "The file doesn't parse, and nothing is saved here until it's fixed:"));
            let p = form::paragraph(s.mtm, &warning, 12.0, false, form::GROUP_W);
            p.setTextColor(Some(&NSColor::systemRedColor()));
            f.add_view(&p);
        }
        build_page(&s.page, &mut f, &s.lua);
        document(&s.scroll, &f.column);
        // The old page's targets go only now: none of their closures is running
        s.page_keep = std::mem::take(&mut f.keep);
        if let Some(doc) = s.scroll.documentView() {
            doc.layoutSubtreeIfNeeded();
        }
        clip.scrollToPoint(NSPoint::new(0.0, y));
        s.scroll.reflectScrolledClipView(&clip);
        s.window.makeFirstResponder(None);
    });
}

/// Builds the sidebar again, but not the page, whose fields may be in use: after a change of the
/// layout, which reorders the widgets there.
pub fn refresh_sidebar_later() {
    crate::runtime::later(0.0, || {
        SETTINGS.with(|cell| {
            if let Some(s) = cell.borrow_mut().as_mut() {
                build_sidebar(s);
            }
        });
    });
}

/// `refresh` once the closure that asks for it has returned: for the controls that add, remove or
/// move something, after which the page changes shape.
pub fn refresh_later() {
    crate::runtime::later(0.0, || refresh(true));
}

fn build_page(page: &str, f: &mut Form, lua: &[String]) {
    match page {
        "bar" => rustybar::bar(f),
        "theme" => rustybar::theme(f),
        "layout" => rustybar::layout(f, lua),
        "aerospace" => aerospace::general(f),
        "gaps" => aerospace::gaps(f),
        "keys" => aerospace::keys(f),
        "monitors" => aerospace::monitors(f),
        "rules" => aerospace::rules(f),
        "borders" => aerospace::borders(f),
        "swipe" => other::swipe(f),
        "middleclick" => other::middleclick(f),
        "brew" => other::brew(f),
        p => match p.strip_prefix("widget:") {
            Some(name) => rustybar::widget(f, name, lua),
            None => rustybar::bar(f),
        },
    }
}

fn tool_of(page: &str) -> Option<Tool> {
    match page {
        "aerospace" | "gaps" | "keys" | "monitors" | "rules" | "borders" => Some(Tool::AeroSpace),
        "swipe" => Some(Tool::Swipe),
        "middleclick" | "brew" => None,
        _ => Some(Tool::RustyBar),
    }
}

struct Entry {
    id: String,
    title: String,
    symbol: &'static str,
    color: Rgba,
    dim: bool,
}

fn entry(id: &str, title: &str, symbol: &'static str, color: &str) -> Entry {
    Entry { id: id.into(), title: title.into(), symbol, color: Rgba::parse(color).unwrap_or(Rgba(0.5, 0.5, 0.5, 1.0)), dim: false }
}

/// The pages, by section; a tool shows only when it's there.
fn entries(lua: &[String]) -> Vec<(&'static str, Vec<Entry>)> {
    let mut out = vec![(
        "RustyBar",
        vec![
            entry("bar", t("Barra", "Bar"), "menubar.rectangle", "#e8763a"),
            entry("theme", t("Tema", "Theme"), "paintpalette.fill", "#c062d8"),
            entry("layout", t("Disposizione", "Layout"), "rectangle.split.3x1.fill", "#3a8ee8"),
        ],
    )];
    let widgets = rustybar::widget_entries(lua)
        .into_iter()
        .map(|(name, title, symbol, used)| Entry { id: format!("widget:{name}"), title, symbol, color: Rgba::parse("#6e7481").expect("hex"), dim: !used })
        .collect();
    out.push((t("Widget", "Widgets"), widgets));
    if store::path(File::AeroSpace).exists() {
        out.push((
            "AeroSpace",
            vec![
                entry("aerospace", t("Generale", "General"), "gearshape.fill", "#5b6b84"),
                entry("gaps", t("Spaziature", "Gaps"), "rectangle.inset.filled", "#3aa0a8"),
                entry("keys", t("Scorciatoie", "Bindings"), "keyboard.fill", "#4f6bdb"),
                entry("monitors", t("Monitor", "Monitors"), "display.2", "#2d9b5a"),
                entry("rules", t("Regole delle finestre", "Window rules"), "macwindow.badge.plus", "#d0882e"),
            ],
        ));
    }
    let mut more = Vec::new();
    if aerospace::borders_available() {
        more.push(entry("borders", t("Bordi", "Borders"), "square.dashed", "#e05b8d"));
    }
    if store::path(File::Swipe).exists() {
        more.push(entry("swipe", "SwipeAeroSpace", "hand.draw.fill", "#8a5be0"));
    }
    if other::middleclick_available() {
        more.push(entry("middleclick", "MiddleClick", "computermouse.fill", "#7d8590"));
    }
    if other::brewfile().is_some() {
        more.push(entry("brew", "Homebrew", "mug.fill", "#c9a03a"));
    }
    if !more.is_empty() {
        out.push((t("Altro", "More"), more));
    }
    out
}

fn build_sidebar(s: &mut Settings) {
    let mtm = s.mtm;
    for v in s.sidebar.arrangedSubviews().iter() {
        s.sidebar.removeArrangedSubview(&v);
        v.removeFromSuperview();
    }
    let mut keep = Vec::new();
    for (n, (section, items)) in entries(&s.lua).into_iter().enumerate() {
        let title = label(mtm, section, 11.0, true, true);
        let wrap = stack(mtm, false, 0.0);
        wrap.setEdgeInsets(NSEdgeInsets { top: if n == 0 { 0.0 } else { 14.0 }, left: 8.0, bottom: 4.0, right: 0.0 });
        wrap.addArrangedSubview(&title);
        s.sidebar.addArrangedSubview(&wrap);
        for e in items {
            let selected = e.id == s.page;
            let fill = selected.then(NSColor::controlAccentColor);
            let row = Rounded::new(mtm, fill, 6.0);
            set_width(&row, SIDEBAR_W - 20.0);
            row.heightAnchor().constraintEqualToConstant(28.0).setActive(true);
            let content = stack(mtm, false, 8.0);
            content.setEdgeInsets(NSEdgeInsets { top: 0.0, left: 6.0, bottom: 0.0, right: 6.0 });
            content.addArrangedSubview(&icon_tile(mtm, e.symbol, e.color));
            let text = label(mtm, &e.title, 13.0, false, false);
            text.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
            let color = if selected {
                NSColor::alternateSelectedControlTextColor()
            } else if e.dim {
                NSColor::secondaryLabelColor()
            } else {
                NSColor::labelColor()
            };
            text.setTextColor(Some(&color));
            content.addArrangedSubview(&text);
            pin(&content, &row, (0.0, 0.0, 0.0, 0.0));
            let id = e.id.clone();
            let target = Target::new(mtm, move || select(&id));
            let obj: &AnyObject = &target;
            let click = unsafe { NSClickGestureRecognizer::initWithTarget_action(NSClickGestureRecognizer::alloc(mtm), Some(obj), Some(sel!(fire:))) };
            row.addGestureRecognizer(&click);
            keep.push(target);
            s.sidebar.addArrangedSubview(&row);
        }
    }
    s.sidebar_keep = keep;
}

/// The colored square with a white symbol of a page, as in System Settings.
fn icon_tile(mtm: MainThreadMarker, symbol: &str, color: Rgba) -> Retained<NSView> {
    let tile = Rounded::new(mtm, Some(form::ns_color(color)), 5.0);
    tile.setTranslatesAutoresizingMaskIntoConstraints(false);
    tile.widthAnchor().constraintEqualToConstant(20.0).setActive(true);
    tile.heightAnchor().constraintEqualToConstant(20.0).setActive(true);
    if let Some(image) = form::symbol(symbol, 11.0) {
        let view = NSImageView::imageViewWithImage(&image, mtm);
        view.setContentTintColor(Some(&NSColor::whiteColor()));
        view.setTranslatesAutoresizingMaskIntoConstraints(false);
        tile.addSubview(&view);
        view.centerXAnchor().constraintEqualToAnchor(&tile.centerXAnchor()).setActive(true);
        view.centerYAnchor().constraintEqualToAnchor(&tile.centerYAnchor()).setActive(true);
    }
    objc2::rc::Retained::into_super(tile)
}

/// Says what happened to the last change, under the page; red for an error.
pub fn status(text: &str, error: bool) {
    let text = text.to_string();
    crate::runtime::later(0.0, move || {
        SETTINGS.with(|s| {
            if let Some(s) = s.borrow().as_ref() {
                s.status.setStringValue(&NSString::from_str(&text));
                let color = if error { NSColor::systemRedColor() } else { NSColor::secondaryLabelColor() };
                s.status.setTextColor(Some(&color));
                s.status.setToolTip(Some(&NSString::from_str(&text)));
            }
        });
    });
}

/// A file was written: says so and, half a second after the last change, applies it.
fn saved(tool: Tool, file: File) {
    status(&format!("{} {}", t("Salvato in", "Saved to"), store::short_path(file)), false);
    let i = tool as usize;
    let mut gens = APPLY.with(Cell::get);
    gens[i] += 1;
    let g = gens[i];
    APPLY.with(|a| a.set(gens));
    crate::runtime::later(0.5, move || {
        if APPLY.with(Cell::get)[i] == g {
            apply(tool, file);
        }
    });
}

fn apply(tool: Tool, file: File) {
    let name = store::short_path(file);
    match tool {
        // The bar follows its files; this only says if they are wrong
        Tool::RustyBar => match crate::config::load() {
            Ok(_) => status(&format!("{} {name} · {}", t("Salvato in", "Saved to"), t("la barra si è ricaricata", "the bar reloaded")), false),
            Err(e) => status(&e, true),
        },
        Tool::AeroSpace if store::mock_dir().is_none() => {
            crate::runtime::spawn("settings-aerospace", move || {
                let result = crate::widgets::aerospace::client::run(&["reload-config", "--no-gui"]);
                crate::runtime::on_main(move || match result {
                    Ok(_) => status(&format!("{} {name} · {}", t("Salvato in", "Saved to"), t("AeroSpace ha ricaricato la config", "AeroSpace reloaded its config")), false),
                    Err(e) => status(&format!("AeroSpace: {}", e.lines().next().unwrap_or("")), true),
                });
            });
        }
        Tool::Swipe => status(&format!("{} {name} · {}", t("Salvato in", "Saved to"), t("SwipeAeroSpace lo ricarica da sola", "SwipeAeroSpace reloads it by itself")), false),
        _ => {}
    }
}
