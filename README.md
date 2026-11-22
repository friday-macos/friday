# FRIDAY for macOS — AI Assistant for Voice & Text

**Talk, type, and keep your work moving on your Mac.** FRIDAY helps you chat with Codex, keep track of projects, and run background tasks from the menu bar.

`Apple Silicon` · `macOS 14+` · `Rust` · `v0.2.0 Preview`

[Language support & English quick start](#language-support--english-quick-start) · [한국어로 시작하기](#시작하기) · [빌드](#소스에서-빌드) · [데이터와 권한](#데이터와-권한) · [라이선스](#라이선스)

FRIDAY는 메뉴 막대에서 열리는 한국어 음성·텍스트 비서다. 작업을 진행하는 동안 새 질문을 보낼 수 있고, Mac과 연결된 브라우저에서 같은 대화와 진행 상황을 볼 수 있다.

| 기능 | 사용 방법 |
| --- | --- |
| **한국어 대화** | **말하기**를 누르거나 **‘프라이데이’ 호출**을 켠다. 메뉴 막대나 `⌃⇧J`로 앱을 열 수 있다. |
| **프로젝트 기억** | 한 대화 화면에서 프로젝트를 오가며 대화와 작업 이력을 이어간다. 기록은 Mac의 SQLite에 저장한다. |
| **백그라운드 작업** | 작업을 보내고 진행 상황과 결과를 작업 화면에서 확인한다. 권한이 필요한 작업은 승인 카드로 표시된다. |
| **iPhone·iPad 연결** | Mac에서 웹 연결을 켜고, 같은 Tailscale 네트워크에서 HTTPS와 연결 코드로 접속한다. |

## Language support & English quick start

**Use the text field to write in your preferred language.** Replies default to Korean in this preview. Ask for a reply in your language in your message, for example “Please reply in English,” “Réponds en français,” or “日本語で答えてください。” The Mac and web interfaces, speech recognition, and voice playback currently use Korean.

1. Use an Apple Silicon Mac running macOS 14 or later. Install the [official Codex CLI](https://developers.openai.com/codex/cli) and run `codex login` with your own ChatGPT account.
2. Build and open `FRIDAY.app` using the [build commands below](#소스에서-빌드). The source build requires Rust, Cargo, the Xcode Command Line Tools, and CMake.
3. Type your message and send with `⌘↵` or **보내기** (Send). You can use the text field on the Mac or in a connected browser.

## 시작하기

1. Apple Silicon Mac에 공식 Codex CLI를 설치하고 본인의 ChatGPT 계정으로 `codex login`을 실행한다. FRIDAY는 Codex app-server에 연결한다. 별도의 OpenAI API 키를 설정할 필요는 없다. [Codex CLI 설치](https://developers.openai.com/codex/cli)
2. [소스에서 빌드](#소스에서-빌드)한 `FRIDAY.app`을 연다. **보내기** 또는 `⌘↵`로 첫 메시지를 보낸다.
3. 음성을 쓸 때 **말하기**를 누른 뒤 macOS의 마이크와 음성 인식 권한을 허용한다. Mac 낭독은 macOS 한국어 음성을 사용할 수 있다.

### 선택 기능

- **브라우저 음성 출력** · Mac에 [Qwen3-TTS 모델](https://huggingface.co/Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice/tree/85e237c12c027371202489a0ec509ded67b5e4b5)을 내려받는다. 모델 파일과 `speech_tokenizer/`를 `~/Library/Application Support/JARVIS/voice-model/`에 둔다. 이 모델은 약 2.3GiB이며 앱 ZIP에 포함되지 않는다.
- **원격 접속** · 앱의 **연결 → 웹 연결 켜기**에서 로컬 주소와 연결 코드를 확인한다. 다른 기기에서는 본인의 Tailscale 네트워크에 로그인하고 [Tailscale Serve](https://tailscale.com/kb/1312/serve) HTTPS 주소를 사용한다. 웹 마이크를 쓸 기기에서도 마이크 권한을 허용한다.
- **브라우저 작업** · 웹 페이지 열기, 읽기, 일부 조작에는 Mac에 별도로 설치된 Ego Browser가 필요하다. 사용할 수 없는 환경에서도 텍스트, Mac 음성 및 기본 작업은 이용할 수 있다.

## 소스에서 빌드

Rust와 Cargo, Xcode 명령줄 도구, CMake가 필요하다.

```sh
cargo build --locked --release --features neural-voice
mkdir -p FRIDAY.app/Contents/MacOS FRIDAY.app/Contents/Resources
cp Info.plist FRIDAY.app/Contents/
cp target/release/jarvis target/release/jarvis-voice FRIDAY.app/Contents/MacOS/
cp -R licenses FRIDAY.app/Contents/Resources/
cp LICENSE FRIDAY.app/Contents/Resources/
codesign --force --sign - FRIDAY.app/Contents/MacOS/jarvis-voice
codesign --force --sign - FRIDAY.app
open FRIDAY.app
```

Whisper 기반 웹 전사를 사용하려면 [Whisper 모델](https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin)에서 `ggml-small-q5_1.bin`을 받아 `FRIDAY.app/Contents/Resources/whisper-small-q5_1.bin`에 넣고 앱을 다시 서명한다. 모델 SHA-256은 `ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb`다.

## 데이터와 권한

- **Mac에 저장** · 대화, 프로젝트, 승인 기록은 `~/Library/Application Support/JARVIS/`에 저장된다. 해당 폴더에는 로컬 작업 결과와 앱이 생성한 설정도 들어갈 수 있다.
- **Codex로 전달** · 사용자가 보낸 텍스트와 음성 전사, 대화에 필요한 문맥은 로그인한 Codex를 통해 처리된다. 사용자가 전송을 승인한 화면 이미지도 Codex에 전달될 수 있다.
- **기기 제어** · 선택 기능에 한해 손쉬운 사용과 화면 기록 권한이 필요할 수 있다. 기본 텍스트 대화는 그 권한 없이 시작할 수 있다.
- **웹 연결** · 같은 기록과 승인 화면을 연결된 브라우저에서 볼 수 있다. 연결 코드는 비공개로 관리한다.

## 라이선스

FRIDAY 소스는 [MIT](LICENSE)로 제공한다. 포함되거나 연동되는 구성 요소의 출처와 조건은 [제3자 고지](licenses/README.md)를 참고한다.
