//! System-audio capture via a CoreAudio Process Tap (macOS 14.2+).
//!
//! Verified by the Step 0 spike (docs/step0-tap-results.md); that document
//! also records why every failure here is silent rather than an error:
//!
//! - Without the System Audio Recording permission every call still returns
//!   noErr and the IO block still fires — the buffers are just zero-filled.
//!   The app must therefore be a signed bundle with
//!   NSAudioCaptureUsageDescription, and `cargo run` will capture silence.
//! - An idle output device runs no IO cycle at all, so a working tap
//!   delivers no callbacks until something plays.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyFormat, AudioDeviceCreateIOProcIDWithBlock, AudioDeviceDestroyIOProcID,
    AudioDeviceIOBlock, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectAddPropertyListenerBlock, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectRemovePropertyListenerBlock, CATapDescription,
};
use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsNonInterleaved, kAudioFormatLinearPCM,
    AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSMutableDictionary, NSNumber, NSString};

use kikimimic_core::capture::{
    channel, CaptureStream, CaptureWriter, Format, SourceDecision, SourceEvent,
};
use kikimimic_core::domain::HostNanos;
use kikimimic_core::CaptureHandle;

use crate::{READY_TIMEOUT, RING};

/// A macOS output device as reported by Core Audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputDevice {
    pub uid: String,
    pub name: String,
}

/// Why a pending output-device switch was dismissed without rebuilding IO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputDeviceSwitchCancellation {
    Rejected,
    StaleDecision,
    AlreadyCapturing,
}

/// Events produced by the speaker capture thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputDeviceSwitchEvent {
    Detected {
        capturing: OutputDevice,
        detected: OutputDevice,
    },
    Switched {
        previous: OutputDevice,
        current: OutputDevice,
    },
    SwitchFailed {
        capturing: OutputDevice,
        detected: OutputDevice,
        error: String,
    },
    Cancelled {
        capturing: OutputDevice,
        detected: OutputDevice,
        reason: OutputDeviceSwitchCancellation,
    },
}

/// A UI decision. `expected_uid` makes decisions safe against a newer device
/// change arriving while a notification is visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputDeviceSwitchDecision {
    Switch { expected_uid: String },
    Cancel { expected_uid: String },
}

/// Start tapping system audio. The tap thread lives until the handle drops.
pub fn open() -> Result<(CaptureStream, Box<dyn CaptureHandle>)> {
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel();
    let flag = Arc::clone(&stop);
    std::thread::Builder::new()
        .name("kikimimic-speaker".into())
        .spawn(move || {
            if let Err(e) = run(&flag, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })?;
    let (stream, events, decisions) = ready_rx
        .recv_timeout(READY_TIMEOUT)
        .context("the system-audio tap did not start in time")??;
    Ok((
        stream,
        Box::new(SystemAudio {
            stop,
            events,
            decisions,
        }),
    ))
}

type Ready = Result<(
    CaptureStream,
    mpsc::Receiver<OutputDeviceSwitchEvent>,
    mpsc::Sender<OutputDeviceSwitchDecision>,
)>;

struct SystemAudio {
    stop: Arc<AtomicBool>,
    events: mpsc::Receiver<OutputDeviceSwitchEvent>,
    decisions: mpsc::Sender<OutputDeviceSwitchDecision>,
}

impl Drop for SystemAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl CaptureHandle for SystemAudio {
    fn poll_event(&mut self) -> Option<SourceEvent> {
        self.events.try_recv().ok().map(source_event)
    }

    fn decide(&mut self, decision: SourceDecision) {
        let _ = self.decisions.send(match decision {
            SourceDecision::Follow { uid } => {
                OutputDeviceSwitchDecision::Switch { expected_uid: uid }
            }
            SourceDecision::Stay { uid } => {
                OutputDeviceSwitchDecision::Cancel { expected_uid: uid }
            }
        });
    }
}

/// The core's view of a switch event. Returning to the device already being
/// captured withdraws the question instead of asking it.
fn source_event(e: OutputDeviceSwitchEvent) -> SourceEvent {
    match e {
        OutputDeviceSwitchEvent::Detected {
            capturing,
            detected,
        } if capturing.uid == detected.uid => SourceEvent::OutputSwitchWithdrawn,
        OutputDeviceSwitchEvent::Detected {
            capturing,
            detected,
        } => SourceEvent::OutputSwitchProposed {
            capturing: capturing.name,
            detected: detected.name,
            uid: detected.uid,
        },
        OutputDeviceSwitchEvent::Switched { current, .. } => {
            SourceEvent::OutputSwitched { name: current.name }
        }
        OutputDeviceSwitchEvent::SwitchFailed { error, .. } => {
            SourceEvent::OutputSwitchFailed { reason: error }
        }
        OutputDeviceSwitchEvent::Cancelled { .. } => SourceEvent::OutputSwitchWithdrawn,
    }
}

static NEXT_AGGREGATE_ID: AtomicU64 = AtomicU64::new(1);

/// Written only by the CoreAudio IO block, which the HAL runs serially on
/// the dispatch queue we hand it. Nothing else touches this state, so the
/// unsynchronised access is sound and the callback stays allocation- and
/// lock-free like the cpal one.
struct IoState {
    writer: CaptureWriter,
}

struct IoSink {
    state: std::cell::UnsafeCell<IoState>,
}

// SAFETY: see IoState's doc comment — access is serialised by the HAL.
unsafe impl Send for IoSink {}
unsafe impl Sync for IoSink {}

impl IoSink {
    /// Copy one IO cycle's input into the core's channel.
    fn consume(&self, in_data: NonNull<AudioBufferList>, captured_nanos: u64) {
        let abl = unsafe { in_data.as_ref() };
        let buffers = unsafe {
            std::slice::from_raw_parts(abl.mBuffers.as_ptr(), abl.mNumberBuffers as usize)
        };
        let state = unsafe { &mut *self.state.get() };
        for buf in buffers {
            if buf.mData.is_null() {
                continue;
            }
            let samples = unsafe {
                std::slice::from_raw_parts(
                    buf.mData as *const f32,
                    buf.mDataByteSize as usize / std::mem::size_of::<f32>(),
                )
            };
            state.writer.push(samples, HostNanos(captured_nanos));
        }
    }
}

/// mach ticks → nanoseconds. The ratio is fixed for the machine, so it is
/// read once and reused; a tap callback must not make system calls.
fn host_time_nanos(ticks: u64) -> u64 {
    static SCALE: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    let (numer, denom) = *SCALE.get_or_init(|| {
        let mut info = mach2::mach_time::mach_timebase_info { numer: 0, denom: 0 };
        // A zero denominator would mean a broken kernel; 1/1 at least keeps
        // the arithmetic sane.
        if unsafe { mach2::mach_time::mach_timebase_info(&mut info) } != 0 || info.denom == 0 {
            return (1, 1);
        }
        (info.numer as u64, info.denom as u64)
    });
    (ticks as u128 * numer as u128 / denom as u128) as u64
}

fn run(stop: &AtomicBool, ready_tx: &mpsc::Sender<Ready>) -> Result<()> {
    let desc = unsafe {
        CATapDescription::initStereoGlobalTapButExcludeProcesses(
            CATapDescription::alloc(),
            &NSArray::new(),
        )
    };
    unsafe {
        desc.setName(&NSString::from_str("Kikimimic"));
        desc.setPrivate(true);
    }
    let mut tap_id: AudioObjectID = 0;
    check(
        unsafe { AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id) },
        "create process tap (is the app bundle signed with \
         NSAudioCaptureUsageDescription?)",
    )?;
    let _tap = TapGuard(tap_id);

    let asbd = tap_format(tap_id)?;
    // The IO block reads the buffers as interleaved f32 frames; any other
    // format would come out as garbage rather than an error, so refuse it
    // loudly here.
    anyhow::ensure!(
        asbd.mFormatID == kAudioFormatLinearPCM
            && asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
            && asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved == 0,
        "tap format is not interleaved f32 PCM (format {:#x}, flags {:#x})",
        asbd.mFormatID,
        asbd.mFormatFlags,
    );
    let channels = (asbd.mChannelsPerFrame as usize).max(1);
    let src_rate = asbd.mSampleRate as u32;
    anyhow::ensure!(src_rate > 0, "tap reported a zero sample rate");

    let (writer, stream) = channel(
        Format {
            rate: src_rate,
            channels: channels as u16,
        },
        RING,
    );
    let fault = writer.fault();
    let sink = Arc::new(IoSink {
        state: std::cell::UnsafeCell::new(IoState { writer }),
    });

    let block = block2::RcBlock::new(
        move |_now: NonNull<AudioTimeStamp>,
              in_data: NonNull<AudioBufferList>,
              in_time: NonNull<AudioTimeStamp>,
              _out_data: NonNull<AudioBufferList>,
              _out_time: NonNull<AudioTimeStamp>| {
            // The HAL's host time for this input buffer: the same mach clock
            // cpal stamps the mic with, which is what puts both lanes on one
            // timeline.
            let captured = host_time_nanos(unsafe { in_time.as_ref() }.mHostTime);
            sink.consume(in_data, captured);
        },
    );
    // A nil queue reportedly fails to register the block on macOS 26, so
    // always hand the HAL an explicit one.
    let queue = dispatch2::DispatchQueue::new("io.github.kikimimic.speaker-io", None);
    let io_block = block2::RcBlock::as_ptr(&block) as AudioDeviceIOBlock;
    let initial_device = default_output_device()?;
    let (mut aggregate, mut io_proc) =
        start_output_io(&desc, &initial_device.uid, &queue, io_block)?;
    let mut tracker = OutputDeviceTracker::new(initial_device);
    let (notification_tx, notification_rx) = mpsc::sync_channel(1);
    let _listener = DefaultOutputListener::new(notification_tx)?;
    let (event_tx, event_rx) = mpsc::channel();
    let (decision_tx, decision_rx) = mpsc::channel();
    // Close the read/register race: a change between the initial lookup and
    // listener registration is surfaced before the pipeline starts polling.
    if let Ok(detected) = default_output_device() {
        emit_detected(&mut tracker, detected, &event_tx);
    }
    let _ = ready_tx.send(Ok((stream, event_rx, decision_tx)));

    while !stop.load(Ordering::SeqCst) {
        if notification_rx.try_recv().is_ok() {
            while notification_rx.try_recv().is_ok() {}
            match default_output_device() {
                Ok(detected) => emit_detected(&mut tracker, detected, &event_tx),
                Err(err) => eprintln!("read changed default output device: {err:#}"),
            }
        }

        while let Ok(decision) = decision_rx.try_recv() {
            // Re-read immediately before acting. A newer HAL notification may
            // still be queued, and an approval must never target stale state.
            match default_output_device() {
                Ok(detected) => emit_detected(&mut tracker, detected, &event_tx),
                Err(err) => {
                    let _ = event_tx.send(OutputDeviceSwitchEvent::SwitchFailed {
                        capturing: tracker.capturing.clone(),
                        detected: tracker.observed.clone(),
                        error: format!("verify current default output device: {err:#}"),
                    });
                    continue;
                }
            }

            match tracker.action_for(&decision) {
                SwitchAction::Cancel(reason) => {
                    let _ = event_tx.send(OutputDeviceSwitchEvent::Cancelled {
                        capturing: tracker.capturing.clone(),
                        detected: tracker.observed.clone(),
                        reason,
                    });
                }
                SwitchAction::Switch => {
                    let previous = tracker.capturing.clone();
                    let detected = tracker.observed.clone();

                    // The tap, callback block, ring, and IoState remain alive;
                    // only the output-clocked aggregate and its IOProc change.
                    drop(io_proc);
                    drop(aggregate);

                    let switched = start_compatible_output_io(
                        &desc,
                        &detected.uid,
                        &queue,
                        io_block,
                        tap_id,
                        asbd,
                    );

                    match switched {
                        Ok((new_aggregate, new_io_proc)) => {
                            aggregate = new_aggregate;
                            io_proc = new_io_proc;
                            tracker.capturing = detected.clone();
                            let _ = event_tx.send(OutputDeviceSwitchEvent::Switched {
                                previous,
                                current: detected,
                            });
                        }
                        Err(switch_err) => {
                            match start_compatible_output_io(
                                &desc,
                                &previous.uid,
                                &queue,
                                io_block,
                                tap_id,
                                asbd,
                            ) {
                                Ok((old_aggregate, old_io_proc)) => {
                                    aggregate = old_aggregate;
                                    io_proc = old_io_proc;
                                    let _ = event_tx.send(OutputDeviceSwitchEvent::SwitchFailed {
                                        capturing: previous,
                                        detected,
                                        error: format!(
                                            "switch output device failed and previous device was restored: {switch_err:#}"
                                        ),
                                    });
                                }
                                Err(restore_err) => {
                                    let message = format!(
                                        "switch output device failed ({switch_err:#}); restoring previous device failed ({restore_err:#})"
                                    );
                                    fault.fail(&message);
                                    let _ = event_tx.send(OutputDeviceSwitchEvent::SwitchFailed {
                                        capturing: previous,
                                        detected,
                                        error: message,
                                    });
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

fn check(status: i32, what: &str) -> Result<()> {
    anyhow::ensure!(status == 0, "{what} failed: OSStatus {status}");
    Ok(())
}

/// The tap's stream format: what the IO block will actually deliver.
fn tap_format(tap_id: AudioObjectID) -> Result<AudioStreamBasicDescription> {
    let mut asbd: AudioStreamBasicDescription = unsafe { std::mem::zeroed() };
    read_property(
        tap_id,
        kAudioTapPropertyFormat,
        &mut asbd,
        "read tap stream format",
    )?;
    Ok(asbd)
}

fn start_output_io(
    desc: &CATapDescription,
    output_uid: &str,
    queue: &dispatch2::DispatchQueue,
    block: AudioDeviceIOBlock,
) -> Result<(AggregateGuard, IoProcGuard)> {
    let aggregate = AggregateGuard(create_aggregate(desc, output_uid)?);
    let mut proc_id: AudioDeviceIOProcID = None;
    check(
        unsafe {
            // SAFETY: `proc_id` is live for the out parameter, the aggregate
            // guard owns `aggregate.0`, and `block` plus `queue` outlive every
            // IOProc created by this helper.
            AudioDeviceCreateIOProcIDWithBlock(
                NonNull::from(&mut proc_id),
                aggregate.0,
                Some(queue),
                block,
            )
        },
        "create IO block",
    )?;
    let io_proc = IoProcGuard {
        agg_id: aggregate.0,
        proc_id,
    };
    // Aggregate composition is asynchronous; starting IO into a half-built
    // device is what "succeeds" and then delivers nothing.
    std::thread::sleep(Duration::from_millis(300));
    check(
        unsafe {
            // SAFETY: the IOProc was created for this live aggregate above.
            AudioDeviceStart(aggregate.0, proc_id)
        },
        "start tap IO",
    )?;
    Ok((aggregate, io_proc))
}

fn start_compatible_output_io(
    desc: &CATapDescription,
    output_uid: &str,
    queue: &dispatch2::DispatchQueue,
    block: AudioDeviceIOBlock,
    tap_id: AudioObjectID,
    expected_format: AudioStreamBasicDescription,
) -> Result<(AggregateGuard, IoProcGuard)> {
    let (aggregate, io_proc) = start_output_io(desc, output_uid, queue, block)?;
    let current_format = match tap_format(tap_id) {
        Ok(format) => format,
        Err(error) => {
            drop(io_proc);
            drop(aggregate);
            return Err(error);
        }
    };
    if current_format != expected_format {
        drop(io_proc);
        drop(aggregate);
        anyhow::bail!("tap format changed from {expected_format:?} to {current_format:?}");
    }
    Ok((aggregate, io_proc))
}

fn emit_detected(
    tracker: &mut OutputDeviceTracker,
    detected: OutputDevice,
    events: &mpsc::Sender<OutputDeviceSwitchEvent>,
) {
    if let Some(event) = tracker.observe(detected) {
        let _ = events.send(event);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SwitchAction {
    Switch,
    Cancel(OutputDeviceSwitchCancellation),
}

struct OutputDeviceTracker {
    capturing: OutputDevice,
    observed: OutputDevice,
}

impl OutputDeviceTracker {
    fn new(capturing: OutputDevice) -> Self {
        Self {
            observed: capturing.clone(),
            capturing,
        }
    }

    fn observe(&mut self, detected: OutputDevice) -> Option<OutputDeviceSwitchEvent> {
        if self.observed.uid == detected.uid {
            return None;
        }
        self.observed = detected.clone();
        Some(OutputDeviceSwitchEvent::Detected {
            capturing: self.capturing.clone(),
            detected,
        })
    }

    fn action_for(&self, decision: &OutputDeviceSwitchDecision) -> SwitchAction {
        let expected_uid = match decision {
            OutputDeviceSwitchDecision::Switch { expected_uid }
            | OutputDeviceSwitchDecision::Cancel { expected_uid } => expected_uid,
        };
        if expected_uid != &self.observed.uid {
            return SwitchAction::Cancel(OutputDeviceSwitchCancellation::StaleDecision);
        }
        if matches!(decision, OutputDeviceSwitchDecision::Cancel { .. }) {
            return SwitchAction::Cancel(OutputDeviceSwitchCancellation::Rejected);
        }
        if self.capturing.uid == self.observed.uid {
            return SwitchAction::Cancel(OutputDeviceSwitchCancellation::AlreadyCapturing);
        }
        SwitchAction::Switch
    }
}

type PropertyListener = block2::RcBlock<dyn Fn(u32, NonNull<AudioObjectPropertyAddress>) + 'static>;

/// Owns the exact block, queue, and address used to register the Core Audio
/// listener, so removal and callback draining use matching arguments.
struct DefaultOutputListener {
    listener: PropertyListener,
    queue: dispatch2::DispatchRetained<dispatch2::DispatchQueue>,
    address: AudioObjectPropertyAddress,
}

impl DefaultOutputListener {
    fn new(notifications: mpsc::SyncSender<()>) -> Result<Self> {
        let listener: PropertyListener = block2::RcBlock::new(move |_count, _addresses| {
            // A capacity-one signal coalesces bursts and never blocks the
            // serial listener queue. Device lookup happens on the capture
            // thread.
            let _ = notifications.try_send(());
        });
        let queue =
            dispatch2::DispatchQueue::new("dev.wagomu.kikimimic.default-output-listener", None);
        let mut address = AudioObjectPropertyAddress {
            mSelector: kAudioHardwarePropertyDefaultOutputDevice,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        check(
            unsafe {
                // SAFETY: the address is live for this call, and `listener`
                // and `queue` are retained by both Core Audio and this guard
                // until the matching remove call.
                AudioObjectAddPropertyListenerBlock(
                    kAudioObjectSystemObject as AudioObjectID,
                    NonNull::from(&mut address),
                    Some(&queue),
                    block2::RcBlock::as_ptr(&listener),
                )
            },
            "register default output device listener",
        )?;
        Ok(Self {
            listener,
            queue,
            address,
        })
    }
}

impl Drop for DefaultOutputListener {
    fn drop(&mut self) {
        let status = unsafe {
            // SAFETY: these are the same system object, property address,
            // queue, and block used at registration.
            AudioObjectRemovePropertyListenerBlock(
                kAudioObjectSystemObject as AudioObjectID,
                NonNull::from(&mut self.address),
                Some(&self.queue),
                block2::RcBlock::as_ptr(&self.listener),
            )
        };
        if status != 0 {
            eprintln!("remove default output device listener failed: OSStatus {status}");
            return;
        }
        // Listener blocks are delivered asynchronously on this serial queue.
        // A synchronous no-op runs after every previously submitted callback,
        // so the captured sender cannot be released while one is executing.
        self.queue.exec_sync(|| {});
    }
}

/// A private aggregate device carrying the tap, clocked by the default
/// output device (which appears as both main sub-device and sub-device).
fn create_aggregate(desc: &CATapDescription, output_uid: &str) -> Result<AudioObjectID> {
    let out_uid = NSString::from_str(output_uid);
    // Core Audio destroys aggregate devices asynchronously. Reusing one UID
    // immediately can therefore collide with the previous device, including
    // during rollback after a failed switch.
    // SAFETY: `desc` is a live CATapDescription and both getters return
    // Objective-C objects owned for the duration of this expression.
    let tap_uid = unsafe { desc.UUID().UUIDString() };
    let aggregate_uid = format!(
        "dev.wagomu.kikimimic.speaker-lane.{}.{}",
        tap_uid,
        NEXT_AGGREGATE_ID.fetch_add(1, Ordering::Relaxed)
    );
    let sub_dev = NSMutableDictionary::<NSString, AnyObject>::new();
    let sub_devs = unsafe {
        set(&sub_dev, kAudioSubDeviceUIDKey, &out_uid);
        NSArray::from_retained_slice(&[sub_dev])
    };
    let sub_tap = NSMutableDictionary::<NSString, AnyObject>::new();
    let taps = unsafe {
        set(&sub_tap, kAudioSubTapUIDKey, &desc.UUID().UUIDString());
        set(
            &sub_tap,
            kAudioSubTapDriftCompensationKey,
            &NSNumber::new_bool(true),
        );
        NSArray::from_retained_slice(&[sub_tap])
    };

    let agg = NSMutableDictionary::<NSString, AnyObject>::new();
    unsafe {
        set(
            &agg,
            kAudioAggregateDeviceNameKey,
            &NSString::from_str("Kikimimic speaker lane"),
        );
        set(
            &agg,
            kAudioAggregateDeviceUIDKey,
            &NSString::from_str(&aggregate_uid),
        );
        set(
            &agg,
            kAudioAggregateDeviceIsPrivateKey,
            &NSNumber::new_bool(true),
        );
        set(
            &agg,
            kAudioAggregateDeviceIsStackedKey,
            &NSNumber::new_bool(false),
        );
        set(
            &agg,
            kAudioAggregateDeviceTapAutoStartKey,
            &NSNumber::new_bool(true),
        );
        set(&agg, kAudioAggregateDeviceMainSubDeviceKey, &out_uid);
        set(&agg, kAudioAggregateDeviceSubDeviceListKey, &sub_devs);
        set(&agg, kAudioAggregateDeviceTapListKey, &taps);
    }
    // NSDictionary is toll-free bridged to CFDictionary.
    let cf: &CFDictionary = unsafe { &*(Retained::as_ptr(&agg) as *const CFDictionary) };
    let mut agg_id: AudioObjectID = 0;
    check(
        unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut agg_id)) },
        "create aggregate device",
    )?;
    Ok(agg_id)
}

fn default_output_device() -> Result<OutputDevice> {
    let mut dev: AudioObjectID = 0;
    read_property(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyDefaultOutputDevice,
        &mut dev,
        "get default output device",
    )?;
    let uid = read_string_property(dev, kAudioDevicePropertyDeviceUID, "get output device UID")?;
    let name = read_string_property(dev, kAudioObjectPropertyName, "get output device name")
        .map(|value| value.to_string())
        .unwrap_or_else(|_| uid.to_string());
    Ok(OutputDevice {
        uid: uid.to_string(),
        name,
    })
}

fn read_string_property(
    object: AudioObjectID,
    selector: u32,
    what: &str,
) -> Result<Retained<NSString>> {
    // CFStringRef, toll-free bridged to NSString; the property returns +1
    // and Retained::from_raw takes over that reference.
    let mut value: *mut NSString = std::ptr::null_mut();
    read_property(object, selector, &mut value, what)?;
    // SAFETY: Core Audio returns a retained CFStringRef for these string
    // properties; NSString is toll-free bridged and takes over that +1 here.
    unsafe { Retained::from_raw(value) }.with_context(|| format!("{what} returned null"))
}

fn read_property<T>(object: AudioObjectID, selector: u32, out: &mut T, what: &str) -> Result<()> {
    let mut addr = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut size = std::mem::size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&mut addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new(out as *mut T as *mut c_void).expect("out is a live reference"),
        )
    };
    check(status, what)
}

/// NSDictionary insert with a CoreAudio `&CStr` key.
unsafe fn set(
    dict: &NSMutableDictionary<NSString, AnyObject>,
    key: &std::ffi::CStr,
    value: &AnyObject,
) {
    let key = NSString::from_str(key.to_str().expect("CoreAudio keys are ASCII"));
    dict.setObject_forKey(value, ProtocolObject::from_ref(&*key));
}

/// The HAL objects outlive Rust scopes unless explicitly destroyed, and a
/// leaked tap keeps showing up in the audio system. Guards make the teardown
/// order (IO → aggregate → tap) survive early returns.
struct IoProcGuard {
    agg_id: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
}

impl Drop for IoProcGuard {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.agg_id, self.proc_id);
            AudioDeviceDestroyIOProcID(self.agg_id, self.proc_id);
        }
    }
}

struct AggregateGuard(AudioObjectID);

impl Drop for AggregateGuard {
    fn drop(&mut self) {
        unsafe { AudioHardwareDestroyAggregateDevice(self.0) };
    }
}

struct TapGuard(AudioObjectID);

impl Drop for TapGuard {
    fn drop(&mut self) {
        unsafe { AudioHardwareDestroyProcessTap(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(uid: &str) -> OutputDevice {
        OutputDevice {
            uid: uid.to_owned(),
            name: format!("Device {uid}"),
        }
    }

    #[test]
    fn repeated_uid_is_deduplicated_but_return_to_capturing_is_reported() {
        let mut tracker = OutputDeviceTracker::new(device("a"));
        assert_eq!(tracker.observe(device("a")), None);
        assert!(matches!(
            tracker.observe(device("b")),
            Some(OutputDeviceSwitchEvent::Detected { detected, .. }) if detected.uid == "b"
        ));
        assert_eq!(tracker.observe(device("b")), None);
        assert!(matches!(
            tracker.observe(device("a")),
            Some(OutputDeviceSwitchEvent::Detected {
                capturing,
                detected,
            }) if capturing.uid == "a" && detected.uid == "a"
        ));
    }

    #[test]
    fn stale_switch_approval_is_never_applied() {
        let mut tracker = OutputDeviceTracker::new(device("a"));
        let _ = tracker.observe(device("c"));
        assert_eq!(
            tracker.action_for(&OutputDeviceSwitchDecision::Switch {
                expected_uid: "b".to_owned(),
            }),
            SwitchAction::Cancel(OutputDeviceSwitchCancellation::StaleDecision)
        );
    }

    #[test]
    fn rejection_and_already_capturing_do_not_rebuild() {
        let mut tracker = OutputDeviceTracker::new(device("a"));
        let _ = tracker.observe(device("b"));
        assert_eq!(
            tracker.action_for(&OutputDeviceSwitchDecision::Cancel {
                expected_uid: "b".to_owned(),
            }),
            SwitchAction::Cancel(OutputDeviceSwitchCancellation::Rejected)
        );

        let _ = tracker.observe(device("a"));
        assert_eq!(
            tracker.action_for(&OutputDeviceSwitchDecision::Switch {
                expected_uid: "a".to_owned(),
            }),
            SwitchAction::Cancel(OutputDeviceSwitchCancellation::AlreadyCapturing)
        );
    }

    #[test]
    fn matching_approval_requests_a_rebuild() {
        let mut tracker = OutputDeviceTracker::new(device("a"));
        let _ = tracker.observe(device("b"));
        assert_eq!(
            tracker.action_for(&OutputDeviceSwitchDecision::Switch {
                expected_uid: "b".to_owned(),
            }),
            SwitchAction::Switch
        );
    }

    #[test]
    fn returning_to_the_captured_device_withdraws_the_question() {
        let event = OutputDeviceSwitchEvent::Detected {
            capturing: device("a"),
            detected: device("a"),
        };
        assert_eq!(source_event(event), SourceEvent::OutputSwitchWithdrawn);
    }

    #[test]
    fn a_new_default_output_is_proposed_by_uid() {
        let event = OutputDeviceSwitchEvent::Detected {
            capturing: device("a"),
            detected: device("b"),
        };
        assert!(matches!(
            source_event(event),
            SourceEvent::OutputSwitchProposed { uid, .. } if uid == "b"
        ));
    }
}
