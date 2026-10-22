//! Integration tests covering open RPC parity rows across station, home station,
//! jump clone, and corp station management services.

use crate::common;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord, OwnerNoteRecord,
    TrainedSkillRecord, UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;
const STATION_JITA: i64 = 60_003_760;
const SYS_JITA: u32 = 30_000_142;
const CORP_CALDARI_NAVY: i64 = 1_000_035;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn events_from(notifs: &[PendingMachoNotification], event: &str) -> Vec<Vec<PyRep>> {
    notifs
        .iter()
        .filter(|n| n.event == event)
        .map(|n| n.args.clone())
        .collect()
}

fn seed_test_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
}

#[tokio::test]
async fn test_station_services_and_corp_station_mgr_parity() {
    // parity: rpc:corpStationMgr.DoStandingCheckForStationService, rpc:station.GetGuests, rpc:stationSvc.GetAllianceSystems, rpc:stationSvc.GetSolarSystem, rpc:stationSvc.GetStationItemBits, rpc:stationSvc.GetStationsForOwner, rpc:stationSvc.GetSystemsForAlliance
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 201, "demo", "demo-secret", PILOT1).await;
    let mut s2 = setup_pilot(&ctx, 202, "other", "other-secret", PILOT2).await;

    // Initially PILOT1 is docked at STATION_JITA, PILOT2 is undocked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.station_id = Some(STATION_JITA);
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT2) {
            c2.station_id = None;
            c2.structure_id = None;
        }
    });
    s1.station_id = Some(STATION_JITA);
    s2.station_id = None;
    s2.structure_id = None;

    // 1. station.GetGuests
    // When only PILOT1 is docked
    let guests_single = rpc(&ctx, &mut s1, "station", "GetGuests", &[])
        .await
        .expect("station.GetGuests single");
    let PyRep::List(guest_list_1) = guests_single else {
        panic!("station.GetGuests must return a List");
    };
    let mut cids_1 = Vec::new();
    for g in &guest_list_1 {
        let PyRep::Tuple(t) = g else {
            panic!("Guest entry must be a Tuple");
        };
        assert_eq!(t.len(), 4, "Guest tuple must have 4 elements (char, corp, ally, fac)");
        cids_1.push(t[0].as_i64().expect("characterID"));
    }
    assert!(cids_1.contains(&PILOT1), "PILOT1 must be in guest list");
    assert!(!cids_1.contains(&PILOT2), "PILOT2 undocked must NOT be in guest list");

    // Negative: undocked pilot calls station.GetGuests
    let err_undocked = rpc(&ctx, &mut s2, "station", "GetGuests", &[])
        .await
        .expect_err("Undocked pilot calling station.GetGuests must fail");
    assert_eq!(err_undocked.code, "NotDockedInStation");

    // Dock PILOT2 at STATION_JITA
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c2) = snap.characters.get_mut(&PILOT2) {
            c2.station_id = Some(STATION_JITA);
        }
    });
    s2.station_id = Some(STATION_JITA);

    let guests_both = rpc(&ctx, &mut s1, "station", "GetGuests", &[])
        .await
        .expect("station.GetGuests both");
    let PyRep::List(guest_list_2) = guests_both else {
        panic!("station.GetGuests must return a List");
    };
    let mut cids_2 = Vec::new();
    for g in &guest_list_2 {
        let PyRep::Tuple(t) = g else {
            panic!("Guest entry must be a Tuple");
        };
        cids_2.push(t[0].as_i64().expect("characterID"));
    }
    assert!(cids_2.contains(&PILOT1), "PILOT1 must be in guest list");
    assert!(cids_2.contains(&PILOT2), "PILOT2 docked must be in guest list");

    // 2. stationSvc.GetStationItemBits
    let bits_rep = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStationItemBits",
        &[PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect("stationSvc.GetStationItemBits");
    let PyRep::Tuple(bits) = bits_rep else {
        panic!("GetStationItemBits must return a 4-tuple");
    };
    assert_eq!(bits.len(), 4);
    assert_eq!(bits[0], PyRep::Integer(CORP_CALDARI_NAVY));
    assert_eq!(bits[1], PyRep::Integer(STATION_JITA));
    assert_eq!(bits[2], PyRep::Integer(14)); // Caldari Navy Assembly Plant operationID
    assert_eq!(bits[3], PyRep::Integer(52678)); // stationTypeID

    // Negative: invalid station ID
    let err_bits = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStationItemBits",
        &[PyRep::Integer(69_999_999)],
    )
    .await
    .expect_err("GetStationItemBits with invalid station must fail");
    assert_eq!(err_bits.code, "StationNotFound");

    // 3. stationSvc.GetSolarSystem
    // Lookup system by station ID
    let sys_for_st = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetSolarSystem",
        &[PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect("stationSvc.GetSolarSystem by station ID");
    assert_eq!(sys_for_st, PyRep::Integer(SYS_JITA as i64));

    // Lookup system details by solarSystemID
    let sys_info = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetSolarSystem",
        &[PyRep::Integer(SYS_JITA as i64)],
    )
    .await
    .expect("stationSvc.GetSolarSystem by solarSystemID");
    let PyRep::KeyVal(sys_kv) = sys_info else {
        panic!("GetSolarSystem by sysID must return KeyVal");
    };
    assert_eq!(sys_kv.get("solarSystemID"), Some(&PyRep::Integer(SYS_JITA as i64)));
    assert_eq!(sys_kv.get("solarSystemName"), Some(&PyRep::String("Jita".into())));

    // Negative: non-existent station ID
    let err_sys = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetSolarSystem",
        &[PyRep::Integer(69_999_999)],
    )
    .await
    .expect_err("GetSolarSystem with unknown station ID must fail");
    assert_eq!(err_sys.code, "StationNotFound");

    // 4. stationSvc.GetStationsForOwner
    let owner_stations = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStationsForOwner",
        &[PyRep::Integer(CORP_CALDARI_NAVY)],
    )
    .await
    .expect("stationSvc.GetStationsForOwner");
    let PyRep::List(st_list) = owner_stations else {
        panic!("GetStationsForOwner must return List");
    };
    assert!(!st_list.is_empty(), "Caldari Navy must own stations in SDE");
    let PyRep::KeyVal(first_st) = &st_list[0] else {
        panic!("Station record must be KeyVal");
    };
    assert_eq!(
        first_st.get("corporationID"),
        Some(&PyRep::Integer(CORP_CALDARI_NAVY))
    );
    assert!(first_st.get("stationID").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    // Unknown owner returns empty list
    let empty_st = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetStationsForOwner",
        &[PyRep::Integer(99_999_999)],
    )
    .await
    .expect("GetStationsForOwner for empty owner");
    assert_eq!(empty_st, PyRep::List(vec![]));

    // 5. stationSvc.GetAllianceSystems & stationSvc.GetSystemsForAlliance
    let test_alliance_id = 99_000_123i64;
    ctx.db.with_mut_snapshot(|snap| {
        let entry = snap.system_states.entry(SYS_JITA).or_default();
        entry.solar_system_id = SYS_JITA;
        entry.sov_alliance_id = Some(test_alliance_id);
    });

    let ally_sys_1 = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetAllianceSystems",
        &[PyRep::Integer(test_alliance_id)],
    )
    .await
    .expect("stationSvc.GetAllianceSystems");
    assert_eq!(ally_sys_1, PyRep::List(vec![PyRep::Integer(SYS_JITA as i64)]));

    let ally_sys_2 = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetSystemsForAlliance",
        &[PyRep::Integer(test_alliance_id)],
    )
    .await
    .expect("stationSvc.GetSystemsForAlliance");
    assert_eq!(ally_sys_2, PyRep::List(vec![PyRep::Integer(SYS_JITA as i64)]));

    let unheld_sys = rpc(
        &ctx,
        &mut s1,
        "stationSvc",
        "GetAllianceSystems",
        &[PyRep::Integer(99_999_888)],
    )
    .await
    .expect("GetAllianceSystems for unheld alliance");
    assert_eq!(unheld_sys, PyRep::List(vec![]));

    // 6. corpStationMgr.DoStandingCheckForStationService
    // Station 60003760 offers fitting (65536) and cloning (512)
    let standing_ok = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(65536), PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect("DoStandingCheckForStationService default standings");
    assert_eq!(standing_ok, PyRep::Bool(true));

    // Negative: standing below minimum
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.standings.insert(CORP_CALDARI_NAVY, -5.0);
        }
    });
    let err_standing = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(65536), PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect_err("DoStandingCheckForStationService with low standing must fail");
    assert_eq!(err_standing.code, "StandingTooLowForStationService");

    // Negative: service unavailable at station
    let err_unavailable = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(999_999), PyRep::Integer(STATION_JITA)],
    )
    .await
    .expect_err("DoStandingCheckForStationService with unoffered service must fail");
    assert_eq!(err_unavailable.code, "StationServiceUnavailable");

    // Negative: station not found
    let err_nostation = rpc(
        &ctx,
        &mut s1,
        "corpStationMgr",
        "DoStandingCheckForStationService",
        &[PyRep::Integer(65536), PyRep::Integer(69_999_999)],
    )
    .await
    .expect_err("DoStandingCheckForStationService with non-existent station must fail");
    assert_eq!(err_nostation.code, "StationNotFound");
}

#[tokio::test]
async fn test_home_station_and_jump_clones_parity() {
    // parity: rpc:home_station.get_home_station, rpc:home_station.get_home_station_candidates, rpc:home_station.get_next_remote_change_time, rpc:home_station.reset_remote_change_time, rpc:home_station.self_destruct_clone, rpc:jumpCloneSvc.CancelShipCloneInstallation, rpc:jumpCloneSvc.GetNumClonesInPilotsStructure, rpc:jumpCloneSvc.SetJumpCloneName, rpc:jumpCloneSvc.ValidateInstallJumpClone
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", PILOT1).await;
    let mut s2 = setup_pilot(&ctx, 302, "other", "other-secret", PILOT2).await;

    // Reset PILOT1 home station to STATION_JITA and clean cooldowns
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.home_station_id = STATION_JITA;
            c1.station_id = Some(STATION_JITA);
            c1.balance = IskCents(100_000_000); // 1,000,000 ISK
        }
        snap.owner_notes.remove(&(-10_000_099 - PILOT1));
    });
    s1.station_id = Some(STATION_JITA);

    // 1. home_station.get_home_station
    let home_st = rpc(&ctx, &mut s1, "home_station", "get_home_station", &[])
        .await
        .expect("home_station.get_home_station");
    assert_eq!(home_st, PyRep::Integer(STATION_JITA));

    // 2. home_station.get_home_station_candidates
    let cands_rep = rpc(
        &ctx,
        &mut s1,
        "home_station",
        "get_home_station_candidates",
        &[],
    )
    .await
    .expect("home_station.get_home_station_candidates");
    let PyRep::List(cands) = cands_rep else {
        panic!("get_home_station_candidates must return List");
    };
    assert!(
        cands.contains(&PyRep::Integer(STATION_JITA)),
        "Candidate list must contain current home station"
    );

    // 3. home_station.get_next_remote_change_time & reset_remote_change_time
    let next_time_initial = rpc(
        &ctx,
        &mut s1,
        "home_station",
        "get_next_remote_change_time",
        &[],
    )
    .await
    .expect("home_station.get_next_remote_change_time");
    assert_eq!(next_time_initial, PyRep::Integer(0));

    // Seed a cooldown in owner_notes
    let future_cooldown = 1_800_000_000i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            -10_000_099 - PILOT1,
            OwnerNoteRecord {
                note_id: -10_000_099 - PILOT1,
                character_id: -PILOT1,
                item_id: STATION_JITA,
                label: "home_station_remote_cooldown".into(),
                note: future_cooldown.to_string(),
                timestamp_unix: future_cooldown as u64,
            },
        );
    });

    let next_time_active = rpc(
        &ctx,
        &mut s1,
        "home_station",
        "get_next_remote_change_time",
        &[],
    )
    .await
    .expect("get_next_remote_change_time active");
    assert_eq!(next_time_active, PyRep::Integer(future_cooldown));

    // Reset cooldown
    let reset_rep = rpc(
        &ctx,
        &mut s1,
        "home_station",
        "reset_remote_change_time",
        &[],
    )
    .await
    .expect("home_station.reset_remote_change_time");
    assert_eq!(reset_rep, PyRep::Integer(0));

    let next_time_cleared = rpc(
        &ctx,
        &mut s1,
        "home_station",
        "get_next_remote_change_time",
        &[],
    )
    .await
    .expect("get_next_remote_change_time cleared");
    assert_eq!(next_time_cleared, PyRep::Integer(0));

    // 4. home_station.self_destruct_clone
    ctx.drain_notifications_for_char(PILOT1);
    let self_destruct_rep = rpc(
        &ctx,
        &mut s1,
        "home_station",
        "self_destruct_clone",
        &[],
    )
    .await
    .expect("home_station.self_destruct_clone");
    assert_eq!(self_destruct_rep, PyRep::Integer(STATION_JITA));
    assert_eq!(s1.station_id, Some(STATION_JITA));

    let n_destruct = ctx.drain_notifications_for_char(PILOT1);
    let destruct_events = events_from(&n_destruct, "OnCloneDestruction");
    assert_eq!(destruct_events.len(), 1, "Must emit OnCloneDestruction");

    // 5. jumpCloneSvc.ValidateInstallJumpClone
    // Train Infomorph Psychology (type_id 24242) to level 1 and ensure wallet funds
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT1).or_default();
        skills.insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 8000.0,
            },
        );
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.balance = IskCents(100_000_000); // 1,000,000 ISK (fee is 900,000 ISK = 90,000,000 cents)
        }
        snap.jump_clones.remove(&PILOT1);
    });
    s1.station_id = Some(STATION_JITA);

    let validate_ok = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "ValidateInstallJumpClone",
        &[],
    )
    .await
    .expect("jumpCloneSvc.ValidateInstallJumpClone");
    assert_eq!(validate_ok, PyRep::Bool(true));

    // Negative: not docked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.station_id = None;
            c1.structure_id = None;
        }
    });
    s1.station_id = None;
    s1.structure_id = None;
    let err_undocked = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "ValidateInstallJumpClone",
        &[],
    )
    .await
    .expect_err("ValidateInstallJumpClone when undocked must fail");
    assert_eq!(err_undocked.code, "MustBeDocked");

    // Re-dock PILOT1
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.station_id = Some(STATION_JITA);
        }
    });
    s1.station_id = Some(STATION_JITA);

    // Negative: insufficient funds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.balance = IskCents(10_000_000); // 100,000 ISK < 900,000 ISK
        }
    });
    let err_funds = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "ValidateInstallJumpClone",
        &[],
    )
    .await
    .expect_err("ValidateInstallJumpClone with low ISK must fail");
    assert_eq!(err_funds.code, "NotEnoughMoney");

    // Negative: clone limit reached (0 skills)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.balance = IskCents(100_000_000);
        }
        if let Some(skills) = snap.skills.get_mut(&PILOT1) {
            skills.remove(&24242);
        }
    });
    let err_limit = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "ValidateInstallJumpClone",
        &[],
    )
    .await
    .expect_err("ValidateInstallJumpClone with 0 clone capacity must fail");
    assert_eq!(err_limit.code, "JumpCloneLimitReached");

    // 6. jumpCloneSvc.SetJumpCloneName
    let clone_id = 777_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.jump_clones.insert(
            PILOT1,
            vec![JumpCloneRecord {
                jump_clone_id: clone_id,
                character_id: PILOT1,
                location_id: STATION_JITA,
                implants: vec![],
            }],
        );
    });

    let set_name_rep = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "SetJumpCloneName",
        &[PyRep::Integer(clone_id), PyRep::String("Combat Vanguard".into())],
    )
    .await
    .expect("jumpCloneSvc.SetJumpCloneName");
    assert_eq!(set_name_rep, PyRep::None);

    let custom_name = ctx.db.with_snapshot(|snap| {
        eve_server::services::jump_clone_names::name_of(snap, clone_id)
    });
    assert_eq!(custom_name, "Combat Vanguard");

    // Clear name
    rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "SetJumpCloneName",
        &[PyRep::Integer(clone_id), PyRep::String("".into())],
    )
    .await
    .expect("Clear JumpCloneName");
    let cleared_name = ctx.db.with_snapshot(|snap| {
        eve_server::services::jump_clone_names::name_of(snap, clone_id)
    });
    assert_eq!(cleared_name, "");

    // Negative: clone not owned
    let err_clone_not_found = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "SetJumpCloneName",
        &[PyRep::Integer(999_999), PyRep::String("Invalid".into())],
    )
    .await
    .expect_err("SetJumpCloneName on unknown clone must fail");
    assert_eq!(err_clone_not_found.code, "JumpCloneNotFound");

    // Negative: name too long
    let long_name = "A".repeat(101);
    let err_long = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "SetJumpCloneName",
        &[PyRep::Integer(clone_id), PyRep::String(long_name)],
    )
    .await
    .expect_err("SetJumpCloneName over 100 chars must fail");
    assert_eq!(err_long.code, "JumpCloneNameTooLong");

    // 7. jumpCloneSvc.GetNumClonesInPilotsStructure
    let test_structure_id = 1_000_000_000_888i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            test_structure_id,
            UpwellStructureRecord {
                structure_id: test_structure_id,
                type_id: 35832, // Astrahus
                owner_corp_id: CORP_CALDARI_NAVY,
                solar_system_id: SYS_JITA,
                name: "Jita Freeport Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1000,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        // PILOT1 has 2 clones at structure, PILOT2 has 1 clone at structure
        snap.jump_clones.insert(
            PILOT1,
            vec![
                JumpCloneRecord {
                    jump_clone_id: 888_001,
                    character_id: PILOT1,
                    location_id: test_structure_id,
                    implants: vec![],
                },
                JumpCloneRecord {
                    jump_clone_id: 888_002,
                    character_id: PILOT1,
                    location_id: test_structure_id,
                    implants: vec![],
                },
            ],
        );
        snap.jump_clones.insert(
            PILOT2,
            vec![JumpCloneRecord {
                jump_clone_id: 888_003,
                character_id: PILOT2,
                location_id: test_structure_id,
                implants: vec![],
            }],
        );
    });

    let num_clones_p1 = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "GetNumClonesInPilotsStructure",
        &[PyRep::Integer(test_structure_id)],
    )
    .await
    .expect("jumpCloneSvc.GetNumClonesInPilotsStructure for PILOT1");
    assert_eq!(num_clones_p1, PyRep::Integer(2));

    let num_clones_p2 = rpc(
        &ctx,
        &mut s2,
        "jumpCloneSvc",
        "GetNumClonesInPilotsStructure",
        &[PyRep::Integer(test_structure_id)],
    )
    .await
    .expect("jumpCloneSvc.GetNumClonesInPilotsStructure for PILOT2");
    assert_eq!(num_clones_p2, PyRep::Integer(1));

    // Negative: missing structure ID when not docked
    s1.structure_id = None;
    let err_nostruct = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "GetNumClonesInPilotsStructure",
        &[],
    )
    .await
    .expect_err("GetNumClonesInPilotsStructure with no structure must fail");
    assert_eq!(err_nostruct.code, "MissingStructureId");

    // 8. jumpCloneSvc.CancelShipCloneInstallation
    let host_ship_id = 930_001i64;
    let cvb_mod_id = 930_002i64;
    seed_test_item(&ctx, host_ship_id, 28352, PILOT1, SYS_JITA as i64, InventoryFlag::None, 1);
    seed_test_item(&ctx, cvb_mod_id, 23735, PILOT1, host_ship_id, InventoryFlag::HiSlot0, 1);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cvb_mod_id, true);
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.active_ship_id = host_ship_id;
        }
    });
    s1.ship_id = Some(host_ship_id);
    s1.solar_system_id = Some(SYS_JITA);

    ctx.drain_notifications_for_char(PILOT1);
    ctx.drain_notifications_for_char(PILOT2);

    // Offer installation to PILOT2
    rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "OfferShipCloneInstallation",
        &[PyRep::Integer(PILOT2), PyRep::Integer(host_ship_id)],
    )
    .await
    .expect("OfferShipCloneInstallation");

    let n_offer = ctx.drain_notifications_for_char(PILOT2);
    let offer_events = events_from(&n_offer, "OnShipJumpCloneInstallationOffered");
    assert_eq!(offer_events.len(), 1, "PILOT2 must receive offer notification");
    assert_eq!(offer_events[0][0], PyRep::Integer(PILOT1));
    assert_eq!(offer_events[0][1], PyRep::Integer(PILOT2));
    assert_eq!(offer_events[0][2], PyRep::Integer(host_ship_id));

    // Cancel installation
    let cancel_rep = rpc(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "CancelShipCloneInstallation",
        &[],
    )
    .await
    .expect("jumpCloneSvc.CancelShipCloneInstallation");
    assert_eq!(cancel_rep, PyRep::Bool(true));

    let n_cancel_p1 = ctx.drain_notifications_for_char(PILOT1);
    let n_cancel_p2 = ctx.drain_notifications_for_char(PILOT2);
    assert_eq!(
        events_from(&n_cancel_p1, "OnShipJumpCloneInstallationCanceled").len(),
        1,
        "PILOT1 must receive cancel notification"
    );
    assert_eq!(
        events_from(&n_cancel_p2, "OnShipJumpCloneInstallationCanceled").len(),
        1,
        "PILOT2 must receive cancel notification"
    );
}
