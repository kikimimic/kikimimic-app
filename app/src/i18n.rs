//! UI text in English and Japanese. Every string has both, or the build
//! fails; the core sends typed notices, so all wording lives here.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ui {
    En,
    Ja,
}

impl Ui {
    /// From the OS's preferred languages (an app launched from Finder has no
    /// LANG), overridable with KKM_UI_LANG=en|ja.
    pub fn detect() -> Self {
        if let Ok(forced) = std::env::var("KKM_UI_LANG") {
            return Self::from_locale(&forced);
        }
        sys_locale::get_locales()
            .next()
            .map_or(Self::En, |l| Self::from_locale(&l))
    }

    pub fn from_locale(locale: &str) -> Self {
        if locale.to_ascii_lowercase().starts_with("ja") {
            Self::Ja
        } else {
            Self::En
        }
    }
}

macro_rules! texts {
    ($($name:ident: $en:expr, $ja:expr;)*) => {
        impl Ui {
            $(pub fn $name(self) -> &'static str {
                match self {
                    Self::En => $en,
                    Self::Ja => $ja,
                }
            })*
        }
    };
}

texts! {
    tab_session: "Session", "セッション";
    tab_recordings: "Recordings", "録音";
    tab_models: "Models", "モデル";
    tab_settings: "Settings", "設定";
    stop: "■ Stop", "■ 停止";
    record_meeting: "● Record meeting", "● 会議を録音";
    face_to_face: "Face to face", "対面";
    idle: "Idle", "待機中";
    listening: "Listening", "聞き取り中";
    finishing: "Finishing translations…", "翻訳を仕上げています…";
    follow: "Follow", "切り替える";
    stay: "Stay", "そのまま";
    translating: "Translating…", "訳しています…";
    all_recordings: "← All recordings", "← 録音一覧";
    identifying: "Identifying speakers… (the first run downloads the model)", "話者を判別しています…（初回はモデルをダウンロードします）";
    identify_speakers: "Identify speakers", "話者を判別";
    no_recordings: "No recordings yet.", "まだ録音はありません。";
    ready: "Ready", "使用可能";
    accept_license: "Accept license", "ライセンスに同意";
    download: "Download", "ダウンロード";
    in_use: "  (in use)", "  （使用中）";
    spoken_languages: "Spoken languages (up to two)", "話される言語（最大2つ）";
    translate_into: "Translate into", "翻訳先";
    dont_translate: "Don't translate", "翻訳しない";
    mutual: "Also translate the other way (⇄)", "逆方向にも翻訳する（⇄）";
    recognition_model: "Recognition model", "文字起こしモデル";
    unheard_languages: "This model cannot recognize these spoken languages, so their speech will not be transcribed: ", "このモデルは次の言語を認識できないため、その発話は文字起こしされません: ";
    translation_model: "Translation model", "翻訳モデル";
    none: "None", "なし";
    glossary: "Glossary", "辞書";
    glossary_help: "Fixes what the recognizer gets wrong, and keeps the right spelling in translations.", "誤認識を正しい表記に直し、翻訳でもその表記を保ちます。";
    misrecognized_as: "Misrecognized as…", "誤認識された表記";
    correct_spelling: "Correct spelling", "正しい表記";
    add: "Add", "追加";
    remove: "Remove", "削除";
    system_audio_silent: "System audio is silent. Allow Kikimimic in System Settings > Privacy & Security > Screen & System Audio Recording.", "システム音声が無音です。システム設定 > プライバシーとセキュリティ > 画面とシステムオーディオ録音 で Kikimimic を許可してください。";
}

impl Ui {
    pub fn error(self, message: &str) -> String {
        match self {
            Self::En => format!("Error: {message}"),
            Self::Ja => format!("エラー: {message}"),
        }
    }

    pub fn output_changed(self, detected: &str, capturing: &str) -> String {
        match self {
            Self::En => format!("Output changed to {detected} (capturing {capturing}). Follow it?"),
            Self::Ja => format!(
                "出力が {detected} に変わりました（いまは {capturing} を録音中）。切り替えますか？"
            ),
        }
    }

    pub fn now_capturing(self, name: &str) -> String {
        match self {
            Self::En => format!("Now capturing {name}"),
            Self::Ja => format!("{name} を録音しています"),
        }
    }

    pub fn translation_failed(self, message: &str) -> String {
        match self {
            Self::En => format!("Translation failed: {message}"),
            Self::Ja => format!("翻訳に失敗しました: {message}"),
        }
    }

    pub fn downloading(self, percent: f64) -> String {
        match self {
            Self::En => format!("Downloading {percent:.0}%"),
            Self::Ja => format!("ダウンロード中 {percent:.0}%"),
        }
    }

    pub fn source_unavailable(self, source: &str, reason: &str) -> String {
        match self {
            Self::En => format!("{source} unavailable: {reason}"),
            Self::Ja => format!("{source} を使えません: {reason}"),
        }
    }

    pub fn recording_stopped(self, reason: &str) -> String {
        match self {
            Self::En => format!("Recording stopped: {reason}"),
            Self::Ja => format!("録音を停止しました: {reason}"),
        }
    }

    pub fn overrun(self, lane: &str, ms: u64) -> String {
        match self {
            Self::En => format!("{lane}: {ms} ms of audio dropped"),
            Self::Ja => format!("{lane}: 音声が {ms} ms 欠けました"),
        }
    }

    pub fn decode_failed(self, lane: &str, reason: &str) -> String {
        match self {
            Self::En => format!("{lane}: recognition failed: {reason}"),
            Self::Ja => format!("{lane}: 認識に失敗しました: {reason}"),
        }
    }

    pub fn translation_unavailable(self, reason: &str) -> String {
        match self {
            Self::En => format!("Translation unavailable: {reason}"),
            Self::Ja => format!("翻訳を使えません: {reason}"),
        }
    }

    pub fn recording_summary(self, duration: &str, lines: usize, snippet: &str) -> String {
        match self {
            Self::En => format!("{duration} · {lines} lines · {snippet}"),
            Self::Ja => format!("{duration} · {lines}行 · {snippet}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn japanese_locales_get_japanese() {
        assert_eq!(Ui::from_locale("ja-JP"), Ui::Ja);
        assert_eq!(Ui::from_locale("ja"), Ui::Ja);
        assert_eq!(Ui::from_locale("en-US"), Ui::En);
        assert_eq!(Ui::from_locale("fr"), Ui::En);
    }
}
