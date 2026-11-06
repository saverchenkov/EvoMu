//! Integration tests covering Upwell structure combat/fitting/ACL enforcement, V24 ownerGroup
//! membership roles, POS forcefield entry & password clearing, Equinox Sovereignty & Entosis
//! campaign mechanics, Shipcaster combat state preservation, and Faction Warfare militia rules.

#![forbid(unsafe_code)]

use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, IskCents, StarbaseDetailSettings,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_ship_forcefield::can_enter_forcefield;
use eve_server::services::station_structure::structure_ansiblex_navigation::handle_get_jb_structure_destination;
use eve_server::services::station_structure::structure_lifecycle_rpc::handle_jettison_structure_fuel;
use eve_server::services::tactical_modules::ActiveEntosisLinkState;
use glam::DVec3;
use std::collections::HashMap;

const JITA_44: i64 = 60_003_760;
const PERIMETER_SYS: u32 = 30_000_144;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn setup_two_pilots_and_corp(ctx: &ServerContext) -> (SessionState, SessionState, i64) {
    let mut s1 = SessionState::new(851);
    crate::common::login_pilot(ctx, &mut s1, "demo", "demo-secret").await;
    rpc(ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)]).await.unwrap();
    let mut s2 = SessionState::new(852);
    crate::common::login_pilot(ctx, &mut s2, "other", "other-secret").await;
    rpc(ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P2)]).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        for &cid in &[P1, P2] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::from_isk(5_000_000_000);
            }
            for &(sk, lvl) in &[(3363_u32, 5_u8), (3368, 5), (20185, 5)] {
                snap.skills.entry(cid).or_default().insert(sk, TrainedSkillRecord { type_id: sk, level: lvl, skill_points: 256_000.0 });
            }
        }
    });
    ctx.advance_secs(12);
    let corp_id = rpc(ctx, &mut s1, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Defense Corp".into()), PyRep::String("UDFC".into())])
        .await.unwrap().dict_get("corporationID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(12);
    (s1, s2, corp_id)
}

#[tokio::test]
async fn test_structure_combat_state_ansiblex_acl_and_fitting_dropoff() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2, corp_id) = setup_two_pilots_and_corp(&ctx).await;

    let (st_id, jb_a, jb_b) = (880_507_001_i64, 880_507_010_i64, 880_507_011_i64);
    let (fuel_item, mod_item, ore_item) = (880_508_001_i64, 880_508_002_i64, 880_508_003_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(st_id, UpwellStructureRecord {
            structure_id: st_id, type_id: 35_832, owner_corp_id: corp_id, solar_system_id: PERIMETER_SYS,
            name: "Citadel Alpha".into(), state: "AnchorVulnerable".into(), quantum_core_installed: false,
            shield_hp: 1e5, armor_hp: 1e5, hull_hp: 1e5, fuel_blocks: 50, ..Default::default()
        });
        for &(jid, sys) in &[(jb_a, PERIMETER_SYS), (jb_b, 30_000_142_u32)] {
            snap.structures.insert(jid, UpwellStructureRecord {
                structure_id: jid, type_id: 35_841, owner_corp_id: corp_id, solar_system_id: sys,
                name: format!("JB {jid}"), state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100, ..Default::default()
            });
        }
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.jump_bridge_links.insert(jb_a, jb_b);
        ext.jump_bridge_links.insert(jb_b, jb_a);
        ext.access_profiles.entry(jb_a).or_default().blocked_characters.insert(P2);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);

        snap.items.insert(fuel_item, InventoryItemRecord {
            item_id: fuel_item, type_id: 4_051, owner_id: corp_id, location_id: st_id,
            flag: InventoryFlag::StructureFuel, quantity: 50, singleton: false, version: 1, ..Default::default()
        });
        snap.items.insert(mod_item, InventoryItemRecord {
            item_id: mod_item, type_id: 35_892, owner_id: corp_id, location_id: st_id,
            flag: InventoryFlag::ServiceSlot0, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        snap.module_online_states.insert(mod_item, true);
        snap.items.insert(ore_item, InventoryItemRecord {
            item_id: ore_item, type_id: 1230, owner_id: P2, location_id: 100_000_002,
            flag: InventoryFlag::Cargo, quantity: 10, singleton: false, version: 1, ..Default::default()
        });
    });

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&P1).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(st_id);
        ch.solar_system_id = PERIMETER_SYS;
    });
    s1.station_id = None;
    s1.structure_id = Some(st_id);
    assert!(rpc(&ctx, &mut s1, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.is_err());

    ctx.db.with_mut_snapshot(|s| s.structures.get_mut(&st_id).unwrap().state = "ShieldVulnerable".into());
    assert!(rpc(&ctx, &mut s1, "structureControl", "TakeControl", &[PyRep::Integer(st_id)]).await.is_err());

    assert!(rpc(&ctx, &mut s2, "structureJumpBridgeMgr", "GetLinkedStructure", &[PyRep::Integer(jb_a)]).await.is_err());
    assert!(handle_get_jb_structure_destination(&ctx, &mut s2, &[PyRep::Integer(jb_a)]).is_err());

    ctx.db.with_mut_snapshot(|s| {
        s.structures.get_mut(&st_id).unwrap().quantum_core_installed = true;
        let ch = s.characters.get_mut(&P1).unwrap();
        ch.structure_id = None;
        ch.station_id = Some(JITA_44);
    });
    s1.structure_id = None;
    s1.station_id = Some(JITA_44);
    assert_eq!(
        handle_jettison_structure_fuel(
            &ctx, &mut s1, &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(fuel_item)])],
        ).await.unwrap_err().code,
        "NotDockedInStructure"
    );

    let perim = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let _ = perim.spawn_ball_with_meta(
        Ball { id: 100_000_002, type_id: 606, position: DVec3::new(500.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        CombatState::default(), SlimItemMeta::default(),
    ).await;
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(s);
        ext.access_profiles.entry(st_id).or_default().blocked_characters.insert(P2);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
        let ch = s.characters.get_mut(&P2).unwrap();
        ch.station_id = None;
        ch.solar_system_id = PERIMETER_SYS;
        ch.active_ship_id = 100_000_002;
    });
    s2.station_id = None;
    s2.solar_system_id = Some(PERIMETER_SYS);
    s2.ship_id = Some(100_000_002);
    assert!(rpc(&ctx, &mut s2, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(ore_item)])]).await.is_err());

    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&P1).unwrap();
        ch.station_id = None;
        ch.structure_id = Some(st_id);
        ch.solar_system_id = PERIMETER_SYS;
    });
    s1.station_id = None;
    s1.structure_id = Some(st_id);
    handle_jettison_structure_fuel(
        &ctx, &mut s1, &[PyRep::Integer(st_id), PyRep::List(vec![PyRep::Integer(fuel_item)])],
    ).await.expect("Docked JettisonStructureFuel");
    assert_eq!(ctx.db.with_snapshot(|s| s.module_online_states.get(&mod_item).copied()), Some(false));
}

#[tokio::test]
async fn test_ownergroup_v24_roles_and_pos_forcefield_acl_password() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2, corp_id) = setup_two_pilots_and_corp(&ctx).await;

    let (p3_outsider, st_id) = (90_000_093_i64, 880_509_001_i64);
    ctx.db.provision_gm_account("out93", "out93-secret", p3_outsider, "Outsider 93");
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(st_id, UpwellStructureRecord {
            structure_id: st_id, type_id: 35_832, owner_corp_id: corp_id, solar_system_id: PERIMETER_SYS,
            name: "ACL Citadel".into(), state: "ShieldVulnerable".into(), quantum_core_installed: true, fuel_blocks: 100, ..Default::default()
        });
    });
    let g1 = rpc(&ctx, &mut s1, "ownerGroupManager", "Create", &[PyRep::String("Group 1".into()), PyRep::String("".into())])
        .await.unwrap().dict_get("groupID").and_then(|v| v.as_i64()).unwrap();
    let g2 = rpc(&ctx, &mut s1, "ownerGroupManager", "Create", &[PyRep::String("Group 2".into()), PyRep::String("".into())])
        .await.unwrap().dict_get("groupID").and_then(|v| v.as_i64()).unwrap();

    rpc(&ctx, &mut s1, "ownerGroupManager", "AddMembers", &[
        PyRep::Integer(g1),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(P2), PyRep::Integer(1)]), PyRep::Tuple(vec![PyRep::Integer(p3_outsider), PyRep::Integer(3)])]),
    ]).await.expect("AddMembers with MANAGER=1 and EXCLUDED=3");
    rpc(&ctx, &mut s1, "ownerGroupManager", "AddMembers", &[
        PyRep::Integer(g2),
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(p3_outsider), PyRep::Integer(3)])]),
    ]).await.unwrap();
    ctx.db.with_mut_snapshot(|s| {
        let mut ext = eve_server::services::station_structure::structure_upwell_ops::load_ext_state(s);
        let prof = ext.access_profiles.entry(st_id).or_default();
        prof.access_group_ids.insert(g1);
        prof.access_group_ids.insert(g2);
        eve_server::services::station_structure::structure_upwell_ops::save_ext_state(s, &ext);
    });
    assert!(ctx.db.with_snapshot(|s| {
        eve_server::services::structure_acl_defense_rules::verify_structure_docking_acl(&ctx, s, st_id, p3_outsider).is_err()
    }));

    assert!(rpc(&ctx, &mut s2, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(g1), PyRep::List(vec![PyRep::Integer(p3_outsider)]), PyRep::Integer(2)]).await.is_err());
    rpc(&ctx, &mut s2, "ownerGroupManager", "UpdateMembershipTypes", &[PyRep::Integer(g1), PyRep::List(vec![PyRep::Integer(p3_outsider)]), PyRep::Integer(3)])
        .await.expect("UpdateMembershipTypes EXCLUDED=3");

    rpc(&ctx, &mut s1, "ownerGroupManager", "DeleteMembers", &[PyRep::Integer(g1), PyRep::List(vec![PyRep::Integer(p3_outsider)])]).await.unwrap();
    assert!(ctx.db.with_snapshot(|s| {
        eve_server::services::structure_acl_defense_rules::verify_structure_docking_acl(&ctx, s, st_id, p3_outsider).is_err()
    }));

    let (tower_id, p2_ship) = (880_510_001_i64, 100_000_002_i64);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&P2).unwrap().corporation_id = corp_id;
        s.corporations.get_mut(&corp_id).unwrap().members.insert(P2);
        s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(P2, 0);
        s.items.insert(tower_id, InventoryItemRecord {
            item_id: tower_id, type_id: 12_235, owner_id: corp_id, location_id: PERIMETER_SYS as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        s.starbase_settings.insert(tower_id, StarbaseDetailSettings {
            password: Some("secret42".into()), allow_corporation_members: false, allow_alliance_members: false, ..Default::default()
        });
        s.module_online_states.insert(tower_id, true);
    });
    s2.corporation_id = Some(corp_id);
    s2.ship_id = Some(p2_ship);

    assert!(!can_enter_forcefield(&ctx, P2, p2_ship, tower_id));
    rpc(&ctx, &mut s2, "ship", "SetShipPassword", &[PyRep::String("secret42".into())]).await.expect("SetShipPassword");
    assert!(can_enter_forcefield(&ctx, P2, p2_ship, tower_id));
    rpc(&ctx, &mut s2, "ship", "SetShipPassword", &[PyRep::String("".into())]).await.expect("Clear SetShipPassword");
    assert!(!can_enter_forcefield(&ctx, P2, p2_ship, tower_id));
    let _ = corp_roles::ROLE_STARBASE_CONFIG;
}

#[tokio::test]
async fn test_sov_hub_vuln_window_contested_entosis_decap_hacked_recovery_and_cyno_jammer() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, _s2, corp_id) = setup_two_pilots_and_corp(&ctx).await;

    let null_sys = ctx.sde.solar_systems.values().find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000).map(|s| s.solar_system_id).expect("null-sec system");
    let (hub_id, ally_def, ally_atk) = (880_512_001_i64, 99_512_001_i64, 99_512_002_i64);
    let now = ctx.now_unix();
    let cur_hour = ((now % 86_400) / 3_600) as u64;
    ctx.db.with_mut_snapshot(|s| {
        s.structures.insert(hub_id, UpwellStructureRecord {
            structure_id: hub_id, type_id: 81_238, owner_corp_id: corp_id, solar_system_id: null_sys,
            name: "Sov Hub".into(), state: "ArmorReinforced".into(), quantum_core_installed: true, ..Default::default()
        });
        s.structure_vulnerability_windows.insert(hub_id, (cur_hour, 2));
        s.sov_runtime.hacked_hubs.insert(hub_id, now.saturating_sub(10));
    });
    assert!(ctx.db.with_snapshot(|s| eve_server::services::sov_gameplay::is_hub_within_vulnerability_window_in_snap(s, hub_id, now + 1_800)));

    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, now).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.structures[&hub_id].state.clone()), "ShieldVulnerable");

    let node_id = 880_512_101_i64;
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().alliance_id = Some(ally_def);
        s.characters.get_mut(&P1).unwrap().alliance_id = Some(ally_def);
        s.characters.get_mut(&P2).unwrap().alliance_id = Some(ally_atk);
        s.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ally_def);
        s.item_positions.insert(node_id, [0.0, 0.0, 0.0]);
        s.sov_runtime.command_nodes.insert(node_id, (hub_id, null_sys, ally_atk));
        s.sov_runtime.entosis_capture_progress.insert(node_id, (P2, ally_atk, 100_000_002, 239.0, now));
    });
    let db_key = std::sync::Arc::as_ptr(&ctx.db) as usize;
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        for &(sid, cid, aid, mid) in &[(100_000_001_i64, P1, ally_def, 901_i64), (100_000_002, P2, ally_atk, 902)] {
            tst.entosis_active_links.insert((db_key, sid), ActiveEntosisLinkState {
                ship_id: sid, char_id: cid, side_id: aid, module_id: mid, target_id: node_id,
                cycle_index: 2, cycle_duration_s: 120.0, started_unix: now.saturating_sub(10), expires_unix: now + 300, is_capital: false,
            });
        }
    }
    let ns = ctx.systems.get_or_spawn(null_sys);
    for &(bid, x) in &[(node_id, 0.0), (100_000_001, 1_000.0), (100_000_002, 2_000.0)] {
        let _ = ns.spawn_ball_with_meta(
            Ball { id: bid, type_id: 587, position: DVec3::new(x, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop },
            CombatState::default(), SlimItemMeta::default(),
        ).await;
    }
    ctx.advance_secs(10);
    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, ctx.now_unix()).await;
    assert_eq!(ctx.db.with_snapshot(|s| s.sov_runtime.entosis_capture_progress[&node_id].3), 239.0);

    ctx.tactical_state.write().unwrap().entosis_active_links.remove(&(db_key, 100_000_002));
    ctx.advance_secs(10);
    eve_server::services::sov_gameplay::tick_sov_gameplay(&ctx, ctx.now_unix()).await;
    ctx.db.with_snapshot(|s| {
        assert!(s.sov_runtime.command_nodes.contains_key(&node_id));
        assert!((s.sov_runtime.entosis_capture_progress[&node_id].3 - 229.0).abs() < 1e-6);
    });

    s1.solar_system_id = Some(null_sys);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&P1).unwrap().solar_system_id = null_sys;
        s.items.insert(880_512_201, InventoryItemRecord { item_id: 880_512_201, type_id: 33_700, owner_id: P1, location_id: null_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    let jam_mtu = rpc(&ctx, &mut s1, "beyonce", "GetCynoJammerState", &[]).await.unwrap();
    assert_eq!(jam_mtu.dict_get("isJammed"), Some(&PyRep::Bool(false)));

    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(880_512_202, InventoryItemRecord { item_id: 880_512_202, type_id: 33_476, owner_id: P1, location_id: null_sys as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default() });
    });
    let jam_inhib = rpc(&ctx, &mut s1, "beyonce", "GetCynoJammerState", &[]).await.unwrap();
    assert_eq!(jam_inhib.dict_get("isJammed"), Some(&PyRep::Bool(true)));
}

#[tokio::test]
async fn test_shipcaster_preserves_combat_damage_and_fw_militia_invariants() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let (mut s1, mut s2, corp_id) = setup_two_pilots_and_corp(&ctx).await;

    let (ship_id, sys_a, sys_b, pad_id) = (100_000_001_i64, PERIMETER_SYS, 30_000_142_u32, 880_514_001_i64);
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&P1).unwrap();
        ch.station_id = None;
        ch.solar_system_id = sys_a;
        ch.active_ship_id = ship_id;
        s.items.get_mut(&ship_id).unwrap().location_id = sys_a as i64;
        s.items.insert(pad_id, InventoryItemRecord {
            item_id: pad_id, type_id: 87_162, owner_id: 500_001, location_id: sys_b as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
        s.item_positions.insert(pad_id, [10_000.0, 0.0, 0.0]);
    });
    s1.station_id = None;
    s1.solar_system_id = Some(sys_a);
    s1.ship_id = Some(ship_id);

    let ha = ctx.systems.get_or_spawn(sys_a);
    let mut damaged_cs = ctx.build_ship_combat_state(P1, ship_id, 606);
    damaged_cs.shield_hp = (damaged_cs.max_shield_hp * 0.45).max(1.0);
    damaged_cs.armor_hp = (damaged_cs.max_armor_hp * 0.35).max(1.0);
    damaged_cs.structure_hp = (damaged_cs.max_structure_hp * 0.55).max(1.0);
    damaged_cs.capacitor = (damaged_cs.max_capacitor * 0.25).max(1.0);
    damaged_cs.entropic_spool_cycles = 7;
    let expected_hp = (damaged_cs.shield_hp, damaged_cs.armor_hp, damaged_cs.structure_hp, damaged_cs.capacitor);
    let _ = ha.spawn_ball_with_meta(
        Ball { id: ship_id, type_id: 606, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop },
        damaged_cs, ctx.build_ship_slim_meta(P1, 606),
    ).await;

    eve_server::services::shipcaster_ops::execute_shipcaster_jump(
        &ctx, &mut s1, P1, ship_id, sys_a, sys_b, DVec3::new(10_000.0, 0.0, 0.0), DVec3::X,
    ).await.expect("execute_shipcaster_jump");
    let post_cs = ctx.systems.get_or_spawn(sys_b).get_combat_state(ship_id).await.unwrap().expect("jumped ship combat state");
    assert!((post_cs.shield_hp - expected_hp.0).abs() < 1e-4);
    assert!((post_cs.armor_hp - expected_hp.1).abs() < 1e-4);
    assert!((post_cs.structure_hp - expected_hp.2).abs() < 1e-4);
    assert!((post_cs.capacitor - expected_hp.3).abs() < 1e-4);
    assert_eq!(post_cs.entropic_spool_cycles, 0);
    assert!(post_cs.is_cloaked);

    ctx.db.with_mut_snapshot(|s| {
        for &cid in &[P1, P2] {
            let ch = s.characters.get_mut(&cid).unwrap();
            ch.station_id = Some(JITA_44);
            ch.corporation_id = corp_id;
        }
        s.corporations.get_mut(&corp_id).unwrap().members.insert(P2);
    });
    s1.station_id = Some(JITA_44);
    s1.ship_id = None;
    s2.station_id = Some(JITA_44);
    s2.ship_id = None;
    s2.corporation_id = Some(corp_id);

    let neutral_status = rpc(&ctx, &mut s1, "facWarMgr", "GetCorpFactionalWarStatus", &[PyRep::Integer(corp_id)]).await.unwrap();
    assert_eq!(neutral_status.dict_get("factionID"), Some(&PyRep::None));
    assert_eq!(neutral_status.dict_get("status"), Some(&PyRep::Integer(0)));

    rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500_004)]).await.expect("P2 direct enlist Gallente");
    rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_001)]).await.expect("Corp enlist Caldari");
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&P2].war_faction_id), Some(500_001));
    assert!(!ctx.db.with_snapshot(|s| s.fw_runtime.direct_enlisted_chars.contains_key(&P2)));

    assert_eq!(rpc(&ctx, &mut s2, "fwCharacterEnlistmentMgr", "RemoveMyDirectEnlistment", &[]).await.unwrap_err().code, "FwNotEnlisted");
    assert_eq!(s2.war_faction_id, Some(500_001));

    ctx.advance_secs(90_000);
    rpc(&ctx, &mut s1, "facWarMgr", "LeaveFactionAsCorporation", &[]).await.expect("LeaveFactionAsCorporation");
    assert_eq!(rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCorporation", &[PyRep::Integer(500_001)]).await.unwrap_err().code, "MilitiaCooldownActive");
}
