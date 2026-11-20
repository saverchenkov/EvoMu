//! Unit tests for `notify_emitters_character_services`.

use super::*;
use crate::router::{NotificationTarget, ServerContext};
use eve_db::clock::VirtualServerClock;
use eve_db::{
    FleetRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_online_ctx() -> (ServerContext, i64) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char_id = 90_000_001;
    db.acquire_online_lock(1, 501, Some(char_id), true, 1_775_400_000)
        .expect("online lock");
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char_id)
}

#[test]
fn attribute_cluster_agent_alliance_and_cosmetic_emitters() {
    let (ctx, char_id) = make_online_ctx();

    // loc_id == owner_id emits only OnModuleAttributeChange
    emit_setattr_notifications(&ctx, char_id, char_id, 5001, 6, 12345, 250.0, 100.0);
    // loc_id > 0 && loc_id != owner_id emits both OnModuleAttributeChange and OnLocationAttributeChange
    emit_setattr_notifications(&ctx, char_id, 100_000_001, 5002, 6, 12346, 300.0, 250.0);

    emit_cluster_shutdown_cancelled(&ctx, "   ");
    emit_cluster_shutdown_cancelled(&ctx, "CustomShutdownLabel");
    emit_global_config_updated(&ctx, "machoNet.maxQueue", &PyRep::Integer(256));
    emit_air_npe_skipped(&ctx, char_id);
    emit_datacore_bought(&ctx, char_id, 3_018_921, 9_500_000.0);

    // Agent mission dialogue: standard accepted vs epic arc (agent_type_id == 10) completed with [d] header & empty body
    emit_agent_mission_dialogue(
        &ctx,
        char_id,
        3_018_921,
        2,
        "[d] Reconnaissance",
        "Fly to the beacon.",
        false,
    );
    emit_agent_mission_dialogue(&ctx, char_id, 3_018_922, 10, "[d]   ", "   ", true);

    // Alliance allow war update for member corp 98_000_001
    let ally_id = 99_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.corporation_id = 98_000_001;
        }
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.alliance_id = Some(ally_id);
        }
    });
    emit_allow_war_updated_for_alliance(&ctx, ally_id, true);

    emit_invalidate_cache_for_locations(&ctx, NotificationTarget::Character(char_id), &[]);
    emit_invalidate_cache_for_locations(&ctx, NotificationTarget::Character(char_id), &[60_003_760]);
    emit_campaign_enrollment_changed(&ctx, char_id);
    emit_challenge_expired(&ctx, char_id, 77, PyRep::Integer(88));
    emit_contextual_offer_purchased(&ctx, char_id, 0);
    emit_contextual_offer_purchased(&ctx, char_id, 321);
    emit_crate_loot_spawned(&ctx, char_id);
    emit_fast_checkout_enabled_changed(&ctx, 1, true);
    emit_facilities_updated(&ctx, Some(30_000_142), char_id, 60_003_760);
    emit_facilities_updated(&ctx, None, char_id, 60_003_760);

    let drained = ctx.drain_notifications_for_char(char_id);
    let events: Vec<&str> = drained.iter().map(|n| n.event.as_str()).collect();
    assert_eq!(events[0], "OnModuleAttributeChange");
    assert_eq!(events[1], "OnModuleAttributeChange");
    assert_eq!(events[2], "OnLocationAttributeChange");
    assert_eq!(events[3], "OnClusterShutdownCancelled");
    assert_eq!(
        drained[3].args,
        vec![PyRep::String("UI/Shared/ClusterShutdownDelayed".into())]
    );
    assert_eq!(
        drained[4].args,
        vec![PyRep::String("CustomShutdownLabel".into())]
    );
    assert!(events.contains(&"OnGlobalConfigUpdated"));
    assert!(events.contains(&"OnAirNpeStateChanged"));
    assert!(events.contains(&"OnDatacoreBought"));
    assert!(events.contains(&"OnIncomingAgentMessage"));
    assert!(events.contains(&"OnIncomingTransmission"));
    assert!(events.contains(&"OnEpicJournalChange"));
    assert!(events.contains(&"OnAllowWarUpdated"));
    assert!(events.contains(&"OnContextualOfferPurchased"));
    assert!(events.contains(&"OnCrateLootSpawned"));
    assert!(events.contains(&"OnFastCheckoutEnabledChanged"));
    assert!(events.contains(&"OnFacilitiesUpdated"));
}

#[test]
fn industry_monitors_skills_fleet_respawn_loot_and_misc_emitters() {
    let (ctx, char_id) = make_online_ctx();
    let station_id = 60_003_760_i64;
    let struct_id = 1_030_000_000_001_i64;
    let clone_mod_id = 990_001_i64;
    let tritanium_id = 990_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut monitors = BTreeMap::new();
        monitors.insert(
            701,
            IndustryMaterialMonitorSub {
                char_id,
                owner_id: char_id,
                location_id: station_id,
                flag_id: InventoryFlag::Hangar as i64,
            },
        );
        save_industry_material_monitors(snap, &monitors);
        snap.items.insert(
            tritanium_id,
            InventoryItemRecord {
                item_id: tritanium_id,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 2_500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            601,
            FleetRecord {
                fleet_id: 601,
                boss_id: char_id,
                members: vec![char_id],
                motd: String::new(),
                free_move: false,
                is_registered: false,
                ..Default::default()
            },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 1_000_170,
                solar_system_id: 30_000_142,
                name: "JitaCloneHub".into(),
                state: "shield_vulnerable".into(),
                position: [0.0, 0.0, 0.0],
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            clone_mod_id,
            InventoryItemRecord {
                item_id: clone_mod_id,
                type_id: 35_894,
                owner_id: 1_000_170,
                location_id: struct_id,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(clone_mod_id, true);
    });

    emit_industry_materials_consumed(
        &ctx,
        char_id,
        10,
        station_id,
        station_id,
        char_id,
        false,
        InventoryFlag::Hangar,
        None,
        &[(34, 500)],
    );
    emit_logon_skills_trained(&ctx, char_id, &[], true);
    emit_logon_skills_trained(
        &ctx,
        char_id,
        &[(3300, 1, 2, 8000.0), (3300, 1, 3, 45255.0)],
        true,
    );
    emit_destination_change(&ctx, char_id, 30_002_187, true, false);
    emit_fleet_respawn_points_if_any(&ctx, 0);
    emit_fleet_respawn_points_if_any(&ctx, 601);
    emit_fleet_loot_event_if_in_fleet(&ctx, char_id, None, &[(34, 10)]);
    emit_fleet_loot_event_if_in_fleet(&ctx, char_id, Some(601), &[(34, 10), (34, 5)]);
    emit_alpha_injection_available(&ctx, char_id);
    emit_ccp_notification(
        &ctx,
        NotificationTarget::Character(char_id),
        "Title",
        "Sub",
        "Body",
        1,
        99,
    );
    emit_community_fittings_updated(&ctx);
    emit_login_milestone_achieved(&ctx, char_id, 3, 250_000.0);
    emit_milestone_timer_started(&ctx, char_id, 300);

    let drained = ctx.drain_notifications_for_char(char_id);
    let events: HashSet<&str> = drained.iter().map(|n| n.event.as_str()).collect();
    assert!(events.contains("OnIndustryMaterials"));
    assert!(events.contains("OnLogonSkillsTrained"));
    assert!(events.contains("OnDestinationChange"));
    assert!(events.contains("OnFleetRespawnPointsUpdate"));
    assert!(events.contains("OnFleetLootEvent"));
    assert!(events.contains("OnAlphaInjectionAvailable"));
    assert!(events.contains("OnCCPNotification"));
    assert!(events.contains("OnCommunityFittingsUpdated"));
    assert!(events.contains("OnLoginMilestoneAchieved"));
    assert!(events.contains("OnMilestoneTimerStarted"));
}

#[test]
fn handle_batch3_gm_slash_cmd_branches() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = Some(30_000_142_u32);

    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/nonexistent").is_none());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/achievements reset")
        .unwrap()
        .is_ok());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/chatban")
        .unwrap()
        .is_err());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, &format!("/chatban {char_id} 60"))
        .unwrap()
        .is_ok());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, &format!("/chatban {char_id} perm"))
        .unwrap()
        .is_ok());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, &format!("/chatunban {char_id}"))
        .unwrap()
        .is_ok());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/campaign seasonal off")
        .unwrap()
        .is_ok());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/shutdown 120 Delay")
        .unwrap()
        .is_ok());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/shutdown cancel")
        .unwrap()
        .is_ok());
    assert_eq!(
        handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/fastcheckout true")
            .unwrap()
            .expect("fastcheckout"),
        PyRep::Bool(true)
    );
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/spawnanomaly 10001")
        .unwrap()
        .is_ok());
    assert!(handle_batch3_gm_slash_cmd(&ctx, char_id, sys_id, "/fwcapture 30000142 1000")
        .unwrap()
        .is_ok());
}

#[test]
fn on_industry_materials_emitter_enforces_location_scoped_roles_and_corp_membership() {
    let (ctx, _) = make_online_ctx();
    let station_hq = 60_003_760_i64;
    let station_other = 60_003_761_i64;

    let corp_a = 98_310_101_i64;
    let corp_b = 98_310_102_i64;
    let npc_corp = 1_000_170_i64;
    let uninserted_corp = 98_310_888_i64;

    let char_ceo = 91_310_001_i64;
    let char_hq_take1 = 91_310_002_i64;
    let char_hq_query1 = 91_310_003_i64;
    let char_ex_member = 91_310_004_i64;
    let char_excluded = 91_310_005_i64;
    let char_npc = 91_310_006_i64;
    let char_uninserted = 91_310_007_i64;

    let mon_ceo_hq = 8_001_i64;
    let mon_ceo_other = 8_002_i64;
    let mon_take1_hq = 8_003_i64;
    let mon_take1_other = 8_004_i64;
    let mon_take1_hq_div2 = 8_005_i64;
    let mon_query1_hq = 8_006_i64;
    let mon_query1_other = 8_007_i64;
    let mon_ex_hq = 8_008_i64;
    let mon_excluded_hq = 8_009_i64;
    let mon_npc_hq = 8_010_i64;
    let mon_uninserted_hq = 8_011_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp_a_rec = eve_db::CorporationRecord::new(corp_a, "Emitter Corp", "EMTC", char_ceo);
        corp_a_rec.hq_station_id = Some(station_hq);
        corp_a_rec.members = HashSet::from([char_ceo, char_hq_take1, char_hq_query1]);
        // Leftover DB roles for ex-member and excluded member must NOT authorize pushes.
        corp_a_rec.member_roles = std::collections::HashMap::from([
            (char_ex_member, eve_db::corp_roles::ROLE_DIRECTOR),
            (char_excluded, eve_db::corp_roles::ROLE_DIRECTOR),
        ]);
        snap.corporations.insert(corp_a, corp_a_rec);

        let mut corp_b_rec = eve_db::CorporationRecord::new(corp_b, "Moved Corp", "MVDC", char_ex_member);
        corp_b_rec.members = HashSet::from([char_ex_member]);
        snap.corporations.insert(corp_b, corp_b_rec);

        // Even if an NPC corp record exists in snapshot with CEO/Director, it must be rejected.
        let mut npc_rec = eve_db::CorporationRecord::new(npc_corp, "NPC Corp", "NPCC", char_npc);
        npc_rec.hq_station_id = Some(station_hq);
        npc_rec.members = HashSet::from([char_npc]);
        npc_rec.member_roles = std::collections::HashMap::from([
            (char_npc, eve_db::corp_roles::ROLE_DIRECTOR),
        ]);
        snap.corporations.insert(npc_corp, npc_rec);

        // Location-scoped HQ-only Take1 (bit 13) and Query1 (bit 20) — global roles == 0.
        snap.corp_role_assignments.insert(
            (corp_a, char_hq_take1),
            eve_db::CorpMemberRoleAssignment {
                character_id: char_hq_take1,
                roles_at_hq: eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[0],
                ..Default::default()
            },
        );
        snap.corp_role_assignments.insert(
            (corp_a, char_hq_query1),
            eve_db::CorpMemberRoleAssignment {
                character_id: char_hq_query1,
                roles_at_hq: eve_db::corp_roles::ROLE_HANGAR_CAN_QUERY[0],
                ..Default::default()
            },
        );
        // Uninserted corp has stale corp_role_assignments without a snap.corporations entry.
        snap.corp_role_assignments.insert(
            (uninserted_corp, char_uninserted),
            eve_db::CorpMemberRoleAssignment {
                character_id: char_uninserted,
                roles: eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_FACTORY_MANAGER,
                ..Default::default()
            },
        );

        for (idx, (cid, corpid, name)) in [
            (char_ceo, corp_a, "EmitCEO"),
            (char_hq_take1, corp_a, "EmitHqTake1"),
            (char_hq_query1, corp_a, "EmitHqQuery1"),
            (char_ex_member, corp_b, "EmitExMember"),
            (char_excluded, corp_a, "EmitExcluded"),
            (char_npc, npc_corp, "EmitNpc"),
            (char_uninserted, uninserted_corp, "EmitUninserted"),
        ]
        .into_iter()
        .enumerate()
        {
            let uid = 200 + idx as u64;
            let uname = format!("emit_user_{idx}");
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: "test".into(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    user_id: uid,
                    corporation_id: corpid,
                    station_id: Some(station_hq),
                    home_station_id: station_hq,
                    name: name.into(),
                    ..Default::default()
                },
            );
        }

        let sag1 = InventoryFlag::CorpSAG1 as i64;
        let sag2 = InventoryFlag::CorpSAG2 as i64;
        let monitors = BTreeMap::from([
            (mon_ceo_hq, IndustryMaterialMonitorSub { char_id: char_ceo, owner_id: corp_a, location_id: station_hq, flag_id: sag1 }),
            (mon_ceo_other, IndustryMaterialMonitorSub { char_id: char_ceo, owner_id: corp_a, location_id: station_other, flag_id: sag1 }),
            (mon_take1_hq, IndustryMaterialMonitorSub { char_id: char_hq_take1, owner_id: corp_a, location_id: station_hq, flag_id: sag1 }),
            (mon_take1_other, IndustryMaterialMonitorSub { char_id: char_hq_take1, owner_id: corp_a, location_id: station_other, flag_id: sag1 }),
            (mon_take1_hq_div2, IndustryMaterialMonitorSub { char_id: char_hq_take1, owner_id: corp_a, location_id: station_hq, flag_id: sag2 }),
            (mon_query1_hq, IndustryMaterialMonitorSub { char_id: char_hq_query1, owner_id: corp_a, location_id: station_hq, flag_id: sag1 }),
            (mon_query1_other, IndustryMaterialMonitorSub { char_id: char_hq_query1, owner_id: corp_a, location_id: station_other, flag_id: sag1 }),
            (mon_ex_hq, IndustryMaterialMonitorSub { char_id: char_ex_member, owner_id: corp_a, location_id: station_hq, flag_id: sag1 }),
            (mon_excluded_hq, IndustryMaterialMonitorSub { char_id: char_excluded, owner_id: corp_a, location_id: station_hq, flag_id: sag1 }),
            (mon_npc_hq, IndustryMaterialMonitorSub { char_id: char_npc, owner_id: npc_corp, location_id: station_hq, flag_id: sag1 }),
            (mon_uninserted_hq, IndustryMaterialMonitorSub { char_id: char_uninserted, owner_id: uninserted_corp, location_id: station_hq, flag_id: sag1 }),
        ]);
        save_industry_material_monitors(snap, &monitors);

        let seed_trit = |snap: &mut eve_db::WorldStateSnapshot, id: i64, owner: i64, loc: i64, flag: InventoryFlag, qty: i32| {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 34,
                    owner_id: owner,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        };
        seed_trit(snap, 991_001, corp_a, station_hq, InventoryFlag::CorpSAG1, 1_500);
        seed_trit(snap, 991_002, corp_a, station_other, InventoryFlag::CorpSAG1, 900);
        seed_trit(snap, 991_003, corp_a, station_hq, InventoryFlag::CorpSAG2, 600);
        seed_trit(snap, 991_004, npc_corp, station_hq, InventoryFlag::CorpSAG1, 400);
        seed_trit(snap, 991_005, uninserted_corp, station_hq, InventoryFlag::CorpSAG1, 300);
    });

    let all_chars = [
        char_ceo,
        char_hq_take1,
        char_hq_query1,
        char_ex_member,
        char_excluded,
        char_npc,
        char_uninserted,
    ];
    for (idx, cid) in all_chars.into_iter().enumerate() {
        ctx.db
            .acquire_online_lock(200 + idx as u64, 900 + idx as u64, Some(cid), true, 1_775_400_000)
            .expect("online lock for emitter test char");
        let _ = ctx.drain_notifications_for_char(cid);
    }

    let collect_monitor_ids = |cid: i64| -> Vec<(i64, i64)> {
        ctx.drain_notifications_for_char(cid)
            .into_iter()
            .filter(|n| n.event == "OnIndustryMaterials")
            .map(|n| {
                let mid = n.args[0].as_i64().expect("monitorID");
                let qty = match &n.args[1] {
                    PyRep::Dict(entries) => entries
                        .iter()
                        .find(|(k, _)| k.as_i64() == Some(34))
                        .and_then(|(_, v)| v.as_i64())
                        .unwrap_or(-1),
                    other => panic!("expected Dict, got {other:?}"),
                };
                (mid, qty)
            })
            .collect()
    };

    // 1. Exercise emit_industry_materials_consumed across HQ, other station, CorpSAG2, NPC corp, and uninserted corp.
    emit_industry_materials_consumed(&ctx, char_ceo, 1, station_hq, station_hq, corp_a, true, InventoryFlag::CorpSAG1, None, &[(34, 100)]);
    emit_industry_materials_consumed(&ctx, char_ceo, 2, station_other, station_other, corp_a, true, InventoryFlag::CorpSAG1, None, &[(34, 100)]);
    emit_industry_materials_consumed(&ctx, char_ceo, 3, station_hq, station_hq, corp_a, true, InventoryFlag::CorpSAG2, None, &[(34, 100)]);
    emit_industry_materials_consumed(&ctx, char_npc, 4, station_hq, station_hq, npc_corp, true, InventoryFlag::CorpSAG1, None, &[(34, 100)]);
    emit_industry_materials_consumed(&ctx, char_uninserted, 5, station_hq, station_hq, uninserted_corp, true, InventoryFlag::CorpSAG1, None, &[(34, 100)]);

    assert_eq!(collect_monitor_ids(char_ceo), vec![(mon_ceo_hq, 1_500), (mon_ceo_other, 900)]);
    assert_eq!(
        collect_monitor_ids(char_hq_take1),
        vec![(mon_take1_hq, 1_500)],
        "HQ Take1 must receive OnIndustryMaterials at HQ CorpSAG1 only (not other station or CorpSAG2)"
    );
    assert_eq!(
        collect_monitor_ids(char_hq_query1),
        vec![(mon_query1_hq, 1_500)],
        "HQ Query1 must receive OnIndustryMaterials at HQ CorpSAG1 only (not other station)"
    );
    assert!(collect_monitor_ids(char_ex_member).is_empty(), "ex-member must not receive OnIndustryMaterials");
    assert!(collect_monitor_ids(char_excluded).is_empty(), "excluded member must not receive OnIndustryMaterials");
    assert!(collect_monitor_ids(char_npc).is_empty(), "NPC corp monitor must not receive OnIndustryMaterials");
    assert!(collect_monitor_ids(char_uninserted).is_empty(), "uninserted corp monitor must not receive OnIndustryMaterials");
    let _ = mon_take1_other;
    let _ = mon_take1_hq_div2;
    let _ = mon_query1_other;
    let _ = mon_ex_hq;
    let _ = mon_excluded_hq;
    let _ = mon_npc_hq;
    let _ = mon_uninserted_hq;

    // 2. Exercise emit_industry_materials_for_scopes with the exact same scopes.
    let sag1 = InventoryFlag::CorpSAG1 as i64;
    let sag2 = InventoryFlag::CorpSAG2 as i64;
    emit_industry_materials_for_scopes(
        &ctx,
        &[
            (corp_a, station_hq, sag1, 34),
            (corp_a, station_other, sag1, 34),
            (corp_a, station_hq, sag2, 34),
            (npc_corp, station_hq, sag1, 34),
            (uninserted_corp, station_hq, sag1, 34),
        ],
    );

    assert_eq!(collect_monitor_ids(char_ceo), vec![(mon_ceo_hq, 1_500), (mon_ceo_other, 900)]);
    assert_eq!(collect_monitor_ids(char_hq_take1), vec![(mon_take1_hq, 1_500)]);
    assert_eq!(collect_monitor_ids(char_hq_query1), vec![(mon_query1_hq, 1_500)]);
    assert!(collect_monitor_ids(char_ex_member).is_empty());
    assert!(collect_monitor_ids(char_excluded).is_empty());
    assert!(collect_monitor_ids(char_npc).is_empty());
    assert!(collect_monitor_ids(char_uninserted).is_empty());
}
