# TextCraft in WebOS

TextCraft uses the same Rust document, Markdown and command crates in the
browser. Its WebOS layout puts New, Open and Save above the document tabs.
The browser Rust build comes from the `nc-app` binary and its wasm-specific
platform adapter.

Build with Rust 1.99.1 and the `wasm32-unknown-unknown` target.

```sh
cargo build --release --locked --target wasm32-unknown-unknown --package nc-app --bin notepad
wasm-bindgen --target web --no-typescript --out-dir browser --out-name textcraft target/wasm32-unknown-unknown/release/notepad.wasm
```

Use the wasm-bindgen CLI version in `Cargo.lock`. The WebOS package supplies
an external bootstrap and a Files host adapter for the generated module.
The Rust side requests reads, file selection, and revision-checked writes
through that interface. The editor marks a document clean after the
workspace confirms the write. Save As can keep a separate copy after a
conflict. The WebOS window controls the desktop window lifecycle.
