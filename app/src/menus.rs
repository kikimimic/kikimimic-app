//! The menu bar and the Dock menu: a second way to start and stop a
//! recording without bringing the window forward. The window stays the
//! main place to work; these only reach what is needed mid-meeting.

use gpui::{App, Menu, MenuItem, WindowHandle, actions};
use kikimimic_core::SessionMode;

use crate::i18n::Ui;
use crate::window::MainWindow;

actions!(
    kikimimic,
    [
        Quit,
        RecordMeeting,
        RecordFaceToFace,
        StopRecording,
        ToggleAutoRecord,
        ToggleCaptions
    ]
);

/// The recording actions, shared by the menu bar and the Dock menu.
fn recording_items(ui: Ui) -> Vec<MenuItem> {
    vec![
        MenuItem::action(ui.record_meeting(), RecordMeeting),
        MenuItem::action(ui.face_to_face(), RecordFaceToFace),
        MenuItem::action(ui.stop(), StopRecording),
        MenuItem::separator(),
        MenuItem::action(ui.toggle_auto_record(), ToggleAutoRecord),
        MenuItem::action(ui.toggle_captions(), ToggleCaptions),
    ]
}

pub fn menus(ui: Ui) -> Vec<Menu> {
    vec![
        Menu::new("Kikimimic").items([MenuItem::action(ui.quit(), Quit)]),
        Menu::new(ui.recording_menu()).items(recording_items(ui)),
    ]
}

pub fn install(main: WindowHandle<MainWindow>, cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(move |_: &RecordMeeting, cx| {
        let _ = main.update(cx, |w, _, cx| w.record(SessionMode::Meeting, cx));
    });
    cx.on_action(move |_: &RecordFaceToFace, cx| {
        let _ = main.update(cx, |w, _, cx| w.record(SessionMode::FaceToFace, cx));
    });
    cx.on_action(move |_: &StopRecording, cx| {
        let _ = main.update(cx, |w, _, cx| w.stop_by_user(cx));
    });
    cx.on_action(move |_: &ToggleAutoRecord, cx| {
        let _ = main.update(cx, |w, _, cx| w.toggle_auto_record(cx));
    });
    cx.on_action(move |_: &ToggleCaptions, cx| {
        let _ = main.update(cx, |w, _, cx| w.toggle_captions(cx));
    });
    if let Ok(ui) = main.read_with(cx, |w, _| w.ui) {
        relabel(ui, cx);
    }
}

pub fn relabel(ui: Ui, cx: &mut App) {
    cx.set_menus(menus(ui));
    cx.set_dock_menu(recording_items(ui));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recording_menu_reaches_what_a_meeting_needs() {
        let m = menus(Ui::Ja);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].name.as_ref(), "録音");
        let names: Vec<String> = m[1]
            .items
            .iter()
            .filter_map(|i| match i {
                MenuItem::Action { name, .. } => Some(name.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            [
                "● 会議を録音",
                "対面",
                "■ 停止",
                "自動録音を切り替える",
                "字幕を切り替える"
            ]
        );
    }
}
