use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

pub struct Player {
    model: PathBuf,
    executable: PathBuf,
    process: RefCell<Option<Process>>,
    current: RefCell<Option<Utterance>>,
    next_id: Cell<u64>,
    failed: Cell<bool>,
}

pub(crate) struct Process {
    child: Child,
    command: ChildStdin,
    pub(crate) events: Receiver<Value>,
}

struct Utterance {
    id: u64,
    text: String,
    played: bool,
    level: Option<(Instant, f32)>,
    deadline: Instant,
}

pub struct Failure {
    pub text: String,
    pub played: bool,
    pub message: String,
}

impl Player {
    pub fn installed(model: PathBuf) -> Option<Self> {
        let executable = std::env::current_exe().ok()?.with_file_name("jarvis-voice");
        if !executable.is_file() || !ready(&model) {
            return None;
        }
        Some(Self {
            model,
            executable,
            process: RefCell::new(None),
            current: RefCell::new(None),
            next_id: Cell::new(0),
            failed: Cell::new(false),
        })
    }

    pub fn warm(&self) -> Result<(), String> {
        if self.failed.get() {
            return Err("로컬 신경망 음성 초기화에 실패해 macOS 음성을 사용해.".into());
        }
        let mut slot = self.process.borrow_mut();
        if let Some(process) = slot.as_mut()
            && matches!(process.child.try_wait(), Ok(None))
        {
            return Ok(());
        }
        *slot = Some(Process::start(&self.executable, &self.model, false)?);
        Ok(())
    }

    pub fn speak(&self, text: &str, volume: u16) -> Result<(), String> {
        self.warm()?;
        let id = self.next_id.get().wrapping_add(1).max(1);
        self.next_id.set(id);
        let mut process = self.process.borrow_mut();
        let Some(process) = process.as_mut() else {
            return Err("로컬 음성 엔진을 열 수 없어.".into());
        };
        process.send(&json!({"id": id, "text": text, "volume": volume}))?;
        *self.current.borrow_mut() = Some(Utterance {
            id,
            text: text.to_owned(),
            played: false,
            level: None,
            deadline: Instant::now() + Duration::from_secs(45),
        });
        Ok(())
    }

    pub fn set_volume(&self, volume: u16) {
        if let Some(process) = self.process.borrow_mut().as_mut() {
            let _ = process.send(&json!({"volume": volume}));
        }
    }

    pub fn speaking(&self) -> bool {
        self.current.borrow().is_some()
    }

    pub fn playback_level(&self) -> Option<f32> {
        let current = self.current.borrow();
        let utterance = current.as_ref()?;
        let level = utterance
            .level
            .filter(|(at, _)| at.elapsed() < Duration::from_millis(120))
            .map_or(0.0, |(_, level)| level);
        Some(level)
    }

    pub fn stop(&self) {
        if self.current.borrow_mut().take().is_some()
            && let Some(process) = self.process.borrow_mut().as_mut()
        {
            let _ = process.send(&json!({"id": 0}));
        }
    }

    pub fn poll(&self) -> Option<Failure> {
        let mut slot = self.process.borrow_mut();
        let process = slot.as_mut()?;
        let mut current = self.current.borrow_mut();
        let active = current.as_mut()?;
        let failure = loop {
            if Instant::now() >= active.deadline {
                break "음성 생성 시간이 너무 길어.".to_owned();
            }
            match process.events.try_recv() {
                Ok(event) if event["event"] == "fatal" => {
                    self.failed.set(true);
                    break event["message"]
                        .as_str()
                        .unwrap_or("로컬 음성 엔진이 초기화되지 않았어.")
                        .to_owned();
                }
                Ok(event) if event["id"] == active.id => match event["event"].as_str() {
                    Some("playing") => {
                        active.played = true;
                        active.deadline = Instant::now() + Duration::from_secs(120);
                    }
                    Some("level") => {
                        if let Some(level) = event["rms"].as_f64() {
                            active.level = Some((Instant::now(), level.clamp(0.0, 1.0) as f32));
                        }
                    }
                    Some("finished") => {
                        *current = None;
                        return None;
                    }
                    Some("error") => {
                        break event["message"]
                            .as_str()
                            .unwrap_or("음성을 만들지 못했어.")
                            .to_owned();
                    }
                    _ => {}
                },
                Ok(_) => {}
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    break "로컬 음성 엔진 연결이 끊겼어.".to_owned();
                }
            }
        };
        slot.take();
        current.take().map(|active| Failure {
            text: active.text,
            played: active.played,
            message: failure,
        })
    }
}

pub(crate) fn ready(model: &Path) -> bool {
    [
        "config.json",
        "model.safetensors",
        "speech_tokenizer/model.safetensors",
        "vocab.json",
        "merges.txt",
    ]
    .iter()
    .all(|file| model.join(file).is_file())
}

impl Process {
    pub(crate) fn start(executable: &Path, model: &Path, remote: bool) -> Result<Self, String> {
        let mut command = Command::new(executable);
        command.arg(model);
        if remote {
            command.arg("--pcm");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        let command = child.stdin.take().expect("piped stdin");
        let output = child.stdout.take().expect("piped stdout");
        let (sender, events) = mpsc::sync_channel(128);
        let process = Self {
            child,
            command,
            events,
        };
        thread::Builder::new()
            .name("jarvis-voice-events".into())
            .spawn(move || {
                for line in BufReader::new(output).lines().map_while(Result::ok) {
                    if let Ok(event) = serde_json::from_str::<Value>(&line)
                        && sender.send(event).is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(process)
    }

    pub(crate) fn send(&mut self, message: &Value) -> Result<(), String> {
        writeln!(self.command, "{message}")
            .and_then(|()| self.command.flush())
            .map_err(|error| error.to_string())
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
