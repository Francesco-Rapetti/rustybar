//! The name of the app in front, from NSWorkspace's activation notification.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSRunningApplication, NSWorkspace, NSWorkspaceApplicationKey, NSWorkspaceDidActivateApplicationNotification};
use objc2_foundation::{NSNotification, NSObjectProtocol};

use super::{Cx, Rx, Widget};
use crate::ui::Node;

#[derive(Default)]
pub struct FrontApp {
    name: String,
    bundle: String,
    observer: Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>,
}

impl FrontApp {
    fn set(&mut self, app: &NSRunningApplication) {
        self.name = app.localizedName().map(|n| n.to_string()).unwrap_or_default();
        self.bundle = app.bundleIdentifier().map(|n| n.to_string()).unwrap_or_default();
    }
}

impl Widget for FrontApp {
    fn start(&mut self, cx: &mut Cx) {
        let ws = NSWorkspace::sharedWorkspace();
        if let Some(app) = ws.frontmostApplication() {
            self.set(&app);
        }
        let handle = cx.handle::<FrontApp>();
        let block = RcBlock::new(move |n: NonNull<NSNotification>| {
            let n = unsafe { n.as_ref() };
            let app = n
                .userInfo()
                .and_then(|info| info.objectForKey(unsafe { NSWorkspaceApplicationKey }))
                .and_then(|o| o.downcast::<NSRunningApplication>().ok());
            if let Some(app) = app {
                let (name, bundle) = (
                    app.localizedName().map(|n| n.to_string()).unwrap_or_default(),
                    app.bundleIdentifier().map(|n| n.to_string()).unwrap_or_default(),
                );
                handle.send(move |w, cx| {
                    w.name = name;
                    w.bundle = bundle;
                    cx.redraw();
                });
            }
        });
        self.observer = Some(unsafe {
            ws.notificationCenter().addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceDidActivateApplicationNotification),
                None,
                None,
                &block,
            )
        });
    }

    fn bar(&self, rx: &Rx) -> Option<Node> {
        if self.name.is_empty() {
            return None;
        }
        let th = rx.theme;
        Some(rx.pill(vec![Node::text(&self.name, th.font(), th.text).max_width(260.0)]))
    }

    crate::any_widget!();
}

impl Drop for FrontApp {
    fn drop(&mut self) {
        if let Some(o) = self.observer.take() {
            unsafe { NSWorkspace::sharedWorkspace().notificationCenter().removeObserver(o.as_ref()) };
        }
    }
}
