//! Recording-local speaker editing. Automatic evidence stays in the core library.
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    Context, Entity, EventEmitter, IntoElement, Render, ScrollHandle, Window, div, prelude::*, px,
    rgb,
};
use kikimimic_core::annotations::{self, AnnotationFile, Annotations, ReviewState};
use kikimimic_core::library::{self, Session};

use crate::widgets::text_input::TextInput;
use crate::window::words::{self, Playback, WordAction};
use crate::window::{button, mm_ss};
use kikimimic_core::word_timing::WordTiming;

pub struct Seek(pub u64);

pub struct SpeakerEditor {
    dir: PathBuf,
    raw: Session,
    source: String,
    file: Option<AnnotationFile>,
    saved: Annotations,
    name_input: Entity<TextInput>,
    name_baseline: String,
    name_source: String,
    participant: Option<String>,
    participants_open: bool,
    selected: Option<usize>,
    scroll: ScrollHandle,
    message: Option<String>,
    error: Option<String>,
    word_data: Option<Arc<WordTiming>>,
    word_editing: bool,
    word_playback: Option<Playback>,
}

impl EventEmitter<Seek> for SpeakerEditor {}
impl EventEmitter<WordAction> for SpeakerEditor {}

impl SpeakerEditor {
    pub(crate) fn set_word_timing(
        &mut self,
        data: Option<Arc<WordTiming>>,
        editing: bool,
        cx: &mut Context<Self>,
    ) {
        self.word_data = data;
        self.word_editing = editing;
        cx.notify();
    }

    pub(crate) fn set_word_playback(&mut self, playback: Option<Playback>, cx: &mut Context<Self>) {
        self.word_playback = playback;
        cx.notify();
    }

    pub fn new(dir: PathBuf, raw: Session, cx: &mut Context<Self>) -> Self {
        let name_input = cx.new(|cx| TextInput::new("表示名（空欄で名前を解除）", cx));
        cx.observe(&name_input, |_, _, cx| cx.notify()).detach();
        let mut editor = Self {
            dir,
            raw,
            source: String::new(),
            file: None,
            saved: Annotations::default(),
            name_input,
            name_baseline: String::new(),
            name_source: String::new(),
            participant: None,
            participants_open: true,
            selected: None,
            scroll: ScrollHandle::new(),
            message: None,
            error: None,
            word_data: None,
            word_editing: false,
            word_playback: None,
        };
        editor.load_file();
        editor
    }

    fn load_file(&mut self) {
        let loaded = annotations::source_fingerprint(&self.dir, &self.raw.lines)
            .and_then(|source| AnnotationFile::load(&self.dir).map(|file| (source, file)));
        match loaded {
            Ok((source, file)) => {
                self.source = source;
                self.saved = file.annotations.clone();
                self.file = Some(file);
                self.error = None;
            }
            Err(e) => {
                self.file = None;
                self.error = Some(format!(
                    "話者修正を読み込めません: {e:#}。元の本文を表示しています。ファイルを修復して再読込してください。"
                ));
            }
        }
    }

    pub fn is_dirty(&self, cx: &gpui::App) -> bool {
        self.file
            .as_ref()
            .is_some_and(|f| f.annotations != self.saved)
            || (self.participant.is_some() && self.name_input.read(cx).text() != self.name_baseline)
    }

    pub fn warn_unsaved(&mut self, cx: &mut Context<Self>) {
        self.message = Some(
            "未保存の変更があります。保存するか「変更を破棄して再読込」を選んでください。".into(),
        );
        cx.notify();
    }

    /// Rerun completion changes evidence, while retaining an unsaved draft.
    pub fn refresh_evidence(&mut self, cx: &mut Context<Self>) {
        match library::read_raw(&self.dir).and_then(|raw| {
            annotations::source_fingerprint(&self.dir, &raw.lines).map(|source| (raw, source))
        }) {
            Ok((raw, source)) => {
                if raw == self.raw && source == self.source {
                    return;
                }
                let dirty = self.is_dirty(cx);
                if source != self.source {
                    // Stage typed names in the OLD namespace before changing it.
                    match self.stage_name(cx) {
                        Ok(()) => {
                            self.participant = None;
                            self.name_baseline.clear();
                            self.name_input.update(cx, |input, cx| input.clear(cx));
                        }
                        Err(e) => {
                            self.error = Some(format!(
                                "旧結果の名前入力を保持しています: {e}。名前を修正して保存するか、変更を破棄してください。"
                            ));
                        }
                    }
                    self.message = Some(
                        "話者分離結果が変わりました。旧結果の名前・割当ては再確認してください。"
                            .into(),
                    );
                }
                self.raw = raw;
                self.selected = None;
                self.source = source;
                if !dirty {
                    self.load_file();
                }
            }
            Err(e) => self.error = Some(format!("再読込に失敗しました: {e:#}")),
        }
        cx.notify();
    }

    fn stage_name(&mut self, cx: &gpui::App) -> anyhow::Result<()> {
        if let (Some(id), Some(file)) = (&self.participant, &mut self.file) {
            file.annotations
                .rename(&self.name_source, id, self.name_input.read(cx).text())?;
            self.name_baseline = self.name_input.read(cx).text().to_string();
        }
        Ok(())
    }

    fn choose_participant(&mut self, id: String, cx: &mut Context<Self>) {
        if let Err(e) = self.stage_name(cx) {
            self.error = Some(e.to_string());
            cx.notify();
            return;
        }
        self.participants_open = true;
        let name = self
            .file
            .as_ref()
            .and_then(|f| {
                f.annotations
                    .names
                    .iter()
                    .find(|n| n.source == self.source && n.speaker == id)
            })
            .map(|n| n.name.clone())
            .unwrap_or_default();
        self.name_baseline = name.clone();
        self.name_source = self.source.clone();
        self.name_input
            .update(cx, |input, cx| input.set_text(name, cx));
        self.participant = Some(id);
        self.error = None;
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let result = self.stage_name(cx).and_then(|_| {
            self.file
                .as_mut()
                .ok_or_else(|| anyhow::anyhow!("注釈を読み込めるまで保存できません"))?
                .save(&self.dir, &self.source)
        });
        match result {
            Ok(()) => {
                if let Some(file) = &self.file {
                    self.saved = file.annotations.clone();
                }
                self.error = None;
                self.message =
                    Some("話者修正を保存しました。次回の表示・出力に反映します。".into());
            }
            Err(e) => {
                self.error = Some(format!(
                    "保存できませんでした: {e:#}。変更は画面に残っています。"
                ));
                self.message = None;
            }
        }
        cx.notify();
    }

    fn discard_and_reload(&mut self, cx: &mut Context<Self>) {
        // Explicitly labelled destructive-to-draft action; no implicit discard.
        match library::read_raw(&self.dir) {
            Ok(raw) => {
                self.raw = raw;
                self.load_file();
                self.participant = None;
                self.name_baseline.clear();
                self.name_input.update(cx, |input, cx| input.clear(cx));
                self.selected = None;
                self.message = Some("保存済みの内容を再読込しました。".into());
            }
            Err(e) => self.error = Some(format!("再読込に失敗しました: {e:#}")),
        }
        cx.notify();
    }

    fn set_row(
        &mut self,
        index: usize,
        speaker: Option<String>,
        reviewed: bool,
        cx: &mut Context<Self>,
    ) {
        if let (Some(file), Some(line)) = (&mut self.file, self.raw.lines.get(index)) {
            // Exact duplicate evidence is ambiguous, even if its IDs differ.
            let anchor = annotations::Anchor::from(line);
            if self
                .raw
                .lines
                .iter()
                .filter(|l| annotations::Anchor::from(*l) == anchor)
                .count()
                != 1
            {
                self.error = Some(
                    "同じlane・時刻・本文の発言が重複しているため、安全に修正できません。".into(),
                );
            } else {
                file.annotations
                    .correct(line, &self.source, speaker, reviewed);
                self.error = None;
                self.message = None;
            }
        }
        cx.notify();
    }

    fn next_unreviewed(&mut self, cx: &mut Context<Self>) {
        let Some(file) = &self.file else {
            return;
        };
        let states = file
            .annotations
            .reconcile(&self.raw.lines, &self.source)
            .rows;
        if let Some(index) = next_unreviewed(&states, self.selected) {
            self.selected = Some(index);
            self.scroll.scroll_to_item(index);
            cx.emit(Seek(self.raw.lines[index].start_ms));
        } else {
            self.message = Some("すべての発言が確認済みです。確認は任意です。".into());
        }
        cx.notify();
    }
}

fn next_unreviewed(states: &[ReviewState], current: Option<usize>) -> Option<usize> {
    let start = current.map_or(0, |i| i + 1);
    (start..states.len())
        .chain(0..start.min(states.len()))
        .find(|&i| !states[i].reviewed)
}

impl Render for SpeakerEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div().flex().flex_col().gap_2().w_full();
        let dirty = self.is_dirty(cx);
        let mut toolbar = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .items_center()
            .child(
                button("speaker-save", "話者修正を保存", false)
                    .debug_selector(|| "speaker-save".into())
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            )
            .child(
                button("speaker-reload", "変更を破棄して再読込", false)
                    .on_click(cx.listener(|this, _, _, cx| this.discard_and_reload(cx))),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(rgb(0x57606a))
                    .child(if dirty {
                        "未保存の変更あり · 出力には保存済みの内容を使用"
                    } else {
                        "保存済み · 確認は任意（未確認でも出力できます）"
                    }),
            );
        if self.file.is_some() {
            toolbar = toolbar.child(
                button("next-unreviewed", "次の未確認を聞く", false)
                    .debug_selector(|| "next-unreviewed".into())
                    .on_click(cx.listener(|this, _, _, cx| this.next_unreviewed(cx))),
            );
        }
        col = col.child(toolbar);
        if let Some(error) = &self.error {
            col = col.child(div().text_color(rgb(0xcf222e)).child(error.clone()));
        }
        if let Some(message) = &self.message {
            col = col.child(div().text_color(rgb(0x57606a)).child(message.clone()));
        }

        let Some(file) = &self.file else {
            // Corrupt annotations must not hide the automatic evidence.
            for (i, line) in self.raw.lines.iter().enumerate() {
                let start = line.start_ms;
                col = col.child(
                    div()
                        .flex()
                        .flex_col()
                        .py_1()
                        .child(
                            button(("raw-seek", i), mm_ss(start), false)
                                .on_click(cx.listener(move |_, _, _, cx| cx.emit(Seek(start)))),
                        )
                        .child(format!(
                            "{} · {}",
                            line.speaker.as_deref().unwrap_or(&line.lane),
                            line.text
                        )),
                );
            }
            return col.into_any_element();
        };
        let doc = &file.annotations;
        let report = doc.reconcile(&self.raw.lines, &self.source);
        let participants = self
            .raw
            .lines
            .iter()
            .filter_map(|l| l.speaker.clone())
            .chain(report.rows.iter().filter_map(|r| r.speaker.clone()))
            .collect::<BTreeSet<_>>();
        col = col.child(
            button(
                "participants",
                if self.participants_open {
                    "▾ 参加者 · 会議内で名前付け"
                } else {
                    "▸ 参加者 · 会議内で名前付け"
                },
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.participants_open = !this.participants_open;
                cx.notify();
            })),
        );
        if self.participants_open {
            let mut panel = div()
                .flex()
                .flex_col()
                .gap_2()
                .p_2()
                .border_1()
                .border_color(rgb(0xd0d7de))
                .rounded(px(6.));
            let mut ids = div().flex().flex_wrap().gap_2();
            for (i, id) in participants.iter().enumerate() {
                let speaker = id.clone();
                ids = ids.child(
                    button(
                        ("participant", i),
                        format!("{} → {}", id, doc.display_name(&self.source, id)),
                        self.participant.as_ref() == Some(id),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.choose_participant(speaker.clone(), cx)
                    })),
                );
            }
            panel = panel.child(ids);
            if participants.is_empty() {
                panel = panel.child("話者分離後に自動話者IDへ名前を付けられます。");
            }
            if let Some(id) = &self.participant {
                panel = panel
                    .child(format!(
                        "会議内の {id} の表示名（同名でもIDは統合しません）"
                    ))
                    .child(self.name_input.clone());
            }
            for &i in &report.inactive_names {
                let name = &doc.names[i];
                panel = panel.child(div().text_size(px(12.)).text_color(rgb(0x9a6700))
                    .child(format!("旧話者分離の名前（未適用）: {} → {}。現在のIDを聞き直して名前を設定してください。", name.speaker, name.name)));
            }
            col = col.child(panel);
        }
        if !report.unapplied.is_empty() {
            let mut warning = div()
                .flex()
                .flex_col()
                .gap_1()
                .text_size(px(12.))
                .text_color(rgb(0x9a6700))
                .child(format!(
                    "再確認が必要な修正 {} 件（元の修正は保持されています）",
                    report.unapplied.len()
                ));
            for &i in &report.unapplied {
                let c = &doc.corrections[i];
                let selected = c
                    .speaker
                    .as_deref()
                    .map(|s| doc.display_name(&c.source, s))
                    .unwrap_or("未割当");
                warning = warning.child(format!(
                    "{}–{} · {} · 保存した割当て: {} · {}",
                    mm_ss(c.anchor.start_ms),
                    mm_ss(c.anchor.end_ms),
                    c.anchor.lane,
                    selected,
                    c.anchor.text
                ));
            }
            col = col.child(warning);
        }
        let unreviewed = report.rows.iter().filter(|r| !r.reviewed).count();
        col = col.child(div().text_size(px(12.)).text_color(rgb(0x57606a))
            .child(format!("未確認 {unreviewed} / {} 発言 · 名前は参加者一覧、誤割当ては「この発言だけ」から修正", report.rows.len())));
        let mut transcript = div()
            .id("speaker-transcript")
            .flex()
            .flex_col()
            .max_h(px(420.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        for (i, (line, state)) in self.raw.lines.iter().zip(&report.rows).enumerate() {
            let start = line.start_ms;
            let mut meta = div()
                .flex()
                .flex_wrap()
                .gap_2()
                .items_center()
                .text_size(px(12.))
                .child(
                    button(("seek", i), mm_ss(start), false)
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(Seek(start)))),
                );
            if let Some(id) = &state.speaker {
                let id = id.clone();
                meta = meta.child(
                    button(
                        ("speaker-name", i),
                        format!("{} ({id})", doc.display_name(&self.source, &id)),
                        false,
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.choose_participant(id.clone(), cx)),
                    ),
                );
            } else {
                meta = meta.child(format!("{} · 未割当", line.lane));
            }
            let selected_speaker = state.speaker.clone();
            let reviewed = state.reviewed;
            meta = meta
                .child(
                    button(("assign", i), "この発言だけ", self.selected == Some(i))
                        .debug_selector(move || format!("assign-{i}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected = if this.selected == Some(i) {
                                None
                            } else {
                                Some(i)
                            };
                            cx.notify();
                        })),
                )
                .child(
                    button(
                        ("review", i),
                        if reviewed {
                            "✓ 確認済み"
                        } else {
                            "○ 未確認"
                        },
                        reviewed,
                    )
                    .debug_selector(move || format!("review-{i}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_row(i, selected_speaker.clone(), !reviewed, cx)
                    })),
                );
            let mut row = div()
                .flex()
                .flex_col()
                .gap_1()
                .py_2()
                .px_2()
                .border_b_1()
                .border_color(rgb(0xd0d7de));
            if self.selected == Some(i) {
                row = row.bg(rgb(0xf6f8fa));
            }
            row = row.child(meta).child(line.text.clone());
            if let Some(t) = &line.translation {
                row = row.child(div().text_color(rgb(0x0969da)).child(t.clone()));
            }
            if state.needs_reconfirmation {
                row = row.child(div().text_color(rgb(0x9a6700)).text_size(px(12.))
                    .child(format!("旧割当て {} は再確認が必要です。現在の結果で話者を選び直してください。", state.pending_speaker.as_deref().unwrap_or("未割当"))));
            }
            if self.selected == Some(i) {
                let mut choices = div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .items_center()
                    .child("この発言だけの割当て:");
                for (n, id) in participants.iter().enumerate() {
                    let id = id.clone();
                    let reviewed = state.reviewed;
                    choices = choices.child(
                        button(
                            gpui::SharedString::from(format!("choice-{i}-{n}")),
                            format!("{} ({id})", doc.display_name(&self.source, &id)),
                            state.speaker.as_ref() == Some(&id),
                        )
                        .debug_selector(move || format!("choice-{i}-{n}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_row(i, Some(id.clone()), reviewed, cx)
                        })),
                    );
                }
                let reviewed = state.reviewed;
                choices = choices.child(
                    button(("unassign", i), "未割当", state.speaker.is_none()).on_click(
                        cx.listener(move |this, _, _, cx| this.set_row(i, None, reviewed, cx)),
                    ),
                );
                row = row.child(choices);
            }
            if let Some(timed) = self.word_data.as_ref().and_then(|data| data.lines.get(i)) {
                row = row.child(words::word_line(
                    i,
                    line,
                    timed,
                    self.word_editing,
                    self.word_playback.as_ref(),
                    cx,
                ));
            }
            transcript = transcript.child(row);
        }
        col.child(transcript).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let dir = std::env::temp_dir().join(format!(
                "kkm-speaker-ui-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            // Editor tests never play audio; a lane file marks the recording.
            std::fs::write(dir.join("mic.wav"), []).unwrap();
            std::fs::write(dir.join("transcript.jsonl"), concat!(
                "{\"type\":\"utterance\",\"id\":1,\"lane\":\"mic\",\"startMs\":100,\"endMs\":900,\"text\":\"first\",\"speaker\":\"speaker_0\"}\n",
                "{\"type\":\"utterance\",\"id\":2,\"lane\":\"mic\",\"startMs\":1000,\"endMs\":1800,\"text\":\"second\",\"speaker\":\"speaker_1\"}\n"
            )).unwrap();
            Self(dir)
        }
        fn editor(&self, cx: &mut Context<SpeakerEditor>) -> SpeakerEditor {
            SpeakerEditor::new(self.0.clone(), library::read_raw(&self.0).unwrap(), cx)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[gpui::test]
    fn word_controls_share_the_edited_transcript_and_emit_recording_positions(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (editor, cx) = cx.add_window_view(|_, cx| fixture.editor(cx));
        let actions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let received = actions.clone();
        editor
            .update(cx, |_, cx| {
                cx.subscribe_self(move |_, event: &WordAction, _| {
                    received.lock().unwrap().push(event.clone());
                })
            })
            .detach();
        editor.update(cx, |editor, cx| {
            let data: WordTiming = serde_json::from_value(serde_json::json!({
                "schema": 1, "fingerprint": "render-fixture", "source": "fixture", "model": "fixture",
                "lines": [{ "words": [
                    { "text": "最初", "timing": { "start_ms": 100, "end_ms": 200 } },
                    { "text": "の語", "timing": { "start_ms": 250, "end_ms": 400 } }
                ], "breaks": [] }]
            })).unwrap();
            editor.set_word_timing(Some(Arc::new(data)), true, cx);
        });
        cx.run_until_parked();
        for selector in [
            "word-seek-0-1",
            "word-boundary-0-1",
            "assign-0",
            "choice-0-1",
            "speaker-save",
        ] {
            let bounds = cx
                .debug_bounds(selector)
                .expect("rendered integrated control");
            cx.simulate_click(bounds.center(), Default::default());
            cx.run_until_parked();
        }
        assert_eq!(
            *actions.lock().unwrap(),
            [
                WordAction::Seek {
                    position: 250,
                    lane: "mic".into()
                },
                WordAction::ToggleBreak { line: 0, word: 1 },
            ]
        );
        assert!(cx.debug_bounds("word-seek-0-1").is_some());
        assert_eq!(
            library::read(&fixture.0).unwrap().lines[0]
                .speaker
                .as_deref(),
            Some("speaker_1")
        );
    }

    #[gpui::test]
    fn rendered_buttons_save_a_row_correction_and_review_does_not_reset_it(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let (editor, cx) = cx.add_window_view(|_, cx| fixture.editor(cx));
        cx.run_until_parked();
        for selector in [
            "assign-0",
            "choice-0-1",
            "review-0",
            "review-0",
            "speaker-save",
        ] {
            let bounds = cx.debug_bounds(selector).expect("rendered speaker control");
            cx.simulate_click(bounds.center(), Default::default());
            cx.run_until_parked();
        }
        editor.update(cx, |editor, cx| {
            assert!(editor.error.is_none(), "{:?}", editor.error);
            assert!(!editor.is_dirty(cx));
            let report = editor
                .file
                .as_ref()
                .unwrap()
                .annotations
                .reconcile(&editor.raw.lines, &editor.source);
            assert!(!report.rows[0].reviewed);
            assert_eq!(report.rows[0].speaker.as_deref(), Some("speaker_1"));
        });
        assert_eq!(
            library::read(&fixture.0).unwrap().lines[0]
                .speaker
                .as_deref(),
            Some("speaker_1")
        );
        let bounds = cx.debug_bounds("next-unreviewed").unwrap();
        cx.simulate_click(bounds.center(), Default::default());
        cx.run_until_parked();
        editor.update(cx, |editor, _| assert_eq!(editor.selected, Some(1)));
    }

    #[gpui::test]
    fn editor_persists_selection_independent_of_review_and_typed_names(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let editor = cx.new(|cx| fixture.editor(cx));
        editor.update(cx, |editor, cx| {
            editor.choose_participant("speaker_1".into(), cx);
            editor
                .name_input
                .update(cx, |input, cx| input.set_text("佐藤", cx));
            assert!(editor.is_dirty(cx));
            editor.set_row(0, Some("speaker_1".into()), true, cx);
            editor.set_row(0, Some("speaker_1".into()), false, cx);
            editor.save(cx);
            assert!(editor.error.is_none());
            assert!(!editor.is_dirty(cx));
            let displayed = library::read(&fixture.0).unwrap();
            assert_eq!(displayed.lines[0].speaker.as_deref(), Some("佐藤"));
            editor.discard_and_reload(cx);
            let states = editor
                .file
                .as_ref()
                .unwrap()
                .annotations
                .reconcile(&editor.raw.lines, &editor.source);
            assert_eq!(states.rows[0].speaker.as_deref(), Some("speaker_1"));
            assert!(!states.rows[0].reviewed);
            assert!(states.unapplied.is_empty());
            editor.next_unreviewed(cx);
            assert_eq!(editor.selected, Some(0));
            editor.next_unreviewed(cx);
            assert_eq!(editor.selected, Some(1));
        });
    }

    #[gpui::test]
    fn editor_save_failure_keeps_draft_and_corrupt_sidecar_is_not_reset(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        let editor = cx.new(|cx| fixture.editor(cx));
        editor.update(cx, |editor, cx| {
            editor.set_row(0, Some("speaker_1".into()), false, cx);
            std::fs::write(fixture.0.join(annotations::FILE), "broken").unwrap();
            editor.save(cx);
            assert!(editor.error.is_some());
            assert!(editor.is_dirty(cx));
            assert_eq!(
                editor.file.as_ref().unwrap().annotations.corrections[0]
                    .speaker
                    .as_deref(),
                Some("speaker_1")
            );
            assert_eq!(
                std::fs::read_to_string(fixture.0.join(annotations::FILE)).unwrap(),
                "broken"
            );
            // After the conflicting disk change is repaired, retry the same draft.
            std::fs::remove_file(fixture.0.join(annotations::FILE)).unwrap();
            editor.save(cx);
            assert!(editor.error.is_none());
            assert!(!editor.is_dirty(cx));
            std::fs::write(fixture.0.join(annotations::FILE), "broken").unwrap();
            editor.discard_and_reload(cx);
            assert!(editor.file.is_none());
            assert!(editor.error.is_some());
            assert_eq!(editor.raw.lines.len(), 2);
            editor.save(cx);
            assert_eq!(
                std::fs::read_to_string(fixture.0.join(annotations::FILE)).unwrap(),
                "broken"
            );
        });
    }

    #[gpui::test]
    fn invalid_name_draft_does_not_block_new_evidence_or_move_it_to_a_new_speaker_namespace(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let editor = cx.new(|cx| fixture.editor(cx));
        editor.update(cx, |editor, cx| {
            editor.choose_participant("speaker_0".into(), cx);
            let old_source = editor.source.clone();
            editor
                .name_input
                .update(cx, |input, cx| input.set_text("名".repeat(101), cx));
            editor.set_row(0, Some("speaker_1".into()), true, cx);
            std::fs::create_dir_all(fixture.0.join("postprocess")).unwrap();
            std::fs::write(
                fixture.0.join(kikimimic_core::postprocess::RTTM),
                "SPEAKER mix 1 0 2 <NA> <NA> speaker_0 <NA> <NA>\n",
            )
            .unwrap();
            let transcript = std::fs::read_to_string(fixture.0.join("transcript.jsonl")).unwrap();
            std::fs::write(
                fixture.0.join(kikimimic_core::postprocess::TRANSCRIPT),
                transcript.replace("first", "new evidence"),
            )
            .unwrap();
            editor.refresh_evidence(cx);
            assert_ne!(editor.source, old_source);
            assert_eq!(editor.raw.lines[0].text, "new evidence");
            assert_eq!(editor.name_input.read(cx).text().chars().count(), 101);
            assert_eq!(editor.name_source, old_source);
            assert_eq!(
                editor.file.as_ref().unwrap().annotations.corrections.len(),
                1
            );
            editor
                .name_input
                .update(cx, |input, cx| input.set_text("旧結果の名前", cx));
            editor.save(cx);
            assert!(editor.error.is_none());
            let doc = &editor.file.as_ref().unwrap().annotations;
            assert_eq!(doc.names[0].source, old_source);
            assert_ne!(
                library::read(&fixture.0).unwrap().lines[0]
                    .speaker
                    .as_deref(),
                Some("旧結果の名前")
            );
        });
    }

    #[gpui::test]
    fn evidence_refresh_preserves_draft_in_old_namespace_for_reconfirmation(
        cx: &mut TestAppContext,
    ) {
        let fixture = Fixture::new();
        let editor = cx.new(|cx| fixture.editor(cx));
        editor.update(cx, |editor, cx| {
            editor.choose_participant("speaker_0".into(), cx);
            editor
                .name_input
                .update(cx, |input, cx| input.set_text("林", cx));
            editor.set_row(0, Some("speaker_1".into()), true, cx);
            std::fs::create_dir_all(fixture.0.join("postprocess")).unwrap();
            std::fs::write(
                fixture.0.join(kikimimic_core::postprocess::RTTM),
                "SPEAKER mix 1 0 2 <NA> <NA> speaker_0 <NA> <NA>\n",
            )
            .unwrap();
            editor.refresh_evidence(cx);
            assert!(editor.is_dirty(cx));
            let doc = &editor.file.as_ref().unwrap().annotations;
            let report = doc.reconcile(&editor.raw.lines, &editor.source);
            assert_eq!(report.unapplied.len(), 1);
            assert_eq!(report.inactive_names.len(), 1);
            assert_eq!(doc.names[0].name, "林");
            assert!(!report.rows[0].reviewed);
            editor.save(cx);
            assert!(editor.error.is_none());
            assert_ne!(
                library::read(&fixture.0).unwrap().lines[0]
                    .speaker
                    .as_deref(),
                Some("林")
            );
        });
    }

    #[test]
    fn review_navigation_wraps_skips_reviewed_and_keeps_unreviewed_selection() {
        let states = vec![
            ReviewState {
                speaker: Some("speaker_1".into()),
                reviewed: false,
                needs_reconfirmation: false,
                pending_speaker: None,
            },
            ReviewState {
                speaker: None,
                reviewed: true,
                needs_reconfirmation: false,
                pending_speaker: None,
            },
            ReviewState {
                speaker: Some("speaker_0".into()),
                reviewed: false,
                needs_reconfirmation: false,
                pending_speaker: None,
            },
        ];
        assert_eq!(next_unreviewed(&states, None), Some(0));
        assert_eq!(next_unreviewed(&states, Some(0)), Some(2));
        assert_eq!(next_unreviewed(&states, Some(2)), Some(0));
        assert_eq!(states[0].speaker.as_deref(), Some("speaker_1"));
        assert_eq!(next_unreviewed(&[], None), None);
        assert_eq!(next_unreviewed(&states[1..2], None), None);
    }
}
