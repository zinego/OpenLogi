//! Live control capture for one device: divert the MX dedicated gesture button, the
//! DPI/ModeShift button, and the thumb wheel over HID++ and turn their events
//! into [`CapturedInput`] the GUI can dispatch.
//!
//! [`run_capture_session`] holds a single HID++ channel open for one device,
//! enables diversion on whichever of those controls it exposes, registers one
//! message listener, and restores every control's default mapping on shutdown.
//! Using one channel matters: a second channel to the same device would split
//! its input-report stream, so all captured controls share this session.
//!
//! The session is transport-only — it has no opinion on what an input *does*.
//! In particular, it preserves the gesture button's raw press/motion/release
//! lifecycle so the agent watcher's [`openlogi_core::binding::SwipeAccumulator`]
//! can interpret it without HID transport policy.
//! The thumb wheel is special: diverting it stops native horizontal scroll, so
//! the agent re-synthesises scroll from the [`CapturedInput::Scroll`] deltas —
//! the wheel is therefore only diverted when the user's thumbwheel config
//! leaves its defaults (click bound, rotation rebound, or sensitivity changed).

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use hidpp::{channel::HidppChannel, device::Device, protocol::v20};
use openlogi_core::binding::ButtonId;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

use crate::reprog_controls::{self, RawControlEvent, ReprogControlsV4};
use crate::route::{DeviceRoute, open_route_channel};
use crate::thumbwheel::{self, Thumbwheel};
use crate::write::SharedChannel;

/// Shared slot holding the active capture session's open channel, so DPI /
/// SmartShift writes can reuse it instead of opening a fresh one. `None`
/// whenever no session is connected.
pub type CaptureChannel = Arc<RwLock<Option<SharedChannel>>>;

/// Controls that one live HID++ session should divert from their native
/// firmware behavior.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureRequest {
    /// Whether to divert the thumb wheel for click and rotation events.
    pub capture_thumbwheel: bool,
    /// Logical buttons whose HID++ controls should be diverted with raw-XY
    /// reporting when the device advertises both required capabilities.
    pub gesture_buttons: BTreeSet<ButtonId>,
}

/// One input captured from the active device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapturedInput {
    /// A diverted gesture control transitioned from released to pressed.
    GesturePressed(ButtonId),
    /// Raw signed movement while one unambiguous gesture control is held.
    GestureMotion {
        /// Logical button whose diverted control owns this motion.
        button: ButtonId,
        /// Horizontal delta (`+` = right, in the device's raw units).
        delta_x: i16,
        /// Vertical delta (`+` = down, in the device's raw units).
        delta_y: i16,
    },
    /// A diverted gesture control transitioned from pressed to released.
    GestureReleased(ButtonId),
    /// A gesture became ambiguous or its capture session ended while held.
    GestureCancelled(ButtonId),
    /// A diverted button was pressed — the DPI/ModeShift button
    /// ([`ButtonId::DpiToggle`]) or the thumb-wheel single tap
    /// ([`ButtonId::Thumbwheel`]).
    ButtonPressed(ButtonId),
    /// Thumb-wheel rotation to re-synthesise as horizontal scroll, in the
    /// wheel's `diverted_res` increments. Emitted while the wheel is diverted
    /// (click bound, rotation rebound, or sensitivity changed).
    Scroll(i16),
}

/// Why a capture session could not start (or had to stop).
#[derive(Debug, Error)]
pub enum GestureError {
    /// HID transport-level failure while enumerating or opening the device.
    #[error("HID transport error")]
    Hid(#[from] async_hid::HidError),
    /// No connected device matched the capture route.
    #[error("no connected device matched the capture route")]
    DeviceNotFound,
    /// The device at the target index did not answer HID++.
    #[error("device at index {0:#04x} did not respond to HID++")]
    DeviceUnreachable(u8),
    /// A HID++ feature call returned an error; inner string carries context.
    #[error("HID++ protocol error: {0}")]
    Hidpp(String),
}

/// Movement + button state accumulated across messages. Lives behind a `Mutex`
/// because the channel's read thread invokes the listener by shared reference.
#[derive(Default)]
struct CaptureAccum {
    /// Whether teardown has closed this capture session. Listener callbacks
    /// cloned before guard removal must not emit after this becomes true.
    closed: bool,
    /// The sole gesture control currently allowed to own raw-XY events.
    active_gesture: Option<ButtonId>,
    /// Whether any DPI/ModeShift control was held in the last event — for
    /// rising-edge press detection.
    dpi_down: bool,
}

/// Capture the requested gesture buttons, DPI/ModeShift button, and optional
/// thumb wheel on `route` until `shutdown` resolves,
/// forwarding each event to `sink`.
///
/// Each requested gesture control is diverted with raw-XY reporting only when
/// its `0x1b04` capability entry advertises both temporary diversion and raw
/// XY. Controls absent from the request retain their native behavior. The
/// DPI/ModeShift capture and the channel-reuse slot are independent of this.
///
/// Opens and holds one HID++ channel, diverts whichever of those controls the
/// device exposes, and listens. Returns once `shutdown` fires (or its sender is
/// dropped), after restoring every diverted control. Setup errors are returned;
/// failures to restore on the way out are logged, not propagated.
pub async fn run_capture_session(
    route: DeviceRoute,
    request: CaptureRequest,
    sink: mpsc::UnboundedSender<CapturedInput>,
    shutdown: oneshot::Receiver<()>,
    channel_slot: CaptureChannel,
) -> Result<(), GestureError> {
    let chan = open_route_channel(&route)
        .await?
        .ok_or(GestureError::DeviceNotFound)?;
    let device_index = route.device_index();
    let armed = arm_controls(&chan, device_index, &request).await?;

    // Publish this device's open channel so DPI/SmartShift writes reuse it
    // instead of opening their own. Cleared on the way out.
    if let Ok(mut slot) = channel_slot.write() {
        *slot = Some(SharedChannel::new(Arc::clone(&chan), route.clone()));
    }

    let accum = Arc::new(Mutex::new(CaptureAccum::default()));
    let reprog_index = armed.reprog.as_ref().map(|(_, idx)| *idx);
    let thumb_index = armed.thumb.as_ref().map(|(_, idx)| *idx);
    let dpi_set = armed.dpi_cids.clone();
    let gesture_controls = armed.gesture_controls.clone();
    let listener = chan.add_msg_listener_guarded({
        let accum = Arc::clone(&accum);
        let sink = sink.clone();
        move |raw, matched| {
            if matched {
                return;
            }
            let msg = v20::Message::from(raw);
            // Teardown and every event emission are serialized by this lock:
            // a callback cloned before listener removal either completes first
            // or observes `closed` and emits nothing.
            let mut acc = accum.lock().unwrap_or_else(PoisonError::into_inner);
            if acc.closed {
                return;
            }
            if let Some(idx) = reprog_index
                && let Some(event) = reprog_controls::decode_event(&msg, device_index, idx)
            {
                handle_reprog(&mut acc, event, &gesture_controls, &dpi_set, &sink);
                return;
            }
            if let Some(idx) = thumb_index
                && let Some(event) = thumbwheel::decode_event(&msg, device_index, idx)
            {
                if event.single_tap {
                    let _ = sink.send(CapturedInput::ButtonPressed(ButtonId::Thumbwheel));
                }
                if event.rotation != 0 {
                    let _ = sink.send(CapturedInput::Scroll(event.rotation));
                }
            }
        }
    });

    info!(
        index = device_index,
        gesture_buttons = armed.gesture_controls.len(),
        dpi_buttons = armed.dpi_cids.len(),
        thumbwheel = armed.thumb.is_some(),
        "control capture active"
    );
    let _ = shutdown.await;

    drop(listener);
    // No falling-edge report can arrive after the listener is removed. Close
    // an in-flight lifecycle explicitly so downstream gesture state cannot
    // remain stuck when the session is stopped or otherwise torn down.
    {
        let mut acc = accum.lock().unwrap_or_else(PoisonError::into_inner);
        close_capture(&mut acc, &sink);
    }
    if let Ok(mut slot) = channel_slot.write() {
        *slot = None;
    }
    armed.disarm().await;
    debug!(index = device_index, "control capture stopped");
    Ok(())
}

/// The set of controls a session has diverted, kept so they can be handed back
/// to the firmware on teardown.
struct ArmedControls {
    /// `0x1b04` accessor + feature index, present when the device exposes it.
    reprog: Option<(ReprogControlsV4, u8)>,
    /// Diverted raw-XY control IDs and their logical button identities.
    gesture_controls: BTreeMap<u16, ButtonId>,
    /// DPI/ModeShift CIDs diverted as plain buttons.
    dpi_cids: Vec<u16>,
    /// `0x2150` accessor + feature index, present when the thumb wheel is
    /// diverted.
    thumb: Option<(Thumbwheel, u8)>,
    /// Every control enabled by the transaction, in enable order.
    capture_controls: Vec<CaptureControl>,
}

impl ArmedControls {
    /// Restore every diverted control. Failures are logged, not propagated.
    async fn disarm(&self) {
        for &control in self.capture_controls.iter().rev() {
            restore(self.set_reporting(control, false).await, control.label());
        }
    }

    /// Apply one reporting state through the accessor for its feature.
    async fn set_reporting(
        &self,
        control: CaptureControl,
        enabled: bool,
    ) -> Result<(), GestureError> {
        set_reporting_on(
            self.reprog.as_ref().map(|(rc, _)| rc),
            self.thumb.as_ref().map(|(tw, _)| tw),
            control,
            enabled,
        )
        .await
    }
}

/// One reporting control participating in the all-or-nothing arm transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureControl {
    /// A `0x1b04` reprogrammable control and whether it needs raw XY.
    Reprog { cid: u16, raw_xy: bool },
    /// The `0x2150` thumb-wheel event stream.
    Thumbwheel,
}

impl CaptureControl {
    /// Diagnostic label used when a best-effort restore fails.
    const fn label(self) -> &'static str {
        match self {
            Self::Reprog { raw_xy: true, .. } => "gesture button",
            Self::Reprog { raw_xy: false, .. } => "DPI button",
            Self::Thumbwheel => "thumb wheel",
        }
    }
}

/// Resolve features off the device's root and divert the controls we capture:
/// requested gesture buttons (raw-XY) and DPI/ModeShift buttons over `0x1b04`,
/// and the optionally requested thumb wheel over `0x2150`. The
/// root-feature lookup mirrors `write::open_feature`,
/// since hidpp 0.2's registry doesn't carry the features OpenLogi reimplements.
async fn arm_controls(
    chan: &Arc<HidppChannel>,
    slot: u8,
    request: &CaptureRequest,
) -> Result<ArmedControls, GestureError> {
    let device = Device::new(Arc::clone(chan), slot)
        .await
        .map_err(|_| GestureError::DeviceUnreachable(slot))?;

    let mut reprog: Option<(ReprogControlsV4, u8)> = None;
    let mut gesture_controls = BTreeMap::new();
    let mut dpi_cids: Vec<u16> = Vec::new();
    if let Some(info) = device
        .root()
        .get_feature(reprog_controls::FEATURE_ID)
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
    {
        let rc = ReprogControlsV4::new(Arc::clone(chan), slot, info.index);
        let controls = enumerate_controls(&rc).await?;

        gesture_controls = select_gesture_controls(&request.gesture_buttons, &controls);
        for &cid in &reprog_controls::DPI_MODE_SHIFT_CIDS {
            if controls.iter().any(|c| c.cid == cid && c.is_divertable()) {
                dpi_cids.push(cid);
            }
        }
        reprog = Some((rc, info.index));
    }

    let mut thumb: Option<(Thumbwheel, u8)> = None;
    if request.capture_thumbwheel
        && let Some(info) = device
            .root()
            .get_feature(thumbwheel::FEATURE_ID)
            .await
            .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?
    {
        let tw = Thumbwheel::new(Arc::clone(chan), slot, info.index);
        // Consume the getInfo error here, before the next await: Hidpp20Error
        // isn't Send, so holding it across an await would make this future
        // (spawned on tokio) non-Send.
        let supports_single_tap = match tw.get_info().await {
            Ok(twinfo) => twinfo.supports_single_tap,
            Err(e) => {
                warn!(error = ?e, "thumb wheel getInfo failed");
                false
            }
        };
        // Divert whenever capture was requested: rotation rebinds and the
        // sensitivity multiplier need the diverted event stream even on wheels
        // that report no single-tap capability (e.g. MX Master 4) — lacking the
        // tap only means a bound click can never fire.
        if !supports_single_tap {
            debug!("thumb wheel reports no single tap — click not capturable");
        }
        thumb = Some((tw, info.index));
    }

    let capture_controls = gesture_controls
        .keys()
        .copied()
        .map(|cid| CaptureControl::Reprog { cid, raw_xy: true })
        .chain(
            dpi_cids
                .iter()
                .copied()
                .map(|cid| CaptureControl::Reprog { cid, raw_xy: false }),
        )
        .chain(thumb.is_some().then_some(CaptureControl::Thumbwheel))
        .collect::<Vec<_>>();

    let reprog_reporting = reprog.as_ref().map(|(rc, _)| rc.clone());
    let thumb_reporting = thumb.as_ref().map(|(tw, _)| tw.clone());
    set_capture_reporting_transactionally(
        capture_controls.iter().copied(),
        move |control, enabled| {
            let reprog_reporting = reprog_reporting.clone();
            let thumb_reporting = thumb_reporting.clone();
            async move {
                set_reporting_on(
                    reprog_reporting.as_ref(),
                    thumb_reporting.as_ref(),
                    control,
                    enabled,
                )
                .await
            }
        },
    )
    .await?;

    if gesture_controls.is_empty() && dpi_cids.is_empty() && thumb.is_none() {
        debug!(slot, "no capturable controls — idle session");
    }
    Ok(ArmedControls {
        reprog,
        gesture_controls,
        dpi_cids,
        thumb,
        capture_controls,
    })
}

/// Apply one reporting state using the appropriate HID++ feature accessor.
async fn set_reporting_on(
    reprog: Option<&ReprogControlsV4>,
    thumb: Option<&Thumbwheel>,
    control: CaptureControl,
    enabled: bool,
) -> Result<(), GestureError> {
    match control {
        CaptureControl::Reprog { cid, raw_xy } => {
            let Some(reprog) = reprog else {
                return Err(GestureError::Hidpp(
                    "missing reprogrammable-controls accessor".to_owned(),
                ));
            };
            reprog
                .set_cid_reporting(cid, enabled, enabled && raw_xy)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))
        }
        CaptureControl::Thumbwheel => {
            let Some(thumb) = thumb else {
                return Err(GestureError::Hidpp(
                    "missing thumb-wheel accessor".to_owned(),
                ));
            };
            thumb
                .set_reporting(enabled, false)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))
        }
    }
}

/// Log (don't propagate) a failure to hand a control back to the firmware.
fn restore<E: std::fmt::Display>(result: Result<(), E>, what: &str) {
    if let Err(e) = result {
        warn!(error = %e, control = what, "failed to restore control mapping on shutdown");
    }
}

/// Read the device's full reprogrammable-control table in one pass, so we can
/// test several CIDs without rescanning per control.
async fn enumerate_controls(
    rc: &ReprogControlsV4,
) -> Result<Vec<reprog_controls::CtrlIdInfo>, GestureError> {
    let count = rc
        .get_count()
        .await
        .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?;
    let mut controls = Vec::with_capacity(usize::from(count));
    for index in 0..count {
        controls.push(
            rc.get_ctrl_id_info(index)
                .await
                .map_err(|e| GestureError::Hidpp(format!("{e:?}")))?,
        );
    }
    Ok(controls)
}

/// Intersect the requested logical gesture buttons with controls that the
/// current device can both temporarily divert and report as raw XY.
fn select_gesture_controls(
    requested: &BTreeSet<ButtonId>,
    controls: &[reprog_controls::CtrlIdInfo],
) -> BTreeMap<u16, ButtonId> {
    controls
        .iter()
        .filter(|control| control.is_divertable() && control.supports_raw_xy())
        .filter_map(|control| {
            let button = reprog_controls::gesture_button_for_cid(control.cid)?;
            requested.contains(&button).then_some((control.cid, button))
        })
        .collect()
}

/// Enable all requested reporting as one best-effort transaction. A failed
/// enable is disabled too because firmware may have applied the request even
/// when the response was lost; then every confirmed prior enable is restored
/// in reverse order before the original error returns.
async fn set_capture_reporting_transactionally<E, I, Set, SetFuture>(
    controls: I,
    mut set: Set,
) -> Result<(), E>
where
    I: IntoIterator<Item = CaptureControl>,
    Set: FnMut(CaptureControl, bool) -> SetFuture,
    SetFuture: Future<Output = Result<(), E>>,
{
    let mut armed = Vec::new();
    for control in controls {
        if let Err(error) = set(control, true).await {
            // The device can apply a reporting change even when its response
            // is lost, so restore the failed control as well as confirmed ones.
            let _ = set(control, false).await;
            for prior in armed.into_iter().rev() {
                let _ = set(prior, false).await;
            }
            return Err(error);
        }
        armed.push(control);
    }
    Ok(())
}

/// Update `acc` and emit the button-keyed raw gesture lifecycle plus a
/// [`ButtonId::DpiToggle`] press on the rising edge of any diverted
/// DPI/ModeShift control.
fn handle_reprog(
    acc: &mut CaptureAccum,
    event: RawControlEvent,
    gesture_controls: &BTreeMap<u16, ButtonId>,
    dpi_cids: &[u16],
    sink: &mpsc::UnboundedSender<CapturedInput>,
) {
    if acc.closed {
        return;
    }
    match event {
        RawControlEvent::DivertedButtons(cids) => {
            let mut held = gesture_controls
                .iter()
                .filter_map(|(cid, button)| cids.contains(cid).then_some(*button));
            let first = held.next();
            let next = if held.next().is_none() { first } else { None };
            let ambiguous = first.is_some() && next.is_none();

            if ambiguous {
                if let Some(active) = acc.active_gesture.take() {
                    let _ = sink.send(CapturedInput::GestureCancelled(active));
                }
            } else if next != acc.active_gesture {
                if let Some(active) = acc.active_gesture.take() {
                    let end = if next.is_some() {
                        CapturedInput::GestureCancelled(active)
                    } else {
                        CapturedInput::GestureReleased(active)
                    };
                    let _ = sink.send(end);
                }
                if let Some(button) = next {
                    acc.active_gesture = Some(button);
                    let _ = sink.send(CapturedInput::GesturePressed(button));
                }
            }

            let dpi_down = dpi_cids.iter().any(|cid| cids.contains(cid));
            if dpi_down && !acc.dpi_down {
                let _ = sink.send(CapturedInput::ButtonPressed(ButtonId::DpiToggle));
            }
            acc.dpi_down = dpi_down;
        }
        RawControlEvent::RawXy { dx, dy } => {
            if let Some(button) = acc.active_gesture {
                let _ = sink.send(CapturedInput::GestureMotion {
                    button,
                    delta_x: dx,
                    delta_y: dy,
                });
            }
        }
    }
}

/// Atomically close the capture and end an in-flight gesture without pretending
/// the missing falling edge was a normal release.
fn close_capture(acc: &mut CaptureAccum, sink: &mpsc::UnboundedSender<CapturedInput>) {
    if acc.closed {
        return;
    }
    acc.closed = true;
    if let Some(button) = acc.active_gesture.take() {
        let _ = sink.send(CapturedInput::GestureCancelled(button));
    }
}
#[cfg(test)]
mod tests;
