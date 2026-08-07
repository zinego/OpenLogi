# Multi-button Gesture Parity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Match Logi Options+ with Forward Pan and Back Window Navigation active simultaneously in the GUI and on a real M650.

**Architecture:** Per-button `Binding::{Single, Gesture, Pan}` is the runtime source of truth. Schema-v3 `gesture_owner` is consumed by schema-v4 normalization, agent projections collect every typed binding, and GUI editors carry an explicit `ButtonId`. Physical evidence selects the Pan input repair.

**Tech Stack:** Rust 2024, serde/TOML, GPUI, CGEventTap, tarpc IPC, cargo, GitHub Actions.

---

### Task 1: Normalize legacy single-owner config

**Files:**
- Modify: `crates/openlogi-core/src/config.rs`
- Modify: `crates/openlogi-core/src/config/device.rs`
- Modify: `crates/openlogi-core/src/config/settings.rs`
- Modify: `crates/openlogi-core/src/lib.rs`
- Modify: `docs/CONFIGURATION.md`

- [ ] **Step 1: Write failing migration tests**

Load schema 3 with owner Forward, Forward Pan, and dormant Back Gesture. Assert Forward stays Pan, Back becomes `Single(Click)`, schema becomes 4, and saved TOML omits `gesture_owner`. Cover owner Off, invalid owner, absent dedicated button, and schema-4 Forward Pan plus Back Window Navigation round trip.

- [ ] **Step 2: Verify RED**

Run `cargo test -p openlogi-core gesture_owner -- --nocapture`. Expected: old scalar owner remains live and serialized.

- [ ] **Step 3: Implement private legacy normalization**

```rust
enum LegacyGestureOwner { Off, Button(ButtonId) }

fn normalize_legacy_owner(
    bindings: &mut BTreeMap<ButtonId, Binding>,
    owner: Option<LegacyGestureOwner>,
) {
    for (button, binding) in bindings.iter_mut() {
        let active = matches!(owner, Some(LegacyGestureOwner::Button(id)) if id == *button);
        if binding.is_gesture() && !active {
            *binding = Binding::Single(binding.click_action_or(default_binding(*button)));
        }
    }
}
```

Keep legacy decoding private to schema 3. Remove public/live owner APIs, bump the schema to 4, and document independent per-button bindings.

- [ ] **Step 4: Verify GREEN**

Run `cargo test -p openlogi-core`, `cargo clippy -p openlogi-core --all-targets -- -D warnings`, and fmt check.

- [ ] **Step 5: Commit**

Commit `refactor(core): model gestures per button`.

### Task 2: Project and dispatch every gesture binding

**Files:**
- Modify: `crates/openlogi-agent-core/src/bindings.rs`
- Modify: `crates/openlogi-agent-core/src/hook_runtime.rs`
- Modify: `crates/openlogi-agent-core/src/orchestrator.rs`
- Modify: `crates/openlogi-agent-core/src/watchers/gesture.rs`

- [ ] **Step 1: Write failing tests**

Assert `oshook_gestures_for` returns Back Window Navigation and Forward Pan together; per-app Single drops only one; the dedicated HID gesture can coexist. Assert Back press then Forward press cancels Back without a click and only Forward consumes later motion/release.

- [ ] **Step 2: Verify RED**

Run focused `oshook_gestures` and `overlapping_gesture` tests. Expected: projection contains only one owner.

- [ ] **Step 3: Implement binding-driven projection**

```rust
config.effective_bindings(key, app_bundle)
    .into_iter()
    .filter(|(button, _)| button.is_os_hook_button())
    .filter_map(|(button, binding)| gesture_mode(binding, false).map(|mode| (button, mode)))
    .collect()
```

Make `hid_gesture_for` classify the dedicated button independently. Make `HoldState::begin` cancel a different active button before installing the new hold.

- [ ] **Step 4: Verify GREEN and commit**

Run agent-core/agent tests and all-target clippy, then commit `feat(agent): dispatch gestures per button`.

### Task 3: Configure gestures independently in the GUI

**Files:**
- Modify: `crates/openlogi-gui/src/state.rs`
- Modify: `crates/openlogi-gui/src/mouse_model/view.rs`
- Modify: `crates/openlogi-gui/src/mouse_model/picker.rs`
- Modify: `crates/openlogi-gui/src/gesture_presets.rs`
- Modify: `crates/openlogi-gui/locales/*.yml`

- [ ] **Step 1: Write failing pure-state tests**

Applying Pan to Forward and Window Navigation to Back must preserve both bindings. Changing Back must not mutate Forward; per-app replacement affects one button; labels classify independently.

- [ ] **Step 2: Verify RED with the standalone pure-module harness**

Expected: current helpers implicitly target one owner and cannot preserve both modes.

- [ ] **Step 3: Add explicit-button state APIs**

```rust
fn current_complete_binding(&self, button: ButtonId) -> Option<Binding>;
fn commit_complete_binding(&mut self, button: ButtonId, binding: Binding);
fn commit_gesture_preset(&mut self, button: ButtonId, preset: GesturePreset);
fn commit_pan_click(&mut self, button: ButtonId, action: Action);
fn commit_gesture_direction(
    &mut self,
    button: ButtonId,
    direction: GestureDirection,
    action: Action,
);
```

Delete the owner selector and owner-based rendering. Every card classifies its complete binding and opens a picker carrying its `ButtonId`. Ordinary actions and Window Navigation, Pan, and Custom remain reachable per card.

- [ ] **Step 4: Verify and commit**

Run the pure state/preset harness, 20-locale ordered parity, fmt, and diff check. Use ARM64 CI for the full GPUI build when local Metal is unavailable. Commit `feat(gui): configure gestures per button`.

### Task 4: Diagnose and fix real M650 Pan input

**Files:**
- Modify: `crates/openlogi-hook/src/macos.rs` only when raw-event evidence proves a translation defect
- Modify: `crates/openlogi-agent-core/src/hook_runtime.rs` only when live-map evidence proves a runtime defect
- Test: temporary listen-only macOS probe, not committed

- [ ] **Step 1: Capture physical evidence**

Run a HeadInsert listen-only tap logging event type, button number, delta X/Y, location, and user-data while polling OpenLogi's event monitor. Capture a real Forward click and hold/move/release.

- [ ] **Step 2: Choose only the evidence-backed repair**

- No down/up: inspect the M650 Forward CID `getCidReporting` and clear stale diversion/remap.
- Wrong button number: correct the device/model mapping from the captured value.
- Button 4 plus motion but no hold: fix live HookMaps/reload.
- Zero deltas with changing location: derive physical motion from successive locations.

- [ ] **Step 3: TDD the observed event shape**

Add a regression through `translate` and `handle_event`; prove RED, implement the minimal repair, prove GREEN, and preserve the feedback-loop regression.

- [ ] **Step 4: Verify and commit**

Run hook/agent tests, all-target clippy, and Linux target test check. Commit `fix(hook): handle physical m650 pan input` only if code changes are required.

### Task 5: Build and accept the actual installed product

**Files:**
- Runtime config: `~/.config/openlogi/config.toml`
- Installed app: `/Applications/OpenLogi.app`

- [ ] **Step 1: Push normally and build the exact SHA**

Run related source gates, push without force, trigger `Build sign=false`, and verify the ARM64 artifact head SHA, checksum, architecture, bundle version, and nested Agent.

- [ ] **Step 2: Install recoverably and reauthorize**

Back up the existing app, install/ad-hoc sign the new bundle, re-register `org.openlogi.agent` only if its requirement changed, and launch GUI plus Agent as PPID 1.

- [ ] **Step 3: Configure through the GUI**

Set Forward Pan, Back Window Navigation, and Middle Mission Control. Read TOML back and prove both complete bindings coexist without `gesture_owner`.

- [ ] **Step 4: Run installed-binary automation**

Verify exact two-axis Pan output without feedback, fixed pointer, Smart Zoom click, canonical Back window actions, unchanged wheel, stable config, and restart persistence.

- [ ] **Step 5: Run real M650 acceptance**

Verify Forward hold+move pans the foreground webpage with pointer fixed; Forward click Smart Zooms; Back Left/Right/Up/Down/Click performs Window Navigation; Middle opens Mission Control; wheel remains native. Do not mark complete before these pass.
