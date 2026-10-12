//! Bounded observer-side metrics. No filesystem, spool, snapshot writer, or
//! replay archive. Completed frames replace prior frames in anonymous memory.
use serde::{Serialize,Deserialize};
use std::collections::{BTreeMap, BTreeSet};
use ui_observation_protocol::{
    canonical_json, Observation, ObservationBody, StreamValidator,
};

/// Shared logical retention budget. Capacity is allocated only as facts arrive.
#[derive(Debug)]
pub struct MemoryBudget {
    limit: usize,
    used: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}
impl MemoryBudget {
    pub fn new(limit: usize) -> Result<std::sync::Arc<Self>, String> {
        if limit == 0 { return Err("memory budget must be positive".into()); }
        Ok(std::sync::Arc::new(Self { limit, used: 0.into(), peak: 0.into() }))
    }
    pub fn limit(&self) -> usize { self.limit }
    pub fn used(&self) -> usize { self.used.load(std::sync::atomic::Ordering::Acquire) }
    pub fn peak(&self) -> usize { self.peak.load(std::sync::atomic::Ordering::Acquire) }
    pub fn reserve(self: &std::sync::Arc<Self>, bytes: usize) -> Result<MemoryReservation,String> {
        self.resize(0,bytes)?;
        Ok(MemoryReservation { budget:self.clone(), bytes })
    }
    pub(crate) fn resize(&self, old: usize, new: usize) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        if new <= old { self.used.fetch_sub(old-new, Ordering::AcqRel); return Ok(()); }
        let added = new-old;
        let previous = self.used.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |used| used.checked_add(added).filter(|next| *next <= self.limit))
            .map_err(|_| "shared observation memory budget exceeded".to_owned())?;
        self.peak.fetch_max(previous+added, Ordering::AcqRel);
        Ok(())
    }
}

/// Capacity held by an anonymous allocation until its owner releases it.
#[derive(Debug)]
pub struct MemoryReservation { budget:std::sync::Arc<MemoryBudget>, bytes:usize }
impl MemoryReservation { pub fn bytes(&self)->usize { self.bytes } }
impl Drop for MemoryReservation {
    fn drop(&mut self) { let _ = self.budget.resize(self.bytes,0); }
}

#[derive(Clone, Copy, Debug)]
pub struct MemoryLimits {
    pub keys: usize,
    /// Logical canonical payload budget across the current and completed frame.
    pub bytes: usize,
}

impl Default for MemoryLimits {
    fn default() -> Self {
        Self {
            keys: 1024,
            bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MemorySnapshot {
    pub schema: String,
    pub run_nonce: String,
    pub completed_frame: Option<u64>,
    /// Validated record count at publication of this completed frame. Unlike
    /// records_observed, this never includes a later partial frame.
    #[serde(default)]
    pub completed_through_records: Option<u64>,
    pub records_observed: u64,
    pub stream_complete: bool,
    pub retained_payload_bytes: usize,
    pub latest: BTreeMap<String, Observation>,
}

pub struct MemoryObserver {
    budget: std::sync::Arc<MemoryBudget>,
    charged: usize,
    nonce: String,
    validator: Option<StreamValidator>,
    limits: MemoryLimits,
    known_keys: BTreeSet<String>,
    current: BTreeMap<String, Observation>,
    completed: BTreeMap<String, Observation>,
    current_frame: Option<u64>,
    sealed_frame: Option<u64>,
    completed_frame: Option<u64>,
    completed_through_records: Option<u64>,
    current_sizes: BTreeMap<String, usize>,
    current_bytes: usize,
    completed_bytes: usize,
    count: u64,
    ended: bool,
    error: Option<String>,
}

impl MemoryObserver {
    pub fn new(nonce: &str, limits: MemoryLimits) -> Result<Self, String> {
        Self::with_budget(nonce, limits, MemoryBudget::new(limits.bytes)?)
    }
    pub fn with_budget(nonce: &str, limits: MemoryLimits, budget: std::sync::Arc<MemoryBudget>) -> Result<Self, String> {
        if !ui_observation_protocol::is_nonce(nonce) || limits.keys == 0 || limits.bytes == 0 {
            return Err("memory observer requires a valid nonce and positive bounds".into());
        }
        Ok(Self {
            budget,
            charged: 0,
            nonce: nonce.into(),
            validator: Some(StreamValidator::new(nonce)),
            limits,
            known_keys: BTreeSet::new(),
            current: BTreeMap::new(),
            completed: BTreeMap::new(),
            current_frame: None,
            sealed_frame: None,
            completed_frame: None,
            completed_through_records: None,
            current_sizes: BTreeMap::new(),
            current_bytes: 0,
            completed_bytes: 0,
            count: 0,
            ended: false,
            error: None,
        })
    }

    /// A malformed, dropped or over-budget stream permanently invalidates all
    /// snapshots. Never serve a stale successful view after an observer failure.
    pub fn push(&mut self, observation: Observation) -> Result<(), String> {
        self.push_with(observation, |_| Ok(()))
    }

    /// Deliver every validated record to a bounded in-memory consumer before
    /// latest-frame coalescing. A callback error invalidates this observer;
    /// transition loss can never silently leave a successful snapshot.
    /// The callback must not block or persist observations.
    pub fn push_with(
        &mut self,
        observation: Observation,
        consume: impl FnOnce(&Observation) -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let result = self.accept(observation, None, consume);
        if let Err(error) = &result {
            self.error = Some(error.clone());
        }
        result
    }

    /// Reuse framing validation and canonical size without serializing again.
    pub fn push_decoded(&mut self, frame: ui_observation_protocol::DecodedFrame) -> Result<(), String> {
        if let Some(error) = &self.error { return Err(error.clone()); }
        let (observation, bytes) = frame.into_parts();
        let result = self.accept(observation, Some(bytes), |_| Ok(()));
        if let Err(error) = &result { self.error = Some(error.clone()); }
        result
    }

    fn accept(
        &mut self,
        observation: Observation,
        validated_bytes: Option<usize>,
        consume: impl FnOnce(&Observation) -> Result<(), String>,
    ) -> Result<(), String> {
        if self.ended {
            return Err("record after stream closure".into());
        }
        let payload_bytes = if let Some(bytes) = validated_bytes { bytes } else {
            observation.validate().map_err(|e| e.to_string())?;
            canonical_json(&observation).map_err(|e| e.to_string())?.len()
        };
        if payload_bytes > self.limits.bytes {
            return Err("observation exceeds admitted memory payload bound".into());
        }
        if let ObservationBody::StreamEnd(end)=&observation.record {
            if end.records_dropped!=0 || Some(end.records_emitted)!=observation.sequence.checked_add(1) { return Err("stream closure reports missing or inconsistent observations".into()); }
            self.validator.as_mut().ok_or("validator closed")?.push(&observation).map_err(|e|e.to_string())?;
            self.count=self.count.checked_add(1).ok_or("observation count overflow")?;
            consume(&observation)?;
            self.complete_frame();
            self.ended=true;
            return Ok(());
        }
        if self.sealed_frame == Some(observation.frame_sequence) && !matches!(observation.record,ObservationBody::StreamEnd(_)) {
            return Err("observation after completed frame".into());
        }
        if let ObservationBody::StateRevision(revision)=&observation.record {
            if revision.target.as_str()==ui_observation_protocol::FRAME_COMPLETE_KEY {
                if revision.revision!=observation.frame_sequence || revision.cause_action_sequence.is_some() {
                    return Err("invalid frame completion revision".into());
                }
                self.validator.as_mut().ok_or("validator closed")?.push(&observation).map_err(|e|e.to_string())?;
                if self.current_frame!=Some(observation.frame_sequence) { self.complete_frame(); self.current_frame=Some(observation.frame_sequence); }
                self.count=self.count.checked_add(1).ok_or("observation count overflow")?;
                consume(&observation)?;
                self.publish_frame();
                self.sealed_frame=Some(observation.frame_sequence);
                return Ok(());
            }
        }
        let key = match &observation.record {
            ObservationBody::Element(value) => value.key.as_str().to_owned(),
            ObservationBody::Region(value) => value.key.as_str().to_owned(),
            ObservationBody::Window(_) => observation.window_key.as_str().to_owned(),
            ObservationBody::ActionAck(value) => format!("ack.{}", value.target),
            ObservationBody::StateRevision(value) => format!("revision.{}", value.target),
            ObservationBody::RecordDropped(value) => {
                return Err(format!(
                    "product dropped observations: reason={:?}, count={}",
                    value.reason, value.count
                ))
            }
            ObservationBody::StreamEnd(value) => {
                if value.records_dropped != 0
                    || Some(value.records_emitted) != observation.sequence.checked_add(1)
                {
                    return Err(
                        "stream closure reports missing or inconsistent observations".into(),
                    );
                }
                "stream.end".into()
            }
        };
        if !self.known_keys.contains(&key) && self.known_keys.len() >= self.limits.keys {
            return Err("observation key retention bound exceeded".into());
        }
        self.validator
            .as_mut()
            .ok_or("validator closed")?
            .push(&observation)
            .map_err(|e| e.to_string())?;
        if self.current_frame != Some(observation.frame_sequence) {
            self.complete_frame();
            self.current_frame = Some(observation.frame_sequence);
        }
        let previous = self.current_sizes.get(&key).copied().unwrap_or(0);
        let new_bytes = self
            .current_bytes
            .saturating_sub(previous)
            .checked_add(payload_bytes)
            .ok_or("observation byte accounting overflow")?;
        if new_bytes.saturating_add(self.completed_bytes) > self.limits.bytes {
            return Err("observation byte retention bound exceeded".into());
        }
        let retained = new_bytes + self.completed_bytes;
        self.budget.resize(self.charged, retained)?;
        self.charged = retained;
        self.known_keys.insert(key.clone());
        self.current_bytes = new_bytes;
        self.count = self
            .count
            .checked_add(1)
            .ok_or("observation count overflow")?;
        self.ended = matches!(observation.record, ObservationBody::StreamEnd(_));
        consume(&observation)?;
        self.current_sizes.insert(key.clone(), payload_bytes);
        self.current.insert(key, observation);
        if self.ended {
            self.complete_frame();
        }
        Ok(())
    }

    fn complete_frame(&mut self) {
        if self.current.is_empty() { return; }
        self.publish_frame();
    }

    fn publish_frame(&mut self) {
        self.current_sizes.clear();
        self.completed = std::mem::take(&mut self.current);
        self.completed_bytes = std::mem::take(&mut self.current_bytes);
        self.completed_frame = self.current_frame;
        self.completed_through_records = Some(self.count);
        self.budget.resize(self.charged, self.completed_bytes).expect("release budget");
        self.charged = self.completed_bytes;
    }

    pub fn snapshot(&self) -> Result<MemorySnapshot, String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        Ok(MemorySnapshot {
            schema: "ui.observation.memory.v1".into(),
            run_nonce: self.nonce.clone(),
            completed_frame: self.completed_frame,
            completed_through_records: self.completed_through_records,
            records_observed: self.count,
            stream_complete: self.ended,
            retained_payload_bytes: self.current_bytes + self.completed_bytes,
            latest: self.completed.clone(),
        })
    }

    /// Validate EOF without destroying the completed view. An incomplete EOF
    /// permanently invalidates subsequent reads, just like a malformed record.
    pub fn close(&mut self) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        let result = self
            .validator
            .take()
            .ok_or_else(|| "validator closed".to_owned())
            .and_then(|validator| validator.finish().map_err(|e| e.to_string()));
        if let Err(error) = &result {
            self.error = Some(error.clone());
        }
        result
    }

    pub fn finish(mut self) -> Result<MemorySnapshot, String> {
        self.close()?;
        self.snapshot()
    }
}

impl Drop for MemoryObserver {
    fn drop(&mut self) { let _ = self.budget.resize(self.charged, 0); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui_observation_protocol::{
        DropReason, ElementKey, RecordDropped, StateRevision, StreamEnd,
    };
    const NONCE: &str = "0123456789abcdef0123456789abcdef";

    fn revision(sequence: u64, frame: u64) -> Observation {
        Observation::new(
            NONCE.into(),
            sequence,
            frame,
            ElementKey::parse("window.main").unwrap(),
            ObservationBody::StateRevision(StateRevision {
                target: ElementKey::parse("fixture.root").unwrap(),
                revision: sequence,
                cause_action_sequence: None,
            }),
        )
        .unwrap()
    }

    #[test]
    fn large_valid_region_uses_the_explicit_memory_bound_not_a_label_sized_wire_default() {
        use ui_observation_protocol::{Fixed6,Fraction,Rect,RegionObservation,Role};
        let rect=Rect::new(Fixed6::ZERO,Fixed6::ZERO,Fixed6::ONE,Fixed6::ONE).unwrap();
        let mut observation=revision(0,0);
        observation.record=ObservationBody::Region(RegionObservation{
            key:ElementKey::parse("fixture.region").unwrap(),role:Role::ScrollView,
            accessible_name:None,parent:None,layout_rect:rect,clip_rect:rect,viewport_rect:rect,
            window_content_rect:rect,visible_rect:rect,visible_fraction:Fraction::ONE,
            content_extent_x:Fixed6::ONE,content_extent_y:Fixed6::ONE,
            offset_x:Fixed6::ZERO,offset_y:Fixed6::ZERO,scroll_range_x:Fixed6::ZERO,scroll_range_y:Fixed6::ZERO,
            exposed_children:(0..3000).map(|i|ElementKey::parse(format!("fixture.child.{i}.{}","a".repeat(32))).unwrap()).collect(),
        });
        let size=canonical_json(&observation).unwrap().len();assert!(size>ui_observation_protocol::MAX_PAYLOAD);
        let mut admitted=MemoryObserver::new(NONCE,MemoryLimits{keys:4,bytes:size*2}).unwrap();
        admitted.push(observation.clone()).unwrap();
        let mut complete=revision(1,0);
        complete.record=ObservationBody::StateRevision(StateRevision{
            target:ElementKey::parse(ui_observation_protocol::FRAME_COMPLETE_KEY).unwrap(),revision:0,cause_action_sequence:None});
        admitted.push(complete).unwrap();
        assert_eq!(admitted.snapshot().unwrap().latest["fixture.region"],observation);
        let mut rejected=MemoryObserver::new(NONCE,MemoryLimits{keys:4,bytes:size-1}).unwrap();
        assert!(rejected.push(observation).is_err());assert!(rejected.snapshot().is_err());
    }

    #[test]
    fn explicit_empty_frame_replaces_previous_controls_and_seals_the_pass() {
        let complete = |sequence, frame| Observation::new(NONCE.into(), sequence, frame,
            ElementKey::parse("window.main").unwrap(),
            ObservationBody::StateRevision(ui_observation_protocol::StateRevision {
                target: ElementKey::parse(ui_observation_protocol::FRAME_COMPLETE_KEY).unwrap(),
                revision: frame, cause_action_sequence: None,
            })).unwrap();
        let mut observer = MemoryObserver::new(NONCE, MemoryLimits::default()).unwrap();
        observer.push(revision(0, 0)).unwrap();
        observer.push(complete(1, 0)).unwrap();
        assert_eq!(observer.snapshot().unwrap().latest.len(), 1);
        assert_eq!(observer.snapshot().unwrap().completed_through_records, Some(2));
        observer.push(complete(2, 1)).unwrap();
        let empty = observer.snapshot().unwrap();
        assert_eq!(empty.completed_frame, Some(1));
        assert_eq!(empty.completed_through_records, Some(3));
        assert!(empty.latest.is_empty());
        assert_eq!(empty.retained_payload_bytes, 0);
        assert!(observer.push(revision(3, 1)).is_err());
        assert!(observer.snapshot().is_err());
    }

    #[test]
    fn two_gib_budget_is_lazy_shared_and_releases_capacity() {
        let budget = MemoryBudget::new(2usize * 1024 * 1024 * 1024).unwrap();
        assert_eq!(budget.used(), 0);
        budget.resize(0, 1536 * 1024 * 1024).unwrap();
        budget.resize(0, 512 * 1024 * 1024).unwrap();
        assert_eq!(budget.used(), budget.limit());
        assert!(budget.resize(0, 1).is_err());
        budget.resize(1536 * 1024 * 1024, 0).unwrap();
        budget.resize(512 * 1024 * 1024, 0).unwrap();
        let mut first = MemoryObserver::with_budget(NONCE, MemoryLimits::default(), budget.clone()).unwrap();
        let mut second = MemoryObserver::with_budget(NONCE, MemoryLimits::default(), budget.clone()).unwrap();
        first.push(revision(0,0)).unwrap(); second.push(revision(0,0)).unwrap();
        assert_eq!(budget.used(), first.charged + second.charged);
        let retained = second.charged;
        drop(first); assert_eq!(budget.used(), retained);
        drop(second); assert_eq!(budget.used(), 0);
    }

    #[test]
    fn many_frames_replace_memory_and_only_completed_frames_are_served() {
        let mut observer = MemoryObserver::new(
            NONCE,
            MemoryLimits {
                keys: 8,
                bytes: 4096,
            },
        )
        .unwrap();
        for n in 0..10_000 {
            observer.push(revision(n, n)).unwrap();
            let view = observer.snapshot().unwrap();
            assert!(view.retained_payload_bytes <= 4096);
            assert_eq!(view.completed_frame, n.checked_sub(1));
            assert_eq!(view.completed_through_records, (n > 0).then_some(n));
            assert!(view.latest.len() <= 1);
        }
        observer
            .push(
                Observation::new(
                    NONCE.into(),
                    10_000,
                    9_999,
                    ElementKey::parse("window.main").unwrap(),
                    ObservationBody::StreamEnd(StreamEnd {
                        records_emitted: 10_001,
                        records_dropped: 0,
                    }),
                )
                .unwrap(),
            )
            .unwrap();
        let view = observer.finish().unwrap();
        assert!(view.stream_complete);
        assert_eq!(view.completed_frame, Some(9_999));
        assert_eq!(view.records_observed, 10_001);
    }

    #[test]
    fn exhaustion_and_missing_closure_never_serve_a_stale_success() {
        let mut observer = MemoryObserver::new(NONCE, MemoryLimits { keys: 1, bytes: 16 }).unwrap();
        assert!(observer.push(revision(0, 0)).is_err());
        assert!(observer.snapshot().is_err());
        assert!(observer.push(revision(1, 1)).is_err());
        let mut unfinished = MemoryObserver::new(NONCE, MemoryLimits::default()).unwrap();
        unfinished.push(revision(0, 0)).unwrap();
        assert!(unfinished.close().is_err());
        assert!(unfinished.snapshot().is_err());
    }

    #[test]
    fn transition_consumer_sees_every_record_before_coalescing() {
        let mut observer = MemoryObserver::new(NONCE, MemoryLimits::default()).unwrap();
        let mut expected = 0;
        for n in 0..10_000 {
            observer
                .push_with(revision(n, 0), |record| {
                    assert_eq!(record.sequence, expected);
                    expected += 1;
                    Ok(())
                })
                .unwrap();
        }
        assert_eq!(expected, 10_000);
        assert!(observer.snapshot().unwrap().retained_payload_bytes < 4096);
    }

    #[test]
    fn transition_consumer_loss_and_invalid_input_poison_views() {
        let mut observer = MemoryObserver::new(NONCE, MemoryLimits::default()).unwrap();
        let mut delivered = 0;
        observer
            .push_with(revision(0, 0), |_| {
                delivered += 1;
                Ok(())
            })
            .unwrap();
        assert!(observer
            .push_with(revision(2, 0), |_| {
                delivered += 1;
                Ok(())
            })
            .is_err());
        assert_eq!(delivered, 1);
        assert!(observer.snapshot().is_err());
        let mut observer = MemoryObserver::new(NONCE, MemoryLimits::default()).unwrap();
        assert!(observer
            .push_with(revision(0, 0), |_| Err("bounded consumer full".into()))
            .is_err());
        assert!(observer.snapshot().is_err());
    }

    #[test]
    fn dropped_records_invalidate_completed_views() {
        let mut observer = MemoryObserver::new(NONCE, MemoryLimits::default()).unwrap();
        observer.push(revision(0, 0)).unwrap();
        observer.push(revision(1, 1)).unwrap();
        assert!(observer.snapshot().unwrap().completed_frame.is_some());
        let dropped = Observation::new(
            NONCE.into(),
            2,
            1,
            ElementKey::parse("window.main").unwrap(),
            ObservationBody::RecordDropped(RecordDropped {
                reason: DropReason::QueueFull,
                count: 1,
            }),
        )
        .unwrap();
        assert!(observer.push(dropped).is_err());
        assert!(observer.snapshot().is_err());
    }
}
