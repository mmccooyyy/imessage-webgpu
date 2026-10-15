//! DOM overlay handles (spec art_d3wTgg6t §5): the nav bar, join card, and
//! input bar live in index.html; this module grabs those elements and keeps
//! them in sync with store-derived state. All conversation/connection state
//! stays in chat-core — the DOM only mirrors what the store says. Event
//! wiring lives in `app.rs`, where the App it feeds is defined.

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlButtonElement, HtmlElement, HtmlInputElement};

fn element(document: &Document, id: &str) -> Result<web_sys::Element, JsValue> {
    document
        .get_element_by_id(id)
        .ok_or_else(|| JsValue::from_str(&format!("missing #{id}")))
}

/// The overlay elements, grabbed once at boot. Missing elements are a build
/// error in index.html — fail loudly at boot instead of silently dead UI.
pub struct Overlay {
    join_card: web_sys::Element,
    join_form: web_sys::Element,
    name_input: HtmlInputElement,
    join_error: web_sys::Element,
    msg_input: HtmlInputElement,
    send_btn: HtmlButtonElement,
    presence: web_sys::Element,
}

impl Overlay {
    pub fn mount(document: &Document) -> Result<Self, JsValue> {
        Ok(Self {
            join_card: element(document, "join-card")?,
            join_form: element(document, "join-form")?,
            name_input: element(document, "name-input")?.dyn_into()?,
            join_error: element(document, "join-error")?,
            msg_input: element(document, "msg-input")?.dyn_into()?,
            send_btn: element(document, "send-btn")?.dyn_into()?,
            presence: element(document, "presence")?,
        })
    }

    pub fn join_form(&self) -> &web_sys::Element {
        &self.join_form
    }

    pub fn msg_input(&self) -> &HtmlInputElement {
        &self.msg_input
    }

    pub fn send_btn(&self) -> &HtmlButtonElement {
        &self.send_btn
    }

    pub fn show_join(&self, show: bool) {
        self.join_card
            .dyn_ref::<HtmlElement>()
            .map(|h| h.set_hidden(!show));
    }

    /// The typed name from the join card.
    pub fn take_name(&self) -> Option<String> {
        let name = self.name_input.value().trim().to_string();
        if name.is_empty() {
            return None;
        }
        Some(name)
    }

    pub fn set_join_error(&self, message: Option<&str>) {
        self.join_error.set_text_content(message);
    }

    pub fn focus_name(&self) {
        let _ = self.name_input.focus();
    }

    /// The typed message, if the input has content; clears the input.
    pub fn take_msg_text(&self) -> Option<String> {
        let text = self.msg_input.value().trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.msg_input.set_value("");
        Some(text)
    }

    /// Per-frame sync from store-derived UI state. The DOM is the mirror,
    /// not the source of truth.
    pub fn sync(&self, input_enabled: bool, presence_text: &str) {
        self.msg_input.set_disabled(!input_enabled);
        self.send_btn.set_disabled(!input_enabled);
        self.presence.set_text_content(Some(presence_text));
    }
}
