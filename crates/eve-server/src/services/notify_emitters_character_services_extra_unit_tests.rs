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
            CorporationRecord {
                corporation_id: corp_id,
                ceo_id: 90_000_999, // char_id is NOT CEO
                name: "IndyCorp".into(),
                ticker: "INDY".into(),
                ..Default::default()
            },
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
