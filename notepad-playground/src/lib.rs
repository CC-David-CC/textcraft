// SPDX-License-Identifier: MIT
//! The browser host interface and the editor's in-memory copy of opened files.
#![cfg(target_arch = "wasm32")]

use js_sys::{Function, Reflect};
use wasm_bindgen::{JsCast as _, JsValue};

/// `textcraftWorkspace[method](...args)`; None when the page has none or it threw.
pub fn call(method: &str, args: &[JsValue]) -> Option<JsValue> {
    let api = Reflect::get(&js_sys::global(), &"textcraftWorkspace".into())
        .ok()
        .filter(JsValue::is_object)?;
    let f = Reflect::get(&api, &method.into())
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    f.apply(&api, &args.iter().collect::<js_sys::Array>()).ok()
}

pub mod fs {
    // What isn't here (OpenOptions, rename, ...) is std's, which fails on a page.
    use ::std::cell::RefCell;
    use ::std::collections::BTreeMap;
    pub use ::std::fs::*;
    use ::std::io;
    use ::std::path::{Path, PathBuf};

    thread_local! {
        static FILES: RefCell<BTreeMap<PathBuf, Vec<u8>>> = const { RefCell::new(BTreeMap::new()) };
    }

    /// Stores a file that came in from Files or a drop.
    pub fn put(path: &Path, bytes: Vec<u8>) {
        FILES.with_borrow_mut(|f| f.insert(path.to_path_buf(), bytes));
    }

    pub fn read<P: AsRef<Path>>(path: P) -> io::Result<Vec<u8>> {
        FILES
            .with_borrow(|f| f.get(path.as_ref()).cloned())
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }

    pub fn write<P: AsRef<Path>, C: AsRef<[u8]>>(path: P, bytes: C) -> io::Result<()> {
        put(path.as_ref(), bytes.as_ref().to_vec());
        Ok(())
    }
}
