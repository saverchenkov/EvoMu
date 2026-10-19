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

// ============================================================================
// Q-7: Space & Combat Services (`eve_public.solarsystem.v1.*`, `eve_public.structure.v1.*`, `eve_public.killmail.v1.*`, etc.)
// ============================================================================

/// `eve_public.solarsystem.v1.GetSolarSystemRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSolarSystemRequest {
    pub solar_system_id: u32,
}

impl QuasarMessage for GetSolarSystemRequest {
    const FULL_NAME: &'static str = "eve_public.solarsystem.v1.GetSolarSystemRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.solar_system_id != 0 {
            encode_varint_field(1, self.solar_system_id as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.solar_system_id = v as u32;
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.solarsystem.v1.GetSolarSystemResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetSolarSystemResponse {
    pub solar_system_id: u32,
    pub solar_system_name: String,
    pub constellation_id: u32,
    pub region_id: u32,
    pub security_status: f64,
    pub stargate_ids: Vec<u64>,
    pub station_ids: Vec<u64>,
    pub star_id: Option<u32>,
}

impl QuasarMessage for GetSolarSystemResponse {
    const FULL_NAME: &'static str = "eve_public.solarsystem.v1.GetSolarSystemResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.solar_system_id != 0 {
            encode_varint_field(1, self.solar_system_id as u64, &mut out);
        }
        if !self.solar_system_name.is_empty() {
            encode_string_field(2, &self.solar_system_name, &mut out);
        }
        if self.constellation_id != 0 {
            encode_varint_field(3, self.constellation_id as u64, &mut out);
        }
        if self.region_id != 0 {
            encode_varint_field(4, self.region_id as u64, &mut out);
        }
        if self.security_status != 0.0 {
            encode_fixed64_field(5, self.security_status.to_bits(), &mut out);
        }
        for gate in &self.stargate_ids {
            encode_varint_field(6, *gate, &mut out);
        }
        for sta in &self.station_ids {
            encode_varint_field(7, *sta, &mut out);
        }
        if let Some(star) = self.star_id {
            if star != 0 {
                encode_varint_field(8, star as u64, &mut out);
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
                        res.solar_system_id = v as u32;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.solar_system_name = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.constellation_id = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.region_id = v as u32;
                    }
                }
                5 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.security_status = f64::from_bits(bits);
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res.stargate_ids.push(v);
                    }
                }
                7 => {
                    if let Some(v) = f.value.as_varint() {
                        res.station_ids.push(v);
                    }
                }
                8 => {
                    if let Some(v) = f.value.as_varint() {
                        res.star_id = Some(v as u32);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.structure.v1.GetStructureRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetStructureRequest {
    pub structure_id: u64,
}

impl QuasarMessage for GetStructureRequest {
    const FULL_NAME: &'static str = "eve_public.structure.v1.GetStructureRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.structure_id != 0 {
            encode_varint_field(1, self.structure_id, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.structure_id = v;
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.structure.v1.GetStructureResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetStructureResponse {
    pub structure_id: u64,
    pub name: String,
    pub type_id: u32,
    pub solar_system_id: u32,
    pub owner_corporation_id: u64,
    pub state: String,
    pub fuel_expires_at_unix: u64,
    pub position_x: f64,
    pub position_y: f64,
    pub position_z: f64,
}

impl QuasarMessage for GetStructureResponse {
    const FULL_NAME: &'static str = "eve_public.structure.v1.GetStructureResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.structure_id != 0 {
            encode_varint_field(1, self.structure_id, &mut out);
        }
        if !self.name.is_empty() {
            encode_string_field(2, &self.name, &mut out);
        }
        if self.type_id != 0 {
            encode_varint_field(3, self.type_id as u64, &mut out);
        }
        if self.solar_system_id != 0 {
            encode_varint_field(4, self.solar_system_id as u64, &mut out);
        }
        if self.owner_corporation_id != 0 {
            encode_varint_field(5, self.owner_corporation_id, &mut out);
        }
        if !self.state.is_empty() {
            encode_string_field(6, &self.state, &mut out);
        }
        if self.fuel_expires_at_unix != 0 {
            encode_varint_field(7, self.fuel_expires_at_unix, &mut out);
        }
        if self.position_x != 0.0 {
            encode_fixed64_field(8, self.position_x.to_bits(), &mut out);
        }
        if self.position_y != 0.0 {
            encode_fixed64_field(9, self.position_y.to_bits(), &mut out);
        }
        if self.position_z != 0.0 {
            encode_fixed64_field(10, self.position_z.to_bits(), &mut out);
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
                        res.structure_id = v;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.name = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.type_id = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.solar_system_id = v as u32;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.owner_corporation_id = v;
                    }
                }
                6 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.state = s.to_string();
                    }
                }
                7 => {
                    if let Some(v) = f.value.as_varint() {
                        res.fuel_expires_at_unix = v;
                    }
                }
                8 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.position_x = f64::from_bits(bits);
                    }
                }
                9 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.position_y = f64::from_bits(bits);
                    }
                }
                10 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.position_z = f64::from_bits(bits);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.killmail.v1.GetKillmailRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetKillmailRequest {
    pub killmail_id: u64,
    pub killmail_hash: Option<String>,
}

impl QuasarMessage for GetKillmailRequest {
    const FULL_NAME: &'static str = "eve_public.killmail.v1.GetKillmailRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.killmail_id != 0 {
            encode_varint_field(1, self.killmail_id, &mut out);
        }
        if let Some(hash) = &self.killmail_hash {
            if !hash.is_empty() {
                encode_string_field(2, hash, &mut out);
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
                        res.killmail_id = v;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.killmail_hash = Some(s.to_string());
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.killmail.v1.GetKillmailResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetKillmailResponse {
    pub killmail_id: u64,
    pub killmail_time_unix: u64,
    pub solar_system_id: u32,
    pub ship_type_id: u32,
    pub victim_character_id: u64,
    pub attacker_character_id: u64,
    pub total_value_isk: f64,
    pub killmail_hash: String,
}

impl QuasarMessage for GetKillmailResponse {
    const FULL_NAME: &'static str = "eve_public.killmail.v1.GetKillmailResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.killmail_id != 0 {
            encode_varint_field(1, self.killmail_id, &mut out);
        }
        if self.killmail_time_unix != 0 {
            encode_varint_field(2, self.killmail_time_unix, &mut out);
        }
        if self.solar_system_id != 0 {
            encode_varint_field(3, self.solar_system_id as u64, &mut out);
        }
        if self.ship_type_id != 0 {
            encode_varint_field(4, self.ship_type_id as u64, &mut out);
        }
        if self.victim_character_id != 0 {
            encode_varint_field(5, self.victim_character_id, &mut out);
        }
        if self.attacker_character_id != 0 {
            encode_varint_field(6, self.attacker_character_id, &mut out);
        }
        if self.total_value_isk != 0.0 {
            encode_fixed64_field(7, self.total_value_isk.to_bits(), &mut out);
        }
        if !self.killmail_hash.is_empty() {
            encode_string_field(8, &self.killmail_hash, &mut out);
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
                        res.killmail_id = v;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.killmail_time_unix = v;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.solar_system_id = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.ship_type_id = v as u32;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.victim_character_id = v;
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res.attacker_character_id = v;
                    }
                }
                7 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.total_value_isk = f64::from_bits(bits);
                    }
                }
                8 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.killmail_hash = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.ship.v1.GetShipRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetShipRequest {
    pub character_id: Option<u32>,
    pub ship_id: Option<u64>,
}

impl QuasarMessage for GetShipRequest {
    const FULL_NAME: &'static str = "eve_public.ship.v1.GetShipRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        if let Some(sid) = self.ship_id {
            if sid != 0 {
                encode_varint_field(2, sid, &mut out);
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
                        res.ship_id = Some(v);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.ship.v1.GetShipResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetShipResponse {
    pub ship_id: u64,
    pub ship_type_id: u32,
    pub ship_name: String,
    pub max_velocity: f64,
    pub shield_hp: f64,
    pub armor_hp: f64,
    pub hull_hp: f64,
}

impl QuasarMessage for GetShipResponse {
    const FULL_NAME: &'static str = "eve_public.ship.v1.GetShipResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.ship_id != 0 {
            encode_varint_field(1, self.ship_id, &mut out);
        }
        if self.ship_type_id != 0 {
            encode_varint_field(2, self.ship_type_id as u64, &mut out);
        }
        if !self.ship_name.is_empty() {
            encode_string_field(3, &self.ship_name, &mut out);
        }
        if self.max_velocity != 0.0 {
            encode_fixed64_field(4, self.max_velocity.to_bits(), &mut out);
        }
        if self.shield_hp != 0.0 {
            encode_fixed64_field(5, self.shield_hp.to_bits(), &mut out);
        }
        if self.armor_hp != 0.0 {
            encode_fixed64_field(6, self.armor_hp.to_bits(), &mut out);
        }
        if self.hull_hp != 0.0 {
            encode_fixed64_field(7, self.hull_hp.to_bits(), &mut out);
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
                        res.ship_id = v;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.ship_type_id = v as u32;
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.ship_name = s.to_string();
                    }
                }
                4 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.max_velocity = f64::from_bits(bits);
                    }
                }
                5 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.shield_hp = f64::from_bits(bits);
                    }
                }
                6 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.armor_hp = f64::from_bits(bits);
                    }
                }
                7 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.hull_hp = f64::from_bits(bits);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.stargate.v1.GetStargateRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetStargateRequest {
    pub stargate_id: u64,
}

impl QuasarMessage for GetStargateRequest {
    const FULL_NAME: &'static str = "eve_public.stargate.v1.GetStargateRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.stargate_id != 0 {
            encode_varint_field(1, self.stargate_id, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.stargate_id = v;
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.stargate.v1.GetStargateResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetStargateResponse {
    pub stargate_id: u64,
    pub name: String,
    pub solar_system_id: u32,
    pub destination_stargate_id: u64,
    pub destination_system_id: u32,
}

impl QuasarMessage for GetStargateResponse {
    const FULL_NAME: &'static str = "eve_public.stargate.v1.GetStargateResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.stargate_id != 0 {
            encode_varint_field(1, self.stargate_id, &mut out);
        }
        if !self.name.is_empty() {
            encode_string_field(2, &self.name, &mut out);
        }
        if self.solar_system_id != 0 {
            encode_varint_field(3, self.solar_system_id as u64, &mut out);
        }
        if self.destination_stargate_id != 0 {
            encode_varint_field(4, self.destination_stargate_id, &mut out);
        }
        if self.destination_system_id != 0 {
            encode_varint_field(5, self.destination_system_id as u64, &mut out);
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
                        res.stargate_id = v;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.name = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.solar_system_id = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.destination_stargate_id = v;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.destination_system_id = v as u32;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.station.v1.GetStationRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetStationRequest {
    pub station_id: u64,
}

impl QuasarMessage for GetStationRequest {
    const FULL_NAME: &'static str = "eve_public.station.v1.GetStationRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.station_id != 0 {
            encode_varint_field(1, self.station_id, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.station_id = v;
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.station.v1.GetStationResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetStationResponse {
    pub station_id: u64,
    pub name: String,
    pub solar_system_id: u32,
    pub type_id: u32,
    pub owner_corporation_id: u64,
    pub reprocessing_efficiency: f64,
}

impl QuasarMessage for GetStationResponse {
    const FULL_NAME: &'static str = "eve_public.station.v1.GetStationResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.station_id != 0 {
            encode_varint_field(1, self.station_id, &mut out);
        }
        if !self.name.is_empty() {
            encode_string_field(2, &self.name, &mut out);
        }
        if self.solar_system_id != 0 {
            encode_varint_field(3, self.solar_system_id as u64, &mut out);
        }
        if self.type_id != 0 {
            encode_varint_field(4, self.type_id as u64, &mut out);
        }
        if self.owner_corporation_id != 0 {
            encode_varint_field(5, self.owner_corporation_id, &mut out);
        }
        if self.reprocessing_efficiency != 0.0 {
            encode_fixed64_field(6, self.reprocessing_efficiency.to_bits(), &mut out);
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
                        res.station_id = v;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.name = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.solar_system_id = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.type_id = v as u32;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.owner_corporation_id = v;
                    }
                }
                6 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.reprocessing_efficiency = f64::from_bits(bits);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// Cosmic signature entry for probe scanning.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeScanSignature {
    pub signature_id: String,
    pub signature_type: String,
    pub signal_strength: f64,
}

impl ProbeScanSignature {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.signature_id.is_empty() {
            encode_string_field(1, &self.signature_id, &mut out);
        }
        if !self.signature_type.is_empty() {
            encode_string_field(2, &self.signature_type, &mut out);
        }
        if self.signal_strength != 0.0 {
            encode_fixed64_field(3, self.signal_strength.to_bits(), &mut out);
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
                        res.signature_id = s.to_string();
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.signature_type = s.to_string();
                    }
                }
                3 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.signal_strength = f64::from_bits(bits);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.probe.v1.GetProbeScansRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetProbeScansRequest {
    pub solar_system_id: u32,
}

impl QuasarMessage for GetProbeScansRequest {
    const FULL_NAME: &'static str = "eve_public.probe.v1.GetProbeScansRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.solar_system_id != 0 {
            encode_varint_field(1, self.solar_system_id as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.solar_system_id = v as u32;
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.probe.v1.GetProbeScansResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetProbeScansResponse {
    pub solar_system_id: u32,
    pub signatures: Vec<ProbeScanSignature>,
}

impl QuasarMessage for GetProbeScansResponse {
    const FULL_NAME: &'static str = "eve_public.probe.v1.GetProbeScansResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.solar_system_id != 0 {
            encode_varint_field(1, self.solar_system_id as u64, &mut out);
        }
        for sig in &self.signatures {
            encode_bytes_field(2, &sig.encode_proto(), &mut out);
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
                        res.solar_system_id = v as u32;
                    }
                }
                2 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.signatures.push(ProbeScanSignature::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

// ============================================================================
// Q-8: Sovereignty, Faction Warfare & Events (`eve_public.sovereignty.v1.*`, `eve_public.factionwarfare.v1.*`, etc.)
// ============================================================================

/// `eve_public.sovereignty.v1.GetSovereigntyRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSovereigntyRequest {
    pub solar_system_id: u32,
}

impl QuasarMessage for GetSovereigntyRequest {
    const FULL_NAME: &'static str = "eve_public.sovereignty.v1.GetSovereigntyRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.solar_system_id != 0 {
            encode_varint_field(1, self.solar_system_id as u64, &mut out);
        }
        out
    }

    fn decode_proto(buf: &[u8]) -> Result<Self, QuasarError> {
        let wm = WireMessage::decode(buf)?;
        let mut res = Self::default();
        for f in wm.fields {
            if f.field_number == 1 {
                if let Some(v) = f.value.as_varint() {
                    res.solar_system_id = v as u32;
                }
            }
        }
        Ok(res)
    }
}

/// `eve_public.sovereignty.v1.GetSovereigntyResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetSovereigntyResponse {
    pub solar_system_id: u32,
    pub alliance_id: Option<u64>,
    pub corporation_id: Option<u64>,
    pub faction_id: Option<u64>,
    pub military_index: u32,
    pub industrial_index: u32,
    pub strategic_index: u32,
    pub dbs_multiplier: f64,
}

impl QuasarMessage for GetSovereigntyResponse {
    const FULL_NAME: &'static str = "eve_public.sovereignty.v1.GetSovereigntyResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.solar_system_id != 0 {
            encode_varint_field(1, self.solar_system_id as u64, &mut out);
        }
        if let Some(aid) = self.alliance_id {
            if aid != 0 {
                encode_varint_field(2, aid, &mut out);
            }
        }
        if let Some(cid) = self.corporation_id {
            if cid != 0 {
                encode_varint_field(3, cid, &mut out);
            }
        }
        if let Some(fid) = self.faction_id {
            if fid != 0 {
                encode_varint_field(4, fid, &mut out);
            }
        }
        if self.military_index != 0 {
            encode_varint_field(5, self.military_index as u64, &mut out);
        }
        if self.industrial_index != 0 {
            encode_varint_field(6, self.industrial_index as u64, &mut out);
        }
        if self.strategic_index != 0 {
            encode_varint_field(7, self.strategic_index as u64, &mut out);
        }
        if self.dbs_multiplier != 0.0 {
            encode_fixed64_field(8, self.dbs_multiplier.to_bits(), &mut out);
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
                        res.solar_system_id = v as u32;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.alliance_id = Some(v);
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.corporation_id = Some(v);
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.faction_id = Some(v);
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.military_index = v as u32;
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res.industrial_index = v as u32;
                    }
                }
                7 => {
                    if let Some(v) = f.value.as_varint() {
                        res.strategic_index = v as u32;
                    }
                }
                8 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.dbs_multiplier = f64::from_bits(bits);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.factionwarfare.v1.GetFactionWarfareRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetFactionWarfareRequest {
    pub character_id: Option<u32>,
    pub faction_id: Option<u64>,
}

impl QuasarMessage for GetFactionWarfareRequest {
    const FULL_NAME: &'static str = "eve_public.factionwarfare.v1.GetFactionWarfareRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        if let Some(fid) = self.faction_id {
            if fid != 0 {
                encode_varint_field(2, fid, &mut out);
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
                        res.faction_id = Some(v);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.factionwarfare.v1.GetFactionWarfareResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetFactionWarfareResponse {
    pub faction_id: u64,
    pub current_rank: u32,
    pub highest_rank: u32,
    pub kills: u64,
    pub victory_points: u64,
    pub contested_systems: u32,
}

impl QuasarMessage for GetFactionWarfareResponse {
    const FULL_NAME: &'static str = "eve_public.factionwarfare.v1.GetFactionWarfareResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.faction_id != 0 {
            encode_varint_field(1, self.faction_id, &mut out);
        }
        if self.current_rank != 0 {
            encode_varint_field(2, self.current_rank as u64, &mut out);
        }
        if self.highest_rank != 0 {
            encode_varint_field(3, self.highest_rank as u64, &mut out);
        }
        if self.kills != 0 {
            encode_varint_field(4, self.kills, &mut out);
        }
        if self.victory_points != 0 {
            encode_varint_field(5, self.victory_points, &mut out);
        }
        if self.contested_systems != 0 {
            encode_varint_field(6, self.contested_systems as u64, &mut out);
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
                        res.faction_id = v;
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.current_rank = v as u32;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.highest_rank = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.kills = v;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.victory_points = v;
                    }
                }
                6 => {
                    if let Some(v) = f.value.as_varint() {
                        res.contested_systems = v as u32;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.projectdiscovery.v1.GetProjectDiscoveryRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetProjectDiscoveryRequest {
    pub character_id: Option<u32>,
}

impl QuasarMessage for GetProjectDiscoveryRequest {
    const FULL_NAME: &'static str = "eve_public.projectdiscovery.v1.GetProjectDiscoveryRequest";

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

/// `eve_public.projectdiscovery.v1.GetProjectDiscoveryResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetProjectDiscoveryResponse {
    pub character_id: u32,
    pub score_percent: f64,
    pub rank: u32,
    pub daily_xp_collected: u32,
    pub daily_xp_cap: u32,
    pub active_task_id: String,
}

impl QuasarMessage for GetProjectDiscoveryResponse {
    const FULL_NAME: &'static str = "eve_public.projectdiscovery.v1.GetProjectDiscoveryResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        if self.score_percent != 0.0 {
            encode_fixed64_field(2, self.score_percent.to_bits(), &mut out);
        }
        if self.rank != 0 {
            encode_varint_field(3, self.rank as u64, &mut out);
        }
        if self.daily_xp_collected != 0 {
            encode_varint_field(4, self.daily_xp_collected as u64, &mut out);
        }
        if self.daily_xp_cap != 0 {
            encode_varint_field(5, self.daily_xp_cap as u64, &mut out);
        }
        if !self.active_task_id.is_empty() {
            encode_string_field(6, &self.active_task_id, &mut out);
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
                    if let Some(bits) = f.value.as_fixed64() {
                        res.score_percent = f64::from_bits(bits);
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.rank = v as u32;
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.daily_xp_collected = v as u32;
                    }
                }
                5 => {
                    if let Some(v) = f.value.as_varint() {
                        res.daily_xp_cap = v as u32;
                    }
                }
                6 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.active_task_id = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// Operation / AIR career program item.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OperationItem {
    pub operation_id: u32,
    pub title: String,
    pub is_completed: bool,
    pub progress: f64,
}

impl OperationItem {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.operation_id != 0 {
            encode_varint_field(1, self.operation_id as u64, &mut out);
        }
        if !self.title.is_empty() {
            encode_string_field(2, &self.title, &mut out);
        }
        if self.is_completed {
            encode_varint_field(3, 1, &mut out);
        }
        if self.progress != 0.0 {
            encode_fixed64_field(4, self.progress.to_bits(), &mut out);
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
                        res.operation_id = v as u32;
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.title = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.is_completed = v != 0;
                    }
                }
                4 => {
                    if let Some(bits) = f.value.as_fixed64() {
                        res.progress = f64::from_bits(bits);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.operation.v1.GetOperationsRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetOperationsRequest {
    pub character_id: Option<u32>,
}

impl QuasarMessage for GetOperationsRequest {
    const FULL_NAME: &'static str = "eve_public.operation.v1.GetOperationsRequest";

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

/// `eve_public.operation.v1.GetOperationsResponse`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GetOperationsResponse {
    pub character_id: u32,
    pub operations: Vec<OperationItem>,
}

impl QuasarMessage for GetOperationsResponse {
    const FULL_NAME: &'static str = "eve_public.operation.v1.GetOperationsResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        for op in &self.operations {
            encode_bytes_field(2, &op.encode_proto(), &mut out);
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
                        res.operations.push(OperationItem::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

// ============================================================================
// Q-9: Cosmetics & App Infrastructure (`eve_public.cosmetic.v1.*`, `eve_public.settings.v1.*`, etc.)
// ============================================================================

/// `eve_public.cosmetic.v1.GetCosmeticsRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetCosmeticsRequest {
    pub character_id: Option<u32>,
    pub ship_type_id: Option<u32>,
}

impl QuasarMessage for GetCosmeticsRequest {
    const FULL_NAME: &'static str = "eve_public.cosmetic.v1.GetCosmeticsRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        if let Some(stid) = self.ship_type_id {
            if stid != 0 {
                encode_varint_field(2, stid as u64, &mut out);
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
                        res.ship_type_id = Some(v as u32);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.cosmetic.v1.GetCosmeticsResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetCosmeticsResponse {
    pub character_id: u32,
    pub licensed_skin_ids: Vec<u32>,
    pub applied_skin_id: Option<u32>,
    pub evermarks_balance: u64,
}

impl QuasarMessage for GetCosmeticsResponse {
    const FULL_NAME: &'static str = "eve_public.cosmetic.v1.GetCosmeticsResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        for skin in &self.licensed_skin_ids {
            encode_varint_field(2, *skin as u64, &mut out);
        }
        if let Some(applied) = self.applied_skin_id {
            if applied != 0 {
                encode_varint_field(3, applied as u64, &mut out);
            }
        }
        if self.evermarks_balance != 0 {
            encode_varint_field(4, self.evermarks_balance, &mut out);
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
                        res.licensed_skin_ids.push(v as u32);
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.applied_skin_id = Some(v as u32);
                    }
                }
                4 => {
                    if let Some(v) = f.value.as_varint() {
                        res.evermarks_balance = v;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// Client key-value setting pair.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSettingEntry {
    pub key: String,
    pub value: String,
}

impl ClientSettingEntry {
    pub fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if !self.key.is_empty() {
            encode_string_field(1, &self.key, &mut out);
        }
        if !self.value.is_empty() {
            encode_string_field(2, &self.value, &mut out);
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
                        res.key = s.to_string();
                    }
                }
                2 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.value = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.settings.v1.GetClientSettingsRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetClientSettingsRequest {
    pub user_id: Option<u64>,
    pub character_id: Option<u32>,
    pub section: Option<String>,
}

impl QuasarMessage for GetClientSettingsRequest {
    const FULL_NAME: &'static str = "eve_public.settings.v1.GetClientSettingsRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(uid) = self.user_id {
            if uid != 0 {
                encode_varint_field(1, uid, &mut out);
            }
        }
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(2, cid as u64, &mut out);
            }
        }
        if let Some(sec) = &self.section {
            if !sec.is_empty() {
                encode_string_field(3, sec, &mut out);
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
                        res.user_id = Some(v);
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = Some(v as u32);
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.section = Some(s.to_string());
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.settings.v1.GetClientSettingsResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetClientSettingsResponse {
    pub user_id: u64,
    pub character_id: u32,
    pub settings: Vec<ClientSettingEntry>,
}

impl QuasarMessage for GetClientSettingsResponse {
    const FULL_NAME: &'static str = "eve_public.settings.v1.GetClientSettingsResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.user_id != 0 {
            encode_varint_field(1, self.user_id, &mut out);
        }
        if self.character_id != 0 {
            encode_varint_field(2, self.character_id as u64, &mut out);
        }
        for entry in &self.settings {
            encode_bytes_field(3, &entry.encode_proto(), &mut out);
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
                        res.character_id = v as u32;
                    }
                }
                3 => {
                    if let Some(b) = f.value.as_bytes() {
                        res.settings.push(ClientSettingEntry::decode_proto(b)?);
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.settings.v1.SetClientSettingsRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetClientSettingsRequest {
    pub user_id: Option<u64>,
    pub character_id: Option<u32>,
    pub key: String,
    pub value: String,
}

impl QuasarMessage for SetClientSettingsRequest {
    const FULL_NAME: &'static str = "eve_public.settings.v1.SetClientSettingsRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(uid) = self.user_id {
            if uid != 0 {
                encode_varint_field(1, uid, &mut out);
            }
        }
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(2, cid as u64, &mut out);
            }
        }
        if !self.key.is_empty() {
            encode_string_field(3, &self.key, &mut out);
        }
        if !self.value.is_empty() {
            encode_string_field(4, &self.value, &mut out);
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
                        res.user_id = Some(v);
                    }
                }
                2 => {
                    if let Some(v) = f.value.as_varint() {
                        res.character_id = Some(v as u32);
                    }
                }
                3 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.key = s.to_string();
                    }
                }
                4 => {
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.value = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.settings.v1.SetClientSettingsResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetClientSettingsResponse {
    pub success: bool,
    pub key: String,
    pub updated_at_unix: u64,
}

impl QuasarMessage for SetClientSettingsResponse {
    const FULL_NAME: &'static str = "eve_public.settings.v1.SetClientSettingsResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.success {
            encode_varint_field(1, 1, &mut out);
        }
        if !self.key.is_empty() {
            encode_string_field(2, &self.key, &mut out);
        }
        if self.updated_at_unix != 0 {
            encode_varint_field(3, self.updated_at_unix, &mut out);
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
                        res.key = s.to_string();
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.updated_at_unix = v;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.paragon.v1.GetParagonMissionsRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetParagonMissionsRequest {
    pub character_id: Option<u32>,
}

impl QuasarMessage for GetParagonMissionsRequest {
    const FULL_NAME: &'static str = "eve_public.paragon.v1.GetParagonMissionsRequest";

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

/// `eve_public.paragon.v1.GetParagonMissionsResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetParagonMissionsResponse {
    pub character_id: u32,
    pub evermarks_balance: u64,
    pub missions_completed: u32,
}

impl QuasarMessage for GetParagonMissionsResponse {
    const FULL_NAME: &'static str = "eve_public.paragon.v1.GetParagonMissionsResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        if self.evermarks_balance != 0 {
            encode_varint_field(2, self.evermarks_balance, &mut out);
        }
        if self.missions_completed != 0 {
            encode_varint_field(3, self.missions_completed as u64, &mut out);
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
                        res.evermarks_balance = v;
                    }
                }
                3 => {
                    if let Some(v) = f.value.as_varint() {
                        res.missions_completed = v as u32;
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.ui.v1.GetUiLayoutRequest`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetUiLayoutRequest {
    pub character_id: Option<u32>,
    pub window_name: Option<String>,
}

impl QuasarMessage for GetUiLayoutRequest {
    const FULL_NAME: &'static str = "eve_public.ui.v1.GetUiLayoutRequest";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if let Some(cid) = self.character_id {
            if cid != 0 {
                encode_varint_field(1, cid as u64, &mut out);
            }
        }
        if let Some(win) = &self.window_name {
            if !win.is_empty() {
                encode_string_field(2, win, &mut out);
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
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.window_name = Some(s.to_string());
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}

/// `eve_public.ui.v1.GetUiLayoutResponse`
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetUiLayoutResponse {
    pub character_id: u32,
    pub layout_json: String,
}

impl QuasarMessage for GetUiLayoutResponse {
    const FULL_NAME: &'static str = "eve_public.ui.v1.GetUiLayoutResponse";

    fn encode_proto(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.character_id != 0 {
            encode_varint_field(1, self.character_id as u64, &mut out);
        }
        if !self.layout_json.is_empty() {
            encode_string_field(2, &self.layout_json, &mut out);
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
                    if let Ok(Some(s)) = f.value.as_str() {
                        res.layout_json = s.to_string();
                    }
                }
                _ => {}
            }
        }
        Ok(res)
    }
}
