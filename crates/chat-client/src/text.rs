//! glyphon text layer (spec art_d3wTgg6t §3/§5): bundled Inter + Noto Emoji,
//! one shaped `Buffer` per text run, rebuilt only on relayout (dirty-buffer).

use glyphon::{
    Attrs, Buffer, Cache, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache, TextAtlas,
    TextRenderer, Viewport, Weight,
};

pub const FONT_INTER_REGULAR: &[u8] = include_bytes!("../assets/Inter-Regular.ttf");
pub const FONT_INTER_SEMIBOLD: &[u8] = include_bytes!("../assets/Inter-SemiBold.ttf");
pub const FONT_NOTO_EMOJI: &[u8] = include_bytes!("../assets/NotoEmoji.ttf");

/// Shaped text measured in CSS px (see `layout::Measured`).
#[derive(Clone, Copy, Debug, Default)]
pub struct TextSize {
    pub width: f32,
    pub height: f32,
}

/// One shaped run. Buffers are created in a fixed order during relayout and
/// positioned at render time — the mapping is positional (see `app.rs`).
pub struct TextLayer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
    /// Shaping cache — retained for the relayout path; glyphon holds the
    /// live reference during prepare.
    #[allow(dead_code)]
    pub cache: Cache,
    pub atlas: TextAtlas,
    pub viewport: Viewport,
    pub text_renderer: TextRenderer,
    pub buffers: Vec<Buffer>,
}

impl TextLayer {
    /// Build the layer and load the bundled fonts. Font loading is infallible
    /// at this level: a missing glyph degrades to tofu, never a panic.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        format: wgpu::TextureFormat,
        multisample: wgpu::MultisampleState,
        viewport_px: (u32, u32),
    ) -> Self {
        let mut font_system = FontSystem::new();
        {
            let db = font_system.db_mut();
            db.load_font_data(FONT_INTER_REGULAR.to_vec());
            db.load_font_data(FONT_INTER_SEMIBOLD.to_vec());
            db.load_font_data(FONT_NOTO_EMOJI.to_vec());
            db.set_sans_serif_family("Inter");
        }

        let cache = Cache::new(device);
        let mut atlas = TextAtlas::new(device, queue, &cache, format);
        let text_renderer = TextRenderer::new(&mut atlas, device, multisample, None);
        let mut viewport = Viewport::new(device, &cache);
        viewport.update(
            queue,
            Resolution {
                width: viewport_px.0,
                height: viewport_px.1,
            },
        );

        Self {
            font_system,
            swash_cache: SwashCache::new(),
            cache,
            atlas,
            viewport,
            text_renderer,
            buffers: Vec::new(),
        }
    }

    /// Shape one run at `wrap_width`, cache its buffer, return its buffer
    /// index and size. Specs must reference the returned index — buffer order
    /// is shape-call order, and callers shape conditionally (e.g. "Delivered"
    /// is measured even when no label is emitted), so a hand-maintained
    /// counter silently desyncs.
    pub fn shape(&mut self, text: &str, metrics: Metrics, wrap_width: f32) -> (usize, TextSize) {
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        buffer.set_size(Some(wrap_width), None);
        buffer.set_text(
            text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut width = 0.0f32;
        let mut height = 0.0f32;
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height += run.line_height;
        }
        self.buffers.push(buffer);
        let ix = self.buffers.len() - 1;
        (ix, TextSize { width, height })
    }

    pub fn clear_buffers(&mut self) {
        self.buffers.clear();
    }
}

/// Weight helper kept here so callers don't touch glyphon types directly.
pub fn regular_metrics() -> Metrics {
    Metrics {
        font_size: crate::layout::FONT_SIZE,
        line_height: crate::layout::LINE_HEIGHT,
    }
}

pub fn small_metrics() -> Metrics {
    Metrics {
        font_size: crate::layout::PILL_FONT_SIZE,
        line_height: crate::layout::PILL_LINE_HEIGHT,
    }
}

/// Keeps `Weight` referenced for the semibold runs the input bar will need.
#[allow(dead_code)]
pub fn semibold_attrs() -> Attrs<'static> {
    Attrs::new()
        .family(Family::SansSerif)
        .weight(Weight::SEMIBOLD)
}
