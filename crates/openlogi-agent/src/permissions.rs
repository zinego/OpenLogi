//! Privacy permissions owned by the background agent process.

use openlogi_agent_core::ipc::PermissionStatus;

const ACCESS_GRANTED: u32 = 0;
const ACCESS_DENIED: u32 = 1;

fn classify_input_monitoring(access: u32) -> PermissionStatus {
    match access {
        ACCESS_GRANTED => PermissionStatus::Granted,
        ACCESS_DENIED => PermissionStatus::Denied,
        _ => PermissionStatus::Unknown,
    }
}

/// Query whether this agent process may listen to HID devices.
#[must_use]
pub fn input_monitoring() -> PermissionStatus {
    platform::input_monitoring()
}

/// Ask macOS to register/prompt this agent for Input Monitoring access.
pub fn request_input_monitoring() {
    platform::request_input_monitoring();
}

#[cfg(target_os = "macos")]
mod platform {
    #![expect(
        unsafe_code,
        reason = "IOKit exposes Input Monitoring access only through C functions"
    )]

    use openlogi_agent_core::ipc::PermissionStatus;

    use super::classify_input_monitoring;

    const REQUEST_TYPE_LISTEN_EVENT: u32 = 1;

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOHIDCheckAccess(request_type: u32) -> u32;
        fn IOHIDRequestAccess(request_type: u32) -> bool;
    }

    pub(super) fn input_monitoring() -> PermissionStatus {
        // SAFETY: side-effect-free query with the documented ListenEvent value.
        classify_input_monitoring(unsafe { IOHIDCheckAccess(REQUEST_TYPE_LISTEN_EVENT) })
    }

    pub(super) fn request_input_monitoring() {
        // SAFETY: requests the documented ListenEvent permission for this process.
        let _ = unsafe { IOHIDRequestAccess(REQUEST_TYPE_LISTEN_EVENT) };
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use openlogi_agent_core::ipc::PermissionStatus;

    pub(super) fn input_monitoring() -> PermissionStatus {
        PermissionStatus::Granted
    }

    pub(super) fn request_input_monitoring() {}
}

#[cfg(test)]
mod tests {
    use openlogi_agent_core::ipc::PermissionStatus;

    use super::classify_input_monitoring;

    #[test]
    fn input_monitoring_access_values_preserve_denied_and_unknown() {
        assert_eq!(classify_input_monitoring(0), PermissionStatus::Granted);
        assert_eq!(classify_input_monitoring(1), PermissionStatus::Denied);
        assert_eq!(classify_input_monitoring(2), PermissionStatus::Unknown);
        assert_eq!(classify_input_monitoring(99), PermissionStatus::Unknown);
    }
}
