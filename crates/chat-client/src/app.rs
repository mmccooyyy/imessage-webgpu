//! App shell (spec art_d3wTgg6t §5): requestAnimationFrame loop, DOM event
//! plumbing, socket wiring, and GPU-device-loss recovery.
//!
//! Division of labor:
//! - `chat-core::Store` owns ALL conversation/connection state; the shell
//!   only runs the [`chat_core::Effect`]s the store emits and feeds events
//!   back in (socket lifecycle, decoded frames, viewport, clock).
//! - `net` owns the websocket transport (reader/writer tasks per connection).
//! - `overlay` owns the DOM elements; the shell keeps them in sync per frame.
//! - The renderer owns wgpu; this module owns time and input.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::mpsc;

use chat_core::{ConnState, Effect, Event, Store};
use chat_protocol::{ClientFrame, ServerFrame};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlCanvasElement, HtmlInputElement, Window};

use crate::layout;
use crate::net::{self, NetEvent};
use crate::overlay::Overlay;
use crate::renderer::{InitError, Renderer};
use crate::scene::Scene;
use crate::ux;

/// The requestAnimationFrame callback type, shared by the loop scaffolding.
type FrameClosure = Closure<dyn FnMut(f64)>;

/// True when `?text=0` — skip the glyphon pass entirely. Diagnostic switch
/// for backend isolation: if bubbles present without text, the failure is in
/// the text pipeline; if the canvas is still black, presentation itself is
/// failing (headless SwiftShader's known signature).
fn text_disabled_from_query() -> bool {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|search| {
            search
                .trim_start_matches('?')
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == "text")
                .map(|(_, v)| v == "0")
        })
        .unwrap_or(false)
}

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

/// Shell clock in millis (the store-clock origin). `performance.now()` shares
/// the rAF timestamp's time origin, so both sources agree.
fn now_ms() -> u64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now() as u64)
        .unwrap_or(0)
}

/// `?name=` — auto-join affordance for scripted demos (the join card is the
/// human path); absent or empty means "show the join card".
fn name_from_query() -> Option<String> {
    web_sys::window()
        .and_then(|w| w.location().search().ok())
        .and_then(|search| {
            search
                .trim_start_matches('?')
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .find(|(k, _)| *k == "name")
                .map(|(_, v)| v.to_string())
        })
        .filter(|v| !v.is_empty())
}

/// The conversation-start pill label (spec §5 mock: "Today 4:12 PM"), in
/// the browser's local time via `js_sys::Date` (wasm has no TZ database).
fn session_time_label() -> String {
    let d = js_sys::Date::new_0();
    let (h24, min) = (d.get_hours(), d.get_minutes());
    let ampm = if h24 >= 12 { "PM" } else { "AM" };
    let h = h24 % 12;
    let h = if h == 0 { 12 } else { h };
    format!("Today {h}:{min:02} {ampm}")
}

/// Derive the `/ws` URL from the page's location (spec §5.4: same origin,
/// scheme-derived — never hard-coded).
fn page_ws_url() -> String {
    let Some(window) = web_sys::window() else {
        return ux::ws_url("http:", "127.0.0.1:8080");
    };
    let loc = window.location();
    let protocol = loc.protocol().unwrap_or_default();
    let host = loc.host().unwrap_or_default();
    ux::ws_url(&protocol, &host)
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
        match Renderer::new(
            canvas.clone(),
            msaa_from_query(),
            text_disabled_from_query(),
        )
        .await
        {
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
    if let Some(el) = document.get_element_by_id("boot-status") {
        el.set_text_content(Some(&format!("renderer: {backend}")));
    }
}

struct App {
    renderer: Renderer,
    canvas: HtmlCanvasElement,
    scene: Scene,
    last_scene: Scene,
    scroll: f32,
    dirty: bool,
    reduced_motion: bool,
    destroyed: bool,
    rebuilding: bool,
    lost_rx: mpsc::Receiver<wgpu::DeviceLostReason>,
    /// Keeps event-listener closures alive for the page's lifetime.
    _listeners: Vec<Closure<dyn FnMut(JsValue)>>,

    // ── chat-core integration: all conversation state lives in the store ──
    store: Store,
    /// The display name sent with `hello`; set by the join card (or `?name=`).
    name: Option<String>,
    /// Frames queued while no socket is live; flushed into the next
    /// connection's writer channel behind its `hello`. `Typing` frames are
    /// never queued — stale typing state would be wrong.
    outbox: VecDeque<ClientFrame>,
    /// Live connection's writer handle; `None` when disconnected.
    socket: Option<futures::channel::mpsc::UnboundedSender<ClientFrame>>,
    net_tx: mpsc::Sender<NetEvent>,
    net_rx: mpsc::Receiver<NetEvent>,
    /// Presentation-only transcript pills ("Sam joined" / "Sam left").
    presence: ux::PresenceLog,
    /// Conversation-start pill label ("Today 4:12 PM"), captured at the
    /// first connect and kept across reconnects (spec §5 mock).
    date_label: Option<String>,
    overlay: Overlay,

    // ── outgoing typing throttle (shell-owned transmission policy) ──
    typing_active: bool,
    last_typing_on: Option<u64>,
    last_keystroke: u64,
    /// xorshift state for the backoff jitter draw (the store draws no
    /// randomness itself).
    rng: u32,
}

thread_local! {
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

/// Per-frame body, dispatched through the APP slot so the rAF closure stays
/// tiny. The slot is populated before the first frame fires.
fn app_frame(ts: f64) {
    APP.with(|slot| {
        if let Some(app) = slot.borrow().as_ref() {
            app.borrow_mut().frame(ts);
        }
    });
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

        let (net_tx, net_rx) = mpsc::channel::<NetEvent>();
        let overlay = Overlay::mount(&document).expect("overlay elements");

        let app = App {
            renderer,
            canvas,
            scene: Scene::default(),
            last_scene: Scene::default(),
            scroll: 0.0,
            dirty: true,
            reduced_motion,
            destroyed: false,
            rebuilding: false,
            lost_rx,
            _listeners: Vec::new(),
            store: Store::new(),
            name: name_from_query(),
            outbox: VecDeque::new(),
            socket: None,
            net_tx,
            net_rx,
            presence: ux::PresenceLog::default(),
            date_label: None,
            typing_active: false,
            last_typing_on: None,
            last_keystroke: 0,
            rng: 0x9e37_79b9,
            overlay,
        };

        // The join card is the human path; `?name=` auto-joins for demos.
        if app.name.is_none() {
            app.overlay.show_join(true);
            app.overlay.focus_name();
        } else {
            app.overlay.show_join(false);
        }

        let app = Rc::new(RefCell::new(app));

        // `?name=` auto-join: kick the first connection exactly as the join
        // card would — the store's tick only retries from Reconnecting, so
        // without this a scripted demo hangs in Connecting forever.
        if app.borrow().name.is_some() {
            app.borrow_mut().connect();
        }
        App::wire_events(&app, &document, &window);
        APP.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&app)));

        // Recursive rAF closure: re-queues itself every frame (spec §5 —
        // no winit, the browser event loop is ours).
        let frame_fn: Rc<RefCell<Option<FrameClosure>>> = Rc::new(RefCell::new(None));
        let frame_fn2 = Rc::clone(&frame_fn);
        *frame_fn.borrow_mut() = Some(Closure::new(move |ts: f64| {
            app_frame(ts);
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

        // Wheel scroll over the canvas; the store owns stick-to-bottom, so
        // the viewport event flows through it.
        let wheel_cb = {
            let app2 = Rc::clone(app);
            Closure::new(move |e: JsValue| {
                if let Ok(wheel) = e.dyn_into::<web_sys::WheelEvent>() {
                    wheel.prevent_default();
                    let mut app = app2.borrow_mut();
                    app.scroll += wheel.delta_y() as f32;
                    app.scroll = layout::clamp_scroll(
                        app.scroll,
                        app.renderer.content_height(),
                        app.renderer.viewport_css().1,
                    );
                    let max = layout::stick_to_bottom(
                        app.renderer.content_height(),
                        app.renderer.viewport_css().1,
                    );
                    let at_bottom = app.scroll >= max - 1.0;
                    app.dirty = true;
                    // ViewportScrolled only ever answers with Render, which
                    // the dirty flag above already covers.
                    let _ = app
                        .store
                        .apply(Event::ViewportScrolled { at_bottom }, now_ms());
                }
            })
        };
        let _ = app
            .borrow()
            .canvas
            .add_event_listener_with_callback("wheel", wheel_cb.as_ref().unchecked_ref());

        // Dev drills (spec §7 risk #9): 'd' destroys the device
        // (reason=Destroyed → skip re-init), 'r' forces the same re-init
        // path a real loss takes. Ignored while typing — the message input
        // is a legitimate place to press these keys.
        let key_cb = {
            let app2 = Rc::clone(app);
            Closure::new(move |e: JsValue| {
                if let Ok(key) = e.dyn_into::<web_sys::KeyboardEvent>() {
                    let in_input = key
                        .target()
                        .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
                        .is_some();
                    if in_input {
                        return;
                    }
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

        // ── Overlay events (join, send, typing) ──
        let overlay_listeners = {
            let mut listeners = Vec::new();

            // Join: form submit (Enter or the button) → hello with the name.
            let join_cb = {
                let app2 = Rc::clone(app);
                Closure::new(move |e: JsValue| {
                    if let Ok(event) = e.dyn_into::<web_sys::Event>() {
                        event.prevent_default();
                        let name = app2.borrow().overlay.take_name();
                        if let Some(name) = name {
                            app2.borrow_mut().handle_join(name);
                        }
                    }
                })
            };
            let form_target: web_sys::EventTarget = app
                .borrow()
                .overlay
                .join_form()
                .clone()
                .dyn_into()
                .expect("join form target");
            let _ = form_target
                .add_event_listener_with_callback("submit", join_cb.as_ref().unchecked_ref());
            listeners.push(join_cb);

            // Send: button click and Enter in the message input.
            let send_cb = {
                let app2 = Rc::clone(app);
                Closure::new(move |_e: JsValue| {
                    let text = app2.borrow().overlay.take_msg_text();
                    if let Some(text) = text {
                        app2.borrow_mut().handle_send(text);
                    }
                })
            };
            let send_target: web_sys::EventTarget = app
                .borrow()
                .overlay
                .send_btn()
                .clone()
                .dyn_into()
                .expect("send button target");
            let _ = send_target
                .add_event_listener_with_callback("click", send_cb.as_ref().unchecked_ref());
            listeners.push(send_cb);

            let enter_cb = {
                let app2 = Rc::clone(app);
                Closure::new(move |e: JsValue| {
                    if let Ok(key) = e.dyn_into::<web_sys::KeyboardEvent>() {
                        if key.key() == "Enter" {
                            let text = app2.borrow().overlay.take_msg_text();
                            if let Some(text) = text {
                                app2.borrow_mut().handle_send(text);
                            }
                        }
                    }
                })
            };
            let input_target: web_sys::EventTarget = app
                .borrow()
                .overlay
                .msg_input()
                .clone()
                .dyn_into()
                .expect("msg input target");
            let _ = input_target
                .add_event_listener_with_callback("keydown", enter_cb.as_ref().unchecked_ref());
            listeners.push(enter_cb);

            // Typing: every keystroke feeds the throttle (which decides when
            // a Typing frame is actually due).
            let typing_cb = {
                let app2 = Rc::clone(app);
                Closure::new(move |_e: JsValue| {
                    app2.borrow_mut().handle_typing_keystroke();
                })
            };
            let _ = input_target
                .add_event_listener_with_callback("input", typing_cb.as_ref().unchecked_ref());
            listeners.push(typing_cb);

            listeners
        };

        let mut app_mut = app.borrow_mut();
        app_mut._listeners = vec![resize_cb, vis_cb, wheel_cb, key_cb];
        app_mut._listeners.extend(overlay_listeners);
    }

    /// The one re-init entry: rebuild the whole renderer through the same
    /// idempotent init path, then relayout (spec §6.6). Frames pause while
    /// `rebuilding` is set. Conversation state is untouched — the store
    /// survives renderer re-initialization.
    fn start_reinit(app: &Rc<RefCell<App>>) {
        let canvas = app.borrow().canvas.clone();
        app.borrow_mut().rebuilding = true;
        wasm_bindgen_futures::spawn_local(async move {
            let rc = APP.with(|slot| slot.borrow().as_ref().map(Rc::clone));
            if let Some(rc) = rc {
                match Renderer::new(canvas, msaa_from_query(), text_disabled_from_query()).await {
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

    // ── user intents ──

    /// Join card submitted: send `hello` under this name.
    fn handle_join(&mut self, name: String) {
        self.overlay.show_join(false);
        self.overlay.set_join_error(None);
        self.name = Some(name);
        self.connect();
        self.dirty = true;
    }

    /// User intent: send the typed message (optimistic pending bubble).
    fn handle_send(&mut self, text: String) {
        let now = now_ms();
        let cmid = ux::new_cmid();
        // A successful send retires the local typing indicator.
        if self.typing_active {
            self.typing_active = false;
            self.send_frame(ClientFrame::Typing { on: false });
        }
        let effects = self.store.send(text.clone(), cmid, now);
        if effects.is_empty() {
            // The store refused the send (initial Connecting window between
            // reconnect attempts). Restore the draft — a mid-attempt refusal
            // must never eat the user's text silently (spec §5: the input
            // stays usable while reconnecting).
            self.overlay.set_msg_input(&text);
            return;
        }
        self.run_effects(effects, now);
    }

    /// Keystroke in the message input: feed the ~2.5 s typing throttle.
    fn handle_typing_keystroke(&mut self) {
        let now = now_ms();
        self.last_keystroke = now;
        self.typing_active = true;
        if ux::typing_on_due(self.last_typing_on, now) {
            self.last_typing_on = Some(now);
            self.send_frame(ClientFrame::Typing { on: true });
        }
    }

    // ── socket plumbing ──

    /// Open a websocket and send hello (plus any queued frames) first.
    fn connect(&mut self) {
        let Some(name) = self.name.clone() else {
            return; // no name yet — the join card owns first contact
        };
        // The time pill stamps when this chat session began; reconnects
        // keep the original stamp (get_or_insert).
        self.date_label.get_or_insert_with(session_time_label);
        // Back to Connecting (also clears ConnectingFailed so the store is
        // consistent even before the first frame event arrives).
        let _ = self.store.apply(Event::SocketOpened, now_ms());
        let mut initial = VecDeque::new();
        initial.push_back(ClientFrame::Hello { name });
        initial.extend(self.outbox.drain(..));
        match net::connect(&page_ws_url(), initial, self.net_tx.clone()) {
            Some(tx) => self.socket = Some(tx),
            None => {} // Closed already queued; the store handles it next frame
        }
    }

    /// Transmit one client frame. `Typing` frames are dropped while
    /// disconnected (stale typing state is worse than silence); everything
    /// else queues in the outbox for the next connection.
    fn send_frame(&mut self, frame: ClientFrame) {
        let queueable = !matches!(frame, ClientFrame::Typing { .. });
        match &self.socket {
            Some(tx) => {
                if tx.unbounded_send(frame.clone()).is_err() {
                    // Writer already gone; queue until the reconnect lands.
                    self.socket = None;
                    if queueable {
                        self.outbox.push_back(frame);
                    }
                }
            }
            None => {
                if queueable {
                    self.outbox.push_back(frame);
                }
            }
        }
    }

    /// Run the effects a store transition asked for.
    fn run_effects(&mut self, effects: Vec<Effect>, _now: u64) {
        for effect in effects {
            match effect {
                Effect::Connect => self.connect(),
                Effect::SendMsg(frame) => self.send_frame(frame),
                Effect::Resume { last_seq } => self.send_frame(ClientFrame::Resume { last_seq }),
                // Informational — tick is the authority that fires retries.
                Effect::RetryAfter(_) => {}
                Effect::Render => self.dirty = true,
                Effect::Scroll => {} // stick-to-bottom lives in the store flag
            }
        }
    }

    /// Drain socket events (frames, closes) into the store.
    fn drain_net(&mut self, now: u64) {
        while let Ok(event) = self.net_rx.try_recv() {
            match event {
                NetEvent::Frame(frame) => {
                    // Presence pills: announce transitions the roster did not
                    // already contain. Names resolve BEFORE the store applies
                    // the frame — a leave removes the peer being named.
                    match &frame {
                        ServerFrame::Presence {
                            join: Some(peer), ..
                        } => {
                            let known = self.store.peers().any(|p| p.id == peer.id);
                            let mine = Some(peer.id.as_str()) == self.store.me();
                            if !known && !mine {
                                self.presence.push(format!("{} joined", peer.name));
                            }
                        }
                        ServerFrame::Presence {
                            leave: Some(id), ..
                        } => {
                            let gone = self
                                .store
                                .peers()
                                .find(|p| p.id == *id)
                                .map(|p| p.name.clone());
                            if let Some(name) = gone {
                                self.presence.push(format!("{name} left"));
                            }
                        }
                        _ => {}
                    }
                    let effects = self.store.apply(Event::Frame(frame), now);
                    self.run_effects(effects, now);
                }
                NetEvent::Closed => {
                    self.socket = None;
                    let jitter = self.next_jitter();
                    let effects = self.store.apply(Event::SocketClosed { jitter }, now);
                    self.run_effects(effects, now);
                }
            }
        }
    }

    /// xorshift32 → jitter draw in [0, 1).
    fn next_jitter(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        (x & 0xffff) as f32 / 65536.0
    }

    /// Per-frame DOM sync: input enabled per connection state, presence
    /// line, join-card visibility including the failed-connection card.
    fn sync_overlay(&mut self) {
        let conn = self.store.conn().clone();
        let input_enabled = self.store.is_live() || matches!(conn, ConnState::Reconnecting { .. });

        match &conn {
            ConnState::ConnectingFailed => {
                // Never connected: back to the join card with the failure.
                if self.name.is_some() {
                    self.overlay.set_join_error(Some(
                        "Couldn't connect — check the server and join again.",
                    ));
                }
                self.overlay.show_join(true);
            }
            _ => {
                self.overlay.set_join_error(None);
                self.overlay.show_join(self.name.is_none());
            }
        }

        let others: Vec<String> = self
            .store
            .peers()
            .filter(|p| Some(p.id.as_str()) != self.store.me())
            .map(|p| p.name.clone())
            .collect();
        let presence_text = if others.is_empty() {
            "just you".to_string()
        } else {
            others.join(", ")
        };
        // Spec §5 mock: the send affordance grays while Reconnecting —
        // visually dimmed but still clickable (sends queue locally).
        let send_dimmed = matches!(conn, ConnState::Reconnecting { .. });
        self.overlay
            .sync(input_enabled, send_dimmed, &presence_text);
    }

    fn frame(&mut self, ts: f64) {
        if self.destroyed || self.rebuilding {
            return;
        }
        let now = ts as u64;

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
                    match Renderer::new(canvas, msaa_from_query(), text_disabled_from_query()).await
                    {
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

        // 2. Socket events → store; typing expiry + reconnect timers.
        self.drain_net(now);
        let effects = self.store.tick(now);
        self.run_effects(effects, now);
        // Retire the local typing indicator after the idle gap.
        if ux::typing_off_due(self.typing_active, self.last_keystroke, now) {
            self.typing_active = false;
            self.send_frame(ClientFrame::Typing { on: false });
        }

        // 3. Catch size drift (resize/visibility/hidpi) — re-configures the
        //    surface and forces a relayout.
        if self.renderer.resize(&self.canvas) {
            self.dirty = true;
        }

        // 4. Store → scene; scene change → relayout (shaping happens only here).
        self.scene = ux::build_scene(&self.store, &self.presence, self.date_label.as_deref(), now);
        if !self.scene.same_as(&self.last_scene) {
            self.last_scene = self.scene.clone();
            self.renderer.relayout(&self.scene);
            self.dirty = true;
        }

        // 5. Typing dots animate (unless reduced motion) → keep redrawing.
        let animating = self.scene.typing_peer.is_some() && !self.reduced_motion;
        if animating {
            self.dirty = true;
        }

        // 6. Stick-to-bottom on new content unless the user scrolled up
        //    (the store owns the flag).
        let vh = self.renderer.viewport_css().1;
        let content_h = self.renderer.content_height();
        if self.store.stick_to_bottom() {
            self.scroll = layout::stick_to_bottom(content_h, vh);
        }
        self.scroll = layout::clamp_scroll(self.scroll, content_h, vh);

        // 7. Overlay mirrors the store's connection state.
        self.sync_overlay();

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
    }
}
