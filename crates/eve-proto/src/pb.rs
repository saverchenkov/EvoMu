//! `pb` — Descriptor-checked Protobuf builder (`#933`, C-9) and `RawEncodeFieldRatchet`.
//!
//! Replaces unchecked raw `encode_{varint,bytes,string,fixed32,fixed64}_field(field_num, ...)`
//! calls with schema-validated construction against the V24.01 [`QuasarDescriptorRegistry`]:
//!
//! ```rust,no_run
//! use eve_proto::pb;
//!
//! let bytes = pb::message("eve_public.ui.Annotation")
//!     .set("key", "group")
//!     .set("value", "Explorer")
//!     .encode()
//!     .unwrap();
//! ```
//!
//! Any unknown message type, misspelled field name or nested dot-path, wrong wire/value type,
//! or unknown enum variant fails cleanly with a descriptive [`PbError`] listing valid field names
//! (or valid enum variants) for that descriptor.

use crate::quasar::{
    encode_bytes_field, strip_quasar_type_url_prefix, DescriptorProto, DynamicQuasarMessage,
    DynamicValue, FieldDescriptorProto, FieldLabel, FieldType, GoogleAny, GoogleDuration,
    GoogleTimestamp, QuasarDescriptorRegistry, QuasarError, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use crate::MAX_RECURSION_DEPTH;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use thiserror::Error;

/// Error returned by [`ProtoBuilder`] when constructing or encoding a descriptor-checked message.
#[derive(Debug, Error, Clone, PartialEq)]
pub enum PbError {
    #[error("Unknown protobuf message type '{type_name}' (not found in QuasarDescriptorRegistry)")]
    UnknownMessageType { type_name: String },

    #[error("Invalid empty field path '{field_path}' on protobuf message '{message_type}'")]
    InvalidFieldPath {
        message_type: String,
        field_path: String,
    },

    #[error("Unknown field '{field}' on protobuf message '{message_type}'. Valid fields: [{}]", valid_fields.join(", "))]
    UnknownField {
        message_type: String,
        field: String,
        valid_fields: Vec<String>,
    },

    #[error("Cannot traverse nested path '.{remaining_path}' through non-message field '{field}' ({field_type:?}) on '{message_type}'")]
    NonMessageTraversal {
        message_type: String,
        field: String,
        field_type: FieldType,
        remaining_path: String,
    },

    #[error("Cannot traverse nested path '.{remaining_path}' through repeated field '{field}' on '{message_type}'; build the element with .add_with(...) or .add(...) instead")]
    RepeatedPathSegment {
        message_type: String,
        field: String,
        remaining_path: String,
    },

    #[error("Type mismatch for field '{field}' on '{message_type}': expected {expected}, got {actual}")]
    TypeMismatch {
        message_type: String,
        field: String,
        expected: String,
        actual: String,
    },

    #[error("Submessage type mismatch for field '{field}' on '{message_type}': expected '{expected_type}', got '{actual_type}'")]
    MessageTypeMismatch {
        message_type: String,
        field: String,
        expected_type: String,
        actual_type: String,
    },

    #[error("Unknown enum variant '{value}' for field '{field}' ({enum_type}) on '{message_type}'. Valid values: [{}]", valid_values.join(", "))]
    UnknownEnumValue {
        message_type: String,
        field: String,
        enum_type: String,
        value: String,
        valid_values: Vec<String>,
    },

    #[error("Cannot call .add() on singular field '{field}' of '{message_type}'; use .set() instead")]
    SingularFieldForAdd { message_type: String, field: String },

    #[error("Cannot call .set() with a single value on repeated field '{field}' of '{message_type}'; use .add(), .add_all(), or PbValue::repeated(...)")]
    RepeatedFieldNeedsAddOrList { message_type: String, field: String },

    #[error("Invalid raw wire bytes for submessage field '{field}' on '{message_type}': {detail}")]
    InvalidRawSubmessage {
        message_type: String,
        field: String,
        detail: String,
    },

    #[error("Protobuf builder nesting depth exceeded ({MAX_RECURSION_DEPTH}) at '{field_path}' on '{message_type}'")]
    RecursionLimitExceeded {
        message_type: String,
        field_path: String,
    },

    #[error(transparent)]
    DescriptorError(#[from] QuasarError),
}

/// Typed value passed to [`ProtoBuilder::set`] or [`ProtoBuilder::add`].
#[derive(Debug, Clone)]
pub enum PbValue {
    Bool(bool),
    I32(i32),
    I64(i64),
    U32(u32),
    U64(u64),
    F32(f32),
    F64(f64),
    String(String),
    Bytes(Vec<u8>),
    EnumName(String),
    Builder(Box<ProtoBuilder>),
    DynamicMessage(Box<DynamicQuasarMessage>),
    RawMessage(Vec<u8>),
    Repeated(Vec<PbValue>),
}

impl PbValue {
    /// Creates an enum-variant-by-name value (resolved against `EnumDescriptorProto`).
    pub fn enum_name(name: impl Into<String>) -> Self {
        Self::EnumName(name.into())
    }

    /// Creates a pre-encoded submessage payload value (wire-framed bytes for a `Message` field).
    pub fn raw_message(bytes: impl AsRef<[u8]>) -> Self {
        Self::RawMessage(bytes.as_ref().to_vec())
    }

    /// Creates a repeated list value from any iterator whose items convert into [`PbValue`].
    pub fn repeated<I, T>(items: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<PbValue>,
    {
        Self::Repeated(items.into_iter().map(Into::into).collect())
    }

    fn kind_name(&self) -> &'static str {
        match self {
            Self::Bool(_) => "bool",
            Self::I32(_) => "i32",
            Self::I64(_) => "i64",
            Self::U32(_) => "u32",
            Self::U64(_) => "u64",
            Self::F32(_) => "f32",
            Self::F64(_) => "f64",
            Self::String(_) => "string",
            Self::Bytes(_) => "bytes",
            Self::EnumName(_) => "enum_name",
            Self::Builder(_) => "ProtoBuilder",
            Self::DynamicMessage(_) => "DynamicQuasarMessage",
            Self::RawMessage(_) => "raw_message_bytes",
            Self::Repeated(_) => "repeated",
        }
    }
}

/// Shorthand for [`PbValue::enum_name`].
pub fn enum_name(name: impl Into<String>) -> PbValue {
    PbValue::enum_name(name)
}

/// Shorthand for [`PbValue::raw_message`].
pub fn raw_message(bytes: impl AsRef<[u8]>) -> PbValue {
    PbValue::raw_message(bytes)
}

impl From<bool> for PbValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl From<i32> for PbValue {
    fn from(v: i32) -> Self {
        Self::I32(v)
    }
}

impl From<i64> for PbValue {
    fn from(v: i64) -> Self {
        Self::I64(v)
    }
}

impl From<u32> for PbValue {
    fn from(v: u32) -> Self {
        Self::U32(v)
    }
}

impl From<u64> for PbValue {
    fn from(v: u64) -> Self {
        Self::U64(v)
    }
}

impl From<f32> for PbValue {
    fn from(v: f32) -> Self {
        Self::F32(v)
    }
}

impl From<f64> for PbValue {
    fn from(v: f64) -> Self {
        Self::F64(v)
    }
}

impl From<&str> for PbValue {
    fn from(v: &str) -> Self {
        Self::String(v.to_string())
    }
}

impl From<String> for PbValue {
    fn from(v: String) -> Self {
        Self::String(v)
    }
}

impl From<&String> for PbValue {
    fn from(v: &String) -> Self {
        Self::String(v.clone())
    }
}

impl From<&[u8]> for PbValue {
    fn from(v: &[u8]) -> Self {
        Self::Bytes(v.to_vec())
    }
}

impl From<Vec<u8>> for PbValue {
    fn from(v: Vec<u8>) -> Self {
        Self::Bytes(v)
    }
}

impl From<&Vec<u8>> for PbValue {
    fn from(v: &Vec<u8>) -> Self {
        Self::Bytes(v.clone())
    }
}

impl<const N: usize> From<[u8; N]> for PbValue {
    fn from(v: [u8; N]) -> Self {
        Self::Bytes(v.to_vec())
    }
}

impl<const N: usize> From<&[u8; N]> for PbValue {
    fn from(v: &[u8; N]) -> Self {
        Self::Bytes(v.to_vec())
    }
}

impl From<ProtoBuilder> for PbValue {
    fn from(v: ProtoBuilder) -> Self {
        Self::Builder(Box::new(v))
    }
}

impl From<DynamicQuasarMessage> for PbValue {
    fn from(v: DynamicQuasarMessage) -> Self {
        Self::DynamicMessage(Box::new(v))
    }
}

impl From<GoogleTimestamp> for PbValue {
    fn from(ts: GoogleTimestamp) -> Self {
        let mut msg = DynamicQuasarMessage::new("google.protobuf.Timestamp");
        if ts.seconds != 0 {
            msg.set(1, DynamicValue::Int64(ts.seconds));
        }
        if ts.nanos != 0 {
            msg.set(2, DynamicValue::Int32(ts.nanos));
        }
        Self::DynamicMessage(Box::new(msg))
    }
}

impl From<GoogleDuration> for PbValue {
    fn from(dur: GoogleDuration) -> Self {
        let mut msg = DynamicQuasarMessage::new("google.protobuf.Duration");
        if dur.seconds != 0 {
            msg.set(1, DynamicValue::Int64(dur.seconds));
        }
        if dur.nanos != 0 {
            msg.set(2, DynamicValue::Int32(dur.nanos));
        }
        Self::DynamicMessage(Box::new(msg))
    }
}

macro_rules! impl_from_vec_pb {
    ($($t:ty),* $(,)?) => {
        $(
            impl From<Vec<$t>> for PbValue {
                fn from(items: Vec<$t>) -> Self {
                    Self::Repeated(items.into_iter().map(Into::into).collect())
                }
            }
        )*
    };
}

impl_from_vec_pb!(
    ProtoBuilder,
    DynamicQuasarMessage,
    String,
    &str,
    bool,
    i32,
    i64,
    u32,
    u64,
    f32,
    f64,
);

#[derive(Debug, Clone)]
struct PbNode {
    full_name: String,
    fields: BTreeMap<u32, PbStoredValue>,
}

impl PbNode {
    fn new(full_name: impl Into<String>) -> Self {
        Self {
            full_name: full_name.into(),
            fields: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone)]
enum PbStoredValue {
    Dynamic(DynamicValue),
    SubMessage(Box<PbNode>),
    RawMessage(Vec<u8>),
    Repeated(Vec<PbStoredValue>),
}

#[derive(Debug, Clone)]
struct ProtoBuilderState {
    registry: Arc<QuasarDescriptorRegistry>,
    root: PbNode,
}

/// Fluent, descriptor-checked Protobuf message builder (`#933`, C-9).
#[derive(Debug, Clone)]
pub struct ProtoBuilder {
    state: Result<ProtoBuilderState, PbError>,
}

static PB_FALLBACK_REGISTRY: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();

fn default_registry() -> Result<Arc<QuasarDescriptorRegistry>, PbError> {
    if let Ok(reg) = QuasarDescriptorRegistry::open_default() {
        return Ok(reg);
    }
    if let Some(reg) = PB_FALLBACK_REGISTRY.get() {
        return Ok(Arc::clone(reg));
    }
    if std::env::var_os("EVOMU_QUASAR_DESCRIPTORS_PATH").is_none() {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
            let p = PathBuf::from(manifest_dir);
            if let Some(root) = p.parent().and_then(|parent| parent.parent()) {
                candidates.push(root.to_path_buf());
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            for anc in cwd.ancestors() {
                candidates.push(anc.to_path_buf());
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            for anc in exe.ancestors() {
                candidates.push(anc.to_path_buf());
            }
        }
        for root in candidates {
            let sqlite = root.join("data").join("quasar_descriptors.sqlite");
            let manifest = root.join("data").join("quasar_descriptor_manifest.json");
            if sqlite.exists() && manifest.exists() {
                if let Ok(reg) = QuasarDescriptorRegistry::open(&sqlite, &manifest) {
                    let arc = Arc::new(reg);
                    let _ = PB_FALLBACK_REGISTRY.set(Arc::clone(&arc));
                    return Ok(arc);
                }
            }
        }
    }
    Err(PbError::DescriptorError(
        QuasarDescriptorRegistry::open_default().unwrap_err(),
    ))
}

/// Starts building a descriptor-checked protobuf message of `type_name` (full name or `type_url`)
/// using the default [`QuasarDescriptorRegistry`].
pub fn message(type_name: impl AsRef<str>) -> ProtoBuilder {
    match default_registry() {
        Ok(reg) => message_with_registry(reg, type_name),
        Err(err) => ProtoBuilder { state: Err(err) },
    }
}

/// Starts building a descriptor-checked protobuf message of `type_name` against an explicit
/// [`QuasarDescriptorRegistry`].
pub fn message_with_registry(
    registry: Arc<QuasarDescriptorRegistry>,
    type_name: impl AsRef<str>,
) -> ProtoBuilder {
    let raw_name = type_name.as_ref();
    let norm = strip_quasar_type_url_prefix(raw_name);
    let Some(desc) = registry.get_message(norm) else {
        return ProtoBuilder {
            state: Err(PbError::UnknownMessageType {
                type_name: raw_name.to_string(),
            }),
        };
    };
    let full_name = desc.full_name.clone();
    ProtoBuilder {
        state: Ok(ProtoBuilderState {
            registry,
            root: PbNode::new(full_name),
        }),
    }
}

impl ProtoBuilder {
    /// Returns the resolved message full name if initialization succeeded.
    pub fn message_type(&self) -> Result<&str, PbError> {
        match &self.state {
            Ok(s) => Ok(&s.root.full_name),
            Err(e) => Err(e.clone()),
        }
    }

    /// Validates that no errors were recorded while constructing this builder.
    pub fn validate(&self) -> Result<(), PbError> {
        self.state.as_ref().map(|_| ()).map_err(Clone::clone)
    }

    /// Sets `field_path` (either a field name or nested dot-separated path like `"goal.uuid"`)
    /// to `value`, validating field names, intermediate message types, oneofs, and wire/value types.
    pub fn set(mut self, field_path: &str, value: impl Into<PbValue>) -> Self {
        let val = value.into();
        if let Ok(state) = &mut self.state {
            if let Err(err) = apply_field_op(
                &state.registry,
                &mut state.root,
                field_path,
                FieldOp::Set(val),
                0,
            ) {
                self.state = Err(err);
            }
        }
        self
    }

    /// Sets `field_path` if `value` is `Some`, otherwise leaves the builder unchanged.
    pub fn set_opt<V: Into<PbValue>>(self, field_path: &str, value: Option<V>) -> Self {
        match value {
            Some(v) => self.set(field_path, v),
            None => self,
        }
    }

    /// Sets `field_path` to pre-encoded submessage wire bytes (`PbValue::RawMessage`), validating
    /// that `field_path` resolves to a `Message` field and that `wire_bytes` is well-formed protobuf.
    pub fn set_raw_message(self, field_path: &str, wire_bytes: impl AsRef<[u8]>) -> Self {
        self.set(field_path, PbValue::raw_message(wire_bytes))
    }

    /// Builds a nested message field at `field_path` using the descriptor-resolved target message type.
    pub fn set_with(
        mut self,
        field_path: &str,
        build_fn: impl FnOnce(ProtoBuilder) -> ProtoBuilder,
    ) -> Self {
        if let Ok(state) = &mut self.state {
            match resolve_leaf_target_type(&state.registry, &state.root.full_name, field_path, false)
            {
                Ok(target_type) => {
                    let sub = build_fn(message_with_registry(
                        Arc::clone(&state.registry),
                        &target_type,
                    ));
                    if let Err(err) = apply_field_op(
                        &state.registry,
                        &mut state.root,
                        field_path,
                        FieldOp::Set(PbValue::Builder(Box::new(sub))),
                        0,
                    ) {
                        self.state = Err(err);
                    }
                }
                Err(err) => self.state = Err(err),
            }
        }
        self
    }

    /// Appends one element `value` to repeated field `field_path`.
    pub fn add(mut self, field_path: &str, value: impl Into<PbValue>) -> Self {
        let val = value.into();
        if let Ok(state) = &mut self.state {
            if let Err(err) = apply_field_op(
                &state.registry,
                &mut state.root,
                field_path,
                FieldOp::Add(val),
                0,
            ) {
                self.state = Err(err);
            }
        }
        self
    }

    /// Appends one element to repeated message field `field_path` using the descriptor-resolved
    /// target message type.
    pub fn add_with(
        mut self,
        field_path: &str,
        build_fn: impl FnOnce(ProtoBuilder) -> ProtoBuilder,
    ) -> Self {
        if let Ok(state) = &mut self.state {
            match resolve_leaf_target_type(&state.registry, &state.root.full_name, field_path, true)
            {
                Ok(target_type) => {
                    let sub = build_fn(message_with_registry(
                        Arc::clone(&state.registry),
                        &target_type,
                    ));
                    if let Err(err) = apply_field_op(
                        &state.registry,
                        &mut state.root,
                        field_path,
                        FieldOp::Add(PbValue::Builder(Box::new(sub))),
                        0,
                    ) {
                        self.state = Err(err);
                    }
                }
                Err(err) => self.state = Err(err),
            }
        }
        self
    }

    /// Appends every element in `values` to repeated field `field_path`.
    pub fn add_all<I, V>(mut self, field_path: &str, values: I) -> Self
    where
        I: IntoIterator<Item = V>,
        V: Into<PbValue>,
    {
        for v in values {
            self = self.add(field_path, v);
        }
        self
    }

    /// Conditionally applies `f` to `self` when `cond` is `true`.
    pub fn when(self, cond: bool, f: impl FnOnce(Self) -> Self) -> Self {
        if cond {
            f(self)
        } else {
            self
        }
    }

    /// Applies `f` unconditionally (useful for inline loops or helper blocks in a fluent chain).
    pub fn with(self, f: impl FnOnce(Self) -> Self) -> Self {
        f(self)
    }

    /// Encodes the validated message into canonical protobuf wire bytes.
    pub fn encode(self) -> Result<Vec<u8>, PbError> {
        let state = self.state?;
        let mut out = Vec::new();
        encode_node(&state.registry, &state.root, &mut out)?;
        Ok(out)
    }

    /// Encodes the validated message into `out`.
    pub fn encode_into(self, out: &mut Vec<u8>) -> Result<(), PbError> {
        let state = self.state?;
        encode_node(&state.registry, &state.root, out)
    }

    /// Encodes the validated message into a [`GoogleAny`] with `type.googleapis.com/<full_name>`.
    pub fn to_any(self) -> Result<GoogleAny, PbError> {
        let state = self.state?;
        let type_url = format!("{TYPE_GOOGLEAPIS_PREFIX}{}", state.root.full_name);
        let mut value = Vec::new();
        encode_node(&state.registry, &state.root, &mut value)?;
        Ok(GoogleAny { type_url, value })
    }

    /// Converts the validated builder into a [`DynamicQuasarMessage`].
    pub fn to_dynamic(self) -> Result<DynamicQuasarMessage, PbError> {
        let state = self.state?;
        node_to_dynamic(&state.registry, &state.root)
    }

    /// Encodes the message, panicking with the descriptive [`PbError`] on any schema violation.
    pub fn must_encode(self) -> Vec<u8> {
        self.encode()
            .unwrap_or_else(|e| panic!("ProtoBuilder encode failed: {e}"))
    }

    /// Encodes the message into `out`, panicking with the descriptive [`PbError`] on failure.
    pub fn must_encode_into(self, out: &mut Vec<u8>) {
        self.encode_into(out)
            .unwrap_or_else(|e| panic!("ProtoBuilder encode_into failed: {e}"));
    }

    /// Encodes the message as a [`GoogleAny`], panicking with the descriptive [`PbError`] on failure.
    pub fn must_to_any(self) -> GoogleAny {
        self.to_any()
            .unwrap_or_else(|e| panic!("ProtoBuilder to_any failed: {e}"))
    }

    /// Converts to [`DynamicQuasarMessage`], panicking with the descriptive [`PbError`] on failure.
    pub fn must_to_dynamic(self) -> DynamicQuasarMessage {
        self.to_dynamic()
            .unwrap_or_else(|e| panic!("ProtoBuilder to_dynamic failed: {e}"))
    }
}

enum FieldOp {
    Set(PbValue),
    Add(PbValue),
}

fn valid_field_names(desc: &DescriptorProto) -> Vec<String> {
    desc.fields.iter().map(|f| f.name.clone()).collect()
}

fn lookup_field<'a>(
    desc: &'a DescriptorProto,
    segment: &str,
) -> Result<&'a FieldDescriptorProto, PbError> {
    desc.fields
        .iter()
        .find(|f| f.name == segment)
        .ok_or_else(|| PbError::UnknownField {
            message_type: desc.full_name.clone(),
            field: segment.to_string(),
            valid_fields: valid_field_names(desc),
        })
}

fn resolve_leaf_target_type(
    registry: &QuasarDescriptorRegistry,
    root_type: &str,
    field_path: &str,
    for_add: bool,
) -> Result<String, PbError> {
    if field_path.is_empty() || field_path.split('.').any(str::is_empty) {
        return Err(PbError::InvalidFieldPath {
            message_type: root_type.to_string(),
            field_path: field_path.to_string(),
        });
    }
    let segments: Vec<&str> = field_path.split('.').collect();
    let mut cur_type = root_type.to_string();
    for (idx, &seg) in segments.iter().enumerate() {
        let desc = registry
            .get_message(&cur_type)
            .ok_or_else(|| PbError::UnknownMessageType {
                type_name: cur_type.clone(),
            })?;
        let fdesc = lookup_field(desc, seg)?;
        let is_last = idx + 1 == segments.len();
        if is_last {
            if for_add && fdesc.label != FieldLabel::Repeated {
                return Err(PbError::SingularFieldForAdd {
                    message_type: desc.full_name.clone(),
                    field: fdesc.name.clone(),
                });
            }
            if !for_add && fdesc.label == FieldLabel::Repeated {
                return Err(PbError::RepeatedFieldNeedsAddOrList {
                    message_type: desc.full_name.clone(),
                    field: fdesc.name.clone(),
                });
            }
            if !matches!(fdesc.field_type, FieldType::Message | FieldType::Group) {
                return Err(PbError::TypeMismatch {
                    message_type: desc.full_name.clone(),
                    field: fdesc.name.clone(),
                    expected: format!("{:?} (non-message)", fdesc.field_type),
                    actual: "ProtoBuilder".to_string(),
                });
            }
            let target = strip_quasar_type_url_prefix(&fdesc.type_name).to_string();
            if registry.get_message(&target).is_none() {
                return Err(PbError::UnknownMessageType { type_name: target });
            }
            return Ok(target);
        }
        if fdesc.label == FieldLabel::Repeated {
            return Err(PbError::RepeatedPathSegment {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                remaining_path: segments[idx + 1..].join("."),
            });
        }
        if !matches!(fdesc.field_type, FieldType::Message | FieldType::Group) {
            return Err(PbError::NonMessageTraversal {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                field_type: fdesc.field_type,
                remaining_path: segments[idx + 1..].join("."),
            });
        }
        cur_type = strip_quasar_type_url_prefix(&fdesc.type_name).to_string();
    }
    unreachable!("segments is non-empty")
}

fn evict_oneof_siblings(desc: &DescriptorProto, node: &mut PbNode, fdesc: &FieldDescriptorProto) {
    if let Some(oneof_idx) = fdesc.oneof_index {
        for sib in &desc.fields {
            if sib.oneof_index == Some(oneof_idx) && sib.number != fdesc.number {
                node.fields.remove(&sib.number);
            }
        }
    }
}

fn apply_field_op(
    registry: &QuasarDescriptorRegistry,
    node: &mut PbNode,
    field_path: &str,
    op: FieldOp,
    depth: usize,
) -> Result<(), PbError> {
    if depth > MAX_RECURSION_DEPTH {
        return Err(PbError::RecursionLimitExceeded {
            message_type: node.full_name.clone(),
            field_path: field_path.to_string(),
        });
    }
    if field_path.is_empty() || field_path.split('.').any(str::is_empty) {
        return Err(PbError::InvalidFieldPath {
            message_type: node.full_name.clone(),
            field_path: field_path.to_string(),
        });
    }

    let desc = registry
        .get_message(&node.full_name)
        .ok_or_else(|| PbError::UnknownMessageType {
            type_name: node.full_name.clone(),
        })?;

    if let Some((head, rest)) = field_path.split_once('.') {
        let fdesc = lookup_field(desc, head)?;
        if fdesc.label == FieldLabel::Repeated {
            return Err(PbError::RepeatedPathSegment {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                remaining_path: rest.to_string(),
            });
        }
        if !matches!(fdesc.field_type, FieldType::Message | FieldType::Group) {
            return Err(PbError::NonMessageTraversal {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                field_type: fdesc.field_type,
                remaining_path: rest.to_string(),
            });
        }
        let target_type = strip_quasar_type_url_prefix(&fdesc.type_name);
        if registry.get_message(target_type).is_none() {
            return Err(PbError::UnknownMessageType {
                type_name: target_type.to_string(),
            });
        }
        evict_oneof_siblings(desc, node, fdesc);
        let entry = node
            .fields
            .entry(fdesc.number)
            .or_insert_with(|| PbStoredValue::SubMessage(Box::new(PbNode::new(target_type))));
        if !matches!(entry, PbStoredValue::SubMessage(_)) {
            *entry = PbStoredValue::SubMessage(Box::new(PbNode::new(target_type)));
        }
        let PbStoredValue::SubMessage(sub_node) = entry else {
            unreachable!();
        };
        return apply_field_op(registry, sub_node, rest, op, depth + 1);
    }

    let fdesc = lookup_field(desc, field_path)?;
    match op {
        FieldOp::Set(val) => {
            if fdesc.label == FieldLabel::Repeated {
                let PbValue::Repeated(items) = val else {
                    return Err(PbError::RepeatedFieldNeedsAddOrList {
                        message_type: desc.full_name.clone(),
                        field: fdesc.name.clone(),
                    });
                };
                let mut converted = Vec::with_capacity(items.len());
                for item in items {
                    converted.push(convert_single_value(registry, desc, fdesc, item)?);
                }
                node.fields
                    .insert(fdesc.number, PbStoredValue::Repeated(converted));
            } else {
                let converted = convert_single_value(registry, desc, fdesc, val)?;
                evict_oneof_siblings(desc, node, fdesc);
                node.fields.insert(fdesc.number, converted);
            }
        }
        FieldOp::Add(val) => {
            if fdesc.label != FieldLabel::Repeated {
                return Err(PbError::SingularFieldForAdd {
                    message_type: desc.full_name.clone(),
                    field: fdesc.name.clone(),
                });
            }
            let converted = convert_single_value(registry, desc, fdesc, val)?;
            match node.fields.get_mut(&fdesc.number) {
                Some(PbStoredValue::Repeated(list)) => list.push(converted),
                _ => {
                    node.fields
                        .insert(fdesc.number, PbStoredValue::Repeated(vec![converted]));
                }
            }
        }
    }
    Ok(())
}

fn convert_signed_32(
    desc: &DescriptorProto,
    fdesc: &FieldDescriptorProto,
    val: PbValue,
) -> Result<i32, PbError> {
    let actual = val.kind_name();
    let res = match val {
        PbValue::I32(v) => Some(v),
        PbValue::I64(v) => i32::try_from(v).ok(),
        PbValue::U32(v) => i32::try_from(v).ok(),
        PbValue::U64(v) => i32::try_from(v).ok(),
        _ => None,
    };
    res.ok_or_else(|| PbError::TypeMismatch {
        message_type: desc.full_name.clone(),
        field: fdesc.name.clone(),
        expected: format!("{:?} (32-bit signed integer)", fdesc.field_type),
        actual: actual.to_string(),
    })
}

fn convert_unsigned_32(
    desc: &DescriptorProto,
    fdesc: &FieldDescriptorProto,
    val: PbValue,
) -> Result<u32, PbError> {
    let actual = val.kind_name();
    let res = match val {
        PbValue::U32(v) => Some(v),
        PbValue::U64(v) => u32::try_from(v).ok(),
        PbValue::I32(v) => u32::try_from(v).ok(),
        PbValue::I64(v) => u32::try_from(v).ok(),
        _ => None,
    };
    res.ok_or_else(|| PbError::TypeMismatch {
        message_type: desc.full_name.clone(),
        field: fdesc.name.clone(),
        expected: format!("{:?} (32-bit unsigned integer)", fdesc.field_type),
        actual: actual.to_string(),
    })
}

fn convert_signed_64(
    desc: &DescriptorProto,
    fdesc: &FieldDescriptorProto,
    val: PbValue,
) -> Result<i64, PbError> {
    let actual = val.kind_name();
    let res = match val {
        PbValue::I32(v) => Some(i64::from(v)),
        PbValue::I64(v) => Some(v),
        PbValue::U32(v) => Some(i64::from(v)),
        PbValue::U64(v) => i64::try_from(v).ok(),
        _ => None,
    };
    res.ok_or_else(|| PbError::TypeMismatch {
        message_type: desc.full_name.clone(),
        field: fdesc.name.clone(),
        expected: format!("{:?} (64-bit signed integer)", fdesc.field_type),
        actual: actual.to_string(),
    })
}

fn convert_unsigned_64(
    desc: &DescriptorProto,
    fdesc: &FieldDescriptorProto,
    val: PbValue,
) -> Result<u64, PbError> {
    let actual = val.kind_name();
    let res = match val {
        PbValue::U32(v) => Some(u64::from(v)),
        PbValue::U64(v) => Some(v),
        PbValue::I32(v) => u64::try_from(v).ok(),
        PbValue::I64(v) => u64::try_from(v).ok(),
        _ => None,
    };
    res.ok_or_else(|| PbError::TypeMismatch {
        message_type: desc.full_name.clone(),
        field: fdesc.name.clone(),
        expected: format!("{:?} (64-bit unsigned integer)", fdesc.field_type),
        actual: actual.to_string(),
    })
}

fn convert_single_value(
    registry: &QuasarDescriptorRegistry,
    desc: &DescriptorProto,
    fdesc: &FieldDescriptorProto,
    val: PbValue,
) -> Result<PbStoredValue, PbError> {
    match fdesc.field_type {
        FieldType::Double => match val {
            PbValue::F64(v) => Ok(PbStoredValue::Dynamic(DynamicValue::Double(v))),
            PbValue::F32(v) => Ok(PbStoredValue::Dynamic(DynamicValue::Double(f64::from(v)))),
            other => Err(PbError::TypeMismatch {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                expected: "Double (f64/f32)".to_string(),
                actual: other.kind_name().to_string(),
            }),
        },
        FieldType::Float => match val {
            PbValue::F32(v) => Ok(PbStoredValue::Dynamic(DynamicValue::Float(v))),
            PbValue::F64(v) => Ok(PbStoredValue::Dynamic(DynamicValue::Float(v as f32))),
            other => Err(PbError::TypeMismatch {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                expected: "Float (f32/f64)".to_string(),
                actual: other.kind_name().to_string(),
            }),
        },
        FieldType::Int32 => Ok(PbStoredValue::Dynamic(DynamicValue::Int32(
            convert_signed_32(desc, fdesc, val)?,
        ))),
        FieldType::Sint32 => Ok(PbStoredValue::Dynamic(DynamicValue::Sint32(
            convert_signed_32(desc, fdesc, val)?,
        ))),
        FieldType::Sfixed32 => Ok(PbStoredValue::Dynamic(DynamicValue::Sfixed32(
            convert_signed_32(desc, fdesc, val)?,
        ))),
        FieldType::Uint32 => Ok(PbStoredValue::Dynamic(DynamicValue::Uint32(
            convert_unsigned_32(desc, fdesc, val)?,
        ))),
        FieldType::Fixed32 => Ok(PbStoredValue::Dynamic(DynamicValue::Fixed32(
            convert_unsigned_32(desc, fdesc, val)?,
        ))),
        FieldType::Int64 => Ok(PbStoredValue::Dynamic(DynamicValue::Int64(
            convert_signed_64(desc, fdesc, val)?,
        ))),
        FieldType::Sint64 => Ok(PbStoredValue::Dynamic(DynamicValue::Sint64(
            convert_signed_64(desc, fdesc, val)?,
        ))),
        FieldType::Sfixed64 => Ok(PbStoredValue::Dynamic(DynamicValue::Sfixed64(
            convert_signed_64(desc, fdesc, val)?,
        ))),
        FieldType::Uint64 => Ok(PbStoredValue::Dynamic(DynamicValue::Uint64(
            convert_unsigned_64(desc, fdesc, val)?,
        ))),
        FieldType::Fixed64 => Ok(PbStoredValue::Dynamic(DynamicValue::Fixed64(
            convert_unsigned_64(desc, fdesc, val)?,
        ))),
        FieldType::Bool => match val {
            PbValue::Bool(b) => Ok(PbStoredValue::Dynamic(DynamicValue::Bool(b))),
            other => Err(PbError::TypeMismatch {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                expected: "Bool (bool)".to_string(),
                actual: other.kind_name().to_string(),
            }),
        },
        FieldType::String => match val {
            PbValue::String(s) => Ok(PbStoredValue::Dynamic(DynamicValue::String(s))),
            other => Err(PbError::TypeMismatch {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                expected: "String (&str / String)".to_string(),
                actual: other.kind_name().to_string(),
            }),
        },
        FieldType::Bytes => match val {
            PbValue::Bytes(b) => Ok(PbStoredValue::Dynamic(DynamicValue::Bytes(b))),
            other => Err(PbError::TypeMismatch {
                message_type: desc.full_name.clone(),
                field: fdesc.name.clone(),
                expected: "Bytes (&[u8] / Vec<u8>)".to_string(),
                actual: other.kind_name().to_string(),
            }),
        },
        FieldType::Enum => match val {
            PbValue::EnumName(name) => {
                let enum_name_key = strip_quasar_type_url_prefix(&fdesc.type_name);
                let enum_desc = registry
                    .enums_by_full_name
                    .get(enum_name_key)
                    .ok_or_else(|| PbError::UnknownEnumValue {
                        message_type: desc.full_name.clone(),
                        field: fdesc.name.clone(),
                        enum_type: enum_name_key.to_string(),
                        value: name.clone(),
                        valid_values: Vec::new(),
                    })?;
                if let Some(ev) = enum_desc.values.iter().find(|v| v.name == name) {
                    Ok(PbStoredValue::Dynamic(DynamicValue::Enum(ev.number)))
                } else {
                    Err(PbError::UnknownEnumValue {
                        message_type: desc.full_name.clone(),
                        field: fdesc.name.clone(),
                        enum_type: enum_desc.full_name.clone(),
                        value: name,
                        valid_values: enum_desc.values.iter().map(|v| v.name.clone()).collect(),
                    })
                }
            }
            other => {
                let num = convert_signed_32(desc, fdesc, other)?;
                Ok(PbStoredValue::Dynamic(DynamicValue::Enum(num)))
            }
        },
        FieldType::Message | FieldType::Group => {
            let expected_target = strip_quasar_type_url_prefix(&fdesc.type_name);
            match val {
                PbValue::Builder(sub_builder) => {
                    let sub_state = sub_builder.state?;
                    if sub_state.root.full_name != expected_target {
                        return Err(PbError::MessageTypeMismatch {
                            message_type: desc.full_name.clone(),
                            field: fdesc.name.clone(),
                            expected_type: expected_target.to_string(),
                            actual_type: sub_state.root.full_name,
                        });
                    }
                    Ok(PbStoredValue::SubMessage(Box::new(sub_state.root)))
                }
                PbValue::DynamicMessage(sub_dyn) => {
                    let actual_norm = strip_quasar_type_url_prefix(&sub_dyn.full_name);
                    if actual_norm != expected_target {
                        return Err(PbError::MessageTypeMismatch {
                            message_type: desc.full_name.clone(),
                            field: fdesc.name.clone(),
                            expected_type: expected_target.to_string(),
                            actual_type: actual_norm.to_string(),
                        });
                    }
                    Ok(PbStoredValue::Dynamic(DynamicValue::Message(sub_dyn)))
                }
                PbValue::RawMessage(bytes) => {
                    WireMessage::decode(&bytes).map_err(|e| PbError::InvalidRawSubmessage {
                        message_type: desc.full_name.clone(),
                        field: fdesc.name.clone(),
                        detail: e.to_string(),
                    })?;
                    Ok(PbStoredValue::RawMessage(bytes))
                }
                other => Err(PbError::TypeMismatch {
                    message_type: desc.full_name.clone(),
                    field: fdesc.name.clone(),
                    expected: format!("Message({expected_target})"),
                    actual: other.kind_name().to_string(),
                }),
            }
        }
    }
}

fn encode_node(
    registry: &QuasarDescriptorRegistry,
    node: &PbNode,
    out: &mut Vec<u8>,
) -> Result<(), PbError> {
    let desc = registry.get_message(&node.full_name);
    for (&field_num, stored) in &node.fields {
        let ftype = desc
            .and_then(|d| d.fields.iter().find(|f| f.number == field_num))
            .map(|f| f.field_type);
        encode_stored_value(registry, field_num, ftype, stored, out)?;
    }
    Ok(())
}

fn encode_stored_value(
    registry: &QuasarDescriptorRegistry,
    field_num: u32,
    ftype: Option<FieldType>,
    stored: &PbStoredValue,
    out: &mut Vec<u8>,
) -> Result<(), PbError> {
    match stored {
        PbStoredValue::Dynamic(dv) => {
            registry.encode_dynamic_field(field_num, ftype, dv, out)?;
        }
        PbStoredValue::SubMessage(sub) => {
            let mut sub_bytes = Vec::new();
            encode_node(registry, sub, &mut sub_bytes)?;
            encode_bytes_field(field_num, &sub_bytes, out);
        }
        PbStoredValue::RawMessage(bytes) => {
            encode_bytes_field(field_num, bytes, out);
        }
        PbStoredValue::Repeated(items) => {
            if !items.is_empty() && ftype.is_some_and(FieldType::is_packable_scalar) {
                let mut dyn_items = Vec::with_capacity(items.len());
                for item in items {
                    if let PbStoredValue::Dynamic(dv) = item {
                        dyn_items.push(dv.clone());
                    }
                }
                if dyn_items.len() == items.len() {
                    registry.encode_dynamic_field(
                        field_num,
                        ftype,
                        &DynamicValue::Repeated(dyn_items),
                        out,
                    )?;
                    return Ok(());
                }
            }
            for item in items {
                encode_stored_value(registry, field_num, ftype, item, out)?;
            }
        }
    }
    Ok(())
}

fn node_to_dynamic(
    registry: &QuasarDescriptorRegistry,
    node: &PbNode,
) -> Result<DynamicQuasarMessage, PbError> {
    let desc = registry.get_message(&node.full_name);
    let mut msg = DynamicQuasarMessage::new(node.full_name.clone());
    for (&field_num, stored) in &node.fields {
        let target_type = desc
            .and_then(|d| d.fields.iter().find(|f| f.number == field_num))
            .map(|f| f.type_name.as_str())
            .unwrap_or("");
        msg.fields.insert(
            field_num,
            stored_to_dynamic(registry, target_type, stored)?,
        );
    }
    Ok(msg)
}

fn stored_to_dynamic(
    registry: &QuasarDescriptorRegistry,
    target_type: &str,
    stored: &PbStoredValue,
) -> Result<DynamicValue, PbError> {
    match stored {
        PbStoredValue::Dynamic(dv) => Ok(dv.clone()),
        PbStoredValue::SubMessage(sub) => {
            Ok(DynamicValue::Message(Box::new(node_to_dynamic(registry, sub)?)))
        }
        PbStoredValue::RawMessage(bytes) => {
            let sub = registry.decode_dynamic_or_wire(target_type, bytes)?;
            Ok(DynamicValue::Message(Box::new(sub)))
        }
        PbStoredValue::Repeated(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(stored_to_dynamic(registry, target_type, item)?);
            }
            Ok(DynamicValue::Repeated(out))
        }
    }
}

// ============================================================================
// C-9 (#933) Falling Ratchet for Raw `encode_*_field` Calls in `eve-server/src`
// ============================================================================

const LEGACY_ENCODE_FIELD_NAMES: [&str; 5] = [
    "encode_varint_field",
    "encode_bytes_field",
    "encode_string_field",
    "encode_fixed32_field",
    "encode_fixed64_field",
];

/// Summary of raw `encode_*_field(...)` calls found in a Rust source tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEncodeFieldScanReport {
    pub raw_encode_field_calls: usize,
    pub files_with_raw_encode_field: usize,
    pub per_file_calls: HashMap<String, usize>,
}

/// Falling ratchet limiting raw `encode_{varint,bytes,string,fixed32,fixed64}_field` calls
/// under `crates/eve-server/src/` (`#933`, C-9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawEncodeFieldRatchet {
    pub version: u32,
    pub max_raw_encode_field_calls: usize,
    pub max_files_with_raw_encode_field: usize,
    #[serde(default)]
    pub notes: String,
}

impl RawEncodeFieldRatchet {
    /// Returns the repository path to `crates/eve-proto/ratchets/raw_encode_field_ratchet.json`.
    pub fn default_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("ratchets")
            .join("raw_encode_field_ratchet.json")
    }

    /// Returns the repository path to `crates/eve-server/src`.
    pub fn default_server_src_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../eve-server/src")
    }

    /// Loads `raw_encode_field_ratchet.json` from its default path.
    pub fn load_default() -> Result<Self, String> {
        Self::load_from_path(&Self::default_path())
    }

    /// Loads a [`RawEncodeFieldRatchet`] from `path`.
    pub fn load_from_path(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        serde_json::from_str(&raw)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))
    }

    /// Scans `crates/eve-server/src` and verifies the ratchet ceilings have not regressed.
    pub fn verify_default(&self) -> Result<RawEncodeFieldScanReport, String> {
        let report = scan_raw_encode_field_calls_in_dir(&Self::default_server_src_dir())?;
        self.assert_ratchet(&report)?;
        Ok(report)
    }

    /// Enforces that both total `encode_*_field(...)` calls and distinct files remain at or
    /// below the falling ratchet ceiling.
    pub fn assert_ratchet(&self, report: &RawEncodeFieldScanReport) -> Result<(), String> {
        if report.raw_encode_field_calls > self.max_raw_encode_field_calls {
            return Err(format!(
                "Raw encode_*_field(...) count regressed above falling ratchet: found {}, max_raw_encode_field_calls={}. Use eve_proto::pb::message(type_name) instead.",
                report.raw_encode_field_calls, self.max_raw_encode_field_calls
            ));
        }
        if report.files_with_raw_encode_field > self.max_files_with_raw_encode_field {
            return Err(format!(
                "Files containing encode_*_field(...) regressed above ratchet: found {}, max_files_with_raw_encode_field={}.",
                report.files_with_raw_encode_field, self.max_files_with_raw_encode_field
            ));
        }
        Ok(())
    }
}

impl fmt::Display for RawEncodeFieldScanReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} calls across {} files",
            self.raw_encode_field_calls, self.files_with_raw_encode_field
        )
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn strip_rust_comments_and_literals(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;

    while i < n {
        if b[i] == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' {
                out.push(b' ');
                i += 1;
            }
        } else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut depth = 1usize;
            out.push(b' ');
            out.push(b' ');
            i += 2;
            while i < n && depth > 0 {
                if i + 1 < n && b[i] == b'/' && b[i + 1] == b'*' {
                    depth += 1;
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                } else if i + 1 < n && b[i] == b'*' && b[i + 1] == b'/' {
                    depth -= 1;
                    out.push(b' ');
                    out.push(b' ');
                    i += 2;
                } else {
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            }
        } else if b[i] == b'r'
            && (i == 0
                || !is_ident_byte(b[i - 1])
                || (b[i - 1] == b'b' && (i == 1 || !is_ident_byte(b[i - 2]))))
        {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < n && b[j] == b'#' {
                hashes += 1;
                j += 1;
            }
            if j < n && b[j] == b'"' {
                for _ in i..=j {
                    out.push(b' ');
                }
                i = j + 1;
                while i < n {
                    if b[i] == b'"'
                        && i + hashes < n
                        && b[i + 1..=i + hashes].iter().all(|&c| c == b'#')
                    {
                        for _ in 0..=hashes {
                            out.push(b' ');
                        }
                        i += 1 + hashes;
                        break;
                    }
                    out.push(if b[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            } else {
                out.push(b[i]);
                i += 1;
            }
        } else if b[i] == b'"' {
            out.push(b' ');
            i += 1;
            while i < n {
                let ch = b[i];
                out.push(if ch == b'\n' { b'\n' } else { b' ' });
                i += 1;
                if ch == b'\\' && i < n {
                    let esc = b[i];
                    out.push(if esc == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                    continue;
                }
                if ch == b'"' {
                    break;
                }
            }
        } else if b[i] == b'\'' {
            if i + 2 < n && b[i + 1] == b'\\' {
                let mut k = i + 2;
                let limit = (i + 10).min(n);
                while k < limit && b[k] != b'\'' && b[k] != b'\n' {
                    k += 1;
                }
                if k < limit && b[k] == b'\'' {
                    for _ in i..=k {
                        out.push(b' ');
                    }
                    i = k + 1;
                    continue;
                }
            } else if i + 2 < n && b[i + 2] == b'\'' && b[i + 1] != b'\n' {
                out.extend_from_slice(b"   ");
                i += 3;
                continue;
            }
            out.push(b[i]);
            i += 1;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }

    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

/// Counts legacy `encode_{varint,bytes,string,fixed32,fixed64}_field(...)` call sites in `src`,
/// ignoring comments and string/char literals.
pub fn count_raw_encode_field_calls_in_source(src: &str) -> usize {
    let clean = strip_rust_comments_and_literals(src);
    let b = clean.as_bytes();
    let n = b.len();
    let mut count = 0usize;
    let mut i = 0usize;

    while i < n {
        if !is_ident_byte(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < n && is_ident_byte(b[i]) {
            i += 1;
        }
        let ident = &clean[start..i];
        if LEGACY_ENCODE_FIELD_NAMES.contains(&ident) {
            let mut j = i;
            while j < n && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < n && b[j] == b'(' {
                count += 1;
            }
        }
    }
    count
}

/// Recursively scans all `.rs` files in `dir` and counts raw `encode_*_field(...)` calls.
pub fn scan_raw_encode_field_calls_in_dir(dir: &Path) -> Result<RawEncodeFieldScanReport, String> {
    let mut stack = vec![dir.to_path_buf()];
    let mut raw_encode_field_calls = 0usize;
    let mut files_with_raw_encode_field = 0usize;
    let mut per_file_calls = HashMap::new();

    while let Some(cur) = stack.pop() {
        let entries = std::fs::read_dir(&cur)
            .map_err(|e| format!("failed to read {}: {e}", cur.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("failed to read entry in {}: {e}", cur.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let src = std::fs::read_to_string(&path)
                    .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
                let c = count_raw_encode_field_calls_in_source(&src);
                if c > 0 {
                    raw_encode_field_calls += c;
                    files_with_raw_encode_field += 1;
                    let rel = path
                        .strip_prefix(dir)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    per_file_calls.insert(rel, c);
                }
            }
        }
    }

    Ok(RawEncodeFieldScanReport {
        raw_encode_field_calls,
        files_with_raw_encode_field,
        per_file_calls,
    })
}

#[cfg(test)]
#[path = "pb_unit_tests.rs"]
mod pb_unit_tests;
