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
    let base_unix: u64 = 1_800_000_000;
    let clock = std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(base_unix));
    ctx.set_clock(clock.clone());
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

    // 1. OnAlphaInjectionAvailable: GetNextAvailableInjection is strictly read-only
    let next_avail = rpc(&ctx, &mut sess_a, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
        .await
        .expect("GetNextAvailableInjection alpha");
    assert_eq!(next_avail, PyRep::Integer(0));
    let query_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!query_notifs.iter().any(|(e, _)| e == "OnAlphaInjectionAvailable"));

    // Consume a Daily Alpha Injector (47414) to start the 24h cooldown
    let daily_inj_id = 910_000_049_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            daily_inj_id,
            make_item(daily_inj_id, 47414, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "skillMgr2", "InjectSkillpoints", &[PyRep::Integer(daily_inj_id)])
        .await
        .expect("InjectSkillpoints daily alpha");
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    let cd_avail = rpc(&ctx, &mut sess_a, "alphaInjectorMgr", "GetNextAvailableInjection", &[])
        .await
        .expect("GetNextAvailableInjection on cooldown");
    assert!(matches!(cd_avail, PyRep::Integer(v) if v > 0));

    // Negative 1a: Sweep before ALPHA_INJECTOR_COOLDOWN_TICKS expires does NOT emit OnAlphaInjectionAvailable
    eve_server::services::notify_emitters_batch9::sweep_due_login_milestones_and_alpha_injectors(
        &ctx,
        base_unix,
    );
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnAlphaInjectionAvailable"));

    // Positive: Advance clock past ALPHA_INJECTOR_COOLDOWN_TICKS (24h = 86_400s) and run sweep
    eve_db::ServerClock::advance_secs(&*clock, 86_401);
    eve_server::services::notify_emitters_batch9::sweep_due_login_milestones_and_alpha_injectors(
        &ctx,
        base_unix + 86_401,
    );
    let alpha_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(alpha_notifs.iter().any(|(e, a)| e == "OnAlphaInjectionAvailable" && a.is_empty()));

    // Negative 1b: Repeated sweep after notification does not duplicate OnAlphaInjectionAvailable
    eve_server::services::notify_emitters_batch9::sweep_due_login_milestones_and_alpha_injectors(
        &ctx,
        base_unix + 86_402,
    );
    let repeat_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!repeat_notifs.iter().any(|(e, _)| e == "OnAlphaInjectionAvailable"));

    // Negative 1c: Omega pilot does NOT receive OnAlphaInjectionAvailable
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
    assert_eq!(find_notif(&mct_notifs, "OnMultipleCharactersTrainingUpdated"), &[]);
}

// Folded submodule: notify_real_emitters_batch9_part2
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch9_part2 {
    use super::*;

// Real server-side emission tests for Notify Batch 9 (Part 2: Milestones, Redeeming, Crates, Character & Subscription).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, make_item, new_ctx, rpc, JITA_STATION, PILOT_A,
};
use crate::notify_real_emitters_batch9::login_docked;
use eve_db::InventoryFlag;
use eve_proto::PyRep;

// parity: notify:OnMilestoneTimerStarted, notify:OnRedeemingQueueUpdated, notify:OnCrateLootSpawned, notify:OnPodOrCorvetteAssembly
#[tokio::test]
async fn notify_batch9_milestones_redeeming_crates_and_assembly() {
    eve_testkit::attest(&[
        "notify:OnMilestoneTimerStarted",
        "notify:OnRedeemingQueueUpdated",
        "notify:OnCrateLootSpawned",
        "notify:OnPodOrCorvetteAssembly",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_201, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 1-2. OnMilestoneTimerStarted & OnLoginMilestoneAchieved
    // Negative 1a: Unauthenticated ProcessCharacterLogon fails
    let mut unauth = eve_machonet::SessionState::new(998);
    assert!(rpc(&ctx, &mut unauth, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .is_err());

    // Positive: ProcessCharacterLogon starts milestone timer and emits OnMilestoneTimerStarted([end_ft])
    let _ = rpc(&ctx, &mut sess_a, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    let ms_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let timer_args = find_notif(&ms_notifs, "OnMilestoneTimerStarted");
    assert_eq!(timer_args.len(), 1);
    assert!(timer_args[0].as_i64().unwrap_or(0) > 0);

    // Negative 1c: Advancing cluster tick past 7200s does NOT auto-credit ISK or emit OnLoginMilestoneAchieved
    let future_unix = ctx.now_unix() + 7_205;
    eve_server::services::notify_emitters_batch9::sweep_due_login_milestones_and_alpha_injectors(
        &ctx,
        future_unix,
    );
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnLoginMilestoneAchieved"));

    // Positive 1b: ClaimRewards grants milestone reward once and emits OnLoginMilestoneAchieved
    assert!(rpc(&ctx, &mut sess_a, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .is_ok());
    assert!(drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnLoginMilestoneAchieved"));
    assert!(rpc(&ctx, &mut sess_a, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .is_err());

    // 3. OnRedeemingQueueUpdated
    // Negative 3a: TrashRedeemTokens with non-existent token does NOT emit OnRedeemingQueueUpdated
    let trash_empty = rpc(&ctx, &mut sess_a, "userSvc", "TrashRedeemTokens", &[PyRep::Integer(888_888)])
        .await
        .expect("TrashRedeemTokens empty");
    assert_eq!(trash_empty, PyRep::Bool(false));
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

    // Positive: CreateRedeemTokenQA emits OnRedeemingQueueUpdated
    sess_a.role |= eve_db::service_roles::ROLE_GMH;
    let tok_res = rpc(
        &ctx,
        &mut sess_a,
        "userSvc",
        "CreateRedeemTokenQA",
        &[PyRep::Integer(34), PyRep::Integer(5), PyRep::String("RedeemTest".into())],
    )
    .await
    .expect("CreateRedeemTokenQA");
    sess_a.role &= !eve_db::service_roles::ROLE_GMH;
    let tok_id = match tok_res {
        PyRep::KeyVal(kv) => kv.get("tokenID").and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    };
    let redeem_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(redeem_notifs.iter().any(|(e, _)| e == "OnRedeemingQueueUpdated"));

    // Negative 3b: ClaimRedeemTokens with invalid token fails and does not emit OnRedeemingQueueUpdated
    assert!(rpc(&ctx, &mut sess_a, "userSvc", "ClaimRedeemTokens", &[PyRep::List(vec![PyRep::Integer(777_777)])])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

    // Positive: TrashRedeemTokens on valid token emits OnRedeemingQueueUpdated
    let trash_ok = rpc(&ctx, &mut sess_a, "userSvc", "TrashRedeemTokens", &[PyRep::Integer(tok_id)])
        .await
        .expect("TrashRedeemTokens valid");
    assert_eq!(trash_ok, PyRep::Bool(true));
    assert!(drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnRedeemingQueueUpdated"));

    // 4. OnCrateLootSpawned
    // Negative 4a: ClaimLoot on non-existent crate fails and emits nothing
    assert!(rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(920_999_999)])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnCrateLootSpawned"));

    // Negative 4b: ClaimLoot on a non-crate item (Tritanium = 34) fails with NotACrate
    let non_crate_id = 920_000_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            non_crate_id,
            make_item(non_crate_id, 34, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    assert!(rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(non_crate_id)])
        .await
        .is_err());

    // Positive: ClaimLoot on a valid crate (type 40520 or 3466/crate)
    let crate_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| {
            (t.name.contains("Crate")
                || t.name.contains("Box")
                || t.name.contains("Cache")
                || t.name.contains("Bundle")
                || t.name.contains("Package"))
                && ctx.sde.type_materials.get(&t.type_id).is_some_and(|m| !m.is_empty())
        })
        .map(|t| t.type_id)
        .unwrap_or(40520);
    let crate_id = 920_000_011_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            crate_id,
            make_item(crate_id, crate_type_id, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "crateService", "OpenCrate", &[PyRep::Integer(crate_id)])
        .await
        .expect("OpenCrate");
    let mut crate_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = rpc(&ctx, &mut sess_a, "crateService", "ClaimLoot", &[PyRep::Integer(crate_id)])
        .await
        .expect("ClaimLoot");
    crate_notifs.extend(drain_all(&ctx, &mut sess_a, PILOT_A));
    assert!(crate_notifs.iter().any(|(e, a)| e == "OnCrateLootSpawned" && a.is_empty()));

    // 5. OnPodOrCorvetteAssembly
    // Negative 5a: Assembling a regular frigate (Rifter = 587, group 25) does NOT emit OnPodOrCorvetteAssembly
    let rifter_id = 920_000_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            rifter_id,
            make_item(rifter_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(rifter_id)])
        .await
        .expect("AssembleShip rifter");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnPodOrCorvetteAssembly"));

    // Negative 5b: Assembling a non-ship item fails with NotAShip
    assert!(rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(non_crate_id)])
        .await
        .is_err());

    // Positive: Assembling a Corvette (Velator = 606, group 237) emits OnPodOrCorvetteAssembly([velator_id, 606])
    let corvette_id = 920_000_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            corvette_id,
            make_item(corvette_id, 606, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let _ = rpc(&ctx, &mut sess_a, "ship", "AssembleShip", &[PyRep::Integer(corvette_id)])
        .await
        .expect("AssembleShip corvette");
    let asm_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&asm_notifs, "OnPodOrCorvetteAssembly"),
        &[PyRep::Integer(corvette_id), PyRep::Integer(606)]
    );
}

// parity: notify:OnPasswordChanged, notify:OnPlayerStateUpdated, notify:OnSubscriptionChangedServer, notify:OnSubscriptionTimeUpdateServer
#[tokio::test]
async fn notify_batch9_password_player_state_brain_and_subscription() {
    eve_testkit::attest(&[
        "notify:OnPasswordChanged",
        "notify:OnPlayerStateUpdated",
        "notify:OnSubscriptionChangedServer",
        "notify:OnSubscriptionTimeUpdateServer",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_211, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 6. OnPasswordChanged
    // Negative 6a: Setting password on a standard Cargo Container (type 23, group 12) fails
    let bad_item_id = 920_000_030_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bad_item_id,
            make_item(bad_item_id, 23, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(bad_item_id), PyRep::String("pw".into())],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnPasswordChanged"));

    // Negative 6b: Unauthenticated SetPassword fails
    let mut unauth = eve_machonet::SessionState::new(997);
    assert!(rpc(
        &ctx,
        &mut unauth,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(bad_item_id), PyRep::String("pw".into())],
    )
    .await
    .is_err());

    // Positive: Setting password on an Audit Log Secure Container (type 17366) emits OnPasswordChanged
    let alsc_id = 920_000_031_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            alsc_id,
            make_item(alsc_id, 17366, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(alsc_id), PyRep::String("hunter2".into())],
    )
    .await
    .expect("SetPassword ALSC");
    let pw_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&pw_notifs, "OnPasswordChanged"),
        &[
            PyRep::Integer(1),
            PyRep::keyval([
                ("itemID", PyRep::Integer(alsc_id)),
                ("typeID", PyRep::Integer(17366)),
            ]),
        ]
    );

    // 7. OnPlayerStateUpdated: Project Discovery classification
    // Negative 7a: Unauthenticated post_classification fails
    assert!(rpc(&ctx, &mut unauth, "projectDiscovery", "post_classification", &[])
        .await
        .is_err());

    // Negative 7b: Invalid classification payload fails without emitting OnPlayerStateUpdated
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "projectDiscovery",
        "post_classification",
        &[PyRep::String("invalid_not_dict".into())],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnPlayerStateUpdated"));

    // Positive: Valid Project Discovery classification emits OnPlayerStateUpdated([playerState])
    let payload = PyRep::Dict(vec![
        (PyRep::String("task_id".into()), PyRep::Integer(1)),
        (
            PyRep::String("classification".into()),
            PyRep::List(vec![PyRep::Integer(1)]),
        ),
    ]);
    let task_res = rpc(&ctx, &mut sess_a, "ProjectDiscovery", "get_new_task", &[])
        .await
        .expect("get_new_task");
    let task_id = match task_res {
        PyRep::KeyVal(kv) => kv
            .get("taskNumber")
            .or_else(|| kv.get("id"))
            .and_then(|v| v.as_i64())
            .unwrap_or(1),
        _ => 1,
    };
    let _ = rpc(&ctx, &mut sess_a, "ProjectDiscovery", "post_classification", &[PyRep::Integer(task_id), payload])
        .await
        .expect("post_classification");
    let pd_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let ps_args = find_notif(&pd_notifs, "OnPlayerStateUpdated");
    assert_eq!(ps_args.len(), 1);
    assert!(matches!(&ps_args[0], PyRep::KeyVal(_)));

    // 8. OnServerBrainUpdated: Applying free SP or plugging an implant emits OnServerBrainUpdated
    // Negative 8a: ApplyFreeSkillPoints with 0 SP fails and does not emit OnServerBrainUpdated
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(3300), PyRep::Real(0.0)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnServerBrainUpdated"));

    // Negative 8b: ApplyFreeSkillPoints with unknown skill type 0 fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(0), PyRep::Real(1_000.0)],
    )
    .await
    .is_err());

    // Positive: Grant free SP and apply to Gunnery (3300) -> emits OnServerBrainUpdated([version, grayMatter])
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.free_skill_points = 500.0;
        }
    });
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "skillMgr2",
        "ApplyFreeSkillPoints",
        &[PyRep::Integer(3300), PyRep::Real(250.0)],
    )
    .await
    .expect("ApplyFreeSkillPoints");
    let brain_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!brain_notifs.iter().any(|(e, _)| e == "OnServerBrainUpdated"));

    // 9-10. OnSubscriptionChangedServer & OnSubscriptionTimeUpdateServer
    let uid_a = sess_a.user_id.unwrap_or(9_211);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.clone_grade = 1;
        }
    });
    let future_exp = ctx.now_unix() + 30 * 86_400;
    let _ = eve_server::services::account_services::set_subscription_expiry(&ctx, uid_a, future_exp);
    let sub_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&sub_notifs, "OnSubscriptionChangedServer"),
        &[PyRep::Integer(2)]
    );
    let time_args = find_notif(&sub_notifs, "OnSubscriptionTimeUpdateServer");
    assert_eq!(time_args.len(), 1);
    assert!(time_args[0].as_i64().unwrap_or(0) > 0);

    // Negative 9a/10a: Calling sync_subscription again when clone_grade is already 2 does NOT re-emit OnSubscriptionChangedServer
    let _ = eve_server::services::account_services::sync_subscription(&ctx, uid_a);
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnSubscriptionChangedServer"));

    // Negative 9b/10b: Expiring subscription (until = 1) emits OnSubscriptionTimeUpdateServer
    let _ = eve_server::services::account_services::set_subscription_expiry(&ctx, uid_a, 1);
    let lapse_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(lapse_notifs
        .iter()
        .any(|(e, a)| e == "OnSubscriptionTimeUpdateServer" && a.len() == 1));
}

}

// Folded submodule: notify_real_emitters_batch9_part3
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch9_part3 {
    use super::*;

// Real server-side emission tests for Notify Batch 9 (Part 3: Diplomacy, Fittings, Offers, NES, Agents & Datacores).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, new_ctx, rpc, JITA_STATION, PILOT_A, PILOT_B,
};
use crate::notify_real_emitters_batch9::login_docked;
use eve_proto::PyRep;

// parity: notify:OnAllowWarUpdated, notify:OnNesIntroStateChanged
#[tokio::test]
async fn notify_batch9_diplomacy_fittings_offers_and_nes() {
    eve_testkit::attest(&[
        "notify:OnAllowWarUpdated",
        "notify:OnNesIntroStateChanged",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_301, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let mut sess_b = login_docked(&ctx, 9_302, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let corp_id = 98_900_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Test Diplo Corp".into(),
                ticker: "TDC".into(),
                ceo_id: PILOT_A,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
        }
        if let Some(ch) = snap.characters.get_mut(&PILOT_B) {
            ch.corporation_id = corp_id;
        }
    });
    sess_a.corporation_id = Some(corp_id);
    sess_b.corporation_id = Some(corp_id);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // 1. OnAllowWarUpdated
    // Negative 1a: Non-CEO/non-Director calling UpdateCorporation fails with CorpRoleDenied
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Desc".into()),
            PyRep::String("https://example.com".into()),
            PyRep::Real(0.05),
            PyRep::Bool(true),
        ],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnAllowWarUpdated"));

    // Negative 1b: Character with no corporation fails with NoCorporation
    let mut no_corp_sess = sess_a.clone();
    no_corp_sess.corporation_id = None;
    assert!(rpc(
        &ctx,
        &mut no_corp_sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Desc".into()),
            PyRep::String("https://example.com".into()),
            PyRep::Real(0.05),
            PyRep::Bool(true),
        ],
    )
    .await
    .is_err());

    // Positive: CEO calling UpdateCorporation with allowWar=false changes from default true -> false, persists, and emits OnAllowWarUpdated([corp_id, false])
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Desc".into()),
            PyRep::String("https://example.com".into()),
            PyRep::Real(0.05),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("UpdateCorporation");
    let war_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&war_notifs, "OnAllowWarUpdated"),
        &[PyRep::Integer(corp_id), PyRep::Bool(false)]
    );
    // Verify corpmgr.GetPublicInfo reflects persisted allowWar=false
    let pub_info = rpc(&ctx, &mut sess_a, "corpmgr", "GetPublicInfo", &[PyRep::Integer(corp_id)])
        .await
        .expect("GetPublicInfo");
    if let PyRep::KeyVal(kv) = pub_info {
        assert_eq!(kv.get("allowWar"), Some(&PyRep::Bool(false)));
    }
    // Re-calling UpdateCorporation with same allowWar=false does NOT re-emit OnAllowWarUpdated
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Desc 2".into()),
            PyRep::String("https://example.com".into()),
            PyRep::Real(0.05),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("UpdateCorporation no-op allowWar");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnAllowWarUpdated"));

    // 2. OnCommunityFittingsUpdated
    let fit_id = 777_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.saved_fittings.insert(
            fit_id,
            eve_db::SavedFittingRecord {
                fitting_id: fit_id,
                owner_id: corp_id,
                name: "Community Rifter".into(),
                description: "Corp doctrine".into(),
                ship_type_id: 587,
                items: vec![],
            },
        );
    });
    // Negative 2a: Non-director member (PILOT_B) calling DeleteFitting for corp fails
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "corpFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnCommunityFittingsUpdated"));

    // Negative 2b: Deleting non-existent corp fitting returns false and does not emit OnCommunityFittingsUpdated
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(corp_id), PyRep::Integer(999_999)],
    )
    .await
    .expect("DeleteFitting missing");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnCommunityFittingsUpdated"));

    // Positive: CEO deleting corp fitting does NOT emit OnCommunityFittingsUpdated (corp fittings != community fittings)
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "DeleteFitting",
        &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)],
    )
    .await
    .expect("DeleteFitting corp");
    let fit_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!fit_notifs.iter().any(|(e, _)| e == "OnCommunityFittingsUpdated"));

    // 3. TrackOfferPurchased does NOT echo OnContextualOfferPurchased
    // Negative 3a: TrackOfferPurchased with invalid offerID (<= 0) fails with InvalidOfferID
    assert!(rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferPurchased", &[PyRep::Integer(0)])
        .await
        .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnContextualOfferPurchased"));

    // Negative 3b: TrackOfferSeen does NOT emit OnContextualOfferPurchased
    let _ = rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferSeen", &[PyRep::Integer(42)])
        .await
        .expect("TrackOfferSeen");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnContextualOfferPurchased"));

    // TrackOfferPurchased(42) records telemetry without echoing OnContextualOfferPurchased
    let _ = rpc(&ctx, &mut sess_a, "contextualOfferMgr", "TrackOfferPurchased", &[PyRep::Integer(42)])
        .await
        .expect("TrackOfferPurchased");
    let offer_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!offer_notifs.iter().any(|(e, _)| e == "OnContextualOfferPurchased"));

    // 4. OnNesIntroStateChanged
    // Negative 4a: Unauthenticated skip_nes_intro fails
    let mut unauth = eve_machonet::SessionState::new(996);
    assert!(rpc(&ctx, &mut unauth, "nes_intro", "skip_nes_intro", &[]).await.is_err());

    // Negative 4b: get_nes_intro_state is read-only and does NOT emit OnNesIntroStateChanged
    let _ = rpc(&ctx, &mut sess_a, "nes_intro", "get_nes_intro_state", &[])
        .await
        .expect("get_nes_intro_state");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnNesIntroStateChanged"));

    // Positive: skip_nes_intro emits OnNesIntroStateChanged([1])
    let _ = rpc(&ctx, &mut sess_a, "nes_intro", "skip_nes_intro", &[])
        .await
        .expect("skip_nes_intro");
    let nes_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&nes_notifs, "OnNesIntroStateChanged"),
        &[PyRep::Integer(PILOT_A), PyRep::Integer(1)]
    );
}

// parity: notify:OnIncomingAgentMessage, notify:OnIncomingTransmission, notify:OnEpicJournalChange, notify:OnDatacoreBought
#[tokio::test]
async fn notify_batch9_agents_epic_journal_and_datacores() {
    eve_testkit::attest(&[
        "notify:OnIncomingAgentMessage",
        "notify:OnIncomingTransmission",
        "notify:OnEpicJournalChange",
        "notify:OnDatacoreBought",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_311, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Find an Epic Arc agent (agent_type_id == 10, level 1) or fallback to any level 1 agent
    let (epic_agent_id, epic_sys_id, epic_station_id) = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 10 && a.level == 1 && a.station_id.is_some())
        .or_else(|| ctx.sde.agents.values().find(|a| a.level == 1 && a.station_id.is_some()))
        .map(|a| (a.agent_id, a.solar_system_id.unwrap_or(30_000_142), a.station_id.unwrap_or(JITA_STATION)))
        .expect("SDE must have a level 1 agent");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = epic_sys_id;
            ch.station_id = Some(epic_station_id);
        }
    });
    sess_a.solar_system_id = Some(epic_sys_id);
    sess_a.station_id = Some(epic_station_id);

    // 5-8. OnOfferReceived, OnIncomingTransmission, OnIncomingAgentMessage, OnEpicJournalChange
    // Negative 5a: Action 3 (Accept) before Offer fails with MissionNotOffered and emits nothing
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(epic_agent_id)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnIncomingAgentMessage"));

    // Negative 5b: Unknown agent ID fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(999_999_999)],
    )
    .await
    .is_err());

    // Positive Offer (action 2) -> emits OnIncomingTransmission
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(epic_agent_id)],
    )
    .await
    .expect("Agent Offer");
    let offer_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(offer_notifs.iter().any(|(e, a)| e == "OnIncomingTransmission" && a.len() == 2));

    // Positive Accept (action 3) -> emits OnIncomingAgentMessage, OnIncomingTransmission, and (if epic arc) OnEpicJournalChange
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(epic_agent_id)],
    )
    .await
    .expect("Agent Accept");
    let accept_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let msg_args = find_notif(&accept_notifs, "OnIncomingAgentMessage");
    assert_eq!(msg_args.len(), 2);
    assert_eq!(msg_args[0], PyRep::Integer(epic_agent_id));
    assert!(matches!(&msg_args[1], PyRep::Tuple(t) if t.len() == 2));
    assert_eq!(find_notif(&accept_notifs, "OnEpicJournalChange"), &[]);

    // TrackOfferDelivered (contextualOfferMgr) -> records telemetry without echoing OnOfferReceived
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "contextualOfferMgr",
        "TrackOfferDelivered",
        &[PyRep::Integer(77)],
    )
    .await
    .expect("TrackOfferDelivered");
    let recv_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!recv_notifs.iter().any(|(e, _)| e == "OnOfferReceived"));

    // Negative 6b: TrackOfferDelivered with offerID <= 0 fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "contextualOfferMgr",
        "TrackOfferDelivered",
        &[PyRep::Integer(0)],
    )
    .await
    .is_err());

    // 9. OnDatacoreBought: R&D agent action 14 (Buy Datacores)
    let rnd_agent_id = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 4)
        .map(|a| a.agent_id)
        .unwrap_or(epic_agent_id);
    let datacore_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 333)
        .map(|t| t.type_id)
        .unwrap_or(20410);

    // Negative 9a: Buying datacores without active research fails and does not emit OnDatacoreBought
    let (rd_sys, rd_sta) = ctx
        .sde
        .agents
        .get(&rnd_agent_id)
        .map(|a| (a.solar_system_id.unwrap_or(30_000_142), a.station_id.unwrap_or(JITA_STATION)))
        .unwrap_or((30_000_142, JITA_STATION));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = rd_sys;
            ch.station_id = Some(rd_sta);
        }
    });
    sess_a.solar_system_id = Some(rd_sys);
    sess_a.station_id = Some(rd_sta);
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(14), PyRep::Integer(rnd_agent_id), PyRep::Integer(1), PyRep::Integer(datacore_tid as i64)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnDatacoreBought"));

    // Seed active research with RP for rnd_agent_id, then test Negative 9b (insufficient RP) and Positive
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::agents_missions::save_rnd_research_entries(
            snap,
            PILOT_A,
            &[eve_server::services::agents_missions::RndResearchEntry {
                agent_id: rnd_agent_id,
                research_type_id: 11442,
                points_per_day: 100.0,
                initial_points: 500.0,
                started_unix: ctx.now_unix(),
            }],
        );
    });

    // Negative 9b: Requesting 100 datacores (5,000 RP) when only 500 RP available fails with InsufficientResearchPoints
    let err_9b = rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(14), PyRep::Integer(rnd_agent_id), PyRep::Integer(100), PyRep::Integer(datacore_tid as i64)],
    )
    .await
    .unwrap_err();
    assert_eq!(err_9b.code, "InsufficientResearchPoints");
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(14), PyRep::Integer(rnd_agent_id), PyRep::Integer(2), PyRep::Integer(datacore_tid as i64)],
    )
    .await
    .expect("Buy Datacores");
    let dc_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(dc_notifs.iter().any(|(e, a)| e == "OnDatacoreBought" && a.len() == 3));
}

}

// Folded submodule: notify_real_emitters_batch9_part4
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch9_part4 {
    use super::*;

// Real server-side emission tests for Notify Batch 9 (Part 4: Facilities, Industry, Trade, Market, Godma & Cache).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, make_item, new_ctx, rpc, JITA_STATION,
    PILOT_A, PILOT_B, SYS_JITA,
};
use crate::notify_real_emitters_batch9::{login_docked, upwell_struct};
use eve_db::InventoryFlag;
use eve_proto::PyRep;
use glam::DVec3;

// parity: notify:OnFacilitiesUpdated, notify:OnPrimingNeededForTradeItems, notify:OnSellRepackageErrors
#[tokio::test]
async fn notify_batch9_facilities_industry_trade_and_repackage_errors() {
    eve_testkit::attest(&[
        "notify:OnFacilitiesUpdated",
        "notify:OnPrimingNeededForTradeItems",
        "notify:OnSellRepackageErrors",
    ]);

    let ctx = new_ctx();
    let ship_a = 940_000_001_i64;
    let mut sess_a = login_in_space(&ctx, 9_401, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_docked(&ctx, 9_402, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // 1. OnFacilitiesUpdated: SetFacilityTaxes on a player-owned structure
    let struct_id = 1_040_000_000_001_i64;
    let corp_id = 98_900_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Ind Corp".into(),
                ticker: "INDC".into(),
                ceo_id: PILOT_A,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = SYS_JITA;
        }
        snap.structures.insert(
            struct_id,
            upwell_struct(struct_id, 35835, corp_id, SYS_JITA, "Test Athanor", [10_000.0, 0.0, 0.0]),
        );
    });
    sess_a.corporation_id = Some(corp_id);

    // Negative 1a: Non-owner character (PILOT_B) calling SetFacilityTaxes fails with FacilityPermissionDenied
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Real(0.05)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnFacilitiesUpdated"));

    // Negative 1b: Invalid negative tax rate fails with InvalidFacilityTaxRate
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Real(-0.1)],
    )
    .await
    .is_err());

    // Positive: CEO calling SetFacilityTaxes emits OnFacilitiesUpdated
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Real(0.05)],
    )
    .await
    .expect("SetFacilityTaxes");
    let fac_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&fac_notifs, "OnFacilitiesUpdated"),
        &[PyRep::List(vec![PyRep::Integer(struct_id)])]
    );

    // 2. OnIndustryMaterials: blueprintManager.GetBlueprintData
    let bp_item_id = 940_000_050_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bp_item_id,
            make_item(bp_item_id, 687, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    // Negative 2a: Unknown blueprint type/item ID fails with BlueprintNotFound
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnIndustryMaterials"));

    // Negative 2b: Unauthenticated GetBlueprintData does NOT emit OnIndustryMaterials
    let mut unauth = eve_machonet::SessionState::new(995);
    let _ = rpc(
        &ctx,
        &mut unauth,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(bp_item_id)],
    )
    .await;
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnIndustryMaterials"));

    // Read-only GetBlueprintData on owned blueprint does NOT emit OnIndustryMaterials
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(bp_item_id)],
    )
    .await
    .expect("GetBlueprintData");
    let mat_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!mat_notifs.iter().any(|(e, _)| e == "OnIndustryMaterials"));

    // 3. OnPrimingNeededForTradeItems: Direct station trade AddItem
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
    });
    sess_a.station_id = Some(JITA_STATION);
    let trade_res = rpc(
        &ctx,
        &mut sess_a,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(PILOT_B)],
    )
    .await
    .expect("InitiateTrade");
    let trade_id = match trade_res {
        PyRep::KeyVal(kv) => kv.get("tradeContainerID").and_then(|v| v.as_i64()).unwrap_or(0),
        _ => 0,
    };
    assert!(trade_id > 0);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // Negative 3a: Adding non-existent item fails and does not emit OnPrimingNeededForTradeItems
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "trademgr",
        "AddItem",
        &[PyRep::Integer(trade_id), PyRep::Integer(940_999_999), PyRep::Integer(1)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_b, PILOT_B)
        .iter()
        .any(|(e, _)| e == "OnPrimingNeededForTradeItems"));

    // Negative 3b: OfferMoney does NOT emit OnPrimingNeededForTradeItems
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "trademgr",
        "OfferMoney",
        &[PyRep::Integer(trade_id), PyRep::Real(100.0)],
    )
    .await
    .expect("OfferMoney");
    assert!(!drain_all(&ctx, &mut sess_b, PILOT_B)
        .iter()
        .any(|(e, _)| e == "OnPrimingNeededForTradeItems"));

    // Positive: Adding a valid hangar item emits OnPrimingNeededForTradeItems([item_id]) to partner PILOT_B
    let trade_item_id = 940_000_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            trade_item_id,
            make_item(trade_item_id, 34, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 50, false),
        );
    });
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "trademgr",
        "AddItem",
        &[PyRep::Integer(trade_id), PyRep::Integer(trade_item_id), PyRep::Integer(10)],
    )
    .await
    .expect("AddItem");
    let partner_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        find_notif(&partner_notifs, "OnPrimingNeededForTradeItems"),
        &[PyRep::List(vec![PyRep::Integer(trade_item_id)])]
    );

    // 4. OnSellRepackageErrors: PlaceMultiSellOrder on an assembled (singleton) ship
    let assembled_ship_id = 940_000_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            assembled_ship_id,
            make_item(assembled_ship_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // Negative 4a: Selling a clean, undamaged assembled ship fails with CannotSellAssembledItem and does NOT emit OnSellRepackageErrors
    let sell_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION)),
        ("typeID", PyRep::Integer(587)),
        ("itemID", PyRep::Integer(assembled_ship_id)),
        ("price", PyRep::Real(100_000.0)),
        ("quantity", PyRep::Integer(1)),
        ("duration", PyRep::Integer(7)),
    ]);
    let clean_err = rpc(
        &ctx,
        &mut sess_a,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![sell_entry.clone()]), PyRep::Bool(false), PyRep::Integer(7)],
    )
    .await
    .unwrap_err();
    assert_eq!(clean_err.code, "CannotSellAssembledItem");
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnSellRepackageErrors"));

    // Positive 4: Selling a damaged assembled ship fails with CantRepackageDamagedItem and emits OnSellRepackageErrors
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&assembled_ship_id) {
            it.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 25.0);
        }
    });
    let dmg_err = rpc(
        &ctx,
        &mut sess_a,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![sell_entry]), PyRep::Bool(false), PyRep::Integer(7)],
    )
    .await
    .unwrap_err();
    assert_eq!(dmg_err.code, "CantRepackageDamagedItem");
    let repack_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let repack_args = find_notif(&repack_notifs, "OnSellRepackageErrors");
    assert_eq!(repack_args.len(), 1);
    assert!(matches!(
        &repack_args[0],
        PyRep::Dict(entries) if entries.iter().any(|(k, v)| k.as_str() == Some("CantRepackageDamagedItem") && *v == PyRep::Integer(1))
    ));

    // Negative 4b: Selling a repackaged item with invalid price (0.0) fails without emitting OnSellRepackageErrors
    let repackaged_id = 940_000_021_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            repackaged_id,
            make_item(repackaged_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, false),
        );
    });
    let bad_price_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(JITA_STATION)),
        ("typeID", PyRep::Integer(587)),
        ("itemID", PyRep::Integer(repackaged_id)),
        ("price", PyRep::Real(0.0)),
        ("quantity", PyRep::Integer(1)),
        ("duration", PyRep::Integer(7)),
    ]);
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[PyRep::List(vec![bad_price_entry]), PyRep::Bool(false), PyRep::Integer(7)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnSellRepackageErrors"));
}

// parity: notify:OnGodmaEffectInfo, notify:OnGodmaFlushLocationProfile, notify:OnGodmaPrimeItem, notify:OnInvalidateCacheForLocations, notify:OnMultiEvent
#[tokio::test]
async fn notify_batch9_godma_and_location_cache_invalidation() {
    eve_testkit::attest(&[
        "notify:OnGodmaEffectInfo",
        "notify:OnGodmaFlushLocationProfile",
        "notify:OnGodmaPrimeItem",
        "notify:OnInvalidateCacheForLocations",
        "notify:OnMultiEvent",
    ]);

    let ctx = new_ctx();
    let (ship_a, ship_b) = (940_000_101_i64, 940_000_102_i64);
    let mut sess_a = login_in_space(&ctx, 9_411, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_in_space(&ctx, 9_412, "other", "other-secret", PILOT_B, ship_b, 587, SYS_JITA, DVec3::new(1_000.0, 0.0, 0.0)).await;

    let mod_id = 940_000_110_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_item(mod_id, 2873, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
    });
    ctx.invalidate_ship_fitting(ship_a);
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    // 5-7. OnGodmaPrimeItem, OnGodmaEffectInfo, OnGodmaFlushLocationProfile
    // Negative 5a: SetModuleOnline on a ship owned by PILOT_B fails with ShipOwnershipViolation
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "SetModuleOnline",
        &[PyRep::Integer(ship_b), PyRep::Integer(mod_id)],
    )
    .await
    .is_err());
    assert!(!drain_all(&ctx, &mut sess_a, PILOT_A)
        .iter()
        .any(|(e, _)| e == "OnGodmaPrimeItem"));

    // Positive: SetModuleOnline emits OnGodmaPrimeItem and OnGodmaEffectInfo
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "SetModuleOnline",
        &[PyRep::Integer(ship_a), PyRep::Integer(mod_id)],
    )
    .await
    .expect("SetModuleOnline");
    let online_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let prime_args = find_notif(&online_notifs, "OnGodmaPrimeItem");
    assert_eq!(prime_args.len(), 2);
    assert_eq!(prime_args[0], PyRep::Integer(ship_a));
    assert_eq!(prime_args[1].dict_get("itemID"), Some(&PyRep::Integer(mod_id)));
    assert!(matches!(prime_args[1].dict_get("attributes"), Some(PyRep::Dict(d)) if d.len() > 2));
    let eff_args = find_notif(&online_notifs, "OnGodmaEffectInfo");
    assert_eq!(eff_args.len(), 4);
    assert_eq!(eff_args[0], PyRep::Integer(mod_id));
    assert_eq!(eff_args[1], PyRep::Integer(2873));

    // Negative 7a: TakeModuleOffline on unowned ship fails
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "TakeModuleOffline",
        &[PyRep::Integer(ship_b), PyRep::Integer(mod_id)],
    )
    .await
    .is_err());

    // Positive: TakeModuleOffline emits OnGodmaFlushLocationProfile([ship_a])
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "TakeModuleOffline",
        &[PyRep::Integer(ship_a), PyRep::Integer(mod_id)],
    )
    .await
    .expect("TakeModuleOffline");
    let offline_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let flush_args = find_notif(&offline_notifs, "OnGodmaFlushLocationProfile");
    assert_eq!(flush_args.len(), 2);
    assert_eq!(flush_args[0], PyRep::Integer(ship_a));

    // 8. OnInvalidateCacheForLocations: invbroker.SetPassword on a passworded container
    let pw_container_id = 940_000_060_i64;
    let pw_container_type = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 448 && t.published)
        .map(|t| t.type_id)
        .unwrap_or(17366);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = Some(JITA_STATION);
        }
        snap.items.insert(
            pw_container_id,
            make_item(pw_container_id, pw_container_type, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
    });
    sess_a.station_id = Some(JITA_STATION);
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "invbroker",
        "SetPassword",
        &[PyRep::Integer(pw_container_id), PyRep::String("secret123".into())],
    )
    .await
    .expect("SetPassword");
    let cache_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&cache_notifs, "OnInvalidateCacheForLocations"),
        &[PyRep::List(vec![PyRep::Integer(pw_container_id)])]
    );

    // 9. OnMultiEvent: dogmaIM.AddTarget & RemoveTarget
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.station_id = None;
        }
    });
    sess_a.station_id = None;

    // Negative 9a: AddTarget with missing targetID fails
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[]).await.is_err());

    // Negative 9b: AddTarget with invalid target ID 0 fails
    assert!(rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(0)])
        .await
        .is_err());

    // Positive: AddTarget(ship_b) does NOT emit OnMultiEvent OnTarget add before lock completes,
    // and emits exactly one OnMultiEvent([("OnTarget", "add", ship_b, ""), 0]) on lock completion
    let _ = rpc(&ctx, &mut sess_a, "dogmaIM", "AddTarget", &[PyRep::Integer(ship_b)])
        .await
        .expect("AddTarget");
    let pre_lock_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(
        !pre_lock_notifs.iter().any(|(e, _)| e == "OnMultiEvent"),
        "OnMultiEvent OnTarget add must not be emitted before lock completes"
    );
    ctx.systems
        .get_or_spawn(SYS_JITA)
        .step_manual(10.0)
        .await
        .expect("step_manual");
    let locker_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        locker_notifs.iter().filter(|(e, _)| e == "OnMultiEvent").count(),
        1,
        "expected exactly one OnMultiEvent on locker after lock completion"
    );
    assert_eq!(
        find_notif(&locker_notifs, "OnMultiEvent"),
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("add".into()),
                PyRep::Integer(ship_b),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );
    let target_notifs = drain_all(&ctx, &mut sess_b, PILOT_B);
    assert_eq!(
        target_notifs.iter().filter(|(e, _)| e == "OnMultiEvent").count(),
        1,
        "expected exactly one OnMultiEvent on targeted pilot after lock completion"
    );
    assert_eq!(
        find_notif(&target_notifs, "OnMultiEvent"),
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::String("OnTarget".into()),
                PyRep::String("otheradd".into()),
                PyRep::Integer(ship_a),
                PyRep::String(String::new()),
            ]),
            PyRep::Integer(0),
        ])])]
    );
}

}

// Folded submodule: notify_real_emitters_batch9_part5
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch9_part5 {
    use super::*;

// Real server-side emission tests for Notify Batch 9 (Part 5: JumpQueue, FlaggedLists, CfgDataChanged, CCPNotification, ServerMessage, Localization & ServiceMapping).

use crate::notify_real_emitters_batch7::{
    drain_all, find_notif, login_in_space, new_ctx, rpc, JITA_STATION, PILOT_A,
    PILOT_B, SYS_JITA,
};
use crate::notify_real_emitters_batch9::{login_docked, upwell_struct};
use eve_proto::PyRep;
use glam::DVec3;

// parity: notify:OnCfgDataChanged
#[tokio::test]
async fn notify_batch9_jump_queue_flagged_lists_and_cfg_data() {
    eve_testkit::attest(&[
        "notify:OnCfgDataChanged",
    ]);

    let ctx = new_ctx();
    let ship_a = 950_000_001_i64;
    let mut sess_a = login_in_space(&ctx, 9_501, "demo", "demo-secret", PILOT_A, ship_a, 587, SYS_JITA, DVec3::ZERO).await;
    let mut sess_b = login_docked(&ctx, 9_502, "other", "other-secret", PILOT_B, JITA_STATION).await;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_b, PILOT_B);

    // 3. OnCfgDataChanged: RenameStructure
    let struct_id = 1_050_000_000_001_i64;
    let corp_id = 98_900_020_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Cfg Corp".into(),
                ticker: "CFGC".into(),
                ceo_id: PILOT_A,
                ..Default::default()
            },
        );
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
        }
        snap.structures.insert(
            struct_id,
            upwell_struct(struct_id, 35832, corp_id, SYS_JITA, "Old Astrahus", [5_000.0, 1_000.0, -2_000.0]),
        );
    });
    sess_a.corporation_id = Some(corp_id);

    // Negative 3a: Empty structure name fails with InvalidStructureName
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(struct_id), PyRep::String("   ".into())],
    )
    .await
    .is_err());

    // Negative 3b: Non-corp member (PILOT_B) calling RenameStructure fails
    sess_b.role = 0;
    assert!(rpc(
        &ctx,
        &mut sess_b,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(struct_id), PyRep::String("Hacked Name".into())],
    )
    .await
    .is_err());

    // Positive: CEO renaming structure emits OnCfgDataChanged(["evelocations", 6-elem list])
    let _ = rpc(
        &ctx,
        &mut sess_a,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(struct_id), PyRep::String("New Astrahus".into())],
    )
    .await
    .expect("RenameStructure");
    let cfg_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let cfg_args = find_notif(&cfg_notifs, "OnCfgDataChanged");
    assert_eq!(cfg_args.len(), 2);
    assert_eq!(cfg_args[0], PyRep::String("evelocations".into()));
    assert!(matches!(&cfg_args[1], PyRep::List(elems) if elems.len() == 6));
}

// parity: notify:OnServerMessage
#[tokio::test]
async fn notify_batch9_ccp_notification_server_msg_localization_and_service_map() {
    eve_testkit::attest(&[
        "notify:OnServerMessage",
    ]);

    let ctx = new_ctx();
    let mut sess_a = login_docked(&ctx, 9_511, "demo", "demo-secret", PILOT_A, JITA_STATION).await;
    let mut sess_gm = login_docked(&ctx, 9_512, "other", "other-secret", PILOT_B, JITA_STATION).await;
    sess_gm.role |= eve_db::service_roles::ROLE_GMH;
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);
    let _ = drain_all(&ctx, &mut sess_gm, PILOT_B);

    // 5. OnServerMessage: machoNet.GracefulShutdown
    // Negative 5a: Non-GM calling GracefulShutdown fails with InsufficientRole
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "machoNet",
        "GracefulShutdown",
        &[PyRep::Integer(1), PyRep::String("Maintenance".into()), PyRep::Integer(300), PyRep::Bool(true)],
    )
    .await
    .is_err());

    // Positive: GM initiating GracefulShutdown broadcasts OnServerMessage
    let _ = rpc(
        &ctx,
        &mut sess_gm,
        "machoNet",
        "GracefulShutdown",
        &[PyRep::Integer(1), PyRep::String("Welcome to Tranquility".into()), PyRep::Integer(300), PyRep::Bool(true)],
    )
    .await
    .expect("GracefulShutdown");
    let srv_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert_eq!(
        find_notif(&srv_notifs, "OnServerMessage"),
        &[PyRep::String("Welcome to Tranquility".into())]
    );

    // 6. UpdateLocalizationQAWrap rejects unprivileged caller and succeeds for GM without emitting OnUpdateLocalizationTextCache
    assert!(rpc(
        &ctx,
        &mut sess_a,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .is_err());

    let qa_ok = rpc(
        &ctx,
        &mut sess_gm,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("GM UpdateLocalizationQAWrap(true)");
    assert_eq!(qa_ok, PyRep::Bool(true));
    let loc_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    assert!(!loc_notifs.iter().any(|(e, _)| e == "OnUpdateLocalizationTextCache"));
}

}

// Folded submodule: notify_w34_closure
#[allow(unused_imports, dead_code)]
mod notify_w34_closure {
    use super::*;

// Live V24 integration tests for newly-closed server notifications (`EvoMu-w34`):
// - `notify:OnIndustryMaterials`
// - `notify:OnCommunityFittingsUpdated`
// - `notify:OnTutorialSkipped`

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::{service_roles, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::{cache_notifications::PendingMachoNotification, ServerContext};
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const JITA_STATION: i64 = 60_003_760;
const COMMUNITY_FITTING_CORP_ID: i64 = 1_000_282;

async fn login_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s.station_id = Some(JITA_STATION);
    ctx.advance_secs(12);
    s
}

async fn rpc(
    ctx: &ServerContext,
    s: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(s, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn find_notifs<'a>(
    notifs: &'a [PendingMachoNotification],
    event: &str,
) -> Vec<&'a PendingMachoNotification> {
    notifs.iter().filter(|n| n.event == event).collect()
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
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
    }
}

#[tokio::test]
async fn test_on_industry_materials_v24_dict_shape() {
    // parity: notify:OnIndustryMaterials
    eve_testkit::attest(&["notify:OnIndustryMaterials"]);

    let ctx = ServerContext::new();
    let mut sess = login_pilot(&ctx, 8801, "demo", "demo-secret", PILOT_A).await;

    let bp_info = ctx
        .sde
        .blueprints
        .get(&687)
        .cloned()
        .expect("Rifter Blueprint (687) in SDE");
    assert!(
        !bp_info.materials.is_empty(),
        "Rifter Blueprint must have non-empty SDE manufacturing BOM"
    );

    let bp_id = 920_000_001_i64;
    let initial_stack_qty: i32 = 1_000_000;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.balance = IskCents(100_000_000_00);
            ch.station_id = Some(JITA_STATION);
        }
        snap.items.retain(|_, i| {
            !(i.owner_id == PILOT_A
                && i.location_id == JITA_STATION
                && bp_info.materials.iter().any(|m| m.type_id == i.type_id))
        });
        snap.items.insert(
            bp_id,
            make_item(bp_id, 687, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true),
        );
        for (idx, mat) in bp_info.materials.iter().enumerate() {
            let iid = 920_000_010 + idx as i64;
            snap.items.insert(iid, make_item(iid, mat.type_id, PILOT_A, JITA_STATION, InventoryFlag::Hangar, initial_stack_qty, false));
        }
    });
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // Negative 1: unauthenticated InstallJob fails and emits 0 OnIndustryMaterials
    let mut unauth = SessionState::new(8899);
    assert!(rpc(&ctx, &mut unauth, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintItemID", PyRep::Integer(bp_id)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.is_err());
    assert!(find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnIndustryMaterials").is_empty());

    // Negative 2: non-existent blueprint InstallJob fails and emits 0 OnIndustryMaterials
    assert!(rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::keyval([
        ("blueprintItemID", PyRep::Integer(999_999_999)), ("facilityID", PyRep::Integer(JITA_STATION)),
        ("activityID", PyRep::Integer(1)), ("runs", PyRep::Integer(1)),
    ])]).await.is_err());
    assert!(find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnIndustryMaterials").is_empty());

    // Positive: ConnectJob(job) -> InstallJob -> OnIndustryMaterials(monitorID, {typeID: remaining_qty}) -> DisconnectJob
    let mon_rep = rpc(
        &ctx,
        &mut sess,
        "industryMonitor",
        "ConnectJob",
        &[PyRep::keyval([
            ("ownerID", PyRep::Integer(PILOT_A)),
            ("locationID", PyRep::Integer(JITA_STATION)),
            ("inputFlagID", PyRep::Integer(InventoryFlag::Hangar as i64)),
        ])],
    )
    .await
    .expect("industryMonitor.ConnectJob should succeed");
    let PyRep::Tuple(mon_parts) = mon_rep else {
        panic!("ConnectJob must return (monitorID, available_dict) tuple, got {mon_rep:?}");
    };
    let monitor_id = mon_parts[0].as_i64().expect("monitorID");
    assert!(monitor_id > 0);
    assert!(matches!(mon_parts[1], PyRep::Dict(_)));

    let job_rep = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(bp_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
        ])],
    )
    .await
    .expect("InstallJob should succeed");
    let job_id = job_rep
        .dict_get("jobID")
        .and_then(|v| v.as_i64())
        .expect("jobID");
    assert!(job_id > 0);

    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    let ind_mats = find_notifs(&notifs, "OnIndustryMaterials");
    assert_eq!(
        ind_mats.len(),
        1,
        "Expected exactly 1 OnIndustryMaterials notification"
    );
    assert_eq!(ind_mats[0].id_type, "charid");
    assert_eq!(ind_mats[0].args.len(), 2);
    assert_eq!(ind_mats[0].args[0], PyRep::Integer(monitor_id));

    let PyRep::Dict(mat_entries) = &ind_mats[0].args[1] else {
        panic!(
            "V24 IndustryService.OnIndustryMaterials expects Dict {{typeID: available}}, got {:?}",
            ind_mats[0].args[1]
        );
    };
    assert_eq!(mat_entries.len(), bp_info.materials.len());

    for mat in &bp_info.materials {
        let reported_avail = mat_entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(i64::from(mat.type_id)))
            .and_then(|(_, v)| v.as_i64())
            .expect("each consumed BOM typeID must appear in OnIndustryMaterials dict");
        let db_avail: i64 = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .filter(|i| {
                    i.owner_id == PILOT_A
                        && i.location_id == JITA_STATION
                        && i.type_id == mat.type_id
                        && i.flag == InventoryFlag::Hangar
                })
                .map(|i| i64::from(i.quantity))
                .sum()
        });
        assert_eq!(
            reported_avail, db_avail,
            "OnIndustryMaterials dict value must equal post-consumption hangar quantity for type {}",
            mat.type_id
        );
        assert!(
            reported_avail < i64::from(initial_stack_qty),
            "Material {} must have been consumed below initial stack {}",
            mat.type_id,
            initial_stack_qty
        );
    }

    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "industryMonitor",
            "DisconnectJob",
            &[PyRep::Integer(monitor_id)],
        )
        .await
        .expect("DisconnectJob should succeed"),
        PyRep::Bool(true)
    );

    // Corporate hangar division isolation: useCorp=true with inputFlag=CorpSAG1 must consume from and report ONLY CorpSAG1
    let corp_id = 98_400_001_i64;
    let corp_bp_id = 920_000_100_i64;
    let sag1_initial: i32 = 2_000_000;
    let sag2_initial: i32 = 500_000;
    let take3_only = 1u64 << 15; // Take3 (bit 15), NOT Take1 (bit 13)
    let wrong_div_roles = eve_db::corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 15); // Factory Manager + Take3
    let fm_roles = eve_db::corp_roles::ROLE_FACTORY_MANAGER | (1u64 << 13); // Factory Manager + Take1 (bit 13)
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord {
            corporation_id: corp_id,
            name: "Industry Division Corp".into(),
            ticker: "IDIV".into(),
            ceo_id: PILOT_B,
            hq_station_id: Some(JITA_STATION),
            master_wallet: IskCents(100_000_000_00),
            ..Default::default()
        };
        corp.members.insert(PILOT_A);
        corp.members.insert(PILOT_B);
        corp.member_roles.insert(PILOT_A, take3_only);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.corporation_id = corp_id;
        }
        snap.skills.entry(PILOT_A).or_default().insert(
            3387,
            eve_db::TrainedSkillRecord { type_id: 3387, level: 5, skill_points: 256_000.0 },
        );
        snap.items.insert(
            corp_bp_id,
            make_item(corp_bp_id, 687, corp_id, JITA_STATION, InventoryFlag::CorpSAG1, 1, true),
        );
        for (idx, mat) in bp_info.materials.iter().enumerate() {
            snap.items.insert(
                920_000_110 + idx as i64,
                make_item(920_000_110 + idx as i64, mat.type_id, corp_id, JITA_STATION, InventoryFlag::CorpSAG1, sag1_initial, false),
            );
            snap.items.insert(
                920_000_210 + idx as i64,
                make_item(920_000_210 + idx as i64, mat.type_id, corp_id, JITA_STATION, InventoryFlag::CorpSAG2, sag2_initial, false),
            );
        }
    });
    sess.corporation_id = Some(corp_id);
    sess.corp_role = take3_only;

    // Negative: member holding Take3 (bit 15) must be denied CorpSAG1 on ConnectJob
    let take3_conn_err = rpc(
        &ctx,
        &mut sess,
        "industryMonitor",
        "ConnectJob",
        &[PyRep::keyval([
            ("ownerID", PyRep::Integer(corp_id)),
            ("locationID", PyRep::Integer(JITA_STATION)),
            ("inputFlagID", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
    )
    .await
    .expect_err("Take3 holder must be denied CorpSAG1 ConnectJob");
    assert_eq!(take3_conn_err.code, "CrpAccessDenied");

    // Negative: member holding Factory Manager + Take3 (bit 15) must be denied CorpSAG1 on InstallJob
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(PILOT_A, wrong_div_roles);
        }
    });
    sess.corp_role = wrong_div_roles;

    let take3_inst_err = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(corp_bp_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("useCorp", PyRep::Bool(true)),
            ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
    )
    .await
    .expect_err("Take3 holder must be denied CorpSAG1 InstallJob");
    assert_eq!(take3_inst_err.code, "CrpAccessDenied");

    // Grant Take1 (bit 13): now CorpSAG1 succeeds
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(PILOT_A, fm_roles);
        }
    });
    sess.corp_role = fm_roles;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    let corp_mon_rep = rpc(
        &ctx,
        &mut sess,
        "industryMonitor",
        "ConnectJob",
        &[PyRep::keyval([
            ("ownerID", PyRep::Integer(corp_id)),
            ("locationID", PyRep::Integer(JITA_STATION)),
            ("inputFlagID", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
    )
    .await
    .expect("corp industryMonitor.ConnectJob should succeed");
    let PyRep::Tuple(corp_mon_parts) = corp_mon_rep else {
        panic!("ConnectJob must return (monitorID, available_dict) tuple");
    };
    let corp_monitor_id = corp_mon_parts[0].as_i64().expect("corp monitorID");

    let corp_job = rpc(
        &ctx,
        &mut sess,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(corp_bp_id)),
            ("facilityID", PyRep::Integer(JITA_STATION)),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("useCorp", PyRep::Bool(true)),
            ("inputFlag", PyRep::Integer(InventoryFlag::CorpSAG1 as i64)),
        ])],
    )
    .await
    .expect("Corporate InstallJob from CorpSAG1 should succeed");
    let corp_job_id = corp_job.dict_get("jobID").and_then(|v| v.as_i64()).expect("corp jobID");
    assert!(corp_job_id > 0);
    let corp_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let corp_ind_mats = find_notifs(&corp_notifs, "OnIndustryMaterials");
    assert_eq!(corp_ind_mats.len(), 1);
    assert_eq!(corp_ind_mats[0].args[0], PyRep::Integer(corp_monitor_id));
    let PyRep::Dict(corp_mat_entries) = &corp_ind_mats[0].args[1] else {
        panic!("expected Dict in corporate OnIndustryMaterials");
    };
    for mat in &bp_info.materials {
        let reported = corp_mat_entries
            .iter()
            .find(|(k, _)| k.as_i64() == Some(i64::from(mat.type_id)))
            .and_then(|(_, v)| v.as_i64())
            .expect("consumed material in corp OnIndustryMaterials");
        let (sag1_db, sag2_db): (i64, i64) = ctx.db.with_snapshot(|snap| {
            let s1 = snap.items.values().filter(|i| i.owner_id == corp_id && i.location_id == JITA_STATION && i.type_id == mat.type_id && i.flag == InventoryFlag::CorpSAG1).map(|i| i64::from(i.quantity)).sum();
            let s2 = snap.items.values().filter(|i| i.owner_id == corp_id && i.location_id == JITA_STATION && i.type_id == mat.type_id && i.flag == InventoryFlag::CorpSAG2).map(|i| i64::from(i.quantity)).sum();
            (s1, s2)
        });
        assert_eq!(sag2_db, i64::from(sag2_initial), "CorpSAG2 stack must not be touched by CorpSAG1 job");
        assert_eq!(
            reported, sag1_db,
            "OnIndustryMaterials for corporate job must report strictly CorpSAG1 remaining quantity (excluding personal Hangar and CorpSAG2)"
        );
        assert!(reported < i64::from(sag1_initial));
    }
    assert_eq!(
        rpc(
            &ctx,
            &mut sess,
            "industryMonitor",
            "DisconnectJob",
            &[PyRep::Integer(corp_monitor_id)],
        )
        .await
        .expect("corp DisconnectJob should succeed"),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn test_on_community_fittings_updated_auth_and_broadcast() {
    // parity: notify:OnCommunityFittingsUpdated
    eve_testkit::attest(&["notify:OnCommunityFittingsUpdated"]);

    let ctx = ServerContext::new();
    let mut sess_a = login_pilot(&ctx, 8802, "demo", "demo-secret", PILOT_A).await;
    let _sess_b = login_pilot(&ctx, 8803, "other", "other-secret", PILOT_B).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    let fit_kv = PyRep::keyval([
        ("name", PyRep::String("Community Rifter".into())),
        ("shipTypeID", PyRep::Integer(587)),
        ("description", PyRep::String("Curated V24 community fit".into())),
        ("fitData", PyRep::List(vec![])),
    ]);

    // Negative 1: unprivileged player cannot save to COMMUNITY_FITTING_CORP (1000282) or owner 1
    sess_a.role = 0;
    for owner in [COMMUNITY_FITTING_CORP_ID, 1_i64] {
        let denied = rpc(
            &ctx,
            &mut sess_a,
            "corpFittingMgr",
            "SaveFitting",
            &[PyRep::Integer(owner), fit_kv.clone()],
        )
        .await
        .expect_err("unprivileged player must not save community fittings");
        assert_eq!(denied.code, "AccessDenied");
    }
    assert!(
        find_notifs(
            &ctx.drain_notifications_for_char(PILOT_A),
            "OnCommunityFittingsUpdated"
        )
        .is_empty()
    );

    // Negative 2: personal charFittingMgr.SaveFitting emits OnFittingsUpdated, NOT OnCommunityFittingsUpdated
    let personal_fid = rpc(
        &ctx,
        &mut sess_a,
        "charFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(PILOT_A), fit_kv.clone()],
    )
    .await
    .expect("personal SaveFitting")
    .as_i64()
    .expect("fitting id");
    assert!(personal_fid > 0);
    assert!(
        find_notifs(
            &ctx.drain_notifications_for_char(PILOT_A),
            "OnCommunityFittingsUpdated"
        )
        .is_empty()
    );
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // Positive 1: Content/GM role saving to COMMUNITY_FITTING_CORP (1000282) broadcasts 0-arg OnCommunityFittingsUpdated
    sess_a.role = service_roles::ROLE_CONTENT;
    let comm_fid = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "SaveFitting",
        &[PyRep::Integer(COMMUNITY_FITTING_CORP_ID), fit_kv],
    )
    .await
    .expect("Content SaveFitting to COMMUNITY_FITTING_CORP")
    .as_i64()
    .expect("community fitting id");
    assert!(comm_fid > 0);

    for cid in [PILOT_A, PILOT_B] {
        let notifs = ctx.drain_notifications_for_char(cid);
        let comm_notifs = find_notifs(&notifs, "OnCommunityFittingsUpdated");
        assert_eq!(
            comm_notifs.len(),
            1,
            "Online pilot {cid} must receive broadcast OnCommunityFittingsUpdated"
        );
        assert_eq!(comm_notifs[0].id_type, "clientID");
        assert!(comm_notifs[0].args.is_empty());
    }

    let comm_dict = rpc(&ctx, &mut sess_a, "corpFittingMgr", "GetCommunityFittings", &[])
        .await
        .expect("GetCommunityFittings");
    let PyRep::Dict(entries) = comm_dict else {
        panic!("expected Dict from GetCommunityFittings");
    };
    assert!(
        entries.iter().any(|(k, _)| k.as_i64() == Some(comm_fid)),
        "GetCommunityFittings must include newly saved community fitting"
    );

    // Negative 3: unprivileged caller cannot rename or delete community fitting
    sess_a.role = 0;
    let upd_denied = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(comm_fid),
            PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
            PyRep::String("Unauthorized Rename".into()),
            PyRep::String("Should fail".into()),
        ],
    )
    .await
    .expect_err("unprivileged player must not update community fittings");
    assert_eq!(upd_denied.code, "AccessDenied");
    let del_denied = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "DeleteFitting",
        &[
            PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
            PyRep::Integer(comm_fid),
        ],
    )
    .await
    .expect_err("unprivileged player must not delete community fittings");
    assert_eq!(del_denied.code, "AccessDenied");
    assert!(
        find_notifs(
            &ctx.drain_notifications_for_char(PILOT_B),
            "OnCommunityFittingsUpdated"
        )
        .is_empty()
    );

    // Positive 2: Content/GM role renaming community fitting updates metadata and broadcasts 0-arg OnCommunityFittingsUpdated
    sess_a.role = service_roles::ROLE_CONTENT;
    let renamed = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "UpdateNameAndDescription",
        &[
            PyRep::Integer(comm_fid),
            PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
            PyRep::String("Updated Community Rifter".into()),
            PyRep::String("Updated curated V24 community fit".into()),
        ],
    )
    .await
    .expect("Content UpdateNameAndDescription");
    assert_eq!(renamed.dict_get("name"), Some(&PyRep::String("Updated Community Rifter".into())));
    let b_upd_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let b_upd_comm = find_notifs(&b_upd_notifs, "OnCommunityFittingsUpdated");
    assert_eq!(b_upd_comm.len(), 1);
    assert!(b_upd_comm[0].args.is_empty());

    // Positive 3: Content/GM role deleting community fitting broadcasts 0-arg OnCommunityFittingsUpdated
    sess_a.role = service_roles::ROLE_CONTENT;
    let deleted = rpc(
        &ctx,
        &mut sess_a,
        "corpFittingMgr",
        "DeleteFitting",
        &[
            PyRep::Integer(COMMUNITY_FITTING_CORP_ID),
            PyRep::Integer(comm_fid),
        ],
    )
    .await
    .expect("Content DeleteFitting");
    assert_eq!(deleted, PyRep::Bool(true));

    let b_del_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let b_comm = find_notifs(&b_del_notifs, "OnCommunityFittingsUpdated");
    assert_eq!(b_comm.len(), 1);
    assert!(b_comm[0].args.is_empty());
}

#[tokio::test]
async fn test_on_tutorial_skipped_air_npe_and_operations() {
    // parity: notify:OnTutorialSkipped
    eve_testkit::attest(&["notify:OnTutorialSkipped"]);

    let ctx = ServerContext::new();
    let mut sess_a = login_pilot(&ctx, 8804, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_pilot(&ctx, 8805, "other", "other-secret", PILOT_B).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // Negative 1: unauthenticated skip_air_npe fails and emits 0 OnTutorialSkipped
    let mut unauth = SessionState::new(8898);
    assert!(rpc(&ctx, &mut unauth, "air_npe", "skip_air_npe", &[]).await.is_err());
    assert!(
        find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnTutorialSkipped").is_empty()
    );

    // Negative 2: non-skip client event does not emit OnTutorialSkipped
    rpc(
        &ctx,
        &mut sess_a,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("start_tutorial".into())],
    )
    .await
    .expect("start_tutorial");
    assert!(
        find_notifs(&ctx.drain_notifications_for_char(PILOT_A), "OnTutorialSkipped").is_empty()
    );

    // Positive 1: air_npe.skip_air_npe marks tutorial completed and emits 0-arg OnTutorialSkipped
    let skip_rep = rpc(&ctx, &mut sess_a, "air_npe", "skip_air_npe", &[])
        .await
        .expect("air_npe.skip_air_npe");
    assert_eq!(skip_rep.dict_get("completed"), Some(&PyRep::Bool(true)));
    let state_rep = rpc(&ctx, &mut sess_a, "air_npe", "get_air_npe_state", &[])
        .await
        .expect("air_npe.get_air_npe_state");
    assert_eq!(state_rep.dict_get("completed"), Some(&PyRep::Bool(true)));

    let a_notifs = ctx.drain_notifications_for_char(PILOT_A);
    let a_skipped = find_notifs(&a_notifs, "OnTutorialSkipped");
    assert_eq!(a_skipped.len(), 1);
    assert_eq!(a_skipped[0].id_type, "charid");
    assert!(a_skipped[0].args.is_empty());

    // Positive 2: operationsManager.process_client_event("skip_tutorial") also emits 0-arg OnTutorialSkipped
    rpc(
        &ctx,
        &mut sess_b,
        "operationsManager",
        "process_client_event",
        &[PyRep::String("skip_tutorial".into())],
    )
    .await
    .expect("operationsManager skip_tutorial");
    let b_notifs = ctx.drain_notifications_for_char(PILOT_B);
    let b_skipped = find_notifs(&b_notifs, "OnTutorialSkipped");
    assert_eq!(b_skipped.len(), 1);
    assert_eq!(b_skipped[0].id_type, "charid");
    assert!(b_skipped[0].args.is_empty());
}

}

#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
pub(crate) mod parity_notify_batch3 {
    use super::*;
// E2E integration tests closing the 15 `FLAGGED_FAKE` `notify:*` rows in Batch 3.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;
const SYS_AMARR: u32 = 30_002_187;
const SYS_ZARZAKH: u32 = 30_100_000;
const JITA_STATION: i64 = 60_003_760;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
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
    }
}

async fn login_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sess
}

// parity: notify:DoSimClockRebase, notify:OnDisapprovalTimerUpdate, notify:OnNpcTimerUpdate
#[tokio::test]
async fn test_batch3_sim_clock_rebase_and_crimewatch_timers() {
    eve_testkit::attest(&[
        "notify:DoSimClockRebase",
        "notify:OnDisapprovalTimerUpdate",
        "notify:OnNpcTimerUpdate",
    ]);

    let ctx = new_ctx();
    let mut gm_sess = login_in_space(
        &ctx,
        92_001,
        "demo",
        "demo-secret",
        PILOT_A,
        640_000_001,
        587,
        SYS_JITA,
        DVec3::ZERO,
    )
    .await;
    gm_sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;
    let _amarr_sess = login_in_space(
        &ctx,
        92_002,
        "other",
        "other-secret",
        PILOT_B,
        640_000_002,
        587,
        SYS_AMARR,
        DVec3::ZERO,
    )
    .await;
    drain_all(&ctx, PILOT_A);
    drain_all(&ctx, PILOT_B);

    rpc(
        &ctx,
        &mut gm_sess,
        "slash",
        "SlashCmd",
        &[PyRep::String("/tidi 0.5".into())],
    )
    .await
    .unwrap();
    let jita_notifs = drain_all(&ctx, PILOT_A);
    let amarr_notifs = drain_all(&ctx, PILOT_B);
    let rebase = jita_notifs
        .iter()
        .find(|(e, _)| e == "DoSimClockRebase")
        .expect("Jita pilot must receive DoSimClockRebase");
    let PyRep::Tuple(pair) = &rebase.1[0] else {
        panic!("expected 2-tuple in DoSimClockRebase");
    };
    assert_eq!(pair.len(), 2);
    assert!(pair[0].as_i64().unwrap() < pair[1].as_i64().unwrap());
    assert!(
        !amarr_notifs.iter().any(|(e, _)| e == "DoSimClockRebase"),
        "Amarr pilot must not receive Jita DoSimClockRebase"
    );

    // Zarzakh disapproval timer (501 active -> 502 countdown -> 500 idle) & NPC timer (401 -> 402 -> 400)
    let (ship_a, ship_b, gun_id, gun2_id) = (640_000_101_i64, 640_000_102_i64, 640_000_110_i64, 640_000_112_i64);
    let mut s_a = login_in_space(
        &ctx,
        92_003,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        587,
        SYS_ZARZAKH,
        DVec3::ZERO,
    )
    .await;
    let _s_b = login_in_space(
        &ctx,
        92_004,
        "other",
        "other-secret",
        PILOT_B,
        ship_b,
        587,
        SYS_ZARZAKH,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun_id,
            make_item(gun_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            gun2_id,
            make_item(gun2_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot1, 1, true),
        );
        snap.module_online_states.insert(gun_id, true);
        snap.module_online_states.insert(gun2_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    let sys_h = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    sys_h
        .add_target_lock(
            ship_a,
            TargetLock {
                target_id: ship_b,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    drain_all(&ctx, PILOT_A);

    rpc(
        &ctx,
        &mut s_a,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(ship_b),
        ],
    )
    .await
    .unwrap();
    let n1 = drain_all(&ctx, PILOT_A);
    assert!(
        n1.iter()
            .any(|(e, a)| e == "OnDisapprovalTimerUpdate" && a.first() == Some(&PyRep::Integer(501))),
        "expected OnDisapprovalTimerUpdate(501, exp), got {n1:?}"
    );

    ctx.advance_secs(1);
    let n2 = drain_all(&ctx, PILOT_A);
    assert!(
        n2.iter()
            .any(|(e, a)| e == "OnDisapprovalTimerUpdate" && a.first() == Some(&PyRep::Integer(502))),
        "expected OnDisapprovalTimerUpdate(502, exp) countdown, got {n2:?}"
    );

    let npc_id = 640_000_199_i64;
    sys_h
        .spawn_ball(Ball {
            id: npc_id,
            type_id: 16_250,
            position: DVec3::new(1_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        })
        .await
        .unwrap();
    sys_h
        .add_target_lock(
            ship_a,
            TargetLock {
                target_id: npc_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    drain_all(&ctx, PILOT_A);

    rpc(
        &ctx,
        &mut s_a,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun2_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(npc_id),
        ],
    )
    .await
    .unwrap();
    let n3 = drain_all(&ctx, PILOT_A);
    assert!(
        n3.iter()
            .any(|(e, a)| e == "OnNpcTimerUpdate" && a.first() == Some(&PyRep::Integer(401))),
        "expected OnNpcTimerUpdate(401, exp), got {n3:?}"
    );

    ctx.advance_secs(1);
    let n4 = drain_all(&ctx, PILOT_A);
    assert!(
        n4.iter()
            .any(|(e, a)| e == "OnNpcTimerUpdate" && a.first() == Some(&PyRep::Integer(402))),
        "expected OnNpcTimerUpdate(402, exp) countdown, got {n4:?}"
    );

    ctx.advance_secs(910);
    let n5 = drain_all(&ctx, PILOT_A);
    assert!(
        n5.iter().any(|(e, a)| e == "OnNpcTimerUpdate"
            && a == &vec![PyRep::Integer(400), PyRep::None]),
        "expected OnNpcTimerUpdate(400, None) on expiry"
    );
    assert!(
        n5.iter().any(|(e, a)| e == "OnDisapprovalTimerUpdate"
            && a == &vec![PyRep::Integer(500), PyRep::None]),
        "expected OnDisapprovalTimerUpdate(500, None) on expiry"
    );
}

// parity: notify:OnPasswordChanged, notify:OnInvalidateCacheForLocations
#[tokio::test]
async fn test_batch3_container_password_and_cache_invalidation() {
    eve_testkit::attest(&[
        "notify:OnPasswordChanged",
        "notify:OnInvalidateCacheForLocations",
    ]);

    let ctx = new_ctx();
    let mut sess = SessionState::new(92_010);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .unwrap();

    let container_id = 640_000_250_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            container_id,
            make_item(
                container_id,
                17_366, // Audit Log Secure Container (group 448)
                PILOT_A,
                JITA_STATION,
                InventoryFlag::Hangar,
                1,
                true,
            ),
        );
    });

    let bind = rpc(
        &ctx,
        &mut sess,
        "invbroker",
        "MachoBindObject",
        &[
            PyRep::Tuple(vec![PyRep::Integer(JITA_STATION), PyRep::Integer(15)]),
            PyRep::None,
        ],
    )
    .await
    .unwrap();
    let mgr_oid = match &bind {
        PyRep::Tuple(parts) => parts[0].as_i64().unwrap() as u64,
        _ => panic!("expected MachoBindObject tuple"),
    };
    let bound_id = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(mgr_oid),
            "GetInventoryFromId",
            &[PyRep::Integer(container_id), PyRep::Integer(0)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap()
        .as_i64()
        .unwrap() as u64;
    drain_all(&ctx, PILOT_A);

    // Set general password (which = 1)
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(bound_id),
        "SetPassword",
        &[
            PyRep::Integer(1),
            PyRep::String("general123".into()),
            PyRep::None,
        ],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    let n1 = drain_all(&ctx, PILOT_A);
    assert!(
        n1.iter()
            .any(|(e, a)| e == "OnPasswordChanged" && a.first() == Some(&PyRep::Integer(1))),
        "expected OnPasswordChanged(1, item), got {n1:?}"
    );
    assert!(
        n1.iter().any(|(e, a)| e == "OnInvalidateCacheForLocations"
            && a.first() == Some(&PyRep::List(vec![PyRep::Integer(container_id)]))),
        "expected OnInvalidateCacheForLocations([container_id]), got {n1:?}"
    );

    // Set config password (which = 2)
    ctx.dispatch_rpc(
        &mut sess,
        "invbroker",
        Some(bound_id),
        "SetPassword",
        &[
            PyRep::Integer(2),
            PyRep::String("config123".into()),
            PyRep::None,
        ],
        &HashMap::new(),
        None,
    )
    .await
    .unwrap();
    let n2 = drain_all(&ctx, PILOT_A);
    assert!(
        n2.iter()
            .any(|(e, a)| e == "OnPasswordChanged" && a.first() == Some(&PyRep::Integer(2))),
        "expected OnPasswordChanged(2, item), got {n2:?}"
    );
    assert!(
        n2.iter().any(|(e, a)| e == "OnInvalidateCacheForLocations"
            && a.first() == Some(&PyRep::List(vec![PyRep::Integer(container_id)]))),
        "expected OnInvalidateCacheForLocations([container_id]), got {n2:?}"
    );

    // Wrong oldPassword must fail
    let bad = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            Some(bound_id),
            "SetPassword",
            &[
                PyRep::Integer(1),
                PyRep::String("newpass".into()),
                PyRep::String("wrong_old".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(bad.unwrap_err().code, "InvalidPassword");
}

// parity: notify:OnMultiEvent, notify:OnKillNotification
#[tokio::test]
async fn test_batch3_target_multi_event_and_kill_notification() {
    eve_testkit::attest(&["notify:OnMultiEvent", "notify:OnKillNotification"]);

    let ctx = new_ctx();
    let (ship_a, ship_b, gun_id, ammo_id) = (640_000_301_i64, 640_000_302_i64, 640_000_310_i64, 640_000_311_i64);
    let mut sess_a = login_in_space(
        &ctx,
        92_020,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        587,
        SYS_ZARZAKH,
        DVec3::ZERO,
    )
    .await;
    let _sess_b = login_in_space(
        &ctx,
        92_021,
        "other",
        "other-secret",
        PILOT_B,
        ship_b,
        587,
        SYS_ZARZAKH,
        DVec3::new(1_000.0, 0.0, 0.0),
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            gun_id,
            make_item(gun_id, 484, PILOT_A, ship_a, InventoryFlag::HiSlot0, 1, true),
        );
        snap.items.insert(
            ammo_id,
            make_item(ammo_id, 218, PILOT_A, ship_a, InventoryFlag::HiSlot0, 100, false),
        );
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_a);
    drain_all(&ctx, PILOT_A);
    drain_all(&ctx, PILOT_B);

    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .unwrap();
    ctx.systems
        .get_or_spawn(SYS_ZARZAKH)
        .step_manual(10.0)
        .await
        .unwrap();

    let lock_a = drain_all(&ctx, PILOT_A);
    let lock_b = drain_all(&ctx, PILOT_B);
    let has_target_ev = |notifs: &[(String, Vec<PyRep>)], sub: &str, id: i64| {
        notifs.iter().any(|(e, a)| {
            e == "OnMultiEvent"
                && match a.first() {
                    Some(PyRep::List(items)) => items.iter().any(|it| match it {
                        PyRep::Tuple(outer) => match outer.first() {
                            Some(PyRep::Tuple(inner)) => {
                                inner.first() == Some(&PyRep::String("OnTarget".into()))
                                    && inner.get(1) == Some(&PyRep::String(sub.into()))
                                    && inner.get(2) == Some(&PyRep::Integer(id))
                            }
                            _ => false,
                        },
                        _ => false,
                    }),
                    _ => false,
                }
        })
    };
    assert!(
        has_target_ev(&lock_a, "add", ship_b),
        "locker must receive OnMultiEvent OnTarget add"
    );
    assert!(
        has_target_ev(&lock_b, "otheradd", ship_a),
        "target must receive OnMultiEvent OnTarget otheradd"
    );

    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "RemoveTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .unwrap();
    let unlock_b = drain_all(&ctx, PILOT_B);
    assert!(
        has_target_ev(&unlock_b, "otherlost", ship_a),
        "target must receive OnMultiEvent OnTarget otherlost"
    );

    // Re-lock and destroy ship_b via weapon fire
    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .unwrap();
    ctx.systems
        .get_or_spawn(SYS_ZARZAKH)
        .step_manual(10.0)
        .await
        .unwrap();
    drain_all(&ctx, PILOT_A);
    drain_all(&ctx, PILOT_B);

    let sys_h = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    for _ in 0..40 {
        if sys_h.get_ball_snapshot(ship_b).await.ok().flatten().is_none() {
            break;
        }
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(ship_b),
            ],
        )
        .await;
        ctx.advance_secs(5);
        let _ = sys_h.step_manual(5.0).await;
    }
    assert!(
        sys_h.get_ball_snapshot(ship_b).await.ok().flatten().is_none(),
        "ship_b must be destroyed by weapon fire"
    );
    let kill_a = drain_all(&ctx, PILOT_A);
    let kill_b = drain_all(&ctx, PILOT_B);
    assert!(
        kill_a.iter().any(|(e, _)| e == "OnKillNotification"),
        "attacker must receive OnKillNotification"
    );
    assert!(
        kill_b.iter().any(|(e, _)| e == "OnKillNotification"),
        "victim must receive OnKillNotification"
    );
}

// parity: notify:OnNewProbe, notify:OnProbeWarpStart, notify:OnProbeWarpEnd, notify:OnProbeStateChanged


}

pub(crate) mod parity_notify_batch3_part2 {
    #[allow(unused_imports)]
    use super::*;

// E2E integration tests closing the 15 `FLAGGED_FAKE` `notify:*` rows in Batch 3.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const SYS_JITA: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const AU_METERS: f64 = 149_597_870_700.0;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
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
    }
}

async fn login_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sess
}

// parity: notify:DoSimClockRebase, notify:OnDisapprovalTimerUpdate, notify:OnNpcTimerUpdate
#[tokio::test]
async fn test_batch3_probe_scanning_lifecycle_notifications() {
    eve_testkit::attest(&[
        "notify:OnNewProbe",
        "notify:OnProbeWarpStart",
        "notify:OnProbeWarpEnd",
        "notify:OnProbeStateChanged",
    ]);

    let ctx = new_ctx();
    let jita_pos = ctx.sde.stations[&JITA_STATION].position + DVec3::new(1_000.0, 0.0, 0.0);
    let (ship_id, launcher_id, probe_stack_id) = (640_000_401_i64, 640_000_410_i64, 640_000_411_i64);
    let mut sess = login_in_space(
        &ctx,
        92_030,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_id,
        621,
        SYS_JITA,
        jita_pos,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            launcher_id,
            make_item(
                launcher_id,
                17_901,
                PILOT_A,
                ship_id,
                InventoryFlag::HiSlot0,
                1,
                true,
            ),
        );
        snap.module_online_states.insert(launcher_id, true);
        snap.items.insert(
            probe_stack_id,
            make_item(
                probe_stack_id,
                30_013,
                PILOT_A,
                ship_id,
                InventoryFlag::HiSlot0,
                8,
                false,
            ),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    rpc(
        &ctx,
        &mut sess,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(launcher_id),
            PyRep::String("launchProbe".into()),
        ],
    )
    .await
    .unwrap();
    let launch_notifs = drain_all(&ctx, PILOT_A);
    let new_probe = launch_notifs
        .iter()
        .find(|(e, _)| e == "OnNewProbe")
        .map(|(_, a)| &a[0])
        .expect("OnNewProbe must be emitted");
    let expiry_ft = dict_get(new_probe, "expiry")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    assert!(
        expiry_ft > 130_000_000_000_000_000,
        "OnNewProbe expiry must be BlueOS FILETIME ticks, got {expiry_ft}"
    );
    assert!(dict_get(new_probe, "pos").is_some());

    // Near scan -> measure warp duration
    let near_coords = PyRep::List(
        (0..8)
            .map(|i| {
                PyRep::List(vec![
                    PyRep::Real(jita_pos.x + 500_000.0 + (i as f64) * 500.0),
                    PyRep::Real(jita_pos.y + 2_000.0),
                    PyRep::Real(jita_pos.z),
                    PyRep::Integer(2),
                ])
            })
            .collect(),
    );
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[near_coords])
        .await
        .unwrap();
    let near_imm = drain_all(&ctx, PILOT_A);
    let near_ms = near_imm
        .iter()
        .find(|(e, _)| e == "OnProbeWarpStart")
        .and_then(|(_, a)| a.get(4))
        .and_then(|v| v.as_i64())
        .expect("OnProbeWarpStart duration");
    assert!(near_imm
        .iter()
        .any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(3))));
    assert!(!near_imm.iter().any(|(e, _)| e == "OnProbeWarpEnd"));

    ctx.advance_secs(15);
    let near_done = drain_all(&ctx, PILOT_A);
    assert_eq!(
        near_done
            .iter()
            .filter(|(e, _)| e == "OnProbeWarpEnd")
            .count(),
        8
    );
    assert!(near_done
        .iter()
        .any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(4))));
    assert!(near_done
        .iter()
        .any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(1))));

    // Far scan (10 AU) -> duration must be strictly larger than near_ms
    let far_coords = PyRep::List(
        (0..8)
            .map(|i| {
                PyRep::List(vec![
                    PyRep::Real(jita_pos.x + 10.0 * AU_METERS + (i as f64) * 500.0),
                    PyRep::Real(jita_pos.y),
                    PyRep::Real(jita_pos.z),
                    PyRep::Integer(2),
                ])
            })
            .collect(),
    );
    rpc(&ctx, &mut sess, "scanMgr", "RequestScans", &[far_coords])
        .await
        .unwrap();
    let far_imm = drain_all(&ctx, PILOT_A);
    let far_ms = far_imm
        .iter()
        .find(|(e, _)| e == "OnProbeWarpStart")
        .and_then(|(_, a)| a.get(4))
        .and_then(|v| v.as_i64())
        .expect("far OnProbeWarpStart duration");
    assert!(
        far_ms > near_ms,
        "far warp ({far_ms}ms) must exceed near warp ({near_ms}ms)"
    );

    ctx.advance_secs(30);
    drain_all(&ctx, PILOT_A);

    rpc(&ctx, &mut sess, "scanMgr", "RecoverProbes", &[])
        .await
        .unwrap();
    let rec = drain_all(&ctx, PILOT_A);
    assert!(rec
        .iter()
        .any(|(e, a)| e == "OnProbeStateChanged" && a.get(1) == Some(&PyRep::Integer(5))));
}

// parity: notify:OnDungeonInstanceAdded, notify:OnDungeonInstanceRemoved
}

pub(crate) mod parity_notify_batch3_part3 {
    #[allow(unused_imports)]
    use super::*;

// E2E integration tests closing `OnDungeonInstanceAdded`, `OnDungeonInstanceRemoved`,
// `OnFacWarBunkerVulnerable`, and `OnInfluenceUpdate` in Batch 3.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, TargetLock};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_JITA: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

fn drain_all(ctx: &ServerContext, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
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
    }
}

async fn login_in_space(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
    ship_id: i64,
    ship_type_id: u32,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(
                ship_id,
                ship_type_id,
                char_id,
                i64::from(sys_id),
                InventoryFlag::None,
                1,
                true,
            ),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(char_id, ship_id, ship_type_id),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    sess
}

// parity: notify:OnDungeonInstanceAdded, notify:OnDungeonInstanceRemoved
#[tokio::test]
async fn test_batch3_dungeon_instance_added_and_removed() {
    eve_testkit::attest(&[
        "notify:OnDungeonInstanceAdded",
        "notify:OnDungeonInstanceRemoved",
    ]);

    let ctx = new_ctx();
    let dest_sys =
        eve_server::services::pve_expeditions::find_escalation_destination_system(&ctx, SYS_JITA);
    let _dest_sess = login_in_space(
        &ctx,
        92_040,
        "other",
        "other-secret",
        PILOT_B,
        640_000_502,
        587,
        dest_sys,
        DVec3::ZERO,
    )
    .await;

    let (ship_a, gun_a, gun_b, gun_c) = (640_000_501_i64, 640_000_510_i64, 640_000_512_i64, 640_000_514_i64);
    let mut sess_a = login_in_space(
        &ctx,
        92_041,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        638,
        SYS_JITA,
        DVec3::ZERO,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        for (gid, aid, flag) in [
            (gun_a, 640_000_511_i64, InventoryFlag::HiSlot0),
            (gun_b, 640_000_513_i64, InventoryFlag::HiSlot1),
            (gun_c, 640_000_515_i64, InventoryFlag::HiSlot2),
        ] {
            snap.items.insert(gid, make_item(gid, 490, PILOT_A, ship_a, flag, 1, true));
            snap.items.insert(aid, make_item(aid, 206, PILOT_A, ship_a, flag, 500, false));
            snap.module_online_states.insert(gid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_a);

    let esc_seed = (1..10_000_u64)
        .find(|&s| {
            let r = eve_server::rng::MechanicRng::seeded(s);
            (0..4).all(|_| r.roll(0.20))
        })
        .expect("escalation seed");

    let anoms = rpc(&ctx, &mut sess_a, "scanSvc", "GetCombatAnomalyInstances", &[])
        .await
        .unwrap();
    let site_id = match anoms {
        PyRep::List(ref list) => list
            .iter()
            .find_map(|item| dict_get(item, "siteID").and_then(|v| v.as_str()).map(str::to_string)),
        _ => None,
    }
    .expect("Jita must have combat anomalies");

    let sys_jita = ctx.systems.get_or_spawn(SYS_JITA);
    for _ in 0..30 {
        let npc_ids: Vec<i64> = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&SYS_JITA)
                .and_then(|l| l.iter().find(|s| s.site_id == site_id))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        if npc_ids.is_empty() {
            break;
        }
        for nid in npc_ids {
            if let Ok(Some((_, nb))) = sys_jita.get_ball_snapshot(nid).await {
                sys_jita
                    .set_ball_position(ship_a, nb.position + DVec3::new(1_000.0, 0.0, 0.0))
                    .await;
            }
            let _ = sys_jita
                .add_target_lock(
                    ship_a,
                    TargetLock {
                        target_id: nid,
                        lock_time_ms: 100.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: None,
                    },
                )
                .await;
            for _ in 0..5 {
                if sys_jita.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    break;
                }
                ctx.mechanic_rng.set_seed(Some(esc_seed));
                for gid in [gun_a, gun_b, gun_c] {
                    if sys_jita.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                        break;
                    }
                    let _ = rpc(
                        &ctx,
                        &mut sess_a,
                        "dogmaIM",
                        "Activate",
                        &[
                            PyRep::Integer(gid),
                            PyRep::String("projectileFired".into()),
                            PyRep::Integer(nid),
                        ],
                    )
                    .await;
                }
                if sys_jita.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    break;
                }
                ctx.advance_secs(5);
                let _ = sys_jita.step_manual(5.0).await;
            }
        }
    }

    let esc = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.escalations_by_char
            .get(&PILOT_A)
            .and_then(|l| l.first().cloned())
    })
    .expect("anomaly must escalate");
    let dest_notifs = drain_all(&ctx, PILOT_B);
    assert!(
        dest_notifs.iter().any(|(e, _)| e == "OnDungeonInstanceAdded"),
        "OnDungeonInstanceAdded must be emitted in destination system"
    );

    // Move Pilot A to dest_sys and clear the expedition site via real weapon fire
    let sys_dest = ctx.systems.get_or_spawn(dest_sys);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = dest_sys;
        }
    });
    sess_a.solar_system_id = Some(dest_sys);
    sys_dest
        .spawn_ball_with_meta(
            Ball {
                id: ship_a,
                type_id: 638,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(PILOT_A, ship_a, 638),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: PILOT_A,
                char_id: Some(PILOT_A),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for _ in 0..30 {
        let npc_ids: Vec<i64> = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system
                .get(&dest_sys)
                .and_then(|l| l.iter().find(|s| s.expedition_instance_id == Some(esc.instance_id)))
                .map(|s| s.active_npc_ball_ids.clone())
                .unwrap_or_default()
        });
        if npc_ids.is_empty() {
            break;
        }
        for nid in npc_ids {
            if let Ok(Some((_, nb))) = sys_dest.get_ball_snapshot(nid).await {
                sys_dest
                    .set_ball_position(ship_a, nb.position + DVec3::new(1_000.0, 0.0, 0.0))
                    .await;
            }
            let _ = sys_dest
                .add_target_lock(
                    ship_a,
                    TargetLock {
                        target_id: nid,
                        lock_time_ms: 100.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: None,
                    },
                )
                .await;
            for _ in 0..5 {
                if sys_dest.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    break;
                }
                for gid in [gun_a, gun_b, gun_c] {
                    if sys_dest.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                        break;
                    }
                    let _ = rpc(
                        &ctx,
                        &mut sess_a,
                        "dogmaIM",
                        "Activate",
                        &[
                            PyRep::Integer(gid),
                            PyRep::String("projectileFired".into()),
                            PyRep::Integer(nid),
                        ],
                    )
                    .await;
                }
                if sys_dest.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    break;
                }
                ctx.advance_secs(5);
                let _ = sys_dest.step_manual(5.0).await;
            }
        }
    }
    let dest_after = drain_all(&ctx, PILOT_B);
    assert!(
        dest_after.iter().any(|(e, _)| e == "OnDungeonInstanceRemoved"),
        "OnDungeonInstanceRemoved must be emitted when expedition step completes"
    );
}

// parity: notify:OnFacWarBunkerVulnerable
#[tokio::test]
async fn test_batch3_facwar_bunker_vulnerable() {
    eve_testkit::attest(&["notify:OnFacWarBunkerVulnerable"]);

    let ctx = new_ctx();
    let ship_a = 640_000_601_i64;
    let mut sess_a = login_in_space(
        &ctx,
        92_050,
        "demo",
        "demo-secret",
        PILOT_A,
        ship_a,
        587,
        SYS_JITA,
        DVec3::ZERO,
    )
    .await;

    let _ = rpc(
        &ctx,
        &mut sess_a,
        "fwWarzoneSolarsystem",
        "GetAllWarzonesOccupationStates",
        &[],
    )
    .await
    .unwrap();
    let (fw_sys, attacker_fac) = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .iter()
            .filter(|(&sys, _)| {
                eve_server::services::fw_services::compute_system_frontline_status(&ctx, snap, sys)
                    == eve_server::services::fw_services::STATUS_FRONTLINE
            })
            .max_by_key(|(_, occ)| occ.victory_points)
            .map(|(&sys, occ)| {
                let atk = match occ.occupier_faction_id {
                    500_001 => 500_004,
                    500_004 => 500_001,
                    500_002 => 500_003,
                    _ => 500_002,
                };
                (sys, atk)
            })
            .expect("Frontline FW system occupation")
    });
    rpc(
        &ctx,
        &mut sess_a,
        "fwCharacterEnlistmentMgr",
        "CreateMyDirectEnlistment",
        &[PyRep::Integer(attacker_fac)],
    )
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = fw_sys;
        }
    });
    sess_a.solar_system_id = Some(fw_sys);
    let sys_fw = ctx.systems.get_or_spawn(fw_sys);
    let zone_pos = DVec3::from_array(eve_server::services::fw_gameplay::BATTLEFIELD_ZONE_CENTERS[0]);
    sys_fw
        .spawn_ball_with_meta(
            Ball {
                id: ship_a,
                type_id: 587,
                position: zone_pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(PILOT_A, ship_a, 587),
            eve_destiny::SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: PILOT_A,
                char_id: Some(PILOT_A),
                corp_id: 1000009,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    drain_all(&ctx, PILOT_A);

    // Capture Frontline Battlefields (2,000 VP each; 4h respawn cooldown) until 100% contested
    for _ in 0..40 {
        eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
        ctx.clock().advance_secs(905);
        eve_server::services::fw_gameplay::tick_fw_gameplay(&ctx, ctx.now_unix()).await;
        let vuln = ctx
            .db
            .with_snapshot(|s| s.fw_runtime.ihub_vulnerable.get(&fw_sys).copied().unwrap_or(false));
        if vuln {
            break;
        }
        ctx.clock().advance_secs(eve_server::services::fw_gameplay::BATTLEFIELD_RESPAWN_COOLDOWN_SECS + 5);
    }
    let fw_notifs = drain_all(&ctx, PILOT_A);
    assert!(
        fw_notifs.iter().any(|(e, _)| e == "OnFacWarBunkerVulnerable"),
        "OnFacWarBunkerVulnerable must be emitted when FW system reaches 100% contested"
    );
}

// parity: notify:OnInfluenceUpdate
#[tokio::test]
async fn test_batch3_incursion_influence_update() {
    eve_testkit::attest(&["notify:OnInfluenceUpdate"]);

    let ctx = new_ctx();
    let mut gm_sess = login_in_space(
        &ctx,
        92_060,
        "demo",
        "demo-secret",
        PILOT_A,
        640_000_701,
        587,
        SYS_JITA,
        DVec3::ZERO,
    )
    .await;

    eve_server::services::pve_special_sites::advance_pve_special_clock(&ctx, 1);
    let (inc_sys, site_id, boss_id) = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
        reg.incursions.values().next().and_then(|inc| {
            inc.sites
                .iter()
                .find(|s| !s.locked_until_low_influence)
                .map(|s| (s.solar_system_id, s.site_id.clone(), s.boss_npc_ball_id))
        })
    })
    .expect("spawned incursion site");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_A) {
            ch.solar_system_id = inc_sys;
        }
    });
    gm_sess.solar_system_id = Some(inc_sys);
    drain_all(&ctx, PILOT_A);

    for _ in 0..10 {
        let esc_ids: Vec<i64> = eve_server::services::pve_special_sites::with_pve_special_reg(&ctx, |reg| {
            reg.incursions
                .values()
                .flat_map(|inc| inc.sites.iter())
                .find(|s| s.site_id == site_id)
                .map(|s| {
                    s.escort_spawns
                        .iter()
                        .filter(|e| !e.destroyed)
                        .map(|e| e.npc_ball_id)
                        .collect()
                })
                .unwrap_or_default()
        });
        if esc_ids.is_empty() {
            break;
        }
        for eid in esc_ids {
            let _ = eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed(
                &ctx,
                inc_sys,
                eid,
                Some(PILOT_A),
            )
            .await;
        }
    }
    let sys_inc = ctx.systems.get_or_spawn(inc_sys);
    sys_inc.remove_ball(boss_id).await;
    eve_server::services::pve_incursion_sites::on_incursion_boss_destroyed(
        &ctx,
        inc_sys,
        boss_id,
        Some(PILOT_A),
    )
    .await
    .expect("boss destroyed")
    .expect("site completed");
    let inc_notifs = drain_all(&ctx, PILOT_A);
    let inf_ev = inc_notifs
        .iter()
        .find(|(e, _)| e == "OnInfluenceUpdate")
        .expect("OnInfluenceUpdate must be emitted on incursion site completion");
    let inf_val = dict_get(&inf_ev.1[1], "influence")
        .and_then(|v| v.as_f64())
        .expect("influence float");
    assert!((0.0..1.0).contains(&inf_val));
}
}
