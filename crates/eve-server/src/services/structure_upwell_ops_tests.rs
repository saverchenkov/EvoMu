//! Unit tests for `structure_upwell_ops` (unanchor/rename, reinforce schedule update, asset safety, structure descriptions, guests, hangar view, clone counts, and access helpers).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle::{
    self, STATE_ARMOR_REINFORCED, STATE_SHIELD_VULNERABLE,
};
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, UpwellStructureRecord};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
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

fn make_session(char_id: i64, corp_id: i64, structure_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(750);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30000142);
    s.structure_id = structure_id;
    s.role = role;
    s.corp_role = u64::MAX;
    s
}

fn seed_upwell_fixture(ctx: &ServerContext, char_id: i64, corp_id: i64, sid: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = format!("Upwell Corp {corp_id}");
            __corp.ticker = "UPW".into();
            __corp.ceo_id = char_id;
            __corp.hq_station_id = Some(60003760);
            __corp.tax_rate = 0.05;
            __corp.members = HashSet::from([char_id]);
            __corp.set_division_balance(1, IskCents(500_000_000));
            __corp };
        corp.ensure_wallet_divisions();
        corp.set_division_balance(1, IskCents(500_000_000));
        snap.corporations.insert(corp_id, corp);

        let mut ch = snap.characters.values().next().cloned().expect("demo char");
        ch.character_id = char_id;
        ch.name = format!("Manager {char_id}");
        ch.corporation_id = corp_id;
        ch.alliance_id = None;
        ch.solar_system_id = 30000142;
        ch.station_id = None;
        ch.structure_id = Some(sid);
        ch.active_ship_id = 88001;
        ch.home_station_id = sid;
        ch.balance = IskCents(500_000_000);
        snap.characters.insert(char_id, ch);

        snap.structures.insert(
            sid,
            UpwellStructureRecord {
                structure_id: sid,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30000142,
                name: "Initial Astrahus".into(),
                position: [1_000_000_000.0, 0.0, 0.0],
                state: STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 500_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
}

#[tokio::test(flavor = "current_thread")]
async fn unanchor_cancel_rename_description_and_reinforce_schedule_work() {
    let ctx = test_ctx();
    let sid = 10_000_920_001i64;
    seed_upwell_fixture(&ctx, 7501, 98750, sid);
    let mut sess = make_session(7501, 98750, Some(sid), 1);
    let kw = HashMap::new();

    // Unanchor sets timer & evicts home station back to NPC station
    let unanchor_res = handle(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Unanchor",
        &[PyRep::Integer(sid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("unanchor");
    assert!(matches!(unanchor_res, PyRep::KeyVal(_)));
    ctx.db.with_snapshot(|snap| {
        assert_ne!(snap.characters[&7501].home_station_id, sid);
        assert!(load_ext_state(snap).unanchoring_until_unix.contains_key(&sid));
    });

    // Duplicate unanchor rejected while timer active
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureDeployment",
            "Unanchor",
            &[PyRep::Integer(sid)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "StructureAlreadyUnanchoring"
    );

    // Cancel unanchor
    let cancel_res = handle(
        &ctx,
        &mut sess,
        "structureDeployment",
        "CancelUnanchor",
        &[PyRep::Integer(sid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("cancel unanchor");
    assert!(matches!(cancel_res, PyRep::KeyVal(_)));

    // Rename & set/get description
    let _ = handle(
        &ctx,
        &mut sess,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(sid), PyRep::String("Fortress Prime".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("rename");
    let _ = handle(
        &ctx,
        &mut sess,
        "structureDirectory",
        "SetStructureDescription",
        &[PyRep::Integer(sid), PyRep::String("Public Market Hub".into())],
        &kw,
    )
    .await
    .unwrap()
    .expect("set desc");
    let desc = handle(
        &ctx,
        &mut sess,
        "structureDirectory",
        "GetStructureDescription",
        &[PyRep::Integer(sid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("get desc");
    assert_eq!(desc.as_str(), Some("Public Market Hub"));

    // Invalid reinforce schedule rejected; valid queues pending update & applies after delay
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureVulnerability",
            "UpdateReinforceTimesFromClient",
            &[PyRep::Integer(sid), PyRep::Integer(25), PyRep::Integer(6)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidReinforceSchedule"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&sid).unwrap().state = STATE_ARMOR_REINFORCED.into();
        snap.structures.get_mut(&sid).unwrap().state = STATE_SHIELD_VULNERABLE.into();
        let prof = structure_lifecycle::structure_profile(&ctx, 35832).unwrap();
        let st = snap.structures.get_mut(&sid).unwrap();
        st.shield_hp = prof.shield_hp;
        st.armor_hp = prof.armor_hp;
    });
    let queue_res = handle(
        &ctx,
        &mut sess,
        "structureVulnerability",
        "UpdateReinforceTimesFromClient",
        &[PyRep::Integer(sid), PyRep::Integer(20), PyRep::Integer(6)],
        &kw,
    )
    .await
    .unwrap()
    .expect("queue reinforce update");
    assert!(matches!(queue_res, PyRep::KeyVal(_)));
    let delay = reinforce_change_delay_secs(&ctx, 30000142);
    let future_now = ctx.now_unix() + delay + 10;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        apply_pending_reinforce_schedule(&ctx, snap, &mut ext, sid, future_now);
        assert!(!ext.pending_reinforce_schedule.contains_key(&sid));
    });
}

#[tokio::test(flavor = "current_thread")]
async fn asset_safety_manual_move_gm_eject_delivery_and_auto_delivery_work() {
    let ctx = test_ctx();
    let sid1 = 10_000_925_001i64;
    let sid2 = 10_000_925_002i64;
    seed_upwell_fixture(&ctx, 7601, 98760, sid1);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st2 = snap.structures[&sid1].clone();
        st2.structure_id = sid2;
        st2.name = "Destination Citadel".into();
        snap.structures.insert(sid2, st2);
        snap.items.insert(
            89101,
            InventoryItemRecord {
                item_id: 89101,
                type_id: 4051,
                owner_id: 7601,
                location_id: sid1,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            89102,
            InventoryItemRecord {
                item_id: 89102,
                type_id: 4051,
                owner_id: 98760,
                location_id: sid1,
                flag: InventoryFlag::CorpSAG1,
                quantity: 50,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Undocked session so manual personal asset safety move is allowed
    let mut sess = make_session(7601, 98760, None, service_roles::ROLE_GMH);
    let kw = HashMap::new();

    // Move personal assets & corp assets to safety
    let wrap_personal_rep = handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "MovePersonalAssetsToSafety",
        &[PyRep::Integer(30000142), PyRep::Integer(sid1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("move personal");
    let wrap_personal = match &wrap_personal_rep {
        PyRep::KeyVal(kv) => kv.get("wrapID").and_then(|v| v.as_i64()).expect("wrapID"),
        _ => panic!("expected KeyVal"),
    };

    let _wrap_corp = handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "MoveCorpAssetsToSafety",
        &[PyRep::Integer(30000142), PyRep::Integer(sid1)],
        &kw,
    )
    .await
    .unwrap()
    .expect("move corp");

    // Query wraps for character, corp, wrap names, and candidate structures
    let char_wraps = handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "GetItemsInSafetyForCharacter",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("char wraps");
    assert!(matches!(char_wraps, PyRep::List(ref l) if !l.is_empty()));

    assert!(handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "GetItemsInSafetyForCorp",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert!(handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "GetWrapNames",
        &[PyRep::List(vec![PyRep::Integer(wrap_personal)])],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert!(handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "GetStructuresICanDeliverTo",
        &[PyRep::Integer(30000142)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // Early delivery rejected; GM MoveEjectTimeGM accelerates timer so MoveSafetyWrapToStructure succeeds
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "structureAssetSafety",
            "MoveSafetyWrapToStructure",
            &[PyRep::Integer(wrap_personal), PyRep::Integer(sid2)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AssetSafetyCooldownActive"
    );

    let _ = handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "MoveEjectTimeGM",
        &[PyRep::Integer(wrap_personal), PyRep::Integer(ctx.now_unix() as i64)],
        &kw,
    )
    .await
    .unwrap()
    .expect("gm move eject");

    let deliver_res = handle(
        &ctx,
        &mut sess,
        "structureAssetSafety",
        "MoveSafetyWrapToStructure",
        &[PyRep::Integer(wrap_personal), PyRep::Integer(sid2)],
        &kw,
    )
    .await
    .unwrap()
    .expect("deliver wrap");
    assert!(matches!(deliver_res, PyRep::KeyVal(_)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&89101].location_id), sid2);

    // Backdate remaining corp wrap past 20d to trigger auto-delivery
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_ext_state(snap);
        for w in ext.asset_safety_wraps.values_mut() {
            if w.delivered_to_id.is_none() {
                w.created_unix = 0;
            }
        }
        save_ext_state(snap, &ext);
    });
    sync_auto_delivered_asset_safety_wraps(&ctx);
    ctx.db.with_snapshot(|s| {
        let ext = load_ext_state(s);
        assert!(ext.asset_safety_wraps.values().any(|w| w.auto_delivered));
    });
}

#[tokio::test(flavor = "current_thread")]
async fn guests_hangar_view_clones_and_structure_access_profile_helpers_work() {
    let ctx = test_ctx();
    let sid = 10_000_928_001i64;
    seed_upwell_fixture(&ctx, 7701, 98770, sid);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch2 = snap.characters[&7701].clone();
        ch2.character_id = 7702;
        ch2.corporation_id = 98999;
        snap.characters.insert(7702, ch2);
        snap.corporations.insert(
            98999,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = 98999;
                __corp.name = "Outsider Corp".into();
                __corp.ticker = "OUT".into();
                __corp },
        );
    });
    let mut sess = make_session(7701, 98770, Some(sid), 1);
    let kw = HashMap::new();

    // Guests & HangarView & NumClones
    let guests = handle(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(sid)], &kw)
        .await
        .unwrap()
        .expect("get guests");
    let PyRep::Dict(guest_entries) = &guests else {
        panic!("expected Dict from structureGuests.GetGuests, got {guests:?}");
    };
    assert_eq!(guest_entries.len(), 2);
    assert_eq!(guest_entries[0].0, PyRep::Integer(7701));
    assert_eq!(
        guest_entries[0].1,
        PyRep::Tuple(vec![PyRep::Integer(98770), PyRep::None, PyRep::None])
    );
    assert_eq!(guest_entries[1].0, PyRep::Integer(7702));
    assert_eq!(
        guest_entries[1].1,
        PyRep::Tuple(vec![PyRep::Integer(98999), PyRep::None, PyRep::None])
    );

    let mut undocked_sess = make_session(7702, 98999, None, 2);
    let empty_none = handle(&ctx, &mut undocked_sess, "structureGuests", "GetGuests", &[PyRep::None], &kw)
        .await
        .unwrap()
        .expect("get guests with None structureID");
    assert_eq!(empty_none, PyRep::Dict(Vec::new()));

    let hangar = handle(
        &ctx,
        &mut sess,
        "structureHangarViewMgr",
        "GetMyHangarViewState",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("hangar view");
    assert!(matches!(hangar, PyRep::KeyVal(_)));

    let clones = handle(
        &ctx,
        &mut sess,
        "jumpCloneSvc",
        "GetNumClonesInPilotsStructure",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("num clones");
    assert!(matches!(clones, PyRep::Integer(_)));

    // Configure & modify access profile nodes + evaluate character access + credit corp div1
    configure_structure_access_profile(
        &ctx,
        &sess,
        sid,
        Some(true),
        &[],
        &[],
        &[98999],
        &[98770],
    )
    .expect("configure access profile");
    modify_structure_access_node(&ctx, &sess, sid, 7702, false, true).expect("modify access node");

    ctx.db.with_snapshot(|snap| {
        assert!(evaluate_character_structure_access(&ctx, snap, sid, 7701));
        assert!(!evaluate_character_structure_access(&ctx, snap, sid, 7799));
    });

    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        credit_structure_owner_corp_div1(
            snap,
            98770,
            IskCents(25_000_000),
            85,
            "StructureServiceFee",
            7701,
            "Test service fee".into(),
            now_u,
        );
    });
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.corporations[&98770].master_wallet_balance(), IskCents(525_000_000));
    });
}

#[tokio::test]
async fn test_hangar_view_state_covers_v24_upkeep_damage_and_timer_fields() {
    let ctx = test_ctx();
    let now: u64 = 1_700_000_000;
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(now)));

    let corp_id = 98_780_i64;
    let char_id = 95_000_780_i64;
    let st_id = 1_000_990_001_i64;
    let ship_id = 1_900_990_001_i64;
    let mod_id = 1_900_990_002_i64;
    let kw = HashMap::new();

    ctx.db
        .provision_gm_account("hangar_1069", "secret", char_id, "Hangar Tester");
    let uid = ctx.db.with_snapshot(|snap| snap.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock_with_computer_hash(uid, 106_901, Some(char_id), None, true, now)
        .expect("online lock");

    let profile = structure_lifecycle::structure_profile(&ctx, 35_834).expect("Keepstar profile");
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord::new(corp_id, "Hangar Test Corp", "HTC", char_id),
        );
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_834, // Keepstar
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Test Keepstar".into(),
                position: [10_000.0, 0.0, 0.0],
                state: STATE_SHIELD_VULNERABLE.into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: profile.shield_hp * 0.5,
                armor_hp: profile.armor_hp * 0.25,
                hull_hp: profile.hull_hp * 0.75,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 606, // Velator
                owner_id: char_id,
                location_id: st_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState {
        user_id: Some(uid),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        structure_id: Some(st_id),
        station_id: Some(st_id),
        ship_id: Some(ship_id),
        role: service_roles::ROLE_PLAYER,
        ..Default::default()
    };

    // 1. LowPower Keepstar (no online service module) with fractional damage and idle timers.
    let low_power = handle(&ctx, &mut sess, "structureHangarViewMgr", "GetMyHangarViewState", &[], &kw)
        .await
        .unwrap()
        .expect("hangar view");
    let PyRep::KeyVal(kv) = &low_power else {
        panic!("expected KeyVal, got {low_power:?}");
    };
    assert_eq!(kv["characterID"].as_i64(), Some(char_id));
    assert_eq!(kv["structureID"].as_i64(), Some(st_id));
    assert_eq!(kv["structureTypeID"].as_i64(), Some(35_834));
    assert_eq!(kv["shipID"].as_i64(), Some(ship_id));
    assert_eq!(kv["shipTypeID"].as_i64(), Some(606));
    assert_eq!(kv["upkeepState"].as_i64(), Some(2));
    assert_eq!(kv["operatingState"].as_i64(), Some(110));
    assert_eq!(kv["state"].as_i64(), Some(110));
    let PyRep::Tuple(dmg) = &kv["damageState"] else {
        panic!("damageState must be a 3-tuple");
    };
    assert_eq!(dmg.len(), 3);
    assert!((dmg[0].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!((dmg[1].as_f64().unwrap() - 0.25).abs() < 1e-9);
    assert!((dmg[2].as_f64().unwrap() - 0.75).abs() < 1e-9);
    assert_eq!(kv["timerProgress"], PyRep::None);
    assert_eq!(kv["timerIsProgressing"].as_bool(), Some(false));
    assert_eq!(kv["timerIsPaused"].as_bool(), Some(false));
    assert_eq!(kv["timerEndAt"], PyRep::None);

    // 2. FullPower Keepstar (online Standup Cloning Center I service module) + shared dock notification parity.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 35_894,
                owner_id: corp_id,
                location_id: st_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
    });
    let full_power = handle(&ctx, &mut sess, "structureHangarViewMgr", "GetMyHangarViewState", &[], &kw)
        .await
        .unwrap()
        .expect("hangar view");
    let PyRep::KeyVal(kv_full) = &full_power else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv_full["upkeepState"].as_i64(), Some(1));

    let _ = ctx.drain_notifications_for_char(char_id);
    emit_structure_dock_notifications(&ctx, st_id, char_id, corp_id, None, None, 35_834, ship_id);
    let notifs = ctx.drain_notifications_for_char(char_id);
    let hv_notif = notifs
        .into_iter()
        .find(|n| n.event == "OnHangarViewStateUpdated")
        .expect("OnHangarViewStateUpdated emitted");
    assert_eq!(hv_notif.args.len(), 2);
    assert_eq!(hv_notif.args[0].as_i64(), Some(st_id));
    assert_eq!(hv_notif.args[1], full_power);

    // 3. Abandoned Keepstar after 7+ days in low power.
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.remove(&mod_id);
        snap.items.remove(&mod_id);
        let mut ext = load_ext_state(snap);
        ext.lifecycle
            .entry(st_id)
            .or_default()
            .low_power_since_unix = Some(now - (structure_lifecycle::ABANDON_AFTER_LOW_POWER_SECS + 60));
        save_ext_state(snap, &ext);
    });
    let abandoned = build_hangar_view_state(&ctx, char_id, st_id, Some(ship_id), None);
    let PyRep::KeyVal(kv_aband) = &abandoned else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv_aband["upkeepState"].as_i64(), Some(3));

    // 4. ArmorReinforced with active reinforcement timer.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.state = STATE_ARMOR_REINFORCED.into();
            st.reinforced_until_unix = now + 3_600;
        }
        let mut ext = load_ext_state(snap);
        let entry = ext.lifecycle.entry(st_id).or_default();
        entry.low_power_since_unix = None;
        entry.armor_reinforce_secs_applied = Some(7_200);
        save_ext_state(snap, &ext);
    });
    let reinforced = build_hangar_view_state(&ctx, char_id, st_id, Some(ship_id), None);
    let PyRep::KeyVal(kv_rf) = &reinforced else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv_rf["operatingState"].as_i64(), Some(111));
    assert_eq!(kv_rf["state"].as_i64(), Some(111));
    assert_eq!(kv_rf["upkeepState"].as_i64(), Some(2));
    assert!((kv_rf["timerProgress"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(kv_rf["timerIsProgressing"].as_bool(), Some(true));
    assert_eq!(kv_rf["timerIsPaused"].as_bool(), Some(false));
    assert_eq!(
        kv_rf["timerEndAt"].as_i64(),
        Some(crate::sp6_lifecycle::unix_to_filetime(now + 3_600))
    );

    // 5. Active armor repair timer.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&st_id) {
            st.state = structure_lifecycle::STATE_ARMOR_VULNERABLE.into();
            st.reinforced_until_unix = 0;
        }
        let mut ext = load_ext_state(snap);
        ext.lifecycle.entry(st_id).or_default().repair_until_unix =
            Some(now + (structure_lifecycle::REPAIR_SHIELD_ARMOR_SECS / 2));
        save_ext_state(snap, &ext);
    });
    let repairing = build_hangar_view_state(&ctx, char_id, st_id, Some(ship_id), None);
    let PyRep::KeyVal(kv_rep) = &repairing else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv_rep["operatingState"].as_i64(), Some(112));
    assert!((kv_rep["timerProgress"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(kv_rep["timerIsProgressing"].as_bool(), Some(true));
    assert_eq!(kv_rep["timerIsPaused"].as_bool(), Some(false));
    assert_eq!(
        kv_rep["timerEndAt"].as_i64(),
        Some(crate::sp6_lifecycle::unix_to_filetime(
            now + (structure_lifecycle::REPAIR_SHIELD_ARMOR_SECS / 2)
        ))
    );

    // 6. Synthetic fallback when structure is absent from snapshot.
    let fallback = build_hangar_view_state(&ctx, char_id, 9_999_999_999, None, Some(35_834));
    let PyRep::KeyVal(kv_fb) = &fallback else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv_fb["structureTypeID"].as_i64(), Some(35_834));
    assert_eq!(kv_fb["shipID"], PyRep::None);
    assert_eq!(kv_fb["shipTypeID"], PyRep::None);
    assert_eq!(kv_fb["upkeepState"].as_i64(), Some(2));
    assert_eq!(kv_fb["operatingState"].as_i64(), Some(110));
    assert_eq!(kv_fb["timerIsProgressing"].as_bool(), Some(false));
    assert_eq!(kv_fb["timerIsPaused"].as_bool(), Some(false));
}

#[tokio::test]
async fn get_items_in_safety_for_corp_enforces_accountant_or_director() {
    let ctx = test_ctx();
    let corp_id = 98_000_851;
    let ceo_id = 90_000_851;
    let dir_id = 90_000_852;
    let acct_id = 90_000_853;
    let cfg_id = 90_000_854;
    let plain_id = 90_000_855;
    let unseeded_corp_id = 98_000_852;
    let unseeded_plain_id = 90_000_856;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "SafetyRbacCorp".into();
        corp.ticker = "SRBC".into();
        corp.ceo_id = ceo_id;
        corp.members.extend([ceo_id, dir_id, acct_id, cfg_id, plain_id]);
        corp.member_roles.insert(dir_id, eve_db::corp_roles::ROLE_DIRECTOR);
        corp.member_roles
            .insert(acct_id, eve_db::corp_roles::ROLE_ACCOUNTANT);
        corp.member_roles
            .insert(cfg_id, eve_db::corp_roles::ROLE_CONFIG_EQUIPMENT);
        snap.corporations.insert(corp_id, corp);

        for (cid, coid) in [
            (ceo_id, corp_id),
            (dir_id, corp_id),
            (acct_id, corp_id),
            (cfg_id, corp_id),
            (plain_id, corp_id),
            (unseeded_plain_id, unseeded_corp_id),
        ] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    corporation_id: coid,
                    ..Default::default()
                },
            );
        }
        let mut ext = load_ext_state(snap);
        ext.asset_safety_wraps.insert(
            7_900_851,
            AssetSafetyWrapRecord {
                wrap_id: 7_900_851,
                owner_id: corp_id,
                is_corp: true,
                source_structure_id: 1_090_000_851,
                source_structure_name: "Test Fortizar".into(),
                solar_system_id: 30_000_142,
                created_unix: 1_000,
                deliverable_after_unix: 2_000,
                nearest_npc_station_id: Some(60_003_760),
                delivered_to_id: None,
                auto_delivered: false,
                retrieval_fee_paid: false,
                item_ids: vec![1],
                estimated_value_isk: 1_000.0,
            },
        );
        save_ext_state(snap, &ext);
    });

    let kw = HashMap::new();
    for (cid, label) in [(ceo_id, "ceo"), (dir_id, "director"), (acct_id, "accountant")] {
        let mut sess = make_session(cid, corp_id, None, service_roles::ROLE_PLAYER);
        sess.corp_role = 0;
        let PyRep::List(rows) = handle(
            &ctx,
            &mut sess,
            "structureAssetSafety",
            "GetItemsInSafetyForCorp",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("{label} must be permitted: {e:?}")) else {
            panic!("expected List from GetItemsInSafetyForCorp");
        };
        assert_eq!(rows.len(), 1, "{label} should see corporate safety wrap");
    }

    for (cid, label) in [(plain_id, "roleless"), (cfg_id, "config_equipment_only")] {
        let mut sess = make_session(cid, corp_id, None, service_roles::ROLE_PLAYER);
        sess.corp_role = 0;
        let err = handle(
            &ctx,
            &mut sess,
            "structureAssetSafety",
            "GetItemsInSafetyForCorp",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err.code, "CorpRoleDenied", "unexpected error for {label}");
    }

    // Player corp >= 98M without snap.corporations record still enforces session.corp_role.
    let mut unseeded_denied = make_session(unseeded_plain_id, unseeded_corp_id, None, service_roles::ROLE_PLAYER);
    unseeded_denied.corp_role = 0;
    assert_eq!(
        handle(
            &ctx,
            &mut unseeded_denied,
            "structureAssetSafety",
            "GetItemsInSafetyForCorp",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
}

#[tokio::test]
async fn get_items_in_safety_for_corp_rejects_npc_corp_and_omitted_members() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let corp_id = 98_000_859_i64;
    let ceo_id = 90_000_859_i64;
    let omitted_id = 90_000_860_i64;
    let npc_char_id = 90_000_861_i64;
    let npc_corp_id = 1_000_009_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::default();
        corp.corporation_id = corp_id;
        corp.name = "Safety Member Guard Corp".into();
        corp.ticker = "SMGC".into();
        corp.ceo_id = ceo_id;
        corp.members.insert(ceo_id);
        corp.member_roles
            .insert(omitted_id, eve_db::corp_roles::ROLE_ACCOUNTANT);
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);

        snap.characters.insert(
            omitted_id,
            eve_db::CharacterRecord {
                character_id: omitted_id,
                corporation_id: corp_id,
                ..Default::default()
            },
        );
        snap.characters.insert(
            npc_char_id,
            eve_db::CharacterRecord {
                character_id: npc_char_id,
                corporation_id: npc_corp_id,
                ..Default::default()
            },
        );
    });

    // Non-GM pilot in an NPC corp or unseeded < 98M corp is denied even with ROLE_ACCOUNTANT.
    for cid in [npc_corp_id, 500_001_i64] {
        let mut sess = make_session(90_000_862, cid, None, service_roles::ROLE_PLAYER);
        sess.corp_role = u64::MAX;
        assert_eq!(
            handle(
                &ctx,
                &mut sess,
                "structureAssetSafety",
                "GetItemsInSafetyForCorp",
                &[],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "CorpRoleDenied"
        );
    }

    // Pilot omitted from non-empty corp.members is denied even with ROLE_ACCOUNTANT in session & DB.
    let mut omitted_sess = make_session(omitted_id, corp_id, None, service_roles::ROLE_PLAYER);
    omitted_sess.corp_role = eve_db::corp_roles::ROLE_ACCOUNTANT;
    assert_eq!(
        handle(
            &ctx,
            &mut omitted_sess,
            "structureAssetSafety",
            "GetItemsInSafetyForCorp",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Stale session.corporation_id pointing to a player corp cannot bypass DB NPC corp membership.
    let mut stale_sess = make_session(npc_char_id, corp_id, None, service_roles::ROLE_PLAYER);
    stale_sess.corp_role = u64::MAX;
    assert_eq!(
        handle(
            &ctx,
            &mut stale_sess,
            "structureAssetSafety",
            "GetItemsInSafetyForCorp",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
}
