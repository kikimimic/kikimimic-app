//! UI text in English and Japanese. Every string has both, or the build
//! fails; the core sends typed notices, so all wording lives here.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ui {
    En,
    Ja,
}

impl Ui {
    /// The language chosen in Settings, else the OS's.
    pub fn new(chosen: Option<&str>) -> Self {
        chosen.map_or_else(Self::detect, Self::from_locale)
    }

    /// The code saved as `Settings::ui_language`.
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Ja => "ja",
        }
    }

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
    tab_models: "Models", "モデル";
    ui_language: "Display language", "表示言語";
    same_as_os: "Same as macOS", "macOS に合わせる";
    stop: "■ Stop", "■ 停止";
    record_meeting: "● Record meeting", "● 会議を録音";
    face_to_face: "Face to face", "対面";
    idle: "Idle", "待機中";
    listening: "Listening", "聞き取り中";
    finishing: "Finishing translations…", "翻訳を仕上げています…";
    follow: "Follow", "切り替える";
    stay: "Stay", "そのまま";
    translating: "Translating…", "訳しています…";
    identifying: "Identifying speakers… (the first run downloads the model)", "話者を判別しています…（初回はモデルをダウンロードします）";
    identify_speakers: "Identify speakers", "話者を判別";
    no_recordings: "No recordings yet.", "まだ録音はありません。";
    ready: "Ready", "使用可能";
    built_into_macos: "Built into macOS", "macOS 標準";
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
    clear_transcript: "Clear", "クリア";
    save_face_to_face: "Save face-to-face audio and transcript", "対面の音声と文字起こしを保存";
    clear_on_start: "Clear the screen when a recording starts (otherwise right-click to clear)", "録音開始時に画面をクリアする（オフのときは右クリックでクリア）";
    add: "Add", "追加";
    remove: "Remove", "削除";
    move_to_trash: "Move to Trash", "ゴミ箱に入れる";
    system_audio_silent: "System audio is silent. Allow Kikimimic in System Settings > Privacy & Security > Screen & System Audio Recording.", "システム音声が無音です。システム設定 > プライバシーとセキュリティ > 画面とシステムオーディオ録音 で Kikimimic を許可してください。";
    google_calendar: "Google Calendar", "Google カレンダー";
    calendar_description: "Shortly before a meeting starts, a card appears at the top of the screen so you can start recording in one click. Only your calendar is read; audio and transcripts never leave your computer.", "会議が始まる直前に画面の上にカードを出し、ワンクリックで録音を始められます。読むのはカレンダーだけで、音声と文字起こしは外に出ません。";
    calendar_unavailable: "Not available in this build.", "このビルドでは使えません。";
    sign_in_google: "Sign in with Google", "Google でサインイン";
    signing_in: "Finish signing in in your browser…", "ブラウザでサインインを完了してください…";
    cancel: "Cancel", "キャンセル";
    sign_out: "Sign out", "サインアウト";
    needs_sign_in: "Google no longer accepts the saved sign-in. Sign in again.", "保存したサインインを Google が受け付けなくなりました。もう一度サインインしてください。";
    join_and_record: "Join & record", "参加して録音";
    record: "Record", "録音を開始";
    dismiss: "Dismiss", "閉じる";
    no_title: "(No title)", "（タイトルなし）";
    untitled_recording: "Recording", "会議の録音";
    search_meetings: "Search meetings and what was said", "会議と発言を検索";
    filter_all: "All", "すべて";
    filter_unfinished: "Unfinished", "未完了";
    today: "Today", "今日";
    yesterday: "Yesterday", "昨日";
    undated: "Undated", "日付なし";
    progress_none: "Not processed", "未処理";
    progress_transcribed: "Transcribed", "文字起こし済み";
    progress_speakers: "Speakers done", "話者済み";
    progress_minutes: "Minutes done", "議事録済み";
    drop_audio: "Drop audio here to import", "音声をドロップして取り込む";
    drop_audio_formats: "WAV / M4A / MP3 · several at once", "WAV / M4A / MP3 · 複数可";
    import_details: "Details", "詳細";
    settings_button: "Settings", "設定";
    show_live: "Show live", "ライブを見る";
    recording_meeting: "Recording a meeting", "会議を録音中";
    recording_face: "Recording face to face", "対面で録音中";
    face_saved: "Face to face: saved", "対面: 保存する";
    face_not_saved: "Face to face: not saved", "対面: 保存しない";
    captions_shown: "Captions: on", "字幕: 表示";
    captions_hidden: "Captions: off", "字幕: 非表示";
    processed_locally: "Processed on this Mac", "ローカル処理";
    pick_a_recording: "Pick a meeting on the left, or start recording.", "左の一覧から会議を選ぶか、録音を始めてください。";
    matches_in_speech: "In what was said", "発言・メモ";
    no_matches: "No matches", "一致なし";
    show_older: "Show older meetings", "以前の会議を表示";
    section_general: "General", "一般";
    section_link: "Connections", "連携";
    section_advanced: "Advanced", "詳細";
    section_recording: "Recording", "録音";
    saved_automatically: "Changes are saved automatically", "変更は自動で保存";
    destinations_help: "Write each meeting to a folder you choose, under a file name and in a format you choose. A note vault (Obsidian or any other) is just a folder.", "決めたフォルダに、決めたファイル名と形式で会議を書き出します。Obsidian などのノートの保管庫も、ただのフォルダとして扱います。";
    dest_name: "Name", "名前";
    dest_folder: "Folder", "フォルダ";
    dest_file_name: "File name", "ファイル名";
    dest_format: "Format", "形式";
    dest_when: "Write", "書き出すとき";
    dest_enabled: "Enabled", "使う";
    dest_template: "Template", "テンプレート";
    format_template: "Template (Markdown / text)", "テンプレート（Markdown / テキスト）";
    when_manual: "Only when I ask", "手動のみ";
    when_minutes: "When minutes are ready", "議事録ができたら";
    when_recording: "When a recording or import ends", "録音・取り込みが終わったら";
    template_inline: "Built-in template", "既定のテンプレート";
    template_use_inline: "Use the built-in one", "既定に戻す";
    template_variables: "Variables:", "変数:";
    choose: "Choose…", "選ぶ…";
    choose_file: "Use a file…", "ファイルを選ぶ…";
    apply: "Apply", "適用";
    on: "On", "オン";
    off: "Off", "オフ";
    step_transcript: "Transcript", "文字起こし";
    step_speakers: "Speakers", "話者";
    step_minutes: "Minutes", "議事録";
    step_export: "Export", "書き出し";
    write_pending: "Write to destinations", "書き出し先に書き出す";
    minutes_not_made: "No minutes yet", "議事録はまだありません";
    not_transcribed_yet: "Not transcribed yet", "まだ文字起こしされていません";
    all_done: "Done", "完了";
    play: "▶ Play", "▶ 再生";
    pause: "■ Stop", "■ 停止";
    click_a_word: "Click a word to play from it", "単語をクリックするとそこから再生";
    tab_text: "Transcript", "本文";
    tab_subtitles: "Subtitles", "字幕";
    tab_stats: "Statistics", "統計";
    minutes_summary: "Summary", "要約";
    minutes_decisions: "Decisions", "決定事項";
    minutes_actions: "Action items", "宿題";
    minutes_open: "Open questions", "未決事項";
    loading: "Loading…", "読み込み中…";
    export_fresh: "Up to date", "最新";
    export_stale: "Out of date", "古い";
    export_not_written: "Not written", "未書き出し";
    edit_destinations: "Edit destinations…", "書き出し先を編集…";
    write: "Write", "書き出す";
    rewrite: "Write again", "書き出し直す";
    lane_me: "Me", "自分";
    lane_them: "Them", "相手";
    live_title: "Live", "ライブ";
    advisor: "Advisor", "アドバイザー";
    quit: "Quit Kikimimic", "Kikimimic を終了";
    recording_menu: "Recording", "録音";
    view_menu: "View", "表示";
    ask_button: "Ask", "質問";
    commands_button: "Commands", "コマンド";
    palette_placeholder: "Find an action (↑↓ then Enter)", "操作を探す（↑↓ で選んで Enter）";
    palette_hint: "Everything here is also a button on screen.", "ここにある操作は、すべて画面上のボタンからも行えます。";
    palette_nothing: "No matching action.", "一致する操作がありません。";
    cmd_search: "Search meetings", "会議を検索";
    cmd_import: "Import audio…", "音声を取り込む…";
    cmd_write_pending: "Write this meeting to its destinations", "この会議を書き出し先へ書き出す";
    cmd_make_minutes: "Make minutes for this meeting", "この会議の議事録を作る";
    cmd_export_settings: "Export destinations", "書き出し先の設定";
    ask_title: "Ask", "質問";
    not_chosen: "not chosen", "未設定";
    live_stopped: "Live (stopped)", "ライブ（停止中）";
    live_empty: "Listening… what is said appears here as one stream. Captions show over the meeting app.", "聞き取っています… 話し始めると、ここに 1 本の流れで出ます。会議アプリの上には字幕が出ます。";
    close: "Close", "閉じる";
    ask_this_meeting: "Ask about this meeting", "この会議に質問";
    ask_needs_meeting: "Open a meeting or start recording to ask about it.", "会議を開くか録音を始めると、その会議について質問できます。";
    toggle_auto_record: "Toggle auto-record", "自動録音を切り替える";
    toggle_captions: "Toggle captions", "字幕を切り替える";
    started_automatically: "Started automatically", "自動で開始";
    auto_record_on: "Auto-record: on", "自動録音: オン";
    auto_record_off: "Auto-record: off", "自動録音: オフ";
    auto_record_help: "Record calendar meetings by themselves: start when one begins, stop a minute after it ends. A meeting you stop yourself is not restarted.", "カレンダーの会議を自動で録音します。始まったら録音し、終わって 1 分たったら止めます。自分で止めた会議は再開しません。";
    stop_advisor: "Stop advisor", "アドバイザーを停止";
    no_advice: "Nothing to point out right now.", "いま出せる助言はありません。";
    advice_follow_up: "Follow-up", "持ち帰り";
    advice_open_question: "Open question", "未回答の質問";
    advice_undecided: "Undecided", "未決";
    ask: "Ask", "聞く";
    ask_placeholder: "Ask about this meeting…", "この会議について質問…";
    asking: "Asking…", "問い合わせ中…";
    unconfirmed_answer: "The transcript does not back an answer, so none is given.", "文字起こしに根拠が見つからないため、答えを確定できません。";
    choose_agent_first: "Choose Claude Code or Codex in Settings › Connections first.", "先に 設定 › 連携 で Claude Code か Codex を選んでください。";
    sends_transcript_and_question: "What is sent: this meeting's transcript (originals only) and your question. Not sent: audio, translations, other meetings.", "送るもの: この会議の文字起こし（原文のみ）と質問。送らないもの: 音声、訳文、ほかの会議。";
    sends_transcript: "What is sent: this meeting's transcript (originals only). Not sent: audio, translations, other meetings.", "送るもの: この会議の文字起こし（原文のみ）。送らないもの: 音声、訳文、ほかの会議。";
    send: "Send", "送る";
    dont_send: "Don't send", "送らない";
    make_minutes: "Make minutes", "議事録を作る";
    remake_minutes: "Make again", "作り直す";
    making_minutes: "Making minutes…", "議事録を作っています…";
    minutes_from_transcript: "Every item links to what it is based on; owners and deadlines appear only if someone said them.", "各項目は根拠の発言につながっています。担当と期限は、発言にあったときだけ入ります。";
    agents: "Coding agent", "エージェント";
    agents_help: "Questions and minutes go to the agent you choose, through its command-line tool you are signed in to. It sends the meeting's transcript to its company; you confirm before the first time each run.", "質問と議事録は、選んだエージェントに、サインイン済みのコマンドラインツール経由で送ります。会議の文字起こしがその会社に送られるため、起動ごとに最初の 1 回は送る前に確認します。";
    mcp_title: "Let agents read meetings (MCP)", "エージェントから会議を読む（MCP）";
    mcp_help: "Register Kikimimic once and a Claude Code or Codex session in your terminal can list, search and read meetings — including the one being recorded. Read-only.", "一度登録すると、ターミナルの Claude Code や Codex から会議の一覧・検索・文字起こし（録音中の会議も）を読めます。読み取り専用です。";
}

impl Ui {
    pub fn signed_in_as(self, email: &str) -> String {
        match self {
            Self::En => format!("Signed in as {email}"),
            Self::Ja => format!("{email} でサインイン中"),
        }
    }

    pub fn last_read(self, time: &str) -> String {
        match self {
            Self::En => format!("Last read at {time}"),
            Self::Ja => format!("最後に読んだ時刻 {time}"),
        }
    }

    pub fn starts_in(self, clock: &str) -> String {
        match self {
            Self::En => format!("in {clock}"),
            Self::Ja => format!("あと {clock}"),
        }
    }

    pub fn started_ago(self, clock: &str) -> String {
        match self {
            Self::En => format!("started {clock} ago"),
            Self::Ja => format!("{clock} 前に開始"),
        }
    }

    pub fn google_error(self, error: &crate::google::Error) -> String {
        use crate::google::Error;
        match (self, error) {
            (Self::En, Error::Denied(reason)) => format!("Google sign-in was cancelled ({reason})."),
            (Self::Ja, Error::Denied(reason)) => format!("Google のサインインが中止されました（{reason}）。"),
            (Self::En, Error::TimedOut) => "Google sign-in timed out.".into(),
            (Self::Ja, Error::TimedOut) => "Google のサインインがタイムアウトしました。".into(),
            (_, Error::Cancelled) => String::new(),
            (Self::En, Error::NoRefreshToken) => "Google did not return a refresh token.".into(),
            (Self::Ja, Error::NoRefreshToken) => "Google がリフレッシュトークンを返しませんでした。".into(),
            (Self::En, Error::MissingScopes) => "Kikimimic needs to read your calendars and their events. Sign in again and leave both permissions ticked.".into(),
            (Self::Ja, Error::MissingScopes) => "カレンダーとその予定を読む権限が必要です。もう一度サインインして、2つの権限をどちらも許可してください。".into(),
            (Self::En, Error::Other(message)) => format!("Google Calendar: {message}"),
            (Self::Ja, Error::Other(message)) => format!("Google カレンダー: {message}"),
        }
    }

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
}

impl Ui {
    pub fn send_to(self, agent: &str, company: &str) -> String {
        match self {
            Self::En => format!("Send to {agent} ({company})?"),
            Self::Ja => format!("{agent}（{company}）に送ります。"),
        }
    }

    pub fn asks_go_to(self, agent: &str) -> String {
        match self {
            Self::En => format!("Questions: {agent}"),
            Self::Ja => format!("質問: {agent}"),
        }
    }

    pub fn answers_from(self, agent: &str) -> String {
        match self {
            Self::En => format!(
                "Answered by {agent}, quoting the transcript. Unbacked answers are not shown."
            ),
            Self::Ja => {
                format!("{agent} が文字起こしを引用して答えます。根拠のない答えは表示しません。")
            }
        }
    }

    pub fn speech_share(self, percent: f64) -> String {
        match self {
            Self::En => format!("{percent:.0}% of speech"),
            Self::Ja => format!("発話の割合 {percent:.0}%"),
        }
    }

    pub fn lane_metrics(self, longest_s: f64, cpm: f64, questions: usize) -> String {
        match self {
            Self::En => {
                format!("Longest {longest_s:.1} s · {cpm:.0} chars/min · {questions} questions")
            }
            Self::Ja => {
                format!("最長の連続発話 {longest_s:.1} 秒 · {cpm:.0} 文字/分 · 質問 {questions}")
            }
        }
    }

    pub fn overlaps(self, n: usize) -> String {
        match self {
            Self::En => {
                format!("{n} overlapping utterances (counted reliably only with headphones)")
            }
            Self::Ja => format!("重なった発話 {n}（ヘッドホン使用時だけ正しく数えられます）"),
        }
    }

    pub fn day(self, month: u32, day: u32, weekday: u32) -> String {
        const JA: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
        const EN: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let w = weekday as usize % 7;
        match self {
            Self::En => format!("{} {month}/{day}", EN[w]),
            Self::Ja => format!("{month}/{day}（{}）", JA[w]),
        }
    }

    pub fn next_meeting(self, title: &str, at: &str) -> String {
        match self {
            Self::En => format!("Next: {title} at {at}"),
            Self::Ja => format!("次: {title}  {at}"),
        }
    }

    pub fn unfinished_count(self, n: usize) -> String {
        match self {
            Self::En => format!("{n} unfinished"),
            Self::Ja => format!("後処理待ち {n}"),
        }
    }

    pub fn importing(self, n: usize) -> String {
        match self {
            Self::En => format!("Importing {n}"),
            Self::Ja => format!("取り込み {n}"),
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

    #[test]
    fn a_chosen_language_wins_over_the_os() {
        assert_eq!(Ui::new(Some("en")), Ui::En);
        assert_eq!(Ui::new(Some("ja")), Ui::Ja);
    }
}
