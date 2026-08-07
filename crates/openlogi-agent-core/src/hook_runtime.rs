//! Runtime bridge between background input events and OpenLogi actions.
//!
//! The CGEventTap hook and the HID++ gesture watcher run outside any UI thread.
//! This module is the shared runtime surface between them and the bound config:
//! the binding map, lazy hook installation, and action dispatch for both hook
//! and gesture events.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, RwLock};
use std::thread;

use openlogi_core::binding::{
    Action, ButtonId, PanAccumulator, PanOutput, SwipeAccumulator, default_binding,
};
use openlogi_hid::CaptureChannel;
use openlogi_hook::{EventDisposition, Hook, MouseEvent};
use tracing::{info, warn};

use crate::DpiCycleState;
use crate::bindings::GestureMode;
use crate::event_monitor::SharedEventMonitor;
use crate::hardware::{toggle_smartshift_in_background, write_dpi_in_background};

/// The two button maps the OS-hook callback reads, kept behind ONE lock so a
/// config rebuild publishes both atomically — a press during an owner switch can
/// never see the new single-action bindings against the old gesture map (or vice
/// versa), and the common case reads one lock instead of two.
#[derive(Default)]
pub struct HookMaps {
    /// Changes on every config/device/application projection rebuild so an
    /// in-flight hold can fail closed even when the resulting binding is equal.
    pub generation: u64,
    /// Per-button single action — the single-action dispatch path.
    pub bindings: BTreeMap<ButtonId, Action>,
    /// Per-direction maps for the OS-hook gesture buttons (Middle/Back/Forward in
    /// gesture mode), so a hold+swipe resolves to a bound action. The dedicated
    /// HID++ gesture button (0x00c3) uses the gesture watcher's separate map
    /// instead — it never reaches the OS hook.
    pub gestures: BTreeMap<ButtonId, GestureMode>,
}

/// Shared, atomically-published [`HookMaps`], threaded between the config owner
/// (orchestrator), the OS-hook callback, and the gesture watcher.
pub type SharedHookMaps = Arc<RwLock<HookMaps>>;

/// Tracks which OS-hook button (Middle/Back/Forward) is mid-hold and defers the
/// swipe detection itself to a shared [`SwipeAccumulator`], which commits a swipe
/// *mid-motion* like the HID++ gesture-button path in `openlogi-hid`. This wrapper
/// adds only the button identity the accumulator doesn't track; a press that
/// never commits a direction is a plain click, fired on release.
enum HoldGesture {
    Directional {
        directions: BTreeMap<openlogi_core::binding::GestureDirection, Action>,
        fallback: Action,
        swipe: SwipeAccumulator,
    },
    Pan {
        click: Action,
        accumulator: PanAccumulator,
    },
}

#[derive(Debug, PartialEq, Eq)]
enum HoldOutput {
    Suppress,
    Action(Action),
    PanDelta { x: i32, y: i32 },
}

#[derive(Default)]
struct HoldState {
    button: Option<ButtonId>,
    generation: u64,
    gesture: Option<HoldGesture>,
}

impl HoldState {
    /// Begin a hold for `button`.
    fn begin(&mut self, button: ButtonId, mode: GestureMode, generation: u64) {
        self.button = Some(button);
        self.generation = generation;
        self.gesture = Some(match mode {
            GestureMode::Directional(directions) => {
                let mut swipe = SwipeAccumulator::default();
                swipe.begin();
                let fallback = directions
                    .get(&openlogi_core::binding::GestureDirection::Click)
                    .cloned()
                    .unwrap_or_else(|| default_binding(button));
                HoldGesture::Directional {
                    directions,
                    fallback,
                    swipe,
                }
            }
            GestureMode::Pan(pan) => {
                let mut accumulator = PanAccumulator::default();
                accumulator.begin();
                HoldGesture::Pan {
                    click: pan.click,
                    accumulator,
                }
            }
        });
    }

    fn mode_matches(&self, button: ButtonId, mode: Option<&GestureMode>, generation: u64) -> bool {
        if self.button != Some(button) || self.generation != generation {
            return false;
        }
        matches!(
            (&self.gesture, mode),
            (
                Some(HoldGesture::Directional { directions, .. }),
                Some(GestureMode::Directional(current)),
            ) if directions == current
        ) || matches!(
            (&self.gesture, mode),
            (
                Some(HoldGesture::Pan { click, .. }),
                Some(GestureMode::Pan(current)),
            ) if click == &current.click
        )
    }

    fn active_button(&self) -> Option<ButtonId> {
        self.button
    }

    /// Feed a pointer-move delta into the active hold, tagging a committed swipe
    /// with the held button. Returns `Some((button, direction))` exactly once per
    /// hold, or `None` while still too short, already fired, or not holding.
    fn accumulate(&mut self, dx: i32, dy: i32) -> HoldOutput {
        match self.gesture.as_mut() {
            Some(HoldGesture::Directional {
                directions,
                fallback,
                swipe,
            }) => swipe
                .accumulate(dx, dy)
                .map_or(HoldOutput::Suppress, |direction| {
                    HoldOutput::Action(
                        directions
                            .get(&direction)
                            .cloned()
                            .unwrap_or_else(|| fallback.clone()),
                    )
                }),
            Some(HoldGesture::Pan { accumulator, .. }) => match accumulator.accumulate(dx, dy) {
                PanOutput::Delta { x, y } => HoldOutput::PanDelta { x, y },
                PanOutput::Idle | PanOutput::Click | PanOutput::End => HoldOutput::Suppress,
            },
            None => HoldOutput::Suppress,
        }
    }

    /// End the hold for `button`. Returns `Some(true)` when it ended a hold that
    /// never committed a swipe (the caller should fire the `Click` action),
    /// `Some(false)` when a swipe already fired, and `None` for a stray release
    /// of a button we weren't holding.
    fn end(&mut self, button: ButtonId) -> Option<HoldOutput> {
        if self.button != Some(button) {
            return None;
        }
        self.button = None;
        let gesture = self.gesture.take()?;
        Some(match gesture {
            HoldGesture::Directional {
                directions,
                mut swipe,
                ..
            } => {
                if swipe.end() {
                    HoldOutput::Action(
                        directions
                            .get(&openlogi_core::binding::GestureDirection::Click)
                            .cloned()
                            .unwrap_or_else(|| default_binding(button)),
                    )
                } else {
                    HoldOutput::Suppress
                }
            }
            HoldGesture::Pan {
                click,
                mut accumulator,
            } => match accumulator.end() {
                PanOutput::Click => HoldOutput::Action(click),
                PanOutput::Idle | PanOutput::Delta { .. } | PanOutput::End => HoldOutput::Suppress,
            },
        })
    }

    /// Cancel any in-progress hold without firing anything — used when the OS
    /// interrupts capture. A dropped button-up would otherwise leave a stale hold
    /// that the next stray pointer move turns into a phantom swipe.
    fn cancel(&mut self) -> HoldOutput {
        self.button = None;
        if let Some(HoldGesture::Pan { accumulator, .. }) = self.gesture.as_mut() {
            let _ = accumulator.cancel();
        }
        self.gesture = None;
        HoldOutput::Suppress
    }
}

/// Atomic two-axis Pan accumulator with a single outstanding worker wake.
#[derive(Default)]
struct PanPending {
    packed: AtomicU64,
    wake_pending: AtomicBool,
}

impl PanPending {
    fn enqueue(&self, x: i32, y: i32) -> bool {
        let mut current = self.packed.load(Ordering::Relaxed);
        loop {
            let (old_x, old_y) = unpack_delta(current);
            let next = pack_delta(old_x.saturating_add(x), old_y.saturating_add(y));
            match self.packed.compare_exchange_weak(
                current,
                next,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        !self.wake_pending.swap(true, Ordering::AcqRel)
    }

    fn take(&self) -> Option<(i32, i32)> {
        let packed = self.packed.swap(0, Ordering::AcqRel);
        (packed != 0).then(|| unpack_delta(packed))
    }

    fn finish_cycle(&self) -> bool {
        self.wake_pending.store(false, Ordering::Release);
        self.packed.load(Ordering::Acquire) != 0 && !self.wake_pending.swap(true, Ordering::AcqRel)
    }

    #[cfg(test)]
    fn drain(&self) -> Option<(i32, i32)> {
        let delta = self.take();
        self.wake_pending.store(false, Ordering::Release);
        delta
    }
}

fn pack_delta(x: i32, y: i32) -> u64 {
    let x = x.to_be_bytes();
    let y = y.to_be_bytes();
    u64::from_be_bytes([x[0], x[1], x[2], x[3], y[0], y[1], y[2], y[3]])
}

fn unpack_delta(packed: u64) -> (i32, i32) {
    let bytes = packed.to_be_bytes();
    (
        i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        i32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
    )
}

/// Non-blocking Pan producer shared by the OS hook and HID++ watcher.
#[derive(Clone)]
pub struct PanEmitter {
    pending: Arc<PanPending>,
    wake: SyncSender<()>,
}

impl PanEmitter {
    /// Spawn the coalescing injection worker.
    #[must_use]
    pub fn new() -> Self {
        let pending = Arc::new(PanPending::default());
        let (wake, receiver) = sync_channel(1);
        let worker_pending = Arc::clone(&pending);
        thread::spawn(move || {
            while receiver.recv().is_ok() {
                loop {
                    if let Some((x, y)) = worker_pending.take() {
                        openlogi_inject::post_pan_scroll(x, y);
                    }
                    if !worker_pending.finish_cycle() {
                        break;
                    }
                }
            }
        });
        Self { pending, wake }
    }

    /// Coalesce `x`/`y` and request at most one bounded worker wake.
    pub fn emit(&self, x: i32, y: i32) {
        if self.pending.enqueue(x, y) {
            let _ = self.wake.try_send(());
        }
    }
}

impl Default for PanEmitter {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    /// In-progress gesture hold, one instance per hook-callback thread: the
    /// single macOS tap thread, or — on Linux — one thread per device, so two
    /// mice never share a hold (a press on one can't hijack the other's swipe).
    /// Thread-local rather than a shared `Mutex` keeps the hot path lock-free and
    /// free of cross-thread contention on the freeze-sensitive callback.
    static HOLD: RefCell<HoldState> = RefCell::new(HoldState::default());
}

/// Attempt to start the OS hook. Returns `None` if Accessibility is not
/// granted or on an unsupported platform — the app continues without crashing.
pub fn start(
    hooks: SharedHookMaps,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture: CaptureChannel,
    monitor: SharedEventMonitor,
    pan_emitter: PanEmitter,
) -> Option<Hook> {
    if !Hook::has_accessibility() {
        warn!(
            "Accessibility not granted — events will not be captured. \
             Open System Settings → Privacy & Security → Accessibility."
        );
        return None;
    }

    let context = HookContext {
        hooks,
        dpi_cycle,
        capture,
        monitor,
        pan_emitter,
    };
    // The per-hold pointer accumulator lives in the thread-local `HOLD`; the
    // callback must never block — see the freeze-hazard note in `macos.rs`.
    let result = Hook::start(move |event| handle_event(&context, &event));

    match result {
        Ok(hook) => {
            info!("OS mouse hook installed");
            Some(hook)
        }
        Err(e) => {
            warn!(error = %e, "could not install OS mouse hook — events will not be captured");
            None
        }
    }
}

struct HookContext {
    hooks: SharedHookMaps,
    dpi_cycle: Arc<RwLock<DpiCycleState>>,
    capture: CaptureChannel,
    monitor: SharedEventMonitor,
    pan_emitter: PanEmitter,
}

fn handle_event(context: &HookContext, event: &MouseEvent) -> EventDisposition {
    context.monitor.record(event);
    match event {
        MouseEvent::Button { id, pressed } => handle_button(context, *id, *pressed),
        MouseEvent::Moved { delta_x, delta_y } => handle_motion(context, *delta_x, *delta_y),
        MouseEvent::CaptureInterrupted => {
            let _ = HOLD.with_borrow_mut(HoldState::cancel);
            EventDisposition::PassThrough
        }
        MouseEvent::Scroll { .. } => EventDisposition::PassThrough,
    }
}

fn handle_button(context: &HookContext, id: ButtonId, pressed: bool) -> EventDisposition {
    if !id.is_os_hook_button() {
        return EventDisposition::PassThrough;
    }
    if pressed {
        let gesture = context.hooks.read().ok().and_then(|maps| {
            maps.gestures
                .get(&id)
                .cloned()
                .map(|mode| (maps.generation, mode))
        });
        if let Some((generation, gesture)) = gesture {
            HOLD.with_borrow_mut(|hold| hold.begin(id, gesture, generation));
            return EventDisposition::Suppress;
        }
    } else {
        let (generation, current) = context.hooks.read().ok().map_or((0, None), |maps| {
            (maps.generation, maps.gestures.get(&id).cloned())
        });
        let matches = HOLD.with_borrow(|hold| hold.mode_matches(id, current.as_ref(), generation));
        if !matches && HOLD.with_borrow(|hold| hold.active_button() == Some(id)) {
            let _ = HOLD.with_borrow_mut(HoldState::cancel);
            return EventDisposition::Suppress;
        }
        if let Some(output) = HOLD.with_borrow_mut(|hold| hold.end(id)) {
            if let HoldOutput::Action(action) = output {
                dispatch_action(&action, &context.dpi_cycle, &context.capture);
            }
            return EventDisposition::Suppress;
        }
    }

    let action = context
        .hooks
        .read()
        .ok()
        .and_then(|maps| maps.bindings.get(&id).cloned());
    let Some(action) = action else {
        return EventDisposition::PassThrough;
    };
    if is_native_click(id, &action) {
        return EventDisposition::PassThrough;
    }
    if pressed {
        info!(button = %id, action = %action.label(), "button → executing bound action");
        dispatch_action(&action, &context.dpi_cycle, &context.capture);
    }
    EventDisposition::Suppress
}

fn handle_motion(context: &HookContext, delta_x: i32, delta_y: i32) -> EventDisposition {
    let Some(button) = HOLD.with_borrow(HoldState::active_button) else {
        return EventDisposition::PassThrough;
    };
    let (generation, current) = context.hooks.read().ok().map_or((0, None), |maps| {
        (maps.generation, maps.gestures.get(&button).cloned())
    });
    if !HOLD.with_borrow(|hold| hold.mode_matches(button, current.as_ref(), generation)) {
        let _ = HOLD.with_borrow_mut(HoldState::cancel);
        return EventDisposition::PassThrough;
    }
    match HOLD.with_borrow_mut(|hold| hold.accumulate(delta_x, delta_y)) {
        HoldOutput::Action(action) => {
            info!(button = %button, action = %action.label(), "gesture swipe → executing bound action");
            dispatch_action(&action, &context.dpi_cycle, &context.capture);
            EventDisposition::PassThrough
        }
        HoldOutput::PanDelta { x, y } => {
            context.pan_emitter.emit(x, y);
            EventDisposition::Suppress
        }
        HoldOutput::Suppress if matches!(current, Some(GestureMode::Pan(_))) => {
            EventDisposition::Suppress
        }
        HoldOutput::Suppress => EventDisposition::PassThrough,
    }
}

/// Whether `action` is just `id`'s own native event — i.e. the button is mapped
/// to the very click (or extra-button press) it already produces. In that case
/// the hook should pass the event through to the OS rather than suppress and
/// re-synthesise it. For Back/Forward this keeps the genuine hardware button
/// 4/5 intact instead of round-tripping it through synthesis.
fn is_native_click(id: ButtonId, action: &Action) -> bool {
    matches!(
        (id, action),
        (ButtonId::LeftClick, Action::LeftClick)
            | (ButtonId::RightClick, Action::RightClick)
            | (ButtonId::MiddleClick, Action::MiddleClick)
            | (ButtonId::Back, Action::MouseBack)
            | (ButtonId::Forward, Action::MouseForward)
    )
}

/// Route a bound action either to OS-level event synthesis
/// ([`Action::execute`]) or to one of OpenLogi's hardware-side handlers.
///
/// `dpi_cycle` is held across a write lock long enough to advance the index
/// and snapshot the new DPI + target; the actual HID write spawns its own
/// thread via [`write_dpi_in_background`] to keep event callbacks non-blocking.
/// `capture` lets those writes reuse the capture session's open channel.
pub fn dispatch_action(
    action: &Action,
    dpi_cycle: &Arc<RwLock<DpiCycleState>>,
    capture: &CaptureChannel,
) {
    let next = match action {
        Action::CycleDpiPresets => match dpi_cycle.write() {
            Ok(mut guard) => guard.cycle(),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — cycle skipped");
                None
            }
        },
        Action::SetDpiPreset(i) => match dpi_cycle.write() {
            Ok(mut guard) => guard.set(usize::from(*i)),
            Err(e) => {
                warn!(error = %e, "dpi_cycle lock poisoned — set skipped");
                None
            }
        },
        Action::ToggleSmartShift => {
            let target = dpi_cycle.read().ok().and_then(|g| g.target.clone());
            info!("SmartShift toggle → flipping wheel mode");
            toggle_smartshift_in_background(Some(capture), target);
            return;
        }
        other => {
            openlogi_inject::execute(other);
            None
        }
    };
    if let Some((dpi, target)) = next {
        info!(dpi, "DPI action → writing to device");
        write_dpi_in_background(Some(capture), target, dpi);
    } else if matches!(action, Action::CycleDpiPresets | Action::SetDpiPreset(_)) {
        info!(
            action = %action.label(),
            "no DPI presets configured for active device — press ignored"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openlogi_core::binding::{GESTURE_SWIPE_THRESHOLD, PAN_DEADZONE, PanBinding};

    #[test]
    fn pan_hold_suppresses_motion_and_clicks_only_inside_deadzone() {
        let mut hold = HoldState::default();
        hold.begin(
            ButtonId::Back,
            GestureMode::Pan(PanBinding {
                click: Action::SmartZoom,
            }),
            1,
        );

        assert_eq!(hold.accumulate(PAN_DEADZONE - 1, 0), HoldOutput::Suppress);
        assert_eq!(
            hold.end(ButtonId::Back),
            Some(HoldOutput::Action(Action::SmartZoom))
        );

        hold.begin(
            ButtonId::Back,
            GestureMode::Pan(PanBinding {
                click: Action::SmartZoom,
            }),
            1,
        );
        assert_eq!(
            hold.accumulate(PAN_DEADZONE, -7),
            HoldOutput::PanDelta {
                x: PAN_DEADZONE,
                y: -7,
            }
        );
        assert_eq!(hold.end(ButtonId::Back), Some(HoldOutput::Suppress));
    }

    #[test]
    fn pan_pending_coalesces_two_axes_behind_one_wake() {
        let pending = PanPending::default();
        assert!(pending.enqueue(3, -4), "the first delta requests a wake");
        assert!(
            !pending.enqueue(-1, 9),
            "a pending wake coalesces later motion"
        );
        assert_eq!(pending.drain(), Some((2, 5)));
        assert!(pending.enqueue(7, 8), "draining re-arms the next wake");
    }

    #[test]
    fn cancelling_pan_resets_without_click() {
        let mut hold = HoldState::default();
        hold.begin(
            ButtonId::Back,
            GestureMode::Pan(PanBinding {
                click: Action::SmartZoom,
            }),
            1,
        );
        assert_eq!(hold.cancel(), HoldOutput::Suppress);
        assert_eq!(hold.end(ButtonId::Back), None);
    }

    #[test]
    fn projection_generation_change_cancels_an_equal_pan_binding() {
        let mode = GestureMode::Pan(PanBinding {
            click: Action::SmartZoom,
        });
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back, mode.clone(), 7);
        assert!(!hold.mode_matches(ButtonId::Back, Some(&mode), 8));
        let _ = hold.cancel();
        assert_eq!(hold.end(ButtonId::Back), None);
    }

    #[test]
    fn os_hook_pan_suppresses_press_and_motion_until_interrupted() {
        let context = HookContext {
            hooks: Arc::new(RwLock::new(HookMaps {
                generation: 1,
                bindings: BTreeMap::new(),
                gestures: BTreeMap::from([(
                    ButtonId::Back,
                    GestureMode::Pan(PanBinding {
                        click: Action::None,
                    }),
                )]),
            })),
            dpi_cycle: Arc::new(RwLock::new(DpiCycleState::default())),
            capture: Arc::new(RwLock::new(None)),
            monitor: Arc::new(crate::event_monitor::EventMonitor::default()),
            pan_emitter: PanEmitter::new(),
        };

        assert_eq!(
            handle_event(
                &context,
                &MouseEvent::Button {
                    id: ButtonId::Back,
                    pressed: true,
                },
            ),
            EventDisposition::Suppress
        );
        assert_eq!(
            handle_event(
                &context,
                &MouseEvent::Moved {
                    delta_x: 1,
                    delta_y: -1,
                },
            ),
            EventDisposition::Suppress
        );
        assert_eq!(
            handle_event(&context, &MouseEvent::CaptureInterrupted),
            EventDisposition::PassThrough
        );
    }

    // The mid-swipe gate itself is unit-tested on `SwipeAccumulator` in
    // `openlogi-core`; these cover only what `HoldState` adds on top — tagging a
    // commit with the held button, and matching the button on release.

    #[test]
    fn accumulate_tags_a_committed_swipe_with_the_held_button() {
        let mut hold = HoldState::default();
        hold.begin(
            ButtonId::Back,
            GestureMode::Directional(BTreeMap::from([(
                openlogi_core::binding::GestureDirection::Right,
                Action::Copy,
            )])),
            1,
        );
        let Some(HoldGesture::Directional { swipe, .. }) = hold.gesture.as_mut() else {
            panic!("directional hold");
        };
        swipe.backdate_hold_for_test();

        // A clear rightward swipe commits, tagged with the held button.
        assert_eq!(
            hold.accumulate(GESTURE_SWIPE_THRESHOLD + 10, 0),
            HoldOutput::Action(Action::Copy)
        );
        assert_eq!(
            hold.accumulate(50, 0),
            HoldOutput::Suppress,
            "commits at most once per hold"
        );
        // A release after a committed swipe is NOT a click.
        assert_eq!(hold.end(ButtonId::Back), Some(HoldOutput::Suppress));
    }

    #[test]
    fn sparse_directional_map_preserves_click_fallback() {
        let mut hold = HoldState::default();
        hold.begin(
            ButtonId::Back,
            GestureMode::Directional(BTreeMap::from([(
                openlogi_core::binding::GestureDirection::Click,
                Action::Copy,
            )])),
            1,
        );
        let Some(HoldGesture::Directional { swipe, .. }) = hold.gesture.as_mut() else {
            panic!("directional hold");
        };
        swipe.backdate_hold_for_test();
        assert_eq!(
            hold.accumulate(-(GESTURE_SWIPE_THRESHOLD + 10), 0),
            HoldOutput::Action(Action::Copy)
        );
    }

    #[test]
    fn end_matches_the_held_button() {
        let mut hold = HoldState::default();
        hold.begin(ButtonId::Back, GestureMode::Directional(BTreeMap::new()), 1);
        // A stray release of a button we weren't holding is ignored...
        assert_eq!(hold.end(ButtonId::Forward), None);
        // ...and ending the held button with no swipe is a plain click.
        assert_eq!(
            hold.end(ButtonId::Back),
            Some(HoldOutput::Action(default_binding(ButtonId::Back)))
        );
    }
}
