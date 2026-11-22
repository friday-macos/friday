use serde_json::{Value, json};
use std::{
    env,
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use url::Url;

type Outcome<T> = Result<T, String>;

const RESPONSE: &str = "JARVIS_BROWSER ";
const DRIVER: &str = r#"
const created = input.id === null;
const task = await taskSpace(created ? input.name : input.id);
try {
    if (input.action === "close") {
        await task.finish({keep: []});
        console.log("JARVIS_BROWSER {}");
    } else {
        const page = task.page("p1");
        if (input.action === "open") {
            await page.goto(input.target, {timeout: 10000});
        }
        if (input.expected && await page.url() !== input.expected) {
            throw new Error("page changed");
        }
        let snapshot;
        if (input.action === "read" || input.action === "click") {
            const current = (await page.snapshot({scope:"full_page"})).slice(0, 32000);
            if (await page.url() !== input.expected) throw new Error("page changed");
            if (input.action === "read") snapshot = current;
            if (input.action === "click") {
                const rows = current.split("\n");
                const i = rows.findIndex(row => row.includes("[ref=" + input.target + ",") ||
                    row.includes("[ref=" + input.target + "]"));
                const target = i < 0 ? "" : rows.slice(i, i + input.rows).join("\n");
                if (target !== input.control) throw new Error("element changed");
                await page.click("ref=" + input.target, {timeout: 6500});
            }
        }
        console.log("JARVIS_BROWSER " + JSON.stringify({
            id: task.spaceId, url: await page.url(), title: await page.title(), snapshot
        }));
    }
} catch (error) {
    if (created) await task.finish({keep: []}).catch(() => {});
    console.log("JARVIS_BROWSER " + JSON.stringify({
        error: ["page changed", "element changed"].includes(error.message) ? "changed" : "failed"
    }));
}
"#;

pub struct Browser<'a> {
    cancelled: &'a AtomicBool,
    id: Option<u64>,
    name: String,
    page: Option<Page>,
    snapshot: Option<String>,
}

struct Page {
    url: Url,
    title: String,
}

impl<'a> Browser<'a> {
    pub fn new(cancelled: &'a AtomicBool) -> Self {
        Self {
            cancelled,
            id: None,
            name: format!(
                "JARVIS browser {:016x}",
                getrandom::u64().unwrap_or_else(|_| std::process::id().into())
            ),
            page: None,
            snapshot: None,
        }
    }

    pub fn current_url(&self) -> Outcome<&Url> {
        self.page
            .as_ref()
            .map(|page| &page.url)
            .ok_or("먼저 웹페이지를 열어줘.".into())
    }

    pub fn page_label(&self) -> Outcome<String> {
        let page = self.page.as_ref().ok_or("먼저 웹페이지를 열어줘.")?;
        Ok(format!("{} · {}", page.title, page.url))
    }

    pub fn open(&mut self, url: &Url, deadline: Instant) -> Outcome<String> {
        self.snapshot = None;
        self.page = None;
        self.update(json!({"action":"open","target":url.as_str()}), deadline)?;
        Ok(format!(
            "웹페이지를 열었어. 현재 주소: {}",
            self.current_url()?
        ))
    }

    pub fn read(&mut self, deadline: Instant) -> Outcome<String> {
        self.snapshot = None;
        let expected = self
            .page
            .as_ref()
            .ok_or("먼저 웹페이지를 열어줘.")?
            .url
            .as_str();
        let response = self.run(json!({"action":"read","expected":expected}), deadline)?;
        let text = response["snapshot"]
            .as_str()
            .ok_or("웹페이지 내용을 읽지 못했어.")?;
        self.snapshot = Some(text.to_owned());
        Ok(text.to_owned())
    }

    pub fn control(&self, reference: &str) -> Outcome<String> {
        let id = reference
            .parse::<u32>()
            .map_err(|_| "웹 요소 번호를 확인해줘.")?;
        let snapshot = self.snapshot.as_deref().ok_or("웹페이지를 먼저 읽어줘.")?;
        let start = format!("[ref={id},");
        let end = format!("[ref={id}]");
        let mut lines = snapshot
            .lines()
            .skip_while(|line| !line.contains(&start) && !line.contains(&end));
        let first = lines
            .next()
            .ok_or("최근에 읽은 페이지에서 웹 요소를 찾을 수 없어.")?;
        let indentation = first.len() - first.trim_start().len();
        let mut label = first.to_owned();
        for line in lines.take(3) {
            if line.len() - line.trim_start().len() <= indentation {
                break;
            }
            label.push('\n');
            label.push_str(line);
        }
        if label.len() > 1800 {
            return Err("승인 화면에 표시하기에는 웹 요소 설명이 너무 길어.".into());
        }
        Ok(label)
    }

    pub fn click(&mut self, reference: &str, control: &str, deadline: Instant) -> Outcome<String> {
        let expected = self
            .page
            .as_ref()
            .ok_or("먼저 웹페이지를 열어줘.")?
            .url
            .as_str();
        let input = json!({
            "action":"click", "expected":expected, "target":reference,
            "control":control, "rows":control.lines().count()
        });
        self.snapshot = None;
        self.update(input, deadline)?;
        Ok(format!(
            "웹 요소를 눌렀어. 현재 페이지: {}",
            self.current_url()?
        ))
    }

    fn update(&mut self, input: Value, deadline: Instant) -> Outcome<()> {
        let response = self.run(input, deadline)?;
        self.id = response["id"].as_u64().or(self.id);
        let url = web_url(response["url"].as_str().unwrap_or_default())?;
        let title = response["title"]
            .as_str()
            .unwrap_or_default()
            .chars()
            .take(250)
            .collect();
        self.page = Some(Page { url, title });
        Ok(())
    }

    fn run(&self, mut input: Value, deadline: Instant) -> Outcome<Value> {
        input["id"] = json!(self.id);
        input["name"] = json!(self.name);
        run_ego(&input, self.cancelled, deadline)
    }
}

impl Drop for Browser<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            let _ = run_ego(
                &json!({"action":"close","id":id}),
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(3),
            );
        }
    }
}

pub fn web_url(value: &str) -> Outcome<Url> {
    let url = Url::parse(value).map_err(|_| "올바른 HTTPS 주소를 입력해줘.")?;
    if url.scheme() != "https"
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("사용자 정보가 없는 HTTPS 주소만 열거나 읽을 수 있어.".into());
    }
    Ok(url)
}

fn run_ego(input: &Value, cancelled: &AtomicBool, deadline: Instant) -> Outcome<Value> {
    let paths = env::home_dir().map(|home| home.join(".local/bin/ego-browser"));
    let program = paths
        .into_iter()
        .chain([
            PathBuf::from("/opt/homebrew/bin/ego-browser"),
            PathBuf::from("/usr/local/bin/ego-browser"),
        ])
        .find(|path| path.is_file())
        .ok_or("Mac에 Ego Browser가 설치되어 있어야 해.")?;
    if cancelled.load(Ordering::Relaxed) || Instant::now() >= deadline {
        return Err("웹 작업이 중단됐어.".into());
    }
    let deadline = deadline.min(Instant::now() + Duration::from_secs(21));
    let mut child = Command::new(program)
        .arg("nodejs")
        .arg("-e")
        .arg(format!("const input = {input};\n{DRIVER}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "Ego Browser를 시작하지 못했어.")?;
    let mut stderr = child.stderr.take().expect("piped stderr");
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .by_ref()
            .take(160_000)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if !cancelled.load(Ordering::Relaxed) && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(45));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("Ego Browser가 응답하지 않았거나 웹 작업이 중단됐어.");
            }
        }
    };
    let status = status?;
    let output = reader
        .join()
        .ok()
        .and_then(Result::ok)
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or("Ego Browser 출력을 읽지 못했어.")?;
    let response: Value = output
        .lines()
        .find_map(|line| line.strip_prefix(RESPONSE))
        .and_then(|line| serde_json::from_str(line).ok())
        .filter(|_| status.success())
        .ok_or("Ego Browser가 페이지를 처리하지 못했어.")?;
    match response["error"].as_str() {
        Some("changed") => {
            Err("페이지나 선택한 요소가 바뀌었어. 현재 페이지를 다시 확인해줘.".into())
        }
        Some(_) => Err("Ego Browser에서 페이지를 읽거나 조작하지 못했어.".into()),
        None => Ok(response),
    }
}
