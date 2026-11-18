//! Settings: one screen, a row of sections, every change saved at once.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Section {
    #[default]
    General,
    Glossary,
    Export,
    Link,
    Models,
    Advanced,
}

impl MainWindow {
    pub(super) fn models_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let store = self.kkm.models().clone();
        let settings = self.kkm.settings();
        let mut col = div().flex().flex_col().gap_3();
        for (i, m) in store.catalog().models().iter().enumerate() {
            if m.kind == ModelKind::Vad || !crate::engines::runs_here(m) {
                continue;
            }
            let id = m.id.clone();
            let size: u64 = m.files.iter().map(|f| f.bytes).sum();
            let state = store.state(&m.id).unwrap_or(ModelState::Missing {
                partial: 0,
                total: size,
            });
            let in_use =
                settings.asr_model == m.id || settings.mt_model.as_deref() == Some(m.id.as_str());
            let mut row = div().flex().gap_3().items_center().child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                        "{}{}",
                        m.title,
                        if in_use { ui.in_use() } else { "" }
                    )))
                    .child(div().text_size(px(12.)).text_color(gray()).child(
                        if m.files.is_empty() {
                            format!("{:?} · {}", m.kind, ui.built_into_macos())
                        } else {
                            format!(
                                "{:?} · {:.1} GB · {}",
                                m.kind,
                                size as f64 / 1e9,
                                m.license.name
                            )
                        },
                    )),
            );
            let progress = self.downloads.get(&m.id);
            row =
                match (state, progress) {
                    (_, Some(Progress::Running { received, total })) => {
                        row.child(ui.downloading(if total == 0 {
                            0.0
                        } else {
                            received as f64 * 100.0 / total as f64
                        }))
                    }
                    (ModelState::Ready, _) => {
                        row.child(div().text_color(rgb(0x1a7f37)).child(ui.ready()))
                    }
                    (ModelState::NeedsLicense, _) => {
                        let url = m.license.url.clone().unwrap_or_default();
                        row.child(div().text_size(px(12.)).text_color(gray()).child(url))
                            .child(button(("accept", i), ui.accept_license(), false).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.error = this.kkm.models().accept_license(&id).err();
                                    cx.notify();
                                }),
                            ))
                    }
                    (ModelState::Missing { .. }, failed) => {
                        if let Some(Progress::Failed(e)) = failed {
                            row = row.child(div().text_color(rgb(0xcf222e)).child(e));
                        }
                        row.child(button(("download", i), ui.download(), false).on_click(
                            cx.listener(move |this, _, _, cx| {
                                if this.downloads.start(this.kkm.models().clone(), id.clone()) {
                                    this.watch_downloads(cx);
                                }
                                cx.notify();
                            }),
                        ))
                    }
                };
            col = col.child(row);
        }
        col.into_any_element()
    }

    pub(super) fn settings_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let mut tabs = div().flex().gap_1();
        for (section, id, label) in [
            (Section::General, "set-general", ui.section_general()),
            (Section::Glossary, "set-glossary", ui.glossary()),
            (Section::Export, "set-export", ui.step_export()),
            (Section::Link, "set-link", ui.section_link()),
            (Section::Models, "set-models", ui.tab_models()),
            (Section::Advanced, "set-advanced", ui.section_advanced()),
        ] {
            tabs = tabs.child(
                button(id, label, self.section == section).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.section = section;
                        cx.notify();
                    },
                )),
            );
        }
        let body = match self.section {
            Section::General => self.general_section(cx).into_any_element(),
            Section::Glossary => self.glossary_section(cx).into_any_element(),
            Section::Export => self.export_section(cx).into_any_element(),
            Section::Link => div()
                .flex()
                .flex_col()
                .gap_6()
                .child(self.agent_section(cx))
                .child(self.calendar_section(cx))
                .into_any_element(),
            Section::Models => self.models_view(cx),
            Section::Advanced => self.evaluation.clone().into_any_element(),
        };
        div()
            .id("settings")
            .size_full()
            .overflow_y_scroll()
            .child(
                super::shell::column()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .pb_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(ui.settings_button()),
                            )
                            .child(tabs)
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .text_color(gray())
                                    .child(ui.saved_automatically()),
                            ),
                    )
                    .child(body),
            )
            .into_any_element()
    }

    fn general_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        let ui = self.ui;
        let settings = self.kkm.settings();
        let policy = settings.languages.clone();
        let spoken: Vec<String> = policy
            .spoken
            .iter()
            .map(|l| l.as_str().to_string())
            .collect();
        let mut col = div().flex().flex_col().gap_3();

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.ui_language()),
        );
        let mut row = div().flex().gap_2();
        for (id, code, label) in [
            ("ui-lang-os", None, ui.same_as_os()),
            ("ui-lang-en", Some(Ui::En), "English"),
            ("ui-lang-ja", Some(Ui::Ja), "日本語"),
        ] {
            let code = code.map(|u| u.code().to_string());
            let on = settings.ui_language == code;
            row = row.child(
                button(id, label, on)
                    .debug_selector(move || id.into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let code = code.clone();
                        this.update_settings(|s| s.ui_language = code, cx)
                    })),
            );
        }
        col = col.child(row);

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.spoken_languages()),
        );
        let mut row = div().flex().gap_2().flex_wrap();
        for (code, _, name) in kikimimic_core::language::NAMES {
            let on = spoken.iter().any(|s| s == code);
            let code = *code;
            row = row.child(
                button(SharedString::from(format!("spoken-{code}")), *name, on).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.update_settings(|s| toggle_spoken(&mut s.languages, code), cx)
                    }),
                ),
            );
        }
        col = col.child(row);

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.translate_into()),
        );
        let mut row = div().flex().gap_2().flex_wrap().child(
            button("target-none", ui.dont_translate(), policy.target.is_none()).on_click(
                cx.listener(|this, _, _, cx| {
                    this.update_settings(|s| s.languages.target = None, cx)
                }),
            ),
        );
        for (code, _, name) in kikimimic_core::language::NAMES {
            let on = policy.target.as_ref().is_some_and(|t| t.as_str() == *code);
            let code = *code;
            row = row.child(
                button(SharedString::from(format!("target-{code}")), *name, on).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.update_settings(|s| s.languages.target = Lang::parse(code).ok(), cx)
                    }),
                ),
            );
        }
        col = col
            .child(row)
            .child(
                button("mutual", ui.mutual(), policy.mutual).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.update_settings(|s| s.languages.mutual = !s.languages.mutual, cx)
                    },
                )),
            );

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.recognition_model()),
        );
        let catalog = self.kkm.models().catalog();
        let mut row = div().flex().gap_2().flex_wrap();
        for (i, m) in catalog.models().iter().enumerate() {
            if m.kind != ModelKind::Asr || !crate::engines::runs_here(m) {
                continue;
            }
            let id = m.id.clone();
            let on = settings.asr_model == m.id;
            row = row.child(
                button(("asr", i), m.title.clone(), on).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let id = id.clone();
                        this.update_settings(|s| s.asr_model = id, cx)
                    },
                )),
            );
        }
        col = col.child(row);
        if let Some(asr) = catalog.get(&settings.asr_model) {
            let unheard: Vec<&str> = asr
                .unheard(&policy.spoken)
                .map(|l| {
                    kikimimic_core::language::NAMES
                        .iter()
                        .find(|(code, ..)| *code == l.as_str())
                        .map_or(l.as_str(), |(_, _, name)| *name)
                })
                .collect();
            if !unheard.is_empty() {
                col = col.child(div().text_color(rgb(0x9a6700)).child(format!(
                    "{}{}",
                    ui.unheard_languages(),
                    unheard.join(", ")
                )));
            }
        }

        col = col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.translation_model()),
        );
        let mut row = div().flex().gap_2().flex_wrap().child(
            button("mt-none", ui.none(), settings.mt_model.is_none()).on_click(
                cx.listener(|this, _, _, cx| this.update_settings(|s| s.mt_model = None, cx)),
            ),
        );
        for (i, m) in self.kkm.models().catalog().models().iter().enumerate() {
            if m.kind != ModelKind::Mt {
                continue;
            }
            let id = m.id.clone();
            let on = settings.mt_model.as_deref() == Some(m.id.as_str());
            row = row.child(button(("mt", i), m.title.clone(), on).on_click(cx.listener(
                move |this, _, _, cx| {
                    let id = id.clone();
                    this.update_settings(|s| s.mt_model = Some(id), cx)
                },
            )));
        }
        col = col.child(row);

        col.child(
            div()
                .font_weight(FontWeight::SEMIBOLD)
                .child(ui.section_recording()),
        )
        .child(
            button(
                "clear-on-start",
                ui.clear_on_start(),
                settings.clear_on_start,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_settings(|s| s.clear_on_start = !s.clear_on_start, cx)
            })),
        )
        .child(
            button("auto-record", ui.auto_record_on(), settings.auto_record).on_click(cx.listener(
                |this, _, _, cx| this.update_settings(|s| s.auto_record = !s.auto_record, cx),
            )),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(gray())
                .child(ui.auto_record_help()),
        )
        .child(
            button(
                "save-face",
                ui.save_face_to_face(),
                settings.save_face_to_face,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.update_settings(|s| s.save_face_to_face = !s.save_face_to_face, cx)
            })),
        )
    }

    /// Put the selected destination's text into the editing fields.
    pub(super) fn load_destination_fields(&mut self, cx: &mut Context<Self>) {
        let dests = self.kkm.settings().destinations;
        let Some(d) = dests.get(self.dest_selected) else {
            return;
        };
        let (name, folder, file) = (d.name.clone(), d.folder.clone(), d.file_name.clone());
        self.dest_name.update(cx, |i, cx| i.set_text(name, cx));
        self.dest_folder.update(cx, |i, cx| i.set_text(folder, cx));
        self.dest_file.update(cx, |i, cx| i.set_text(file, cx));
    }

    fn update_destination(
        &mut self,
        change: impl FnOnce(&mut kikimimic_core::destinations::Destination),
        cx: &mut Context<Self>,
    ) {
        let i = self.dest_selected;
        self.update_settings(
            |s| {
                if let Some(d) = s.destinations.get_mut(i) {
                    change(d);
                }
            },
            cx,
        );
        self.refresh_recordings(cx);
    }

    fn export_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        use kikimimic_core::destinations::{Destination, Format, VARIABLES, When};
        let ui = self.ui;
        let dests = self.kkm.settings().destinations;
        let mut col = div().flex().flex_col().gap_3().child(
            div()
                .text_size(px(12.))
                .text_color(gray())
                .child(ui.destinations_help()),
        );
        let mut list = div().flex().flex_wrap().gap_1();
        for (i, d) in dests.iter().enumerate() {
            let label = if d.enabled {
                d.name.clone()
            } else {
                format!("{}（{}）", d.name, ui.off())
            };
            list = list.child(
                button(("dest-pick", i), label, self.dest_selected == i).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.dest_selected = i;
                        this.load_destination_fields(cx);
                        cx.notify();
                    },
                )),
            );
        }
        list = list.child(button("dest-add", ui.add(), false).on_click(cx.listener(
            |this, _, _, cx| {
                this.update_settings(
                    |s| {
                        let n = s.destinations.len() + 1;
                        s.destinations.push(Destination {
                            id: format!("dest-{}", chrono::Local::now().format("%Y%m%d%H%M%S")),
                            name: format!("書き出し先 {n}"),
                            folder: "~/Documents/Kikimimic".into(),
                            file_name: "{{date}} {{title}}.md".into(),
                            format: Format::Template,
                            when: When::Manual,
                            enabled: true,
                            template: kikimimic_core::destinations::DEFAULT_TEMPLATE.into(),
                            template_file: None,
                        })
                    },
                    cx,
                );
                this.dest_selected = this.kkm.settings().destinations.len().saturating_sub(1);
                this.load_destination_fields(cx);
            },
        )));
        col = col.child(list);
        let Some(d) = dests.get(self.dest_selected) else {
            return col;
        };
        let field = |label: &'static str, input: Entity<TextInput>| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().w(px(120.)).flex_none().child(label))
                .child(div().flex_1().child(input))
        };
        let choice =
            |row: gpui::Div,
             id: &'static str,
             n: usize,
             label: &'static str,
             on: bool,
             f: Box<dyn Fn(&mut Destination)>| {
                row.child(button((id, n), label, on).on_click(
                    cx.listener(move |this, _, _, cx| this.update_destination(|d| f(d), cx)),
                ))
            };
        let mut formats = div().flex().gap_1();
        for (n, (format, label)) in [
            (Format::Template, ui.format_template()),
            (Format::Srt, "SRT"),
            (Format::Json, "JSON"),
        ]
        .into_iter()
        .enumerate()
        {
            let on = d.format == format;
            formats = choice(
                formats,
                "dest-format",
                n,
                label,
                on,
                Box::new(move |d| d.format = format.clone()),
            );
        }
        let mut whens = div().flex().gap_1();
        for (n, (when, label)) in [
            (When::Manual, ui.when_manual()),
            (When::AfterMinutes, ui.when_minutes()),
            (When::AfterRecording, ui.when_recording()),
        ]
        .into_iter()
        .enumerate()
        {
            let on = d.when == when;
            whens = choice(
                whens,
                "dest-when",
                n,
                label,
                on,
                Box::new(move |d| d.when = when.clone()),
            );
        }
        let row = |label: &'static str, content: gpui::Div| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(div().w(px(120.)).flex_none().child(label))
                .child(content)
        };
        let mut editor = div()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(px(6.))
            .border_1()
            .border_color(rgb(0xd0d7de))
            .child(field(ui.dest_name(), self.dest_name.clone()))
            .child(field(ui.dest_folder(), self.dest_folder.clone()).child(
                button("dest-folder-pick", ui.choose(), false).on_click(cx.listener(
                    |_, _, _, cx| {
                        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
                            files: false,
                            directories: true,
                            multiple: false,
                            prompt: None,
                        });
                        cx.spawn(async move |this, cx| {
                            if let Ok(Ok(Some(paths))) = receiver.await
                                && let Some(p) = paths.into_iter().next()
                            {
                                let _ = this.update(cx, |this, cx| {
                                    let folder = p.display().to_string();
                                    this.dest_folder.update(cx, |i, cx| i.set_text(folder, cx));
                                });
                            }
                        })
                        .detach();
                    },
                )),
            ))
            .child(field(ui.dest_file_name(), self.dest_file.clone()))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        button("dest-delete", ui.remove(), false).on_click(cx.listener(
                            |this, _, _, cx| {
                                let i = this.dest_selected;
                                this.update_settings(
                                    |s| {
                                        if i < s.destinations.len() {
                                            s.destinations.remove(i);
                                        }
                                    },
                                    cx,
                                );
                                this.dest_selected = 0;
                                this.load_destination_fields(cx);
                                this.refresh_recordings(cx);
                            },
                        )),
                    )
                    .child(button("dest-apply", ui.apply(), true).on_click(cx.listener(
                        |this, _, _, cx| {
                            let name = this.dest_name.read(cx).text().trim().to_string();
                            let folder = this.dest_folder.read(cx).text().trim().to_string();
                            let file = this.dest_file.read(cx).text().trim().to_string();
                            if name.is_empty() || folder.is_empty() || file.is_empty() {
                                return;
                            }
                            this.update_destination(
                                |d| {
                                    d.name = name;
                                    d.folder = folder;
                                    d.file_name = file;
                                },
                                cx,
                            );
                        },
                    ))),
            )
            .child(row(ui.dest_format(), formats))
            .child(row(ui.dest_when(), whens))
            .child(row(
                ui.dest_enabled(),
                div().child(
                    button(
                        "dest-enabled",
                        if d.enabled { ui.on() } else { ui.off() },
                        d.enabled,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.update_destination(|d| d.enabled = !d.enabled, cx)
                    })),
                ),
            ));
        if d.format == Format::Template {
            let source = match &d.template_file {
                Some(f) => f.display().to_string(),
                None => ui.template_inline().to_string(),
            };
            editor = editor.child(row(
                ui.dest_template(),
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_size(px(12.)).child(source))
                    .child(
                        button("tpl-file", ui.choose_file(), false).on_click(cx.listener(
                            |_, _, _, cx| {
                                let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
                                    files: true,
                                    directories: false,
                                    multiple: false,
                                    prompt: None,
                                });
                                cx.spawn(async move |this, cx| {
                                    if let Ok(Ok(Some(paths))) = receiver.await
                                        && let Some(p) = paths.into_iter().next()
                                    {
                                        let _ = this.update(cx, |this, cx| {
                                            this.update_destination(
                                                |d| d.template_file = Some(p),
                                                cx,
                                            )
                                        });
                                    }
                                })
                                .detach();
                            },
                        )),
                    )
                    .when(d.template_file.is_some(), |r| {
                        r.child(
                            button("tpl-inline", ui.template_use_inline(), false).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.update_destination(|d| d.template_file = None, cx)
                                }),
                            ),
                        )
                    }),
            ));
            editor = editor.child(div().text_size(px(12.)).text_color(gray()).child(format!(
                    "{} {}",
                    ui.template_variables(),
                    VARIABLES
                        .iter()
                        .map(|v| format!("{{{{{v}}}}}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )));
        }
        col.child(editor)
    }

    fn agent_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        use kikimimic_core::agent::Agent;
        let ui = self.ui;
        let chosen = self.kkm.settings().agent;
        let mut choices =
            div()
                .flex()
                .gap_1()
                .child(button("agent-none", ui.none(), chosen.is_none()).on_click(
                    cx.listener(|this, _, _, cx| this.update_settings(|s| s.agent = None, cx)),
                ));
        for (i, agent) in Agent::ALL.into_iter().enumerate() {
            choices = choices.child(
                button(("agent", i), agent.name(), chosen == Some(agent)).on_click(cx.listener(
                    move |this, _, _, cx| this.update_settings(|s| s.agent = Some(agent), cx),
                )),
            );
        }
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "/Applications/Kikimimic.app/Contents/MacOS/kikimimic".into());
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(ui.agents()))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(gray())
                    .child(ui.agents_help()),
            )
            .child(choices)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .pt_2()
                    .child(ui.mcp_title()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(gray())
                    .child(ui.mcp_help()),
            )
            .child(
                div()
                    .p_2()
                    .rounded(px(6.))
                    .bg(rgb(0xf6f8fa))
                    .text_size(px(12.))
                    .font_family("Menlo")
                    .child(format!("claude mcp add kikimimic -- \"{exe}\" mcp"))
                    .child(format!("codex mcp add kikimimic -- \"{exe}\" mcp")),
            )
    }

    fn glossary_section(&self, cx: &mut Context<Self>) -> gpui::Div {
        let ui = self.ui;
        let settings = self.kkm.settings();
        let mut col = div().flex().flex_col().gap_3();
        col = col
            .child(div().font_weight(FontWeight::SEMIBOLD).child(ui.glossary()))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(gray())
                    .child(ui.glossary_help()),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(div().w(px(220.)).child(self.wrong.clone()))
                    .child("→")
                    .child(div().w(px(220.)).child(self.right.clone()))
                    .child(button("add", ui.add(), false).on_click(cx.listener(
                        |this, _, _, cx| {
                            let wrong = this.wrong.read(cx).text().trim().to_string();
                            let right = this.right.read(cx).text().trim().to_string();
                            if wrong.is_empty() || right.is_empty() {
                                return;
                            }
                            this.update_settings(
                                |s| s.glossary.entries.push(GlossaryEntry { wrong, right }),
                                cx,
                            );
                            this.wrong.update(cx, |i, cx| i.clear(cx));
                            this.right.update(cx, |i, cx| i.clear(cx));
                        },
                    ))),
            );
        for (i, e) in settings.glossary.entries.iter().enumerate() {
            col = col.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(format!("{} → {}", e.wrong, e.right))
                    .child(
                        button(("remove", i), ui.remove(), false).on_click(cx.listener(
                            move |this, _, _, cx| {
                                this.update_settings(
                                    |s| {
                                        if i < s.glossary.entries.len() {
                                            s.glossary.entries.remove(i);
                                        }
                                    },
                                    cx,
                                )
                            },
                        )),
                    ),
            );
        }
        col
    }

    pub(super) fn calendar_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = self.ui;
        let calendar = self.calendar.read(cx);
        let mut col = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(ui.google_calendar()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(gray())
                    .child(ui.calendar_description()),
            );
        let sign_in = |label: &'static str| {
            button("sign-in", label, false).on_click(
                cx.listener(|this, _, _, cx| this.calendar.update(cx, |c, cx| c.sign_in(cx))),
            )
        };
        let status = match &calendar.status {
            Status::Unavailable => div().text_color(gray()).child(ui.calendar_unavailable()),
            Status::SignedOut => div().flex().child(sign_in(ui.sign_in_google())),
            Status::NeedsSignIn { .. } => div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().text_color(rgb(0x9a6700)).child(ui.needs_sign_in()))
                .child(sign_in(ui.sign_in_google())),
            Status::SigningIn => div()
                .flex()
                .gap_2()
                .items_center()
                .child(ui.signing_in())
                .child(
                    button("cancel-sign-in", ui.cancel(), false).on_click(cx.listener(
                        |this, _, _, cx| this.calendar.update(cx, |c, cx| c.cancel_sign_in(cx)),
                    )),
                ),
            Status::SignedIn { email } => {
                let mut row = div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(ui.signed_in_as(email))
                    .child(
                        button("sign-out", ui.sign_out(), false).on_click(cx.listener(
                            |this, _, _, cx| this.calendar.update(cx, |c, cx| c.sign_out(cx)),
                        )),
                    );
                if let Some(at) = calendar.last_read {
                    row = row.child(
                        div()
                            .text_color(gray())
                            .child(ui.last_read(&at.format("%H:%M").to_string())),
                    );
                }
                row
            }
        };
        col = col.child(status);
        if let Some(error) = &calendar.last_error {
            let text = ui.google_error(error);
            if !text.is_empty() {
                col = col.child(div().text_color(rgb(0xcf222e)).child(text));
            }
        }
        col
    }
}

/// Toggle a spoken language, keeping one or two.
fn toggle_spoken(policy: &mut LanguagePolicy, code: &str) {
    let Ok(lang) = Lang::parse(code) else { return };
    let mut langs: Vec<Lang> = policy.spoken.iter().cloned().collect();
    if let Some(i) = langs.iter().position(|l| *l == lang) {
        if langs.len() > 1 {
            langs.remove(i);
        }
    } else {
        langs.push(lang);
        if langs.len() > 2 {
            langs.remove(0);
        }
    }
    if let Ok(spoken) = Spoken::try_from(langs) {
        policy.spoken = spoken;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(p: &LanguagePolicy) -> Vec<&str> {
        p.spoken.iter().map(|l| l.as_str()).collect()
    }

    #[test]
    fn toggling_keeps_one_or_two_spoken_languages() {
        let mut p = LanguagePolicy::default(); // ja, en
        toggle_spoken(&mut p, "ko");
        assert_eq!(codes(&p), ["en", "ko"], "the oldest gives way");
        toggle_spoken(&mut p, "en");
        assert_eq!(codes(&p), ["ko"]);
        toggle_spoken(&mut p, "ko");
        assert_eq!(codes(&p), ["ko"], "the last language stays");
    }
}
