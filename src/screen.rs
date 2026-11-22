use block2::RcBlock;
use objc2::{AnyThread, ClassType, msg_send, rc::Retained, sel};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
use objc2_core_graphics::{CGImage, CGMainDisplayID, CGPreflightScreenCaptureAccess};
use objc2_foundation::{NSArray, NSBundle, NSDictionary, NSError};
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration, SCWindow,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

pub const PERMISSION_MESSAGE: &str = "프라이데이에 화면 기록 권한이 필요해. 개인정보 보호 및 보안의 화면 및 시스템 오디오 기록에서 허용해줘.";

type Outcome<T> = Result<T, String>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Display(u32),
    Window {
        id: u32,
        pid: i32,
        bundle: String,
        title: String,
    },
}

impl Source {
    pub fn label(&self) -> String {
        match self {
            Self::Display(_) => "주 화면".into(),
            Self::Window { bundle, title, .. } if title.is_empty() => {
                format!("{bundle} · 제목 없는 창")
            }
            Self::Window { bundle, title, .. } => format!("{bundle} · {title}"),
        }
    }
}

pub fn allowed() -> bool {
    CGPreflightScreenCaptureAccess()
}

pub fn select(
    bundle: &str,
    title: &str,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Outcome<Source> {
    let own_only = !allowed();
    if own_only && !can_capture_own(bundle) {
        return Err(PERMISSION_MESSAGE.into());
    }
    if bundle.is_empty() {
        return if title.is_empty() {
            Ok(Source::Display(CGMainDisplayID()))
        } else {
            Err("창 제목을 지정할 때는 앱 번들 ID가 필요해.".into())
        };
    }
    let bundle = bundle.to_owned();
    let title = title.to_owned();
    let (sender, receiver) = mpsc::channel();
    find_content(own_only, move |content| {
        let result = content.and_then(|content| {
            let windows = unsafe { content.windows() };
            let mut matching = windows
                .iter()
                .filter_map(|window| describe(&window))
                .filter(|source| {
                    matches!(source, Source::Window { bundle: app, title: current, .. }
                    if *app == bundle && (title.is_empty() || *current == title)
                    && (!own_only || matches!(source, Source::Window { pid, .. } if *pid as u32 == std::process::id())))
                });
            let selected = matching
                .next()
                .ok_or("캡처할 창을 찾지 못했어. 앱의 창을 열고 다시 요청해줘.")?;
            if matching.next().is_some() {
                return Err(
                    "해당 앱에 일치하는 창이 여러 개 있어. 캡처할 창의 정확한 제목을 지정해줘."
                        .into(),
                );
            }
            Ok(selected)
        });
        let _ = sender.send(result);
    });
    wait_for(receiver, deadline, cancelled)
}

pub fn capture(source: Source, deadline: Instant, cancelled: &AtomicBool) -> Outcome<Vec<u8>> {
    let own_only = !allowed();
    if own_only
        && !matches!(&source, Source::Window { bundle, pid, .. } if *pid as u32 == std::process::id() && can_capture_own(bundle))
    {
        return Err(PERMISSION_MESSAGE.into());
    }
    let (sender, receiver) = mpsc::channel();
    find_content(own_only, move |content| {
        let started = content.and_then(|content| capture_content(content, &source, sender.clone()));
        if let Err(error) = started {
            let _ = sender.send(Err(error));
        }
    });
    wait_for(receiver, deadline, cancelled)
}

fn can_capture_own(bundle: &str) -> bool {
    if bundle.is_empty()
        || NSBundle::mainBundle()
            .bundleIdentifier()
            .is_none_or(|id| id.to_string() != bundle)
    {
        return false;
    }
    // This Apple API was introduced in macOS 14.4. It exposes only content that
    // the operating system already allows the current process to capture.
    let supported: bool = unsafe {
        msg_send![SCShareableContent::class(), respondsToSelector: sel!(getCurrentProcessShareableContentWithCompletionHandler:)]
    };
    supported
}

fn find_content(own_only: bool, handler: impl Fn(Outcome<&SCShareableContent>) + 'static) {
    let callback = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            // ScreenCaptureKit keeps both parameters alive for the duration of its callback.
            let result = unsafe { content.as_ref() }.ok_or_else(|| capture_error(error));
            handler(result);
        },
    );
    unsafe {
        if own_only {
            SCShareableContent::getCurrentProcessShareableContentWithCompletionHandler(&callback);
        } else {
            SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
                true, true, &callback
            );
        }
    }
}

fn describe(window: &SCWindow) -> Option<Source> {
    unsafe {
        if !window.isOnScreen() || window.windowLayer() != 0 {
            return None;
        }
        let app = window.owningApplication()?;
        Some(Source::Window {
            id: window.windowID(),
            pid: app.processID(),
            bundle: app.bundleIdentifier().to_string(),
            title: window
                .title()
                .map(|value| value.to_string())
                .unwrap_or_default(),
        })
    }
}

fn capture_content(
    content: &SCShareableContent,
    source: &Source,
    sender: mpsc::Sender<Outcome<Vec<u8>>>,
) -> Outcome<()> {
    unsafe {
        let filter = match source {
            Source::Display(id) => {
                let display = content
                    .displays()
                    .iter()
                    .find(|display| display.displayID() == *id)
                    .ok_or("승인한 주 화면이 연결 해제됐어.")?;
                SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &NSArray::new(),
                )
            }
            Source::Window { .. } => {
                let window = content
                    .windows()
                    .iter()
                    .find(|window| describe(window).as_ref() == Some(source))
                    .ok_or("승인한 창이 닫혔거나 제목이 바뀌었어. 다시 요청해줘.")?;
                SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window)
            }
        };
        let size = filter.contentRect().size;
        let scale = f64::from(filter.pointPixelScale());
        let (width, height) = output_dimensions(size.width * scale, size.height * scale)?;
        let configuration = SCStreamConfiguration::new();
        configuration.setWidth(width);
        configuration.setHeight(height);
        configuration.setShowsCursor(false);
        if matches!(source, Source::Window { .. }) {
            configuration.setScalesToFit(true);
        }
        let callback = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let result = image
                .as_ref()
                .ok_or_else(|| capture_error(error))
                .and_then(encode);
            let _ = sender.send(result);
        });
        SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
            &filter,
            &configuration,
            Some(&callback),
        );
    }
    Ok(())
}

fn output_dimensions(width: f64, height: f64) -> Outcome<(usize, usize)> {
    if !width.is_finite() || !height.is_finite() || width < 1.0 || height < 1.0 {
        return Err("캡처할 창의 크기를 확인할 수 없어.".into());
    }
    let scale = (1600.0 / width.max(height)).min(1.0);
    Ok((
        (width * scale).round().max(1.0) as usize,
        (height * scale).round().max(1.0) as usize,
    ))
}

fn encode(image: &CGImage) -> Outcome<Vec<u8>> {
    let bitmap = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), image);
    let properties = NSDictionary::new();
    let jpeg: Retained<_> = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::JPEG, &properties)
    }
    .ok_or("캡처한 이미지를 JPEG로 변환하지 못했어.")?;
    Ok(jpeg.to_vec())
}

fn capture_error(error: *mut NSError) -> String {
    let error = unsafe { error.as_ref() };
    error
        .map(|error| format!("macOS 화면 캡처 실패 · {}", error.localizedDescription()))
        .unwrap_or_else(|| "macOS에서 화면 캡처 결과를 받지 못했어.".into())
}

fn wait_for<T>(
    receiver: Receiver<Outcome<T>>,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Outcome<T> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(15));
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if cancelled.load(Ordering::Relaxed) {
            return Err("화면 작업이 중단됐어.".into());
        }
        if remaining.is_zero() {
            return Err("macOS 화면 응답을 기다리는 시간이 지났어.".into());
        }
        match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(result) if !cancelled.load(Ordering::Relaxed) => return result,
            Ok(_) => return Err("화면 작업이 중단됐어.".into()),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("macOS 화면 응답이 중단됐어.".into()),
        }
    }
}
