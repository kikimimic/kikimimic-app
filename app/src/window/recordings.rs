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

    pub(super) fn recordings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let mut col = div()
            .id("recordings")
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .size_full()
            .overflow_y_scroll();
        if let Some((dir, _session)) = &self.open_recording {
            let labelling = match &self.state.read(cx).post_process {
                Some((d, state)) if d == dir => Some(state.clone()),
                _ => None,
            };
            let dir = dir.clone();
            let mut bar = div().flex().gap_2().items_center().child(
                button("back", ui.all_recordings(), false).on_click(cx.listener(
                    |this, _, _, cx| {
                        if this
                            .speaker_editor
                            .as_ref()
                            .is_some_and(|e| e.read(cx).is_dirty(cx))
                        {
                            if let Some(editor) = &this.speaker_editor {
                                editor.update(cx, |editor, cx| editor.warn_unsaved(cx));
                            }
                            cx.notify();
                            return;
                        }
                        this.open_recording = None;
                        this.speaker_editor = None;
                        this.error = None;
                        this.stop_playback();
                        cx.notify();
                    },
                )),
            );
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
            return col.into_any_element();
        }
        col = col.child(self.imports_view(cx));
        if self.recordings.is_empty() {
            return col
                .child(div().text_color(gray()).child(ui.no_recordings()))
                .into_any_element();
        }
        for (i, r) in self.recordings.iter().enumerate() {
            let dir = r.dir.clone();
            let title = r.title.clone().unwrap_or_else(|| r.name.clone());
            col =
                col.child(
                    div()
                        .id(("recording", i))
                        .flex()
                        .flex_col()
                        .p_2()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(0xf6f8fa)))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_size(px(12.)).text_color(gray()).child(
                            ui.recording_summary(&mm_ss(r.duration_ms), r.utterances, &r.snippet),
                        ))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let path = std::path::PathBuf::from(&dir);
                            match library::read_raw(&path) {
                                Ok(s) => {
                                    this.stop_playback();
                                    let editor = cx
                                        .new(|cx| SpeakerEditor::new(path.clone(), s.clone(), cx));
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
                                    this.speaker_editor = Some(editor);
                                    this.open_recording = Some((path, s));
                                    this.load_words(std::path::PathBuf::from(&dir), cx);
                                    this.sync_word_editor(cx);
                                    this.error = None;
                                }
                                Err(e) => this.error = Some(format!("{e:#}")),
                            }
                            cx.notify();
                        })),
                );
        }
        col.into_any_element()
    }
}
