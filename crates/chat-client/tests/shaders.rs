//! WGSL shaders must parse and validate with the same naga the runtime uses.
//!
//! The browser WebGPU backend compiles shaders lazily: a broken shader there
//! degrades to silently skipped draws (the render pass still clears and
//! presents), which is invisible in logs and screenshots until you look
//! closely. Parsing natively in CI fails the build instead (spec risk #1 —
//! "text rendering is hard-mode"; the same class of failure hit the bubble
//! shader's fragment-stage field access).

use naga::front::wgsl;

#[test]
fn bubbles_wgsl_parses_and_validates() {
    let source = include_str!("../src/bubbles.wgsl");
    let module = wgsl::parse_str(source).expect("bubbles.wgsl must parse with the runtime's naga");
    let mut validator = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    );
    validator
        .validate(&module)
        .expect("bubbles.wgsl must validate");
}
