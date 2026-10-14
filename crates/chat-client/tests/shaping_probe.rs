//! Native probe: reproduce the browser-side shaping path with cosmic-text
//! directly (no wgpu) to see whether body text measures zero-width.

use cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping};

#[test]
fn body_text_measures_nonzero() {
    let mut font_system = cosmic_text::FontSystem::new();
    font_system.db_mut().load_font_data(
        std::fs::read(
            "/home/user/work/imessage-webgpu/crates/chat-client/assets/Inter-Regular.ttf",
        )
        .expect("Inter-Regular.ttf readable"),
    );
    font_system.db_mut().set_sans_serif_family("Inter");

    let metrics = Metrics {
        font_size: 17.0,
        line_height: 22.0,
    };
    for text in [
        "judges walk the tables at 3",
        "yo is the two-browser demo ready for Friday? \u{1f389}",
        "shipping it tonight — open two tabs, chat live",
    ] {
        let mut buffer = Buffer::new(&mut font_system, metrics);
        buffer.set_size(Some(900.0), None);
        buffer.set_text(
            text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut font_system, false);
        let mut width = 0.0f32;
        let mut runs = 0;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            runs += 1;
        }
        println!("text {text:?}: runs {runs}, line_w {width}");
        assert!(width > 50.0, "expected nonzero width for {text:?}");
    }
}
