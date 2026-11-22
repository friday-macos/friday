mod browser;
mod codex;
mod control;
mod desktop;
mod local_stt;
mod memory;
mod neural;
mod remote;
mod screen;
mod speech_language;
mod speech_stream;
mod tasks;
mod voice;
mod web_voice;

use desktop::{Action, Desktop};
use eframe::egui;
use memory::{BackgroundKind, Decision, Exchange, Memory, PendingBackground};
use serde_json::{Value, json};
use speech_language::SpeechLanguage;
use std::{
    cmp::Reverse,
    env, fs, io,
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, TryRecvError},
};

const MAX_INPUT_CHARS: usize = 16_000;
use tasks::{Outcome, Task};
use voice::{MacVoice, Voice, VoiceUpdate};

const MAX_BACKGROUND_TASKS: usize = 3;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let explicit_dir = match arguments.next() {
        None => None,
        Some(flag) if flag == "--project-dir" => Some(PathBuf::from(
            arguments
                .next()
                .ok_or("사용법: jarvis [--project-dir 폴더]")?,
        )),
        _ => return Err("사용법: jarvis [--project-dir 폴더]".into()),
    };
    if arguments.next().is_some() {
        return Err("사용법: jarvis [--project-dir 폴더]".into());
    }
    let project = memory::project(explicit_dir.as_deref())?;
    let directory = if let Some(directory) = env::var_os("JARVIS_DATA_DIR") {
        let directory = PathBuf::from(directory);
        if directory.is_absolute() {
            directory
        } else {
            env::current_dir()?.join(directory)
        }
    } else {
        let user_home = env::var_os("HOME").ok_or("사용자 홈 폴더를 찾지 못했어.")?;
        PathBuf::from(user_home).join("Library/Application Support/JARVIS")
    };
    let mut memory = Memory::open(&directory)?;
    let directory = directory.canonicalize()?;
    let state_dir = directory.join("codex");
    let workspaces = directory.join("tasks");
    fs::create_dir_all(&workspaces)?;
    if workspaces.canonicalize()? != workspaces {
        return Err("작업 폴더가 프라이데이 데이터 경로 바깥으로 연결돼 있어.".into());
    }
    if state_dir.to_str().is_none() {
        return Err("프라이데이 데이터 경로를 UTF-8로 읽을 수 없어.".into());
    }
    let project = match project {
        Some(path) => {
            let path = path
                .into_os_string()
                .into_string()
                .map_err(|_| "프로젝트 경로를 UTF-8로 읽을 수 없어.")?;
            if explicit_dir.is_some() {
                memory.set_current_project(&path)?;
            }
            path
        }
        None => memory.current_project()?,
    };
    let pending = memory.pending_background()?;
    let history = memory.history()?;
    let decisions = memory.decisions()?;
    let playback_volume = memory.playback_volume()?;
    let speech_language = memory.speech_language()?;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1050.0, 690.0])
            .with_min_inner_size([405.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native(
        "FRIDAY",
        options,
        Box::new(move |context| {
            configure_ui(&context.egui_ctx)?;
            let desktop = Desktop::start(&context.egui_ctx)?;
            let notice = desktop.shortcut_error.clone();
            let mut app = Jarvis {
                memory,
                state_dir,
                workspaces,
                project,
                project_picker: None,
                history,
                decisions,
                draft: String::new(),
                tasks: Vec::new(),
                approvals: Vec::new(),
                desktop,
                voice: Voice::new(MacVoice::new(
                    directory.join("voice-model"),
                    playback_volume,
                    speech_language,
                )),
                playback_volume,
                speech_language,
                voice_draft: false,
                send_voice_draft: false,
                dashboard: false,
                permissions_open: false,
                remote_open: false,
                remote: None,
                web_voice: None,
                quitting: false,
                focus_composer: false,
                notice,
            };
            app.restore_background(pending, &context.egui_ctx);
            Ok(Box::new(app))
        }),
    )?;
    Ok(())
}

fn configure_ui(ctx: &egui::Context) -> io::Result<()> {
    let system_font = fs::read("/System/Library/Fonts/Supplemental/AppleGothic.ttf")
        .or_else(|_| fs::read("/System/Library/Fonts/AppleSDGothicNeo.ttc"))?;
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "korean".into(),
        egui::FontData::from_owned(system_font).into(),
    );
    fonts
        .families
        .entry(egui::FontFamily::Proportional)
        .or_default()
        .push("korean".into());
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.visuals.panel_fill = egui::Color32::from_rgb(8, 12, 20);
        style.visuals.window_fill = egui::Color32::from_rgb(16, 25, 37);
        style.visuals.faint_bg_color = egui::Color32::from_rgb(16, 25, 37);
        style.visuals.selection.bg_fill = egui::Color32::from_rgb(28, 92, 102);
        style.visuals.hyperlink_color = egui::Color32::from_rgb(100, 244, 241);
        style.visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(23, 35, 47);
        style.visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(31, 67, 77);
        style.visuals.window_corner_radius = egui::CornerRadius::same(16);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
    });
    Ok(())
}

fn voice_orb(ui: &mut egui::Ui, mode: &str, wide: bool, signal_level: Option<f32>) {
    use egui::{Color32, Shape, Stroke, pos2, vec2};
    let height = if wide { 255.0 } else { 114.0 };
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), egui::Sense::hover());
    let center = rect.center();
    let radius = height * 0.34;
    let level = (signal_level.unwrap_or(0.0) * 5.0).clamp(0.0, 1.0);
    let animated = matches!(mode, "listening" | "speaking" | "working");
    let time = if animated {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(42));
        if signal_level.is_some() {
            level
        } else {
            ui.input(|input| input.time) as f32
        }
    } else {
        0.0
    };
    let painter = ui.painter_at(rect);
    let teal = Color32::from_rgb(100, 244, 241);
    for offset in (1..=23).rev() {
        painter.circle_filled(
            center,
            radius + offset as f32 * 1.25,
            Color32::from_rgba_unmultiplied(23, 215, 229, 4 + (level * 5.0) as u8),
        );
    }
    painter.circle_filled(center, radius, Color32::from_rgb(8, 27, 41));
    for inset in (0..10).rev() {
        painter.circle_stroke(
            center,
            radius - inset as f32 * 1.2,
            Stroke::new(
                2.4_f32,
                Color32::from_rgba_unmultiplied(54, 239, 245, (22 - inset) * 4),
            ),
        );
    }
    painter.circle_stroke(center, radius, Stroke::new(1.7_f32, teal));
    for row in 0..11 {
        let points = (0..=70)
            .map(|index| {
                let x = (index as f32 / 70.0 * 2.0 - 1.0) * 0.91;
                let envelope = (1.0 - x * x).sqrt();
                let y = (x * 3.8 + time * 3.1 + row as f32 * 0.19).sin()
                    * (0.10 + 0.012 * row as f32 + 0.16 * level)
                    * envelope;
                pos2(
                    center.x + radius * x,
                    center.y + radius * (y + 0.14 * envelope),
                )
            })
            .collect();
        painter.add(Shape::line(
            points,
            Stroke::new(0.8_f32, Color32::from_rgba_unmultiplied(73, 245, 235, 76)),
        ));
    }
    if signal_level.is_some() {
        let center = center + vec2(0.0, radius + if wide { 31.0 } else { 14.0 });
        let spacing = if wide { 5.2 } else { 3.8 };
        let active = (level * 12.0).round() as i32;
        for index in -12_i32..=12 {
            let x = center.x + index as f32 * spacing;
            let highlighted = level > 0.0 && index.abs() <= active;
            let extent = if highlighted { 2.0 + level * 5.0 } else { 1.5 };
            let color = if highlighted {
                teal
            } else {
                Color32::from_rgb(33, 87, 100)
            };
            painter.line_segment(
                [pos2(x, center.y - extent), pos2(x, center.y + extent)],
                Stroke::new(1.8_f32, color),
            );
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Submission {
    Chat,
    Background,
    FileTask,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpeechDelivery {
    Silent,
    Native,
    Browser,
}

struct Approval {
    id: i64,
    input: Value,
}

struct Jarvis {
    memory: Memory,
    state_dir: PathBuf,
    workspaces: PathBuf,
    project: String,
    project_picker: Option<Receiver<Option<PathBuf>>>,
    history: Vec<Exchange>,
    decisions: Vec<Decision>,
    draft: String,
    tasks: Vec<Task>,
    approvals: Vec<Approval>,
    desktop: Desktop,
    voice: Voice,
    playback_volume: u16,
    speech_language: SpeechLanguage,
    voice_draft: bool,
    send_voice_draft: bool,
    dashboard: bool,
    permissions_open: bool,
    remote_open: bool,
    remote: Option<remote::Bridge>,
    web_voice: Option<web_voice::Session>,
    quitting: bool,
    focus_composer: bool,
    notice: Option<String>,
}

impl Jarvis {
    fn approval_count(&self) -> usize {
        self.approvals.len()
            + self
                .tasks
                .iter()
                .filter(|task| task.approval.is_some())
                .count()
    }

    fn foreground_busy(&self) -> bool {
        self.tasks.iter().any(|task| !task.background)
    }

    fn voice_mode(&self) -> &'static str {
        if self.voice.speaking() {
            "speaking"
        } else if let Some(mode) = self.voice.dictation_mode() {
            mode
        } else if let Some(session) = self
            .web_voice
            .as_ref()
            .filter(|session| session.phase() != "idle")
        {
            session.phase()
        } else if self.foreground_busy() {
            "working"
        } else if self.voice.wake_enabled() {
            "wake_wait"
        } else {
            "idle"
        }
    }

    fn background_count(&self) -> usize {
        self.tasks.iter().filter(|task| task.background).count()
    }

    fn restore_background(&mut self, pending: Vec<PendingBackground>, ctx: &egui::Context) {
        for job in pending {
            if job.awaiting_approval {
                self.approvals.push(Approval {
                    id: job.id,
                    input: job.input,
                });
                continue;
            }
            match self.memory.claim_recovery(job.id) {
                Ok(true) => {
                    self.start_task(job.id, true, SpeechDelivery::Silent, None, job.input, ctx)
                }
                Ok(false) => (),
                Err(error) => {
                    self.notice = Some(format!("백그라운드 작업을 복구하지 못했어 · {error}"))
                }
            }
        }
    }

    fn cancel_task(&mut self, id: i64) {
        if let Some(task) = self.tasks.iter().find(|task| task.id == id) {
            if task.background
                && let Err(error) = self.memory.cancel_background(id)
            {
                self.notice = Some(format!("작업 중단을 저장하지 못했어 · {error}"));
            }
            task.cancel();
        }
    }

    fn begin_shutdown(&mut self) {
        self.remote = None;
        self.web_voice = None;
        self.quitting = true;
        if let Err(error) = self.memory.cancel_unfinished_background() {
            self.notice = Some(format!("앱 종료 전 작업 중단을 저장하지 못했어 · {error}"));
        }
        for task in &self.tasks {
            task.cancel();
        }
        while let Some(approval) = self.approvals.pop() {
            self.complete(approval.id, Err("승인 전에 프라이데이를 종료했어.".into()));
        }
    }

    fn submit(&mut self, ctx: &egui::Context, submission: Submission) {
        self.send_voice_draft = false;
        let speech = if submission == Submission::Chat && self.voice_draft {
            SpeechDelivery::Native
        } else {
            SpeechDelivery::Silent
        };
        let prompt = self.draft.trim().to_owned();
        if prompt.is_empty() {
            return;
        }
        match self.submit_request(ctx, submission, prompt, speech) {
            Ok(_) => {
                self.draft.clear();
                self.voice_draft = false;
            }
            Err(error) => self.notice = Some(error),
        }
    }

    fn submit_request(
        &mut self,
        ctx: &egui::Context,
        submission: Submission,
        prompt: String,
        speech: SpeechDelivery,
    ) -> Result<i64, String> {
        let background = submission != Submission::Chat;
        if self.quitting || (!background && self.foreground_busy()) {
            return Err("현재 대화가 응답 중이야.".into());
        }
        if background && self.background_count() + self.approvals.len() >= MAX_BACKGROUND_TASKS {
            return Err("백그라운드 작업이 가득 찼어.".into());
        }
        if prompt.is_empty() || prompt.chars().count() > MAX_INPUT_CHARS {
            return Err(format!("1자부터 {MAX_INPUT_CHARS}자까지 보낼 수 있어."));
        }

        let start = (|| -> rusqlite::Result<_> {
            let projects = self.memory.projects()?;
            let matched: Vec<_> = projects
                .iter()
                .filter(|path| mentions_project(&prompt, project_name(path)))
                .collect();
            let project = match matched.as_slice() {
                [] => self.project.clone(),
                [path] => (*path).clone(),
                paths
                    if paths.iter().any(|path| path.as_str() == self.project)
                        && paths.iter().all(|path| {
                            project_name(path).eq_ignore_ascii_case(project_name(&self.project))
                        }) =>
                {
                    self.project.clone()
                }
                _ => return Ok(None),
            };
            let mut input = self.memory.context(&project, &prompt)?;
            if speech != SpeechDelivery::Silent {
                input["channel"] = Value::String("voice".into());
            }
            let pending_kind = match submission {
                Submission::Chat => None,
                Submission::Background => Some((BackgroundKind::Analysis, &input)),
                Submission::FileTask => Some((BackgroundKind::FileApproval, &input)),
            };
            let id = self.memory.begin(&project, &prompt, pending_kind)?;
            Ok(Some((id, input, project)))
        })();
        let (id, input, project) =
            match start {
                Ok(Some(start)) => start,
                Ok(None) => return Err(
                    "같은 이름이거나 함께 언급된 프로젝트가 있어. 프로젝트 폴더에서 열어 구분해줘."
                        .into(),
                ),
                Err(error) => return Err(format!("대화를 저장하지 못했어 · {error}")),
            };
        self.notice = None;
        if !background && speech != SpeechDelivery::Native {
            self.voice.end_session(ctx);
        }
        if !background && self.project != project {
            self.select_project(project.clone());
        }
        self.history.push(Exchange {
            id,
            project,
            prompt,
            background,
            reply: None,
            error: None,
        });
        if self.history.len() > 100
            && let Some(index) = self.history.iter().position(|exchange| {
                !self.tasks.iter().any(|task| task.id == exchange.id)
                    && !self
                        .approvals
                        .iter()
                        .any(|approval| approval.id == exchange.id)
            })
        {
            self.history.remove(index);
        }
        if submission == Submission::FileTask {
            self.approvals.push(Approval { id, input });
            self.dashboard = true;
        } else {
            self.start_task(id, background, speech, None, input, ctx);
        }
        Ok(id)
    }

    fn start_task(
        &mut self,
        id: i64,
        background: bool,
        speech: SpeechDelivery,
        workspace: Option<PathBuf>,
        input: Value,
        ctx: &egui::Context,
    ) {
        let project = self
            .history
            .iter()
            .find(|exchange| exchange.id == id)
            .map(|exchange| exchange.project.clone())
            .unwrap_or_default();
        match Task::start(tasks::Request {
            id,
            background,
            speak_reply: speech != SpeechDelivery::Silent,
            state_dir: self.state_dir.clone(),
            workspace,
            input,
            project,
            ctx: ctx.clone(),
        }) {
            Ok(task) => {
                if speech == SpeechDelivery::Native {
                    self.voice.begin_stream(task.id);
                }
                self.tasks.push(task);
            }
            Err(error) => self.complete(id, Err(format!("Codex 요청을 시작하지 못했어 · {error}"))),
        }
    }

    fn log_decision(&mut self, exchange_id: i64, action: String, approved: bool) -> bool {
        let decision = Decision {
            exchange_id,
            action,
            approved,
        };
        if let Err(error) = self.memory.record_decision(&decision) {
            self.notice = Some(format!("승인 결정을 기록하지 못했어 · {error}"));
            return false;
        }
        if self.decisions.len() >= 200 {
            self.decisions.remove(0);
        }
        self.decisions.push(decision);
        true
    }

    fn decide(&mut self, id: i64, approve: bool, ctx: &egui::Context) {
        let Some(index) = self.approvals.iter().position(|approval| approval.id == id) else {
            return;
        };
        if approve {
            match self.memory.approve_file_work(id) {
                Ok(true) => (),
                Ok(false) => {
                    self.notice = Some("이 파일 작업의 승인 대기 상태를 찾을 수 없어.".into());
                    return;
                }
                Err(error) => {
                    self.notice =
                        Some(format!("파일 작업의 승인 상태를 저장하지 못했어 · {error}"));
                    return;
                }
            }
        } else if let Err(error) = self.memory.cancel_background(id) {
            self.notice = Some(format!("파일 작업 거절을 저장하지 못했어 · {error}"));
            return;
        }
        let approval = self.approvals.remove(index);
        if !self.log_decision(id, "격리된 폴더에서 파일 작업 시작".into(), approve) && approve
        {
            self.complete(
                id,
                Err("승인 기록을 저장하지 못해 파일 작업을 시작하지 않았어.".into()),
            );
            return;
        }
        if approve {
            self.start_task(
                id,
                true,
                SpeechDelivery::Silent,
                Some(self.workspaces.join(id.to_string())),
                approval.input,
                ctx,
            );
        } else {
            self.complete(id, Err("파일 작업 승인을 거절했어.".into()));
        }
    }

    fn open(&mut self, ctx: &egui::Context, dashboard: bool) {
        self.dashboard = dashboard;
        self.focus_composer = !dashboard;
        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn select_project(&mut self, path: String) {
        if let Err(error) = self.memory.set_current_project(&path) {
            self.notice = Some(format!("선택한 프로젝트를 저장하지 못했어 · {error}"));
        }
        self.project = path;
    }

    fn select_document_project(&mut self, pid: Option<i32>) {
        if !self.draft.trim().is_empty() || self.foreground_busy() || self.voice.is_dictating() {
            return;
        }
        let Some(document) = pid.and_then(desktop::document_path) else {
            return;
        };
        let projects = match self.memory.projects() {
            Ok(projects) => projects,
            Err(error) => {
                self.notice = Some(format!("프로젝트 목록을 읽지 못했어 · {error}"));
                return;
            }
        };
        if let Some(root) = memory::document_project(&document, &projects)
            && let Ok(path) = root.into_os_string().into_string()
            && path != self.project
        {
            self.select_project(path);
        }
    }

    fn finish_project_picker(&mut self) {
        let Some(result) = &self.project_picker else {
            return;
        };
        let path = match result.try_recv() {
            Ok(path) => path,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        self.project_picker = None;
        let Some(path) = path else { return };
        match memory::project(Some(&path)) {
            Ok(Some(root)) => match root.into_os_string().into_string() {
                Ok(path) => self.select_project(path),
                Err(_) => self.notice = Some("프로젝트 경로를 UTF-8로 읽을 수 없어.".into()),
            },
            Ok(None) => (),
            Err(error) => self.notice = Some(format!("프로젝트 폴더를 열 수 없어 · {error}")),
        }
    }

    fn remote_panel(&mut self, ctx: &egui::Context) {
        let mut open = self.remote_open;
        let mut toggle = false;
        egui::Window::new("웹 연결")
            .open(&mut open)
            .collapsible(false)
            .show(ctx, |ui| {
                if let Some(bridge) = &self.remote {
                    ui.label(format!("이 Mac · http://127.0.0.1:{}", bridge.port));
                    ui.weak("다른 기기에서는 승인된 Tailscale HTTPS 주소를 사용해.");
                    if ui.button("연결 코드 복사").clicked() {
                        ctx.copy_text(bridge.token.clone());
                    }
                    if local_stt::bundled_model().is_none()
                        && voice::permission_status().1 != "허용됨"
                        && ui.button("웹 음성 인식 허용").clicked()
                    {
                        self.notice =
                            voice::authorize_browser_speech(ctx, self.speech_language).err();
                    }
                    toggle = ui.button("웹 연결 끄기").clicked();
                } else {
                    ui.label("이 Mac에서 웹 인터페이스를 열고 연결 코드를 발급해.");
                    toggle = ui.button("웹 연결 켜기").clicked();
                }
            });
        self.remote_open = open;
        if toggle {
            if self.remote.is_some() {
                self.remote = None;
                self.web_voice = None;
            } else {
                let port = env::var("JARVIS_WEB_PORT")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(8765);
                match remote::Bridge::start(ctx.clone(), port) {
                    Ok(bridge) => self.remote = Some(bridge),
                    Err(error) => self.notice = Some(format!("웹 연결을 열지 못했어 · {error}")),
                }
            }
        }
    }

    fn poll_remote(&mut self, ctx: &egui::Context) {
        for _ in 0..8 {
            let Some(call) = self
                .remote
                .as_ref()
                .and_then(|bridge| bridge.requests.try_recv().ok())
            else {
                return;
            };
            if std::time::Instant::now() >= call.expires {
                continue;
            }
            let reply = match call.action {
                remote::Action::State => remote::Reply::Json(200, self.remote_state()),
                remote::Action::Command(command) => self.remote_command(ctx, &command),
                remote::Action::Preview { id, nonce } => {
                    let preview = self
                        .tasks
                        .iter()
                        .find(|task| task.id == id)
                        .and_then(|task| task.approval.as_ref())
                        .filter(|approval| {
                            approval.serial == nonce && std::time::Instant::now() < approval.expires
                        })
                        .and_then(|approval| approval.preview.as_ref());
                    match preview.and_then(|path| fs::read(path).ok()) {
                        Some(bytes) => remote::Reply::Jpeg(bytes),
                        None => remote::error(404, "해당 미리보기가 없거나 승인이 만료됐어."),
                    }
                }
            };
            let _ = call.reply.try_send(reply);
        }
    }

    fn dashboard_rank(&self, exchange: &Exchange) -> Option<u8> {
        if self
            .approvals
            .iter()
            .any(|approval| approval.id == exchange.id)
        {
            return Some(0);
        }
        if let Some(task) = self.tasks.iter().find(|task| task.id == exchange.id) {
            return Some(
                if task
                    .approval
                    .as_ref()
                    .is_some_and(|approval| std::time::Instant::now() < approval.expires)
                {
                    0
                } else {
                    1
                },
            );
        }
        (exchange.background
            || self
                .decisions
                .iter()
                .any(|decision| decision.exchange_id == exchange.id))
        .then_some(2)
    }

    fn remote_state(&self) -> Value {
        let exchanges: Vec<_> = self.history.iter().map(|item| {
            let task = self.tasks.iter().find(|task| task.id == item.id);
            let file_approval = self.approvals.iter().any(|approval| approval.id == item.id);
            let pending = task.and_then(|task| task.approval.as_ref())
                .filter(|approval| std::time::Instant::now() < approval.expires);
            let approval = if let Some(pending) = pending {
                json!({"title": pending.description, "detail": pending.detail, "nonce": pending.serial, "preview": pending.preview.is_some()})
            } else if file_approval {
                json!({"title": "격리된 폴더에서 파일 작업 시작", "detail": self.workspaces.join(item.id.to_string()), "nonce": 0})
            } else {
                Value::Null
            };
            let status = if !approval.is_null() { "approval" }
                else if item.error.is_some() { "failed" }
                else if item.reply.is_some() { "done" }
                else if task.is_some_and(Task::cancelling) { "stopping" }
                else if task.is_some() { "running" }
                else { "interrupted" };
            let decisions: Vec<_> = self.decisions.iter().filter(|decision| decision.exchange_id == item.id)
                .map(|decision| json!({"action": decision.action, "approved": decision.approved})).collect();
            json!({"id": item.id, "project": project_name(&item.project), "prompt": item.prompt,
                "reply": item.reply, "error": item.error, "background": item.background, "status": status,
                "approval": approval, "decisions": decisions,
                "dashboard_rank": self.dashboard_rank(item),
                "progress_label": task.map(Task::progress_label),
                "plan": task.and_then(Task::plan)
                    .map(|plan| json!({"step": plan.step, "completed": plan.completed, "total": plan.total}))})
        }).collect();
        json!({
            "host":"MacBook Pro", "project":project_name(&self.project),
            "mode":self.voice_mode(),
            "busy":self.foreground_busy(), "running":self.background_count(), "approvals":self.approval_count(),
            "exchanges":exchanges
        })
    }

    fn remote_command(&mut self, ctx: &egui::Context, command: &Value) -> remote::Reply {
        let Some(action) = command["action"].as_str() else {
            return remote::error(400, "작업을 지정해줘.");
        };
        if action.starts_with("voice_") {
            return self.remote_voice_command(ctx, action, command);
        }
        if action == "submit" {
            let submission = match command["kind"].as_str() {
                Some("chat") => Submission::Chat,
                Some("background") => Submission::Background,
                Some("file") => Submission::FileTask,
                _ => return remote::error(400, "요청 종류를 지정해줘."),
            };
            let Some(prompt) = command["text"].as_str() else {
                return remote::error(400, "메시지를 입력해줘.");
            };
            return match self.submit_request(
                ctx,
                submission,
                prompt.trim().to_owned(),
                SpeechDelivery::Silent,
            ) {
                Ok(id) => remote::Reply::Json(200, json!({"id":id})),
                Err(error) => remote::error(409, &error),
            };
        }
        let Some(id) = command["id"].as_i64() else {
            return remote::error(400, "작업을 지정해줘.");
        };
        match action {
            "cancel"
                if self
                    .tasks
                    .iter()
                    .any(|task| task.id == id && !task.cancelling()) =>
            {
                self.cancel_task(id)
            }
            "decide" => {
                let (Some(nonce), Some(approve)) =
                    (command["nonce"].as_u64(), command["approve"].as_bool())
                else {
                    return remote::error(400, "승인 대상을 확인해줘.");
                };
                if nonce == 0 && self.approvals.iter().any(|approval| approval.id == id) {
                    self.decide(id, approve, ctx);
                } else if !self.decide_tool(id, Some(nonce), approve) {
                    return remote::error(409, "승인 요청이 이미 끝났거나 바뀌었어.");
                }
            }
            _ => return remote::error(409, "실행할 작업을 찾지 못했어."),
        }
        remote::Reply::Json(200, json!({"ok":true}))
    }

    fn remote_voice_command(
        &mut self,
        ctx: &egui::Context,
        command: &str,
        data: &Value,
    ) -> remote::Reply {
        let key = data["session"].as_str();
        if command == "voice_open" {
            if self
                .web_voice
                .as_ref()
                .is_some_and(|session| !session.expired())
            {
                return remote::error(409, "다른 브라우저에서 음성 대화를 사용 중이야.");
            }
            let transcriber = local_stt::bundled_model();
            if transcriber.is_none() && voice::permission_status().1 != "허용됨" {
                return remote::error(409, "Mac의 프라이데이에 음성 인식 권한을 허용해줘.");
            }
            let model = self.state_dir.with_file_name("voice-model");
            return match web_voice::Session::new(&model, transcriber, ctx, self.speech_language) {
                Ok(session) => {
                    let key = session.key().to_owned();
                    self.web_voice = Some(session);
                    remote::Reply::Json(200, json!({"session":key}))
                }
                Err(error) => remote::error(503, &error),
            };
        }
        let Some(session) = self
            .web_voice
            .as_mut()
            .filter(|session| session.matches(key) && !session.expired())
        else {
            return remote::error(409, "음성 연결이 끝났어. 다시 시작해줘.");
        };
        session.touch();
        if command == "voice_status" {
            return remote::Reply::Json(
                200,
                session.status(
                    data["generation"].as_u64(),
                    data["cursor"].as_u64().unwrap_or(0),
                ),
            );
        }
        let result = match command {
            "voice_listen" => session.listen(ctx).map(|id| {
                if let Some(id) = id
                    && let Some(task) = self.tasks.iter_mut().find(|task| task.id == id)
                {
                    task.approval = None;
                    task.cancel();
                }
            }),
            "voice_audio" => session.feed(data["data"].as_str().unwrap_or_default()),
            "voice_commit" => session.commit(),
            "voice_close" => {
                self.web_voice = None;
                Ok(())
            }
            _ => return remote::error(404, "음성 동작을 찾지 못했어."),
        };
        match result {
            Ok(()) => remote::Reply::Json(200, json!({"ok":true})),
            Err(error) => remote::error(409, &error),
        }
    }

    fn poll_web_voice(&mut self, ctx: &egui::Context) {
        if self
            .web_voice
            .as_ref()
            .is_some_and(web_voice::Session::expired)
        {
            self.web_voice = None;
        }
        let idle = !self.foreground_busy() && !self.quitting;
        let Some(session) = self.web_voice.as_mut() else {
            return;
        };
        session.poll();
        let Some(text) = session.transcript(idle) else {
            return;
        };
        let result = self.submit_request(ctx, Submission::Chat, text, SpeechDelivery::Browser);
        if let Some(session) = &mut self.web_voice {
            match result {
                Ok(id) => {
                    session.turn = Some(id);
                    if let Some(item) = self.history.iter().find(|item| item.id == id)
                        && let Some(error) = &item.error
                    {
                        session.finish(id, Err(error));
                    }
                }
                Err(error) => session.set_notice(error),
            }
        }
    }

    fn decide_tool(&mut self, id: i64, nonce: Option<u64>, approve: bool) -> bool {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else {
            return false;
        };
        let matches = task.approval.as_ref().is_some_and(|approval| {
            nonce.is_none_or(|nonce| nonce == approval.serial)
                && std::time::Instant::now() < approval.expires
        });
        if !matches {
            return false;
        }
        let approval = task.approval.take().expect("matched pending approval");
        let persisted = self.log_decision(id, approval.description.clone(), approve);
        approval.decide(approve && persisted);
        persisted
    }

    fn permissions(&mut self, ctx: &egui::Context) {
        if !self.permissions_open {
            return;
        }
        let (microphone, speech) = voice::permission_status();
        let accessibility = if control::accessibility_allowed() {
            "허용됨"
        } else {
            "허용 필요"
        };
        let capture = if screen::allowed() {
            "허용됨"
        } else {
            "허용 필요"
        };
        let mut open_settings = false;
        egui::Window::new("macOS 권한")
            .open(&mut self.permissions_open)
            .collapsible(false)
            .show(ctx, |ui| {
                egui::Grid::new("permission-state")
                    .num_columns(2)
                    .spacing([20.0, 8.0])
                    .show(ui, |ui| {
                        for (label, status) in [
                            ("마이크", microphone),
                            ("음성 인식", speech),
                            ("손쉬운 사용", accessibility),
                            ("화면 및 시스템 오디오 기록", capture),
                        ] {
                            ui.label(label);
                            ui.label(status);
                            ui.end_row();
                        }
                    });
                ui.weak("시스템 설정 → 개인정보 보호 및 보안");
                open_settings = ui.button("시스템 설정 열기").clicked();
            });
        if open_settings
            && let Err(error) = std::process::Command::new("/usr/bin/open")
                .args(["-b", "com.apple.systempreferences"])
                .spawn()
        {
            self.notice = Some(format!("시스템 설정을 열지 못했어 · {error}"));
        }
    }

    fn dictate(&mut self, ctx: &egui::Context) {
        self.notice = self.voice.dictate(ctx).err();
    }

    fn complete(&mut self, id: i64, outcome: Outcome) {
        if let Err(error) = self.memory.finish(id, &outcome) {
            self.notice = Some(format!("결과를 SQLite에 저장하지 못했어 · {error}"));
        }
        if let Some(exchange) = self.history.iter_mut().find(|exchange| exchange.id == id) {
            match outcome {
                Ok(reply) => exchange.reply = Some(reply),
                Err(error) => exchange.error = Some(error),
            }
        }
    }

    fn composer(&mut self, ui: &mut egui::Ui) {
        let idle = !self.foreground_busy();
        let shortcut = idle
            && ui.input_mut(|input| input.consume_key(egui::Modifiers::MAC_CMD, egui::Key::Enter));
        ui.horizontal(|ui| {
            let available = ui.available_width();
            let input = ui.add(
                egui::TextEdit::multiline(&mut self.draft)
                    .hint_text("프라이데이에게 메시지 보내기")
                    .desired_width(available - 90.0)
                    .desired_rows(3),
            );
            if std::mem::take(&mut self.focus_composer) {
                input.request_focus();
            }
            if input.changed() {
                self.send_voice_draft = false;
                if self.draft.trim().is_empty() {
                    self.voice_draft = false;
                }
            }
            let send = ui
                .add_enabled(
                    idle && !self.draft.trim().is_empty(),
                    egui::Button::new("보내기").min_size(egui::vec2(78.0, 34.0)),
                )
                .on_hover_text("⌘↵");
            if send.clicked() || shortcut {
                self.submit(ui.ctx(), Submission::Chat);
            }
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .button(if self.voice.is_dictating() {
                    "말 마침"
                } else {
                    "말하기"
                })
                .clicked()
            {
                self.dictate(ui.ctx());
            }
            let mut wake = self.voice.wake_enabled();
            if ui.checkbox(&mut wake, "‘프라이데이’ 호출").clicked() {
                self.notice = self.voice.toggle_wake(ui.ctx()).err();
            }
            if self.voice.speaking() && ui.button("낭독 중단").clicked() {
                self.voice.stop_speaking();
            }
            if self.voice.following_up() && ui.button("음성 대화 끝내기").clicked() {
                self.voice.end_session(ui.ctx());
            }
            if ui
                .add_enabled(
                    !self.draft.trim().is_empty()
                        && self.background_count() + self.approvals.len() < MAX_BACKGROUND_TASKS,
                    egui::Button::new("백그라운드로 보내기"),
                )
                .clicked()
            {
                self.submit(ui.ctx(), Submission::Background);
            }
            if ui
                .add_enabled(
                    !self.draft.trim().is_empty()
                        && self.background_count() + self.approvals.len() < MAX_BACKGROUND_TASKS,
                    egui::Button::new("파일 작업 요청"),
                )
                .on_hover_text("승인 후 프라이데이 전용 폴더에서 Codex로 파일을 만들어")
                .clicked()
            {
                self.submit(ui.ctx(), Submission::FileTask);
            }
        });
        ui.horizontal_wrapped(|ui| {
            let mut language = self.speech_language;
            ui.label("음성 언어 / Voice language");
            let can_change_language = !self.foreground_busy()
                && !self.voice.is_dictating()
                && !self.voice.speaking()
                && self.web_voice.is_none();
            ui.add_enabled_ui(can_change_language, |ui| {
                egui::ComboBox::from_id_salt("speech-language")
                    .selected_text(language.name)
                    .show_ui(ui, |ui| {
                        for option in SpeechLanguage::ALL {
                            ui.selectable_value(&mut language, option, option.name);
                        }
                    })
                    .response
                    .on_hover_text("Voice language · Mac and web");
            });
            if language != self.speech_language {
                match self.memory.set_speech_language(language) {
                    Ok(()) => {
                        self.speech_language = language;
                        self.voice.set_language(language, ui.ctx());
                    }
                    Err(error) => self.notice = Some(error.to_string()),
                }
            }
            ui.label("낭독 음량");
            let slider = ui.add(
                egui::Slider::new(&mut self.playback_volume, 0..=200)
                    .step_by(5.0)
                    .suffix("%"),
            );
            if slider.changed() {
                self.voice.set_playback_volume(self.playback_volume);
                if let Err(error) = self.memory.set_playback_volume(self.playback_volume) {
                    self.notice = Some(format!("낭독 음량을 저장하지 못했어 · {error}"));
                }
            }
        });
        if !self.voice.status().is_empty() {
            ui.label(self.voice.status());
        }
        if let Some(notice) = &self.notice {
            ui.colored_label(egui::Color32::LIGHT_YELLOW, notice);
        }
    }
}

impl eframe::App for Jarvis {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(action) = self.desktop.actions.try_recv() {
            match action {
                Action::Open(pid) => {
                    self.open(ctx, false);
                    self.select_document_project(pid);
                }
                Action::Speak(pid) => {
                    self.open(ctx, false);
                    self.select_document_project(pid);
                    self.dictate(ctx);
                }
                Action::Tasks => self.open(ctx, true),
                Action::Hide => ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false)),
                Action::Quit => self.begin_shutdown(),
            }
        }
        self.finish_project_picker();
        let hide = ctx.input_mut(|input| input.consume_key(egui::Modifiers::MAC_CMD, egui::Key::W));
        if !self.quitting && (hide || ctx.input(|input| input.viewport().close_requested())) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if let Some(update) = self.voice.tick(ctx) {
            match update {
                VoiceUpdate::Woke => self.open(ctx, false),
                VoiceUpdate::Notice(notice) => self.notice = Some(notice),
                VoiceUpdate::Transcript {
                    text,
                    interrupted_reply,
                } => {
                    self.open(ctx, false);
                    self.notice = None;
                    let empty = self.draft.trim().is_empty();
                    if !empty {
                        self.draft.push('\n');
                    }
                    self.draft.push_str(&text);
                    self.voice_draft = true;
                    self.send_voice_draft = empty || self.send_voice_draft;
                    if empty
                        && interrupted_reply
                        && let Some(task) = self.tasks.iter_mut().find(|task| {
                            !task.background
                                && task.speak_reply
                                && self
                                    .web_voice
                                    .as_ref()
                                    .is_none_or(|session| session.turn != Some(task.id))
                        })
                    {
                        task.approval = None;
                        task.cancel();
                    }
                }
            }
        }
        for task in &mut self.tasks {
            task.poll_updates();
        }
        let mut index = 0;
        while index < self.tasks.len() {
            let task = &self.tasks[index];
            for text in task.speech() {
                if let Some(session) = self
                    .web_voice
                    .as_mut()
                    .filter(|session| session.turn == Some(task.id))
                {
                    session.queue(task.id, &text);
                } else if let Err(error) = self.voice.queue_stream(task.id, text, ctx) {
                    self.notice = Some(error);
                }
            }
            let Some(outcome) = task.result() else {
                index += 1;
                continue;
            };
            let task = self.tasks.remove(index);
            // Closing the result channel follows the last speech frame, but it can arrive
            // between the first drain and the result read.
            for text in task.speech() {
                if let Some(session) = self
                    .web_voice
                    .as_mut()
                    .filter(|session| session.turn == Some(task.id))
                {
                    session.queue(task.id, &text);
                } else if let Err(error) = self.voice.queue_stream(task.id, text, ctx) {
                    self.notice = Some(error);
                }
            }
            if let Some(session) = self
                .web_voice
                .as_mut()
                .filter(|session| session.turn == Some(task.id))
            {
                session.finish(task.id, outcome.as_deref().map_err(String::as_str));
            } else if task.speak_reply
                && let Err(error) = self
                    .voice
                    .finish_stream(task.id, outcome.as_deref().ok(), ctx)
            {
                self.notice = Some(error);
            }
            self.complete(task.id, outcome);
        }
        if self.send_voice_draft && !self.foreground_busy() && !self.quitting {
            self.submit(ctx, Submission::Chat);
        }
        self.poll_web_voice(ctx);
        self.poll_remote(ctx);
        self.desktop.status(
            self.voice.listening(),
            self.background_count(),
            self.approval_count(),
        );
        if self.quitting && self.tasks.is_empty() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if !self.quitting {
            self.begin_shutdown();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let bar = egui::Frame::new()
            .fill(egui::Color32::from_rgb(14, 23, 34))
            .inner_margin(egui::Margin::symmetric(22, 14));
        egui::Panel::top("status")
            .resizable(false)
            .frame(bar)
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("◉")
                            .size(23.0)
                            .color(egui::Color32::from_rgb(100, 244, 241)),
                    );
                    ui.label(egui::RichText::new("F R I D A Y").size(22.0).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(if self.dashboard { "대화" } else { "작업" })
                            .clicked()
                        {
                            self.dashboard = !self.dashboard;
                        }
                        if ui.button("연결").clicked() {
                            self.remote_open = true;
                        }
                        if ui.button("권한").clicked() {
                            self.permissions_open = true;
                        }
                        if self.foreground_busy() {
                            ui.label("Codex 응답 대기 중");
                            ui.spinner();
                        } else {
                            ui.weak("대기 중");
                        }
                    });
                });
                let approvals = self.approval_count();
                if approvals > 0 {
                    ui.label(format!("승인 대기 {approvals}개"));
                }
                let background = self.background_count();
                if background > 0 {
                    ui.label(format!("백그라운드 작업 {background}개 실행 중"));
                }
                ui.horizontal(|ui| {
                    ui.weak(format!("프로젝트 · {}", project_name(&self.project)))
                        .on_hover_text(&self.project);
                    if ui
                        .add_enabled(
                            self.project_picker.is_none(),
                            egui::Button::new("폴더 선택"),
                        )
                        .clicked()
                    {
                        self.project_picker = desktop::choose_project(ui.ctx());
                    }
                    if !self.project.is_empty() && ui.button("일반 대화").clicked() {
                        self.select_project(String::new());
                    }
                });
            });
        egui::Panel::bottom("composer")
            .resizable(false)
            .frame(bar)
            .show_inside(ui, |ui| {
                self.composer(ui);
            });
        self.permissions(ui.ctx());
        self.remote_panel(ui.ctx());
        if !self.dashboard {
            let mode = self.voice_mode();
            let signal_level = match mode {
                "listening" if self.voice.listening() => Some(self.voice.microphone_level()),
                "speaking" => self.voice.playback_level(),
                _ => None,
            };
            let status = match mode {
                "speaking" => "읽어주는 중",
                "listening" => "듣는 중",
                "recognizing" => "음성 인식 중",
                "wake_wait" => "호출어 대기 중",
                "working" => "답변 준비 중",
                _ => "대기 중",
            };
            if ui.available_width() >= 740.0 {
                egui::Panel::left("voice-orb")
                    .resizable(false)
                    .default_size(330.0)
                    .show_inside(ui, |ui| {
                        ui.add_space(25.0);
                        ui.vertical_centered(|ui| {
                            ui.weak("MacBook Pro");
                            voice_orb(ui, mode, true, signal_level);
                            ui.heading(status);
                            ui.add_space(12.0);
                            ui.weak(format!(
                                "백그라운드 {}개   ·   승인 대기 {}개",
                                self.background_count(),
                                self.approval_count()
                            ));
                        });
                    });
            } else if ui.available_height() >= 240.0 {
                egui::Panel::top("compact-orb")
                    .resizable(false)
                    .show_inside(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            voice_orb(ui, mode, false, signal_level);
                            ui.weak(status);
                        });
                    });
            }
        }
        let mut decision = None;
        let mut tool_decision = None;
        let mut cancelled_task = None;
        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.heading(if self.dashboard {
                "작업 관제"
            } else {
                "대화"
            });
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .stick_to_bottom(!self.dashboard)
                .show(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(15, 12))
                        .show(ui, |ui| {
                            let mut entries = Vec::new();
                            for exchange in &self.history {
                                if self.dashboard {
                                    if let Some(rank) = self.dashboard_rank(exchange) {
                                        entries.push((rank, exchange));
                                    }
                                } else {
                                    entries.push((0, exchange));
                                }
                            }
                            if self.dashboard {
                                entries.sort_unstable_by_key(|(rank, exchange)| {
                                    (*rank, Reverse(exchange.id))
                                });
                                if entries.is_empty() {
                                    ui.weak("아직 실행된 작업이 없어.");
                                }
                            }
                            for (_, exchange) in entries {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · {}",
                                        if exchange.background {
                                            "백그라운드"
                                        } else {
                                            "나"
                                        },
                                        project_name(&exchange.project)
                                    ))
                                    .strong(),
                                );
                                ui.label(&exchange.prompt);
                                for decision in self
                                    .decisions
                                    .iter()
                                    .filter(|decision| decision.exchange_id == exchange.id)
                                {
                                    let status = if decision.approved {
                                        "승인"
                                    } else {
                                        "거절"
                                    };
                                    ui.weak(format!("{status} · {}", decision.action));
                                }
                                if let Some(reply) = &exchange.reply {
                                    if self.dashboard {
                                        ui.weak("완료");
                                    }
                                    ui.label(
                                        egui::RichText::new("FRIDAY")
                                            .color(egui::Color32::from_rgb(139, 199, 242))
                                            .strong(),
                                    );
                                    ui.label(reply);
                                    if ui.button("읽어주기").clicked() {
                                        self.notice = self.voice.speak(reply, ui.ctx()).err();
                                    }
                                } else if self
                                    .approvals
                                    .iter()
                                    .any(|approval| approval.id == exchange.id)
                                {
                                    ui.strong("승인 대기");
                                    let folder = self.workspaces.join(exchange.id.to_string());
                                    ui.label("새 작업 폴더에서 파일을 만들도록 승인할 수 있어.");
                                    ui.label(folder.display().to_string());
                                    ui.horizontal(|ui| {
                                        if ui.button("이번 작업 승인").clicked() {
                                            decision = Some((exchange.id, true));
                                        }
                                        if ui.button("거절").clicked() {
                                            decision = Some((exchange.id, false));
                                        }
                                    });
                                } else if let Some(error) = &exchange.error {
                                    if self.dashboard {
                                        ui.weak("실패");
                                    }
                                    ui.colored_label(egui::Color32::LIGHT_YELLOW, error);
                                } else if let Some(task) =
                                    self.tasks.iter().find(|task| task.id == exchange.id)
                                {
                                    if !task.cancelling() && task.approval.is_none() {
                                        ui.weak(task.progress_label());
                                    }
                                    if let Some(plan) = task.plan() {
                                        if let Some(step) = &plan.step {
                                            ui.label(step);
                                        }
                                        let count =
                                            format!("단계 {} / {}", plan.completed, plan.total);
                                        ui.add(
                                            egui::ProgressBar::new(
                                                plan.completed as f32 / plan.total as f32,
                                            )
                                            .desired_width(ui.available_width().min(360.0))
                                            .fill(egui::Color32::from_rgb(35, 139, 154))
                                            .text(count),
                                        );
                                    }
                                    if let Some(approval) = &task.approval {
                                        ui.strong("승인 대기");
                                        ui.strong(&approval.description);
                                        ui.label(&approval.detail);
                                        if let Some(preview) = &approval.preview
                                            && ui.button("전송할 화면 열어 보기").clicked()
                                        {
                                            let _ = std::process::Command::new("/usr/bin/open")
                                                .arg(preview)
                                                .spawn();
                                        }
                                        ui.horizontal(|ui| {
                                            if ui.button("이번 동작 승인").clicked() {
                                                tool_decision = Some((task.id, true));
                                            }
                                            if ui.button("거절").clicked() {
                                                tool_decision = Some((task.id, false));
                                            }
                                        });
                                    }
                                    ui.horizontal(|ui| {
                                        ui.weak(if task.cancelling() {
                                            "중단 중"
                                        } else {
                                            "실행 중"
                                        });
                                        if ui
                                            .add_enabled(
                                                !task.cancelling(),
                                                egui::Button::new("중단"),
                                            )
                                            .clicked()
                                        {
                                            cancelled_task = Some(task.id);
                                        }
                                    });
                                } else {
                                    ui.weak("응답이 저장되기 전에 종료됐어.");
                                }
                                let folder = self.workspaces.join(exchange.id.to_string());
                                if exchange.background && folder.is_dir() {
                                    ui.weak(folder.display().to_string());
                                }
                                let artifacts = self
                                    .state_dir
                                    .with_file_name("control")
                                    .join(exchange.id.to_string());
                                if artifacts.is_dir()
                                    && ui.button("Mac 작업 결과 폴더 열기").clicked()
                                {
                                    let _ = std::process::Command::new("/usr/bin/open")
                                        .arg(&artifacts)
                                        .spawn();
                                }
                                ui.add_space(7.0);
                                ui.separator();
                            }
                        });
                });
        });
        if let Some((id, approve)) = decision {
            self.decide(id, approve, ui.ctx());
        }
        if let Some(id) = cancelled_task {
            self.cancel_task(id);
        }
        if let Some((id, approve)) = tool_decision {
            self.decide_tool(id, None, approve);
        }
    }
}

fn project_name(path: &str) -> &str {
    if path.is_empty() {
        return "일반";
    }
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

fn mentions_project(prompt: &str, name: &str) -> bool {
    let prompt = prompt.to_lowercase();
    let name = name.to_lowercase();
    [&format!("{name} 프로젝트"), &format!("프로젝트 {name}")]
        .iter()
        .any(|pattern| {
            prompt.match_indices(pattern.as_str()).any(|(start, _)| {
                let left_ok = prompt[..start]
                    .chars()
                    .next_back()
                    .is_none_or(|ch| !ch.is_alphanumeric());
                let after = &prompt[start + pattern.len()..];
                let right_ok = after.chars().next().is_none_or(|ch| !ch.is_alphanumeric())
                    || [
                        "에서", "으로", "에게", "은", "는", "을", "를", "이", "가", "의",
                    ]
                    .iter()
                    .any(|particle| {
                        after.strip_prefix(particle).is_some_and(|rest| {
                            rest.chars().next().is_none_or(|ch| !ch.is_alphanumeric())
                        })
                    });
                left_ok && right_ok
            })
        })
}
