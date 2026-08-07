# Multi-button gesture parity with Logi Options+

## Outcome

OpenLogi must let every gesture-capable mouse button own its binding independently. On the test
Signature M650 L, Forward is configured as Pan, Back as Window Navigation, and Middle Click as
Mission Control at the same time. The GUI and the physical mouse behavior must agree.

## Product behavior

- Remove the device-wide single-select "Gesture Button" control.
- A Middle, Back, Forward, or dedicated HID++ gesture-button card opens the same binding picker.
- The picker offers ordinary actions plus the gesture presets Window Navigation, Pan, and Custom.
- Pan holds the pointer still, converts physical XY travel into continuous two-axis pixel scrolling,
  and performs Smart Zoom only when released inside the deadzone.
- Window Navigation uses the canonical macOS map: Left/Right switch desktops, Up opens Mission
  Control, Down opens App Expose, and Click opens Mission Control.
- Two different buttons may be configured with different gesture presets simultaneously. If a user
  physically overlaps two holds, the newest press cancels the previous hold without firing its click.

## Data model and migration

`Binding` is the source of truth per button: `Single`, `Gesture`, or `Pan`. Runtime projections select
all effective per-button Gesture/Pan bindings rather than filtering through one `gesture_owner`.
Per-app bindings continue to replace the whole button binding.

Existing schema-v3 files remain readable. One-time normalization consumes a legacy explicit
`gesture_owner`: its selected button keeps its Gesture/Pan binding, while dormant Gesture/Pan maps on
non-owners are demoted to `Single` using their Click action (or the button's native default when Click
is absent). `Off` demotes every dormant map. This preserves the behavior that was live before upgrade
without accidentally activating previously hidden gesture maps. New writes omit the obsolete owner
field once normalization completes. Ordinary button mappings are never silently promoted except for
the explicit M650 test configuration chosen by the user.

## Runtime and physical input

The OS-hook projection includes every Middle/Back/Forward effective Gesture/Pan binding. The HID++
projection independently reads the dedicated gesture button. Pointer motion accepts MouseMoved and
all dragged event types. Physical verification must record the actual M650 button-down, motion delta,
and button-up chain; synthetic CGEvent success alone is insufficient evidence.

## GUI

Every rendered card classifies its complete `Binding`. Gesture/Pan cards open the gesture preset
editor; Single cards open the ordinary action picker, with an entry to promote that specific button
to a preset. Changing one card atomically replaces only that button's complete binding, persists once,
and reloads the agent once. Labels show `Pan`, `Window Navigation`, or `Custom`, never a device-wide
owner state.

## Verification

- Core migration and round-trip tests cover simultaneous Forward Pan + Back Window Navigation.
- Agent-core tests project and dispatch both buttons concurrently, including per-app overlays and
  overlapping-hold cancellation.
- GUI pure-state tests prove one card changes without demoting another and labels classify both.
- macOS hook tests cover the physical motion event shapes and feedback prevention.
- Installed ARM64 app verification runs with the PPID-1 bundled agent and stable configuration.
- Final hardware acceptance uses the real M650: Forward hold+move pans without pointer travel; Forward
  click Smart Zooms; Back performs all five Window Navigation directions; Middle opens Mission Control;
  ordinary wheel behavior is unchanged.
