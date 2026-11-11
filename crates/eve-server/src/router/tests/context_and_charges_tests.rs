//! Unit tests for `ServerContext` virtual clock, skill queue advancement, fittings, and weapon charge loading.

use crate::router::ServerContext;
use eve_db::{
    CosmicSignatureRecord, InventoryFlag, InventoryItemRecord, SkillQueueEntry,
    TrainedSkillRecord,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[test]
fn context_clock_scan_progress_cooldowns_and_skill_queue_progression() {
    let prod = ServerContext::new_production();
    assert!(!prod.is_clock_paused());

    let ctx = test_ctx();
    let char_id = 90_000_001;
    let ship_id = 100_000_001;

    // Signature scan tracking & progress clamping
    assert!(!ctx.has_scanned_signature(char_id, 30_000_142, 700_001));
    ctx.record_character_scan_progress(char_id, 30_000_142, "SIG-701", 62.5);
    assert_eq!(
        ctx.get_character_scan_progress(char_id, 30_000_142, "SIG-701"),
        Some(62.5)
    );
    let sig_rec = CosmicSignatureRecord {
        signature_id: "SIG-701".into(),
        solar_system_id: 30_000_142,
        sig_group: "Relic".into(),
        type_name: "Ruined Guristas Monument".into(),
        position: [0.0; 3],
        signal_strength: 10.0,
        wormhole_type_id: None,
        destination_system_id: None,
        created_unix: 0,
        lifetime_seconds: 86_400,
        max_mass_kg: 0.0,
        remaining_mass_kg: 0.0,
        max_jump_mass_kg: 0.0,
        collapsed: false,
    };
    assert!(
        (ctx.effective_signature_signal_strength(char_id, 30_000_142, &sig_rec) - 62.5).abs()
            < 1e-6
    );
    ctx.mark_signature_scanned(char_id, 30_000_142, 700_001);
    assert!(ctx.has_scanned_signature(char_id, 30_000_142, 700_001));

    // Reactivation cooldown
    ctx.record_module_reactivation_delay(ship_id, 500_001, 15.0);
    assert!(ctx.is_module_in_reactivation_delay(ship_id, 500_001));

    // Clock controls & time dilation
    ctx.set_clock_paused(true);
    assert_eq!(ctx.step_clock_tick(5), 0);
    ctx.set_clock_paused(false);
    ctx.set_time_acceleration_multiplier(2);
    assert_eq!(ctx.time_acceleration_multiplier(), 2);
    let t0 = ctx.now_unix_secs();
    ctx.advance_virtual_clock(10);
    ctx.advance_time_seconds(5);
    assert!(ctx.now_unix_secs() >= t0 + 15);
    let _ = ctx.clock();
    assert!(ctx.now_blueos_filetime_ticks() > 0);
    assert!(!ctx.has_fleet_extras_for(999_999));

    // Skill queue progression & effective Alpha 5M SP cap
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
        }
        let skills = snap.skills.entry(char_id).or_default();
        skills.insert(
            3402,
            TrainedSkillRecord {
                type_id: 3402,
                level: 0,
                skill_points: 0.0,
            },
        );
        snap.skill_queues.insert(
            char_id,
            vec![SkillQueueEntry {
                type_id: 3402,
                target_level: 2,
                start_sp: 0.0,
                end_sp: 1_415.0,
                duration_seconds: 60.0,
                started_at_unix: Some(now),
            }],
        );
    });

    ctx.advance_secs(3_600);
    let levels = ctx.character_skill_levels(char_id);
    assert!(levels.get(&3402).copied().unwrap_or(0) >= 2);

    // Alpha clone caps total effective skill points at 5,000,000 SP
    let mut high_skill_id = 3402_u32;
    let mut candidate_tids: Vec<u32> = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.category_id == 16
                && t.published
                && crate::services::character_skills::alpha_skill_cap_for_character(
                    &ctx.sde, 1, t.type_id,
                ) < 5
        })
        .map(|t| t.type_id)
        .collect();
    candidate_tids.sort_unstable();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 1;
        }
        let skills = snap.skills.entry(char_id).or_default();
        skills.clear();
        for (idx, tid) in candidate_tids.into_iter().take(6).enumerate() {
            if idx == 5 {
                high_skill_id = tid;
            }
            skills.insert(
                tid,
                TrainedSkillRecord {
                    type_id: tid,
                    level: 5,
                    skill_points: 1_280_000.0,
                },
            );
        }
    });
    let alpha_levels = ctx.character_skill_levels(char_id);
    assert!(alpha_levels.get(&high_skill_id).copied().unwrap_or(0) < 5);

    // Ship fitting rebuild & cache invalidation
    ctx.invalidate_ship_fitting(ship_id);
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    ctx.store_ship_fitting(ship_id, fit);
}

#[test]
fn charge_compatibility_capacity_and_loading_into_weapon_slot() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let ship_id = 100_095_001;
    let turret_id = 100_095_002;
    let ammo_stack_id = 100_095_003;

    let mut weapons: Vec<_> = ctx
        .sde
        .types
        .values()
        .filter(|w| w.published && w.category_id == 7)
        .collect();
    weapons.sort_by_key(|w| w.type_id);
    let (wtype, ctype) = weapons
        .into_iter()
        .find_map(|w| {
            let cg = *w.default_attributes.get(&604)? as u32;
            let csize = w.default_attributes.get(&128).copied();
            let mut compat: Vec<_> = ctx
                .sde
                .types
                .values()
                .filter(|c| {
                    c.category_id == 8
                        && c.group_id == cg
                        && c.default_attributes.get(&128).copied() == csize
                        && ServerContext::is_charge_compatible_with_weapon(w, c)
                })
                .collect();
            compat.sort_by_key(|c| c.type_id);
            let ch = compat.first().copied()?;
            compat
                .iter()
                .any(|c| c.type_id != ch.type_id)
                .then_some((w, ch))
        })
        .expect("SDE has compatible weapon + charge pair with alternative charge");
    let tritype = &ctx.sde.types[&34];
    assert!(ServerContext::is_charge_compatible_with_weapon(wtype, ctype));
    assert!(!ServerContext::is_charge_compatible_with_weapon(
        wtype, tritype
    ));
    let cap = ServerContext::weapon_slot_max_charge_capacity(wtype, ctype);
    assert!(cap >= 1);

    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for attr_id in [182_u16, 183, 184, 277, 278, 279, 1285, 1289, 1290] {
            if let Some(&req_sk) = ctype.default_attributes.get(&attr_id) {
                skills.insert(
                    req_sk as u32,
                    TrainedSkillRecord {
                        type_id: req_sk as u32,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        for (id, tid, loc, flag, qty, singleton) in [
            (ship_id, 587, 30_000_142_i64, InventoryFlag::None, 1, true),
            (
                turret_id,
                wtype.type_id,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
                true,
            ),
            (
                ammo_stack_id,
                ctype.type_id,
                ship_id,
                InventoryFlag::Cargo,
                cap + 50,
                false,
            ),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
    });

    let (_loaded_id, _loaded_tid, loaded_qty, _changed_flag) = ctx
        .load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            ammo_stack_id,
            None,
        )
        .unwrap();
    assert_eq!(loaded_qty, cap);

    // Remaining quantity in Cargo decreased by `cap`
    let rem_cargo = ctx
        .db
        .with_snapshot(|s| s.items[&ammo_stack_id].quantity);
    assert_eq!(rem_cargo, 50);

    // Reloading same ammo when already full returns existing stack unchanged (`room == 0`)
    let (id_same, _tid_same, qty_same, _) = ctx
        .load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            ammo_stack_id,
            None,
        )
        .unwrap();
    assert_eq!(qty_same, cap);

    // ChangeAmmo to a different ammo type unloads existing charge into Cargo (or merges)
    let alt_ammo_id = 100_070_020;
    let charge_type_id = ctype.type_id;
    let alt_type_id = ctx
        .sde
        .types
        .values()
        .filter(|c| {
            c.category_id == 8
                && c.group_id == ctype.group_id
                && c.default_attributes.get(&128).copied()
                    == ctype.default_attributes.get(&128).copied()
                && (c.volume - ctype.volume).abs() < 1e-6
                && c.type_id != charge_type_id
        })
        .min_by_key(|c| c.type_id)
        .or_else(|| {
            ctx.sde
                .types
                .values()
                .filter(|c| {
                    c.category_id == 8
                        && c.group_id == ctype.group_id
                        && c.default_attributes.get(&128).copied()
                            == ctype.default_attributes.get(&128).copied()
                        && c.type_id != charge_type_id
                })
                .min_by_key(|c| c.type_id)
        })
        .map(|c| c.type_id)
        .expect("compatible alternative charge in SDE");
    let alt_type = &ctx.sde.types[&alt_type_id];
    let alt_cap = ServerContext::weapon_slot_max_charge_capacity(wtype, alt_type);
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for attr_id in [182_u16, 183, 184, 277, 278, 279, 1285, 1289, 1290] {
            if let Some(&req_sk) = alt_type.default_attributes.get(&attr_id) {
                skills.entry(req_sk as u32).or_insert(TrainedSkillRecord {
                    type_id: req_sk as u32,
                    level: 5,
                    skill_points: 256_000.0,
                });
            }
        }
        snap.items.insert(
            alt_ammo_id,
            InventoryItemRecord {
                item_id: alt_ammo_id,
                type_id: alt_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let (id_swapped, tid_swapped, qty_swapped, _) = ctx
        .load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            alt_ammo_id,
            None,
        )
        .unwrap();
    assert!(id_swapped > 0);
    assert_eq!(tid_swapped, alt_type_id);
    assert_eq!(qty_swapped, 10.min(alt_cap));
    // Previous charge merged back into `ammo_stack_id` in Cargo!
    let merged_back = ctx
        .db
        .with_snapshot(|s| (!s.items.contains_key(&id_same), s.items[&ammo_stack_id].quantity));
    assert_eq!(merged_back, (true, 50 + cap));

    // ChangeAmmo when NO matching cargo stack exists moves/merges old stack into Cargo
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&ammo_stack_id);
        snap.items.insert(
            ammo_stack_id,
            InventoryItemRecord {
                item_id: ammo_stack_id,
                type_id: charge_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 5,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let (_id_back, _tid_back, qty_back, _) = ctx
        .load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            ammo_stack_id,
            None,
        )
        .unwrap();
    assert_eq!(qty_back, 5.min(cap));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.items[&alt_ammo_id].flag),
        InventoryFlag::Cargo
    );

    // Topping off a partial charge stack when `room > 0`
    let topoff_id = 100_070_021;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            topoff_id,
            InventoryItemRecord {
                item_id: topoff_id,
                type_id: charge_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 8,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    let (_id_top, _tid_top, qty_top, _) = ctx
        .load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            topoff_id,
            None,
        )
        .unwrap();
    assert_eq!(qty_top, (qty_back + 8).min(cap));
}

#[tokio::test]
async fn context_advance_secs_expires_jetcans_extractions_moon_probes_and_skill_caps() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_id = 30_000_142;
    let jetcan_id = 900_700_001;
    let ast_id = 900_700_002;
    let ext_id = 900_700_003;

    // Seed expired jetcan + moon extraction + arrived moon probes + Alpha 5M SP cap
    let moon_id = ctx
        .sde
        .celestials_in_system(sys_id)
        .iter()
        .find(|c| c.group_id == 8)
        .map(|c| c.celestial_id)
        .unwrap_or(40_009_049);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            jetcan_id,
            InventoryItemRecord {
                item_id: jetcan_id,
                type_id: 23,
                owner_id: 90_000_001,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
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
        snap.sp6_lifecycle.space_object_meta.insert(
            jetcan_id,
            eve_db::PersistedSpaceObjectMeta {
                created_unix: Some(now.saturating_sub(4_000)),
                ..Default::default()
            },
        );
        snap.asteroids.insert(
            ast_id,
            eve_db::AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 40_009_050,
                type_id: 1230,
                quantity: 1_000,
                initial_quantity: 1_000,
                position: (1_000.0, 0.0, 0.0),
                radius: 250.0,
            },
        );
        snap.moon_extractions.insert(
            ext_id,
            eve_db::MoonExtractionRecord {
                structure_id: 60_000_001,
                moon_id,
                extraction_start_unix: now.saturating_sub(5_000),
                chunk_arrival_unix: now.saturating_sub(3_600),
                natural_decay_unix: now + 10,
                status: "Ready".to_string(),
                auto_fracture: false,
                event_id: ext_id,
                moon_ores: vec![(1230, 1.0)],
                spawned_asteroid_ids: vec![ast_id],
            },
        );
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = sys_id;
            c.clone_grade = 1;
            c.skill_points = 5_100_000.0;
        }
        snap.skill_queues.insert(
            90_000_001,
            vec![eve_db::SkillQueueEntry {
                type_id: 3400,
                target_level: 2,
                start_sp: 250.0,
                end_sp: 1_415.0,
                duration_seconds: 120.0,
                started_at_unix: Some(now),
            }],
        );
    });

    if let Ok(mut probes) = ctx.active_moon_probes.write() {
        probes.push(crate::router::ActiveMoonSurveyProbe {
            probe_id: 900_700_010,
            probe_type_id: 30_488,
            owner_char_id: 90_000_001,
            solar_system_id: sys_id,
            hit_moon_id: Some(moon_id),
            arrival_unix_secs: now + 15,
        });
        probes.push(crate::router::ActiveMoonSurveyProbe {
            probe_id: 900_700_011,
            probe_type_id: 30_488,
            owner_char_id: 90_000_001,
            solar_system_id: sys_id,
            hit_moon_id: None,
            arrival_unix_secs: now + 15,
        });
    }

    ctx.advance_secs(20);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&jetcan_id));
        assert_eq!(snap.moon_extractions[&ext_id].status, "Decayed");
        assert!(snap.paused_skill_queues.contains(&90_000_001));
    });
    assert!(ctx
        .active_moon_probes
        .read()
        .unwrap()
        .is_empty());

    // Verify from_config & seed_demo_world_fixtures helpers
    let prod = ServerContext::new_production();
    prod.seed_demo_world_fixtures();
    assert!(prod.db.with_snapshot(|s| !s.characters.is_empty()));
}

#[test]
fn rebuild_ship_fitting_prioritizes_skilled_modules_preserves_offline_and_syncs_cached_charges() {
    let ctx = test_ctx();
    let char_id = 90_000_001;
    let ship_id = 900_810_001;
    let skilled_gun_id = 900_810_002;
    let unskilled_oversized_id = 900_810_003;
    let explicit_offline_id = 900_810_004;
    let charge_item_id = 900_810_005;

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        sk.clear();
        for tid in [3300_u32, 3301, 3302] {
            sk.insert(
                tid,
                TrainedSkillRecord {
                    type_id: tid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Skilled 125mm Gatling AutoCannon I (484) in HiSlot0
        snap.items.insert(
            skilled_gun_id,
            InventoryItemRecord {
                item_id: skilled_gun_id,
                type_id: 484,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Unskilled oversized Large Shield Extender II (3841, 175 PG > 41 PG) seeded without skills
        snap.items.insert(
            unskilled_oversized_id,
            InventoryItemRecord {
                item_id: unskilled_oversized_id,
                type_id: 3841,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Explicitly offline Damage Control II (2048, missing skill 3394)
        snap.items.insert(
            explicit_offline_id,
            InventoryItemRecord {
                item_id: explicit_offline_id,
                type_id: 2048,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            charge_item_id,
            InventoryItemRecord {
                item_id: charge_item_id,
                type_id: 185, // EMP S
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 40,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(unskilled_oversized_id, true);
        snap.module_online_states.insert(explicit_offline_id, false);
    });

    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(
        fit.modules.get(&skilled_gun_id).map(|m| (m.state, m.charge_type_id, m.charge_quantity)),
        Some((eve_dogma::ModuleState::Online, Some(185), 40)),
        "skilled turret must come online with loaded charge"
    );
    assert_eq!(
        fit.modules.get(&explicit_offline_id).map(|m| m.state),
        Some(eve_dogma::ModuleState::Offline),
        "explicitly offline missing-skill module must remain in fit.modules as Offline"
    );
    assert!(
        !fit.modules.contains_key(&unskilled_oversized_id),
        "unskilled oversized seeded module must not stay in fit.modules"
    );
    assert_eq!(
        ctx.db.with_snapshot(|s| s.module_online_states.get(&unskilled_oversized_id).copied()),
        Some(true),
        "unskilled oversized seeded module must not be auto-offlined in DB snapshot"
    );

    // Swap charge in DB snapshot without calling invalidate_ship_fitting -> cached fit updates
    ctx.store_ship_fitting(ship_id, fit);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.items.get_mut(&charge_item_id) {
            ch.type_id = 182; // Fusion S
            ch.quantity = 25;
        }
    });
    let cached_fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(
        cached_fit.modules.get(&skilled_gun_id).map(|m| (m.charge_type_id, m.charge_quantity)),
        Some((Some(182), 25)),
        "cached fitting must resync swapped slot charge"
    );

    // Remove charge item completely -> cached fit clears slot charge cleanly
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&charge_item_id);
    });
    let cleared_fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    assert_eq!(
        cleared_fit.modules.get(&skilled_gun_id).map(|m| (m.charge_type_id, m.charge_quantity)),
        Some((None, 0)),
        "cached fitting must clear removed slot charge"
    );
}

