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

The M650 exposes its reprogrammable controls through HID++ feature `0x1b04`. Runtime capability
discovery identifies Middle as CID `0x0052`, Back as CID `0x0053`, and Forward as CID `0x0056`.
The acceptance mouse reports Back and Forward with flags `0x0d31`, which include temporary diversion
and raw XY. OpenLogi therefore treats HID++ diversion plus raw XY as the authoritative Back/Forward
gesture path on this device. It never assumes that a delayed macOS side-button event establishes the
physical hold: the observed M650 event tap emits the Back/Forward down/up pair only after movement has
already occurred.

Every effective per-button `Gesture` or `Pan` binding enters a HID capture request. The session
intersects that request with the device's live control table and diverts only controls that advertise
both `DIVERTABLE` and `RAW_XY`. `DivertedButtons` produces button-keyed pressed and released edges;
signed raw-XY reports become button-keyed motion for the one active diverted control. Back `0x0053`
therefore drives the Window Navigation accumulator while Forward `0x0056` independently drives the
Pan accumulator. A raw-XY report does not include its source CID. If two requested gesture controls
are held together, the session cancels the active lifecycle and ignores ambiguous motion until exactly
one requested control remains held, when it begins a new keyed lifecycle.

The OS-hook projection remains available for Middle, Back, and Forward as a compatibility path. It is
the normal path for an ordinary `Single` binding and may interpret a typed gesture only when the
device does not expose a usable raw-XY control and macOS supplies an observable down-motion-up chain.
Middle Click commonly follows this path when configured as `Single(MissionControl)`. A control that
lacks raw XY and reports only a delayed down/up pair cannot provide Pan or directional parity. The
current OS projection cannot detect that delayed-only shape before interpreting the pair, so it may
treat release as a deadzone click. This is a known fallback limitation, not native-behavior or
availability-reporting support. Acceptance covers only controls whose HID raw-XY path is active or
whose event tap supplies a complete lifecycle; capability-aware OS projection and unavailable-state UI
remain outside this change. The dedicated HID++ gesture button continues through the same keyed HID
session when its live control advertises the required capabilities.

A Pan hold starts at the keyed press. Normally every physical raw-XY report contributes one two-axis
pixel-scroll delta. The Bluetooth-direct M650 acceptance route (`046d:b02a`) has one narrowly scoped
exception: for Back `0x0053` and Forward `0x0056`, OpenLogi discards the first raw-XY report after each
new press. A 60-second raw capture split into 17 complete press/release sessions (10 Forward, 7 Back)
showed that the first report is not a usable delta for the new hold and is consistent with a pre-press
device accumulator. Click-like sessions began with values as large as Forward `(859,-1898)` and Back `(-70,-76)`,
while following reports were zero or approximately one unit. Movement sessions likewise began with
a discontinuity, then carried sustained motion in later reports. This is empirical M650
behavior, not a HID++ protocol invariant.

The exception is selected from the exact direct-device VID/PID and logical Back/Forward button. It does
not apply to a dedicated gesture control on the same route, another direct product ID, or Bolt/Unifying
routes whose device PID is not represented by `DeviceRoute`. Those paths preserve the first raw-XY
report. The compatibility cost is that the acceptance M650 can lose the small amount of genuine motion
coalesced into its first post-press report; subsequent reports still provide the continuous gesture.
This is preferable to turning a click into a hundreds- or thousands-unit Pan, but it must not be
generalized without equivalent multi-session hardware evidence and a no-stale-device regression test.

HID diversion is expected to prevent ordinary cursor travel; the macOS copied-event freeze path remains
a guard for OS-hook gesture input and pins replacement motion to the press location with both pointer
deltas cleared. Release inside the deadzone dispatches the Pan click action once; release after
continuous movement does not. Window Navigation uses the same keyed lifecycle but commits at most one
directional action. Physical verification records both HID++ `DivertedButtons`/raw-XY messages and any
corresponding CGEvent stream; synthetic CGEvent success alone is insufficient evidence.

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

The HID capture request and OS-hook fallback both derive from all effective gesture bindings in one
generation. A single active hold remains intentional: HID raw XY lacks a source CID, while the OS hook
supplies one pointer-motion stream. A new unambiguous press cancels the prior hold; simultaneous HID
holds cancel and suppress ambiguous motion. HID++ and OS-hook inputs share cancellation arbitration so
a stale release from one path cannot fire the other's click action. A per-app promotion or demotion
changes the complete request, invalidates the prior input epoch, restores the old session's controls,
and opens a session for the new button set.

Pan uses copied-event replacement at the active tap rather than reposting the original motion. A copy
lets OpenLogi freeze the pointer without recursively processing its own replacement. Synthetic output
is stamped and ignored by OpenLogi's translation gate. Smart Zoom uses the macOS zoom-toggle gesture
event shape because a Command-Plus shortcut only means webpage zoom-in and does not match the Options+
toggle behavior. This event shape contains private macOS fields, so real Safari/Edge acceptance is
mandatory after every relevant OS change.

## Failure, degradation, and rollback

If Accessibility permission is absent or the event tap is disabled, OpenLogi must fail open for
OS-hook controls: native mouse buttons and pointer movement continue rather than being swallowed. HID
capture is independent of Accessibility, but it must arm only controls selected by the current binding
and confirmed by the live `0x1b04` table. An unsupported or unrequested control remains under firmware
control. The GUI reports authorization state and must not describe a preset as runtime-verified.

Capture setup is one transaction across gesture CIDs, DPI controls, and the thumb wheel. Capability
discovery completes before any mutation. If any enable fails, OpenLogi first attempts to disable the
failed control itself because the device may have applied a request whose response was lost, then
attempts to disable every previously enabled control in reverse order. A reachable session stopped for
pairing takeover, configuration reload, or a per-app switch follows the same best-effort reverse
restore path; restore errors are warnings because transport loss can make the device unreachable. The
session cancels its active hold before restoration, so a cancelled hold never emits Smart Zoom or a
directional click.

Device removal and process termination cannot rely on that async teardown completing. HID++ temporary
diversion is expected to clear when the transport or device resets, but this behavior is an external
firmware boundary rather than a code-level guarantee. Acceptance must verify native-control readback
after a reachable teardown and must physically verify recovery after Agent restart and device
disconnect/reconnect. If either recovery fails, the build does not pass even when unit rollback tests do.

If HID capture shows no Forward press/raw-XY/release chain, the implementation must not compensate with
guessed CGEvent button numbers or synthetic input. Inspect the live `0x1b04` table, the exact
`setCidReporting` responses, receiver ownership, and competing Logitech software. If a requested
control lacks `DIVERTABLE` or `RAW_XY`, typed Gesture/Pan is unsupported unless the OS hook supplies a
complete physical lifecycle; the present fallback does not expose an unavailable state and can misread
a delayed-only pair as a click. If the HID report becomes ambiguous because two controls are held,
cancel it rather than attributing motion by timing. Unsupported non-macOS Pan remains inert and
preserves its config for later use on macOS.

If a future M650 firmware or transport no longer exhibits the pre-press first report, repeated raw
captures will show the first report aligned with the following motion instead of a discontinuity. The
rollback is to remove `046d:b02a` Back/Forward from the first-report discard policy and reinstall the
prior bundle; do not compensate by increasing the Pan deadzone. If another product exhibits the same
behavior, add it only after button-specific repeated captures and preserve a regression proving that
unlisted products and dedicated gesture controls deliver their first report.

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
- HID tests discover and arm requested Back/Forward CIDs, preserve signed XY, reject unsupported
  controls, cancel ambiguous two-button holds, and roll back partial setup across gesture, DPI, and
  thumb-wheel controls. Device-policy tests discard the first Back/Forward report only for direct
  `046d:b02a`, while preserving the first report for its dedicated gesture control and other products.
- Agent-core tests project and dispatch both buttons concurrently, including per-app overlays,
  session-epoch invalidation, keyed stale-release rejection, and overlapping-hold cancellation.
- GUI pure-state tests prove one card changes without demoting another and labels classify both.
- macOS hook tests cover the fallback physical motion event shapes and feedback prevention.
- Installed ARM64 app verification runs with the PPID-1 bundled agent and stable configuration.
- Final hardware acceptance uses the real M650: Forward hold+move pans without pointer travel; Forward
  click Smart Zooms; Back performs all five Window Navigation directions; Middle opens Mission Control;
  ordinary wheel behavior is unchanged. Repeated Forward click, Forward hold+move, and Back captures
  must show the complete first-report and subsequent-report timeline so the product-scoped discard does
  not hide the first meaningful movement. The evidence includes the HID++ lifecycle, best-effort
  restore responses, native-control readback after reload, and physical recovery after Agent restart
  and device reconnect.

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
middle click. The raw evidence bundle records HID++ CID, diverted-button set, signed XY, reporting
enable/restore responses, CGEvent type, button number, deltas, location, user-data, Agent monitor output,
installed binary hashes, config, profile switch, interruption/overlap recovery, and restart result.
Every row must pass repeatedly before the feature is considered complete.
