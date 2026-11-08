//! Deep branch-coverage and mutation-killing unit tests for `character_clone_jump_rules`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::character_clone_jump_rules::{
    activate_clone_vat_bay, check_alpha_clone_install_capacity,
    check_structure_clone_bay_access_at, compute_structure_clone_fee, handle_clone_jump,
    is_ship_clone_vat_bay_active, load_clone_vat_bay_state, save_clone_vat_bay_state,
    CloneVatBayState, POCHVEN_REGION_ID, THERA_SYSTEM_ID,
};
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureAccessProfile,
};
use eve_db::{
    CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord,
    TrainedSkillRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, DVec3, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let mut titan = sde.types.get(&587).unwrap().clone();
        titan.type_id = 995_001;
        titan.group_id = 30;
        titan.category_id = 6;
        titan.name = "TestTitan".into();
        sde.types.insert(995_001, titan);

        let mut cvb = sde.types.get(&587).unwrap().clone();
        cvb.type_id = 995_002;
        cvb.group_id = 815;
        cvb.category_id = 7;
        cvb.name = "Clone Vat Bay I".into();
        cvb.default_attributes.clear();
        cvb.default_attributes.insert(73, 120_000.0);
        cvb.default_attributes.insert(713, 16273.0);
        cvb.default_attributes.insert(714, 150.0);
        sde.types.insert(995_002, cvb);

        let mut cloak = sde.types.get(&587).unwrap().clone();
        cloak.type_id = 995_003;
        cloak.group_id = 330;
        cloak.category_id = 7;
        cloak.name = "Prototype Cloaking Device I".into();
        sde.types.insert(995_003, cloak);

        Arc::new(sde)
    }))
}

fn new_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn make_sess(
    uid: u64,
    cid: i64,
    corp_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
    structure_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(777);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s.structure_id = structure_id;
    s.ship_id = ship_id;
    s
}

#[test]
fn structure_fees_preconditions_fulcrum_thera_and_structure_to_structure_jump() {
    let ctx = new_ctx();
    let struct_dep = 1_040_500_001_000i64;
    let struct_dest = 1_040_500_002_000i64;
    let struct_thera = 1_040_500_003_000i64;
    let owner_corp = 98_500_001i64;

    let (hs_sys, hs_st, no_bay_st) = {
        let mut with_bay = None;
        let mut without_bay = None;
        for sys in ctx.sde.solar_systems.values() {
            if sys.solar_system_id < 31_000_000 && sys.region_id != POCHVEN_REGION_ID {
                for st in &sys.stations {
                    if st.has_clone_bay && st.station_id != 60_015_187 && with_bay.is_none() {
                        with_bay = Some((sys.solar_system_id, st.station_id));
                    } else if !st.has_clone_bay && without_bay.is_none() {
                        without_bay = Some(st.station_id);
                    }
                }
            }
        }
        let (s, wb) = with_bay.expect("station with clone bay");
        (s, wb, without_bay.expect("station without clone bay"))
    };

    let (uid, cid, corp_id, cap_id) = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.values_mut().next().expect("demo char");
        ch.solar_system_id = hs_sys;
        ch.station_id = Some(hs_st);
        ch.structure_id = None;
        ch.balance = IskCents::from_isk(5_000_000);
        ch.last_clone_jump_unix = 0;
        let (uid, cid, corp) = (ch.user_id, ch.character_id, ch.corporation_id);

        snap.corporations.insert(
            owner_corp,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = owner_corp;
                __corp.name = "StructOwner".into();
                __corp },
        );
        for (sid, sys) in [
            (struct_dep, hs_sys),
            (struct_dest, hs_sys),
            (struct_thera, THERA_SYSTEM_ID),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35832,
                    owner_corp_id: owner_corp,
                    solar_system_id: sys,
                    name: format!("Struct_{sid}"),
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    ..Default::default()
                },
            );
        }
        let mut ext = load_ext_state(snap);
        let mut prof = StructureAccessProfile {
            profile_id: 8001,
            name: "CustomFeeProfile".into(),
            owner_corp_id: owner_corp,
            ..Default::default()
        };
        prof.service_settings.insert(35894, 250); // 250 ISK -> 25000 cents
        ext.access_profiles.insert(struct_dep, prof.clone());
        ext.access_profiles.insert(struct_dest, prof.clone());
        ext.access_profiles.insert(struct_thera, prof);
        save_ext_state(snap, &ext);

        assert_eq!(compute_structure_clone_fee(snap, struct_dep, 10.0).0, 25_000);
        assert!(check_alpha_clone_install_capacity(snap, 999_999_999).is_ok());

        snap.next_entity_id += 1;
        let cap = snap.next_entity_id;
        snap.items.insert(
            cap,
            InventoryItemRecord {
                item_id: cap,
                type_id: 670,
                owner_id: cid,
                location_id: hs_st,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&cid).unwrap().active_ship_id = cap;
        (uid, cid, corp, cap)
    });

    // 1. Undocked session -> MustBeDocked; invalid clone ID -> InvalidJumpCloneID; missing clone -> JumpCloneNotFound
    let mut undocked = make_sess(uid, cid, corp_id, hs_sys, None, None, Some(cap_id));
    assert_eq!(
        handle_clone_jump(&ctx, &mut undocked, &[PyRep::Integer(1)]).unwrap().unwrap_err().code,
        "MustBeDocked"
    );

    let mut sess = make_sess(uid, cid, corp_id, hs_sys, Some(hs_st), None, Some(cap_id));
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(0)]).unwrap().unwrap_err().code,
        "InvalidJumpCloneID"
    );
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(hs_st), PyRep::Integer(-5)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidJumpCloneID"
    );
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(9999)]).unwrap().unwrap_err().code,
        "JumpCloneNotFound"
    );

    // 2. Seed jump clones for: no-bay station, missing station, Fulcrum (60_015_187), Thera structure, and Upwell structures
    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.insert(
            cid,
            vec![
                JumpCloneRecord { jump_clone_id: 1, character_id: cid, location_id: no_bay_st, implants: vec![] },
                JumpCloneRecord { jump_clone_id: 2, character_id: cid, location_id: 69_999_999, implants: vec![] },
                JumpCloneRecord { jump_clone_id: 3, character_id: cid, location_id: 60_015_187, implants: vec![] },
                JumpCloneRecord { jump_clone_id: 4, character_id: cid, location_id: struct_dest, implants: vec![] },
                JumpCloneRecord { jump_clone_id: 5, character_id: cid, location_id: struct_thera, implants: vec![] },
            ],
        );
    });

    // Missing Infomorph Psychology (24242) -> InfomorphPsychologyRequired
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(4)]).unwrap().unwrap_err().code,
        "InfomorphPsychologyRequired"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(
            24242,
            TrainedSkillRecord { type_id: 24242, level: 3, skill_points: 50_000.0 },
        );
    });

    // Destination station without clone bay -> StationHasNoCloneBay; unknown station -> CloneDestinationNotFound
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(1)]).unwrap().unwrap_err().code,
        "StationHasNoCloneBay"
    );
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(2)]).unwrap().unwrap_err().code,
        "CloneDestinationNotFound"
    );

    // Fulcrum (60_015_187) requires pirate militia enlistment (500_010 or 500_011)
    if ctx.sde.solar_systems.values().flat_map(|s| &s.stations).any(|st| st.station_id == 60_015_187 && st.has_clone_bay) {
        assert_eq!(
            handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(3)]).unwrap().unwrap_err().code,
            "FulcrumCloneRequiresPirateEnlistment"
        );
    }

    // InsufficientFunds when balance < 900,000 ISK at NPC station
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().balance = IskCents(100);
    });
    assert_eq!(
        handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(4)]).unwrap().unwrap_err().code,
        "InsufficientFunds"
    );

    // Restore balance, set zarzakh_gate_locks, and jump from struct_dep -> struct_thera (Thera exception allowed!)
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&cid).unwrap();
        ch.balance = IskCents::from_isk(5_000_000);
        ch.station_id = None;
        ch.structure_id = Some(struct_dep);
        snap.zarzakh_gate_locks.insert(cid, (50_001, 9_999_999_999));
    });
    sess.station_id = None;
    sess.structure_id = Some(struct_dep);

    let thera_jump = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(struct_dep), PyRep::Integer(5)])
        .unwrap()
        .expect("jump to Thera structure allowed");
    assert_eq!(thera_jump.dict_get("locationID"), Some(&PyRep::Integer(struct_thera)));
    assert_eq!(sess.structure_id, Some(struct_thera));
    assert_eq!(sess.solar_system_id, Some(THERA_SYSTEM_ID));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.zarzakh_gate_locks.contains_key(&cid));
        assert_eq!(
            snap.corporations[&owner_corp].master_wallet_balance(),
            IskCents(25_000)
        );
    });

    // Check structure service general_settings[35894] = false -> StructureAccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.access_profiles
            .get_mut(&struct_dep)
            .unwrap()
            .general_settings
            .insert(35894, false);
        save_ext_state(snap, &ext);
        assert_eq!(
            check_structure_clone_bay_access_at(snap, struct_dep, cid, None).unwrap_err().code,
            "StructureAccessDenied"
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn clone_vat_bay_activation_and_jump_to_ship_clone_vat_bay_branches() {
    let ctx = new_ctx();
    let sys_id = 30_002_505u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let host_pilot = 97_500_001i64;
    let jumper_pilot = 97_500_002i64;
    let titan_id = 97_500_100i64;
    let rifter_id = 97_500_101i64;
    let cvb_mod = 97_500_200i64;
    let cloak_mod = 97_500_201i64;

    let hs_st = ctx
        .sde
        .solar_systems
        .values()
        .flat_map(|s| &s.stations)
        .find(|st| st.has_clone_bay && st.station_id != 60_015_187)
        .unwrap()
        .station_id;

    ctx.db.with_mut_snapshot(|snap| {
        let mut base_ch = snap.characters.values().next().cloned().unwrap();
        base_ch.clone_grade = 2;
        let mut h = base_ch.clone();
        h.character_id = host_pilot;
        h.user_id = host_pilot as u64;
        h.solar_system_id = sys_id;
        h.station_id = None;
        h.structure_id = None;
        h.active_ship_id = titan_id;
        snap.characters.insert(host_pilot, h);

        let mut j = base_ch;
        j.character_id = jumper_pilot;
        j.user_id = jumper_pilot as u64;
        j.solar_system_id = 30_000_142;
        j.station_id = Some(hs_st);
        j.structure_id = None;
        j.balance = IskCents::from_isk(10_000_000);
        j.last_clone_jump_unix = 0;
        snap.characters.insert(jumper_pilot, j);

        snap.skills.entry(jumper_pilot).or_default().insert(
            24242,
            TrainedSkillRecord { type_id: 24242, level: 5, skill_points: 256_000.0 },
        );

        for (iid, tid, owner, loc, flg) in [
            (titan_id, 995_001, host_pilot, sys_id as i64, InventoryFlag::None),
            (rifter_id, 587, host_pilot, sys_id as i64, InventoryFlag::None),
            (97_500_199, 999_999_999, host_pilot, sys_id as i64, InventoryFlag::None),
            (cvb_mod, 995_002, host_pilot, titan_id, InventoryFlag::HiSlot0),
            (97_500_299, 999_999_999, host_pilot, titan_id, InventoryFlag::HiSlot2),
            (cloak_mod, 995_003, host_pilot, titan_id, InventoryFlag::HiSlot1),
            (97_500_300, 670, jumper_pilot, hs_st, InventoryFlag::Hangar),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: owner,
                    location_id: loc,
                    flag: flg,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.characters.get_mut(&jumper_pilot).unwrap().active_ship_id = 97_500_300;
        snap.jump_clones.insert(
            jumper_pilot,
            vec![JumpCloneRecord {
                jump_clone_id: 701,
                character_id: jumper_pilot,
                location_id: titan_id,
                implants: vec![9899],
            }],
        );
    });

    // 1. activate_clone_vat_bay error branches: ShipNotFound, ShipCannotFitModule, ShipCloaked, ModuleNotFound, NotEnoughFuel
    assert_eq!(
        activate_clone_vat_bay(&ctx, &sys, host_pilot, 97_500_199, cvb_mod).await.unwrap_err().code,
        "ShipNotFound"
    );
    assert_eq!(
        activate_clone_vat_bay(&ctx, &sys, host_pilot, rifter_id, cvb_mod).await.unwrap_err().code,
        "ShipCannotFitModule"
    );

    let ball = Ball {
        id: titan_id,
        type_id: 995_001,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 1.0e9,
        radius: 2_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::default();
    cs.is_cloaked = true;
    let _ = sys.spawn_ball_with_meta(ball, cs.clone(), SlimItemMeta::default()).await;

    assert_eq!(
        activate_clone_vat_bay(&ctx, &sys, host_pilot, titan_id, cvb_mod).await.unwrap_err().code,
        "ShipCloaked"
    );
    cs.is_cloaked = false;
    let _ = sys.set_combat_state(titan_id, cs).await;

    assert_eq!(
        activate_clone_vat_bay(&ctx, &sys, host_pilot, titan_id, 97_500_299).await.unwrap_err().code,
        "ModuleNotFound"
    );
    assert_eq!(
        activate_clone_vat_bay(&ctx, &sys, host_pilot, titan_id, cvb_mod).await.unwrap_err().code,
        "NotEnoughFuel"
    );

    // Seed 100 Liquid Ozone in SpecializedFuelBay + 80 in Cargo (needs 150 total)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            97_500_401,
            InventoryItemRecord {
                item_id: 97_500_401,
                type_id: 16273,
                owner_id: host_pilot,
                location_id: titan_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            97_500_402,
            InventoryItemRecord {
                item_id: 97_500_402,
                type_id: 16273,
                owner_id: host_pilot,
                location_id: titan_id,
                flag: InventoryFlag::Cargo,
                quantity: 80,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let cvb_res = activate_clone_vat_bay(&ctx, &sys, host_pilot, titan_id, cvb_mod)
        .await
        .expect("activate cvb");
    assert_eq!(cvb_res.dict_get("fuelConsumed"), Some(&PyRep::Integer(150)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&97_500_401));
        assert_eq!(snap.items[&97_500_402].quantity, 30);
        assert!(is_ship_clone_vat_bay_active(&ctx, snap, titan_id, ctx.now_unix()));
        assert!(!is_ship_clone_vat_bay_active(&ctx, snap, rifter_id, ctx.now_unix()));
    });

    // 2. CloneJump to Titan Clone Vat Bay:
    // - NotInHostShipFleet when jumper is not in host's fleet
    let mut j_sess = make_sess(
        jumper_pilot as u64,
        jumper_pilot,
        1_000_009,
        30_000_142,
        Some(hs_st),
        None,
        Some(97_500_300),
    );
    assert_eq!(
        handle_clone_jump(&ctx, &mut j_sess, &[PyRep::Integer(701)]).unwrap().unwrap_err().code,
        "NotInHostShipFleet"
    );

    // - Add fleet, activate cloak on Titan -> ShipCloaked
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            801,
            FleetRecord {
                fleet_id: 801,
                boss_id: host_pilot,
                members: vec![host_pilot, jumper_pilot],
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod);
    });
    assert_eq!(
        handle_clone_jump(&ctx, &mut j_sess, &[PyRep::Integer(701)]).unwrap().unwrap_err().code,
        "ShipCloaked"
    );

    // - Uncloak Titan -> CloneJump succeeds and spawns jumper capsule in space!
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod);
    });
    let jump_ok = handle_clone_jump(&ctx, &mut j_sess, &[PyRep::Integer(701)])
        .unwrap()
        .expect("clone jump to Titan vat bay");
    assert_eq!(jump_ok.dict_get("solarSystemID"), Some(&PyRep::Integer(sys_id as i64)));
    assert_eq!(j_sess.station_id, None);
    assert_eq!(j_sess.structure_id, None);
    assert_eq!(j_sess.solar_system_id, Some(sys_id));

    ctx.db.with_mut_snapshot(|snap| {
        let mut st = CloneVatBayState::default();
        st.active_by_ship.insert(titan_id, (cvb_mod, 100));
        save_clone_vat_bay_state(snap, &st);
        let loaded = load_clone_vat_bay_state(snap);
        assert_eq!(loaded.active_by_ship.get(&titan_id), Some(&(cvb_mod, 100)));
    });
}
