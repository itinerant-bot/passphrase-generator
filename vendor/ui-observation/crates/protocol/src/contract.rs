use crate::{ElementKey, Fixed6, Fraction, Role, PROTOCOL_SCHEMA};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EvidenceScope {
    Headless,
    NativeHost,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpectedDisposition {
    Pass,
    Reject,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Contract {
    pub schema: String,
    pub id: String,
    pub protocol: String,
    pub execution: EvidenceScope,
    pub fixture: String,
    pub required_platform_facilities: Vec<String>,
    pub private_canaries: Vec<String>,
    #[serde(rename = "case")]
    pub cases: Vec<Case>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub source: String,
    pub expected: ExpectedDisposition,
    pub expected_failure_code: Option<String>,
    #[serde(default, rename = "action")]
    pub actions: Vec<Action>,
    #[serde(default, rename = "predicate")]
    pub predicates: Vec<Predicate>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "snake_case",
    deny_unknown_fields
)]
pub enum Action {
    PointerMove {
        id: String,
        target: ElementKey,
    },
    PointerClick {
        id: String,
        target: ElementKey,
        button: String,
    },
    Focus {
        id: String,
        target: ElementKey,
    },
    Key {
        id: String,
        target: ElementKey,
        key: String,
    },
    Text {
        id: String,
        target: ElementKey,
        value_class: String,
    },
    ScrollHorizontal {
        id: String,
        target: ElementKey,
        delta: Fixed6,
    },
}

impl Action {
    fn id(&self) -> &str {
        match self {
            Self::PointerMove { id, .. }
            | Self::PointerClick { id, .. }
            | Self::Focus { id, .. }
            | Self::Key { id, .. }
            | Self::Text { id, .. }
            | Self::ScrollHorizontal { id, .. } => id,
        }
    }
}

/// Closed predicate representation. Fields are strict, while evaluation lives
/// in the independent evaluator crate.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "snake_case",
    deny_unknown_fields
)]
#[allow(clippy::large_enum_variant)]
pub enum Predicate {
    StreamIntegrity {
        id: String,
        require_exact_nonce: bool,
        require_contiguous_sequence: bool,
        require_nondecreasing_frame: bool,
        require_clean_end: bool,
    },
    CanonicalRoundTrip {
        id: String,
        require_byte_identical: bool,
    },
    ForbiddenRecordFields {
        id: String,
        fields: Vec<String>,
    },
    AdapterReturnEquivalent {
        id: String,
        target: ElementKey,
    },
    ElementSemantics {
        id: String,
        target: ElementKey,
        role: Role,
        accessible_name: String,
        enabled: bool,
    },
    VisibleIntersectionRecomputed {
        id: String,
        target: ElementKey,
        minimum_fraction: Fraction,
    },
    ActionAcknowledgedOnce {
        id: String,
        action: String,
        target: ElementKey,
    },
    AdapterVersionEquivalence {
        id: String,
        versions: Vec<String>,
    },
    RegionExtents {
        id: String,
        target: ElementKey,
        require_content_wider_than_viewport: bool,
        require_positive_scroll_range: bool,
    },
    StableTargetVisible {
        id: String,
        target: ElementKey,
        minimum_fraction: Fraction,
    },
    OffsetChangedAfterAction {
        id: String,
        target: ElementKey,
        action: String,
        axis: String,
        minimum_delta: Fixed6,
    },
    ContentDisplacedAfterAction {
        id: String,
        target: ElementKey,
        action: String,
        axis: String,
    },
    ColumnAlignmentPreserved {
        id: String,
        header: ElementKey,
        body: ElementKey,
    },
    ElementParent {
        id: String,
        target: ElementKey,
        parent: ElementKey,
    },
    AccessibilityAgreement {
        id: String,
        target: ElementKey,
        require_role: bool,
        require_name: bool,
        require_bounds: bool,
        require_enabled: bool,
        require_parent: bool,
    },
    RawBytesExcludeCanaries {
        id: String,
    },
    StreamNonceCorrelated {
        id: String,
    },
    ChannelCapabilities {
        id: String,
        write_only: bool,
        control_input: bool,
        filesystem_path: bool,
    },
    ObservationDisabled {
        id: String,
        require_no_files: bool,
        #[serde(rename = "require_ordinary-result_equal")]
        require_ordinary_result_equal: bool,
    },
    ObserverFailureNoninterference {
        id: String,
        require_no_panic: bool,
        require_no_deadlock: bool,
        require_ordinary_result_equal: bool,
        require_durable_state_equal: bool,
    },
    ContentClass {
        id: String,
        target: ElementKey,
        class: String,
        exact_value: Option<String>,
        require_digest: Option<bool>,
        require_byte_length: Option<bool>,
    },
    OrdinaryRenderFunctionUsed {
        id: String,
        require_fixture_only_path_absent: bool,
    },
    ShapeAndObservationAgreement {
        id: String,
        target: ElementKey,
    },
    StateRevisionAfterAction {
        id: String,
        action: String,
        target: ElementKey,
    },
    EvidenceScopeExact {
        id: String,
        value: String,
    },
    PredicateVariantCoverage {
        id: String,
        require_every_v1_variant: bool,
    },
    EvaluatorDependencyFence {
        id: String,
        forbidden_crates: Vec<String>,
    },
    LaunchedBinaryBound {
        id: String,
        require_executable_digest: bool,
        require_argv: bool,
        require_cwd: bool,
        require_environment: bool,
        require_process_tree: bool,
    },
    NativeWindowCorrelated {
        id: String,
        require_exactly_one: bool,
        require_process_owner: bool,
        require_identifier: bool,
        require_title_constraint: bool,
        require_nonzero_area: bool,
        require_on_screen: bool,
        require_scale: bool,
        require_lifetime: bool,
    },
    NativePresentationCount {
        id: String,
        minimum: u64,
    },
    NativeAccessibilityAgreement {
        id: String,
        target: ElementKey,
    },
    NativeTargetPointValid {
        id: String,
        action: String,
        require_current_frame: bool,
        require_window_containment: bool,
        require_accessibility_containment: bool,
    },
    NativeActionCorrelated {
        id: String,
        action: String,
        target: ElementKey,
        require_one_native_receipt: bool,
        require_one_application_ack: bool,
        require_state_revision: bool,
        require_no_unintended_ack: bool,
    },
    DurableArtifactChange {
        id: String,
        action: String,
        artifact: String,
        before: String,
        after: String,
        require_changed_digest: bool,
    },
    PublicInspectionExact {
        id: String,
        action: String,
        command: String,
        argv: Vec<String>,
        stdout: String,
        exit_code: i32,
    },
    DurableMutationSetExact {
        id: String,
        allowed: Vec<String>,
    },
    VendoredSourceAuthority {
        id: String,
        require_full_git_commit: bool,
        require_file_sha3_256: bool,
        require_protocol_version: bool,
        require_adapter_versions: Vec<String>,
        forbid_ambient_path_dependencies: bool,
        forbid_mutable_branches: bool,
        forbid_submodules: bool,
    },
    PackageManifestComplete {
        id: String,
        require_contract: bool,
        require_trace: bool,
        require_report: bool,
        require_binary_digests: bool,
        require_source_authority: bool,
        require_host_facts: bool,
    },
    OfflineReplayIdentical {
        id: String,
        require_no_product: bool,
        require_no_gui: bool,
        require_no_network: bool,
        require_no_llm: bool,
        require_byte_identical_report: bool,
    },
}

impl Predicate {
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::StreamIntegrity { id, .. }
            | Self::CanonicalRoundTrip { id, .. }
            | Self::ForbiddenRecordFields { id, .. }
            | Self::AdapterReturnEquivalent { id, .. }
            | Self::ElementSemantics { id, .. }
            | Self::VisibleIntersectionRecomputed { id, .. }
            | Self::ActionAcknowledgedOnce { id, .. }
            | Self::AdapterVersionEquivalence { id, .. }
            | Self::RegionExtents { id, .. }
            | Self::StableTargetVisible { id, .. }
            | Self::OffsetChangedAfterAction { id, .. }
            | Self::ContentDisplacedAfterAction { id, .. }
            | Self::ColumnAlignmentPreserved { id, .. }
            | Self::ElementParent { id, .. }
            | Self::AccessibilityAgreement { id, .. }
            | Self::RawBytesExcludeCanaries { id }
            | Self::StreamNonceCorrelated { id }
            | Self::ChannelCapabilities { id, .. }
            | Self::ObservationDisabled { id, .. }
            | Self::ObserverFailureNoninterference { id, .. }
            | Self::ContentClass { id, .. }
            | Self::OrdinaryRenderFunctionUsed { id, .. }
            | Self::ShapeAndObservationAgreement { id, .. }
            | Self::StateRevisionAfterAction { id, .. }
            | Self::EvidenceScopeExact { id, .. }
            | Self::PredicateVariantCoverage { id, .. }
            | Self::EvaluatorDependencyFence { id, .. }
            | Self::LaunchedBinaryBound { id, .. }
            | Self::NativeWindowCorrelated { id, .. }
            | Self::NativePresentationCount { id, .. }
            | Self::NativeAccessibilityAgreement { id, .. }
            | Self::NativeTargetPointValid { id, .. }
            | Self::NativeActionCorrelated { id, .. }
            | Self::DurableArtifactChange { id, .. }
            | Self::PublicInspectionExact { id, .. }
            | Self::DurableMutationSetExact { id, .. }
            | Self::VendoredSourceAuthority { id, .. }
            | Self::PackageManifestComplete { id, .. }
            | Self::OfflineReplayIdentical { id, .. } => id,
        }
    }
}

#[derive(Debug, Error)]
pub enum ContractError {
    #[error("AUTHORITY_IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("MALFORMED_AUTHORITY: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("AUTHORITY_PATH_UNSAFE")]
    UnsafePath,
    #[error("AUTHORITY_ID_MISMATCH")]
    IdMismatch,
    #[error("AUTHORITY_SCHEMA_MISMATCH")]
    SchemaMismatch,
    #[error("DUPLICATE_AUTHORITY_ID")]
    DuplicateId,
    #[error("INVALID_EXPECTED_DISPOSITION")]
    InvalidDisposition,
    #[error("HEADLESS_FACILITY_CLAIM")]
    HeadlessFacility,
}

impl Contract {
    pub fn decode(text: &str, expected_id: &str) -> Result<Self, ContractError> {
        let contract: Self = toml::from_str(text)?;
        if contract.id != expected_id {
            return Err(ContractError::IdMismatch);
        }
        contract.validate()?;
        Ok(contract)
    }

    pub fn load(path: &Path) -> Result<Self, ContractError> {
        if path.is_symlink()
            || path.parent() != Some(Path::new("contracts"))
            || path.extension().and_then(|s| s.to_str()) != Some("toml")
        {
            return Err(ContractError::UnsafePath);
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or(ContractError::UnsafePath)?;
        let text = std::fs::read_to_string(path)?;
        Self::decode(&text, stem)
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if self.schema != "ui.observation.contract.v1" || self.protocol != PROTOCOL_SCHEMA {
            return Err(ContractError::SchemaMismatch);
        }
        if self.execution == EvidenceScope::Headless
            && !self.required_platform_facilities.is_empty()
        {
            return Err(ContractError::HeadlessFacility);
        }
        let mut ids = BTreeSet::new();
        for case in &self.cases {
            if !ids.insert(case.id.as_str()) {
                return Err(ContractError::DuplicateId);
            }
            match (case.expected, case.expected_failure_code.as_deref()) {
                (ExpectedDisposition::Pass, None) | (ExpectedDisposition::Reject, Some(_)) => {}
                _ => return Err(ContractError::InvalidDisposition),
            }
            let mut nested = BTreeSet::new();
            for action in &case.actions {
                if !nested.insert(action.id()) {
                    return Err(ContractError::DuplicateId);
                }
            }
            for predicate in &case.predicates {
                if !nested.insert(predicate.id()) {
                    return Err(ContractError::DuplicateId);
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tracked_contract_strictly_decodes() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        if !root.join("contracts").is_dir() {
            // Application-side vendored snapshots intentionally contain only
            // the four dependency crates. Repository authority contracts are
            // exercised in the source workspace, not assumed to exist beside
            // a consumer's vendored dependency graph.
            return;
        }
        let mut count = 0;
        for entry in std::fs::read_dir(root.join("contracts")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml") {
                let relative = Path::new("contracts").join(path.file_name().unwrap());
                let previous = std::env::current_dir().unwrap();
                std::env::set_current_dir(&root).unwrap();
                let result = Contract::load(&relative);
                std::env::set_current_dir(previous).unwrap();
                result.unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                count += 1;
            }
        }
        assert_eq!(count, 12);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let raw = r#"schema="ui.observation.contract.v1"
id="x"
protocol="ui.observation.v1"
execution="headless"
fixture="x"
required_platform_facilities=[]
private_canaries=[]
surprise=true
"#;
        assert!(toml::from_str::<Contract>(raw).is_err());
    }
}
