//! Browser shell entry. The WebGPU renderer replaces the boot marker below in
//! the renderer PR; for now this proves the trunk → wasm → DOM pipeline boots.

fn main() {
    // Panics surface in the browser console instead of vanishing (spec §6.10).
    console_error_panic_hook::set_once();

    wasm_logger::init(wasm_logger::Config::new(log::Level::Info));
    log::info!("chat-client boot");

    // Boot marker until the renderer lands: proves wasm boots and can drive
    // the DOM overlay that owns nav bar / input / send (spec §5).
    let document = web_sys::window()
        .expect("no global `window`")
        .document()
        .expect("no `document` on window");
    let overlay = document
        .get_element_by_id("overlay")
        .expect("#overlay div missing from index.html");
    overlay.set_inner_html("chat-client stub booted");
}
