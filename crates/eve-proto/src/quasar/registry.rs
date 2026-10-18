//! `quasar::registry` — 1,455-file `QuasarDescriptorRegistry`, `DynamicQuasarMessage` codec,
//! and deterministic Rust codegen (`Q-1`).

use super::*;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DynamicValue {
    Double(f64),
    Float(f32),
    Int64(i64),
    Uint64(u64),
    Int32(i32),
    Uint32(u32),
    Sint32(i32),
    Sint64(i64),
    Fixed32(u32),
    Fixed64(u64),
    Sfixed32(i32),
    Sfixed64(i64),
    Bool(bool),
    String(String),
    Bytes(Vec<u8>),
    Enum(i32),
    Message(Box<DynamicQuasarMessage>),
    Repeated(Vec<DynamicValue>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DynamicQuasarMessage {
    pub full_name: String,
    pub fields: BTreeMap<u32, DynamicValue>,
}

impl DynamicQuasarMessage {
    pub fn new(full_name: impl Into<String>) -> Self {
        Self {
            full_name: full_name.into(),
            fields: BTreeMap::new(),
        }
    }

    pub fn type_url(&self) -> String {
        format!("{TYPE_GOOGLEAPIS_PREFIX}{}", self.full_name)
    }

    pub fn set(&mut self, field_number: u32, value: DynamicValue) {
        self.fields.insert(field_number, value);
    }

    pub fn get(&self, field_number: u32) -> Option<&DynamicValue> {
        self.fields.get(&field_number)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuasarManifest {
    pub schema_version: u32,
    pub domain_count: usize,
    pub descriptor_file_count: usize,
    pub unique_package_count: usize,
    pub top_level_message_count: usize,
    pub total_message_count: usize,
    pub total_enum_count: usize,
    pub total_service_count: usize,
    pub total_field_count: usize,
    pub total_serialized_pb_bytes: usize,
    pub combined_serialized_pb_sha256: String,
    pub combined_serialized_pb_md5: String,
    pub per_domain_file_counts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone)]
pub struct QuasarDescriptorRegistry {
    pub manifest: QuasarManifest,
    pub files: Vec<FileDescriptorProto>,
    pub files_by_pyj_path: HashMap<String, usize>,
    pub files_by_proto_name: HashMap<String, usize>,
    pub files_by_domain: BTreeMap<String, Vec<usize>>,
    pub messages_by_full_name: HashMap<String, DescriptorProto>,
    pub enums_by_full_name: HashMap<String, EnumDescriptorProto>,
    pub services_by_full_name: HashMap<String, ServiceDescriptorProto>,
}

static DEFAULT_QUASAR_REGISTRY: OnceLock<Arc<QuasarDescriptorRegistry>> = OnceLock::new();

fn workspace_root() -> PathBuf {
    if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(manifest_dir);
        if let Some(root) = p.parent().and_then(|parent| parent.parent()) {
            return root.to_path_buf();
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn builtin_google_descriptors() -> Vec<DescriptorProto> {
    vec![
        DescriptorProto {
            name: "Timestamp".into(),
            full_name: "google.protobuf.Timestamp".into(),
            fields: vec![
                FieldDescriptorProto {
                    name: "seconds".into(),
                    number: 1,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Int64,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "seconds".into(),
                },
                FieldDescriptorProto {
                    name: "nanos".into(),
                    number: 2,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Int32,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "nanos".into(),
                },
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
        DescriptorProto {
            name: "Duration".into(),
            full_name: "google.protobuf.Duration".into(),
            fields: vec![
                FieldDescriptorProto {
                    name: "seconds".into(),
                    number: 1,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Int64,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "seconds".into(),
                },
                FieldDescriptorProto {
                    name: "nanos".into(),
                    number: 2,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Int32,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "nanos".into(),
                },
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
        DescriptorProto {
            name: "Any".into(),
            full_name: "google.protobuf.Any".into(),
            fields: vec![
                FieldDescriptorProto {
                    name: "type_url".into(),
                    number: 1,
                    label: FieldLabel::Optional,
                    field_type: FieldType::String,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "typeUrl".into(),
                },
                FieldDescriptorProto {
                    name: "value".into(),
                    number: 2,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Bytes,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "value".into(),
                },
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
        DescriptorProto {
            name: "Empty".into(),
            full_name: "google.protobuf.Empty".into(),
            fields: Vec::new(),
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
        DescriptorProto {
            name: "Value".into(),
            full_name: "google.protobuf.Value".into(),
            fields: vec![
                FieldDescriptorProto {
                    name: "number_value".into(),
                    number: 2,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Double,
                    type_name: String::new(),
                    oneof_index: Some(0),
                    json_name: "numberValue".into(),
                },
                FieldDescriptorProto {
                    name: "string_value".into(),
                    number: 3,
                    label: FieldLabel::Optional,
                    field_type: FieldType::String,
                    type_name: String::new(),
                    oneof_index: Some(0),
                    json_name: "stringValue".into(),
                },
                FieldDescriptorProto {
                    name: "bool_value".into(),
                    number: 4,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Bool,
                    type_name: String::new(),
                    oneof_index: Some(0),
                    json_name: "boolValue".into(),
                },
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: vec![OneofDescriptorProto {
                name: "kind".into(),
            }],
            is_map_entry: false,
        },
    ]
}

impl QuasarDescriptorRegistry {
    pub fn open_default() -> Result<Arc<Self>, QuasarError> {
        if let Some(reg) = DEFAULT_QUASAR_REGISTRY.get() {
            return Ok(Arc::clone(reg));
        }
        let root = workspace_root();
        let sqlite_path = std::env::var("EVOMU_QUASAR_DESCRIPTORS_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| root.join("data").join("quasar_descriptors.sqlite"));
        let manifest_path = root.join("data").join("quasar_descriptor_manifest.json");

        let reg = Arc::new(Self::open(&sqlite_path, &manifest_path)?);
        let _ = DEFAULT_QUASAR_REGISTRY.set(Arc::clone(&reg));
        Ok(reg)
    }

    /// Opens the default `QuasarDescriptorRegistry` if `data/quasar_descriptors.sqlite` is present,
    /// or returns an empty registry containing the built-in Google Well-Known Types so server startup
    /// never panics when the local gitignored descriptor DB has not been extracted yet.
    pub fn open_default_or_empty() -> Arc<Self> {
        Self::open_default().unwrap_or_else(|err| {
            eprintln!("[eve-proto] QuasarDescriptorRegistry::open_default_or_empty fallback: {err}");
            Arc::new(Self::empty_with_builtins())
        })
    }

    pub fn empty_with_builtins() -> Self {
        let mut messages_by_full_name = HashMap::new();
        for wkt in builtin_google_descriptors() {
            messages_by_full_name.insert(wkt.full_name.clone(), wkt);
        }
        Self {
            manifest: QuasarManifest {
                schema_version: 1,
                descriptor_file_count: 0,
                domain_count: 0,
                unique_package_count: 0,
                top_level_message_count: 0,
                total_message_count: messages_by_full_name.len(),
                total_enum_count: 0,
                total_service_count: 0,
                total_field_count: 0,
                total_serialized_pb_bytes: 0,
                combined_serialized_pb_sha256: String::new(),
                combined_serialized_pb_md5: String::new(),
                per_domain_file_counts: BTreeMap::new(),
            },
            files: Vec::new(),
            files_by_pyj_path: HashMap::new(),
            files_by_proto_name: HashMap::new(),
            files_by_domain: BTreeMap::new(),
            messages_by_full_name,
            enums_by_full_name: HashMap::new(),
            services_by_full_name: HashMap::new(),
        }
    }

    pub fn open(sqlite_path: &Path, manifest_path: &Path) -> Result<Self, QuasarError> {
        if !sqlite_path.exists() {
            return Err(QuasarError::MissingDescriptorDatabase(
                sqlite_path.display().to_string(),
            ));
        }
        let manifest_str = std::fs::read_to_string(manifest_path).map_err(|e| {
            QuasarError::ManifestMismatch(format!(
                "Failed reading manifest '{}': {e}",
                manifest_path.display()
            ))
        })?;
        let manifest: QuasarManifest = serde_json::from_str(&manifest_str).map_err(|e| {
            QuasarError::ManifestMismatch(format!(
                "Invalid JSON in '{}': {e}",
                manifest_path.display()
            ))
        })?;

        let conn = Connection::open_with_flags(sqlite_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| QuasarError::DatabaseError(e.to_string()))?;

        let mut stmt = conn
            .prepare(
                "SELECT pyj_path, domain, sha256, md5, serialized_pb
                 FROM quasar_file_descriptors
                 ORDER BY domain ASC, pyj_path ASC",
            )
            .map_err(|e| QuasarError::DatabaseError(e.to_string()))?;

        let mut rows = stmt
            .query([])
            .map_err(|e| QuasarError::DatabaseError(e.to_string()))?;

        let mut files = Vec::with_capacity(EXPECTED_QUASAR_FILE_COUNT);
        let mut combined_bytes = Vec::with_capacity(manifest.total_serialized_pb_bytes + 65_536);
        let mut per_domain_counts: BTreeMap<String, usize> = BTreeMap::new();

        while let Some(row) = rows
            .next()
            .map_err(|e| QuasarError::DatabaseError(e.to_string()))?
        {
            let pyj_path: String = row
                .get(0)
                .map_err(|e| QuasarError::DatabaseError(e.to_string()))?;
            let domain: String = row
                .get(1)
                .map_err(|e| QuasarError::DatabaseError(e.to_string()))?;
            let recorded_sha256: String = row
                .get(2)
                .map_err(|e| QuasarError::DatabaseError(e.to_string()))?;
            let recorded_md5: String = row
                .get(3)
                .map_err(|e| QuasarError::DatabaseError(e.to_string()))?;
            let raw_bytes: Vec<u8> = row
                .get(4)
                .map_err(|e| QuasarError::DatabaseError(e.to_string()))?;

            let path_bytes = pyj_path.as_bytes();
            combined_bytes.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
            combined_bytes.extend_from_slice(path_bytes);
            combined_bytes.extend_from_slice(&(raw_bytes.len() as u32).to_le_bytes());
            combined_bytes.extend_from_slice(&raw_bytes);

            let fd = FileDescriptorProto::decode_wire(&pyj_path, &domain, raw_bytes)?;
            if fd.sha256 != recorded_sha256 || fd.md5 != recorded_md5 {
                return Err(QuasarError::ManifestMismatch(format!(
                    "Descriptor checksum mismatch for {pyj_path}"
                )));
            }
            *per_domain_counts.entry(domain).or_insert(0) += 1;
            files.push(fd);
        }

        if files.len() != EXPECTED_QUASAR_FILE_COUNT
            || files.len() != manifest.descriptor_file_count
        {
            return Err(QuasarError::ManifestMismatch(format!(
                "Expected {EXPECTED_QUASAR_FILE_COUNT} descriptors, loaded {}",
                files.len()
            )));
        }
        if per_domain_counts.len() != EXPECTED_QUASAR_DOMAIN_COUNT
            || per_domain_counts != manifest.per_domain_file_counts
        {
            return Err(QuasarError::ManifestMismatch(format!(
                "Domain counts mismatch: expected {} domains, got {}",
                EXPECTED_QUASAR_DOMAIN_COUNT,
                per_domain_counts.len()
            )));
        }

        let actual_sha256 = sha256_hex(&combined_bytes);
        let actual_md5 = md5_hex(&combined_bytes);
        if actual_sha256 != manifest.combined_serialized_pb_sha256 {
            return Err(QuasarError::ManifestMismatch(format!(
                "Combined SHA-256 mismatch: expected {}, got {actual_sha256}",
                manifest.combined_serialized_pb_sha256
            )));
        }
        if actual_md5 != manifest.combined_serialized_pb_md5 {
            return Err(QuasarError::ManifestMismatch(format!(
                "Combined MD5 mismatch: expected {}, got {actual_md5}",
                manifest.combined_serialized_pb_md5
            )));
        }

        let mut files_by_pyj_path = HashMap::with_capacity(files.len());
        let mut files_by_proto_name = HashMap::with_capacity(files.len());
        let mut files_by_domain: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut messages_by_full_name = HashMap::new();
        let mut enums_by_full_name = HashMap::new();
        let mut services_by_full_name = HashMap::new();

        for gm in builtin_google_descriptors() {
            messages_by_full_name.insert(gm.full_name.clone(), gm);
        }

        fn register_msg_tree(
            m: &DescriptorProto,
            msgs: &mut HashMap<String, DescriptorProto>,
            enums: &mut HashMap<String, EnumDescriptorProto>,
        ) {
            msgs.insert(m.full_name.clone(), m.clone());
            for e in &m.enum_types {
                enums.insert(e.full_name.clone(), e.clone());
            }
            for sub in &m.nested_types {
                register_msg_tree(sub, msgs, enums);
            }
        }

        for (idx, f) in files.iter().enumerate() {
            files_by_pyj_path.insert(f.pyj_path.clone(), idx);
            files_by_proto_name.insert(f.name.clone(), idx);
            files_by_domain.entry(f.domain.clone()).or_default().push(idx);
            for m in &f.messages {
                register_msg_tree(m, &mut messages_by_full_name, &mut enums_by_full_name);
            }
            for e in &f.enums {
                enums_by_full_name.insert(e.full_name.clone(), e.clone());
            }
            for s in &f.services {
                services_by_full_name.insert(s.full_name.clone(), s.clone());
            }
        }

        Ok(Self {
            manifest,
            files,
            files_by_pyj_path,
            files_by_proto_name,
            files_by_domain,
            messages_by_full_name,
            enums_by_full_name,
            services_by_full_name,
        })
    }

    pub fn normalize_type_name(full_name_or_type_url: &str) -> &str {
        full_name_or_type_url
            .strip_prefix(TYPE_GOOGLEAPIS_PREFIX)
            .unwrap_or(full_name_or_type_url)
            .trim_start_matches('.')
    }

    pub fn get_message(&self, full_name_or_type_url: &str) -> Option<&DescriptorProto> {
        let key = Self::normalize_type_name(full_name_or_type_url);
        self.messages_by_full_name.get(key)
    }

    pub fn get_file_by_pyj(&self, pyj_path: &str) -> Option<&FileDescriptorProto> {
        self.files_by_pyj_path
            .get(pyj_path)
            .and_then(|&idx| self.files.get(idx))
    }

    /// Deterministically constructs a populated sample `DynamicQuasarMessage` for any message in the registry.
    pub fn instantiate_populated_sample(
        &self,
        full_name_or_type_url: &str,
        seed: u64,
    ) -> Result<DynamicQuasarMessage, QuasarError> {
        let mut visited = HashSet::new();
        self.instantiate_sample_inner(full_name_or_type_url, seed, 0, &mut visited)
    }

    fn instantiate_sample_inner(
        &self,
        full_name_or_type_url: &str,
        seed: u64,
        depth: usize,
        visited: &mut HashSet<String>,
    ) -> Result<DynamicQuasarMessage, QuasarError> {
        let desc = self
            .get_message(full_name_or_type_url)
            .ok_or_else(|| QuasarError::UnknownMessageType(full_name_or_type_url.to_string()))?;

        let mut msg = DynamicQuasarMessage::new(desc.full_name.clone());
        if depth > 6 {
            return Ok(msg);
        }
        visited.insert(desc.full_name.clone());

        let mut chosen_oneofs = HashSet::new();
        for f in &desc.fields {
            if let Some(oneof_idx) = f.oneof_index {
                if !chosen_oneofs.insert(oneof_idx) {
                    continue;
                }
            }
            let fseed = seed
                .wrapping_add((f.number as u64).wrapping_mul(31))
                .wrapping_add(1);
            let single_val = match f.field_type {
                FieldType::Double => Some(DynamicValue::Double((fseed as f64) * 1.25 + 0.5)),
                FieldType::Float => Some(DynamicValue::Float((fseed as f32) * 1.5 + 0.25)),
                FieldType::Int64 => Some(DynamicValue::Int64(-(fseed as i64) - 10)),
                FieldType::Uint64 => Some(DynamicValue::Uint64(fseed + 100)),
                FieldType::Int32 => Some(DynamicValue::Int32(-((fseed % 10_000) as i32) - 1)),
                FieldType::Fixed64 => Some(DynamicValue::Fixed64(fseed.wrapping_mul(0x1000_0001))),
                FieldType::Fixed32 => {
                    Some(DynamicValue::Fixed32(((fseed % 100_000) as u32) + 0x0102_0304))
                }
                FieldType::Bool => Some(DynamicValue::Bool(true)),
                FieldType::String => Some(DynamicValue::String(format!(
                    "{}_{}",
                    f.name,
                    (fseed % 997) + 1
                ))),
                FieldType::Bytes => Some(DynamicValue::Bytes(vec![
                    (fseed & 0xFF) as u8,
                    ((fseed >> 8) & 0xFF) as u8,
                    0xAA,
                    0x55,
                ])),
                FieldType::Uint32 => Some(DynamicValue::Uint32(((fseed % 50_000) as u32) + 1)),
                FieldType::Enum => {
                    let ev = self
                        .enums_by_full_name
                        .get(&f.type_name)
                        .and_then(|en| {
                            en.values
                                .iter()
                                .find(|v| v.number != 0)
                                .or_else(|| en.values.first())
                        })
                        .map(|v| v.number)
                        .unwrap_or(1);
                    Some(DynamicValue::Enum(ev))
                }
                FieldType::Sfixed32 => Some(DynamicValue::Sfixed32(-((fseed % 5_000) as i32) - 7)),
                FieldType::Sfixed64 => Some(DynamicValue::Sfixed64(-(fseed as i64) - 77)),
                FieldType::Sint32 => Some(DynamicValue::Sint32(-((fseed % 20_000) as i32) - 3)),
                FieldType::Sint64 => Some(DynamicValue::Sint64(-(fseed as i64) - 33)),
                FieldType::Message | FieldType::Group => {
                    let target = Self::normalize_type_name(&f.type_name);
                    if visited.contains(target) {
                        None
                    } else {
                        let sub = self.instantiate_sample_inner(
                            target,
                            fseed,
                            depth + 1,
                            visited,
                        )?;
                        Some(DynamicValue::Message(Box::new(sub)))
                    }
                }
            };

            if let Some(val) = single_val {
                if f.label == FieldLabel::Repeated {
                    if f.field_type.is_packable_scalar() {
                        msg.set(f.number, DynamicValue::Repeated(vec![val.clone(), val]));
                    } else {
                        msg.set(f.number, DynamicValue::Repeated(vec![val]));
                    }
                } else {
                    msg.set(f.number, val);
                }
            }
        }

        visited.remove(&desc.full_name);
        Ok(msg)
    }

    pub fn encode_dynamic(&self, msg: &DynamicQuasarMessage) -> Result<Vec<u8>, QuasarError> {
        let desc = self
            .get_message(&msg.full_name)
            .ok_or_else(|| QuasarError::UnknownMessageType(msg.full_name.clone()))?;
        let mut out = Vec::new();
        for (&field_num, val) in &msg.fields {
            let fdesc = desc.fields.iter().find(|f| f.number == field_num);
            let ftype = fdesc.map(|f| f.field_type);
            self.encode_dynamic_field(field_num, ftype, val, &mut out)?;
        }
        Ok(out)
    }

    fn encode_dynamic_field(
        &self,
        field_num: u32,
        ftype: Option<FieldType>,
        val: &DynamicValue,
        out: &mut Vec<u8>,
    ) -> Result<(), QuasarError> {
        match val {
            DynamicValue::Double(v) => encode_fixed64_field(field_num, v.to_bits(), out),
            DynamicValue::Float(v) => encode_fixed32_field(field_num, v.to_bits(), out),
            DynamicValue::Int64(v) => encode_varint_field(field_num, *v as u64, out),
            DynamicValue::Uint64(v) => encode_varint_field(field_num, *v, out),
            DynamicValue::Int32(v) => {
                if ftype == Some(FieldType::Sint32) {
                    encode_varint_field(field_num, encode_zigzag32(*v) as u64, out);
                } else {
                    encode_varint_field(field_num, *v as i64 as u64, out);
                }
            }
            DynamicValue::Uint32(v) => encode_varint_field(field_num, *v as u64, out),
            DynamicValue::Sint32(v) => {
                encode_varint_field(field_num, encode_zigzag32(*v) as u64, out)
            }
            DynamicValue::Sint64(v) => encode_varint_field(field_num, encode_zigzag64(*v), out),
            DynamicValue::Fixed32(v) => encode_fixed32_field(field_num, *v, out),
            DynamicValue::Fixed64(v) => encode_fixed64_field(field_num, *v, out),
            DynamicValue::Sfixed32(v) => encode_fixed32_field(field_num, *v as u32, out),
            DynamicValue::Sfixed64(v) => encode_fixed64_field(field_num, *v as u64, out),
            DynamicValue::Bool(v) => encode_varint_field(field_num, u64::from(*v), out),
            DynamicValue::String(s) => encode_string_field(field_num, s, out),
            DynamicValue::Bytes(b) => encode_bytes_field(field_num, b, out),
            DynamicValue::Enum(v) => encode_varint_field(field_num, *v as i64 as u64, out),
            DynamicValue::Message(sub) => {
                let sub_bytes = self.encode_dynamic(sub)?;
                encode_bytes_field(field_num, &sub_bytes, out);
            }
            DynamicValue::Repeated(items) => {
                if !items.is_empty() && ftype.is_some_and(|ft| ft.is_packable_scalar()) {
                    let mut packed = Vec::with_capacity(items.len() * 8);
                    for item in items {
                        match item {
                            DynamicValue::Double(v) => {
                                packed.extend_from_slice(&v.to_bits().to_le_bytes())
                            }
                            DynamicValue::Float(v) => {
                                packed.extend_from_slice(&v.to_bits().to_le_bytes())
                            }
                            DynamicValue::Int64(v) => encode_varint(*v as u64, &mut packed),
                            DynamicValue::Uint64(v) => encode_varint(*v, &mut packed),
                            DynamicValue::Int32(v) => {
                                if ftype == Some(FieldType::Sint32) {
                                    encode_varint(encode_zigzag32(*v) as u64, &mut packed);
                                } else {
                                    encode_varint(*v as i64 as u64, &mut packed);
                                }
                            }
                            DynamicValue::Uint32(v) => encode_varint(*v as u64, &mut packed),
                            DynamicValue::Sint32(v) => {
                                encode_varint(encode_zigzag32(*v) as u64, &mut packed)
                            }
                            DynamicValue::Sint64(v) => {
                                encode_varint(encode_zigzag64(*v), &mut packed)
                            }
                            DynamicValue::Fixed32(v) => {
                                packed.extend_from_slice(&v.to_le_bytes())
                            }
                            DynamicValue::Fixed64(v) => {
                                packed.extend_from_slice(&v.to_le_bytes())
                            }
                            DynamicValue::Sfixed32(v) => {
                                packed.extend_from_slice(&(*v as u32).to_le_bytes())
                            }
                            DynamicValue::Sfixed64(v) => {
                                packed.extend_from_slice(&(*v as u64).to_le_bytes())
                            }
                            DynamicValue::Bool(v) => encode_varint(u64::from(*v), &mut packed),
                            DynamicValue::Enum(v) => {
                                encode_varint(*v as i64 as u64, &mut packed)
                            }
                            _ => {}
                        }
                    }
                    encode_bytes_field(field_num, &packed, out);
                } else {
                    for item in items {
                        self.encode_dynamic_field(field_num, ftype, item, out)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn decode_dynamic(
        &self,
        full_name_or_type_url: &str,
        buf: &[u8],
    ) -> Result<DynamicQuasarMessage, QuasarError> {
        self.decode_dynamic_inner(full_name_or_type_url, buf, 0)
    }

    fn decode_dynamic_inner(
        &self,
        full_name_or_type_url: &str,
        buf: &[u8],
        depth: usize,
    ) -> Result<DynamicQuasarMessage, QuasarError> {
        if depth > MAX_RECURSION_DEPTH {
            return Err(QuasarError::RecursionLimitExceeded);
        }
        let desc = self
            .get_message(full_name_or_type_url)
            .ok_or_else(|| QuasarError::UnknownMessageType(full_name_or_type_url.to_string()))?;

        let wm = WireMessage::decode(buf)?;
        let mut msg = DynamicQuasarMessage::new(desc.full_name.clone());

        for wf in wm.fields {
            let Some(fdesc) = desc.fields.iter().find(|f| f.number == wf.field_number) else {
                continue;
            };

            if fdesc.label == FieldLabel::Repeated
                && fdesc.field_type.is_packable_scalar()
                && wf.value.wire_type() == WireType::LengthDelimited
            {
                let packed_bytes = wf.value.as_bytes().unwrap_or_default();
                let mut ppos = 0usize;
                let mut unpacked = match msg.fields.remove(&fdesc.number) {
                    Some(DynamicValue::Repeated(v)) => v,
                    _ => Vec::new(),
                };
                while ppos < packed_bytes.len() {
                    let item = match fdesc.field_type {
                        FieldType::Double => {
                            if ppos + 8 > packed_bytes.len() {
                                return Err(QuasarError::UnexpectedEof);
                            }
                            let v = f64::from_le_bytes(
                                packed_bytes[ppos..ppos + 8].try_into().unwrap(),
                            );
                            ppos += 8;
                            DynamicValue::Double(v)
                        }
                        FieldType::Float => {
                            if ppos + 4 > packed_bytes.len() {
                                return Err(QuasarError::UnexpectedEof);
                            }
                            let v = f32::from_le_bytes(
                                packed_bytes[ppos..ppos + 4].try_into().unwrap(),
                            );
                            ppos += 4;
                            DynamicValue::Float(v)
                        }
                        FieldType::Fixed64 => {
                            if ppos + 8 > packed_bytes.len() {
                                return Err(QuasarError::UnexpectedEof);
                            }
                            let v = u64::from_le_bytes(
                                packed_bytes[ppos..ppos + 8].try_into().unwrap(),
                            );
                            ppos += 8;
                            DynamicValue::Fixed64(v)
                        }
                        FieldType::Sfixed64 => {
                            if ppos + 8 > packed_bytes.len() {
                                return Err(QuasarError::UnexpectedEof);
                            }
                            let v = i64::from_le_bytes(
                                packed_bytes[ppos..ppos + 8].try_into().unwrap(),
                            );
                            ppos += 8;
                            DynamicValue::Sfixed64(v)
                        }
                        FieldType::Fixed32 => {
                            if ppos + 4 > packed_bytes.len() {
                                return Err(QuasarError::UnexpectedEof);
                            }
                            let v = u32::from_le_bytes(
                                packed_bytes[ppos..ppos + 4].try_into().unwrap(),
                            );
                            ppos += 4;
                            DynamicValue::Fixed32(v)
                        }
                        FieldType::Sfixed32 => {
                            if ppos + 4 > packed_bytes.len() {
                                return Err(QuasarError::UnexpectedEof);
                            }
                            let v = i32::from_le_bytes(
                                packed_bytes[ppos..ppos + 4].try_into().unwrap(),
                            );
                            ppos += 4;
                            DynamicValue::Sfixed32(v)
                        }
                        FieldType::Int64 => {
                            DynamicValue::Int64(decode_varint(packed_bytes, &mut ppos)? as i64)
                        }
                        FieldType::Uint64 => {
                            DynamicValue::Uint64(decode_varint(packed_bytes, &mut ppos)?)
                        }
                        FieldType::Int32 => {
                            DynamicValue::Int32(decode_varint(packed_bytes, &mut ppos)? as i32)
                        }
                        FieldType::Uint32 => {
                            DynamicValue::Uint32(decode_varint(packed_bytes, &mut ppos)? as u32)
                        }
                        FieldType::Bool => {
                            DynamicValue::Bool(decode_varint(packed_bytes, &mut ppos)? != 0)
                        }
                        FieldType::Enum => {
                            DynamicValue::Enum(decode_varint(packed_bytes, &mut ppos)? as i32)
                        }
                        FieldType::Sint32 => DynamicValue::Sint32(decode_zigzag32(
                            decode_varint(packed_bytes, &mut ppos)? as u32,
                        )),
                        FieldType::Sint64 => DynamicValue::Sint64(decode_zigzag64(decode_varint(
                            packed_bytes,
                            &mut ppos,
                        )?)),
                        _ => unreachable!(),
                    };
                    unpacked.push(item);
                }
                msg.fields.insert(fdesc.number, DynamicValue::Repeated(unpacked));
                continue;
            }

            let expected_wt = fdesc.field_type.expected_wire_type();
            let actual_wt = wf.value.wire_type();
            if expected_wt != actual_wt {
                return Err(QuasarError::WireTypeMismatch {
                    field: fdesc.name.clone(),
                    number: fdesc.number,
                    expected: expected_wt,
                    actual: actual_wt,
                });
            }

            let decoded_val = match fdesc.field_type {
                FieldType::Double => {
                    DynamicValue::Double(f64::from_bits(wf.value.as_fixed64().unwrap_or(0)))
                }
                FieldType::Float => {
                    DynamicValue::Float(f32::from_bits(wf.value.as_fixed32().unwrap_or(0)))
                }
                FieldType::Int64 => DynamicValue::Int64(wf.value.as_varint().unwrap_or(0) as i64),
                FieldType::Uint64 => DynamicValue::Uint64(wf.value.as_varint().unwrap_or(0)),
                FieldType::Int32 => DynamicValue::Int32(wf.value.as_varint().unwrap_or(0) as i32),
                FieldType::Fixed64 => DynamicValue::Fixed64(wf.value.as_fixed64().unwrap_or(0)),
                FieldType::Fixed32 => DynamicValue::Fixed32(wf.value.as_fixed32().unwrap_or(0)),
                FieldType::Bool => DynamicValue::Bool(wf.value.as_varint().unwrap_or(0) != 0),
                FieldType::String => {
                    let s = wf.value.as_str()?.unwrap_or_default().to_string();
                    DynamicValue::String(s)
                }
                FieldType::Bytes => {
                    DynamicValue::Bytes(wf.value.as_bytes().unwrap_or_default().to_vec())
                }
                FieldType::Uint32 => DynamicValue::Uint32(wf.value.as_varint().unwrap_or(0) as u32),
                FieldType::Enum => DynamicValue::Enum(wf.value.as_varint().unwrap_or(0) as i32),
                FieldType::Sfixed32 => {
                    DynamicValue::Sfixed32(wf.value.as_fixed32().unwrap_or(0) as i32)
                }
                FieldType::Sfixed64 => {
                    DynamicValue::Sfixed64(wf.value.as_fixed64().unwrap_or(0) as i64)
                }
                FieldType::Sint32 => DynamicValue::Sint32(decode_zigzag32(
                    wf.value.as_varint().unwrap_or(0) as u32,
                )),
                FieldType::Sint64 => {
                    DynamicValue::Sint64(decode_zigzag64(wf.value.as_varint().unwrap_or(0)))
                }
                FieldType::Message | FieldType::Group => {
                    let sub_bytes = wf.value.as_bytes().unwrap_or_default();
                    let sub_msg =
                        self.decode_dynamic_inner(&fdesc.type_name, sub_bytes, depth + 1)?;
                    DynamicValue::Message(Box::new(sub_msg))
                }
            };

            if fdesc.label == FieldLabel::Repeated {
                match msg.fields.get_mut(&fdesc.number) {
                    Some(DynamicValue::Repeated(list)) => list.push(decoded_val),
                    _ => {
                        msg.fields
                            .insert(fdesc.number, DynamicValue::Repeated(vec![decoded_val]));
                    }
                }
            } else {
                if let Some(oneof_idx) = fdesc.oneof_index {
                    let sibling_nums: Vec<u32> = desc
                        .fields
                        .iter()
                        .filter(|f| f.oneof_index == Some(oneof_idx) && f.number != fdesc.number)
                        .map(|f| f.number)
                        .collect();
                    for snum in sibling_nums {
                        msg.fields.remove(&snum);
                    }
                }
                msg.fields.insert(fdesc.number, decoded_val);
            }
        }

        Ok(msg)
    }

    /// Generates idiomatic Rust struct/enum source code (`prost`-style) from any `FileDescriptorProto`.
    pub fn generate_rust_module_for_file(file: &FileDescriptorProto) -> String {
        fn rust_field_type(f: &FieldDescriptorProto) -> String {
            let base = match f.field_type {
                FieldType::Double => "f64".to_string(),
                FieldType::Float => "f32".to_string(),
                FieldType::Int64 | FieldType::Sint64 | FieldType::Sfixed64 => "i64".to_string(),
                FieldType::Uint64 | FieldType::Fixed64 => "u64".to_string(),
                FieldType::Int32 | FieldType::Sint32 | FieldType::Sfixed32 | FieldType::Enum => {
                    "i32".to_string()
                }
                FieldType::Uint32 | FieldType::Fixed32 => "u32".to_string(),
                FieldType::Bool => "bool".to_string(),
                FieldType::String => "String".to_string(),
                FieldType::Bytes => "Vec<u8>".to_string(),
                FieldType::Message | FieldType::Group => {
                    let short = f.type_name.rsplit('.').next().unwrap_or("DynamicQuasarMessage");
                    format!("Box<{short}>")
                }
            };
            match f.label {
                FieldLabel::Repeated => format!("Vec<{base}>"),
                _ if matches!(f.field_type, FieldType::Message | FieldType::Group) => {
                    format!("Option<{base}>")
                }
                _ => base,
            }
        }

        fn emit_msg(m: &DescriptorProto, out: &mut String) {
            use std::fmt::Write;
            let _ = writeln!(out, "// full_name: {}", m.full_name);
            let _ = writeln!(
                out,
                "#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]"
            );
            let _ = writeln!(out, "pub struct {} {{", m.name);
            for f in &m.fields {
                let fname = match f.name.as_str() {
                    "type" | "match" | "loop" | "in" | "ref" | "mod" | "self" | "super"
                    | "crate" => format!("r#{}", f.name),
                    other => other.to_string(),
                };
                let _ = writeln!(
                    out,
                    "    pub {fname}: {}, // tag={}",
                    rust_field_type(f),
                    f.number
                );
            }
            let _ = writeln!(out, "}}");
            for e in &m.enum_types {
                let _ = writeln!(out, "#[repr(i32)]");
                let _ = writeln!(
                    out,
                    "#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]"
                );
                let _ = writeln!(out, "pub enum {}_{} {{", m.name, e.name);
                for v in &e.values {
                    let _ = writeln!(out, "    {} = {},", v.name, v.number);
                }
                let _ = writeln!(out, "}}");
            }
            for sub in &m.nested_types {
                emit_msg(sub, out);
            }
        }

        let mut out = String::new();
        use std::fmt::Write;
        let _ = writeln!(
            &mut out,
            "// Auto-generated from {} (package `{}`)",
            file.name, file.package
        );
        for e in &file.enums {
            let _ = writeln!(&mut out, "#[repr(i32)]");
            let _ = writeln!(
                &mut out,
                "#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]"
            );
            let _ = writeln!(&mut out, "pub enum {} {{", e.name);
            for v in &e.values {
                let _ = writeln!(&mut out, "    {} = {},", v.name, v.number);
            }
            let _ = writeln!(&mut out, "}}");
        }
        for m in &file.messages {
            emit_msg(m, &mut out);
        }
        out
    }
}
