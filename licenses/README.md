# 제3자 구성 요소

| 구성 요소 | 배포 범위 | 고지 |
| --- | --- | --- |
| Whisper 모델 | 웹 음성 인식 모델을 포함한 패키지 | [MIT](whisper-model.txt) · [모델 출처](https://huggingface.co/ggerganov/whisper.cpp) |
| whisper.cpp | 음성 인식 | [MIT](whisper.cpp.txt) |
| whisper-rs | 음성 인식 Rust 인터페이스 | [MIT](whisper-rs.txt) |
| qwen3-tts-candle | 선택 음성 합성 실행 파일 | [고정 리비전의 MIT 메타데이터](https://github.com/HeiSir2014/qwen3-tts-candle/blob/c2bd9ee4f0c5a64876bb99721c05d4c648ed0eb8/crates/qwen3-tts/Cargo.toml) |
| Qwen3-TTS 모델 | 공식 사이트에서 별도로 내려받는 모델 | [Apache 2.0](https://huggingface.co/Qwen/Qwen3-TTS-12Hz-0.6B-CustomVoice) |
| Codex CLI, Tailscale, Ego Browser | 사용자가 별도로 설치하는 외부 도구 | 각 공급자의 라이선스와 이용 조건 적용 |

Rust 패키지와 버전은 저장소의 `Cargo.lock`에서 확인할 수 있다.
