use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

/// Microphone (TCC) status of the responsible process. A CLI started from a terminal gets
/// the terminal's grant. Denial must not read as silence (M0 finding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicPermission {
    NotDetermined,
    Restricted,
    Denied,
    Granted,
}

pub fn microphone() -> MicPermission {
    let Some(audio) = (unsafe { AVMediaTypeAudio }) else { return MicPermission::NotDetermined };
    match unsafe { AVCaptureDevice::authorizationStatusForMediaType(audio) } {
        s if s == AVAuthorizationStatus::Authorized => MicPermission::Granted,
        s if s == AVAuthorizationStatus::Denied => MicPermission::Denied,
        s if s == AVAuthorizationStatus::Restricted => MicPermission::Restricted,
        _ => MicPermission::NotDetermined,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reports the permission of whatever process runs the tests; the value is not asserted.
    #[test]
    fn microphone_status_is_readable() {
        let p = microphone();
        println!("microphone permission: {p:?}");
    }
}
