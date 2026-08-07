use super::*;

fn press() -> RawControlEvent {
    RawControlEvent::DivertedButtons([reprog_controls::GESTURE_BUTTON_CID, 0, 0, 0])
}

fn release() -> RawControlEvent {
    RawControlEvent::DivertedButtons([0, 0, 0, 0])
}

#[test]
fn gesture_reports_raw_press_motion_and_release_lifecycle() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: -120, dy: 5 },
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), &[], &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed),
            Ok(CapturedInput::GestureMotion {
                delta_x: -120,
                delta_y: 5,
            }),
            Ok(CapturedInput::GestureReleased),
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn raw_motion_is_forwarded_only_while_the_gesture_cid_is_held() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 10, dy: -20 },
        &[],
        &tx,
    );
    assert!(rx.try_recv().is_err(), "motion before press is ignored");

    handle_reprog(&mut acc, press(), &[], &tx);
    handle_reprog(&mut acc, press(), &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 10, dy: -20 },
        &[],
        &tx,
    );
    handle_reprog(&mut acc, release(), &[], &tx);
    handle_reprog(
        &mut acc,
        RawControlEvent::RawXy { dx: 30, dy: 40 },
        &[],
        &tx,
    );

    assert_eq!(
        [rx.try_recv(), rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed),
            Ok(CapturedInput::GestureMotion {
                delta_x: 10,
                delta_y: -20,
            }),
            Ok(CapturedInput::GestureReleased),
        ],
        "a repeated held frame does not create another rising edge"
    );
    assert!(rx.try_recv().is_err(), "motion after release is ignored");
}

#[test]
fn stopping_an_active_gesture_cancels_it_once() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();

    handle_reprog(&mut acc, press(), &[], &tx);
    cancel_active_gesture(&mut acc, &tx);
    cancel_active_gesture(&mut acc, &tx);

    assert_eq!(
        [rx.try_recv(), rx.try_recv()],
        [
            Ok(CapturedInput::GesturePressed),
            Ok(CapturedInput::GestureCancelled),
        ]
    );
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_held_dpi_button_presses_once_on_the_rising_edge() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[dpi], &tx);
    handle_reprog(&mut acc, down, &[dpi], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
    );
    assert!(rx.try_recv().is_err(), "a held DPI button presses once");
}

#[test]
fn a_dpi_button_re_presses_after_a_release() {
    // Rising-edge detection must re-arm: press → release → press is two
    // distinct presses. The release (a frame without the CID) is what resets
    // the edge; without it a re-press would be swallowed as "still held".
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut acc = CaptureAccum::default();
    let dpi = reprog_controls::DPI_MODE_SHIFT_CIDS[0];
    let down = RawControlEvent::DivertedButtons([dpi, 0, 0, 0]);
    let up = RawControlEvent::DivertedButtons([0, 0, 0, 0]);

    handle_reprog(&mut acc, down, &[dpi], &tx);
    handle_reprog(&mut acc, up, &[dpi], &tx);
    handle_reprog(&mut acc, down, &[dpi], &tx);

    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle))
    );
    assert_eq!(
        rx.try_recv(),
        Ok(CapturedInput::ButtonPressed(ButtonId::DpiToggle)),
        "a release re-arms the rising edge"
    );
    assert!(rx.try_recv().is_err());
}
