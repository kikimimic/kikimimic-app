//! Asking the chosen coding agent about a meeting — the open one, or the one
//! being recorded — and having it draft minutes. Nothing is sent until the
//! user has confirmed, once per agent per run, what goes where.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use kikimimic_core::agent::{self, Agent};
use kikimimic_core::meeting_qa::Answer;

use super::*;
use crate::theme;

/// One question and, once it arrives, its answer with the quoted lines'
/// positions (kept from the transcript the agent was shown).
pub(super) struct Entry {
    pub dir: PathBuf,
    pub question: String,
    pub result: Option<Result<Reply, String>>,
}

pub(super) struct Reply {
    pub text: String,
    pub confirmed: bool,
    /// Position and quote of each cited line.
    pub quotes: Vec<(u64, String)>,
}

/// What waits for the user's go-ahead before anything is sent.
pub(super) enum Pending {
    Question { dir: PathBuf, question: String },
    Minutes { dir: PathBuf },
}

pub(super) struct AskState {
    pub input: Entity<TextInput>,
    pub entries: Vec<Entry>,
    pub pending: Option<Pending>,
    pub consented: HashSet<Agent>,
    pub making_minutes: Option<PathBuf>,
}

impl AskState {
    pub fn new(ui: Ui, cx: &mut Context<MainWindow>) -> Self {
        Self {
            input: cx.new(|cx| TextInput::new(ui.ask_placeholder(), cx)),
            entries: Vec::new(),
            pending: None,
            consented: HashSet::new(),
            making_minutes: None,
        }
    }
}

impl MainWindow {
    fn agent_or_explain(&mut self, cx: &mut Context<Self>) -> Option<Agent> {
        let agent = self.kkm.settings().agent;
        if agent.is_none() {
            self.error = Some(self.ui.choose_agent_first().into());
            cx.notify();
        }
        agent
    }

    fn submit_question(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let question = self.ask.input.read(cx).text().trim().to_string();
        if question.is_empty() {
            return;
        }
        let Some(agent) = self.agent_or_explain(cx) else {
            return;
        };
        self.ask.input.update(cx, |i, cx| i.clear(cx));
        let pending = Pending::Question { dir, question };
        if self.ask.consented.contains(&agent) {
            self.send(pending, agent, cx);
        } else {
            self.ask.pending = Some(pending);
        }
        cx.notify();
    }

    pub(super) fn request_minutes(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let Some(agent) = self.agent_or_explain(cx) else {
            return;
        };
        let pending = Pending::Minutes { dir };
        if self.ask.consented.contains(&agent) {
            self.send(pending, agent, cx);
        } else {
            self.ask.pending = Some(pending);
        }
        cx.notify();
    }

    fn send(&mut self, pending: Pending, agent: Agent, cx: &mut Context<Self>) {
        match pending {
            Pending::Question { dir, question } => {
                let run = self.run_agent;
                self.ask.entries.push(Entry {
                    dir: dir.clone(),
                    question: question.clone(),
                    result: None,
                });
                let index = self.ask.entries.len() - 1;
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            let session = library::read(&dir).map_err(|e| format!("{e:#}"))?;
                            let snapshot = agent::snapshot(&session);
                            let output = run(agent, &agent::question_prompt(&session, &question))
                                .map_err(|e| format!("{e:#}"))?;
                            Ok(match agent::parse_answer(&snapshot, &output) {
                                Answer::Supported { text, citations } => Reply {
                                    text,
                                    confirmed: true,
                                    quotes: citations
                                        .into_iter()
                                        .filter_map(|c| {
                                            snapshot
                                                .source(&c.utterance_id)
                                                .map(|s| (s.start_ms, c.quote))
                                        })
                                        .collect(),
                                },
                                Answer::Unconfirmed => Reply {
                                    text: String::new(),
                                    confirmed: false,
                                    quotes: Vec::new(),
                                },
                            })
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        if let Some(entry) = this.ask.entries.get_mut(index) {
                            entry.result = Some(result);
                        }
                        cx.notify();
                    });
                })
                .detach();
            }
            Pending::Minutes { dir } => {
                let run = self.run_agent;
                self.ask.making_minutes = Some(dir.clone());
                cx.spawn(async move |this, cx| {
                    let d = dir.clone();
                    let result = cx
                        .background_spawn(async move {
                            let session = library::read(&d)?;
                            let output = run(agent, &agent::minutes_prompt(&session))?;
                            let minutes = agent::parse_minutes(&session, &output)?;
                            kikimimic_core::minutes::save(&d, &minutes, &session.lines)
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.ask.making_minutes = None;
                        match result {
                            Ok(()) => this.auto_export(
                                dir.clone(),
                                kikimimic_core::destinations::When::AfterMinutes,
                                cx,
                            ),
                            Err(e) => this.error = Some(format!("{e:#}")),
                        }
                        this.refresh_detail(cx);
                        this.refresh_recordings(cx);
                    });
                })
                .detach();
            }
        }
        cx.notify();
    }

    /// The go-ahead row when it is about minutes (shown in the minutes tab).
    pub(super) fn consent_row_for_minutes(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        matches!(self.ask.pending, Some(Pending::Minutes { .. }))
            .then(|| self.consent_row(cx))
            .flatten()
    }

    /// The go-ahead row: who receives what, and the two choices.
    fn consent_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ui = self.ui;
        let pending = self.ask.pending.as_ref()?;
        let agent = self.kkm.settings().agent?;
        let what = match pending {
            Pending::Question { .. } => ui.sends_transcript_and_question(),
            Pending::Minutes { .. } => ui.sends_transcript(),
        };
        Some(
            div()
                .flex()
                .flex_col()
                .gap_2()
                .p_3()
                .rounded(px(6.))
                .bg(rgb(0xfff8c5))
                .child(ui.send_to(agent.name(), agent.recipient()))
                .child(div().text_size(px(12.)).child(what))
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .justify_end()
                        .child(button("consent-cancel", ui.dont_send(), false).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.ask.pending = None;
                                cx.notify();
                            }),
                        ))
                        .child(
                            button("consent-send", ui.send(), true)
                                .debug_selector(|| "consent-send".into())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(p) = this.ask.pending.take() {
                                        this.ask.consented.insert(agent);
                                        this.send(p, agent, cx);
                                    }
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Questions about `dir`, their answers, and the box to ask another.
    pub(super) fn ask_panel(&self, dir: &Path, cx: &mut Context<Self>) -> AnyElement {
        let ui = self.ui;
        let agent = self.kkm.settings().agent;
        let mut col = div().flex().flex_col().gap_3();
        for (i, entry) in self
            .ask
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.dir == dir)
        {
            let mut block = div().flex().flex_col().gap_1().child(
                div()
                    .self_end()
                    .px_3()
                    .py_1()
                    .rounded(px(6.))
                    .bg(theme::subtle())
                    .child(entry.question.clone()),
            );
            block = match &entry.result {
                None => block.child(div().text_color(theme::muted()).child(ui.asking())),
                Some(Err(e)) => block.child(div().text_color(theme::danger()).child(e.clone())),
                Some(Ok(r)) if !r.confirmed => block.child(
                    div()
                        .text_color(theme::warn())
                        .child(ui.unconfirmed_answer()),
                ),
                Some(Ok(r)) => {
                    let mut b = block.child(div().child(r.text.clone()));
                    for (q, (at, quote)) in r.quotes.iter().enumerate() {
                        let at = *at;
                        b = b.child(
                            div()
                                .flex()
                                .gap_2()
                                .text_size(px(12.))
                                .child(
                                    div()
                                        .id(("quote", i * 100 + q))
                                        .text_color(rgb(0x0969da))
                                        .cursor_pointer()
                                        .child(mm_ss(at))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.play_recording(at, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .text_color(theme::muted())
                                        .child(format!("「{quote}」")),
                                ),
                        );
                    }
                    b
                }
            };
            col = col.child(block);
        }
        if matches!(self.ask.pending, Some(Pending::Question { .. }))
            && let Some(row) = self.consent_row(cx)
        {
            col = col.child(row);
        }
        let d = dir.to_path_buf();
        col.child(
            div()
                .flex()
                .gap_2()
                .items_center()
                .child(div().flex_1().child(self.ask.input.clone()))
                .child(
                    button("ask-send", ui.ask(), false)
                        .debug_selector(|| "ask-send".into())
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.submit_question(d.clone(), cx)),
                        ),
                ),
        )
        .child(
            div()
                .text_size(px(12.))
                .text_color(theme::muted())
                .child(match agent {
                    Some(a) => ui.answers_from(a.name()),
                    None => ui.choose_agent_first().to_string(),
                }),
        )
        .into_any_element()
    }
}
