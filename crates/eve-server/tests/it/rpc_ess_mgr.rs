//! Integration tests covering open RPC parity rows for Encounter Surveillance System (`essMgr`).

use crate::common;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
use glam::DVec3;
use std::collections::HashMap;

const NULLSEC_SYS_ID: u32 = 30_004_759; // VFK-IV (K-space null-sec)
const CHAR_ID: i64 = 90_000_001;
const SHIP_ID: i64 = 940_001;
const ESS_BALL_ID: i64 = 940_010;
const RIFTER_TYPE_ID: u32 = 587;
const ESS_BEACON_TYPE_ID: u32 = 32882;
const KEY_15M_TYPE_ID: u32 = 57714;
const KEY_45M_TYPE_ID: u32 = 57715;

async fn setup_ess_environment(
    ctx: &ServerContext,
    main_bank_isk: f64,
    reserve_bank_isk: f64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(3101);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(CHAR_ID)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    sess.solar_system_id = Some(NULLSEC_SYS_ID);
    sess.ship_id = Some(SHIP_ID);
    sess.station_id = None;
    sess.structure_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = NULLSEC_SYS_ID;
            ch.active_ship_id = SHIP_ID;
        }

        snap.system_states.insert(
            NULLSEC_SYS_ID,
            eve_db::SystemDynamicState {
                solar_system_id: NULLSEC_SYS_ID,
                dbs_multiplier: 1.35,
                ess_main_bank: eve_db::IskCents::from_f64_isk(main_bank_isk),
                ess_reserve_bank: eve_db::IskCents::from_f64_isk(reserve_bank_isk),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );

        snap.items.insert(
            SHIP_ID,
            eve_db::InventoryItemRecord {
                item_id: SHIP_ID,
                type_id: RIFTER_TYPE_ID,
                owner_id: CHAR_ID,
                location_id: NULLSEC_SYS_ID as i64,
                flag: eve_db::InventoryFlag::None,
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
    });

    let sys_handle = ctx.systems.get_or_spawn(NULLSEC_SYS_ID);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: SHIP_ID,
                type_id: RIFTER_TYPE_ID,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 35.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(RIFTER_TYPE_ID),
        )
        .await
        .unwrap();

    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ESS_BALL_ID,
                type_id: ESS_BEACON_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ESS_BEACON_TYPE_ID),
        )
        .await
        .unwrap();

    sess
}

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

#[tokio::test]
async fn test_ess_mgr_telemetry_links_and_thefts() {
    // parity: rpc:essMgr.AttemptLinkToReserveBank
    // parity: rpc:essMgr.GetDataForClientSolarSystem
    // parity: rpc:essMgr.GetMainBankTheftsForClientSolarSystem
    // parity: rpc:essMgr.GetReserveBankTheftsForClientSolarSystem
    // parity: rpc:essMgr.IsClientLinkedToReserveBank
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_ess_environment(&ctx, 15_000_000.0, 35_000_000.0).await;
    let kw = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(NULLSEC_SYS_ID);

    // 1. GetDataForClientSolarSystem: returns telemetry dictionary
    let data_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "GetDataForClientSolarSystem",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetDataForClientSolarSystem");
    let data_kv = kv_map(&data_rep);
    assert_eq!(
        data_kv.get("mainValue"),
        Some(&PyRep::Real(15_000_000.0))
    );
    assert_eq!(
        data_kv.get("reserveValue"),
        Some(&PyRep::Real(35_000_000.0))
    );
    assert_eq!(
        data_kv.get("essID"),
        Some(&PyRep::Integer(820_000_000 + NULLSEC_SYS_ID as i64))
    );

    // 2. IsClientLinkedToReserveBank: initially false
    let linked_before = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "IsClientLinkedToReserveBank",
            &[],
            &kw,
            None,
        )
        .await
        .expect("IsClientLinkedToReserveBank before link");
    assert_eq!(linked_before, PyRep::Bool(false));

    // 3. AttemptLinkToReserveBank: requires ESS Reserve Security Key in cargo
    let key_item_id = 950_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            key_item_id,
            eve_db::InventoryItemRecord {
                item_id: key_item_id,
                type_id: KEY_15M_TYPE_ID,
                owner_id: CHAR_ID,
                location_id: SHIP_ID,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let res_start = kv_map(
        &ctx.dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToReserveBank start"),
    );
    assert_eq!(res_start.get("linkStartedOnly"), Some(&PyRep::Bool(true)));
    assert_eq!(res_start.get("linked"), Some(&PyRep::Bool(true)));

    // Verify key was consumed on link start
    let key_exists = ctx.db.with_snapshot(|s| s.items.contains_key(&key_item_id));
    assert!(!key_exists, "Reserve security key must be consumed on link start");

    // IsClientLinkedToReserveBank should now be true
    let linked_during = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "IsClientLinkedToReserveBank",
            &[],
            &kw,
            None,
        )
        .await
        .expect("IsClientLinkedToReserveBank during link");
    assert_eq!(linked_during, PyRep::Bool(true));

    // Step 5 Destiny ticks to satisfy the robbery timer
    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await.unwrap();
    }

    // Complete Reserve Bank robbery
    let res_complete = kv_map(
        &ctx.dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToReserveBank complete"),
    );
    assert_eq!(
        res_complete.get("stolenISK"),
        Some(&PyRep::Real(35_000_000.0))
    );
    assert_eq!(res_complete.get("linkStartedOnly"), Some(&PyRep::Bool(false)));

    // IsClientLinkedToReserveBank should now be false
    let linked_after = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "IsClientLinkedToReserveBank",
            &[],
            &kw,
            None,
        )
        .await
        .expect("IsClientLinkedToReserveBank after robbery");
    assert_eq!(linked_after, PyRep::Bool(false));

    // 4. GetReserveBankTheftsForClientSolarSystem: verify recorded theft
    let reserve_thefts = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "GetReserveBankTheftsForClientSolarSystem",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetReserveBankTheftsForClientSolarSystem");
    let PyRep::List(res_rows) = reserve_thefts else {
        panic!("Expected PyList from GetReserveBankTheftsForClientSolarSystem");
    };
    assert_eq!(res_rows.len(), 1, "Should have 1 recorded reserve bank theft");
    let res_entry = kv_map(&res_rows[0]);
    assert_eq!(res_entry.get("characterID"), Some(&PyRep::Integer(CHAR_ID)));
    assert_eq!(res_entry.get("stolenISK"), Some(&PyRep::Real(35_000_000.0)));

    // 5. Complete Main Bank robbery to test GetMainBankTheftsForClientSolarSystem
    let main_start = kv_map(
        &ctx.dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToMainBank start"),
    );
    assert_eq!(main_start.get("linkStartedOnly"), Some(&PyRep::Bool(true)));

    for _ in 0..5 {
        let _ = sys_handle.step_manual(1.0).await.unwrap();
    }

    let main_complete = kv_map(
        &ctx.dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToMainBank complete"),
    );
    assert_eq!(
        main_complete.get("stolenISK"),
        Some(&PyRep::Real(15_000_000.0))
    );

    let main_thefts = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "GetMainBankTheftsForClientSolarSystem",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMainBankTheftsForClientSolarSystem");
    let PyRep::List(main_rows) = main_thefts else {
        panic!("Expected PyList from GetMainBankTheftsForClientSolarSystem");
    };
    assert_eq!(main_rows.len(), 1, "Should have 1 recorded main bank theft");
    let main_entry = kv_map(&main_rows[0]);
    assert_eq!(main_entry.get("characterID"), Some(&PyRep::Integer(CHAR_ID)));
    assert_eq!(main_entry.get("stolenISK"), Some(&PyRep::Real(15_000_000.0)));
}

#[tokio::test]
async fn test_ess_mgr_unlinks_unlock_and_cache() {
    // parity: rpc:essMgr.RequestMainBankUnlink
    // parity: rpc:essMgr.RequestReserveBankUnlink
    // parity: rpc:essMgr.RequestUnlockReserveBank
    // parity: rpc:essMgr.clear_cache
    // parity: rpc:essMgr.prime_cache_result
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = setup_ess_environment(&ctx, 20_000_000.0, 50_000_000.0).await;
    let kw = HashMap::new();

    // 1. clear_cache & prime_cache_result
    let clear_rep = ctx
        .dispatch_rpc(&mut sess, "essMgr", None, "clear_cache", &[], &kw, None)
        .await
        .expect("clear_cache");
    assert_eq!(clear_rep, PyRep::None);

    let prime_rep = ctx
        .dispatch_rpc(&mut sess, "essMgr", None, "prime_cache_result", &[], &kw, None)
        .await
        .expect("prime_cache_result");
    let prime_kv = kv_map(&prime_rep);
    assert_eq!(
        prime_kv.get("mainValue"),
        Some(&PyRep::Real(20_000_000.0))
    );
    assert_eq!(
        prime_kv.get("reserveValue"),
        Some(&PyRep::Real(50_000_000.0))
    );

    // 2. RequestMainBankUnlink: link to Main Bank then cleanly unlink
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToMainBank start");

    let is_linked_in_snap = ctx.db.with_snapshot(|s| {
        s.system_states
            .get(&NULLSEC_SYS_ID)
            .map(|st| st.ess_linked_character_id == Some(CHAR_ID))
            .unwrap_or(false)
    });
    assert!(is_linked_in_snap, "Pilot must be linked in system state");

    let unlink_main_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "RequestMainBankUnlink",
            &[],
            &kw,
            None,
        )
        .await
        .expect("RequestMainBankUnlink");
    assert_eq!(unlink_main_rep, PyRep::Bool(true));

    let still_linked = ctx.db.with_snapshot(|s| {
        s.system_states
            .get(&NULLSEC_SYS_ID)
            .and_then(|st| st.ess_linked_character_id)
    });
    assert_eq!(still_linked, None, "Pilot must be unlinked after RequestMainBankUnlink");

    // 3. RequestReserveBankUnlink: link to Reserve Bank with key then cleanly unlink
    let key_item_id = 950_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            key_item_id,
            eve_db::InventoryItemRecord {
                item_id: key_item_id,
                type_id: KEY_15M_TYPE_ID,
                owner_id: CHAR_ID,
                location_id: SHIP_ID,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("AttemptLinkToReserveBank start");

    let unlink_res_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "RequestReserveBankUnlink",
            &[],
            &kw,
            None,
        )
        .await
        .expect("RequestReserveBankUnlink");
    assert_eq!(unlink_res_rep, PyRep::Bool(true));

    let still_linked_res = ctx.db.with_snapshot(|s| {
        s.system_states
            .get(&NULLSEC_SYS_ID)
            .and_then(|st| st.ess_linked_character_id)
    });
    assert_eq!(still_linked_res, None, "Pilot must be unlinked after RequestReserveBankUnlink");

    // 4. RequestUnlockReserveBank: consumes a 45-minute ESS Reserve Security Key (57715)
    let key45_item_id = 950_003;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            key45_item_id,
            eve_db::InventoryItemRecord {
                item_id: key45_item_id,
                type_id: KEY_45M_TYPE_ID,
                owner_id: CHAR_ID,
                location_id: SHIP_ID,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let unlock_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "RequestUnlockReserveBank",
            &[PyRep::Integer(key45_item_id)],
            &kw,
            None,
        )
        .await
        .expect("RequestUnlockReserveBank with 45m key");
    let unlock_kv = kv_map(&unlock_rep);
    assert_eq!(unlock_kv.get("unlocked"), Some(&PyRep::Bool(true)));
    assert_eq!(
        unlock_kv.get("durationMinutes"),
        Some(&PyRep::Integer(45))
    );
    assert_eq!(
        unlock_kv.get("keyTypeID"),
        Some(&PyRep::Integer(KEY_45M_TYPE_ID as i64))
    );
    assert_eq!(
        unlock_kv.get("solarSystemID"),
        Some(&PyRep::Integer(NULLSEC_SYS_ID as i64))
    );

    // Verify key was consumed
    let key45_exists = ctx.db.with_snapshot(|s| s.items.contains_key(&key45_item_id));
    assert!(!key45_exists, "45-minute key must be consumed on reserve bank unlock");

    // Verify unlock timestamp in runtime
    let unlocked_until = with_combat_runtime(&ctx, |rt| {
        rt.ess_reserve_unlocked_until.get(&NULLSEC_SYS_ID).copied()
    });
    assert!(
        unlocked_until.is_some_and(|ts| ts >= ctx.now_unix() + 45 * 60),
        "Reserve bank must be marked unlocked for 45 minutes"
    );

    // Error path: RequestUnlockReserveBank without key returns MissingEssReserveSecurityKey
    let no_key_err = ctx
        .dispatch_rpc(
            &mut sess,
            "essMgr",
            None,
            "RequestUnlockReserveBank",
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(no_key_err.code, "MissingEssReserveSecurityKey");
}
