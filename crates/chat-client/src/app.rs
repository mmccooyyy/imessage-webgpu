//! App shell (spec art_d3wTgg6t §5): requestAnimationFrame loop, DOM event
//! plumbing, scroll state, and GPU-device-loss recovery. The renderer owns
//! wgpu; this module owns time and input.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;

use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlCanvasElement, Window};

use crate::layout;
use crate::renderer::{InitError, Renderer};
use crate::scene::Scene;

/// The requestAnimationFrame callback type, shared by the loop scaffolding.
type FrameClosure = Closure<dyn FnMut(f64)>;

/// MSAA sample count from the `?msaa=` query param (0 → 1, disables MSAA).
/// Default 4×; the flag is an escape hatch for adapters whose MSAA resolve
/// misbehaves (some headless SwiftShader configurations).
fn msaa_from_query() -> u32 {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|search| {
            search
                .trim_start_matches('?')
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == "msaa")
                .and_then(|(_, v)| v.parse::<u32>().ok())
        })
        .map(|n| if n == 0 { 1 } else { n })
        .unwrap_or(4)
}

pub fn boot() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;
    let canvas = document
        .get_element_by_id("app")
        .ok_or_else(|| JsValue::from_str("missing #app canvas"))?
        .dyn_into::<HtmlCanvasElement>()?;

    wasm_bindgen_futures::spawn_local(async move {
        match Renderer::new(canvas.clone(), msaa_from_query()).await {
            Ok(renderer) => {
                clear_fallback_flag();
                let backend = renderer.backend_name.clone();
                set_boot_status(&document, &backend);
                App::start(renderer, canvas, document, window);
            }
            Err(InitError::NoAdapter) => {
                // navigator.gpu existed but no adapter — the spec's try/catch
                // path: load the WebGL2 bundle instead.
                log::error!("no adapter on primary backend — loading WebGL2 bundle");
                set_boot_status(&document, "fallback: WebGL2 bundle");
                load_fallback_bundle();
            }
            Err(e) => {
                log::error!("renderer init failed: {e}");
                set_boot_status(&document, "renderer init failed");
            }
        }
    });
    Ok(())
}
/// Swap to the WebGL2 feature build by navigating to its self-booting page —
/// trunk emits hashed entry names, so the wasm side can't import the entry
/// directly. The sessionStorage guard makes the fallback one-shot so a GL
/// failure can't loop; the success path clears the flag.
#[wasm_bindgen(
    inline_js = r#"export function load_fallback_bundle() { try { if (location.pathname.indexOf('/webgl2/') === 0 && sessionStorage.getItem('chat-client-fallback') === '1') { console.error('fallback bundle already tried once - stopping'); return; } sessionStorage.setItem('chat-client-fallback', '1'); } catch (e) {} location.replace('/webgl2/'); } export function clear_fallback_flag() { try { sessionStorage.removeItem('chat-client-fallback'); } catch (e) {} }"#
)]
extern "C" {
    #[wasm_bindgen(js_name = load_fallback_bundle)]
    fn load_fallback_bundle();
    #[wasm_bindgen(js_name = clear_fallback_flag)]
    fn clear_fallback_flag();
}

fn set_boot_status(document: &Document, backend: &str) {
    if let Some(el) = document.get_element_by_id("overlay") {
        el.set_text_content(Some(&format!("renderer: {backend}")));
    }
}

struct App {
    renderer: Renderer,
    canvas: HtmlCanvasElement,
    scene: Scene,
    last_scene: Scene,
    scroll: f32,
    stick_bottom: bool,
    dirty: bool,
    reduced_motion: bool,
    destroyed: bool,
    rebuilding: bool,
    lost_rx: mpsc::Receiver<wgpu::DeviceLostReason>,
    /// Keeps event-listener closures alive for the page's lifetime.
    _listeners: Vec<Closure<dyn FnMut(JsValue)>>,
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

impl App {
    fn start(renderer: Renderer, canvas: HtmlCanvasElement, document: Document, window: Window) {
        let (lost_tx, lost_rx) = mpsc::channel::<wgpu::DeviceLostReason>();
        renderer
            .device()
            .set_device_lost_callback(Box::new(move |reason, detail| {
                log::error!("device lost: {reason:?} — {detail}");
                let _ = lost_tx.send(reason);
            }));

        let reduced_motion = window
            .match_media("(prefers-reduced-motion: reduce)")
            .ok()
            .flatten()
            .map(|q| q.matches())
            .unwrap_or(false);

        let app = Rc::new(RefCell::new(App {
            renderer,
            canvas,
            scene: Scene::demo(),
            last_scene: Scene::default(),
            scroll: 0.0,
            stick_bottom: true,
            dirty: true,
            reduced_motion,
            destroyed: false,
            rebuilding: false,
            lost_rx,
            _listeners: Vec::new(),
        }));

        App::wire_events(&app, &document, &window);
        let app_for_frame = Rc::clone(&app);
        APP.with(|slot| *slot.borrow_mut() = Some(app));

        // Recursive rAF closure: re-queues itself every frame (spec §5 —
        // no winit, the browser event loop is ours).
        let frame_fn: Rc<RefCell<Option<FrameClosure>>> = Rc::new(RefCell::new(None));
        let frame_fn2 = Rc::clone(&frame_fn);
        *frame_fn.borrow_mut() = Some(Closure::new(move |ts: f64| {
            app_for_frame.borrow_mut().frame(ts);
            if let Some(cb) = frame_fn2.borrow().as_ref() {
                if let Some(window) = web_sys::window() {
                    let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
                }
            }
        }));
        if let Some(cb) = frame_fn.borrow().as_ref() {
            let _ = window.request_animation_frame(cb.as_ref().unchecked_ref());
        }
        // The closure re-refs itself each frame; it must outlive this call.
        std::mem::forget(frame_fn);
    }

    fn wire_events(app: &Rc<RefCell<App>>, document: &Document, window: &Window) {
        // resize / visibilitychange → mark dirty; frame() detects the size
        // drift and re-configures (covers both, spec §6.5).
        let on_dirty = |app: &Rc<RefCell<App>>| {
            let app2 = Rc::clone(app);
            Closure::new(move |_e: JsValue| {
                app2.borrow_mut().dirty = true;
            })
        };
        let resize_cb = on_dirty(app);
        let _ =
            window.add_event_listener_with_callback("resize", resize_cb.as_ref().unchecked_ref());
        let vis_cb = on_dirty(app);
        let _ = document
            .add_event_listener_with_callback("visibilitychange", vis_cb.as_ref().unchecked_ref());

        // Wheel scroll over the canvas; release stick-to-bottom on scroll-up.
        let wheel_cb = {
            let app2 = Rc::clone(app);
            Closure::new(move |e: JsValue| {
                if let Ok(wheel) = e.dyn_into::<web_sys::WheelEvent>() {
                    wheel.prevent_default();
                    let mut app = app2.borrow_mut();
                    app.scroll += wheel.delta_y() as f32;
                    let max = layout::stick_to_bottom(
                        app.renderer.content_height(),
                        app.renderer.viewport_css().1,
                    );
                    app.scroll = layout::clamp_scroll(
                        app.scroll,
                        app.renderer.content_height(),
                        app.renderer.viewport_css().1,
                    );
                    app.stick_bottom = app.scroll >= max - 1.0;
                    app.dirty = true;
                }
            })
        };
        let _ = app
            .borrow()
            .canvas
            .add_event_listener_with_callback("wheel", wheel_cb.as_ref().unchecked_ref());

        // Dev drills (spec §7 risk #9): 'd' destroys the device
        // (reason=Destroyed → skip re-init), 'r' forces the same re-init
        // path a real loss takes.
        let key_cb = {
            let app2 = Rc::clone(app);
            Closure::new(move |e: JsValue| {
                if let Ok(key) = e.dyn_into::<web_sys::KeyboardEvent>() {
                    match key.key().as_str() {
                        "d" => {
                            log::info!("drill: device.destroy()");
                            app2.borrow().renderer.device().destroy();
                        }
                        "r" => {
                            log::info!("drill: forced renderer re-init");
                            App::start_reinit(&app2);
                        }
                        _ => {}
                    }
                }
            })
        };
        let _ = window.add_event_listener_with_callback("keydown", key_cb.as_ref().unchecked_ref());

        let mut app_mut = app.borrow_mut();
        app_mut._listeners = vec![resize_cb, vis_cb, wheel_cb, key_cb];
    }

    /// The one re-init entry: rebuild the whole renderer through the same
    /// idempotent init path, then relayout (spec §6.6). Frames pause while
    /// `rebuilding` is set.
    fn start_reinit(app: &Rc<RefCell<App>>) {
        let canvas = app.borrow().canvas.clone();
        app.borrow_mut().rebuilding = true;
        wasm_bindgen_futures::spawn_local(async move {
            let rc = APP.with(|slot| slot.borrow().as_ref().map(Rc::clone));
            if let Some(rc) = rc {
                match Renderer::new(canvas, msaa_from_query()).await {
                    Ok(mut renderer) => {
                        let mut app = rc.borrow_mut();
                        renderer.relayout(&app.scene);
                        app.renderer = renderer;
                        app.rebuilding = false;
                        app.dirty = true;
                        log::info!("renderer re-initialized");
                    }
                    Err(e) => log::error!("re-init failed: {e}"),
                }
            }
        });
    }

    fn frame(&mut self, ts: f64) {
        if self.destroyed || self.rebuilding {
            return;
        }

        // 1. Drain device-lost notifications.
        if let Ok(reason) = self.lost_rx.try_recv() {
            if reason == wgpu::DeviceLostReason::Destroyed {
                log::info!("device destroyed deliberately — not re-initializing");
                self.destroyed = true;
                return;
            }
            log::info!("device lost — re-initializing through the idempotent path");
            self.rebuilding = true;
            let canvas = self.canvas.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let rc = APP.with(|slot| slot.borrow().as_ref().map(Rc::clone));
                if let Some(rc) = rc {
                    match Renderer::new(canvas, msaa_from_query()).await {
                        Ok(mut renderer) => {
                            let mut app = rc.borrow_mut();
                            renderer.relayout(&app.scene);
                            app.renderer = renderer;
                            app.rebuilding = false;
                            app.dirty = true;
                            log::info!("renderer re-initialized after device loss");
                        }
                        Err(e) => log::error!("re-init failed: {e}"),
                    }
                }
            });
            return;
        }

        // 2. Catch size drift (resize/visibility/hidpi) — re-configures the
        //    surface and forces a relayout.
        if self.renderer.resize(&self.canvas) {
            self.dirty = true;
        }

        // 3. Scene change → relayout (shaping happens only here).
        if !self.scene.same_as(&self.last_scene) {
            self.last_scene = self.scene.clone();
            self.renderer.relayout(&self.scene);
            self.dirty = true;
        }

        // 4. Typing dots animate (unless reduced motion) → keep redrawing.
        let animating = self.scene.typing_peer.is_some() && !self.reduced_motion;
        if animating {
            self.dirty = true;
        }

        // 5. Stick-to-bottom on new content unless the user scrolled up.
        let (vw, vh) = self.renderer.viewport_css();
        let content_h = self.renderer.content_height();
        if self.stick_bottom {
            self.scroll = layout::stick_to_bottom(content_h, vh);
        }
        self.scroll = layout::clamp_scroll(self.scroll, content_h, vh);

        if !self.dirty {
            return;
        }

        let dot_phase = if animating {
            (ts as f32) / 1000.0 * 6.0
        } else {
            0.0
        };
        if let crate::renderer::RenderOutcome::Drawn = self.renderer.render(self.scroll, dot_phase)
        {
            self.dirty = animating; // non-animating scenes go back to sleep
        }
        let _ = vw;
    }
}
