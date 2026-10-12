use crate::{canonical_json, sha3_256, Fixed6, Fraction, GeometryError, Rect};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};
use thiserror::Error;

pub const PROTOCOL_SCHEMA: &str = "ui.observation.v1";

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ElementKey(String);

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum KeyError {
    #[error("INVALID_ELEMENT_KEY: keys must be 1..=128 lowercase ASCII path characters")]
    Invalid,
}

impl ElementKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, KeyError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && !value.starts_with('.')
            && !value.ends_with('.')
            && !value.contains("..")
            && value.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
            });
        valid.then_some(Self(value)).ok_or(KeyError::Invalid)
    }

    /// Derives a privacy-safe stable child key from an application identifier.
    /// The identifier bytes never enter the key or serialized observation.
    pub fn derived(prefix: &str, identifier: &[u8]) -> Result<Self, KeyError> {
        let prefix = Self::parse(prefix)?;
        let digest = sha3_256(identifier).to_hex();
        Self::parse(format!("{}.{digest}", prefix.as_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ElementKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for ElementKey {
    type Err = KeyError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum Content {
    PublicText { value: String },
    StableIdentifier { value: String },
    DigestOnly { sha3_256: String, byte_length: u64 },
    PrivateOmitted { byte_length: u64 },
}

#[derive(Debug, Error, Clone, Eq, PartialEq)]
pub enum ContentError {
    #[error("EMPTY_PUBLIC_CONTENT")]
    EmptyPublic,
    #[error("CONTENT_TOO_LARGE")]
    TooLarge,
    #[error("INVALID_STABLE_IDENTIFIER")]
    InvalidStableIdentifier,
}

impl Content {
    const MAX_PUBLIC_BYTES: usize = 4096;

    pub fn public(value: impl Into<String>) -> Result<Self, ContentError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ContentError::EmptyPublic);
        }
        if value.len() > Self::MAX_PUBLIC_BYTES {
            return Err(ContentError::TooLarge);
        }
        Ok(Self::PublicText { value })
    }

    pub fn stable_identifier(value: impl Into<String>) -> Result<Self, ContentError> {
        let value = value.into();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err(ContentError::InvalidStableIdentifier);
        }
        Ok(Self::StableIdentifier { value })
    }

    #[must_use]
    pub fn digest_only(bytes: &[u8]) -> Self {
        Self::DigestOnly {
            sha3_256: sha3_256(bytes).to_hex(),
            byte_length: bytes.len() as u64,
        }
    }

    #[must_use]
    pub const fn private_omitted(byte_length: u64) -> Self {
        Self::PrivateOmitted { byte_length }
    }

    #[must_use]
    pub fn public_value(&self) -> Option<&str> {
        match self {
            Self::PublicText { value } => Some(value),
            _ => None,
        }
    }
}

/// Incremental classification of already-rendered text. Only a fixed-size
/// production SHA3 state and the exact byte count are retained; callers do not
/// need to materialize a second copy of a large editor or document.
pub struct ContentDigest {
    hasher: sha3::Sha3_256,
    byte_length: u64,
}

impl ContentDigest {
    #[must_use]
    pub fn new() -> Self {
        use sha3::Digest as _;
        Self { hasher: sha3::Sha3_256::new(), byte_length: 0 }
    }

    pub fn update(&mut self, bytes: &[u8]) -> Result<(), ContentError> {
        use sha3::Digest as _;
        let length = self.byte_length.checked_add(bytes.len() as u64).ok_or(ContentError::TooLarge)?;
        self.hasher.update(bytes);
        self.byte_length = length;
        Ok(())
    }

    #[must_use]
    pub fn finish(self) -> Content {
        use sha3::Digest as _;
        Content::DigestOnly {
            sha3_256: crate::Digest(self.hasher.finalize().into()).to_hex(),
            byte_length: self.byte_length,
        }
    }
}

impl Default for ContentDigest {
    fn default() -> Self { Self::new() }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Window,
    Region,
    Label,
    Button,
    TextInput,
    Tab,
    List,
    ListItem,
    Table,
    Row,
    Cell,
    ScrollView,
    Status,
}

impl Role {
    #[must_use]
    pub const fn is_actionable(self) -> bool {
        matches!(
            self,
            Self::Button | Self::TextInput | Self::Tab | Self::ListItem
        )
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct States {
    pub enabled: bool,
    pub focused: bool,
    pub hovered: bool,
    pub pressed: bool,
    pub clicked: bool,
    pub selected: bool,
    pub editable: bool,
}

/// Descriptions only: these records never accept or dispatch input. Presence
/// advertises a contract; a later observation must independently prove effects.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind { Activate, ReplaceText, Select, Expand, Collapse }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevealSpec {
    pub on: InteractionKind,
    pub target: ElementKey,
    pub role: Role,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlMetadata {
    pub interactions: Vec<InteractionKind>,
    /// Logical scope identity, explicitly classified; never inferred from state.
    pub scope: Option<Content>,
    /// Expected revealed controls, not evidence that activation occurred.
    pub reveals: Vec<RevealSpec>,
}

impl ControlMetadata {
    pub fn validate(&self) -> Result<(), ObservationError> {
        if self.interactions.len() > 5 || self.reveals.len() > 32 {
            return Err(ObservationError::InvalidControlMetadata);
        }
        for (i, interaction) in self.interactions.iter().enumerate() {
            if self.interactions[..i].contains(interaction) {
                return Err(ObservationError::InvalidControlMetadata);
            }
        }
        for (i, reveal) in self.reveals.iter().enumerate() {
            if !self.interactions.contains(&reveal.on) || self.reveals[..i].contains(reveal) {
                return Err(ObservationError::InvalidControlMetadata);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ElementSpec {
    pub key: ElementKey,
    pub role: Role,
    pub accessible_name: Content,
    pub parent: Option<ElementKey>,
    /// Explicitly classified public/identifier content crossing the observation
    /// boundary. Omitted by default; never infer it from application state.
    pub content: Option<Content>,
    pub control: Option<ControlMetadata>,
    /// Actual editor value after UI processing, separately classified from name.
    pub text_value: Option<Content>,
    /// The same selection value used to render the widget.
    pub selected: bool,
}

impl ElementSpec {
    #[must_use]
    pub fn new(key: ElementKey, role: Role, accessible_name: Content) -> Self {
        Self {
            key,
            role,
            accessible_name,
            parent: None,
            content: None,
            control: None,
            text_value: None,
            selected: false,
        }
    }

    #[must_use]
    pub fn action(key: ElementKey, role: Role, accessible_name: Content) -> Self {
        Self {
            key,
            role,
            accessible_name,
            parent: None,
            content: None,
            control: None,
            text_value: None,
            selected: false,
        }
    }

    #[must_use]
    pub fn with_parent(mut self, parent: ElementKey) -> Self {
        self.parent = Some(parent);
        self
    }

    #[must_use]
    pub fn with_content(mut self, content: Content) -> Self {
        self.content = Some(content);
        self
    }
    #[must_use]
    pub fn with_control(mut self, control: ControlMetadata) -> Self {
        self.control = Some(control);
        self
    }

    #[must_use]
    pub fn with_selected(mut self, selected: bool) -> Self { self.selected = selected; self }

    #[must_use]
    pub fn with_text_value(mut self, value: Content) -> Self {
        self.text_value = Some(value);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegionSpec {
    pub key: ElementKey,
    pub role: Role,
    pub accessible_name: Option<Content>,
    pub parent: Option<ElementKey>,
}

impl RegionSpec {
    pub fn new(key: impl Into<String>, role: Role) -> Result<Self, KeyError> {
        Ok(Self {
            key: ElementKey::parse(key)?,
            role,
            accessible_name: None,
            parent: None,
        })
    }

    #[must_use]
    pub fn with_accessible_name(mut self, accessible_name: Content) -> Self {
        self.accessible_name = Some(accessible_name);
        self
    }

    #[must_use]
    pub fn with_parent(mut self, parent: ElementKey) -> Self {
        self.parent = Some(parent);
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowObservation {
    pub title: Content,
    pub content_rect: Rect,
    pub viewport_rect: Rect,
    pub scale: Fixed6,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ElementObservation {
    pub key: ElementKey,
    pub role: Role,
    pub accessible_name: Content,
    pub content: Option<Content>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ControlMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_value: Option<Content>,
    pub widget_id: Content,
    pub parent: Option<ElementKey>,
    pub layout_rect: Rect,
    pub clip_rect: Rect,
    pub viewport_rect: Rect,
    pub window_content_rect: Rect,
    pub visible_rect: Rect,
    pub visible_fraction: Fraction,
    pub layer: i64,
    pub states: States,
}

impl ElementObservation {
    pub fn validate(&self) -> Result<(), ObservationError> {
        if let Some(control) = &self.control { control.validate()?; }
        if self.text_value.is_some() && self.role != Role::TextInput {
            return Err(ObservationError::InvalidControlMetadata);
        }
        if self.role.is_actionable()
            && self
                .accessible_name
                .public_value()
                .is_none_or(str::is_empty)
        {
            return Err(ObservationError::EmptyActionableName);
        }
        let visible = Rect::intersect_all(&[
            self.layout_rect,
            self.clip_rect,
            self.viewport_rect,
            self.window_content_rect,
        ])
        .expect("four rectangles");
        let fraction = self.layout_rect.visible_fraction(visible)?;
        if visible != self.visible_rect || fraction != self.visible_fraction {
            return Err(ObservationError::VisibilityDisagreement);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionObservation {
    pub key: ElementKey,
    pub role: Role,
    pub accessible_name: Option<Content>,
    pub parent: Option<ElementKey>,
    pub layout_rect: Rect,
    pub clip_rect: Rect,
    pub viewport_rect: Rect,
    pub window_content_rect: Rect,
    pub visible_rect: Rect,
    pub visible_fraction: Fraction,
    pub content_extent_x: Fixed6,
    pub content_extent_y: Fixed6,
    pub offset_x: Fixed6,
    pub offset_y: Fixed6,
    pub scroll_range_x: Fixed6,
    pub scroll_range_y: Fixed6,
    pub exposed_children: Vec<ElementKey>,
}

impl RegionObservation {
    pub fn validate(&self) -> Result<(), ObservationError> {
        let visible = Rect::intersect_all(&[
            self.layout_rect,
            self.clip_rect,
            self.viewport_rect,
            self.window_content_rect,
        ])
        .expect("four rectangles");
        let fraction = self.layout_rect.visible_fraction(visible)?;
        if visible != self.visible_rect || fraction != self.visible_fraction {
            return Err(ObservationError::VisibilityDisagreement);
        }
        if self.scroll_range_x < Fixed6::ZERO || self.scroll_range_y < Fixed6::ZERO {
            return Err(ObservationError::InvalidScrollExtent);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionAck {
    pub target: ElementKey,
    pub action_sequence: u64,
    pub response: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateRevision {
    pub target: ElementKey,
    pub revision: u64,
    pub cause_action_sequence: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordDropped {
    pub reason: DropReason,
    pub count: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    Oversized,
    QueueFull,
    Serialization,
    ChannelClosed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamEnd {
    pub records_emitted: u64,
    pub records_dropped: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum ObservationBody {
    Window(WindowObservation),
    Element(ElementObservation),
    Region(RegionObservation),
    ActionAck(ActionAck),
    StateRevision(StateRevision),
    RecordDropped(RecordDropped),
    StreamEnd(StreamEnd),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub schema: String,
    pub run_nonce: String,
    pub sequence: u64,
    pub frame_sequence: u64,
    pub window_key: ElementKey,
    #[serde(flatten)]
    pub record: ObservationBody,
}

#[derive(Debug, Error)]
pub enum ObservationError {
    #[error("INVALID_CONTROL_METADATA")]
    InvalidControlMetadata,
    #[error("SCHEMA_MISMATCH")]
    SchemaMismatch,
    #[error("INVALID_NONCE")]
    InvalidNonce,
    #[error("EMPTY_ACTIONABLE_NAME")]
    EmptyActionableName,
    #[error("VISIBILITY_DISAGREEMENT")]
    VisibilityDisagreement,
    #[error("INVALID_SCROLL_EXTENT")]
    InvalidScrollExtent,
    #[error(transparent)]
    Geometry(#[from] GeometryError),
}

impl Observation {
    pub fn new(
        run_nonce: String,
        sequence: u64,
        frame_sequence: u64,
        window_key: ElementKey,
        record: ObservationBody,
    ) -> Result<Self, ObservationError> {
        if !is_nonce(&run_nonce) {
            return Err(ObservationError::InvalidNonce);
        }
        let observation = Self {
            schema: PROTOCOL_SCHEMA.to_owned(),
            run_nonce,
            sequence,
            frame_sequence,
            window_key,
            record,
        };
        observation.validate()?;
        Ok(observation)
    }

    pub fn validate(&self) -> Result<(), ObservationError> {
        if self.schema != PROTOCOL_SCHEMA {
            return Err(ObservationError::SchemaMismatch);
        }
        if !is_nonce(&self.run_nonce) {
            return Err(ObservationError::InvalidNonce);
        }
        match &self.record {
            ObservationBody::Element(element) => element.validate()?,
            ObservationBody::Region(region) => region.validate()?,
            _ => {}
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, crate::canonical::CanonicalError> {
        canonical_json(self)
    }
}

#[must_use]
pub fn is_nonce(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_is_bounded_and_declarations_are_not_effects() {
        let reveal = RevealSpec { on: InteractionKind::Activate,
            target: ElementKey::parse("dialog.editor").unwrap(), role: Role::TextInput };
        let mut metadata = ControlMetadata { interactions: vec![InteractionKind::Activate],
            scope: None, reveals: vec![reveal.clone()] };
        assert!(metadata.validate().is_ok());
        metadata.interactions.clear();
        assert!(metadata.validate().is_err());
        metadata.interactions.push(InteractionKind::Activate);
        metadata.reveals = vec![reveal; 33];
        assert!(metadata.validate().is_err());
        metadata.reveals.clear();
        metadata.interactions.push(InteractionKind::Activate);
        assert!(metadata.validate().is_err());
    }

    #[test]
    fn keys_reject_display_text_and_bad_hierarchy() {
        for good in ["window", "question.submit", "row_1.cell-2"] {
            assert!(ElementKey::parse(good).is_ok());
        }
        for bad in [
            "",
            ".root",
            "root.",
            "root..child",
            "Submit",
            "user name",
            "é",
        ] {
            assert!(ElementKey::parse(bad).is_err());
        }
    }

    #[test]
    fn content_has_no_implicit_plaintext_path() {
        let private = Content::private_omitted(17);
        assert_eq!(
            canonical_json(&private).unwrap(),
            br#"{"byte_length":17,"class":"private_omitted"}"#
        );
        let digest = Content::digest_only(b"secret");
        let encoded = serde_json::to_string(&digest).unwrap();
        assert!(!encoded.contains("secret"));
        assert!(encoded.contains("sha3_256"));
    }
}
