use eframe::egui;
use serde_json::{Value, json};
use std::{
    io,
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};
use tiny_http::{Header, Method, Request, Response, Server};

const MAX_BODY: usize = 96 * 1024;

pub struct Bridge {
    pub requests: Receiver<Call>,
    pub token: String,
    pub port: u16,
    stop: Arc<AtomicBool>,
}

pub struct Call {
    pub action: Action,
    pub reply: SyncSender<Reply>,
    pub expires: Instant,
}

pub enum Action {
    State,
    Command(Value),
    Preview { id: i64, nonce: u64 },
}

pub enum Reply {
    Json(u16, Value),
    Jpeg(Vec<u8>),
}

impl Bridge {
    pub fn start(
        ctx: egui::Context,
        port: u16,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(io::Error::other)?;
        use base64::Engine as _;
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key);
        let server = Server::http((Ipv4Addr::LOCALHOST, port))?;
        let port = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| io::Error::other("No TCP address"))?
            .port();
        let (send, requests) = mpsc::sync_channel(16);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = stop.clone();
        let secret = token.clone();
        std::thread::Builder::new()
            .name("jarvis-web".into())
            .spawn(move || {
                while !stop_worker.load(Ordering::Relaxed) {
                    match server.recv_timeout(Duration::from_millis(200)) {
                        Ok(Some(request)) => serve(request, &secret, &send, &ctx),
                        Ok(None) => (),
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            requests,
            token,
            port,
            stop,
        })
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn serve(mut request: Request, token: &str, send: &SyncSender<Call>, ctx: &egui::Context) {
    let (code, content_type, body) = route(&mut request, token, send, ctx);
    let mut response = Response::from_data(body).with_status_code(code);
    for (name, value) in [
        ("Content-Type", content_type),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' blob:; media-src 'self' blob:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
        ),
        (
            "Permissions-Policy",
            "microphone=(self), camera=(), geolocation=()",
        ),
    ] {
        response.add_header(Header::from_bytes(name, value).expect("static HTTP header"));
    }
    let _ = request.respond(response);
}

fn route(
    request: &mut Request,
    token: &str,
    send: &SyncSender<Call>,
    ctx: &egui::Context,
) -> (u16, &'static str, Vec<u8>) {
    if request.method() == &Method::Get {
        let asset: Option<(&str, &str)> = match request.url() {
            "/" => Some((
                "text/html; charset=utf-8",
                include_str!("../web/index.html"),
            )),
            "/style.css" => Some(("text/css; charset=utf-8", include_str!("../web/style.css"))),
            "/app.js" => Some((
                "text/javascript; charset=utf-8",
                include_str!("../web/app.js"),
            )),
            "/voice.js" => Some((
                "text/javascript; charset=utf-8",
                include_str!("../web/voice.js"),
            )),
            "/mic.js" => Some((
                "text/javascript; charset=utf-8",
                include_str!("../web/mic.js"),
            )),
            _ => None,
        };
        if let Some((content_type, asset)) = asset {
            return (200, content_type, asset.as_bytes().to_vec());
        }
    }
    if !request.url().starts_with("/api/") {
        return response(error(404, "페이지를 찾지 못했어."));
    }
    let authenticated = request.headers().iter().any(|header| {
        header.field.equiv("Authorization")
            && header
                .value
                .as_str()
                .strip_prefix("Bearer ")
                .is_some_and(|value| equal_secret(value.as_bytes(), token.as_bytes()))
    });
    if !authenticated {
        return response(error(401, "Mac 앱의 연결 코드를 입력해줘."));
    }
    let action = match (request.method(), request.url()) {
        (&Method::Get, "/api/state") => Action::State,
        (&Method::Post, "/api/command") => {
            if request.body_length().is_none_or(|len| len > MAX_BODY) {
                return response(error(413, "요청이 너무 길어."));
            }
            let mut bytes = Vec::new();
            if request.as_reader().read_to_end(&mut bytes).is_err() {
                return response(error(400, "요청을 읽지 못했어."));
            }
            match serde_json::from_slice(&bytes) {
                Ok(value) => Action::Command(value),
                Err(_) => return response(error(400, "요청 형식을 확인해줘.")),
            }
        }
        (&Method::Get, path) if path.starts_with("/api/preview/") => {
            let Some((id, nonce)) = path.trim_start_matches("/api/preview/").split_once('/') else {
                return response(error(404, "미리보기를 찾지 못했어."));
            };
            match (id.parse(), nonce.parse()) {
                (Ok(id), Ok(nonce)) => Action::Preview { id, nonce },
                _ => return response(error(404, "미리보기를 찾지 못했어.")),
            }
        }
        _ => return response(error(404, "요청을 찾지 못했어.")),
    };
    let (reply, result) = mpsc::sync_channel(1);
    let wait = Duration::from_secs(3);
    if send
        .try_send(Call {
            action,
            reply,
            expires: Instant::now() + wait,
        })
        .is_err()
    {
        return response(error(503, "Mac 앱이 바빠. 잠시 뒤 다시 시도해줘."));
    }
    ctx.request_repaint();
    response(
        result
            .recv_timeout(wait)
            .unwrap_or_else(|_| error(503, "Mac 앱이 응답하지 않았어.")),
    )
}

pub fn error(code: u16, message: &str) -> Reply {
    Reply::Json(code, json!({"error": message}))
}

fn response(reply: Reply) -> (u16, &'static str, Vec<u8>) {
    match reply {
        Reply::Json(code, value) => (
            code,
            "application/json; charset=utf-8",
            value.to_string().into_bytes(),
        ),
        Reply::Jpeg(bytes) => (200, "image/jpeg", bytes),
    }
}

fn equal_secret(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0, |difference, (a, b)| difference | (a ^ b))
            == 0
}
