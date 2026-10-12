//! Run-owned anonymous, physically pinned shared counters. No history or file I/O.
//! Views are unmapped while still locked; no unlocked lifetime contains metrics.
use std::{io, sync::atomic::{AtomicU64, Ordering}, os::windows::io::{OwnedHandle, AsRawHandle, FromRawHandle}};
use windows_sys::Win32::{Foundation::{INVALID_HANDLE_VALUE, HANDLE_FLAG_INHERIT, GetHandleInformation, SetHandleInformation}, Security::SECURITY_ATTRIBUTES, System::Memory::{CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, VirtualLock, FILE_MAP_ALL_ACCESS, PAGE_READWRITE, MEMORY_MAPPED_VIEW_ADDRESS}};
#[repr(C)]
struct Counters { magic: AtomicU64, capacity: AtomicU64, pending: AtomicU64, peak: AtomicU64, accepted: AtomicU64, written: AtomicU64, failure: AtomicU64 }
pub struct QueueTelemetry { view: MEMORY_MAPPED_VIEW_ADDRESS, handle: OwnedHandle }
// SAFETY: mapped storage contains only aligned atomic counters, and remains mapped until the last Arc drops.
unsafe impl Send for QueueTelemetry {}
unsafe impl Sync for QueueTelemetry {}
impl QueueTelemetry {
    pub fn create(capacity: usize) -> io::Result<Self> {
        let security = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: std::ptr::null_mut(), bInheritHandle:1 };
        // SAFETY: anonymous paging-file mapping with valid attributes and no name.
        let raw = unsafe { CreateFileMappingW(INVALID_HANDLE_VALUE, &security, PAGE_READWRITE, 0, std::mem::size_of::<Counters>() as u32, std::ptr::null()) };
        if raw.is_null() { return Err(io::Error::last_os_error()); }
        // SAFETY: freshly created handle has one owner.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let result = Self::map(handle)?;
        // SAFETY: new zero-filled mapping, properly aligned and exclusively initialized before inheritance.
        unsafe { result.view.Value.cast::<Counters>().write(Counters { magic: AtomicU64::new(0x5345455251554555), capacity:AtomicU64::new(capacity as u64), pending:AtomicU64::new(0), peak:AtomicU64::new(0), accepted:AtomicU64::new(0), written:AtomicU64::new(0), failure:AtomicU64::new(0) }); }
        Ok(result)
    }
    fn map(handle:OwnedHandle) -> io::Result<Self> {
        // SAFETY: mapping handle is owned; requested view is exactly the counter layout.
        let view = unsafe { MapViewOfFile(handle.as_raw_handle(), FILE_MAP_ALL_ACCESS, 0, 0, std::mem::size_of::<Counters>()) };
        if view.Value.is_null() { return Err(io::Error::last_os_error()); }
        // Pin before initializing or reading any counter. A paging-capable view is
        // never admitted: VirtualLock guarantees these pages cannot be paged out.
        if unsafe { VirtualLock(view.Value, std::mem::size_of::<Counters>()) } == 0 {
            let error = io::Error::last_os_error();
            unsafe { UnmapViewOfFile(view); }
            return Err(error);
        }
        Ok(Self { view, handle })
    }
    pub fn inherited() -> Option<std::sync::Arc<Self>> {
        let value=std::env::var("UI_OBSERVATION_V1_QUEUE_HANDLE").ok()?;
        std::env::remove_var("UI_OBSERVATION_V1_QUEUE_HANDLE");
        let raw=value.parse::<usize>().ok()? as *mut core::ffi::c_void;
        if raw.is_null() || raw == INVALID_HANDLE_VALUE { return None; }
        let mut flags=0;
        // SAFETY: inspect the numeric handle before taking ownership; only explicitly inherited handles are accepted.
        if unsafe { GetHandleInformation(raw,&mut flags) } == 0 || flags & HANDLE_FLAG_INHERIT == 0 { return None; }
        if unsafe { SetHandleInformation(raw,HANDLE_FLAG_INHERIT,0) } == 0 { return None; }
        let handle=unsafe { OwnedHandle::from_raw_handle(raw) };
        let result=Self::map(handle).ok()?;
        if result.counters().magic.load(Ordering::Acquire) != 0x5345455251554555 { return None; }
        Some(std::sync::Arc::new(result))
    }
    pub fn handle(&self) -> usize { self.handle.as_raw_handle() as usize }
    fn counters(&self) -> &Counters { unsafe { &*self.view.Value.cast::<Counters>() } }
    pub fn queued(&self,bytes:usize) { let c=self.counters(); let pending=c.pending.fetch_add(bytes as u64,Ordering::AcqRel)+bytes as u64; c.peak.fetch_max(pending,Ordering::AcqRel); c.accepted.fetch_add(1,Ordering::AcqRel); }
    pub fn released(&self,bytes:usize) { self.counters().pending.fetch_sub(bytes as u64,Ordering::AcqRel); }
    pub fn written(&self) { self.counters().written.fetch_add(1,Ordering::AcqRel); }
    pub fn failed(&self,code:u64) { let _ = self.counters().failure.compare_exchange(0,code,Ordering::AcqRel,Ordering::Acquire); }
    pub fn snapshot(&self) -> super::QueueSnapshot { let c=self.counters(); super::QueueSnapshot { capacity_bytes:c.capacity.load(Ordering::Acquire), pending_bytes:c.pending.load(Ordering::Acquire), peak_pending_bytes:c.peak.load(Ordering::Acquire), records_accepted:c.accepted.load(Ordering::Acquire), records_written:c.written.load(Ordering::Acquire), failure_code:c.failure.load(Ordering::Acquire) } }
}
impl Drop for QueueTelemetry { fn drop(&mut self) { unsafe { UnmapViewOfFile(self.view); } } }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_backlog_counts_pending_memory_and_preserves_failure() {
        let telemetry=QueueTelemetry::create(2usize*1024*1024*1024).unwrap();
        assert_eq!(telemetry.snapshot().pending_bytes,0);
        telemetry.queued(1024); telemetry.queued(2048);
        assert_eq!(telemetry.snapshot().pending_bytes,3072);
        telemetry.written(); telemetry.released(1024);
        let snapshot=telemetry.snapshot();
        assert_eq!(snapshot.peak_pending_bytes,3072);
        assert_eq!(snapshot.records_accepted,2);
        assert_eq!(snapshot.records_written,1);
        telemetry.failed(2); telemetry.released(2048);
        assert_eq!(telemetry.snapshot().pending_bytes,0);
        assert_eq!(telemetry.snapshot().failure_code,2);
    }
    #[test]
    fn child_emits_bulk_stream() {
        if std::env::var("UI_OBSERVATION_TEST_CHILD").as_deref()!=Ok("bulk") { return; }
        let runtime=crate::ObservationRuntime::from_inherited_v1();
        for n in 0..100_000 {
            assert_eq!(runtime.record(n/16,ui_observation_protocol::ElementKey::parse("window.main").unwrap(),
                ui_observation_protocol::ObservationBody::StateRevision(ui_observation_protocol::StateRevision {
                    target:ui_observation_protocol::ElementKey::parse(format!("fixture.control.{}",n%512)).unwrap(), revision:n, cause_action_sequence:None,
                })),crate::RecordStatus::Recorded);
        }
        assert_eq!(runtime.finish(6250,ui_observation_protocol::ElementKey::parse("window.main").unwrap()),crate::RecordStatus::Recorded);
    }
    #[test]
    fn inherited_mapping_reports_the_full_cross_process_bulk_stream() {
        let nonce="0123456789abcdef0123456789abcdef";
        let mut command=crate::WindowsObservedCommand::new(std::env::current_exe().unwrap());
        command.args(["--exact","queue_windows::tests::child_emits_bulk_stream","--nocapture"]).env("UI_OBSERVATION_TEST_CHILD","bulk");
        let (mut process,stream)=command.spawn(nonce).unwrap().into_parts();
        let probe=process.queue_probe();
        let mut stream=stream.buffered();
        let mut memory=crate::MemoryObserver::new(nonce,crate::MemoryLimits { keys:1024,bytes:128*1024*1024 }).unwrap();
        let mut count=0;
        while let Some(frame)=stream.read_decoded().unwrap() { assert_eq!(frame.observation().sequence,count); memory.push_decoded(frame).unwrap(); count+=1; }
        memory.close().unwrap();
        assert!(process.wait().unwrap().success());
        assert_eq!(count,100_001);
        let counters=process.queue_snapshot();
        assert_eq!(counters.records_accepted,count);
        assert_eq!(counters.records_written,count);
        assert_eq!(counters.pending_bytes,0);
        assert!(counters.peak_pending_bytes>0);
        assert_eq!(counters.failure_code,0);
        assert_eq!(probe().records_written,count);
        drop(process);
        assert_eq!(probe().records_written,count);
    }

}
