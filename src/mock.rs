//! Made-up data, for screenshots and tests: with `RUSTYBAR_MOCK` set to a folder, the widgets that
//! show personal data (events, networks, devices…) read `<folder>/<widget>.json` instead, and
//! nothing of yours, nor a permission prompt, ever reaches the bar.

use serde::de::DeserializeOwned;

/// None when mocking is off; else the widget's file, or its default when there's none.
pub fn load<T: DeserializeOwned + Default>(name: &str) -> Option<T> {
    let dir = std::env::var_os("RUSTYBAR_MOCK")?;
    let path = std::path::Path::new(&dir).join(format!("{name}.json"));
    let Ok(text) = std::fs::read_to_string(&path) else { return Some(T::default()) };
    match serde_json::from_str(&text) {
        Ok(data) => Some(data),
        Err(e) => {
            crate::log(&format!("{}: {e}", path.display()));
            Some(T::default())
        }
    }
}
