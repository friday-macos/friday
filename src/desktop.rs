use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
use block2::RcBlock;
use core_foundation::{base::CFType, string::CFString};
use eframe::egui;
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSModalResponse, NSModalResponseOK, NSOpenPanel, NSWorkspace};
use objc2_foundation::NSString;
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver},
};
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};
use url::Url;

#[derive(Clone, Copy)]
pub enum Action {
    Open(Option<i32>),
    Speak(Option<i32>),
    Hide,
    Tasks,
    Quit,
}

pub struct Desktop {
    tray: TrayIcon,
    tasks_item: MenuItem,
    status: (bool, usize, usize),
    _hotkey: Option<GlobalHotKeyManager>,
    pub actions: Receiver<Action>,
    pub shortcut_error: Option<String>,
}

impl Desktop {
    pub fn start(ctx: &egui::Context) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let menu = Menu::new();
        let open = MenuItem::new("대화 열기", true, None);
        let speak = MenuItem::new("말하기", true, None);
        let hide = MenuItem::new("창 숨기기", true, None);
        let tasks = MenuItem::new("작업 보기", true, None);
        let quit = MenuItem::new("프라이데이 종료", true, None);
        menu.append_items(&[&open, &speak, &tasks, &hide, &quit])?;
        let actions_by_id = [
            (open.id().clone(), Action::Open(None)),
            (speak.id().clone(), Action::Speak(None)),
            (tasks.id().clone(), Action::Tasks),
            (hide.id().clone(), Action::Hide),
            (quit.id().clone(), Action::Quit),
        ];
        let tray = TrayIconBuilder::new()
            .with_title("FRIDAY")
            .with_menu(Box::new(menu))
            .build()?;
        let (sender, actions) = mpsc::channel();
        let menu_sender = sender.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, action)) = actions_by_id.iter().find(|(id, _)| id == &event.id) {
                let action = match action {
                    Action::Open(_) => Action::Open(frontmost_pid()),
                    Action::Speak(_) => Action::Speak(frontmost_pid()),
                    other => *other,
                };
                let _ = menu_sender.send(action);
                if !matches!(action, Action::Hide) {
                    bring_forward();
                }
                menu_ctx.request_repaint();
            }
        }));
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyJ);
        let registered = GlobalHotKeyManager::new().and_then(|manager| {
            manager.register(hotkey)?;
            Ok(manager)
        });
        let (manager, shortcut_error) = match registered {
            Ok(manager) => {
                let hotkey_ctx = ctx.clone();
                GlobalHotKeyEvent::set_event_handler(Some(move |event: GlobalHotKeyEvent| {
                    if event.id == hotkey.id() && event.state == HotKeyState::Pressed {
                        let _ = sender.send(Action::Open(frontmost_pid()));
                        bring_forward();
                        hotkey_ctx.request_repaint();
                    }
                }));
                (Some(manager), None)
            }
            Err(error) => (
                None,
                Some(format!("⌃⇧J 단축키를 등록하지 못했어 · {error}")),
            ),
        };
        Ok(Self {
            tray,
            tasks_item: tasks,
            status: (false, 0, 0),
            _hotkey: manager,
            actions,
            shortcut_error,
        })
    }

    pub fn status(&mut self, listening: bool, running: usize, approvals: usize) {
        let next = (listening, running, approvals);
        if self.status == next {
            return;
        }
        if self.status.0 != listening {
            self.tray.set_title(Some(if listening {
                "FRIDAY · 마이크"
            } else {
                "FRIDAY"
            }));
        }
        match (running, approvals) {
            (0, 0) => self.tasks_item.set_text("작업 보기"),
            _ => self.tasks_item.set_text(format!(
                "작업 보기 · 실행 {running} · 승인 대기 {approvals}"
            )),
        }
        self.status = next;
    }
}

fn frontmost_pid() -> Option<i32> {
    let pid = NSWorkspace::sharedWorkspace()
        .frontmostApplication()?
        .processIdentifier();
    (pid > 0 && pid as u32 != std::process::id()).then_some(pid)
}

pub fn document_path(pid: i32) -> Option<PathBuf> {
    if !crate::control::accessibility_allowed() {
        return None;
    }
    let app = AXUIElement::application(pid);
    app.set_messaging_timeout(0.15).ok()?;
    let window = app.focused_window().or_else(|_| app.main_window()).ok()?;
    let document: AXAttribute<CFType> = AXAttribute::new(&CFString::new("AXDocument"));
    let locator = window
        .attribute(&document)
        .ok()?
        .downcast::<CFString>()?
        .to_string();
    if let Ok(url) = Url::parse(&locator) {
        url.to_file_path().ok()
    } else {
        let path = PathBuf::from(locator);
        path.is_absolute().then_some(path)
    }
}

// The Carbon hotkey and AppKit menu callbacks run on the macOS app thread.
// Restore a hidden native window before asking egui to focus its composer.
fn bring_forward() {
    let Some(main) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(main);
    let title = NSString::from_str("FRIDAY");
    if let Some(window) = app.windows().iter().find(|window| window.title() == title) {
        if window.isMiniaturized() {
            window.deminiaturize(None);
        }
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        window.makeKeyAndOrderFront(None);
    }
}

pub fn choose_project(ctx: &egui::Context) -> Option<Receiver<Option<PathBuf>>> {
    let main = MainThreadMarker::new()?;
    let panel = NSOpenPanel::openPanel(main);
    panel.setCanChooseDirectories(true);
    panel.setCanChooseFiles(false);
    panel.setAllowsMultipleSelection(false);
    panel.setTitle(Some(&NSString::from_str("프로젝트 폴더 선택")));
    let (send, result) = mpsc::channel();
    let repaint = ctx.clone();
    let retained = panel.clone();
    let callback = RcBlock::new(move |response: NSModalResponse| {
        let path = if response == NSModalResponseOK {
            retained
                .URLs()
                .firstObject()
                .and_then(|url| url.path())
                .map(|name| PathBuf::from(name.to_string()))
        } else {
            None
        };
        let _ = send.send(path);
        repaint.request_repaint();
    });
    panel.beginWithCompletionHandler(&callback);
    Some(result)
}
