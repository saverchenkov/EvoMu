//! Extra unit and mutation-killing tests for `pve_special_sites.rs`.

use crate::router::ServerContext;
use crate::services::pve_special_sites::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn find_highsec_system(sde: &SdeRegistry) -> SolarSystemId {
    for &preferred in crate::services::pve_homefront_sites::HOMEFRONT_SYSTEMS {
        if sde.solar_systems.contains_key(&preferred) {
            return preferred;
        }
    }
    30_000_142
}

fn find_ship_type_id(sde: &SdeRegistry) -> u32 {
    sde.types
        .values()
        .filter(|t| t.category_id == 6 && t.group_id == 25 && t.capacity > 50.0)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(587)
}

fn make_ctx_and_pilot(
    sys_id: SolarSystemId,
    with_ship: bool,
) -> (ServerContext, SessionState, i64, i64) {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let char_id: i64 = 90_020_001;
    let ship_id: i64 = if with_ship { 1_020_000_001 } else { 0 };
    let ship_tid = find_ship_type_id(&sde);
    db.provision_gm_account("extra_spec_pilot", "pw", char_id, "ExtraSpecPilot");
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(1_000_000);
        }
        if with_ship {
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_tid,
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_000_000)));
    let mut session = SessionState::new(201);
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = if with_ship { Some(ship_id) } else { None };
    (ctx, session, char_id, ship_id)
}

#[test]
fn spec_now_sec_and_next_spec_seq_cover_zero_and_clock_comparison_branches() {
    let mut reg = PveSpecialSitesRegistry::default();
    assert_eq!(spec_now_sec(&reg), 1_775_000_000);
    reg.sim_clock_sec = 1_800_000_050;
    assert_eq!(spec_now_sec(&reg), 1_800_000_050);
    reg.sim_clock_sec = 1_800_000_500;
    assert_eq!(spec_now_sec(&reg), 1_800_000_500);

    assert_eq!(next_spec_seq(&mut reg), 1);
    assert_eq!(next_spec_seq(&mut reg), 2);
}

#[test]
fn with_pve_special_reg_db_handles_corrupt_json_and_syncs_incursions_state() {
    let sde = shared_sde();
    let sys_id = find_highsec_system(&sde);
    let (ctx, _, _, _) = make_ctx_and_pilot(sys_id, false);

    // 1. Corrupt legacy blob JSON migrates as default
    ctx.db.with_mut_snapshot(|snap| {
        snap.pve_special_sites = Default::default();
        snap.service_state_blobs
            .insert("incursions_state".to_string(), "{invalid json".to_string());
        snap.migrate_legacy_service_state();
    });
    with_pve_special_reg_db(&ctx.db, 1_775_000_010, |reg| {
        assert_eq!(reg.next_seq, 0);
        reg.next_seq = 5;
    });

    // 2. Corrupt legacy owner note JSON migrates as default when typed state is empty
    ctx.db.with_mut_snapshot(|snap| {
        snap.pve_special_sites = Default::default();
        snap.service_state_blobs.remove("incursions_state");
        snap.owner_notes.insert(
            -8_300_000_000_000,
            eve_db::OwnerNoteRecord {
                note_id: -8_300_000_000_000,
                character_id: -83,
                item_id: 0,
                label: "pve_special_sites_registry".into(),
                note: "not-json".into(),
                timestamp_unix: 100,
            },
        );
        snap.migrate_legacy_service_state();
    });
    with_pve_special_reg_db(&ctx.db, 1_775_000_020, |reg| {
        assert_eq!(reg.next_seq, 0);
        let cid = 20_000_001_u32;
        reg.incursions.insert(
            cid,
            IncursionTaleRecord {
                tale_id: 901,
                constellation_id: cid,
                constellation_name: "TestConst".into(),
                staging_solar_system_id: 30_000_001,
                vanguard_solar_system_ids: vec![30_000_001, 30_000_002],
                assault_solar_system_ids: vec![30_000_002, 30_000_003],
                hq_solar_system_id: 30_000_004,
                influence: 0.85,
                state: "Mobilizing".into(),
                mothership_spawned: true,
                mothership_defeated: false,
                sites: Vec::new(),
                lockout_until_filetime: None,
                tale_stop_published: false,
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let inc = snap.incursions.get(&20_000_001).expect("synced incursion");
        assert!(inc.has_boss);
        assert_eq!(inc.state, "mobilizing");
        assert_eq!(
            inc.infested_solar_systems,
            vec![30_000_001, 30_000_002, 30_000_003, 30_000_004]
        );
    });

    // When mothership_defeated is true, has_boss becomes false; when state == "Ended", removed
    with_pve_special_reg_db(&ctx.db, 1_775_000_030, |reg| {
        let inc = reg.incursions.get_mut(&20_000_001).unwrap();
        inc.mothership_defeated = true;
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.incursions.get(&20_000_001).unwrap().has_boss);
    });

    with_pve_special_reg_db(&ctx.db, 1_775_000_040, |reg| {
        let inc = reg.incursions.get_mut(&20_000_001).unwrap();
        inc.state = "Ended".into();
    });
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.incursions.contains_key(&20_000_001));
    });
}

#[test]
fn ensure_triglavian_sites_for_system_covers_fortress_edencom_factions_and_triglavian() {
    let mut reg = PveSpecialSitesRegistry::default();
    let fortress_sys = edencom_fortress_systems()[0];
    ensure_triglavian_sites_for_system(&mut reg, fortress_sys, 500_001);
    assert!(
        !reg.triglavian_sites.contains_key(&fortress_sys),
        "fortress system must not spawn triglavian sites"
    );

    let edencom_systems = edencom_minor_victory_systems();
    let expected_factions = [
        (500_001_u32, 52_940_u32, 52_938_u32, 52_934_u32),
        (500_002_u32, 52_963_u32, 52_961_u32, 52_957_u32),
        (500_003_u32, 52_931_u32, 52_929_u32, 52_925_u32),
        (500_004_u32, 52_954_u32, 52_952_u32, 52_948_u32),
    ];
    for (idx, &(fac, bs, bc, fr)) in expected_factions.iter().enumerate() {
        let sys_id = edencom_systems[idx];
        ensure_triglavian_sites_for_system(&mut reg, sys_id, fac);
        let sites = reg.triglavian_sites.get(&sys_id).expect("edencom sites");
        assert_eq!(sites.len(), 2);
        assert_eq!(sites[0].faction_owner_id, 500_027);
        assert_eq!(sites[0].boss_type_id, bs);
        assert_eq!(sites[0].escort_type_ids, vec![bc, fr]);
        assert_eq!(sites[1].boss_type_id, bc);
        assert_eq!(sites[1].escort_type_ids, vec![fr]);
    }

    // Idempotent call does not overwrite completed state
    let first_edencom = edencom_systems[0];
    reg.triglavian_sites.get_mut(&first_edencom).unwrap()[0].completed = true;
    ensure_triglavian_sites_for_system(&mut reg, first_edencom, 500_001);
    assert!(reg.triglavian_sites.get(&first_edencom).unwrap()[0].completed);

    // Non-EDENCOM system spawns Major Conduit and Minor Conduit owned by 500_026
    let trig_sys = triglavian_minor_victory_systems()[0];
    ensure_triglavian_sites_for_system(&mut reg, trig_sys, 500_001);
    let trig_sites = reg.triglavian_sites.get(&trig_sys).expect("trig sites");
    assert_eq!(trig_sites.len(), 2);
    assert_eq!(trig_sites[0].faction_owner_id, 500_026);
    assert_eq!(trig_sites[0].dungeon_id, 8802);
    assert_eq!(trig_sites[0].boss_type_id, 48_122);
    assert_eq!(trig_sites[1].dungeon_id, 8742);
    assert_eq!(trig_sites[1].boss_type_id, 48_126);
}

#[test]
fn build_combat_state_and_deposit_loot_cover_fallback_and_container_branches() {
    let sde = shared_sde();
    let fallback_cs = build_special_site_npc_combat_state(&sde, 999_999_999);
    assert_eq!(fallback_cs.max_shield_hp, 1_200.0);
    assert_eq!(fallback_cs.max_armor_hp, 1_200.0);
    assert_eq!(fallback_cs.max_structure_hp, 1_000.0);

    let sys_id = find_highsec_system(&sde);
    let (ctx, _, char_id, ship_id) = make_ctx_and_pilot(sys_id, true);

    // Fits inside ship cargo -> returns None
    let direct = ctx.db.with_mut_snapshot(|snap| {
        deposit_loot_into_ship_or_container_in_snapshot(
            snap,
            &ctx.sde,
            char_id,
            sys_id,
            ship_id,
            &[(77_001, 34, 10)],
        )
    });
    assert!(direct.is_none());
    ctx.db.with_snapshot(|snap| {
        let item = snap.items.get(&77_001).expect("loot in ship");
        assert_eq!(item.location_id, ship_id);
        assert_eq!(item.flag, InventoryFlag::Cargo);
    });

    // No ship (ship_id == 0) -> spawns jetcan container (type 23)
    let container_id = ctx.db.with_mut_snapshot(|snap| {
        deposit_loot_into_ship_or_container_in_snapshot(
            snap,
            &ctx.sde,
            char_id,
            sys_id,
            0,
            &[(77_002, 34, 10)],
        )
    });
    let cid = container_id.expect("spawned cargo container");
    ctx.db.with_snapshot(|snap| {
        let cont = snap.items.get(&cid).expect("container record");
        assert_eq!(cont.type_id, 23);
        let loot = snap.items.get(&77_002).expect("loot in container");
        assert_eq!(loot.location_id, cid);
    });
}

#[test]
fn build_interference_state_keyval_and_gate_access_cover_all_bands() {
    for (band, expected_label) in [(1_u8, "Low"), (2_u8, "Medium"), (3_u8, "High")] {
        let kv = build_interference_state_keyval(30_000_142, 0.5, band);
        assert_eq!(
            kv.dict_get("bandLabel").and_then(|v| v.as_str()),
            Some(expected_label)
        );
        assert_eq!(
            kv.dict_get("interferencePct").and_then(|v| v.as_f64()),
            Some(50.0)
        );
    }

    let sys_id = 31_000_001;
    let (ctx, _, _, _) = make_ctx_and_pilot(sys_id, true);
    ensure_special_sites_initialized(&ctx);

    let gate_ball_id = read_pve_special_reg(&ctx, |reg| {
        reg.drifter_hives.get(&sys_id).unwrap().rooms[0].beacon_ball_id
    });
    let err = check_drifter_hive_gate_access(&ctx, sys_id, gate_ball_id)
        .expect_err("uncleared room must block acceleration gate");
    assert_eq!(err.code, "RoomNotCleared");

    // Mark room NPCs destroyed -> gate opens
    with_pve_special_reg(&ctx, |reg| {
        let room = &mut reg.drifter_hives.get_mut(&sys_id).unwrap().rooms[0];
        room.destroyed_npc_ball_ids = room.npc_ball_ids.clone();
    });
    check_drifter_hive_gate_access(&ctx, sys_id, gate_ball_id)
        .expect("destroyed NPCs should allow gate access");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_special_dungeon_instance_homefront_fleet_scaling_and_error_guards() {
    let sde = shared_sde();
    let sys_id = find_highsec_system(&sde);
    let (ctx, mut session, char_id, _) = make_ctx_and_pilot(sys_id, true);

    // Guard: docked in station
    session.station_id = Some(60_003_760);
    let err = complete_special_dungeon_instance(&ctx, &mut session, 1, 1)
        .await
        .expect_err("docked in station");
    assert_eq!(err.code, "ShipDocked");
    session.station_id = None;

    // Guard: NotInSpace
    session.solar_system_id = None;
    let err = complete_special_dungeon_instance(&ctx, &mut session, 1, 1)
        .await
        .expect_err("not in space");
    assert_eq!(err.code, "NotInSpace");
    session.solar_system_id = Some(sys_id);

    // Guard: DungeonNotFound
    let err = complete_special_dungeon_instance(&ctx, &mut session, 999_888_777, 1)
        .await
        .expect_err("unknown dungeon");
    assert_eq!(err.code, "DungeonNotFound");

    // Seed 3 custom homefront sites to test < optimal, == optimal, and > optimal fleet scaling
    with_pve_special_reg(&ctx, |reg| {
        reg.homefront_sites.insert(
            sys_id,
            vec![
                HomefrontSiteRecord {
                    instance_id: 810_001,
                    dungeon_id: 7001,
                    dungeon_name: "HF Under".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 4,
                    base_pilot_isk: 10_000_000,
                    completed: false,
                    reward_paid_isk: 0,
                    boss_npc_ball_id: 1_910_001,
                    boss_type_id: 48_090,
                },
                HomefrontSiteRecord {
                    instance_id: 810_002,
                    dungeon_id: 7002,
                    dungeon_name: "HF Exact".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 1,
                    base_pilot_isk: 10_000_000,
                    completed: false,
                    reward_paid_isk: 0,
                    boss_npc_ball_id: 1_910_002,
                    boss_type_id: 48_090,
                },
                HomefrontSiteRecord {
                    instance_id: 810_003,
                    dungeon_id: 7003,
                    dungeon_name: "HF Over".into(),
                    solar_system_id: sys_id,
                    optimal_fleet_size: 1,
                    base_pilot_isk: 10_000_000,
                    completed: false,
                    reward_paid_isk: 0,
                    boss_npc_ball_id: 1_910_003,
                    boss_type_id: 48_090,
                },
            ],
        );
    });

    // Wrong system guard
    session.solar_system_id = Some(30_000_001);
    if sys_id != 30_000_001 {
        let err = complete_special_dungeon_instance(&ctx, &mut session, 810_001, 1)
            .await
            .expect_err("wrong system");
        assert_eq!(err.code, "NotInSiteSystem");
    }
    session.solar_system_id = Some(sys_id);

    // Boss alive guard -> remove boss ball to complete
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    ensure_special_sites_spawned(&ctx).await;
    let err = complete_special_dungeon_instance(&ctx, &mut session, 810_001, 1)
        .await
        .expect_err("boss alive");
    assert_eq!(err.code, "WaveNpcsRemaining");

    for bid in [1_910_001_i64, 1_910_002, 1_910_003] {
        let _ = sys_handle.remove_ball(bid).await;
    }

    // 1) eff_fleet_count (1) < optimal (4) -> mult = 0.25 -> 2,500,000 ISK
    let res1 = complete_special_dungeon_instance(&ctx, &mut session, 810_001, 1)
        .await
        .expect("complete under-sized");
    assert_eq!(
        res1.dict_get("rewardPaidIsk").and_then(|v| v.as_i64()),
        Some(2_500_000)
    );

    // Completing again -> SiteAlreadyCompleted
    let err = complete_special_dungeon_instance(&ctx, &mut session, 810_001, 1)
        .await
        .expect_err("already completed");
    assert_eq!(err.code, "SiteAlreadyCompleted");

    // 2) eff_fleet_count (1) == optimal (1) -> mult = 1.0 -> 10,000,000 ISK
    let res2 = complete_special_dungeon_instance(&ctx, &mut session, 810_002, 0)
        .await
        .expect("complete optimal-sized");
    assert_eq!(
        res2.dict_get("rewardPaidIsk").and_then(|v| v.as_i64()),
        Some(10_000_000)
    );

    // 3) Put pilot in a 2-member fleet in same system so actual_fleet_count == 2 > optimal (1) -> mult = 0.75 -> 7,500,000 ISK
    let wingman_id = 90_020_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.fleet_id = Some(501);
        }
        if let Some(mut w) = snap.characters.get(&char_id).cloned() {
            w.character_id = wingman_id;
            w.fleet_id = Some(501);
            w.solar_system_id = sys_id;
            w.station_id = None;
            w.structure_id = None;
            snap.characters.insert(wingman_id, w);
        }
    });
    let res3 = complete_special_dungeon_instance(&ctx, &mut session, 810_003, 0)
        .await
        .expect("complete over-sized fleet");
    assert_eq!(
        res3.dict_get("rewardPaidIsk").and_then(|v| v.as_i64()),
        Some(7_500_000)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn complete_special_dungeon_instance_triglavian_standings_and_escort_guards() {
    let edencom_sys = edencom_minor_victory_systems()[0];
    let (ctx, mut session, char_id, _) = make_ctx_and_pilot(edencom_sys, true);
    ensure_special_sites_spawned(&ctx).await;

    let (site0, site1) = read_pve_special_reg(&ctx, |reg| {
        let list = reg.triglavian_sites.get(&edencom_sys).unwrap();
        (list[0].clone(), list[1].clone())
    });
    let sys_handle = ctx.systems.get_or_spawn(edencom_sys);

    // Remove boss of site0, leave escorts alive -> WaveNpcsRemaining for escort
    let _ = sys_handle.remove_ball(site0.boss_npc_ball_id).await;
    let err = complete_special_dungeon_instance(&ctx, &mut session, site0.instance_id, 1)
        .await
        .expect_err("escort still alive");
    assert_eq!(err.code, "WaveNpcsRemaining");

    for &ebid in &site0.escort_ball_ids {
        let _ = sys_handle.remove_ball(ebid).await;
    }

    // Complete EDENCOM-owned site (500_027) -> +0.15 to 500_026, -0.15 to 500_027
    let ok0 = complete_special_dungeon_instance(&ctx, &mut session, site0.instance_id, 1)
        .await
        .expect("complete edencom site");
    assert_eq!(
        ok0.dict_get("baseIsk").and_then(|v| v.as_i64()),
        Some(site0.base_isk)
    );
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert!((ch.standings.get(&500_026).copied().unwrap_or(0.0) - 0.15).abs() < 1e-6);
        assert!((ch.standings.get(&500_027).copied().unwrap_or(0.0) + 0.15).abs() < 1e-6);
    });

    // Flip site1 to Triglavian-owned (500_026) and clear its NPCs -> asymptotic +0.15225 to 500_027, -0.15225 to 500_026
    with_pve_special_reg(&ctx, |reg| {
        reg.triglavian_sites.get_mut(&edencom_sys).unwrap()[1].faction_owner_id = 500_026;
    });
    let _ = sys_handle.remove_ball(site1.boss_npc_ball_id).await;
    for &ebid in &site1.escort_ball_ids {
        let _ = sys_handle.remove_ball(ebid).await;
    }
    complete_special_dungeon_instance(&ctx, &mut session, site1.instance_id, 1)
        .await
        .expect("complete triglavian site");
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id).unwrap();
        assert!((ch.standings.get(&500_026).copied().unwrap_or(1.0) - (-0.00225)).abs() < 1e-6);
        assert!((ch.standings.get(&500_027).copied().unwrap_or(1.0) - 0.00225).abs() < 1e-6);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_handlers_triglavian_signatures_and_drifter_hive_progression() {
    let hive_sys = 31_000_001_u32;
    let (ctx, mut session, _, _) = make_ctx_and_pilot(hive_sys, true);
    let kw = HashMap::new();

    // GetSignatureInstances in Drifter wormhole returns hive dungeon dict
    let sig_res = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetSignatureInstances",
        &[],
        &kw,
    )
    .await
    .expect("handled in drifter sys")
    .expect("ok sigs");
    assert!(matches!(sig_res, PyRep::Dict(_)));

    // GetTriglavianSiteInstances invalid arg errors
    let bad_arg = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::String("not-an-int".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("non-int arg");
    assert_eq!(bad_arg.code, "UnknownSolarSystem");

    let unknown_sys = handle_pve_special_sites(
        &ctx,
        &mut session,
        "dungeonInstanceCacheMgr",
        "GetTriglavianSiteInstances",
        &[PyRep::Integer(99_999_999)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("unknown sys");
    assert_eq!(unknown_sys.code, "UnknownSolarSystem");

    // Drifter hive room progression: clear room 1 -> room 2 -> room 3 -> cleared
    let sys_handle = ctx.systems.get_or_spawn(hive_sys);
    for expected_next_room in [2_i64, 3_i64] {
        let npc_ids = read_pve_special_reg(&ctx, |reg| {
            let h = reg.drifter_hives.get(&hive_sys).unwrap();
            h.rooms
                .iter()
                .find(|r| r.room_index == h.current_room)
                .unwrap()
                .npc_ball_ids
                .clone()
        });
        for bid in npc_ids {
            let _ = sys_handle.remove_ball(bid).await;
        }
        let step = complete_special_dungeon_instance(&ctx, &mut session, hive_sys as i64, 1)
            .await
            .expect("advance room");
        assert_eq!(
            step.dict_get("currentRoom").and_then(|v| v.as_i64()),
            Some(expected_next_room)
        );
        assert_eq!(
            step.dict_get("cleared").and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    // Final room: warden alive blocks completion, removing warden completes hive
    let warden_bid = read_pve_special_reg(&ctx, |reg| {
        reg.drifter_hives.get(&hive_sys).unwrap().warden_npc_ball_id
    });
    let err = complete_special_dungeon_instance(&ctx, &mut session, hive_sys as i64, 1)
        .await
        .expect_err("warden still alive");
    assert_eq!(err.code, "WaveNpcsRemaining");
    let _ = sys_handle.remove_ball(warden_bid).await;
    let final_kv = complete_special_dungeon_instance(&ctx, &mut session, hive_sys as i64, 1)
        .await
        .expect("hive cleared");
    assert_eq!(
        final_kv.dict_get("cleared").and_then(|v| v.as_bool()),
        Some(true)
    );
}
