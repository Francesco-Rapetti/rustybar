//! The language of the texts: Italian when the Mac's first language is Italian, English otherwise.

use std::sync::OnceLock;

use objc2_foundation::NSLocale;

pub fn italian() -> bool {
    static IT: OnceLock<bool> = OnceLock::new();
    *IT.get_or_init(|| {
        NSLocale::preferredLanguages()
            .firstObject()
            .map(|l| l.to_string().starts_with("it"))
            .unwrap_or(false)
    })
}

/// The Italian or the English text.
pub fn t<'a>(it: &'a str, en: &'a str) -> &'a str {
    if italian() { it } else { en }
}
