//! ⌘K: find an action by name and run it. A shortcut only — every action
//! listed is also a button somewhere on screen, so nothing is hidden here.

use gpui::{Focusable, KeyDownEvent};
use kikimimic_core::destinations;

use super::*;
use crate::theme;
use crate::widgets::text_input;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Command {
    RecordMeeting,
    RecordFaceToFace,
    Stop,
    ShowLive,
    Ask,
    Search,
    Import,
    WriteThis,
    MinutesThis,
    Captions,
    AutoRecord,
    Settings,
    ExportSettings,
}

pub(super) struct Palette {
    pub input: Entity<TextInput>,
    pub selected: usize,
}

impl MainWindow {
    /// What can be done right now, with its shortcut if it has one.
    pub(super) fn palette_commands(
        &self,
        cx: &gpui::App,
    ) -> Vec<(&'static str, &'static str, Command)> {
        let ui = self.ui;
        let listening = self.state.read(cx).is_listening();
        let open = self.open_recording.is_some();
        let mut all = Vec::new();
        if listening {
            all.push((ui.stop(), "", Command::Stop));
            all.push((ui.show_live(), "", Command::ShowLive));
            all.push((ui.toggle_captions(), "", Command::Captions));
        } else {
            all.push((ui.record_meeting(), "⌘R", Command::RecordMeeting));
            all.push((ui.face_to_face(), "⇧⌘R", Command::RecordFaceToFace));
        }
        all.push((ui.ask_title(), "⌘J", Command::Ask));
        all.push((ui.cmd_search(), "", Command::Search));
        all.push((ui.cmd_import(), "", Command::Import));
        if open {
            all.push((ui.cmd_write_pending(), "", Command::WriteThis));
            all.push((ui.cmd_make_minutes(), "", Command::MinutesThis));
        }
        all.push((ui.toggle_auto_record(), "", Command::AutoRecord));
        all.push((ui.settings_button(), "⌘,", Command::Settings));
        all.push((ui.cmd_export_settings(), "", Command::ExportSettings));
        let query = self
            .palette
            .as_ref()
            .map(|p| p.input.read(cx).text().trim().to_lowercase())
            .unwrap_or_default();
        all.into_iter()
            .filter(|(label, _, _)| query.is_empty() || label.to_lowercase().contains(&query))
            .collect()
    }

    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| TextInput::new(self.ui.palette_placeholder(), cx));
        cx.observe(&input, |this, _, cx| {
            if let Some(p) = this.palette.as_mut() {
                p.selected = 0;
            }
            cx.notify();
        })
        .detach();
        window.focus(&input.focus_handle(cx), cx);
        self.palette = Some(Palette { input, selected: 0 });
        cx.notify();
    }

    fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.palette = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn run_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(selected) = self.palette.as_ref().map(|p| p.selected) else {
            return;
        };
        let command = self.palette_commands(cx).get(selected).map(|c| c.2);
        self.close_palette(window, cx);
        if let Some(command) = command {
            self.run_command(command, window, cx);
        }
    }

    pub(super) fn run_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            Command::RecordMeeting => self.record(SessionMode::Meeting, cx),
            Command::RecordFaceToFace => self.record(SessionMode::FaceToFace, cx),
            Command::Stop => self.stop_by_user(cx),
            Command::ShowLive => self.show(View::Live, cx),
            Command::Ask => self.toggle_ask(cx),
            Command::Search => window.focus(&self.search.focus_handle(cx), cx),
            Command::Import => self.pick_import(false, cx),
            Command::WriteThis => {
                if let Some((dir, _)) = &self.open_recording {
                    let dests = self
                        .kkm
                        .settings()
                        .destinations
                        .into_iter()
                        .filter(|d: &destinations::Destination| d.enabled)
                        .collect();
                    self.write_destinations(dir.clone(), dests, cx);
                }
            }
            Command::MinutesThis => {
                if let Some((dir, _)) = &self.open_recording {
                    let dir = dir.clone();
                    self.detail_tab = super::detail::DetailTab::Minutes;
                    self.show(View::Recording, cx);
                    self.request_minutes(dir, cx);
                }
            }
            Command::Captions => self.toggle_captions(cx),
            Command::AutoRecord => self.toggle_auto_record(cx),
            Command::Settings => self.show(View::Settings, cx),
            Command::ExportSettings => {
                self.section = super::settings::Section::Export;
                self.show(View::Settings, cx);
            }
        }
    }

    /// The palette over the window, or nothing when it is closed.
    pub(super) fn palette_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ui = self.ui;
        let palette = self.palette.as_ref()?;
        let commands = self.palette_commands(cx);
        let selected = palette.selected.min(commands.len().saturating_sub(1));
        let mut list = div().flex().flex_col().gap_px();
        if commands.is_empty() {
            list = list.child(
                div()
                    .p_2()
                    .text_size(px(12.))
                    .text_color(theme::muted())
                    .child(ui.palette_nothing()),
            );
        }
        for (i, (label, keys, command)) in commands.into_iter().enumerate() {
            list = list.child(
                div()
                    .id(("palette-item", i))
                    .flex()
                    .items_center()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .cursor_pointer()
                    .when(i == selected, |d| d.bg(theme::subtle()))
                    .hover(|s| s.bg(theme::subtle()))
                    .child(div().flex_1().child(label))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme::muted())
                            .child(keys),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.close_palette(window, cx);
                        this.run_command(command, window, cx);
                    })),
            );
        }
        let card = div()
            .id("palette")
            .debug_selector(|| "palette".into())
            .w(px(520.))
            .mt(px(80.))
            .p_2()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0xffffff))
            .rounded(px(10.))
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            // Enter is the field's own key; catch it on the way down.
            .capture_action(cx.listener(|this, _: &text_input::Enter, window, cx| {
                this.run_selected(window, cx);
                cx.stop_propagation();
            }))
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                let count = this.palette_commands(cx).len();
                let Some(p) = this.palette.as_mut() else {
                    return;
                };
                match e.keystroke.key.as_str() {
                    "down" => p.selected = (p.selected + 1).min(count.saturating_sub(1)),
                    "up" => p.selected = p.selected.saturating_sub(1),
                    "escape" => return this.close_palette(window, cx),
                    _ => return,
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_palette(window, cx)))
            .child(palette.input.clone())
            .child(list)
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme::muted())
                    .child(ui.palette_hint()),
            );
        Some(
            deferred(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .justify_center()
                    .items_start()
                    .child(card),
            )
            .with_priority(2)
            .into_any_element(),
        )
    }
}
