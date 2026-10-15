# QA evidence — iMessage fidelity polish

All evidence was captured against the **hosted single-origin deployment**
(`https://063rvk6mz2-8080.hosted.obvious.ai`, serving `dist/` + `/ws` from one
axum process) at native 1440×757 via CDP `Page.captureScreenshot` polling, and
uploaded through `obvious autobuild upload` against the tested head SHA
(`e753ed5c1ce97857aa079fc982489468f2992ab6`).

| Case | Criterion | Evidence |
|------|-----------|----------|
| tc-1 | Fidelity numeric targets: iOS blue `#0A84FF`/white outgoing, `#E9E9EB`/near-black incoming, ≈18px radius, ≈72% max width, 7×13px padding, 3px in-burst / 10px between senders, tail on the last bubble only, time pill, presence pills, "Delivered" | `tc-1-fidelity-targets.png` |
| tc-2 | Typing dots animate; `prefers-reduced-motion` renders static dots | `tc-2-typing-dots.png`, `tc-2-typing-animation.webm`, `tc-2-reduced-motion-static-dots.png` (0 pixels differ between consecutive frames under emulation) |
| tc-3 | Reconnecting state per the four-state mock: banner pill, grouped pending pair with "Sending…", dimmed (still usable) send | `tc-3-reconnecting-banner.png` |
| tc-4 | Live two-tab exchange on the hosted URL — messages in matching order, "Delivered" under the outgoing bubble | `tc-4-two-tab-exchange.webm` |
| tc-5 | Reconnect drill: server killed → "Reconnecting…" banner → send queued while offline (draft restored if refused mid-attempt) → restart → the queued message delivers exactly once, no duplicates | `tc-5-reconnect-drill.webm`, `tc-5-resumed-delivered-once.png` |

## Environment notes

- **Bundle under test:** the WebGL2 bundle (`/webgl2/`). Headless Chrome in this
  sandbox runs SwiftShader, which cannot provide a WebGPU device (the `/webgpu/`
  bundle probes blank — spec risk #8). This matches the spec's named fallback:
  same app code, and the WebGPU path is verified on WebGPU-capable hardware
  before merge (PR #6 evidence, `art_KgBVeaWM`).
- **Server restart and history:** the room is an in-memory ring by design; after
  a server restart the fresh `welcome` carries an empty history and the
  transcript converges to it. The drill's queued message survives via its
  `cmid` retry and is delivered exactly once.
- Captures live in project QA storage (attached to PR #8); this document
  records what was captured and how.
