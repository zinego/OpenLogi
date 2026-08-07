//! Pure gesture-preset selection and whole-binding configuration mutations.

use std::collections::BTreeMap;

use openlogi_core::binding::{
    Action, Binding, ButtonId, GestureDirection, PanBinding, default_gesture_binding,
    default_pan_binding, window_navigation_binding,
};
use openlogi_core::config::Config;

/// Preset shown by the gesture binding editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GesturePreset {
    /// The exact five-direction macOS window-management map.
    WindowNavigation,
    /// Continuous two-axis scrolling with an editable click fallback.
    Pan,
    /// Any user-edited directional map that no longer matches a preset.
    Custom,
}

/// Display/editor class of one button's complete binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CompleteBindingClass {
    /// One ordinary press action.
    Single,
    /// The exact canonical Window Navigation direction map.
    WindowNavigation,
    /// Continuous two-axis Pan with an editable click fallback.
    Pan,
    /// A directional map that does not exactly match Window Navigation.
    Custom,
}

const MACOS_GESTURE_PRESETS: &[GesturePreset] = &[
    GesturePreset::WindowNavigation,
    GesturePreset::Pan,
    GesturePreset::Custom,
];
const PORTABLE_GESTURE_PRESETS: &[GesturePreset] =
    &[GesturePreset::WindowNavigation, GesturePreset::Custom];

/// Presets a platform may create. Existing Pan configs remain classifiable on
/// every platform, but only macOS offers Pan as a selectable runtime mode.
#[must_use]
pub(crate) const fn available_gesture_presets(is_macos: bool) -> &'static [GesturePreset] {
    if is_macos {
        MACOS_GESTURE_PRESETS
    } else {
        PORTABLE_GESTURE_PRESETS
    }
}

/// Recognize the two exact presets; every other binding is custom.
#[must_use]
pub(crate) fn classify_gesture_preset(binding: &Binding) -> GesturePreset {
    if *binding == window_navigation_binding() {
        GesturePreset::WindowNavigation
    } else if binding.is_pan() {
        GesturePreset::Pan
    } else {
        GesturePreset::Custom
    }
}

/// Classify one card from its own complete binding.
#[must_use]
pub(crate) fn classify_complete_binding(binding: &Binding) -> CompleteBindingClass {
    match binding {
        Binding::Single(_) => CompleteBindingClass::Single,
        Binding::Pan(_) => CompleteBindingClass::Pan,
        Binding::Gesture(_) if *binding == window_navigation_binding() => {
            CompleteBindingClass::WindowNavigation
        }
        Binding::Gesture(_) => CompleteBindingClass::Custom,
    }
}

/// Replace only a Pan binding's click fallback, preserving its typed mode.
#[must_use]
pub(crate) fn pan_binding_with_click(binding: &Binding, click: Action) -> Binding {
    match binding {
        Binding::Pan(_) => Binding::Pan(PanBinding { click }),
        other => other.clone(),
    }
}

/// Store one complete binding in the selected scope. Keeping this as one
/// mutation prevents preset application from exposing partial direction maps.
pub(crate) fn apply_binding_to_scope(
    config: &mut Config,
    device_key: &str,
    app_bundle: Option<&str>,
    button: ButtonId,
    binding: Binding,
) {
    if let Some(bundle) = app_bundle {
        config.set_per_app_binding(device_key, bundle, button, Some(binding));
    } else {
        config.set_binding(device_key, button, binding);
    }
}

fn custom_gesture_map() -> BTreeMap<GestureDirection, Action> {
    GestureDirection::ALL
        .into_iter()
        .map(|direction| (direction, default_gesture_binding(direction)))
        .collect()
}

fn custom_gesture_binding() -> Binding {
    Binding::Gesture(custom_gesture_map())
}

/// Resolve a selector choice to the canonical whole binding it installs.
#[must_use]
pub(crate) fn binding_for_gesture_preset(preset: GesturePreset) -> Binding {
    match preset {
        GesturePreset::WindowNavigation => window_navigation_binding(),
        GesturePreset::Pan => default_pan_binding(),
        GesturePreset::Custom => custom_gesture_binding(),
    }
}

/// Return the one complete binding needed for a newly selected preset.
/// Reselecting the active preset is a no-op so customized values within that
/// mode are not replaced by its canonical factory defaults.
#[must_use]
pub(crate) fn binding_for_gesture_selection(
    current: &Binding,
    selected: GesturePreset,
) -> Option<Binding> {
    (matches!(current, Binding::Single(_)) || classify_gesture_preset(current) != selected)
        .then(|| binding_for_gesture_preset(selected))
}

/// Return a complete directional binding with one edited direction.
#[must_use]
pub(crate) fn gesture_binding_with_direction(
    binding: &Binding,
    direction: GestureDirection,
    action: Action,
) -> Binding {
    let mut map = match binding {
        Binding::Gesture(map) => map.clone(),
        Binding::Single(_) | Binding::Pan(_) => custom_gesture_map(),
    };
    map.insert(direction, action);
    Binding::Gesture(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_complete_bindings_independently() {
        assert_eq!(
            classify_complete_binding(&Binding::Single(Action::Copy)),
            CompleteBindingClass::Single
        );
        assert_eq!(
            classify_complete_binding(&window_navigation_binding()),
            CompleteBindingClass::WindowNavigation
        );
        assert_eq!(
            classify_complete_binding(&default_pan_binding()),
            CompleteBindingClass::Pan
        );
        assert_eq!(
            classify_complete_binding(&Binding::Gesture(BTreeMap::from([(
                GestureDirection::Left,
                Action::Copy,
            )]))),
            CompleteBindingClass::Custom
        );
    }

    #[test]
    fn forward_pan_and_back_window_navigation_coexist() {
        let mut config = Config::default();
        apply_binding_to_scope(
            &mut config,
            "mouse",
            None,
            ButtonId::Forward,
            default_pan_binding(),
        );
        apply_binding_to_scope(
            &mut config,
            "mouse",
            None,
            ButtonId::Back,
            window_navigation_binding(),
        );

        let bindings = config.effective_bindings("mouse", None);
        assert_eq!(
            bindings.get(&ButtonId::Forward),
            Some(&default_pan_binding())
        );
        assert_eq!(
            bindings.get(&ButtonId::Back),
            Some(&window_navigation_binding())
        );
    }

    #[test]
    fn replacing_back_does_not_mutate_forward() {
        let mut config = Config::default();
        apply_binding_to_scope(
            &mut config,
            "mouse",
            None,
            ButtonId::Forward,
            default_pan_binding(),
        );
        apply_binding_to_scope(
            &mut config,
            "mouse",
            None,
            ButtonId::Back,
            window_navigation_binding(),
        );
        apply_binding_to_scope(
            &mut config,
            "mouse",
            None,
            ButtonId::Back,
            Binding::Single(Action::Copy),
        );

        let bindings = config.effective_bindings("mouse", None);
        assert_eq!(
            bindings.get(&ButtonId::Forward),
            Some(&default_pan_binding())
        );
        assert_eq!(
            bindings.get(&ButtonId::Back),
            Some(&Binding::Single(Action::Copy))
        );
    }

    #[test]
    fn per_app_replacement_changes_only_target_button() {
        let mut config = Config::default();
        apply_binding_to_scope(
            &mut config,
            "mouse",
            None,
            ButtonId::Forward,
            default_pan_binding(),
        );
        apply_binding_to_scope(
            &mut config,
            "mouse",
            None,
            ButtonId::Back,
            window_navigation_binding(),
        );
        apply_binding_to_scope(
            &mut config,
            "mouse",
            Some("com.apple.Safari"),
            ButtonId::Back,
            Binding::Single(Action::Copy),
        );

        let global = config.effective_bindings("mouse", None);
        let safari = config.effective_bindings("mouse", Some("com.apple.Safari"));
        assert_eq!(
            global.get(&ButtonId::Back),
            Some(&window_navigation_binding())
        );
        assert_eq!(
            safari.get(&ButtonId::Back),
            Some(&Binding::Single(Action::Copy))
        );
        assert_eq!(safari.get(&ButtonId::Forward), Some(&default_pan_binding()));
    }

    #[test]
    fn recognizes_exact_presets_and_custom_bindings() {
        assert_eq!(
            classify_gesture_preset(&window_navigation_binding()),
            GesturePreset::WindowNavigation
        );
        assert_eq!(
            classify_gesture_preset(&default_pan_binding()),
            GesturePreset::Pan
        );
        let custom = Binding::Gesture(BTreeMap::from([(GestureDirection::Click, Action::Copy)]));
        assert_eq!(classify_gesture_preset(&custom), GesturePreset::Custom);
    }

    #[test]
    fn hides_pan_off_macos_but_keeps_existing_pan_classifiable() {
        assert_eq!(
            available_gesture_presets(false),
            &[GesturePreset::WindowNavigation, GesturePreset::Custom]
        );
        assert_eq!(
            classify_gesture_preset(&default_pan_binding()),
            GesturePreset::Pan
        );
    }

    #[test]
    fn preset_factories_use_core_canonical_bindings() {
        assert_eq!(
            binding_for_gesture_preset(GesturePreset::WindowNavigation),
            window_navigation_binding()
        );
        assert_eq!(
            binding_for_gesture_preset(GesturePreset::Pan),
            default_pan_binding()
        );
    }

    #[test]
    fn applies_complete_global_and_per_app_values() {
        let mut config = Config::default();
        apply_binding_to_scope(
            &mut config,
            "mouse",
            None,
            ButtonId::Back,
            window_navigation_binding(),
        );
        apply_binding_to_scope(
            &mut config,
            "mouse",
            Some("com.apple.Safari"),
            ButtonId::Back,
            default_pan_binding(),
        );

        assert_eq!(
            config.bindings_for("mouse").get(&ButtonId::Back),
            Some(&window_navigation_binding())
        );
        assert_eq!(
            config
                .effective_bindings("mouse", Some("com.apple.Safari"))
                .get(&ButtonId::Back),
            Some(&default_pan_binding())
        );
    }

    #[test]
    fn direction_edit_preserves_other_arms_and_becomes_custom() {
        let edited = gesture_binding_with_direction(
            &window_navigation_binding(),
            GestureDirection::Left,
            Action::Copy,
        );
        assert_eq!(classify_gesture_preset(&edited), GesturePreset::Custom);
        assert_eq!(
            edited.direction_action(GestureDirection::Left),
            Some(&Action::Copy)
        );
        assert_eq!(
            edited.direction_action(GestureDirection::Right),
            Some(&Action::NextDesktop)
        );
    }

    #[test]
    fn pan_click_edit_preserves_pan_mode() {
        assert_eq!(
            pan_binding_with_click(&default_pan_binding(), Action::MissionControl),
            Binding::Pan(PanBinding {
                click: Action::MissionControl,
            })
        );
    }

    #[test]
    fn reselecting_pan_preserves_custom_click_without_a_mutation() {
        let binding = Binding::Pan(PanBinding {
            click: Action::MissionControl,
        });

        assert_eq!(
            binding_for_gesture_selection(&binding, GesturePreset::Pan),
            None
        );
        assert_eq!(binding.click_action(), Action::MissionControl);
    }

    #[test]
    fn reselecting_custom_preserves_the_complete_binding_without_a_mutation() {
        let binding = gesture_binding_with_direction(
            &window_navigation_binding(),
            GestureDirection::Left,
            Action::Copy,
        );

        assert_eq!(
            binding_for_gesture_selection(&binding, GesturePreset::Custom),
            None
        );
        assert_eq!(
            binding.direction_action(GestureDirection::Left),
            Some(&Action::Copy)
        );
    }

    #[test]
    fn selecting_a_different_preset_returns_one_complete_binding_mutation() {
        let current = Binding::Pan(PanBinding {
            click: Action::MissionControl,
        });

        assert_eq!(
            binding_for_gesture_selection(&current, GesturePreset::WindowNavigation),
            Some(window_navigation_binding())
        );
    }

    #[test]
    fn selecting_custom_promotes_a_single_binding() {
        let selected = binding_for_gesture_selection(
            &Binding::Single(Action::MouseBack),
            GesturePreset::Custom,
        );

        assert!(matches!(selected, Some(Binding::Gesture(_))));
    }
}
