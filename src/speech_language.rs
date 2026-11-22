#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeechLanguage {
    pub code: &'static str,
    pub locale: &'static str,
    pub name: &'static str,
}

impl SpeechLanguage {
    pub const KOREAN: Self = Self {
        code: "ko",
        locale: "ko-KR",
        name: "한국어",
    };

    pub const ALL: [Self; 10] = [
        Self::KOREAN,
        Self {
            code: "en",
            locale: "en-US",
            name: "English",
        },
        Self {
            code: "zh",
            locale: "zh-CN",
            name: "中文",
        },
        Self {
            code: "ja",
            locale: "ja-JP",
            name: "日本語",
        },
        Self {
            code: "es",
            locale: "es-ES",
            name: "Español",
        },
        Self {
            code: "fr",
            locale: "fr-FR",
            name: "Français",
        },
        Self {
            code: "de",
            locale: "de-DE",
            name: "Deutsch",
        },
        Self {
            code: "it",
            locale: "it-IT",
            name: "Italiano",
        },
        Self {
            code: "pt",
            locale: "pt-BR",
            name: "Português",
        },
        Self {
            code: "ru",
            locale: "ru-RU",
            name: "Русский",
        },
    ];

    pub fn from_code(code: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|language| language.code == code)
            .unwrap_or(Self::KOREAN)
    }

    pub fn uses_neural_voice(self) -> bool {
        matches!(self.code, "ko" | "en")
    }
}
