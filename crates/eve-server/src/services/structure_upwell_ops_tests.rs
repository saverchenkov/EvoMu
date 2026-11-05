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
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: format!("Upwell Corp {corp_id}"),
            ticker: "UPW".into(),
            ceo_id: char_id,
            hq_station_id: Some(60003760),
            tax_rate: 0.05,
            master_wallet: IskCents(500_000_000),
            members: HashSet::from([char_id]),
            ..Default::default()
        };
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
            CorporationRecord {
                corporation_id: 98999,
                name: "Outsider Corp".into(),
                ticker: "OUT".into(),
                ..Default::default()
            },
        );
    });
    let mut sess = make_session(7701, 98770, Some(sid), 1);
    let kw = HashMap::new();

    // Guests & HangarView & NumClones
    let guests = handle(&ctx, &mut sess, "structureGuests", "GetGuests", &[PyRep::Integer(sid)], &kw)
        .await
        .unwrap()
        .expect("get guests");
    assert!(matches!(guests, PyRep::List(_)));

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
        assert_eq!(s.corporations[&98770].master_wallet, IskCents(525_000_000));
    });
}
