# xross roadmap: performance, code quality, future work

Written against `e39d352`. It sits alongside the multi-monitor plan, and several items
here are meant to be folded into that work rather than done twice (see
[Sequence](#5-sequence)).

How items are written:

- **ID** (O = optimization, R = refactor, S = scope, F = future) for referring to them in
  commits.
- **Effort:** S = under half a day, M = 1–2 days, L = several days.
- **Why** points at the code as it is today (`file:line`), **Change** says what to do,
  **Verify** says how we'll know it worked.

Line numbers drift; search for the quoted code if they don't match.

## Index

| ID | What | Effort | Milestone |
|---|---|---|---|
| P0 | Frame and wakeup instrumentation | S | A |
| O1 | Redraw only when something changed | M | A |
| O2 | One per-frame context instead of recomputing views | S | A |
| O3 | Close-button hover tracked on input, not per frame | S | A |
| O4 | Allocation-free title bars | S | A |
| O5 | Cursor frames without cloning pixels | S | A |
| O6 | One close-icon texture for all windows | S | A |
| O7 | Opaque regions for rounded windows | S–M | A |
| O8 | Cheaper shadows (only if P0 says so) | M | later |
| O9 | Nothing blocking on the event loop | S | A |
| O10 | Capture: cheap readback, damage, regions | M | D |
| O11 | Capture: GPU (dmabuf) buffers | L | D |
| O12 | O(1) window lookups on commit and X11 events | S | A |
| O13 | Startup commands don't wait for Xwayland | S | A |
| O14 | Start Xwayland on first X client | M | later |
| O15 | Hidden windows marked suspended | S | C |
| O16 | Alt-tab thumbnails live on every workspace | S | A |
| R1 | Split `XrossState` | L | A (part), C |
| R2 | One per-window data struct | S | A |
| R3 | Workspaces own their windows | L | C |
| R4 | One queue for deferred actions | S–M | C |
| R5 | Keyboard filter as ordered stages | M | C |
| R6 | Shared title-text cache | S | D |
| R7 | `Animation2` and one idle-timer helper | S | A |
| R8 | Shared tile transform | S | A |
| R9 | IPC message enum | S | A |
| R10 | Config passed in, not a global | M | C |
| R11 | Child environment without `set_var` | S | A |
| R12 | Small cleanups | S | A |
| R13 | Split the four largest modules | M | D |
| R14 | Headless state tests | M | D |
| S1 | Portals: stay out, configure from the NixOS module | S | A |
| S2 | Close icon drawn by shader; drop resvg and `icons.rs` | S–M | D |
| S3 | Retire wlr-screencopy when tools allow | S | later |
| S4 | Test-only IPC behind a flag | S | A |
| S5 | Type-to-launch: keep, pin its contract | S | A |
| S6 | Things xross should not grow | — | — |
| F* | Future work to note | — | E |

---

## 0. Measure first

### P0. Frame and wakeup instrumentation (S)

**Why:** most items below are argued from reading the code. A baseline makes them
measurable and catches regressions.

**Change:**
- Count, per output: renders, renders with no damage, and wakeups per second.
- Time the element-list build (`output_elements`) and the `render_frame` call with
  `std::time::Instant` (cheap; no new dependency).
- `xross msg stats` logs the averages since the last call and resets them.

**Benchmark scenarios** (scripted where possible with `xross msg`):
1. Idle desktop for 60 s, one terminal open.
2. Ten windows, typing in one.
3. Overview open/close loop (`xross msg overview` ×20).
4. Alt-tab held for 5 s with 6 windows.
5. `wf-recorder` for 30 s.
6. Dragging an X11 window (xterm) around for 10 s.

Outside tools: `powertop` (wakeups), `perf stat -p $(pidof xross)`, `intel_gpu_top` or
`radeontop`.

**Verify:** numbers for all six scenarios are written down at the top of the PR that adds
this, so later PRs can compare against them.

---

## 1. Optimizations

### O1. Redraw only when something changed (M)

**Why:**
- `src/backend/udev.rs:523`: when a frame has no damage (`Ok(false)`), `render` arms a
  timer one frame later. An idle desktop wakes at the refresh rate (60–165/s).
- Each wake rebuilds the element list and runs `post_render`: `space.refresh()`, frame
  callbacks, `popups.cleanup()`, `flush_clients()`.
- Nested: `src/backend/winit.rs:111` calls `request_redraw()` after every frame.

**Change:**
- Per-output redraw state, as niri does:
  `enum Redraw { Idle, Queued, WaitingForVblank { queued_again: bool } }`.
- `XrossState::queue_redraw_all()` (and `queue_redraw(&Output)` once multi-monitor
  lands). It arms a zero-delay idle render if the output is `Idle`.
- Triggers that must queue a redraw:
  - `CompositorHandler::commit` (every client commit, including layer surfaces, the
    cursor surface and the lock surface). Every frame-callback request comes with a
    commit, so this keeps clients animating.
  - Pointer motion while xross draws the cursor (`draw_cursor`), and any hover change
    (switcher tile, close button).
  - `run_action`, overview/switcher/launch changes, config apply, output mode or scale
    change, lock and unlock, a new capture request.
  - Timed UI: the workspace OSD hiding (`ws_osd_until`) needs a timer that queues a redraw
    at that time.
- After rendering: if any animation is still running (camera, overview progress and centre
  and shifts, window geometry/open animations, closing windows, gestures), queue again.
  `tick_window_animations` already returns whether animations are running
  (`src/shell/snap.rs:74`); give camera and overview the same.
- On vblank: render if `queued_again`, else go `Idle`.
- An empty render (no damage) submits no frame, so there's no vblank. Send frame callbacks
  and then run an "estimated vblank" timer one refresh later. That keeps clients throttled
  to the refresh rate without spinning.
- Escape hatch: `XROSS_ALWAYS_REDRAW=1` restores today's behaviour, for bisecting a
  missed trigger.
- Nested: `request_redraw()` only when queued.

**Risk:** a missed trigger leaves stale pixels until the next input. The escape hatch and
the scripted scenarios cover it.

**Verify:**
- P0 scenario 1: wakeups/s drop from ~refresh rate to ~1 (the housekeeping timer).
- Scenarios 2–4 look the same in nested screenshots and on btty.

### O2. One per-frame context (S)

**Why:**
- `workspace_views` runs at least three times per frame: `src/render.rs:296`, `:524`
  (`post_render`) and `:565` (`presentation_feedback`). It also runs again through
  `frame()` in `slot_views` and `selection_rect`.
- Each call checks `overview_shows` → `has_windows` (`src/shell/workspace.rs:60`) for every
  workspace. That's a scan of every window, so O(workspaces × windows) per call, even
  outside the overview.

**Change:**
- `struct FrameCtx { now, p, in_overview, views: Vec<WsView>, visible: Vec<WsId>,
  focused: Option<Window>, hovered_close: Option<Window>, fullscreen_on_top: bool }`,
  built once at the top of `output_elements_for`.
- `has_windows` for all workspaces comes from one pass over the windows
  (`HashMap<WsId, usize>`) until R3 makes it O(1).
- Store the last `FrameCtx` on the state (in a `RefCell`), so `post_render` and
  `presentation_feedback` reuse it.
- `views.iter().find(|v| v.id == Some(ws))` in the window loop is fine at these sizes;
  leave it.

**Verify:** P0 build time down in scenarios 2 and 3; identical screenshots.

### O3. Close-button hover tracked on input (S)

**Why:** `src/render.rs:257` runs `pointer_hit` every frame to learn whether the pointer
is over a close button. That's a full layer-shell plus surface hit-test per frame.

**Change:**
- `hovered_close: Option<Window>` on the state, updated in `on_pointer_motion`.
- Also recompute it after anything that moves windows under a still pointer (relayout,
  workspace switch, window close). `refresh_pointer_focus` already runs in most of those
  places; set it there.
- A change queues a redraw (O1).

**Verify:** hovering the close button still highlights it, including after a Mod+arrow
snap with the pointer left in place.

### O4. Allocation-free title bars (S)

**Why:** per window, per frame, `ssd::render_elements` does all of this:
- clones the title (`window_title`, `src/shell/switcher.rs:52`, which also locks the
  toplevel state),
- builds a `format!` cache key (`src/shell/ssd.rs:199`),
- clones the whole `Appearance` struct, Strings included (`:196`), and the icon theme name,
- re-parses the shadow colour (`:329`).

**Change:**
- Resolve the appearance once in `apply_config`: font key, clamped font px, shadow colour as
  `[f32; 4]`, shadow reach, radius. Store the result next to the config.
- `with_window_title(window, |title: &str| …)` instead of returning a `String`. Compare it
  to the cached key in place, and only allocate when the title really changed.

**Verify:** titles still update live (e.g. `printf '\e]0;x\a'` in foot); P0 build time.

### O5. Cursor frames without cloning pixels (S)

**Why:** `frame()` (`src/cursor.rs:170–184`) returns an owned `Image`, cloning
`pixels_rgba` and `pixels_argb` on every frame the cursor is drawn.

**Change:**
- Return the frame index only. Read `xhot`, `yhot` and `size` from the cached
  `&self.icons[name][idx]`.
- Cache the "nearest size" filter per (icon, size) instead of redoing it per frame.

**Verify:** animated cursors (`wait`/`progress`) still animate; no allocation per frame
(heaptrack, or a counting allocator in a test).

### O6. One close-icon texture for all windows (S)

**Why:** `src/shell/ssd.rs:240–256` builds a `MemoryRenderBuffer` from the (cached) icon
pixels inside each window's `DecorationCache`. Every window uploads its own identical
texture.

**Change:** a thread-local cache keyed by (theme, scale, colour) holding the
`MemoryRenderBuffer` (it's reference-counted; clones share the texture). It's rebuilt
when the config or scale changes.

**Verify:** the icon still follows focus colour and scale changes.

### O7. Opaque regions for rounded windows (S–M)

**Why:** `RoundedElement::opaque_regions` returns nothing (`src/shaders.rs:230`). Smithay's
damage tracker therefore can't skip what's hidden behind any rounded window: windows
below, their shadows and the background all get drawn.

**Change:**
- When the inner element reports opaque regions and `alpha == 1.0`, return the inner
  regions intersected with the clip rectangle minus its four corner squares. As three
  rectangles: the middle band at full width, plus the top and bottom bands inset by the
  radius on each side.
- Ceil the radius in physical pixels, so anti-aliased corner pixels are never claimed.
- Rounded rects drawn by the bar shader (`PixelShaderElement`) already default to
  non-opaque. The bar could claim its middle as well; skip that unless P0 shows it
  matters.

**Verify:**
- Smithay's debug tint (`DEBUG_FLAGS`) shows windows behind a floating window no longer
  repainting when only the front one changes.
- P0 GPU time in scenario 2.
- No visual change at the corners.

### O8. Cheaper shadows (M, only if P0 says so)

**Why:** every decorated window draws a shadow quad as large as the window plus `reach`,
with an `exp()` per pixel (`SHADOW` in `src/shaders.rs`). O7 already removes the part
under the window itself.

**Change, if still needed:** render the shadow once per (corner radius, reach, colour,
scale) into a small 9-slice texture, and draw it as nine texture quads. Size changes then
cost nothing.

**Verify:** P0 GPU time with 10 floating windows; pixel-compare screenshots.

### O9. Nothing blocking on the event loop (S)

**Why:** three things block the compositor:
- `xrdb` runs through `sh -c … .status()` (`src/shell/x11.rs:139`). The compositor waits
  until xrdb has finished talking to Xwayland. That's a stall, and a deadlock risk if
  Xwayland is ever waiting on us at that moment.
- `fc-match` runs synchronously the first time a font is used (`src/ui/text.rs:147`). That
  happens inside rendering (`ssd::render_elements` → `with_ui_font`), and again after a
  font change in the config, so a frame is dropped each time.
- The first decorated window walks the icon theme and rasterises an SVG mid-render
  (`ui::icons::icon`).

**Change:**
- `xrdb`: `spawn()` and push the `Child` onto `self.children`. Log a non-zero exit from
  `reap_children`.
- Fonts: resolve the configured font in `apply_config` (path and `FontVec` loaded there,
  cached by pattern). The render path only reads the cache.
- Close icon: build it in `apply_config` and when the scale changes (together with O6).

**Verify:** P0 shows no frame-time spike on the first window or on a font change in the
config.

### O10. Capture: cheap readback, damage, regions (M)

**Why:** for every captured frame, `src/capture.rs`:
- builds a new offscreen texture and a new damage tracker, so it always draws everything,
- reads the result back (`copy_framebuffer` + `map_texture`),
- copies it again (`to_vec`, `:84`),
- converts every pixel's channel order in a loop (`:110–116`).

At 2560×1600@60 that's about 1 GB/s per copy, with three copies, and a GPU stall per
frame. Region captures (`slurp | grim -g`) still draw the whole output.

**Change:**
- Read back as `Fourcc::Argb8888`, so rows can be copied with one `copy_from_slice` per
  row. Write straight from the mapping into the shm buffer; no intermediate `Vec`.
- Keep one offscreen texture and one `OutputDamageTracker` per capture session (keyed by
  session, recreated when the size changes).
- Pass the damage to `Frame::success(transform, damage, time)`; it takes
  `Option<Vec<Rectangle>>`. For wlr `copy_with_damage`, send the damage too. Encoders then
  skip unchanged frames.
- wlr region captures: draw only the region, offsetting the elements by the region's
  origin into a region-sized texture.

**Verify:** P0 scenario 5 (CPU %, dropped frames); `grim` and `grim -g` produce
byte-identical images before and after.

### O11. Capture: GPU (dmabuf) buffers (L)

**Why:** only shm buffers are offered (`src/handlers/protocols.rs:258`,
`src/handlers/wlr_screencopy.rs:126`). OBS and portal screencasts through
xdg-desktop-portal-wlr prefer dmabuf. With dmabuf there are no CPU copies at all.

**Change:**
- Add the renderer's render formats and modifiers to `capture_constraints` (ext), and send
  `linux_dmabuf` + `buffer_done` (wlr).
- When the client's buffer is a dmabuf: import it, `bind` it as the render target, and draw
  with the session's damage tracker (from O10).
- Depends on O10's per-session structure.

**Verify:** `wf-recorder` with a dmabuf-capable build; OBS through the portal (after S1);
P0 scenario 5.

### O12. O(1) window lookups (S)

**Why:**
- One commit runs up to four linear scans over all windows:
  - `window_for_surface` when unmapping (`src/handlers/compositor.rs:38`),
  - the root lookup (`:49`),
  - `resize_grab::handle_commit` (`src/grabs/resize_grab.rs:245`),
  - the unmapped-window scan in `handle_xdg_commit` (`src/shell/xdg.rs:249`).
- X11: `window_for_x11` scans on every `configure_notify` (many per second while an X app
  is dragged), and twice in `configure_request`.

**Change:** store the `Window` in the root surface's `data_map` when it's mapped (the X11
surface too), and look it up there. Remove it on unmap. Alternatively, keep a
`HashMap<ObjectId, Window>` on the state, maintained in map/unmap.

**Verify:** unit test for map → lookup → unmap → lookup is `None`; P0 scenario 6.

### O13. Startup commands don't wait for Xwayland (S)

**Why:** startup commands (`-c` and `startup.commands`) run only on `XWaylandEvent::Ready`
(`src/shell/x11.rs:64`), so Xwayland's start time adds to the session's start time.

**Change:**
- Smithay's `XWayland` has `display_number()` straight after `spawn`. Set `DISPLAY` from
  it immediately and run the startup commands then.
- X clients that connect early wait in the socket queue until Xwayland accepts.
- `apply_xwayland_scale`, `update_x_resources` and the WM attach stay on `Ready`.

**Risk:** an X11 app started this way reads X resources before `xrdb` has run, so it gets
the wrong DPI. Only startup X11 apps with `native_scaling` are affected. If that matters,
hold back only commands marked X11 (not worth it by default).

**Verify:** time from login to the first terminal window; an X11 startup app still
appears.

### O14. Start Xwayland on first X client (M, later)

**Why:** sessions with no X11 apps still pay for an Xwayland process (tens of MB) and its
startup.

**Change:**
- Bind the X11 sockets ourselves (lock file, abstract and unix sockets) and watch them.
- Spawn Xwayland with `-listenfd` on the first connection.
- Smithay's `XWayland::spawn` binds its own sockets, so this needs either a small upstream
  change (spawn from existing sockets) or our own spawn path. Check upstream first.
- Behind `xwayland.lazy = true` until it has proven itself.

**Verify:** no Xwayland process until `xterm` starts; `xterm` still appears on the first
try.

### O15. Hidden windows marked suspended (S, with R3)

**Why:** windows on other workspaces get no frame callbacks (good), but Space still tells
them they're on the output. They never learn they're hidden, so browsers keep timers and
video decode running.

**Change:**
- Set `xdg_toplevel::State::Suspended` (Smithay supports it, xdg-shell v6) on windows of
  workspaces that aren't visible, and clear it when they come into view.
- With R3, hidden windows also get `wl_surface.leave`.

**Verify:** a playing YouTube tab on another workspace drops to near-zero CPU.

### O16. Alt-tab thumbnails live on every workspace (S)

**Why:** frame callbacks only go to windows whose workspace is in view
(`src/render.rs:524`). The switcher shows windows from every workspace, so those
thumbnails freeze on their last frame.

**Change:** while the switcher is visible, also send frame callbacks (and take
presentation feedback, `:565`) for the switcher's items.

**Verify:** a clock or `top` on another workspace keeps ticking in its alt-tab thumbnail.

### Considered and skipped

- **inotify instead of the 1 s config `stat`:** one `stat` per second costs nothing; not
  worth a dependency.
- **pidfd / SIGCHLD reaping:** same reasoning; `reap_children` piggybacks on that timer.
- **Mipmapped overview thumbnails:** they'd fix shimmering at 1/4 scale, but cost a copy
  per window per frame. Revisit only if someone notices.
- **Threaded rendering:** single-GPU and single-output today; there's nothing to overlap.

---

## 2. Code-quality refactors

### R1. Split `XrossState` (L, staged)

**Why:**
- 88 public fields (`src/state.rs`), and every module adds its own `impl XrossState`.
  Anything can touch anything.
- Borrow-checker workarounds like `let space = &self.space;` before closures are a symptom.
- Hardly anything outside `layout/` can be unit-tested.

**Target shape:**
```rust
pub struct XrossState {
    pub core: Core,           // display/loop handles, signal, start_time, socket_name
    pub proto: Protocols,     // every *_state field
    pub wm: Wm,               // space, unmapped, popups, workspaces (groups), camera,
                              // overview, switcher session, mru, closing, snap combo
    pub input: InputState,    // suppressed keys/buttons, mod_tap, switcher_hold,
                              // active_swipe, launch typing, deferred queue (R4)
    pub ui: UiCaches,         // osd/overview/switcher buffers, shaders, cursor, text (R6)
    pub backend: Backend,     // enum { Udev(SharedUdev), Winit(..) } — replaces two
                              // Options and the draw_cursor flag
    pub xwayland: Xwayland,   // wm, display, client, last dpi
    pub session: Session,     // config path/stamp, binds, overrides, children, prewarm
}
```

**Stages:**
1. Pure moves with no behaviour change: `Protocols`, `UiCaches`, `Backend` enum,
   `Xwayland`. (Milestone A.)
2. `Wm` as part of multi-monitor step 2, where it becomes `groups` and `monitors`.
   (Milestone C.)

**Verify:** `cargo nextest`, clippy, scripted nested scenarios; every stage is
behaviour-neutral.

### R2. One per-window data struct (S)

**Why:** per-window state lives in four separate `user_data` entries, each looked up
separately (often per frame):
- `WsTag` (`src/shell/workspace.rs:49–58`),
- `WinState` (`src/shell/snap.rs:89`),
- `DrawnWithSsd` (`src/shell/ssd.rs:74–84`),
- `RefCell<DecorationCache>` (`:150`).

The resize grab adds more. Each entry has its own `insert_if_missing` dance.

**Change:**
- `struct WindowData { workspace: Cell<Option<WsId>>, anim: WinState, drawn_ssd: Cell<bool>,
  deco: RefCell<DecorationCache>, resize: … }`.
- Read it with `fn data(w: &Window) -> &WindowData` (one `insert_if_missing`, in one place).

**Verify:** tests and clippy; no behaviour change.

### R3. Workspaces own their windows (L, with multi-monitor step 2)

**Why:** one `Space` holds every workspace's windows at real, overlapping positions.
- About ten loops filter with `workspace_of(w) == Some(ws)`.
- `has_windows` is a scan.
- Smithay's `element_under` can't be used, so hit-testing is our own.
- Hidden windows get `wl_surface.enter`.

**Change:**
- `Workspace { id, tiling, windows: Vec<Window> }` (stacking order, bottom to top).
- Only windows of visible workspaces are mapped in the `Space`. Workspace switches
  map/unmap, after the camera settles, as compaction already waits.
- The overview and the switcher draw from the workspace lists, not the Space.
- `has_windows` becomes `!ws.windows.is_empty()`.
- Watch out for X11 stacking (`raise_window`), focus on switch, MRU, and popups of hidden
  windows.

**Verify:** grid tests unchanged; scripted nested scenarios for switch, drag-drop in the
overview, and alt-tab across workspaces.

### R4. One queue for deferred actions (S–M)

**Why:** actions that change focus can't run inside the keyboard filter, so each feature
added a flag:
- `overview_toggle_pending`, `overview_close_pending`, `overview_focus_pending`,
- `switcher_commit_pending`, `pending_tap`,
- the arming at the end of `run_overview_pending`.

Ordering between them is implicit (the order of the `if`s in `on_key`'s tail and in
`run_overview_pending`).

**Change:**
- `enum Deferred { ToggleOverview, CloseOverview(Option<Coord>), FocusAfterOverview(Window),
  SwitcherCommit, Run(BoundAction), ArmLaunch }` and `self.input.deferred: Vec<Deferred>`.
- Drained in order, in one function, after keyboard and pointer handling.
- The lock case clears the queue (as `run_overview_pending` does today).

**Verify:** tests for tap → overview, Enter in the overview, switcher commit on modifier
release, and type-to-launch.

### R5. Keyboard filter as ordered stages (M)

**Why:**
- `on_key` (`src/input/mod.rs:170–288`) is one closure with nine stages whose order *is*
  the behaviour: lock, launch typing, switcher hold, switcher keys, taps, overview,
  releases, VT switch, binds.
- `vt_switch` is checked in three places.
- The order can only be tested through the whole compositor.

**Change:**
- Extract a `KeyCtx { keycode, state, mods, modified_sym, raw_sym, is_modifier }` from the
  `KeysymHandle` up front.
- Each stage is a `fn(&mut XrossState, &KeyCtx) -> Option<FilterResult<FilterAction>>`, run
  in a fixed array order. VT switch becomes one stage.
- Unit tests per stage, plus one test that pins the order.

**Verify:** existing launch-typing tests, new stage tests, and the scripted `xross msg key`
scenarios.

### R6. Shared title-text cache (S)

**Why:** rasterising text into a `MemoryRenderBuffer` and caching it is written twice:
`src/shell/ssd.rs:190–235` (per window) and `src/shell/switcher.rs:168–203` (its own
`HashMap`).

**Change:**
- `ui::text::TitleCache` keyed by (text, px, scale, colour, font), returning
  `(MemoryRenderBuffer, logical size, src rect)`.
- Entries unused for N frames are dropped.
- Used by both call sites.

**Verify:** titles in bars and in the switcher are pixel-identical.

### R7. `Animation2` and one idle-timer helper (S)

**Why:**
- Pairs of `Option<Animation>` with the same retarget/shift/done code appear in the camera
  (`src/shell/camera.rs`), the overview centre (`center_anim`) and the overview shifts.
- Three copies of "end the emulated gesture after N ms without events":
  `src/shell/camera.rs:166`, `src/shell/overview.rs:522` and `:617`.

**Change:**
- `struct Animation2 { x: Option<Animation>, y: Option<Animation> }` with `value(now, rest)`,
  `retarget`, `shift`, `tick`, `is_moving`.
- `fn arm_idle_timer(&self, limit, last_event: fn(&XrossState) -> Option<Duration>,
  on_idle: fn(&mut XrossState))`.

**Verify:** tests; scroll-emulated swipes nested still end and snap.

### R8. Shared tile transform (S)

**Why:** live windows (`src/render.rs:325`) and closing windows (`:383`) repeat the same
"rect on its workspace → rect inside the drawn tile" maths, and the same open/close scale
maths around a centre.

**Change:** `fn into_tile(rect, tile, s, output_loc) -> Rectangle` and
`fn scale_about_centre(rect, k)`.

### R9. IPC message enum (S)

**Why:** `src/ipc.rs` parses by string prefix in two places. The server has one chain;
the client keeps a separate list of prefixes it lets through.

**Change:**
- `enum Msg { Action(Action), Spawn(String), Screenshot(PathBuf), Pointer(f64, f64),
  Key { code: u32, states: KeyStates } }` with `FromStr` and `Display`.
- The client parses (validating and making the screenshot path absolute) and sends
  `to_string()`. The server parses once and matches.
- Unit tests for round-trips.

### R10. Config passed in, not a global (M, with multi-monitor config)

**Why:**
- `config::current()` is a thread-local `Rc<Config>` used at 30 call sites. It's a hidden
  input to rendering and layout.
- Per-monitor settings (scale, mode) need per-output values, which a single global can't
  express cleanly.

**Change:**
- `state.config: Rc<Config>` plus the resolved values from O4.
- Free functions such as `switch_spring()` take `&Config`.
- Do it while adding `[monitor."…"]`.

### R11. Child environment without `set_var` (S)

**Why:** `unsafe { std::env::set_var }` appears in:
- `src/main.rs` (`WAYLAND_DISPLAY`, `XDG_*`),
- `src/shell/x11.rs` (`DISPLAY`),
- `src/cursor.rs` (`XCURSOR_PATH`, `XCURSOR_THEME`),
- `src/ipc.rs:99` (`XROSS_SOCKET`).

Mutating the process environment is unsound once any thread exists (Smithay and Mesa may
start some).

**Change:**
- `state.child_env: Vec<(OsString, OsString)>`, applied in `spawn()` with `.envs(…)` and
  passed to `XWayland::spawn` (which takes an env iterator).
- `dbus-update-activation-environment` is spawned the same way, so it still sees the
  values.

**Verify:** `env` from a terminal started by xross shows all of them; X11 apps get
`DISPLAY`.

### R12. Small cleanups (S)

- Remove the crate-wide `#![allow(irrefutable_let_patterns)]` (`src/main.rs:1`) and fix the
  few sites it hides.
- `src/state.rs`: imports and type aliases instead of fully qualified paths in fields (e.g.
  `std::rc::Rc<std::cell::RefCell<smithay::backend::winit::WinitGraphicsBackend<…>>>`).
- `src/shell/ssd.rs`: the `TitleText` doc comment has a duplicated, garbled line
  (`(title, scale, focused, "font\")`).
- `output_rect()` → `output_rect(&Output)`; this comes with multi-monitor anyway.

### R13. Split the four largest modules (M)

- `src/shell/overview.rs` (699) → `overview/{model.rs, view.rs, input.rs}`:
  - model: open/close, selection, pending actions,
  - view: `Frame`, `tile_rect`, views and slots,
  - input: buttons, drag, pan, gestures.
- `src/config.rs` (654) → `config/{schema.rs, load.rs, apply.rs}`:
  - schema: the serde types and defaults,
  - load: layering, the read-only default copy, the watch timer,
  - apply: `apply_config`, scale and binds.
- `src/input/mod.rs` (608) → `input/{keyboard.rs, pointer.rs, gesture.rs}`, with R5's
  stages in `keyboard.rs`.
- `src/render.rs` (576): leave it until O2/R8 shrink it; then decide.

### R14. Headless state tests (M)

**Why:** the overview, switcher, taps and launch typing are state machines tested only by
scripted nested runs, which need a display.

**Change:**
- Make `XrossState::new` work without a listening socket (an option to skip
  `ListeningSocketSource`) and without a backend. The renderer is only needed to render.
- Tests drive `on_key`, `overview_button`, `run_action` and assert on state:
  - tap Super → overview open,
  - Enter → closed on the selected workspace,
  - Alt-tab hold and release → focus moved,
  - type in overview → keys recorded.
- Windows can be faked with test clients through `wayland-client` in-process, or by
  testing below the `Window` level. Pick whichever is less code; try in-process clients
  first.

---

## 3. Scope

### S1. Portals: stay out, configure from the NixOS module (S)

xross shouldn't implement any portal backend. It already exports its environment to
systemd and D-Bus in `--session` mode. What's missing is picking backends.
`xdg-desktop-portal` reads `xross-portals.conf`, chosen by `XDG_CURRENT_DESKTOP=xross`.

Add an option to the flake's NixOS module (this is xross's module, not your system
config):

```nix
programs.xross.portals.enable = true;   # default true
# sets:
xdg.portal = {
  enable = true;
  extraPortals = [ pkgs.xdg-desktop-portal-gtk pkgs.xdg-desktop-portal-wlr ];
  config.xross = {
    default = [ "gtk" ];
    "org.freedesktop.impl.portal.ScreenCast" = [ "wlr" ];
    "org.freedesktop.impl.portal.Screenshot" = [ "wlr" ];
  };
};
```

**Verify:**
- `busctl --user tree org.freedesktop.portal.Desktop`.
- A file chooser from Firefox.
- A screen share in a browser (this will exercise O10/O11).

Not xross's job either, but worth a README line: a polkit agent and a keyring come from
the user's config.

### S2. Close icon drawn by shader; drop resvg and `icons.rs` (S–M)

**Why:** `src/ui/icons.rs` (243 lines: theme index parsing, inheritance, SVG/PNG loading)
plus the `resvg` dependency exist for one icon, the title bar's close button.

**Change:**
- Draw the × as a signed-distance shape in a small pixel shader: two rotated rounded bars,
  in the existing `ROUNDED_RECT` style.
- Remove `icons.rs`, the `resvg` dependency and `appearance.icon_theme`. Keep the
  deprecated key accepted with a warning.
- `png` stays (`src/screenshot.rs` uses it).

**Trade-off:** the icon no longer matches the icon theme. Alternatively, keep the theme
path behind a cargo feature.

**Verify:** release binary size and clean build time before and after (`cargo build
--release --timings`); the hover state looks the same.

### S3. Retire wlr-screencopy when tools allow (S, later)

`src/handlers/wlr_screencopy.rs` is our own implementation, because Smithay has none.
Current grim and xdg-desktop-portal-wlr can use ext-image-copy-capture. Before removing
it:
- check the versions in your nixpkgs channel,
- check wf-recorder (which may still be wlr-only).

Keep it until all three work without it.

### S4. Test-only IPC behind a flag (S)

**Why:** `xross msg key|key-down|key-up|pointer` (`src/ipc.rs:49–80`) lets any process of
the user inject input into any window. That bypasses Wayland's input isolation (it's
xdotool for free). Lock still holds, because the lock stage runs first, but it's still a
hole. `screenshot` is also test-only (nested).

**Change:**
- Accept these only when xross was started with `--test-ipc`; the devenv `nested` script
  passes it.
- Otherwise log "test command disabled" and ignore.
- Actions and `spawn` stay. `spawn` hands out an activation token, which means any local
  process can raise a window. Acceptable; note it in the README.

### S5. Type-to-launch: keep, pin its contract (S)

`src/input/launch.rs` (362 lines) is the most bespoke code per feature:
- process groups,
- matching layer surfaces by pgid,
- replaying keys through `KeyboardHandle::input` vs `input_forward`,
- relying on Smithay ignoring a repeated press of a held key.

Keep it, and:
- State the contract at the top of the module: what launchers must do (open a keyboard
  layer surface in the spawned process group, within 3 s).
- Add a test that pins the Smithay behaviour it relies on (a press of an already-held key
  is absorbed). A Smithay bump that changes it then fails loudly.
- Revisit on every Smithay rev bump.

### S6. Things xross should not grow

These belong in separate clients over the protocols xross already speaks:
- bar or panel, notifications, wallpaper (layer-shell),
- screen locker (session-lock), launcher,
- portal backends (S1),
- output config GUI (wlr-output-management, F2),
- clipboard history (data-control),
- idle daemon (idle-notify),
- screenshot UI (grim/slurp).

**Size check:** about 11.7k lines including tests is lean for this feature set. The goal
is to keep the four large modules from growing (R13), not to shrink the total.

---

## 4. Future work to note

### F1. Planned

- **Multi-monitor:** steps 1–3 in the multi-monitor plan. Absorbs R1 stage 2, R3, R10,
  O15 and `output_rect(&Output)`.

### F2. Protocols not yet offered

"In Smithay" means Smithay already implements it at our rev, so it's mostly wiring (S).

| Protocol | Who needs it | In Smithay | Priority |
|---|---|---|---|
| pointer-constraints + relative-pointer | games, Blender, VMs | yes | high |
| pointer-gestures | pinch-zoom in Firefox, image viewers | yes | high |
| linux-drm-syncobj (explicit sync) | NVIDIA proprietary driver | yes | high if NVIDIA |
| text-input v3 + input-method v2 | IMEs (fcitx5, CJK input) | yes | medium |
| keyboard-shortcuts-inhibit | VMs, remote desktop (needs an escape bind) | yes | medium |
| xdg-foreign | portal dialogs parented to apps (after S1) | yes | medium |
| security-context | Flatpak: hide privileged protocols from sandboxes | yes | medium |
| ext-foreign-toplevel-list | taskbars, window lists | yes | medium |
| wlr-foreign-toplevel-management | waybar's taskbar (activate/close) | no | medium |
| wlr-gamma-control | gammastep, wlsunset (night light) | no | medium |
| wlr-output-management | kanshi, wdisplays (after multi-monitor) | no | medium |
| virtual-keyboard, virtual-pointer | wayvnc, on-screen keyboards | yes | low (security: gate like S4) |
| xwayland-keyboard-grab | X11 VMs and games grabbing the keyboard | yes | low |
| single-pixel-buffer, alpha-modifier | newer toolkits, mpv | yes | low |
| content-type, fifo, commit-timing | games, video players, tearing control | partly | low |
| tablet v2 | drawing tablets | yes | low |
| xdg-toplevel-icon, xdg-system-bell, xdg-toplevel-tag | icons in alt-tab, bells | yes | low |
| drm-lease | VR headsets | yes | low |

### F3. Backend

- **Multi-GPU:** outputs wired to a second GPU (some laptops). Known limit in the
  multi-monitor plan.
- **VRR / adaptive sync:** a per-monitor config option once monitors have config.
- **Direct scanout:** fullscreen windows aren't wrapped in `RoundedElement`, and the
  crop/rescale wrappers forward `underlying_storage`, so scanout should already work.
  Confirm with DRM debug logging during fullscreen mpv, and document it.
- HDR and 10-bit output: out of scope for now.

### F4. XWayland

- O14 (lazy start).
- **Evaluate xwayland-satellite:**
  - Pro: X11 apps become plain Wayland clients, which deletes the X11 WM half of
    `src/shell/x11.rs`, X11 branches in decorations, focus and close, and override-redirect
    handling.
  - Con: we lose `native_scaling`/`Xft.dpi` control; it's an extra process to package.
- **Xwayland crash recovery:** today `disconnected` drops the WM and X11 apps are gone
  until a restart. Respawn it, at least once.
- With multi-monitor, X11 apps follow the primary monitor's scale (Xwayland has one scale).
  Document it.

### F5. Robustness

- **GPU context loss:** recreate the renderer and textures instead of logging errors
  forever.
- **On exit:** SIGTERM the process groups of children we started (the launcher prewarm
  already does this), so a crashed session doesn't leave orphans.
- **`xross check-config [PATH]`:** parse, print problems and the bind table, exit non-zero
  on errors. Useful while editing.

### F6. CI and testing

- **`nix flake check`:** build the package, run `cargo nextest` and clippy (`-D warnings`)
  in a check derivation.
- **GitHub Actions running `nix flake check`** on PRs (the repo has a GitHub remote).
- R14 headless tests, then scripted nested scenarios in CI under a headless host
  (`weston --backend=headless`, or cage), if that proves stable.

---

## 5. Sequence

Refactors that touch the same code as multi-monitor are scheduled inside it, to avoid
rewriting that code twice.

**Milestone A: quick wins, before multi-monitor.** Each line is one PR (or one commit,
reviewed separately):
1. P0 instrumentation, plus baseline numbers.
2. O1 redraw on demand.
3. O2 + O3 per-frame context and hover.
4. O4 + O5 + O6 allocation fixes.
5. O7 opaque regions.
6. O9 + O13 nothing blocking; startup commands at spawn.
7. O12 lookups, O16 live thumbnails.
8. R2, R7, R8, R9, R11, R12: mechanical cleanups.
9. R1 stage 1: protocol, UI, backend and Xwayland sub-structs.
10. S1 portals in the NixOS module, S4 test IPC flag, S5 contract test.

**Milestone B:** multi-monitor step 1 (DRM multi-CRTC, monitor config, mirror).

**Milestone C:** multi-monitor step 2 (separate grids). It includes R1 stage 2 (`Wm` →
groups), R3, R4, R5, R10 and O15. Then step 3 (span).

**Milestone D:**
- O10 then O11 (capture).
- S2 (close icon by shader).
- R6, R13, R14.

**Milestone E:** future work by need. Probably first: pointer constraints, relative
pointer and gestures (F2, all in Smithay), then syncobj if NVIDIA matters, then
gamma-control.
