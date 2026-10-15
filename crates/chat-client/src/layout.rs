//! Pure bubble-layout math — no wgpu, no web-sys, natively testable.
//!
//! Everything is in CSS pixels; the renderer converts to device pixels by the
//! device pixel ratio when building GPU data. Text is shaped elsewhere at a
//! fixed wrap width and hands its measured size in via [`Measurements`], so
//! this module stays a pure function of (scene, measurements, viewport).
// The host build exists to run these modules' tests; the wasm renderer
// consumes the rest of this module's items.
#![cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]

use crate::scene::{Banner, Scene, Side};

/// Max bubble width as a fraction of the conversation width (spec §5).
pub const MAX_BUBBLE_FRACTION: f32 = 0.72;
pub const BUBBLE_PAD_H: f32 = 13.0;
pub const BUBBLE_PAD_V: f32 = 7.0;
/// Inside a group vs between groups (spec §5: ≈3 px inside, ≈10 px between).
pub const GROUP_GAP: f32 = 3.0;
pub const BETWEEN_GAP: f32 = 10.0;
pub const CORNER_RADIUS: f32 = 18.0;
/// Grouped joints and tail nubs pull in (spec §5).
pub const GROUP_CORNER: f32 = 10.0;
pub const FONT_SIZE: f32 = 17.0;
pub const LINE_HEIGHT: f32 = 22.0;
/// Typing-dots bubble size.
pub const TYPING_BUBBLE_W: f32 = 76.0;
pub const TYPING_BUBBLE_H: f32 = 36.0;
/// Dot radius and spacing for the typing indicator.
pub const TYPING_DOT_R: f32 = 4.0;
pub const TYPING_DOT_GAP: f32 = 14.0;
pub const PILL_PAD_H: f32 = 10.0;
pub const PILL_PAD_V: f32 = 5.0;
pub const PILL_FONT_SIZE: f32 = 12.0;
pub const PILL_LINE_HEIGHT: f32 = 15.0;
/// Top padding where the date pill sits.
pub const PILL_TOP: f32 = 10.0;
/// Gap between a bubble and its "Delivered" label.
pub const DELIVERED_GAP: f32 = 3.0;

/// Which gray status label rides under the last outgoing bubble.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusKind {
    /// Ack arrived — the terminal state for v1 (spec §5).
    #[default]
    Delivered,
    /// In-flight while Reconnecting (spec §5 mock: "sending…").
    Sending,
}
/// Left/right breathing room for edge-to-edge bubbles.
pub const CONV_MARGIN: f32 = 14.0;

/// Shaped-text size for one text run, CSS px.
#[derive(Clone, Copy, Debug, Default)]
pub struct Measured {
    pub width: f32,
    pub height: f32,
}

/// Measurements parallel to the scene's text runs.
#[derive(Clone, Debug, Default)]
pub struct Measurements {
    /// Parallel to `scene.messages`.
    pub messages: Vec<Measured>,
    pub date_pill: Option<Measured>,
    /// Parallel to `scene.pills` (presence pills under the date pill).
    pub pills: Vec<Measured>,
    pub delivered_label: Measured,
    pub sending_label: Measured,
}

/// One bubble quad in content space (before scroll), CSS px.
#[derive(Clone, Copy, Debug)]
pub struct BubbleGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Corner radii: top-left, top-right, bottom-right, bottom-left.
    pub radii: [f32; 4],
    pub side: Side,
    /// Only the last bubble of a group carries the tail (spec §5).
    pub tail: bool,
}

/// One positioned text run; colored at render time by `role`.
#[derive(Clone, Copy, Debug)]
pub struct TextRun {
    pub x: f32,
    pub y: f32,
    pub role: TextRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextRole {
    /// White inside an outgoing bubble.
    OutgoingBody,
    /// Near-black inside an incoming bubble.
    IncomingBody,
    /// Small gray status label ("Delivered").
    Delivered,
    /// Small gray pill text (date).
    Pill,
}

/// A centered pill (date, banner) bubble geometry.
#[derive(Clone, Copy, Debug)]
pub struct PillGeom {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Laid-out frame content, CSS px, in content space (y grows downward from
/// the top of the conversation; the renderer subtracts the scroll offset).
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub content_height: f32,
    pub bubbles: Vec<BubbleGeom>,
    /// Typing-dots bubble, if the scene shows one.
    pub typing_bubble: Option<BubbleGeom>,
    pub pills: Vec<PillGeom>,
    /// Status label ("Delivered" / "Sending…") under the last outgoing
    /// message — kind in [`status_kind`](Layout::status_kind).
    pub status: Option<TextRun>,
    pub status_kind: StatusKind,
}

/// Compute the wrap width cosmic-text should shape message text at: the max
/// bubble width minus horizontal padding. Pure — shared by native tests.
pub fn wrap_width(viewport_w: f32) -> f32 {
    (MAX_BUBBLE_FRACTION * viewport_w - 2.0 * BUBBLE_PAD_H).max(40.0)
}

/// Proportionally clamp a backing store so neither dimension exceeds the
/// adapter's max texture extent — WebGL2 caps at 2048 px, and a hidpi canvas
/// (e.g. 1440×900 @2 = 2880×1800) would panic in Surface::configure without
/// this. CSS layout size is untouched; the browser scales the smaller
/// backing store up.
pub fn clamp_to_max_extent(w: u32, h: u32, max_extent: u32) -> (u32, u32) {
    let long = w.max(h);
    if long <= max_extent || max_extent == 0 {
        return (w, h);
    }
    let scale = max_extent as f64 / long as f64;
    (
        ((w as f64) * scale).floor().max(1.0) as u32,
        ((h as f64) * scale).floor().max(1.0) as u32,
    )
}

fn clamp_w(w: f32, max_w: f32) -> f32 {
    w.min(max_w)
}

/// The banner pill pins to the viewport top (banner_pill: y=10, h≈25) and
/// draws before content — the pill stack must start below it, or the pills
/// overdraw the banner during reconnects.
const BANNER_STACK_TOP: f32 = 45.0;

/// Lay out a scene. `viewport` is the conversation surface size in CSS px.
pub fn layout(scene: &Scene, measured: &Measurements, viewport: (f32, f32)) -> Layout {
    let max_bubble_w = MAX_BUBBLE_FRACTION * viewport.0;
    let inner_w = viewport.0 - 2.0 * CONV_MARGIN;
    let mut out = Layout::default();
    let mut y = if scene.banner.is_some() {
        BANNER_STACK_TOP
    } else {
        0.0
    };

    // Date pill.
    if let (Some(_), Some(m)) = (&scene.date_pill, measured.date_pill) {
        let w = m.width + 2.0 * PILL_PAD_H;
        let h = m.height + 2.0 * PILL_PAD_V;
        out.pills.push(PillGeom {
            x: (viewport.0 - w) / 2.0,
            y: y + PILL_TOP,
            w,
            h,
        });
        y += PILL_TOP + h + BETWEEN_GAP;
    }

    // Presence pills ("Sam joined" / "Sam left"), stacked under the date pill.
    for (_, m) in scene.pills.iter().zip(measured.pills.iter()) {
        let w = m.width + 2.0 * PILL_PAD_H;
        let h = m.height + 2.0 * PILL_PAD_V;
        out.pills.push(PillGeom {
            x: (viewport.0 - w) / 2.0,
            y,
            w,
            h,
        });
        y += h + GROUP_GAP;
    }

    // Bubbles: group consecutive same-side messages.
    let mut i = 0usize;
    let mut prev_side: Option<Side> = None;
    while i < scene.messages.len() {
        let side = scene.messages[i].side;
        let gap_after_prev = match prev_side {
            Some(s) if s == side => GROUP_GAP,
            Some(_) => BETWEEN_GAP,
            None => 0.0,
        };
        y += gap_after_prev;

        // Group extent: consecutive messages with the same side.
        let mut j = i;
        while j + 1 < scene.messages.len() && scene.messages[j + 1].side == side {
            j += 1;
        }
        let last_of_group = j;

        let mut group_y = y;
        for k in i..=j {
            let m = measured.messages.get(k).copied().unwrap_or_default();
            let w = clamp_w(m.width + 2.0 * BUBBLE_PAD_H, max_bubble_w);
            let h = m.height + 2.0 * BUBBLE_PAD_V;
            // Incoming hugs the left margin; outgoing right-aligns (spec §5).
            let x = match side {
                Side::Incoming => CONV_MARGIN,
                Side::Outgoing => CONV_MARGIN + inner_w - w,
            };
            let is_tail = k == last_of_group;
            // The tail attaches at the pulled-in nub corner; other corners
            // stay round. Radii order: top-left, top-right, bottom-right,
            // bottom-left.
            let r = CORNER_RADIUS;
            let rg = GROUP_CORNER;
            let radii = match side {
                Side::Incoming => [r, r, r, rg], // tail nub at bottom-left
                Side::Outgoing => [r, r, rg, r], // tail nub at bottom-right
            };
            out.bubbles.push(BubbleGeom {
                x,
                y: group_y,
                w,
                h,
                radii,
                side,
                tail: is_tail,
            });
            group_y += h;

            // Status under the final outgoing message of the conversation:
            // "Delivered" once ack'd, "Sending…" while Reconnecting, nothing
            // for a live in-flight send (state table: pending shows none).
            let is_last_message = k + 1 == scene.messages.len();
            if is_last_message && side == Side::Outgoing {
                let kind = match (scene.messages[k].delivered, scene.banner) {
                    (true, _) => Some(StatusKind::Delivered),
                    (false, Some(Banner::Reconnecting)) => Some(StatusKind::Sending),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let m = match kind {
                        StatusKind::Delivered => measured.delivered_label,
                        StatusKind::Sending => measured.sending_label,
                    };
                    out.status = Some(TextRun {
                        x: x + w - m.width,
                        y: group_y + DELIVERED_GAP,
                        role: TextRole::Delivered,
                    });
                    out.status_kind = kind;
                    group_y += m.height + DELIVERED_GAP;
                }
            }

            if k < j {
                group_y += GROUP_GAP;
            }
        }
        y = group_y;
        prev_side = Some(side);
        i = j + 1;
    }

    // Typing indicator: an incoming-side dot bubble after the last message.
    if scene.typing_peer.is_some() {
        y += match prev_side {
            Some(Side::Incoming) => GROUP_GAP,
            Some(_) => BETWEEN_GAP,
            None => 0.0,
        };
        out.typing_bubble = Some(BubbleGeom {
            x: CONV_MARGIN,
            y,
            w: TYPING_BUBBLE_W,
            h: TYPING_BUBBLE_H,
            radii: [CORNER_RADIUS, CORNER_RADIUS, CORNER_RADIUS, GROUP_CORNER],
            side: Side::Incoming,
            tail: true,
        });
        y += TYPING_BUBBLE_H;
    }

    out.content_height = y + 16.0; // bottom breathing room
    out
}

/// Scroll offset that keeps the conversation pinned to the bottom. Pure.
pub fn stick_to_bottom(content_height: f32, viewport_h: f32) -> f32 {
    (content_height - viewport_h).max(0.0)
}

/// Clamp a user-driven scroll into the valid range. Pure.
pub fn clamp_scroll(scroll: f32, content_height: f32, viewport_h: f32) -> f32 {
    scroll.clamp(0.0, stick_to_bottom(content_height, viewport_h))
}

/// Banner pill geometry, drawn last, pinned to the top of the viewport.
pub fn banner_pill(viewport_w: f32, text: &Measured) -> PillGeom {
    let w = text.width + 2.0 * PILL_PAD_H;
    let h = text.height + 2.0 * PILL_PAD_V;
    PillGeom {
        x: (viewport_w - w) / 2.0,
        y: 10.0,
        w,
        h,
    }
}

/// Which banner text to show, if any.
pub fn banner_text(banner: Banner) -> &'static str {
    match banner {
        Banner::Connecting => "Connecting…",
        Banner::Reconnecting => "Reconnecting…",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::Message;

    #[test]
    fn clamp_passes_through_within_extent() {
        assert_eq!(clamp_to_max_extent(1280, 577, 2048), (1280, 577));
        assert_eq!(clamp_to_max_extent(2048, 2048, 2048), (2048, 2048));
        assert_eq!(clamp_to_max_extent(0, 0, 2048), (0, 0));
    }

    #[test]
    fn clamp_proportionally_scales_hidpi_canvas() {
        // 1440×900 @2 dpr — the WebGL2 2048 px cap would panic configure().
        assert_eq!(clamp_to_max_extent(2880, 1800, 2048), (2048, 1280));
        // Portrait: the height is the long side.
        assert_eq!(clamp_to_max_extent(900, 2880, 2048), (640, 2048));
        // Never degenerate: results stay ≥1.
        let (w, h) = clamp_to_max_extent(4000, 4, 2048);
        assert_eq!((w, h), (2048, 2));
    }

    fn m(w: f32, h: f32) -> Measured {
        Measured {
            width: w,
            height: h,
        }
    }

    fn msg(id: &str, side: Side, delivered: bool) -> Message {
        Message {
            id: id.into(),
            side,
            text: "text".into(),
            delivered,
        }
    }

    #[test]
    fn single_incoming_bubble_hugs_content() {
        let scene = Scene {
            messages: vec![msg("a", Side::Incoming, false)],
            ..Default::default()
        };
        let measured = Measurements {
            messages: vec![m(100.0, 22.0)],
            ..Default::default()
        };
        let out = layout(&scene, &measured, (400.0, 800.0));
        assert_eq!(out.bubbles.len(), 1);
        let b = out.bubbles[0];
        assert!((b.w - (100.0 + 2.0 * BUBBLE_PAD_H)).abs() < 0.01);
        assert!((b.h - (22.0 + 2.0 * BUBBLE_PAD_V)).abs() < 0.01);
        assert!(b.tail);
        assert_eq!(
            b.radii,
            [CORNER_RADIUS, CORNER_RADIUS, CORNER_RADIUS, GROUP_CORNER]
        );
    }

    #[test]
    fn outgoing_right_aligns() {
        let scene = Scene {
            messages: vec![msg("a", Side::Outgoing, true)],
            ..Default::default()
        };
        let measured = Measurements {
            messages: vec![m(100.0, 22.0)],
            delivered_label: m(50.0, 12.0),
            ..Default::default()
        };
        let out = layout(&scene, &measured, (400.0, 800.0));
        let b = out.bubbles[0];
        let inner_w = 400.0 - 2.0 * CONV_MARGIN;
        assert!((b.x - (CONV_MARGIN + inner_w - b.w)).abs() < 0.01);
        // Delivered label sits under the bubble, right-aligned to it.
        let label = out.status.expect("delivered run present");
        assert!(label.y > b.y + b.h);
        assert!((label.x + 50.0 - (b.x + b.w)).abs() < 0.01);
        assert_eq!(out.status_kind, StatusKind::Delivered);
    }

    // Spec §5 mock: the in-flight bubble shows "sending…" while
    // Reconnecting, and nothing while live.
    #[test]
    fn sending_label_only_while_reconnecting() {
        let scene = |banner| Scene {
            messages: vec![msg("a", Side::Outgoing, false)],
            banner,
            ..Default::default()
        };
        let measured = Measurements {
            messages: vec![m(100.0, 22.0)],
            sending_label: m(46.0, 12.0),
            ..Default::default()
        };
        let live = layout(&scene(None), &measured, (400.0, 800.0));
        assert!(live.status.is_none(), "live in-flight send has no label");

        let reconnecting = layout(
            &scene(Some(Banner::Reconnecting)),
            &measured,
            (400.0, 800.0),
        );
        let b = reconnecting.bubbles[0];
        let label = reconnecting.status.expect("sending label present");
        assert!(label.y > b.y + b.h);
        assert!((label.x + 46.0 - (b.x + b.w)).abs() < 0.01);
        assert_eq!(reconnecting.status_kind, StatusKind::Sending);
    }

    #[test]
    fn group_uses_inner_gap_and_single_tail() {
        let scene = Scene {
            messages: vec![
                msg("a", Side::Incoming, false),
                msg("b", Side::Incoming, false),
            ],
            ..Default::default()
        };
        let measured = Measurements {
            messages: vec![m(80.0, 22.0), m(60.0, 22.0)],
            ..Default::default()
        };
        let out = layout(&scene, &measured, (400.0, 800.0));
        assert_eq!(out.bubbles.len(), 2);
        let (a, b) = (out.bubbles[0], out.bubbles[1]);
        assert!(!a.tail, "first bubble of group has no tail");
        assert!(b.tail);
        assert!(
            (b.y - (a.y + a.h + GROUP_GAP)).abs() < 0.01,
            "group gap is tight"
        );
        assert_eq!(
            a.radii,
            [CORNER_RADIUS, CORNER_RADIUS, CORNER_RADIUS, GROUP_CORNER]
        );
    }

    #[test]
    fn side_change_uses_between_gap() {
        let scene = Scene {
            messages: vec![
                msg("a", Side::Incoming, false),
                msg("b", Side::Outgoing, false),
            ],
            ..Default::default()
        };
        let measured = Measurements {
            messages: vec![m(80.0, 22.0), m(60.0, 22.0)],
            ..Default::default()
        };
        let out = layout(&scene, &measured, (400.0, 800.0));
        let (a, b) = (out.bubbles[0], out.bubbles[1]);
        assert!((b.y - (a.y + a.h + BETWEEN_GAP)).abs() < 0.01);
    }

    #[test]
    fn wide_text_clamps_to_max_bubble_width() {
        let scene = Scene {
            messages: vec![msg("a", Side::Incoming, false)],
            ..Default::default()
        };
        let measured = Measurements {
            messages: vec![m(900.0, 66.0)],
            ..Default::default()
        };
        let out = layout(&scene, &measured, (400.0, 800.0));
        let b = out.bubbles[0];
        assert!((b.w - (MAX_BUBBLE_FRACTION * 400.0)).abs() < 0.01);
        assert_eq!(b.side, Side::Incoming);
    }

    #[test]
    fn typing_bubble_after_messages() {
        let scene = Scene {
            messages: vec![msg("a", Side::Outgoing, false)],
            typing_peer: Some("Sam".into()),
            ..Default::default()
        };
        let measured = Measurements {
            messages: vec![m(80.0, 22.0)],
            ..Default::default()
        };
        let out = layout(&scene, &measured, (400.0, 800.0));
        let b = out.typing_bubble.expect("typing bubble present");
        assert!(b.y > out.bubbles[0].y);
        assert_eq!(b.side, Side::Incoming);
    }

    #[test]
    fn date_pill_sits_on_top() {
        let scene = Scene {
            date_pill: Some("Today 4:12 PM".into()),
            ..Default::default()
        };
        let measured = Measurements {
            date_pill: Some(m(90.0, 15.0)),
            ..Default::default()
        };
        let out = layout(&scene, &measured, (400.0, 800.0));
        let pill = out.pills[0];
        assert!((pill.x - (400.0 - pill.w) / 2.0).abs() < 0.01);
        assert_eq!(pill.y, PILL_TOP);
    }

    #[test]
    fn scroll_helpers() {
        assert_eq!(stick_to_bottom(500.0, 800.0), 0.0);
        assert_eq!(stick_to_bottom(1200.0, 800.0), 400.0);
        assert_eq!(clamp_scroll(-5.0, 1200.0, 800.0), 0.0);
        assert_eq!(clamp_scroll(999.0, 1200.0, 800.0), 400.0);
    }

    #[test]
    fn wrap_width_floor() {
        assert!(wrap_width(0.0) >= 40.0);
    }
}
