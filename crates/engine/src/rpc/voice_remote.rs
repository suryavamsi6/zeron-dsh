use super::*;

pub(super) fn handles(method: &str) -> bool {
    matches!(
        method,
        methods::VOICE_CAPABILITIES_V2
            | methods::PREPARE_VOICE_V2
            | methods::OWN_VOICE_V2
            | methods::NEGOTIATE_VOICE_V2
            | methods::CONFIRM_VOICE_MEDIA_V2
            | methods::REPORT_VOICE_MEDIA_V2
            | methods::STOP_VOICE_V2
            | methods::CANCEL_VOICE_ATTEMPT_V2
    )
}

pub(super) fn unsupported() -> RpcError {
    RpcError::Failed(
        "Realtime voice is unsupported: the official DeepSeek SDK has no realtime voice transport. Local dictation remains available.".into(),
    )
}
