//! Skills, Clones, Jump Clones, Home Station, Wallet, Market, Contracts, Industry, Bookmarks,
//! Standings, Agents, and Station notification emission tests (Increment 6).

use crate::common;

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const STATION_JITA: i64 = 60_003_760;
const STATION_AMARR: i64 = 60_008_494;
const SYS_JITA: u32 = 30_000_142;

async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    call(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    s
}

async fn call(ctx: &ServerContext, s: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn events_from(notifs: &[PendingMachoNotification], event: &str) -> Vec<Vec<PyRep>> {
    notifs
        .iter()
        .filter(|n| n.event == event)
        .map(|n| n.args.clone())
        .collect()
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
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
                singleton,
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
async fn test_skills_implants_boosters_respec_clones_and_insurance_notifications() {
    // parity: notify:OnServerSkillsChanged, notify:OnServerSkillsRemoved, notify:OnFreeSkillPointsChanged, notify:OnNewSkillQueueSaved, notify:OnSkillQueuePausedServer, notify:OnRespecInfoChanged, notify:OnServerImplantsChanged, notify:OnServerBoostersChanged, notify:OnCloneDestruction, notify:OnStationJumpCloneCacheInvalidated, notify:OnJumpCloneCacheInvalidated, notify:OnJumpCloneTransitionCompleted, notify:OnHomeStationChanged, notify:OnShipInsured
    eve_testkit::attest(&[
        "notify:OnServerSkillsChanged",
        "notify:OnServerSkillsRemoved",
        "notify:OnFreeSkillPointsChanged",
        "notify:OnNewSkillQueueSaved",
        "notify:OnSkillQueuePausedServer",
        "notify:OnRespecInfoChanged",
        "notify:OnServerImplantsChanged",
        "notify:OnServerBoostersChanged",
        "notify:OnCloneDestruction",
        "notify:OnStationJumpCloneCacheInvalidated",
        "notify:OnJumpCloneCacheInvalidated",
        "notify:OnJumpCloneTransitionCompleted",
        "notify:OnHomeStationChanged",
        "notify:OnShipInsured",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 7601, "demo", "demo-secret", P1).await;
    let _s2 = select(&ctx, 7602, "other", "other-secret", P2).await;
    for c in [P1, P2] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. InjectSkillIntoBrain + SaveSkillQueue + AbortTraining + ApplyFreeSkillPoints + ExtractSkills
    let skillbook_id = 910_001i64;
    let skill_tid = 3300u32; // Gunnery
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&P1) {
            sk.remove(&skill_tid);
        }
    });
    seed_item(&ctx, skillbook_id, skill_tid, P1, STATION_JITA, InventoryFlag::Hangar, 1, false);
    call(
        &ctx,
        &mut s1,
        "skillMgr2",
        "InjectSkillIntoBrain",
        &[PyRep::Integer(skillbook_id)],
    )
    .await;
    let n1 = ctx.drain_notifications_for_char(P1);
    let sc = events_from(&n1, "OnServerSkillsChanged");
    assert_eq!(sc.len(), 1);
    let PyRep::Dict(skill_dict) = &sc[0][0] else {
        panic!("expected skillInfos dict, got {:?}", sc[0]);
    };
    assert_eq!(skill_dict.len(), 1);
    assert_eq!(skill_dict[0].0, PyRep::Integer(skill_tid as i64));
    assert_eq!(skill_dict[0].1.dict_get("trainedSkillLevel"), Some(&PyRep::Integer(0)));
    assert_eq!(skill_dict[0].1.dict_get("trainedSkillPoints"), Some(&PyRep::Real(0.0)));
    assert!(events_from(&n1, "OnSkillsChanged").is_empty(), "client-local OnSkillsChanged must not be emitted");
    assert!(ctx.drain_notifications_for_char(P2).is_empty());

    // SaveSkillQueue -> OnNewSkillQueueSaved(newQueue)
    call(
        &ctx,
        &mut s1,
        "skillMgr2",
        "SaveSkillQueue",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(skill_tid as i64),
            PyRep::Integer(1),
        ])])],
    )
    .await;
    let n_sq = ctx.drain_notifications_for_char(P1);
    let sq_saved = events_from(&n_sq, "OnNewSkillQueueSaved");
    assert_eq!(sq_saved.len(), 1);
    let PyRep::Rowset { rows: sq_rows, .. } = &sq_saved[0][0] else {
        panic!("expected Rowset in OnNewSkillQueueSaved");
    };
    assert_eq!(sq_rows.len(), 1);
    assert_eq!(sq_rows[0][0], PyRep::Integer(skill_tid as i64));
    assert_eq!(sq_rows[0][1], PyRep::Integer(1));
    assert!(events_from(&n_sq, "OnSkillQueueChanged").is_empty());

    // AbortTraining -> OnSkillQueuePausedServer()
    call(&ctx, &mut s1, "skillMgr2", "AbortTraining", &[]).await;
    let n_pause = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_pause, "OnSkillQueuePausedServer"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_pause, "OnSkillQueuePaused").is_empty());

    // ApplyFreeSkillPoints (1-arg applies to queue -> OnFreeSkillPointsChanged; 2-arg applies to skill -> OnServerSkillsChanged)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.free_skill_points = 600_250.0;
        }
    });
    call(&ctx, &mut s1, "skillMgr2", "ApplyFreeSkillPoints", &[PyRep::Integer(250)]).await;
    let n_fsp = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_fsp, "OnFreeSkillPointsChanged"),
        vec![vec![PyRep::Real(600_000.0)]]
    );
    call(
        &ctx,
        &mut s1,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(skill_tid as i64), PyRep::Integer(550_000)],
    )
    .await;
    let n_apply = ctx.drain_notifications_for_char(P1);
    let sc_apply = events_from(&n_apply, "OnServerSkillsChanged");
    assert_eq!(sc_apply.len(), 1);
    let PyRep::Dict(apply_dict) = &sc_apply[0][0] else {
        panic!("expected skillInfos dict");
    };
    assert_eq!(apply_dict[0].0, PyRep::Integer(skill_tid as i64));
    assert_eq!(apply_dict[0].1.dict_get("trainedSkillPoints"), Some(&PyRep::Real(256_000.0)));

    // ExtractSkills -> OnServerSkillsRemoved(skillInfos, timeStamp)
    let extractor_id = 910_002i64;
    seed_item(&ctx, extractor_id, 40519, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    let skill_tid2 = 3301u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.skill_points = 5_600_000.0;
        }
        let sk = snap.skills.entry(P1).or_default();
        sk.insert(skill_tid2, eve_db::TrainedSkillRecord {
            type_id: skill_tid2,
            level: 5,
            skill_points: 256_000.0,
        });
    });
    let extract_map = vec![
        (PyRep::Integer(skill_tid as i64), PyRep::Integer(250_000)),
        (PyRep::Integer(skill_tid2 as i64), PyRep::Integer(250_000)),
    ];

    // EVOMU-P0-01: verify missing/invalid extractor, wrong owner, wrong type, and insufficient SP are rejected
    let err_missing = ctx.dispatch_rpc(&mut s1, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(extract_map.clone()), PyRep::None], &HashMap::new(), None).await;
    assert_eq!(err_missing.unwrap_err().code, "MissingSkillExtractor");

    let stolen_extractor = 910_003i64;
    seed_item(&ctx, stolen_extractor, 40519, P2, STATION_JITA, InventoryFlag::Hangar, 1, false);
    let err_owner = ctx.dispatch_rpc(&mut s1, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(extract_map.clone()), PyRep::Integer(stolen_extractor)], &HashMap::new(), None).await;
    assert_eq!(err_owner.unwrap_err().code, "MissingSkillExtractor");

    let wrong_type = 910_004i64;
    seed_item(&ctx, wrong_type, 3300, P1, STATION_JITA, InventoryFlag::Hangar, 1, false);
    let err_type = ctx.dispatch_rpc(&mut s1, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(extract_map.clone()), PyRep::Integer(wrong_type)], &HashMap::new(), None).await;
    assert_eq!(err_type.unwrap_err().code, "MissingSkillExtractor");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.skill_points = 5_200_000.0;
        }
    });
    let err_sp = ctx.dispatch_rpc(&mut s1, "skillMgr2", None, "ExtractSkills", &[PyRep::Dict(extract_map.clone()), PyRep::Integer(extractor_id)], &HashMap::new(), None).await;
    assert_eq!(err_sp.unwrap_err().code, "InsufficientSkillPoints");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.skill_points = 5_600_000.0;
        }
    });

    call(
        &ctx,
        &mut s1,
        "skillMgr2",
        "ExtractSkills",
        &[PyRep::Dict(extract_map), PyRep::Integer(extractor_id)],
    )
    .await;
    let n_ext = ctx.drain_notifications_for_char(P1);
    let rem_ev = events_from(&n_ext, "OnServerSkillsRemoved");
    assert_eq!(rem_ev.len(), 1);
    let PyRep::Dict(rem_dict) = &rem_ev[0][0] else {
        panic!("expected skillInfos dict in OnServerSkillsRemoved");
    };
    assert_eq!(rem_dict[0].0, PyRep::Integer(skill_tid as i64));
    assert_eq!(rem_dict[0].1.dict_get("typeID"), Some(&PyRep::Integer(skill_tid as i64)));
    assert!(ctx.drain_notifications_for_char(P2).is_empty());
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&extractor_id), "extractor must be consumed from hangar");
        let has_injector = snap.items.values().any(|it| it.owner_id == P1 && it.type_id == 40520);
        assert!(has_injector, "Large Skill Injector (type 40520) must be minted");
    });

    // 2. RespecCharacter -> OnRespecInfoChanged()
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "RespecCharacter",
        &[
            PyRep::Integer(20),
            PyRep::Integer(20),
            PyRep::Integer(20),
            PyRep::Integer(20),
            PyRep::Integer(19),
        ],
    )
    .await;
    let n_respec = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_respec, "OnRespecInfoChanged"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_respec, "OnRespecInfoUpdated").is_empty());

    // 3. InjectImplant -> OnServerImplantsChanged() & UseBooster -> OnServerBoostersChanged()
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(P1).or_default();
        sk.insert(
            3411, // Cybernetics V
            TrainedSkillRecord {
                type_id: 3411,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            3405, // Biology V
            TrainedSkillRecord {
                type_id: 3405,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let implant_id = 910_003i64;
    seed_item(&ctx, implant_id, 9899, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    call(&ctx, &mut s1, "dogmaIM", "InjectImplant", &[PyRep::Integer(implant_id)]).await;
    let n_imp = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_imp, "OnServerImplantsChanged"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_imp, "OnImplantsChanged").is_empty());

    let booster_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 303 && t.default_attributes.get(&330).copied().unwrap_or(0.0) > 0.0 && !t.default_attributes.contains_key(&182))
        .map(|t| t.type_id)
        .expect("SDE combat booster type");
    let booster_id = 910_004i64;
    seed_item(&ctx, booster_id, booster_tid, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    call(&ctx, &mut s1, "dogmaIM", "UseBooster", &[PyRep::Integer(booster_id)]).await;
    let n_bst = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_bst, "OnServerBoostersChanged"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_bst, "OnBoosterUpdated").is_empty());

    // 4. InstallCloneInStation + CloneJump + DestroyInstalledClone
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.balance = IskCents::from_f64_isk(50_000_000.0);
        }
        snap.skills.entry(P1).or_default().insert(
            24242, // Infomorph Psychology
            TrainedSkillRecord {
                type_id: 24242,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let jc_rep = call(&ctx, &mut s1, "jumpCloneSvc", "InstallCloneInStation", &[]).await;
    let jc_id = jc_rep.dict_get("jumpCloneID").and_then(|v| v.as_i64()).expect("jumpCloneID");
    let n_jc_inst = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_jc_inst, "OnStationJumpCloneCacheInvalidated"),
        vec![vec![PyRep::Integer(STATION_JITA), PyRep::Integer(P1)]]
    );
    assert_eq!(events_from(&n_jc_inst, "OnJumpCloneCacheInvalidated"), vec![Vec::<PyRep>::new()]);
    assert!(events_from(&n_jc_inst, "OnCloneJumpUpdate").is_empty());

    call(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "CloneJump",
        &[PyRep::Integer(jc_id)],
    )
    .await;
    let n_cj = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_cj, "OnJumpCloneTransitionCompleted"), vec![Vec::<PyRep>::new()]);
    assert_eq!(events_from(&n_cj, "OnJumpCloneCacheInvalidated"), vec![Vec::<PyRep>::new()]);

    let remaining_jc = ctx.db.with_snapshot(|snap| {
        snap.jump_clones
            .get(&P1)
            .and_then(|v| v.first().map(|c| c.jump_clone_id))
            .unwrap_or(jc_id)
    });
    call(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "DestroyInstalledClone",
        &[PyRep::Integer(remaining_jc)],
    )
    .await;
    let n_jc_del = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_jc_del, "OnStationJumpCloneCacheInvalidated"),
        vec![vec![PyRep::Integer(STATION_JITA), PyRep::Integer(P1)]]
    );
    assert_eq!(events_from(&n_jc_del, "OnJumpCloneCacheInvalidated"), vec![Vec::<PyRep>::new()]);

    // 5. Home station change & self_destruct_clone -> OnHomeStationChanged & OnCloneDestruction
    call(
        &ctx,
        &mut s1,
        "home_station",
        "set_home_station",
        &[PyRep::Integer(STATION_JITA)],
    )
    .await;
    let n_hs = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_hs, "OnHomeStationChanged"),
        vec![vec![PyRep::Integer(STATION_JITA)]]
    );
    call(&ctx, &mut s1, "home_station", "self_destruct_clone", &[]).await;
    let n_sd = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_sd, "OnCloneDestruction"), vec![Vec::<PyRep>::new()]);

    // 6. InsureShip -> OnShipInsured()
    call(
        &ctx,
        &mut s1,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(100_000_001), PyRep::Real(1.0), PyRep::Bool(false)],
    )
    .await;
    let n_ins = ctx.drain_notifications_for_char(P1);
    assert_eq!(events_from(&n_ins, "OnShipInsured"), vec![Vec::<PyRep>::new()]);
    assert!(ctx.drain_notifications_for_char(P2).is_empty());
}

#[tokio::test]
async fn test_ship_clone_vat_bookmarks_standings_agents_and_item_locking_notifications() {
    // parity: notify:OnShipJumpCloneInstallationOffered, notify:OnShipJumpCloneInstallationCanceled, notify:OnShipJumpCloneInstallationDone, notify:OnShipJumpCloneCacheInvalidated, notify:OnSharedBookmarksFolderUpdated, notify:OnSharedBookmarksFolderDeleted, notify:OnSharedBookmarksFoldersAccessChanged, notify:OnSharedBookmarksFoldersAccessLost, notify:OnStandingSet, notify:OnStandingsModified, notify:OnAgentAdded, notify:OnLockedItemChange
    eve_testkit::attest(&[
        "notify:OnShipJumpCloneInstallationOffered",
        "notify:OnShipJumpCloneInstallationCanceled",
        "notify:OnShipJumpCloneInstallationDone",
        "notify:OnShipJumpCloneCacheInvalidated",
        "notify:OnSharedBookmarksFolderUpdated",
        "notify:OnSharedBookmarksFolderDeleted",
        "notify:OnSharedBookmarksFoldersAccessChanged",
        "notify:OnSharedBookmarksFoldersAccessLost",
        "notify:OnStandingSet",
        "notify:OnStandingsModified",
        "notify:OnAgentAdded",
        "notify:OnLockedItemChange",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 7611, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 7612, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("bystander6", "bystander6-secret");
    let p3 = ctx
        .db
        .create_character(uid3, "Bystander Six", 1, 1, 11, 17, &ctx.sde)
        .expect("create p3")
        .character_id;
    let _s3 = select(&ctx, 7613, "bystander6", "bystander6-secret", p3).await;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.balance = IskCents::from_f64_isk(500_000_000.0);
        }
        snap.skills.entry(P2).or_default().insert(
            24242,
            TrainedSkillRecord {
                type_id: 24242,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    // Create corporation via real RPCs so P1 (CEO) and P2 (member) share a corp
    let corp_rep = call(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Increment Six Corp".into()), PyRep::String("INC6".into())],
    )
    .await;
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    let app_rep = call(
        &ctx,
        &mut s2,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("Joining".into())],
    )
    .await;
    let app_id = app_rep.dict_get("applicationID").and_then(|v| v.as_i64()).expect("app_id");
    call(&ctx, &mut s1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id)]).await;
    s2.corporation_id = Some(corp_id);
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. Ship Clone Vat Bay: OfferShipCloneInstallation, CancelShipCloneInstallation, AcceptShipCloneInstallation
    let rorq_id = 920_001i64;
    let cvb_id = 920_002i64;
    seed_item(&ctx, rorq_id, 28352, P1, SYS_JITA as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, cvb_id, 23735, P1, rorq_id, InventoryFlag::HiSlot0, 1, true);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(cvb_id, true);
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.active_ship_id = rorq_id;
        }
    });
    s1.ship_id = Some(rorq_id);
    s1.solar_system_id = Some(SYS_JITA);

    call(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "OfferShipCloneInstallation",
        &[PyRep::Integer(P2)],
    )
    .await;
    let n_offer_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_offer_p2, "OnShipJumpCloneInstallationOffered"),
        vec![vec![
            PyRep::Integer(P1),
            PyRep::Integer(P2),
            PyRep::Integer(rorq_id),
            PyRep::Bool(true),
        ]]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());

    call(&ctx, &mut s1, "jumpCloneSvc", "CancelShipCloneInstallation", &[]).await;
    let n_cancel_p1 = ctx.drain_notifications_for_char(P1);
    let n_cancel_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_cancel_p1, "OnShipJumpCloneInstallationCanceled"),
        vec![Vec::<PyRep>::new()]
    );
    assert_eq!(
        events_from(&n_cancel_p2, "OnShipJumpCloneInstallationCanceled"),
        vec![Vec::<PyRep>::new()]
    );

    call(
        &ctx,
        &mut s1,
        "jumpCloneSvc",
        "OfferShipCloneInstallation",
        &[PyRep::Integer(P2)],
    )
    .await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s2, "jumpCloneSvc", "AcceptShipCloneInstallation", &[]).await;
    let n_acc_p1 = ctx.drain_notifications_for_char(P1);
    let n_acc_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_acc_p1, "OnShipJumpCloneInstallationDone"),
        vec![vec![
            PyRep::Integer(P1),
            PyRep::Integer(P2),
            PyRep::Integer(rorq_id),
            PyRep::Bool(true),
        ]]
    );
    assert_eq!(
        events_from(&n_acc_p1, "OnShipJumpCloneCacheInvalidated"),
        vec![vec![PyRep::Integer(rorq_id), PyRep::Integer(P2)]]
    );
    assert_eq!(
        events_from(&n_acc_p2, "OnShipJumpCloneInstallationDone"),
        vec![vec![
            PyRep::Integer(P1),
            PyRep::Integer(P2),
            PyRep::Integer(rorq_id),
            PyRep::Bool(true),
        ]]
    );
    assert_eq!(
        events_from(&n_acc_p2, "OnJumpCloneCacheInvalidated"),
        vec![Vec::<PyRep>::new()]
    );
    assert!(events_from(&n_acc_p1, "OnShipCloneJumpUpdate").is_empty());
    assert!(ctx.drain_notifications_for_char(p3).is_empty());

    // 2. Shared Bookmarks: AddFolder, AddToKnownFolders, RemoveFromKnownFolders, DeleteFolder
    let folder_rep = call(
        &ctx,
        &mut s1,
        "accessGroupBookmarkMgr",
        "AddFolder",
        &[PyRep::String("Shared Ops".into()), PyRep::Bool(true)],
    )
    .await;
    let folder_id = folder_rep.dict_get("folderID").and_then(|v| v.as_i64()).expect("folderID");
    let n_f_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_f_p2, "OnSharedBookmarksFolderUpdated"),
        vec![vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Integer(folder_id)]),
            PyRep::String("FolderChanged".into()),
            PyRep::Integer(corp_id),
        ])])]]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());
    ctx.drain_notifications_for_char(P1);

    call(
        &ctx,
        &mut s2,
        "accessGroupBookmarkMgr",
        "AddToKnownFolders",
        &[PyRep::Integer(folder_id), PyRep::Bool(true)],
    )
    .await;
    let n_sub_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_sub_p2, "OnSharedBookmarksFoldersAccessChanged"),
        vec![vec![PyRep::Dict(vec![(
            PyRep::Integer(folder_id),
            PyRep::Integer(1),
        )])]]
    );
    ctx.drain_notifications_for_char(P1);

    call(
        &ctx,
        &mut s2,
        "accessGroupBookmarkMgr",
        "RemoveFromKnownFolders",
        &[PyRep::Integer(folder_id)],
    )
    .await;
    let n_unsub_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_unsub_p2, "OnSharedBookmarksFoldersAccessLost"),
        vec![vec![PyRep::List(vec![PyRep::Integer(folder_id)])]]
    );

    call(
        &ctx,
        &mut s1,
        "accessGroupBookmarkMgr",
        "DeleteFolder",
        &[PyRep::Integer(folder_id)],
    )
    .await;
    let n_del_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_del_p2, "OnSharedBookmarksFolderDeleted"),
        vec![vec![PyRep::Integer(folder_id)]]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());
    ctx.drain_notifications_for_char(P1);

    // 3. Standings & Agents: AddContact for NPC Agent -> OnAgentAdded, Plus mission completion -> OnStandingSet & OnStandingsModified
    let (dist_agent_id, dist_corp_id, dist_station_id, dist_sys_id) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .map(|a| {
            (
                a.agent_id,
                a.corporation_id,
                a.station_id.unwrap_or(STATION_JITA),
                a.solar_system_id.unwrap_or(SYS_JITA),
            )
        })
        .expect("SDE distribution agent");
    call(
        &ctx,
        &mut s1,
        "charMgr",
        "AddContact",
        &[
            PyRep::Integer(dist_agent_id),
            PyRep::Real(5.0),
            PyRep::Bool(false),
            PyRep::Bool(false),
            PyRep::String(String::new()),
        ],
    )
    .await;
    let n_ag = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_ag, "OnAgentAdded"),
        vec![vec![PyRep::Integer(dist_agent_id)]]
    );

    // Colocate P1 with dist_agent_id and complete a courier mission -> OnStandingSet & OnStandingsModified
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = dist_sys_id;
            ch.station_id = Some(dist_station_id);
            ch.standings.insert(dist_corp_id, 2.0);
        }
    });
    s1.solar_system_id = Some(dist_sys_id);
    s1.station_id = Some(dist_station_id);

    let m_offer = call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)],
    )
    .await;
    let courier_mid = m_offer.dict_get("missionID").and_then(|v| v.as_i64()).expect("missionID");
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(courier_mid)],
    )
    .await;
    seed_item(&ctx, 920_050, 3683, P1, dist_station_id, InventoryFlag::Hangar, 100, false);
    ctx.drain_notifications_for_char(P1);
    call(
        &ctx,
        &mut s1,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(courier_mid)],
    )
    .await;
    let n_msn = ctx.drain_notifications_for_char(P1);
    let st_set = events_from(&n_msn, "OnStandingSet");
    assert!(!st_set.is_empty(), "CompleteMission must emit OnStandingSet");
    assert!(
        st_set
            .iter()
            .any(|s| s[0] == PyRep::Integer(dist_corp_id) && s[1] == PyRep::Integer(P1)),
        "OnStandingSet must include corp standing update: {st_set:?}"
    );
    let st_mod = events_from(&n_msn, "OnStandingsModified");
    assert_eq!(st_mod.len(), 1);
    assert!(matches!(&st_mod[0][0], PyRep::List(l) if !l.is_empty()));
    assert!(events_from(&n_msn, "OnNPCStandingChange").is_empty());

    // 4. Item Locking: ALSCLockItems -> OnLockedItemChange(itemID, ownerID, locationID)
    let lock_item_id = 920_099i64;
    let container_id = 920_098i64;
    seed_item(&ctx, container_id, 17366, corp_id, STATION_JITA, InventoryFlag::Hangar, 1, true);
    seed_item(&ctx, lock_item_id, 34, corp_id, container_id, InventoryFlag::Locked, 100, false);
    call(
        &ctx,
        &mut s1,
        "invbroker",
        "ALSCLockItems",
        &[
            PyRep::Integer(container_id),
            PyRep::List(vec![PyRep::Integer(lock_item_id)]),
        ],
    )
    .await;
    let n_lock = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_lock, "OnLockedItemChange"),
        vec![vec![
            PyRep::Integer(lock_item_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(container_id),
        ]]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());
}

#[tokio::test]
async fn test_wallet_bills_lp_trade_market_and_industry_notifications() {
    // parity: notify:OnAccountChange, notify:OnBillReceived, notify:OnOfficeRentalChange, notify:OnLPChange, notify:OnCorpLPChange, notify:OnTradeInitiate, notify:OnTradeOfferReset, notify:OnTradeMoneyOffer, notify:OnTradeOffer, notify:OnTradeComplete, notify:OnTradeCancel, notify:OnOwnOrdersChanged, notify:OnMarketItemReceived, notify:OnIndustryJob
    eve_testkit::attest(&[
        "notify:OnAccountChange",
        "notify:OnBillReceived",
        "notify:OnOfficeRentalChange",
        "notify:OnLPChange",
        "notify:OnCorpLPChange",
        "notify:OnTradeInitiate",
        "notify:OnTradeOfferReset",
        "notify:OnTradeMoneyOffer",
        "notify:OnTradeOffer",
        "notify:OnTradeComplete",
        "notify:OnTradeCancel",
        "notify:OnOwnOrdersChanged",
        "notify:OnMarketItemReceived",
        "notify:OnIndustryJob",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 7621, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 7622, "other", "other-secret", P2).await;

    let npc_corp = 1_000_035i64;
    let target_npc_corp = 1_000_125i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.balance = IskCents::from_f64_isk(500_000_000.0);
            c1.loyalty_points.insert(npc_corp, 5_000);
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.balance = IskCents::from_f64_isk(100_000_000.0);
        }
    });

    let corp_rep = call(
        &ctx,
        &mut s1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Wallet Industry Corp".into()), PyRep::String("WINC".into())],
    )
    .await;
    let corp_id = corp_rep.dict_get("corporationID").and_then(|v| v.as_i64()).expect("corp_id");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.master_wallet = IskCents::from_f64_isk(500_000_000.0);
            for div in &mut corp.wallet_divisions {
                div.balance = IskCents::from_f64_isk(500_000_000.0);
            }
        }
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.balance = IskCents::from_f64_isk(100_000_000.0);
        }
    });
    for c in [P1, P2] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. GiveCash -> OnAccountChange("cash", ownerID, newBalance)
    call(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[PyRep::Integer(P2), PyRep::Real(1_000.0), PyRep::String("Tip".into())],
    )
    .await;
    let n_cash1 = ctx.drain_notifications_for_char(P1);
    let n_cash2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_cash1, "OnAccountChange"),
        vec![vec![
            PyRep::String("cash".into()),
            PyRep::Integer(P1),
            PyRep::Real(99_999_000.0),
        ]]
    );
    assert_eq!(
        events_from(&n_cash2, "OnAccountChange"),
        vec![vec![
            PyRep::String("cash".into()),
            PyRep::Integer(P2),
            PyRep::Real(100_001_000.0),
        ]]
    );
    assert!(events_from(&n_cash1, "OnPersonalAccountChangedClient").is_empty());

    // 2. RentOffice -> OnBillReceived(bill) & OnOfficeRentalChange(corporationID, officeID)
    let rent_rep = call(&ctx, &mut s1, "officeManager", "RentOffice", &[PyRep::Integer(STATION_JITA)]).await;
    let office_id = rent_rep.dict_get("officeFolderID").and_then(|v| v.as_i64()).expect("officeFolderID");
    let n_rent = ctx.drain_notifications_for_char(P1);
    let bills = events_from(&n_rent, "OnBillReceived");
    assert_eq!(bills.len(), 1);
    assert_eq!(bills[0][0].dict_get("debtorID"), Some(&PyRep::Integer(corp_id)));
    assert_eq!(
        events_from(&n_rent, "OnOfficeRentalChange"),
        vec![vec![PyRep::Integer(corp_id), PyRep::Integer(office_id)]]
    );
    assert!(events_from(&n_rent, "OnOfficeRentalChanged").is_empty());
    assert!(ctx.drain_notifications_for_char(P2).is_empty());

    // 3. LP transfers: TransferLPFromMyWalletToOtherCorp -> OnLPChange(corpID, newLP) & TransferLPFromMyCorpWalletToOtherCorp -> OnCorpLPChange(corpID, newLP)
    call(
        &ctx,
        &mut s1,
        "LPSvc",
        "TransferLPFromMyWalletToOtherCorp",
        &[PyRep::Integer(corp_id), PyRep::Integer(npc_corp), PyRep::Integer(2_000)],
    )
    .await;
    let n_lp = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_lp, "OnLPChange"),
        vec![vec![PyRep::Integer(npc_corp), PyRep::Integer(3_000)]]
    );

    call(
        &ctx,
        &mut s1,
        "LPSvc",
        "TransferLPFromMyCorpWalletToOtherCorp",
        &[PyRep::Integer(target_npc_corp), PyRep::Integer(npc_corp), PyRep::Integer(500)],
    )
    .await;
    let n_clp = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_clp, "OnCorpLPChange"),
        vec![vec![PyRep::Integer(npc_corp), PyRep::Integer(1_500)]]
    );

    // 4. Direct Player Trade: InitiateTrade, AddItem, OfferMoney, MakeOffer, Abort
    let trade_item_id = 930_001i64;
    seed_item(&ctx, trade_item_id, 34, P1, STATION_JITA, InventoryFlag::Hangar, 50, false);
    let init_rep = call(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await;
    let trade_id = init_rep.dict_get("tradeContainerID").and_then(|v| v.as_i64()).expect("tradeContainerID");
    let n_ti1 = ctx.drain_notifications_for_char(P1);
    let n_ti2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_ti1, "OnTradeInitiate"),
        vec![vec![PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Integer(trade_id)]]
    );
    assert_eq!(
        events_from(&n_ti2, "OnTradeInitiate"),
        vec![vec![PyRep::Integer(P1), PyRep::Integer(P2), PyRep::Integer(trade_id)]]
    );

    call(
        &ctx,
        &mut s1,
        "trademgr",
        "AddItem",
        &[PyRep::Integer(trade_id), PyRep::Integer(trade_item_id)],
    )
    .await;
    let n_to2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_to2, "OnTradeOffer"),
        vec![vec![
            PyRep::Integer(trade_id),
            PyRep::Bool(false),
            PyRep::Bool(false),
        ]]
    );
    assert_eq!(
        events_from(&n_to2, "OnTradeOfferReset"),
        vec![vec![PyRep::Integer(trade_id)]]
    );
    ctx.drain_notifications_for_char(P1);

    call(
        &ctx,
        &mut s2,
        "trademgr",
        "OfferMoney",
        &[PyRep::Integer(trade_id), PyRep::Real(5_000.0)],
    )
    .await;
    let n_tm1 = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_tm1, "OnTradeMoneyOffer"),
        vec![vec![
            PyRep::Integer(trade_id),
            PyRep::Real(0.0),
            PyRep::Real(5_000.0),
        ]]
    );
    ctx.drain_notifications_for_char(P2);

    call(
        &ctx,
        &mut s1,
        "trademgr",
        "MakeOffer",
        &[PyRep::Integer(trade_id), PyRep::Bool(true)],
    )
    .await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(
        &ctx,
        &mut s2,
        "trademgr",
        "MakeOffer",
        &[PyRep::Integer(trade_id), PyRep::Bool(true)],
    )
    .await;
    let n_tc1 = ctx.drain_notifications_for_char(P1);
    let n_tc2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_tc1, "OnTradeComplete"),
        vec![vec![PyRep::Integer(trade_id)]]
    );
    assert_eq!(
        events_from(&n_tc2, "OnTradeComplete"),
        vec![vec![PyRep::Integer(trade_id)]]
    );

    let init2 = call(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await;
    let trade_id2 = init2.dict_get("tradeContainerID").and_then(|v| v.as_i64()).expect("tradeContainerID");
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s1, "trademgr", "Abort", &[PyRep::Integer(trade_id2)]).await;
    assert_eq!(
        events_from(&ctx.drain_notifications_for_char(P2), "OnTradeCancel"),
        vec![vec![PyRep::Integer(trade_id2)]]
    );
    ctx.drain_notifications_for_char(P1);

    // 5. Market: PlaceMultiSellOrder + PlaceBuyOrder immediate fill -> OnOwnOrdersChanged & OnMarketItemReceived
    let sell_item_id = 930_010i64;
    seed_item(&ctx, sell_item_id, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    call(
        &ctx,
        &mut s1,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(STATION_JITA)),
                ("typeID", PyRep::Integer(34)),
                ("price", PyRep::Real(10.0)),
                ("quantity", PyRep::Integer(100)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await;
    let n_sell = ctx.drain_notifications_for_char(P1);
    assert!(!events_from(&n_sell, "OnOwnOrdersChanged").is_empty());

    call(
        &ctx,
        &mut s2,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(34),
            PyRep::Real(10.0),
            PyRep::Integer(100),
            PyRep::Integer(0),
            PyRep::Integer(1),
        ],
    )
    .await;
    let n_buy = ctx.drain_notifications_for_char(P2);
    assert!(!events_from(&n_buy, "OnOwnOrdersChanged").is_empty());
    let mkt_rec = events_from(&n_buy, "OnMarketItemReceived");
    assert_eq!(mkt_rec.len(), 1);
    assert_eq!(mkt_rec[0][1], PyRep::Integer(34));
    assert_eq!(mkt_rec[0][2], PyRep::Integer(InventoryFlag::Hangar as i64));
    assert_eq!(mkt_rec[0][3], PyRep::Integer(STATION_JITA));
    assert!(events_from(&n_buy, "OnMarketEscrowChanged").is_empty());
    assert!(events_from(&n_buy, "OnClientEvent_MarketItemReceived").is_empty());
    ctx.drain_notifications_for_char(P1);

    // 6. Industry: InstallJob -> OnIndustryJob(jobID, ownerID, blueprintID, installerID, status=1, runs)
    let job_rep = call(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(691), PyRep::Integer(1), PyRep::Integer(1)],
    )
    .await;
    let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    let n_ind = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_ind, "OnIndustryJob"),
        vec![vec![
            PyRep::Integer(job_id),
            PyRep::Integer(P1),
            PyRep::Integer(100_000_007),
            PyRep::Integer(P1),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ]]
    );
    assert!(events_from(&n_ind, "OnIndustryJobClient").is_empty());
}

#[tokio::test]
async fn test_blueprints_and_contracts_notifications() {
    // parity: notify:OnBlueprintsUpdated, notify:OnContractCreated, notify:OnContractAssigned, notify:OnContractAccepted, notify:OnContractCompleted, notify:OnContractRejected, notify:OnContractFailed, notify:OnContractOutbid, notify:OnContractBuyout
    eve_testkit::attest(&[
        "notify:OnBlueprintsUpdated",
        "notify:OnContractCreated",
        "notify:OnContractAssigned",
        "notify:OnContractAccepted",
        "notify:OnContractCompleted",
        "notify:OnContractRejected",
        "notify:OnContractFailed",
        "notify:OnContractOutbid",
        "notify:OnContractBuyout",
    ]);

    let ctx = ServerContext::new();
    let mut s1 = select(&ctx, 7631, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 7632, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("bidder6", "bidder6-secret");
    let p3 = ctx
        .db
        .create_character(uid3, "Bidder Six", 1, 1, 11, 17, &ctx.sde)
        .expect("create p3")
        .character_id;
    let mut s3 = select(&ctx, 7633, "bidder6", "bidder6-secret", p3).await;

    ctx.db.with_mut_snapshot(|snap| {
        for cid in [P1, P2, p3] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::from_f64_isk(200_000_000.0);
                ch.station_id = Some(STATION_JITA);
            }
            snap.skills.entry(cid).or_default().insert(
                25235, // Contracting
                TrainedSkillRecord {
                    type_id: 25235,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    s1.station_id = Some(STATION_JITA);
    s2.station_id = Some(STATION_JITA);
    s3.station_id = Some(STATION_JITA);
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // 1. OnBlueprintsUpdated(ownerID) on InstallJob / CancelJob
    let bp_id = 940_001i64;
    seed_item(&ctx, bp_id, 683, P1, STATION_JITA, InventoryFlag::Hangar, 1, true);
    let job_rep = call(
        &ctx,
        &mut s1,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(bp_id), PyRep::Integer(683), PyRep::Integer(3), PyRep::Integer(1)],
    )
    .await;
    let job_id = job_rep.dict_get("jobID").and_then(|v| v.as_i64()).expect("jobID");
    let n_bp = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_bp, "OnBlueprintsUpdated"),
        vec![vec![PyRep::Integer(P1)]]
    );
    call(&ctx, &mut s1, "industryManager", "CancelJob", &[PyRep::Integer(job_id)]).await;
    assert_eq!(
        events_from(&ctx.drain_notifications_for_char(P1), "OnBlueprintsUpdated"),
        vec![vec![PyRep::Integer(P1)]]
    );

    // 2. Contracts: CreateContract (assigned to P2) -> OnContractCreated & OnContractAssigned, AcceptContract -> OnContractAccepted & OnContractCompleted
    let item1 = 940_010i64;
    seed_item(&ctx, item1, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    let c1_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1), // ItemExchange
            PyRep::Integer(1), // Personal
            PyRep::Integer(P2),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_JITA),
            PyRep::Real(10_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item1)]),
        ],
    )
    .await;
    let cid1 = c1_rep.dict_get("contractID").and_then(|v| v.as_i64()).expect("cid1");
    let n_c1_p1 = ctx.drain_notifications_for_char(P1);
    let n_c1_p2 = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        events_from(&n_c1_p1, "OnContractCreated"),
        vec![vec![PyRep::Integer(cid1)]]
    );
    assert!(events_from(&n_c1_p1, "OnContractEscrowChanged").is_empty());
    assert_eq!(
        events_from(&n_c1_p2, "OnContractAssigned"),
        vec![vec![PyRep::Integer(cid1)]]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());

    call(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid1)]).await;
    let n_acc_p1 = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_acc_p1, "OnContractAccepted"),
        vec![vec![PyRep::Integer(cid1)]]
    );
    assert_eq!(
        events_from(&n_acc_p1, "OnContractCompleted"),
        vec![vec![PyRep::Integer(cid1)]]
    );
    ctx.drain_notifications_for_char(P2);

    // 3. RejectContract -> OnContractRejected
    let item2 = 940_011i64;
    seed_item(&ctx, item2, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    let c2_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(P2),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_JITA),
            PyRep::Real(10_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item2)]),
        ],
    )
    .await;
    let cid2 = c2_rep.dict_get("contractID").and_then(|v| v.as_i64()).expect("cid2");
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s2, "contractProxy", "RejectContract", &[PyRep::Integer(cid2)]).await;
    assert_eq!(
        events_from(&ctx.drain_notifications_for_char(P1), "OnContractRejected"),
        vec![vec![PyRep::Integer(cid2)]]
    );

    // 4. Courier contract + FailContract -> OnContractFailed
    let item3 = 940_012i64;
    seed_item(&ctx, item3, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    let c3_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(3), // Courier
            PyRep::Integer(1),
            PyRep::Integer(P2),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_AMARR),
            PyRep::Real(0.0),
            PyRep::Real(5_000.0),
            PyRep::Real(10_000.0),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item3)]),
        ],
    )
    .await;
    let cid3 = c3_rep.dict_get("contractID").and_then(|v| v.as_i64()).expect("cid3");
    call(&ctx, &mut s2, "contractProxy", "AcceptContract", &[PyRep::Integer(cid3)]).await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);
    call(&ctx, &mut s2, "contractProxy", "FailContract", &[PyRep::Integer(cid3)]).await;
    assert_eq!(
        events_from(&ctx.drain_notifications_for_char(P1), "OnContractFailed"),
        vec![vec![PyRep::Integer(cid3)]]
    );

    // 5. Auction contract: P2 bids, P3 outbids & buys out -> OnContractOutbid (to P2), OnContractBuyout & OnContractCompleted (to P1)
    let item4 = 940_013i64;
    seed_item(&ctx, item4, 34, P1, STATION_JITA, InventoryFlag::Hangar, 100, false);
    let c4_rep = call(
        &ctx,
        &mut s1,
        "contractProxy",
        "CreateContract",
        &[
            PyRep::Integer(2), // Auction
            PyRep::Integer(0), // Public
            PyRep::Integer(0),
            PyRep::Integer(1440),
            PyRep::Integer(1),
            PyRep::Integer(STATION_JITA),
            PyRep::Integer(STATION_JITA),
            PyRep::Real(100_000.0),
            PyRep::Real(0.0),
            PyRep::Real(500_000.0),
            PyRep::String(String::new()),
            PyRep::String(String::new()),
            PyRep::List(vec![PyRep::Integer(item4)]),
        ],
    )
    .await;
    let cid4 = c4_rep.dict_get("contractID").and_then(|v| v.as_i64()).expect("cid4");
    ctx.drain_notifications_for_char(P1);

    call(
        &ctx,
        &mut s2,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(cid4), PyRep::Real(150_000.0)],
    )
    .await;
    ctx.drain_notifications_for_char(P1);
    ctx.drain_notifications_for_char(P2);

    call(
        &ctx,
        &mut s3,
        "contractProxy",
        "PlaceBid",
        &[PyRep::Integer(cid4), PyRep::Real(500_000.0)],
    )
    .await;
    let n_outbid_p2 = ctx.drain_notifications_for_char(P2);
    let n_buyout_p1 = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        events_from(&n_outbid_p2, "OnContractOutbid"),
        vec![vec![PyRep::Integer(cid4)]]
    );
    assert_eq!(
        events_from(&n_buyout_p1, "OnContractBuyout"),
        vec![vec![PyRep::Integer(cid4)]]
    );
    assert_eq!(
        events_from(&n_buyout_p1, "OnContractCompleted"),
        vec![vec![PyRep::Integer(cid4)]]
    );
}
