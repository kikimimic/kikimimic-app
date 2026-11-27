//! The system status item is available even when another app has focus.

use futures::{StreamExt, channel::mpsc};
use gpui::{App, BorrowAppContext, Global, Window, WindowHandle};
use kikimimic_core::{
    SessionMode,
    event::{Notice, Phase, StatusView},
};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_app_kit::{
    NSColor, NSImage, NSMenu, NSMenuItem, NSSquareStatusItemLength, NSStatusBar, NSStatusItem,
    NSView, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{i18n::Ui, window::MainWindow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordingStatus {
    Ready,
    Starting,
    Recording,
    Listening,
    Stopping,
    Failed,
}

impl RecordingStatus {
    fn for_phase(phase: &Phase, saving: bool) -> Self {
        match phase {
            Phase::Idle => Self::Ready,
            Phase::Listening if saving => Self::Recording,
            Phase::Listening => Self::Listening,
            Phase::Stopping => Self::Stopping,
            Phase::Error { .. } => Self::Failed,
        }
    }
}

#[derive(Clone, Copy)]
enum Command {
    RecordOrStop,
    Open,
    Settings,
    Quit,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. AppKit sends
    // menu actions on the main thread.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = mpsc::UnboundedSender<Command>]
    struct MenuTarget;

    impl MenuTarget {
        #[unsafe(method(kikimimicStatusItemAction:))]
        fn invoke(&self, item: &NSMenuItem) {
            let command = match item.tag() {
                tag if tag == Command::RecordOrStop as isize => Command::RecordOrStop,
                tag if tag == Command::Open as isize => Command::Open,
                tag if tag == Command::Settings as isize => Command::Settings,
                tag if tag == Command::Quit as isize => Command::Quit,
                _ => return,
            };
            let _ = self.ivars().unbounded_send(command);
        }
    }
);

struct StatusMenu {
    main: WindowHandle<MainWindow>,
    item: Retained<NSStatusItem>,
    recording: Retained<NSMenuItem>,
    open: Retained<NSMenuItem>,
    settings: Retained<NSMenuItem>,
    quit: Retained<NSMenuItem>,
    // NSMenuItem keeps only a weak reference to its target.
    _target: Retained<MenuTarget>,
    ui: Ui,
    status: RecordingStatus,
}

impl Global for StatusMenu {}

impl StatusMenu {
    fn refresh(&self) {
        use RecordingStatus::*;
        let ui = self.ui;
        let (title, label, symbol, enabled) = match self.status {
            Ready => (ui.status_start(), "Kikimimic", "waveform", true),
            Starting => (
                ui.status_starting(),
                ui.status_starting(),
                "ellipsis.circle",
                false,
            ),
            Recording => (
                ui.status_stop(),
                ui.status_recording(),
                "record.circle.fill",
                true,
            ),
            Listening => (ui.status_stop_session(), ui.listening(), "mic.fill", true),
            Stopping => (
                ui.status_stopping(),
                ui.status_stopping(),
                "ellipsis.circle",
                false,
            ),
            Failed => (
                ui.status_retry(),
                ui.status_failed(),
                "exclamationmark.circle",
                true,
            ),
        };
        self.recording.setTitle(&NSString::from_str(title));
        self.recording.setEnabled(enabled);
        self.open.setTitle(&NSString::from_str(ui.status_open()));
        self.settings
            .setTitle(&NSString::from_str(ui.settings_button()));
        self.quit.setTitle(&NSString::from_str(ui.quit()));

        let mtm = MainThreadMarker::new().expect("AppKit runs on the main thread");
        if let Some(button) = self.item.button(mtm) {
            let label = NSString::from_str(label);
            let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
                &NSString::from_str(symbol),
                Some(&label),
            );
            if let Some(image) = &image {
                image.setTemplate(true);
            }
            button.setImage(image.as_deref());
            button.setToolTip(Some(&label));
            let red = (self.status == Recording).then(NSColor::systemRedColor);
            button.setContentTintColor(red.as_deref());
        }
    }
}

pub fn install(main: WindowHandle<MainWindow>, ui: Ui, cx: &mut App) {
    let mtm = MainThreadMarker::new().expect("AppKit runs on the main thread");
    let (sender, mut receiver) = mpsc::unbounded();
    let target = MenuTarget::alloc(mtm).set_ivars(sender);
    // SAFETY: NSObject's init is its designated initializer.
    let target: Retained<MenuTarget> = unsafe { msg_send![super(target), init] };
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    let add = |command: Command| {
        let item = NSMenuItem::new(mtm);
        // SAFETY: This selector is implemented by MenuTarget and the
        // target remains alive in StatusMenu while the menu is installed.
        unsafe {
            item.setAction(Some(sel!(kikimimicStatusItemAction:)));
            item.setTarget(Some(target.as_ref()));
        }
        item.setTag(command as isize);
        menu.addItem(&item);
        item
    };
    let recording = add(Command::RecordOrStop);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let open = add(Command::Open);
    let settings = add(Command::Settings);
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    let quit = add(Command::Quit);
    let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSSquareStatusItemLength);
    item.setMenu(Some(&menu));

    let status = StatusMenu {
        main,
        item,
        recording,
        open,
        settings,
        quit,
        _target: target,
        ui,
        status: RecordingStatus::Ready,
    };
    status.refresh();
    cx.set_global(status);

    // Defer AppKit actions to GPUI's foreground task so menu tracking can
    // finish before we change the menu or activate a window.
    cx.spawn(async move |cx| {
        while let Some(command) = receiver.next().await {
            cx.update(|cx| match command {
                Command::RecordOrStop => toggle_recording(cx),
                Command::Open => show_window(cx, false),
                Command::Settings => show_window(cx, true),
                Command::Quit => cx.quit(),
            });
        }
    })
    .detach();

    let _ = main.update(cx, |_, window, cx| {
        window.on_window_should_close(cx, |window, cx| {
            let Some(native) = native_window(window) else {
                return true;
            };
            cx.foreground_executor()
                .spawn(async move { native.orderOut(None) })
                .detach();
            false
        });
    });
}

fn native_window(window: &Window) -> Option<Retained<NSWindow>> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return None;
    };
    // SAFETY: GPUI exposes an NSView pointer valid for the lifetime of the
    // borrowed window handle. AppKit retains the returned NSWindow.
    let view = unsafe { &*handle.ns_view.cast::<NSView>().as_ptr() };
    view.window()
}

pub fn reopen(cx: &mut App) {
    show_window(cx, false);
}

fn show_window(cx: &mut App, settings: bool) {
    let Some(menu) = cx.try_global::<StatusMenu>() else {
        return;
    };
    let main = menu.main;
    cx.activate(true);
    let _ = main.update(cx, |main, window, cx| {
        if settings {
            main.open_settings(cx);
        }
        window.activate_window();
    });
}

fn toggle_recording(cx: &mut App) {
    let menu = cx.global::<StatusMenu>();
    let main = menu.main;
    match menu.status {
        RecordingStatus::Ready | RecordingStatus::Failed => {
            if main
                .update(cx, |main, _, cx| main.record(SessionMode::Meeting, cx))
                .is_ok()
            {
                set_status(RecordingStatus::Starting, cx);
            }
        }
        RecordingStatus::Recording | RecordingStatus::Listening => {
            let _ = main.update(cx, |main, _, cx| main.stop_by_user(cx));
        }
        RecordingStatus::Starting | RecordingStatus::Stopping => {}
    }
}

pub fn relabel(ui: Ui, cx: &mut App) {
    if cx.has_global::<StatusMenu>() {
        cx.update_global::<StatusMenu, _>(|menu, _| {
            menu.ui = ui;
            menu.refresh();
        });
    }
}

pub fn apply(status: &StatusView, cx: &mut App) {
    let Some(menu) = cx.try_global::<StatusMenu>() else {
        return;
    };
    if menu.status == RecordingStatus::Starting && status.phase == Phase::Idle {
        // The runtime can report an unavailable input as Idle before a
        // remaining input starts listening.
        return;
    }
    let saving = status.recording_dir.is_some()
        && !status
            .notices
            .iter()
            .any(|n| matches!(n, Notice::RecordingOff { .. }));
    set_status(RecordingStatus::for_phase(&status.phase, saving), cx);
}

fn set_status(status: RecordingStatus, cx: &mut App) {
    if cx.global::<StatusMenu>().status != status {
        cx.update_global::<StatusMenu, _>(|menu, _| {
            menu.status = status;
            menu.refresh();
        });
    }
}

#[cfg(test)]
#[test]
fn status_item_only_shows_recording_while_audio_is_being_saved() {
    assert_eq!(
        RecordingStatus::for_phase(&Phase::Listening, true),
        RecordingStatus::Recording
    );
    assert_eq!(
        RecordingStatus::for_phase(&Phase::Listening, false),
        RecordingStatus::Listening
    );
    assert_eq!(
        RecordingStatus::for_phase(&Phase::Stopping, true),
        RecordingStatus::Stopping
    );
}
