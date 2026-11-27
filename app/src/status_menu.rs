//! The system status item is available even when another app has focus.

use std::time::{Duration, Instant};

use futures::{StreamExt, channel::mpsc};
use gpui::{App, BorrowAppContext, Global, Task, Window, WindowHandle};
use kikimimic_core::{
    SessionMode,
    event::{Notice, Phase, StatusView},
};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained, sel};
use objc2_app_kit::{
    NSCellImagePosition, NSColor, NSFont, NSFontWeightRegular, NSImage, NSImageSymbolConfiguration,
    NSMenu, NSMenuItem, NSSquareStatusItemLength, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength, NSView, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSObject, NSString};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::{
    i18n::Ui,
    window::{MainWindow, mm_ss},
};

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
    recording_timer: Option<Task<()>>,
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
            Recording => (ui.status_stop(), ui.status_recording(), "circle.fill", true),
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
            )
            .map(|image| {
                if self.status != Recording {
                    image.setTemplate(true);
                    return image;
                }
                // SAFETY: AppKit exports this immutable standard font weight.
                let weight = unsafe { NSFontWeightRegular };
                let size =
                    NSImageSymbolConfiguration::configurationWithPointSize_weight(10., weight);
                let colors = NSArray::from_retained_slice(&[NSColor::systemRedColor()]);
                let red = NSImageSymbolConfiguration::configurationWithPaletteColors(&colors);
                let configuration = size.configurationByApplyingConfiguration(&red);
                let image = image
                    .imageWithSymbolConfiguration(&configuration)
                    .unwrap_or(image);
                image.setTemplate(false);
                image
            });
            button.setImage(image.as_deref());
            button.setToolTip(Some(&label));
        }
    }

    fn update_recording_timer(&mut self, cx: &App) {
        // Dropping the task cancels its pending tick.
        drop(self.recording_timer.take());
        let mtm = MainThreadMarker::new().expect("AppKit runs on the main thread");
        let Some(button) = self.item.button(mtm) else {
            return;
        };
        if self.status != RecordingStatus::Recording {
            button.setTitle(&NSString::from_str(""));
            button.setImagePosition(NSCellImagePosition::ImageOnly);
            self.item.setLength(NSSquareStatusItemLength);
            return;
        }

        let started = Instant::now();
        button.setImagePosition(NSCellImagePosition::ImageLeft);
        self.item.setLength(NSVariableStatusItemLength);
        button.setTitle(&NSString::from_str(&recording_time(0)));
        self.recording_timer = Some(cx.spawn(async move |cx| {
            loop {
                // Align updates to the elapsed seconds even when the main
                // thread is temporarily busy.
                let elapsed = started.elapsed();
                cx.background_executor()
                    .timer(Duration::from_secs(elapsed.as_secs() + 1) - elapsed)
                    .await;
                button.setTitle(&NSString::from_str(&recording_time(
                    started.elapsed().as_secs(),
                )));
            }
        }));
    }
}

fn recording_time(seconds: u64) -> String {
    let hours = seconds / 3600;
    let within_hour = mm_ss((seconds % 3600) * 1000);
    if hours == 0 {
        within_hour
    } else {
        format!("{hours}:{within_hour}")
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
    if let Some(button) = item.button(mtm) {
        let size = button.font().map_or(13., |font| font.pointSize());
        // SAFETY: AppKit exports this immutable standard font weight.
        let weight = unsafe { NSFontWeightRegular };
        let font = NSFont::monospacedDigitSystemFontOfSize_weight(size, weight);
        button.setFont(Some(&font));
    }

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
        recording_timer: None,
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
        cx.update_global::<StatusMenu, _>(|menu, cx| {
            menu.status = status;
            menu.refresh();
            menu.update_recording_timer(cx);
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

#[cfg(test)]
#[test]
fn recording_time_shows_seconds_and_expands_at_one_hour() {
    assert_eq!(recording_time(0), "00:00");
    assert_eq!(recording_time(65), "01:05");
    assert_eq!(recording_time(3599), "59:59");
    assert_eq!(recording_time(3600), "1:00:00");
}
