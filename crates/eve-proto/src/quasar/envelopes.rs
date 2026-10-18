//! `quasar::envelopes` — V24.01 Quasar Public & Internal Gateway Envelope Types (`Q-2`).

use super::*;
use serde::{Deserialize, Serialize};

// ============================================================================
// Gateway Envelopes (`eve_public/public.proto`, `eve/eve.proto`, `internal_gateway.proto`)
// ============================================================================

/// `.eve_public.ActiveIdentity` (`eve_public/public.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveIdentity {
    pub character: Option<EvePublicCharacterId>,
    pub corporation: Option<EvePublicCorporationId>,
    pub roles: Option<EvePublicCorpRoleCollection>,
}

impl QuasarMessage for ActiveIdentity {
    const FULL_NAME: &'static str = "eve_public.ActiveIdentity";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(c) = &self.character {
            encode_bytes_field(1, &c.encode_proto(), &mut out);
        }
        if let Some(corp) = &self.corporation {
            encode_bytes_field(2, &corp.encode_proto(), &mut out);
        }
        if let Some(roles) = &self.roles {
            encode_bytes_field(3, &roles.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.character = Some(EvePublicCharacterId::decode_proto(b)?);
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.corporation = Some(EvePublicCorporationId::decode_proto(b)?);
                    }
                }
                3 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.roles = Some(EvePublicCorpRoleCollection::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve_public.AuthoritativeContext` (`eve_public/public.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthoritativeContext {
    pub received: Option<GoogleTimestamp>,
    pub ip_address: Option<EveIpAddress>,
    pub tenant: String,
    pub no_authenticated_user: bool,
    pub authenticated_user: Option<EvePublicUserId>,
    pub no_active_character: bool,
    pub active_character: Option<EvePublicCharacterId>,
    pub gateway_instance_uuid: Vec<u8>,
    pub no_active_identity: bool,
    pub identity: Option<ActiveIdentity>,
}

impl AuthoritativeContext {
    pub fn for_session(
        user_id: Option<i32>,
        character_id: Option<u32>,
        corporation_id: Option<u32>,
        corp_roles_bitmask: u64,
        tenant: impl Into<String>,
        gateway_instance_uuid: Vec<u8>,
    ) -> Self {
        Self::for_session_at(
            user_id,
            character_id,
            corporation_id,
            corp_roles_bitmask,
            tenant,
            gateway_instance_uuid,
            0,
        )
    }

    pub fn for_session_at(
        user_id: Option<i32>,
        character_id: Option<u32>,
        corporation_id: Option<u32>,
        corp_roles_bitmask: u64,
        tenant: impl Into<String>,
        gateway_instance_uuid: Vec<u8>,
        now_unix: u64,
    ) -> Self {
        let (no_authenticated_user, authenticated_user) = match user_id {
            Some(uid) => (false, Some(EvePublicUserId { sequential: uid })),
            None => (true, None),
        };
        let (no_active_character, active_character, no_active_identity, identity) =
            match character_id {
                Some(cid) => (
                    false,
                    Some(EvePublicCharacterId { sequential: cid }),
                    false,
                    Some(ActiveIdentity {
                        character: Some(EvePublicCharacterId { sequential: cid }),
                        corporation: corporation_id
                            .map(|corp| EvePublicCorporationId { sequential: corp }),
                        roles: Some(EvePublicCorpRoleCollection {
                            bitmask: corp_roles_bitmask,
                        }),
                    }),
                ),
                None => (true, None, true, None),
            };
        Self {
            received: Some(GoogleTimestamp {
                seconds: now_unix as i64,
                nanos: 0,
            }),
            ip_address: Some(EveIpAddress::V4(0x7F00_0001)),
            tenant: tenant.into(),
            no_authenticated_user,
            authenticated_user,
            no_active_character,
            active_character,
            gateway_instance_uuid,
            no_active_identity,
            identity,
        }
    }

    pub fn effective_user_id(&self) -> Option<i32> {
        if self.no_authenticated_user {
            None
        } else {
            self.authenticated_user.map(|u| u.sequential)
        }
    }

    pub fn effective_character_id(&self) -> Option<u32> {
        if self.no_active_character && self.no_active_identity {
            return None;
        }
        self.active_character
            .map(|c| c.sequential)
            .or_else(|| {
                self.identity
                    .as_ref()
                    .and_then(|id| id.character.map(|c| c.sequential))
            })
    }

    pub fn effective_corporation_id(&self) -> Option<u32> {
        self.identity
            .as_ref()
            .and_then(|id| id.corporation.map(|c| c.sequential))
    }
}

impl QuasarMessage for AuthoritativeContext {
    const FULL_NAME: &'static str = "eve_public.AuthoritativeContext";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(ts) = &self.received {
            encode_bytes_field(1, &ts.encode_proto(), &mut out);
        }
        if let Some(ip) = &self.ip_address {
            encode_bytes_field(2, &ip.encode_proto(), &mut out);
        }
        if !self.tenant.is_empty() {
            encode_string_field(3, &self.tenant, &mut out);
        }
        if let Some(user) = &self.authenticated_user {
            encode_bytes_field(5, &user.encode_proto(), &mut out);
        } else if self.no_authenticated_user {
            encode_varint_field(4, 1, &mut out);
        }
        if let Some(ch) = &self.active_character {
            encode_bytes_field(7, &ch.encode_proto(), &mut out);
        } else if self.no_active_character {
            encode_varint_field(6, 1, &mut out);
        }
        if !self.gateway_instance_uuid.is_empty() {
            encode_bytes_field(8, &self.gateway_instance_uuid, &mut out);
        }
        if let Some(id) = &self.identity {
            encode_bytes_field(10, &id.encode_proto(), &mut out);
        } else if self.no_active_identity {
            encode_varint_field(9, 1, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.received = Some(GoogleTimestamp::decode_proto(b)?);
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.ip_address = Some(EveIpAddress::decode_proto(b)?);
                    }
                }
                3 => {
                    if let Some(s) = f.value.as_str()? {
                        res.tenant = s.to_string();
                    }
                }
                4 => {
                    res.no_authenticated_user = f.value.as_varint().unwrap_or(0) != 0;
                    if res.no_authenticated_user {
                        res.authenticated_user = None;
                    }
                }
                5 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.authenticated_user = Some(EvePublicUserId::decode_proto(b)?);
                        res.no_authenticated_user = false;
                    }
                }
                6 => {
                    res.no_active_character = f.value.as_varint().unwrap_or(0) != 0;
                    if res.no_active_character {
                        res.active_character = None;
                    }
                }
                7 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.active_character = Some(EvePublicCharacterId::decode_proto(b)?);
                        res.no_active_character = false;
                    }
                }
                8 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.gateway_instance_uuid = b.to_vec();
                    }
                }
                9 => {
                    res.no_active_identity = f.value.as_varint().unwrap_or(0) != 0;
                    if res.no_active_identity {
                        res.identity = None;
                    }
                }
                10 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.identity = Some(ActiveIdentity::decode_proto(b)?);
                        res.no_active_identity = false;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve_public.Request` (`eve_public/public.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvePublicRequest {
    pub issued: Option<GoogleTimestamp>,
    pub correlation_uuid: Vec<u8>,
    pub payload: Option<GoogleAny>,
    pub external_origin: String,
    pub application_instance_uuid: Vec<u8>,
    pub authoritative_context: Option<AuthoritativeContext>,
    pub trace_context: Option<TraceContext>,
}

impl QuasarMessage for EvePublicRequest {
    const FULL_NAME: &'static str = "eve_public.Request";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(issued) = &self.issued {
            encode_bytes_field(1, &issued.encode_proto(), &mut out);
        }
        if !self.correlation_uuid.is_empty() {
            encode_bytes_field(2, &self.correlation_uuid, &mut out);
        }
        if let Some(payload) = &self.payload {
            encode_bytes_field(3, &payload.encode_proto(), &mut out);
        }
        if !self.external_origin.is_empty() {
            encode_string_field(4, &self.external_origin, &mut out);
        }
        if !self.application_instance_uuid.is_empty() {
            encode_bytes_field(5, &self.application_instance_uuid, &mut out);
        }
        if let Some(ctx) = &self.authoritative_context {
            encode_bytes_field(6, &ctx.encode_proto(), &mut out);
        }
        if let Some(tc) = &self.trace_context {
            encode_bytes_field(7, &tc.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.issued = Some(GoogleTimestamp::decode_proto(b)?);
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.correlation_uuid = b.to_vec();
                    }
                }
                3 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.payload = Some(GoogleAny::decode_proto(b)?);
                    }
                }
                4 => {
                    if let Some(s) = f.value.as_str()? {
                        res.external_origin = s.to_string();
                    }
                }
                5 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.application_instance_uuid = b.to_vec();
                    }
                }
                6 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.authoritative_context = Some(AuthoritativeContext::decode_proto(b)?);
                    }
                }
                7 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.trace_context = Some(TraceContext::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve_public.Response` (`eve_public/public.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvePublicResponse {
    pub dispatched: Option<GoogleTimestamp>,
    pub correlation_uuid: Vec<u8>,
    pub status_code: u32,
    pub status_message: String,
    pub payload: Option<GoogleAny>,
    pub internal_origin: String,
    pub application_instance_uuid: Vec<u8>,
    pub gateway_instance_uuid: Vec<u8>,
    pub trace_context: Option<TraceContext>,
}

impl QuasarMessage for EvePublicResponse {
    const FULL_NAME: &'static str = "eve_public.Response";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(dispatched) = &self.dispatched {
            encode_bytes_field(1, &dispatched.encode_proto(), &mut out);
        }
        if !self.correlation_uuid.is_empty() {
            encode_bytes_field(2, &self.correlation_uuid, &mut out);
        }
        if self.status_code != 0 {
            encode_varint_field(3, self.status_code as u64, &mut out);
        }
        if !self.status_message.is_empty() {
            encode_string_field(4, &self.status_message, &mut out);
        }
        if let Some(payload) = &self.payload {
            encode_bytes_field(5, &payload.encode_proto(), &mut out);
        }
        if !self.internal_origin.is_empty() {
            encode_string_field(6, &self.internal_origin, &mut out);
        }
        if !self.application_instance_uuid.is_empty() {
            encode_bytes_field(7, &self.application_instance_uuid, &mut out);
        }
        if !self.gateway_instance_uuid.is_empty() {
            encode_bytes_field(8, &self.gateway_instance_uuid, &mut out);
        }
        if let Some(tc) = &self.trace_context {
            encode_bytes_field(9, &tc.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.dispatched = Some(GoogleTimestamp::decode_proto(b)?);
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.correlation_uuid = b.to_vec();
                    }
                }
                3 => res.status_code = f.value.as_varint().unwrap_or(0) as u32,
                4 => {
                    if let Some(s) = f.value.as_str()? {
                        res.status_message = s.to_string();
                    }
                }
                5 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.payload = Some(GoogleAny::decode_proto(b)?);
                    }
                }
                6 => {
                    if let Some(s) = f.value.as_str()? {
                        res.internal_origin = s.to_string();
                    }
                }
                7 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.application_instance_uuid = b.to_vec();
                    }
                }
                8 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.gateway_instance_uuid = b.to_vec();
                    }
                }
                9 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.trace_context = Some(TraceContext::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve_public.Notice.TargetGroup` (`eve_public/public.proto`).
///
/// Field numbers and wire types from V24 `public.proto`:
/// 1: `application_instance_uuid` (`bytes`)
/// 2: `solar_system` (`uint32`)
/// 3: `user` (`sint32` ZigZag)
/// 4: `character` (`uint32`)
/// 5: `corporation` (`uint32`)
/// 6: `alliance` (`int32`)
/// 7: `bubble_instance_uuid` (`bytes`)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NoticeTargetGroup {
    ApplicationInstanceUuid(Vec<u8>),
    SolarSystem(u32),
    User(i32),
    Character(u32),
    Corporation(u32),
    Alliance(i32),
    BubbleInstanceUuid(Vec<u8>),
}

impl NoticeTargetGroup {
    pub fn target_group_name(&self) -> &'static str {
        match self {
            Self::ApplicationInstanceUuid(_) => "application_instance_uuid",
            Self::SolarSystem(_) => "solar_system",
            Self::User(_) => "user",
            Self::Character(_) => "character",
            Self::Corporation(_) => "corporation",
            Self::Alliance(_) => "alliance",
            Self::BubbleInstanceUuid(_) => "bubble_instance_uuid",
        }
    }
}

impl QuasarMessage for NoticeTargetGroup {
    const FULL_NAME: &'static str = "eve_public.Notice.TargetGroup";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::ApplicationInstanceUuid(b) => encode_bytes_field(1, b, &mut out),
            Self::SolarSystem(sys) => encode_varint_field(2, *sys as u64, &mut out),
            Self::User(u) => encode_varint_field(3, encode_zigzag32(*u) as u64, &mut out),
            Self::Character(c) => encode_varint_field(4, *c as u64, &mut out),
            Self::Corporation(corp) => encode_varint_field(5, *corp as u64, &mut out),
            Self::Alliance(all) => encode_varint_field(6, *all as i64 as u64, &mut out),
            Self::BubbleInstanceUuid(b) => encode_bytes_field(7, b, &mut out),
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::Character(0);
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res = Self::ApplicationInstanceUuid(b.to_vec());
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res = Self::SolarSystem(v as u32);
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res = Self::User(decode_zigzag32(v as u32));
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res = Self::Character(v as u32);
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res = Self::Corporation(v as u32);
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res = Self::Alliance(v as i32);
                    }
                }
                7 => {
                    if let Some(b) = f.value.as_bytes() {
                        res = Self::BubbleInstanceUuid(b.to_vec());
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve_public.Notice` (`eve_public/public.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvePublicNotice {
    pub dispatched: Option<GoogleTimestamp>,
    pub uuid: Vec<u8>,
    pub internal_origin: String,
    pub tenant: String,
    pub payload: Option<GoogleAny>,
    pub target_group: Option<NoticeTargetGroup>,
    pub trace_context: Option<TraceContext>,
}

impl QuasarMessage for EvePublicNotice {
    const FULL_NAME: &'static str = "eve_public.Notice";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(ts) = &self.dispatched {
            encode_bytes_field(1, &ts.encode_proto(), &mut out);
        }
        if !self.uuid.is_empty() {
            encode_bytes_field(2, &self.uuid, &mut out);
        }
        if !self.internal_origin.is_empty() {
            encode_string_field(3, &self.internal_origin, &mut out);
        }
        if !self.tenant.is_empty() {
            encode_string_field(4, &self.tenant, &mut out);
        }
        if let Some(payload) = &self.payload {
            encode_bytes_field(5, &payload.encode_proto(), &mut out);
        }
        if let Some(tg) = &self.target_group {
            encode_bytes_field(6, &tg.encode_proto(), &mut out);
        }
        if let Some(tc) = &self.trace_context {
            encode_bytes_field(7, &tc.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.dispatched = Some(GoogleTimestamp::decode_proto(b)?);
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.uuid = b.to_vec();
                    }
                }
                3 => {
                    if let Some(s) = f.value.as_str()? {
                        res.internal_origin = s.to_string();
                    }
                }
                4 => {
                    if let Some(s) = f.value.as_str()? {
                        res.tenant = s.to_string();
                    }
                }
                5 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.payload = Some(GoogleAny::decode_proto(b)?);
                    }
                }
                6 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.target_group = Some(NoticeTargetGroup::decode_proto(b)?);
                    }
                }
                7 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.trace_context = Some(TraceContext::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve_public.Event` (`eve_public/public.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvePublicEvent {
    pub occurred: Option<GoogleTimestamp>,
    pub uuid: Vec<u8>,
    pub payload: Option<GoogleAny>,
    pub journey: Vec<u8>,
    pub external_origin: String,
    pub authoritative_context: Option<AuthoritativeContext>,
    pub application_instance_uuid: Vec<u8>,
    pub trace_context: Option<TraceContext>,
}

impl QuasarMessage for EvePublicEvent {
    const FULL_NAME: &'static str = "eve_public.Event";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(ts) = &self.occurred {
            encode_bytes_field(1, &ts.encode_proto(), &mut out);
        }
        if !self.uuid.is_empty() {
            encode_bytes_field(2, &self.uuid, &mut out);
        }
        if let Some(payload) = &self.payload {
            encode_bytes_field(3, &payload.encode_proto(), &mut out);
        }
        if !self.journey.is_empty() {
            encode_bytes_field(4, &self.journey, &mut out);
        }
        if !self.external_origin.is_empty() {
            encode_string_field(5, &self.external_origin, &mut out);
        }
        if let Some(ctx) = &self.authoritative_context {
            encode_bytes_field(6, &ctx.encode_proto(), &mut out);
        }
        if !self.application_instance_uuid.is_empty() {
            encode_bytes_field(7, &self.application_instance_uuid, &mut out);
        }
        if let Some(tc) = &self.trace_context {
            encode_bytes_field(8, &tc.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.occurred = Some(GoogleTimestamp::decode_proto(b)?);
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.uuid = b.to_vec();
                    }
                }
                3 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.payload = Some(GoogleAny::decode_proto(b)?);
                    }
                }
                4 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.journey = b.to_vec();
                    }
                }
                5 => {
                    if let Some(s) = f.value.as_str()? {
                        res.external_origin = s.to_string();
                    }
                }
                6 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.authoritative_context = Some(AuthoritativeContext::decode_proto(b)?);
                    }
                }
                7 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.application_instance_uuid = b.to_vec();
                    }
                }
                8 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.trace_context = Some(TraceContext::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve.Request` (`eve/eve.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EveInternalRequest {
    pub correlation_uuid: String,
    pub payload: Option<GoogleAny>,
    pub language: i32,
    pub tenant: String,
    pub publisher: String,
    pub forwarded_for: String,
    pub trace_context: Option<TraceContext>,
    pub ignore_cache: bool,
    pub created: Option<GoogleTimestamp>,
}

impl QuasarMessage for EveInternalRequest {
    const FULL_NAME: &'static str = "eve.Request";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.correlation_uuid.is_empty() {
            encode_string_field(1, &self.correlation_uuid, &mut out);
        }
        if let Some(payload) = &self.payload {
            encode_bytes_field(2, &payload.encode_proto(), &mut out);
        }
        if self.language != 0 {
            encode_varint_field(3, self.language as i64 as u64, &mut out);
        }
        if !self.tenant.is_empty() {
            encode_string_field(4, &self.tenant, &mut out);
        }
        if !self.publisher.is_empty() {
            encode_string_field(5, &self.publisher, &mut out);
        }
        if !self.forwarded_for.is_empty() {
            encode_string_field(6, &self.forwarded_for, &mut out);
        }
        if let Some(tc) = &self.trace_context {
            encode_bytes_field(7, &tc.encode_proto(), &mut out);
        }
        if self.ignore_cache {
            encode_varint_field(8, 1, &mut out);
        }
        if let Some(ts) = &self.created {
            encode_bytes_field(9, &ts.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(s) = f.value.as_str()? {
                        res.correlation_uuid = s.to_string();
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.payload = Some(GoogleAny::decode_proto(b)?);
                    }
                }
                3 => res.language = f.value.as_varint().unwrap_or(0) as i32,
                4 => {
                    if let Some(s) = f.value.as_str()? {
                        res.tenant = s.to_string();
                    }
                }
                5 => {
                    if let Some(s) = f.value.as_str()? {
                        res.publisher = s.to_string();
                    }
                }
                6 => {
                    if let Some(s) = f.value.as_str()? {
                        res.forwarded_for = s.to_string();
                    }
                }
                7 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.trace_context = Some(TraceContext::decode_proto(b)?);
                    }
                }
                8 => res.ignore_cache = f.value.as_varint().unwrap_or(0) != 0,
                9 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.created = Some(GoogleTimestamp::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve.Response` (`eve/eve.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EveInternalResponse {
    pub correlation_uuid: String,
    pub status_code: u32,
    pub status_message: String,
    pub payload: Option<GoogleAny>,
    pub tenant: String,
    pub publisher: String,
    pub payload_not_cached: bool,
    pub payload_not_paged: bool,
    pub recipient: String,
    pub created: Option<GoogleTimestamp>,
    pub trace_context: Option<TraceContext>,
}

impl QuasarMessage for EveInternalResponse {
    const FULL_NAME: &'static str = "eve.Response";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.correlation_uuid.is_empty() {
            encode_string_field(1, &self.correlation_uuid, &mut out);
        }
        if self.status_code != 0 {
            encode_varint_field(2, self.status_code as u64, &mut out);
        }
        if !self.status_message.is_empty() {
            encode_string_field(3, &self.status_message, &mut out);
        }
        if let Some(payload) = &self.payload {
            encode_bytes_field(4, &payload.encode_proto(), &mut out);
        }
        if !self.tenant.is_empty() {
            encode_string_field(5, &self.tenant, &mut out);
        }
        if !self.publisher.is_empty() {
            encode_string_field(6, &self.publisher, &mut out);
        }
        if self.payload_not_cached {
            encode_varint_field(7, 1, &mut out);
        }
        if self.payload_not_paged {
            encode_varint_field(9, 1, &mut out);
        }
        if !self.recipient.is_empty() {
            encode_string_field(11, &self.recipient, &mut out);
        }
        if let Some(ts) = &self.created {
            encode_bytes_field(12, &ts.encode_proto(), &mut out);
        }
        if let Some(tc) = &self.trace_context {
            encode_bytes_field(13, &tc.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(s) = f.value.as_str()? {
                        res.correlation_uuid = s.to_string();
                    }
                }
                2 => res.status_code = f.value.as_varint().unwrap_or(0) as u32,
                3 => {
                    if let Some(s) = f.value.as_str()? {
                        res.status_message = s.to_string();
                    }
                }
                4 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.payload = Some(GoogleAny::decode_proto(b)?);
                    }
                }
                5 => {
                    if let Some(s) = f.value.as_str()? {
                        res.tenant = s.to_string();
                    }
                }
                6 => {
                    if let Some(s) = f.value.as_str()? {
                        res.publisher = s.to_string();
                    }
                }
                7 => res.payload_not_cached = f.value.as_varint().unwrap_or(0) != 0,
                9 => res.payload_not_paged = f.value.as_varint().unwrap_or(0) != 0,
                11 => {
                    if let Some(s) = f.value.as_str()? {
                        res.recipient = s.to_string();
                    }
                }
                12 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.created = Some(GoogleTimestamp::decode_proto(b)?);
                    }
                }
                13 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.trace_context = Some(TraceContext::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve_public.internal_gateway.Request` (`eve_public/internal_gateway.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalGatewayRequest {
    pub ack_id: u64,
    pub request: Option<EvePublicRequest>,
}

impl QuasarMessage for InternalGatewayRequest {
    const FULL_NAME: &'static str = "eve_public.internal_gateway.Request";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.ack_id != 0 {
            encode_varint_field(1, self.ack_id, &mut out);
        }
        if let Some(req) = &self.request {
            encode_bytes_field(2, &req.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => res.ack_id = f.value.as_varint().unwrap_or(0),
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.request = Some(EvePublicRequest::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `.eve_public.internal_gateway.Response` (`eve_public/internal_gateway.proto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InternalGatewayResponse {
    pub response: Option<EvePublicResponse>,
    pub ack_id: u64,
}

impl QuasarMessage for InternalGatewayResponse {
    const FULL_NAME: &'static str = "eve_public.internal_gateway.Response";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(rsp) = &self.response {
            encode_bytes_field(1, &rsp.encode_proto(), &mut out);
        }
        if self.ack_id != 0 {
            encode_varint_field(2, self.ack_id, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.response = Some(EvePublicResponse::decode_proto(b)?);
                    }
                }
                2 => res.ack_id = f.value.as_varint().unwrap_or(0),
                _ => {}
            }
        }
        Ok(res)
    }
}
