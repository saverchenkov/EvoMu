//! `quasar::services` — Strongly-Typed Quasar Backend Service Envelopes and Messages
//! (`Q-3` Identity & Entitlements, `Q-4` Character Progression, `Q-5` Fitting Simulation,
//! `Q-6` Market & Wallet).

use super::*;
use serde::{Deserialize, Serialize};

// ============================================================================
// Q-3: Identity & Entitlements Services (`eve_public.identity.v1.*`, `eve_public.account.v1.*`)
// ============================================================================

/// `eve_public.identity.v1.GetEntitlementsRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetEntitlementsRequest {
    pub user_id: Option<u64>,
}

impl QuasarMessage for GetEntitlementsRequest {
    const FULL_NAME: &'static str = "eve_public.identity.v1.GetEntitlementsRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(uid) = self.user_id {
            if uid != 0 {
                encode_varint_field(1, uid, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.user_id = Some(v);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.identity.v1.GetEntitlementsResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetEntitlementsResponse {
    pub user_id: u64,
    pub is_omega: bool,
    pub omega_expires_at_unix: u64,
    pub entitlements: Vec<String>,
}

impl QuasarMessage for GetEntitlementsResponse {
    const FULL_NAME: &'static str = "eve_public.identity.v1.GetEntitlementsResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.user_id != 0 {
            encode_varint_field(1, self.user_id, &mut out);
        }
        if self.is_omega {
            encode_varint_field(2, 1, &mut out);
        }
        if self.omega_expires_at_unix != 0 {
            encode_varint_field(3, self.omega_expires_at_unix, &mut out);
        }
        for ent in &self.entitlements {
            encode_string_field(4, ent, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.user_id = v;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_omega = v != 0;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.omega_expires_at_unix = v;
                    }
                }
                4 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.entitlements.push(s.to_string());
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.identity.v1.GetIdentityRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetIdentityRequest {
    pub user_id: Option<u64>,
}

impl QuasarMessage for GetIdentityRequest {
    const FULL_NAME: &'static str = "eve_public.identity.v1.GetIdentityRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(uid) = self.user_id {
            if uid != 0 {
                encode_varint_field(1, uid, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.user_id = Some(v);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.identity.v1.GetIdentityResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetIdentityResponse {
    pub user_id: u64,
    pub username: String,
    pub is_omega: bool,
    pub plex_balance: i64,
    pub created_at_unix: u64,
}

impl QuasarMessage for GetIdentityResponse {
    const FULL_NAME: &'static str = "eve_public.identity.v1.GetIdentityResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.user_id != 0 {
            encode_varint_field(1, self.user_id, &mut out);
        }
        if !self.username.is_empty() {
            encode_string_field(2, &self.username, &mut out);
        }
        if self.is_omega {
            encode_varint_field(3, 1, &mut out);
        }
        if self.plex_balance != 0 {
            encode_varint_field(4, self.plex_balance as u64, &mut out);
        }
        if self.created_at_unix != 0 {
            encode_varint_field(5, self.created_at_unix, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.user_id = v;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.username = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_omega = v != 0;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.plex_balance = v as i64;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.created_at_unix = v;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.account.v1.GetAccountRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAccountRequest {
    pub user_id: Option<u64>,
}

impl QuasarMessage for GetAccountRequest {
    const FULL_NAME: &'static str = "eve_public.account.v1.GetAccountRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(uid) = self.user_id {
            if uid != 0 {
                encode_varint_field(1, uid, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.user_id = Some(v);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.account.v1.GetAccountResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAccountResponse {
    pub user_id: u64,
    pub username: String,
    pub is_omega: bool,
    pub omega_expires_at_unix: u64,
    pub plex_balance: i64,
    pub entitlements: Vec<String>,
}

impl QuasarMessage for GetAccountResponse {
    const FULL_NAME: &'static str = "eve_public.account.v1.GetAccountResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.user_id != 0 {
            encode_varint_field(1, self.user_id, &mut out);
        }
        if !self.username.is_empty() {
            encode_string_field(2, &self.username, &mut out);
        }
        if self.is_omega {
            encode_varint_field(3, 1, &mut out);
        }
        if self.omega_expires_at_unix != 0 {
            encode_varint_field(4, self.omega_expires_at_unix, &mut out);
        }
        if self.plex_balance != 0 {
            encode_varint_field(5, self.plex_balance as u64, &mut out);
        }
        for ent in &self.entitlements {
            encode_string_field(6, ent, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.user_id = v;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.username = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_omega = v != 0;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.omega_expires_at_unix = v;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.plex_balance = v as i64;
                    }
                }
                6 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.entitlements.push(s.to_string());
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.account.v1.GetOmegaStatusRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetOmegaStatusRequest {
    pub user_id: Option<u64>,
}

impl QuasarMessage for GetOmegaStatusRequest {
    const FULL_NAME: &'static str = "eve_public.account.v1.GetOmegaStatusRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(uid) = self.user_id {
            if uid != 0 {
                encode_varint_field(1, uid, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.user_id = Some(v);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.account.v1.GetOmegaStatusResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetOmegaStatusResponse {
    pub is_omega: bool,
    pub omega_expires_at_unix: u64,
    pub status_label: String,
}

impl QuasarMessage for GetOmegaStatusResponse {
    const FULL_NAME: &'static str = "eve_public.account.v1.GetOmegaStatusResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.is_omega {
            encode_varint_field(1, 1, &mut out);
        }
        if self.omega_expires_at_unix != 0 {
            encode_varint_field(2, self.omega_expires_at_unix, &mut out);
        }
        if !self.status_label.is_empty() {
            encode_string_field(3, &self.status_label, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_omega = v != 0;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.omega_expires_at_unix = v;
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.status_label = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.account.v1.GetPlexBalanceRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetPlexBalanceRequest {
    pub user_id: Option<u64>,
}

impl QuasarMessage for GetPlexBalanceRequest {
    const FULL_NAME: &'static str = "eve_public.account.v1.GetPlexBalanceRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(uid) = self.user_id {
            if uid != 0 {
                encode_varint_field(1, uid, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.user_id = Some(v);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.account.v1.GetPlexBalanceResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetPlexBalanceResponse {
    pub user_id: u64,
    pub plex_balance: i64,
}

impl QuasarMessage for GetPlexBalanceResponse {
    const FULL_NAME: &'static str = "eve_public.account.v1.GetPlexBalanceResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.user_id != 0 {
            encode_varint_field(1, self.user_id, &mut out);
        }
        if self.plex_balance != 0 {
            encode_varint_field(2, self.plex_balance as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.user_id = v;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.plex_balance = v as i64;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

// ============================================================================
// Q-4: Character Progression Services (`eve_public.character.v1.*`)
// ============================================================================

/// `eve_public.character.v1.CharacterAttributes`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CharacterAttributes {
    pub intelligence: f64,
    pub memory: f64,
    pub charisma: f64,
    pub perception: f64,
    pub willpower: f64,
}

impl CharacterAttributes {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.intelligence != 0.0 {
            encode_fixed64_field(1, self.intelligence.to_bits(), &mut out);
        }
        if self.memory != 0.0 {
            encode_fixed64_field(2, self.memory.to_bits(), &mut out);
        }
        if self.charisma != 0.0 {
            encode_fixed64_field(3, self.charisma.to_bits(), &mut out);
        }
        if self.perception != 0.0 {
            encode_fixed64_field(4, self.perception.to_bits(), &mut out);
        }
        if self.willpower != 0.0 {
            encode_fixed64_field(5, self.willpower.to_bits(), &mut out);
        }
        out
    }

    pub fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.intelligence = f64::from_bits(v);
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.memory = f64::from_bits(v);
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.charisma = f64::from_bits(v);
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.perception = f64::from_bits(v);
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.willpower = f64::from_bits(v);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.GetProgressionRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetProgressionRequest {
    pub character_id: Option<u32>,
}

impl QuasarMessage for GetProgressionRequest {
    const FULL_NAME: &'static str = "eve_public.character.v1.GetProgressionRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.character_id = Some(v as u32);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.GetProgressionResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetProgressionResponse {
    pub character_id: u32,
    pub total_sp: u64,
    pub unallocated_sp: u32,
    pub skills_count: u32,
    pub career_points: u32,
    pub attributes: Option<CharacterAttributes>,
}

impl QuasarMessage for GetProgressionResponse {
    const FULL_NAME: &'static str = "eve_public.character.v1.GetProgressionResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        if self.total_sp != 0 {
            encode_varint_field(2, self.total_sp, &mut out);
        }
        if self.unallocated_sp != 0 {
            encode_varint_field(3, self.unallocated_sp as u64, &mut out);
        }
        if self.skills_count != 0 {
            encode_varint_field(4, self.skills_count as u64, &mut out);
        }
        if self.career_points != 0 {
            encode_varint_field(5, self.career_points as u64, &mut out);
        }
        if let Some(attr) = &self.attributes {
            encode_bytes_field(6, &attr.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = v as u32;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.total_sp = v;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.unallocated_sp = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.skills_count = v as u32;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.career_points = v as u32;
                    }
                }
                6 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.attributes = Some(CharacterAttributes::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.DailyOpportunity`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyOpportunity {
    pub id: String,
    pub name: String,
    pub description: String,
    pub progress: u64,
    pub target: u64,
    pub is_completed: bool,
    pub is_claimed: bool,
    pub isk_reward: i64,
    pub sp_reward: u32,
}

impl DailyOpportunity {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.id.is_empty() {
            encode_string_field(1, &self.id, &mut out);
        }
        if !self.name.is_empty() {
            encode_string_field(2, &self.name, &mut out);
        }
        if !self.description.is_empty() {
            encode_string_field(3, &self.description, &mut out);
        }
        if self.progress != 0 {
            encode_varint_field(4, self.progress, &mut out);
        }
        if self.target != 0 {
            encode_varint_field(5, self.target, &mut out);
        }
        if self.is_completed {
            encode_varint_field(6, 1, &mut out);
        }
        if self.is_claimed {
            encode_varint_field(7, 1, &mut out);
        }
        if self.isk_reward != 0 {
            encode_varint_field(8, self.isk_reward as u64, &mut out);
        }
        if self.sp_reward != 0 {
            encode_varint_field(9, self.sp_reward as u64, &mut out);
        }
        out
    }

    pub fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.id = s.to_string();
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.name = s.to_string();
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.description = s.to_string();
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.progress = v;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.target = v;
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_completed = v != 0;
                    }
                }
                7 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_claimed = v != 0;
                    }
                }
                8 => {
                    if let Some(v) = f.value.as_varint() {
                        res.isk_reward = v as i64;
                    }
                }
                9 => {
                    if let Some(v) = f.value.as_varint() {
                        res.sp_reward = v as u32;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.GetDailyOpportunitiesRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetDailyOpportunitiesRequest {
    pub character_id: Option<u32>,
}

impl QuasarMessage for GetDailyOpportunitiesRequest {
    const FULL_NAME: &'static str = "eve_public.character.v1.GetDailyOpportunitiesRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.character_id = Some(v as u32);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.GetDailyOpportunitiesResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetDailyOpportunitiesResponse {
    pub character_id: u32,
    pub opportunities: Vec<DailyOpportunity>,
}

impl QuasarMessage for GetDailyOpportunitiesResponse {
    const FULL_NAME: &'static str = "eve_public.character.v1.GetDailyOpportunitiesResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        for opp in &self.opportunities {
            encode_bytes_field(2, &opp.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = v as u32;
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.opportunities.push(DailyOpportunity::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.ClaimDailyOpportunityRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDailyOpportunityRequest {
    pub character_id: Option<u32>,
    pub opportunity_id: String,
}

impl QuasarMessage for ClaimDailyOpportunityRequest {
    const FULL_NAME: &'static str = "eve_public.character.v1.ClaimDailyOpportunityRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        if !self.opportunity_id.is_empty() {
            encode_string_field(2, &self.opportunity_id, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = Some(v as u32);
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.opportunity_id = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.ClaimDailyOpportunityResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimDailyOpportunityResponse {
    pub success: bool,
    pub opportunity_id: String,
    pub isk_reward: i64,
    pub sp_reward: u32,
    pub message: String,
}

impl QuasarMessage for ClaimDailyOpportunityResponse {
    const FULL_NAME: &'static str = "eve_public.character.v1.ClaimDailyOpportunityResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.success {
            encode_varint_field(1, 1, &mut out);
        }
        if !self.opportunity_id.is_empty() {
            encode_string_field(2, &self.opportunity_id, &mut out);
        }
        if self.isk_reward != 0 {
            encode_varint_field(3, self.isk_reward as u64, &mut out);
        }
        if self.sp_reward != 0 {
            encode_varint_field(4, self.sp_reward as u64, &mut out);
        }
        if !self.message.is_empty() {
            encode_string_field(5, &self.message, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.success = v != 0;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.opportunity_id = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.isk_reward = v as i64;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.sp_reward = v as u32;
                    }
                }
                5 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.message = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.Achievement`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Achievement {
    pub id: String,
    pub name: String,
    pub category: String,
    pub current_progress: u64,
    pub target_progress: u64,
    pub is_unlocked: bool,
    pub unlocked_at_unix: u64,
}

impl Achievement {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.id.is_empty() {
            encode_string_field(1, &self.id, &mut out);
        }
        if !self.name.is_empty() {
            encode_string_field(2, &self.name, &mut out);
        }
        if !self.category.is_empty() {
            encode_string_field(3, &self.category, &mut out);
        }
        if self.current_progress != 0 {
            encode_varint_field(4, self.current_progress, &mut out);
        }
        if self.target_progress != 0 {
            encode_varint_field(5, self.target_progress, &mut out);
        }
        if self.is_unlocked {
            encode_varint_field(6, 1, &mut out);
        }
        if self.unlocked_at_unix != 0 {
            encode_varint_field(7, self.unlocked_at_unix, &mut out);
        }
        out
    }

    pub fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.id = s.to_string();
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.name = s.to_string();
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.category = s.to_string();
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.current_progress = v;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.target_progress = v;
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_unlocked = v != 0;
                    }
                }
                7 => {
                    if let Some(v) = f.value.as_varint() {
                        res.unlocked_at_unix = v;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.GetAchievementsRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAchievementsRequest {
    pub character_id: Option<u32>,
}

impl QuasarMessage for GetAchievementsRequest {
    const FULL_NAME: &'static str = "eve_public.character.v1.GetAchievementsRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.character_id = Some(v as u32);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.GetAchievementsResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAchievementsResponse {
    pub character_id: u32,
    pub achievements: Vec<Achievement>,
}

impl QuasarMessage for GetAchievementsResponse {
    const FULL_NAME: &'static str = "eve_public.character.v1.GetAchievementsResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        for ach in &self.achievements {
            encode_bytes_field(2, &ach.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = v as u32;
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.achievements.push(Achievement::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.ClaimAchievementRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimAchievementRequest {
    pub character_id: Option<u32>,
    pub achievement_id: String,
}

impl QuasarMessage for ClaimAchievementRequest {
    const FULL_NAME: &'static str = "eve_public.character.v1.ClaimAchievementRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        if !self.achievement_id.is_empty() {
            encode_string_field(2, &self.achievement_id, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = Some(v as u32);
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.achievement_id = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.character.v1.ClaimAchievementResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimAchievementResponse {
    pub success: bool,
    pub achievement_id: String,
    pub message: String,
}

impl QuasarMessage for ClaimAchievementResponse {
    const FULL_NAME: &'static str = "eve_public.character.v1.ClaimAchievementResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.success {
            encode_varint_field(1, 1, &mut out);
        }
        if !self.achievement_id.is_empty() {
            encode_string_field(2, &self.achievement_id, &mut out);
        }
        if !self.message.is_empty() {
            encode_string_field(3, &self.message, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.success = v != 0;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.achievement_id = s.to_string();
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.message = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

// ============================================================================
// Q-5: Fitting Simulation Services (`eve_public.fitting.v1.*`)
// ============================================================================

/// `eve_public.fitting.v1.SimulatedModule`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulatedModule {
    pub type_id: u32,
    pub flag_id: u32,
    pub state: u32,
    pub charge_type_id: Option<u32>,
}

impl SimulatedModule {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.type_id != 0 {
            encode_varint_field(1, self.type_id as u64, &mut out);
        }
        if self.flag_id != 0 {
            encode_varint_field(2, self.flag_id as u64, &mut out);
        }
        if self.state != 0 {
            encode_varint_field(3, self.state as u64, &mut out);
        }
        if let Some(c) = self.charge_type_id {
            if c != 0 {
                encode_varint_field(4, c as u64, &mut out);
            }
        }
        out
    }

    pub fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.type_id = v as u32;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.flag_id = v as u32;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.state = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.charge_type_id = Some(v as u32);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.fitting.v1.FittingStats`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FittingStats {
    pub cpu_used: f64,
    pub cpu_output: f64,
    pub powergrid_used: f64,
    pub powergrid_output: f64,
    pub calibration_used: f64,
    pub calibration_output: f64,
    pub capacitor_capacity: f64,
    pub capacitor_recharge_rate: f64,
    pub max_velocity: f64,
    pub mass: f64,
    pub inertia_modifier: f64,
    pub align_time_seconds: f64,
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub hull_hp: f64,
    pub effective_hp: f64,
    pub dps: f64,
}

impl FittingStats {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.cpu_used != 0.0 {
            encode_fixed64_field(1, self.cpu_used.to_bits(), &mut out);
        }
        if self.cpu_output != 0.0 {
            encode_fixed64_field(2, self.cpu_output.to_bits(), &mut out);
        }
        if self.powergrid_used != 0.0 {
            encode_fixed64_field(3, self.powergrid_used.to_bits(), &mut out);
        }
        if self.powergrid_output != 0.0 {
            encode_fixed64_field(4, self.powergrid_output.to_bits(), &mut out);
        }
        if self.calibration_used != 0.0 {
            encode_fixed64_field(5, self.calibration_used.to_bits(), &mut out);
        }
        if self.calibration_output != 0.0 {
            encode_fixed64_field(6, self.calibration_output.to_bits(), &mut out);
        }
        if self.capacitor_capacity != 0.0 {
            encode_fixed64_field(7, self.capacitor_capacity.to_bits(), &mut out);
        }
        if self.capacitor_recharge_rate != 0.0 {
            encode_fixed64_field(8, self.capacitor_recharge_rate.to_bits(), &mut out);
        }
        if self.max_velocity != 0.0 {
            encode_fixed64_field(9, self.max_velocity.to_bits(), &mut out);
        }
        if self.mass != 0.0 {
            encode_fixed64_field(10, self.mass.to_bits(), &mut out);
        }
        if self.inertia_modifier != 0.0 {
            encode_fixed64_field(11, self.inertia_modifier.to_bits(), &mut out);
        }
        if self.align_time_seconds != 0.0 {
            encode_fixed64_field(12, self.align_time_seconds.to_bits(), &mut out);
        }
        if self.shield_hp != 0.0 {
            encode_fixed64_field(13, self.shield_hp.to_bits(), &mut out);
        }
        if self.armor_hp != 0.0 {
            encode_fixed64_field(14, self.armor_hp.to_bits(), &mut out);
        }
        if self.hull_hp != 0.0 {
            encode_fixed64_field(15, self.hull_hp.to_bits(), &mut out);
        }
        if self.effective_hp != 0.0 {
            encode_fixed64_field(16, self.effective_hp.to_bits(), &mut out);
        }
        if self.dps != 0.0 {
            encode_fixed64_field(17, self.dps.to_bits(), &mut out);
        }
        out
    }

    pub fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.cpu_used = f64::from_bits(v);
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.cpu_output = f64::from_bits(v);
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.powergrid_used = f64::from_bits(v);
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.powergrid_output = f64::from_bits(v);
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.calibration_used = f64::from_bits(v);
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.calibration_output = f64::from_bits(v);
                    }
                }
                7 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.capacitor_capacity = f64::from_bits(v);
                    }
                }
                8 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.capacitor_recharge_rate = f64::from_bits(v);
                    }
                }
                9 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.max_velocity = f64::from_bits(v);
                    }
                }
                10 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.mass = f64::from_bits(v);
                    }
                }
                11 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.inertia_modifier = f64::from_bits(v);
                    }
                }
                12 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.align_time_seconds = f64::from_bits(v);
                    }
                }
                13 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.shield_hp = f64::from_bits(v);
                    }
                }
                14 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.armor_hp = f64::from_bits(v);
                    }
                }
                15 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.hull_hp = f64::from_bits(v);
                    }
                }
                16 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.effective_hp = f64::from_bits(v);
                    }
                }
                17 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.dps = f64::from_bits(v);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.fitting.v1.SimulateFittingRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimulateFittingRequest {
    pub ship_type_id: u32,
    pub modules: Vec<SimulatedModule>,
    pub character_id: Option<u32>,
}

impl QuasarMessage for SimulateFittingRequest {
    const FULL_NAME: &'static str = "eve_public.fitting.v1.SimulateFittingRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.ship_type_id != 0 {
            encode_varint_field(1, self.ship_type_id as u64, &mut out);
        }
        for m in &self.modules {
            encode_bytes_field(2, &m.encode_proto(), &mut out);
        }
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(3, cid as u64, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.ship_type_id = v as u32;
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.modules.push(SimulatedModule::decode_proto(b)?);
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = Some(v as u32);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.fitting.v1.SimulateFittingResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SimulateFittingResponse {
    pub ship_type_id: u32,
    pub is_valid: bool,
    pub validation_errors: Vec<String>,
    pub stats: Option<FittingStats>,
}

impl QuasarMessage for SimulateFittingResponse {
    const FULL_NAME: &'static str = "eve_public.fitting.v1.SimulateFittingResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.ship_type_id != 0 {
            encode_varint_field(1, self.ship_type_id as u64, &mut out);
        }
        if self.is_valid {
            encode_varint_field(2, 1, &mut out);
        }
        for err in &self.validation_errors {
            encode_string_field(3, err, &mut out);
        }
        if let Some(st) = &self.stats {
            encode_bytes_field(4, &st.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.ship_type_id = v as u32;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_valid = v != 0;
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.validation_errors.push(s.to_string());
                    }
                }
                4 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.stats = Some(FittingStats::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

// ============================================================================
// Q-6: Market & Wallet Services (`eve_public.market.v1.*`, `eve_public.wallet.v1.*`)
// ============================================================================

/// `eve_public.market.v1.MarketOrder`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MarketOrder {
    pub order_id: u64,
    pub type_id: u32,
    pub is_buy_order: bool,
    pub price: f64,
    pub volume_entered: i32,
    pub volume_remain: i32,
    pub min_volume: i32,
    pub order_range: i32,
    pub duration_days: i32,
    pub location_id: i64,
    pub solar_system_id: u32,
    pub issued_unix: u64,
}

impl MarketOrder {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.order_id != 0 {
            encode_varint_field(1, self.order_id, &mut out);
        }
        if self.type_id != 0 {
            encode_varint_field(2, self.type_id as u64, &mut out);
        }
        if self.is_buy_order {
            encode_varint_field(3, 1, &mut out);
        }
        if self.price != 0.0 {
            encode_fixed64_field(4, self.price.to_bits(), &mut out);
        }
        if self.volume_entered != 0 {
            encode_varint_field(5, self.volume_entered as u64, &mut out);
        }
        if self.volume_remain != 0 {
            encode_varint_field(6, self.volume_remain as u64, &mut out);
        }
        if self.min_volume != 0 {
            encode_varint_field(7, self.min_volume as u64, &mut out);
        }
        if self.order_range != 0 {
            encode_varint_field(8, self.order_range as u64, &mut out);
        }
        if self.duration_days != 0 {
            encode_varint_field(9, self.duration_days as u64, &mut out);
        }
        if self.location_id != 0 {
            encode_varint_field(10, self.location_id as u64, &mut out);
        }
        if self.solar_system_id != 0 {
            encode_varint_field(11, self.solar_system_id as u64, &mut out);
        }
        if self.issued_unix != 0 {
            encode_varint_field(12, self.issued_unix, &mut out);
        }
        out
    }

    pub fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.order_id = v;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.type_id = v as u32;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_buy_order = v != 0;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.price = f64::from_bits(v);
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.volume_entered = v as i32;
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res.volume_remain = v as i32;
                    }
                }
                7 => {
                    if let Some(v) = f.value.as_varint() {
                        res.min_volume = v as i32;
                    }
                }
                8 => {
                    if let Some(v) = f.value.as_varint() {
                        res.order_range = v as i32;
                    }
                }
                9 => {
                    if let Some(v) = f.value.as_varint() {
                        res.duration_days = v as i32;
                    }
                }
                10 => {
                    if let Some(v) = f.value.as_varint() {
                        res.location_id = v as i64;
                    }
                }
                11 => {
                    if let Some(v) = f.value.as_varint() {
                        res.solar_system_id = v as u32;
                    }
                }
                12 => {
                    if let Some(v) = f.value.as_varint() {
                        res.issued_unix = v;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.market.v1.GetOrdersRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetOrdersRequest {
    pub region_id: u32,
    pub type_id: u32,
    pub order_type: String, // "all", "buy", "sell"
}

impl QuasarMessage for GetOrdersRequest {
    const FULL_NAME: &'static str = "eve_public.market.v1.GetOrdersRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.region_id != 0 {
            encode_varint_field(1, self.region_id as u64, &mut out);
        }
        if self.type_id != 0 {
            encode_varint_field(2, self.type_id as u64, &mut out);
        }
        if !self.order_type.is_empty() {
            encode_string_field(3, &self.order_type, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.region_id = v as u32;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.type_id = v as u32;
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.order_type = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.market.v1.GetOrdersResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetOrdersResponse {
    pub orders: Vec<MarketOrder>,
}

impl QuasarMessage for GetOrdersResponse {
    const FULL_NAME: &'static str = "eve_public.market.v1.GetOrdersResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for o in &self.orders {
            encode_bytes_field(1, &o.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(b) = f.value.as_bytes() {
                    res.orders.push(MarketOrder::decode_proto(b)?);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.market.v1.MarketHistoryEntry`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MarketHistoryEntry {
    pub date_unix: u64,
    pub average_price: f64,
    pub highest_price: f64,
    pub lowest_price: f64,
    pub order_count: i64,
    pub volume: i64,
}

impl MarketHistoryEntry {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.date_unix != 0 {
            encode_varint_field(1, self.date_unix, &mut out);
        }
        if self.average_price != 0.0 {
            encode_fixed64_field(2, self.average_price.to_bits(), &mut out);
        }
        if self.highest_price != 0.0 {
            encode_fixed64_field(3, self.highest_price.to_bits(), &mut out);
        }
        if self.lowest_price != 0.0 {
            encode_fixed64_field(4, self.lowest_price.to_bits(), &mut out);
        }
        if self.order_count != 0 {
            encode_varint_field(5, self.order_count as u64, &mut out);
        }
        if self.volume != 0 {
            encode_varint_field(6, self.volume as u64, &mut out);
        }
        out
    }

    pub fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.date_unix = v;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.average_price = f64::from_bits(v);
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.highest_price = f64::from_bits(v);
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_fixed64() {
                        res.lowest_price = f64::from_bits(v);
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.order_count = v as i64;
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res.volume = v as i64;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.market.v1.GetMarketHistoryRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetMarketHistoryRequest {
    pub region_id: u32,
    pub type_id: u32,
}

impl QuasarMessage for GetMarketHistoryRequest {
    const FULL_NAME: &'static str = "eve_public.market.v1.GetMarketHistoryRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.region_id != 0 {
            encode_varint_field(1, self.region_id as u64, &mut out);
        }
        if self.type_id != 0 {
            encode_varint_field(2, self.type_id as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.region_id = v as u32;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.type_id = v as u32;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.market.v1.GetMarketHistoryResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetMarketHistoryResponse {
    pub region_id: u32,
    pub type_id: u32,
    pub entries: Vec<MarketHistoryEntry>,
}

impl QuasarMessage for GetMarketHistoryResponse {
    const FULL_NAME: &'static str = "eve_public.market.v1.GetMarketHistoryResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.region_id != 0 {
            encode_varint_field(1, self.region_id as u64, &mut out);
        }
        if self.type_id != 0 {
            encode_varint_field(2, self.type_id as u64, &mut out);
        }
        for e in &self.entries {
            encode_bytes_field(3, &e.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.region_id = v as u32;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.type_id = v as u32;
                    }
                }
                3 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.entries.push(MarketHistoryEntry::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.wallet.v1.GetBalanceRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetBalanceRequest {
    pub character_id: Option<u32>,
}

impl QuasarMessage for GetBalanceRequest {
    const FULL_NAME: &'static str = "eve_public.wallet.v1.GetBalanceRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.character_id = Some(v as u32);
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.wallet.v1.GetBalanceResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetBalanceResponse {
    pub character_id: u32,
    pub isk_balance_units: i64,
    pub isk_balance_cents: i64,
    pub plex_balance: i64,
    pub aur_balance: i64,
}

impl QuasarMessage for GetBalanceResponse {
    const FULL_NAME: &'static str = "eve_public.wallet.v1.GetBalanceResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        if self.isk_balance_units != 0 {
            encode_varint_field(2, self.isk_balance_units as u64, &mut out);
        }
        if self.isk_balance_cents != 0 {
            encode_varint_field(3, self.isk_balance_cents as u64, &mut out);
        }
        if self.plex_balance != 0 {
            encode_varint_field(4, self.plex_balance as u64, &mut out);
        }
        if self.aur_balance != 0 {
            encode_varint_field(5, self.aur_balance as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = v as u32;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.isk_balance_units = v as i64;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.isk_balance_cents = v as i64;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.plex_balance = v as i64;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.aur_balance = v as i64;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.wallet.v1.WalletJournalEntry`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WalletJournalEntry {
    pub entry_id: u64,
    pub date_unix: u64,
    pub ref_type_id: i32,
    pub amount: i64,
    pub balance: i64,
    pub reason: String,
}

impl WalletJournalEntry {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.entry_id != 0 {
            encode_varint_field(1, self.entry_id, &mut out);
        }
        if self.date_unix != 0 {
            encode_varint_field(2, self.date_unix, &mut out);
        }
        if self.ref_type_id != 0 {
            encode_varint_field(3, self.ref_type_id as u64, &mut out);
        }
        if self.amount != 0 {
            encode_varint_field(4, self.amount as u64, &mut out);
        }
        if self.balance != 0 {
            encode_varint_field(5, self.balance as u64, &mut out);
        }
        if !self.reason.is_empty() {
            encode_string_field(6, &self.reason, &mut out);
        }
        out
    }

    pub fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.entry_id = v;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.date_unix = v;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.ref_type_id = v as i32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.amount = v as i64;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.balance = v as i64;
                    }
                }
                6 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.reason = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.wallet.v1.GetJournalRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetJournalRequest {
    pub character_id: Option<u32>,
    pub limit: Option<u32>,
}

impl QuasarMessage for GetJournalRequest {
    const FULL_NAME: &'static str = "eve_public.wallet.v1.GetJournalRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        if let Some(lim) = self.limit {
            if lim != 0 {
                encode_varint_field(2, lim as u64, &mut out);
            }
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = Some(v as u32);
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.limit = Some(v as u32);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.wallet.v1.GetJournalResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetJournalResponse {
    pub character_id: u32,
    pub entries: Vec<WalletJournalEntry>,
}

impl QuasarMessage for GetJournalResponse {
    const FULL_NAME: &'static str = "eve_public.wallet.v1.GetJournalResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        for entry in &self.entries {
            encode_bytes_field(2, &entry.encode_proto(), &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            match f.field_number {
                1 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = v as u32;
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.entries.push(WalletJournalEntry::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}
