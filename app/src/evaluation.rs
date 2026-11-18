//! Optional advanced Settings panel for local RTTM evaluation.

use std::path::PathBuf;

use gpui::{
    Context, Entity, EventEmitter, IntoElement, PathPromptOptions, Render, SharedString, Window,
    div, prelude::*, px, rgb,
};
use kikimimic_core::diarization_evaluation::{Interval, Mode, Options};
use kikimimic_core::library::{self, Recording};

use crate::evaluation_io::{self, Artifact};
use crate::widgets::text_input::TextInput;

pub struct Seek {
    pub dir: PathBuf,
    pub position_ms: u64,
}

pub struct EvaluationPanel {
    expanded: bool,
    busy: bool,
    recordings: Vec<Recording>,
    selected: Option<usize>,
    reference: Option<PathBuf>,
    hypothesis: Option<PathBuf>,
    mode: Mode,
    custom_range: bool,
    start: Entity<TextInput>,
    end: Entity<TextInput>,
    collar: Entity<TextInput>,
    skip_overlap: bool,
    result: Option<Artifact>,
    message: Option<String>,
    shown_intervals: usize,
}

fn button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px_3()
        .py_1()
        .rounded(px(6.))
        .border_1()
        .border_color(rgb(0xd0d7de))
        .cursor_pointer()
        .bg(if selected {
            rgb(0x1f2328)
        } else {
            rgb(0xffffff)
        })
        .text_color(if selected {
            rgb(0xffffff)
        } else {
            rgb(0x1f2328)
        })
        .hover(|s| s.border_color(rgb(0x8c959f)))
        .child(label.into())
}

impl EventEmitter<Seek> for EvaluationPanel {}

impl EvaluationPanel {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            expanded: false,
            busy: false,
            recordings: vec![],
            selected: None,
            reference: None,
            hypothesis: None,
            mode: Mode::ModelDisagreement,
            custom_range: false,
            start: cx.new(|cx| TextInput::new("0", cx)),
            end: cx.new(|cx| TextInput::new("録音の終了秒", cx)),
            collar: cx.new(|cx| TextInput::new("0", cx)),
            skip_overlap: false,
            result: None,
            message: None,
            shown_intervals: 50,
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let recordings = cx
                .background_executor()
                .spawn(async { library::list(&crate::paths::recordings_dir()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.recordings = recordings;
                this.selected = None;
                this.reference = None;
                this.hypothesis = None;
                this.result = None;
                this.busy = false;
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn choose(&mut self, reference: bool, cx: &mut Context<Self>) {
        if self.busy || self.selected.is_none() {
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                if reference {
                    "参照RTTMを選択"
                } else {
                    "仮説RTTMを選択"
                }
                .into(),
            ),
        });
        self.busy = true;
        cx.spawn(async move |this, cx| {
            let chosen = prompt.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match chosen {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            if reference {
                                this.reference = Some(path);
                            } else {
                                this.hypothesis = Some(path);
                            }
                            this.result = None;
                            this.message = None;
                        }
                    }
                    Ok(Ok(None)) => {}
                    _ => this.message = Some("ファイル選択に失敗しました".into()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn run(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let request = (|| -> anyhow::Result<_> {
            let recording = self
                .selected
                .and_then(|i| self.recordings.get(i))
                .ok_or_else(|| anyhow::anyhow!("録音を選んでください"))?;
            let reference = self
                .reference
                .clone()
                .ok_or_else(|| anyhow::anyhow!("参照RTTMを明示的に選んでください"))?;
            let options = (
                self.mode,
                self.custom_range,
                self.start.read(cx).text().to_owned(),
                self.end.read(cx).text().to_owned(),
                self.collar.read(cx).text().to_owned(),
                self.skip_overlap,
            );
            let dir = PathBuf::from(&recording.dir);
            let hypothesis = self
                .hypothesis
                .clone()
                .unwrap_or_else(|| dir.join("postprocess/nemotron.rttm"));
            Ok((dir, reference, hypothesis, options))
        })();
        let (dir, reference, hypothesis, options) = match request {
            Ok(r) => r,
            Err(e) => {
                self.message = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.result = None;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (mode, custom, start, end, collar, skip_overlap) = options;
                    let duration = library::audio_duration_seconds(&dir)?;
                    let options =
                        make_options(mode, duration, custom, &start, &end, &collar, skip_overlap)?;
                    evaluation_io::run(dir, reference, hypothesis, options)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(result) => {
                        this.result = Some(result);
                        this.shown_intervals = 50;
                    }
                    Err(error) => this.message = Some(format!("評価失敗: {error:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn seek(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(artifact) = &self.result
            && let Some(interval) = artifact.report.disagreements.get(index)
        {
            cx.emit(Seek {
                dir: artifact.recording_dir.clone(),
                position_ms: (interval.interval.start * 1000.).floor() as u64,
            });
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(artifact) = self.result.clone() else {
            return;
        };
        self.busy = true;
        self.message = None;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { evaluation_io::save(&artifact) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.message = Some(match result {
                    Ok(path) => format!("保存しました: {}", path.display()),
                    Err(error) => format!("保存失敗: {error:#}（結果は再保存できます）"),
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}

fn number(text: &str, default: f64) -> anyhow::Result<f64> {
    let n = if text.trim().is_empty() {
        default
    } else {
        text.trim().parse()?
    };
    anyhow::ensure!(
        n.is_finite() && n >= 0.,
        "秒数は有限の0以上の値を入力してください"
    );
    Ok(n)
}

fn make_options(
    mode: Mode,
    duration: f64,
    custom: bool,
    start: &str,
    end: &str,
    collar: &str,
    skip_overlap: bool,
) -> anyhow::Result<Options> {
    let uem = if custom {
        let interval = Interval {
            start: number(start, 0.)?,
            end: number(end, duration)?,
        };
        anyhow::ensure!(
            interval.start < interval.end && interval.end <= duration,
            "評価範囲は録音内で開始 < 終了にしてください"
        );
        vec![interval]
    } else {
        vec![]
    };
    Ok(Options {
        mode,
        recording_duration: duration,
        uem,
        collar: number(collar, 0.)?,
        skip_overlap,
    })
}

fn conditions(options: &Options) -> String {
    let range = if options.uem.is_empty() {
        format!("録音全体 0–{:.3}s", options.recording_duration)
    } else {
        options
            .uem
            .iter()
            .map(|i| format!("{:.3}–{:.3}s", i.start, i.end))
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "範囲: {range} / collar全幅 {:.3}s (±{:.3}s) / 同時発話{}",
        options.collar,
        options.collar / 2.,
        if options.skip_overlap {
            "を除外"
        } else {
            "を含む"
        }
    )
}

impl Render for EvaluationPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .border_t_1()
            .border_color(rgb(0xd0d7de))
            .pt_3()
            .child(
                button(
                    "evaluation-expand",
                    if self.expanded {
                        "▾ 詳細: 話者分離の評価"
                    } else {
                        "▸ 詳細: 話者分離の評価"
                    },
                    false,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.expanded = !this.expanded;
                    if this.expanded && this.recordings.is_empty() {
                        this.refresh(cx);
                    }
                    cx.notify();
                })),
            );
        if !self.expanded {
            return col;
        }
        col = col.child(div().text_size(px(12.)).text_color(rgb(0x6e7781)).child("RTTMのみをローカルで比較します。人手参照がない場合は精度を測定できません。音声送信・モデル取得は行いません。"))
            .child(button("evaluation-refresh", "録音一覧を更新", false).on_click(cx.listener(|this, _, _, cx| this.refresh(cx))));
        if self.recordings.is_empty() && !self.busy {
            col = col.child("録音がありません");
        }
        let mut list = div()
            .id("evaluation-recordings")
            .flex()
            .flex_col()
            .gap_1()
            .max_h(px(160.))
            .overflow_y_scroll();
        for (i, recording) in self.recordings.iter().enumerate() {
            list = list.child(
                button(
                    ("evaluation-recording", i),
                    format!(
                        "{} ({:.1}s)",
                        recording.title.as_deref().unwrap_or(&recording.name),
                        recording.duration_ms as f64 / 1000.
                    ),
                    self.selected == Some(i),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if this.busy {
                        return;
                    }
                    this.selected = Some(i);
                    this.reference = None;
                    this.hypothesis = None;
                    this.result = None;
                    this.message = None;
                    cx.notify();
                })),
            );
        }
        col = col.child(list);
        let mut modes = div().flex().gap_2().flex_wrap();
        for (id, mode, label) in [
            (
                "human",
                Mode::HumanReference,
                "人手で確認した参照に対する評価",
            ),
            ("models", Mode::ModelDisagreement, "モデル間の不一致"),
        ] {
            modes = modes.child(button(id, label, self.mode == mode).on_click(cx.listener(
                move |this, _, _, cx| {
                    if !this.busy {
                        this.mode = mode;
                        this.result = None;
                        this.message = None;
                        cx.notify();
                    }
                },
            )));
        }
        col = col.child(modes).child(self.mode.description());
        if self.mode == Mode::HumanReference {
            col =
                col.child("選択する参照RTTMが人手で確認済みであることを確認して実行してください。");
        }
        col = col
            .child(
                button("evaluation-reference", "参照RTTMを選ぶ…", false)
                    .on_click(cx.listener(|this, _, _, cx| this.choose(true, cx))),
            )
            .child(format!(
                "参照: {}",
                self.reference
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "未選択".into())
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        button("evaluation-hypothesis", "仮説RTTMを選ぶ…", false)
                            .on_click(cx.listener(|this, _, _, cx| this.choose(false, cx))),
                    )
                    .child(
                        button(
                            "evaluation-default",
                            "録音のNemotron結果を使う",
                            self.hypothesis.is_none(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.busy {
                                this.hypothesis = None;
                                this.result = None;
                                cx.notify();
                            }
                        })),
                    ),
            )
            .child(format!(
                "仮説: {}",
                self.hypothesis
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "選択した録音の postprocess/nemotron.rttm".into())
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        button("evaluation-all", "録音全体", !self.custom_range).on_click(
                            cx.listener(|this, _, _, cx| {
                                if !this.busy {
                                    this.custom_range = false;
                                    cx.notify();
                                }
                            }),
                        ),
                    )
                    .child(
                        button("evaluation-range", "開始/終了を指定", self.custom_range).on_click(
                            cx.listener(|this, _, _, cx| {
                                if !this.busy {
                                    this.custom_range = true;
                                    cx.notify();
                                }
                            }),
                        ),
                    ),
            );
        if self.custom_range {
            col = col.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child("開始秒")
                    .child(div().w(px(130.)).child(self.start.clone()))
                    .child("終了秒")
                    .child(div().w(px(160.)).child(self.end.clone())),
            );
        }
        col = col
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .flex_wrap()
                    .child("collar全幅(秒、境界の±半幅)")
                    .child(div().w(px(100.)).child(self.collar.clone()))
                    .child(
                        button(
                            "evaluation-overlap",
                            if self.skip_overlap {
                                "同時発話を除く"
                            } else {
                                "同時発話を含む"
                            },
                            self.skip_overlap,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if !this.busy {
                                this.skip_overlap = !this.skip_overlap;
                                cx.notify();
                            }
                        })),
                    ),
            )
            .child(
                button(
                    "evaluation-run",
                    if self.busy {
                        "処理中…"
                    } else {
                        "評価を実行"
                    },
                    false,
                )
                .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
            );
        if let Some(artifact) = &self.result {
            let report = &artifact.report;
            let label = if report.options.mode == Mode::HumanReference {
                "DER"
            } else {
                "モデル間不一致率（精度ではありません）"
            };
            let rate = report
                .der
                .or(report.model_disagreement_rate)
                .map(|r| format!("{:.2}%", r * 100.))
                .unwrap_or_else(|| "未定義（評価内の参照話者時間が0秒）".into());
            col = col
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(format!("{label}: {rate}")),
                )
                .child(report.interpretation.clone())
                .child(conditions(&report.options))
                .child(format!(
                    "実行時参照: {} / 仮説: {}",
                    artifact.reference_path.display(),
                    artifact.hypothesis_path.display()
                ))
                .child(format!(
                    "miss {:.3}s / false alarm {:.3}s / confusion {:.3}s",
                    report.seconds.miss, report.seconds.false_alarm, report.seconds.confusion
                ))
                .child(format!(
                    "参照話者時間 {:.3}s / 評価対象時間 {:.3}s（無音を含む）",
                    report.reference_speaker_seconds, report.evaluated_seconds
                ))
                .child(format!(
                    "話者対応（仮説 → 参照）: {}",
                    report
                        .mapping
                        .iter()
                        .map(|(h, r)| format!("{h} → {r}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
                .child(
                    button("evaluation-save", "この実行結果をJSONで保存", false)
                        .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                )
                .child(format!(
                    "聞き直す不一致区間: {}件（時刻は元の録音基準）",
                    report.disagreements.len()
                ));
            for (i, disagreement) in report
                .disagreements
                .iter()
                .take(self.shown_intervals)
                .enumerate()
            {
                col = col.child(
                    button(
                        ("evaluation-seek", i),
                        format!(
                            "{:.3}–{:.3}s  参照 [{}] / 仮説 [{}]  聞き直す",
                            disagreement.interval.start,
                            disagreement.interval.end,
                            disagreement.reference_speakers.join(", "),
                            disagreement.hypothesis_speakers.join(", ")
                        ),
                        false,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.seek(i, cx))),
                );
            }
            if report.disagreements.len() > self.shown_intervals {
                col = col.child(button("evaluation-more", "さらに50件表示", false).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.shown_intervals += 50;
                        cx.notify();
                    }),
                ));
            }
        }
        if let Some(message) = &self.message {
            col = col.child(div().text_color(rgb(0x0969da)).child(message.clone()));
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use std::{
        cell::RefCell,
        fs,
        rc::Rc,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[gpui::test]
    fn evaluates_the_exact_sample_clock_instead_of_display_milliseconds(cx: &mut TestAppContext) {
        let base = std::env::temp_dir().join(format!(
            "kkm-evaluation-fractional-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _scratch = Scratch(base.clone());
        let dir = base.join("recording");
        fs::create_dir_all(dir.join("postprocess")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(dir.join("mic.wav"), spec).unwrap();
        for _ in 0..16_001 {
            wav.write_sample(0i16).unwrap();
        }
        wav.finalize().unwrap();
        let reference = dir.join("human.rttm");
        let rttm = "SPEAKER audio 1 0 1.0000625 <NA> <NA> speaker_0 <NA> <NA>\n";
        fs::write(&reference, rttm).unwrap();
        fs::write(dir.join("postprocess/nemotron.rttm"), rttm).unwrap();
        let panel = cx.new(EvaluationPanel::new);
        panel.update(cx, |panel, cx| {
            panel.recordings = library::list(&base);
            assert_eq!(panel.recordings[0].duration_ms, 1000);
            panel.selected = Some(0);
            panel.reference = Some(reference);
            panel.mode = Mode::HumanReference;
            panel.run(cx);
        });
        cx.run_until_parked();
        panel.update(cx, |panel, _| {
            let artifact = panel.result.as_ref().expect("exact WAV end is valid RTTM");
            assert_eq!(artifact.report.der, Some(0.0));
            assert_eq!(artifact.report.options.recording_duration, 1.0000625);
        });
    }

    #[gpui::test]
    fn gpui_selects_reference_runs_saves_and_emits_recording_seek(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join(format!(
            "kkm-evaluation-panel-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _scratch = Scratch(dir.clone());
        fs::create_dir_all(dir.join("postprocess")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(dir.join("mic.wav"), spec).unwrap();
        for _ in 0..160_000 {
            wav.write_sample(0i16).unwrap();
        }
        wav.finalize().unwrap();
        let reference = dir.join("human.rttm");
        fs::write(&reference, "SPEAKER audio 1 0 5 <NA> <NA> A <NA> <NA>\n").unwrap();
        fs::write(
            dir.join("postprocess/nemotron.rttm"),
            "SPEAKER audio 1 0 3 <NA> <NA> x <NA> <NA>\n",
        )
        .unwrap();
        let panel = cx.new(EvaluationPanel::new);
        panel.update(cx, |panel, cx| {
            assert!(!panel.expanded);
            assert_eq!(panel.mode, Mode::ModelDisagreement);
            panel.recordings = vec![Recording {
                name: "fixture".into(),
                dir: dir.display().to_string(),
                title: None,
                started: None,
                duration_ms: 10_000,
                lanes: vec!["mic".into()],
                snippet: String::new(),
                utterances: 0,
            }];
            panel.selected = Some(0);
            panel.run(cx);
            assert!(panel.message.as_ref().unwrap().contains("参照RTTM"));
            panel.choose(true, cx);
        });
        assert!(cx.did_prompt_for_paths());
        cx.simulate_path_prompt_response(|options| {
            assert!(options.files && !options.directories && !options.multiple);
            Some(vec![reference.clone()])
        });
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            assert_eq!(panel.reference, Some(reference.clone()));
            panel.run(cx);
            assert!(panel.busy);
            // Repeated commands cannot race the running job.
            panel.refresh(cx);
            panel.run(cx);
        });
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            assert!(!panel.busy, "{:?}", panel.message);
            let artifact = panel.result.as_ref().expect("completed local evaluation");
            assert_eq!(artifact.report.der, None);
            assert_eq!(artifact.report.model_disagreement_rate, Some(0.4));
            assert_eq!(artifact.report.seconds.miss, 2.);
            assert!(
                artifact
                    .report
                    .interpretation
                    .contains("証明ではありません")
            );
            panel.save(cx);
        });
        cx.run_until_parked();
        let file = fs::read_dir(dir.join("postprocess/evaluations"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let saved: Artifact = serde_json::from_slice(&fs::read(file).unwrap()).unwrap();
        assert_eq!(saved.report.options.mode, Mode::ModelDisagreement);
        assert_eq!(saved.reference_path, reference);
        assert!(saved.hypothesis_path.ends_with("postprocess/nemotron.rttm"));
        let seeks = Rc::new(RefCell::new(Vec::new()));
        let received = seeks.clone();
        cx.update(|cx| {
            cx.subscribe(&panel, move |_, seek: &Seek, _| {
                received
                    .borrow_mut()
                    .push((seek.dir.clone(), seek.position_ms))
            })
            .detach()
        });
        panel.update(cx, |panel, cx| panel.seek(0, cx));
        assert_eq!(*seeks.borrow(), vec![(dir.clone(), 3000)]);
        // Human provenance is a separate execution, not a relabel of old results.
        panel.update(cx, |panel, cx| {
            panel.mode = Mode::HumanReference;
            panel.run(cx);
        });
        cx.run_until_parked();
        panel.update(cx, |panel, cx| {
            assert_eq!(panel.result.as_ref().unwrap().report.der, Some(0.4));
            fs::remove_file(&reference).unwrap();
            panel.run(cx);
        });
        cx.run_until_parked();
        panel.update(cx, |panel, _| {
            assert!(panel.result.is_none());
            assert!(panel.message.as_ref().unwrap().contains("評価失敗"));
            assert!(!panel.busy);
        });
    }
    #[test]
    fn controls_validate_ranges_and_keep_modes_and_collar_explicit() {
        let options =
            make_options(Mode::ModelDisagreement, 10., true, "2", "7", "0.25", true).unwrap();
        assert_eq!(options.uem, vec![Interval { start: 2., end: 7. }]);
        assert_eq!(options.mode, Mode::ModelDisagreement);
        assert_eq!(options.collar, 0.25);
        assert!(conditions(&options).contains("±0.125"));
        assert!(conditions(&options).contains("同時発話を除外"));
        for (start, end, collar) in [
            ("7", "2", "0"),
            ("0", "11", "0"),
            ("NaN", "9", "0"),
            ("0", "9", "-1"),
        ] {
            assert!(
                make_options(Mode::HumanReference, 10., true, start, end, collar, false).is_err()
            );
        }
        assert!(
            make_options(Mode::HumanReference, 10., false, "bad", "bad", "", false)
                .unwrap()
                .uem
                .is_empty()
        );
    }
}
