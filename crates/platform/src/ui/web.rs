// SPDX-License-Identifier: MIT
//! The WebOS Files adapter for the Rust editor.

use super::{
    actions,
    state::{DocKind, SaveThen, Shell, Tab},
};
use nc_textproc::{Encoding, encoding};
use notepad_playground::{call, fs};
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use wasm_bindgen::{JsCast as _, JsValue, closure::Closure};

const TYPES: [&str; 18] = [
    "txt", "md", "markdown", "log", "ini", "cfg", "conf", "json", "xml", "yaml", "yml", "toml",
    "csv", "tsv", "html", "htm", "css", "js",
];

struct Incoming {
    path: PathBuf,
    bytes: Vec<u8>,
    revision: Option<String>,
}

struct Snapshot {
    id: u64,
    path: PathBuf,
    text: String,
    bytes: Vec<u8>,
    encoding: Encoding,
    bom: bool,
    then: SaveThen,
}

enum SaveKind {
    Current { lossy: bool },
    PickPath,
}

enum Event {
    Open(Incoming),
    SaveRequested {
        id: u64,
        kind: SaveKind,
        then: SaveThen,
    },
    Picked {
        id: u64,
        result: Result<Option<PathBuf>, String>,
        then: SaveThen,
    },
    Written {
        snapshot: Snapshot,
        result: Result<String, String>,
    },
    Error(String),
}

thread_local! {
    static CTX: OnceCell<egui::Context> = const { OnceCell::new() };
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
    static UNSAVED: Cell<bool> = const { Cell::new(false) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
    static SAVING: RefCell<BTreeSet<u64>> = const { RefCell::new(BTreeSet::new()) };
    static BINDING: RefCell<Option<(PathBuf, String)>> = const { RefCell::new(None) };
}

pub(super) fn next_document_id() -> u64 {
    NEXT_ID.with(|id| {
        let next = id.get();
        id.set(next.wrapping_add(1));
        next
    })
}

pub(super) fn command_available(command: nc_commands::CommandId) -> bool {
    !matches!(
        command.as_str(),
        "cmd.file.new_window" | "cmd.file.page_setup" | "cmd.file.print"
    )
}

pub(super) fn busy() -> bool {
    SAVING.with_borrow(|documents| !documents.is_empty())
}

fn push(event: Event) {
    EVENTS.with_borrow_mut(|events| events.push(event));
    CTX.with(|ctx| ctx.get().map(egui::Context::request_repaint));
}

pub(crate) mod eframe {
    pub use ::eframe::*;

    #[derive(Default)]
    #[allow(dead_code)]
    pub struct NativeOptions {
        pub viewport: egui::ViewportBuilder,
    }

    pub fn run_native(
        _name: &str,
        _options: NativeOptions,
        creator: ::eframe::AppCreator<'static>,
    ) -> ::eframe::Result {
        super::start(creator);
        Ok(())
    }
}

fn get(object: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(object, &key.into()).unwrap_or(JsValue::UNDEFINED)
}

fn workspace_path(value: &JsValue) -> Option<PathBuf> {
    let path = get(value, "path").as_string()?;
    path.strip_prefix("browser:/")
        .map(|path| Path::new("/").join(path))
}

async fn reply(value: Option<JsValue>) -> Result<Option<JsValue>, String> {
    let promise = value
        .and_then(|value| value.dyn_into::<js_sys::Promise>().ok())
        .ok_or_else(|| "WebOS Files is unavailable.".to_owned())?;
    wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map(|value| Some(value).filter(|value| !value.is_null() && !value.is_undefined()))
        .map_err(|error| {
            get(&error, "message")
                .as_string()
                .unwrap_or_else(|| "The Files operation failed.".into())
        })
}

fn incoming(file: JsValue) {
    if let Some(path) = workspace_path(&file) {
        let revision = get(&file, "sha256").as_string();
        let bytes = js_sys::Uint8Array::new(&get(&file, "bytes")).to_vec();
        push(Event::Open(Incoming {
            path,
            bytes,
            revision,
        }));
    }
}

pub(super) fn open_dialog() {
    let types: js_sys::Array = TYPES.iter().map(|kind| JsValue::from(*kind)).collect();
    let asked = call("open", &[types.into()]);
    wasm_bindgen_futures::spawn_local(async move {
        match reply(asked).await {
            Ok(Some(file)) => incoming(file),
            Ok(None) => {}
            Err(error) => push(Event::Error(error)),
        }
    });
}

pub(super) fn reopen(path: &Path) {
    let requested = call("readOpen", &[format!("browser:{}", path.display()).into()]);
    wasm_bindgen_futures::spawn_local(async move {
        match reply(requested).await {
            Ok(Some(file)) => incoming(file),
            Ok(None) => {}
            Err(error) => push(Event::Error(error)),
        }
    });
}

fn take_then(shell: &mut Shell, tab: usize) -> SaveThen {
    if shell
        .save_then
        .as_ref()
        .is_some_and(|(candidate, _)| *candidate == tab)
    {
        return shell
            .save_then
            .take()
            .map_or(SaveThen::Keep, |(_, then)| then);
    }
    SaveThen::Keep
}

pub(super) fn save_as_dialog(shell: &mut Shell, tab: usize) {
    queue_save(shell, tab, SaveKind::PickPath);
}

pub(super) fn save(shell: &mut Shell, tab: usize, lossy: bool) {
    queue_save(shell, tab, SaveKind::Current { lossy });
}

fn queue_save(shell: &mut Shell, tab: usize, kind: SaveKind) {
    let Some(id) = shell.tabs.get(tab).map(|document| document.workspace_id) else {
        return;
    };
    let then = take_then(shell, tab);
    // The editor processes this frame's text input before we take the save snapshot.
    push(Event::SaveRequested { id, kind, then });
}

fn ask_save_path(document: &Tab, then: SaveThen) {
    let id = document.workspace_id;
    if !SAVING.with_borrow_mut(|saving| saving.insert(id)) {
        return;
    }
    let extension = if document.kind == DocKind::Markdown {
        ".md"
    } else {
        ".txt"
    };
    let name = if document.file.has_path() {
        document.title()
    } else {
        format!("Untitled{extension}")
    };
    let asked = call("saveAs", &[name.into(), extension.into()]);
    wasm_bindgen_futures::spawn_local(async move {
        let result = reply(asked)
            .await
            .map(|value| value.as_ref().and_then(workspace_path));
        push(Event::Picked { id, result, then });
    });
}

fn save_requested(shell: &mut Shell, id: u64, kind: SaveKind, then: SaveThen) {
    let Some(tab) = shell
        .tabs
        .iter()
        .position(|document| document.workspace_id == id)
    else {
        return;
    };
    let document = &shell.tabs[tab];
    if let SaveKind::Current { lossy } = kind
        && document.workspace_revision.is_some()
    {
        let path = document.file.path().to_owned();
        write(shell, tab, path, then, lossy);
    } else {
        ask_save_path(document, then);
    }
}

fn write(shell: &mut Shell, tab: usize, path: PathBuf, then: SaveThen, lossy: bool) {
    let Some(document) = shell.tabs.get(tab) else {
        return;
    };
    if shell
        .tabs
        .iter()
        .enumerate()
        .any(|(index, other)| index != tab && other.matches_path(&path))
    {
        shell.notification = Some("That file is already open in another tab.".into());
        return;
    }
    let id = document.workspace_id;
    if !SAVING.with_borrow_mut(|saving| saving.insert(id)) {
        return;
    }
    let (encoding, bom) = (document.file.encoding(), document.file.has_bom());
    let text = document.document().save_text();
    let bytes = if lossy {
        encoding::encode_lossy(&text, encoding, bom).0
    } else {
        match encoding::encode(&text, encoding, bom) {
            Ok(bytes) => bytes,
            Err(error) => {
                SAVING.with_borrow_mut(|saving| saving.remove(&id));
                shell.notification = Some(error.to_string());
                return;
            }
        }
    };
    let base = document
        .workspace_revision
        .as_ref()
        .filter(|_| path == document.file.path());
    let base = base.map_or(JsValue::NULL, |sha| sha.as_str().into());
    let data = js_sys::Uint8Array::from(bytes.as_slice());
    let pending = call(
        "persist",
        &[
            format!("browser:{}", path.display()).into(),
            data.into(),
            base,
        ],
    );
    let snapshot = Snapshot {
        id,
        path,
        text,
        bytes,
        encoding,
        bom,
        then,
    };
    wasm_bindgen_futures::spawn_local(async move {
        let result = reply(pending).await.and_then(|receipt| {
            receipt
                .and_then(|value| value.as_string())
                .ok_or_else(|| "Files did not confirm the save.".into())
        });
        push(Event::Written { snapshot, result });
    });
}

fn accept_open(shell: &mut Shell, file: Incoming) {
    if !shell.tabs.iter().any(|tab| tab.matches_path(&file.path)) {
        fs::put(&file.path, file.bytes);
    } else {
        actions::open_path(shell, &file.path, None);
        return;
    }
    actions::open_path(shell, &file.path, None);
    if let Some(tab) = shell
        .tabs
        .iter_mut()
        .find(|tab| tab.matches_path(&file.path))
    {
        tab.workspace_revision = file.revision;
    } else if let Some(pending) = &mut shell.pending_replace
        && pending.opening.matches_path(&file.path)
    {
        pending.opening.workspace_revision = file.revision;
    }
}

fn accept_write(shell: &mut Shell, snapshot: Snapshot, result: Result<String, String>) {
    SAVING.with_borrow_mut(|saving| saving.remove(&snapshot.id));
    let Some(index) = shell
        .tabs
        .iter()
        .position(|tab| tab.workspace_id == snapshot.id)
    else {
        return;
    };
    let revision = match result {
        Ok(revision) => revision,
        Err(error) => {
            call("status", &[error.as_str().into()]);
            shell.notification = Some(error);
            return;
        }
    };
    fs::put(&snapshot.path, snapshot.bytes.clone());
    let tab = &mut shell.tabs[index];
    tab.file.record_workspace_write(
        snapshot.path,
        snapshot.encoding,
        snapshot.bom,
        snapshot.bytes,
        &snapshot.text,
    );
    tab.workspace_revision = Some(revision);
    let message = format!("Saved {}", tab.title());
    call("status", &[message.as_str().into()]);
    shell.notification = Some(message);
    if !tab.is_dirty() && snapshot.then != SaveThen::Keep {
        shell.save_then = Some((index, snapshot.then));
        actions::finish_save(shell, index);
    }
}

pub(super) fn frame(shell: &mut Shell, ctx: &egui::Context) {
    if !busy() {
        for file in ctx.input(|input| input.raw.dropped_files.clone()) {
            wasm_bindgen_futures::spawn_local(async move {
                if let (Ok(bytes), Some(name)) = (file.bytes_async().await, file.path().file_name())
                {
                    let path = Path::new("/TextCraft/Imported").join(name);
                    push(Event::Open(Incoming {
                        path,
                        bytes,
                        revision: None,
                    }));
                }
            });
        }
    }
    for event in EVENTS.take() {
        match event {
            Event::Open(file) => accept_open(shell, file),
            Event::SaveRequested { id, kind, then } => save_requested(shell, id, kind, then),
            Event::Picked { id, result, then } => {
                SAVING.with_borrow_mut(|saving| saving.remove(&id));
                match result {
                    Ok(Some(path)) => {
                        if let Some(tab) = shell.tabs.iter().position(|tab| tab.workspace_id == id)
                        {
                            write(shell, tab, path, then, false);
                        }
                    }
                    Ok(None) => {}
                    Err(error) => shell.notification = Some(error),
                }
            }
            Event::Written { snapshot, result } => accept_write(shell, snapshot, result),
            Event::Error(error) => shell.notification = Some(error),
        }
    }
    let unsaved = shell.tabs.iter().any(Tab::is_dirty);
    if UNSAVED.replace(unsaved) != unsaved {
        call("setUnsaved", &[unsaved.into()]);
    }
    let binding = shell.active_tab().and_then(|tab| {
        tab.workspace_revision
            .as_ref()
            .map(|sha| (tab.file.path().to_owned(), sha.clone()))
    });
    BINDING.with_borrow_mut(|previous| {
        if *previous != binding {
            if let Some((path, sha)) = &binding {
                call(
                    "bind",
                    &[
                        format!("browser:{}", path.display()).into(),
                        sha.as_str().into(),
                    ],
                );
            } else {
                call("bind", &[JsValue::NULL, JsValue::NULL]);
            }
            *previous = binding;
        }
    });
}

fn start(creator: ::eframe::AppCreator<'static>) {
    std::panic::set_hook(Box::new(|info| {
        web_sys::console::error_1(&format!("TextCraft: {info}").into())
    }));
    call(
        "onOpen",
        &[Closure::<dyn Fn(JsValue)>::new(incoming).into_js_value()],
    );
    wasm_bindgen_futures::spawn_local(async move {
        let Some(document) = web_sys::window().and_then(|window| window.document()) else {
            call("failed", &["TextCraft requires a browser document.".into()]);
            return;
        };
        let Some(canvas) = document
            .get_element_by_id("notecraft_canvas")
            .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
        else {
            call("failed", &["TextCraft's editor canvas is missing.".into()]);
            return;
        };
        let creator = move |cc: &::eframe::CreationContext<'_>| {
            CTX.with(|ctx| ctx.set(cc.egui_ctx.clone()).ok());
            creator(cc)
        };
        let runner = ::eframe::WebRunner::new();
        let started = runner
            .start(canvas, ::eframe::WebOptions::default(), Box::new(creator))
            .await;
        match started {
            Ok(()) => {
                std::mem::forget(runner);
                if let Some(loading) = document.get_element_by_id("notecraft_loading") {
                    loading.remove();
                }
                call("ready", &[]);
            }
            Err(error) => {
                let message = format!("TextCraft could not start: {error:?}");
                if let Some(loading) = document.get_element_by_id("notecraft_loading") {
                    loading.set_text_content(Some(&message));
                }
                call("failed", &[message.into()]);
            }
        }
    });
}
