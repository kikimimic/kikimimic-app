//! Recordings: the list, one recording's detail, and playback.

use super::*;

impl MainWindow {
    pub(super) fn play_recording(&mut self, position: u64, cx: &mut Context<Self>) {
        self.play_word(position, "mix", cx);
    }

    pub(super) fn stop_playback(&mut self) {
        if let Some(mut child) = self.playback.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(path) = self.playback_status.take() {
            let _ = std::fs::remove_file(path);
        }
        self.words.playback = None;
    }

    pub(super) fn play_word(&mut self, position: u64, lane: &str, cx: &mut Context<Self>) {
        if let Some((dir, _)) = &self.open_recording {
            self.play_audio(&dir.clone(), position, lane, cx);
        }
    }

    pub(super) fn play_from(
        &mut self,
        dir: &std::path::Path,
        position: u64,
        cx: &mut Context<Self>,
    ) {
        self.play_audio(dir, position, "mix", cx);
    }

    pub(super) fn play_audio(
        &mut self,
        dir: &std::path::Path,
        position: u64,
        lane: &str,
        cx: &mut Context<Self>,
    ) {
        if self.state.read(cx).is_listening() {
            self.error = Some("Stop recording before playback".into());
            cx.notify();
            return;
        }
        self.stop_playback();
        self.sync_word_editor(cx);
        {
            let script =
                std::env::temp_dir().join(format!("kikimimic-player-{}.swift", std::process::id()));
            let status = std::env::temp_dir().join(format!(
                "kikimimic-player-{}-{}.json",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            let result =
                std::fs::write(&script, include_str!("../playback.swift")).and_then(|_| {
                    std::process::Command::new("/usr/bin/swift")
                        .arg(script)
                        .arg(dir)
                        .arg((position as f64 / 1000.0).to_string())
                        .arg(&status)
                        .arg("autoplay")
                        .arg(lane)
                        .spawn()
                });
            match result {
                Ok(child) => {
                    self.playback = Some(child);
                    self.playback_status = Some(status.clone());
                    self.observe_playback(status, dir.to_path_buf(), cx);
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        cx.notify();
    }

    /// Open a recording in the main column. An editor with unsaved changes
    /// keeps the recording open and says so instead of losing them.
    pub(super) fn open_recording_dir(&mut self, path: std::path::PathBuf, cx: &mut Context<Self>) {
        self.view = View::Recording;
        if self
            .open_recording
            .as_ref()
            .is_some_and(|(open, _)| open == &path)
        {
            cx.notify();
            return;
        }
        if let Some(editor) = &self.speaker_editor
            && editor.read(cx).is_dirty(cx)
        {
            editor.update(cx, |editor, cx| editor.warn_unsaved(cx));
            cx.notify();
            return;
        }
        match library::read_raw(&path) {
            Ok(s) => {
                self.stop_playback();
                let editor = cx.new(|cx| SpeakerEditor::new(path.clone(), s.clone(), cx));
                cx.subscribe(&editor, |this, _, event: &Seek, cx| {
                    this.play_recording(event.0, cx)
                })
                .detach();
                cx.subscribe(
                    &editor,
                    |this, _, event: &words::WordAction, cx| match event {
                        words::WordAction::Seek { position, lane } => {
                            this.play_word(*position, lane, cx)
                        }
                        words::WordAction::ToggleBreak { line, word } => {
                            this.toggle_word_break(*line, *word, cx)
                        }
                    },
                )
                .detach();
                cx.observe(&editor, |_, _, cx| cx.notify()).detach();
                self.speaker_editor = Some(editor);
                self.open_recording = Some((path.clone(), s));
                self.load_words(path, cx);
                self.sync_word_editor(cx);
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        cx.notify();
    }

    pub(super) fn recordings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let mut col = super::shell::column();
        let Some((dir, _session)) = &self.open_recording else {
            return div()
                .size_full()
                .child(col.child(div().pt_6().text_color(gray()).child(ui.pick_a_recording())))
                .into_any_element();
        };
        let labelling = match &self.state.read(cx).post_process {
            Some((d, state)) if d == dir => Some(state.clone()),
            _ => None,
        };
        let dir = dir.clone();
        let mut bar = div().flex().gap_2().items_center();
        for (id, label, extension) in [
            ("export-md", "文字起こし .md", "md"),
            ("export-srt", "字幕 .srt", "srt"),
        ] {
            let path = dir.clone();
            bar = bar.child(button(id, label, false).on_click(cx.listener(
                move |this, _, _, cx| {
                    let result = library::read(&path).and_then(|session| {
                        let text = if extension == "md" {
                            kikimimic_core::export::markdown(&session)
                        } else {
                            kikimimic_core::export::srt(&session)
                        };
                        std::fs::write(path.join(format!("transcript.{extension}")), text)
                            .map_err(Into::into)
                    });
                    this.error = result.err().map(|e| e.to_string());
                    if this.error.is_none() {
                        let _ = std::process::Command::new("open").arg(&path).spawn();
                    }
                    cx.notify();
                },
            )));
        }
        bar = match labelling {
            Some(PostProcessState::Running) => {
                bar.child(div().text_color(gray()).child(ui.identifying()))
            }
            other => {
                let diarize_dir = dir.clone();
                if let Some(PostProcessState::Failed { message }) = other {
                    bar = bar.child(div().text_color(rgb(0xcf222e)).child(message));
                }
                bar.child(button("diarize", ui.identify_speakers(), false).on_click(
                    cx.listener(move |this, _, _, _| this.kkm.diarize(diarize_dir.clone())),
                ))
            }
        };
        bar = bar.child(
            button("play-audio", "Play audio", false)
                .on_click(cx.listener(|this, _, _, cx| this.play_recording(0, cx))),
        );
        col = col.child(bar).child(self.word_toolbar(dir.clone(), cx));
        if let Some(editor) = &self.speaker_editor {
            col = col.child(editor.clone());
        }
        div()
            .id("recording")
            .size_full()
            .overflow_y_scroll()
            .child(col)
            .into_any_element()
    }
}
