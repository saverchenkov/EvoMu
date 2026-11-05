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
