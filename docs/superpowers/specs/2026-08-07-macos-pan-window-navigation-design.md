# macOS Pan and Window Navigation Gesture Design

## Goal

Match the observed Logi Options+ behavior for the `Pan` and `Window Navigation`
gesture presets on macOS. The first hardware target is a Signature M650 connected
directly over Bluetooth. Linux and Windows behavior is outside this change.

## Observed Options+ Behavior

The behavior was recorded from Logi Options+ with OpenLogi stopped so the two
agents did not compete for HID++ access.

### Pan

- Holding the gesture button freezes the pointer.
- Pointer motion becomes continuous two-axis content movement.
- Content moves in the same direction as the physical mouse.
- Releasing the button ends the pan immediately.
- Clicking without moving performs browser Smart Zoom.

### Window Navigation

- Left: previous desktop.
- Right: next desktop.
- Up: Mission Control.
- Down: App Expose.
- Click: Mission Control.

## Configuration Model

`Binding` gains a typed `Pan` variant alongside the existing `Single` and
`Gesture` variants. Pan is a hold-lifecycle mode, not an `Action`: representing it
as four repeated actions would lose continuous motion and release semantics.

The TOML shape is intentionally distinct from both an externally tagged `Action`
and a direction-keyed gesture map:

```toml
[devices."device-key".bindings.Back.Pan]
click = "SmartZoom"
```

Existing single actions and direction maps continue to deserialize unchanged.
Per-application bindings store `Binding` rather than `Action`; old application
overlays deserialize as `Binding::Single` and retain their current serialized
form.

`SmartZoom` is appended to `Action`. Any enum or service shape that crosses the
bincode/tarpc boundary remains append-only; if the golden wire bytes change,
`PROTOCOL_VERSION` and all affected golden fixtures are updated together.

## Runtime Architecture

HID++ reports expose gesture-button press, signed raw XY motion, and release as a
lifecycle. The HID crate transports those events without consulting config.
`openlogi-agent-core` selects one of two policies from the active binding:

- `Gesture`: feed motion into the existing directional swipe accumulator and
  dispatch one action.
- `Pan`: feed motion into a dedicated pan state machine and continuously emit
  two-axis deltas until release or cancellation.

OS-hook gesture buttons use the same policy. During Pan, their original mouse
movement is suppressed so the cursor remains fixed. A dedicated HID++ gesture
button already diverts raw XY at the device, so its pointer does not move.

Pan uses a small movement deadzone to distinguish an intentional click from
sensor jitter. Motion is buffered inside the deadzone; when Pan activates, the
buffered delta is emitted so the beginning of the gesture is not lost. A release
before activation dispatches the configured click action. A release after
activation dispatches no click.

Configuration reload, application focus change, device disconnect, capture
restart, and event-tap interruption cancel the active gesture. Cancellation never
dispatches the click fallback.

## macOS Injection

The event-tap callback never performs synchronous high-frequency injection. It
adds signed deltas to atomic accumulators and signals a bounded worker. The worker
coalesces pending deltas and posts one pixel-unit CG scroll event containing both
axes. Queue saturation therefore coalesces motion instead of blocking the tap or
losing direction.

The injected event is stamped with `SYNTHETIC_EVENT_USER_DATA`, preserving the
existing re-entry guard. Axis signs are chosen so content follows physical mouse
motion, matching the observed Options+ behavior.

Smart Zoom posts the macOS Smart Magnify event at the current pointer location.
It must be manually accepted in both Edge and Safari as adaptive page zoom and
restore; system Accessibility zoom is not an acceptable substitute.

## GUI

The gesture editor gains `Window Navigation`, `Pan`, and `Custom` modes on macOS.

- Window Navigation atomically installs the five observed actions. Editing any
  direction changes the displayed mode to Custom.
- Pan shows four continuous Pan directions and an editable click action whose
  default is Smart Zoom.
- Global and per-application editors use the same binding model.
- Pan is hidden on non-macOS targets rather than displayed as a non-working
  option.

Applying a preset performs one config save and one agent reload. English strings
are inserted at the same ordered position in every locale; existing terminology
is reused for translations.

## Failure Behavior

- Missing Accessibility permission leaves OS-hook Pan unavailable without
  crashing or swallowing clicks.
- Unsupported devices do not advertise a gesture mode they cannot capture.
- A poisoned config/runtime lock passes input through rather than freezing it.
- Synthetic events never re-enter gesture recognition.
- An interrupted or stale hold is cancelled and cannot commit a later phantom
  pan or click.

## Verification

Automated coverage includes:

- old and new TOML round trips, including per-app legacy actions;
- Pan deadzone, buffered first motion, two-axis output, release, and cancellation;
- directional gestures remaining unchanged;
- HID++ press/motion/release lifecycle;
- hook suppression only while Pan is active;
- coalescing without sign loss;
- locale parity and IPC golden bytes.

The local gate is:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Hardware acceptance on the Bluetooth M650 covers pointer freeze, four directions,
diagonal Pan, slow and fast movement, release, Smart Zoom in Edge and Safari,
Window Navigation, global/per-app switching, and interruption recovery. Automated
tests are not reported as proof of hardware equivalence.
