#![forbid(unsafe_code)]

//! Closed, toolkit-neutral records for UI observation protocol v1.
//!
//! This crate intentionally has no UI, runtime, or evaluator dependency. Values
//! which can carry product text require an explicit [`Content`] classification.

mod canonical;
mod contract;
mod frame;
mod geometry;
mod model;
mod trace;

pub use canonical::{canonical_json, decode_canonical_json, sha3_256, Digest};
pub use contract::{
    Action, Case, Contract, ContractError, EvidenceScope, ExpectedDisposition, Predicate,
};
pub use frame::{
    DecodedFrame, PreparedFrame, FrameError, FrameReader, FrameWriter, WireFrame, StreamError, StreamValidator, MAGIC, MAX_PAYLOAD,
};
pub use geometry::{Fixed6, Fraction, GeometryError, Rect};
pub use model::{
    is_nonce, ActionAck, ControlMetadata, InteractionKind, RevealSpec, Content, ContentDigest, ContentError, DropReason, ElementKey, ElementObservation,
    ElementSpec, KeyError, Observation, ObservationBody, RecordDropped, RegionObservation,
    RegionSpec, Role, StateRevision, States, StreamEnd, WindowObservation, PROTOCOL_SCHEMA,
};
pub use trace::{
    ChannelMetadata, NativeAccessibilityReceipt, NativeDurableReceipt, NativeHostReceipt,
    NativeInputReceipt, NativePresentationReceipt, NativeProcessReceipt, NativeReceiptsArtifact,
    NativeRect, NativeWindowReceipt, TraceArtifact, TraceMetadata, NATIVE_RECEIPTS_SCHEMA,
    TRACE_SCHEMA,
};

/// Reserved StateRevision target emitted after the complete UI pass.
/// Its revision equals frame_sequence and cause_action_sequence is absent.
pub const FRAME_COMPLETE_KEY: &str = "ui.observation.frame.complete";
