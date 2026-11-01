//! Phase 5M mining crystal tiers, scrapmetal exclusions, Prismaticite/Unrefined Mineral
//! reactions, Mining Foreman Burst charges, and phased asteroid fields (`MN-07`..`MN-08`, part 2).

#![forbid(unsafe_code)]

use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, MiningLedgerRecord, TrainedSkillRecord,
    UpwellStructureRecord,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::{BTreeSet, HashMap};

async fn setup_session(ctx: &ServerContext, session_id: u64, user: &str, secret: &str, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(user.into()), PyRep::String(secret.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn make_item(id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, qty: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id: id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity: qty,
        singleton,
        version: 1,
        ..Default::default()
    }
}

fn get_dict_val<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == key).map(|(_, v)| v),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        _ => None,
    }
}

#[tokio::test]
async fn test_mining_crystal_tier_ore_compatibility_and_loading_rules() {
    // parity: feat:mining.crystal-tier-ore-compatibility
    eve_testkit::attest(&["feat:mining.crystal-tier-ore-compatibility"]);
    use crate::mining_cycle_engine::{activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, VELDSPAR};
    const MINER_I: u32 = 483;
    const STRIP_I: u32 = 17_482;
    const MOD_STRIP_II: u32 = 17_912;
    const MOD_DEEP_STRIP_II: u32 = 24_305;
    const ORE_DEEP_CORE: u32 = 28_748;
    const BANIDINE: u32 = 28_617;

    let (mod_id, crystal_id, cargo_crystal_id) = (100_000_865_i64, 100_000_866_i64, 100_000_867_i64);
    let skills = [
        (3_386, 5), (3_410, 5), (3_402, 5), (3_380, 5), (3_327, 5), (11_395, 5),
        (17_940, 5), (32_918, 5), (3_429, 5), (60_377, 5), (60_378, 5), (60_379, 5),
        (60_380, 5), (60_381, 5), (12_189, 5), (46_152, 5), (46_153, 5), (46_154, 5),
        (46_155, 5), (46_156, 5),
    ];
    let mut p = pilot(17_478, &skills, &[(mod_id, MOD_STRIP_II, InventoryFlag::HiSlot0)]).await;
    let bid = p.beyonce_id;
    rpc(&mut p, "beyonce", Some(bid), "CmdStop", &[]).await.unwrap();
    p.ctx.set_mechanic_rng_seed(Some(7));

    let tiers: &[(u32, u32, u32, u32)] = &[
        (MOD_STRIP_II, 60_276, VELDSPAR, 20),
        (MOD_STRIP_II, 60_285, 20, VELDSPAR),
        (MOD_STRIP_II, 60_291, 1_225, VELDSPAR),
        (MOD_STRIP_II, 60_297, 1_223, VELDSPAR),
        (MOD_STRIP_II, 60_303, 52_306, VELDSPAR),
        (MOD_DEEP_STRIP_II, 18_054, 11_396, VELDSPAR),
        (MOD_STRIP_II, 46_355, 45_490, 45_494),
        (MOD_STRIP_II, 46_365, 45_494, 45_490),
        (MOD_STRIP_II, 46_367, 45_498, 45_490),
        (MOD_STRIP_II, 46_369, 45_502, 45_490),
        (MOD_STRIP_II, 46_371, 45_510, 45_490),
    ];

    let base_m3 = 120.0 * 1.25 * 1.25 * 1.15 * 1.10;
    let sys = p.ctx.systems.get_or_spawn(JITA);
    let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;

    for (idx, &(miner_tid, crystal_tid, match_ore, mismatch_ore)) in tiers.iter().enumerate() {
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        let match_rock = 1_000_000_200_i64 + (idx as i64) * 2;
        let mismatch_rock = match_rock + 1;
        rock(&p, match_rock, match_ore, 100_000, 2_000.0).await;
        rock(&p, mismatch_rock, mismatch_ore, 100_000, 2_000.0).await;
        lock(&mut p, match_rock).await;
        lock(&mut p, mismatch_rock).await;
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&mod_id).unwrap().type_id = miner_tid;
            snap.items.insert(crystal_id, make_item(crystal_id, crystal_tid, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 1, true));
        });
        p.ctx.invalidate_ship_fitting(p.ship_id);

        let expected_base = if miner_tid == MOD_DEEP_STRIP_II { 80.0 * 1.25 * 1.25 * 1.15 * 1.10 } else { base_m3 };
        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        activate(&mut p, mod_id, match_rock).await.expect("activate on matching ore");
        let out_match = advance(&p, 45).await;
        assert_eq!(out_match[0].crystal.map(|c| (c.type_id, c.matched)), Some((crystal_tid, true)));
        assert!((out_match[0].mining_amount_m3 - expected_base * 1.5).abs() < 1e-9, "tier {idx} matched yield");
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

        sys.set_ball_position(p.ship_id, p.ship_pos).await;
        sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
        activate(&mut p, mod_id, mismatch_rock).await.expect("activate on mismatched ore");
        let out_mis = advance(&p, 45).await;
        assert_eq!(out_mis[0].crystal.map(|c| (c.type_id, c.matched)), Some((crystal_tid, false)));
        assert!((out_mis[0].mining_amount_m3 - expected_base).abs() < 1e-9, "tier {idx} mismatched yield");
        rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

        rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(match_rock)]).await.unwrap();
        rpc(&mut p, "dogmaIM", None, "RemoveTarget", &[PyRep::Integer(mismatch_rock)]).await.unwrap();
    }

    sys.set_ball_position(p.ship_id, p.ship_pos).await;
    let banidine_rock = 1_000_000_250_i64;
    rock(&p, banidine_rock, BANIDINE, 100_000, 2_000.0).await;
    lock(&mut p, banidine_rock).await;
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = MOD_STRIP_II;
        snap.items.get_mut(&crystal_id).unwrap().type_id = 60_276;
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    activate(&mut p, mod_id, banidine_rock).await.expect("activate on mission-unique Banidine");
    let out_mission = advance(&p, 45).await;
    assert_eq!(out_mission[0].crystal.map(|c| c.matched), Some(false));
    assert!((out_mission[0].mining_amount_m3 - base_m3).abs() < 1e-9);
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(mod_id), PyRep::String("miningLaser".into())]).await.unwrap();

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&crystal_id);
        snap.items.insert(cargo_crystal_id, make_item(cargo_crystal_id, 60_276, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 5, false));
    });
    let ship_id = p.ship_id;
    for unmod_tid in [MINER_I, STRIP_I] {
        p.ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&mod_id).unwrap().type_id = unmod_tid);
        p.ctx.invalidate_ship_fitting(ship_id);
        let err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
        assert_eq!(err, "IncompatibleAmmo");
    }

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_id).unwrap().type_id = MOD_STRIP_II;
        snap.items.get_mut(&cargo_crystal_id).unwrap().type_id = 18_054;
    });
    p.ctx.invalidate_ship_fitting(ship_id);
    let err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
    assert_eq!(err, "IncompatibleAmmo");

    for (miner_tid, cryst_tid, req_skill_id) in [
        (MOD_STRIP_II, 60_276_u32, 60_377_u32),
        (MOD_DEEP_STRIP_II, 18_054_u32, 12_189_u32),
        (ORE_DEEP_CORE, 18_054_u32, 12_189_u32),
    ] {
        p.ctx.db.with_mut_snapshot(|snap| {
            snap.items.retain(|&id, it| id == mod_id || id == cargo_crystal_id || it.flag != InventoryFlag::HiSlot0);
            snap.items.get_mut(&mod_id).unwrap().type_id = miner_tid;
            let ch = snap.items.get_mut(&cargo_crystal_id).unwrap();
            ch.type_id = cryst_tid;
            ch.location_id = ship_id;
            ch.flag = InventoryFlag::Cargo;
            ch.quantity = 5;
            snap.skills.get_mut(&CHAR_ID).unwrap().remove(&req_skill_id);
        });
        p.ctx.invalidate_ship_fitting(ship_id);
        let sk_err = rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap_err();
        assert_eq!(sk_err, "SkillPrerequisitesNotMet");

        p.ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(CHAR_ID).or_default().insert(req_skill_id, TrainedSkillRecord { type_id: req_skill_id, level: 5, skill_points: 256_000.0 });
        });
        p.ctx.invalidate_ship_fitting(ship_id);
        rpc(&mut p, "dogmaIM", None, "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(vec![PyRep::Integer(mod_id)]), PyRep::List(vec![PyRep::Integer(cargo_crystal_id)])]).await.unwrap();
    }
}

#[tokio::test]
async fn test_mining_reprocessing_curves_scrapmetal_and_prismaticite() {
    // parity: feat:mining.npc-reprocessing-equipment-and-tax-curve, feat:mining.scrapmetal-yield-exclusions, feat:mining.prismaticite-reprocessing-and-unrefined-reactions
    eve_testkit::attest(&[
        "feat:mining.npc-reprocessing-equipment-and-tax-curve",
        "feat:mining.scrapmetal-yield-exclusions",
        "feat:mining.prismaticite-reprocessing-and-unrefined-reactions",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let mut sess = setup_session(&ctx, 902, "demo", "demo-secret", char_id).await;

    // 1. NPC Reprocessing equipment base yield & standing tax curve (0.0 -> 5%, 6.67 -> 0%)
    let jita_station = 60_003_760_i64;
    sess.station_id = Some(jita_station);
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.station_id = Some(jita_station);
        ch.balance = IskCents::from_isk(500_000_000);
        ch.standings.insert(1_000_035, 0.0);
    });
    let quote_zero = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    assert!((get_dict_val(&quote_zero, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.05).abs() < 1e-6);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(1_000_035, 6.67);
    });
    let quote_high = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &[], &kw, None).await.unwrap();
    assert!((get_dict_val(&quote_high, "stationTaxRate").and_then(|v| v.as_f64()).unwrap() - 0.0).abs() < 1e-6);

    // 2. Scrapmetal yield exclusions (`feat:mining.scrapmetal-yield-exclusions`):
    // Dock in a Null-Sec rigged Tatara (35836) with Reprocessing V, Reprocessing Efficiency V,
    // Simple Ore Processing V, Scrapmetal Processing V, and RX-804 (+4% implant 27170).
    let ns_sys = ctx.sde.solar_systems.values().find(|s| s.security_status < -0.2 && s.solar_system_id < 31_000_000).unwrap().solar_system_id;
    let rig_tid = ctx.sde.types.values().find(|t| matches!(t.group_id, 1941..=1945) && t.default_attributes.get(&1547) == Some(&3.0)).map(|t| t.type_id).unwrap_or(46_497);
    let tatara_id = 1_045_000_000_801_i64;
    let corp_id = ctx.db.with_snapshot(|s| s.characters[&char_id].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(tatara_id, UpwellStructureRecord {
            structure_id: tatara_id, type_id: 35_836, owner_corp_id: corp_id, solar_system_id: ns_sys,
            position: [0.0, 0.0, 0.0], name: "NS Rigged Tatara".into(), state: "ShieldVulnerable".into(),
            fuel_blocks: 5_000, quantum_core_installed: true, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        snap.items.insert(100_000_868, make_item(100_000_868, 35_899, corp_id, tatara_id, InventoryFlag::ServiceSlot0, 1, true));
        snap.items.insert(100_000_869, make_item(100_000_869, rig_tid, corp_id, tatara_id, InventoryFlag::RigSlot0, 1, true));
        snap.module_online_states.insert(100_000_868, true);
        let sk = snap.skills.entry(char_id).or_default();
        for (sid, lvl) in [(3_385, 5), (3_389, 5), (60_377, 5), (12_196, 5), (45_746, 5), (90_398, 5)] {
            sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
        }
        snap.active_implants.entry(char_id).or_default().push(27_170); // RX-804 +4% reprocessing yield
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.solar_system_id = ns_sys;
        ch.station_id = None;
        ch.structure_id = Some(tatara_id);
        snap.items.insert(100_000_870, make_item(100_000_870, 482, char_id, tatara_id, InventoryFlag::Hangar, 1, false));   // Miner II module
        snap.items.insert(100_000_871, make_item(100_000_871, 587, char_id, tatara_id, InventoryFlag::Hangar, 1, false));   // Packaged Rifter
        snap.items.insert(100_000_872, make_item(100_000_872, 587, char_id, tatara_id, InventoryFlag::Hangar, 1, true));    // Assembled Rifter (must fail)
        snap.items.insert(100_000_873, make_item(100_000_873, 1_230, char_id, tatara_id, InventoryFlag::Hangar, 1_000, false)); // Veldspar ore
    });
    sess.station_id = None;
    sess.structure_id = Some(tatara_id);
    sess.solar_system_id = Some(ns_sys);

    // Assembled ship is rejected
    let asm_err = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_872)], &kw, None).await.unwrap_err();
    assert_eq!(asm_err.code, "ItemMustBeRepackaged");

    // Ore at rigged NS Tatara receives structure + rig + nullsec + skills + RX-804 (> 75%)
    let ore_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_873)], &kw, None).await.unwrap();
    let ore_yield = get_dict_val(&ore_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap();
    assert!(ore_yield > 0.75, "Ore at rigged NS Tatara must receive structure/rig/nullsec/implant bonuses (got {ore_yield})");

    // Module (482) and packaged ship (587) ignore Upwell structure, rig, security-band, Reprocessing, and RX-804 bonuses -> exact 55% at Scrapmetal V
    let mod_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_870)], &kw, None).await.unwrap();
    let ship_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_871)], &kw, None).await.unwrap();
    assert!((get_dict_val(&mod_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.55).abs() < 1e-6);
    assert!((get_dict_val(&ship_rep, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.55).abs() < 1e-6);

    // Scrapmetal Processing III -> exact 0.50 * (1 + 0.02 * 3) = 0.53 even in rigged NS Tatara with RX-804
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.get_mut(&char_id).unwrap().get_mut(&12_196).unwrap().level = 3;
        snap.items.insert(100_000_874, make_item(100_000_874, 482, char_id, tatara_id, InventoryFlag::Hangar, 1, false));
    });
    let mod_lvl3 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_874)], &kw, None).await.unwrap();
    assert!((get_dict_val(&mod_lvl3, "yieldRate").and_then(|v| v.as_f64()).unwrap() - 0.53).abs() < 1e-6);

    // 3. Prismaticite per-batch random mineral & Unrefined Mineral reactions (`feat:mining.prismaticite-reprocessing-and-unrefined-reactions`):
    let mut single_batch_minerals = BTreeSet::new();
    let mut single_batch_qtys = BTreeSet::new();
    for (idx, seed) in [11_u64, 29, 73, 151, 313, 619].into_iter().enumerate() {
        let iid = 100_000_880 + idx as i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(iid, make_item(iid, 90_041, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
        });
        ctx.set_mechanic_rng_seed(Some(seed));
        let rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(iid)], &kw, None).await.unwrap();
        let PyRep::List(outs) = get_dict_val(&rep, "outputs").unwrap() else { panic!("expected outputs list") };
        assert_eq!(outs.len(), 1, "Single 100-unit Prismaticite batch must yield exactly 1 mineral type");
        let tid = get_dict_val(&outs[0], "typeID").and_then(|v| v.as_i64()).unwrap() as u32;
        let qty = get_dict_val(&outs[0], "quantity").and_then(|v| v.as_i64()).unwrap();
        assert!(ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 18));
        single_batch_minerals.insert(tid);
        single_batch_qtys.insert((tid, qty));
    }
    assert!(single_batch_minerals.len() >= 2, "Different RNG seeds must roll distinct mineral types across Prismaticite batches");

    // Multi-batch stack (1,200 Prismaticite = 12 batches) rolls independently per batch into multiple minerals:
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(100_000_890, make_item(100_000_890, 90_041, char_id, tatara_id, InventoryFlag::Hangar, 1_200, false));
    });
    ctx.set_mechanic_rng_seed(Some(42));
    let multi_rep = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(100_000_890)], &kw, None).await.unwrap();
    let PyRep::List(multi_outs) = get_dict_val(&multi_rep, "outputs").unwrap() else { panic!() };
    assert!(multi_outs.len() >= 2, "12-batch Prismaticite stack must aggregate independent per-batch mineral rolls");

    // Unrefined Mineral reaction (90274 Unrefined Tritanium Formula, group 1888):
    // Negative: having only Standup Biochemical Reactor I (45539) online rejects InstallJob with FacilityServiceOffline!
    let (formula_id, biochem_svc_id, comp_svc_id) = (100_000_891_i64, 100_000_892_i64, 100_000_893_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(formula_id, make_item(formula_id, 90_274, char_id, tatara_id, InventoryFlag::Hangar, 1, true));
        snap.items.insert(100_000_894, make_item(100_000_894, 90_307, char_id, tatara_id, InventoryFlag::Hangar, 200, false)); // Compressed Prismaticite
        snap.items.insert(100_000_895, make_item(100_000_895, 16_634, char_id, tatara_id, InventoryFlag::Hangar, 200, false)); // Atmospheric Gases
        let mut biochem = make_item(biochem_svc_id, 45_539, corp_id, tatara_id, InventoryFlag::ServiceSlot1, 1, true);
        biochem.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(biochem_svc_id, biochem);
        snap.module_online_states.insert(biochem_svc_id, true);
    });
    let job_args = [PyRep::keyval([
        ("blueprintID", PyRep::Integer(formula_id)),
        ("facilityID", PyRep::Integer(tatara_id)),
        ("activityID", PyRep::Integer(9)),
        ("runs", PyRep::Integer(2)),
    ])];
    let wrong_reactor_err = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &job_args, &kw, None).await.unwrap_err();
    assert_eq!(wrong_reactor_err.code, "CompositeReactorRequired", "Unrefined Mineral reaction must require Standup Composite Reactor I (45537)");

    // Online Standup Composite Reactor I (45537) -> InstallJob succeeds -> CompleteJob delivers 200x Unrefined Tritanium (90283)
    ctx.db.with_mut_snapshot(|snap| {
        let mut comp = make_item(comp_svc_id, 45_537, corp_id, tatara_id, InventoryFlag::ServiceSlot2, 1, true);
        comp.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(comp_svc_id, comp);
        snap.module_online_states.insert(comp_svc_id, true);
    });
    let inst_rep = ctx.dispatch_rpc(&mut sess, "industryManager", None, "InstallJob", &job_args, &kw, None).await.expect("Install Unrefined Tritanium reaction");
    let job_id = get_dict_val(&inst_rep, "jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(800);
    ctx.dispatch_rpc(&mut sess, "industryManager", None, "CompleteJob", &[PyRep::Integer(job_id)], &kw, None).await.expect("Complete Unrefined Tritanium reaction");

    let unrefined_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|i| i.owner_id == char_id && i.location_id == tatara_id && i.type_id == 90_283).map(|i| i.item_id).collect()
    });
    assert!(!unrefined_ids.is_empty(), "Reaction must deliver Unrefined Tritanium (90283)");

    // Split into two 100-unit batches and reprocess under distinct RNG seeds -> both yield Tritanium (34) in variable quantities!
    let (u1, u2) = (100_000_896_i64, 100_000_897_i64);
    ctx.db.with_mut_snapshot(|snap| {
        for id in &unrefined_ids { snap.items.remove(id); }
        snap.items.insert(u1, make_item(u1, 90_283, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
        snap.items.insert(u2, make_item(u2, 90_283, char_id, tatara_id, InventoryFlag::Hangar, 100, false));
    });
    ctx.set_mechanic_rng_seed(Some(101));
    let u_rep1 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(u1)], &kw, None).await.unwrap();
    ctx.set_mechanic_rng_seed(Some(999_983));
    let u_rep2 = ctx.dispatch_rpc(&mut sess, "reprocessingSvc", None, "Reprocess", &[PyRep::Integer(u2)], &kw, None).await.unwrap();
    let PyRep::List(u_out1) = get_dict_val(&u_rep1, "outputs").unwrap() else { panic!() };
    let PyRep::List(u_out2) = get_dict_val(&u_rep2, "outputs").unwrap() else { panic!() };
    assert_eq!(get_dict_val(&u_out1[0], "typeID").and_then(|v| v.as_i64()), Some(34));
    assert_eq!(get_dict_val(&u_out2[0], "typeID").and_then(|v| v.as_i64()), Some(34));
    let q1 = get_dict_val(&u_out1[0], "quantity").and_then(|v| v.as_i64()).unwrap();
    let q2 = get_dict_val(&u_out2[0], "quantity").and_then(|v| v.as_i64()).unwrap();
    assert_ne!(q1, q2, "Unrefined Tritanium reprocessing must produce variable Tritanium output across batches ({q1} vs {q2})");
}

#[tokio::test]
async fn test_mining_foreman_burst_charges_orca_porpoise_rorqual_and_preservation() {
    // parity: feat:mining.foreman-burst-charges
    eve_testkit::attest(&["feat:mining.foreman-burst-charges"]);
    use crate::mining_cycle_engine::{activate, advance, lock, pilot, rock, rpc, CHAR_ID, JITA, VELDSPAR};
    const ORCA: u32 = 28_606;
    const PORPOISE: u32 = 42_244;
    const RORQUAL: u32 = 28_352;
    const BURST_II: u32 = 43_551;
    const CHG_FIELD: u32 = 42_829;
    const CHG_OPT: u32 = 42_830;
    const CHG_PRES: u32 = 42_831;
    const CHG_ARMOR_WRONG: u32 = 42_832;
    const MOD_STRIP_II: u32 = 17_912;
    const ICE_HARV_I: u32 = 16_278;
    const TYPE_A_I: u32 = 60_276;

    let (strip_id, ice_id, cryst_id) = (800_600_001_i64, 800_600_002_i64, 800_600_003_i64);
    let miner_skills = [
        (3_386, 5), (3_410, 5), (16_281, 5), (17_940, 5), (60_377, 5),
        (3_402, 5), (3_380, 5), (3_327, 5), (3_429, 5), (3_436, 5),
    ];
    let mut p = pilot(
        17_478,
        &miner_skills,
        &[
            (strip_id, MOD_STRIP_II, InventoryFlag::HiSlot0),
            (ice_id, ICE_HARV_I, InventoryFlag::HiSlot1),
        ],
    )
    .await;
    let kw = HashMap::new();
    let sys = p.ctx.systems.get_or_spawn(JITA);
    let (ore_rock, ice_rock) = (750_600_001_i64, 750_600_002_i64);
    rock(&p, ore_rock, VELDSPAR, 500_000, 3_000.0).await;
    rock(&p, ice_rock, 16_262, 500, 3_000.0).await;
    lock(&mut p, ore_rock).await;
    lock(&mut p, ice_rock).await;

    // Negative: fitting Mining Foreman Burst II (43551) onto a Retriever (17478) via invbroker.Add fails hull restriction
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(800_600_009, make_item(800_600_009, BURST_II, CHAR_ID, p.ship_id, InventoryFlag::Cargo, 1, true));
    });
    let p_ship = p.ship_id;
    let fit_err = rpc(&mut p, "invbroker", None, "Add", &[PyRep::Integer(800_600_009), PyRep::Integer(p_ship), PyRep::Integer(InventoryFlag::HiSlot2 as i64)]).await;
    assert!(fit_err.is_err(), "Mining Barge (Retriever) must not fit Mining Foreman Burst");

    // Set up second real pilot (90_000_002) in Orca (28606), form fleet via fleetObjectHandler.CreateFleet + Invite
    let booster_cid = 90_000_002_i64;
    let (b_ship, burst_mod, chg_item, bystander_ship) = (9_400_600_001_i64, 9_400_600_002_i64, 9_400_600_003_i64, 9_400_600_099_i64);
    let mut bsess = setup_session(&p.ctx, 903, "other", "other-secret", booster_cid).await;
    p.ctx.db.with_mut_snapshot(|snap| {
        let b_sk = snap.skills.entry(booster_cid).or_default();
        for (sid, lvl) in [(3_348, 5), (22_536, 5), (22_552, 5), (29_637, 5), (28_374, 5)] {
            b_sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 1_000_000.0 });
        }
        let ch = snap.characters.get_mut(&booster_cid).unwrap();
        ch.clone_grade = 2; // Omega clone required for Tech II Command Burst Specialist skill 22552
        ch.solar_system_id = JITA;
        ch.station_id = None;
        ch.active_ship_id = b_ship;
        snap.items.insert(b_ship, make_item(b_ship, ORCA, booster_cid, JITA as i64, InventoryFlag::None, 1, true));
        snap.items.insert(burst_mod, make_item(burst_mod, BURST_II, booster_cid, b_ship, InventoryFlag::HiSlot0, 1, true));
        snap.module_online_states.insert(burst_mod, true);
        // Non-fleet bystander ship in range (must NOT receive fleet bursts)
        snap.items.insert(bystander_ship, make_item(bystander_ship, 17_478, 90_000_099, JITA as i64, InventoryFlag::None, 1, true));
    });
    bsess.solar_system_id = Some(JITA);
    bsess.station_id = None;
    bsess.ship_id = Some(b_ship);
    sys.spawn_ball(Ball {
        id: b_ship, type_id: ORCA, position: p.ship_pos + DVec3::new(2_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 250_000_000.0, radius: 250.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();
    sys.spawn_ball(Ball {
        id: bystander_ship, type_id: 17_478, position: p.ship_pos + DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 100.0, inertia: 1.0, mass: 20_000_000.0, radius: 100.0,
        speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await.unwrap();

    p.ctx.advance_secs(12);
    p.ctx.dispatch_rpc(&mut bsess, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None).await.expect("CreateFleet");
    p.ctx.dispatch_rpc(&mut bsess, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(CHAR_ID), PyRep::None, PyRep::None, PyRep::None], &kw, None).await.expect("Invite miner");
    p.ctx.dispatch_rpc(&mut p.sess, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(bsess.fleet_id.unwrap())], &kw, None).await.expect("AcceptInvite");

    // Negative: loading Armor Command Burst charge (42832, group 1774) into Mining Foreman Burst II fails with InvalidChargeGroup
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(chg_item, make_item(chg_item, CHG_ARMOR_WRONG, booster_cid, b_ship, InventoryFlag::HiSlot0, 5, false));
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    let wrong_chg = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.unwrap_err();
    assert_eq!(wrong_chg.code, "InvalidChargeGroup");

    // 1. Orca (28606, +3%/lvl Ind Cmd V = 1.15x strength, 2.0 * 1.25 = 37.5 km range) + Optimization (42830):
    // Consumes 1 charge (5 -> 4) and buffs both Strip Miner AND Ice Harvester duration, excluding non-fleet bystander.
    p.ctx.db.with_mut_snapshot(|snap| snap.items.get_mut(&chg_item).unwrap().type_id = CHG_OPT);
    p.ctx.invalidate_ship_fitting(b_ship);
    let orca_rep = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Orca Optimization burst");
    assert_eq!(p.ctx.db.get_item(chg_item).unwrap().quantity, 4, "One burst charge must be consumed per activation");
    let PyRep::List(aff_ships) = get_dict_val(&orca_rep, "affectedShipIDs").unwrap() else { panic!() };
    assert!(aff_ships.contains(&PyRep::Integer(p.ship_id)));
    assert!(!aff_ships.contains(&PyRep::Integer(bystander_ship)), "Non-fleet ship in AoE must never receive bursts");
    assert!((get_dict_val(&orca_rep, "aoeRangeMeters").and_then(|v| v.as_f64()).unwrap() - 37_500.0).abs() < 1e-3);

    let max_cap = sys.get_combat_state(p.ship_id).await.unwrap().unwrap().max_capacitor;
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    let ice_act = activate(&mut p, ice_id, ice_rock).await.expect("Activate Ice Harvester under Orca Optimization");
    let orca_mult = 1.25 * 1.5 * 1.15; // T2 1.25 * Mining Director V 1.5 * Orca Ind Cmd V 1.15 = 2.15625
    let base_ice_ms = 240_000.0 * 0.75 * 0.90 * 0.875; // Retriever Ice Harvester I = 141,750 ms
    assert_eq!(ice_act.dict_get("cycleTimeMs").and_then(|v| v.as_i64()), Some((base_ice_ms * (1.0 - 0.15 * orca_mult)) as i64));
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(ice_id), PyRep::String("miningLaser".into())]).await.unwrap();

    // 2. Porpoise (42244, +2%/lvl = 1.10x strength) + Field Enhancement (42829) & Rorqual (28352, +5%/lvl = 1.25x strength) + Preservation (42831):
    let _ = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_mod)], &kw, None).await;
    p.ctx.advance_virtual_clock(95);
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&b_ship).unwrap().type_id = PORPOISE;
        snap.items.get_mut(&chg_item).unwrap().type_id = CHG_FIELD;
        snap.module_active_charges.remove(&burst_mod);
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.unwrap();
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    let porp_act = activate(&mut p, strip_id, ore_rock).await.unwrap();
    let porp_mult = 1.25 * 1.5 * 1.10;
    assert!((porp_act.dict_get("maxRange").and_then(|v| v.as_f64()).unwrap() - 15_000.0 * (1.0 + 0.40 * porp_mult)).abs() < 1e-3);
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();

    // 3. Rorqual (28352) + Mining Equipment Preservation Charge (42831, dbuff 25: -15% * 1.25 * 1.5 * 1.25 = -35.15625% crystal volatility!):
    // Base Type A I volatility = 0.025 -> under Rorqual Preservation = 0.025 * (1 - 0.3515625) = 0.0162109375.
    // Pick deterministic RNG seed whose 3rd draw (crystal volatility) lies in (0.017, 0.024):
    // -> WITHOUT Preservation (< 0.025) crystal takes 0.05 damage; WITH Preservation (> 0.01621) crystal takes 0.0 damage!
    let _ = p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Deactivate", &[PyRep::Integer(burst_mod)], &kw, None).await;
    p.ctx.advance_virtual_clock(95);
    let pres_seed = (1u64..)
        .find(|&s| {
            let mut st = s;
            let mut d = || eve_server::rng::u64_to_unit_f64(eve_server::rng::splitmix64_next(&mut st));
            d() >= 0.02 && d() >= 0.40 && (0.018..0.024).contains(&d())
        })
        .unwrap();
    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(cryst_id, make_item(cryst_id, TYPE_A_I, CHAR_ID, p.ship_id, InventoryFlag::HiSlot0, 1, true));
    });
    p.ctx.invalidate_ship_fitting(p.ship_id);
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    activate(&mut p, strip_id, ore_rock).await.unwrap();
    p.ctx.set_mechanic_rng_seed(Some(pres_seed));
    let unbuffed_cyc = advance(&p, 45).await;
    assert_eq!(unbuffed_cyc[0].crystal_damage, 0.05, "Without Preservation burst, roll in (0.018..0.024) damages crystal");
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();

    p.ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&b_ship).unwrap().type_id = RORQUAL;
        snap.items.get_mut(&chg_item).unwrap().type_id = CHG_PRES;
        snap.module_active_charges.remove(&burst_mod);
    });
    p.ctx.invalidate_ship_fitting(b_ship);
    p.ctx.dispatch_rpc(&mut bsess, "dogmaIM", None, "Activate", &[PyRep::Integer(burst_mod), PyRep::String("moduleBonusWarfareLinkMining".into())], &kw, None).await.expect("Activate Rorqual Preservation burst");
    sys.set_capacitor(p.ship_id, max_cap).await.unwrap();
    activate(&mut p, strip_id, ore_rock).await.unwrap();
    p.ctx.set_mechanic_rng_seed(Some(pres_seed));
    let buffed_cyc = advance(&p, 45).await;
    assert_eq!(buffed_cyc[0].crystal_damage, 0.0, "Rorqual Preservation burst lowers crystal volatility from 2.5% to 1.621% so roll misses");
    rpc(&mut p, "dogmaIM", None, "Deactivate", &[PyRep::Integer(strip_id), PyRep::String("miningLaser".into())]).await.unwrap();
}

#[tokio::test]
async fn test_mining_wormhole_sites_phased_fields_and_market_goals() {
    // parity: feat:mining.wormhole-ore-sites, feat:mining.phased-asteroid-field-lifecycle, feat:mining.mobile-phase-anchor, feat:mining.mining-market, feat:mining.mining-goals
    eve_testkit::attest(&[
        "feat:mining.wormhole-ore-sites",
        "feat:mining.phased-asteroid-field-lifecycle",
        "feat:mining.mobile-phase-anchor",
        "feat:mining.mining-market",
        "feat:mining.mining-goals",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    use eve_server::services::mining_sites::{PhasedAsteroidFieldState, resolve_wormhole_ore_site, WORMHOLE_ORE_SITES};
    assert_eq!(WORMHOLE_ORE_SITES.len(), 11);
    for def in WORMHOLE_ORE_SITES {
        let ores = resolve_wormhole_ore_site(&ctx.sde, def).expect("W-space site ores must exist in SDE");
        assert!(!ores.is_empty());
        assert!(!def.guards.is_empty());
    }

    let mut field = PhasedAsteroidFieldState::new("SIG-001".to_string(), 30_000_142);
    assert!(!field.beacon_spawned);
    assert_eq!(field.mining_efficiency(), 0.10);
    field.advance_fracture(0.45);
    assert!(field.beacon_spawned);
    assert!(!field.connect_industrial_command_ship(28_606));
    assert!(!field.connect_industrial_command_ship(42_244));
    assert_eq!(field.mining_efficiency(), 0.10);
    assert!(field.connect_industrial_command_ship(42_244));
    assert_eq!(field.mining_efficiency(), 1.0);
    field.advance_fracture(0.50);
    assert!(field.collapsed);
    assert_eq!(field.mining_efficiency(), 0.0);

    for (tid, expected_vol) in [(1_230_u32, 0.10_f64), (34_u32, 0.01_f64), (28_432_u32, 0.15_f64)] {
        let t = ctx.sde.types.get(&tid).unwrap();
        assert!(t.published && (t.volume - expected_vol).abs() < 1e-4);
    }

    let char_id = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.mining_ledger.push(MiningLedgerRecord {
            character_id: char_id,
            solar_system_id: 30_000_142,
            type_id: 1_230,
            quantity: 5_000,
            date: "2026-10-20".into(),
            timestamp_unix: 1_729_430_000,
        });
    });
    let ledger_total: i64 = ctx.db.with_snapshot(|snap| {
        snap.mining_ledger.iter().filter(|e| e.character_id == char_id).map(|e| e.quantity).sum()
    });
    assert_eq!(ledger_total, 5_000);
}
