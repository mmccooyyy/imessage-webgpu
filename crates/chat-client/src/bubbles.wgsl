// Bubble pass: instanced rounded-rect SDF quads with optional iMessage tail.
//
// One quad per bubble (6 verts from vertex_index, no vertex buffer). Instance
// data is entirely f32 so the CPU side can build bytes without a pod crate.
// Colors arrive PREMULTIPLIED; blending is One / OneMinusSrcAlpha.
//
// Instance attributes are vertex-stage-only: per-instance constants the
// fragment needs (box size, tail direction) are forwarded through VsOut with
// @flat interpolation.

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) local: vec2<f32>, // px inside the quad, origin top-left
    @location(1) @interpolate(flat) box_wh: vec2<f32>, // box w/h (rect may be larger for the tail)
    @location(2) @interpolate(flat) tail_dir: f32, // -1 left, +1 right, 0 none
    @location(3) @interpolate(flat) radii: vec4<f32>, // tl, tr, br, bl in px
    @location(4) @interpolate(flat) color: vec4<f32>, // premultiplied
    @location(5) @interpolate(flat) box_tl: vec2<f32>, // box top-left inside the quad
};

// rect: x, y, w, h of the quad in surface px (may extend past the box to
//                      make room for the tail)
// radii: tl, tr, br, bl of the box in px
// extras: tail_dir (-1 left, +1 right, 0 none), unused, box_w, box_h
struct Instance {
    @location(1) rect: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) radii: vec4<f32>,
    @location(4) extras: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u_viewport: vec4<f32>; // width, height, _, _

// Must match TAIL_EXT_X / TAIL_EXT_Y in renderer.rs (quad padding for the tail).
const TAIL_EXT_X: f32 = 10.0;
const TAIL_EXT_Y: f32 = 14.0;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    let x = f32(vi == 1u || vi == 4u || vi == 5u);
    let y = f32(vi == 2u || vi == 3u || vi == 5u);
    let corner = vec2<f32>(x, y);
    let local = corner * inst.rect.zw;
    let world = inst.rect.xy + local;
    let clip_pos = vec2<f32>(
        world.x / u_viewport.x * 2.0 - 1.0,
        1.0 - world.y / u_viewport.y * 2.0,
    );
    var out: VsOut;
    out.clip = vec4<f32>(clip_pos, 0.0, 1.0);
    out.local = local;
    out.box_wh = inst.extras.zw;
    out.tail_dir = inst.extras.x;
    out.radii = inst.radii;
    out.color = inst.color;
    // CPU side: tail_dir > 0 puts the quad at the box's own x (box at the
    // quad's left); tail_dir < 0 shifts the quad left (box offset by the
    // tail extension on x).
    out.box_tl = vec2<f32>(select(0.0, TAIL_EXT_X, inst.extras.x < 0.0), 0.0);
    return out;
}

// Rounded-box SDF, p relative to the box CENTER, y-down.
// radii = tl, tr, br, bl.
fn sd_box(p: vec2<f32>, half: vec2<f32>, radii: vec4<f32>) -> f32 {
    let top_r = select(radii.x, radii.y, p.x >= 0.0);
    let bottom_r = select(radii.w, radii.z, p.x >= 0.0);
    let r = select(top_r, bottom_r, p.y >= 0.0);
    let q = abs(p) - (half - vec2<f32>(r));
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

@fragment
fn fs_main(out: VsOut) -> @location(0) vec4<f32> {
    let box_wh = out.box_wh;
    let tail_dir = out.tail_dir;
    // The box is anchored at box_tl inside the (possibly larger) tail quad.
    let bc = out.box_tl + box_wh * 0.5;
    var d = sd_box(out.local - bc, box_wh * 0.5, out.radii);

    if (tail_dir != 0.0) {
        // iMessage-ish tail: a blob smoothed into the bottom corner of the
        // box, on the bubble's own side.
        let tx = out.box_tl.x + select(4.0, box_wh.x - 4.0, tail_dir > 0.0);
        let tc = vec2<f32>(tx, out.box_tl.y + box_wh.y - 2.0);
        let td = length(out.local - tc) - 11.0;
        d = smin(d, td, 7.0);
    }

    let aa = max(fwidth(d) * 0.7, 1e-4);
    let coverage = 1.0 - smoothstep(-aa, aa, d);
    return vec4<f32>(out.color.rgb * coverage, out.color.a * coverage);
}
