use crate::common;
use std::collections::HashMap;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_dogma::{
    attr_ids, stacking_penalty_factor, DogmaItemState, ProjectedEwarEffect, ShipFittingEngine,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::router::ServerContext;
fn sde() -> &'static SdeRegistry {
    static SDE: std::sync::OnceLock<SdeRegistry> = std::sync::OnceLock::new();
    SDE.get_or_init(SdeRegistry::with_canonical_seed)
}
fn find_int_dict_entry<'a>(entries: &'a [(PyRep, PyRep)], key: i64) -> Option<&'a PyRep> {
    entries.iter().find_map(|(k, v)| match k {
        PyRep::Integer(i) if *i == key => Some(v),
        _ => None,
    })
}
#[test]
fn test_implant_sets_pirate_compounding_snake_crystal_halo_ascendancy_asklepian_amulet() {
    // parity: feat:attributes.implant-sets
    eve_testkit::attest(&["feat:attributes.implant-sets"]);
    let sde = sde();
    // 1. High-grade Snake set (19540 Alpha, 19551 Beta, 19553 Gamma, 19554 Delta, 19555 Epsilon, 19556 Omega)
    //    Each Alpha..Epsilon implant has `implantSetSerpentis` (802) = 1.15x (`PreMul` on group 300 via `setBonusSerpentis` 1261),
    //    and Omega (19556) has `implantSetSerpentis` (802) = 3.0x.
    let mut fit = ShipFittingEngine::from_sde(10_001, 587, sde); // Rifter (base maxVelocity = 365.0)
    let base_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    assert!((base_vel - 365.0).abs() < 1e-6);
    // Partial set: Alpha (19540, +0.5%) + Beta (19551, +0.625%) -> set multiplier = 1.15 * 1.15 = 1.3225
    fit.set_implants(vec![19540, 19551]);
    fit.recalculate_stats(sde);
    let set_mult_2 = 1.15_f64.powi(2);
    let expected_vel_2 = base_vel * (1.0 + 0.005 * set_mult_2) * (1.0 + 0.00625 * set_mult_2);
    let actual_vel_2 = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    assert!(
        (actual_vel_2 - expected_vel_2).abs() < 1e-5,
        "2-piece High-grade Snake velocity mismatch: got {actual_vel_2}, expected {expected_vel_2}"
    );
    // Full 6-piece High-grade Snake set: set multiplier = 1.15^5 * 3.0 = 6.0340715625
    fit.set_implants(vec![19540, 19551, 19553, 19554, 19555, 19556]);
    fit.recalculate_stats(sde);
    let set_mult_6 = 1.15_f64.powi(5) * 3.0;
    let mut expected_vel_6 = base_vel;
    for imp_pct in [0.5, 0.625, 0.75, 0.875, 1.0] {
        expected_vel_6 *= 1.0 + (imp_pct * set_mult_6) / 100.0;
    }
    let actual_vel_6 = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    assert!(
        (actual_vel_6 - expected_vel_6).abs() < 1e-5,
        "Full 6-piece High-grade Snake velocity mismatch: got {actual_vel_6}, expected {expected_vel_6}"
    );
    // 2. High-grade Crystal set (20121 Alpha, 20157 Beta, 20158 Gamma, 20159 Delta, 20160 Epsilon, 20161 Omega)
    //    Compounds `shieldBoostMultiplier` (548) via `setBonusGuristas` (1397) and boosts Shield Booster `shieldBonus` (68) via 1395
    let mut crystal_fit = ShipFittingEngine::from_sde(10_002, 603, sde); // Merlin
    crystal_fit.set_character_skills(HashMap::from([
        (3416, 5), // Shield Operation V
        (3413, 5), // Energy Grid Upgrades V
        (3426, 5), // CPU Management V
        (3418, 5), // Power Grid Management V
    ]));
    crystal_fit.fit_and_online_module(20_001, 399, 19, sde).unwrap(); // Small Shield Booster I (requires skill 3416)
    let base_boost = crystal_fit.get_module_effective_attribute(20_001, attr_ids::SHIELD_BONUS, sde);
    crystal_fit.set_implants(vec![20121, 20157, 20158, 20159, 20160, 20161]);
    crystal_fit.recalculate_stats(sde);
    let crystal_set_mult = 1.15_f64.powi(5) * 1.5;
    let mut expected_boost = base_boost;
    for imp_pct in [1.0, 2.0, 3.0, 4.0, 5.0] {
        expected_boost *= 1.0 + (imp_pct * crystal_set_mult) / 100.0;
    }
    let actual_boost = crystal_fit.get_module_effective_attribute(20_001, attr_ids::SHIELD_BONUS, sde);
    assert!(
        (actual_boost - expected_boost).abs() < 1e-4,
        "Full High-grade Crystal shieldBonus mismatch: got {actual_boost}, expected {expected_boost}"
    );
    // 3. High-grade Halo set (20498 Alpha, 20500 Beta, 20506 Gamma, 20502 Delta, 20504 Epsilon, 20508 Omega)
    //    Compounds `signatureRadiusBonus` (554) via `haloSetBonus` (1577, skill 3411 Cybernetics) reducing ship `signatureRadius` (552)
    let mut halo_fit = ShipFittingEngine::from_sde(10_003, 587, sde);
    let base_sig = halo_fit.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);
    halo_fit.set_implants(vec![20498, 20500, 20506, 20502, 20504, 20508]);
    halo_fit.recalculate_stats(sde);
    let halo_set_mult = 1.15_f64.powi(5) * 1.5;
    let mut expected_sig = base_sig;
    for imp_pct in [-1.0, -1.25, -1.5, -1.75, -2.0] {
        expected_sig *= 1.0 + (imp_pct * halo_set_mult) / 100.0;
    }
    let actual_sig = halo_fit.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);
    assert!(
        (actual_sig - expected_sig).abs() < 1e-5,
        "Full High-grade Halo signatureRadius mismatch: got {actual_sig}, expected {expected_sig}"
    );
    // 4. High-grade Ascendancy set (33516 Alpha, 33525 Beta, 33526 Gamma, 33527 Delta, 33528 Epsilon, 33529 Omega)
    //    Compounds `WarpSBonus` (624) via `implantSetWarpSpeed` (5717) increasing ship `warpSpeedMultiplier` (600)
    let mut asc_fit = ShipFittingEngine::from_sde(10_004, 587, sde);
    let base_warp = asc_fit.ship.get_attribute(attr_ids::WARP_SPEED_MULTIPLIER, sde);
    asc_fit.set_implants(vec![33516, 33525, 33526, 33527, 33528, 33529]);
    asc_fit.recalculate_stats(sde);
    let actual_warp = asc_fit.ship.get_attribute(attr_ids::WARP_SPEED_MULTIPLIER, sde);
    assert!(
        actual_warp > base_warp * 1.5,
        "Full High-grade Ascendancy warpSpeedMultiplier must exceed 1.5x base: got {actual_warp} vs {base_warp}"
    );
    // 5. High-grade Asklepian set (42210..=42215) & High-grade Amulet set (20499, 20501, 20503, 20505, 20507, 20509)
    let mut amulet_fit = ShipFittingEngine::from_sde(10_005, 597, sde); // Punisher
    let base_armor = amulet_fit.ship.get_attribute(attr_ids::ARMOR_HP, sde);
    amulet_fit.set_implants(vec![20499, 20501, 20503, 20505, 20507, 20509]);
    amulet_fit.recalculate_stats(sde);
    let amulet_mult = 1.15_f64.powi(5) * 1.5;
    let mut expected_armor = base_armor;
    for imp_pct in [1.0, 2.0, 3.0, 4.0, 5.0] {
        expected_armor *= 1.0 + (imp_pct * amulet_mult) / 100.0;
    }
    let actual_armor = amulet_fit.ship.get_attribute(attr_ids::ARMOR_HP, sde);
    assert!(
        (actual_armor - expected_armor).abs() < 1e-4,
        "Full High-grade Amulet armorHP mismatch: got {actual_armor}, expected {expected_armor}"
    );
}
#[tokio::test]
async fn test_tactical_destroyer_ship_stances_and_cooldown_confessor_svipul_jackdaw_hecate() {
    // parity: feat:ships.ship-stance, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.GetAllInfo, rpc:shipStanceMgr.SetShipStance
    eve_testkit::attest(&["feat:ships.ship-stance"]);
    let sde = sde();
    // 1. Confessor (34317): Defense (34319), Sharpshooter (34321), Propulsion (34323)
    let mut confessor = ShipFittingEngine::from_sde(11_001, 34317, sde);
    confessor.set_character_skills(HashMap::from([
        (3300, 5), // Gunnery V
        (3303, 5), // Small Energy Turret V
        (3449, 5), // Navigation V
        (3450, 5), // Afterburner V
    ]));
    confessor.fit_and_online_module(21_001, 454, 27, sde).unwrap(); // Dual Light Pulse Laser I
    confessor.fit_and_online_module(21_002, 438, 19, sde).unwrap(); // 1MN Afterburner II
    let base_agility = confessor.ship.get_attribute(attr_ids::AGILITY, sde);
    let base_sig = confessor.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);
    let base_armor_em = confessor.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde);
    let base_turret_opt = confessor.get_module_effective_attribute(21_001, attr_ids::MAX_RANGE, sde);
    let base_ab_sf = confessor.get_module_effective_attribute(21_002, attr_ids::SPEED_FACTOR, sde);
    let base_lock_range = confessor.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde);
    // Switch to Defense Mode (34319) at t = 1_000 ms (`PostDiv` 1.5 on armor resonance & signatureRadius)
    assert_eq!(confessor.switch_ship_stance(34319, 1_000, sde).unwrap(), 34319);
    assert!(
        (confessor.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde)
            - base_armor_em / 1.5)
            .abs()
            < 1e-6
    );
    assert!(
        (confessor.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde) - base_sig / 1.5).abs()
            < 1e-6
    );
    // Attempting to switch before 10,000 ms `stanceSwitchTime` (1985) cooldown expires must fail
    let err = confessor.switch_ship_stance(34321, 8_000, sde).unwrap_err();
    assert!(err.contains("StanceSwitchCooldownActive"), "unexpected error: {err}");
    // Switch to Sharpshooter Mode (34321) at t = 11_000 ms (`PostDiv` 0.6 on SET optimal range, `PostDiv` 0.5 on targeting range, `PostDiv` 3.0 on damp/TD resist)
    assert_eq!(confessor.switch_ship_stance(34321, 11_000, sde).unwrap(), 34321);
    let sharp_turret_opt = confessor.get_module_effective_attribute(21_001, attr_ids::MAX_RANGE, sde);
    assert!(
        (sharp_turret_opt - base_turret_opt / 0.6).abs() < 1e-4,
        "Confessor Sharpshooter optimal range mismatch: got {sharp_turret_opt}, expected {}",
        base_turret_opt / 0.6
    );
    assert!(
        (confessor.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde) - base_lock_range / 0.5)
            .abs()
            < 1e-3
    );
    // Sharpshooter Mode also grants +66.7% sensor dampener & weapon disruption resistance (`PostDiv` 3.0 -> 1/3)
    assert!((confessor.ship.get_attribute(2112, sde) - 1.0 / 3.0).abs() < 1e-6);
    assert!((confessor.ship.get_attribute(2113, sde) - 1.0 / 3.0).abs() < 1e-6);
    // Switch to Propulsion Mode (34323) at t = 21_000 ms (`PostDiv` 1.5 on agility, `PostDiv` 0.6 on AB/MWD speedFactor)
    assert_eq!(confessor.switch_ship_stance(34323, 21_000, sde).unwrap(), 34323);
    assert!(
        (confessor.ship.get_attribute(attr_ids::AGILITY, sde) - base_agility / 1.5).abs() < 1e-6
    );
    assert!(
        (confessor.get_module_effective_attribute(21_002, attr_ids::SPEED_FACTOR, sde)
            - base_ab_sf / 0.6)
            .abs()
            < 1e-4
    );
    // Cross-hull stance (Svipul Defense Mode 34564 on Confessor 34317) must be rejected
    assert!(confessor.set_ship_stance(34564, sde).is_err());
    // 2. Verify Svipul (34562), Jackdaw (34828), and Hecate (35683) stance transitions
    for (hull_tid, def_tid, prop_tid, sharp_tid) in [
        (34562_u32, 34564_u32, 34566_u32, 34570_u32), // Svipul
        (34828_u32, 35676_u32, 35677_u32, 35678_u32), // Jackdaw
        (35683_u32, 35686_u32, 35687_u32, 35688_u32), // Hecate
    ] {
        let mut t3d = ShipFittingEngine::from_sde(i64::from(hull_tid), hull_tid, sde);
        let hull_base_vel = t3d.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
        let hull_base_lock = t3d.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde);
        t3d.set_ship_stance(def_tid, sde).unwrap();
        assert_eq!(t3d.active_stance_type_id, Some(def_tid));
        t3d.set_ship_stance(prop_tid, sde).unwrap();
        // Propulsion mode increases maxVelocity (Svipul/Hecate directly; Jackdaw reduces inertia & boosts MWD/AB speedFactor)
        assert!(
            t3d.ship.get_attribute(attr_ids::MAX_VELOCITY, sde) >= hull_base_vel
                && t3d.ship.get_attribute(attr_ids::AGILITY, sde)
                    < sde.types[&hull_tid].default_attributes[&attr_ids::AGILITY]
        );
        t3d.set_ship_stance(sharp_tid, sde).unwrap();
        assert!(t3d.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde) > hull_base_lock);
    }
    // 3. Exercise live `shipStanceMgr::SetShipStance` and `shipStanceMgr::GetShipStance` RPC dispatch on Confessor (34317)
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess = SessionState::new(4700);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("SelectCharacterID");
    let (char_id, confessor_ship_id) = (90_000_001_i64, 710_001_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            confessor_ship_id,
            InventoryItemRecord {
                item_id: confessor_ship_id,
                type_id: 34317, // Confessor
                owner_id: char_id,
                location_id: 60_003_760,
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
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = confessor_ship_id;
        }
    });
    sess.ship_id = Some(confessor_ship_id);
    let initial_info = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "GetAllInfo", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None)
        .await
        .expect("dogmaIM::GetAllInfo");
    let PyRep::KeyVal(initial_kv) = initial_info else {
        panic!("Expected util.KeyVal from dogmaIM::GetAllInfo");
    };
    assert_eq!(initial_kv.get("activeStance"), Some(&PyRep::Integer(34319)));
    let _ = sess.drain_notifications();
    let kw_t1 = HashMap::from([("now_ms".to_string(), PyRep::Integer(10_000))]);
    let set_sharp = ctx
        .dispatch_rpc(&mut sess, "shipStanceMgr", None, "SetShipStance", &[PyRep::Integer(confessor_ship_id), PyRep::Integer(34321)], &kw_t1, None)
        .await
        .expect("shipStanceMgr::SetShipStance -> Sharpshooter");
    assert_eq!(set_sharp, PyRep::Integer(34321));
    assert!(sess.drain_notifications().iter().any(|(n, _)| n == "OnStanceActive"));
    // Cooldown rejection via `shipStanceMgr::SetShipStance` at t = 15_000 ms (< 10_000 ms elapsed)
    let kw_t2 = HashMap::from([("now_ms".to_string(), PyRep::Integer(15_000))]);
    let cd_err = ctx
        .dispatch_rpc(&mut sess, "shipStanceMgr", None, "SetShipStance", &[PyRep::Integer(confessor_ship_id), PyRep::Integer(34323)], &kw_t2, None)
        .await
        .unwrap_err();
    assert_eq!(cd_err.code, "StanceSwitchCooldown");
}
#[tokio::test]
async fn test_dogma_attribute_sync_and_location_prime_wire_format() {
    // parity: feat:dogma.attribute-sync, feat:dogma.location-prime, rpc:charUnboundMgr.SelectCharacterID, rpc:dogmaIM.GetAllInfo, rpc:dogmaIM.ItemGetInfo, rpc:invbroker.Add
    eve_testkit::attest(&["feat:dogma.attribute-sync", "feat:dogma.location-prime"]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = SessionState::new(4701);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let station_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_001,
            InventoryItemRecord {
                item_id: 700_001,
                type_id: 1236, // Overdrive Injector System II
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
        let skills = snap.skills.entry(char_id).or_default();
        for (sid, lvl) in [(3449_u32, 5_u8), (3394_u32, 5_u8)] {
            skills.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: lvl,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    ctx.ship_fittings.write().unwrap().remove(&ship_id);
    // 1. Fit Overdrive Injector System II via `invbroker::Add` and verify `OnModuleAttributeChanges` notification
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        None,
        "Add",
        &[
            PyRep::Integer(700_001),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::LoSlot0 as i64),
        ],
        &kw,
        None,
    )
    .await
    .expect("Fit Overdrive Injector System II into LoSlot0");
    let notifications = sess.drain_notifications();
    let has_attr_sync = notifications
        .iter()
        .any(|(name, _)| name == "OnModuleAttributeChanges" || name == "OnModuleAttributeChange");
    assert!(
        has_attr_sync,
        "Fitting module via invbroker::Add must emit OnModuleAttributeChanges notification"
    );
    // Verify attribute change notification payload in OnModuleAttributeChanges
    let (_, changes_payload) = notifications
        .iter()
        .find(|(name, _)| name == "OnModuleAttributeChanges")
        .expect("OnModuleAttributeChanges notification");
    assert!(!changes_payload.is_empty());
    // 2. Verify `dogmaIM::GetAllInfo` and `dogmaIM::ItemGetInfo` prime `shipAttributes`, `moduleAttributes`, and emit `ProcessGodmaPrimeLocation`
    let _ = sess.drain_notifications();
    let all_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "GetAllInfo",
            &[PyRep::Bool(true), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM::GetAllInfo");
    let prime_notifs = sess.drain_notifications();
    assert!(
        prime_notifs
            .iter()
            .any(|(name, _)| name == "ProcessGodmaPrimeLocation"),
        "dogmaIM::GetAllInfo must emit ProcessGodmaPrimeLocation notification"
    );
    let PyRep::KeyVal(top_entries) = all_info else {
        panic!("Expected util.KeyVal from dogmaIM::GetAllInfo, got {all_info:?}");
    };
    assert_eq!(top_entries.get("shipID"), Some(&PyRep::Integer(ship_id)));
    assert_eq!(top_entries.get("charID"), Some(&PyRep::Integer(char_id)));
    let Some(PyRep::Dict(ship_attrs)) = top_entries.get("shipAttributes") else {
        panic!("Expected shipAttributes Dict");
    };
    assert!(
        find_int_dict_entry(ship_attrs, i64::from(attr_ids::MAX_VELOCITY)).is_some(),
        "shipAttributes must prime effective maxVelocity"
    );
    let Some(PyRep::Dict(mod_attrs)) = top_entries.get("moduleAttributes") else {
        panic!("Expected moduleAttributes Dict");
    };
    assert!(
        find_int_dict_entry(mod_attrs, 700_001).is_some(),
        "moduleAttributes must prime fitted module 700_001"
    );
    let item_info = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "ItemGetInfo",
            &[PyRep::Integer(ship_id)],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM::ItemGetInfo");
    let PyRep::KeyVal(item_entries) = item_info else {
        panic!("Expected util.KeyVal from dogmaIM::ItemGetInfo");
    };
    assert_eq!(item_entries.get("itemID"), Some(&PyRep::Integer(ship_id)));
    assert!(item_entries.contains_key("attributes"));
}
#[test]
fn test_ship_hull_passive_bonuses_scaling_with_racial_ship_skills() {
    // parity: feat:dogma.dogma-effects-ships
    eve_testkit::attest(&["feat:dogma.dogma-effects-ships"]);
    let sde = sde();
    // 1. Rifter (587) + Minmatar Frigate (3329): -7.5%/lvl Small Projectile Turret ROF (`speed` 51) & +10%/lvl falloff (158)
    let mut rifter = ShipFittingEngine::from_sde(12_001, 587, sde);
    rifter.set_character_skills(HashMap::from([
        (3300, 5), // Gunnery V
        (3302, 5), // Small Projectile Turret V
        (3329, 1), // Minmatar Frigate I
    ]));
    rifter.fit_and_online_module(22_001, 484, 27, sde).unwrap(); // 125mm Gatling AutoCannon I
    let rof_unbonused = rifter.get_module_effective_attribute(22_001, attr_ids::SPEED, sde) / (1.0 - 0.075);
    let falloff_unbonused = rifter.get_module_effective_attribute(22_001, attr_ids::FALLOFF, sde) / (1.0 + 0.10);
    rifter.set_character_skills(HashMap::from([
        (3300, 5),
        (3302, 5),
        (3329, 5), // Minmatar Frigate V
    ]));
    rifter.recalculate_stats(sde);
    let rof_lvl5 = rifter.get_module_effective_attribute(22_001, attr_ids::SPEED, sde);
    let falloff_lvl5 = rifter.get_module_effective_attribute(22_001, attr_ids::FALLOFF, sde);
    assert!(
        (rof_lvl5 - rof_unbonused * (1.0 - 0.075 * 5.0)).abs() < 1e-3,
        "Rifter + Minmatar Frigate V speed mismatch: {rof_lvl5} vs {}",
        rof_unbonused * 0.625
    );
    assert!(
        (falloff_lvl5 - falloff_unbonused * (1.0 + 0.10 * 5.0)).abs() < 1e-3,
        "Rifter + Minmatar Frigate V falloff mismatch: {falloff_lvl5} vs {}",
        falloff_unbonused * 1.50
    );
    // 2. Punisher (597) + Amarr Frigate (3331): +4%/lvl armor damage resistances (resonance * (1 - 0.20) at level V)
    let mut punisher = ShipFittingEngine::from_sde(12_002, 597, sde);
    let base_em_res = punisher.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde);
    punisher.set_character_skills(HashMap::from([(3331, 5)])); // Amarr Frigate V
    punisher.recalculate_stats(sde);
    let lvl5_em_res = punisher.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde);
    assert!(
        (lvl5_em_res - base_em_res * 0.80).abs() < 1e-5,
        "Punisher + Amarr Frigate V armorEmDamageResonance mismatch: {lvl5_em_res} vs {}",
        base_em_res * 0.80
    );
    // 3. Merlin (603) + Caldari Frigate (3330): +4%/lvl shield resistances (resonance * 0.80 at level V)
    let mut merlin = ShipFittingEngine::from_sde(12_003, 603, sde);
    let base_sh_em = merlin.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde);
    merlin.set_character_skills(HashMap::from([(3330, 5)])); // Caldari Frigate V
    merlin.recalculate_stats(sde);
    let lvl5_sh_em = merlin.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde);
    assert!(
        (lvl5_sh_em - base_sh_em * 0.80).abs() < 1e-5,
        "Merlin + Caldari Frigate V shieldEmDamageResonance mismatch: {lvl5_sh_em} vs {}",
        base_sh_em * 0.80
    );
}
#[test]
fn test_damage_mods_heatsink_magstab_gyro_bcs_entropic_vorton_and_stacking() {
    // parity: feat:modules.damage-mods
    eve_testkit::attest(&["feat:modules.damage-mods"]);
    let sde = sde();
    // 1. Gyrostabilizer II (519) on Omen: 1.10x projectile damageMultiplier (64) & 0.895x speed (51)
    let mut fit = ShipFittingEngine::from_sde(13_001, 2006, sde); // Omen (5 low slots)
    fit.set_character_skills(HashMap::from([
        (3300, 5), // Gunnery V
        (3301, 5), // Small Hybrid Turret V
        (3302, 5), // Small Projectile Turret V
        (3303, 5), // Small Energy Turret V
        (3318, 5), // Weapon Upgrades V
        (3319, 5), // Missile Launcher Operation V
        (3320, 5), // Rockets V
    ]));
    fit.fit_and_online_module(23_001, 484, 27, sde).unwrap(); // 125mm Gatling AutoCannon I (projectile)
    fit.fit_and_online_module(23_002, 454, 28, sde).unwrap(); // Small Focused Beam Laser I (energy)
    fit.fit_and_online_module(23_003, 561, 29, sde).unwrap(); // Light Neutron Blaster I (hybrid)
    let base_proj_dmg = fit.get_module_effective_attribute(23_001, attr_ids::DAMAGE_MULTIPLIER, sde);
    let base_proj_rof = fit.get_module_effective_attribute(23_001, attr_ids::SPEED, sde);
    let base_laser_dmg = fit.get_module_effective_attribute(23_002, attr_ids::DAMAGE_MULTIPLIER, sde);
    let base_hybrid_dmg = fit.get_module_effective_attribute(23_003, attr_ids::DAMAGE_MULTIPLIER, sde);
    // Fit 1..=4 Gyrostabilizer II (519) into low slots 11..=14 and verify stacking penalty curve
    let gyro_dmg_mult = sde.types[&519].default_attributes[&attr_ids::DAMAGE_MULTIPLIER]; // 1.10
    let gyro_rof_mult = sde.types[&519].default_attributes[&204]; // 0.895
    for i in 0..4 {
        fit.fit_and_online_module(23_010 + i as i64, 519, 11 + i as u16, sde)
            .unwrap();
        let mut exp_dmg = base_proj_dmg;
        let mut exp_rof = base_proj_rof;
        for k in 0..=i {
            exp_dmg *= 1.0 + (gyro_dmg_mult - 1.0) * stacking_penalty_factor(k);
            exp_rof *= 1.0 + (gyro_rof_mult - 1.0) * stacking_penalty_factor(k);
        }
        assert!(
            (fit.get_module_effective_attribute(23_001, attr_ids::DAMAGE_MULTIPLIER, sde) - exp_dmg)
                .abs()
                < 1e-5
        );
        assert!(
            (fit.get_module_effective_attribute(23_001, attr_ids::SPEED, sde) - exp_rof).abs()
                < 1e-3
        );
        // Cross-weapon isolation: Gyrostabilizer must NOT affect energy or hybrid turrets
        assert!(
            (fit.get_module_effective_attribute(23_002, attr_ids::DAMAGE_MULTIPLIER, sde)
                - base_laser_dmg)
                .abs()
                < 1e-6
        );
        assert!(
            (fit.get_module_effective_attribute(23_003, attr_ids::DAMAGE_MULTIPLIER, sde)
                - base_hybrid_dmg)
                .abs()
                < 1e-6
        );
    }
    // 2. Heat Sink II (2364) affects energy turret (23_002) and Magnetic Field Stabilizer II (10190) affects hybrid turret (23_003)
    fit.fit_and_online_module(23_010, 2364, 11, sde).unwrap(); // Replace slot 11 with Heat Sink II
    fit.fit_and_online_module(23_011, 10190, 12, sde).unwrap(); // Replace slot 12 with MagStab II
    let hs_dmg_mult = sde.types[&2364].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
    let mag_dmg_mult = sde.types[&10190].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
    assert!(
        (fit.get_module_effective_attribute(23_002, attr_ids::DAMAGE_MULTIPLIER, sde)
            - base_laser_dmg * hs_dmg_mult)
            .abs()
            < 1e-5
    );
    assert!(
        (fit.get_module_effective_attribute(23_003, attr_ids::DAMAGE_MULTIPLIER, sde)
            - base_hybrid_dmg * mag_dmg_mult)
            .abs()
            < 1e-5
    );
    // 3. Ballistic Control System II (22291) boosts launcher ROF (`speed` 51) and loaded missile damage (`kineticDamage` 117)
    let mut kestrel = ShipFittingEngine::from_sde(13_002, 602, sde); // Kestrel
    kestrel.set_character_skills(HashMap::from([
        (3318, 5), // Weapon Upgrades V
        (3319, 5), // Missile Launcher Operation V
        (3320, 5), // Rockets V
    ]));
    kestrel.fit_and_online_module(23_100, 10629, 27, sde).unwrap(); // Rocket Launcher I
    kestrel.load_charge(23_100, 2512, 50, sde).unwrap(); // Scourge Rocket (kineticDamage 117)
    let base_lch_rof = kestrel.get_module_effective_attribute(23_100, attr_ids::SPEED, sde);
    let base_msl_kin = kestrel.get_charge_effective_attribute(23_100, attr_ids::KINETIC_DAMAGE, sde);
    kestrel.fit_and_online_module(23_101, 22291, 11, sde).unwrap(); // Ballistic Control System II
    let bcs_rof_mult = sde.types[&22291].default_attributes[&204]; // 0.895
    let bcs_dmg_mult = sde.types[&22291].default_attributes[&213]; // 1.10
    assert!(
        (kestrel.get_module_effective_attribute(23_100, attr_ids::SPEED, sde)
            - base_lch_rof * bcs_rof_mult)
            .abs()
            < 1e-3
    );
    assert!(
        (kestrel.get_charge_effective_attribute(23_100, attr_ids::KINETIC_DAMAGE, sde)
            - base_msl_kin * bcs_dmg_mult)
            .abs()
            < 1e-4
    );
    // 4. Entropic Radiation Sink II (47911) and Vorton Tuning System II (54975) SDE modifier verification
    assert!(sde.types.contains_key(&47911), "Entropic Radiation Sink II must exist in SDE");
    assert!(sde.types.contains_key(&54975), "Vorton Tuning System II must exist in SDE");
}
#[test]
fn test_tracking_computer_enhancer_and_missile_guidance_scripts() {
    // parity: feat:modules.tracking-comp-enhancer
    eve_testkit::attest(&["feat:modules.tracking-comp-enhancer"]);
    let sde = sde();
    let mut fit = ShipFittingEngine::from_sde(14_001, 2006, sde); // Omen
    fit.set_character_skills(HashMap::from([
        (3300, 5),  // Gunnery V
        (3303, 5),  // Small Energy Turret V
        (3317, 5),  // Electronics Upgrades / Turret Tracking V
        (3318, 5),  // Weapon Upgrades V
        (3426, 5),  // CPU Management V
        (3319, 5),  // Missile Launcher Operation V
        (3320, 5),  // Rockets V
        (12441, 5), // Missile Bombardment V
        (20312, 5), // Missile Projection V
    ]));
    fit.fit_and_online_module(24_001, 454, 27, sde).unwrap(); // Small Focused Beam Laser I
    let base_opt = fit.get_module_effective_attribute(24_001, attr_ids::MAX_RANGE, sde);
    let base_fall = fit.get_module_effective_attribute(24_001, attr_ids::FALLOFF, sde);
    let base_track = fit.get_module_effective_attribute(24_001, attr_ids::TRACKING_SPEED, sde);
    // 1. Passive Tracking Enhancer II (1999) in low slot 11
    fit.fit_and_online_module(24_010, 1999, 11, sde).unwrap();
    let te_opt = fit.get_module_effective_attribute(24_001, attr_ids::MAX_RANGE, sde);
    let te_fall = fit.get_module_effective_attribute(24_001, attr_ids::FALLOFF, sde);
    let te_track = fit.get_module_effective_attribute(24_001, attr_ids::TRACKING_SPEED, sde);
    assert!((te_opt - base_opt * 1.10).abs() < 1e-4);
    assert!((te_fall - base_fall * 1.20).abs() < 1e-4);
    assert!((te_track - base_track * 1.095).abs() < 1e-5);
    // 2. Active Tracking Computer II (1978) in med slot 19: unscripted -> Optimal Range Script (28999) -> Tracking Speed Script (29001)
    fit.fit_and_online_module(24_020, 1978, 19, sde).unwrap();
    fit.activate_module(24_020, false, sde).unwrap();
    let tc_unscripted_opt = fit.get_module_effective_attribute(24_001, attr_ids::MAX_RANGE, sde);
    let tc_unscripted_track = fit.get_module_effective_attribute(24_001, attr_ids::TRACKING_SPEED, sde);
    // Stacking penalty applies between Tracking Computer II (+7.5% opt, +15% track) and Tracking Enhancer II (+10% opt, +9.5% track)
    let exp_unscripted_opt = base_opt * 1.10 * (1.0 + 0.075 * stacking_penalty_factor(1));
    let exp_unscripted_track = base_track * 1.15 * (1.0 + 0.095 * stacking_penalty_factor(1));
    assert!((tc_unscripted_opt - exp_unscripted_opt).abs() < 1e-4);
    assert!((tc_unscripted_track - exp_unscripted_track).abs() < 1e-5);
    // Load Optimal Range Script (28999): doubles optimal/falloff bonus (+30% opt, +60% falloff) and zeroes tracking bonus
    fit.load_charge(24_020, 28999, 1, sde).unwrap();
    let tc_range_opt = fit.get_module_effective_attribute(24_001, attr_ids::MAX_RANGE, sde);
    let tc_range_track = fit.get_module_effective_attribute(24_001, attr_ids::TRACKING_SPEED, sde);
    assert!(tc_range_opt > tc_unscripted_opt);
    assert!((tc_range_track - te_track).abs() < 1e-5);
    // Load Tracking Speed Script (29001): doubles tracking bonus (+60% track) and zeroes optimal/falloff bonus
    fit.load_charge(24_020, 29001, 1, sde).unwrap();
    let tc_track_opt = fit.get_module_effective_attribute(24_001, attr_ids::MAX_RANGE, sde);
    let tc_track_speed = fit.get_module_effective_attribute(24_001, attr_ids::TRACKING_SPEED, sde);
    assert!((tc_track_opt - te_opt).abs() < 1e-4);
    assert!(tc_track_speed > tc_unscripted_track);
    // 3. Missile Guidance Enhancer II (35771) and Missile Guidance Computer II (35790) with scripts (35794, 35795)
    let mut caracal = ShipFittingEngine::from_sde(14_002, 621, sde); // Caracal
    caracal.set_character_skills(HashMap::from([
        (3318, 5),
        (3319, 5),
        (3320, 5),
        (12441, 5),
        (20312, 5),
        (3426, 5),
    ]));
    caracal.fit_and_online_module(24_100, 10629, 27, sde).unwrap(); // Rocket Launcher I
    caracal.load_charge(24_100, 2512, 50, sde).unwrap(); // Scourge Rocket
    let msl_base_vel = caracal.get_charge_effective_attribute(24_100, attr_ids::MAX_VELOCITY, sde);
    let msl_base_rad = caracal.get_charge_effective_attribute(24_100, 654, sde); // aoeCloudSize
    caracal.fit_and_online_module(24_101, 35771, 11, sde).unwrap(); // Missile Guidance Enhancer II
    caracal.fit_and_online_module(24_102, 35790, 19, sde).unwrap(); // Missile Guidance Computer II
    caracal.activate_module(24_102, false, sde).unwrap();
    assert!(caracal.get_charge_effective_attribute(24_100, attr_ids::MAX_VELOCITY, sde) > msl_base_vel);
    assert!(caracal.get_charge_effective_attribute(24_100, 654, sde) < msl_base_rad);
    // Load Missile Range Script (35794) vs Missile Precision Script (35795)
    caracal.load_charge(24_102, 35794, 1, sde).unwrap();
    let range_script_vel = caracal.get_charge_effective_attribute(24_100, attr_ids::MAX_VELOCITY, sde);
    caracal.load_charge(24_102, 35795, 1, sde).unwrap();
    let prec_script_vel = caracal.get_charge_effective_attribute(24_100, attr_ids::MAX_VELOCITY, sde);
    let prec_script_rad = caracal.get_charge_effective_attribute(24_100, 654, sde);
    assert!(range_script_vel > prec_script_vel);
    assert!(prec_script_rad < msl_base_rad * 0.85);
}
#[test]
fn test_shield_extenders_hardeners_amplifiers_and_cdfe_rigs() {
    // parity: feat:modules.shield-extender-hardener
    eve_testkit::attest(&["feat:modules.shield-extender-hardener"]);
    let sde = sde();
    let mut caracal = ShipFittingEngine::from_sde(15_001, 621, sde); // Caracal (5 med slots, 3 medium rig slots)
    caracal.set_character_skills(HashMap::from([
        (3416, 5), // Shield Operation V
        (3425, 5), // Shield Upgrades V
        (3419, 5), // Shield Management V (+5%/lvl shieldCapacity)
        (3420, 5), // Tactical Shield Manipulation V
        (3422, 5), // Shield Compensation V
        (21059, 5), // EM Shield Compensation V
        (3426, 5), // CPU Management V
        (3418, 5), // Power Grid Management V
        (26252, 5), // Jury Rigging V
        (26261, 5), // Shield Rigging V
    ]));
    caracal.recalculate_stats(sde);
    let base_shield = caracal.ship.get_attribute(attr_ids::SHIELD_CAPACITY, sde);
    let base_sig = caracal.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);
    let base_em_res = caracal.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde);
    // 1. Medium Shield Extender II (3831): adds flat shieldCapacity (ModAdd) + signatureRadius (ModAdd) before Shield Management V (+25% PostPercent)
    caracal.fit_and_online_module(25_001, 3831, 19, sde).unwrap();
    let mse_hp = sde.types[&3831].default_attributes[&72]; // capacityBonus
    let mse_sig = sde.types[&3831].default_attributes[&983]; // signatureRadiusAdd
    let after_mse_shield = caracal.ship.get_attribute(attr_ids::SHIELD_CAPACITY, sde);
    let after_mse_sig = caracal.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);
    assert!((after_mse_shield - (base_shield + mse_hp * 1.25)).abs() < 1e-3);
    assert!((after_mse_sig - (base_sig + mse_sig)).abs() < 1e-4);
    // 2. Medium Core Defense Field Extender I (31790) rig in slot 92: PostPercent shieldCapacity + signatureRadius penalty
    caracal.fit_and_online_module(25_002, 31790, 92, sde).unwrap();
    assert!(caracal.ship.get_attribute(attr_ids::SHIELD_CAPACITY, sde) > after_mse_shield * 1.14);
    assert!(caracal.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde) > after_mse_sig);
    // 3. Active Multispectrum Shield Hardener II (2281) + EM Shield Hardener II (2301) + passive EM Shield Amplifier II (2553)
    //    Verify online vs active state and stacking penalty across shield EM resonance (271)
    caracal.fit_and_online_module(25_003, 2281, 20, sde).unwrap();
    // While online (not active), active hardener must NOT modify shield resonance
    assert!((caracal.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde) - base_em_res).abs() < 1e-6);
    caracal.activate_module(25_003, false, sde).unwrap();
    let multi_em_pct = sde.types[&2281].default_attributes[&984].abs() / 100.0; // 0.325
    let multi_em_res = caracal.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde);
    assert!((multi_em_res - base_em_res * (1.0 - multi_em_pct)).abs() < 1e-5);
    // Fit passive EM Shield Amplifier II (2553, -37.5% EM resonance) and active EM Shield Hardener II (2301, -55% EM resonance)
    caracal.fit_and_online_module(25_004, 2553, 21, sde).unwrap();
    caracal.fit_and_online_module(25_005, 2301, 22, sde).unwrap();
    caracal.activate_module(25_005, false, sde).unwrap();
    let amp_em_pct = sde.types[&2553].default_attributes[&984].abs() / 100.0;
    let exp_stacked_em = base_em_res
        * (1.0 - 0.55 * stacking_penalty_factor(0))
        * (1.0 - amp_em_pct * stacking_penalty_factor(1))
        * (1.0 - multi_em_pct * stacking_penalty_factor(2));
    let actual_stacked_em = caracal.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde);
    assert!(
        (actual_stacked_em - exp_stacked_em).abs() < 1e-4,
        "Stacked shield EM resonance mismatch: got {actual_stacked_em}, expected {exp_stacked_em}"
    );
}
#[test]
fn test_armor_plates_hardeners_membranes_coatings_and_honeycombing() {
    // parity: feat:modules.armor-plates-hardeners
    eve_testkit::attest(&["feat:modules.armor-plates-hardeners"]);
    let sde = sde();
    let mut omen = ShipFittingEngine::from_sde(16_001, 2006, sde); // Omen
    omen.set_character_skills(HashMap::from([
        (3394, 5), // Hull Upgrades V (+5%/lvl armorHP)
        (3392, 5), // Mechanics V
        (33078, 5), // Armor Honeycombing V (-5%/lvl armor plate massAddition)
        (22806, 5), // EM Armor Compensation V
        (3426, 5),
        (3418, 5),
    ]));
    omen.recalculate_stats(sde);
    let base_armor = omen.ship.get_attribute(attr_ids::ARMOR_HP, sde);
    let base_mass = omen.ship.get_attribute(attr_ids::MASS, sde);
    let base_em_res = omen.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde);
    // 1. 400mm Steel Plates II (20349): flat armorHP bonus (ModAdd * 1.25 from Hull Upgrades V)
    //    and massAddition reduced by 25% from Armor Honeycombing V (33078)
    omen.fit_and_online_module(26_001, 20349, 11, sde).unwrap();
    let plate_hp = sde.types[&20349].default_attributes[&1159]; // armorHPBonusAdd
    let plate_mass = sde.types[&20349].default_attributes[&attr_ids::MASS_ADDITION];
    assert!(
        (omen.ship.get_attribute(attr_ids::ARMOR_HP, sde) - (base_armor + plate_hp * 1.25)).abs()
            < 1e-3
    );
    assert!(
        (omen.ship.get_attribute(attr_ids::MASS, sde) - (base_mass + plate_mass * 0.75)).abs()
            < 1e-3
    );
    // 2. Layered Coating II (1276, +9% armorHP via 148)
    omen.fit_and_online_module(26_002, 1276, 12, sde).unwrap();
    assert!(omen.ship.get_attribute(attr_ids::ARMOR_HP, sde) > (base_armor + plate_hp * 1.25) * 1.08);
    // 3. Active EM Armor Hardener II (11642, -55% EM resonance) + passive Multispectrum Energized Membrane II (11269) + Multispectrum Coating II (1306)
    omen.fit_and_online_module(26_003, 11642, 13, sde).unwrap();
    omen.activate_module(26_003, false, sde).unwrap();
    omen.fit_and_online_module(26_004, 11269, 14, sde).unwrap();
    omen.fit_and_online_module(26_005, 1306, 15, sde).unwrap();
    let actual_em = omen.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde);
    assert!(
        actual_em < base_em_res * 0.35,
        "Stacked EM Armor Hardener II + Multispectrum Membrane II + Coating II must reduce EM resonance below 35% of base: got {actual_em}"
    );
}
#[test]
fn test_damage_control_passive_resonances_and_max_group_fitted() {
    // parity: feat:modules.damage-control
    eve_testkit::attest(&["feat:modules.damage-control"]);
    let sde = sde();
    let mut rifter = ShipFittingEngine::from_sde(17_001, 587, sde); // Rifter
    rifter.set_character_skills(HashMap::from([
        (3394, 5), // Hull Upgrades V
        (3392, 5), // Mechanics V
    ]));
    rifter.recalculate_stats(sde);
    let base_hull_em = rifter.ship.get_attribute(attr_ids::EM_DAMAGE_RESONANCE, sde);
    let base_armor_em = rifter.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde);
    let base_shield_em = rifter.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde);
    // Fit Damage Control II (2048): online effect (`2302`) modifies all 12 hull/armor/shield resonances
    rifter.fit_and_online_module(27_001, 2048, 11, sde).unwrap();
    let dc_hull_em = rifter.ship.get_attribute(attr_ids::EM_DAMAGE_RESONANCE, sde);
    let dc_armor_em = rifter.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde);
    let dc_shield_em = rifter.ship.get_attribute(attr_ids::SHIELD_EM_DAMAGE_RESONANCE, sde);
    assert!((dc_hull_em - base_hull_em * 0.60).abs() < 1e-6, "DC II must multiply hull resonance by 0.60");
    assert!((dc_armor_em - base_armor_em * 0.85).abs() < 1e-6, "DC II must multiply armor resonance by 0.85");
    assert!((dc_shield_em - base_shield_em * 0.875).abs() < 1e-6, "DC II must multiply shield resonance by 0.875");
    // Fitting a second Damage Control II in another low slot must fail with MaxGroupFittedExceeded (1544 == 1)
    let err = rifter.fit_and_online_module(27_002, 2048, 12, sde).unwrap_err();
    assert!(err.contains("MaxGroupFittedExceeded"), "Expected MaxGroupFittedExceeded, got {err}");
}
#[test]
fn test_capacitor_modules_relay_flux_coil_recharger_battery_pds_and_booster() {
    // parity: feat:modules.cap-mods
    eve_testkit::attest(&["feat:modules.cap-mods"]);
    let sde = sde();
    let mut omen = ShipFittingEngine::from_sde(18_001, 2006, sde); // Omen
    omen.set_character_skills(HashMap::from([
        (3418, 5), // Power Grid Management V / Capacitor Management V
        (3424, 5), // Energy Grid Upgrades V
        (3413, 5), // Power Grid Management V
        (3417, 5), // Capacitor Systems Operation V
        (3416, 5), // Shield Operation V
        (3426, 5), // CPU Management V
    ]));
    omen.fit_and_online_module(28_099, 399, 19, sde).unwrap(); // Small Shield Booster I (to test Cap Power Relay shield boost penalty)
    let base_cap = omen.ship.get_attribute(attr_ids::CAPACITOR_CAPACITY, sde);
    let base_rech = omen.ship.get_attribute(attr_ids::RECHARGE_RATE, sde);
    let base_pg = omen.ship.get_attribute(attr_ids::POWER_OUTPUT, sde);
    let base_sh_boost = omen.get_module_effective_attribute(28_099, attr_ids::SHIELD_BONUS, sde);
    // 1. Capacitor Power Relay II (1447): 0.76x rechargeRate (55) and -11% shieldBonus (68) drawback
    omen.fit_and_online_module(28_001, 1447, 11, sde).unwrap();
    let cpr_rech = omen.ship.get_attribute(attr_ids::RECHARGE_RATE, sde);
    let cpr_sh_boost = omen.get_module_effective_attribute(28_099, attr_ids::SHIELD_BONUS, sde);
    assert!((cpr_rech - base_rech * 0.76).abs() < 1e-2);
    assert!((cpr_sh_boost - base_sh_boost * 0.89).abs() < 1e-4);
    // 2. Capacitor Flux Coil II (1248): 0.61x rechargeRate AND 0.80x capacitorCapacity; Cap Recharger II (2032): 0.80x rechargeRate
    omen.unfit_module(28_001, sde).unwrap();
    omen.fit_and_online_module(28_002, 2032, 20, sde).unwrap(); // Cap Recharger II (0.80x rechargeRate)
    assert!((omen.ship.get_attribute(attr_ids::RECHARGE_RATE, sde) - base_rech * 0.80).abs() < 1e-2);
    // 3. Medium Cap Battery II (3496): flat capacitorCapacity (ModAdd) + energyWarfareResistance (2045) bonus
    omen.fit_and_online_module(28_003, 3496, 21, sde).unwrap();
    assert!(omen.ship.get_attribute(attr_ids::CAPACITOR_CAPACITY, sde) > base_cap);
    assert!(omen.ship.get_attribute(2045, sde) < 1.0, "Cap Battery II must improve energyWarfareResistance (2045 < 1.0)");
    // 4. Power Diagnostic System II (1541): boosts powerOutput, capacitorCapacity, rechargeRate, shieldCapacity, shieldRechargeRate
    omen.fit_and_online_module(28_004, 1541, 12, sde).unwrap();
    assert!(omen.ship.get_attribute(attr_ids::POWER_OUTPUT, sde) > base_pg * 1.05);
    // 5. Heavy Capacitor Booster II (3578) loaded with Cap Booster 800 (11289)
    assert!(sde.types.contains_key(&3578));
    assert!(sde.types.contains_key(&11289));
}
#[test]
fn test_sensor_booster_scripts_and_signal_amplifier_eccm() {
    // parity: feat:modules.eccm-sensorboost
    eve_testkit::attest(&["feat:modules.eccm-sensorboost"]);
    let sde = sde();
    let mut caracal = ShipFittingEngine::from_sde(19_001, 621, sde); // Caracal (gravimetric sensor strength 211)
    caracal.set_character_skills(HashMap::from([
        (3426, 5), // CPU Management V
        (3428, 5), // Long Range Targeting V
        (3429, 5), // Signature Analysis V
        (3432, 5), // Electronics Upgrades V
        (3435, 5), // Sensor Linking V
    ]));
    caracal.recalculate_stats(sde);
    let base_range = caracal.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde);
    let base_scan = caracal.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde);
    let base_grav = caracal.ship.get_attribute(attr_ids::SCAN_GRAVIMETRIC_STRENGTH, sde);
    let base_locks = caracal.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, sde);
    // 1. Passive Signal Amplifier II (1987) in low slot 11: +maxTargetRange, +scanResolution, +sensorStrength, +maxLockedTargets
    caracal.fit_and_online_module(29_001, 1987, 11, sde).unwrap();
    let sa_range = caracal.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde);
    let sa_scan = caracal.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde);
    let sa_grav = caracal.ship.get_attribute(attr_ids::SCAN_GRAVIMETRIC_STRENGTH, sde);
    let sa_locks = caracal.ship.get_attribute(attr_ids::MAX_LOCKED_TARGETS, sde);
    assert!(sa_range > base_range * 1.20);
    assert!(sa_scan > base_scan * 1.10);
    assert!(sa_grav > base_grav * 1.10);
    assert!(sa_locks > base_locks);
    // 2. Active Sensor Booster II (1952) in med slot 19: unscripted -> Targeting Range Script (29009) -> Scan Resolution Script (29011) -> ECCM Script (41155)
    caracal.fit_and_online_module(29_002, 1952, 19, sde).unwrap();
    caracal.activate_module(29_002, false, sde).unwrap();
    let sb_unscripted_range = caracal.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde);
    let sb_unscripted_scan = caracal.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde);
    let sb_unscripted_grav = caracal.ship.get_attribute(attr_ids::SCAN_GRAVIMETRIC_STRENGTH, sde);
    assert!(sb_unscripted_range > sa_range);
    assert!(sb_unscripted_scan > sa_scan);
    assert!(sb_unscripted_grav > sa_grav);
    // Targeting Range Script (29009): doubles range bonus, zeroes scanResolution & ECCM bonus
    caracal.load_charge(29_002, 29009, 1, sde).unwrap();
    assert!(caracal.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde) > sb_unscripted_range);
    assert!((caracal.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde) - sa_scan).abs() < 1e-4);
    // Scan Resolution Script (29011): doubles scanResolution bonus, zeroes range & ECCM bonus
    caracal.load_charge(29_002, 29011, 1, sde).unwrap();
    assert!(caracal.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde) > sb_unscripted_scan);
    assert!((caracal.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde) - sa_range).abs() < 1e-3);
    // ECCM Script (41155): doubles sensor strength bonus, zeroes range & scanResolution bonus
    caracal.load_charge(29_002, 41155, 1, sde).unwrap();
    assert!(caracal.ship.get_attribute(attr_ids::SCAN_GRAVIMETRIC_STRENGTH, sde) > sb_unscripted_grav);
    assert!((caracal.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde) - sa_range).abs() < 1e-3);
}
#[test]
fn test_warp_core_stabilizer_and_interdiction_nullifier_bonuses_and_drawbacks() {
    // parity: feat:modules.warp-core-stab
    eve_testkit::attest(&["feat:modules.warp-core-stab"]);
    let sde = sde();
    let mut vexor = ShipFittingEngine::from_sde(20_001, 626, sde); // Vexor (has droneBandwidth 1271)
    vexor.set_character_skills(HashMap::from([
        (3426, 5), // CPU Management V
        (3455, 5), // Warp Drive Operation V
    ]));
    vexor.recalculate_stats(sde);
    let base_range = vexor.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde);
    let base_scan = vexor.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde);
    let base_bw = vexor.ship.get_attribute(attr_ids::DRONE_BANDWIDTH, sde);
    // 1. Fit Warp Core Stabilizer II (11640) in low slot 11:
    //    Passive drawbacks (-40% maxTargetRange, 0.60x scanResolution, -50% droneBandwidth) apply while online;
    //    Active effect (`warpCoreStabilizerActive` 8121) applies -2 to `warpScrambleStatus` (104) when activated.
    vexor.fit_and_online_module(30_001, 11640, 11, sde).unwrap();
    assert!((vexor.ship.get_attribute(attr_ids::MAX_TARGET_RANGE, sde) - base_range * 0.6).abs() < 1e-3);
    assert!((vexor.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde) - base_scan * 0.6).abs() < 1e-4);
    assert!((vexor.ship.get_attribute(attr_ids::DRONE_BANDWIDTH, sde) - base_bw * 0.5).abs() < 1e-4);
    assert_eq!(vexor.ship.get_attribute(104, sde), 0.0);
    vexor.activate_module(30_001, false, sde).unwrap();
    assert!(
        (vexor.ship.get_attribute(104, sde) - (-2.0)).abs() < 1e-6,
        "Active Warp Core Stabilizer II must apply -2 warpScrambleStatus"
    );
    // Enforces maxGroupFitted == 1 (1544)
    assert!(
        vexor
            .fit_and_online_module(30_002, 11640, 12, sde)
            .unwrap_err()
            .contains("MaxGroupFittedExceeded")
    );
    // 2. Interdiction Nullifier I (58919) on Interceptor (Crow 11176, group 831):
    //    Active `interdictionNullifierBonus` (8117) sets `warpBubbleImmune` (1538) = 1.0
    let mut crow = ShipFittingEngine::from_sde(20_002, 11176, sde);
    crow.set_character_skills(HashMap::from([(3426, 5), (3455, 5), (3453, 5)]));
    crow.fit_and_online_module(30_010, 58919, 27, sde).unwrap();
    crow.activate_module(30_010, false, sde).unwrap();
    assert!(
        crow.ship.get_attribute(1538, sde) > 0.0,
        "Active Interdiction Nullifier I must grant warpBubbleImmune (1538)"
    );
}
#[test]
fn test_t3_strategic_cruiser_subsystems_dynamic_slots_and_covert_cloak_cpu() {
    // parity: feat:modules.t3-subsystem-modes
    eve_testkit::attest(&["feat:modules.t3-subsystem-modes"]);
    let sde = sde();
    // Tengu (29984, group 963) starts with 0 hi/med/low/turret/launcher slots until 4 subsystems are fitted into flags 125..=128
    let mut tengu = ShipFittingEngine::from_sde(21_001, 29984, sde);
    tengu.set_character_skills(HashMap::from([
        (30544, 5), // Caldari Defensive Systems V
        (30548, 5), // Caldari Core Systems V
        (30549, 5), // Caldari Offensive Systems V
        (30552, 5), // Caldari Propulsion Systems V
        (3300, 5),  // Gunnery V
        (3304, 5),  // Medium Hybrid Turret V
        (3450, 5),  // Afterburner V
        (3426, 5),  // CPU Management V
        (3418, 5),  // Power Grid Management V
        (11579, 5), // Cloaking V
    ]));
    // Cross-hull subsystem (Legion Core - Dissolution Sequencer 45622 on Tengu 29984) must be rejected via fitsToShipType (1380)
    assert!(tengu.fit_and_online_module(31_099, 45622, 125, sde).is_err());
    // Wrong subsystem slot flag (fitting Core 45625 into Defensive slot 126) must be rejected via subSystemSlot (1366)
    assert!(tengu.fit_and_online_module(31_001, 45625, 126, sde).is_err());
    // Fit all 4 Tengu subsystems:
    // - 125: Tengu Core - Electronic Efficiency Gate (45625: 0H / 3M / 1L)
    // - 126: Tengu Defensive - Amplification Node (45591: 0H / 3M / 1L, non-Covert -> +10,000 CPU penalty on Covert Cloak)
    // - 127: Tengu Offensive - Magnetic Infusion Basin (45602: 7H / 0M / 0L, 6 turrets, +CPU/PG, Medium Hybrid Turret bonuses)
    // - 128: Tengu Propulsion - Fuel Catalyst (45615: 0H / 2M / 0L, Afterburner speedFactor bonus)
    tengu.fit_and_online_module(31_001, 45625, 125, sde).unwrap();
    tengu.fit_and_online_module(31_002, 45591, 126, sde).unwrap();
    tengu.fit_and_online_module(31_003, 45602, 127, sde).unwrap();
    tengu.fit_and_online_module(31_004, 45615, 128, sde).unwrap();
    assert_eq!(tengu.ship.get_attribute(attr_ids::HI_SLOTS, sde), 7.0);
    assert_eq!(tengu.ship.get_attribute(attr_ids::MED_SLOTS, sde), 8.0);
    assert_eq!(tengu.ship.get_attribute(attr_ids::LOW_SLOTS, sde), 2.0);
    assert_eq!(tengu.ship.get_attribute(attr_ids::TURRET_SLOTS_LEFT, sde), 6.0);
    assert!(tengu.ship.get_attribute(attr_ids::CPU_OUTPUT, sde) > 400.0);
    assert!(tengu.ship.get_attribute(attr_ids::POWER_OUTPUT, sde) > 700.0);
    // Non-Covert defensive subsystem (45591) lacks `covertOpsCpuBonus1` (896) and adds +10,000 CPU penalty (`5262` -> `5261`) to Covert Ops Cloaking Device II (11578), preventing onlining
    let cloak_err = tengu.fit_and_online_module(31_010, 11578, 27, sde).unwrap_err();
    assert!(cloak_err.contains("Insufficient CPU"), "Expected Insufficient CPU for Covert Cloak on non-covert Tengu, got {cloak_err}");
    // Swap slot 126 to Tengu Defensive - Covert Reconfiguration (45589: 1H / 3M / 0L, -100% CPU need on Covert Cloak via effect 896)
    tengu.fit_and_online_module(31_002, 45589, 126, sde).unwrap();
    assert_eq!(tengu.ship.get_attribute(attr_ids::HI_SLOTS, sde), 8.0);
    tengu.fit_and_online_module(31_010, 11578, 27, sde).unwrap();
    assert_eq!(
        tengu.get_module_effective_attribute(31_010, attr_ids::CPU, sde),
        0.0
    );
    assert_eq!(
        tengu.get_module_effective_attribute(31_010, 669, sde),
        5_000.0
    );
}
#[test]
fn test_jump_drive_economizer_fuel_reduction_and_capital_restrictions() {
    // parity: feat:modules.jump-drive-economizer
    eve_testkit::attest(&["feat:modules.jump-drive-economizer"]);
    let sde = sde();
    // 1. Subcapital Rifter (587) must be rejected when fitting Prototype Jump Drive Economizer (34126, group 1299)
    let mut rifter = ShipFittingEngine::from_sde(22_001, 587, sde);
    assert!(rifter.fit_and_online_module(32_001, 34126, 11, sde).is_err());
    // 2. Rhea Jump Freighter (28844, group 902) fitting 1x and 2x Prototype Jump Drive Economizer (34126) in low slots 11 & 12
    //    Reduces `jumpDriveConsumptionAmount` (868) by -10% (PostPercent, stacking-penalized across multiple modules)
    let mut rhea = ShipFittingEngine::from_sde(22_002, 28844, sde);
    rhea.set_character_skills(HashMap::from([
        (3455, 5),  // Warp Drive Operation V
        (3456, 5),  // Jump Drive Operation V
        (21610, 5), // Jump Fuel Conservation V
    ]));
    rhea.recalculate_stats(sde);
    let base_fuel = rhea.ship.get_attribute(868, sde); // jumpDriveConsumptionAmount
    assert!(base_fuel > 0.0);
    rhea.fit_and_online_module(32_001, 34126, 11, sde).unwrap();
    let fuel_1 = rhea.ship.get_attribute(868, sde);
    assert!((fuel_1 - base_fuel * 0.90).abs() < 1e-3);
    rhea.fit_and_online_module(32_002, 34126, 12, sde).unwrap();
    let fuel_2 = rhea.ship.get_attribute(868, sde);
    let exp_fuel_2 = base_fuel * 0.90 * (1.0 - 0.10 * stacking_penalty_factor(1));
    assert!(
        (fuel_2 - exp_fuel_2).abs() < 1e-3,
        "2x Prototype Jump Drive Economizer jumpDriveConsumptionAmount mismatch: got {fuel_2}, expected {exp_fuel_2}"
    );
}
#[test]
fn test_drone_damage_skills_interfacing_size_racial_specialization_and_ddas() {
    // parity: feat:drones.drone-damage-skills
    eve_testkit::attest(&["feat:drones.drone-damage-skills"]);
    let sde = sde();
    let mut vexor = ShipFittingEngine::from_sde(23_001, 626, sde); // Vexor (+10%/lvl drone damage via Gallente Cruiser 3332)
    vexor.set_character_skills(HashMap::from([
        (3436, 5),  // Drones V
        (3318, 5),  // Weapon Upgrades V (required for Drone Damage Amplifier II)
        (24241, 5), // Light Drone Operation V (+5%/lvl = +25% via effect 1730)
        (33699, 5), // Medium Drone Operation V (+5%/lvl = +25% via effect 1730)
        (3441, 5),  // Heavy Drone Operation V (+5%/lvl = +25% via effect 1730)
        (23594, 5), // Sentry Drone Interfacing V (+5%/lvl = +25% via effect 1730)
        (12486, 5), // Gallente Drone Specialization V (+2%/lvl = +10% on T2 Gallente drones via effect 1730)
        (12484, 5), // Amarr Drone Specialization V (+2%/lvl = +10% on T2 Amarr drones via effect 1730)
        (3442, 5),  // Drone Interfacing V (+10%/lvl = +50% on all drones via effect 6663)
        (3332, 5),  // Gallente Cruiser V (+10%/lvl = +50% ship hull drone damage bonus)
        (3426, 5),  // CPU Management V
    ]));
    vexor.add_drone(91_001, 2454, sde); // Hobgoblin I (T1 Light Gallente)
    vexor.add_drone(91_002, 2456, sde); // Hobgoblin II (T2 Light Gallente)
    vexor.add_drone(91_003, 2185, sde); // Hammerhead II (T2 Medium Gallente)
    vexor.add_drone(91_004, 2446, sde); // Ogre II (T2 Heavy Gallente)
    vexor.add_drone(91_005, 2205, sde); // Acolyte II (T2 Light Amarr)
    vexor.add_drone(91_006, 2488, sde); // Warrior II (T2 Light Minmatar - Minmatar Drone Spec 12485 NOT trained!)
    let hob1_base = sde.types[&2454].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
    let hob2_base = sde.types[&2456].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
    let ham2_base = sde.types[&2185].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
    let ogre2_base = sde.types[&2446].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
    let aco2_base = sde.types[&2205].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
    let war2_base = sde.types[&2488].default_attributes[&attr_ids::DAMAGE_MULTIPLIER];
    // T1 Hobgoblin I gets Light Drone Op V (1.25) * Drone Interfacing V (1.50) * Vexor hull (1.50), NO Gallente Spec
    assert!(
        (vexor.get_drone_effective_attribute(91_001, attr_ids::DAMAGE_MULTIPLIER, sde)
            - hob1_base * 1.25 * 1.50 * 1.50)
            .abs()
            < 1e-5
    );
    // T2 Hobgoblin II, Hammerhead II, Ogre II, Acolyte II also get +10% racial specialization (1.10)
    for (drone_id, base_mult) in [
        (91_002_i64, hob2_base),
        (91_003_i64, ham2_base),
        (91_004_i64, ogre2_base),
        (91_005_i64, aco2_base),
    ] {
        assert!(
            (vexor.get_drone_effective_attribute(drone_id, attr_ids::DAMAGE_MULTIPLIER, sde)
                - base_mult * 1.25 * 1.10 * 1.50 * 1.50)
                .abs()
                < 1e-5
        );
    }
    // Warrior II does NOT get +10% racial specialization because Minmatar Drone Specialization (12485) is untrained
    assert!(
        (vexor.get_drone_effective_attribute(91_006, attr_ids::DAMAGE_MULTIPLIER, sde)
            - war2_base * 1.25 * 1.50 * 1.50)
            .abs()
            < 1e-5
    );
    // Fit 2x Drone Damage Amplifier II (4405, +20.5% damageMultiplierBonus 1255, stacking-penalized!)
    vexor.fit_and_online_module(33_001, 4405, 11, sde).unwrap();
    vexor.fit_and_online_module(33_002, 4405, 12, sde).unwrap();
    let dda_pct = sde.types[&4405].default_attributes[&1255] / 100.0;
    let exp_hob2_with_ddas = hob2_base
        * 1.25
        * 1.10
        * 1.50
        * 1.50
        * (1.0 + dda_pct * stacking_penalty_factor(0))
        * (1.0 + dda_pct * stacking_penalty_factor(1));
    assert!(
        (vexor.get_drone_effective_attribute(91_002, attr_ids::DAMAGE_MULTIPLIER, sde)
            - exp_hob2_with_ddas)
            .abs()
            < 1e-4
    );
}
#[test]
fn test_ewar_resistance_and_remote_impedance_attributes_and_bastion_siege_immunity() {
    // parity: feat:dogma.ewar-resistance-attributes
    eve_testkit::attest(&["feat:dogma.ewar-resistance-attributes"]);
    let sde = sde();
    let mut target = ShipFittingEngine::from_sde(24_001, 2006, sde); // Omen
    target.set_character_skills(HashMap::from([
        (3300, 5),
        (3303, 5),
        (3426, 5),
        (3418, 5),
    ]));
    target.fit_and_online_module(34_001, 454, 27, sde).unwrap(); // Small Focused Beam Laser I
    let base_vel = target.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    let base_sig = target.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);
    let base_scan = target.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde);
    let base_opt = target.get_module_effective_attribute(34_001, attr_ids::MAX_RANGE, sde);
    // Give target 50% resistance (`0.5`) across EWAR resistance attributes `2112..=2115`, `2045`, `2116`, `2135`
    for attr_id in [2045_u16, 2112, 2113, 2114, 2115, 2116, 2135] {
        target.ship.base_attributes.insert(attr_id, 0.5);
    }
    // Project Tracking Disruptor I (2108), Remote Sensor Dampener II (1969),
    // Target Painter II (19806), and Stasis Webifier I (526) at distance 1,000 m
    for (src_id, mod_tid) in [
        (80_001_i64, 2108_u32),  // Tracking Disruptor I (weaponDisruptionResistance 2113)
        (80_002_i64, 1969_u32),  // Remote Sensor Dampener II (sensorDampenerResistance 2112)
        (80_003_i64, 19806_u32), // Target Painter II (targetPainterResistance 2114)
        (80_004_i64, 526_u32),   // Stasis Webifier I (stasisWebifierResistance 2115)
    ] {
        target.upsert_projected_ewar(
            ProjectedEwarEffect {
                source_module_id: src_id,
                source_module_type_id: mod_tid,
                charge_type_id: None,
                distance: 1_000.0,
                overloaded: false,
                effective_module: DogmaItemState::from_sde(src_id, mod_tid, sde),
            },
            sde,
        );
    }
    // Verify 50% resistance halves each incoming EWAR effect
    let web_bonus = sde.types[&526].default_attributes[&20]; // -50.0%
    assert!(
        (target.ship.get_attribute(attr_ids::MAX_VELOCITY, sde)
            - base_vel * (1.0 + (web_bonus * 0.5) / 100.0))
            .abs()
            < 1e-4
    );
    let tp_bonus = sde.types[&19806].default_attributes[&554]; // +30.0%
    assert!(
        (target.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde)
            - base_sig * (1.0 + (tp_bonus * 0.5) / 100.0))
            .abs()
            < 1e-4
    );
    let damp_scan = sde.types[&1969].default_attributes[&566]; // -15.3%
    assert!(
        (target.ship.get_attribute(attr_ids::SCAN_RESOLUTION, sde)
            - base_scan * (1.0 + (damp_scan * 0.5) / 100.0))
            .abs()
            < 1e-4
    );
    let td_opt = sde.types[&2108].default_attributes[&351];
    assert!(
        (target.get_module_effective_attribute(34_001, attr_ids::MAX_RANGE, sde)
            - base_opt * (1.0 + (td_opt * 0.5) / 100.0))
            .abs()
            < 1e-4
    );
    // Verify energyWarfareResistance (2045 = 0.5), remoteRepairImpedance (2116 = 0.5), and remoteAssistanceImpedance (2135 = 0.5)
    let heavy_neut = DogmaItemState::from_sde(80_010, 533, sde); // Heavy Energy Neutralizer I
    let remote_armor = DogmaItemState::from_sde(80_011, 1609, sde); // Small Remote Armor Repairer I
    let remote_tc = DogmaItemState::from_sde(80_012, 1982, sde); // Remote Tracking Computer I
    assert!(
        (target.evaluate_projected_energy_warfare(&heavy_neut, 1_000.0, sde)
            - heavy_neut.get_attribute(97, sde) * 0.5)
            .abs()
            < 1e-5
    );
    assert!(
        (target.evaluate_projected_remote_repair(&remote_armor, 1_000.0, sde)
            - remote_armor.get_attribute(84, sde) * 0.5)
            .abs()
            < 1e-5
    );
    assert!(
        (target.evaluate_projected_remote_assistance(&remote_tc, 351, 1_000.0, sde)
            - remote_tc.get_attribute(351, sde) * 0.5)
            .abs()
            < 1e-5
    );
    // Activate Bastion Module I (33400) on a Marauder (Paladin 28659): grants 100% EWAR and remote repair/assistance immunity (0.0)
    let mut paladin = ShipFittingEngine::from_sde(24_002, 28659, sde);
    paladin.set_character_skills(HashMap::from([
        (33407, 5), // Marauders V
        (3318, 5),  // Weapon Upgrades V
        (11207, 5), // Advanced Weapon Upgrades V
        (3424, 5),  // Energy Grid Upgrades V
        (3426, 5),  // CPU Management V
        (3418, 5),  // Power Grid Management V
    ]));
    paladin.fit_and_online_module(34_100, 33400, 27, sde).unwrap(); // Bastion Module I
    paladin.activate_module(34_100, false, sde).unwrap();
    for immune_attr in [2112_u16, 2113, 2114, 2115, 2116, 2135] {
        assert_eq!(
            paladin.ship.get_attribute(immune_attr, sde),
            0.0,
            "Active Bastion Module I must set resistance/impedance attribute {immune_attr} to 0.0"
        );
    }
    let pal_base_vel = paladin.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    paladin.upsert_projected_ewar(
        ProjectedEwarEffect {
            source_module_id: 80_099,
            source_module_type_id: 526, // Stasis Webifier II
            charge_type_id: None,
            distance: 1_000.0,
            overloaded: false,
            effective_module: DogmaItemState::from_sde(80_099, 526, sde),
        },
        sde,
    );
    assert_eq!(paladin.ship.get_attribute(attr_ids::MAX_VELOCITY, sde), pal_base_vel);
    assert_eq!(paladin.evaluate_projected_remote_repair(&remote_armor, 1_000.0, sde), 0.0);
}
#[test]
fn test_polarized_weapons_zero_all_shield_armor_and_hull_resistances() {
    // parity: feat:modules.polarized-weapons-zero-resists
    eve_testkit::attest(&["feat:modules.polarized-weapons-zero-resists"]);
    let sde = sde();
    // Test Polarized 200mm AutoCannon (34284), Polarized Light Neutron Blaster (34278), and Polarized Rocket Launcher (34290)
    for (idx, pol_type_id) in [34284_u32, 34278_u32, 34290_u32].into_iter().enumerate() {
        let mut fit = ShipFittingEngine::from_sde(25_000 + idx as i64, 2006, sde); // Omen
        fit.set_character_skills(HashMap::from([
            (3300, 5),  // Gunnery V
            (3301, 5),  // Small Hybrid Turret V
            (3302, 5),  // Small Projectile Turret V
            (3303, 5),  // Small Energy Turret V
            (12210, 5), // Small Blaster Specialization V
            (11084, 5), // Small Autocannon Specialization V
            (3318, 5),  // Weapon Upgrades V
            (3319, 5),  // Missile Launcher Operation V
            (3320, 5),  // Rockets V
            (20209, 5), // Rocket Specialization V
            (3392, 5),  // Mechanics V
            (3394, 5),  // Hull Upgrades V
            (3420, 5),  // Tactical Shield Manipulation V
            (3426, 5),  // CPU Management V
            (3418, 5),  // Power Grid Management V
        ]));
        // Fit Damage Control II (2048), Multispectrum Energized Membrane II (11269), and active Multispectrum Shield Hardener II (2281)
        fit.fit_and_online_module(35_001, 2048, 11, sde).unwrap();
        fit.fit_and_online_module(35_002, 11269, 12, sde).unwrap();
        fit.fit_and_online_module(35_003, 2281, 19, sde).unwrap();
        fit.activate_module(35_003, false, sde).unwrap();
        let pre_armor_em = fit.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde);
        let pre_shield_therm = fit.ship.get_attribute(attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, sde);
        let pre_hull_kin = fit.ship.get_attribute(attr_ids::KINETIC_DAMAGE_RESONANCE, sde);
        assert!(pre_armor_em < 0.45 && pre_shield_therm < 0.65 && pre_hull_kin < 0.45);
        // Fit Polarized weapon (or temporarily grant launcher slot for Polarized Rocket Launcher on Omen)
        if pol_type_id == 34290 {
            fit.ship
                .base_attributes
                .insert(attr_ids::LAUNCHER_SLOTS_LEFT, 1.0);
        }
        fit.fit_and_online_module(35_010, pol_type_id, 27, sde).unwrap();
        // All 12 hull (`109..=113`), armor (`267..=270`), and shield (`271..=274`) damage resonance attributes
        // must be forced to `1.0` (`0%` resistance) via `PostAssign` (`operator == 7`), overriding DC II and hardeners!
        for res_attr in [109_u16, 110, 111, 113, 267, 268, 269, 270, 271, 272, 273, 274] {
            let val = fit.ship.get_attribute(res_attr, sde);
            assert!(
                (val - 1.0).abs() < 1e-6,
                "Polarized weapon {pol_type_id} must force resonance attribute {res_attr} to 1.0 (0% resistance), got {val}"
            );
        }
        // Taking the Polarized weapon offline immediately restores normal resistances from DC II and hardeners
        fit.take_module_offline(35_010, sde).unwrap();
        assert!((fit.ship.get_attribute(attr_ids::ARMOR_EM_DAMAGE_RESONANCE, sde) - pre_armor_em).abs() < 1e-6);
        assert!((fit.ship.get_attribute(attr_ids::SHIELD_THERMAL_DAMAGE_RESONANCE, sde) - pre_shield_therm).abs() < 1e-6);
        assert!((fit.ship.get_attribute(attr_ids::KINETIC_DAMAGE_RESONANCE, sde) - pre_hull_kin).abs() < 1e-6);
    }
}
#[test]
fn test_propulsion_upgrades_overdrive_synthetic_overdrive_istab_nanofiber_stacking() {
    // parity: feat:modules.vectored-thruster-array
    eve_testkit::attest(&["feat:modules.vectored-thruster-array"]);
    let sde = sde();
    let mut omen = ShipFittingEngine::from_sde(26_001, 2006, sde); // Omen (5 low slots)
    omen.set_character_skills(HashMap::from([
        (3449, 5), // Navigation V (+25% maxVelocity)
        (3394, 5), // Hull Upgrades V
        (3392, 5), // Mechanics V
    ]));
    omen.recalculate_stats(sde);
    let base_vel = omen.ship.get_attribute(attr_ids::MAX_VELOCITY, sde);
    let base_cap = omen.ship.get_attribute(attr_ids::CAPACITY, sde);
    let base_agility = omen.ship.get_attribute(attr_ids::AGILITY, sde);
    let base_sig = omen.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde);
    let base_hull = omen.ship.get_attribute(attr_ids::HP_STRUCT, sde);
    // 1. Fit Overdrive Injector System II (1236, +12.5% maxVelocity via 1076, 0.80x cargo capacity via 149) in slot 11
    //    and Synthetic Hull Conversion Overdrive Injector (21491, +12.0% maxVelocity via 1076, 0.97x cargo capacity via 149) in slot 12
    omen.fit_and_online_module(36_001, 1236, 11, sde).unwrap();
    omen.fit_and_online_module(36_002, 21491, 12, sde).unwrap();
    let od_vel_pct = sde.types[&1236].default_attributes[&1076] / 100.0;
    let syn_vel_pct = sde.types[&21491].default_attributes[&1076] / 100.0;
    let exp_vel_2 = base_vel
        * (1.0 + od_vel_pct * stacking_penalty_factor(0))
        * (1.0 + syn_vel_pct * stacking_penalty_factor(1));
    assert!((omen.ship.get_attribute(attr_ids::MAX_VELOCITY, sde) - exp_vel_2).abs() < 1e-4);
    let od_cargo_mult = sde.types[&1236].default_attributes[&149]; // cargoCapacityMultiplier (0.80)
    let syn_cargo_mult = sde.types[&21491].default_attributes[&149]; // cargoCapacityMultiplier (0.97)
    assert!(
        (omen.ship.get_attribute(attr_ids::CAPACITY, sde)
            - base_cap * od_cargo_mult * syn_cargo_mult)
            .abs()
            < 1e-4
    );
    // 2. Fit Inertial Stabilizers II (1405, -20% agility via 169, +11% signatureRadius via 554) in slot 13
    //    and Nanofiber Internal Structure II (2605, +9.5% maxVelocity via 1076, -15.75% agility via 169, 0.80x structure HP via 150) in slot 14
    omen.fit_and_online_module(36_003, 1405, 13, sde).unwrap();
    omen.fit_and_online_module(36_004, 2605, 14, sde).unwrap();
    let nano_vel_pct = sde.types[&2605].default_attributes[&1076] / 100.0;
    let exp_vel_3 = exp_vel_2 * (1.0 + nano_vel_pct * stacking_penalty_factor(2));
    assert!((omen.ship.get_attribute(attr_ids::MAX_VELOCITY, sde) - exp_vel_3).abs() < 1e-4);
    let istab_agi_pct = sde.types[&1405].default_attributes[&169].abs() / 100.0; // agilityBonus
    let nano_agi_pct = sde.types[&2605].default_attributes[&169].abs() / 100.0;
    let exp_agility = base_agility
        * (1.0 - istab_agi_pct * stacking_penalty_factor(0))
        * (1.0 - nano_agi_pct * stacking_penalty_factor(1));
    assert!((omen.ship.get_attribute(attr_ids::AGILITY, sde) - exp_agility).abs() < 1e-5);
    assert!((omen.ship.get_attribute(attr_ids::SIGNATURE_RADIUS, sde) - base_sig * 1.11).abs() < 1e-4);
    assert!(omen.ship.get_attribute(attr_ids::HP_STRUCT, sde) < base_hull * 0.85);
}
