use std::collections::HashSet;

use eve_testkit::client_contract::{
    validate_notification_contract_with_metrics, ClientContractRatchet,
};
use eve_testkit::notify_ratchet::RawNotifyRatchet;
use serde_json::Value;

use super::client_compat::normalize_macho_notification_with_sde;
use super::notify_registry::{
    all_registered_notifications, OnCapitalSystemChanged, OnCrimewatchEngagementEnded,
    OnMailSent, OnNpcTimerUpdate, OnSeasonStarted, TypedNotification,
};
use eve_proto::PyRep;

fn load_client_notify_spec_map() -> serde_json::Map<String, Value> {
    let exp_path = ClientContractRatchet::default_expectations_path();
    let raw = std::fs::read_to_string(&exp_path).unwrap_or_else(|e| {
        panic!(
            "failed to read client expectations at {}: {e}",
            exp_path.display()
        )
    });
    let root: Value = serde_json::from_str(&raw).expect("valid client_expectations JSON");
    root.get("notify")
        .and_then(|v| v.as_object())
        .cloned()
        .expect("client_expectations JSON must contain top-level 'notify' object")
}

#[test]
fn typed_notification_registry_matches_client_expectations_and_is_non_vacuous() {
    let notify_map = load_client_notify_spec_map();
    let entries = all_registered_notifications();
    assert!(
        entries.len() >= 200,
        "expected >= 200 registered notifications, found {}",
        entries.len()
    );

    let mut seen_events = HashSet::new();
    let mut failures = Vec::new();

    for entry in entries {
        if !seen_events.insert(entry.event) {
            failures.push(format!("duplicate registered event '{}'", entry.event));
            continue;
        }
        if entry.min_arity > entry.max_arity {
            failures.push(format!(
                "{}: invalid arity bounds {}..={}",
                entry.event, entry.min_arity, entry.max_arity
            ));
            continue;
        }
        if !notify_map.contains_key(entry.event) {
            failures.push(format!(
                "{}: registered notification missing from client_expectations['notify']",
                entry.event
            ));
            continue;
        }

        let samples = (entry.sample_payloads)();
        if samples.is_empty() {
            failures.push(format!("{}: sample_payloads() must not be empty", entry.event));
            continue;
        }

        for (idx, sample) in samples.into_iter().enumerate() {
            if sample.len() < entry.min_arity || sample.len() > entry.max_arity {
                failures.push(format!(
                    "{} sample[{idx}]: len {} outside registered arity {}..={}",
                    entry.event,
                    sample.len(),
                    entry.min_arity,
                    entry.max_arity
                ));
                continue;
            }
            let normalized = normalize_macho_notification_with_sde(entry.event, sample, None);
            let (violations, had_unexercised) = validate_notification_contract_with_metrics(
                None,
                &notify_map,
                entry.event,
                &normalized,
            );
            if !violations.is_empty() {
                failures.push(format!(
                    "{} sample[{idx}]: contract violations: {:?}",
                    entry.event, violations
                ));
            }
            if had_unexercised {
                failures.push(format!(
                    "{} sample[{idx}]: had unexercised collection elements (vacuous sample)",
                    entry.event
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "Typed notification registry contract failures ({}):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn typed_notification_validator_rejects_mismatched_arity_and_bad_shape() {
    let notify_map = load_client_notify_spec_map();

    let (empty_mail_violations, _) =
        validate_notification_contract_with_metrics(None, &notify_map, "OnMailSent", &[]);
    assert!(
        !empty_mail_violations.is_empty(),
        "OnMailSent with 0 args must fail contract validation"
    );

    let (bad_crimewatch_violations, _) = validate_notification_contract_with_metrics(
        None,
        &notify_map,
        "OnCrimewatchEngagementEnded",
        &[PyRep::Integer(1), PyRep::Integer(2)],
    );
    assert!(
        !bad_crimewatch_violations.is_empty(),
        "OnCrimewatchEngagementEnded with 2 args must fail 1-arg contract validation"
    );
}

#[test]
fn typed_notification_into_args_preserves_registered_arity() {
    let cw = OnCrimewatchEngagementEnded {
        other_char_id: 90_000_002,
    };
    assert_eq!(OnCrimewatchEngagementEnded::EVENT, "OnCrimewatchEngagementEnded");
    assert_eq!(OnCrimewatchEngagementEnded::DEFAULT_ID_TYPE, "charid");
    assert_eq!(cw.into_args(), vec![PyRep::Integer(90_000_002)]);

    let mail = OnMailSent {
        message_id: 1,
        sender_id: 2,
        sent_date: 4,
        to_character_ids: vec![3],
        to_list_id: None,
        to_corp_or_alliance_id: None,
        title: "hi".to_string(),
        status_mask: 0,
    };
    assert_eq!(mail.into_args().len(), 8);

    let season = OnSeasonStarted {};
    assert_eq!(season.into_args().len(), 0);

    let cap = OnCapitalSystemChanged {
        change: PyRep::Tuple(vec![PyRep::Integer(1), PyRep::None, PyRep::Integer(2)]),
    };
    assert_eq!(OnCapitalSystemChanged::DEFAULT_ID_TYPE, "allianceid");
    assert_eq!(cap.into_args().len(), 1);

    let timer_min = OnNpcTimerUpdate {
        state: 401,
        expiry_time: PyRep::Integer(1_335_000_000_000_000_00),
        do_alert: None,
    };
    assert_eq!(timer_min.into_args().len(), 2);
    let timer_max = OnNpcTimerUpdate {
        state: 401,
        expiry_time: PyRep::Integer(1_335_000_000_000_000_00),
        do_alert: Some(true),
    };
    assert_eq!(timer_max.into_args().len(), 3);
}

#[test]
fn raw_notify_ratchet_holds_and_migrated_files_stay_clean() {
    let ratchet = RawNotifyRatchet::load_default().expect("load raw_notify_ratchet.json");
    let report = ratchet
        .verify_default()
        .expect("raw untyped notify call count must stay at or below falling ratchet");

    let zero_raw_files = [
        "services/notify_emitters_missing.rs",
        "services/notify_emitters_missing2.rs",
        "services/notify_emitters_channel_events.rs",
        "services/mail_notify.rs",
        "services/notify_emitters_operations_nodegraph.rs",
        "services/notify_emitters_fleet_combat.rs",
        "services/notify_emitters_seasons_tactical.rs",
        "services/notify_emitters_skills_trade_godma.rs",
        "services/notify_emitters_character_services.rs",
        "services/fleet_social_notify.rs",
        "services/crimewatch_timers.rs",
        "services/season_manager.rs",
        "services/social_corp_fleet.rs",
        // Wave 5D Batch 2 (Refs #932)
        "services/structure_upwell_ops.rs",
        "services/space_probe_scanning.rs",
        "services/fleet_social_finder_tactical.rs",
        "services/fleet_social_corp_gov.rs",
        "services/fleet_social_corps.rs",
        "services/fleet_social_access_group_bookmarks.rs",
        "services/fleet_social_bookmarks.rs",
        "services/fleet_social.rs",
        "services/fleet_social_corp_tail_dispatch.rs",
        "services/petitioner_ops.rs",
        "services/fleet_social_chat.rs",
        "services/combat_crimewatch.rs",
        "services/character_contacts.rs",
        "services/combat_fighters.rs",
        "services/sov_services.rs",
        "services/fleet_social_scanning.rs",
        "services/project_discovery.rs",
        "services/insurgency_services.rs",
        "services/hypernet_raffle_ops.rs",
        "services/structure_weapons_ops.rs",
        "services/operations_manager.rs",
        "services/mining_advanced.rs",
        "services/social_wars_helpers.rs",
        "services/lp_store.rs",
        "services/wallet_serverops.rs",
        "services/fleet_social_fleets.rs",
        "esi/fleet_routes.rs",
        "services/jump_clone_names.rs",
        "services/wallet_services_ext.rs",
        "services/station_structure.rs",
        "services/fw_services.rs",
        "services/fitting_mgr_service.rs",
        "services/air_npe_nodegraph_runner.rs",
        "services/pve_exploration.rs",
        "services/pve_crab_beacons.rs",
        "services/fleet_social_corp_tail.rs",
        "services/sov_gameplay.rs",
        "services/config_service.rs",
        "services/pvp_filament_arena.rs",
        "services/market_contracts.rs",
        "services/inventory_stacking.rs",
        "services/fitting_ewar_capital.rs",
        "services/corp_registry_ops.rs",
        "services/character_skill_ops.rs",
        "services/abyssal_void_gates.rs",
        "services/space_docking_cloaking.rs",
        "services/market_trading.rs",
        "services/fw_gameplay_insurgency_lifecycle.rs",
        "services/fw_enlistment.rs",
        "services/fleet_social_mail.rs",
        // Batch 4 (#932)
        "services/notify_emitters_space_dungeons.rs",
        "services/agents_missions.rs",
        "services/structure_lifecycle_rpc.rs",
        "services/combat_npcs_drones.rs",
        "services/structure_lifecycle.rs",
        "services/fleet_social_access_group_bookmarks_ops.rs",
        "services/fitting_conduit_jump.rs",
        "services/crimewatch_bounty.rs",
        "services/cosmetics_ui_ops.rs",
        "services/corp_medals_bulletins.rs",
        "services/combat_weapons_spool.rs",
        "services/campaigns_and_milestones.rs",
        "services/tourney_service.rs",
        "services/structure_control_ops.rs",
        "services/space_destiny.rs",
        "services/pve_special_sites.rs",
        "services/operations_manager_helpers.rs",
        "services/mailing_lists_ops.rs",
        "services/fleet_social_fleet_ops.rs",
        "services/fleet_social_calendar_ops.rs",
        "services/fitting_cyno_portal.rs",
        "services/combat_fighters_abilities.rs",
        "services/character_skills_helpers.rs",
        "services/character_expert_systems.rs",
        "services/character_contacts_helpers.rs",
        "services/agents_standings_arcs.rs",
        "services/abyssal_void_mgr.rs",
    ];
    for rel in zero_raw_files {
        let count = report.per_file_raw_notify.get(rel).copied().unwrap_or(0);
        assert_eq!(
            count, 0,
            "migrated file {rel} must have 0 raw untyped notify calls, found {count}"
        );
    }

    let dynamic_helper_files = [
        "services/notify_emitters_ess_bounties.rs",
        "services/notify_emitters_combat_ess_progression.rs",
        "services/notify_emitters_tournaments_anomalies.rs",
    ];
    for rel in dynamic_helper_files {
        let count = report.per_file_raw_notify.get(rel).copied().unwrap_or(0);
        assert!(
            count <= 1,
            "file {rel} must only keep its 1 dynamic helper call, found {count}"
        );
    }
}
