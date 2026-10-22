//! On-demand domain loaders for V24.01 `.fsdbinary` (`cfsd`) resources (`T-2`, `D-2`).
//!
//! Exposes typed domain tables through [`SdeRegistry`] for agents in space, NPC characters,
//! corporation activities, NPC corporation divisions, contraband types, dogma units, accounting
//! entry types and keys, meta groups, graphic IDs, icon IDs, and sound IDs.

#![forbid(unsafe_code)]

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInSpaceInfo {
    pub agent_id: u32,
    pub dungeon_id: u32,
    pub solar_system_id: SolarSystemId,
    pub spawn_point_id: u32,
    pub type_id: TypeId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcCharacterRecord {
    pub character_id: i64,
    pub corporation_id: i64,
    pub race_id: Option<u32>,
    pub bloodline_id: u32,
    pub ancestry_id: Option<u32>,
    pub station_id: Option<i64>,
    pub name_id: u32,
    pub is_agent: bool,
    pub agent_level: Option<u32>,
    pub agent_division_id: Option<u32>,
    pub agent_type_id: Option<u32>,
    pub is_locator: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorporationActivityInfo {
    pub activity_id: u32,
    pub name_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NpcCorporationDivisionInfo {
    pub division_id: u32,
    pub internal_name: String,
    pub description: Option<String>,
    pub name_id: u32,
    pub description_id: Option<u32>,
    pub leader_type_name_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ContrabandFactionRule {
    pub faction_id: u32,
    pub attack_min_sec: f32,
    pub confiscate_min_sec: f32,
    pub fine_by_value: f32,
    pub standing_loss: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DogmaUnitInfo {
    pub unit_id: u32,
    pub name: String,
    pub display_name_id: Option<u32>,
    pub description_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingEntryTypeInfo {
    pub entry_type_id: u32,
    pub name: String,
    pub journal_message: String,
    pub name_id: u32,
    pub journal_message_id: u32,
    pub description_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountingKeyInfo {
    pub key_id: u32,
    pub key_name: String,
    pub key_type: String,
    pub currency: Option<String>,
    pub description: Option<String>,
    pub name_id: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MetaGroupInfo {
    pub meta_group_id: u32,
    pub name_id: u32,
    pub description_id: Option<u32>,
    pub icon_id: Option<u32>,
    pub color: Option<[f32; 4]>,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphicIdInfo {
    pub graphic_id: u32,
    pub graphic_file: Option<String>,
    pub icon_folder: Option<String>,
    pub sof_faction_name: Option<String>,
    pub sof_hull_name: Option<String>,
    pub sof_race_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IconIdInfo {
    pub icon_id: u32,
    pub icon_file: String,
    pub description: String,
    pub obsolete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoundIdInfo {
    pub sound_id: u32,
    pub wwise_event: String,
}

fn u32_le(buf: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(buf[pos..pos + 4].try_into().expect("u32 slice"))
}

fn u64_le(buf: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(buf[pos..pos + 8].try_into().expect("u64 slice"))
}

fn f32_le(buf: &[u8], pos: usize) -> f32 {
    f32::from_le_bytes(buf[pos..pos + 4].try_into().expect("f32 slice"))
}

fn cfsd_str(buf: &[u8], rel: u64) -> String {
    if rel == 0 {
        return String::new();
    }
    let abs = 32 + rel as usize;
    let len = u64_le(buf, abs - 8) as usize;
    std::str::from_utf8(&buf[abs..abs + len])
        .expect("cfsd UTF-8 string")
        .to_string()
}

fn cfsd_opt_str(buf: &[u8], rel: u64) -> Option<String> {
    (rel != 0).then(|| cfsd_str(buf, rel))
}

fn cfsd_records(raw: &[u8], stride: usize) -> Vec<usize> {
    let cnt = u64_le(raw, 40) as usize;
    let buckets = u64_le(raw, 48) as usize;
    let mut out = Vec::with_capacity(cnt);
    for b in 0..buckets {
        let boff = u64_le(raw, 56 + b * 8) as usize;
        if boff == 0 {
            continue;
        }
        let abs = 32 + boff;
        let n = u64_le(raw, abs - 8) as usize;
        for i in 0..n {
            out.push(abs + i * stride);
        }
    }
    out
}

impl SdeRegistry {
    pub fn read_cfsd_bytes(&self, res_path: &str, stride: usize) -> Result<(Vec<u8>, Vec<usize>), SdeError> {
        let path = self.cdn_resource_path(res_path)?;
        let raw = std::fs::read(&path)
            .map_err(|_| SdeError::MissingDatabase(path.display().to_string()))?;
        let recs = cfsd_records(&raw, stride);
        Ok((raw, recs))
    }

    /// Loads `res:/staticdata/agentsinspace.fsdbinary` (`360` agents in space, stride `20`).
    pub fn load_agents_in_space(&self) -> Result<HashMap<u32, AgentInSpaceInfo>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/agentsinspace.fsdbinary", 20)?;
        let mut out = HashMap::with_capacity(recs.len());
        for rec in recs {
            let aid = u32_le(&raw, rec);
            out.insert(
                aid,
                AgentInSpaceInfo {
                    agent_id: aid,
                    dungeon_id: u32_le(&raw, rec + 4),
                    solar_system_id: u32_le(&raw, rec + 8),
                    spawn_point_id: u32_le(&raw, rec + 12),
                    type_id: u32_le(&raw, rec + 16),
                },
            );
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/npccharacters.fsdbinary` (`11,424` NPC characters, stride `136`).
    pub fn load_npc_characters(&self) -> Result<HashMap<i64, NpcCharacterRecord>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/npccharacters.fsdbinary", 136)?;
        let mut out = HashMap::with_capacity(recs.len());
        for rec in recs {
            let cid = u64_le(&raw, rec) as i64;
            let mask = u32_le(&raw, rec + 128);
            let is_agent = (mask & 1) != 0;
            out.insert(
                cid,
                NpcCharacterRecord {
                    character_id: cid,
                    corporation_id: i64::from(u32_le(&raw, rec + 80)),
                    race_id: ((mask & 128) != 0).then(|| u32_le(&raw, rec + 100)),
                    bloodline_id: u32_le(&raw, rec + 68),
                    ancestry_id: ((mask & 2) != 0).then(|| u32_le(&raw, rec + 64)),
                    station_id: ((mask & 256) != 0).then(|| i64::from(u32_le(&raw, rec + 112))),
                    name_id: u32_le(&raw, rec + 92),
                    is_agent,
                    agent_division_id: is_agent.then(|| u32_le(&raw, rec + 16)),
                    agent_level: is_agent.then(|| u32_le(&raw, rec + 20)),
                    agent_type_id: is_agent.then(|| u32_le(&raw, rec + 24)),
                    is_locator: is_agent && raw[rec + 28] != 0,
                },
            );
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/corporationactivities.fsdbinary` (`20` corporation activities,
    /// stride `8`).
    pub fn load_corporation_activities(
        &self,
    ) -> Result<HashMap<u32, CorporationActivityInfo>, SdeError> {
        let (raw, recs) =
            self.read_cfsd_bytes("res:/staticdata/corporationactivities.fsdbinary", 8)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u32_le(&raw, rec);
                (
                    id,
                    CorporationActivityInfo {
                        activity_id: id,
                        name_id: u32_le(&raw, rec + 4),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/npccorporationdivisions.fsdbinary` (`10` NPC corporation divisions,
    /// stride `40`).
    pub fn load_npc_corporation_divisions(
        &self,
    ) -> Result<HashMap<u32, NpcCorporationDivisionInfo>, SdeError> {
        let (raw, recs) =
            self.read_cfsd_bytes("res:/staticdata/npccorporationdivisions.fsdbinary", 40)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u64_le(&raw, rec) as u32;
                let mask = u32_le(&raw, rec + 36);
                (
                    id,
                    NpcCorporationDivisionInfo {
                        division_id: id,
                        description: ((mask & 1) != 0)
                            .then(|| cfsd_str(&raw, u64_le(&raw, rec + 8))),
                        internal_name: cfsd_str(&raw, u64_le(&raw, rec + 16)),
                        description_id: ((mask & 2) != 0).then(|| u32_le(&raw, rec + 24)),
                        leader_type_name_id: u32_le(&raw, rec + 28),
                        name_id: u32_le(&raw, rec + 32),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/contrabandtypes.fsdbinary` (`8` contraband types mapped to
    /// per-faction enforcement rules, stride `24`).
    pub fn load_contraband_types(
        &self,
    ) -> Result<HashMap<TypeId, HashMap<u32, ContrabandFactionRule>>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/contrabandtypes.fsdbinary", 24)?;
        let mut out = HashMap::with_capacity(recs.len());
        for rec in recs {
            let tid = u64_le(&raw, rec) as TypeId;
            let d_rel = u64_le(&raw, rec + 8) as usize;
            let d_len = u64_le(&raw, rec + 16) as usize;
            let abs = 32 + d_rel;
            let buckets = u64_le(&raw, abs - 8) as usize;
            let mut rules = HashMap::with_capacity(d_len);
            for b in 0..buckets {
                let boff = u64_le(&raw, abs + b * 8) as usize;
                if boff == 0 {
                    continue;
                }
                let babs = 32 + boff;
                let bn = u64_le(&raw, babs - 8) as usize;
                for j in 0..bn {
                    let e = babs + j * 20;
                    let fid = u32_le(&raw, e);
                    rules.insert(
                        fid,
                        ContrabandFactionRule {
                            faction_id: fid,
                            attack_min_sec: f32_le(&raw, e + 4),
                            confiscate_min_sec: f32_le(&raw, e + 8),
                            fine_by_value: f32_le(&raw, e + 12),
                            standing_loss: f32_le(&raw, e + 16),
                        },
                    );
                }
            }
            out.insert(tid, rules);
        }
        Ok(out)
    }

    /// Loads `res:/staticdata/dogmaunits.fsdbinary` (`60` dogma units, stride `32`).
    pub fn load_dogma_units(&self) -> Result<HashMap<u32, DogmaUnitInfo>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/dogmaunits.fsdbinary", 32)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u64_le(&raw, rec) as u32;
                let mask = u32_le(&raw, rec + 24);
                (
                    id,
                    DogmaUnitInfo {
                        unit_id: id,
                        name: cfsd_str(&raw, u64_le(&raw, rec + 8)),
                        description_id: ((mask & 1) != 0).then(|| u32_le(&raw, rec + 16)),
                        display_name_id: ((mask & 2) != 0).then(|| u32_le(&raw, rec + 20)),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/accountingentrytypes.fsdbinary` (`218` wallet journal entry types,
    /// stride `40`).
    pub fn load_accounting_entry_types(
        &self,
    ) -> Result<HashMap<u32, AccountingEntryTypeInfo>, SdeError> {
        let (raw, recs) =
            self.read_cfsd_bytes("res:/staticdata/accountingentrytypes.fsdbinary", 40)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u64_le(&raw, rec) as u32;
                let mask = u32_le(&raw, rec + 36);
                (
                    id,
                    AccountingEntryTypeInfo {
                        entry_type_id: id,
                        journal_message: cfsd_str(&raw, u64_le(&raw, rec + 8)),
                        name: cfsd_str(&raw, u64_le(&raw, rec + 16)),
                        description_id: ((mask & 1) != 0).then(|| u32_le(&raw, rec + 24)),
                        journal_message_id: u32_le(&raw, rec + 28),
                        name_id: u32_le(&raw, rec + 32),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/accountingkeys.fsdbinary` (`19` wallet accounting keys, stride `48`).
    pub fn load_accounting_keys(&self) -> Result<HashMap<u32, AccountingKeyInfo>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/accountingkeys.fsdbinary", 48)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u64_le(&raw, rec) as u32;
                let mask = u32_le(&raw, rec + 44);
                (
                    id,
                    AccountingKeyInfo {
                        key_id: id,
                        currency: cfsd_opt_str(&raw, u64_le(&raw, rec + 8)),
                        description: ((mask & 1) != 0)
                            .then(|| cfsd_str(&raw, u64_le(&raw, rec + 16))),
                        key_name: cfsd_str(&raw, u64_le(&raw, rec + 24)),
                        key_type: cfsd_str(&raw, u64_le(&raw, rec + 32)),
                        name_id: u32_le(&raw, rec + 40),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/metagroups.fsdbinary` (`13` meta groups, stride `48`).
    pub fn load_meta_groups(&self) -> Result<HashMap<u32, MetaGroupInfo>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/metagroups.fsdbinary", 48)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u64_le(&raw, rec) as u32;
                let mask = u32_le(&raw, rec + 44);
                (
                    id,
                    MetaGroupInfo {
                        meta_group_id: id,
                        description: ((mask & 2) != 0)
                            .then(|| cfsd_str(&raw, u64_le(&raw, rec + 8))),
                        color: ((mask & 1) != 0).then(|| {
                            [
                                f32_le(&raw, rec + 16),
                                f32_le(&raw, rec + 20),
                                f32_le(&raw, rec + 24),
                                f32_le(&raw, rec + 28),
                            ]
                        }),
                        description_id: ((mask & 4) != 0).then(|| u32_le(&raw, rec + 32)),
                        icon_id: ((mask & 8) != 0).then(|| u32_le(&raw, rec + 36)),
                        name_id: u32_le(&raw, rec + 40),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/graphicids.fsdbinary` (`6,159` graphic definitions, stride `160`).
    pub fn load_graphic_ids(&self) -> Result<HashMap<u32, GraphicIdInfo>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/graphicids.fsdbinary", 160)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u64_le(&raw, rec) as u32;
                (
                    id,
                    GraphicIdInfo {
                        graphic_id: id,
                        graphic_file: cfsd_opt_str(&raw, u64_le(&raw, rec + 40)),
                        icon_folder: cfsd_opt_str(&raw, u64_le(&raw, rec + 48)),
                        sof_faction_name: cfsd_opt_str(&raw, u64_le(&raw, rec + 64)),
                        sof_hull_name: cfsd_opt_str(&raw, u64_le(&raw, rec + 72)),
                        sof_race_name: cfsd_opt_str(&raw, u64_le(&raw, rec + 88)),
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/iconids.fsdbinary` (`4,804` icon definitions, stride `32`).
    pub fn load_icon_ids(&self) -> Result<HashMap<u32, IconIdInfo>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/iconids.fsdbinary", 32)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u64_le(&raw, rec) as u32;
                (
                    id,
                    IconIdInfo {
                        icon_id: id,
                        icon_file: cfsd_str(&raw, u64_le(&raw, rec + 8)),
                        description: cfsd_str(&raw, u64_le(&raw, rec + 16)),
                        obsolete: raw[rec + 24] != 0,
                    },
                )
            })
            .collect())
    }

    /// Loads `res:/staticdata/soundids.fsdbinary` (`1,470` sound definitions, stride `16`).
    pub fn load_sound_ids(&self) -> Result<HashMap<u32, SoundIdInfo>, SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/soundids.fsdbinary", 16)?;
        Ok(recs
            .into_iter()
            .map(|rec| {
                let id = u64_le(&raw, rec) as u32;
                (
                    id,
                    SoundIdInfo {
                        sound_id: id,
                        wwise_event: cfsd_str(&raw, u64_le(&raw, rec + 8)),
                    },
                )
            })
            .collect())
    }

    /// Directly decodes `res:/staticdata/types.fsdbinary` (`53,288` item types, stride `152`) from
    /// the raw client CFSD binary file, returning `(raw_byte_size, records_by_type_id)`.
    pub fn load_raw_types_fsdbinary(&self) -> Result<(usize, HashMap<TypeId, RawCfsdTypeRecord>), SdeError> {
        let (raw, recs) = self.read_cfsd_bytes("res:/staticdata/types.fsdbinary", 152)?;
        let f64_at = |pos: usize| f64::from_le_bytes(raw[pos..pos + 8].try_into().expect("f64 slice"));
        let i64_at = |pos: usize| i64::from_le_bytes(raw[pos..pos + 8].try_into().expect("i64 slice"));
        let i32_at = |pos: usize| i32::from_le_bytes(raw[pos..pos + 4].try_into().expect("i32 slice"));
        let mut out = HashMap::with_capacity(recs.len());
        for rec in recs {
            let tid = u64_le(&raw, rec) as TypeId;
            let mask = u32_le(&raw, rec + 148);
            out.insert(
                tid,
                RawCfsdTypeRecord {
                    type_id: tid,
                    group_id: i32_at(rec + 84) as GroupId,
                    base_price: if (mask & (1 << 0)) != 0 { f64_at(rec + 8) } else { 0.0 },
                    capacity: if (mask & (1 << 1)) != 0 { f64_at(rec + 16) } else { 0.0 },
                    mass: if (mask & (1 << 11)) != 0 { f64_at(rec + 32) } else { 0.0 },
                    portion_size: i64_at(rec + 40).max(1) as u64,
                    radius: if (mask & (1 << 19)) != 0 { f64_at(rec + 48) } else { 0.0 },
                    volume: if (mask & (1 << 23)) != 0 { f64_at(rec + 56) } else { 0.0 },
                    published: raw[rec + 145] != 0,
                    description_id: ((mask & (1 << 3)) != 0).then(|| i32_at(rec + 68) as u32),
                    faction_id: ((mask & (1 << 5)) != 0).then(|| i32_at(rec + 72) as u32),
                    graphic_id: ((mask & (1 << 6)) != 0).then(|| i32_at(rec + 80) as u32),
                    icon_id: ((mask & (1 << 7)) != 0).then(|| i32_at(rec + 88) as u32),
                    market_group_id: ((mask & (1 << 10)) != 0).then(|| i32_at(rec + 96) as u32),
                    meta_group_id: ((mask & (1 << 12)) != 0).then(|| i32_at(rec + 100) as u32),
                    meta_level: ((mask & (1 << 13)) != 0).then(|| i64::from(i32_at(rec + 104))),
                    race_id: ((mask & (1 << 18)) != 0).then(|| i32_at(rec + 116) as u32),
                    sound_id: ((mask & (1 << 20)) != 0).then(|| i32_at(rec + 120) as u32),
                    tech_level: ((mask & (1 << 21)) != 0).then(|| i32_at(rec + 124) as u32),
                    variation_parent_type_id: ((mask & (1 << 22)) != 0).then(|| i32_at(rec + 136) as TypeId),
                    wreck_type_id: ((mask & (1 << 24)) != 0).then(|| i32_at(rec + 140) as TypeId),
                    type_name_id: u32_le(&raw, rec + 132),
                },
            );
        }
        Ok((raw.len(), out))
    }
}

/// Decoded 152-byte record from `res:/staticdata/types.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawCfsdTypeRecord {
    pub type_id: TypeId,
    pub group_id: GroupId,
    pub base_price: f64,
    pub capacity: f64,
    pub mass: f64,
    pub portion_size: u64,
    pub radius: f64,
    pub volume: f64,
    pub published: bool,
    pub description_id: Option<u32>,
    pub faction_id: Option<u32>,
    pub graphic_id: Option<u32>,
    pub icon_id: Option<u32>,
    pub market_group_id: Option<u32>,
    pub meta_group_id: Option<u32>,
    pub meta_level: Option<i64>,
    pub race_id: Option<u32>,
    pub sound_id: Option<u32>,
    pub tech_level: Option<u32>,
    pub variation_parent_type_id: Option<TypeId>,
    pub wreck_type_id: Option<TypeId>,
    pub type_name_id: u32,
}
