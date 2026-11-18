//! Settings: one screen, a row of sections, every change saved at once.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Section {
    #[default]
    General,
    Glossary,
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
            Section::Link => self.calendar_section(cx).into_any_element(),
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
