//! Additional branch-coverage and mutation-killing unit tests for `notify_emitters_character_services.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::notify_emitters_character_services::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{
    CorporationRecord, FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord,
    WorldRepository,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> (ServerContext, i64) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn setattr_industry_corp_division_roles_skills_and_fleet_emitters_kill_mutations() {
    let (ctx, char_id) = make_ctx();
    let corp_id = 98_000_701_i64;
    let station_id = 60_003_760_i64;
    let container_id = 990_700_i64;

    // 1. emit_setattr_notifications: loc_id <= 0 emits ONLY OnModuleAttributeChange (not OnLocationAttributeChange)
    emit_setattr_notifications(&ctx, char_id, 0, 5001, 6, 1000, 50.0, 25.0);
    emit_setattr_notifications(&ctx, char_id, -1, 5001, 6, 1000, 50.0, 25.0);
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(notifs.len(), 2);
    assert!(notifs.iter().all(|n| n.event == "OnModuleAttributeChange"));

    // 2. emit_contextual_offer_purchased: negative offer_id is ignored
    emit_contextual_offer_purchased(&ctx, char_id, -10);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // 3. emit_industry_materials_consumed: empty or non-positive consumed list is a no-op;
    //    corp division (flag 116..=122) checks division query/take/factory_manager role;
    //    singleton items are excluded from available quantity sum
    emit_industry_materials_consumed(
        &ctx,
        char_id,
        1,
        station_id,
        station_id,
        corp_id,
        true,
        InventoryFlag::CorpSAG1,
        None,
        &[],
    );
    emit_industry_materials_consumed(
        &ctx,
        char_id,
        1,
        station_id,
        station_id,
        corp_id,
        true,
        InventoryFlag::CorpSAG1,
        None,
        &[(34, 0), (35, -5)],
    );
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.ceo_id = 90_000_999;
                __corp.name = "IndyCorp".into();
                __corp.ticker = "INDY".into();
                __corp },
        );
        let mut monitors = BTreeMap::new();
        monitors.insert(
            801,
            IndustryMaterialMonitorSub {
                char_id,
                owner_id: corp_id,
                location_id: container_id,
                flag_id: InventoryFlag::CorpSAG2 as i64, // 117 -> div_idx = 1
            },
        );
        save_industry_material_monitors(snap, &monitors);
        assert_eq!(load_industry_material_monitors(snap).len(), 1);

        // Stack of 400 unlocked non-singleton + 100 singleton (excluded)
        snap.items.insert(
            990_701,
            InventoryItemRecord {
                item_id: 990_701,
                type_id: 34,
                owner_id: corp_id,
                location_id: container_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 400,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            990_702,
            InventoryItemRecord {
                item_id: 990_702,
                type_id: 34,
                owner_id: corp_id,
                location_id: container_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 100,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Without corp division role -> skipped!
    emit_industry_materials_consumed(
        &ctx,
        char_id,
        1,
        station_id,
        station_id,
        corp_id,
        true,
        InventoryFlag::CorpSAG2,
        Some(container_id),
        &[(34, 10)],
    );
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Grant query_bit for div_idx = 1 (1 << 21) -> emits OnIndustryMaterials with avail == 400
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(char_id, 1u64 << 21);
    });
    emit_industry_materials_consumed(
        &ctx,
        char_id,
        1,
        station_id,
        station_id,
        corp_id,
        true,
        InventoryFlag::CorpSAG2,
        Some(container_id),
        &[(34, 10)],
    );
    let ind_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(ind_notifs.len(), 1);
    assert_eq!(ind_notifs[0].args[0], PyRep::Integer(801));
    assert_eq!(
        ind_notifs[0].args[1],
        PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(400))])
    );

    // 4. emit_logon_skills_trained: higher level/SP in first tuple is preserved when a lower tuple follows
    emit_logon_skills_trained(
        &ctx,
        char_id,
        &[(3300, 1, 4, 45_255.0), (3300, 2, 3, 8_000.0)],
        false,
    );
    let sk_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(sk_notifs.len(), 1);
    assert_eq!(sk_notifs[0].args[1], PyRep::Bool(false));
    if let PyRep::Dict(entries) = &sk_notifs[0].args[0] {
        assert_eq!(entries[0].1.dict_get("currentLevel"), Some(&PyRep::Integer(1)));
        assert_eq!(entries[0].1.dict_get("skillLevel"), Some(&PyRep::Integer(4)));
        assert_eq!(
            entries[0].1.dict_get("skillPoints"),
            Some(&PyRep::Real(45_255.0))
        );
    } else {
        panic!("expected Dict");
    }

    // 5. emit_fleet_respawn_points_if_any & emit_fleet_loot_event_if_in_fleet edge cases
    emit_fleet_respawn_points_if_any(&ctx, -1);
    emit_fleet_respawn_points_if_any(&ctx, 999_999);
    emit_fleet_loot_event_if_in_fleet(&ctx, char_id, Some(0), &[(34, 10)]);
    emit_fleet_loot_event_if_in_fleet(&ctx, char_id, Some(601), &[(34, 0), (35, -1)]);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());

    // Destroyed or unanchored structure or offline clone service does not emit OnFleetRespawnPointsUpdate
    let st_destroyed = 1_090_000_001_i64;
    let st_offline = 1_090_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            602,
            FleetRecord {
                fleet_id: 602,
                boss_id: char_id,
                members: vec![char_id],
                ..Default::default()
            },
        );
        snap.structures.insert(
            st_destroyed,
            UpwellStructureRecord {
                structure_id: st_destroyed,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Destroyed".into(),
                state: "destroyed".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            st_offline,
            UpwellStructureRecord {
                structure_id: st_offline,
                type_id: 35_832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Offline".into(),
                state: "shield_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            990_710,
            InventoryItemRecord {
                item_id: 990_710,
                type_id: 35_894,
                owner_id: corp_id,
                location_id: st_offline,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(990_710, false);
    });
    emit_fleet_respawn_points_if_any(&ctx, 602);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
}

#[test]
fn batch3_gm_slash_commands_all_validation_and_schedule_branches_kill_mutations() {
    let (ctx, char_id) = make_ctx();
    let sys_id = Some(30_000_142_u32);

    // Empty command returns None
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "   ").is_none());

    // 1. /achievements reset with explicit target_cid
    let target_cid = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
        ext.achievements_by_char
            .entry(target_cid)
            .or_default()
            .push(100);
        crate::services::cosmetics_ui_ops::save_cosmetics_ui_state(snap, &ext);
    });
    let ach_res = handle_batch3_gm_slash_cmd(
        &ctx,
        char_id,
        sys_id,
        &format!("/achievements reset {target_cid}"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        ach_res.dict_get("characterID"),
        Some(&PyRep::Integer(target_cid))
    );
    ctx.db.with_snapshot(|snap| {
        let ext = crate::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap);
        assert!(!ext.achievements_by_char.contains_key(&target_cid));
    });

    // 2. /chatban & /chatunban boundary checks (<= 0, > u32::MAX, "permanent", 0s clamped to 1s)
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/chatban 0")
        .unwrap()
        .is_err());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/chatban -5")
        .unwrap()
        .is_err());
    assert!(
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/chatban 5000000000")
            .unwrap()
            .is_err()
    );
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/chatunban 0")
        .unwrap()
        .is_err());

    let perm_res = handle_batch3_gm_slash_cmd(
        &ctx,
        char_id,
        sys_id,
        &format!("/chatban {char_id} permanent"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(perm_res.dict_get("permanent"), Some(&PyRep::Bool(true)));
    assert_eq!(
        perm_res.dict_get("banExpiry"),
        Some(&PyRep::Integer(i64::MAX))
    );

    let temp_res = handle_batch3_gm_slash_cmd(
        &ctx,
        char_id,
        sys_id,
        &format!("/chatban {char_id} 0"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(temp_res.dict_get("permanent"), Some(&PyRep::Bool(false)));
    let expected_ft = ctx.now_blueos_filetime_ticks() + 10_000_000; // 0 clamped to 1 sec
    assert_eq!(
        temp_res.dict_get("banExpiry"),
        Some(&PyRep::Integer(expected_ft))
    );

    // 3. /campaign enroll & /campaign seasonal validation and success
    let valid_camp_id = *crate::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde)
        .keys()
        .next()
        .expect("SDE has login campaign");

    assert_eq!(
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/campaign enroll 999999999")
            .unwrap()
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );
    let enroll_ok = handle_batch3_gm_slash_cmd(
        &ctx,
        char_id,
        sys_id,
        &format!("/campaign enroll {valid_camp_id} {char_id}"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(enroll_ok.dict_get("enrolled"), Some(&PyRep::Bool(true)));
    assert_eq!(
        enroll_ok.dict_get("campaignID"),
        Some(&PyRep::Integer(valid_camp_id))
    );

    assert_eq!(
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/campaign seasonal")
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidSlashArguments"
    );
    assert_eq!(
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/campaign seasonal 999999999")
            .unwrap()
            .unwrap_err()
            .code,
        "CampaignNotFound"
    );
    assert_eq!(
        handle_batch3_gm_slash_cmd(
            &ctx,
            char_id,
            sys_id,
            &format!("/campaign seasonal {valid_camp_id} 0")
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSlashArguments"
    );
    assert_eq!(
        handle_batch3_gm_slash_cmd(
            &ctx,
            char_id,
            sys_id,
            &format!("/campaign seasonal {valid_camp_id} 366")
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSlashArguments"
    );
    assert_eq!(
        handle_batch3_gm_slash_cmd(
            &ctx,
            char_id,
            sys_id,
            &format!("/campaign seasonal {valid_camp_id} 14 bad_hms")
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSlashArguments"
    );

    let seas_ok = handle_batch3_gm_slash_cmd(
        &ctx,
        char_id,
        sys_id,
        &format!("/campaign seasonal {valid_camp_id} 14 11:30:00"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        seas_ok.dict_get("campaignID"),
        Some(&PyRep::Integer(valid_camp_id))
    );
    assert_eq!(
        seas_ok.dict_get("turnoverTime"),
        Some(&PyRep::String("11:30:00".into()))
    );
    assert_eq!(
        seas_ok.dict_get("endUnix"),
        Some(&PyRep::Integer((ctx.now_unix() + 14 * 86_400) as i64))
    );

    let stop_ok = handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/campaign seasonal stop")
        .unwrap()
        .unwrap();
    assert_eq!(stop_ok.dict_get("active"), Some(&PyRep::Bool(false)));

    // 4. /shutdown non-Delay reason uses ShutdownNotificationTitle; /shutdown cancel custom label
    let _ = ctx.drain_notifications_for_char(char_id);
    let sd_ok = handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/shutdown 60 Reboot")
        .unwrap()
        .unwrap();
    assert_eq!(
        sd_ok.dict_get("shutdownInitiated"),
        Some(&PyRep::Bool(true))
    );
    assert!(ctx.cluster_state.read().unwrap().shutdown_in_progress);
    let sd_notifs = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        sd_notifs[0].args[0],
        PyRep::String("UI/Shared/ShutdownNotificationTitle".into())
    );

    let cancel_ok =
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/shutdown cancel CustomCancelReason")
            .unwrap()
            .unwrap();
    assert!(!ctx.cluster_state.read().unwrap().shutdown_in_progress);
    assert_eq!(
        cancel_ok.dict_get("explanationLabel"),
        Some(&PyRep::String("CustomCancelReason".into()))
    );

    // 5. /fastcheckout toggle: setting true emits notification once; repeating true does not; setting false emits once
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/fastcheckout true")
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(ctx.drain_notifications_for_char(char_id).len(), 1);
    assert_eq!(
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/fastcheckout true")
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );
    assert!(ctx.drain_notifications_for_char(char_id).is_empty());
    assert_eq!(
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/fastcheckout false")
            .unwrap()
            .unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(ctx.drain_notifications_for_char(char_id).len(), 1);
}

#[test]
fn gm_timer_acceleration_slash_commands_cover_all_aliases_branches_and_notifications() {
    static FULL_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    let sde = Arc::clone(FULL_SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)));
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001_i64;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_800_000_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));
    let sys_id = 30_000_142_u32;

    // 1. /advancetime, /fastforward, /ff + default 3600s + session change timer cleared
    let base_now = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, base_now, sys_id, 0, "test_jump");
    assert!(
        crate::services::space_destiny::get_session_change_timer(&ctx, char_id).is_some(),
        "session change timer should be active before /advancetime"
    );
    let adv_default = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/advancetime")
        .unwrap()
        .unwrap();
    assert_eq!(adv_default.dict_get("advancedSeconds"), Some(&PyRep::Integer(3600)));
    assert_eq!(
        adv_default.dict_get("nowUnix"),
        Some(&PyRep::Integer((base_now + 3600) as i64))
    );
    assert!(crate::services::space_destiny::get_session_change_timer(&ctx, char_id).is_none());

    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, ctx.now_unix(), sys_id, 0, "test_jump");
    let ff_alias = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/fastforward 120")
        .unwrap()
        .unwrap();
    assert_eq!(ff_alias.dict_get("advancedSeconds"), Some(&PyRep::Integer(120)));
    assert!(crate::services::space_destiny::get_session_change_timer(&ctx, char_id).is_none());

    let ff_short = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/ff 45")
        .unwrap()
        .unwrap();
    assert_eq!(ff_short.dict_get("advancedSeconds"), Some(&PyRep::Integer(45)));
    assert_eq!(ctx.now_unix(), base_now + 3600 + 120 + 45);

    // 2. /completeskills [all|seconds] + /trainskills + /finishskills
    let skill_tid = 3402_u32; // Science (rank 1)
    let rank = ctx
        .sde
        .types
        .get(&skill_tid)
        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT))
        .copied()
        .unwrap_or(1.0);
    let lvl1_sp = crate::formulas::sp_for_skill_level(rank, 1);
    let lvl5_sp = crate::formulas::sp_for_skill_level(rank, 5);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.clone_grade = 2;
            ch.skill_points = 0.0;
        }
        snap.paused_skill_queues.insert(char_id);
        snap.skill_queues.insert(
            char_id,
            vec![eve_db::SkillQueueEntry {
                type_id: skill_tid,
                target_level: 5,
                start_sp: 0.0,
                end_sp: lvl5_sp,
                duration_seconds: 100_000.0,
                started_at_unix: Some(ctx.now_unix()),
            }],
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    // Partial advance (10s): unpauses queue, advances SP without completing level 1 (<250 SP),
    // and emits OnServerSkillsChanged + OnNewSkillQueueSaved.
    let partial_res = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/completeskills 10")
        .unwrap()
        .unwrap();
    assert_eq!(partial_res.dict_get("remainingQueueLen"), Some(&PyRep::Integer(1)));
    let partial_sp = match partial_res.dict_get("totalSkillPoints") {
        Some(PyRep::Real(v)) => *v,
        other => panic!("expected Real totalSkillPoints, got {other:?}"),
    };
    assert!(partial_sp > 0.0 && partial_sp < lvl1_sp);
    assert!(ctx
        .db
        .with_snapshot(|s| !s.paused_skill_queues.contains(&char_id)));
    let partial_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        partial_notifs.iter().any(|n| n.event == "OnServerSkillsChanged"),
        "expected OnServerSkillsChanged on partial /completeskills"
    );
    assert!(
        partial_notifs.iter().any(|n| n.event == "OnNewSkillQueueSaved"),
        "expected OnNewSkillQueueSaved on partial /completeskills"
    );

    // Complete remaining queue with /trainskills (default "all")
    let _ = ctx.drain_notifications_for_char(char_id);
    let complete_res = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/trainskills")
        .unwrap()
        .unwrap();
    assert_eq!(complete_res.dict_get("remainingQueueLen"), Some(&PyRep::Integer(0)));
    ctx.db.with_snapshot(|snap| {
        let trained = snap.skills[&char_id].get(&skill_tid).expect("trained skill");
        assert_eq!(trained.level, 5);
        assert!((trained.skill_points - lvl5_sp).abs() < 1e-3);
    });
    let complete_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(complete_notifs.iter().any(|n| n.event == "OnSkillTrained"));
    assert!(complete_notifs.iter().any(|n| n.event == "OnServerSkillsChanged"));
    assert!(complete_notifs.iter().any(|n| n.event == "OnNewSkillQueueSaved"));

    // Invalid arg fallback to "all" via /finishskills
    ctx.db.with_mut_snapshot(|snap| {
        snap.paused_skill_queues.insert(char_id);
        snap.skill_queues.insert(
            char_id,
            vec![eve_db::SkillQueueEntry {
                type_id: 3403_u32,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 500.0,
                started_at_unix: Some(ctx.now_unix()),
            }],
        );
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let alias_res = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/finishskills all")
        .unwrap()
        .unwrap();
    assert_eq!(alias_res.dict_get("remainingQueueLen"), Some(&PyRep::Integer(0)));
    let alias_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(alias_notifs.iter().any(|n| n.event == "OnServerSkillsChanged"));
    assert!(alias_notifs.iter().any(|n| n.event == "OnNewSkillQueueSaved"));

    let fallback_res = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/completeskills bogus")
        .unwrap()
        .unwrap();
    assert_eq!(fallback_res.dict_get("remainingQueueLen"), Some(&PyRep::Integer(0)));

    // 3. /upwelltimer [structure_id], /structuretimer, /finishstructure
    let now = ctx.now_unix();
    let sid_reinf = 1_000_000_901_001_i64;
    let sid_unanchor = 1_000_000_901_002_i64;
    let sid_destruct = 1_000_000_901_003_i64;
    let sid_other_sys = 1_000_000_901_004_i64;
    let astrahus_tid = 35_832_u32;
    let corp_id = ctx
        .db
        .with_snapshot(|s| s.characters[&char_id].corporation_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
        }
        snap.structures.insert(
            sid_reinf,
            UpwellStructureRecord {
                structure_id: sid_reinf,
                type_id: astrahus_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Reinforced Citadel".into(),
                position: [1_000_000.0, 0.0, 0.0],
                state: "ArmorReinforced".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 0.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: now + 86_400,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            sid_unanchor,
            UpwellStructureRecord {
                structure_id: sid_unanchor,
                type_id: astrahus_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "Unanchoring Citadel".into(),
                position: [2_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            sid_destruct,
            UpwellStructureRecord {
                structure_id: sid_destruct,
                type_id: astrahus_tid,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "SelfDestruct Citadel".into(),
                position: [3_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 100,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: Some(now + 1_200),
            },
        );
        snap.structures.insert(
            sid_other_sys,
            UpwellStructureRecord {
                structure_id: sid_other_sys,
                type_id: astrahus_tid,
                owner_corp_id: corp_id,
                solar_system_id: 30_002_187,
                name: "Remote Citadel".into(),
                position: [4_000_000.0, 0.0, 0.0],
                state: "ArmorReinforced".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 0.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                reinforced_until_unix: now + 86_400,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.unanchoring_until_unix.insert(sid_unanchor, now + 604_800);
        ext.lifecycle.insert(
            sid_reinf,
            crate::services::station_structure::structure_upwell_ops::StructureLifecycleState {
                repair_until_unix: Some(now + 900),
                ..Default::default()
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    // Target a single structure_id via /upwelltimer <id>
    let upwell_one =
        handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), &format!("/upwelltimer {sid_reinf}"))
            .unwrap()
            .unwrap();
    assert_eq!(upwell_one.dict_get("updatedCount"), Some(&PyRep::Integer(1)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&sid_reinf].state, "ArmorVulnerable");
        assert_eq!(snap.structures[&sid_other_sys].state, "ArmorReinforced");
    });

    // Fast-forward repair timer left over from exiting ArmorReinforced via /structuretimer <id>
    let repair_res = handle_batch3_gm_slash_cmd(
        &ctx,
        char_id,
        None,
        &format!("/structuretimer {sid_reinf}"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(repair_res.dict_get("updatedCount"), Some(&PyRep::Integer(1)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&sid_reinf].state, "ShieldVulnerable");
    });

    // Target all structures in caller's solar system (caller_sys = None falls back to char.solar_system_id)
    let finish_all = handle_batch3_gm_slash_cmd(&ctx, char_id, None, "/finishstructure")
        .unwrap()
        .unwrap();
    assert_eq!(finish_all.dict_get("updatedCount"), Some(&PyRep::Integer(3)));
    ctx.db.with_snapshot(|snap| {
        // sid_unanchor was packaged & removed from structures; sid_destruct was destroyed
        assert!(!snap.structures.contains_key(&sid_unanchor));
        assert!(snap
            .structures
            .get(&sid_destruct)
            .is_none_or(|s| s.state == "Destroyed"));
        // Remote system structure remains untouched
        assert_eq!(snap.structures[&sid_other_sys].state, "ArmorReinforced");
    });

    // 4. /completeindustry [job_id], /finishjobs, /industrytimer
    let job_char = 800_001_i64;
    let job_corp = 800_002_i64;
    let job_other = 800_003_i64;
    let job_delivered = 800_004_i64;
    let make_job = |job_id: i64, installer_id: i64, status: &str, dur: u64| eve_db::IndustryJobRecord {
        job_id,
        installer_id,
        facility_id: 60_003_760,
        blueprint_item_id: 700_000 + job_id,
        blueprint_type_id: 587,
        activity_id: 1,
        runs: 1,
        product_type_id: 587,
        status: status.into(),
        duration_seconds: dur,
        start_unix: now,
        blueprint_location_id: 60_003_760,
        output_location_id: 60_003_760,
        cost: Some(1_000.0),
        licensed_runs: None,
        probability: None,
        completed_unix: None,
        completed_character_id: None,
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(job_char, make_job(job_char, char_id, "Active", 3_600));
        snap.industry_jobs.insert(job_corp, make_job(job_corp, corp_id, "Active", 7_200));
        snap.industry_jobs.insert(job_other, make_job(job_other, char_id + 99, "Active", 1_800));
        snap.industry_jobs.insert(job_delivered, make_job(job_delivered, char_id, "Delivered", 600));
    });

    let ind_one =
        handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), &format!("/completeindustry {job_char}"))
            .unwrap()
            .unwrap();
    assert_eq!(ind_one.dict_get("readyJobCount"), Some(&PyRep::Integer(1)));
    assert_eq!(
        ind_one.dict_get("jobIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(job_char)]))
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&job_char].start_unix, now.saturating_sub(3_600));
        assert_eq!(snap.industry_jobs[&job_corp].start_unix, now);
    });

    // Reset job_char and test /finishjobs & /industrytimer matching caller + corp active jobs only
    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.get_mut(&job_char).unwrap().start_unix = now;
    });
    let ind_all = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/finishjobs")
        .unwrap()
        .unwrap();
    assert_eq!(ind_all.dict_get("readyJobCount"), Some(&PyRep::Integer(2)));
    assert_eq!(
        ind_all.dict_get("jobIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(job_char), PyRep::Integer(job_corp)]))
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&job_other].start_unix, now);
        assert_eq!(snap.industry_jobs[&job_delivered].start_unix, now);
    });
    let ind_alias =
        handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), &format!("/industrytimer {job_other}"))
            .unwrap()
            .unwrap();
    assert_eq!(ind_alias.dict_get("readyJobCount"), Some(&PyRep::Integer(1)));

    // 5. /cleartimers
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, now, sys_id, 0, "test_dock");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.last_clone_jump_unix = now;
            ch.jump_fatigue_minutes = 125.0;
            ch.jump_cooldown_until_unix = now + 600;
            ch.jump_fatigue_until_unix = now + 7_200;
        }
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_timers.insert(char_id, now + 300);
        rt.crimewatch_timer_states.insert(char_id, (1, 1, 1, 0, 0));
        rt.crimewatch_timer_triggered_at.insert(char_id, [now; 5]);
        rt.limited_engagements.entry(char_id).or_default().insert(90_000_002, now + 300);
    });

    let clear_res = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(sys_id), "/cleartimers")
        .unwrap()
        .unwrap();
    assert_eq!(clear_res.dict_get("cleared"), Some(&PyRep::Bool(true)));
    assert!(crate::services::space_destiny::get_session_change_timer(&ctx, char_id).is_none());
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert_eq!(ch.last_clone_jump_unix, 0);
        assert_eq!(ch.jump_fatigue_minutes, 0.0);
        assert_eq!(ch.jump_cooldown_until_unix, 0);
        assert_eq!(ch.jump_fatigue_until_unix, 0);
    });
    crate::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        assert!(!rt.npc_timers.contains_key(&char_id));
        assert!(!rt.crimewatch_timer_states.contains_key(&char_id));
        assert!(!rt.crimewatch_timer_triggered_at.contains_key(&char_id));
        assert!(!rt.limited_engagements.contains_key(&char_id));
    });
}

#[test]
fn emit_allow_war_updated_for_alliance_and_slash_unknown_none_kill_mutants() {
    let (ctx, char_id) = make_ctx();
    let alliance_id = 99_777_001_i64;
    let member_a = 98_777_001_i64;
    let member_b = 98_777_002_i64;
    let non_member = 98_777_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, aid) in [
            (member_a, Some(alliance_id)),
            (member_b, Some(alliance_id)),
            (non_member, None),
        ] {
            let mut corp = CorporationRecord::default();
            corp.corporation_id = cid;
            corp.ceo_id = char_id;
            corp.alliance_id = aid;
            corp.name = format!("Corp-{cid}");
            corp.ticker = format!("C{}", cid % 1000);
            snap.corporations.insert(cid, corp);
        }
    });

    // 1. emit_allow_war_updated_for_alliance persists `corp_allow_war` for member corps only
    //    (kills `emit_allow_war_updated_for_alliance -> ()`).
    emit_allow_war_updated_for_alliance(&ctx, alliance_id, true);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleet_and_social_ops.corp_allow_war.get(&member_a), Some(&true));
        assert_eq!(snap.fleet_and_social_ops.corp_allow_war.get(&member_b), Some(&true));
        assert_eq!(snap.fleet_and_social_ops.corp_allow_war.get(&non_member), None);
    });
    emit_allow_war_updated_for_alliance(&ctx, alliance_id, false);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleet_and_social_ops.corp_allow_war.get(&member_a), Some(&false));
        assert_eq!(snap.fleet_and_social_ops.corp_allow_war.get(&member_b), Some(&false));
    });

    // 2. handle_batch3_gm_slash_cmd returns `None` on non-batch3 command & non-empty keyval on valid command
    //    (kills `-> Some(Ok(Default::default()))` and `-> None`).
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, Some(30_000_142), "").is_none());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, Some(30_000_142), "/unknownslashcmd").is_none());

    let res = handle_batch3_gm_slash_cmd(&ctx, char_id, Some(30_000_142), "/achievements reset")
        .expect("recognized command must return Some")
        .expect("command succeeds");
    assert_eq!(res.dict_get("reset"), Some(&PyRep::Bool(true)));
    assert_eq!(res.dict_get("characterID"), Some(&PyRep::Integer(char_id)));
}
