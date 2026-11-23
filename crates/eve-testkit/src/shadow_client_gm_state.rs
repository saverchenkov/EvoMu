//! Shadow client skill and GM attribute convergence helpers (`#1193`).
//!
//! Keeps [`super::ShadowClient`]'s skill state (`GetSkillHandler` /
//! `OnServerSkillsChanged` / `OnServerSkillsRemoved`) and tracked GM combat
//! attribute updates synchronized with authoritative `WorldStateSnapshot` state.

#![forbid(unsafe_code)]

use super::{ShadowClient, ShadowDivergence};
use eve_db::WorldStateSnapshot;
use eve_proto::PyRep;
use eve_sde::attr_ids;

impl ShadowClient {
    pub fn sync_from_get_skills(&mut self, skills_rsp: &PyRep) {
        self.skills.clear();
        self.skills_synced = true;
        match skills_rsp {
            PyRep::Dict(entries) => {
                for (k, v) in entries {
                    let type_id = k
                        .as_i64()
                        .or_else(|| v.dict_get("typeID").and_then(|x| x.as_i64()))
                        .and_then(|id| u32::try_from(id).ok());
                    if let (Some(tid), Some(skill_entry)) = (type_id, parse_skill_info_entry(v)) {
                        self.skills.insert(tid, skill_entry);
                    }
                }
            }
            PyRep::Rowset { columns, rows } => {
                let tid_ix = columns.iter().position(|c| c == "typeID").unwrap_or(0);
                let lvl_ix = columns
                    .iter()
                    .position(|c| c == "skillLevel" || c == "trainedSkillLevel")
                    .unwrap_or(1);
                let sp_ix = columns
                    .iter()
                    .position(|c| c == "skillPoints" || c == "trainedSkillPoints")
                    .unwrap_or(2);
                for row in rows {
                    let Some(tid) = row
                        .get(tid_ix)
                        .and_then(|v| v.as_i64())
                        .and_then(|id| u32::try_from(id).ok())
                    else {
                        continue;
                    };
                    let lvl = row
                        .get(lvl_ix)
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                        .clamp(0, 5) as u8;
                    let sp = row
                        .get(sp_ix)
                        .and_then(|v| v.as_f64())
                        .filter(|v| v.is_finite())
                        .unwrap_or(0.0);
                    if lvl > 0 && sp >= 0.0 {
                        self.skills.insert(tid, (lvl, sp));
                    }
                }
            }
            _ => {}
        }
    }

    pub fn apply_on_server_skills_changed(&mut self, args: &[PyRep]) {
        self.apply_skills_dict_delta(args);
    }

    pub fn apply_on_server_skills_removed(&mut self, args: &[PyRep]) {
        self.apply_skills_dict_delta(args);
    }

    pub fn compare_skills_against_snapshot(&self, snap: &WorldStateSnapshot) -> Vec<ShadowDivergence> {
        let mut out = Vec::new();
        if !self.skills_synced {
            return out;
        }
        let world_skills = snap.skills.get(&self.char_id);
        if let Some(skills_map) = world_skills {
            for (&tid, rec) in skills_map {
                if rec.level == 0 {
                    continue;
                }
                match self.skills.get(&tid) {
                    None => out.push(ShadowDivergence::MissingShadowSkill {
                        type_id: tid,
                        world_level: rec.level,
                    }),
                    Some(&(sh_lvl, _)) if sh_lvl != rec.level => {
                        out.push(ShadowDivergence::SkillLevelMismatch {
                            type_id: tid,
                            shadow_level: sh_lvl,
                            world_level: rec.level,
                        });
                    }
                    Some(_) => {}
                }
            }
        }
        for (&tid, &(sh_lvl, _)) in &self.skills {
            let world_lvl = world_skills
                .and_then(|m| m.get(&tid))
                .map_or(0, |r| r.level);
            if world_lvl == 0 {
                out.push(ShadowDivergence::StaleShadowSkill {
                    type_id: tid,
                    shadow_level: sh_lvl,
                });
            }
        }
        out
    }

    pub fn compare_item_attributes_against_snapshot(
        &self,
        snap: &WorldStateSnapshot,
    ) -> Vec<ShadowDivergence> {
        let mut out = Vec::new();
        for (&(item_id, attr_id), &shadow_value) in &self.item_attributes {
            let Some(item) = snap.items.get(&item_id) else {
                continue;
            };
            let world_opt = match attr_id {
                attr_ids::DAMAGE | attr_ids::ARMOR_DAMAGE => item
                    .dynamic_attributes
                    .get(&attr_id)
                    .copied()
                    .or_else(|| {
                        (!snap.sp6_lifecycle.ship_combat_hp.contains_key(&item_id)).then_some(0.0)
                    }),
                attr_ids::SHIELD_CHARGE => item
                    .dynamic_attributes
                    .get(&attr_id)
                    .copied()
                    .or_else(|| {
                        snap.sp6_lifecycle
                            .ship_combat_hp
                            .get(&item_id)
                            .map(|hp| hp.shield_hp)
                    }),
                18 => item.dynamic_attributes.get(&attr_id).copied(),
                _ => None,
            };
            if let Some(world_value) = world_opt {
                if (shadow_value - world_value).abs() > 1e-2 {
                    out.push(ShadowDivergence::ItemAttributeMismatch {
                        item_id,
                        attr_id,
                        shadow_value,
                        world_value,
                    });
                }
            }
        }
        out
    }

    pub(super) fn sync_gm_skills_from_snapshot(&mut self, snap: &WorldStateSnapshot) {
        self.skills.clear();
        self.skills_synced = true;
        self.item_attributes.clear();
        if let Some(ch_skills) = snap.skills.get(&self.char_id) {
            for (&type_id, rec) in ch_skills {
                if rec.level > 0 {
                    self.skills.insert(type_id, (rec.level, rec.skill_points));
                }
            }
        }
    }

    pub(super) fn record_combat_item_attribute(&mut self, item_id: i64, attr_id: i64, new_val: f64) {
        if let Ok(attr_u16) = u16::try_from(attr_id) {
            if new_val.is_finite()
                && matches!(
                    attr_u16,
                    attr_ids::DAMAGE | 18 | attr_ids::SHIELD_CHARGE | attr_ids::ARMOR_DAMAGE
                )
            {
                self.item_attributes.insert((item_id, attr_u16), new_val);
            }
        }
    }

    fn apply_skills_dict_delta(&mut self, args: &[PyRep]) {
        let Some(PyRep::Dict(entries)) = args.first() else {
            return;
        };
        for (k, v) in entries {
            let Some(tid) = k
                .as_i64()
                .or_else(|| v.dict_get("typeID").and_then(|x| x.as_i64()))
                .and_then(|id| u32::try_from(id).ok())
            else {
                continue;
            };
            if let Some(entry) = parse_skill_info_entry(v) {
                self.skills.insert(tid, entry);
            } else {
                self.skills.remove(&tid);
            }
        }
    }
}

fn parse_skill_info_entry(v: &PyRep) -> Option<(u8, f64)> {
    if matches!(v, PyRep::None) {
        return None;
    }
    let lvl = v
        .dict_get("skillLevel")
        .or_else(|| v.dict_get("trainedSkillLevel"))
        .or_else(|| v.dict_get("effectiveSkillLevel"))
        .and_then(|x| x.as_i64())?;
    let sp = v
        .dict_get("skillPoints")
        .or_else(|| v.dict_get("trainedSkillPoints"))
        .and_then(|x| x.as_f64())
        .filter(|x| x.is_finite())
        .unwrap_or(0.0);
    if lvl <= 0 || sp < 0.0 {
        return None;
    }
    Some((lvl.clamp(0, 5) as u8, sp))
}
