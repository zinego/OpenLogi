/// Raw-XY travel threshold that activates Pan. Motion below the boundary stays
/// buffered as a click candidate; reaching it flushes the buffered motion. The
/// acceptance M650 produced up to 22 units of settling travel during a physical
/// click, so 32 separates that jitter from a deliberate drag.
pub const PAN_DEADZONE: i32 = 32;

/// Typed result of one Pan state-machine transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanOutput {
    /// No hold is active, or motion is still buffered inside the deadzone.
    Idle,
    /// Signed two-axis motion to inject as one continuous Pan update.
    Delta {
        /// Horizontal raw-XY delta (`+x` is right).
        x: i32,
        /// Vertical raw-XY delta (`+y` is down).
        y: i32,
    },
    /// A held button ended before Pan activated; fire its click fallback.
    Click,
    /// An activated Pan ended, or an in-progress hold was cancelled.
    End,
}

/// Pure hold-lifecycle state for continuous Pan gestures.
///
/// Motion inside [`PAN_DEADZONE`] is accumulated with saturating arithmetic.
/// Once either axis reaches the boundary, the entire buffered prefix is emitted
/// so slow gesture starts are not lost. Subsequent motion is emitted directly.
#[derive(Debug, Default)]
pub struct PanAccumulator {
    holding: bool,
    active: bool,
    buffered_x: i32,
    buffered_y: i32,
}

impl PanAccumulator {
    /// Begin a fresh hold, discarding any stale state from an interrupted hold.
    pub fn begin(&mut self) {
        *self = Self {
            holding: true,
            ..Self::default()
        };
    }

    /// Whether a hold is currently in progress.
    #[must_use]
    pub fn is_holding(&self) -> bool {
        self.holding
    }

    /// Whether the current hold has crossed the deadzone and activated Pan.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Feed signed raw-XY motion into the hold.
    pub fn accumulate(&mut self, x: i32, y: i32) -> PanOutput {
        if !self.holding {
            return PanOutput::Idle;
        }
        if self.active {
            return PanOutput::Delta { x, y };
        }

        self.buffered_x = self.buffered_x.saturating_add(x);
        self.buffered_y = self.buffered_y.saturating_add(y);
        if self
            .buffered_x
            .saturating_abs()
            .max(self.buffered_y.saturating_abs())
            < PAN_DEADZONE
        {
            return PanOutput::Idle;
        }

        self.active = true;
        PanOutput::Delta {
            x: self.buffered_x,
            y: self.buffered_y,
        }
    }

    /// End a hold, returning [`PanOutput::Click`] before activation and
    /// [`PanOutput::End`] after activation. A stray release is idle.
    pub fn end(&mut self) -> PanOutput {
        if !self.holding {
            return PanOutput::Idle;
        }
        let output = if self.active {
            PanOutput::End
        } else {
            PanOutput::Click
        };
        *self = Self::default();
        output
    }

    /// Cancel an interrupted hold without ever committing its click fallback.
    /// Returns [`PanOutput::End`] whenever a pending or active hold was
    /// cancelled, and [`PanOutput::Idle`] when there was no hold to cancel.
    pub fn cancel(&mut self) -> PanOutput {
        if !self.holding {
            return PanOutput::Idle;
        }
        *self = Self::default();
        PanOutput::End
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_without_motion_is_a_click() {
        let mut pan = PanAccumulator::default();
        pan.begin();
        assert_eq!(pan.end(), PanOutput::Click);
        assert_eq!(pan.end(), PanOutput::Idle);
    }

    #[test]
    fn jitter_stays_inside_the_deadzone_and_preserves_click() {
        let mut pan = PanAccumulator::default();
        pan.begin();
        assert_eq!(pan.accumulate(1, -1), PanOutput::Idle);
        assert_eq!(pan.accumulate(1, 1), PanOutput::Idle);
        assert_eq!(pan.end(), PanOutput::Click);
    }

    #[test]
    fn physical_m650_click_jitter_stays_a_click() {
        // Captured from a 180 ms physical Forward click on the acceptance M650.
        // The hand settles back near its origin, so this must not become Pan.
        let trace = [
            (-4, -22),
            (-2, 3),
            (-1, 4),
            (-2, 4),
            (-1, 2),
            (-1, 2),
            (0, 1),
            (0, 1),
            (0, 1),
            (1, 0),
            (0, 1),
            (1, 0),
            (1, 1),
            (1, 0),
            (0, 1),
            (-1, 0),
        ];
        let mut pan = PanAccumulator::default();
        pan.begin();
        for (x, y) in trace {
            assert_eq!(pan.accumulate(x, y), PanOutput::Idle);
        }
        assert_eq!(pan.end(), PanOutput::Click);
    }

    #[test]
    fn activation_emits_buffered_first_motion() {
        let mut pan = PanAccumulator::default();
        pan.begin();
        assert_eq!(pan.accumulate(PAN_DEADZONE - 1, 2), PanOutput::Idle);
        assert_eq!(
            pan.accumulate(2, -1),
            PanOutput::Delta {
                x: PAN_DEADZONE + 1,
                y: 1,
            }
        );
    }

    #[test]
    fn active_pan_emits_each_two_axis_delta_then_end() {
        let mut pan = PanAccumulator::default();
        pan.begin();
        assert!(matches!(
            pan.accumulate(PAN_DEADZONE, 0),
            PanOutput::Delta { .. }
        ));
        assert_eq!(pan.accumulate(-7, 11), PanOutput::Delta { x: -7, y: 11 });
        assert_eq!(pan.end(), PanOutput::End);
    }

    #[test]
    fn buffered_motion_saturates_without_overflow() {
        let mut pan = PanAccumulator::default();
        pan.begin();
        assert_eq!(pan.accumulate(PAN_DEADZONE - 1, 0), PanOutput::Idle);
        assert_eq!(
            pan.accumulate(i32::MAX, i32::MIN),
            PanOutput::Delta {
                x: i32::MAX,
                y: i32::MIN,
            }
        );
    }

    #[test]
    fn cancel_never_commits_a_click_and_resets_state() {
        let mut pan = PanAccumulator::default();
        pan.begin();
        assert_eq!(pan.accumulate(1, 0), PanOutput::Idle);
        assert_eq!(pan.cancel(), PanOutput::End);
        assert_eq!(pan.end(), PanOutput::Idle);

        pan.begin();
        assert!(matches!(
            pan.accumulate(PAN_DEADZONE, 0),
            PanOutput::Delta { .. }
        ));
        assert_eq!(pan.cancel(), PanOutput::End);
        assert_eq!(pan.accumulate(10, 10), PanOutput::Idle);
    }
}
