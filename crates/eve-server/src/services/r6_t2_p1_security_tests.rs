//! Security regression tests for Round-6 Track 2 P1 mechanics/security fixes (`R6-T2-08`..`R6-T2-14` / `#377`..`#383`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fw_gameplay::fw_gameplay_rpc::donate_lp_to_fw_ihub;
use crate::services::station_structure::structure_upwell_ops::{load_ext_state, save_ext_state, StructureAccessProfile};
use crate::services::structure_weapons_ops::trigger_structure_auto_defense_on_grid;
use eve_db::corp_roles::ROLE_DIRECTOR;
use eve_db::{
    AllianceRecord, ContractRecord, CorporationRecord, FwSystemOccupationRecord, InventoryFlag,
    InventoryItemRecord, UpwellStructureRecord, WorldRepository, WorldStateSnapshot,
};
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn make_type(template: &InvType, tid: u32, gid: u32, cat: u32, name: &str) -> InvType {
    let mut t = template.clone();
    t.type_id = tid;
    t.group_id = gid;
    t.category_id = cat;
    t.name = name.into();
    t.volume = 100.0;
    t.capacity = 100_000.0;
    t.radius = 5_000.0;
    t.base_price = 1_000.0;
    t
}

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| {
        let mut sde = SdeRegistry::with_canonical_seed();
        let template = sde.types[&34].clone();
        for (tid, gid, cat, name) in [
            (35_832, 1657, 65, "Astrahus"),
            (35_825, 1404, 65, "Raitaru"),
            (32_458, 1245, 65, "Territorial Claim Unit"),
            (35_892, 1322, 66, "Standup Market Hub I"),
            (35_895, 1327, 66, "Standup Multirole Missile Launcher I"),
            (37_847, 1551, 8, "Standup XL Cruise Missile"),
            (23_057, 549, 87, "Dragonfly I"),
            (35_900, 1430, 66, "Standup M-Set Basic Research"),
            (42_685, 1408, 22, "Standup Cloning Center Core"),
            (4_247, 1136, 4, "Helium Fuel Block"),
        ] {
            sde.types.entry(tid).or_insert_with(|| make_type(&template, tid, gid, cat, name));
        }
        if let Some(m) = sde.types.get_mut(&35_895) {
            m.default_attributes.insert(604, 1551.0);
            m.default_attributes.insert(6, 50.0);
            m.default_attributes.insert(64, 1.0);
            m.default_attributes.insert(51, 10_000.0);
            m.default_attributes.insert(54, 300_000.0);
        }
        if let Some(c) = sde.types.get_mut(&37_847) {
            c.default_attributes.insert(114, 0.0);
            c.default_attributes.insert(116, 0.0);
            c.default_attributes.insert(117, 0.0);
            c.default_attributes.insert(118, 1_000.0);
            c.default_attributes.insert(654, 50.0);
        }
        Arc::new(sde)
    }))
}

fn test_ctx() -> Arc<ServerContext> {
    Arc::new(ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    ))
}

fn seed_corp(snap: &mut WorldStateSnapshot, corp_id: i64, ceo_id: i64, ally_id: Option<i64>) {
    snap.corporations.insert(corp_id, { let mut __corp = CorporationRecord::default();
        __corp.corporation_id = corp_id;
        __corp.name = format!("Corp {corp_id}");
        __corp.ticker = format!("C{}", corp_id % 100);
        __corp.ceo_id = ceo_id;
        __corp.alliance_id = ally_id;
        __corp.members = HashSet::from([ceo_id]);
        __corp.member_roles = HashMap::from([(ceo_id, ROLE_DIRECTOR)]);
        __corp });
}

fn seed_char(snap: &mut WorldStateSnapshot, cid: i64, corp: i64, aid: Option<i64>, sys: u32) {
    let mut c = snap.characters[&90_000_001].clone();
    c.character_id = cid;
    c.user_id = cid as u64;
    c.name = format!("Char {cid}");
    c.corporation_id = corp;
    c.alliance_id = aid;
    c.solar_system_id = sys;
    c.station_id = None;
    c.structure_id = None;
    c.active_ship_id = 0;
    snap.characters.insert(cid, c);
}

fn seed_item(snap: &mut WorldStateSnapshot, item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) {
    snap.items.insert(item_id, InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity,
        singleton: quantity == 1 && !matches!(flag, InventoryFlag::StructureFuel | InventoryFlag::SpecializedAmmoHold),
        version: 1, ..Default::default()
    });
}

fn seed_struct(snap: &mut WorldStateSnapshot, sid: i64, tid: u32, corp: i64, sys: u32, pos: [f64; 3], state: &str) {
    snap.structures.insert(sid, UpwellStructureRecord {
        structure_id: sid, type_id: tid, owner_corp_id: corp, solar_system_id: sys,
        name: format!("Struct {sid}"), position: pos, state: state.into(),
        quantum_core_installed: true, fuel_blocks: 50, ..Default::default()
    });
}

fn sess(char_id: i64, corp_id: i64, ally_id: Option<i64>, sys_id: u32, ship_id: i64) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64), character_id: Some(char_id), corporation_id: Some(corp_id),
        alliance_id: ally_id, solar_system_id: Some(sys_id), ship_id: Some(ship_id),
        corp_role: ROLE_DIRECTOR, ..Default::default()
    }
}

async fn spawn_target_ship(sys: &crate::system_actor::SystemHandle, sid: i64, pos: DVec3, cloaked: bool) {
    let _ = sys.spawn_ball(Ball {
        id: sid, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Rigid,
    }).await;
    let _ = sys.set_combat_state(sid, CombatState {
        shield_hp: 40.0, max_shield_hp: 40.0, armor_hp: 30.0, max_armor_hp: 30.0,
        structure_hp: 30.0, max_structure_hp: 30.0, capacitor: 100.0, max_capacitor: 100.0,
        signature_radius: 100.0,
        shield_resonance: [0.0, 1.0, 0.0, 0.0],
        armor_resonance: [0.0, 1.0, 0.0, 0.0],
        structure_resonance: [0.0, 1.0, 0.0, 0.0],
        is_cloaked: cloaked,
        ..Default::default()
    }).await;
}

#[tokio::test]
async fn r6_t2_08_transfer_corp_asset_ownership_transfers_fit_resets_acl_and_enforces_state_sov() {
    let ctx = test_ctx();
    let (sys_id, corp_a, corp_b, corp_c, ally_1, ally_2) = (30_000_142_u32, 98_100_001_i64, 98_100_002_i64, 98_100_003_i64, 99_100_001_i64, 99_100_002_i64);
    let (char_a, gunner_a, char_b, pilot_guest, st_id, hub_id) = (90_100_001_i64, 90_100_002_i64, 90_100_003_i64, 90_100_004_i64, 1_010_000_000_001_i64, 1_010_000_000_002_i64);

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_a, char_a, Some(ally_1));
        snap.corporations.get_mut(&corp_a).unwrap().members.insert(gunner_a);
        seed_corp(snap, corp_b, char_b, Some(ally_1));
        seed_corp(snap, corp_c, 90_100_009, Some(ally_2));
        for (aid, exec, members) in [(ally_1, corp_a, HashSet::from([corp_a, corp_b])), (ally_2, corp_c, HashSet::from([corp_c]))] {
            snap.alliances.insert(aid, AllianceRecord { alliance_id: aid, name: "A".into(), short_name: "A".into(), executor_corp_id: exec, member_corp_ids: members, ..Default::default() });
        }
        seed_char(snap, char_a, corp_a, Some(ally_1), sys_id);
        snap.characters.get_mut(&char_a).unwrap().active_ship_id = 999_001;
        seed_char(snap, gunner_a, corp_a, Some(ally_1), sys_id);
        seed_char(snap, char_b, corp_b, Some(ally_1), sys_id);
        seed_char(snap, pilot_guest, corp_c, Some(ally_2), sys_id);
        seed_struct(snap, st_id, 35_832, corp_a, sys_id, [1_000.0, 0.0, 0.0], "ArmorReinforced");
        seed_struct(snap, hub_id, 32_458, corp_a, sys_id, [50_000.0, 0.0, 0.0], "ShieldVulnerable");

        seed_item(snap, st_id, 35_832, corp_a, sys_id as i64, InventoryFlag::None, 1);
        seed_item(snap, 1_010_000_010, 35_895, corp_a, st_id, InventoryFlag::HiSlot0, 1);
        seed_item(snap, 1_010_000_011, 35_900, corp_a, st_id, InventoryFlag::RigSlot0, 1);
        seed_item(snap, 1_010_000_012, 35_892, corp_a, st_id, InventoryFlag::ServiceSlot0, 1);
        seed_item(snap, 1_010_000_013, 23_057, corp_a, st_id, InventoryFlag::FighterBay, 5);
        seed_item(snap, 1_010_000_014, 4_247, corp_a, st_id, InventoryFlag::StructureFuel, 50);
        seed_item(snap, 1_010_000_015, 37_847, corp_a, st_id, InventoryFlag::SpecializedAmmoHold, 20);
        seed_item(snap, 1_010_000_016, 42_685, corp_a, st_id, InventoryFlag::QuantumCoreRoom, 1);
        seed_item(snap, 1_010_000_017, 37_847, pilot_guest, st_id, InventoryFlag::Hangar, 3);

        let mut ext = load_ext_state(snap);
        let prof_a = StructureAccessProfile { profile_id: 7001, owner_corp_id: corp_a, corp_only: true, ..Default::default() };
        let prof_b = StructureAccessProfile { profile_id: 7002, owner_corp_id: corp_b, is_default: true, ..Default::default() };
        ext.profiles_by_id.insert(7001, prof_a.clone());
        ext.profiles_by_id.insert(7002, prof_b);
        ext.default_profile_by_corp.insert(corp_b, 7002);
        ext.structure_profile_id.insert(st_id, 7001);
        ext.access_profiles.insert(st_id, prof_a);
        ext.active_pilot_by_structure.insert(st_id, gunner_a);
        save_ext_state(snap, &ext);
    });

    let mut sess_a = sess(char_a, corp_a, Some(ally_1), sys_id, 999_001);
    let empty = HashMap::new();
    let e1 = ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(corp_b)], &empty, None).await.unwrap_err();
    assert_eq!(e1.code, "StructureTransferForbidden");

    let e2 = ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdTransferCorpAssetOwnership", &[PyRep::Integer(hub_id), PyRep::Integer(corp_c)], &empty, None).await.unwrap_err();
    assert_eq!(e2.code, "SovHubAllianceMismatch");

    ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdTransferCorpAssetOwnership", &[PyRep::Integer(hub_id), PyRep::Integer(corp_b)], &empty, None).await.expect("same-ally hub ok");

    ctx.db.with_mut_snapshot(|snap| { snap.structures.get_mut(&st_id).unwrap().state = "ShieldVulnerable".into(); });
    ctx.dispatch_rpc(&mut sess_a, "beyonce", None, "CmdTransferCorpAssetOwnership", &[PyRep::Integer(st_id), PyRep::Integer(corp_b)], &empty, None).await.expect("online struct ok");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&st_id].owner_corp_id, corp_b);
        for iid in 1_010_000_010_i64..=1_010_000_016 {
            assert_eq!((snap.items[&iid].owner_id, snap.items[&iid].version), (corp_b, 2));
        }
        assert_eq!(snap.items[&1_010_000_017].owner_id, pilot_guest);
        let ext = load_ext_state(snap);
        assert_eq!(ext.structure_profile_id.get(&st_id), Some(&7002));
        assert_eq!(ext.access_profiles[&st_id].owner_corp_id, corp_b);
        assert!(!ext.active_pilot_by_structure.contains_key(&st_id));
    });
}

#[tokio::test]
async fn r6_t2_09_standup_missiles_and_auto_defense_damage_attrs_and_death_pipeline() {
    let ctx = test_ctx();
    let (sys_id, corp_a, gunner, victim_1, victim_2) = (30_002_813_u32, 98_100_020_i64, 90_100_020_i64, 90_100_021_i64, 90_100_022_i64);
    let (st_id, mod_id, ammo_id, ship_1, ship_2) = (1_010_000_020_001_i64, 1_010_000_020_010_i64, 1_010_000_020_011_i64, 1_010_000_020_101_i64, 1_010_000_020_102_i64);
    let (launcher_tid, req_charge_grp) = ctx.sde.types.values()
        .find_map(|t| matches!(t.group_id, 1327 | 1562).then(|| (t.type_id, t.default_attributes.get(&604).copied().unwrap_or(0.0) as u32)))
        .unwrap();
    let charge_tid = ctx.sde.types.values()
        .find(|t| t.category_id == 8 && t.group_id == req_charge_grp && t.default_attributes.get(&118).copied().unwrap_or(0.0) > 0.0)
        .or_else(|| ctx.sde.types.values().find(|t| t.category_id == 8 && t.group_id == req_charge_grp))
        .map(|t| t.type_id)
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_a, gunner, None);
        seed_char(snap, gunner, corp_a, None, sys_id);
        snap.characters.get_mut(&gunner).unwrap().active_ship_id = st_id;
        seed_char(snap, victim_1, 98_100_099, None, sys_id);
        seed_char(snap, victim_2, 98_100_099, None, sys_id);
        snap.characters.get_mut(&victim_1).unwrap().active_ship_id = ship_1;
        snap.characters.get_mut(&victim_2).unwrap().active_ship_id = ship_2;
        seed_struct(snap, st_id, 35_832, corp_a, sys_id, [0.0, 0.0, 0.0], "ShieldVulnerable");
        seed_item(snap, st_id, 35_832, corp_a, sys_id as i64, InventoryFlag::None, 1);
        seed_item(snap, mod_id, launcher_tid, corp_a, st_id, InventoryFlag::HiSlot0, 1);
        snap.module_online_states.insert(mod_id, true);
        seed_item(snap, ammo_id, charge_tid, corp_a, st_id, InventoryFlag::SpecializedAmmoHold, 10);
        seed_item(snap, ship_1, 587, victim_1, sys_id as i64, InventoryFlag::None, 1);
        seed_item(snap, ship_2, 587, victim_2, sys_id as i64, InventoryFlag::None, 1);
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(st_id, gunner);
        save_ext_state(snap, &ext);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_target_ship(&sys, ship_1, DVec3::new(20_000.0, 0.0, 0.0), false).await;
    spawn_target_ship(&sys, ship_2, DVec3::new(-20_000.0, 0.0, 0.0), false).await;
    let _ = sys.add_target_lock(st_id, TargetLock { target_id: ship_1, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(300_000.0) }).await;

    let mut g_sess = sess(gunner, corp_a, None, sys_id, st_id);
    let empty = HashMap::new();
    let res = ctx.dispatch_rpc(&mut g_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_id), PyRep::String("missileLauncherOperation".into()), PyRep::Integer(ship_1)], &empty, None).await.expect("fire ok");
    let dealt = match res { PyRep::KeyVal(kv) => kv.get("damageDealt").and_then(PyRep::as_f64).unwrap_or(0.0), _ => 0.0 };
    assert!(dealt >= 99.9);
    assert!(sys.get_ball_snapshot(ship_1).await.unwrap().is_none());

    let mut def = ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        ext.standup_module_cycle_until_unix.clear();
        save_ext_state(snap, &ext);
        seed_item(snap, 1_010_000_020_012, charge_tid, corp_a, st_id, InventoryFlag::HiSlot0, 5);
        crate::services::structure_acl_defense_rules::load_defense_settings(snap, st_id)
    });
    def.auto_defense_enabled = true;
    def.auto_target_attackers = true;
    def.known_attackers.push(victim_2);
    ctx.db.with_mut_snapshot(|snap| crate::services::structure_acl_defense_rules::save_defense_settings(snap, &def));
    trigger_structure_auto_defense_on_grid(&ctx, &sys, sys_id, st_id, Some(victim_2), ctx.now_unix()).await;
    assert!(sys.get_ball_snapshot(ship_2).await.unwrap().is_none());

    ctx.db.with_snapshot(|snap| {
        for v in [victim_1, victim_2] {
            assert_eq!(snap.items.get(&snap.characters[&v].active_ship_id).map(|i| i.type_id), Some(670));
        }
        assert!(snap.killmails.len() >= 2);
    });
}

#[tokio::test]
async fn r6_t2_10_standup_weapons_never_consume_guest_or_locked_ammo() {
    let ctx = test_ctx();
    let (sys_id, corp_a, guest, gunner, st_id, mod_id, ship_id) = (30_002_813_u32, 98_100_030_i64, 90_100_031_i64, 90_100_030_i64, 1_010_000_030_001_i64, 1_010_000_030_010_i64, 1_010_000_030_101_i64);
    let (launcher_tid, req_charge_grp) = ctx.sde.types.values()
        .find_map(|t| matches!(t.group_id, 1327 | 1562).then(|| (t.type_id, t.default_attributes.get(&604).copied().unwrap_or(0.0) as u32)))
        .unwrap();
    let charge_tid = ctx.sde.types.values().find(|t| t.category_id == 8 && t.group_id == req_charge_grp).map(|t| t.type_id).unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_a, gunner, None);
        seed_char(snap, gunner, corp_a, None, sys_id);
        snap.characters.get_mut(&gunner).unwrap().active_ship_id = st_id;
        seed_char(snap, guest, 98_100_098, None, sys_id);
        seed_struct(snap, st_id, 35_832, corp_a, sys_id, [0.0, 0.0, 0.0], "ShieldVulnerable");
        seed_item(snap, st_id, 35_832, corp_a, sys_id as i64, InventoryFlag::None, 1);
        seed_item(snap, mod_id, launcher_tid, corp_a, st_id, InventoryFlag::HiSlot0, 1);
        snap.module_online_states.insert(mod_id, true);
        seed_item(snap, 1_010_000_030_020, charge_tid, guest, st_id, InventoryFlag::Cargo, 5);
        seed_item(snap, 1_010_000_030_021, charge_tid, corp_a, st_id, InventoryFlag::FighterBay, 5);
        seed_item(snap, 1_010_000_030_022, charge_tid, corp_a, st_id, InventoryFlag::CorpSAG7, 5);
        seed_item(snap, 1_010_000_030_023, charge_tid, corp_a, st_id, InventoryFlag::SpecializedAmmoHold, 5);
        snap.contracts.insert(555_001, ContractRecord {
            contract_id: 555_001, issuer_id: gunner, issuer_corp_id: corp_a, status: "Outstanding".into(),
            item_ids: vec![1_010_000_030_023], ..Default::default()
        });
        let mut ext = load_ext_state(snap);
        ext.active_pilot_by_structure.insert(st_id, gunner);
        save_ext_state(snap, &ext);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_target_ship(&sys, ship_id, DVec3::new(15_000.0, 0.0, 0.0), false).await;
    let _ = sys.add_target_lock(st_id, TargetLock { target_id: ship_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(300_000.0) }).await;

    let mut g_sess = sess(gunner, corp_a, None, sys_id, st_id);
    let empty = HashMap::new();
    let err = ctx.dispatch_rpc(&mut g_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_id), PyRep::String("missileLauncherOperation".into()), PyRep::Integer(ship_id)], &empty, None).await.unwrap_err();
    assert_eq!(err.code, "NoChargesLoaded");

    ctx.db.with_mut_snapshot(|snap| { snap.contracts.clear(); });
    ctx.dispatch_rpc(&mut g_sess, "dogmaIM", None, "Activate", &[PyRep::Integer(mod_id), PyRep::String("missileLauncherOperation".into()), PyRep::Integer(ship_id)], &empty, None).await.expect("unlocked ammo succeeds");
    ctx.db.with_snapshot(|snap| {
        assert_eq!((snap.items[&1_010_000_030_023].quantity, snap.items[&1_010_000_030_023].version), (4, 2));
        for unchanged in [1_010_000_030_020_i64, 1_010_000_030_021, 1_010_000_030_022] {
            assert_eq!(snap.items[&unchanged].quantity, 5);
        }
    });
}

#[tokio::test]
async fn r6_t2_11_and_12_fw_alliance_guards_direct_enlist_and_ihub_lp_cap() {
    let ctx = test_ctx();
    let (sys_id, corp_a, corp_b, ally_id, exec_ceo, member_ceo, direct_pilot) = (30_002_505_u32, 98_200_001_i64, 98_200_002_i64, 99_200_001_i64, 90_200_001_i64, 90_200_002_i64, 90_200_003_i64);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_a, exec_ceo, Some(ally_id));
        snap.corporations.get_mut(&corp_a).unwrap().members.insert(direct_pilot);
        seed_corp(snap, corp_b, member_ceo, Some(ally_id));
        snap.alliances.insert(ally_id, AllianceRecord { alliance_id: ally_id, name: "FW".into(), short_name: "FW".into(), executor_corp_id: corp_a, member_corp_ids: HashSet::from([corp_a, corp_b]), ..Default::default() });
        seed_char(snap, exec_ceo, corp_a, Some(ally_id), sys_id);
        seed_char(snap, member_ceo, corp_b, Some(ally_id), sys_id);
        seed_char(snap, direct_pilot, corp_a, Some(ally_id), sys_id);
        snap.fw_runtime.direct_enlisted_chars.insert(direct_pilot, 500_004);
        snap.characters.get_mut(&direct_pilot).unwrap().war_faction_id = Some(500_004);
        snap.fw_corp_enlisted_on.insert(corp_b, now.saturating_sub(3_600));
        snap.fw_system_occupations.insert(sys_id, FwSystemOccupationRecord { solar_system_id: sys_id, owner_faction_id: 500_001, occupier_faction_id: 500_001, victory_points: 100, victory_points_threshold: 3_000 });
    });

    let mut exec_sess = sess(exec_ceo, corp_a, Some(ally_id), sys_id, 999_101);
    let mut member_sess = sess(member_ceo, corp_b, Some(ally_id), sys_id, 999_102);
    let empty = HashMap::new();

    let cd_err = ctx.dispatch_rpc(&mut exec_sess, "facWarMgr", None, "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &empty, None).await.unwrap_err();
    assert_eq!(cd_err.code, "MilitiaCooldownActive");
    ctx.db.with_snapshot(|s| assert!(!s.fw_runtime.corp_war_factions.contains_key(&corp_a)));

    ctx.db.with_mut_snapshot(|s| { s.fw_corp_enlisted_on.clear(); });
    ctx.dispatch_rpc(&mut exec_sess, "facWarMgr", None, "JoinFactionAsAlliance", &[PyRep::Integer(500_001)], &empty, None).await.expect("ally join ok");
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.characters[&direct_pilot].war_faction_id, Some(500_001));
        assert!(!s.fw_runtime.direct_enlisted_chars.contains_key(&direct_pilot));
    });

    assert_eq!(ctx.dispatch_rpc(&mut exec_sess, "facWarMgr", None, "LeaveFactionAsCorporation", &[], &empty, None).await.unwrap_err().code, "AllianceEnlisted");
    assert_eq!(ctx.dispatch_rpc(&mut member_sess, "facWarMgr", None, "LeaveFactionAsCorporation", &[], &empty, None).await.unwrap_err().code, "AllianceEnlisted");

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&exec_ceo).unwrap();
        ch.loyalty_points.insert(1_000_180, 0);
        ch.loyalty_points.insert(500_001, 250_000);
    });
    assert_eq!(donate_lp_to_fw_ihub(&ctx, exec_ceo, sys_id, 60_000).map(|v| (v.0, v.1)), Ok((60_000, 2)));
    assert_eq!(donate_lp_to_fw_ihub(&ctx, exec_ceo, sys_id, 180_000).map(|v| (v.0, v.1)), Ok((200_000, 5)));
    ctx.db.with_snapshot(|s| assert_eq!(s.characters[&exec_ceo].loyalty_points[&500_001], 50_000));
    assert_eq!(donate_lp_to_fw_ihub(&ctx, exec_ceo, sys_id, 1).unwrap_err().code, "IhubMaxTierReached");
}

#[tokio::test]
async fn r6_t2_13_and_14_cone_scan_cloaked_and_map_data_acl() {
    let ctx = test_ctx();
    let (sys_id, corp_a, corp_b, scanner, s_ship, in_cone, behind, cloaked) = (30_000_142_u32, 98_300_001_i64, 98_300_002_i64, 90_300_001_i64, 1_010_000_090_001_i64, 1_010_000_090_002_i64, 1_010_000_090_003_i64, 1_010_000_090_004_i64);

    ctx.db.with_mut_snapshot(|snap| {
        seed_corp(snap, corp_a, scanner, None);
        seed_corp(snap, corp_b, 90_300_099, None);
        seed_char(snap, scanner, corp_a, None, sys_id);
        snap.characters.get_mut(&scanner).unwrap().active_ship_id = s_ship;
        seed_struct(snap, 1_010_000_090_101, 35_825, corp_b, sys_id, [1.0e11, 2.0e11, 3.0e11], "ShieldVulnerable");
        seed_struct(snap, 1_010_000_090_102, 35_825, corp_b, sys_id, [4.0e11, 0.0, 0.0], "ShieldVulnerable");
        let mut ext = load_ext_state(snap);
        ext.access_profiles.insert(1_010_000_090_101, StructureAccessProfile { owner_corp_id: corp_b, corp_only: true, ..Default::default() });
        ext.access_profiles.insert(1_010_000_090_102, StructureAccessProfile { owner_corp_id: corp_b, allowed_corporations: HashSet::from([corp_a]), ..Default::default() });
        save_ext_state(snap, &ext);
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    spawn_target_ship(&sys, s_ship, DVec3::ZERO, false).await;
    spawn_target_ship(&sys, in_cone, DVec3::new(0.0, 0.0, 50_000.0), false).await;
    spawn_target_ship(&sys, behind, DVec3::new(0.0, 0.0, -50_000.0), false).await;
    spawn_target_ship(&sys, cloaked, DVec3::new(0.0, 0.0, 40_000.0), true).await;

    let mut s = sess(scanner, corp_a, None, sys_id, s_ship);
    let empty = HashMap::new();
    let scan_res = ctx.dispatch_rpc(&mut s, "scanSvc", None, "ConeScan", &[PyRep::Real(0.5), PyRep::Real(1.0e9), PyRep::Tuple(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)])], &empty, None).await.expect("cone scan");
    let ids: Vec<i64> = match scan_res {
        PyRep::List(items) => items.into_iter().filter_map(|r| match r { PyRep::KeyVal(kv) => kv.get("itemID").and_then(PyRep::as_i64), _ => None }).filter(|id| [s_ship, in_cone, behind, cloaked].contains(id)).collect(),
        _ => Vec::new(),
    };
    assert_eq!(ids, vec![in_cone]);

    let map_res = ctx.dispatch_rpc(&mut s, "structureDirectory", None, "GetStructureMapData", &[PyRep::Integer(i64::from(sys_id))], &empty, None).await.expect("map data");
    let st_ids: Vec<i64> = match map_res {
        PyRep::List(items) => items.into_iter().filter_map(|r| match r { PyRep::List(cols) => cols.get(2).and_then(PyRep::as_i64), _ => None }).filter(|id| [1_010_000_090_101, 1_010_000_090_102].contains(id)).collect(),
        _ => Vec::new(),
    };
    assert_eq!(st_ids, vec![1_010_000_090_102]);
}
