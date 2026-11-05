use super::*;
use crate::system_actor::{
    ActiveModuleCycle, ActiveNaniteRepair, PendingAnchoringDisruptor, PendingCycleAction,
};
use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

fn make_cycle_ball(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 2_000_000.0,
        radius: 60.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: eve_db::InventoryFlag, quantity: i32) -> eve_db::InventoryItemRecord {
    eve_db::InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && matches!(flag, eve_db::InventoryFlag::HiSlot0 | eve_db::InventoryFlag::MedSlot0 | eve_db::InventoryFlag::LoSlot0),
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn make_cycle_sde() -> Arc<SdeRegistry> {
    let mut sde = SdeRegistry::with_canonical_seed();
    let add_ty = |sde: &mut SdeRegistry, tid: u32, grp: u32, cat: u32, cap: f64, vol: f64, attrs: Vec<(u16, f64)>| {
        sde.types.insert(
            tid,
            InvType {
                type_id: tid,
                group_id: grp,
                category_id: cat,
                name: format!("Item-{tid}"),
                mass: 100.0,
                volume: vol,
                capacity: cap,
                portion_size: 1,
                radius: 5.0,
                base_price: 10_000.0,
                published: true,
                market_group_id: None,
                race_id: None,
                faction_id: None,
                variation_parent_type_id: None,
                description_id: None,
                icon_id: None,
                graphic_id: None,
                sound_id: None,
                meta_group_id: None,
                meta_level: None,
                tech_level: Some(1),
                default_effect_id: None,
                description: String::new(),
                effect_ids: Vec::new(),
                default_attributes: attrs.into_iter().collect(),
            },
        );
    };
    add_ty(&mut sde, 587, 25, 6, 140.0, 27_000.0, vec![]); // Rifter
    add_ty(&mut sde, 30190, 959, 11, 0.0, 5_000.0, vec![]); // Sleeper Emergent Patroller (group 959)
    add_ty(&mut sde, 41476, 1699, 7, 0.0, 50.0, vec![(182, 24571.0)]); // Capital Remote Armor Repairer (skill 24571)
    add_ty(&mut sde, 25861, 1122, 7, 0.0, 5.0, vec![]); // Salvager I (group 1122)
    add_ty(&mut sde, 28668, 916, 8, 0.0, 0.01, vec![]); // Nanite Repair Paste
    add_ty(&mut sde, 2873, 55, 7, 2.0, 5.0, vec![(1795, 5_000.0)]); // Light Autocannon (cap 2.0)
    add_ty(&mut sde, 192, 83, 8, 0.0, 0.0025, vec![]); // Fusion S charge
    add_ty(&mut sde, 12789, 86, 8, 0.0, 0.1, vec![]); // Scorch S frequency crystal (group 86)
    add_ty(&mut sde, 40340, 1672, 7, 0.0, 25.0, vec![(54, 6_000.0), (158, 12_000.0), (20, -80.0)]);
    Arc::new(sde)
}

#[test]
fn step_rah_resonances_and_capital_remote_repair_detection_cover_all_branches() {
    // 1. Pure thermal damage shifts resonance from non-taking channels toward thermal
    let init = [0.85, 0.85, 0.85, 0.85];
    let after_therm = step_rah_resonances(init, [0.0, 500.0, 0.0, 0.0], 15.0);
    assert!(
        after_therm[3] < init[3],
        "thermal resonance (index 3) must decrease (higher resist): got {}",
        after_therm[3]
    );
    assert!(
        after_therm[0] > init[0],
        "unused EM resonance (index 0) must increase: got {}",
        after_therm[0]
    );

    // 2. Zero damage defaults to EM focus
    let after_zero = step_rah_resonances(init, [0.0, 0.0, 0.0, 0.0], 10.0);
    assert!(after_zero[0] < init[0]);

    // 3. All channels already at zero resist (1.0 resonance) -> returns unchanged
    let flat_max = [1.0, 1.0, 1.0, 1.0];
    assert_eq!(step_rah_resonances(flat_max, [100.0, 0.0, 0.0, 0.0], 10.0), flat_max);

    // 4. Capital remote repair detection by skill or high powergrid
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.insert(
        9001,
        InvType {
            type_id: 9001,
            group_id: 41,
            category_id: 7,
            name: "Cap Shield Booster".into(),
            mass: 50.0,
            volume: 50.0,
            capacity: 0.0,
            portion_size: 1,
            radius: 1.0,
            base_price: 1.0,
            published: true,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::from([(182u16, 24568.0)]),
        },
    );
    sde.types.insert(
        9002,
        InvType {
            type_id: 9002,
            group_id: 41,
            category_id: 7,
            name: "High PG Remote".into(),
            mass: 50.0,
            volume: 50.0,
            capacity: 0.0,
            portion_size: 1,
            radius: 1.0,
            base_price: 1.0,
            published: true,
            market_group_id: None,
            race_id: None,
            faction_id: None,
            variation_parent_type_id: None,
            description_id: None,
            icon_id: None,
            graphic_id: None,
            sound_id: None,
            meta_group_id: None,
            meta_level: None,
            tech_level: None,
            default_effect_id: None,
            description: String::new(),
            effect_ids: Vec::new(),
            default_attributes: HashMap::from([(30u16, 25_000.0)]),
        },
    );
    let sde_opt = Some(Arc::new(sde));
    assert!(is_capital_remote_repair_module(9001, &sde_opt));
    assert!(is_capital_remote_repair_module(9002, &sde_opt));
    assert!(!is_capital_remote_repair_module(999_999, &sde_opt));
    assert!(!is_capital_remote_repair_module(9001, &None));
}

#[test]
fn step_cycles_covers_tether_nanite_disruptors_rah_ehe_asb_aar_cap_booster_and_reload() {
    let sde_opt = Some(make_cycle_sde());
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let db_opt = Some(db.clone());
    let char_id = 90_000_001i64;
    let ship1 = 1001i64;
    let ship2 = 1002i64;

    // Seed ship modules, nanite paste, ASB charges, AAR paste, Cap Booster charges, T2 crystal & Fusion S ammo
    let mod_tether_dmg = 5001i64;
    let mod_nanite = 5002i64;
    let mod_rah = 5003i64;
    let mod_ehe = 5004i64;
    let mod_asb = 5005i64;
    let mod_aar = 5006i64;
    let mod_cap_boost = 5007i64;
    let mod_crystal_gun = 5008i64;
    let mod_proj_gun = 5009i64;
    let mod_salvager = 5010i64;
    let mod_cap_rep_1 = 5011i64;
    let mod_cap_rep_2 = 5012i64;

    db.with_mut_snapshot(|snap| {
        let mut tether_item = make_item(mod_tether_dmg, 2873, char_id, ship1, eve_db::InventoryFlag::HiSlot0, 1);
        tether_item.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 1.5);
        snap.items.insert(mod_tether_dmg, tether_item);

        let mut nanite_item = make_item(mod_nanite, 2873, char_id, ship2, eve_db::InventoryFlag::HiSlot0, 1);
        nanite_item.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 20.0);
        snap.items.insert(mod_nanite, nanite_item);

        // Cargo nanite paste for ship2 (10 units)
        snap.items.insert(6001, make_item(6001, 28668, char_id, ship2, eve_db::InventoryFlag::Cargo, 10));

        // ASB module + 1 charge in MedSlot0 on ship2
        snap.items.insert(mod_asb, make_item(mod_asb, 2873, char_id, ship2, eve_db::InventoryFlag::MedSlot0, 1));
        snap.items.insert(6002, make_item(6002, 192, char_id, ship2, eve_db::InventoryFlag::MedSlot0, 1));

        // AAR module + 4 paste in LoSlot0 on ship2
        snap.items.insert(mod_aar, make_item(mod_aar, 2873, char_id, ship2, eve_db::InventoryFlag::LoSlot0, 1));
        snap.items.insert(6003, make_item(6003, 28668, char_id, ship2, eve_db::InventoryFlag::LoSlot0, 4));

        // Cap booster + 1 charge in MedSlot1 on ship2
        snap.items.insert(mod_cap_boost, make_item(mod_cap_boost, 2873, char_id, ship2, eve_db::InventoryFlag::MedSlot1, 1));
        snap.items.insert(6004, make_item(6004, 192, char_id, ship2, eve_db::InventoryFlag::MedSlot1, 1));

        // Volatility crystal gun + 1 crystal in HiSlot1 on ship2
        snap.items.insert(mod_crystal_gun, make_item(mod_crystal_gun, 2873, char_id, ship2, eve_db::InventoryFlag::HiSlot1, 1));
        snap.items.insert(6005, make_item(6005, 12789, char_id, ship2, eve_db::InventoryFlag::HiSlot1, 1));

        // Projectile gun + 1 round in HiSlot2 + 100 rounds in Cargo for auto-reload on ship2
        snap.items.insert(mod_proj_gun, make_item(mod_proj_gun, 2873, char_id, ship2, eve_db::InventoryFlag::HiSlot2, 1));
        snap.items.insert(6006, make_item(6006, 192, char_id, ship2, eve_db::InventoryFlag::HiSlot2, 1));
        snap.items.insert(6007, make_item(6007, 192, char_id, ship2, eve_db::InventoryFlag::Cargo, 100));

        // Salvager + EHE
        snap.items.insert(mod_salvager, make_item(mod_salvager, 25861, char_id, ship2, eve_db::InventoryFlag::HiSlot3, 1));
        snap.items.insert(mod_ehe, make_item(mod_ehe, 40715, char_id, ship2, eve_db::InventoryFlag::LoSlot1, 1));
    });

    let mut bp = Ballpark::new(30_000_142);
    bp.add_ball(make_cycle_ball(ship1, 587, DVec3::ZERO));
    bp.combat_states.insert(
        ship1,
        CombatState {
            is_tethered: true,
            armor_hp: 200.0,
            max_armor_hp: 1_000.0,
            capacitor: 500.0,
            max_capacitor: 1_000.0,
            ..CombatState::default()
        },
    );
    bp.add_ball(make_cycle_ball(ship2, 587, DVec3::new(2_000.0, 0.0, 0.0)));
    bp.combat_states.insert(
        ship2,
        CombatState {
            capacitor: 100.0,
            max_capacitor: 1_000.0,
            armor_hp: 100.0,
            max_armor_hp: 1_000.0,
            ..CombatState::default()
        },
    );

    let mut nanite_repairs = vec![ActiveNaniteRepair {
        char_id,
        ship_id: ship2,
        module_id: mod_nanite,
        initial_damage_hp: 20.0,
        paste_total_needed: 4,
        paste_consumed_so_far: 0,
        duration_s: 5.0,
        elapsed_s: 0.0,
    }];
    let mut anchoring = vec![PendingAnchoringDisruptor {
        id: 7777,
        type_id: 12_198,
        position: DVec3::new(5_000.0, 0.0, 0.0),
        radius: 20_000.0,
        remaining_s: 2.0,
    }];
    let mut rack_heat = HashMap::new();
    let mut rah_dmg = HashMap::from([(ship2, [0.0, 0.0, 200.0, 0.0])]);

    let mut cycles = vec![
        // RAH + EHE on ship2
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_rah,
            is_rah: true,
            rah_shift_amount: 15.0,
            duration_s: 5.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_ehe,
            module_type_id: 40715,
            ehe_hull_resists: Some([0.1, 0.1, 0.1, 0.1]),
            duration_s: 5.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
        // ASB, AAR, CapBooster on ship2
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_asb,
            is_asb: true,
            asb_fallback_cap_need: 250.0,
            shield_boost_amount: 120.0,
            duration_s: 4.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_aar,
            is_aar: true,
            aar_paste_rate: 4,
            aar_base_armor_repair: 50.0,
            armor_repair_amount: 150.0,
            duration_s: 4.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_cap_boost,
            cap_booster_inject_amount: 400.0,
            duration_s: 4.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
        // Volatility crystal gun + Projectile auto-reload gun + Salvager
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_crystal_gun,
            module_type_id: 2873,
            linked_turrets: vec![mod_crystal_gun],
            ammo_type_id: Some(12789),
            crystal_volatility_chance: 1.0,
            crystal_volatility_damage: 1.0,
            duration_s: 5.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_proj_gun,
            module_type_id: 2873,
            linked_turrets: vec![mod_proj_gun],
            ammo_type_id: Some(192),
            duration_s: 5.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_salvager,
            module_type_id: 25861,
            target_id: Some(9001),
            duration_s: 5.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        },
        // Two Capital Remote Armor Repairers on ship1 (tests impedance ranking + mutadaptive spool + overload)
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_cap_rep_1,
            module_type_id: 41476,
            target_id: Some(ship1),
            remote_armor_amount: 100.0,
            mutadaptive_spool_step: 0.1,
            mutadaptive_spool_max: 1.5,
            is_overloaded: true,
            rack_idx: 0,
            heat_damage_per_cycle: 1.0,
            duration_s: 5.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
        ActiveModuleCycle {
            ship_id: ship2,
            owner_char_id: Some(char_id),
            module_id: mod_cap_rep_2,
            module_type_id: 41476,
            target_id: Some(ship1),
            remote_armor_amount: 100.0,
            duration_s: 5.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        },
    ];
    // Target 9001 for salvager
    bp.add_ball(make_cycle_ball(9001, 2873, DVec3::new(3_000.0, 0.0, 0.0)));

    let mut pending_actions = Vec::new();
    let destroyed = step_cycles_and_capacitor(
        &mut bp,
        &mut cycles,
        &mut anchoring,
        &mut nanite_repairs,
        &mut rack_heat,
        &mut rah_dmg,
        &HashMap::new(),
        &HashMap::new(),
        &sde_opt,
        &db_opt,
        &None,
        &Arc::new(RwLock::new(Default::default())),
        &mut pending_actions,
        5.0,
        None,
    );
    assert!(destroyed.is_empty());

    // 1. Anchoring disruptor completed and spawned warp bubble
    assert!(anchoring.is_empty());
    assert!(bp.balls.contains_key(&7777));

    // 2. Nanite repair finished and cleared module damage
    assert!(nanite_repairs.is_empty());
    db.with_snapshot(|snap| {
        assert!(!snap.items[&mod_nanite]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::DAMAGE));
        // Tethered ship1 repaired 1.5 heat damage completely
        assert!(!snap.items[&mod_tether_dmg]
            .dynamic_attributes
            .contains_key(&eve_sde::attr_ids::DAMAGE));
        // EHE module burned out and set offline
        assert_eq!(snap.module_online_states.get(&mod_ehe), Some(&false));
        // Projectile gun auto-reloaded from cargo
        assert!(snap
            .service_state_blobs
            .contains_key(&format!("auto_reloaded_event:{mod_proj_gun}")));
        assert!(snap.service_state_blobs.contains_key("module_reload_until_unix"));
        // Mutadaptive spool updated transient attr 84
        assert!(snap
            .transient_item_attributes
            .get(&mod_cap_rep_1)
            .is_some_and(|m| m.contains_key(&84)));
    });

    // 3. Ship2 RAH shifted kinetic resonance down (index 2 in combat_states)
    assert!(bp.combat_states[&ship2].armor_resonance[2] < 0.85);

    // 4. SalvageEnd emitted
    assert!(pending_actions
        .iter()
        .any(|a| matches!(a, PendingCycleAction::SalvageEnd { module_id, .. } if *module_id == mod_salvager)));

    // 5. Ship1 received impeded remote armor reps (rank 0 = 100, rank 1 = 80 -> 200 + 180 = 380)
    assert!(
        (bp.combat_states[&ship1].armor_hp - 380.0).abs() < 1.0,
        "expected ship1 armor ~380.0, got {}",
        bp.combat_states[&ship1].armor_hp
    );
}
