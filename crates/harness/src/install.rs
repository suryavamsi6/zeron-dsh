use zeron_proto::HarnessId;

use crate::{CancellationToken, Harness, HarnessError};

pub fn can_install(_id: HarnessId) -> bool {
    false
}

pub fn manual_command(id: HarnessId) -> Option<&'static str> {
    (id == HarnessId::DeepSeek).then_some(
        "Set ZERON_DEEPSEEK_SDK_DIR to an isolated npm prefix, then run: npm install --prefix \"$ZERON_DEEPSEEK_SDK_DIR\" --save-exact @deepseek-ai/dsh-sdk-client@0.2.2-rc.3 @deepseek-ai/dsh@0.2.2-rc.3 (Node >=24.9)",
    )
}

pub fn installed(id: HarnessId) -> bool {
    id == HarnessId::DeepSeek && crate::DeepSeekHarness::new().installed()
}

pub async fn install_harness(
    id: HarnessId,
    _cancel: CancellationToken,
) -> Result<(), HarnessError> {
    Err(HarnessError::Install(
        manual_command(id)
            .unwrap_or("This historical harness is no longer supported")
            .into(),
    ))
}

#[cfg(feature = "installer-fixture")]
pub async fn install_with_command(
    _id: HarnessId,
    _command: &str,
    _cancel: CancellationToken,
) -> Result<(), HarnessError> {
    Err(HarnessError::Install(
        "DeepSeek SDK installation is manual".into(),
    ))
}
