use crate::{local_stt, neural};
use block2::RcBlock;
use eframe::egui;
use objc2::{AnyThread, rc::Retained, runtime::Bool};
use objc2_avf_audio::{
    AVAudioApplication, AVAudioApplicationRecordPermission as Microphone, AVAudioEngine,
    AVAudioInputNode, AVAudioPCMBuffer, AVAudioTime,
    AVAudioVoiceProcessingOtherAudioDuckingConfiguration,
    AVAudioVoiceProcessingOtherAudioDuckingLevel, AVSpeechBoundary, AVSpeechSynthesisVoice,
    AVSpeechSynthesizer, AVSpeechUtterance,
};
use objc2_foundation::{NSArray, NSBundle, NSError, NSLocale, NSString};
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask,
    SFSpeechRecognitionTaskHint, SFSpeechRecognizer,
    SFSpeechRecognizerAuthorizationStatus as Speech,
};
use std::{
    cell::Cell,
    collections::VecDeque,
    path::PathBuf,
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
        mpsc::{self, Receiver},
    },
    time::{Duration, Instant},
};

pub trait VoiceEngine {
    fn permission(&self, ctx: &egui::Context) -> Result<Option<Receiver<()>>, String>;
    fn listen(
        &self,
        ctx: &egui::Context,
        wake: bool,
        echo: bool,
    ) -> Result<Box<dyn Listening>, String>;
    fn speak(&self, text: &str) -> Result<(), String>;
    fn speaking(&self) -> bool;
    fn set_playback_volume(&self, _volume: u16) {}
    fn playback_level(&self) -> Option<f32> {
        None
    }
    fn stop_speaking(&self);
    fn warm(&self) {}
    fn poll(&self) -> Option<String> {
        None
    }
}

pub trait Listening {
    fn event(&self) -> Option<Recognition>;
    fn finish_audio(&mut self);
    fn fallback(&mut self, _ctx: &egui::Context) -> Result<Option<local_stt::Worker>, String> {
        Ok(None)
    }
    fn level(&self) -> f32 {
        0.0
    }
    fn echo_cancelled(&self) -> bool {
        false
    }
}

pub enum Recognition {
    Partial(String),
    Final(String),
    Failed(String),
}

pub struct MacVoice {
    speaker: Retained<AVSpeechSynthesizer>,
    neural: Option<neural::Player>,
    playback_volume: Cell<u16>,
}

pub fn permission_status() -> (&'static str, &'static str) {
    let microphone = unsafe { AVAudioApplication::sharedInstance().recordPermission() };
    let microphone = match microphone {
        Microphone::Granted => "허용됨",
        Microphone::Undetermined => "말하기를 누르면 요청",
        _ => "시스템 설정에서 허용 필요",
    };
    let speech = match unsafe { SFSpeechRecognizer::authorizationStatus() } {
        Speech::Authorized => "허용됨",
        Speech::NotDetermined => "말하기를 누르면 요청",
        _ => "시스템 설정에서 허용 필요",
    };
    (microphone, speech)
}

pub fn authorize_browser_speech(ctx: &egui::Context) -> Result<(), String> {
    MacVoice::recognizer()?;
    match unsafe { SFSpeechRecognizer::authorizationStatus() } {
        Speech::Authorized => Ok(()),
        Speech::NotDetermined => {
            let ctx = ctx.clone();
            let callback = RcBlock::new(move |_status: Speech| ctx.request_repaint());
            unsafe { SFSpeechRecognizer::requestAuthorization(&callback) };
            Ok(())
        }
        _ => Err("시스템 설정의 개인정보 보호 및 보안에서 프라이데이 음성 인식을 허용해줘.".into()),
    }
}

impl MacVoice {
    pub fn new(model: PathBuf, playback_volume: u16) -> Self {
        Self {
            speaker: unsafe { AVSpeechSynthesizer::new() },
            neural: neural::Player::installed(model),
            playback_volume: Cell::new(playback_volume),
        }
    }

    pub(crate) fn recognizer() -> Result<Retained<SFSpeechRecognizer>, String> {
        let locale = NSLocale::localeWithLocaleIdentifier(&NSString::from_str("ko-KR"));
        let recognizer =
            unsafe { SFSpeechRecognizer::initWithLocale(SFSpeechRecognizer::alloc(), &locale) }
                .ok_or("이 Mac에서 한국어 음성 인식기를 열 수 없어.")?;
        if !unsafe { recognizer.supportsOnDeviceRecognition() } {
            return Err("이 Mac에서 한국어 오프라인 음성 인식을 사용할 수 없어.".into());
        }
        Ok(recognizer)
    }
}

impl VoiceEngine for MacVoice {
    fn permission(&self, ctx: &egui::Context) -> Result<Option<Receiver<()>>, String> {
        Self::recognizer()?;
        let bundle = NSBundle::mainBundle();
        for key in [
            "NSSpeechRecognitionUsageDescription",
            "NSMicrophoneUsageDescription",
        ] {
            if bundle
                .objectForInfoDictionaryKey(&NSString::from_str(key))
                .is_none()
            {
                return Err("음성 입력은 Finder에서 FRIDAY.app을 열어 사용해줘.".into());
            }
        }
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        match unsafe { SFSpeechRecognizer::authorizationStatus() } {
            Speech::Authorized => (),
            Speech::NotDetermined => {
                let callback = RcBlock::new(move |_status: Speech| {
                    let _ = sender.send(());
                    repaint.request_repaint();
                });
                unsafe { SFSpeechRecognizer::requestAuthorization(&callback) };
                return Ok(Some(receiver));
            }
            _ => {
                return Err(
                    "시스템 설정의 개인정보 보호 및 보안에서 프라이데이 음성 인식을 허용해줘."
                        .into(),
                );
            }
        }
        match unsafe { AVAudioApplication::sharedInstance().recordPermission() } {
            Microphone::Granted => Ok(None),
            Microphone::Undetermined => {
                let callback = RcBlock::new(move |_granted: objc2::runtime::Bool| {
                    let _ = sender.send(());
                    repaint.request_repaint();
                });
                unsafe {
                    AVAudioApplication::requestRecordPermissionWithCompletionHandler(&callback)
                };
                Ok(Some(receiver))
            }
            _ => {
                Err("시스템 설정의 개인정보 보호 및 보안에서 프라이데이 마이크를 허용해줘.".into())
            }
        }
    }

    fn listen(
        &self,
        ctx: &egui::Context,
        wake: bool,
        echo: bool,
    ) -> Result<Box<dyn Listening>, String> {
        // AVFoundation objects stay on the app thread. Callback threads append the buffers
        // Apple provides and send owned strings back to the app loop.
        let recognizer = Self::recognizer()?;
        unsafe {
            if !recognizer.isAvailable() {
                return Err("macOS 한국어 음성 인식이 지금 응답하지 않아.".into());
            }
            let engine = AVAudioEngine::new();
            let node = engine.inputNode();
            // Dictation and idle wake detection use the input device directly.
            let echo_cancelled = echo && node.setVoiceProcessingEnabled_error(true).is_ok();
            if echo_cancelled {
                // The local synthesis player uses a separate output stream.
                node.setVoiceProcessingOtherAudioDuckingConfiguration(
                    AVAudioVoiceProcessingOtherAudioDuckingConfiguration {
                        enableAdvancedDucking: Bool::NO,
                        duckingLevel: AVAudioVoiceProcessingOtherAudioDuckingLevel::Min,
                    },
                );
            }
            let format = node.outputFormatForBus(0);
            if format.channelCount() == 0 || format.sampleRate() <= 0.0 {
                return Err("사용 가능한 마이크 입력이 없어.".into());
            }
            let request = SFSpeechAudioBufferRecognitionRequest::new();
            request.setRequiresOnDeviceRecognition(true);
            request.setShouldReportPartialResults(true);
            if wake {
                request.setContextualStrings(&NSArray::from_retained_slice(&[NSString::from_str(
                    "프라이데이",
                )]));
            } else {
                request.setTaskHint(SFSpeechRecognitionTaskHint::Dictation);
            }
            let (sender, events) = mpsc::channel();
            let repaint = ctx.clone();
            let result_callback = RcBlock::new(
                move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
                    let event = if let Some(result) = result.as_ref() {
                        let text = result.bestTranscription().formattedString().to_string();
                        if result.isFinal() {
                            Recognition::Final(text)
                        } else {
                            Recognition::Partial(text)
                        }
                    } else if let Some(error) = error.as_ref() {
                        Recognition::Failed(error.localizedDescription().to_string())
                    } else {
                        return;
                    };
                    let _ = sender.send(event);
                    repaint.request_repaint();
                },
            );
            let task =
                recognizer.recognitionTaskWithRequest_resultHandler(&request, &result_callback);
            let buffer_request = request.clone();
            let level = Arc::new(AtomicU32::new(0));
            let tap_level = Arc::clone(&level);
            // Keep only manual dictation/follow-up audio, in memory, for local retry.
            let sample_rate = format.sampleRate();
            let fallback_audio = (!wake && local_stt::bundled_model().is_some()).then(|| {
                Arc::new(Mutex::new(Vec::with_capacity(
                    (sample_rate * 45.0) as usize,
                )))
            });
            let tap_audio = fallback_audio.clone();
            let tap = RcBlock::new(
                move |buffer: NonNull<AVAudioPCMBuffer>, _time: NonNull<AVAudioTime>| {
                    let buffer = buffer.as_ref();
                    buffer_request.appendAudioPCMBuffer(buffer);
                    let frames = buffer.frameLength() as usize;
                    if let Some(channel) = buffer.floatChannelData().as_ref()
                        && frames > 0
                    {
                        let stride = buffer.stride();
                        if let Some(audio) = &tap_audio
                            && let Ok(mut audio) = audio.try_lock()
                        {
                            let available = audio.capacity() - audio.len();
                            audio.extend(
                                (0..frames.min(available))
                                    .map(|frame| *channel.as_ptr().add(frame * stride)),
                            );
                        }
                        let mut power = 0.0;
                        let mut sampled = 0;
                        for frame in (0..frames).step_by(4) {
                            let sample = *channel.as_ptr().add(frame * stride);
                            power += sample * sample;
                            sampled += 1;
                        }
                        tap_level
                            .store((power / sampled as f32).sqrt().to_bits(), Ordering::Relaxed);
                    }
                },
            );
            node.installTapOnBus_bufferSize_format_block(
                0,
                1024,
                Some(&format),
                RcBlock::as_ptr(&tap),
            );
            let capture = MacListening {
                _recognizer: recognizer,
                engine,
                node,
                request,
                task,
                events,
                level,
                fallback_audio,
                sample_rate,
                recording: true,
                echo_cancelled,
            };
            capture.engine.prepare();
            capture
                .engine
                .startAndReturnError()
                .map_err(|error| error.localizedDescription().to_string())?;
            Ok(Box::new(capture))
        }
    }

    fn speak(&self, text: &str) -> Result<(), String> {
        self.stop_speaking();
        if let Some(neural) = &self.neural
            && neural.speak(text, self.playback_volume.get()).is_ok()
        {
            return Ok(());
        }
        self.speak_apple(text)
    }

    fn speaking(&self) -> bool {
        self.neural.as_ref().is_some_and(neural::Player::speaking)
            || unsafe { self.speaker.isSpeaking() }
    }

    fn set_playback_volume(&self, volume: u16) {
        self.playback_volume.set(volume);
        if let Some(neural) = &self.neural {
            neural.set_volume(volume);
        }
    }

    fn playback_level(&self) -> Option<f32> {
        self.neural.as_ref()?.playback_level()
    }

    fn stop_speaking(&self) {
        if let Some(neural) = &self.neural {
            neural.stop();
        }
        unsafe {
            self.speaker
                .stopSpeakingAtBoundary(AVSpeechBoundary::Immediate)
        };
    }

    fn warm(&self) {
        if let Some(neural) = &self.neural {
            let _ = neural.warm();
        }
    }

    fn poll(&self) -> Option<String> {
        let failure = self.neural.as_ref()?.poll()?;
        if failure.played {
            Some(format!("로컬 음성 재생이 중단됐어 · {}", failure.message))
        } else {
            let err = self.speak_apple(&failure.text).err();
            Some(err.unwrap_or_else(|| "로컬 음성이 응답하지 않아 macOS 음성으로 읽을게.".into()))
        }
    }
}

impl MacVoice {
    fn speak_apple(&self, text: &str) -> Result<(), String> {
        unsafe {
            let preferred =
                AVSpeechSynthesisVoice::voiceWithLanguage(Some(&NSString::from_str("ko-KR")));
            let preferred_quality = preferred.as_ref().map_or(0, |voice| voice.quality().0);
            let voice = AVSpeechSynthesisVoice::speechVoices()
                .iter()
                .filter(|voice| {
                    voice.language().to_string() == "ko-KR" && voice.quality().0 > preferred_quality
                })
                .max_by_key(|voice| voice.quality().0)
                .or(preferred)
                .ok_or("macOS 한국어 읽기 음성을 찾을 수 없어.")?;
            let utterance = AVSpeechUtterance::speechUtteranceWithString(&NSString::from_str(text));
            utterance.setVoice(Some(&voice));
            utterance.setVolume(f32::from(self.playback_volume.get().min(100)) / 100.0);
            self.speaker.speakUtterance(&utterance);
        }
        Ok(())
    }
}

impl Drop for MacVoice {
    fn drop(&mut self) {
        self.stop_speaking();
    }
}

struct MacListening {
    _recognizer: Retained<SFSpeechRecognizer>,
    engine: Retained<AVAudioEngine>,
    node: Retained<AVAudioInputNode>,
    request: Retained<SFSpeechAudioBufferRecognitionRequest>,
    task: Retained<SFSpeechRecognitionTask>,
    events: Receiver<Recognition>,
    level: Arc<AtomicU32>,
    fallback_audio: Option<Arc<Mutex<Vec<f32>>>>,
    sample_rate: f64,
    recording: bool,
    echo_cancelled: bool,
}

impl Listening for MacListening {
    fn event(&self) -> Option<Recognition> {
        self.events.try_recv().ok()
    }

    fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    fn echo_cancelled(&self) -> bool {
        self.echo_cancelled
    }

    fn fallback(&mut self, ctx: &egui::Context) -> Result<Option<local_stt::Worker>, String> {
        self.finish_audio();
        let Some(audio) = self.fallback_audio.take() else {
            return Ok(None);
        };
        let audio = std::mem::take(&mut *audio.lock().map_err(|_| "마이크 입력을 읽을 수 없어.")?);
        let samples = resample_for_whisper(&audio, self.sample_rate);
        let voiced_frames = samples
            .as_chunks::<320>()
            .0
            .iter()
            .filter(|frame| {
                frame.iter().map(|sample| sample * sample).sum::<f32>() / 320.0 >= 0.008_f32.powi(2)
            })
            .take(6)
            .count();
        if voiced_frames < 6 {
            return Ok(None);
        }
        let Some(model) = local_stt::bundled_model() else {
            return Ok(None);
        };
        let worker = local_stt::Worker::start(model, ctx.clone())?;
        worker.submit(samples)?;
        Ok(Some(worker))
    }

    fn finish_audio(&mut self) {
        if self.recording {
            self.recording = false;
            unsafe {
                self.engine.stop();
                self.node.removeTapOnBus(0);
                self.request.endAudio();
            }
        }
    }
}

impl Drop for MacListening {
    fn drop(&mut self) {
        self.finish_audio();
        unsafe { self.task.cancel() };
    }
}

fn resample_for_whisper(input: &[f32], sample_rate: f64) -> Vec<f32> {
    let step = sample_rate / 16_000.0;
    let length = (input.len() as f64 / step) as usize;
    (0..length)
        .map(|i| {
            let position = i as f64 * step;
            let index = position as usize;
            let fraction = (position - index as f64) as f32;
            let current = input[index];
            let next = input.get(index + 1).copied().unwrap_or(current);
            current + fraction * (next - current)
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Wake,
    Dictation,
    FollowUp,
}

struct Recording {
    mode: Mode,
    handle: Box<dyn Listening>,
    text: String,
    start: Instant,
    last_word: Option<Instant>,
    ending: Option<Instant>,
    ignored_prefix: String,
    interrupted_reply: bool,
    fallback: Option<(local_stt::Worker, Instant)>,
}

enum Input {
    Idle,
    Permission(Mode, Receiver<()>),
    Recording(Recording),
    Retry(Instant),
}

pub enum VoiceUpdate {
    Woke,
    Transcript {
        text: String,
        interrupted_reply: bool,
    },
    Notice(String),
}

pub struct Voice {
    // Drop the active capture before releasing the engine.
    input: Input,
    engine: Box<dyn VoiceEngine>,
    wake: bool,
    quick_failures: u8,
    playback: Option<Playback>,
    reply_listening: bool,
    stream: Option<StreamedReply>,
}

struct StreamedReply {
    id: i64,
    chunks: VecDeque<String>,
    received: bool,
    complete: bool,
}

struct Playback {
    text: String,
    since: Instant,
    old_transcript: String,
    follow_up: bool,
}

impl Voice {
    pub fn new(engine: impl VoiceEngine + 'static) -> Self {
        Self {
            input: Input::Idle,
            engine: Box::new(engine),
            wake: false,
            quick_failures: 0,
            playback: None,
            reply_listening: false,
            stream: None,
        }
    }

    pub fn status(&self) -> &str {
        match &self.input {
            Input::Permission(_, _) => "음성 권한 대기 중",
            Input::Recording(recording) if recording.fallback.is_some() => "로컬 음성 인식 중…",
            Input::Recording(recording) if recording.mode != Mode::Wake => {
                if !recording.text.is_empty() {
                    &recording.text
                } else if recording.mode == Mode::FollowUp {
                    "이어서 말해줘…"
                } else {
                    "듣는 중…"
                }
            }
            Input::Recording(_) if self.reply_listening => "낭독 중 · 마이크 켜짐",
            Input::Recording(_) | Input::Retry(_) => "‘프라이데이’ 호출 대기 중",
            Input::Idle => "",
        }
    }

    pub fn is_dictating(&self) -> bool {
        matches!(
            &self.input,
            Input::Recording(Recording {
                mode: Mode::Dictation | Mode::FollowUp,
                ..
            })
        )
    }

    pub fn listening(&self) -> bool {
        matches!(&self.input, Input::Recording(recording) if recording.ending.is_none())
    }

    pub fn dictation_mode(&self) -> Option<&'static str> {
        if let Input::Recording(recording) = &self.input
            && recording.mode != Mode::Wake
        {
            return Some(if recording.ending.is_some() {
                "recognizing"
            } else {
                "listening"
            });
        }
        None
    }

    pub fn microphone_level(&self) -> f32 {
        match &self.input {
            Input::Recording(recording) if recording.ending.is_none() => recording.handle.level(),
            _ => 0.0,
        }
    }

    pub fn wake_enabled(&self) -> bool {
        self.wake
    }
    pub fn speaking(&self) -> bool {
        self.engine.speaking()
    }

    pub fn set_playback_volume(&self, volume: u16) {
        self.engine.set_playback_volume(volume);
    }

    pub fn playback_level(&self) -> Option<f32> {
        self.engine.playback_level()
    }
    pub fn following_up(&self) -> bool {
        matches!(
            &self.input,
            Input::Recording(Recording {
                mode: Mode::FollowUp,
                ..
            }) | Input::Permission(Mode::FollowUp, _)
        )
    }

    pub fn end_session(&mut self, ctx: &egui::Context) {
        self.stop_speaking();
        if self.following_up() {
            self.resume_wake(ctx);
        }
    }

    pub fn stop_speaking(&mut self) {
        self.stream = None;
        self.stop_playback();
    }

    fn stop_playback(&mut self) {
        self.engine.stop_speaking();
        self.playback = None;
        if self.reply_listening {
            self.reply_listening = false;
            if !self.is_dictating() && !self.wake {
                self.input = Input::Idle;
            }
        }
    }

    pub fn speak(&mut self, text: &str, ctx: &egui::Context) -> Result<(), String> {
        self.stop_speaking();
        self.start_playback(text, ctx)
    }

    fn start_playback(&mut self, text: &str, ctx: &egui::Context) -> Result<(), String> {
        let text = spoken_text(text);
        if text.is_empty() {
            return Err("낭독할 문장이 없어.".into());
        }
        self.engine.speak(&text)?;
        let old_transcript = match &self.input {
            Input::Recording(recording) => recording.text.clone(),
            _ => String::new(),
        };
        self.playback = Some(Playback {
            text: normalized(&text),
            since: Instant::now(),
            old_transcript,
            follow_up: false,
        });
        ctx.request_repaint_after(Duration::from_millis(150));
        Ok(())
    }

    pub fn reply(&mut self, text: &str, ctx: &egui::Context) -> Result<(), String> {
        self.speak(text, ctx)?;
        if let Some(playback) = &mut self.playback {
            playback.follow_up = true;
        }
        self.listen_during_reply(ctx);
        Ok(())
    }

    fn listen_during_reply(&mut self, ctx: &egui::Context) {
        let needs_echo_capture = matches!(&self.input, Input::Recording(recording)
            if recording.mode == Mode::Wake && !recording.handle.echo_cancelled());
        if matches!(self.input, Input::Idle | Input::Retry(_)) || needs_echo_capture {
            self.reply_listening = self.start(Mode::Wake, ctx).is_ok() && !self.wake;
        }
    }

    pub fn begin_stream(&mut self, id: i64) {
        self.stop_speaking();
        self.stream = Some(StreamedReply {
            id,
            chunks: VecDeque::new(),
            received: false,
            complete: false,
        });
    }

    pub fn queue_stream(
        &mut self,
        id: i64,
        text: String,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let Some(stream) = self.stream.as_mut().filter(|stream| stream.id == id) else {
            return Ok(());
        };
        if spoken_text(&text).is_empty() {
            return Ok(());
        }
        stream.received = true;
        stream.chunks.push_back(text);
        self.advance_stream(ctx)
    }

    pub fn finish_stream(
        &mut self,
        id: i64,
        final_text: Option<&str>,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let Some(stream) = self.stream.as_mut().filter(|stream| stream.id == id) else {
            return Ok(());
        };
        let Some(final_text) = final_text else {
            self.stop_speaking();
            return Ok(());
        };
        if !stream.received {
            return self.reply(final_text, ctx);
        }
        stream.complete = true;
        self.advance_stream(ctx)
    }

    fn advance_stream(&mut self, ctx: &egui::Context) -> Result<(), String> {
        if self.playback.is_some() || self.is_dictating() {
            return Ok(());
        }
        let Some(stream) = &mut self.stream else {
            return Ok(());
        };
        if let Some(text) = take_speech_chunk(&mut stream.chunks) {
            if let Err(error) = self.start_playback(&text, ctx) {
                self.stop_speaking();
                return Err(error);
            }
            self.listen_during_reply(ctx);
        } else if stream.complete {
            self.stop_speaking();
            self.start(Mode::FollowUp, ctx)?;
        }
        Ok(())
    }

    pub fn dictate(&mut self, ctx: &egui::Context) -> Result<(), String> {
        self.stop_speaking();
        if let Input::Recording(recording) = &mut self.input
            && recording.mode != Mode::Wake
        {
            finish(recording, Instant::now());
            ctx.request_repaint_after(Duration::from_secs(2));
            return Ok(());
        }
        // The user can cancel a permission wait, but cannot dismiss macOS's own prompt.
        if matches!(
            &self.input,
            Input::Permission(Mode::Dictation | Mode::FollowUp, _)
        ) {
            self.resume_wake(ctx);
            return Ok(());
        }
        self.engine.warm();
        self.start(Mode::Dictation, ctx)
    }

    pub fn toggle_wake(&mut self, ctx: &egui::Context) -> Result<(), String> {
        self.wake = !self.wake;
        self.quick_failures = 0;
        if self.is_dictating()
            || matches!(
                self.input,
                Input::Permission(Mode::Dictation | Mode::FollowUp, _)
            )
        {
            return Ok(());
        }
        if self.wake {
            self.engine.warm();
            self.start(Mode::Wake, ctx)
        } else {
            self.input = Input::Idle;
            Ok(())
        }
    }

    fn start(&mut self, mode: Mode, ctx: &egui::Context) -> Result<(), String> {
        self.input = Input::Idle;
        let input = match self.engine.permission(ctx) {
            Ok(Some(wait)) => Ok(Input::Permission(mode, wait)),
            Ok(None) => self
                .engine
                .listen(
                    ctx,
                    mode == Mode::Wake,
                    mode == Mode::Wake && self.playback.is_some(),
                )
                .map(|handle| {
                    Input::Recording(Recording {
                        mode,
                        handle,
                        text: String::new(),
                        start: Instant::now(),
                        last_word: None,
                        ending: None,
                        ignored_prefix: String::new(),
                        interrupted_reply: false,
                        fallback: None,
                    })
                }),
            Err(error) => Err(error),
        };
        match input {
            Ok(input) => {
                self.input = input;
                ctx.request_repaint();
                Ok(())
            }
            Err(error) => {
                self.wake = false;
                Err(error)
            }
        }
    }

    fn resume_wake(&mut self, ctx: &egui::Context) {
        self.input = if self.wake || (self.reply_listening && self.playback.is_some()) {
            ctx.request_repaint_after(Duration::from_millis(750));
            Input::Retry(Instant::now() + Duration::from_millis(750))
        } else {
            Input::Idle
        };
    }

    pub fn tick(&mut self, ctx: &egui::Context) -> Option<VoiceUpdate> {
        if let Some(error) = self.engine.poll() {
            ctx.request_repaint_after(Duration::from_millis(150));
            return Some(VoiceUpdate::Notice(error));
        }
        let speaking = self.speaking();
        let now = Instant::now();
        if speaking {
            ctx.request_repaint_after(Duration::from_millis(60));
        } else if let Some(playback) = &self.playback {
            let grace =
                Duration::from_millis(750).saturating_sub(now.duration_since(playback.since));
            if !grace.is_zero() {
                ctx.request_repaint_after(grace);
            } else {
                let follow_up = playback.follow_up;
                self.playback = None;
                let result = if self.stream.is_some() {
                    self.advance_stream(ctx)
                } else {
                    self.stop_playback();
                    if follow_up && !self.is_dictating() {
                        self.start(Mode::FollowUp, ctx)
                    } else {
                        Ok(())
                    }
                };
                if let Err(error) = result {
                    return Some(VoiceUpdate::Notice(error));
                }
                if self.playback.is_some() {
                    return None;
                }
            }
        }
        match &mut self.input {
            Input::Idle => return None,
            Input::Permission(mode, wait) => {
                let mode = *mode;
                return match wait.try_recv() {
                    Ok(()) => self.start(mode, ctx).err().map(VoiceUpdate::Notice),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        self.input = Input::Idle;
                        self.wake = false;
                        Some(VoiceUpdate::Notice("음성 권한 확인이 중단됐어.".into()))
                    }
                };
            }
            Input::Retry(deadline) => {
                if now < *deadline {
                    ctx.request_repaint_after(*deadline - now);
                    return None;
                }
                return self.start(Mode::Wake, ctx).err().map(VoiceUpdate::Notice);
            }
            Input::Recording(_) => (),
        }
        let Input::Recording(recording) = &mut self.input else {
            return None;
        };
        if let Some((worker, deadline)) = &recording.fallback {
            let result = match worker.output.try_recv() {
                Ok(Ok(text)) if !text.trim().is_empty() => VoiceUpdate::Transcript {
                    text,
                    interrupted_reply: recording.interrupted_reply,
                },
                Ok(Ok(_)) => VoiceUpdate::Notice(
                    "입력 소리는 받았지만 한국어 문장을 인식하지 못했어.".into(),
                ),
                Ok(Err(error)) => VoiceUpdate::Notice(error),
                Err(mpsc::TryRecvError::Empty) if now < *deadline => {
                    ctx.request_repaint_after(*deadline - now);
                    return None;
                }
                Err(_) => VoiceUpdate::Notice("로컬 음성 인식 응답을 받지 못했어.".into()),
            };
            self.resume_wake(ctx);
            return Some(result);
        }
        let mut result = None;
        while let Some(event) = recording.handle.event() {
            match event {
                Recognition::Partial(text) => {
                    let text = after_prefix(&text, &recording.ignored_prefix).to_owned();
                    let accept = recording.mode != Mode::Wake
                        || (speaking && recording.handle.echo_cancelled())
                        || wake_prefix(&text).is_some();
                    if accept && text != recording.text && !text.trim().is_empty() {
                        recording.last_word = Some(now);
                        recording.text = text;
                    }
                }
                Recognition::Final(text) => {
                    result = Some(Ok(after_prefix(&text, &recording.ignored_prefix).to_owned()));
                    break;
                }
                Recognition::Failed(error) => {
                    result = Some(Err(error));
                    break;
                }
            }
        }
        if recording.mode == Mode::Wake {
            let heard = result
                .as_ref()
                .and_then(|value| value.as_ref().ok())
                .unwrap_or(&recording.text);
            let wake = wake_prefix(heard).filter(|_| {
                !speaking
                    || !self
                        .playback
                        .as_ref()
                        .is_some_and(|playback| playback.text.starts_with(&normalized(heard)))
            });
            let interrupt = self
                .playback
                .as_ref()
                .filter(|playback| {
                    speaking
                        && recording.handle.echo_cancelled()
                        && now.duration_since(playback.since) > Duration::from_millis(280)
                })
                .and_then(|playback| {
                    let new_words = after_prefix(heard, &playback.old_transcript);
                    let new_voice = normalized(new_words);
                    let is_stop = matches!(new_voice.as_str(), "잠깐" | "그만" | "멈춰");
                    ((is_stop || new_voice.chars().count() >= 6)
                        && !playback.text.contains(&new_voice))
                    .then_some((
                        playback.old_transcript.clone(),
                        new_words.to_owned(),
                        is_stop,
                    ))
                });
            if wake.is_some() || interrupt.is_some() {
                let interrupted_reply = self
                    .playback
                    .as_ref()
                    .is_some_and(|playback| playback.follow_up || self.stream.is_some());
                let (ignored, words, is_stop) = match wake {
                    Some(prefix) => {
                        let words = after_prefix(heard, prefix).to_owned();
                        let stop = matches!(
                            normalized(&words).as_str(),
                            "그만" | "멈춰" | "대화끝" | "음성대화끝"
                        );
                        (prefix.to_owned(), words, stop)
                    }
                    None => interrupt.expect("checked above"),
                };
                self.quick_failures = 0;
                self.engine.stop_speaking();
                self.engine.warm();
                self.playback = None;
                self.stream = None;
                self.reply_listening = false;
                if is_stop {
                    self.resume_wake(ctx);
                    return None;
                }
                if result.is_some() {
                    if !words.is_empty() {
                        self.resume_wake(ctx);
                        return Some(VoiceUpdate::Transcript {
                            text: words,
                            interrupted_reply,
                        });
                    }
                    return Some(match self.start(Mode::Dictation, ctx) {
                        Ok(()) => {
                            if let Input::Recording(recording) = &mut self.input {
                                recording.interrupted_reply = interrupted_reply;
                            }
                            VoiceUpdate::Woke
                        }
                        Err(error) => VoiceUpdate::Notice(error),
                    });
                }
                // Keep the current recording so a word spoken just after the wake word
                // (or the start of an interruption) stays in the user's message.
                recording.mode = Mode::Dictation;
                recording.start = now;
                recording.last_word = (!words.is_empty()).then_some(now);
                recording.ignored_prefix = ignored;
                recording.text = words;
                recording.interrupted_reply = interrupted_reply;
                ctx.request_repaint();
                return Some(VoiceUpdate::Woke);
            }
            let elapsed = now.duration_since(recording.start);
            if result.is_some() || elapsed >= Duration::from_secs(45) {
                if result.as_ref().is_some_and(Result::is_err) && elapsed < Duration::from_secs(3) {
                    self.quick_failures += 1;
                    if self.quick_failures >= 3 {
                        self.wake = false;
                        self.input = Input::Idle;
                        return Some(VoiceUpdate::Notice("호출어 인식을 계속 시작할 수 없어. 마이크와 한국어 음성 인식 상태를 확인해줘.".into()));
                    }
                } else {
                    self.quick_failures = 0;
                }
                self.resume_wake(ctx);
            } else {
                ctx.request_repaint_after(Duration::from_secs(45) - elapsed);
            }
            return None;
        }
        if result.is_none() {
            let initial_wait = if recording.mode == Mode::FollowUp {
                12
            } else {
                10
            };
            let deadline = recording.last_word.map_or(
                recording.start + Duration::from_secs(initial_wait),
                |last| {
                    (last + Duration::from_secs(2)).min(recording.start + Duration::from_secs(45))
                },
            );
            if now >= deadline && recording.mode == Mode::FollowUp && recording.last_word.is_none()
            {
                self.resume_wake(ctx);
                return None;
            }
            if let Some(ending) = recording.ending {
                if now >= ending {
                    result = Some(Ok(recording.text.clone()));
                }
            } else if now >= deadline {
                finish(recording, now);
            }
            if result.is_none() {
                ctx.request_repaint_after(
                    recording
                        .ending
                        .unwrap_or(deadline)
                        .saturating_duration_since(now),
                );
                return None;
            }
        }
        let result = result?;
        if recording.mode == Mode::Dictation
            && recording.text.trim().is_empty()
            && result.as_ref().map_or(true, |text| text.trim().is_empty())
        {
            match recording.handle.fallback(ctx) {
                Ok(Some(worker)) => {
                    recording.ending = Some(now);
                    recording.fallback = Some((worker, now + Duration::from_secs(20)));
                    ctx.request_repaint();
                    return None;
                }
                Err(error) => {
                    self.resume_wake(ctx);
                    return Some(VoiceUpdate::Notice(error));
                }
                Ok(None) => (),
            }
        }
        if recording.mode == Mode::FollowUp {
            let words = result
                .as_ref()
                .map(String::as_str)
                .unwrap_or(&recording.text);
            if words.trim().is_empty()
                || matches!(normalized(words).as_str(), "그만" | "대화끝" | "음성대화끝")
            {
                self.resume_wake(ctx);
                return None;
            }
        }
        let interrupted_reply = recording.interrupted_reply;
        let output = match result {
            Ok(text) if !text.trim().is_empty() => VoiceUpdate::Transcript {
                text,
                interrupted_reply,
            },
            _ if !recording.text.trim().is_empty() => VoiceUpdate::Transcript {
                text: recording.text.clone(),
                interrupted_reply,
            },
            Ok(_) => VoiceUpdate::Notice("인식한 음성이 없어.".into()),
            Err(error) if error.eq_ignore_ascii_case("no speech detected") => VoiceUpdate::Notice(
                "마이크에서 말소리를 확인하지 못했어. 시스템 설정의 사운드 → 입력에서 말할 때 입력 레벨이 움직이는지 확인해줘.".into(),
            ),
            Err(error) => VoiceUpdate::Notice(format!("음성 인식이 끝났어 · {error}")),
        };
        self.resume_wake(ctx);
        Some(output)
    }
}

fn wake_prefix(text: &str) -> Option<&str> {
    let text = text.trim_start();
    const NAME: &str = "프라이데이";
    if text.starts_with(NAME) {
        Some(NAME)
    } else {
        text.get(..6)
            .filter(|prefix| prefix.eq_ignore_ascii_case("friday"))
    }
}

fn after_prefix<'a>(text: &'a str, prefix: &str) -> &'a str {
    text.trim_start()
        .strip_prefix(prefix)
        .unwrap_or(text)
        .trim_start_matches(|ch: char| ch.is_whitespace() || ",:!.".contains(ch))
}

fn normalized(text: &str) -> String {
    text.chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

pub(crate) fn take_speech_chunk(queue: &mut VecDeque<String>) -> Option<String> {
    let mut text = queue.pop_front()?;
    let mut count = text.chars().count();
    while let Some(next) = queue.front() {
        let next_count = next.chars().count();
        if count + next_count > 135 {
            break;
        }
        let next = queue.pop_front().expect("next speech chunk");
        text.push('\n');
        text.push_str(&next);
        count += next_count + 1;
    }
    Some(text)
}

pub(crate) fn spoken_text(text: &str) -> String {
    let mut fenced = false;
    let mut spoken = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            fenced = !fenced;
        } else if !fenced && !line.is_empty() && !line.starts_with("[실행 결과물]") {
            if !spoken.is_empty() {
                spoken.push(' ');
            }
            let line = line.trim_start_matches(['#', '-', '>', ' ']);
            let line = line
                .strip_prefix("확인됨: ")
                .or_else(|| line.strip_prefix("추정: "))
                .unwrap_or(line);
            append_spoken_line(&mut spoken, line);
        }
    }
    spoken.chars().take(600).collect()
}

fn append_spoken_line(spoken: &mut String, mut line: &str) {
    while let Some((before, remainder)) = line.split_once('[') {
        spoken.extend(before.chars().filter(|ch| !"`*_".contains(*ch)));
        let Some((label, destination)) = remainder.split_once("](") else {
            line = remainder.trim_start_matches(']');
            continue;
        };
        let Some((_, after)) = destination.split_once(')') else {
            line = remainder;
            continue;
        };
        spoken.extend(label.chars().filter(|ch| !"`*_".contains(*ch)));
        line = after;
    }
    spoken.extend(line.chars().filter(|ch| !"`*_".contains(*ch)));
}

fn finish(recording: &mut Recording, now: Instant) {
    if recording.ending.is_none() {
        recording.handle.finish_audio();
        recording.ending = Some(now + Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
        rc::Rc,
    };

    #[derive(Clone, Default)]
    struct TestEngine {
        talking: Rc<Cell<bool>>,
        words: Rc<RefCell<VecDeque<Recognition>>>,
        spoken: Rc<RefCell<Vec<String>>>,
    }

    impl VoiceEngine for TestEngine {
        fn permission(&self, _: &egui::Context) -> Result<Option<Receiver<()>>, String> {
            Ok(None)
        }

        fn listen(
            &self,
            _: &egui::Context,
            _: bool,
            _: bool,
        ) -> Result<Box<dyn Listening>, String> {
            Ok(Box::new(self.clone()))
        }

        fn speak(&self, text: &str) -> Result<(), String> {
            self.spoken.borrow_mut().push(text.to_owned());
            self.talking.set(true);
            Ok(())
        }

        fn speaking(&self) -> bool {
            self.talking.get()
        }
        fn stop_speaking(&self) {
            self.talking.set(false);
        }
    }

    impl Listening for TestEngine {
        fn event(&self) -> Option<Recognition> {
            self.words.borrow_mut().pop_front()
        }
        fn finish_audio(&mut self) {}
        fn echo_cancelled(&self) -> bool {
            true
        }
    }

    #[test]
    fn spoken_reply_accepts_a_follow_up_without_rearming_manual_reading() {
        let ctx = egui::Context::default();
        let engine = TestEngine::default();
        let mut voice = Voice::new(engine.clone());
        assert_eq!(
            spoken_text("확인됨: [작업 결과](https://example.com)를 읽었어."),
            "작업 결과를 읽었어."
        );

        voice.reply("확인했어.", &ctx).unwrap();
        engine.talking.set(false);
        voice.playback.as_mut().unwrap().since -= Duration::from_secs(1);
        assert!(voice.tick(&ctx).is_none());
        assert!(voice.following_up() && voice.listening());

        engine
            .words
            .borrow_mut()
            .push_back(Recognition::Final("다음 것도 확인해줘".into()));
        assert!(
            matches!(voice.tick(&ctx), Some(VoiceUpdate::Transcript { text, interrupted_reply: false }) if text == "다음 것도 확인해줘")
        );
        assert!(!voice.listening());

        voice.speak("읽어 줄 문장이야.", &ctx).unwrap();
        engine.talking.set(false);
        voice.playback.as_mut().unwrap().since -= Duration::from_secs(1);
        assert!(voice.tick(&ctx).is_none());
        assert!(!voice.listening());

        voice.reply("답했어.", &ctx).unwrap();
        engine.talking.set(false);
        voice.playback.as_mut().unwrap().since -= Duration::from_secs(1);
        voice.tick(&ctx);
        if let Input::Recording(recording) = &mut voice.input {
            recording.start -= Duration::from_secs(13);
        }
        assert!(voice.tick(&ctx).is_none());
        assert!(!voice.listening());
    }

    #[test]
    fn user_interruption_stops_streaming_and_keeps_the_new_transcript() {
        let ctx = egui::Context::default();
        let engine = TestEngine::default();
        let mut voice = Voice::new(engine.clone());
        voice.begin_stream(42);
        voice
            .queue_stream(42, "프라이데이 사용 방법부터 설명할게.".into(), &ctx)
            .unwrap();
        voice
            .queue_stream(42, "이어지는 설명이야.".into(), &ctx)
            .unwrap();
        voice.playback.as_mut().unwrap().since -= Duration::from_secs(1);

        engine
            .words
            .borrow_mut()
            .push_back(Recognition::Partial("프라이데이 사용 방법부터".into()));
        assert!(voice.tick(&ctx).is_none());
        assert!(engine.talking.get());

        engine
            .words
            .borrow_mut()
            .push_back(Recognition::Partial("설명을 한 문장으로 줄여줘".into()));
        assert!(matches!(voice.tick(&ctx), Some(VoiceUpdate::Woke)));
        assert!(!engine.talking.get());
        engine
            .words
            .borrow_mut()
            .push_back(Recognition::Final("설명을 한 문장으로 줄여줘".into()));
        assert!(matches!(
            voice.tick(&ctx),
            Some(VoiceUpdate::Transcript { text, interrupted_reply: true })
                if text == "설명을 한 문장으로 줄여줘"
        ));
        voice
            .finish_stream(42, Some("이어지는 설명이야."), &ctx)
            .unwrap();
        assert_eq!(
            *engine.spoken.borrow(),
            ["프라이데이 사용 방법부터 설명할게."]
        );
    }

    #[test]
    fn native_pcm_is_resampled_to_whisper_rate() {
        let samples: Vec<_> = (0..480).map(|index| index as f32 / 480.0).collect();
        let resampled = resample_for_whisper(&samples, 48_000.0);
        assert_eq!(resampled.len(), 160);
        assert_eq!(resampled[1], samples[3]);
        assert_eq!(resampled[159], samples[477]);
        assert!(resample_for_whisper(&[], 48_000.0).is_empty());
    }

    #[test]
    fn waiting_for_the_wake_word_does_not_enter_dictation() {
        let ctx = egui::Context::default();
        let engine = TestEngine::default();
        let mut voice = Voice::new(engine.clone());
        voice.toggle_wake(&ctx).unwrap();
        assert!(voice.listening());
        assert_eq!(voice.dictation_mode(), None);
        engine
            .words
            .borrow_mut()
            .push_back(Recognition::Partial("오늘 날씨 좋다".into()));
        assert!(voice.tick(&ctx).is_none());
        assert_eq!(voice.status(), "‘프라이데이’ 호출 대기 중");
        engine
            .words
            .borrow_mut()
            .push_back(Recognition::Partial("프라이데이, 테스트할게".into()));
        assert!(matches!(voice.tick(&ctx), Some(VoiceUpdate::Woke)));
        assert_eq!(voice.dictation_mode(), Some("listening"));
        assert_eq!(voice.status(), "테스트할게");
    }

    #[test]
    fn streamed_sentences_are_sequential_and_stop_does_not_replay_the_final_text() {
        let ctx = egui::Context::default();
        let engine = TestEngine::default();
        let mut voice = Voice::new(engine.clone());
        voice.begin_stream(42);
        voice.queue_stream(42, "첫 문장이야.".into(), &ctx).unwrap();
        voice
            .queue_stream(42, "둘째 문장이야.".into(), &ctx)
            .unwrap();
        assert_eq!(*engine.spoken.borrow(), ["첫 문장이야."]);

        engine.talking.set(false);
        voice.playback.as_mut().unwrap().since -= Duration::from_secs(1);
        voice.tick(&ctx);
        assert_eq!(*engine.spoken.borrow(), ["첫 문장이야.", "둘째 문장이야."]);
        assert!(!voice.following_up());

        voice.stop_speaking();
        voice
            .queue_stream(42, "셋째 문장이야.".into(), &ctx)
            .unwrap();
        voice
            .finish_stream(42, Some("전체 답변이야."), &ctx)
            .unwrap();
        assert_eq!(*engine.spoken.borrow(), ["첫 문장이야.", "둘째 문장이야."]);
        assert!(!voice.listening());
    }
}
