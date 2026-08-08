use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use super::*;

fn gesture_controls(buttons: &[ButtonId]) -> BTreeMap<u16, ButtonId> {
    buttons
        .iter()
        .copied()
        .filter_map(|button| {
            reprog_controls::gesture_cid_for_button(button).map(|cid| (cid, button))
        })
        .collect()
}

fn diverted(cids: &[u16]) -> RawControlEvent {
    let mut report = [0; 4];
    report[..cids.len()].copy_from_slice(cids);
    RawControlEvent::DivertedButtons(report)
}

#[test]
fn back_reports_press_motion_and_release_with_its_identity() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = gesture_controls(&[ButtonId::Back]);
    let back = reprog_controls::BACK_BUTTON_CID;
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, diverted(&[back]), &gestures, &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 300, dy: -400 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -120, dy: 5 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(&mut acc, diverted(&[]), &gestures, &[], &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed(ButtonId::Back)),
            Ok(CapturedInput::GestureMotion {
                button: ButtonId::Back,
                delta_x: -120,
                delta_y: 5,
            }),
            Ok(CapturedInput::GestureReleased(ButtonId::Back)),
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn forward_reports_press_motion_and_release_with_its_identity() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = gesture_controls(&[ButtonId::Forward]);
    let forward = reprog_controls::FORWARD_BUTTON_CID;
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, diverted(&[forward]), &gestures, &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -300, dy: 400 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 10, dy: -20 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(&mut acc, diverted(&[]), &gestures, &[], &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed(ButtonId::Forward)),
            Ok(CapturedInput::GestureMotion {
                button: ButtonId::Forward,
                delta_x: 10,
                delta_y: -20,
            }),
            Ok(CapturedInput::GestureReleased(ButtonId::Forward)),
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn first_raw_xy_after_press_is_discarded_as_stale_device_accumulation() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = gesture_controls(&[ButtonId::Forward]);
    let forward = reprog_controls::FORWARD_BUTTON_CID;
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, diverted(&[forward]), &gestures, &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -236, dy: -406 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -1, dy: 2 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(&mut acc, diverted(&[]), &gestures, &[], &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed(ButtonId::Forward)),
            Ok(CapturedInput::GestureMotion {
                button: ButtonId::Forward,
                delta_x: -1,
                delta_y: 2,
            }),
            Ok(CapturedInput::GestureReleased(ButtonId::Forward)),
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn an_unrequested_gesture_cid_is_not_captured() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = gesture_controls(&[ButtonId::Back]);
    let forward = reprog_controls::FORWARD_BUTTON_CID;
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, diverted(&[forward]), &gestures, &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 3, dy: 4 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(&mut acc, diverted(&[]), &gestures, &[], &tx);

    assert!(rx.try_recv().is_err());
}

#[test]
fn switching_gesture_controls_cancels_the_old_button_before_pressing_the_new_one() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = gesture_controls(&[ButtonId::Back, ButtonId::Forward]);
    let back = reprog_controls::BACK_BUTTON_CID;
    let forward = reprog_controls::FORWARD_BUTTON_CID;
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, diverted(&[back]), &gestures, &[], &tx);
    handle_reprog(&mut acc, diverted(&[forward]), &gestures, &[], &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed(ButtonId::Back)),
            Ok(CapturedInput::GestureCancelled(ButtonId::Back)),
            Ok(CapturedInput::GesturePressed(ButtonId::Forward)),
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn simultaneous_gesture_controls_cancel_and_ignore_ambiguous_raw_xy() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = gesture_controls(&[ButtonId::Back, ButtonId::Forward]);
    let back = reprog_controls::BACK_BUTTON_CID;
    let forward = reprog_controls::FORWARD_BUTTON_CID;
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, diverted(&[back]), &gestures, &[], &tx);
    handle_reprog(&mut acc, diverted(&[back, forward]), &gestures, &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 40, dy: 60 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(&mut acc, diverted(&[forward]), &gestures, &[], &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed(ButtonId::Back)),
            Ok(CapturedInput::GestureCancelled(ButtonId::Back)),
            Ok(CapturedInput::GesturePressed(ButtonId::Forward)),
        ]
    );
    assert!(
        rx.try_recv().is_err(),
        "raw XY with two held gesture controls has no reliable source"
    );
}

#[test]
fn closing_an_active_capture_cancels_its_button_once_and_ignores_late_events() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = gesture_controls(&[ButtonId::Forward]);
    let forward = reprog_controls::FORWARD_BUTTON_CID;
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, diverted(&[forward]), &gestures, &[], &tx);
    close_capture(&mut acc, &tx);
    close_capture(&mut acc, &tx);
    handle_reprog(&mut acc, diverted(&[forward]), &gestures, &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 4, dy: -7 },
        &gestures,
        &[],
        &tx,
    );
    handle_reprog(&mut acc, diverted(&[]), &gestures, &[], &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed(ButtonId::Forward)),
            Ok(CapturedInput::GestureCancelled(ButtonId::Forward)),
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn selection_requires_request_diversion_and_raw_xy_capability() {
    let requested = BTreeSet::from([ButtonId::Back, ButtonId::Forward]);
    let back = reprog_controls::BACK_BUTTON_CID;
    let forward = reprog_controls::FORWARD_BUTTON_CID;
    let controls = [
        reprog_controls::CtrlIdInfo {
            cid: back,
            task_id: 0,
            flags: (reprog_controls::CidFlags::DIVERTABLE | reprog_controls::CidFlags::RAW_XY)
                .raw(),
        },
        reprog_controls::CtrlIdInfo {
            cid: forward,
            task_id: 0,
            flags: reprog_controls::CidFlags::DIVERTABLE.raw(),
        },
        reprog_controls::CtrlIdInfo {
            cid: reprog_controls::MIDDLE_BUTTON_CID,
            task_id: 0,
            flags: (reprog_controls::CidFlags::DIVERTABLE | reprog_controls::CidFlags::RAW_XY)
                .raw(),
        },
    ];

    assert_eq!(
        select_gesture_controls(&requested, &controls),
        BTreeMap::from([(back, ButtonId::Back)])
    );
}

#[tokio::test]
async fn gesture_reporting_rolls_back_prior_cids_when_a_later_enable_fails() {
    let back = CaptureControl::Reprog {
        cid: reprog_controls::BACK_BUTTON_CID,
        raw_xy: true,
    };
    let forward = CaptureControl::Reprog {
        cid: reprog_controls::FORWARD_BUTTON_CID,
        raw_xy: true,
    };
    let calls = RefCell::new(Vec::new());
    let outcome = RefCell::new([Ok(()), Err("forward enable failed"), Ok(())].into_iter());

    let result = set_capture_reporting_transactionally([back, forward], |control, enabled| {
        calls.borrow_mut().push((control, enabled));
        std::future::ready(outcome.borrow_mut().next().unwrap_or(Ok(())))
    })
    .await;

    assert_eq!(result, Err("forward enable failed"));
    assert_eq!(
        calls.into_inner(),
        [
            (back, true),
            (forward, true),
            (forward, false),
            (back, false),
        ]
    );
}

#[tokio::test]
async fn dpi_enable_failure_restores_itself_and_an_armed_gesture() {
    let gesture = CaptureControl::Reprog {
        cid: reprog_controls::BACK_BUTTON_CID,
        raw_xy: true,
    };
    let dpi = CaptureControl::Reprog {
        cid: reprog_controls::DPI_MODE_SHIFT_CIDS[0],
        raw_xy: false,
    };
    let calls = RefCell::new(Vec::new());
    let outcome = RefCell::new([Ok(()), Err("dpi enable failed"), Ok(()), Ok(())].into_iter());

    let result = set_capture_reporting_transactionally([gesture, dpi], |control, enabled| {
        calls.borrow_mut().push((control, enabled));
        std::future::ready(outcome.borrow_mut().next().unwrap_or(Ok(())))
    })
    .await;

    assert_eq!(result, Err("dpi enable failed"));
    assert_eq!(
        calls.into_inner(),
        [(gesture, true), (dpi, true), (dpi, false), (gesture, false)]
    );
}

#[tokio::test]
async fn thumbwheel_enable_failure_restores_itself_dpi_and_gesture() {
    let gesture = CaptureControl::Reprog {
        cid: reprog_controls::FORWARD_BUTTON_CID,
        raw_xy: true,
    };
    let dpi = CaptureControl::Reprog {
        cid: reprog_controls::DPI_MODE_SHIFT_CIDS[0],
        raw_xy: false,
    };
    let thumbwheel = CaptureControl::Thumbwheel;
    let calls = RefCell::new(Vec::new());
    let outcome = RefCell::new(
        [
            Ok(()),
            Ok(()),
            Err("thumbwheel enable failed"),
            Ok(()),
            Ok(()),
            Ok(()),
        ]
        .into_iter(),
    );

    let result =
        set_capture_reporting_transactionally([gesture, dpi, thumbwheel], |control, enabled| {
            calls.borrow_mut().push((control, enabled));
            std::future::ready(outcome.borrow_mut().next().unwrap_or(Ok(())))
        })
        .await;

    assert_eq!(result, Err("thumbwheel enable failed"));
    assert_eq!(
        calls.into_inner(),
        [
            (gesture, true),
            (dpi, true),
            (thumbwheel, true),
            (thumbwheel, false),
            (dpi, false),
            (gesture, false),
        ]
    );
}

#[test]
fn dpi_cids_remain_plain_buttons_and_never_become_gesture_sources() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = gesture_controls(&[ButtonId::Back]);
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, diverted(&[dpi]), &gestures, &[dpi], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 8, dy: 9 },
        &gestures,
        &[dpi],
        &tx,
    );

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn gesture_button_cid_mapping_matches_the_m650_and_dedicated_control_table() {
    assert_eq!(
        reprog_controls::gesture_cid_for_button(ButtonId::MiddleClick),
        Some(0x0052)
    );
    assert_eq!(
        reprog_controls::gesture_cid_for_button(ButtonId::Back),
        Some(0x0053)
    );
    assert_eq!(
        reprog_controls::gesture_cid_for_button(ButtonId::Forward),
        Some(0x0056)
    );
    assert_eq!(
        reprog_controls::gesture_cid_for_button(ButtonId::GestureButton),
        Some(0x00c3)
    );
    assert_eq!(
        reprog_controls::gesture_cid_for_button(ButtonId::DpiToggle),
        None
    );

    for (button, cid) in [
        (ButtonId::MiddleClick, reprog_controls::MIDDLE_BUTTON_CID),
        (ButtonId::Back, reprog_controls::BACK_BUTTON_CID),
        (ButtonId::Forward, reprog_controls::FORWARD_BUTTON_CID),
        (ButtonId::GestureButton, reprog_controls::GESTURE_BUTTON_CID),
    ] {
        assert_eq!(reprog_controls::gesture_button_for_cid(cid), Some(button));
    }
    assert_eq!(reprog_controls::gesture_button_for_cid(0x00c4), None);
}

#[test]
fn a_dpi_button_re_presses_after_a_release() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let gestures = BTreeMap::new();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = diverted(&[dpi]);
    let up = diverted(&[]);
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, down, &gestures, &[dpi], &tx);
    handle_reprog(&mut acc, up, &gestures, &[dpi], &tx);
    handle_reprog(&mut acc, down, &gestures, &[dpi], &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle)),
            Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle)),
        ]
    );
    assert!(rx.try_recv().is_err());
}
