//! Real server-side emission tests for Notify Batch 9 (Part 1: Skills & Injectors).

use crate::common;
use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, make_item, new_ctx, rpc, JITA_STATION, PILOT_A, PILOT_B, SYS_JITA,
};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

pub(crate) async fn login_docked(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    station_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    rpc(ctx, &mut s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = SYS_JITA;
        }
    });
    s.station_id = Some(station_id);
    s.structure_id = None;
    s.solar_system_id = Some(SYS_JITA);
    s
}

pub(crate) fn upwell_struct(
    sid: i64,
    tid: u32,
    cid: i64,
    sys_id: u32,
    name: &str,
    pos: [f64; 3],
) -> eve_db::UpwellStructureRecord {
    eve_db::UpwellStructureRecord {
        structure_id: sid,
        type_id: tid,
        owner_corp_id: cid,
        solar_system_id: sys_id,
        name: name.into(),
        position: pos,
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 1000,
        shield_hp: 1.0,
        armor_hp: 1.0,
        hull_hp: 1.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0,
        self_destruct_at_unix: None,
    }
}

// parity: notify:OnAlphaInjectionAvailable, notify:OnNonDiminishingInjectionsAdded, notify:OnNonDiminishingInjectionsRemoved
#[tokio::test]
async fn notify_batch9_alpha_and_non_diminishing_injectors() {
    eve_testkit::attest(&[
        "notify:OnAlphaInjectionAvailable",
        "notify:OnNonDiminishingInjectionsAdded",
        "notify:OnNonDiminishingInjectionsRemoved",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_101, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let mut sess_b = login_docked(&ctx, 9_102, "other", "other-secret", PILOT_B, JITA_STATION).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.clone_grade = 1;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.clone_grade = 2;
        }
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // 1. OnAlphaInjectionAvailable: Alpha pilot queries GetNextAvailableInjection when ready
    let next_avail = rpc(&ctx, &mut sess_a, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
        .await
        .expect("GetNextAvailableInjection alpha");
    assert_eq!(next_avail, PyRep::Integer(0));
    let alpha_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(alpha_notifs.iter().any(|(e, a)| e == "OnAlphaInjectionAvailable" && a.is_empty()));

    // Negative 1a: Repeated query while already notified does not duplicate OnAlphaInjectionAvailable
    let _ = rpc(&ctx, &mut sess_a, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
        .await
        .expect("GetNextAvailableInjection repeat");
    let repeat_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!repeat_notifs.iter().any(|(e, _)| e == "OnAlphaInjectionAvailable"));

    // Negative 1b: Omega pilot does NOT receive OnAlphaInjectionAvailable
    let _ = rpc(&ctx, &mut sess_b, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
        .await
        .expect("GetNextAvailableInjection omega");
    let omega_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert!(!omega_notifs.iter().any(|(e, _)| e == "OnAlphaInjectionAvailable"));

    // 2. OnNonDiminishingInjectionsAdded: Claiming a non-diminishing injector redeem token (type 46375)
    sess_a.role |= eve_db::service_roles::ROLE_GMH;
    let tok_res = rpc(
        &ctx,
        &mut sess_a,
        "userSvc",
        "CreateRedeemTokenQA",
        &[PyRep::Integer(46375), PyRep::Integer(1), PyRep::String("NonDim".into())],
    )
    .await
    .expect("CreateRedeemTokenQA");
    sess_a.role &= !eve_db::service_roles::ROLE_GMH;
    let tok_id = match tok_res {
        PyRep::KeyVal(kv) => kv.get("tokenID").and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    };
    assert!(tok_id > 0);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Negative 2a: Claiming a non-existent token fails and does not emit OnNonDiminishingInjectionsAdded
    assert!(rpc(&ctx, &mut sess_a, "userSvc", "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(999_999)])])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnNonDiminishingInjectionsAdded"));

    // Positive: Claiming the non-diminishing injector token emits OnNonDiminishingInjectionsAdded([1])
    let claim_res = rpc(
        &ctx,
        &mut sess_a,
        "userSvc",
        "ClaimRedeemTokens",
        &[PyRep::List(vec![PyRep::Integer(tok_id)])],
    )
    .await
    .expect("ClaimRedeemTokens");
    let created_item_id = match claim_res {
        PyRep::KeyVal(kv) => match kv.get("createdItemIDs") {
            Some(PyRep::List(items)) => items.first().and_then(|v| v.as_i64()).unwrap_or(0),
            _ => 0,
        },
        _ => 0,
    };
    assert!(created_item_id > 0);
    let claim_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(find_notif(&claim_notifs, "OnNonDiminishingInjectionsAdded"), &[PyRep::Integer(1)]);

    // Negative 2b: Claiming a standard non-injector token (Tritanium = 34) does NOT emit OnNonDiminishingInjectionsAdded
    sess_a.role |= eve_db::service_roles::ROLE_GMH;
    let trit_tok = rpc(
        &ctx,
        &mut sess_a,
        "userSvc",
        "CreateRedeemTokenQA",
        &[PyRep::Integer(34), PyRep::Integer(10), PyRep::String("Trit".into())],
    )
    .await
    .expect("CreateRedeemTokenQA trit");
    sess_a.role &= !eve_db::service_roles::ROLE_GMH;
    let trit_id = match trit_tok {
        PyRep::KeyVal(kv) => kv.get("tokenID").and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    };
    let _ = rpc(&ctx, &mut sess_a, "userSvc", "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(trit_id)])])
        .await
        .expect("ClaimRedeemTokens trit");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnNonDiminishingInjectionsAdded"));

    // 3. OnNonDiminishingInjectionsRemoved: Consuming the last non-diminishing injector emits OnNonDiminishingInjectionsRemoved
    // Negative 3a: Injecting a regular Skill Injector (40520) does NOT emit OnNonDiminishingInjectionsRemoved
    let reg_inj_id = 910_000_050_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            reg_inj_id,
            make_item(reg_inj_id, 40520, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(reg_inj_id)])
        .await
        .expect("InjectSkillpoints regular");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnNonDiminishingInjectionsRemoved"));

    // Positive: Consuming the last non-diminishing injector emits OnNonDiminishingInjectionsRemoved
    let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(created_item_id)])
        .await
        .expect("InjectSkillpoints non-dim");
    let inject_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(inject_notifs.iter().any(|(e, _)| e == "OnNonDiminishingInjectionsRemoved"));
}

// parity: notify:OnSkillForcedRefresh, notify:OnLogonSkillsTrained, notify:OnMultipleCharactersTrainingUpdated
#[tokio::test]
async fn notify_batch9_skill_bundles_refresh_logon_and_mct() {
    eve_testkit::attest(&[
        "notify:OnSkillForcedRefresh",
        "notify:OnLogonSkillsTrained",
        "notify:OnMultipleCharactersTrainingUpdated",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_111, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 4. OnSkillBundleInjected: crateService.ClaimSkillBundle with explicit unlearned SDE skill
    let (crate_type_id, bundle_id) = ctx
        .sde
        .load_cfsd_nested("res:/staticdata/crates.fsdbinary")
        .ok()
        .and_then(|cfsd| {
            cfsd.values().find_map(|v| {
                let tid = v.get("typeID")?.as_u64()? as u32;
                let bid = v.get("skillBundleID")?.as_i64()?;
                ctx.sde.types.contains_key(&tid).then_some((tid, bid))
            })
        })
        .unwrap_or((40520, 101));
    let crate_id = 910_000_055_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            crate_id,
            make_item(crate_id, crate_type_id, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    // Negative 4a: ClaimSkillBundle before OpenCrate fails with CrateNotOpened
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "crateService",
        "ClaimSkillBundle",
        &[PyRep::Integer(crate_id), PyRep::Integer(bundle_id)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnSkillBundleInjected"));

    // Negative 4b: ClaimSkillBundle with unknown crate ID fails with CrateNotFound
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "crateService",
        "ClaimSkillBundle",
        &[PyRep::Integer(999_999_999), PyRep::Integer(bundle_id)],
    )
    .await
    .is_err());

    // Security: OpenCrate + ClaimSkillBundle ignores any client-supplied 3rd-arg skill list (DATA GAP: no FSD bundle skills)
    let unlearned_skill = ctx.db.with_snapshot(|snap| {
        let learned = snap.skills.get(&PILOT_A);
        ctx.sde
            .types
            .values()
            .find(|t| {
                t.category_id == 16
                    && t.published
                    && !learned.is_some_and(|m| m.contains_key(&t.type_id))
            })
            .map(|t| t.type_id)
            .unwrap_or(3300)
    });
    let _ = rpc(&ctx, &mut sess_a, "crateService", "OpenCrate", &[PyRep::Integer(crate_id)])
        .await
        .expect("OpenCrate");
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "crateService",
        "ClaimSkillBundle",
        &[
            PyRep::Integer(crate_id),
            PyRep::Integer(bundle_id),
            PyRep::List(vec![PyRep::Integer(unlearned_skill as i64)]),
        ],
    )
    .await
    .expect("ClaimSkillBundle");
    let bundle_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        !bundle_notifs.iter().any(|(e, _)| e == "OnSkillBundleInjected"),
        "ClaimSkillBundle must not trust client-supplied skill lists"
    );

    // 5. OnSkillForcedRefresh: Obsolete skill migration on character logon/refresh
    // Negative 5a: Character with no obsolete skills does NOT emit OnSkillForcedRefresh
    eve_server::services::character_skill_ops::migrate_obsolete_skills_for_character(&ctx, PILOT_A);
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnSkillForcedRefresh"));

    // Positive: Character with an unpublished/obsolete skill emits OnSkillForcedRefresh on migration
    let obsolete_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 16 && !t.published)
        .map(|t| t.type_id)
        .unwrap_or(999_999);
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(PILOT_A).or_default().insert(
            obsolete_tid,
            eve_db::TrainedSkillRecord {
                type_id: obsolete_tid,
                level: 2,
                skill_points: 1_415.0,
            },
        );
    });
    eve_server::services::character_skill_ops::migrate_obsolete_skills_for_character(&ctx, PILOT_A);
    let refresh_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(refresh_notifs.iter().any(|(e, _)| e == "OnSkillForcedRefresh"));

    // Negative 5b: Calling CheckAndSendNotifications with empty queue does NOT emit OnLogonSkillsTrained
    let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "CheckAndSendNotifications", &[])
        .await
        .expect("CheckAndSendNotifications empty");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnLogonSkillsTrained"));

    // 6. OnLogonSkillsTrained: Queue a skill started in the past and trigger CheckAndSendNotifications
    let now_unix = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.skill_queues.insert(
            PILOT_A,
            vec![eve_db::SkillQueueEntry {
                type_id: 3300,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 10.0,
                started_at_unix: Some(now_unix.saturating_sub(3_600)),
            }],
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "CheckAndSendNotifications", &[])
        .await
        .expect("CheckAndSendNotifications trained");
    let logon_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let logon_args = find_notif(&logon_notifs, "OnLogonSkillsTrained");
    assert_eq!(logon_args.len(), 2);
    assert!(matches!(&logon_args[0], PyRep::Dict(entries) if !entries.is_empty()));
    assert_eq!(logon_args[1], PyRep::Bool(true));

    // Negative 6b: Second CheckAndSendNotifications after queue is drained does NOT re-emit OnLogonSkillsTrained
    let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "CheckAndSendNotifications", &[])
        .await
        .expect("CheckAndSendNotifications second");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnLogonSkillsTrained"));

    // 7. OnMultipleCharactersTrainingUpdated: ActivateMultiTraining & InvalidateMultiCharacterTrainingCache
    // Negative 7a: ActivateMultiTraining without MCT certificate fails with UserError and emits nothing
    assert!(rpc(&ctx, &mut sess_a, "userSvc", "ActivateMultiTraining", &[])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnMultipleCharactersTrainingUpdated"));

    // Negative 7b: Unauthenticated InvalidateMultiCharacterTrainingCache fails
    let mut unauth = eve_machonet::SessionState::new(999);
    assert!(rpc(&ctx, &mut unauth, "userSvc", "InvalidateMultiCharacterTrainingCache", &[])
        .await
        .is_err());

    // Positive: With MCT certificate (type 33027) in hangar, ActivateMultiTraining emits OnMultipleCharactersTrainingUpdated
    let mct_id = 910_000_060_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mct_id,
            make_item(mct_id, 33027, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "userSvc", "ActivateMultiTraining", &[])
        .await
        .expect("ActivateMultiTraining");
    let mct_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(mct_notifs.iter().any(|(e, _)| e == "OnMultipleCharactersTrainingUpdated"));
}
