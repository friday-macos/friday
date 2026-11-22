use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

type ResultText = Result<String, String>;

pub struct Worker {
    input: SyncSender<Vec<f32>>,
    pub output: Receiver<ResultText>,
}

static ACTIVE: AtomicBool = AtomicBool::new(false);

struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}

pub fn bundled_model() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let model = executable
        .parent()?
        .parent()?
        .join("Resources/whisper-small-q5_1.bin");
    model.is_file().then_some(model)
}

impl Worker {
    pub fn start(model: PathBuf, ctx: egui::Context) -> Result<Self, String> {
        if ACTIVE.swap(true, Ordering::AcqRel) {
            return Err("이전 음성 인식이 끝난 뒤 다시 시작해줘.".into());
        }
        let slot = Slot;
        let (input, requests) = mpsc::sync_channel::<Vec<f32>>(1);
        let (events, output) = mpsc::channel();
        std::thread::Builder::new()
            .name("jarvis-web-stt".into())
            .spawn(move || {
                let _slot = slot;
                let mut settings = WhisperContextParameters::default();
                settings.use_gpu(false);
                let result = WhisperContext::new_with_params(&model, settings)
                    .and_then(|context| context.create_state());
                let mut state = match result {
                    Ok(state) => state,
                    Err(_) => {
                        let _ =
                            events.send(Err("Mac의 로컬 한국어 음성 모델을 열지 못했어.".into()));
                        ctx.request_repaint();
                        return;
                    }
                };
                while let Ok(samples) = requests.recv() {
                    let result = transcribe(&mut state, &samples);
                    if events.send(result).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self { input, output })
    }

    pub fn submit(&self, samples: Vec<f32>) -> Result<(), String> {
        self.input
            .try_send(samples)
            .map_err(|_| "Mac의 로컬 음성 인식을 시작하지 못했어.".into())
    }
}

fn transcribe(state: &mut whisper_rs::WhisperState, samples: &[f32]) -> ResultText {
    if samples.len() < 3000 || !samples.iter().any(|sample| sample.abs() >= 0.015) {
        return Ok(String::new());
    }
    let mut params = FullParams::new(SamplingStrategy::BeamSearch {
        beam_size: 3,
        patience: -1.0,
    });
    params.set_n_threads(4);
    params.set_language(Some("ko"));
    params.set_no_context(true);
    params.set_no_timestamps(true);
    params.set_suppress_blank(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    state
        .full(params, samples)
        .map_err(|error| error.to_string())?;
    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(&segment.to_str_lossy().map_err(|error| error.to_string())?);
    }
    Ok(speech_only(&text))
}

fn speech_only(text: &str) -> String {
    let text = text.trim();
    // Whisper also reports non-speech annotations such as "(음악)".
    if matches!(
        (text.chars().next(), text.chars().next_back()),
        (Some('('), Some(')')) | (Some('['), Some(']'))
    ) && text
        .chars()
        .filter(|ch| matches!(ch, '(' | ')' | '[' | ']'))
        .count()
        == 2
    {
        String::new()
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::speech_only;

    #[test]
    fn annotation_alone_is_not_a_user_request() {
        assert_eq!(speech_only(" (끝까지 끝) "), "");
        assert_eq!(speech_only("[배경 음악]"), "");
        assert_eq!(speech_only("한 문장으로 줄여줘."), "한 문장으로 줄여줘.");
        assert_eq!(
            speech_only("(이전 답변)을 줄여줘."),
            "(이전 답변)을 줄여줘."
        );
    }
}
