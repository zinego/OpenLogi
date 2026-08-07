# Multi-button gesture parity with Logi Options+

## Outcome

OpenLogi must let every gesture-capable mouse button own its binding independently. On the test
Signature M650 L, Forward is configured as Pan, Back as Window Navigation, and Middle Click as
Mission Control at the same time. The GUI and the physical mouse behavior must agree.

## Scope and non-goals

This change covers the two Logi Options+ gesture presets that are missing from OpenLogi on macOS:
Pan and Window Navigation. It also removes the product-level assumption that only one physical
button may own a gesture preset. The acceptance device is a Bluetooth-direct Signature M650 L
(`VID 046d`, `PID b02a`), with Forward set to Pan, Back set to Window Navigation, and Middle Click
set to Mission Control.

The change does not reproduce the complete Options+ action catalog, Smart Actions, application
catalog, device artwork, or cloud features. It does not add continuous Pan to Linux or Windows;
those platforms must continue to reject or hide unsupported Pan selection without corrupting a Pan
binding already present in a shared config. It does not make primary left/right clicks suppressible,
and it does not claim that a synthetic CGEvent is equivalent to a physical M650 event. Exact visual
pixel parity with Options+ is outside scope; the required GUI parity is the same reachable presets,
per-button configuration model, labels, and persisted behavior.

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

The macOS event tap is the authority for Middle, Back, and Forward because these controls arrive as
ordinary mouse buttons rather than capturable HID++ gesture controls on the M650. A Pan hold starts
only after the configured button-down. While the hold is active, each physical motion contributes one
two-axis pixel-scroll delta, and the replacement mouse event is pinned to the button-down location with
both pointer deltas cleared. Release inside the deadzone dispatches the Pan click action; release after
continuous movement does not. Window Navigation uses the same hold lifecycle but commits at most one
directional action.

## Environment and external dependencies

Acceptance runs on Apple Silicon macOS with the ARM64 OpenLogi GUI and its nested Agent built from the
same Git SHA. The test requires a connected Signature M650 L, Accessibility permission for both the
GUI and nested Agent identities, a functioning CGEventTap, and a foreground application that exposes
observable scrolling and macOS navigation behavior. Safari or Microsoft Edge provides the webpage for
Pan and Smart Zoom checks; Mission Control, App Expose, Spaces, and Show Desktop must be enabled by
macOS. Bluetooth, the HID++ interface, and the local tarpc socket between GUI and Agent must remain
available throughout the run.

Logi Options+ is a comparison oracle, not a co-running dependency. Record its behavior separately,
then quit its main application and background input processes before OpenLogi acceptance so two agents
do not compete for the same device or remap the same button. The ordinary wheel check uses the same
foreground page and macOS scrolling direction before and after enabling the OpenLogi presets.

Local GPUI builds require full Xcode because the shader build invokes `metal`. When full Xcode is not
available, GitHub Actions must build the exact pushed SHA; cached or fake shader artifacts establish
Rust type/test coverage only and are not runtime evidence. The installed CI artifact may be ad-hoc
signed for local testing, but release-signing status is reported separately and is not inferred from a
successful launch.

## Design decisions and rationale

Per-button `Binding` is the single runtime and persistence authority. Keeping a second scalar owner
would allow the GUI, config, and runtime projections to disagree and would make simultaneous Forward
Pan plus Back Window Navigation impossible. Whole-binding replacement for per-app overrides prevents a
directional map from being partially inherited from an unrelated global preset.

The OS hook consumes all effective gesture bindings in one generation. A single active hold remains
intentional because macOS supplies one pointer-motion stream; if two configured buttons overlap, the
newest press cancels the prior hold. This produces deterministic behavior without applying the same
motion to two presets. HID++ and OS-hook inputs share cancellation arbitration so a stale release from
one path cannot fire the other's click action.

Pan uses copied-event replacement at the active tap rather than reposting the original motion. A copy
lets OpenLogi freeze the pointer without recursively processing its own replacement. Synthetic output
is stamped and ignored by OpenLogi's translation gate. Smart Zoom uses the macOS zoom-toggle gesture
event shape because a Command-Plus shortcut only means webpage zoom-in and does not match the Options+
toggle behavior. This event shape contains private macOS fields, so real Safari/Edge acceptance is
mandatory after every relevant OS change.

## Failure, degradation, and rollback

If Accessibility permission is absent or the event tap is disabled, OpenLogi must fail open: native
mouse buttons and pointer movement continue rather than being swallowed. The GUI reports authorization
state and must not describe a preset as runtime-verified. Callback lock contention, a full action queue,
capture interruption, device removal, app-profile change, or Agent restart cancels the active hold; a
cancelled hold never emits Smart Zoom or a directional click.

If raw physical capture shows no Forward down/up, the implementation must not compensate with guessed
button numbers or synthetic events. Diagnose HID++ diversion and other Logitech software ownership. If
the captured button differs from the model mapping, update the mapping from that evidence. If motion
deltas are zero while locations change, derive travel from successive physical locations and cover that
shape with a regression test. Unsupported non-macOS Pan remains inert and preserves its config for later
use on macOS.

Installation is recoverable: retain the previous `/Applications/OpenLogi.app` bundle before replacing
it, and keep the previous config or a copy before any schema migration. Rollback quits the new GUI and
Agent, restores the prior bundle and config, and re-enables its Accessibility identity if macOS treats
the restored code requirement separately. A failed physical acceptance rolls back the installed build;
it does not redefine completion around passing unit tests or synthetic probes.

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

## Local acceptance procedure and expected results

The acceptance operator first records Logi Options+ Pan and Window Navigation behavior with only
Options+ active, then quits it and launches the exact-sha OpenLogi bundle. After configuring Forward,
Back, and Middle through the GUI, the saved schema-v4 TOML must contain a Forward `Pan`, a complete Back
`Gesture`, and `MiddleClick = "MissionControl"` simultaneously, with no `gesture_owner`. Quitting both
OpenLogi processes and relaunching the bundle must display the same three card labels and load the same
bindings in the Agent.

On a scrollable Safari or Edge page, holding physical Forward and moving vertically must scroll only
vertically; moving horizontally must scroll only horizontally; moving diagonally must produce both axes
from the same hold. The cursor location must remain unchanged until release. A Forward press and release
inside the deadzone must toggle Smart Zoom once, while a completed Pan must not also zoom. Ordinary wheel
rotation before and after the gesture checks must retain the same direction, granularity, and one-event
behavior, without duplicated or inverted output.

Physical Back must implement the canonical five outputs: click and upward gesture open Mission Control,
downward gesture opens App Expose, left switches to the previous desktop, and right switches to the next
desktop. Each hold commits no more than one direction and does not also emit the native browser Back
action. Physical Middle Click must open Mission Control once and must not pass through as an ordinary
middle click. The raw evidence bundle records event type, button number, deltas, location, user-data,
Agent monitor output, installed binary hashes, config, and restart result. Every row must pass repeatedly
before the feature is considered complete.
