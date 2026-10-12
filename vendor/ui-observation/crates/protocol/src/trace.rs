use crate::{ElementKey, EvidenceScope};
use serde::{Deserialize, Serialize};

pub const TRACE_SCHEMA: &str = "ui.observation.trace.v1";
pub const NATIVE_RECEIPTS_SCHEMA: &str = "ui.observation.native-receipts.v1";

/// Retained controller artifact. `product_stream` is the byte-exact inherited
/// channel capture; the remaining fields are closed external facts, never a
/// producer-selected verdict or predicate result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceArtifact {
    pub schema: String,
    pub authority_id: String,
    pub nonce: String,
    pub product_stream: Vec<u8>,
    pub metadata: TraceMetadata,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TraceMetadata {
    pub evidence_scope: Option<EvidenceScope>,
    pub adapter_versions: Vec<String>,
    pub accessibility_targets: Vec<ElementKey>,
    pub shape_targets: Vec<ElementKey>,
    pub ordinary_render_function_used: bool,
    pub fixture_only_path_used: bool,
    pub column_alignment_preserved: bool,
    pub channel: Option<ChannelMetadata>,
    pub native_receipts_present: bool,
    pub source_authority_verified: bool,
    pub package_manifest_complete: bool,
    pub offline_replay_ready: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChannelMetadata {
    pub write_only: bool,
    pub control_input: bool,
    pub filesystem_path: bool,
    pub no_files_when_disabled: bool,
    pub ordinary_result_equal: bool,
    pub durable_state_equal: bool,
    pub no_panic: bool,
    pub no_deadlock: bool,
}

/// Closed, evaluator-owned representation of facts collected outside the
/// product process on a native desktop host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeReceiptsArtifact {
    pub schema: String,
    pub authority_id: String,
    pub nonce: String,
    pub host: NativeHostReceipt,
    pub process: NativeProcessReceipt,
    pub windows: Vec<NativeWindowReceipt>,
    pub presentations: Vec<NativePresentationReceipt>,
    pub accessibility: Vec<NativeAccessibilityReceipt>,
    pub inputs: Vec<NativeInputReceipt>,
    pub durable: Option<NativeDurableReceipt>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHostReceipt {
    pub platform: String,
    pub os_release: String,
    pub session_id: u32,
    pub display_identity: String,
    pub accessibility_backend: String,
    pub input_backend: String,
    pub monotonic_frequency: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeProcessReceipt {
    pub process_id: u32,
    pub executable_sha3_256: String,
    pub argv_sha3_256: String,
    pub cwd_sha3_256: String,
    pub environment_sha3_256: String,
    pub process_tree: Vec<u32>,
    pub launched_at: u64,
    pub exited_at: u64,
    pub exit_code: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl NativeRect {
    #[must_use]
    pub const fn has_area(self) -> bool {
        self.right > self.left && self.bottom > self.top
    }

    #[must_use]
    pub const fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeWindowReceipt {
    pub identifier: String,
    pub process_id: u32,
    pub title: String,
    pub window_rect: NativeRect,
    pub client_rect: NativeRect,
    pub monitor_rect: NativeRect,
    pub dpi: u32,
    pub visible: bool,
    pub first_observed_at: u64,
    pub last_observed_at: u64,
    pub destroyed_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePresentationReceipt {
    pub window_identifier: String,
    pub composition_count: u64,
    pub application_frame_delta: u64,
    pub first_observed_at: u64,
    pub last_observed_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAccessibilityReceipt {
    pub target: ElementKey,
    pub process_id: u32,
    pub role: crate::Role,
    pub name: String,
    pub enabled: bool,
    pub uia_bounds: NativeRect,
    pub application_bounds: NativeRect,
    pub parent_name: Option<String>,
    pub observed_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeInputReceipt {
    pub action: String,
    pub target: ElementKey,
    pub kind: String,
    pub point_x: Option<i32>,
    pub point_y: Option<i32>,
    pub source_frame_sequence: u64,
    pub injected_event_count: u32,
    pub injected_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDurableReceipt {
    pub action: String,
    pub artifact: String,
    pub before_sha3_256: Option<String>,
    pub after_sha3_256: Option<String>,
    pub mutated_paths: Vec<String>,
    pub inspection_command_sha3_256: String,
    pub inspection_stdout: String,
    pub inspection_exit_code: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_rectangle_edges_are_half_open() {
        let rectangle = NativeRect {
            left: -10,
            top: 20,
            right: 30,
            bottom: 40,
        };
        assert!(rectangle.has_area());
        assert!(rectangle.contains(-10, 20));
        assert!(rectangle.contains(29, 39));
        assert!(!rectangle.contains(30, 39));
        assert!(!rectangle.contains(29, 40));
        assert!(!NativeRect {
            right: -10,
            ..rectangle
        }
        .has_area());
    }

    #[test]
    fn native_receipts_reject_extension_fields() {
        let bytes = br#"{"bottom":40,"extra":true,"left":-10,"right":30,"top":20}"#;
        assert!(crate::decode_canonical_json::<NativeRect>(bytes).is_err());
    }
}
