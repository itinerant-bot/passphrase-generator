use crate::{canonical_json, decode_canonical_json, sha3_256, Observation};
use std::io::{Read, Write};
use thiserror::Error;

pub const MAGIC: &[u8; 8] = b"UIOBSV1\n";
/// Legacy default for callers that do not supply a memory admission policy.
/// Production in-memory consumers may explicitly admit larger records.
pub const MAX_PAYLOAD: usize = 65_536;
const WIRE_PAYLOAD_LIMIT: usize = u32::MAX as usize;

fn payload_limit(limit: usize) -> Result<usize, FrameError> {
    if limit == 0 { return Err(FrameError::InvalidLimit); }
    Ok(limit.min(WIRE_PAYLOAD_LIMIT))
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("IO_ERROR: {0}")]
    Io(#[from] std::io::Error),
    #[error("BAD_STREAM_MAGIC")]
    BadMagic,
    #[error("FRAME_TOO_LARGE")]
    TooLarge,
    #[error("FRAME_PAYLOAD_LIMIT_MUST_BE_POSITIVE")]
    InvalidLimit,
    #[error("FRAME_ALLOCATION_FAILED: {0}")]
    Allocation(String),
    #[error("FRAME_MEMORY_ADMISSION_FAILED: {0}")]
    Admission(String),
    #[error("TRUNCATED_FRAME")]
    Truncated,
    #[error("FRAME_DIGEST_MISMATCH")]
    DigestMismatch,
    #[error("INVALID_CANONICAL_PAYLOAD: {0}")]
    Canonical(#[from] crate::canonical::CanonicalError),
    #[error("INVALID_OBSERVATION: {0}")]
    Observation(#[from] crate::model::ObservationError),
}

/// A validated immutable wire payload, prepared once before asynchronous delivery.
pub struct PreparedFrame {
    payload: Vec<u8>,
    ended: bool,
}
impl PreparedFrame {
    pub fn new(observation: &Observation) -> Result<Self, FrameError> {
        Self::with_payload_limit(observation, MAX_PAYLOAD)
    }
    pub fn with_payload_limit(observation: &Observation, limit: usize) -> Result<Self, FrameError> {
        let limit = payload_limit(limit)?;
        observation.validate()?;
        let payload = canonical_json(observation)?;
        if payload.len() > limit { return Err(FrameError::TooLarge); }
        Ok(Self { payload, ended: matches!(observation.record, crate::ObservationBody::StreamEnd(_)) })
    }
    pub fn allocated_bytes(&self) -> usize { self.payload.capacity() }
    pub fn ended(&self) -> bool { self.ended }
}
/// Decoded and validated by the framing boundary; its canonical size is known.
#[derive(Clone)]
pub struct DecodedFrame {
    observation: Observation,
    payload_bytes: usize,
    validation_wall_ns: u64,
}

/// A complete wire frame whose canonical payload has not yet been decoded.
/// Reading and validating the envelope are separable so a bounded receiver can
/// validate several already-read frames concurrently while preserving order.
pub struct WireFrame {
    payload: Vec<u8>,
    expected_digest: [u8; 32],
}
impl WireFrame {
    pub fn decode(self) -> Result<DecodedFrame, FrameError> {
        let validation_started = std::time::Instant::now();
        if sha3_256(&self.payload).0 != self.expected_digest {
            return Err(FrameError::DigestMismatch);
        }
        let observation: Observation = decode_canonical_json(&self.payload)?;
        observation.validate()?;
        let validation_wall_ns=u64::try_from(validation_started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        Ok(DecodedFrame { observation, payload_bytes: self.payload.len(), validation_wall_ns })
    }
}
impl DecodedFrame {
    pub fn observation(&self) -> &Observation { &self.observation }
    pub fn into_parts(self) -> (Observation, usize) { (self.observation, self.payload_bytes) }
    /// Receiver validation wall time, excluding pipe reads. This is diagnostic
    /// timing on the observer, not a product emission or delivery timestamp.
    pub fn validation_wall_ns(&self) -> u64 { self.validation_wall_ns }
    pub fn payload_bytes(&self) -> usize { self.payload_bytes }
}

pub struct FrameWriter<W> {
    inner: W,
    started: bool,
    payload_limit: usize,
}

impl<W: Write> FrameWriter<W> {
    #[must_use]
    pub const fn new(inner: W) -> Self {
        Self {
            inner,
            started: false,
            payload_limit: MAX_PAYLOAD,
        }
    }

    pub fn write(&mut self, observation: &Observation) -> Result<(), FrameError> {
        self.write_prepared(&PreparedFrame::with_payload_limit(observation, self.payload_limit)?)
    }

    pub fn with_payload_limit(mut self, limit: usize) -> Result<Self, FrameError> {
        self.payload_limit = payload_limit(limit)?;
        Ok(self)
    }

    pub fn write_prepared(&mut self, frame: &PreparedFrame) -> Result<(), FrameError> {
        let payload = &frame.payload;
        if payload.len() > self.payload_limit { return Err(FrameError::TooLarge); }
        if !self.started {
            self.inner.write_all(MAGIC)?;
            self.started = true;
        }
        let length = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge)?;
        self.inner.write_all(&length.to_be_bytes())?;
        self.inner.write_all(&sha3_256(&payload).0)?;
        self.inner.write_all(&payload)?;
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), FrameError> {
        self.inner.flush().map_err(FrameError::Io)
    }
    #[must_use]
    pub fn into_inner(self) -> W {
        self.inner
    }
}

pub struct FrameReader<R> {
    inner: R,
    started: bool,
    payload_limit: usize,
}

impl<R: Read> FrameReader<R> {
    #[must_use]
    pub const fn new(inner: R) -> Self {
        Self {
            inner,
            started: false,
            payload_limit: MAX_PAYLOAD,
        }
    }

    /// Buffer pipe reads without resetting framing or stream position.
    pub fn buffered(self) -> FrameReader<std::io::BufReader<R>> {
        FrameReader { inner: std::io::BufReader::with_capacity(65536, self.inner), started: self.started, payload_limit: self.payload_limit }
    }

    pub fn with_payload_limit(mut self, limit: usize) -> Result<Self, FrameError> {
        self.payload_limit = payload_limit(limit)?;
        Ok(self)
    }

    pub fn read(&mut self) -> Result<Option<Observation>, FrameError> {
        Ok(self.read_decoded()?.map(|frame| frame.observation))
    }

    pub fn read_decoded(&mut self) -> Result<Option<DecodedFrame>, FrameError> {
        Ok(self.read_wire()?.map(WireFrame::decode).transpose()?)
    }

    pub fn read_wire(&mut self) -> Result<Option<WireFrame>, FrameError> {
        Ok(self.read_wire_admitted(|_| Ok(()))?.map(|(frame, ())| frame))
    }

    /// Admit the declared payload before reading or allocating its body. The
    /// caller owns the returned reservation through validation and handoff;
    /// any framing failure drops it here. EOF acquires no reservation.
    pub fn read_wire_admitted<A>(&mut self, admit: impl FnOnce(usize) -> Result<A, FrameError>)
        -> Result<Option<(WireFrame, A)>, FrameError> {
        if !self.started {
            let mut magic = [0_u8; 8];
            read_exact_classified(&mut self.inner, &mut magic, false)?;
            if &magic != MAGIC {
                return Err(FrameError::BadMagic);
            }
            self.started = true;
        }
        let mut length = [0_u8; 4];
        if !read_exact_or_eof(&mut self.inner, &mut length)? {
            return Ok(None);
        }
        let length = u32::from_be_bytes(length) as usize;
        if length > self.payload_limit {
            return Err(FrameError::TooLarge);
        }
        let reservation = admit(length)?;
        let mut expected_digest = [0_u8; 32];
        read_exact_classified(&mut self.inner, &mut expected_digest, true)?;
        let mut payload = Vec::new();
        payload.try_reserve_exact(length).map_err(|error| FrameError::Allocation(error.to_string()))?;
        payload.resize(length, 0);
        read_exact_classified(&mut self.inner, &mut payload, true)?;
        Ok(Some((WireFrame { payload, expected_digest }, reservation)))
    }
}

fn read_exact_or_eof(reader: &mut impl Read, buffer: &mut [u8]) -> Result<bool, FrameError> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(FrameError::Truncated),
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(FrameError::Io(error)),
        }
    }
    Ok(true)
}

fn read_exact_classified(
    reader: &mut impl Read,
    buffer: &mut [u8],
    frame: bool,
) -> Result<(), FrameError> {
    match read_exact_or_eof(reader, buffer) {
        Ok(true) => Ok(()),
        Ok(false) if frame => Err(FrameError::Truncated),
        Ok(false) => Err(FrameError::BadMagic),
        Err(error) => Err(error),
    }
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum StreamError {
    #[error("NONCE_MISMATCH")]
    NonceMismatch,
    #[error("SEQUENCE_GAP")]
    SequenceGap,
    #[error("FRAME_SEQUENCE_REGRESSION")]
    FrameRegression,
    #[error("DUPLICATE_ELEMENT_KEY")]
    DuplicateElementKey,
    #[error("STABLE_KEY_ROLE_DRIFT")]
    RoleDrift,
    #[error("RECORD_AFTER_STREAM_END")]
    AfterEnd,
    #[error("CLEAN_STREAM_END_ABSENT")]
    MissingEnd,
}

pub struct StreamValidator {
    nonce: String,
    next_sequence: u64,
    frame_sequence: u64,
    ended: bool,
    frame_keys: std::collections::BTreeSet<ElementKeyRole>,
    roles: std::collections::BTreeMap<String, crate::Role>,
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct ElementKeyRole(String);

impl StreamValidator {
    #[must_use]
    pub fn new(nonce: impl Into<String>) -> Self {
        Self {
            nonce: nonce.into(),
            next_sequence: 0,
            frame_sequence: 0,
            ended: false,
            frame_keys: Default::default(),
            roles: Default::default(),
        }
    }

    pub fn push(&mut self, observation: &Observation) -> Result<(), StreamError> {
        if self.ended {
            return Err(StreamError::AfterEnd);
        }
        if observation.run_nonce != self.nonce {
            return Err(StreamError::NonceMismatch);
        }
        if observation.sequence != self.next_sequence {
            return Err(StreamError::SequenceGap);
        }
        if observation.frame_sequence < self.frame_sequence {
            return Err(StreamError::FrameRegression);
        }
        if observation.frame_sequence > self.frame_sequence {
            self.frame_keys.clear();
            self.frame_sequence = observation.frame_sequence;
        }
        if let crate::ObservationBody::Element(element) = &observation.record {
            let key = element.key.as_str().to_owned();
            if !self.frame_keys.insert(ElementKeyRole(key.clone())) {
                return Err(StreamError::DuplicateElementKey);
            }
            if self
                .roles
                .get(&key)
                .is_some_and(|role| *role != element.role)
            {
                return Err(StreamError::RoleDrift);
            }
            self.roles.insert(key, element.role);
        }
        self.ended = matches!(observation.record, crate::ObservationBody::StreamEnd(_));
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(StreamError::SequenceGap)?;
        Ok(())
    }

    pub fn finish(self) -> Result<(), StreamError> {
        self.ended.then_some(()).ok_or(StreamError::MissingEnd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ElementKey, ObservationBody, StreamEnd};
    use std::io::{self, Cursor};

    fn end(sequence: u64) -> Observation {
        Observation::new(
            "0123456789abcdef0123456789abcdef".into(),
            sequence,
            1,
            ElementKey::parse("fixture.window").unwrap(),
            ObservationBody::StreamEnd(StreamEnd {
                records_emitted: sequence + 1,
                records_dropped: 0,
            }),
        )
        .unwrap()
    }

    fn large_region() -> Observation {
        use crate::{Fixed6, Fraction, Rect, RegionObservation, Role};
        let rect=Rect::new(Fixed6::ZERO,Fixed6::ZERO,Fixed6::ONE,Fixed6::ONE).unwrap();
        let mut observation=end(0);
        observation.record=ObservationBody::Region(RegionObservation{
            key:ElementKey::parse("fixture.region").unwrap(),role:Role::ScrollView,
            accessible_name:None,parent:None,layout_rect:rect,clip_rect:rect,
            viewport_rect:rect,window_content_rect:rect,visible_rect:rect,visible_fraction:Fraction::ONE,
            content_extent_x:Fixed6::ONE,content_extent_y:Fixed6::ONE,
            offset_x:Fixed6::ZERO,offset_y:Fixed6::ZERO,scroll_range_x:Fixed6::ZERO,scroll_range_y:Fixed6::ZERO,
            exposed_children:(0..3000).map(|i|ElementKey::parse(format!("fixture.child.{i}.{}","a".repeat(32))).unwrap()).collect(),
        });
        observation
    }

    #[test]
    fn explicit_payload_admission_preserves_large_records_and_all_frame_failures() {
        let observation=large_region();let size=canonical_json(&observation).unwrap().len();
        assert!(size>MAX_PAYLOAD);
        assert!(matches!(PreparedFrame::new(&observation),Err(FrameError::TooLarge)));
        assert!(matches!(PreparedFrame::with_payload_limit(&observation,size-1),Err(FrameError::TooLarge)));
        assert!(matches!(FrameWriter::new(Vec::new()).with_payload_limit(0),Err(FrameError::InvalidLimit)));
        let mut writer=FrameWriter::new(Vec::new()).with_payload_limit(size).unwrap();
        writer.write(&observation).unwrap();let bytes=writer.into_inner();
        assert!(matches!(FrameReader::new(bytes.as_slice()).read(),Err(FrameError::TooLarge)));
        let mut reader=FrameReader::new(bytes.as_slice()).with_payload_limit(size).unwrap().buffered();
        assert_eq!(reader.read().unwrap(),Some(observation));assert_eq!(reader.read().unwrap(),None);
        let mut corrupt=bytes.clone();corrupt[12]^=1;
        assert!(matches!(FrameReader::new(corrupt.as_slice()).with_payload_limit(size).unwrap().read(),Err(FrameError::DigestMismatch)));
        assert!(matches!(FrameReader::new(&bytes[..bytes.len()-1]).with_payload_limit(size).unwrap().read(),Err(FrameError::Truncated)));
        let mut rejected=FrameReader::new(Cursor::new(bytes)).with_payload_limit(size).unwrap();
        assert!(matches!(rejected.read_wire_admitted::<()>(|actual|{
            assert_eq!(actual,size);Err(FrameError::Admission("caller RAM unavailable".into()))
        }),Err(FrameError::Admission(_))));
        assert_eq!(rejected.inner.position(),12,"rejection must precede digest/body reads");
    }

    #[test]
    fn admitted_capacity_is_released_on_truncation_and_not_acquired_at_eof() {
        use std::{cell::Cell,rc::Rc};
        struct Ticket(Rc<Cell<usize>>);
        impl Drop for Ticket {fn drop(&mut self){self.0.set(self.0.get()+1);}}
        let mut writer=FrameWriter::new(Vec::new());writer.write(&end(0)).unwrap();
        let mut bytes=writer.into_inner();bytes.pop();let drops=Rc::new(Cell::new(0));
        let mut reader=FrameReader::new(bytes.as_slice());
        assert!(matches!(reader.read_wire_admitted(|_|Ok(Ticket(drops.clone()))),Err(FrameError::Truncated)));
        assert_eq!(drops.get(),1);
        let mut reader=FrameReader::new(MAGIC.as_slice());
        assert!(reader.read_wire_admitted::<()>(|_|panic!("EOF cannot acquire capacity")).unwrap().is_none());
    }

    #[test]
    fn frame_round_trip_and_partial_reads() {
        let mut writer = FrameWriter::new(Vec::new());
        writer.write(&end(0)).unwrap();
        let bytes = writer.into_inner();
        struct OneByte(Cursor<Vec<u8>>);
        impl Read for OneByte {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                let length = out.len().min(1);
                self.0.read(&mut out[..length])
            }
        }
        let mut reader = FrameReader::new(OneByte(Cursor::new(bytes)));
        assert_eq!(reader.read().unwrap(), Some(end(0)));
        assert_eq!(reader.read().unwrap(), None);
    }

    #[test]
    fn adversarial_frames_fail_closed() {
        let mut writer = FrameWriter::new(Vec::new());
        writer.write(&end(0)).unwrap();
        let valid = writer.into_inner();
        for cut in 0..MAGIC.len() {
            let result = FrameReader::new(&valid[..cut]).read();
            assert!(result.is_err(), "accepted truncation at {cut}");
        }
        assert_eq!(
            FrameReader::new(&valid[..MAGIC.len()]).read().unwrap(),
            None
        );
        for cut in (MAGIC.len() + 1)..valid.len() {
            let result = FrameReader::new(&valid[..cut]).read();
            assert!(result.is_err(), "accepted truncation at {cut}");
        }
        let mut corrupt = valid.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert!(matches!(
            FrameReader::new(corrupt.as_slice()).read(),
            Err(FrameError::DigestMismatch)
        ));
        let mut oversized = Vec::from(MAGIC);
        oversized.extend_from_slice(&((MAX_PAYLOAD as u32) + 1).to_be_bytes());
        assert!(matches!(
            FrameReader::new(oversized.as_slice()).read(),
            Err(FrameError::TooLarge)
        ));
    }

    #[test]
    fn canonical_decoder_rejects_whitespace() {
        let bytes = end(0).canonical_bytes().unwrap();
        let mut noncanonical = Vec::from(&b" "[..]);
        noncanonical.extend(bytes);
        assert!(decode_canonical_json::<Observation>(&noncanonical).is_err());
    }
}
