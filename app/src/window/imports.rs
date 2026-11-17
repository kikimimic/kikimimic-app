//! Recording-list import controls. Native picker, ordered join draft and the
//! durable core queue stay separate from live-session transcript state.
use super::*;
use kikimimic_core::imports::{ImportMode, ImportState};

fn text(ui: Ui, en: &'static str, ja: &'static str) -> &'static str {
    if ui == Ui::Ja { ja } else { en }
}

impl MainWindow {
    fn pick_import(&mut self, join: bool, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some(text(self.ui, "Select audio files", "音声ファイルを選択").into()),
        });
        cx.spawn(async move |this, cx| {
            let result = receiver.await;
            let _ = this.update(cx, |w, cx| {
                match result {
                    Ok(Ok(Some(paths))) if !paths.is_empty() => {
                        if join {
                            w.import_draft = paths;
                        } else {
                            w.kkm.import_audio(
                                paths,
                                ImportMode::Separate,
                                Box::new(crate::audio_import::NativeDecoder),
                            );
                        }
                    }
                    Ok(Err(e)) => w.error = Some(format!("{e:#}")),
                    _ => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn imports_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let queue = &self.state.read(cx).imports;
        let settings = self.kkm.settings();
        let mut panel = div().flex().flex_col().gap_2().pb_3().border_b_1().border_color(rgb(0xd0d7de))
            .child(div().flex().gap_2()
                .child(button("import-audio", text(ui, "Import WAV / M4A / MP3", "WAV / M4A / MP3を取り込む"), false)
                    .on_click(cx.listener(|w, _, _, cx| w.pick_import(false, cx))))
                .child(button("join-wav", text(ui, "Join WAVs into one meeting", "WAVを1会議に結合"), false)
                    .on_click(cx.listener(|w, _, _, cx| w.pick_import(true, cx)))))
            .child(div().text_size(px(12.)).text_color(gray()).child(format!("ASR: {} · MT: {} · {:?} → {}", settings.asr_model, settings.mt_model.as_deref().unwrap_or("—"), settings.languages.spoken, settings.languages.target.as_ref().map_or("—", |l| l.as_str()))))
            .child(div().text_size(px(12.)).text_color(gray()).child(text(ui,
                "Settings are saved per job. Stop finishes the current file (or joined group); resume starts interrupted files from the beginning.",
                "設定は投入時に保存します。停止は処理中のファイル（結合時は1会議）の完了後。中断したファイルは先頭から再処理します。")));
        if !self.import_draft.is_empty() {
            let mut draft = div().flex().flex_col().gap_2().p_2().border_1().rounded(px(6.)).border_color(rgb(0xd0d7de))
                .child(text(ui, "Join in this order. WAV formats must match. No gap recovery or multi-device synchronization.", "この順序で結合します。WAV形式は一致が必要です。実時間の隙間復元・複数機器同期は行いません。"));
            for (i, path) in self.import_draft.iter().enumerate() {
                let mut row = div().flex().gap_2().items_center().child(format!(
                    "{}. {}",
                    i + 1,
                    path.display()
                ));
                if i > 0 {
                    row = row.child(button(("import-up", i), "↑", false).on_click(cx.listener(
                        move |w, _, _, cx| {
                            if i < w.import_draft.len() {
                                w.import_draft.swap(i, i - 1);
                            }
                            cx.notify();
                        },
                    )));
                }
                if i + 1 < self.import_draft.len() {
                    row = row.child(button(("import-down", i), "↓", false).on_click(cx.listener(
                        move |w, _, _, cx| {
                            if i + 1 < w.import_draft.len() {
                                w.import_draft.swap(i, i + 1);
                            }
                            cx.notify();
                        },
                    )));
                }
                row = row.child(
                    button(("import-remove", i), text(ui, "Remove", "除く"), false).on_click(
                        cx.listener(move |w, _, _, cx| {
                            if i < w.import_draft.len() {
                                w.import_draft.remove(i);
                            }
                            cx.notify();
                        }),
                    ),
                );
                draft = draft.child(row);
            }
            draft = draft.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button(
                            "import-join-start",
                            text(ui, "Queue joined meeting", "結合してキューに追加"),
                            false,
                        )
                        .on_click(cx.listener(|w, _, _, cx| {
                            let inputs = std::mem::take(&mut w.import_draft);
                            w.kkm.import_audio(
                                inputs,
                                ImportMode::JoinWav,
                                Box::new(crate::audio_import::NativeDecoder),
                            );
                            cx.notify();
                        })),
                    )
                    .child(
                        button(
                            "import-join-cancel",
                            text(ui, "Cancel", "キャンセル"),
                            false,
                        )
                        .on_click(cx.listener(|w, _, _, cx| {
                            w.import_draft.clear();
                            cx.notify();
                        })),
                    ),
            );
            panel = panel.child(draft);
        }
        if let Some(error) = &queue.error {
            panel = panel.child(div().text_color(rgb(0xcf222e)).child(error.clone()));
        }
        if !queue.jobs.is_empty() {
            let mut controls = div().flex().gap_2().items_center().child(text(
                ui,
                "Import queue",
                "取り込みキュー",
            ));
            if queue.running {
                controls = controls.child(
                    button(
                        "import-stop",
                        text(ui, "Stop after current file", "現在のファイル完了後に停止"),
                        false,
                    )
                    .on_click(cx.listener(|w, _, _, _| w.kkm.stop_imports())),
                );
            } else {
                if queue.jobs.iter().any(|j| j.state == ImportState::Pending) {
                    controls = controls.child(
                        button(
                            "import-resume",
                            text(ui, "Resume pending files", "待機分を再開"),
                            false,
                        )
                        .on_click(cx.listener(|w, _, _, _| {
                            w.kkm
                                .resume_imports(false, Box::new(crate::audio_import::NativeDecoder))
                        })),
                    );
                }
                if queue
                    .jobs
                    .iter()
                    .any(|j| matches!(j.state, ImportState::Failed { .. }))
                {
                    controls = controls.child(
                        button(
                            "import-retry",
                            text(ui, "Retry failures", "失敗分を再試行"),
                            false,
                        )
                        .on_click(cx.listener(|w, _, _, _| {
                            w.kkm
                                .resume_imports(true, Box::new(crate::audio_import::NativeDecoder))
                        })),
                    );
                }
            }
            panel = panel.child(controls);
            for job in &queue.jobs {
                let (label, error) = match &job.state {
                    ImportState::Pending => (text(ui, "Waiting", "待機"), None),
                    ImportState::Running => (text(ui, "Running", "実行中"), None),
                    ImportState::Done => (text(ui, "Done", "完了"), None),
                    ImportState::Skipped => (
                        text(
                            ui,
                            "Skipped: completed with matching input/settings",
                            "スキップ：同じ入力・設定で完了済み",
                        ),
                        None,
                    ),
                    ImportState::Failed { message } => (text(ui, "Failed", "失敗"), Some(message)),
                };
                let names = job
                    .inputs
                    .iter()
                    .map(|p| p.file_name().unwrap_or_default().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join(" → ");
                let mut row = div().flex().flex_col().text_size(px(12.)).child(format!(
                    "#{:} · {} · {} · ASR: {} · MT: {}",
                    job.id,
                    label,
                    names,
                    job.settings.asr_model,
                    job.settings.mt_model.as_deref().unwrap_or("—")
                ));
                if let Some(error) = error {
                    row = row.child(div().text_color(rgb(0xcf222e)).child(error.clone()));
                }
                panel = panel.child(row);
            }
        }
        panel.into_any_element()
    }
}
