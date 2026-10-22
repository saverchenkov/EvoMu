use crate::*;
use eve_sde::{AttributeId, SdeRegistry, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

impl ShipFittingEngine {
    /// Fits a module into a slot with the specified online state (`online == true` checks CPU & PG,
    /// `online == false` fits into `ModuleState::Offline` without CPU/PG checks).
    pub fn fit_module_with_state(
        &mut self,
        module_item_id: i64,
        module_type_id: TypeId,
        flag_id: u16,
        online: bool,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        let mod_type = sde
            .types
            .get(&module_type_id)
            .ok_or_else(|| format!("Unknown module type {module_type_id}"))?;
        let hull_type = sde
            .types
            .get(&self.ship.type_id)
            .ok_or_else(|| format!("Unknown ship hull type {}", self.ship.type_id))?;

        if mod_type.category_id != 7 && mod_type.category_id != 32 {
            return Err(format!(
                "Item {} (type {}, category {}) is not a fittable module or subsystem",
                mod_type.name, module_type_id, mod_type.category_id
            ));
        }
        if mod_type.effect_ids.is_empty() {
            return Err(format!(
                "Module {} (type {}) has no fitting slot effect_ids defined",
                mod_type.name, module_type_id
            ));
        }

        if self
            .modules
            .values()
            .any(|m| m.flag_id == flag_id && m.item_id != module_item_id)
        {
            return Err(format!("Fitting slot {flag_id} is already occupied"));
        }

        // Validate subsystem slot (attribute 1366: subSystemSlot)
        if let Some(&req_slot_f) = mod_type.default_attributes.get(&1366) {
            let req_slot = req_slot_f as u16;
            if flag_id != req_slot {
                return Err(format!(
                    "SubsystemSlotMismatch: subsystem {} (type {}) requires slot {req_slot}, cannot fit to flag {flag_id}",
                    mod_type.name, module_type_id
                ));
            }
        }

        // Unconditionally validate module slot-type effect against target flag range
        let (req_eff, eff_name) = match flag_id {
            11..=18 => (11_u16, "loPower"),
            19..=26 => (13_u16, "medPower"),
            27..=34 => (12_u16, "hiPower"),
            92..=94 => (2663_u16, "rigSlot"),
            125..=132 => (3772_u16, "subSystem"),
            _ => return Err(format!("Invalid fitting slot flag {flag_id}")),
        };
        if !mod_type.effect_ids.contains(&req_eff) {
            return Err(format!(
                "Module {} (type {}) lacks required slot effect {} ({eff_name}) for flag {flag_id}",
                mod_type.name, module_type_id, req_eff
            ));
        }

        // Unconditionally validate slot index against ship low/med/high/rig/subsystem slot capacity on the hull
        let (slot_idx, max_slots) = match flag_id {
            11..=18 => (
                flag_id - 11,
                self.ship.get_attribute(attr_ids::LOW_SLOTS, sde).max(0.0) as u16,
            ),
            19..=26 => (
                flag_id - 19,
                self.ship.get_attribute(attr_ids::MED_SLOTS, sde).max(0.0) as u16,
            ),
            27..=34 => (
                flag_id - 27,
                self.ship.get_attribute(attr_ids::HI_SLOTS, sde).max(0.0) as u16,
            ),
            92..=94 => (
                flag_id - 92,
                self.ship.get_attribute(attr_ids::RIG_SLOTS, sde).max(0.0) as u16,
            ),
            125..=132 => (
                flag_id - 125,
                if hull_type.group_id == 963 { 4 } else { 0 },
            ),
            _ => unreachable!(),
        };
        if slot_idx >= max_slots {
            return Err(format!(
                "Slot flag {flag_id} (index {slot_idx}) exceeds hull slot capacity ({max_slots}) on {}",
                hull_type.name
            ));
        }

        // Unconditionally validate turret (effect 42) and launcher (effect 40) hardpoint limits on the ship hull
        if mod_type.effect_ids.contains(&42) {
            let max_turrets = self
                .ship
                .get_attribute(attr_ids::TURRET_SLOTS_LEFT, sde)
                .max(0.0) as usize;
            let fitted_turrets = self
                .modules
                .values()
                .filter(|m| {
                    m.item_id != module_item_id
                        && sde
                            .types
                            .get(&m.type_id)
                            .map(|t| t.effect_ids.contains(&42))
                            .unwrap_or(false)
                })
                .count();
            if fitted_turrets >= max_turrets {
                return Err(format!(
                    "Insufficient turret hardpoints on {}: {fitted_turrets} fitted, hull limit is {max_turrets}",
                    hull_type.name
                ));
            }
        }
        if mod_type.effect_ids.contains(&40) {
            let max_launchers = self
                .ship
                .get_attribute(attr_ids::LAUNCHER_SLOTS_LEFT, sde)
                .max(0.0) as usize;
            let fitted_launchers = self
                .modules
                .values()
                .filter(|m| {
                    m.item_id != module_item_id
                        && sde
                            .types
                            .get(&m.type_id)
                            .map(|t| t.effect_ids.contains(&40))
                            .unwrap_or(false)
                })
                .count();
            if fitted_launchers >= max_launchers {
                return Err(format!(
                    "Insufficient launcher hardpoints on {}: {fitted_launchers} fitted, hull limit is {max_launchers}",
                    hull_type.name
                ));
            }
        }

        // Validate maxGroupFitted (attribute 1544)
        if let Some(&limit_f) = mod_type.default_attributes.get(&1544) {
            let limit = limit_f as usize;
            let group_fitted = self
                .modules
                .values()
                .filter(|m| m.item_id != module_item_id)
                .filter(|m| {
                    sde.types
                        .get(&m.type_id)
                        .map(|t| t.group_id == mod_type.group_id)
                        .unwrap_or(false)
                })
                .count();
            if group_fitted >= limit {
                return Err(format!(
                    "MaxGroupFittedExceeded: module {} (group {}) limit is {limit}, already fitted {group_fitted}",
                    mod_type.name, mod_type.group_id
                ));
            }
        }

        // Validate maxTypeFitted (attribute 2431)
        if let Some(&limit_f) = mod_type.default_attributes.get(&2431) {
            let limit = limit_f as usize;
            let type_fitted = self
                .modules
                .values()
                .filter(|m| m.item_id != module_item_id && m.type_id == module_type_id)
                .count();
            if type_fitted >= limit {
                return Err(format!(
                    "MaxTypeFittedExceeded: module {} (type {}) limit is {limit}, already fitted {type_fitted}",
                    mod_type.name, module_type_id
                ));
            }
        }

        // Validate maxGroupOnline (attribute 978 or group 46) if online
        if online {
            let mut max_group_online = mod_type
                .default_attributes
                .get(&978)
                .copied()
                .or_else(|| if mod_type.group_id == 46 { Some(1.0) } else { None });
            if mod_type.group_id == 1770 {
                if let Some(ref mut limit) = max_group_online {
                    let rig_bonus: f64 = self
                        .modules
                        .values()
                        .filter(|m| m.item_id != module_item_id && m.state != ModuleState::Offline)
                        .filter_map(|m| sde.types.get(&m.type_id))
                        .filter(|t| t.effect_ids.contains(&6766))
                        .map(|t| t.default_attributes.get(&2542).copied().unwrap_or(1.0))
                        .sum();
                    *limit += rig_bonus;
                }
            }
            if let Some(limit) = max_group_online {
                let online_count = self
                    .modules
                    .values()
                    .filter(|m| m.item_id != module_item_id && m.state != ModuleState::Offline)
                    .filter(|m| {
                        sde.types
                            .get(&m.type_id)
                            .map(|t| t.group_id == mod_type.group_id)
                            .unwrap_or(false)
                    })
                    .count();
                if (online_count as f64) >= limit {
                    return Err(format!(
                        "MaxGroupOnlineExceeded: module {} (group {}) limit is {limit}",
                        mod_type.name, mod_type.group_id
                    ));
                }
            }
        }

        // Validate hull group/type restrictions (canFitShipGroup01..20, canFitShipType1..12)
        let group_attrs = [
            1298_u16, 1299, 1300, 1301, 1872, 1879, 1880, 1881, 2065, 2396, 2476, 2477, 2478,
            2479, 2480, 2481, 2482, 2483, 2484, 2485,
        ];
        let type_attrs = [
            1302_u16, 1303, 1304, 1305, 1380, 1944, 2103, 2463, 2486, 2487, 2488, 2758, 5948,
        ];
        let allowed_groups: Vec<u32> = group_attrs
            .into_iter()
            .filter_map(|aid| mod_type.default_attributes.get(&aid).copied())
            .filter(|&v| v > 0.0)
            .map(|v| v as u32)
            .collect();
        let allowed_types: Vec<TypeId> = type_attrs
            .into_iter()
            .filter_map(|aid| mod_type.default_attributes.get(&aid).copied())
            .filter(|&v| v > 0.0)
            .map(|v| v as TypeId)
            .collect();
        if (!allowed_groups.is_empty() || !allowed_types.is_empty())
            && !allowed_groups.contains(&hull_type.group_id)
            && !allowed_types.contains(&self.ship.type_id)
        {
            return Err(format!(
                "Module {} (type {}) cannot be fitted to hull {} (type {}, group {}): restricted to groups {:?} or types {:?}",
                mod_type.name,
                module_type_id,
                hull_type.name,
                self.ship.type_id,
                hull_type.group_id,
                allowed_groups,
                allowed_types
            ));
        }

        // Validate rigSize (1547) on rigs against hull rigSize
        let hull_rig_size = self.ship.get_attribute(attr_ids::RIG_SIZE, sde).max(0.0) as u32;
        if mod_type.effect_ids.contains(&2663)
            || mod_type.default_attributes.contains_key(&attr_ids::RIG_SIZE)
        {
            let mod_rig_size = mod_type
                .default_attributes
                .get(&attr_ids::RIG_SIZE)
                .copied()
                .unwrap_or(0.0) as u32;
            if mod_rig_size == 0 || mod_rig_size != hull_rig_size {
                return Err(format!(
                    "Rig size mismatch for {}: module rigSize {mod_rig_size} != hull rigSize {hull_rig_size} on {}",
                    mod_type.name, hull_type.name
                ));
            }
        }

        // Validate required character skills (requiredSkill1..3 and requiredSkill1..3Level)
        for (skill_attr, level_attr) in [
            (attr_ids::REQUIRED_SKILL_1, attr_ids::REQUIRED_SKILL_1_LEVEL),
            (attr_ids::REQUIRED_SKILL_2, attr_ids::REQUIRED_SKILL_2_LEVEL),
            (attr_ids::REQUIRED_SKILL_3, attr_ids::REQUIRED_SKILL_3_LEVEL),
        ] {
            if let Some(&req_skill_f) = mod_type.default_attributes.get(&skill_attr) {
                let req_skill_id = req_skill_f as TypeId;
                if req_skill_id > 0 {
                    let req_level = mod_type
                        .default_attributes
                        .get(&level_attr)
                        .copied()
                        .unwrap_or(1.0)
                        .max(1.0) as u8;
                    let trained_level = self
                        .character_skills
                        .get(&req_skill_id)
                        .copied()
                        .unwrap_or(0);
                    if trained_level < req_level {
                        return Err(format!(
                            "Missing required skill {req_skill_id} level {req_level} (trained: {trained_level}) for module {} (type {})",
                            mod_type.name, module_type_id
                        ));
                    }
                }
            }
        }

        // Validate calibration (for rigs)
        let cal_need = mod_type
            .default_attributes
            .get(&attr_ids::UPGRADE_COST)
            .copied()
            .unwrap_or(0.0);
        if cal_need > 0.0 {
            let current_cal_used: f64 = self
                .modules
                .values()
                .filter(|m| m.item_id != module_item_id && (92..=94).contains(&m.flag_id))
                .filter_map(|m| {
                    sde.types
                        .get(&m.type_id)?
                        .default_attributes
                        .get(&attr_ids::UPGRADE_COST)
                        .copied()
                })
                .sum();
            let cal_capacity = self
                .ship
                .base_attributes
                .get(&attr_ids::UPGRADE_CAPACITY)
                .copied()
                .or_else(|| {
                    hull_type
                        .default_attributes
                        .get(&attr_ids::UPGRADE_CAPACITY)
                        .copied()
                })
                .unwrap_or(0.0);
            if current_cal_used + cal_need > cal_capacity + 1e-6 {
                return Err(format!(
                    "Insufficient Calibration: need {cal_need}, available {}",
                    cal_capacity - current_cal_used
                ));
            }
        }

        // Prevent duplicate modifiers if already fitted
        let prev_entry = self.modules.remove(&module_item_id);
        let prev_load_cpu = {
            self.recalculate_stats(sde);
            self.ship.get_attribute(attr_ids::CPU_LOAD, sde)
        };
        let prev_load_pg = self.ship.get_attribute(attr_ids::POWER_LOAD, sde);
        let cpu_avail = self.ship.get_attribute(attr_ids::CPU_OUTPUT, sde) - prev_load_cpu;
        let pg_avail = self.ship.get_attribute(attr_ids::POWER_OUTPUT, sde) - prev_load_pg;

        self.modules.insert(
            module_item_id,
            FittedModule {
                item_id: module_item_id,
                type_id: module_type_id,
                flag_id,
                state: if online {
                    ModuleState::Online
                } else {
                    ModuleState::Offline
                },
                damage_hp: 0.0,
                max_hp: 40.0,
                charge_type_id: None,
                charge_quantity: 0,
            },
        );
        self.recalculate_stats(sde);

        if online {
            let cpu_load = self.ship.get_attribute(attr_ids::CPU_LOAD, sde);
            let cpu_out = self.ship.get_attribute(attr_ids::CPU_OUTPUT, sde);
            let pg_load = self.ship.get_attribute(attr_ids::POWER_LOAD, sde);
            let pg_out = self.ship.get_attribute(attr_ids::POWER_OUTPUT, sde);

            if cpu_load > cpu_out + 1e-6 {
                let cpu_need = cpu_load - prev_load_cpu;
                self.modules.remove(&module_item_id);
                if let Some(prev) = prev_entry {
                    self.modules.insert(module_item_id, prev);
                }
                self.recalculate_stats(sde);
                return Err(format!("Insufficient CPU: need {cpu_need}, available {cpu_avail}"));
            }
            if pg_load > pg_out + 1e-6 {
                let pg_need = pg_load - prev_load_pg;
                self.modules.remove(&module_item_id);
                if let Some(prev) = prev_entry {
                    self.modules.insert(module_item_id, prev);
                }
                self.recalculate_stats(sde);
                return Err(format!(
                    "Insufficient Powergrid: need {pg_need}, available {pg_avail}"
                ));
            }
        }
        Ok(())
    }

    /// Fits and brings a module online, validating slot effects, hull slot counts, turret/launcher
    /// hardpoints, hull/group restrictions, rig/charge size classes, character skills, and CPU/PG/Calibration.
    pub fn fit_and_online_module(
        &mut self,
        module_item_id: i64,
        module_type_id: TypeId,
        flag_id: u16,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        self.fit_module_with_state(module_item_id, module_type_id, flag_id, true, sde)
    }

    /// Alias for `fit_and_online_module`.
    pub fn fit_module(
        &mut self,
        module_item_id: i64,
        module_type_id: TypeId,
        flag_id: u16,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        self.fit_and_online_module(module_item_id, module_type_id, flag_id, sde)
    }

    /// Brings an offline fitted module online, checking maxGroupOnline and CPU/PG.
    pub fn set_module_online(
        &mut self,
        module_item_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        let (mod_type_id, _flag_id) = {
            let m = self
                .modules
                .get(&module_item_id)
                .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
            (m.type_id, m.flag_id)
        };
        let mod_type = sde
            .types
            .get(&mod_type_id)
            .ok_or_else(|| format!("Unknown module type {mod_type_id}"))?;

        let mut max_group_online = mod_type
            .default_attributes
            .get(&978)
            .copied()
            .or_else(|| if mod_type.group_id == 46 { Some(1.0) } else { None });
        if mod_type.group_id == 1770 {
            if let Some(ref mut limit) = max_group_online {
                let rig_bonus: f64 = self
                    .modules
                    .values()
                    .filter(|m| m.item_id != module_item_id && m.state != ModuleState::Offline)
                    .filter_map(|m| sde.types.get(&m.type_id))
                    .filter(|t| t.effect_ids.contains(&6766))
                    .map(|t| t.default_attributes.get(&2542).copied().unwrap_or(1.0))
                    .sum();
                *limit += rig_bonus;
            }
        }
        if let Some(limit) = max_group_online {
            let online_count = self
                .modules
                .values()
                .filter(|m| m.item_id != module_item_id && m.state != ModuleState::Offline)
                .filter(|m| {
                    sde.types
                        .get(&m.type_id)
                        .map(|t| t.group_id == mod_type.group_id)
                        .unwrap_or(false)
                })
                .count();
            if (online_count as f64) >= limit {
                return Err(format!(
                    "MaxGroupOnlineExceeded: module {} (group {}) limit is {limit}",
                    mod_type.name, mod_type.group_id
                ));
            }
        }

        let prev_load_cpu = self.ship.get_attribute(attr_ids::CPU_LOAD, sde);
        let prev_load_pg = self.ship.get_attribute(attr_ids::POWER_LOAD, sde);
        let cpu_avail = self.ship.get_attribute(attr_ids::CPU_OUTPUT, sde) - prev_load_cpu;
        let pg_avail = self.ship.get_attribute(attr_ids::POWER_OUTPUT, sde) - prev_load_pg;

        let prev_state = self.modules.get(&module_item_id).map(|m| m.state);
        if let Some(m) = self.modules.get_mut(&module_item_id) {
            m.state = ModuleState::Online;
        }
        self.recalculate_stats(sde);

        let cpu_load = self.ship.get_attribute(attr_ids::CPU_LOAD, sde);
        let cpu_out = self.ship.get_attribute(attr_ids::CPU_OUTPUT, sde);
        let pg_load = self.ship.get_attribute(attr_ids::POWER_LOAD, sde);
        let pg_out = self.ship.get_attribute(attr_ids::POWER_OUTPUT, sde);

        if cpu_load > cpu_out + 1e-6 {
            let cpu_need = cpu_load - prev_load_cpu;
            if let (Some(m), Some(st)) = (self.modules.get_mut(&module_item_id), prev_state) {
                m.state = st;
            }
            self.recalculate_stats(sde);
            return Err(format!("Insufficient CPU: need {cpu_need}, available {cpu_avail}"));
        }
        if pg_load > pg_out + 1e-6 {
            let pg_need = pg_load - prev_load_pg;
            if let (Some(m), Some(st)) = (self.modules.get_mut(&module_item_id), prev_state) {
                m.state = st;
            }
            self.recalculate_stats(sde);
            return Err(format!(
                "Insufficient Powergrid: need {pg_need}, available {pg_avail}"
            ));
        }

        Ok(())
    }

    /// Takes an online fitted module offline, removing its CPU/PG and online effects.
    pub fn take_module_offline(
        &mut self,
        module_item_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        let m = self
            .modules
            .get_mut(&module_item_id)
            .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
        m.state = ModuleState::Offline;
        self.recalculate_stats(sde);
        Ok(())
    }

    /// Unfits a module from the ship and recalculates stats.
    pub fn unfit_module(
        &mut self,
        module_item_id: i64,
        sde: &SdeRegistry,
    ) -> Result<FittedModule, String> {
        let removed = self
            .modules
            .remove(&module_item_id)
            .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
        self.recalculate_stats(sde);
        Ok(removed)
    }

    /// Loads a charge or script into a fitted module and recalculates stats.
    pub fn load_charge(
        &mut self,
        module_item_id: i64,
        charge_type_id: TypeId,
        quantity: u32,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        let m = self
            .modules
            .get_mut(&module_item_id)
            .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
        m.charge_type_id = Some(charge_type_id);
        m.charge_quantity = quantity;
        self.recalculate_stats(sde);
        Ok(())
    }

    /// Deactivates an active/overloaded module back to `ModuleState::Online`, removing its active-only modifiers.
    pub fn deactivate_module(
        &mut self,
        module_item_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        {
            let mod_entry = self
                .modules
                .get_mut(&module_item_id)
                .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
            if mod_entry.state == ModuleState::Offline {
                return Err("Module is offline".into());
            }
            mod_entry.state = ModuleState::Online;
        }
        self.recalculate_stats(sde);
        Ok(())
    }

    /// Activates or overloads an online module (e.g. 1MN Afterburner II), applying its active modifiers
    /// idempotently without stacking duplicates if called repeatedly.
    pub fn activate_module(
        &mut self,
        module_item_id: i64,
        overload: bool,
        sde: &SdeRegistry,
    ) -> Result<(), String> {
        {
            let mod_entry = self
                .modules
                .get_mut(&module_item_id)
                .ok_or_else(|| format!("Module {module_item_id} is not fitted"))?;
            if mod_entry.state == ModuleState::Offline {
                return Err("Cannot activate an offline module".into());
            }
            if mod_entry.damage_hp >= mod_entry.max_hp {
                return Err("Module is burned out (100% heat damage)".into());
            }
            mod_entry.state = if overload {
                ModuleState::Overloaded
            } else {
                ModuleState::Active
            };
        }
        self.recalculate_stats(sde);
        Ok(())
    }

    /// Steps thermodynamics for an overloaded module cycle in a rack (`0 = High, 1 = Med, 2 = Low`).
    /// Generates heat, applies heat damage to the module, rolls damage probability to adjacent modules
    /// in the same rack, and sets burned modules to `ModuleState::Offline` (`feat:fitting.overheat`, `DG-4`).
    pub fn apply_heat_cycle(
        &mut self,
        module_id: i64,
        heat_gen_rate: f64,
        dissipation_rate: f64,
        cycle_duration_s: f64,
        roll_u: f64,
        sde: &SdeRegistry,
    ) -> Result<HeatDamageReport, String> {
        let (flag_id, rack_idx, base_heat_dmg) = {
            let m = self.modules.get(&module_id).ok_or_else(|| format!("Module {module_id} not found"))?;
            let r_idx = match m.flag_id {
                27..=34 => 0, // High
                19..=26 => 1, // Med
                11..=18 => 2, // Low
                _ => return Err(format!("Module on flag {} is not in a heat rack", m.flag_id)),
            };
            let mod_state = self.build_effective_module_state(module_id, m.type_id, true, sde);
            let heat_dmg = mod_state.get_attribute(1211, sde);
            let eff_heat_dmg = if heat_dmg > 0.0 { heat_dmg } else { 5.0 };
            (m.flag_id, r_idx, eff_heat_dmg)
        };

        // 1. Advance rack heat
        self.rack_heat[rack_idx] = thermodynamics::step_rack_heat(
            self.rack_heat[rack_idx],
            heat_gen_rate,
            dissipation_rate,
            cycle_duration_s,
        );
        let current_rack_heat = self.rack_heat[rack_idx];

        // 2. Compute effective heat damage to active module
        let thermo_lvl = self.character_skills.get(&28164).copied().unwrap_or(0); // 28164 = Thermodynamics
        let applied_mod_dmg = thermodynamics::effective_heat_damage(base_heat_dmg, thermo_lvl);

        let mut module_burned = false;
        if let Some(m) = self.modules.get_mut(&module_id) {
            m.damage_hp += applied_mod_dmg;
            if m.damage_hp >= m.max_hp {
                m.damage_hp = m.max_hp;
                m.state = ModuleState::Offline;
                module_burned = true;
            }
        }

        // 3. Roll damage to adjacent modules in the same rack
        let mut adjacent_reports = Vec::new();
        let other_modules: Vec<(i64, u16)> = self
            .modules
            .values()
            .filter(|m| m.item_id != module_id)
            .filter(|m| match (rack_idx, m.flag_id) {
                (0, 27..=34) => true,
                (1, 19..=26) => true,
                (2, 11..=18) => true,
                _ => false,
            })
            .map(|m| (m.item_id, m.flag_id))
            .collect();

        for (adj_id, adj_flag) in other_modules {
            let slot_dist = (adj_flag as isize - flag_id as isize).unsigned_abs();
            let prob = thermodynamics::heat_damage_probability(current_rack_heat, slot_dist, 0.5);
            if roll_u < prob {
                let adj_dmg = thermodynamics::effective_heat_damage(base_heat_dmg * 0.5, thermo_lvl);
                let mut adj_burned = false;
                if let Some(am) = self.modules.get_mut(&adj_id) {
                    am.damage_hp += adj_dmg;
                    if am.damage_hp >= am.max_hp {
                        am.damage_hp = am.max_hp;
                        am.state = ModuleState::Offline;
                        adj_burned = true;
                    }
                }
                adjacent_reports.push((adj_id, adj_dmg, adj_burned));
            }
        }

        self.recalculate_stats(sde);

        Ok(HeatDamageReport {
            rack_heat: current_rack_heat,
            module_damage: applied_mod_dmg,
            module_burned,
            adjacent_damaged_modules: adjacent_reports,
        })
    }
}

/// Report returned when applying heat cycle damage to a rack and its fitted modules (`feat:fitting.overheat`, `DG-4`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeatDamageReport {
    pub rack_heat: f64,
    pub module_damage: f64,
    pub module_burned: bool,
    pub adjacent_damaged_modules: Vec<(i64, f64, bool)>,
}

/// Thermodynamics formulas (`#29`–`#34`):
/// Rack heat generation, dissipation, slot-distance attenuation $e^{-d}$, and nanite repair paste cost.
pub mod thermodynamics {
    /// Computes rack heat level after `dt_sec` given active heat generation rate and dissipation rate (`#29`–`#30`).
    pub fn step_rack_heat(current_heat: f64, heat_gen_rate: f64, dissipation_rate: f64, dt_sec: f64) -> f64 {
        let target = if heat_gen_rate > 0.0 { 1.0 } else { 0.0 };
        let rate = if heat_gen_rate > 0.0 {
            heat_gen_rate
        } else {
            dissipation_rate
        };
        let next = target + (current_heat - target) * (-rate * dt_sec).exp();
        next.clamp(0.0, 1.0)
    }

    /// Computes slot-distance heat attenuation factor $A(d) = e^{-d_{\text{slots}} \cdot f_{\text{attenuation}}}$ (`#31`).
    pub fn heat_distance_attenuation(slot_distance: usize, attenuation_factor: f64) -> f64 {
        (-(slot_distance as f64) * attenuation_factor).exp()
    }

    /// Computes heat damage probability for a module at slot distance `d` when rack heat is `rack_heat` (`#32`).
    pub fn heat_damage_probability(rack_heat: f64, slot_distance: usize, attenuation_factor: f64) -> f64 {
        (rack_heat * heat_distance_attenuation(slot_distance, attenuation_factor)).clamp(0.0, 1.0)
    }

    /// Computes effective heat damage per cycle after Thermodynamics skill level (`5%` reduction/level, `#33`).
    pub fn effective_heat_damage(base_heat_damage: f64, thermodynamics_level: u8) -> f64 {
        base_heat_damage * (1.0 - 0.05 * (thermodynamics_level.min(5) as f64))
    }

    /// Computes Nanite Repair Paste quantity required to repair `damaged_hp` on a module (`#34`).
    pub fn nanite_paste_required(damaged_hp: f64, paste_per_hp: f64) -> u32 {
        (damaged_hp.max(0.0) * paste_per_hp).ceil() as u32
    }
}

/// Combat Booster Side-Effect Formulas (`#35`–`#36`):
/// Neurotoxin Control reduces side-effect chance (`5%/lvl`); Neurotoxin Recovery reduces severity (`5%/lvl`).
pub fn evaluate_booster_side_effect(
    base_chance: f64,
    base_penalty_pct: f64,
    neurotoxin_control_level: u8,
    neurotoxin_recovery_level: u8,
    deterministic_roll: f64,
) -> Option<f64> {
    let effective_chance = base_chance * (1.0 - 0.05 * (neurotoxin_control_level.min(5) as f64));
    if deterministic_roll < effective_chance {
        let effective_penalty =
            base_penalty_pct * (1.0 - 0.05 * (neurotoxin_recovery_level.min(5) as f64));
        Some(effective_penalty)
    } else {
        None
    }
}

/// Command Burst `dbuff` Aggregation (`#37`): aggregates multiple active `dbuff` values by `Maximum`, `Minimum`, or `Additive`.
pub fn aggregate_dbuff_values(mode: &str, values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    match mode {
        "Minimum" => values
            .iter()
            .copied()
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0),
        "Additive" => values.iter().sum(),
        _ => values
            .iter()
            .copied()
            .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or(0.0),
    }
}

/// Abyssal Dynamic Item (`mutaplasmid`) Attribute Rolling (`#38`):
/// Applies linear interpolation $A_{\text{rolled}} = A_{\text{base}} \cdot (M_{\min} + u \cdot (M_{\max} - M_{\min}))$.
pub fn roll_abyssal_attributes(
    base_attributes: &HashMap<AttributeId, f64>,
    ranges: &HashMap<AttributeId, [f64; 2]>,
    unit_rolls: &HashMap<AttributeId, f64>,
) -> HashMap<AttributeId, f64> {
    let mut out = base_attributes.clone();
    for (&attr_id, &[min_mult, max_mult]) in ranges {
        if let Some(&base_val) = base_attributes.get(&attr_id) {
            let u = unit_rolls.get(&attr_id).copied().unwrap_or(0.5).clamp(0.0, 1.0);
            let mult = min_mult + u * (max_mult - min_mult);
            out.insert(attr_id, base_val * mult);
        }
    }
    out
}
