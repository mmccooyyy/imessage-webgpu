//! Browser shell entry: wasm-logger + panic hook, then the renderer boots
//! against the `#app` canvas (spec art_d3wTgg6t §5). Text entry, nav, and
//! send stay DOM — the canvas owns only the conversation surface.
//!
//! The pure modules (layout, scene) compile on the host too so their tests
//! run with plain `cargo test`; the wgpu/web-sys shell is wasm-only.

mod layout;
mod scene;
// Shell helpers are consumed by the wasm-only modules; on the host they exist
// so their unit tests run with plain `cargo test`.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
mod ux;

#[cfg(target_arch = "wasm32")]
mod app;
#[cfg(target_arch = "wasm32")]
mod bubbles;
#[cfg(target_arch = "wasm32")]
mod net;
#[cfg(target_arch = "wasm32")]
mod overlay;
#[cfg(target_arch = "wasm32")]
mod renderer;
#[cfg(target_arch = "wasm32")]
mod text;

#[cfg(target_arch = "wasm32")]
fn main() {
    // Panics surface in the browser console instead of vanishing (spec §6.10).
    console_error_panic_hook::set_once();

    wasm_logger::init(wasm_logger::Config::new(log::Level::Info));
    log::info!("chat-client boot");

    app::boot().expect("app boot failed");
}

/// The host build exists only to run the pure-module tests.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}
