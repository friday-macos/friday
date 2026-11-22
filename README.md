# FRIDAY for macOS — AI Assistant for Voice & Text

**A menu bar assistant for talking to Codex, keeping project history, and running background tasks on your Mac.**

`Apple Silicon` · `macOS 14+` · `Rust` · `v0.2.0 Preview`

[Quick start](#quick-start) · [Language support](#language-support) · [Build from source](#build-from-source) · [Privacy and permissions](#privacy-and-permissions) · [License](#license)

| Feature | What you can do |
| --- | --- |
| **Voice and text** | Open FRIDAY from the menu bar or with `Control–Shift–J`. Type a message or select **Talk** (`말하기`). |
| **Project history** | Switch projects and pick up earlier conversations. FRIDAY stores the history in SQLite on your Mac. |
| **Background tasks** | Keep chatting while a task runs. Follow its progress and review actions that need your approval. |
| **Browser access** | Open the same conversation and task dashboard on an iPhone or iPad connected to your Tailscale network. |

## Quick start

1. Install the [Codex CLI](https://developers.openai.com/codex/cli) and run `codex login` with your own ChatGPT account. FRIDAY connects to Codex and uses your existing sign-in.
2. [Build FRIDAY from source](#build-from-source) and open `FRIDAY.app`.
3. Type a message and press `Command–Return` or select **Send** (`보내기`). To use voice, select **Talk** (`말하기`) and allow microphone and speech recognition access when macOS prompts you.

## Language support

| | v0.2.0 preview download | Latest source on `main` |
| --- | --- | --- |
| **Text conversations** | Write in your preferred language and ask for a reply in that language. Replies default to Korean. | Replies follow the language of your message unless you ask for a different one. |
| **Mac voice** | Korean | English, Korean, Chinese, Japanese, Spanish, French, German, Italian, Portuguese, and Russian appear in the voice selector. Availability depends on the speech recognition and reading voices installed on your Mac. |
| **Web voice** | Korean | English and Korean with the optional Qwen3-TTS model. |
| **Mac and web menus** | Korean | Korean |

On the latest source, choose **Voice language** (`음성 언어 / Voice language`) beside the volume controls in the Mac app before starting a voice session. Text works on both the Mac app and the connected browser.

## Build from source

Install Rust and Cargo, the Xcode Command Line Tools, and CMake on an Apple Silicon Mac running macOS 14 or later. From the repository directory, run:

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

### Optional setup

- **Remote access** — In the Mac app, select **Connect** (`연결`) and **Enable Web Connection** (`웹 연결 켜기`) to find the local address and pairing code. To connect from another device, use HTTPS through [Tailscale Serve](https://tailscale.com/kb/1312/serve) on your own Tailscale network. Allow microphone access on the device if you use web voice.
- **Qwen3 voice model** — Download the model files and `speech_tokenizer/` from the [pinned Qwen3-TTS revision](https://huggingface.co/Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice/tree/85e237c12c027371202489a0ec509ded67b5e4b5). Put them in `~/Library/Application Support/JARVIS/voice-model/`. The model is approximately 2.3 GiB and is downloaded separately.
- **Whisper transcription** — Download the [Whisper Small model](https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin), save it as `FRIDAY.app/Contents/Resources/whisper-small-q5_1.bin`, then sign the app again. The model SHA-256 is `ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb`.
- **Browser automation** — Opening, reading, and interacting with web pages requires Ego Browser to be installed on the Mac. Chat, Mac voice, and basic tasks are available independently.

## Privacy and permissions

- FRIDAY stores conversations, project history, approvals, settings, and local task files in `~/Library/Application Support/JARVIS/`.
- Codex receives your submitted messages, speech transcripts, and relevant conversation context. It also receives screen captures you explicitly approve for sharing.
- Accessibility and Screen Recording permissions support optional Mac control and screen capture. You can start a text conversation before granting either permission.
- Paired browsers can view the same conversation and approval history. Keep your pairing code private.

## License

FRIDAY is released under the [MIT License](LICENSE). See [Third-party notices](licenses/README.md) for dependencies and separately downloaded models.
