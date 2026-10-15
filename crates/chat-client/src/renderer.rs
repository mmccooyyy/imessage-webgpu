//! wgpu + glyphon renderer lifecycle (spec art_d3wTgg6t §5, §6).
//!
//! `Renderer::new` is the ONE idempotent construction path — device loss calls
//! it again (spec §6.6). The app layer never touches wgpu types.

use crate::bubbles::BubblePass;
use crate::layout::{self, Layout, Measured, Measurements, PillGeom, TextRole, TextRun};
use crate::scene::Scene;
use crate::text::TextLayer;
use web_sys::HtmlCanvasElement;

/// Why a render frame didn't draw (all retryable per spec §6.5 — never panic).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderOutcome {
    Drawn,
    /// Nothing drawn this frame (occluded/timeout — just skip).
    Skipped,
    /// Surface was lost/stale — re-configured; next frame draws.
    Reconfigured,
}

pub enum InitError {
    NoAdapter,
    NoDevice(String),
    Surface(String),
}

impl core::fmt::Display for InitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            InitError::NoAdapter => write!(f, "no GPU adapter (WebGPU/WebGL2 unavailable)"),
            InitError::NoDevice(e) => write!(f, "device request failed: {e}"),
            InitError::Surface(e) => write!(f, "surface error: {e}"),
        }
    }
}

/// A shaped text run's render-time record. Buffers are stored in `TextLayer`
/// in relayout order; `buffer_ix` maps a run to its shaped buffer.
#[derive(Clone)]
struct RunSpec {
    buffer_ix: usize,
    run: TextRun,
    /// Bubble-interior clip in CSS px (None = viewport clip only).
    clip: Option<[f32; 4]>,
    /// Screen-space run (banner): ignores scroll.
    pinned: bool,
}

pub struct Renderer {
    // Owned first so they drop LAST: the surface and device depend on the
    // instance/adapter staying alive — dropping the instance at the end of
    // init killed the presentation and fired a spurious device loss.
    // Deliberately never read: they are keep-alive handles.
    #[allow(dead_code)]
    instance: wgpu::Instance,
    #[allow(dead_code)]
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    surface_format: wgpu::TextureFormat,
    sample_count: u32,
    msaa_texture: wgpu::Texture,
    msaa_view: wgpu::TextureView,
    bubbles: BubblePass,
    pub text: TextLayer,
    /// `info.backend` string captured at init ("BrowserWebGpu" / "Gl" / …).
    pub backend_name: String,
    /// Laid-out frame content (rebuilt wholesale — dirty-buffer, spec §5).
    pub(crate) frame_layout: Layout,
    run_specs: Vec<RunSpec>,
    banner: Option<(PillGeom, RunSpec)>,
    viewport_css: (f32, f32),
    dpr: f32,
    /// CSS→device px ratio at the render boundary: dpr, reduced when the
    /// backing store is clamped to the adapter's max texture extent.
    scale: f32,
    /// `?text=0` — diagnostic: skip the glyphon pass entirely.
    text_pass_disabled: bool,
    suboptimal: bool,
}

impl Renderer {
    /// The single idempotent init path: instance → adapter → device → surface
    /// → pipelines → font atlas. Backend selection comes from the compile-time
    /// feature (webgpu | webgl); see `app.rs` for the JS-side chooser.
    pub async fn new(
        canvas: HtmlCanvasElement,
        msaa_samples: u32,
        text_pass_disabled: bool,
    ) -> Result<Self, InitError> {
        let backends = if cfg!(feature = "webgl") {
            wgpu::Backends::GL
        } else {
            wgpu::Backends::BROWSER_WEBGPU
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| InitError::Surface(e.to_string()))?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::default(),
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|_| InitError::NoAdapter)?;

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("chat-client device"),
                required_features: wgpu::Features::empty(),
                // Headless/safe-mode GL adapters expose no compute; the
                // default Limits demand 65535 workgroups per dimension and
                // fail the request outright. Downlevel defaults ask only
                // for what WebGL2 guarantees — WebGPU adapters satisfy
                // them too.
                required_limits: wgpu::Limits::downlevel_webgl2_defaults(),
                experimental_features: Default::default(),
                memory_hints: Default::default(),
                trace: Default::default(),
            })
            .await
            .map_err(|e| InitError::NoDevice(e.to_string()))?;

        let backend_name = format!("{:?}", adapter.get_info().backend);
        log::info!("renderer booted on backend {backend_name}");

        // Surface size: CSS px × devicePixelRatio, guarded to ≥1×1 (spec §6.1)
        // and clamped to the adapter's max texture extent (spec §6.1 hidpi —
        // WebGL2 caps at 2048).
        let (css_w, css_h, dpr) = canvas_css_size(&canvas);
        let (w, h) = device_size(css_w, css_h, dpr);
        let (w, h) = layout::clamp_to_max_extent(w, h, device.limits().max_texture_dimension_2d);

        let mut config = surface
            .get_default_config(&adapter, w, h)
            .ok_or(InitError::NoAdapter)?;
        // Premultiplied alpha (spec §6.2): the default `opaque` breaks
        // semi-transparent bubble shadows. Some backends (WebGL2) only
        // support Opaque — honor the adapter's capability list.
        {
            let caps = surface.get_capabilities(&adapter);
            config.alpha_mode = if caps
                .alpha_modes
                .contains(&wgpu::CompositeAlphaMode::PreMultiplied)
            {
                wgpu::CompositeAlphaMode::PreMultiplied
            } else {
                *caps.alpha_modes.first().expect("no alpha modes")
            };
        }
        let surface_format = config.format;
        surface.configure(&device, &config);

        // MSAA 4× by default (spec §5); `msaa=0` in the query string opts out
        // (some headless adapters fail the MSAA resolve). The renderer stays
        // in the WebGPU ∩ WebGL2 subset either way.
        let sample_count = msaa_samples.clamp(1, 4);
        let (msaa_texture, msaa_view) = make_msaa(&device, w, h, surface_format, sample_count);

        let bubbles = BubblePass::new(&device, surface_format, sample_count, (w as f32, h as f32));
        let text = TextLayer::new(
            &device,
            &queue,
            surface_format,
            wgpu::MultisampleState {
                count: sample_count,
                mask: !0,
                alpha_to_coverage_enabled: false,
            },
            (w, h),
        );

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            surface,
            config,
            surface_format,
            sample_count,
            msaa_texture,
            msaa_view,
            bubbles,
            text,
            backend_name,
            frame_layout: Layout::default(),
            run_specs: Vec::new(),
            banner: None,
            viewport_css: (css_w, css_h),
            dpr,
            scale: w as f32 / css_w.max(1.0),
            text_pass_disabled,
            suboptimal: false,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// Content height of the laid-out scene, CSS px (scroll math input).
    pub fn content_height(&self) -> f32 {
        self.frame_layout.content_height
    }

    /// CSS-px viewport size currently configured.
    pub fn viewport_css(&self) -> (f32, f32) {
        self.viewport_css
    }

    /// Re-read the canvas CSS size × DPR and re-configure the surface when it
    /// drifted (resize, visibilitychange, hidpi move). Min 1×1 guard (spec §6.1).
    pub fn resize(&mut self, canvas: &HtmlCanvasElement) -> bool {
        let (css_w, css_h, dpr) = canvas_css_size(canvas);
        let (w, h) = device_size(css_w, css_h, dpr);
        let (w, h) =
            layout::clamp_to_max_extent(w, h, self.device.limits().max_texture_dimension_2d);
        if (css_w, css_h, dpr) == (self.viewport_css.0, self.viewport_css.1, self.dpr)
            && w == self.config.width
            && h == self.config.height
        {
            return false;
        }
        self.viewport_css = (css_w, css_h);
        self.dpr = dpr;
        self.scale = w as f32 / css_w.max(1.0);
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        let (tex, view) = make_msaa(&self.device, w, h, self.surface_format, self.sample_count);
        self.msaa_texture = tex;
        self.msaa_view = view;
        self.bubbles.set_viewport(&self.queue, w as f32, h as f32);
        self.text.viewport.update(
            &self.queue,
            glyphon::Resolution {
                width: w,
                height: h,
            },
        );
        true
    }

    /// Re-configure after a stale/lost surface without a size change (spec §6.5).
    pub fn reconfigure(&mut self) {
        self.surface.configure(&self.device, &self.config);
    }

    /// Shape + measure + position every run (the "layout" half of the
    /// dirty-buffer split). Pure shaping work happens ONLY here.
    pub fn relayout(&mut self, scene: &Scene) {
        let (vw, vh) = self.viewport_css;
        let wrap = layout::wrap_width(vw);

        self.text.clear_buffers();
        let mut measured = Measurements::default();
        // Parallel to the shapes below: buffer indices returned by shape(),
        // so run specs never rely on a hand-maintained counter.
        let mut body_ix = Vec::with_capacity(scene.messages.len());

        // Message bodies: shape at the shared wrap width, measure each.
        for msg in &scene.messages {
            let (ix, size) = self
                .text
                .shape(&msg.text, crate::text::regular_metrics(), wrap);
            body_ix.push(ix);
            measured.messages.push(Measured {
                width: size.width,
                height: size.height,
            });
        }
        // Date pill.
        let date_ix = match &scene.date_pill {
            Some(text) => {
                let (ix, size) = self
                    .text
                    .shape(text, crate::text::small_metrics(), f32::MAX);
                measured.date_pill = Some(Measured {
                    width: size.width,
                    height: size.height,
                });
                Some(ix)
            }
            None => None,
        };
        // Presence pills (parallel to scene.pills).
        let mut pill_ix = Vec::with_capacity(scene.pills.len());
        for text in &scene.pills {
            let (ix, size) = self
                .text
                .shape(text, crate::text::small_metrics(), f32::MAX);
            pill_ix.push(ix);
            measured.pills.push(Measured {
                width: size.width,
                height: size.height,
            });
        }
        // Delivered label. Shaped unconditionally — layout() needs the
        // measurement before it knows whether the label is emitted.
        let (delivered_ix, delivered_size) =
            self.text
                .shape("Delivered", crate::text::small_metrics(), f32::MAX);
        measured.delivered_label = Measured {
            width: delivered_size.width,
            height: delivered_size.height,
        };
        // Banner text (pinned, screen-space).
        let banner_text = scene.banner.map(layout::banner_text);

        self.frame_layout = layout::layout(scene, &measured, (vw, vh));

        // Map shaped buffers → runs. Run specs reference the buffer indices
        // returned by shape() above — never a hand-maintained counter, which
        // desyncs whenever a shaped run has no spec ("Delivered" is always
        // shaped for measurement but its spec only exists when the label is
        // emitted). layout() pushes one bubble per message, so bodies map to
        // bubbles 1:1; pills/delivered/banner geometry comes from the layout
        // output directly.
        let n_bodies = scene.messages.len();
        let mut specs = Vec::with_capacity(n_bodies + 3);

        // Message bodies (1:1 with layout.bubbles).
        for (i, b) in self.frame_layout.bubbles.iter().enumerate().take(n_bodies) {
            let role = match b.side {
                crate::scene::Side::Incoming => TextRole::IncomingBody,
                crate::scene::Side::Outgoing => TextRole::OutgoingBody,
            };
            specs.push(RunSpec {
                buffer_ix: body_ix[i],
                run: TextRun {
                    x: b.x + layout::BUBBLE_PAD_H,
                    y: b.y + layout::BUBBLE_PAD_V,
                    role,
                },
                clip: Some([b.x + 2.0, b.y + 2.0, b.x + b.w - 2.0, b.y + b.h - 2.0]),
                pinned: false,
            });
        }

        // Pill text. frame_layout.pills is [date?, events…] (layout stacks the
        // date pill first); the indices match that order.
        let date_first = date_ix.is_some();
        for (pill_pos, pill) in self.frame_layout.pills.iter().enumerate() {
            let ix = if pill_pos == 0 && date_first {
                date_ix.unwrap_or(usize::MAX)
            } else {
                let event_pos = pill_pos - date_first as usize;
                match pill_ix.get(event_pos) {
                    Some(&ix) => ix,
                    None => continue, // defensive: layout emitted an extra pill
                }
            };
            specs.push(RunSpec {
                buffer_ix: ix,
                run: TextRun {
                    x: pill.x + layout::PILL_PAD_H,
                    y: pill.y + layout::PILL_PAD_V,
                    role: TextRole::Pill,
                },
                clip: Some([pill.x, pill.y, pill.x + pill.w, pill.y + pill.h]),
                pinned: false,
            });
        }

        // Delivered label (layout() emits at most one, under the last
        // outgoing ack'd message).
        if let Some(run) = self.frame_layout.delivered {
            specs.push(RunSpec {
                buffer_ix: delivered_ix,
                run,
                clip: None,
                pinned: false,
            });
        }

        // Banner (pinned pill, screen space); shaped last, index captured.
        // Its RunSpec must ALSO be pushed into `specs` — prepare_text renders
        // only runs from run_specs, so a banner spec held solely in the
        // self.banner tuple leaves the pill's text undrawn.
        self.banner = banner_text.map(|text| {
            let (banner_ix, size) = self
                .text
                .shape(text, crate::text::small_metrics(), f32::MAX);
            let m = Measured {
                width: size.width,
                height: size.height,
            };
            let pill = layout::banner_pill(vw, &m);
            let run = TextRun {
                x: pill.x + layout::PILL_PAD_H,
                y: pill.y + layout::PILL_PAD_V,
                role: TextRole::Pill,
            };
            let spec = RunSpec {
                buffer_ix: banner_ix,
                run,
                clip: None,
                pinned: true,
            };
            specs.push(spec.clone());
            (pill, spec)
        });

        self.run_specs = specs;
    }

    /// Draw one frame. Cheap: no shaping, only instance upload + two passes.
    pub fn render(&mut self, scroll: f32, dot_phase: f32) -> RenderOutcome {
        let frame = self.build_frame(scroll, dot_phase);
        let count = self
            .bubbles
            .upload_instances(&self.device, &self.queue, frame.instances);

        let surface_tex = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                log::warn!("surface suboptimal — will re-configure");
                self.suboptimal = true;
                t
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.reconfigure();
                return RenderOutcome::Reconfigured;
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return RenderOutcome::Skipped;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                log::error!("surface validation error — re-configuring");
                self.reconfigure();
                return RenderOutcome::Reconfigured;
            }
        };
        let view = surface_tex
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Text pass prep (atlas grow can fail → treat as frame-skip, not panic).
        // `?text=0` skips glyphon entirely (backend isolation diagnostic).
        let prepared: Result<(), glyphon::PrepareError> = if self.text_pass_disabled {
            Ok(())
        } else {
            match self.prepare_text(scroll) {
                Ok(()) => Ok(()),
                Err(e) => {
                    log::error!("text prepare failed: {e:?}");
                    Err(e)
                }
            }
        };

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        // MSAA off (sample_count == 1): render straight into the swapchain
        // view with no resolve target.
        let (color_view, resolve_target): (&wgpu::TextureView, Option<&wgpu::TextureView>) =
            if self.sample_count > 1 {
                (&self.msaa_view, Some(&view))
            } else {
                (&view, None)
            };
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("chat frame"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: color_view,
                    resolve_target,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::WHITE),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.bubbles.draw(&mut pass, count);
            if prepared.is_ok() && !self.text_pass_disabled {
                self.text
                    .text_renderer
                    .render(&self.text.atlas, &self.text.viewport, &mut pass)
                    .ok();
            }
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(surface_tex);

        if self.suboptimal {
            self.suboptimal = false;
            self.reconfigure();
        }

        if prepared.is_ok() {
            RenderOutcome::Drawn
        } else {
            RenderOutcome::Skipped
        }
    }

    fn prepare_text(&mut self, scroll: f32) -> Result<(), glyphon::PrepareError> {
        // Build TextAreas borrowing the shaped buffers, then hand them to
        // glyphon. Positions are in surface px; y is scrolled for content
        // runs, screen-fixed for the banner.
        let viewport_px = (self.config.width, self.config.height);
        let scale = self.scale;
        let mut areas = Vec::with_capacity(self.run_specs.len());
        for spec in &self.run_specs {
            let buffer = match self.text.buffers.get(spec.buffer_ix) {
                Some(b) => b,
                None => continue,
            };
            let y_off = if spec.pinned { 0.0 } else { -scroll };
            let left = (spec.run.x + 0.0) * scale;
            let top = (spec.run.y + y_off) * scale;
            // Viewport clip ∩ run clip, in device px.
            let clip = spec.clip.unwrap_or([
                0.0,
                0.0,
                viewport_px.0 as f32 / scale,
                viewport_px.1 as f32 / scale,
            ]);
            let bounds = glyphon::TextBounds {
                left: (clip[0] * scale).floor() as i32,
                top: ((clip[1] + y_off) * scale).floor() as i32,
                right: (clip[2] * scale).ceil() as i32,
                bottom: ((clip[3] + y_off) * scale).ceil() as i32,
            };
            areas.push(glyphon::TextArea {
                buffer,
                left,
                top,
                scale,
                bounds,
                default_color: run_color(spec.run.role),
                custom_glyphs: &[],
            });
        }
        self.text.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.text.font_system,
            &mut self.text.atlas,
            &self.text.viewport,
            areas,
            &mut self.text.swash_cache,
        )
    }

    fn build_frame(&mut self, scroll: f32, dot_phase: f32) -> FrameBytes {
        let scale = self.scale;
        let vh = self.viewport_css.1;
        let mut vals: Vec<f32> = Vec::with_capacity(16 * 64);

        let push = |vals: &mut Vec<f32>,
                    x: f32,
                    y: f32,
                    w: f32,
                    h: f32,
                    color: [f32; 4],
                    radii: [f32; 4],
                    tail_dir: f32| {
            // Quad extends past the box on the tail side so the tail fits.
            let (qx, qy, qw, qh, bw, bh) = if tail_dir == 0.0 {
                (x, y, w, h, w, h)
            } else {
                (
                    if tail_dir > 0.0 { x } else { x - TAIL_EXT_X },
                    y,
                    w + TAIL_EXT_X,
                    h + TAIL_EXT_Y,
                    w,
                    h,
                )
            };
            vals.extend_from_slice(&[qx * scale, qy * scale, qw * scale, qh * scale]);
            vals.extend_from_slice(&color);
            let radii = [
                radii[0] * scale,
                radii[1] * scale,
                radii[2] * scale,
                radii[3] * scale,
            ];
            vals.extend_from_slice(&radii);
            vals.extend_from_slice(&[tail_dir, 0.0, bw * scale, bh * scale]);
        };

        for b in &self.frame_layout.bubbles {
            let y = b.y - scroll;
            if y + b.h < 0.0 || y > vh {
                continue; // offscreen cull
            }
            let color = match b.side {
                crate::scene::Side::Incoming => INCOMING_BUBBLE,
                crate::scene::Side::Outgoing => OUTGOING_BUBBLE,
            };
            let tail_dir = match (b.tail, b.side) {
                (true, crate::scene::Side::Incoming) => -1.0,
                (true, crate::scene::Side::Outgoing) => 1.0,
                (false, _) => 0.0,
            };
            push(&mut vals, b.x, y, b.w, b.h, color, b.radii, tail_dir);
        }

        if let Some(t) = &self.frame_layout.typing_bubble {
            let y = t.y - scroll;
            if y + t.h >= 0.0 && y <= vh {
                push(&mut vals, t.x, y, t.w, t.h, INCOMING_BUBBLE, t.radii, -1.0);
                // Three dots, gentle bounce (reduced-motion handled in app.rs:
                // dot_phase is frozen there).
                let cx = t.x + t.w * 0.5;
                let cy = y + t.h * 0.5;
                for i in 0..3 {
                    let dx = (i as f32 - 1.0) * crate::layout::TYPING_DOT_GAP;
                    let dy = (dot_phase + i as f32 * 0.9).sin() * 2.0;
                    let r = crate::layout::TYPING_DOT_R;
                    push(
                        &mut vals,
                        cx + dx - r,
                        cy + dy - r,
                        r * 2.0,
                        r * 2.0,
                        TYPING_DOT,
                        [r, r, r, r],
                        0.0,
                    );
                }
            }
        }

        // Date pill.
        for p in &self.frame_layout.pills {
            let y = p.y - scroll;
            if y + p.h < 0.0 || y > vh {
                continue;
            }
            push(
                &mut vals,
                p.x,
                y,
                p.w,
                p.h,
                PILL_BUBBLE,
                [p.h * 0.5, p.h * 0.5, p.h * 0.5, p.h * 0.5],
                0.0,
            );
        }

        // Banner pill (screen space, drawn last so nothing overdraws it).
        if let Some((pill, _)) = &self.banner {
            push(
                &mut vals,
                pill.x,
                pill.y,
                pill.w,
                pill.h,
                PILL_BUBBLE,
                [pill.h * 0.5, pill.h * 0.5, pill.h * 0.5, pill.h * 0.5],
                0.0,
            );
        }

        FrameBytes {
            instances: f32_bytes(&vals),
        }
    }
}

struct FrameBytes {
    instances: Vec<u8>,
}

const TAIL_EXT_X: f32 = 10.0;
const TAIL_EXT_Y: f32 = 14.0;

// iOS palette (spec §5): premultiplied are equal since alpha = 1.
const OUTGOING_BUBBLE: [f32; 4] = rgb(0x0A, 0x84, 0xFF); // iOS blue
const INCOMING_BUBBLE: [f32; 4] = rgb(0xE9, 0xE9, 0xEB); // gray
const PILL_BUBBLE: [f32; 4] = rgb(0xE5, 0xE5, 0xEA);
const TYPING_DOT: [f32; 4] = rgb(0x8E, 0x8E, 0x93);

const fn rgb(r: u8, g: u8, b: u8) -> [f32; 4] {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

fn run_color(role: TextRole) -> glyphon::Color {
    match role {
        TextRole::OutgoingBody => glyphon::Color::rgb(255, 255, 255),
        TextRole::IncomingBody => glyphon::Color::rgb(0, 0, 0),
        TextRole::Delivered => glyphon::Color::rgb(0x8E, 0x8E, 0x93),
        TextRole::Pill => glyphon::Color::rgb(0x6C, 0x6C, 0x70),
    }
}

fn f32_bytes(vals: &[f32]) -> Vec<u8> {
    vals.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// (css_w, css_h, dpr) — canvas client size and the device pixel ratio.
fn canvas_css_size(canvas: &HtmlCanvasElement) -> (f32, f32, f32) {
    let w = canvas.client_width().max(1) as f32;
    let h = canvas.client_height().max(1) as f32;
    let dpr = web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0)
        .max(1.0) as f32;
    (w, h, dpr)
}

/// Device px, guarded to ≥1×1 (resize events fire with 0 during layout).
fn device_size(css_w: f32, css_h: f32, dpr: f32) -> (u32, u32) {
    (
        (css_w * dpr).round().max(1.0) as u32,
        (css_h * dpr).round().max(1.0) as u32,
    )
}
fn make_msaa(
    device: &wgpu::Device,
    w: u32,
    h: u32,
    format: wgpu::TextureFormat,
    samples: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    // sample_count 1 disables MSAA: the fields still exist but are never used
    // as an attachment — hand back a harmless 1×1 dummy.
    let (w, h, samples) = if samples <= 1 {
        (1, 1, 1)
    } else {
        (w, h, samples)
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("msaa target"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}
