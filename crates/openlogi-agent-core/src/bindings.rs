//! Binding-map construction: overlay the stored per-device (and per-app)
//! bindings on top of the built-in defaults.
//!
//! Keyed by `config_key` (`Option<&str>`) rather than any UI device record so
//! both the agent and the GUI can build the effective map from a [`Config`].

use std::collections::BTreeMap;

use openlogi_core::binding::{
    Action, Binding, ButtonId, GestureDirection, PanBinding, default_binding,
    default_gesture_binding,
};
use openlogi_core::config::Config;

/// Runtime policy for one gesture-capable button.
///
/// Keeping Pan as a typed mode preserves its hold/release lifecycle; projecting
/// it into an [`Action`] would lose continuous motion and cancellation semantics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GestureMode {
    /// Existing click/four-direction swipe behavior.
    Directional(BTreeMap<GestureDirection, Action>),
    /// Continuous two-axis Pan behavior.
    Pan(PanBinding),
}

fn gesture_mode(binding: Binding, fill_directional_defaults: bool) -> Option<GestureMode> {
    match binding {
        Binding::Single(_) => None,
        Binding::Gesture(stored) => {
            let mut directions = if fill_directional_defaults {
                GestureDirection::ALL
                    .iter()
                    .copied()
                    .map(|direction| (direction, default_gesture_binding(direction)))
                    .collect()
            } else {
                BTreeMap::new()
            };
            directions.extend(stored);
            Some(GestureMode::Directional(directions))
        }
        Binding::Pan(pan) => Some(GestureMode::Pan(pan)),
    }
}

/// Effective per-button single-action map for the device `config_key`, with
/// `app_bundle`'s per-app overlay applied. Unset buttons fall back to
/// [`default_binding`].
///
/// This is the map the OS hook and the HID++ button-press path consume, so a
/// `Binding::Gesture` is projected to its `click_action()` — hold/motion/release
/// is dispatched through [`hid_gestures_for`] or [`oshook_gestures_for`].
#[must_use]
pub fn bindings_for(
    config: &Config,
    config_key: Option<&str>,
    app_bundle: Option<&str>,
) -> BTreeMap<ButtonId, Action> {
    let stored = config_key
        .map(|key| config.effective_bindings(key, app_bundle))
        .unwrap_or_default();
    let mut bindings: BTreeMap<ButtonId, Action> = ButtonId::ALL
        .iter()
        .copied()
        .map(|b| (b, default_binding(b)))
        .collect();
    for (k, binding) in stored {
        // A gesture binding with no explicit `Click` has no opinion on the
        // plain-press action, so leave the button's default seed in place rather
        // than clobbering it with the `Action::None` that `click_action()` would
        // project. (An explicit `Single(Action::None)` — a user-disabled button —
        // still overrides, as it should.)
        if binding.is_gesture() && binding.direction_action(GestureDirection::Click).is_none() {
            continue;
        }
        bindings.insert(k, binding.click_action());
    }
    bindings
}

/// Effective gesture bindings for the device `config_key`. Unset directions
/// fall back to [`default_gesture_binding`].
#[must_use]
pub fn hid_gesture_for(
    config: &Config,
    config_key: Option<&str>,
    app_bundle: Option<&str>,
) -> Option<GestureMode> {
    let binding = config_key
        .and_then(|key| {
            config
                .effective_bindings(key, app_bundle)
                .remove(&ButtonId::GestureButton)
        })
        .unwrap_or_else(|| Binding::Gesture(BTreeMap::new()));
    gesture_mode(binding, true)
}

/// Effective typed gesture modes that the HID++ capture session must divert.
///
/// The dedicated gesture control keeps its canonical directional default. The
/// three buttons that are also visible to the OS hook are included only when
/// their effective global/per-app binding is typed as [`Binding::Gesture`] or
/// [`Binding::Pan`]. A [`Binding::Single`] overlay therefore removes just that
/// button from HID diversion while leaving the other gesture buttons armed.
#[must_use]
pub fn hid_gestures_for(
    config: &Config,
    config_key: Option<&str>,
    app_bundle: Option<&str>,
) -> BTreeMap<ButtonId, GestureMode> {
    let mut modes = BTreeMap::from([(
        ButtonId::GestureButton,
        GestureMode::Directional(
            GestureDirection::ALL
                .iter()
                .copied()
                .map(|direction| (direction, default_gesture_binding(direction)))
                .collect(),
        ),
    )]);
    let Some(key) = config_key else {
        return modes;
    };
    for (button, binding) in config.effective_bindings(key, app_bundle) {
        if !button.is_os_hook_button() && button != ButtonId::GestureButton {
            continue;
        }
        if let Some(mode) = gesture_mode(binding, button == ButtonId::GestureButton) {
            modes.insert(button, mode);
        } else {
            modes.remove(&button);
        }
    }
    modes
}

/// Per-direction maps for the OS-hook gesture buttons (Middle/Back/Forward in
/// gesture mode) on `config_key`, with `app_bundle`'s per-app overlay applied,
/// for the OS hook to resolve a hold+swipe.
///
/// Unlike the dedicated HID++ gesture-button entry in [`hid_gestures_for`]
/// (which seeds every direction from [`default_gesture_binding`] at projection time),
/// this returns each button's raw stored map. A hand-edited sparse map leaves a
/// direction unbound, in which case the OS-hook runtime uses the map's click
/// action (or the button's native default when Click is also absent) as its
/// fallback. The dedicated gesture button is intentionally excluded:
/// it never reaches the OS hook (it's captured over HID++), so it has no entry
/// here.
///
/// A per-app [`Binding::Single`] override removes only that button from the
/// gesture projection, so it falls through to the single-action path while
/// other gesture buttons remain active.
#[must_use]
pub fn oshook_gestures_for(
    config: &Config,
    config_key: Option<&str>,
    app_bundle: Option<&str>,
) -> BTreeMap<ButtonId, GestureMode> {
    let Some(key) = config_key else {
        return BTreeMap::new();
    };
    config
        .effective_bindings(key, app_bundle)
        .into_iter()
        .filter(|(button, _)| button.is_os_hook_button())
        .filter_map(|(button, binding)| gesture_mode(binding, false).map(|mode| (button, mode)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use openlogi_core::binding::{PanBinding, default_pan_binding};

    #[test]
    fn hid_gesture_projects_pan_and_per_app_directional_overlay() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::GestureButton, default_pan_binding());
        cfg.set_per_app_binding(
            "2b042",
            "com.apple.Safari",
            ButtonId::GestureButton,
            Some(Binding::Gesture(BTreeMap::from([(
                GestureDirection::Click,
                Action::MissionControl,
            )]))),
        );

        assert_eq!(
            hid_gesture_for(&cfg, Some("2b042"), None),
            Some(GestureMode::Pan(PanBinding {
                click: Action::SmartZoom,
            }))
        );
        assert!(matches!(
            hid_gesture_for(&cfg, Some("2b042"), Some("com.apple.Safari")),
            Some(GestureMode::Directional(_))
        ));
    }

    #[test]
    fn hid_gestures_route_back_directional_and_forward_pan_together() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Gesture(BTreeMap::from([
                (GestureDirection::Click, Action::MissionControl),
                (GestureDirection::Left, Action::PreviousDesktop),
            ])),
        );
        cfg.set_binding("2b042", ButtonId::Forward, default_pan_binding());

        let modes = hid_gestures_for(&cfg, Some("2b042"), None);

        assert_eq!(modes.len(), 3, "the dedicated default remains capturable");
        assert!(matches!(
            modes.get(&ButtonId::Back),
            Some(GestureMode::Directional(directions))
                if directions.get(&GestureDirection::Click) == Some(&Action::MissionControl)
                    && directions.get(&GestureDirection::Left) == Some(&Action::PreviousDesktop)
        ));
        assert_eq!(
            modes.get(&ButtonId::Forward),
            Some(&GestureMode::Pan(PanBinding {
                click: Action::SmartZoom,
            }))
        );
        assert!(matches!(
            modes.get(&ButtonId::GestureButton),
            Some(GestureMode::Directional(_))
        ));
    }

    #[test]
    fn hid_gestures_per_app_single_removes_only_overridden_button() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, default_pan_binding());
        cfg.set_binding("2b042", ButtonId::Forward, default_pan_binding());
        cfg.set_per_app_binding(
            "2b042",
            "com.apple.Safari",
            ButtonId::Back,
            Some(Binding::Single(Action::BrowserBack)),
        );

        let modes = hid_gestures_for(&cfg, Some("2b042"), Some("com.apple.Safari"));

        assert!(!modes.contains_key(&ButtonId::Back));
        assert!(matches!(
            modes.get(&ButtonId::Forward),
            Some(GestureMode::Pan(_))
        ));
        assert!(modes.contains_key(&ButtonId::GestureButton));
    }

    #[test]
    fn oshook_pan_projects_as_typed_pan_not_an_action() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, default_pan_binding());

        let modes = oshook_gestures_for(&cfg, Some("2b042"), None);
        assert!(matches!(
            modes.get(&ButtonId::Back),
            Some(GestureMode::Pan(_))
        ));
    }

    #[test]
    fn oshook_gestures_collects_back_directional_and_forward_pan() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Gesture(BTreeMap::from([(
                GestureDirection::Left,
                Action::PreviousDesktop,
            )])),
        );
        cfg.set_binding("2b042", ButtonId::Forward, default_pan_binding());

        let modes = oshook_gestures_for(&cfg, Some("2b042"), None);
        assert!(matches!(
            modes.get(&ButtonId::Back),
            Some(GestureMode::Directional(_))
        ));
        assert!(matches!(
            modes.get(&ButtonId::Forward),
            Some(GestureMode::Pan(_))
        ));
    }

    #[test]
    fn click_less_gesture_keeps_default_click_in_projection() {
        // A gesture binding with no explicit `Click` (a migrated sparse v1 map or
        // a hand-edited config) must not project to `Action::None` and silently
        // disable the button — the button's default click survives.
        let mut cfg = Config::default();
        let mut map = BTreeMap::new();
        map.insert(GestureDirection::Up, Action::Copy);
        cfg.set_binding("2b042", ButtonId::GestureButton, Binding::Gesture(map));

        let projected = bindings_for(&cfg, Some("2b042"), None);
        assert_eq!(
            projected.get(&ButtonId::GestureButton),
            Some(&default_binding(ButtonId::GestureButton)),
            "a Click-less gesture must keep the default click, not None"
        );
    }

    #[test]
    fn explicit_gesture_click_overrides_default_in_projection() {
        // A gesture binding that DOES define `Click` projects that action.
        let mut cfg = Config::default();
        let mut map = BTreeMap::new();
        map.insert(GestureDirection::Click, Action::Paste);
        cfg.set_binding("2b042", ButtonId::GestureButton, Binding::Gesture(map));

        let projected = bindings_for(&cfg, Some("2b042"), None);
        assert_eq!(
            projected.get(&ButtonId::GestureButton),
            Some(&Action::Paste)
        );
    }

    #[test]
    fn oshook_gestures_collects_only_os_hook_gesture_buttons() {
        let mut cfg = Config::default();
        // A gesture-mode Back (an OS-hook button) — included, raw map preserved.
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Gesture(BTreeMap::from([(GestureDirection::Up, Action::Copy)])),
        );
        // A single-mode Middle — excluded (not a gesture button).
        cfg.set_binding("2b042", ButtonId::MiddleClick, Action::MiddleClick.into());
        // The dedicated HID++ gesture button — excluded (it never reaches the
        // OS hook, so it must not appear in the hook's gesture map).
        cfg.set_binding(
            "2b042",
            ButtonId::GestureButton,
            Binding::Gesture(BTreeMap::from([(
                GestureDirection::Up,
                Action::MissionControl,
            )])),
        );

        let oshook = oshook_gestures_for(&cfg, Some("2b042"), None);
        assert_eq!(oshook.len(), 1, "only the gesture-mode Back belongs here");
        let Some(GestureMode::Directional(directions)) = oshook.get(&ButtonId::Back) else {
            panic!("Back should project as directional");
        };
        assert_eq!(directions.get(&GestureDirection::Up), Some(&Action::Copy));
        assert!(!oshook.contains_key(&ButtonId::MiddleClick));
        assert!(!oshook.contains_key(&ButtonId::GestureButton));
    }

    #[test]
    fn per_app_single_drops_only_its_oshook_gesture() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Gesture(BTreeMap::from([(
                GestureDirection::Left,
                Action::PreviousDesktop,
            )])),
        );
        cfg.set_binding("2b042", ButtonId::Forward, default_pan_binding());

        cfg.set_per_app_binding(
            "2b042",
            "com.apple.Safari",
            ButtonId::Back,
            Some(Binding::Single(Action::NextTab)),
        );
        let safari = oshook_gestures_for(&cfg, Some("2b042"), Some("com.apple.Safari"));
        assert!(
            !safari.contains_key(&ButtonId::Back),
            "the overridden Back binding becomes an ordinary action"
        );
        assert!(
            safari.contains_key(&ButtonId::Forward),
            "the independent Forward Pan binding remains active"
        );

        assert!(
            oshook_gestures_for(&cfg, Some("2b042"), Some("com.other.App"))
                .contains_key(&ButtonId::Back)
        );
    }

    #[test]
    fn per_app_typed_binding_promotes_only_its_oshook_button() {
        let mut cfg = Config::default();
        cfg.set_binding("2b042", ButtonId::Back, Action::MouseBack.into());
        cfg.set_binding("2b042", ButtonId::Forward, Action::MouseForward.into());
        cfg.set_per_app_binding(
            "2b042",
            "com.apple.Safari",
            ButtonId::Forward,
            Some(default_pan_binding()),
        );

        let safari = oshook_gestures_for(&cfg, Some("2b042"), Some("com.apple.Safari"));
        assert_eq!(safari.len(), 1);
        assert!(matches!(
            safari.get(&ButtonId::Forward),
            Some(GestureMode::Pan(_))
        ));
        assert!(!safari.contains_key(&ButtonId::Back));
        assert!(
            oshook_gestures_for(&cfg, Some("2b042"), Some("com.other.App")).is_empty(),
            "the typed override does not promote the global binding"
        );
    }

    #[test]
    fn dedicated_hid_gesture_coexists_with_oshook_gestures() {
        let mut cfg = Config::default();
        cfg.set_binding(
            "2b042",
            ButtonId::Back,
            Binding::Gesture(BTreeMap::from([(
                GestureDirection::Left,
                Action::PreviousDesktop,
            )])),
        );
        cfg.set_binding("2b042", ButtonId::Forward, default_pan_binding());
        cfg.set_binding(
            "2b042",
            ButtonId::GestureButton,
            Binding::Gesture(BTreeMap::from([(
                GestureDirection::Up,
                Action::MissionControl,
            )])),
        );

        let Some(GestureMode::Directional(defaults)) = hid_gesture_for(&cfg, Some("2b042"), None)
        else {
            panic!("the dedicated HID++ button should remain directional");
        };
        assert_eq!(
            defaults.get(&GestureDirection::Up),
            Some(&Action::MissionControl)
        );
        let oshook = oshook_gestures_for(&cfg, Some("2b042"), None);
        assert!(oshook.contains_key(&ButtonId::Back));
        assert!(oshook.contains_key(&ButtonId::Forward));
    }
}
