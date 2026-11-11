//! Unit tests for `notify_emitters_space_dungeons`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashSet;
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
fn damage_tuple_fw_jammer_ess_anomalies_and_dungeon_progression() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = 30_000_142_u32;

    // Clamps out-of-range ratios to 0.0..=1.0
    let dmg = build_damage_state_tuple(&ctx, 100_000_001, 1.5, -0.2, 0.5);
    let PyRep::Tuple(ref parts) = dmg else {
        panic!("expected tuple");
    };
    assert_eq!(parts[1], PyRep::Real(0.0));
    assert_eq!(parts[2], PyRep::Real(0.5));

    let row = build_util_row(&["a", "b"], vec![PyRep::Integer(1), PyRep::Integer(2)]);
    assert!(matches!(row, PyRep::Object { ref type_name, .. } if type_name == "util.Row"));

    emit_fw_capture_changed(&ctx, sys_id, 501, 500_001, 25.0, 120_000.0);
    // Normal structure vs Tenebrex Cyno Jammer (37534) online & offline
    emit_structure_cfg_and_jammer_notifications(&ctx, sys_id, 1_030_001, 35_832, "Astra", [0.0; 3], true);
    emit_structure_cfg_and_jammer_notifications(&ctx, sys_id, 1_030_002, 37_534, "Jammer", [0.0; 3], true);
    emit_structure_cfg_and_jammer_notifications(&ctx, sys_id, 1_030_002, 37_534, "Jammer", [0.0; 3], false);

    // ESS bank link notifications
    emit_ess_bank_link_notifications(&ctx, sys_id, char_id, "AttemptLinkToMainBank", true, 0.0);
    emit_ess_bank_link_notifications(&ctx, sys_id, char_id, "AttemptLinkToMainBank", false, 5_000_000.0);
    emit_ess_bank_link_notifications(&ctx, sys_id, char_id, "AttemptLinkToReserveBank", true, 0.0);
    emit_ess_main_bank_link_disconnected(&ctx, sys_id, char_id, "OutOfRange");

    // Spawn cosmic anomaly & signature, then test exiting dungeon at position
    let (anom_id, inst_id) = spawn_and_emit_cosmic_anomaly(&ctx, sys_id, 10_001);
    assert!(anom_id.starts_with("ANM-"));
    assert!(inst_id > 910_000_000);
    emit_cosmic_signature_added(&ctx, sys_id, "SIG-900", "Guristas Hideout", [1.0, 2.0, 3.0]);
    emit_exiting_dungeon_on_warp_if_present(&ctx, char_id, sys_id, [175_000.0, 25_000.0, -40_000.0]);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.fleet_id = Some(801);
        }
        snap.fleets.insert(
            801,
            FleetRecord {
                fleet_id: 801,
                boss_id: char_id,
                members: vec![char_id],
                motd: String::new(),
                free_move: false,
                is_registered: false,
                ..Default::default()
            },
        );
    });
    emit_escalation_created_or_advanced(&ctx, char_id, 777, 10_001, true);
    emit_dungeon_progression_notifications(
        &ctx,
        char_id,
        sys_id,
        "Expedition",
        "DedSite",
        10_001,
        inst_id,
        1,
        [0.0, 0.0, 0.0],
        1,
        3,
        false,
        1_000_000,
    );
    emit_dungeon_progression_notifications(
        &ctx,
        char_id,
        sys_id,
        "Distribution",
        "StdSite",
        10_002,
        inst_id + 1,
        2,
        [0.0, 0.0, 0.0],
        2,
        2,
        true,
        1_000_000,
    );
    emit_dungeon_edit(&ctx, char_id, 10_001, 1, [10.0, 20.0, 30.0]);

    let drained = ctx.drain_notifications_for_char(char_id);
    let events: HashSet<&str> = drained.iter().map(|n| n.event.as_str()).collect();
    assert!(events.contains("OnCaptureChanged"));
    assert!(events.contains("OnCfgDataChanged"));
    assert!(events.contains("OnCynoJammerChanged"));
    assert!(events.contains("OnESSMainBankLinkNotification"));
    assert!(events.contains("OnMainBankPlayerHackSuccess"));
    assert!(events.contains("OnESSReserveBankLinkNotification"));
    assert!(events.contains("OnMainBankPlayerLinkDisconnected"));
    assert!(events.contains("OnCosmicAnomalyAdded"));
    assert!(events.contains("OnCosmicSignatureAdded"));
    assert!(events.contains("OnExitingDungeon"));
    assert!(events.contains("OnEscalatingPathChange"));
    assert!(events.contains("OnEscalatingPathMessage"));
    assert!(events.contains("OnFleetEscalatingPathMessage"));
    assert!(events.contains("OnEnteringDungeonRoom"));
    assert!(events.contains("OnDungeonValues"));
    assert!(events.contains("OnProgressionRoomJoined"));
    assert!(events.contains("OnEscalatingPathDungeonEntered"));
    assert!(events.contains("OnDistributionDungeonEntered"));
    assert!(events.contains("OnDungeonCompleted"));
    assert!(events.contains("OnDungeonEdit"));
}

#[test]
fn godma_ewar_drones_cloak_pvp_deathzone_and_dungeon_trigger_emitters() {
    let (ctx, char_id) = make_online_ctx();
    let sys_id = 30_000_142_u32;
    let ship_id = 100_000_001_i64;
    let mod_id = 920_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 440,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    emit_drone_activity_change(&ctx, char_id, 930_001, 1, Some("guard"));
    emit_drone_activity_change(&ctx, char_id, 930_001, 0, None);
    emit_module_activation_godma_notifications(
        &ctx,
        char_id,
        ship_id,
        mod_id,
        440,
        "online",
        5.0,
        Some(999_001),
    );

    ctx.active_ewar_starts
        .write()
        .unwrap()
        .insert((ship_id, mod_id), vec![(999_001, "warpScrambler")]);
    let pairs = flush_ship_all_ewar_sync(&ctx, char_id, Some(sys_id), ship_id);
    assert_eq!(pairs, vec![(ship_id, mod_id)]);

    emit_ecm_jam_landed(&ctx, sys_id, char_id, ship_id, mod_id, 999_001, 20.0);
    emit_fleet_boosted(&ctx, char_id, 42526, 4);
    emit_fleet_jump_beacon_deployable_change(&ctx, None, 10, sys_id, char_id, true);
    emit_incursion_influence_update(&ctx, 20_000_020, 0.8);
    emit_jump_cloak_started(&ctx, char_id, ship_id);
    emit_warp_cloak_or_invuln_cancelled(&ctx, char_id, ship_id, true, true);
    emit_kill_notifications(&ctx, char_id, char_id);
    emit_local_hack_progress_updated(&ctx, sys_id, 800_001, 1.5);
    emit_pvp_match_found_and_expiry(&ctx, char_id, 1_775_401_000);
    emit_pvp_matchmaking_failure(&ctx, char_id, 42);
    emit_destiny_updates(&ctx, char_id, vec![PyRep::Integer(1)]);
    emit_cloak_module_force_deactivated(&ctx, char_id, "Decloaked", 1200.0);
    emit_component_removed_system_environment(&ctx, sys_id);
    emit_deathzone_state_update(&ctx, char_id, sys_id, ship_id, true, 15_000.0);
    emit_deathzone_state_update(&ctx, char_id, sys_id, ship_id, false, 0.0);
    emit_dungeon_camera_interest(&ctx, char_id, 701, 2.0, 5.0);
    emit_dungeon_trigger_ambient_sound(&ctx, char_id, 100, "amb", 701, 702);
    emit_dungeon_trigger_audio(&ctx, char_id, 100, "res:/Audio/a.ogg");
    emit_dungeon_trigger_message(&ctx, char_id, "warn", 123);
    emit_dungeon_trigger_music(&ctx, char_id, 100, 456);
    emit_enter_tournament_system(&ctx, char_id);
    emit_leave_tournament_system(&ctx, char_id);
    emit_ess_reserve_bank_inventory_full(&ctx, sys_id);
    emit_jump_queue_update(&ctx, char_id, sys_id, 10, 2, vec![1, 2], 60);

    let drained = ctx.drain_notifications_for_char(char_id);
    let events: HashSet<&str> = drained.iter().map(|n| n.event.as_str()).collect();
    assert!(events.contains("OnDroneActivityChange"));
    assert!(events.contains("OnGodmaEffectInfo"));
    assert!(events.contains("OnGodmaPrimeItem"));
    assert!(events.contains("OnGodmaFlushLocationProfile"));
    assert!(events.contains("OnEwarEnd"));
    assert!(events.contains("OnJamStart"));
    assert!(events.contains("OnFleetBoosted"));
    assert!(events.contains("OnInfluenceUpdate"));
    assert!(events.contains("OnJumpCloakUpdated"));
    assert!(events.contains("OnJumpCloakCancelled"));
    assert!(events.contains("OnInvulnCancelled"));
    assert!(events.contains("OnKillNotification"));
    assert!(events.contains("OnLocalHackProgressUpdated"));
    assert!(events.contains("OnMatchFound"));
    assert!(events.contains("OnProvingGroundsMatchFound"));
    assert!(events.contains("OnAddExpiryDurationForAbyssalPvpContent"));
    assert!(events.contains("OnMatchMakingFailure"));
    assert!(events.contains("DoDestinyUpdates"));
    assert!(events.contains("OnCloakModuleForceDeactivated"));
    assert!(events.contains("OnComponentRemovedSystemEnvironment"));
    assert!(events.contains("OnDeathzoneStateUpdate"));
    assert!(events.contains("OnDungeonCameraInterest"));
    assert!(events.contains("OnDungeonTriggerAmbientSound"));
    assert!(events.contains("OnDungeonTriggerAudio"));
    assert!(events.contains("OnDungeonTriggerMessage"));
    assert!(events.contains("OnDungeonTriggerMusic"));
    assert!(events.contains("OnEnterTournamentSystem"));
    assert!(events.contains("OnLeaveTournamentSystem"));
    assert!(events.contains("OnEssReserveBankInventoryFull"));
    assert!(events.contains("OnJumpQueueUpdate"));

    let effect_info = drained
        .iter()
        .find(|n| n.event == "OnGodmaEffectInfo")
        .expect("OnGodmaEffectInfo notification");
    assert_eq!(effect_info.args.len(), 4);
    assert_eq!(effect_info.args[0], PyRep::Integer(mod_id));
    assert_eq!(effect_info.args[1], PyRep::Integer(440));
    let PyRep::Dict(active_entries) = &effect_info.args[2] else {
        panic!("expected activeEffects dict in OnGodmaEffectInfo");
    };
    assert_eq!(active_entries.len(), 1);
    let (eff_key, PyRep::List(line)) = &active_entries[0] else {
        panic!("expected active effect list entry");
    };
    assert!(matches!(eff_key, PyRep::Integer(_)));
    assert_eq!(line.len(), 10);
    assert_eq!(line[0], PyRep::Integer(mod_id));
    assert_eq!(line[1], PyRep::Integer(char_id));
    assert_eq!(line[2], PyRep::Integer(ship_id));
    assert_eq!(line[3], PyRep::Integer(999_001));
    assert_eq!(line[4], PyRep::None);
    assert_eq!(line[5], PyRep::List(Vec::new()));
    assert_eq!(&line[6], eff_key);
    assert!(matches!(line[7], PyRep::Integer(ft) if ft > i32::MAX as i64));
    assert_eq!(line[8], PyRep::Integer(5000));
    assert_eq!(line[9], PyRep::Integer(0));

    let prime_item = drained
        .iter()
        .find(|n| n.event == "OnGodmaPrimeItem")
        .expect("OnGodmaPrimeItem notification");
    assert_eq!(prime_item.args.len(), 2);
    assert_eq!(prime_item.args[0], PyRep::Integer(ship_id));
    let PyRep::KeyVal(kv) = &prime_item.args[1] else {
        panic!("expected KeyVal in OnGodmaPrimeItem");
    };
    assert_eq!(kv.get("activeEffects"), Some(&effect_info.args[2]));
}

#[test]
fn on_godma_prime_item_preserves_skill_and_ship_dogma_bonuses() {
    let (ctx, char_id) = make_online_ctx();
    let ship_id = 920_100_i64;
    let mod_id = 920_101_i64;
    let mod_type_id = 484_u32; // 125mm Gatling AutoCannon I

    let base_type = ctx.sde.types.get(&mod_type_id).expect("484 in seed SDE");
    let raw_speed = base_type.default_attributes[&51];
    let raw_dmg_mult = base_type.default_attributes[&64];

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
        }
        let mut sk = snap.skills.entry(char_id).or_default().clone();
        sk.insert(
            3300, // Gunnery V (+10% ROF -> lower speed attr 51)
            eve_db::TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            3301, // Small Projectile Turret V (+25% damageMultiplier attr 64)
            eve_db::TrainedSkillRecord {
                type_id: 3301,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.skills.insert(char_id, sk);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: mod_type_id,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: std::collections::HashMap::from([(3, 12.5)]),
                ..Default::default()
            },
        );
        snap.module_online_states.insert(mod_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    let fit = ctx
        .get_or_rebuild_ship_fitting(char_id, ship_id)
        .expect("ship fit built");
    let m_state = fit.module_state(mod_id, &ctx.sde).expect("module state");
    let expected_speed = m_state.get_attribute(51, &ctx.sde);
    let expected_dmg_mult = m_state.get_attribute(64, &ctx.sde);
    assert!(
        expected_speed < raw_speed,
        "Gunnery V should reduce attr 51 speed ({expected_speed} < {raw_speed})"
    );
    assert!(
        expected_dmg_mult > raw_dmg_mult,
        "Small Projectile Turret V should boost attr 64 damageMultiplier ({expected_dmg_mult} > {raw_dmg_mult})"
    );

    emit_module_activation_godma_notifications(
        &ctx,
        char_id,
        ship_id,
        mod_id,
        mod_type_id,
        "online",
        expected_speed / 1_000.0,
        None,
    );

    let drained = ctx.drain_notifications_for_char(char_id);
    let prime_item = drained
        .iter()
        .find(|n| n.event == "OnGodmaPrimeItem")
        .expect("OnGodmaPrimeItem emitted");
    let PyRep::KeyVal(kv) = &prime_item.args[1] else {
        panic!("expected KeyVal in OnGodmaPrimeItem");
    };
    let Some(PyRep::Dict(entries)) = kv.get("attributes") else {
        panic!("expected attributes Dict");
    };
    let attrs: std::collections::HashMap<i64, f64> = entries
        .iter()
        .filter_map(|(k, v)| match (k.as_i64(), v) {
            (Some(aid), PyRep::Real(val)) => Some((aid, *val)),
            _ => None,
        })
        .collect();
    assert!((attrs[&51] - expected_speed).abs() < 1e-6);
    assert!((attrs[&64] - expected_dmg_mult).abs() < 1e-6);
    assert_eq!(attrs.get(&3), Some(&12.5), "dynamic runtime damage attr preserved");
    assert_eq!(attrs.get(&2), Some(&1.0), "online attr preserved");
}

#[cfg(test)]
#[path = "notify_emitters_space_dungeons_extra_unit_tests.rs"]
mod notify_emitters_space_dungeons_extra_unit_tests;
