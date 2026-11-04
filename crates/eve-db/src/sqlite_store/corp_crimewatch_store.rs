//! SQLite persistence for `D-6` Batch 3: Corporation Governance, Crimewatch, and Warfare durability.
//!
//! Persists corporation governance (`CORP_GOV`, `EMP_HISTORY`, titles, roles, medals, votes,
//! divisions, shareholders, bulletins, ads, contacts), crimewatch state (`CrimeWatchState`,
//! aggressions, security-status transactions, safety levels, drone settings, limited engagements,
//! NPC timers, disapproval, Zarzakh gate locks, wormhole polarizations, structure docking ACL
//! denials), and warfare/FW/sovereignty/insurgency runtime state in `d6_corp_crimewatch_kv`
//! (schema migration `0003_d6_corp_crimewatch_warfare`).

use crate::durable_domain_types::tuple_map_serde;
use crate::sqlite_store::fingerprints::{FnvHasher, TableFingerprints};
use crate::types::*;
use eve_sde::SolarSystemId;
use rusqlite::{params, Connection, Transaction};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const D6_CORP_CRIMEWATCH_SCHEMA_VERSION: i64 = 4;

const D6_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS d6_corp_crimewatch_kv (
    store_key TEXT PRIMARY KEY,
    payload_json TEXT NOT NULL
);
"#;

const KEY_CORP_GOV: &str = "corp_governance_domain";
const KEY_CRIMEWATCH: &str = "crimewatch_domain";
const KEY_WARFARE: &str = "warfare_domain";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct D6CorpCrimewatchFingerprints {
    pub corp_governance_fp: u64,
    pub crimewatch_fp: u64,
    pub warfare_fp: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CorpGovernanceDomainPayload {
    #[serde(default)]
    corp_governance: HashMap<i64, CorpGovState>,
    #[serde(default)]
    employment_history: HashMap<i64, Vec<(i64, u64, Option<u64>)>>,
    #[serde(default)]
    corp_hangar_divisions: HashMap<i64, HashMap<u8, String>>,
    #[serde(default)]
    corp_alliance_history: HashMap<i64, Vec<CorpAllianceHistoryEntry>>,
    #[serde(default)]
    corp_container_logs: HashMap<i64, Vec<CorpContainerLogEntry>>,
    #[serde(default)]
    corp_medals: HashMap<i64, Vec<CorpMedalEntry>>,
    #[serde(default)]
    corp_issued_medals: HashMap<i64, Vec<CorpIssuedMedalEntry>>,
    #[serde(default)]
    corp_member_limits: HashMap<i64, i64>,
    #[serde(default)]
    corp_titles: HashMap<i64, Vec<CorpTitleEntry>>,
    #[serde(default, with = "tuple_map_serde")]
    corp_member_titles: HashMap<(i64, i64), Vec<i64>>,
    #[serde(default, with = "tuple_map_serde")]
    corp_role_assignments: HashMap<(i64, i64), CorpMemberRoleAssignment>,
    #[serde(default)]
    corp_role_history: HashMap<i64, Vec<CorpRoleHistoryEntry>>,
    #[serde(default)]
    corp_shareholders: HashMap<i64, Vec<CorpShareholderEntry>>,
    #[serde(default)]
    char_medals: HashMap<i64, Vec<CharacterMedalRecord>>,
    #[serde(default)]
    char_scoped_roles: HashMap<i64, CharacterScopedRolesRecord>,
    #[serde(default)]
    corp_vote_cases: HashMap<i64, CorpVoteCaseRecord>,
    #[serde(default, with = "tuple_map_serde")]
    corp_vote_ballots: HashMap<(i64, i64), CorpVoteBallotRecord>,
}

impl CorpGovernanceDomainPayload {
    fn from_snapshot(snap: &WorldStateSnapshot) -> Self {
        Self {
            corp_governance: snap.corp_governance.clone(),
            employment_history: snap.employment_history.clone(),
            corp_hangar_divisions: snap.corp_hangar_divisions.clone(),
            corp_alliance_history: snap.corp_alliance_history.clone(),
            corp_container_logs: snap.corp_container_logs.clone(),
            corp_medals: snap.corp_medals.clone(),
            corp_issued_medals: snap.corp_issued_medals.clone(),
            corp_member_limits: snap.corp_member_limits.clone(),
            corp_titles: snap.corp_titles.clone(),
            corp_member_titles: snap.corp_member_titles.clone(),
            corp_role_assignments: snap.corp_role_assignments.clone(),
            corp_role_history: snap.corp_role_history.clone(),
            corp_shareholders: snap.corp_shareholders.clone(),
            char_medals: snap.char_medals.clone(),
            char_scoped_roles: snap.char_scoped_roles.clone(),
            corp_vote_cases: snap.corp_vote_cases.clone(),
            corp_vote_ballots: snap.corp_vote_ballots.clone(),
        }
    }

    fn apply_to_snapshot(self, snap: &mut WorldStateSnapshot) {
        snap.corp_governance = self.corp_governance;
        snap.employment_history = self.employment_history;
        snap.corp_hangar_divisions = self.corp_hangar_divisions;
        snap.corp_alliance_history = self.corp_alliance_history;
        snap.corp_container_logs = self.corp_container_logs;
        snap.corp_medals = self.corp_medals;
        snap.corp_issued_medals = self.corp_issued_medals;
        snap.corp_member_limits = self.corp_member_limits;
        snap.corp_titles = self.corp_titles;
        snap.corp_member_titles = self.corp_member_titles;
        snap.corp_role_assignments = self.corp_role_assignments;
        snap.corp_role_history = self.corp_role_history;
        snap.corp_shareholders = self.corp_shareholders;
        snap.char_medals = self.char_medals;
        snap.char_scoped_roles = self.char_scoped_roles;
        snap.corp_vote_cases = self.corp_vote_cases;
        snap.corp_vote_ballots = self.corp_vote_ballots;
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CrimewatchDomainPayload {
    #[serde(default)]
    crimewatch_states: HashMap<i64, PersistedCrimeWatchState>,
    #[serde(default, with = "tuple_map_serde")]
    crimewatch_aggressions: HashMap<(i64, i64), PersistedAggressionRecord>,
    #[serde(default)]
    combat_crimewatch_state: PersistedCombatCrimewatchState,
    #[serde(default, with = "tuple_map_serde")]
    wh_polarizations: HashMap<(i64, i64), u64>,
    #[serde(default)]
    zarzakh_gate_locks: HashMap<i64, (i64, u64)>,
    #[serde(default)]
    structure_docking_acl_denied: HashMap<i64, Vec<i64>>,
}

impl CrimewatchDomainPayload {
    fn from_snapshot(snap: &WorldStateSnapshot) -> Self {
        let structure_docking_acl_denied = snap
            .structure_docking_acl_denied
            .iter()
            .map(|(&sid, set)| {
                let mut v: Vec<i64> = set.iter().copied().collect();
                v.sort_unstable();
                (sid, v)
            })
            .collect();
        Self {
            crimewatch_states: snap.crimewatch_states.clone(),
            crimewatch_aggressions: snap.crimewatch_aggressions.clone(),
            combat_crimewatch_state: snap.combat_crimewatch_state.clone(),
            wh_polarizations: snap.wh_polarizations.clone(),
            zarzakh_gate_locks: snap.zarzakh_gate_locks.clone(),
            structure_docking_acl_denied,
        }
    }

    fn apply_to_snapshot(self, snap: &mut WorldStateSnapshot) {
        snap.crimewatch_states = self.crimewatch_states;
        snap.crimewatch_aggressions = self.crimewatch_aggressions;
        snap.combat_crimewatch_state = self.combat_crimewatch_state;
        snap.wh_polarizations = self.wh_polarizations;
        snap.zarzakh_gate_locks = self.zarzakh_gate_locks;
        snap.structure_docking_acl_denied = self
            .structure_docking_acl_denied
            .into_iter()
            .map(|(sid, v)| (sid, v.into_iter().collect::<HashSet<i64>>()))
            .collect();
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WarfareDomainPayload {
    #[serde(default)]
    alliance_war_state: PersistedAllianceWarState,
    #[serde(default)]
    war_state: PersistedWarState,
    #[serde(default)]
    fw_runtime: PersistedFwState,
    #[serde(default)]
    sov_runtime: PersistedSovState,
    #[serde(default)]
    insurgency_runtime: PersistedInsurgencyState,
    #[serde(default)]
    sov_prospecting_levels: HashMap<SolarSystemId, u32>,
    #[serde(default)]
    sov_campaign_scores: HashMap<i64, SovCampaignScoreRecord>,
    #[serde(default)]
    fw_character_stats: HashMap<i64, FwCharacterStatsRecord>,
    #[serde(default)]
    fw_corp_enlisted_on: HashMap<i64, u64>,
    #[serde(default)]
    fw_system_occupations: HashMap<SolarSystemId, FwSystemOccupationRecord>,
    #[serde(default)]
    fw_wars: Vec<(i64, i64)>,
    #[serde(default)]
    war_lifecycle_meta: HashMap<i64, WarLifecycleMeta>,
    #[serde(default)]
    alliance_lifecycle_meta: HashMap<i64, AllianceLifecycleMeta>,
}

impl WarfareDomainPayload {
    fn from_snapshot(snap: &WorldStateSnapshot) -> Self {
        Self {
            alliance_war_state: snap.alliance_war_state.clone(),
            war_state: snap.war_state.clone(),
            fw_runtime: snap.fw_runtime.clone(),
            sov_runtime: snap.sov_runtime.clone(),
            insurgency_runtime: snap.insurgency_runtime.clone(),
            sov_prospecting_levels: snap.sov_prospecting_levels.clone(),
            sov_campaign_scores: snap.sov_campaign_scores.clone(),
            fw_character_stats: snap.fw_character_stats.clone(),
            fw_corp_enlisted_on: snap.fw_corp_enlisted_on.clone(),
            fw_system_occupations: snap.fw_system_occupations.clone(),
            fw_wars: snap.fw_wars.clone(),
            war_lifecycle_meta: snap.war_lifecycle_meta.clone(),
            alliance_lifecycle_meta: snap.alliance_lifecycle_meta.clone(),
        }
    }

    fn apply_to_snapshot(self, snap: &mut WorldStateSnapshot) {
        snap.alliance_war_state = self.alliance_war_state;
        snap.war_state = self.war_state;
        snap.fw_runtime = self.fw_runtime;
        snap.sov_runtime = self.sov_runtime;
        snap.insurgency_runtime = self.insurgency_runtime;
        snap.sov_prospecting_levels = self.sov_prospecting_levels;
        snap.sov_campaign_scores = self.sov_campaign_scores;
        snap.fw_character_stats = self.fw_character_stats;
        snap.fw_corp_enlisted_on = self.fw_corp_enlisted_on;
        snap.fw_system_occupations = self.fw_system_occupations;
        snap.fw_wars = self.fw_wars;
        snap.war_lifecycle_meta = self.war_lifecycle_meta;
        snap.alliance_lifecycle_meta = self.alliance_lifecycle_meta;
    }
}

fn hash_serde_entry<T: Serialize + ?Sized>(val: &T) -> u64 {
    let mut h = FnvHasher::new();
    let _ = val.serialize(FnvSerializer(&mut h));
    h.finish()
}

fn canonical_fp<T: Serialize + ?Sized>(val: &T) -> u64 {
    hash_serde_entry(val)
}

#[derive(Debug)]
struct FnvSerError;

impl std::fmt::Display for FnvSerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("fnv serialization error")
    }
}

impl std::error::Error for FnvSerError {}

impl serde::ser::Error for FnvSerError {
    fn custom<T: std::fmt::Display>(_msg: T) -> Self {
        Self
    }
}

struct FnvSerializer<'a>(&'a mut FnvHasher);

struct FnvSeqSer<'a>(&'a mut FnvHasher);

impl serde::ser::SerializeSeq for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTuple for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleVariant for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

struct FnvMapSer<'a> {
    parent: &'a mut FnvHasher,
    len: u64,
    xor: u64,
    pending_key_hash: u64,
}

impl serde::ser::SerializeMap for FnvMapSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), FnvSerError> {
        self.pending_key_hash = hash_serde_entry(key);
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), FnvSerError> {
        let mut eh = FnvHasher::new();
        eh.write_u64(self.pending_key_hash);
        value.serialize(FnvSerializer(&mut eh))?;
        self.xor ^= eh.finish();
        self.len = self.len.wrapping_add(1);
        Ok(())
    }
    fn end(self) -> Result<(), FnvSerError> {
        self.parent.write_u64(self.len);
        self.parent.write_u64(self.xor);
        Ok(())
    }
}

impl serde::ser::SerializeStruct for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_str(key);
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl serde::ser::SerializeStructVariant for FnvSeqSer<'_> {
    type Ok = ();
    type Error = FnvSerError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_str(key);
        value.serialize(FnvSerializer(self.0))
    }
    fn end(self) -> Result<(), FnvSerError> {
        Ok(())
    }
}

impl<'a> serde::Serializer for FnvSerializer<'a> {
    type Ok = ();
    type Error = FnvSerError;
    type SerializeSeq = FnvSeqSer<'a>;
    type SerializeTuple = FnvSeqSer<'a>;
    type SerializeTupleStruct = FnvSeqSer<'a>;
    type SerializeTupleVariant = FnvSeqSer<'a>;
    type SerializeMap = FnvMapSer<'a>;
    type SerializeStruct = FnvSeqSer<'a>;
    type SerializeStructVariant = FnvSeqSer<'a>;

    fn serialize_bool(self, v: bool) -> Result<(), FnvSerError> {
        self.0.write_u8(1);
        self.0.write_bool(v);
        Ok(())
    }
    fn serialize_i8(self, v: i8) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i16(self, v: i16) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i32(self, v: i32) -> Result<(), FnvSerError> {
        self.serialize_i64(i64::from(v))
    }
    fn serialize_i64(self, v: i64) -> Result<(), FnvSerError> {
        self.0.write_u8(2);
        self.0.write_i64(v);
        Ok(())
    }
    fn serialize_u8(self, v: u8) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u16(self, v: u16) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u32(self, v: u32) -> Result<(), FnvSerError> {
        self.serialize_u64(u64::from(v))
    }
    fn serialize_u64(self, v: u64) -> Result<(), FnvSerError> {
        self.0.write_u8(3);
        self.0.write_u64(v);
        Ok(())
    }
    fn serialize_f32(self, v: f32) -> Result<(), FnvSerError> {
        self.serialize_f64(f64::from(v))
    }
    fn serialize_f64(self, v: f64) -> Result<(), FnvSerError> {
        self.0.write_u8(4);
        let bits = if v == 0.0 { 0u64 } else { v.to_bits() };
        self.0.write_u64(bits);
        Ok(())
    }
    fn serialize_char(self, v: char) -> Result<(), FnvSerError> {
        self.serialize_u32(v as u32)
    }
    fn serialize_str(self, v: &str) -> Result<(), FnvSerError> {
        self.0.write_u8(5);
        self.0.write_str(v);
        Ok(())
    }
    fn serialize_bytes(self, v: &[u8]) -> Result<(), FnvSerError> {
        self.0.write_u8(6);
        self.0.write_u64(v.len() as u64);
        self.0.write_bytes(v);
        Ok(())
    }
    fn serialize_none(self) -> Result<(), FnvSerError> {
        self.0.write_u8(7);
        Ok(())
    }
    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), FnvSerError> {
        self.0.write_u8(8);
        value.serialize(self)
    }
    fn serialize_unit(self) -> Result<(), FnvSerError> {
        self.0.write_u8(9);
        Ok(())
    }
    fn serialize_unit_struct(self, name: &'static str) -> Result<(), FnvSerError> {
        self.0.write_u8(10);
        self.0.write_str(name);
        Ok(())
    }
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> Result<(), FnvSerError> {
        self.0.write_u8(11);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        Ok(())
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        value.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), FnvSerError> {
        self.0.write_u8(12);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        value.serialize(self)
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(13);
        if let Some(n) = len {
            self.0.write_u64(n as u64);
        }
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple(self, len: usize) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(14);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(15);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(16);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_map(self, _len: Option<usize>) -> Result<FnvMapSer<'a>, FnvSerError> {
        self.0.write_u8(17);
        Ok(FnvMapSer {
            parent: self.0,
            len: 0,
            xor: 0,
            pending_key_hash: 0,
        })
    }
    fn serialize_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(18);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
    fn serialize_struct_variant(
        self,
        _name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<FnvSeqSer<'a>, FnvSerError> {
        self.0.write_u8(19);
        self.0.write_u32(variant_index);
        self.0.write_str(variant);
        self.0.write_u64(len as u64);
        Ok(FnvSeqSer(self.0))
    }
}

#[derive(Serialize)]
struct CorpGovernanceDomainRef<'a> {
    corp_governance: &'a HashMap<i64, CorpGovState>,
    employment_history: &'a HashMap<i64, Vec<(i64, u64, Option<u64>)>>,
    corp_hangar_divisions: &'a HashMap<i64, HashMap<u8, String>>,
    corp_alliance_history: &'a HashMap<i64, Vec<CorpAllianceHistoryEntry>>,
    corp_container_logs: &'a HashMap<i64, Vec<CorpContainerLogEntry>>,
    corp_medals: &'a HashMap<i64, Vec<CorpMedalEntry>>,
    corp_issued_medals: &'a HashMap<i64, Vec<CorpIssuedMedalEntry>>,
    corp_member_limits: &'a HashMap<i64, i64>,
    corp_titles: &'a HashMap<i64, Vec<CorpTitleEntry>>,
    corp_member_titles: &'a HashMap<(i64, i64), Vec<i64>>,
    corp_role_assignments: &'a HashMap<(i64, i64), CorpMemberRoleAssignment>,
    corp_role_history: &'a HashMap<i64, Vec<CorpRoleHistoryEntry>>,
    corp_shareholders: &'a HashMap<i64, Vec<CorpShareholderEntry>>,
    char_medals: &'a HashMap<i64, Vec<CharacterMedalRecord>>,
    char_scoped_roles: &'a HashMap<i64, CharacterScopedRolesRecord>,
    corp_vote_cases: &'a HashMap<i64, CorpVoteCaseRecord>,
    corp_vote_ballots: &'a HashMap<(i64, i64), CorpVoteBallotRecord>,
}

impl<'a> CorpGovernanceDomainRef<'a> {
    fn from_snapshot(snap: &'a WorldStateSnapshot) -> Self {
        Self {
            corp_governance: &snap.corp_governance,
            employment_history: &snap.employment_history,
            corp_hangar_divisions: &snap.corp_hangar_divisions,
            corp_alliance_history: &snap.corp_alliance_history,
            corp_container_logs: &snap.corp_container_logs,
            corp_medals: &snap.corp_medals,
            corp_issued_medals: &snap.corp_issued_medals,
            corp_member_limits: &snap.corp_member_limits,
            corp_titles: &snap.corp_titles,
            corp_member_titles: &snap.corp_member_titles,
            corp_role_assignments: &snap.corp_role_assignments,
            corp_role_history: &snap.corp_role_history,
            corp_shareholders: &snap.corp_shareholders,
            char_medals: &snap.char_medals,
            char_scoped_roles: &snap.char_scoped_roles,
            corp_vote_cases: &snap.corp_vote_cases,
            corp_vote_ballots: &snap.corp_vote_ballots,
        }
    }
}

struct UnorderedIntSetRef<'a>(&'a HashSet<i64>);

impl Serialize for UnorderedIntSetRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut xor = 0u64;
        for &v in self.0 {
            let mut eh = FnvHasher::new();
            eh.write_i64(v);
            xor ^= eh.finish();
        }
        let mut seq = serializer.serialize_seq(Some(2))?;
        seq.serialize_element(&(self.0.len() as u64))?;
        seq.serialize_element(&xor)?;
        seq.end()
    }
}

struct AclDeniedMapRef<'a>(&'a HashMap<i64, HashSet<i64>>);

impl Serialize for AclDeniedMapRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (&sid, set) in self.0 {
            map.serialize_entry(&sid, &UnorderedIntSetRef(set))?;
        }
        map.end()
    }
}

#[derive(Serialize)]
struct CrimewatchDomainRef<'a> {
    crimewatch_states: &'a HashMap<i64, PersistedCrimeWatchState>,
    crimewatch_aggressions: &'a HashMap<(i64, i64), PersistedAggressionRecord>,
    combat_crimewatch_state: &'a PersistedCombatCrimewatchState,
    wh_polarizations: &'a HashMap<(i64, i64), u64>,
    zarzakh_gate_locks: &'a HashMap<i64, (i64, u64)>,
    structure_docking_acl_denied: AclDeniedMapRef<'a>,
}

impl<'a> CrimewatchDomainRef<'a> {
    fn from_snapshot(snap: &'a WorldStateSnapshot) -> Self {
        Self {
            crimewatch_states: &snap.crimewatch_states,
            crimewatch_aggressions: &snap.crimewatch_aggressions,
            combat_crimewatch_state: &snap.combat_crimewatch_state,
            wh_polarizations: &snap.wh_polarizations,
            zarzakh_gate_locks: &snap.zarzakh_gate_locks,
            structure_docking_acl_denied: AclDeniedMapRef(&snap.structure_docking_acl_denied),
        }
    }
}

#[derive(Serialize)]
struct WarfareDomainRef<'a> {
    alliance_war_state: &'a PersistedAllianceWarState,
    war_state: &'a PersistedWarState,
    fw_runtime: &'a PersistedFwState,
    sov_runtime: &'a PersistedSovState,
    insurgency_runtime: &'a PersistedInsurgencyState,
    sov_prospecting_levels: &'a HashMap<SolarSystemId, u32>,
    sov_campaign_scores: &'a HashMap<i64, SovCampaignScoreRecord>,
    fw_character_stats: &'a HashMap<i64, FwCharacterStatsRecord>,
    fw_corp_enlisted_on: &'a HashMap<i64, u64>,
    fw_system_occupations: &'a HashMap<SolarSystemId, FwSystemOccupationRecord>,
    fw_wars: &'a Vec<(i64, i64)>,
    war_lifecycle_meta: &'a HashMap<i64, WarLifecycleMeta>,
    alliance_lifecycle_meta: &'a HashMap<i64, AllianceLifecycleMeta>,
}

impl<'a> WarfareDomainRef<'a> {
    fn from_snapshot(snap: &'a WorldStateSnapshot) -> Self {
        Self {
            alliance_war_state: &snap.alliance_war_state,
            war_state: &snap.war_state,
            fw_runtime: &snap.fw_runtime,
            sov_runtime: &snap.sov_runtime,
            insurgency_runtime: &snap.insurgency_runtime,
            sov_prospecting_levels: &snap.sov_prospecting_levels,
            sov_campaign_scores: &snap.sov_campaign_scores,
            fw_character_stats: &snap.fw_character_stats,
            fw_corp_enlisted_on: &snap.fw_corp_enlisted_on,
            fw_system_occupations: &snap.fw_system_occupations,
            fw_wars: &snap.fw_wars,
            war_lifecycle_meta: &snap.war_lifecycle_meta,
            alliance_lifecycle_meta: &snap.alliance_lifecycle_meta,
        }
    }
}

pub fn apply_d6_corp_crimewatch_migration(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(D6_SCHEMA_SQL)
        .map_err(|e| e.to_string())?;
    let now_unix = crate::clock::host_wall_unix_secs_and_nanos().0 as i64;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, name, applied_unix) VALUES (?1, ?2, ?3)",
        params![
            D6_CORP_CRIMEWATCH_SCHEMA_VERSION,
            "0004_d6_corp_crimewatch_warfare",
            now_unix
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn compute_d6_corp_crimewatch_fingerprints(
    snap: &WorldStateSnapshot,
) -> D6CorpCrimewatchFingerprints {
    D6CorpCrimewatchFingerprints {
        corp_governance_fp: canonical_fp(&CorpGovernanceDomainRef::from_snapshot(snap)),
        crimewatch_fp: canonical_fp(&CrimewatchDomainRef::from_snapshot(snap)),
        warfare_fp: canonical_fp(&WarfareDomainRef::from_snapshot(snap)),
    }
}

pub fn load_d6_corp_crimewatch(conn: &Connection, snap: &mut WorldStateSnapshot) {
    if let Ok(mut stmt) =
        conn.prepare("SELECT store_key, payload_json FROM d6_corp_crimewatch_kv")
    {
        if let Ok(rows) =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            for (key, js) in rows.flatten() {
                match key.as_str() {
                    KEY_CORP_GOV => {
                        if let Ok(p) = serde_json::from_str::<CorpGovernanceDomainPayload>(&js) {
                            p.apply_to_snapshot(snap);
                        }
                    }
                    KEY_CRIMEWATCH => {
                        if let Ok(p) = serde_json::from_str::<CrimewatchDomainPayload>(&js) {
                            p.apply_to_snapshot(snap);
                        }
                    }
                    KEY_WARFARE => {
                        if let Ok(p) = serde_json::from_str::<WarfareDomainPayload>(&js) {
                            p.apply_to_snapshot(snap);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

pub fn sync_d6_corp_crimewatch(
    tx: &Transaction<'_>,
    snap: &WorldStateSnapshot,
    fp: &mut TableFingerprints,
) -> Result<bool, String> {
    let mut changed = false;
    let dfp = &mut fp.d6_corp_crimewatch;

    let corp_h = canonical_fp(&CorpGovernanceDomainRef::from_snapshot(snap));
    if dfp.corp_governance_fp != corp_h {
        changed = true;
        let corp_payload = CorpGovernanceDomainPayload::from_snapshot(snap);
        let js = serde_json::to_string(&corp_payload).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES (?1, ?2)",
            params![KEY_CORP_GOV, js],
        )
        .map_err(|e| e.to_string())?;
        dfp.corp_governance_fp = corp_h;
    }

    let cw_h = canonical_fp(&CrimewatchDomainRef::from_snapshot(snap));
    if dfp.crimewatch_fp != cw_h {
        changed = true;
        let cw_payload = CrimewatchDomainPayload::from_snapshot(snap);
        let js = serde_json::to_string(&cw_payload).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES (?1, ?2)",
            params![KEY_CRIMEWATCH, js],
        )
        .map_err(|e| e.to_string())?;
        dfp.crimewatch_fp = cw_h;
    }

    let war_h = canonical_fp(&WarfareDomainRef::from_snapshot(snap));
    if dfp.warfare_fp != war_h {
        changed = true;
        let war_payload = WarfareDomainPayload::from_snapshot(snap);
        let js = serde_json::to_string(&war_payload).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT OR REPLACE INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES (?1, ?2)",
            params![KEY_WARFARE, js],
        )
        .map_err(|e| e.to_string())?;
        dfp.warfare_fp = war_h;
    }

    Ok(changed)
}

pub fn rollback_d6_corp_crimewatch(
    conn: &Connection,
    snap: &mut WorldStateSnapshot,
    fp: &TableFingerprints,
) {
    let dfp = &fp.d6_corp_crimewatch;

    if canonical_fp(&CorpGovernanceDomainRef::from_snapshot(snap)) != dfp.corp_governance_fp {
        let js: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM d6_corp_crimewatch_kv WHERE store_key = ?1",
                params![KEY_CORP_GOV],
                |r| r.get(0),
            )
            .ok();
        let payload = js
            .and_then(|s| serde_json::from_str::<CorpGovernanceDomainPayload>(&s).ok())
            .unwrap_or_default();
        payload.apply_to_snapshot(snap);
    }

    if canonical_fp(&CrimewatchDomainRef::from_snapshot(snap)) != dfp.crimewatch_fp {
        let js: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM d6_corp_crimewatch_kv WHERE store_key = ?1",
                params![KEY_CRIMEWATCH],
                |r| r.get(0),
            )
            .ok();
        let payload = js
            .and_then(|s| serde_json::from_str::<CrimewatchDomainPayload>(&s).ok())
            .unwrap_or_default();
        payload.apply_to_snapshot(snap);
    }

    if canonical_fp(&WarfareDomainRef::from_snapshot(snap)) != dfp.warfare_fp {
        let js: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM d6_corp_crimewatch_kv WHERE store_key = ?1",
                params![KEY_WARFARE],
                |r| r.get(0),
            )
            .ok();
        let payload = js
            .and_then(|s| serde_json::from_str::<WarfareDomainPayload>(&s).ok())
            .unwrap_or_default();
        payload.apply_to_snapshot(snap);
    }
}
