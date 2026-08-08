//! Background HID++ control-capture watcher for the active device.
//!
//! Runs [`openlogi_hid::run_capture_session`] on a dedicated thread for whichever
//! device the DPI / SmartShift path currently targets
//! ([`DpiCycleState::target`]), restarts it when the carousel selection — or the
//! thumb-wheel arming — changes, and dispatches each captured input:
//!
//! - each raw gesture-button lifecycle through a watcher-owned
//!   [`openlogi_core::binding::SwipeAccumulator`], dispatching a configured
//!   click, four-direction swipe, or Pan stream through its button's typed mode,
//! - a DPI/ModeShift or thumb-wheel-tap press through the button binding map,
//! - thumb-wheel rotation through the [`ButtonId::ThumbwheelScrollUp`] /
//!   [`ButtonId::ThumbwheelScrollDown`] bindings — either re-synthesised as
//!   continuous, sensitivity-scaled horizontal scroll or accumulated into a
//!   custom action,
//!
//! all via the common action path ([`crate::hook_runtime::dispatch_action`]).
//!
//! Unlike the CGEventTap hook, this needs no macOS Accessibility permission —
//! the events arrive over HID++, and the bound action is synthesised the same
//! way regardless.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use openlogi_core::binding::{
    Action, ButtonId, GestureDirection, PanAccumulator, PanOutput, SwipeAccumulator,
    default_binding,
};
use openlogi_core::config::DEFAULT_THUMBWHEEL_SENSITIVITY;
use openlogi_hid::{
    CaptureChannel, CaptureRequest, CapturedInput, DeviceRoute, run_capture_session,
};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::DpiCycleState;
use crate::bindings::GestureMode;
use crate::gesture_coordinator::{GestureCoordinator, GestureToken};
use crate::hook_runtime::{self, PanEmitter, SharedHookMaps};
use crate::receiver_access::ReceiverAccess;

/// Shared gesture binding map, mirrored from `AppState` (keyed by click/swipe
/// direction). The watcher reads it after interpreting the raw HID lifecycle.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GestureBindingState {
    /// Projection generation; changes on config, app, or device rebuild.
    pub generation: u64,
    /// Effective typed policy for every control diverted through HID++.
    pub modes: BTreeMap<ButtonId, GestureMode>,
}

pub type GestureBindings = Arc<RwLock<GestureBindingState>>;

/// Shared thumb-wheel sensitivity, mirrored from `AppState`. Read on every wheel
/// event; written only by `AppState::set_thumbwheel_sensitivity`.
pub type ThumbwheelSensitivity = Arc<AtomicI32>;

/// How often to re-read the active device target + thumb-wheel arming so a
/// carousel switch or a binding/sensitivity edit re-points / re-arms capture.
/// It also paces the respawn of a session that ended on its own (see `manage`).
const TARGET_POLL: Duration = Duration::from_secs(1);

/// Idle gap after which a partly-accumulated *custom* wheel action is forgotten,
/// so slow intermittent nudges don't eventually cross the threshold.
const ACTION_DECAY: Duration = Duration::from_millis(300);

/// Minimum gap between two fires of the same custom wheel action, so one
/// deliberate flick triggers once instead of repeating across a fast spin.
const ACTION_COOLDOWN: Duration = Duration::from_millis(200);

struct SessionInput {
    epoch: u64,
    input: CapturedInput,
}

/// Monotonic generation shared by projection owners and the capture manager.
#[derive(Clone, Default)]
pub struct CaptureEpoch {
    value: Arc<AtomicU64>,
}

impl CaptureEpoch {
    pub(crate) fn current(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    pub(crate) fn invalidate(&self) {
        self.value.fetch_add(1, Ordering::AcqRel);
    }

    fn take_current(&self, message: &SessionInput) -> Option<CapturedInput> {
        (message.epoch == self.current()).then_some(message.input)
    }
}

async fn forward_session_inputs(
    epoch: u64,
    mut source: mpsc::UnboundedReceiver<CapturedInput>,
    target: mpsc::UnboundedSender<SessionInput>,
) {
    while let Some(input) = source.recv().await {
        if target.send(SessionInput { epoch, input }).is_err() {
            break;
        }
    }
}

/// Shared invalidation state for HID capture sessions.
#[derive(Clone)]
pub struct CaptureSessionControl {
    receiver_access: ReceiverAccess,
    epoch: CaptureEpoch,
    gesture_coordinator: GestureCoordinator,
}

impl CaptureSessionControl {
    /// Couple exclusive receiver ownership with the epoch that invalidates
    /// already-queued input from superseded sessions.
    #[must_use]
    pub fn new(
        receiver_access: ReceiverAccess,
        epoch: CaptureEpoch,
        gesture_coordinator: GestureCoordinator,
    ) -> Self {
        Self {
            receiver_access,
            epoch,
            gesture_coordinator,
        }
    }
}

/// Speed multiplier for the wheel's continuous horizontal scroll. The default
/// sensitivity is 1×; the scale is linear around it.
#[allow(
    clippy::cast_precision_loss,
    reason = "sensitivity is a small 1..=100 integer — exact in f32"
)]
fn scroll_multiplier(sensitivity: i32) -> f32 {
    sensitivity as f32 / DEFAULT_THUMBWHEEL_SENSITIVITY as f32
}

/// Rotation increments required to fire a custom (non-scroll) wheel action.
/// Higher sensitivity → fewer increments; always at least one.
fn action_threshold(sensitivity: i32) -> i32 {
    (2 * DEFAULT_THUMBWHEEL_SENSITIVITY - sensitivity).max(1)
}

/// Spawn the capture-manager thread. It owns a current-thread tokio runtime that
/// keeps one capture session pointed at the active device and dispatches each
/// captured input.
pub fn spawn(
    hook_maps: SharedHookMaps,
    gesture_bindings: GestureBindings,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture_channel: CaptureChannel,
    thumbwheel_sensitivity: ThumbwheelSensitivity,
    pan_emitter: PanEmitter,
    session_control: CaptureSessionControl,
) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                warn!(error = %e, "capture watcher: could not build tokio runtime");
                return;
            }
        };
        runtime.block_on(manage(
            hook_maps,
            gesture_bindings,
            dpi_cycle,
            capture_channel,
            thumbwheel_sensitivity,
            pan_emitter,
            session_control,
        ));
    });
}

/// Whether the thumb wheel must be diverted over HID++ (which suppresses native
/// scroll) so we can re-synthesise its scroll or capture its tap.
///
/// We divert when the sensitivity leaves its default (so we can scale scroll
/// ourselves) or when the click or either rotation direction is rebound away
/// from its default; otherwise the OS scrolls the wheel natively.
fn thumbwheel_armed(hook_maps: &SharedHookMaps, sensitivity: i32) -> bool {
    if sensitivity != DEFAULT_THUMBWHEEL_SENSITIVITY {
        return true;
    }
    hook_maps.read().ok().is_some_and(|maps| {
        [
            ButtonId::Thumbwheel,
            ButtonId::ThumbwheelScrollUp,
            ButtonId::ThumbwheelScrollDown,
        ]
        .iter()
        .any(|&button| {
            maps.bindings
                .get(&button)
                .is_some_and(|action| *action != default_binding(button))
        })
    })
}

/// Build the transport request from the effective typed gesture projection.
/// Keeping this value comparable lets the manager restart the live HID++
/// session whenever one button enters or leaves gesture mode.
fn capture_request(
    capture_thumbwheel: bool,
    modes: &BTreeMap<ButtonId, GestureMode>,
) -> CaptureRequest {
    CaptureRequest {
        capture_thumbwheel,
        gesture_buttons: modes.keys().copied().collect::<BTreeSet<_>>(),
    }
}

/// Whether a finished capture session should make the manager re-arm.
///
/// `done_epoch` identifies the session that signalled completion; `live_epoch`
/// is the session the manager currently believes is running; `has_target` is
/// whether a device is currently targeted. A session ending only warrants a
/// respawn when it is the *current* one (not a stale session already superseded
/// by a deliberate restart, whose epoch no longer matches) and a target is still
/// set (not a deliberate stop-to-idle, e.g. while pairing owns the receiver).
fn should_rearm(done_epoch: u64, live_epoch: u64, has_target: bool) -> bool {
    done_epoch == live_epoch && has_target
}

/// Keep one capture session alive for the active device, restarting it when the
/// device or the thumb-wheel arming changes, and dispatch incoming inputs. Runs
/// for the lifetime of the process.
#[allow(
    clippy::too_many_lines,
    reason = "the select loop keeps capture start, stop, input, and completion transitions together"
)]
async fn manage(
    hook_maps: SharedHookMaps,
    gesture_bindings: GestureBindings,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture_channel: CaptureChannel,
    thumbwheel_sensitivity: ThumbwheelSensitivity,
    pan_emitter: PanEmitter,
    session_control: CaptureSessionControl,
) {
    let (tx, mut rx) = mpsc::unbounded_channel::<SessionInput>();
    // (route, complete transport request, input epoch)
    let mut current: Option<(DeviceRoute, CaptureRequest, u64)> = None;
    let mut stop: Option<oneshot::Sender<()>> = None;
    let mut ticker = tokio::time::interval(TARGET_POLL);
    let mut dispatch_state = CaptureDispatchState {
        gesture: GestureDispatchState::new(session_control.gesture_coordinator.clone()),
        ..CaptureDispatchState::default()
    };
    // Capture sessions run as detached tasks, so an unexpected exit (a transient
    // HID++ read error, a sleep-wake glitch, brief radio loss) would otherwise go
    // unnoticed: the tick below only restarts on a *changed* target, so a session
    // that dies with the target unchanged leaves the gesture button and thumb
    // wheel dead until the next carousel switch, config reload, or agent restart.
    // Each session reports its completion here, tagged with the epoch it started
    // under, so a dead *current* session can be re-armed while stale completions
    // (from an already-superseded session) are ignored.
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<u64>();

    loop {
        tokio::select! {
            Some(message) = rx.recv() => {
                let live_epoch = session_control.epoch.current();
                let session_is_live = current
                    .as_ref()
                    .is_some_and(|(_, _, epoch)| *epoch == live_epoch);
                let Some(input) = session_is_live
                    .then(|| session_control.epoch.take_current(&message))
                    .flatten()
                else {
                    dispatch_state.gesture.cancel();
                    continue;
                };
                dispatch(
                    input,
                    &mut dispatch_state,
                    &DispatchContext {
                        hook_maps: &hook_maps,
                        gesture_bindings: &gesture_bindings,
                        dpi_cycle: &dpi_cycle,
                        capture: &capture_channel,
                        thumbwheel_sensitivity: &thumbwheel_sensitivity,
                        pan_emitter: &pan_emitter,
                    },
                );
            }
            _ = ticker.tick() => {
                // While pairing is waiting or active, release the capture
                // session so run_pairing can own the receiver's HID node (one
                // process can't read it through two channels).
                let want = if session_control.receiver_access.pairing_requested() {
                    None
                } else {
                    let target = dpi_cycle.read().ok().and_then(|guard| guard.target.clone());
                    let sensitivity = thumbwheel_sensitivity.load(Ordering::Relaxed);
                    // Divert every control whose effective binding is typed as
                    // Gesture or Pan. Re-evaluated each tick, so per-app and
                    // config changes re-arm precisely the affected buttons.
                    let modes = gesture_bindings
                        .read()
                        .map(|state| state.modes.clone())
                        .unwrap_or_default();
                    let request = capture_request(
                        thumbwheel_armed(&hook_maps, sensitivity),
                        &modes,
                    );
                    let input_epoch = session_control.epoch.current();
                    target.map(|t| (t, request, input_epoch))
                };
                if want == current {
                    continue;
                }
                dispatch_state.gesture.cancel();
                // Target or thumb-wheel arming changed (or first tick): stop the
                // old session and start one for the new state. Sending on the
                // oneshot lets the old session restore the diverted controls.
                if let Some(stop) = stop.take() {
                    let _ = stop.send(());
                }
                if current.is_some() {
                    if want.is_none() {
                        session_control.epoch.invalidate();
                    }
                    current = None;
                    continue;
                }
                if let Some((route, request, session_epoch)) = want {
                    let Some(receiver_lease) = session_control.receiver_access.try_acquire_for_capture() else {
                        current = None;
                        continue;
                    };
                    current = Some((
                        route.clone(),
                        request.clone(),
                        session_epoch,
                    ));
                    let (stop_tx, stop_rx) = oneshot::channel();
                    let (sink, session_rx) = mpsc::unbounded_channel();
                    let tagged_sink = tx.clone();
                    tokio::spawn(forward_session_inputs(
                        session_epoch,
                        session_rx,
                        tagged_sink,
                    ));
                    let slot = Arc::clone(&capture_channel);
                    let done = done_tx.clone();
                    tokio::spawn(async move {
                        let _receiver_lease = receiver_lease;
                        if let Err(e) = run_capture_session(
                            route,
                            request,
                            sink,
                            stop_rx,
                            slot,
                        )
                        .await
                        {
                            debug!(error = %e, "capture session ended");
                        }
                        // Report completion so the manager can re-arm if this exit
                        // was unexpected rather than a deliberate stop.
                        let _ = done.send(session_epoch);
                    });
                    stop = Some(stop_tx);
                } else {
                    current = None;
                }
            }
            Some(done_epoch) = done_rx.recv() => {
                // A capture session ended on its own. Re-arm only when it is the
                // session we currently believe is live for an active target;
                // clearing `current` lets the next tick start a fresh session.
                // The tick fires at most once per `TARGET_POLL`, which paces the
                // respawn so a permanently failing device can't hot-loop. A stale
                // epoch or a deliberate stop-to-idle is a no-op (see `should_rearm`).
                let live_epoch = session_control.epoch.current();
                if should_rearm(done_epoch, live_epoch, current.is_some()) {
                    warn!("capture session for the active device ended unexpectedly, re-arming");
                    session_control.epoch.invalidate();
                    dispatch_state.gesture.cancel();
                    current = None;
                    // Keep the `stop`/`current` invariant: the session already
                    // exited, so its stop receiver is gone and dropping the sender
                    // here is a no-op, but it stops the next tick from signalling a
                    // session that no longer exists.
                    stop = None;
                }
            }
        }
    }
}

/// Per-direction wheel accumulators. The thumb wheel's two rotation directions
/// bind to independent actions, so each keeps its own running total — sharing
/// one would let a reversal cancel the other direction's progress.
#[derive(Default)]
struct WheelAccumulators {
    up: WheelDirection,
    down: WheelDirection,
}

/// Mutable input interpretation state owned by the capture watcher.
#[derive(Default)]
struct CaptureDispatchState {
    gesture: GestureDispatchState,
    wheels: WheelAccumulators,
}

#[derive(Default)]
struct GestureDispatchState {
    coordinator: GestureCoordinator,
    generation: u64,
    token: Option<GestureToken>,
    button: Option<ButtonId>,
    mode: Option<GestureMode>,
    swipe: SwipeAccumulator,
    pan: PanAccumulator,
}

impl GestureDispatchState {
    fn new(coordinator: GestureCoordinator) -> Self {
        Self {
            coordinator,
            ..Self::default()
        }
    }

    fn cancel(&mut self) {
        let _ = self.swipe.end();
        let _ = self.pan.cancel();
        self.button = None;
        self.mode = None;
        self.token = None;
    }
}

#[derive(Debug, PartialEq, Eq)]
enum GestureOutput {
    Idle,
    Action(Action),
    PanDelta { x: i32, y: i32 },
    End,
}

/// Running state for one rotation direction.
#[derive(Default)]
struct WheelDirection {
    /// Fractional line accumulator for continuous horizontal scroll.
    scroll: f32,
    /// Integer rotation-increment accumulator for a custom (non-scroll) action.
    action: i32,
    /// When the last rotation event for this direction arrived (decay clock).
    last_event: Option<Instant>,
    /// When this direction last fired its custom action (cooldown clock).
    last_fired: Option<Instant>,
}

/// What advancing a direction's accumulator should produce.
#[derive(Debug, PartialEq)]
enum WheelOutput {
    /// Below threshold / suppressed — emit nothing.
    Idle,
    /// Post this many horizontal scroll lines (signed: + right, − left).
    Scroll(i32),
    /// Fire the direction's bound custom action.
    FireAction,
}

/// Route one captured input to its bound action (or re-synthesised scroll).
struct DispatchContext<'a> {
    hook_maps: &'a SharedHookMaps,
    gesture_bindings: &'a GestureBindings,
    dpi_cycle: &'a Arc<RwLock<DpiCycleState>>,
    capture: &'a CaptureChannel,
    thumbwheel_sensitivity: &'a ThumbwheelSensitivity,
    pan_emitter: &'a PanEmitter,
}

fn dispatch(input: CapturedInput, state: &mut CaptureDispatchState, context: &DispatchContext<'_>) {
    match input {
        CapturedInput::GesturePressed(_)
        | CapturedInput::GestureMotion { .. }
        | CapturedInput::GestureReleased(_)
        | CapturedInput::GestureCancelled(_) => {
            let projection = context
                .gesture_bindings
                .read()
                .ok()
                .map(|guard| guard.clone());
            let Some(GestureBindingState { generation, modes }) = projection else {
                state.gesture.cancel();
                return;
            };
            match advance_gesture(&mut state.gesture, generation, &modes, input) {
                GestureOutput::Action(action) => {
                    debug!(action = %action.label(), "gesture → action");
                    hook_runtime::dispatch_action(&action, context.dpi_cycle, context.capture);
                }
                GestureOutput::PanDelta { x, y } => context.pan_emitter.emit(x, y),
                GestureOutput::Idle | GestureOutput::End => {}
            }
        }
        CapturedInput::ButtonPressed(button) => {
            let action = context
                .hook_maps
                .read()
                .ok()
                .and_then(|maps| maps.bindings.get(&button).cloned());
            if let Some(action) = action {
                debug!(?button, action = %action.label(), "HID++ button → action");
                hook_runtime::dispatch_action(&action, context.dpi_cycle, context.capture);
            } else {
                debug!(?button, "HID++ button with no binding — ignored");
            }
        }
        CapturedInput::Scroll(rotation) => {
            // Positive rotation is "up"; each direction has its own binding.
            let up = rotation >= 0;
            let button = if up {
                ButtonId::ThumbwheelScrollUp
            } else {
                ButtonId::ThumbwheelScrollDown
            };
            let action = context
                .hook_maps
                .read()
                .ok()
                .and_then(|maps| maps.bindings.get(&button).cloned())
                .unwrap_or_else(|| default_binding(button));
            let sensitivity = context.thumbwheel_sensitivity.load(Ordering::Relaxed);
            let dir = if up {
                &mut state.wheels.up
            } else {
                &mut state.wheels.down
            };
            let magnitude = i32::from(rotation).abs();
            match advance(dir, &action, magnitude, sensitivity, Instant::now()) {
                WheelOutput::Idle => {}
                WheelOutput::Scroll(lines) => {
                    openlogi_inject::post_horizontal_scroll(lines);
                }
                WheelOutput::FireAction => {
                    debug!(?button, action = %action.label(), "thumb wheel → action");
                    hook_runtime::dispatch_action(&action, context.dpi_cycle, context.capture);
                }
            }
        }
    }
}

/// Interpret one raw HID++ gesture event. Directional swipes commit once during
/// motion; release produces a click only when no direction committed; cancel
/// only resets the hold.
fn gesture_is_current(
    gesture: &GestureDispatchState,
    generation: u64,
    button: ButtonId,
    mode: &GestureMode,
) -> bool {
    gesture.button == Some(button)
        && gesture.generation == generation
        && gesture.mode.as_ref() == Some(mode)
        && gesture
            .token
            .is_some_and(|token| gesture.coordinator.is_current(token))
}

fn advance_gesture(
    gesture: &mut GestureDispatchState,
    generation: u64,
    modes: &BTreeMap<ButtonId, GestureMode>,
    input: CapturedInput,
) -> GestureOutput {
    match input {
        CapturedInput::GesturePressed(button) => {
            let Some(mode) = modes.get(&button) else {
                if gesture.button == Some(button) {
                    gesture.cancel();
                }
                return GestureOutput::Idle;
            };
            gesture.cancel();
            gesture.generation = generation;
            gesture.token = Some(gesture.coordinator.acquire());
            gesture.button = Some(button);
            gesture.mode = Some(mode.clone());
            match mode {
                GestureMode::Directional(_) => gesture.swipe.begin_immediate(),
                GestureMode::Pan(_) => gesture.pan.begin(),
            }
            GestureOutput::Idle
        }
        CapturedInput::GestureMotion {
            button,
            delta_x,
            delta_y,
        } => {
            if gesture.button != Some(button) {
                return GestureOutput::Idle;
            }
            let Some(mode) = modes.get(&button) else {
                gesture.cancel();
                return GestureOutput::End;
            };
            if !gesture_is_current(gesture, generation, button, mode) {
                gesture.cancel();
                return GestureOutput::End;
            }
            match mode {
                GestureMode::Directional(directions) => gesture
                    .swipe
                    .accumulate(i32::from(delta_x), i32::from(delta_y))
                    .and_then(|direction| directions.get(&direction).cloned())
                    .map_or(GestureOutput::Idle, GestureOutput::Action),
                GestureMode::Pan(_) => match gesture
                    .pan
                    .accumulate(i32::from(delta_x), i32::from(delta_y))
                {
                    PanOutput::Delta { x, y } => GestureOutput::PanDelta { x, y },
                    PanOutput::Idle | PanOutput::Click | PanOutput::End => GestureOutput::Idle,
                },
            }
        }
        CapturedInput::GestureReleased(button) => {
            if gesture.button != Some(button) {
                return GestureOutput::Idle;
            }
            let Some(mode) = modes.get(&button) else {
                gesture.cancel();
                return GestureOutput::Idle;
            };
            if !gesture_is_current(gesture, generation, button, mode) {
                gesture.cancel();
                return GestureOutput::Idle;
            }
            gesture.button = None;
            gesture.mode = None;
            gesture.token = None;
            match mode {
                GestureMode::Directional(directions) => {
                    if gesture.swipe.end() {
                        directions
                            .get(&GestureDirection::Click)
                            .cloned()
                            .map_or(GestureOutput::Idle, GestureOutput::Action)
                    } else {
                        GestureOutput::End
                    }
                }
                GestureMode::Pan(pan) => match gesture.pan.end() {
                    PanOutput::Click => GestureOutput::Action(pan.click.clone()),
                    PanOutput::End => GestureOutput::End,
                    PanOutput::Idle | PanOutput::Delta { .. } => GestureOutput::Idle,
                },
            }
        }
        CapturedInput::GestureCancelled(button) => {
            if gesture.button == Some(button) {
                gesture.cancel();
                GestureOutput::End
            } else {
                GestureOutput::Idle
            }
        }
        CapturedInput::ButtonPressed(_) | CapturedInput::Scroll(_) => GestureOutput::Idle,
    }
}

/// Advance one direction's accumulator by `magnitude` rotation increments and
/// decide what to emit. Pure given `now`, so the decay/cooldown/threshold logic
/// is unit-testable without touching the OS.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "magnitude/sensitivity are small integers and `lines` is a trunc'd \
              whole number — both well within f32/i32 range"
)]
fn advance(
    dir: &mut WheelDirection,
    action: &Action,
    magnitude: i32,
    sensitivity: i32,
    now: Instant,
) -> WheelOutput {
    match action {
        // Suppressed: captured but produces nothing.
        Action::None => WheelOutput::Idle,
        // Continuous, sensitivity-scaled horizontal scroll. Direction comes
        // from the action; magnitude from the accumulated rotation.
        Action::HorizontalScrollRight | Action::HorizontalScrollLeft => {
            dir.scroll += magnitude as f32 * scroll_multiplier(sensitivity);
            let lines = dir.scroll.trunc();
            if lines >= 1.0 {
                dir.scroll -= lines;
                let sign = if matches!(action, Action::HorizontalScrollRight) {
                    1
                } else {
                    -1
                };
                WheelOutput::Scroll(sign * lines as i32)
            } else {
                WheelOutput::Idle
            }
        }
        // Any other action: fire once per `action_threshold` increments, with
        // decay (forget stale partial progress) and cooldown (one flick = one
        // fire).
        _ => {
            if dir
                .last_event
                .is_some_and(|t| now.saturating_duration_since(t) > ACTION_DECAY)
            {
                dir.action = 0;
            }
            dir.last_event = Some(now);

            if dir
                .last_fired
                .is_some_and(|t| now.saturating_duration_since(t) < ACTION_COOLDOWN)
            {
                return WheelOutput::Idle;
            }

            dir.action += magnitude;
            if dir.action >= action_threshold(sensitivity) {
                dir.action = 0;
                dir.last_fired = Some(now);
                WheelOutput::FireAction
            } else {
                WheelOutput::Idle
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use openlogi_core::binding::{PAN_DEADZONE, PanBinding};

    const GESTURE_BUTTON: ButtonId = ButtonId::GestureButton;

    fn advance_mode(
        gesture: &mut GestureDispatchState,
        generation: u64,
        mode: &GestureMode,
        input: CapturedInput,
    ) -> GestureOutput {
        let button = match input {
            CapturedInput::GesturePressed(button)
            | CapturedInput::GestureReleased(button)
            | CapturedInput::GestureCancelled(button)
            | CapturedInput::GestureMotion { button, .. } => button,
            CapturedInput::ButtonPressed(_) | CapturedInput::Scroll(_) => GESTURE_BUTTON,
        };
        advance_gesture(
            gesture,
            generation,
            &BTreeMap::from([(button, mode.clone())]),
            input,
        )
    }

    fn gesture_pressed() -> CapturedInput {
        CapturedInput::GesturePressed(GESTURE_BUTTON)
    }

    fn gesture_released() -> CapturedInput {
        CapturedInput::GestureReleased(GESTURE_BUTTON)
    }

    fn gesture_cancelled() -> CapturedInput {
        CapturedInput::GestureCancelled(GESTURE_BUTTON)
    }

    fn gesture_motion(delta_x: i16, delta_y: i16) -> CapturedInput {
        CapturedInput::GestureMotion {
            button: GESTURE_BUTTON,
            delta_x,
            delta_y,
        }
    }

    #[test]
    fn capture_request_changes_when_one_gesture_button_changes() {
        let back = GestureMode::Directional(BTreeMap::from([(
            GestureDirection::Click,
            Action::MissionControl,
        )]));
        let pan = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let back_only = BTreeMap::from([(ButtonId::Back, back.clone())]);
        let both = BTreeMap::from([(ButtonId::Back, back), (ButtonId::Forward, pan)]);

        let before = capture_request(false, &back_only);
        let after = capture_request(false, &both);

        assert_ne!(before, after, "the session key must observe the button set");
        assert_eq!(
            after.gesture_buttons,
            BTreeSet::from([ButtonId::Back, ButtonId::Forward])
        );
        assert!(!after.capture_thumbwheel);
    }

    #[test]
    fn newest_button_press_owns_lifecycle_and_stale_release_is_ignored() {
        let forward = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let back = GestureMode::Directional(BTreeMap::from([(
            GestureDirection::Click,
            Action::MissionControl,
        )]));
        let mut gesture = GestureDispatchState::default();

        assert_eq!(
            advance_mode(
                &mut gesture,
                1,
                &forward,
                CapturedInput::GesturePressed(ButtonId::Forward),
            ),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(
                &mut gesture,
                1,
                &back,
                CapturedInput::GesturePressed(ButtonId::Back),
            ),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(
                &mut gesture,
                1,
                &forward,
                CapturedInput::GestureReleased(ButtonId::Forward),
            ),
            GestureOutput::Idle,
            "the superseded button cannot release the newest hold"
        );
        assert_eq!(gesture.button, Some(ButtonId::Back));
        assert_eq!(
            advance_mode(
                &mut gesture,
                1,
                &back,
                CapturedInput::GestureReleased(ButtonId::Back),
            ),
            GestureOutput::Action(Action::MissionControl)
        );
    }

    #[test]
    fn keyed_back_motion_and_forward_pan_use_independent_modes() {
        let back = GestureMode::Directional(BTreeMap::from([(
            GestureDirection::Left,
            Action::PreviousDesktop,
        )]));
        let forward = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let modes = BTreeMap::from([(ButtonId::Back, back), (ButtonId::Forward, forward)]);
        let mut gesture = GestureDispatchState::default();

        assert_eq!(
            advance_gesture(
                &mut gesture,
                1,
                &modes,
                CapturedInput::GesturePressed(ButtonId::Forward),
            ),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_gesture(
                &mut gesture,
                1,
                &modes,
                CapturedInput::GestureReleased(ButtonId::Forward),
            ),
            GestureOutput::Action(Action::SmartZoom),
            "Forward Pan keeps its click fallback"
        );

        assert_eq!(
            advance_gesture(
                &mut gesture,
                1,
                &modes,
                CapturedInput::GesturePressed(ButtonId::Back),
            ),
            GestureOutput::Idle
        );
        gesture.swipe.backdate_hold_for_test();
        assert_eq!(
            advance_gesture(
                &mut gesture,
                1,
                &modes,
                CapturedInput::GestureMotion {
                    button: ButtonId::Back,
                    delta_x: -120,
                    delta_y: 0,
                },
            ),
            GestureOutput::Action(Action::PreviousDesktop)
        );
        assert_eq!(
            advance_gesture(
                &mut gesture,
                1,
                &modes,
                CapturedInput::GestureReleased(ButtonId::Back),
            ),
            GestureOutput::End
        );

        assert_eq!(
            advance_gesture(
                &mut gesture,
                1,
                &modes,
                CapturedInput::GesturePressed(ButtonId::Forward),
            ),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_gesture(
                &mut gesture,
                1,
                &modes,
                CapturedInput::GestureMotion {
                    button: ButtonId::Forward,
                    delta_x: 40,
                    delta_y: -9,
                },
            ),
            GestureOutput::PanDelta { x: 40, y: -9 }
        );
    }

    #[tokio::test]
    async fn queued_old_sender_input_is_dropped_after_epoch_invalidation() {
        let epoch = CaptureEpoch::default();
        let old_epoch = epoch.current();
        let (old_sender, old_receiver) = mpsc::unbounded_channel();
        let (tagged_sender, mut tagged_receiver) = mpsc::unbounded_channel();
        let deadzone = i16::try_from(PAN_DEADZONE).unwrap_or_default();
        assert!(old_sender.send(gesture_pressed()).is_ok());
        assert!(old_sender.send(gesture_released()).is_ok());
        assert!(old_sender.send(gesture_pressed()).is_ok());
        assert!(old_sender.send(gesture_motion(deadzone, -deadzone)).is_ok());
        assert!(old_sender.send(gesture_released()).is_ok());
        drop(old_sender);

        epoch.invalidate();
        forward_session_inputs(old_epoch, old_receiver, tagged_sender).await;

        let mut accepted = Vec::new();
        while let Some(message) = tagged_receiver.recv().await {
            if let Some(input) = epoch.take_current(&message) {
                accepted.push(input);
            }
        }
        assert!(accepted.is_empty(), "old queued lifecycle must be dropped");
    }

    #[test]
    fn hid_pan_lifecycle_emits_delta_then_ends_without_click() {
        let mode = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let mut gesture = GestureDispatchState::default();

        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_motion(40, -9)),
            GestureOutput::PanDelta { x: 40, y: -9 }
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::End
        );
    }

    #[test]
    fn hid_pan_release_inside_deadzone_dispatches_click_but_cancel_does_not() {
        let mode = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let mut gesture = GestureDispatchState::default();
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::Action(Action::SmartZoom)
        );

        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_cancelled()),
            GestureOutput::End
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::Idle
        );
    }

    #[test]
    fn hid_projection_generation_change_cancels_without_click() {
        let mode = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let mut gesture = GestureDispatchState::default();
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(&mut gesture, 2, &mode, gesture_released()),
            GestureOutput::Idle
        );
        assert!(gesture.mode.is_none());
    }

    #[test]
    fn os_press_invalidates_dedicated_hold_without_phantom_click() {
        let coordinator = crate::gesture_coordinator::GestureCoordinator::default();
        let mode = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let mut gesture = GestureDispatchState::new(coordinator.clone());
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );

        let _os = coordinator.acquire();
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_motion(40, -20)),
            GestureOutput::End
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::Idle
        );
    }

    #[test]
    fn dedicated_press_takes_over_from_os_and_owns_motion_and_release() {
        let coordinator = crate::gesture_coordinator::GestureCoordinator::default();
        let _os = coordinator.acquire();
        let mode = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let mut gesture = GestureDispatchState::new(coordinator.clone());
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_motion(40, -9)),
            GestureOutput::PanDelta { x: 40, y: -9 }
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::End
        );
    }

    #[test]
    fn hid_raw_xy_commits_a_fast_direction_instead_of_misfiring_click() {
        let mode = GestureMode::Directional(BTreeMap::from([
            (GestureDirection::Right, Action::Paste),
            (GestureDirection::Click, Action::Copy),
        ]));
        let mut gesture = GestureDispatchState::default();

        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_motion(120, 5)),
            GestureOutput::Action(Action::Paste),
            "trusted HID++ RawXY must not turn a fast swipe into the Click action"
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::End
        );
    }

    #[test]
    fn gesture_motion_maps_all_four_directions_once() {
        for (delta_x, delta_y, direction, expected) in [
            (120, 5, GestureDirection::Right, Action::Paste),
            (-120, 5, GestureDirection::Left, Action::Copy),
            (5, 120, GestureDirection::Down, Action::AppExpose),
            (5, -120, GestureDirection::Up, Action::MissionControl),
        ] {
            let mode = GestureMode::Directional(BTreeMap::from([(direction, expected.clone())]));
            let mut gesture = GestureDispatchState::default();
            assert_eq!(
                advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
                GestureOutput::Idle
            );
            gesture.swipe.backdate_hold_for_test();
            assert_eq!(
                advance_mode(&mut gesture, 1, &mode, gesture_motion(delta_x, delta_y)),
                GestureOutput::Action(expected)
            );
            assert_eq!(
                advance_mode(&mut gesture, 1, &mode, gesture_motion(delta_x, delta_y)),
                GestureOutput::Idle,
                "a committed direction fires once"
            );
            assert_eq!(
                advance_mode(&mut gesture, 1, &mode, gesture_released()),
                GestureOutput::End,
                "a committed swipe does not also click"
            );
        }
    }

    #[test]
    fn gesture_release_and_cancel_reset_without_phantom_clicks() {
        let mode =
            GestureMode::Directional(BTreeMap::from([(GestureDirection::Click, Action::Copy)]));
        let mut gesture = GestureDispatchState::default();

        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::Idle,
            "a stray release is not a click"
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_cancelled()),
            GestureOutput::End
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::Idle,
            "release after cancellation is not a click"
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_pressed()),
            GestureOutput::Idle
        );
        assert_eq!(
            advance_mode(&mut gesture, 1, &mode, gesture_released()),
            GestureOutput::Action(Action::Copy),
            "cancellation resets the accumulator for the next hold"
        );
    }

    #[test]
    fn multiplier_is_unity_at_default_sensitivity() {
        assert!((scroll_multiplier(DEFAULT_THUMBWHEEL_SENSITIVITY) - 1.0).abs() < f32::EPSILON);
        assert!(scroll_multiplier(DEFAULT_THUMBWHEEL_SENSITIVITY * 2) > 1.9);
        assert!(scroll_multiplier(1) < 0.1);
    }

    #[test]
    fn action_threshold_drops_with_sensitivity_and_floors_at_one() {
        assert_eq!(
            action_threshold(DEFAULT_THUMBWHEEL_SENSITIVITY),
            DEFAULT_THUMBWHEEL_SENSITIVITY
        );
        assert!(
            action_threshold(1) > action_threshold(DEFAULT_THUMBWHEEL_SENSITIVITY),
            "low sensitivity needs more increments"
        );
        assert_eq!(action_threshold(100), 1, "high sensitivity floors at one");
    }

    #[test]
    fn scroll_accumulates_fractionally_at_sub_unity_sensitivity() {
        let mut dir = WheelDirection::default();
        let now = Instant::now();
        // multiplier 0.5: two increments make one whole line.
        let half = DEFAULT_THUMBWHEEL_SENSITIVITY / 2;
        assert_eq!(
            advance(&mut dir, &Action::HorizontalScrollRight, 1, half, now),
            WheelOutput::Idle
        );
        assert_eq!(
            advance(&mut dir, &Action::HorizontalScrollRight, 1, half, now),
            WheelOutput::Scroll(1)
        );
    }

    #[test]
    fn scroll_left_emits_negative_lines() {
        let mut dir = WheelDirection::default();
        let now = Instant::now();
        assert_eq!(
            advance(
                &mut dir,
                &Action::HorizontalScrollLeft,
                1,
                DEFAULT_THUMBWHEEL_SENSITIVITY,
                now
            ),
            WheelOutput::Scroll(-1)
        );
    }

    #[test]
    fn directions_accumulate_independently() {
        // A reversal must not drain the other direction's pending progress.
        let mut up = WheelDirection::default();
        let mut down = WheelDirection::default();
        let now = Instant::now();
        let half = DEFAULT_THUMBWHEEL_SENSITIVITY / 2; // multiplier 0.5
        assert_eq!(
            advance(&mut up, &Action::HorizontalScrollRight, 1, half, now),
            WheelOutput::Idle
        );
        // One tick the other way doesn't cancel `up`'s banked half-line…
        assert_eq!(
            advance(&mut down, &Action::HorizontalScrollLeft, 1, half, now),
            WheelOutput::Idle
        );
        // …so `up`'s next tick still completes its own line.
        assert_eq!(
            advance(&mut up, &Action::HorizontalScrollRight, 1, half, now),
            WheelOutput::Scroll(1)
        );
    }

    #[test]
    fn custom_action_fires_on_threshold_then_respects_cooldown() {
        let mut dir = WheelDirection::default();
        let now = Instant::now();
        // Threshold at default sensitivity is DEFAULT increments.
        for _ in 0..DEFAULT_THUMBWHEEL_SENSITIVITY - 1 {
            assert_eq!(
                advance(
                    &mut dir,
                    &Action::VolumeUp,
                    1,
                    DEFAULT_THUMBWHEEL_SENSITIVITY,
                    now
                ),
                WheelOutput::Idle
            );
        }
        assert_eq!(
            advance(
                &mut dir,
                &Action::VolumeUp,
                1,
                DEFAULT_THUMBWHEEL_SENSITIVITY,
                now
            ),
            WheelOutput::FireAction
        );
        // Immediately after, the cooldown swallows further increments.
        for _ in 0..DEFAULT_THUMBWHEEL_SENSITIVITY {
            assert_eq!(
                advance(
                    &mut dir,
                    &Action::VolumeUp,
                    1,
                    DEFAULT_THUMBWHEEL_SENSITIVITY,
                    now
                ),
                WheelOutput::Idle
            );
        }
    }

    #[test]
    fn none_action_is_suppressed() {
        let mut dir = WheelDirection::default();
        assert_eq!(
            advance(
                &mut dir,
                &Action::None,
                5,
                DEFAULT_THUMBWHEEL_SENSITIVITY,
                Instant::now()
            ),
            WheelOutput::Idle
        );
    }

    #[test]
    fn rearms_when_the_current_session_dies_with_a_target() {
        // The live session ended on its own while a device is still targeted.
        assert!(should_rearm(7, 7, true));
    }

    #[test]
    fn ignores_a_stale_session_superseded_by_a_restart() {
        // An older session reports completion after a deliberate restart already
        // bumped the epoch; re-arming would needlessly cycle the live session.
        assert!(!should_rearm(6, 7, true));
    }

    #[test]
    fn ignores_a_deliberate_stop_to_idle() {
        // The session was stopped on purpose (pairing took the receiver, or no
        // device is targeted): no target means there is nothing to re-arm.
        assert!(!should_rearm(7, 7, false));
    }
}
