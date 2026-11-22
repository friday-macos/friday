use crate::{local_stt, neural, speech_language::SpeechLanguage, voice};
use base64::{Engine, engine::general_purpose::STANDARD};
use block2::RcBlock;
use eframe::egui;
use objc2::{AnyThread, rc::Retained};
use objc2_avf_audio::{AVAudioFormat, AVAudioPCMBuffer};
use objc2_foundation::NSError;
use objc2_speech::{
    SFSpeechAudioBufferRecognitionRequest, SFSpeechRecognitionResult, SFSpeechRecognitionTask,
    SFSpeechRecognizer, SFSpeechRecognizerAuthorizationStatus,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant},
};

const RATE: usize = 16_000;
const PACKET: usize = RATE * 2;
const UTTERANCE: usize = RATE * 30 * 2;

struct AppleAsr {
    _recognizer: Retained<SFSpeechRecognizer>,
    request: Retained<SFSpeechAudioBufferRecognitionRequest>,
    task: Retained<SFSpeechRecognitionTask>,
    format: Retained<AVAudioFormat>,
    events: Receiver<Result<(String, bool), String>>,
    bytes: usize,
    finished: Option<Instant>,
}

impl AppleAsr {
    fn start(ctx: &egui::Context, language: SpeechLanguage) -> Result<Self, String> {
        unsafe {
            if SFSpeechRecognizer::authorizationStatus()
                != SFSpeechRecognizerAuthorizationStatus::Authorized
            {
                return Err("Mac의 프라이데이에 음성 인식 권한을 허용해줘.".into());
            }
            let recognizer = voice::MacVoice::recognizer(language)?;
            if !recognizer.isAvailable() {
                return Err(format!(
                    "Speech recognition is unavailable for {} on the Mac.",
                    language.name
                ));
            }
            let format = AVAudioFormat::initStandardFormatWithSampleRate_channels(
                AVAudioFormat::alloc(),
                RATE as f64,
                1,
            )
            .ok_or("웹 마이크 형식을 열지 못했어.")?;
            let request = SFSpeechAudioBufferRecognitionRequest::new();
            request.setRequiresOnDeviceRecognition(true);
            request.setShouldReportPartialResults(true);
            let (tx, events) = mpsc::channel();
            let ctx = ctx.clone();
            let callback = RcBlock::new(
                move |result: *mut SFSpeechRecognitionResult, error: *mut NSError| {
                    let value = if let Some(result) = result.as_ref() {
                        Ok((
                            result.bestTranscription().formattedString().to_string(),
                            result.isFinal(),
                        ))
                    } else if let Some(error) = error.as_ref() {
                        Err(error.localizedDescription().to_string())
                    } else {
                        return;
                    };
                    let _ = tx.send(value);
                    ctx.request_repaint();
                },
            );
            let task = recognizer.recognitionTaskWithRequest_resultHandler(&request, &callback);
            Ok(Self {
                _recognizer: recognizer,
                request,
                task,
                format,
                events,
                bytes: 0,
                finished: None,
            })
        }
    }

    fn feed(&mut self, packet: &[u8]) -> Result<(), String> {
        validate_packet(packet, self.bytes, self.finished.is_some())?;
        let frames = (packet.len() / 2) as u32;
        unsafe {
            let buffer = AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
                AVAudioPCMBuffer::alloc(),
                &self.format,
                frames,
            )
            .ok_or("마이크 조각을 준비하지 못했어.")?;
            buffer.setFrameLength(frames);
            let channel = buffer
                .floatChannelData()
                .as_ref()
                .ok_or("마이크 채널을 열지 못했어.")?;
            let out = std::slice::from_raw_parts_mut(channel.as_ptr(), frames as usize);
            for (sample, chunk) in out.iter_mut().zip(packet.as_chunks::<2>().0) {
                *sample = f32::from(i16::from_le_bytes(*chunk)) / 32768.0;
            }
            self.request.appendAudioPCMBuffer(&buffer);
        }
        self.bytes += packet.len();
        Ok(())
    }

    fn commit(&mut self) {
        if self.finished.is_none() {
            unsafe { self.request.endAudio() };
            self.finished = Some(Instant::now() + Duration::from_secs(3));
        }
    }
}

impl Drop for AppleAsr {
    fn drop(&mut self) {
        unsafe { self.task.cancel() };
    }
}

struct LocalAsr {
    samples: Vec<f32>,
    finished: Option<Instant>,
}

enum Asr {
    Apple(AppleAsr),
    Local(LocalAsr),
}

fn validate_packet(packet: &[u8], received: usize, finished: bool) -> Result<(), String> {
    if finished
        || packet.is_empty()
        || !packet.len().is_multiple_of(2)
        || packet.len() > PACKET
        || received + packet.len() > UTTERANCE
    {
        return Err("마이크 조각이 비었거나 녹음 길이를 넘었어.".into());
    }
    Ok(())
}

pub struct Session {
    language: SpeechLanguage,
    key: String,
    seen: Instant,
    asr: Option<Asr>,
    local_asr: Option<local_stt::Worker>,
    transcript: String,
    submit: Option<String>,
    pub turn: Option<i64>,
    notice: Option<String>,
    // The browser advances its cursor after it schedules the packets.
    generation: u64,
    next_packet: u64,
    packets: VecDeque<Value>,
    voice: neural::Process,
    render_serial: u64,
    rendering: Option<Instant>,
    queue: VecDeque<String>,
    has_speech: bool,
    turn_done: bool,
}

impl Session {
    pub fn new(
        model: &Path,
        local_model: Option<PathBuf>,
        ctx: &egui::Context,
        language: SpeechLanguage,
    ) -> Result<Self, String> {
        if !language.uses_neural_voice() {
            return Err("Web voice currently uses English or Korean. For other languages, use Mac voice or web text.".into());
        }
        let mut random = [0; 18];
        getrandom::fill(&mut random).map_err(|error| error.to_string())?;
        let helper = std::env::current_exe()
            .map_err(|error| error.to_string())?
            .with_file_name("jarvis-voice");
        if !helper.is_file() || !neural::ready(model) {
            return Err("Mac에서 로컬 Qwen3 음성 모델을 찾지 못했어.".into());
        }
        let local_asr = local_model
            .map(|model| local_stt::Worker::start(model, ctx.clone(), language))
            .transpose()?;
        let voice = neural::Process::start(&helper, model, true)?;
        Ok(Self {
            language,
            key: STANDARD.encode(random),
            seen: Instant::now(),
            asr: None,
            local_asr,
            transcript: String::new(),
            submit: None,
            turn: None,
            notice: None,
            generation: 0,
            next_packet: 0,
            packets: VecDeque::new(),
            voice,
            render_serial: 0,
            rendering: None,
            queue: VecDeque::new(),
            has_speech: false,
            turn_done: false,
        })
    }

    pub fn key(&self) -> &str {
        &self.key
    }
    pub fn expired(&self) -> bool {
        self.seen.elapsed() >= Duration::from_secs(20)
    }
    pub fn touch(&mut self) {
        self.seen = Instant::now();
    }
    pub fn matches(&self, key: Option<&str>) -> bool {
        key == Some(&self.key)
    }

    pub fn listen(&mut self, ctx: &egui::Context) -> Result<Option<i64>, String> {
        if self.asr.is_some() {
            return Err("이미 음성을 받고 있어.".into());
        }
        let asr = if self.local_asr.is_some() {
            Asr::Local(LocalAsr {
                samples: Vec::new(),
                finished: None,
            })
        } else {
            Asr::Apple(AppleAsr::start(ctx, self.language)?)
        };
        self.stop_output();
        self.notice = None;
        self.transcript.clear();
        self.submit = None;
        self.asr = Some(asr);
        Ok(self.turn.take())
    }

    pub fn feed(&mut self, packet: &str) -> Result<(), String> {
        let bytes = STANDARD
            .decode(packet)
            .map_err(|_| "마이크 조각을 읽지 못했어.")?;
        match self.asr.as_mut().ok_or("먼저 듣기를 시작해줘.")? {
            Asr::Apple(asr) => asr.feed(&bytes),
            Asr::Local(asr) => {
                validate_packet(&bytes, asr.samples.len() * 2, asr.finished.is_some())?;
                asr.samples.extend(
                    bytes
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|sample| f32::from(i16::from_le_bytes(*sample)) / 32768.0),
                );
                Ok(())
            }
        }
    }

    pub fn commit(&mut self) -> Result<(), String> {
        match self.asr.as_mut().ok_or("먼저 듣기를 시작해줘.")? {
            Asr::Apple(asr) => asr.commit(),
            Asr::Local(asr) if asr.finished.is_none() => {
                let worker = self
                    .local_asr
                    .as_ref()
                    .expect("local capture owns a worker");
                worker.submit(std::mem::take(&mut asr.samples))?;
                asr.finished = Some(Instant::now() + Duration::from_secs(12));
            }
            Asr::Local(_) => (),
        }
        Ok(())
    }

    pub fn transcript(&mut self, idle: bool) -> Option<String> {
        if idle { self.submit.take() } else { None }
    }

    pub fn set_notice(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    pub fn queue(&mut self, id: i64, text: &str) {
        if self.turn != Some(id) {
            return;
        }
        let text = voice::spoken_text(text);
        if !text.is_empty() {
            self.has_speech = true;
            self.queue.push_back(text);
            self.render();
        }
    }

    pub fn finish(&mut self, id: i64, text: Result<&str, &str>) {
        if self.turn != Some(id) {
            return;
        }
        match text {
            Ok(text) if !self.has_speech => self.queue(id, text),
            Ok(_) => (),
            Err(error) => self.notice = Some(error.to_owned()),
        }
        self.turn_done = true;
    }

    fn stop_output(&mut self) {
        if self.rendering.take().is_some() {
            let _ = self.voice.send(&json!({"id":0}));
        }
        self.generation += 1;
        self.next_packet = 0;
        self.packets.clear();
        self.queue.clear();
        self.has_speech = false;
        self.turn_done = false;
    }

    fn render(&mut self) {
        if self.rendering.is_none()
            && self.packets.len() < 96
            && let Some(text) = voice::take_speech_chunk(&mut self.queue)
        {
            self.render_serial += 1;
            match self
                .voice
                .send(&json!({"id":self.render_serial,"text":text,"language":self.language.code}))
            {
                Ok(()) => self.rendering = Some(Instant::now() + Duration::from_secs(85)),
                Err(error) => {
                    self.notice = Some(error);
                    self.queue.clear();
                }
            }
        }
    }

    pub fn poll(&mut self) {
        if let Some(Asr::Apple(asr)) = &self.asr {
            let mut done = false;
            for event in asr.events.try_iter() {
                match event {
                    Ok((text, is_final)) => {
                        self.transcript = text;
                        if is_final {
                            done = true;
                            break;
                        }
                    }
                    Err(error) => {
                        if self.transcript.trim().is_empty() {
                            self.notice = Some(format!("음성을 인식하지 못했어 · {error}"));
                        }
                        done = true;
                        break;
                    }
                }
            }
            if done
                || asr
                    .finished
                    .is_some_and(|deadline| Instant::now() >= deadline)
            {
                let text = self.transcript.trim();
                if !text.is_empty() {
                    self.submit = Some(text.to_owned());
                }
                self.asr = None;
            }
        }
        if let Some(worker) = &self.local_asr
            && let Some(Asr::Local(asr)) = &self.asr
        {
            match worker.output.try_recv() {
                Ok(Ok(text)) => {
                    self.transcript = text;
                    if self.transcript.is_empty() {
                        self.notice = Some("인식한 음성이 없어.".into());
                    } else {
                        self.submit = Some(self.transcript.clone());
                    }
                    self.asr = None;
                }
                Ok(Err(error)) => {
                    self.notice = Some(error);
                    self.asr = None;
                }
                Err(TryRecvError::Disconnected) => {
                    self.notice = Some("로컬 음성 인식 연결이 끊겼어.".into());
                    self.asr = None;
                }
                Err(TryRecvError::Empty)
                    if asr
                        .finished
                        .is_some_and(|deadline| Instant::now() >= deadline) =>
                {
                    self.notice = Some("음성 인식 시간이 너무 길어.".into());
                    self.asr = None;
                    self.local_asr = None;
                }
                Err(TryRecvError::Empty) => (),
            }
        }
        if self
            .rendering
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.stop_output();
            self.turn_done = true;
            self.notice = Some("한국어 음성 생성 시간이 너무 길어.".into());
        }
        while self.packets.len() < 96 {
            let event = match self.voice.events.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.rendering.take().is_some() {
                        self.notice = Some("로컬 음성 엔진이 종료됐어.".into());
                    }
                    self.queue.clear();
                    break;
                }
            };
            if event["event"] == "fatal" {
                self.notice = Some("로컬 Qwen3 음성을 열지 못했어.".into());
                self.rendering = None;
                self.queue.clear();
            }
            if self.rendering.is_none() || event["id"] != self.render_serial {
                continue;
            }
            match event["event"].as_str() {
                Some("pcm") => {
                    self.packets.push_back(
                        json!({"seq":self.next_packet,"rate":event["rate"],"data":event["data"]}),
                    );
                    self.next_packet += 1;
                }
                Some("finished" | "error") => {
                    if event["event"] == "error" {
                        self.notice = Some("한국어 음성 조각을 생성하지 못했어.".into());
                    }
                    self.rendering = None;
                }
                _ => (),
            }
        }
        self.render();
    }

    pub fn status(&mut self, generation: Option<u64>, cursor: u64) -> Value {
        self.touch();
        let cursor = if generation == Some(self.generation) {
            cursor
        } else {
            0
        };
        while self
            .packets
            .front()
            .is_some_and(|packet| packet["seq"].as_u64().is_some_and(|seq| seq < cursor))
        {
            self.packets.pop_front();
        }
        let packets: Vec<_> = self.packets.iter().take(8).cloned().collect();
        json!({"generation":self.generation,"packets":packets,"mode":self.phase(),"transcript":self.transcript,"error":self.notice})
    }

    pub fn phase(&self) -> &'static str {
        if let Some(asr) = &self.asr {
            if match asr {
                Asr::Apple(asr) => asr.finished.is_some(),
                Asr::Local(asr) => asr.finished.is_some(),
            } {
                "recognizing"
            } else {
                "listening"
            }
        } else if self.rendering.is_some() || !self.queue.is_empty() || !self.packets.is_empty() {
            "speaking"
        } else if self.submit.is_some() || (self.turn.is_some() && !self.turn_done) {
            "working"
        } else {
            "idle"
        }
    }
}
