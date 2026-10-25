//! Integration tests covering home_station and jumpCloneSvc RPC parity rows (Part 2).

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
async fn test_home_station_and_jump_clones_parity() {
    // parity: rpc:home_station.get_home_station, rpc:home_station.get_home_station_candidates, rpc:home_station.get_next_remote_change_time, rpc:home_station.reset_remote_change_time, rpc:home_station.self_destruct_clone, rpc:jumpCloneSvc.CancelShipCloneInstallation, rpc:jumpCloneSvc.GetNumClonesInPilotsStructure, rpc:jumpCloneSvc.SetJumpCloneName, rpc:jumpCloneSvc.ValidateInstallJumpClone
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = setup_pilot(&ctx, 301, "demo", "demo-secret", PILOT1).await;
    let mut s2 = setup_pilot(&ctx, 302, "other", "other-secret", PILOT2).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.home_station_id = STATION_JITA;
            c1.station_id = Some(STATION_JITA);
            c1.balance = IskCents(100_000_000);
        }
        snap.owner_notes.remove(&(-10_000_099 - PILOT1));
    });
    s1.station_id = Some(STATION_JITA);

    let home_st = rpc(&ctx, &mut s1, "home_station", "get_home_station", &[])
        .await
        .expect("home_station.get_home_station");
    assert_eq!(home_st, PyRep::Integer(STATION_JITA));

    let cands_rep = rpc(&ctx, &mut s1, "home_station", "get_home_station_candidates", &[])
        .await
        .expect("home_station.get_home_station_candidates");
    let PyRep::List(cands) = cands_rep else {
        panic!("get_home_station_candidates must return List");
    };
    assert!(cands.contains(&PyRep::Integer(STATION_JITA)));

    let next_time_initial = rpc(&ctx, &mut s1, "home_station", "get_next_remote_change_time", &[])
        .await
        .expect("home_station.get_next_remote_change_time");
    assert_eq!(next_time_initial, PyRep::Integer(0));

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

    let next_time_active = rpc(&ctx, &mut s1, "home_station", "get_next_remote_change_time", &[])
        .await
        .expect("get_next_remote_change_time active");
    assert_eq!(next_time_active, PyRep::Integer(future_cooldown));

    let reset_rep = rpc(&ctx, &mut s1, "home_station", "reset_remote_change_time", &[])
        .await
        .expect("home_station.reset_remote_change_time");
    assert_eq!(reset_rep, PyRep::Integer(0));

    let next_time_cleared = rpc(&ctx, &mut s1, "home_station", "get_next_remote_change_time", &[])
        .await
        .expect("get_next_remote_change_time cleared");
    assert_eq!(next_time_cleared, PyRep::Integer(0));

    ctx.drain_notifications_for_char(PILOT1);
    let in_ship_err = rpc(&ctx, &mut s1, "home_station", "self_destruct_clone", &[])
        .await
        .expect_err("self_destruct_clone in ship must fail");
    // rpc-lint: allow "SelfDestructOnlyInCapsule" is an error code, not a home_station method
    assert_eq!(in_ship_err.code, "SelfDestructOnlyInCapsule");

    let pod_id = ctx.db.try_with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let cid = snap.next_entity_id;
        snap.items.insert(cid, InventoryItemRecord {
            item_id: cid,
            type_id: 670,
            owner_id: PILOT1,
            location_id: STATION_JITA,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        });
        if let Some(ch) = snap.characters.get_mut(&PILOT1) {
            ch.active_ship_id = cid;
        }
        Ok::<i64, eve_db::DbError>(cid)
    }).expect("seed pod");
    s1.ship_id = Some(pod_id);

    let self_destruct_rep = rpc(&ctx, &mut s1, "home_station", "self_destruct_clone", &[])
        .await
        .expect("home_station.self_destruct_clone");
    assert_eq!(self_destruct_rep, PyRep::Integer(STATION_JITA));
    assert_eq!(s1.station_id, Some(STATION_JITA));

    let n_destruct = ctx.drain_notifications_for_char(PILOT1);
    assert_eq!(events_from(&n_destruct, "OnCloneDestruction").len(), 1);

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
            c1.balance = IskCents(100_000_000);
        }
        snap.jump_clones.remove(&PILOT1);
    });
    s1.station_id = Some(STATION_JITA);

    let validate_ok = rpc(&ctx, &mut s1, "jumpCloneSvc", "ValidateInstallJumpClone", &[])
        .await
        .expect("jumpCloneSvc.ValidateInstallJumpClone");
    assert_eq!(validate_ok, PyRep::Bool(true));

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.station_id = None;
            c1.structure_id = None;
        }
    });
    s1.station_id = None;
    s1.structure_id = None;
    let err_undocked = rpc(&ctx, &mut s1, "jumpCloneSvc", "ValidateInstallJumpClone", &[])
        .await
        .expect_err("ValidateInstallJumpClone when undocked must fail");
    assert_eq!(err_undocked.code, "MustBeDocked");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.station_id = Some(STATION_JITA);
            c1.balance = IskCents(10_000_000);
        }
    });
    s1.station_id = Some(STATION_JITA);
    let err_funds = rpc(&ctx, &mut s1, "jumpCloneSvc", "ValidateInstallJumpClone", &[])
        .await
        .expect_err("ValidateInstallJumpClone with low ISK must fail");
    assert_eq!(err_funds.code, "NotEnoughMoney");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.balance = IskCents(100_000_000);
        }
        if let Some(skills) = snap.skills.get_mut(&PILOT1) {
            skills.remove(&24242);
        }
    });
    let err_limit = rpc(&ctx, &mut s1, "jumpCloneSvc", "ValidateInstallJumpClone", &[])
        .await
        .expect_err("ValidateInstallJumpClone with 0 clone capacity must fail");
    assert_eq!(err_limit.code, "JumpCloneLimitReached");

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

    let test_structure_id = 1_000_000_000_888i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            test_structure_id,
            UpwellStructureRecord {
                structure_id: test_structure_id,
                type_id: 35832,
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
                last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
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

    assert_eq!(
        rpc(&ctx, &mut s1, "jumpCloneSvc", "GetNumClonesInPilotsStructure", &[PyRep::Integer(test_structure_id)]).await.unwrap(),
        PyRep::Integer(2)
    );
    assert_eq!(
        rpc(&ctx, &mut s2, "jumpCloneSvc", "GetNumClonesInPilotsStructure", &[PyRep::Integer(test_structure_id)]).await.unwrap(),
        PyRep::Integer(1)
    );

    s1.structure_id = None;
    assert_eq!(
        rpc(&ctx, &mut s1, "jumpCloneSvc", "GetNumClonesInPilotsStructure", &[]).await.unwrap_err().code,
        "MissingStructureId"
    );

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
    assert_eq!(offer_events.len(), 1);
    assert_eq!(offer_events[0][0], PyRep::Integer(PILOT1));
    assert_eq!(offer_events[0][1], PyRep::Integer(PILOT2));
    assert_eq!(offer_events[0][2], PyRep::Integer(host_ship_id));

    let cancel_rep = rpc(&ctx, &mut s1, "jumpCloneSvc", "CancelShipCloneInstallation", &[])
        .await
        .expect("jumpCloneSvc.CancelShipCloneInstallation");
    assert_eq!(cancel_rep, PyRep::Bool(true));

    assert_eq!(events_from(&ctx.drain_notifications_for_char(PILOT1), "OnShipJumpCloneInstallationCanceled").len(), 1);
    assert_eq!(events_from(&ctx.drain_notifications_for_char(PILOT2), "OnShipJumpCloneInstallationCanceled").len(), 1);
}
