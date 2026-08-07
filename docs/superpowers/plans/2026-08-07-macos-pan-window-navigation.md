# macOS Pan and Window Navigation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement macOS Pan and Window Navigation gesture presets with behavior matching the recorded Logi Options+ reference.

**Architecture:** Add a typed continuous Pan binding and keep Window Navigation as an atomic preset over existing actions. Route HID++ and OS-hook gesture lifecycles through agent-core, coalesce Pan deltas off the event-tap thread, and inject two-axis macOS scroll plus Smart Magnify events.

**Tech Stack:** Rust 2024, serde/TOML, HID++ raw XY, CGEventTap/CoreGraphics, GPUI, tarpc/bincode.

---

### Task 1: Typed Pan binding and compatible configuration

**Files:**
- Modify: `crates/openlogi-core/src/binding.rs`
- Modify: `crates/openlogi-core/src/config/device.rs`
- Modify: `crates/openlogi-core/src/config.rs`

- [ ] Add failing tests proving `Binding::Pan` round-trips as `{ Pan = { click = "SmartZoom" } }`, old single/directional bindings remain byte-for-byte stable, and legacy per-app `Action` entries load as `Binding::Single`.
- [ ] Run `cargo test -p openlogi-core binding config` and confirm the new tests fail because Pan and SmartZoom are absent.
- [ ] Append `Action::SmartZoom`, add `PanBinding { click: Action }`, add an unambiguous Pan envelope to `Binding`, and change per-app binding values from `Action` to `Binding` without changing old serialization.
- [ ] Add helpers that build and recognize the exact Window Navigation map and default Pan binding.
- [ ] Run `cargo test -p openlogi-core` and confirm all core tests pass.

### Task 2: Shared Pan state machine

**Files:**
- Create: `crates/openlogi-core/src/binding/pan.rs`
- Modify: `crates/openlogi-core/src/binding.rs`

- [ ] Add failing tests for click-only release, jitter below deadzone, buffered activation delta, subsequent two-axis deltas, release after activation, saturation, and interruption cancellation.
- [ ] Run the focused Pan tests and confirm they fail because `PanAccumulator` does not exist.
- [ ] Implement `PanAccumulator` with `begin`, `accumulate`, `end`, and `cancel`, returning typed `PanOutput::{Idle, Delta { x, y }, Click, End}` values.
- [ ] Run the focused tests, then `cargo test -p openlogi-core`.

### Task 3: Raw HID++ gesture lifecycle

**Files:**
- Modify: `crates/openlogi-hid/src/gesture.rs`
- Modify: `crates/openlogi-hid/src/reprog_controls/event.rs`

- [ ] Add failing tests for `Pressed -> Motion -> Released`, motion while idle, release loss, and session shutdown cancellation while preserving signed raw XY.
- [ ] Run `cargo test -p openlogi-hid gesture` and verify failures are caused by the missing lifecycle variants.
- [ ] Replace HID-local policy dispatch with lifecycle events consumed by agent-core while leaving button and thumbwheel capture unchanged.
- [ ] Run `cargo test -p openlogi-hid`.

### Task 4: macOS continuous injection and Smart Zoom

**Files:**
- Modify: `crates/openlogi-inject/src/inject.rs`
- Modify: `crates/openlogi-inject/src/inject/macos.rs`

- [ ] Add tests around pure axis/sign conversion and the Smart Zoom action dispatch arm.
- [ ] Run focused inject tests and confirm they fail for the missing API/action.
- [ ] Add `post_pan_scroll(delta_x, delta_y)` using one pixel-unit two-axis CG scroll event stamped with `SYNTHETIC_EVENT_USER_DATA`.
- [ ] Add macOS Smart Magnify posting at the current cursor position; non-macOS dispatch fails closed without pretending support.
- [ ] Run `cargo test -p openlogi-inject`.

### Task 5: Agent-core gesture policy and hook integration

**Files:**
- Modify: `crates/openlogi-agent-core/src/bindings.rs`
- Modify: `crates/openlogi-agent-core/src/hook_runtime.rs`
- Modify: `crates/openlogi-agent-core/src/orchestrator.rs`
- Modify: `crates/openlogi-agent-core/src/watchers/gesture.rs`
- Modify: `crates/openlogi-agent/src/main.rs`
- Modify: `crates/openlogi-agent/src/pairing.rs`

- [ ] Add failing tests for Pan projection, OS-hook movement suppression, click fallback, directional-regression behavior, HID Pan lifecycle, application overlay selection, interruption cancellation, and bounded delta coalescing.
- [ ] Run focused agent-core tests and verify the intended failures.
- [ ] Publish typed gesture modes atomically with single-action maps and make application overlays select a whole `Binding`.
- [ ] Add a non-blocking coalescing Pan emitter and route both OS-hook and HID lifecycle input through the shared Pan/Directional policy.
- [ ] Cancel active state on reload, target/session changes, disconnect, and `CaptureInterrupted`.
- [ ] Run `cargo test -p openlogi-agent-core -p openlogi-agent`.

### Task 6: GPUI presets and locale parity

**Files:**
- Modify: `crates/openlogi-gui/src/mouse_model/picker.rs`
- Modify: `crates/openlogi-gui/src/state.rs`
- Modify: `crates/openlogi-gui/src/i18n.rs`
- Modify: `crates/openlogi-gui/locales/*.yml`

- [ ] Add failing state tests for atomically applying Window Navigation and Pan globally and per app, recognizing Custom, and hiding Pan outside macOS.
- [ ] Run focused GUI state and i18n tests and confirm the intended failures.
- [ ] Add the preset selector, Pan summary/click editor, single-save state mutations, and macOS gating using existing GPUI patterns.
- [ ] Insert every English key at the same ordered location in all 20 locales and use existing sibling terminology for translations.
- [ ] Run `cargo test -p openlogi-gui i18n` plus focused GUI state tests.

### Task 7: IPC, documentation, and full verification

**Files:**
- Modify if required: `crates/openlogi-agent-core/src/ipc.rs`
- Modify if required: `crates/openlogi-agent-core/tests/wire_format.rs`
- Modify: `README.md`

- [ ] Run wire-format tests; if bytes changed, append-only update the protocol version and regenerate only the affected golden fixtures.
- [ ] Document the macOS Pan and Window Navigation capability and the Options+ coexistence/testing boundary.
- [ ] Run `cargo fmt --all -- --check`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace`.
- [ ] Run the M650 hardware matrix from the design spec and record every observed and unverified item honestly.
- [ ] Review the complete diff against the design, split focused conventional commits if needed, and push `feat/macos-gesture-presets` to `origin` without force.
