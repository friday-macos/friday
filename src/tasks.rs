use crate::{
    codex,
    control::{Approval, ConnectionToApp},
};
use eframe::egui;
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread,
};

pub type Outcome = Result<String, String>;

pub struct Task {
    pub id: i64,
    pub background: bool,
    pub speak_reply: bool,
    cancelled: Arc<AtomicBool>,
    result: Receiver<Outcome>,
    speech: Receiver<String>,
    approvals: Receiver<Approval>,
    progress_updates: Receiver<codex::Progress>,
    progress: codex::Progress,
    pub approval: Option<Approval>,
}

pub struct Request {
    pub id: i64,
    pub background: bool,
    pub speak_reply: bool,
    pub state_dir: PathBuf,
    pub workspace: Option<PathBuf>,
    pub input: Value,
    pub project: String,
    pub ctx: egui::Context,
}

impl Task {
    pub fn start(request: Request) -> std::io::Result<Self> {
        let Request {
            id,
            background,
            speak_reply,
            state_dir,
            workspace,
            input,
            project,
            ctx,
        } = request;
        let (send_approval, approvals) = mpsc::channel();
        let directory = state_dir
            .parent()
            .expect("Codex state is in the JARVIS data directory");
        let connection = ConnectionToApp {
            background_job: background.then_some(id),
            project,
            artifacts: directory.join("control").join(id.to_string()),
            database: directory.join("memory.sqlite3"),
            approvals: send_approval,
            ctx: ctx.clone(),
        };
        let (sender, result) = mpsc::channel();
        let (send_speech, speech) = mpsc::channel();
        let (send_progress, progress_updates) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = cancelled.clone();
        thread::Builder::new()
            .name(format!("jarvis-turn-{id}"))
            .spawn(move || {
                let on_speech = |text| {
                    let _ = send_speech.send(text);
                    ctx.request_repaint();
                };
                let on_progress = |progress| {
                    let _ = send_progress.send(progress);
                    ctx.request_repaint();
                };
                let outcome = codex::answer(
                    &state_dir,
                    workspace.as_deref(),
                    &input,
                    &worker_cancelled,
                    &connection,
                    codex::Observers {
                        speech: speak_reply.then_some(&on_speech as &dyn Fn(String)),
                        progress: &on_progress,
                    },
                );
                let _ = sender.send(outcome);
                ctx.request_repaint();
            })?;
        Ok(Self {
            id,
            background,
            speak_reply,
            result,
            speech,
            cancelled,
            approvals,
            progress_updates,
            progress: codex::Progress::Connecting,
            approval: None,
        })
    }

    pub fn poll_updates(&mut self) {
        if let Some(progress) = self.progress_updates.try_iter().last() {
            self.progress = progress;
        }
        if self
            .approval
            .as_ref()
            .is_some_and(|approval| std::time::Instant::now() >= approval.expires)
        {
            self.approval = None;
        }
        if let Ok(approval) = self.approvals.try_recv()
            && !self.cancelling()
        {
            self.approval = Some(approval);
        }
    }

    pub fn progress_label(&self) -> &'static str {
        match self.progress {
            codex::Progress::Connecting => "Codex 연결 중",
            codex::Progress::Running(_) => "Codex 작업 중",
        }
    }

    pub fn plan(&self) -> Option<&codex::PlanProgress> {
        match &self.progress {
            codex::Progress::Connecting => None,
            codex::Progress::Running(plan) => plan.as_ref(),
        }
    }

    pub fn speech(&self) -> impl Iterator<Item = String> + '_ {
        self.speech.try_iter()
    }

    pub fn result(&self) -> Option<Outcome> {
        match self.result.try_recv() {
            Ok(outcome) => Some(outcome),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err("Codex 요청이 중단됐어.".into())),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn cancelling(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.cancel();
    }
}
