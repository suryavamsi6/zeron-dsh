//! A reply read aloud: the engine's `ReadAloud` answers with 24 kHz mono PCM
//! (base64 s16le, crates/engine/src/read_aloud.rs), played once through the
//! default output the way the voice feature's is (crates/audio/src/
//! native.rs, the cpal calls mirrored from there). One read at a time:
//! `play` ends the one before it, `stop` ends it without another.
//!
//! A `Play`'s `done` lasts the read — its words played out, or it ended
//! early — so the host action that asked stays open until the sound does,
//! and the script's button can stay Stop reading until then.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use serde_json::Value as Json;
use tokio::sync::Notify;

/// The engine's one shape: this rate, one channel.
const RATE: u32 = 24_000;

/// The read playing now; `play` replaces it, `stop` takes it.
static CURRENT: Mutex<Option<Arc<Play>>> = Mutex::new(None);
/// Reads asked for, ever: `play` starts only the newest's answer.
static NEWEST: AtomicU64 = AtomicU64::new(0);

/// A read's turn: `play` plays a reply only for the newest asked, an older
/// one's answer landing after doing nothing.
pub fn ticket() -> u64 {
    NEWEST.fetch_add(1, Ordering::AcqRel) + 1
}

/// A read underway. `done` goes when the sound does — its words played
/// out, a `stop`, or a newer read's place taken.
pub struct Play {
    stream: Stream,
    over: Arc<AtomicBool>,
    ended: Arc<Notify>,
}

impl Play {
    /// Until the sound ends.
    pub async fn done(&self) {
        self.ended.notified().await;
    }
    fn end(&self) {
        if self.over.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.stream.pause();
        self.ended.notify_one();
    }
}

impl Drop for Play {
    fn drop(&mut self) {
        self.end();
    }
}

/// `reply` through the default output, the read before it ended: its
/// `Play`, or `None` when a newer read was asked while it waited (the
/// newest one's words win, so an older answer landing late plays nothing).
pub fn play(reply: &Json, ticket: u64) -> Result<Option<Arc<Play>>, String> {
    if ticket != NEWEST.load(Ordering::Acquire) {
        return Ok(None);
    }
    let samples = decode(reply)?;
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| "no audio output to read aloud on".to_string())?;
    let supported = device
        .default_output_config()
        .map_err(|_| "no audio output to read aloud on".to_string())?;
    if supported.channels() == 0 || supported.channels() > 8 {
        return Err("no audio output to read aloud on".to_string());
    }
    let rate = supported.sample_rate();
    let format = supported.sample_format();
    let config: StreamConfig = supported.into();
    // 24 kHz to the device's rate, as the voice path resamples its playout
    // (crates/audio/src/native.rs `enqueue`), then a moment of quiet so the
    // last word isn't cut when the stream ends.
    let mut out = Vec::new();
    if let Some(mut resampler) = zeron_audio::dsp::Resampler::new(RATE, rate) {
        for sample in samples {
            resampler.push(sample, &mut out);
        }
    } else {
        out = samples;
    }
    out.resize(out.len() + rate as usize / 5, 0.0);
    let samples = Arc::new(out);
    let at = Arc::new(AtomicUsize::new(0));
    let over = Arc::new(AtomicBool::new(false));
    let ended = Arc::new(Notify::new());
    macro_rules! output {
        ($t:ty) => {
            output_stream::<$t>(
                &device,
                config,
                samples.clone(),
                at.clone(),
                over.clone(),
                ended.clone(),
            )?
        };
    }
    let stream = match format {
        SampleFormat::F32 => output!(f32),
        SampleFormat::F64 => output!(f64),
        SampleFormat::I16 => output!(i16),
        SampleFormat::U16 => output!(u16),
        SampleFormat::I32 => output!(i32),
        SampleFormat::U32 => output!(u32),
        SampleFormat::I8 => output!(i8),
        SampleFormat::U8 => output!(u8),
        _ => return Err("the audio output's format isn't one played here".to_string()),
    };
    stream
        .play()
        .map_err(|_| "the audio output didn't start".to_string())?;
    let play = Arc::new(Play {
        stream,
        over,
        ended,
    });
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if ticket != NEWEST.load(Ordering::Acquire) {
        return Ok(None);
    }
    if let Some(last) = current.take() {
        last.end();
    }
    *current = Some(play.clone());
    Ok(Some(play))
}

/// The read playing now, ended where it stands (StopReadAloud).
pub fn stop() {
    if let Some(play) = CURRENT.lock().unwrap_or_else(|e| e.into_inner()).take() {
        play.end();
    }
}

/// `play` done and forgotten, so a later `stop` doesn't end it twice; a
/// newer read already in its place stays.
pub fn release(play: &Arc<Play>) {
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if current.as_ref().is_some_and(|now| Arc::ptr_eq(now, play)) {
        *current = None;
    }
}

/// The engine's answer as samples: `{sampleRate, channels, pcm}` — base64
/// s16le at RATE, mono, each sample to f32.
fn decode(reply: &Json) -> Result<Vec<f32>, String> {
    if reply.get("sampleRate").and_then(Json::as_u64) != Some(RATE as u64)
        || reply.get("channels").and_then(Json::as_u64) != Some(1)
    {
        return Err("the engine's sound wasn't 24 kHz mono".to_string());
    }
    let pcm = reply
        .get("pcm")
        .and_then(Json::as_str)
        .ok_or_else(|| "the engine gave no sound to play".to_string())?;
    let bytes = STANDARD
        .decode(pcm)
        .map_err(|_| "the engine's sound didn't decode".to_string())?;
    if bytes.len() % 2 != 0 {
        return Err("the engine's sound didn't decode".to_string());
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0)
        .collect())
}

/// The output stream: each frame the samples' next, on every channel;
/// drained or errored, `ended` goes.
fn output_stream<T>(
    device: &cpal::Device,
    config: StreamConfig,
    samples: Arc<Vec<f32>>,
    at: Arc<AtomicUsize>,
    over: Arc<AtomicBool>,
    ended: Arc<Notify>,
) -> Result<Stream, String>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let errors = over.clone();
    let ended_by_error = ended.clone();
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut i = at.load(Ordering::Relaxed);
                for frame in data.chunks_exact_mut(channels) {
                    let value = samples.get(i).copied().unwrap_or(0.0);
                    i += 1;
                    for sample in frame.iter_mut() {
                        *sample = T::from_sample(value);
                    }
                }
                at.store(i, Ordering::Relaxed);
                if i >= samples.len() && !over.swap(true, Ordering::AcqRel) {
                    ended.notify_one();
                }
            },
            move |_| {
                if !errors.swap(true, Ordering::AcqRel) {
                    ended_by_error.notify_one();
                }
            },
            Some(std::time::Duration::from_secs(2)),
        )
        .map_err(|_| "the audio output didn't start".to_string())
}
