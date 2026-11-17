use crate::state::EngineHandle;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeron_proto::voice::*;

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    _engine: EngineHandle,
    _host: String,
    _config: zeron_proto::ChatConfig,
    _voice: Option<String>,
    _cancel: CancellationToken,
    _events: mpsc::Sender<VoiceEvent>,
    _controls: mpsc::Receiver<super::VoiceControl>,
) -> Result<(), VoiceRejection> {
    Err(VoiceRejection::Unsupported)
}
