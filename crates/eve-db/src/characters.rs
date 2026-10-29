//! Character creation, selection, movement, docking/undocking, and clone respawn.

use crate::persistence::WorldRepository;
use crate::types::*;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::{HashMap, HashSet};

impl WorldRepository {
    pub fn get_characters_for_user(&self, user_id: u64) -> Vec<CharacterRecord> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut chars: Vec<CharacterRecord> = guard
            .characters
            .values()
            .filter(|c| c.user_id == user_id)
            .cloned()
            .collect();
        chars.sort_by_key(|c| c.character_id);
        chars
    }

    pub fn get_character(&self, character_id: i64) -> Result<CharacterRecord, DbError> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard
            .characters
            .get(&character_id)
            .cloned()
            .ok_or(DbError::CharacterNotFound(character_id))
    }

    pub fn select_character_for_user(
        &self,
        user_id: u64,
        character_id: i64,
    ) -> Result<CharacterRecord, DbError> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let ch = guard
            .characters
            .get(&character_id)
            .ok_or(DbError::CharacterNotFound(character_id))?;
        if ch.user_id != user_id {
            return Err(DbError::CharacterOwnershipViolation {
                user_id,
                character_id,
            });
        }
        Ok(ch.clone())
    }

    /// Creates a new character for `user_id` with its racial Corvette, NPE progress, and school starting station (`E2E-01`).
    pub fn create_character(
        &self,
        user_id: u64,
        name: &str,
        race_id: u32,
        bloodline_id: u32,
        ancestry_id: u32,
        school_id: u32,
        sde: &SdeRegistry,
    ) -> Result<CharacterRecord, DbError> {
        self.create_character_with_system(
            user_id,
            name,
            race_id,
            bloodline_id,
            ancestry_id,
            school_id,
            sde,
            None,
        )
    }

    pub fn create_character_with_system(
        &self,
        user_id: u64,
        name: &str,
        race_id: u32,
        bloodline_id: u32,
        ancestry_id: u32,
        school_id: u32,
        sde: &SdeRegistry,
        qa_starter_system_id: Option<u32>,
    ) -> Result<CharacterRecord, DbError> {
        if name.trim().len() < 3 {
            return Err(DbError::ValidationError(
                "Character name must be at least 3 characters".into(),
            ));
        }

        // Strictly validate race, bloodline, ancestry, and school lineage against SDE
        let race = sde
            .races
            .get(&race_id)
            .ok_or_else(|| DbError::ValidationError(format!("Unknown race_id {race_id}")))?;
        let bl = sde.bloodlines.get(&bloodline_id).ok_or_else(|| {
            DbError::ValidationError(format!("Unknown bloodline_id {bloodline_id}"))
        })?;
        if bl.race_id != race_id {
            return Err(DbError::ValidationError(format!(
                "Bloodline {bloodline_id} does not belong to race {race_id}"
            )));
        }
        let anc = sde.ancestries.get(&ancestry_id).ok_or_else(|| {
            DbError::ValidationError(format!("Unknown ancestry_id {ancestry_id}"))
        })?;
        // V24.01 character creation sends const.ancestryUndefined (43, bloodline_id == 0).
        let ancestry_id = if anc.bloodline_id == 0 {
            sde.ancestries
                .values()
                .filter(|a| a.bloodline_id == bloodline_id)
                .map(|a| a.ancestry_id)
                .min()
                .unwrap_or(ancestry_id)
        } else if anc.bloodline_id != bloodline_id {
            return Err(DbError::ValidationError(format!(
                "Ancestry {ancestry_id} does not belong to bloodline {bloodline_id}"
            )));
        } else {
            ancestry_id
        };
        let school = sde
            .schools
            .get(&school_id)
            .ok_or_else(|| DbError::ValidationError(format!("Unknown school_id {school_id}")))?;
        if school.race_id != race_id {
            return Err(DbError::ValidationError(format!(
                "School {school_id} does not belong to race {race_id}"
            )));
        }

        self.try_with_mut_snapshot(|guard| {
            let user_char_count = guard
                .characters
                .values()
                .filter(|c| c.user_id == user_id)
                .count();
            if user_char_count >= 3 {
                return Err(DbError::ValidationError(
                    "Maximum of 3 characters per account reached".into(),
                ));
            }
            if guard
                .characters
                .values()
                .any(|c| c.name.eq_ignore_ascii_case(name))
            {
                return Err(DbError::ValidationError(
                    "Character name already exists".into(),
                ));
            }

            guard.next_entity_id += 1;
            let char_id = guard.next_entity_id;
            guard.next_entity_id += 1;
            let ship_id = guard.next_entity_id;

            let (sys_id, station_id, const_id, reg_id) = if let Some(target_sys_id) = qa_starter_system_id {
                if let Some(target_sys) = sde.solar_systems.get(&target_sys_id) {
                    if let Some(st) = target_sys
                        .stations
                        .iter()
                        .find(|s| s.station_id == 60_003_760)
                        .or_else(|| target_sys.stations.first())
                    {
                        (target_sys_id, st.station_id, target_sys.constellation_id, target_sys.region_id)
                    } else {
                        let sys = sde.solar_systems.get(&school.solar_system_id);
                        let const_id = sys.map(|s| s.constellation_id).unwrap_or(20_000_020);
                        let reg_id = sys.map(|s| s.region_id).unwrap_or(10_000_002);
                        (school.solar_system_id, school.station_id, const_id, reg_id)
                    }
                } else {
                    let sys = sde.solar_systems.get(&school.solar_system_id);
                    let const_id = sys.map(|s| s.constellation_id).unwrap_or(20_000_020);
                    let reg_id = sys.map(|s| s.region_id).unwrap_or(10_000_002);
                    (school.solar_system_id, school.station_id, const_id, reg_id)
                }
            } else {
                let sys = sde.solar_systems.get(&school.solar_system_id);
                let const_id = sys.map(|s| s.constellation_id).unwrap_or(20_000_020);
                let reg_id = sys.map(|s| s.region_id).unwrap_or(10_000_002);
                (school.solar_system_id, school.station_id, const_id, reg_id)
            };
            let corp_id = school.corporation_id;
            let corvette_type_id = race.corvette_type_id.unwrap_or(0);

            let mut starter_skills_map = HashMap::new();
            let mut total_sp = 0.0;
            for (&skill_id, &lvl) in &race.starter_skills {
                if lvl > 0 {
                    let rank = sde
                        .types
                        .get(&skill_id)
                        .and_then(|t| t.default_attributes.get(&275).copied())
                        .unwrap_or(1.0);
                    let sp = (250.0 * rank * 2.0_f64.powf(2.5 * (lvl.min(5) as i32 - 1) as f64)).ceil();
                    total_sp += sp;
                    starter_skills_map.insert(
                        skill_id,
                        TrainedSkillRecord {
                            type_id: skill_id,
                            level: lvl,
                            skill_points: sp,
                        },
                    );
                }
            }
            if starter_skills_map.is_empty() {
                total_sp = 400_000.0;
            }

            let rec = CharacterRecord {
                character_id: char_id,
                user_id,
                name: name.to_string(),
                corporation_id: corp_id,
                alliance_id: None,
                war_faction_id: None,
                station_id: Some(station_id),
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: const_id,
                region_id: reg_id,
                active_ship_id: ship_id,
                balance: IskCents::from_isk(5_000),
                plex_balance: 0,
                loyalty_points: HashMap::new(),
                standings: HashMap::new(),
                security_status: 0.0,
                clone_grade: 1,
                home_station_id: station_id,
                race_id,
                bloodline_id,
                ancestry_id,
                school_id,
                neural: NeuralAttributes::default(),
                skill_points: total_sp,
                activity_status: None,
            free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                gender: 0,
                create_date_unix: 1704067200,
                fleet_id: None,
                autopilot_waypoints: Vec::new(),
                version: 1,
                system_epoch: 1,
            };

            guard.characters.insert(char_id, rec.clone());
            guard.skills.insert(char_id, starter_skills_map);
            guard.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: corvette_type_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            guard.npe_progress.insert(
                char_id,
                NpeProgressRecord {
                    character_id: char_id,
                    node_graph_id: 473,
                    current_node_id: 1,
                    completed: false,
                    career_goal_progress: HashMap::new(),
                    claimed_goals: HashSet::new(),
                },
            );
            Ok(rec)
        })
    }

    pub fn undock_character(
        &self,
        user_id: u64,
        character_id: i64,
    ) -> Result<(i64, CharacterRecord, InventoryItemRecord), DbError> {
        self.try_with_mut_snapshot(|guard| {
            let (station_id, active_ship_id, solar_system_id) = {
                let ch = guard
                    .characters
                    .get(&character_id)
                    .ok_or(DbError::CharacterNotFound(character_id))?;
                if ch.user_id != user_id {
                    return Err(DbError::CharacterOwnershipViolation {
                        user_id,
                        character_id,
                    });
                }
                let st_id = ch
                    .station_id
                    .or(ch.structure_id)
                    .ok_or(DbError::NotDocked(character_id))?;
                (st_id, ch.active_ship_id, ch.solar_system_id)
            };

            {
                let ship = guard
                    .items
                    .get(&active_ship_id)
                    .ok_or(DbError::ItemNotFound(active_ship_id))?;
                if ship.owner_id != character_id {
                    return Err(DbError::ItemOwnershipViolation {
                        character_id,
                        item_id: active_ship_id,
                    });
                }
                if ship.location_id != station_id {
                    return Err(DbError::ShipNotAtDockedStation {
                        ship_id: active_ship_id,
                        ship_location_id: ship.location_id,
                        station_id,
                    });
                }
            }

            let ch = guard.characters.get_mut(&character_id).unwrap();
            ch.station_id = None;
            ch.structure_id = None;
            ch.version += 1;
            ch.system_epoch += 1;
            let ch_snapshot = ch.clone();

            let ship = guard.items.get_mut(&active_ship_id).unwrap();
            ship.location_id = solar_system_id as i64;
            ship.flag = InventoryFlag::None;
            ship.version += 1;
            let ship_snapshot = ship.clone();

            Ok((station_id, ch_snapshot, ship_snapshot))
        })
    }

    /// Docks a character in a station or Upwell structure.
    pub fn dock_character(
        &self,
        character_id: i64,
        dock_location_id: i64,
        is_structure: bool,
    ) -> Result<CharacterRecord, DbError> {
        self.try_with_mut_snapshot(|guard| {
            let (active_ship_id, ch_solar_system_id) = {
                let ch = guard
                    .characters
                    .get(&character_id)
                    .ok_or(DbError::CharacterNotFound(character_id))?;
                (ch.active_ship_id, ch.solar_system_id)
            };
            if is_structure {
                let st = guard.structures.get(&dock_location_id).ok_or(
                    DbError::InvalidDestinationLocation {
                        character_id,
                        location_id: dock_location_id,
                    },
                )?;
                if st.solar_system_id != ch_solar_system_id || st.state == "Destroyed" {
                    return Err(DbError::InvalidDestinationLocation {
                        character_id,
                        location_id: dock_location_id,
                    });
                }
            }
            {
                let ch = guard.characters.get_mut(&character_id).unwrap();
                if is_structure {
                    ch.structure_id = Some(dock_location_id);
                    ch.station_id = None;
                } else {
                    ch.station_id = Some(dock_location_id);
                    ch.structure_id = None;
                }
                ch.version += 1;
                ch.system_epoch += 1;
            }
            if let Some(ship) = guard.items.get_mut(&active_ship_id) {
                if ship.location_id == ch_solar_system_id as i64 {
                    ship.location_id = dock_location_id;
                    ship.flag = InventoryFlag::Hangar;
                    ship.version += 1;
                }
            }
            Ok(guard.characters[&character_id].clone())
        })
    }

    /// Transactionally destroys `destroyed_ship_id` (and all fitted/contained items inside it),
    /// reuses or spawns a Capsule (`type_id = 670`) at `character.home_station_id`, and updates the
    /// character's active ship, docked station, and solar system/constellation/region from `sde`.
    pub fn respawn_character_at_home_station(
        &self,
        character_id: i64,
        destroyed_ship_id: i64,
        sde: &SdeRegistry,
    ) -> Result<(CharacterRecord, InventoryItemRecord), DbError> {
        self.try_with_mut_snapshot(|guard| {
            let ch_pre = guard
                .characters
                .get(&character_id)
                .cloned()
                .ok_or(DbError::CharacterNotFound(character_id))?;
            let raw_home_id = ch_pre.home_station_id;

            let resolve_school_fallback = || -> (i64, SolarSystemId, u32, u32) {
                let school_opt = sde
                    .schools
                    .get(&ch_pre.school_id)
                    .or_else(|| sde.schools.values().find(|s| s.race_id == ch_pre.race_id))
                    .or_else(|| sde.schools.values().min_by_key(|s| s.school_id));
                let (sta_id, sys_id) = if let Some(sc) = school_opt {
                    (sc.station_id, sc.solar_system_id)
                } else if let Some(st) = sde.stations.values().min_by_key(|s| s.station_id) {
                    (st.station_id, st.solar_system_id)
                } else {
                    (raw_home_id, ch_pre.solar_system_id)
                };
                let (const_id, reg_id) = sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| (s.constellation_id, s.region_id))
                    .unwrap_or((ch_pre.constellation_id, ch_pre.region_id));
                (sta_id, sys_id, const_id, reg_id)
            };

            let (respawn_loc_id, respawn_is_structure, reset_home, home_sys_id, home_const_id, home_reg_id) =
                if raw_home_id >= 1_000_000_000_000 {
                    let valid_struct = guard.structures.get(&raw_home_id).filter(|st| {
                        let is_op = !matches!(
                            st.state.as_str(),
                            "AnchorVulnerable" | "Anchoring" | "NoCore" | "Onlining" | "Destroyed"
                        );
                        let req_core = sde
                            .types
                            .get(&st.type_id)
                            .and_then(|t| t.default_attributes.get(&3101).copied())
                            .unwrap_or(0.0)
                            > 0.0;
                        let cored = !req_core || st.quantum_core_installed;
                        let has_clone_bay = guard.items.values().any(|it| {
                            it.location_id == raw_home_id
                                && (164..=171).contains(&(it.flag as u16))
                                && guard.module_online_states.get(&it.item_id) == Some(&true)
                                && (it.type_id == 35894
                                    || sde.types.get(&it.type_id).is_some_and(|t| {
                                        t.name.to_ascii_lowercase().contains("clone bay")
                                    }))
                        });
                        is_op && cored && has_clone_bay
                    });
                    if let Some(st) = valid_struct {
                        let sys_id = st.solar_system_id;
                        let (const_id, reg_id) = sde
                            .solar_systems
                            .get(&sys_id)
                            .map(|s| (s.constellation_id, s.region_id))
                            .unwrap_or((ch_pre.constellation_id, ch_pre.region_id));
                        (raw_home_id, true, false, sys_id, const_id, reg_id)
                    } else {
                        let (sta_id, sys_id, const_id, reg_id) = resolve_school_fallback();
                        (sta_id, false, true, sys_id, const_id, reg_id)
                    }
                } else if let Some(st) = sde.stations.get(&raw_home_id) {
                    let sys_id = st.solar_system_id;
                    let (const_id, reg_id) = sde
                        .solar_systems
                        .get(&sys_id)
                        .map(|s| (s.constellation_id, s.region_id))
                        .unwrap_or((ch_pre.constellation_id, ch_pre.region_id));
                    (raw_home_id, false, false, sys_id, const_id, reg_id)
                } else if let Some(sys) = sde
                    .solar_systems
                    .values()
                    .find(|sys| sys.stations.iter().any(|st| st.station_id == raw_home_id))
                {
                    (raw_home_id, false, false, sys.solar_system_id, sys.constellation_id, sys.region_id)
                } else {
                    let (sta_id, sys_id, const_id, reg_id) = resolve_school_fallback();
                    (sta_id, false, true, sys_id, const_id, reg_id)
                };

            // Recursively remove the destroyed ship and all items fitted/contained inside it (including nested containers)
            let mut to_remove = HashSet::from([destroyed_ship_id]);
            loop {
                let mut added = false;
                for (&id, item) in &guard.items {
                    if !to_remove.contains(&id) && to_remove.contains(&item.location_id) {
                        to_remove.insert(id);
                        added = true;
                    }
                }
                if !added {
                    break;
                }
            }
            let is_pod = guard
                .items
                .get(&destroyed_ship_id)
                .map(|i| {
                    i.type_id == 670
                        || i.type_id == 33328
                        || sde.types.get(&i.type_id).map(|t| t.group_id == 29).unwrap_or(false)
                })
                .unwrap_or(true);
            if is_pod {
                guard.active_implants.remove(&character_id);
                guard.items.retain(|_, item| {
                    !(item.location_id == character_id
                        && matches!(item.flag, InventoryFlag::Implant | InventoryFlag::Booster))
                });
            }

            guard.items.retain(|id, _| !to_remove.contains(id));

            let existing_capsule_id = guard
                .items
                .values()
                .find(|i| {
                    i.owner_id == character_id
                        && i.type_id == 670
                        && i.location_id == respawn_loc_id
                })
                .map(|i| i.item_id);
            let (capsule_id, capsule) = if let Some(cid) = existing_capsule_id {
                let cap = guard.items.get_mut(&cid).unwrap();
                cap.flag = InventoryFlag::Hangar;
                cap.version += 1;
                (cid, cap.clone())
            } else {
                guard.next_entity_id += 1;
                let cid = guard.next_entity_id;
                let cap = InventoryItemRecord {
                    item_id: cid,
                    type_id: 670, // Capsule
                    owner_id: character_id,
                    location_id: respawn_loc_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                };
                guard.items.insert(cid, cap.clone());
                (cid, cap)
            };

            let ch = guard.characters.get_mut(&character_id).unwrap();
            ch.active_ship_id = capsule_id;
            if reset_home {
                ch.home_station_id = respawn_loc_id;
            }
            if respawn_is_structure {
                ch.station_id = None;
                ch.structure_id = Some(respawn_loc_id);
            } else {
                ch.station_id = Some(respawn_loc_id);
                ch.structure_id = None;
            }
            ch.solar_system_id = home_sys_id;
            ch.constellation_id = home_const_id;
            ch.region_id = home_reg_id;
            ch.version += 1;
            ch.system_epoch += 1;
            let ch_snapshot = ch.clone();

            Ok((ch_snapshot, capsule))
        })
    }

    /// Compensating transaction to restore docked station state if solar-system actor spawn fails.
    pub fn rollback_undock(&self, character_id: i64, station_id: i64, ship_id: i64) {
        let _ = self.try_with_mut_snapshot(|guard| {
            if let Some(ch) = guard.characters.get_mut(&character_id) {
                ch.station_id = Some(station_id);
                ch.version += 1;
                ch.system_epoch += 1;
            }
            if let Some(ship) = guard.items.get_mut(&ship_id) {
                ship.location_id = station_id;
                ship.flag = InventoryFlag::Hangar;
                ship.version += 1;
            }
            Ok::<(), std::convert::Infallible>(())
        });
    }

    /// Updates a character's solar system (e.g. after Stargate Jump, Cyno Jump, Wormhole, or Filament).
    pub fn set_character_solar_system(
        &self,
        character_id: i64,
        new_solar_system_id: SolarSystemId,
        sde: &SdeRegistry,
    ) -> Result<CharacterRecord, DbError> {
        self.try_with_mut_snapshot(|guard| {
            let (const_id, reg_id) = sde
                .solar_systems
                .get(&new_solar_system_id)
                .map(|s| (s.constellation_id, s.region_id))
                .unwrap_or((20_000_020, 10_000_002));

            let ship_id = {
                let ch = guard
                    .characters
                    .get_mut(&character_id)
                    .ok_or(DbError::CharacterNotFound(character_id))?;
                ch.solar_system_id = new_solar_system_id;
                ch.constellation_id = const_id;
                ch.region_id = reg_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.version += 1;
                ch.system_epoch += 1;
                ch.active_ship_id
            };
            if let Some(ship) = guard.items.get_mut(&ship_id) {
                ship.location_id = new_solar_system_id as i64;
                ship.version += 1;
            }
            Ok(guard.characters[&character_id].clone())
        })
    }

    /// Transactionally applies neural attribute redistribution and enforces the yearly cooldown
    /// timer or consumes a bonus remap (`CH-5`).
    pub fn remap_neural_attributes(
        &self,
        character_id: i64,
        charisma: u8,
        intelligence: u8,
        memory: u8,
        perception: u8,
        willpower: u8,
        now_unix: u64,
    ) -> Result<CharacterRecord, DbError> {
        // Each neural attribute must be between 17 and 27 inclusive
        for (name, val) in [
            ("Charisma", charisma),
            ("Intelligence", intelligence),
            ("Memory", memory),
            ("Perception", perception),
            ("Willpower", willpower),
        ] {
            if !(17..=27).contains(&val) {
                return Err(DbError::ValidationError(format!(
                    "{name} attribute {val} out of range: must be between 17 and 27"
                )));
            }
        }

        // Sum of all 5 attributes must equal 99 (85 base + 14 points)
        let total = charisma as u32
            + intelligence as u32
            + memory as u32
            + perception as u32
            + willpower as u32;
        if total != 99 {
            return Err(DbError::ValidationError(format!(
                "Total neural attribute points must equal 99, got {total}"
            )));
        }

        self.try_with_mut_snapshot(|guard| {
            let ch = guard
                .characters
                .get_mut(&character_id)
                .ok_or(DbError::CharacterNotFound(character_id))?;

            let yearly_cooldown = 365 * 86_400; // 31,536,000 seconds
            let cooldown_active = ch
                .neural
                .remap_cooldown_until_unix
                .map(|until| now_unix < until)
                .unwrap_or(false);

            if cooldown_active {
                if ch.neural.bonus_remaps > 0 {
                    ch.neural.bonus_remaps -= 1;
                } else {
                    let until = ch.neural.remap_cooldown_until_unix.unwrap_or(0);
                    return Err(DbError::ValidationError(format!(
                        "Neural remap is on yearly cooldown until unix timestamp {until}"
                    )));
                }
            } else {
                // Timed yearly remap
                ch.neural.last_remap_unix = Some(now_unix);
                ch.neural.remap_cooldown_until_unix = Some(now_unix + yearly_cooldown);
            }

            ch.neural.charisma = charisma;
            ch.neural.intelligence = intelligence;
            ch.neural.memory = memory;
            ch.neural.perception = perception;
            ch.neural.willpower = willpower;
            ch.version += 1;
            ch.system_epoch += 1;

            Ok(ch.clone())
        })
    }

    /// Records an entry in the character's mining ledger (`MN-03`, `charMiningLedger`),
    /// and mirrors the mining event to any observer structures in the system (`MN-15`, `corpMiningLedger`).
    pub fn record_mining_ledger_entry(
        &self,
        character_id: i64,
        solar_system_id: u32,
        type_id: u32,
        quantity: i64,
        timestamp_unix: u64,
    ) -> Result<(), DbError> {
        self.record_mining_ledger_entry_with_waste(
            character_id,
            solar_system_id,
            type_id,
            quantity,
            0,
            timestamp_unix,
        )
    }

    /// Records an entry in the character's mining ledger including wasted/residue quantity (`MN-16`),
    /// and mirrors the mining event to any observer structures in the system (`MN-15`, `corpMiningLedger`).
    pub fn record_mining_ledger_entry_with_waste(
        &self,
        character_id: i64,
        solar_system_id: u32,
        type_id: u32,
        quantity: i64,
        wasted_quantity: i64,
        timestamp_unix: u64,
    ) -> Result<(), DbError> {
        let date = crate::types::unix_secs_to_date_string(timestamp_unix);
        self.try_with_mut_snapshot(|guard| {
            let idx = guard.mining_ledger.len();
            guard.mining_ledger.push(MiningLedgerRecord {
                character_id,
                solar_system_id,
                type_id,
                quantity,
                date: date.clone(),
                timestamp_unix,
            });
            if wasted_quantity > 0 {
                let mut waste_map: std::collections::BTreeMap<usize, i64> = guard
                    .service_state_blobs
                    .get("mining_ledger_waste")
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default();
                waste_map.insert(idx, wasted_quantity);
                if let Ok(json) = serde_json::to_string(&waste_map) {
                    guard.service_state_blobs.insert("mining_ledger_waste".into(), json);
                }
            }

            // Mirror to observers in the same solar system (MN-15 observer volume tracking)
            let corp_id = guard
                .characters
                .get(&character_id)
                .map(|c| c.corporation_id)
                .unwrap_or(0);
            let observer_ids: Vec<i64> = guard
                .structures
                .values()
                .filter(|s| s.solar_system_id == solar_system_id)
                .map(|s| s.structure_id)
                .collect();

            for obs_id in observer_ids {
                guard
                    .observer_mining_ledger
                    .entry(obs_id)
                    .or_default()
                    .push(crate::types::ObserverMiningRecord {
                        observer_id: obs_id,
                        character_id,
                        recorded_corporation_id: corp_id,
                        type_id,
                        quantity,
                        last_updated_date: date.clone(),
                        timestamp_unix,
                    });
            }

            Ok(())
        })
    }

    /// Returns all mining ledger entries for a character (`MN-03`).
    pub fn get_mining_ledger(&self, character_id: i64) -> Vec<MiningLedgerRecord> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard
            .mining_ledger
            .iter()
            .filter(|r| r.character_id == character_id)
            .cloned()
            .collect()
    }

    /// Returns all mining ledger entries with their wasted/residue quantities for a character (`MN-16`).
    pub fn get_mining_ledger_with_waste(&self, character_id: i64) -> Vec<(MiningLedgerRecord, i64)> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let waste_map: std::collections::BTreeMap<usize, i64> = guard
            .service_state_blobs
            .get("mining_ledger_waste")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default();
        guard
            .mining_ledger
            .iter()
            .enumerate()
            .filter(|(_, r)| r.character_id == character_id)
            .map(|(idx, r)| (r.clone(), waste_map.get(&idx).copied().unwrap_or(0)))
            .collect()
    }

    /// Returns all observer mining records for a given observer structure (`MN-15`).
    pub fn get_observer_mining_ledger(&self, observer_id: i64) -> Vec<crate::types::ObserverMiningRecord> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard
            .observer_mining_ledger
            .get(&observer_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Returns all observer IDs that have recorded mining events (`MN-15`).
    pub fn get_observers_with_mining_events(&self) -> Vec<i64> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<i64> = guard.observer_mining_ledger.keys().copied().collect();
        ids.sort_unstable();
        ids
    }
}

