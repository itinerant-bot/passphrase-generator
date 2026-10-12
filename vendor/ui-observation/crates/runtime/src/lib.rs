#![deny(unsafe_code)]

//! Allocation-bounded, one-way observation delivery.
//!
//! [`ObservationRuntime::from_inherited_v1`] consumes the protocol environment
//! variables and fails to an inert runtime unless the supplied OS resource is a
//! valid inherited write-only stream.

#[cfg(unix)]
#[allow(unsafe_code)]
mod inherited_unix;
#[cfg(windows)]
#[allow(unsafe_code)]
mod inherited_windows;
#[cfg(windows)]
#[allow(unsafe_code)]
mod observer_windows;
#[cfg(windows)]
#[allow(unsafe_code)]
mod queue_windows;

#[cfg(windows)]
pub use observer_windows::{ObservedWindowsChild, ObservedWindowsProcess, WindowsObservedCommand};

use std::sync::{mpsc, Arc, Mutex};
use ui_observation_protocol::{
    ElementKey, FrameWriter, Observation, ObservationBody, RecordDropped, StreamEnd,
};

// Lazily allocated byte budget; startup bursts are not limited by record count.
const DEFAULT_CAPACITY: usize = 2usize * 1024 * 1024 * 1024;

mod memory;
pub use memory::{MemoryBudget, MemoryLimits, MemoryObserver, MemoryReservation, MemorySnapshot};

/// Observer-side launch result on Unix. The product receives only the write
/// descriptor; the caller owns the framed read stream and child lifecycle.
#[cfg(unix)]
pub struct ObservedChild {
    pub child: std::process::Child,
    pub stream: ui_observation_protocol::FrameReader<std::fs::File>,
}

/// Launches an ordinary command with a fresh observer-owned one-way channel.
///
/// The command's existing program/arguments/cwd/environment are preserved. The
/// three protocol variables are replaced with the exact v1 values.
#[cfg(unix)]
pub fn launch_observed(
    command: &mut std::process::Command,
    nonce: &str,
) -> std::io::Result<ObservedChild> {
    if !ui_observation_protocol::is_nonce(nonce) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "nonce must be 128-bit lowercase hexadecimal",
        ));
    }
    use std::os::fd::AsRawFd;
    let (reader, writer) = inherited_unix::observer_pipe()?;
    command
        .env("UI_OBSERVATION_V1_FD", writer.as_raw_fd().to_string())
        .env("UI_OBSERVATION_V1_NONCE", nonce)
        .env_remove("UI_OBSERVATION_V1_HANDLE");
    let child = command.spawn()?;
    drop(writer);
    Ok(ObservedChild {
        child,
        stream: ui_observation_protocol::FrameReader::new(reader),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordStatus {
    Recorded,
    Disabled,
    Dropped,
}

/// A non-blocking destination. Implementations must not retain references to
/// the supplied observation.
pub trait ObservationSink: Send + Sync + 'static {
    fn try_record(&self, observation: Observation) -> RecordStatus;
    fn drop_reason(&self) -> ui_observation_protocol::DropReason { ui_observation_protocol::DropReason::QueueFull }
}

#[derive(Default)]
pub struct NoopSink;

impl ObservationSink for NoopSink {
    fn try_record(&self, _observation: Observation) -> RecordStatus {
        RecordStatus::Disabled
    }
}

struct RuntimeState {
    next_sequence: u64,
    dropped: u64,
    dropped_by_reason: [u64;4],
    finished: bool,
    last_frame_sequence: u64,
}

struct RuntimeInner {
    nonce: String,
    sink: Arc<dyn ObservationSink>,
    state: Mutex<RuntimeState>,
}

/// Cheaply cloneable handle installed in a UI context.
#[derive(Clone)]
pub struct ObservationRuntime(Arc<RuntimeInner>);

impl Default for ObservationRuntime {
    fn default() -> Self {
        Self::disabled()
    }
}

impl ObservationRuntime {
    #[must_use]
    pub fn disabled() -> Self {
        Self::with_sink("00000000000000000000000000000000", Arc::new(NoopSink))
    }

    /// Creates a runtime around a custom sink, primarily for embedded observers
    /// and deterministic headless harnesses.
    #[must_use]
    pub fn with_sink(nonce: impl Into<String>, sink: Arc<dyn ObservationSink>) -> Self {
        Self(Arc::new(RuntimeInner {
            nonce: nonce.into(),
            sink,
            state: Mutex::new(RuntimeState {
                next_sequence: 0,
                dropped: 0,
                dropped_by_reason:[0;4],
                finished: false,
                last_frame_sequence: 0,
            }),
        }))
    }

    /// Adds a second non-blocking destination while preserving any inherited
    /// observer. This lets products retain cheap operational measurements even
    /// when no external observation process was present at launch.
    #[must_use]
    pub fn with_additional_sink(self, sink: Arc<dyn ObservationSink>) -> Self {
        let nonce = self.0.nonce.clone();
        let inherited = self.0.sink.clone();
        // This method replaces the runtime before it is installed. Suppress
        // the consumed wrapper's Drop terminator; the replacement owns both
        // destinations and emits the one authoritative terminator.
        if let Ok(mut state) = self.0.state.lock() {
            state.finished = true;
        }
        Self::with_sink(
            nonce,
            Arc::new(FanoutSink {
                primary: sink,
                secondary: inherited,
            }),
        )
    }

    /// Discovers and consumes the v1 inherited channel variables. Invalid or
    /// incomplete configuration always produces an inert runtime.
    #[must_use]
    pub fn from_inherited_v1() -> Self {
        let fd = std::env::var_os("UI_OBSERVATION_V1_FD");
        let handle = std::env::var_os("UI_OBSERVATION_V1_HANDLE");
        let nonce = std::env::var_os("UI_OBSERVATION_V1_NONCE");
        std::env::remove_var("UI_OBSERVATION_V1_FD");
        std::env::remove_var("UI_OBSERVATION_V1_HANDLE");
        std::env::remove_var("UI_OBSERVATION_V1_NONCE");

        let Some(nonce) = nonce.and_then(|value| value.into_string().ok()) else {
            return Self::disabled();
        };
        if !ui_observation_protocol::is_nonce(&nonce) {
            return Self::disabled();
        }

        #[cfg(unix)]
        {
            if handle.is_some() {
                return Self::disabled();
            }
            let Some(fd) = fd.and_then(|value| value.into_string().ok()) else {
                return Self::disabled();
            };
            let Ok(fd) = fd.parse::<i32>() else {
                return Self::disabled();
            };
            inherited_unix::take_write_stream(fd)
                .and_then(|stream| ChannelSink::new(stream, DEFAULT_CAPACITY))
                .map(|sink| Self::with_sink(nonce, Arc::new(sink)))
                .unwrap_or_else(Self::disabled)
        }

        #[cfg(windows)]
        {
            if fd.is_some() {
                return Self::disabled();
            }
            let Some(handle) = handle.and_then(|value| value.into_string().ok()) else {
                return Self::disabled();
            };
            let Ok(handle) = handle.parse::<usize>() else {
                return Self::disabled();
            };
            inherited_windows::take_write_stream(handle)
                .and_then(|stream| ChannelSink::new(stream, DEFAULT_CAPACITY))
                .map(|sink| Self::with_sink(nonce, Arc::new(sink)))
                .unwrap_or_else(Self::disabled)
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = (fd, handle, nonce);
            Self::disabled()
        }
    }

    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.0.nonce
    }

    /// Attempts to enqueue one fact without waiting for I/O or queue space.
    pub fn record(
        &self,
        frame_sequence: u64,
        window_key: ElementKey,
        body: ObservationBody,
    ) -> RecordStatus {
        let Ok(mut state) = self.0.state.lock() else {
            return RecordStatus::Dropped;
        };
        if state.finished {
            return RecordStatus::Dropped;
        }
        state.last_frame_sequence = state.last_frame_sequence.max(frame_sequence);

        for (index, reason) in [ui_observation_protocol::DropReason::QueueFull, ui_observation_protocol::DropReason::Oversized, ui_observation_protocol::DropReason::Serialization, ui_observation_protocol::DropReason::ChannelClosed].into_iter().enumerate() {
            let count = state.dropped_by_reason[index];
            if count == 0 { continue; }
            let dropped = Observation::new(self.0.nonce.clone(), state.next_sequence, frame_sequence, window_key.clone(), ObservationBody::RecordDropped(RecordDropped { reason, count }));
            if let Ok(dropped) = dropped {
                if self.0.sink.try_record(dropped) == RecordStatus::Recorded {
                    state.next_sequence = state.next_sequence.saturating_add(1);
                    state.dropped = state.dropped.saturating_sub(count);
                    state.dropped_by_reason[index] = 0;
                }
            }
        }

        let Ok(observation) = Observation::new(
            self.0.nonce.clone(),
            state.next_sequence,
            frame_sequence,
            window_key,
            body,
        ) else {
            state.dropped = state.dropped.saturating_add(1);
            state.dropped_by_reason[2] = state.dropped_by_reason[2].saturating_add(1);
            return RecordStatus::Dropped;
        };
        let status = self.0.sink.try_record(observation);
        match status {
            RecordStatus::Recorded => state.next_sequence = state.next_sequence.saturating_add(1),
            RecordStatus::Dropped => {
                state.dropped = state.dropped.saturating_add(1);
                let index = match self.0.sink.drop_reason() {
                    ui_observation_protocol::DropReason::QueueFull=>0,
                    ui_observation_protocol::DropReason::Oversized=>1,
                    ui_observation_protocol::DropReason::Serialization=>2,
                    ui_observation_protocol::DropReason::ChannelClosed=>3,
                };
                state.dropped_by_reason[index] = state.dropped_by_reason[index].saturating_add(1);
            },
            RecordStatus::Disabled => {}
        }
        status
    }

    /// Enqueues the terminal stream fact. Subsequent records are rejected.
    pub fn finish(&self, frame_sequence: u64, window_key: ElementKey) -> RecordStatus {
        let Ok(mut state) = self.0.state.lock() else {
            return RecordStatus::Dropped;
        };
        if state.finished {
            return RecordStatus::Dropped;
        }
        state.last_frame_sequence = state.last_frame_sequence.max(frame_sequence);
        let Ok(observation) = Observation::new(
            self.0.nonce.clone(),
            state.next_sequence,
            frame_sequence,
            window_key,
            ObservationBody::StreamEnd(StreamEnd {
                records_emitted: state.next_sequence.saturating_add(1),
                records_dropped: state.dropped,
            }),
        ) else {
            return RecordStatus::Dropped;
        };
        let status = self.0.sink.try_record(observation);
        if status == RecordStatus::Recorded {
            state.next_sequence = state.next_sequence.saturating_add(1);
            state.finished = true;
        }
        status
    }
}

struct FanoutSink {
    primary: Arc<dyn ObservationSink>,
    secondary: Arc<dyn ObservationSink>,
}

impl ObservationSink for FanoutSink {
    fn try_record(&self, observation: Observation) -> RecordStatus {
        let secondary = self.secondary.try_record(observation.clone());
        let primary = self.primary.try_record(observation);
        match (primary, secondary) {
            (RecordStatus::Recorded, _) | (_, RecordStatus::Recorded) => RecordStatus::Recorded,
            (RecordStatus::Dropped, _) | (_, RecordStatus::Dropped) => RecordStatus::Dropped,
            _ => RecordStatus::Disabled,
        }
    }
}

impl Drop for RuntimeInner {
    fn drop(&mut self) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.finished {
            return;
        }
        let Ok(window_key) = ElementKey::parse("window.main") else {
            return;
        };
        let Ok(observation) = Observation::new(
            self.nonce.clone(),
            state.next_sequence,
            state.last_frame_sequence,
            window_key,
            ObservationBody::StreamEnd(StreamEnd {
                records_emitted: state.next_sequence.saturating_add(1),
                records_dropped: state.dropped,
            }),
        ) else {
            return;
        };
        if self.sink.try_record(observation) == RecordStatus::Recorded {
            state.finished = true;
        }
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct QueueSnapshot {
    pub capacity_bytes:u64, pub pending_bytes:u64, pub peak_pending_bytes:u64,
    pub records_accepted:u64, pub records_written:u64, pub failure_code:u64,
}
struct QueuedObservation {
    frame: ui_observation_protocol::PreparedFrame,
    #[cfg(windows)]
    telemetry: Option<Arc<queue_windows::QueueTelemetry>>,
    bytes: usize,
    used: Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for QueuedObservation {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, std::sync::atomic::Ordering::AcqRel);
        #[cfg(windows)]
        if let Some(telemetry) = &self.telemetry { telemetry.released(self.bytes); }
    }
}
struct ChannelSink {
    sender: Mutex<Option<mpsc::Sender<QueuedObservation>>>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    used: Arc<std::sync::atomic::AtomicUsize>,
    capacity: usize,
    last_failure: std::sync::atomic::AtomicU8,
    #[cfg(windows)]
    telemetry: Option<Arc<queue_windows::QueueTelemetry>>,
}

impl ChannelSink {
    fn new(stream: std::fs::File, capacity: usize) -> Option<Self> {
        let (sender, receiver) = mpsc::channel::<QueuedObservation>();
        let used = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        #[cfg(windows)]
        let telemetry = queue_windows::QueueTelemetry::inherited();
        #[cfg(windows)]
        let worker_telemetry = telemetry.clone();
        let worker = std::thread::Builder::new()
            .name("ui-observation-writer".into())
            .spawn(move || {
                let Ok(mut writer) = FrameWriter::new(std::io::BufWriter::with_capacity(65536, stream))
                    .with_payload_limit(capacity) else { return; };
                while let Ok(first) = receiver.recv() {
                    let mut batch=Vec::with_capacity(1024);
                    let mut ended=first.frame.ended();
                    batch.push(first);
                    while batch.len()<1024 && !ended {
                        match receiver.try_recv() {
                            Ok(next)=> { ended=next.frame.ended(); batch.push(next); },
                            Err(_)=>break,
                        }
                    }
                    for queued in &batch {
                        if writer.write_prepared(&queued.frame).is_err() {
                            #[cfg(windows)]
                            if let Some(t)=&worker_telemetry { t.failed(2); }
                            return;
                        }
                    }
                    if writer.flush().is_err() {
                        #[cfg(windows)]
                        if let Some(t)=&worker_telemetry { t.failed(3); }
                        return;
                    }
                    // Pending credit includes all buffered writes until the pipe
                    // accepts the complete batch. Reader validation is separate.
                    #[cfg(windows)]
                    if let Some(t)=&worker_telemetry { for _ in &batch { t.written(); } }
                    drop(batch);
                    if ended { return; }
                }
                let _ = writer.flush();
            })
            .ok()?;
        Some(Self { sender: Mutex::new(Some(sender)), worker: Mutex::new(Some(worker)), used, capacity, last_failure:std::sync::atomic::AtomicU8::new(0), #[cfg(windows)] telemetry })
    }
}

impl ObservationSink for ChannelSink {
    fn try_record(&self, observation: Observation) -> RecordStatus {
        use std::sync::atomic::Ordering;
        // Queue the canonical payload itself, rather than a second owned model
        // requiring validation and serialization again on the writer thread.
        let frame = match ui_observation_protocol::PreparedFrame::with_payload_limit(&observation, self.capacity) {
            Ok(frame)=>frame,
            Err(error)=> { self.last_failure.store(if matches!(error, ui_observation_protocol::FrameError::TooLarge) {1} else {2},Ordering::Release); return RecordStatus::Dropped; }
        };
        let bytes = frame.allocated_bytes().saturating_add(std::mem::size_of::<QueuedObservation>() + 64);
        if self.used.fetch_update(Ordering::AcqRel, Ordering::Acquire, |used|
            used.checked_add(bytes).filter(|next| *next <= self.capacity)).is_err() {
            self.last_failure.store(0,Ordering::Release);
            #[cfg(windows)]
            if let Some(t) = &self.telemetry { t.failed(1); }
            return RecordStatus::Dropped;
        }
        #[cfg(windows)]
        if let Some(t) = &self.telemetry { t.queued(bytes); }
        let queued = QueuedObservation { frame, bytes, used: self.used.clone(), #[cfg(windows)] telemetry:self.telemetry.clone() };
        let Ok(sender) = self.sender.lock() else { self.last_failure.store(3,Ordering::Release); return RecordStatus::Dropped; };
        let Some(sender) = sender.as_ref() else { self.last_failure.store(3,Ordering::Release); return RecordStatus::Dropped; };
        match sender.send(queued) {
            Ok(()) => RecordStatus::Recorded,
            Err(_) => { self.last_failure.store(3,Ordering::Release); RecordStatus::Dropped },
        }
    }
    fn drop_reason(&self) -> ui_observation_protocol::DropReason {
        match self.last_failure.load(std::sync::atomic::Ordering::Acquire) {
            1=>ui_observation_protocol::DropReason::Oversized,
            2=>ui_observation_protocol::DropReason::Serialization,
            3=>ui_observation_protocol::DropReason::ChannelClosed,
            _=>ui_observation_protocol::DropReason::QueueFull,
        }
    }
}

impl Drop for ChannelSink {
    fn drop(&mut self) {
        if let Ok(sender) = self.sender.get_mut() {
            sender.take();
        }
        if let Ok(worker) = self.worker.get_mut() {
            if let Some(worker) = worker.take() {
                let _ = worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use ui_observation_protocol::{FrameReader, StreamValidator};

    struct Capture(Mutex<Vec<Observation>>);
    impl ObservationSink for Capture {
        fn try_record(&self, observation: Observation) -> RecordStatus {
            self.0.lock().unwrap().push(observation);
            RecordStatus::Recorded
        }
    }

    struct DropFirst {
        calls: Mutex<u64>,
        captured: Mutex<Vec<Observation>>,
    }

    impl ObservationSink for DropFirst {
        fn try_record(&self, observation: Observation) -> RecordStatus {
            let mut calls = self.calls.lock().unwrap();
            *calls += 1;
            if *calls == 1 {
                RecordStatus::Dropped
            } else {
                self.captured.lock().unwrap().push(observation);
                RecordStatus::Recorded
            }
        }
    }

    #[test]
    fn disabled_runtime_is_inert() {
        let runtime = ObservationRuntime::disabled();
        assert_eq!(
            runtime.finish(0, ElementKey::parse("window.main").unwrap()),
            RecordStatus::Disabled
        );
    }

    #[test]
    fn custom_sink_preserves_sequence_and_end() {
        let sink = Arc::new(Capture(Mutex::new(Vec::new())));
        let runtime =
            ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef", sink.clone());
        assert_eq!(
            runtime.finish(7, ElementKey::parse("window.main").unwrap()),
            RecordStatus::Recorded
        );
        let records = sink.0.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sequence, 0);
        assert!(matches!(records[0].record, ObservationBody::StreamEnd(_)));
    }

    #[test]
    fn additional_sink_preserves_both_destinations() {
        let original = Arc::new(Capture(Mutex::new(Vec::new())));
        let additional = Arc::new(Capture(Mutex::new(Vec::new())));
        let runtime =
            ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef", original.clone())
                .with_additional_sink(additional.clone());
        assert_eq!(
            runtime.finish(8, ElementKey::parse("window.main").unwrap()),
            RecordStatus::Recorded
        );
        assert_eq!(original.0.lock().unwrap().len(), 1);
        assert_eq!(additional.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn final_runtime_handle_emits_a_clean_end_automatically() {
        let sink = Arc::new(Capture(Mutex::new(Vec::new())));
        let runtime =
            ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef", sink.clone());
        assert_eq!(
            runtime.record(
                11,
                ElementKey::parse("window.main").unwrap(),
                ObservationBody::Window(ui_observation_protocol::WindowObservation {
                    title: ui_observation_protocol::Content::public("Observed app").unwrap(),
                    content_rect: ui_observation_protocol::Rect::from_f32(0.0, 0.0, 640.0, 480.0,)
                        .unwrap(),
                    viewport_rect: ui_observation_protocol::Rect::from_f32(0.0, 0.0, 640.0, 480.0,)
                        .unwrap(),
                    scale: ui_observation_protocol::Fixed6::ONE,
                }),
            ),
            RecordStatus::Recorded
        );
        let clone = runtime.clone();
        drop(runtime);
        assert_eq!(sink.0.lock().unwrap().len(), 1);
        drop(clone);

        let records = sink.0.lock().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].frame_sequence, 11);
        assert_eq!(records[1].sequence, 1);
        assert!(matches!(records[1].record, ObservationBody::StreamEnd(_)));
    }

    #[test]
    fn retry_after_drop_does_not_create_a_sequence_gap() {
        let sink = Arc::new(DropFirst {
            calls: Mutex::new(0),
            captured: Mutex::new(Vec::new()),
        });
        let runtime =
            ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef", sink.clone());
        let window = ElementKey::parse("window.main").unwrap();
        assert_eq!(runtime.finish(1, window.clone()), RecordStatus::Dropped);
        assert_eq!(runtime.finish(2, window), RecordStatus::Recorded);
        let records = sink.captured.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].sequence, 0);
    }

    #[cfg(windows)]
    #[test]
    fn windows_observer_launch_delivers_a_valid_child_stream() {
        const CHILD_MARKER: &str = "UI_OBSERVATION_RUNTIME_WINDOWS_TEST_CHILD";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let runtime = ObservationRuntime::from_inherited_v1();
            assert_eq!(
                runtime.finish(19, ElementKey::parse("window.main").unwrap()),
                RecordStatus::Recorded
            );
            return;
        }

        let executable = std::env::current_exe().unwrap();
        let mut command = WindowsObservedCommand::new(executable);
        command
            .arg("--exact")
            .arg("tests::windows_observer_launch_delivers_a_valid_child_stream")
            .arg("--nocapture")
            .env(CHILD_MARKER, "1");
        let mut child = command.spawn("0123456789abcdef0123456789abcdef").unwrap();
        let record = child.stream.read().unwrap().unwrap();
        assert_eq!(record.frame_sequence, 19);
        assert!(matches!(record.record, ObservationBody::StreamEnd(_)));
        assert_eq!(child.stream.read().unwrap(), None);
        assert!(child.wait().unwrap().success());
    }

    #[cfg(unix)]
    #[test]
    fn channel_sink_delivers_a_large_valid_region_under_its_existing_ram_capacity() {
        use ui_observation_protocol::{Fixed6,Fraction,Rect,RegionObservation,Role};
        let (reader,writer)=inherited_unix::pipe_pair();
        let sink=Arc::new(ChannelSink::new(writer,4*1024*1024).unwrap());
        let runtime=ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef",sink.clone());
        let rect=Rect::new(Fixed6::ZERO,Fixed6::ZERO,Fixed6::ONE,Fixed6::ONE).unwrap();
        let body=ObservationBody::Region(RegionObservation{
            key:ElementKey::parse("fixture.region").unwrap(),role:Role::ScrollView,
            accessible_name:None,parent:None,layout_rect:rect,clip_rect:rect,viewport_rect:rect,
            window_content_rect:rect,visible_rect:rect,visible_fraction:Fraction::ONE,
            content_extent_x:Fixed6::ONE,content_extent_y:Fixed6::ONE,
            offset_x:Fixed6::ZERO,offset_y:Fixed6::ZERO,scroll_range_x:Fixed6::ZERO,scroll_range_y:Fixed6::ZERO,
            exposed_children:(0..3000).map(|i|ElementKey::parse(format!("fixture.child.{i}.{}","a".repeat(32))).unwrap()).collect(),
        });
        assert_eq!(runtime.record(0,ElementKey::parse("window.main").unwrap(),body.clone()),RecordStatus::Recorded);
        assert_eq!(runtime.finish(0,ElementKey::parse("window.main").unwrap()),RecordStatus::Recorded);
        let mut reader=FrameReader::new(reader).with_payload_limit(4*1024*1024).unwrap().buffered();
        let mut validator=StreamValidator::new("0123456789abcdef0123456789abcdef");
        let first=reader.read().unwrap().unwrap();assert_eq!(first.record,body);validator.push(&first).unwrap();
        let end=reader.read().unwrap().unwrap();validator.push(&end).unwrap();validator.finish().unwrap();
        assert!(reader.read().unwrap().is_none());assert_eq!(sink.used.load(std::sync::atomic::Ordering::Acquire),0);
    }

    #[cfg(unix)]
    #[test]
    fn bulk_burst_exceeds_old_record_cap_and_drains_every_fact() {
        let (reader, writer) = inherited_unix::pipe_pair();
        let sink = Arc::new(ChannelSink::new(writer, 64 * 1024 * 1024).unwrap());
        let runtime = ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef", sink.clone());
        // Deliberately hold the reader until a burst far beyond 4,096 records
        // has arrived. This represents startup/resize bursts, not product cases.
        for sequence in 0..20_000 {
            assert_eq!(runtime.record(sequence / 200,
                ElementKey::parse("window.main").unwrap(),
                ObservationBody::StateRevision(ui_observation_protocol::StateRevision {
                    target: ElementKey::parse("fixture.root").unwrap(),
                    revision: sequence, cause_action_sequence: None,
                })), RecordStatus::Recorded);
        }
        assert_eq!(runtime.finish(100, ElementKey::parse("window.main").unwrap()), RecordStatus::Recorded);
        let mut frames = FrameReader::new(reader).buffered();
        let mut validator = StreamValidator::new("0123456789abcdef0123456789abcdef");
        let mut count = 0;
        while let Some(record) = frames.read().unwrap() {
            assert_eq!(record.sequence, count);
            assert!(!matches!(record.record, ObservationBody::RecordDropped(_)));
            validator.push(&record).unwrap(); count += 1;
        }
        validator.finish().unwrap();
        assert_eq!(count, 20_001);
        assert_eq!(sink.used.load(std::sync::atomic::Ordering::Acquire), 0);
    }

    #[cfg(unix)]
    #[test]
    fn sustained_bulk_pipeline_preserves_order_through_repeated_reader_stalls() {
        let nonce="0123456789abcdef0123456789abcdef";
        let (reader, writer) = inherited_unix::pipe_pair();
        let sink = Arc::new(ChannelSink::new(writer, 256 * 1024 * 1024).unwrap());
        let runtime = ObservationRuntime::with_sink(nonce, sink.clone());
        let consumer = std::thread::spawn(move || {
            let mut frames = FrameReader::new(reader).buffered();
            let mut memory = MemoryObserver::new(nonce, MemoryLimits { keys: 1024, bytes:128 * 1024 * 1024 }).unwrap();
            let mut count=0;
            while let Some(frame)=frames.read_decoded().unwrap() {
                assert_eq!(frame.observation().sequence,count);
                assert!(!matches!(frame.observation().record,ObservationBody::RecordDropped(_)));
                memory.push_decoded(frame).unwrap();
                count+=1;
                if count % 4096 == 0 { std::thread::sleep(std::time::Duration::from_millis(2)); }
            }
            memory.close().unwrap();
            assert_eq!(memory.snapshot().unwrap().records_observed,count);
            count
        });
        for sequence in 0..300_000 {
            assert_eq!(runtime.record(sequence / 16, ElementKey::parse("window.main").unwrap(),
                ObservationBody::StateRevision(ui_observation_protocol::StateRevision {
                    target:ElementKey::parse(format!("fixture.control.{}",sequence%512)).unwrap(),
                    revision:sequence, cause_action_sequence:None,
                })),RecordStatus::Recorded);
        }
        assert_eq!(runtime.finish(18_750,ElementKey::parse("window.main").unwrap()),RecordStatus::Recorded);
        assert_eq!(consumer.join().unwrap(),300_001);
        assert_eq!(sink.used.load(std::sync::atomic::Ordering::Acquire),0);
    }

    #[cfg(unix)]
    #[test]
    fn channel_sink_delivers_a_decodable_clean_stream() {
        let (reader, writer) = inherited_unix::pipe_pair();
        let sink = ChannelSink::new(writer, 4096).unwrap();
        let runtime =
            ObservationRuntime::with_sink("0123456789abcdef0123456789abcdef", Arc::new(sink));
        assert_eq!(
            runtime.finish(4, ElementKey::parse("window.main").unwrap()),
            RecordStatus::Recorded
        );
        let mut frames = FrameReader::new(reader);
        let observation = frames.read().unwrap().unwrap();
        let mut validator = StreamValidator::new("0123456789abcdef0123456789abcdef");
        validator.push(&observation).unwrap();
        validator.finish().unwrap();
        assert_eq!(frames.read().unwrap(), None);
    }
}
