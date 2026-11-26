use std::collections::HashSet;

use eve_testkit::client_contract::{
    validate_notification_contract_with_metrics, ClientContractRatchet,
};
use eve_testkit::notify_ratchet::RawNotifyRatchet;
use serde_json::Value;

use super::client_compat::normalize_macho_notification_with_sde;
use super::notify_registry::{
    all_registered_notifications, BallparkScene, GodmaEffectStartOrTime, OnCapitalSystemChanged,
    OnCrimewatchEngagementEnded, OnMailSent, OnNpcTimerUpdate, OnSeasonStarted, ToNotifyArg,
    TypedNotification,
};
use eve_db::clock::{SimFileTime, WallFileTime};
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

fn check_normalized_notification_contract(
    notify_map: &serde_json::Map<String, Value>,
    event: &str,
    args: Vec<PyRep>,
) -> (Vec<String>, bool) {
    let normalized = normalize_macho_notification_with_sde(event, args, None);
    validate_notification_contract_with_metrics(None, notify_map, event, &normalized)
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
            let (violations, had_unexercised) =
                check_normalized_notification_contract(&notify_map, entry.event, sample);
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
fn typed_issue_1562_and_sibling_notification_contracts() {
    use super::notify_registry::{
        FleetInviteMsgDict, FleetInviteMsgName, OnFittingAdded, OnFittingsUpdated, OnFleetInvite,
        OnShipJumpCloneInstallationDone, OnShipJumpCloneInstallationOffered, OnSlimItemChange,
        ShipJumpCloneInstallationArgs, SlimItemKeyVal, SpaceShipSlimFields, UpdatedFittingKeyVal,
        UpdatedFittingPayload,
    };
    let notify_map = load_client_notify_spec_map();
    let payloads = [
        (
            OnFittingsUpdated::EVENT,
            OnFittingsUpdated {
                fitting: UpdatedFittingPayload::Fitting(UpdatedFittingKeyVal {
                    fitting_id: 42,
                    owner_id: 90_000_001,
                    ship_type_id: 587,
                    name: "Rifter".into(),
                    description: String::new(),
                    fit_data: vec![(3170, 12, 1)],
                }),
            }
            .into_args(),
        ),
        (
            OnFittingsUpdated::EVENT,
            OnFittingsUpdated {
                fitting: UpdatedFittingPayload::Cleared,
            }
            .into_args(),
        ),
        (
            OnFittingAdded::EVENT,
            OnFittingAdded {
                owner_id: 90_000_001,
                fit_id: 42,
            }
            .into_args(),
        ),
        (
            OnShipJumpCloneInstallationOffered::EVENT,
            OnShipJumpCloneInstallationOffered {
                args: ShipJumpCloneInstallationArgs {
                    offering_char_id: 90_000_001,
                    target_char_id: 90_000_002,
                    ship_id: 100_000_001,
                    offered: true,
                },
            }
            .into_args(),
        ),
        (
            OnShipJumpCloneInstallationDone::EVENT,
            OnShipJumpCloneInstallationDone {
                args: ShipJumpCloneInstallationArgs {
                    offering_char_id: 90_000_001,
                    target_char_id: 90_000_002,
                    ship_id: 100_000_001,
                    offered: true,
                },
            }
            .into_args(),
        ),
        (
            OnSlimItemChange::EVENT,
            OnSlimItemChange {
                scene: BallparkScene::try_solar_system(30_000_142).expect("non-zero scene"),
                old_slim: PyRep::Integer(100_000_001),
                new_slim: SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
                    item_id: 100_000_001,
                    type_id: 587,
                    group_id: 25,
                    category_id: 6,
                    owner_id: 90_000_001,
                    char_id: Some(90_000_001),
                    corp_id: Some(98_000_001),
                    alliance_id: None,
                    name: "Wolfpack".into(),
                }),
            }
            .into_args(),
        ),
        (
            OnFleetInvite::EVENT,
            OnFleetInvite {
                fleet_id: 1,
                invite_id: 90_000_001,
                msg_name: FleetInviteMsgName::AskJoinFleet,
                msg_dict: FleetInviteMsgDict { auto_accept: false },
            }
            .into_args(),
        ),
    ];
    for (event, args) in payloads {
        let (violations, _) = check_normalized_notification_contract(&notify_map, event, args);
        assert!(violations.is_empty(), "{event} violations: {violations:?}");
    }
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
        sent_date: WallFileTime::from_ticks(4),
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
        expiry_time: Some(SimFileTime::from_ticks(1_335_000_000_000_000_00)),
        do_alert: None,
    };
    assert_eq!(timer_min.into_args().len(), 2);
    let timer_max = OnNpcTimerUpdate {
        state: 401,
        expiry_time: Some(SimFileTime::from_ticks(1_335_000_000_000_000_00)),
        do_alert: Some(true),
    };
    assert_eq!(timer_max.into_args().len(), 3);
    let timer_idle = OnNpcTimerUpdate {
        state: 400,
        expiry_time: None,
        do_alert: None,
    };
    assert_eq!(
        timer_idle.into_args(),
        vec![PyRep::Integer(400), PyRep::None]
    );

    use super::notify_registry::{
        DoBallsAddedShipRef, OnAsteroidRevealed, OnAsteroidRevealedDetailed, OnGodmaFlushLocation,
        OnHeatAdded, OnHeatRemoved, OnMultiModuleRepairStarted, OnMultipleItemChange,
        OnNewMoonProbe, OnRepairDone, OnShipInsured, OnSpecialFX, OnStanceActive,
        OnSystemWideEffectAdd, OnSystemWideEffectRemove, OnTargetClear,
        OnUpdateSystemWideEffectsInfo,
    };
    assert_eq!(OnAsteroidRevealed::DEFAULT_ID_TYPE, "character");
    assert_eq!(OnAsteroidRevealed { asteroid_id: 100_000_900 }.into_args().len(), 1);
    assert_eq!(OnAsteroidRevealedDetailed::EVENT, "OnAsteroidRevealed");
    assert_eq!(OnAsteroidRevealedDetailed::DEFAULT_ID_TYPE, "character");
    assert_eq!(
        OnAsteroidRevealedDetailed {
            asteroid_id: 100_000_900,
            type_id: 1230,
            quantity: 5000,
        }
        .into_args()
        .len(),
        3
    );
    assert_eq!(DoBallsAddedShipRef::EVENT, "DoBallsAdded");
    assert_eq!(DoBallsAddedShipRef::DEFAULT_ID_TYPE, "solarsystemid");
    assert_eq!(
        DoBallsAddedShipRef {
            item_id: 100_000_010,
            type_id: 587,
        }
        .into_args()
        .len(),
        2
    );

    // Regression (#1616): OnFittingAdded must encode (owner_id, fit_id) with scalar integer fit_id
    // (NOT a Python list), matching V24 fittingSvc.OnFittingAdded(self, ownerID, fitID).
    use super::notify_registry::{build_pending_typed_notification, OnFittingAdded};
    let added = OnFittingAdded {
        owner_id: 90_000_001,
        fit_id: 42,
    };
    assert_eq!(OnFittingAdded::EVENT, "OnFittingAdded");
    assert_eq!(OnFittingAdded::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        added.clone().into_args(),
        vec![PyRep::Integer(90_000_001), PyRep::Integer(42)]
    );
    let pending = build_pending_typed_notification(
        90_000_001,
        Some(1),
        OnFittingAdded::DEFAULT_ID_TYPE,
        0,
        added,
    );
    assert_eq!(
        pending.args,
        vec![PyRep::Integer(90_000_001), PyRep::Integer(42)]
    );
    let wire_rep = eve_proto::marshal::decode(&pending.marshaled_packet)
        .expect("decode marshaled OnFittingAdded MachoPacket");
    let wire_pkt = eve_machonet::MachoPacket::from_pyrep(&wire_rep)
        .expect("parse MachoPacket from decoded wire PyRep");
    let PyRep::Tuple(outer) = &wire_pkt.payload else {
        panic!("expected tuple payload on wire OnFittingAdded");
    };
    let PyRep::Tuple(inner) = &outer[0] else {
        panic!("expected inner tuple on wire OnFittingAdded");
    };
    let PyRep::SubStream(sub) = &inner[1] else {
        panic!("expected SubStream on wire OnFittingAdded");
    };
    let PyRep::Tuple(sub_t) = sub.as_ref() else {
        panic!("expected SubStream tuple on wire OnFittingAdded");
    };
    let PyRep::Tuple(call_t) = &sub_t[1] else {
        panic!("expected call tuple inside SubStream");
    };
    assert_eq!(
        call_t[1],
        PyRep::Tuple(vec![PyRep::Integer(90_000_001), PyRep::Integer(42)]),
        "wire OnFittingAdded second argument must be scalar Integer, not List"
    );
    assert_eq!(OnGodmaFlushLocation { location_id: 100_000_010 }.into_args().len(), 1);
    assert_eq!(
        OnHeatAdded {
            heat_id: 1175,
            module_id: 100_000_020,
        }
        .into_args(),
        vec![PyRep::Integer(1175), PyRep::Integer(100_000_020)]
    );
    assert_eq!(
        OnHeatRemoved {
            heat_id: 1176,
            module_id: 100_000_021,
        }
        .into_args(),
        vec![PyRep::Integer(1176), PyRep::Integer(100_000_021)]
    );
    assert_eq!(
        OnMultiModuleRepairStarted {
            module_ids: vec![PyRep::Integer(100_000_020)],
        }
        .into_args()
        .len(),
        1
    );
    assert_eq!(
        OnMultipleItemChange {
            items: Vec::new(),
            change: PyRep::Dict(Vec::new()),
        }
        .into_args()
        .len(),
        2
    );
    assert_eq!(
        OnNewMoonProbe {
            probe: PyRep::keyval([("probeID", PyRep::Integer(1))]),
        }
        .into_args()
        .len(),
        1
    );
    assert_eq!(OnRepairDone { item_ids: vec![100_000_020] }.into_args().len(), 1);
    assert_eq!(OnShipInsured {}.into_args().len(), 0);
    assert_eq!(
        OnSpecialFX {
            scene: BallparkScene::try_solar_system(30_000_142).expect("non-zero scene"),
            ship_id: 100_000_010,
            module_id: Some(100_000_020),
            module_type_id: Some(483),
            target_id: None,
            other_type_id: None,
            guid: "effects.Laser".to_string(),
            is_offensive: false,
            start: 0,
            active: 0,
            duration: 0.0,
            repeat: 0,
            start_time: SimFileTime::from_ticks(1_335_000_000_000_000_00),
            time_from_start: 0.0,
            graphic_info: None,
        }
        .into_args()
        .len(),
        14
    );
    assert_eq!(OnStanceActive { ship_id: 100_000_010, stance_id: 1 }.into_args().len(), 2);
    assert_eq!(
        OnSystemWideEffectAdd {
            source_item_id: 30_000_142,
            source_type_id: 30_574,
        }
        .into_args()
        .len(),
        2
    );
    assert_eq!(OnSystemWideEffectRemove { source_item_id: 30_000_142 }.into_args().len(), 1);
    assert_eq!(OnUpdateSystemWideEffectsInfo::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        OnUpdateSystemWideEffectsInfo {
            system_wide_effects_on_ship: PyRep::Dict(vec![(
                PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::Integer(30_844)]),
                PyRep::Set(vec![PyRep::Integer(6_753)]),
            )]),
        }
        .into_args()
        .len(),
        1
    );
    assert_eq!(OnTargetClear { targets_to_skip: None }.into_args().len(), 0);
    assert_eq!(
        OnTargetClear {
            targets_to_skip: Some(vec![100_000_010]),
        }
        .into_args()
        .len(),
        1
    );

    use super::notify_registry::{
        OnGodmaShipEffect, OnMachoObjectDisconnect, OnMultiEvent, OnNonDiminishingInjectionsUsed,
        OnReprocessingMaterialsChanged, OnServerBoostersChanged, OnServerImplantsChanged,
        OnShipCosmeticChanged, OnShipCosmeticsChanged, OnShipcasterTargetStateChanged,
        OnSkyhookFirstActivation, OnSkyhookSiloDamageUpdated, OnSovereigntyAudioEvent,
        OnSurveyRewardsAvailable, OnWreckLootAll, PlanetExtractorFinished, PlanetStorageFull,
    };
    assert_eq!(OnMachoObjectDisconnect::DEFAULT_ID_TYPE, "clientID");
    assert_eq!(
        OnMachoObjectDisconnect {
            object_id: PyRep::ascii_bytes("N=1:1".to_string()),
            client_id: 1_000_000_000_001,
            ref_id: 1,
        }
        .into_args()
        .len(),
        3
    );
    assert_eq!(OnMultiEvent::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        OnMultiEvent {
            events: vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![
                    PyRep::String("OnTarget".into()),
                    PyRep::String("add".into()),
                    PyRep::Integer(100_000_010),
                    PyRep::String(String::new()),
                ]),
                PyRep::Integer(0),
            ])],
        }
        .into_args()
        .len(),
        1
    );
    assert_eq!(OnNonDiminishingInjectionsUsed::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        OnNonDiminishingInjectionsUsed { amount: 1 }.into_args(),
        vec![PyRep::Integer(1)]
    );
    assert_eq!(OnReprocessingMaterialsChanged::DEFAULT_ID_TYPE, "charid");
    assert!(OnReprocessingMaterialsChanged.into_args().is_empty());
    assert_eq!(OnServerBoostersChanged::DEFAULT_ID_TYPE, "charid");
    assert!(OnServerBoostersChanged.into_args().is_empty());
    assert_eq!(OnServerImplantsChanged::DEFAULT_ID_TYPE, "charid");
    assert!(OnServerImplantsChanged.into_args().is_empty());
    assert_eq!(
        OnShipcasterTargetStateChanged::DEFAULT_ID_TYPE,
        "solarsystemid"
    );
    assert_eq!(
        OnShipcasterTargetStateChanged {
            item_id: 1_000_000_000_001,
        }
        .into_args(),
        vec![PyRep::Integer(1_000_000_000_001)]
    );
    assert_eq!(OnShipCosmeticChanged::DEFAULT_ID_TYPE, "solarsystemid");
    assert_eq!(
        OnShipCosmeticChanged {
            ship_id: 100_000_010,
            slot_index: 0,
            cosmetic: PyRep::Integer(2),
        }
        .into_args()
        .len(),
        3
    );
    assert_eq!(OnShipCosmeticsChanged::DEFAULT_ID_TYPE, "solarsystemid");
    assert_eq!(
        OnShipCosmeticsChanged {
            ship_id: 100_000_010,
            cosmetics: PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(2))]),
        }
        .into_args()
        .len(),
        2
    );
    assert_eq!(OnSkyhookFirstActivation::DEFAULT_ID_TYPE, "solarsystemid");
    assert_eq!(
        OnSkyhookFirstActivation {
            skyhook_id: 1_000_000_000_001,
            planet_id: 40_000_001,
        }
        .into_args(),
        vec![
            PyRep::Integer(1_000_000_000_001),
            PyRep::Integer(40_000_001),
        ]
    );
    assert_eq!(OnSkyhookSiloDamageUpdated::DEFAULT_ID_TYPE, "solarsystemid");
    assert_eq!(
        OnSkyhookSiloDamageUpdated {
            shield_percentage: 0.75,
        }
        .into_args(),
        vec![PyRep::Real(0.75)]
    );
    assert_eq!(OnSurveyRewardsAvailable::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        OnSurveyRewardsAvailable { survey_id: 2801 }.into_args(),
        vec![PyRep::Integer(2801)]
    );
    assert_eq!(OnWreckLootAll::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        OnWreckLootAll {
            inv_id: 100_000_900,
            items: vec![100_000_901, 100_000_902],
        }
        .into_args(),
        vec![
            PyRep::Integer(100_000_900),
            PyRep::List(vec![
                PyRep::Integer(100_000_901),
                PyRep::Integer(100_000_902),
            ]),
        ]
    );
    assert_eq!(PlanetExtractorFinished::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        PlanetExtractorFinished {
            planet_id: 40_000_001,
            pin_id: 701,
        }
        .into_args(),
        vec![PyRep::Integer(40_000_001), PyRep::Integer(701)]
    );
    assert_eq!(PlanetStorageFull::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        PlanetStorageFull {
            planet_id: 40_000_001,
            pin_id: 501,
        }
        .into_args(),
        vec![PyRep::Integer(40_000_001), PyRep::Integer(501)]
    );
    assert_eq!(OnSovereigntyAudioEvent::DEFAULT_ID_TYPE, "solarsystemid");
    assert_eq!(
        OnSovereigntyAudioEvent {
            event_id: 1,
            text_params: PyRep::Dict(vec![(
                PyRep::String("solarSystemID".into()),
                PyRep::Integer(30_000_142),
            )]),
        }
        .into_args()
        .len(),
        2
    );
    assert_eq!(
        OnGodmaShipEffect {
            item_id: 100_000_020,
            effect: PyRep::Integer(10),
            start_or_time: GodmaEffectStartOrTime::SimTime(SimFileTime::from_ticks(
                1_335_000_000_000_000_00
            )),
            start: Some(PyRep::Integer(0)),
            active: Some(PyRep::Integer(0)),
            environment: Some(PyRep::Tuple(Vec::new())),
            start_time: Some(SimFileTime::from_ticks(1_335_000_000_000_000_00)),
            duration: Some(PyRep::Real(0.0)),
            repeat: Some(PyRep::Integer(0)),
            error: Some(PyRep::None),
            actual_stop_time: None,
        }
        .into_args()
        .len(),
        10
    );
    assert_eq!(
        OnGodmaShipEffect {
            item_id: 100_000_020,
            effect: PyRep::Integer(10),
            start_or_time: GodmaEffectStartOrTime::SimTime(SimFileTime::from_ticks(
                1_335_000_000_000_000_00
            )),
            start: Some(PyRep::Integer(0)),
            active: Some(PyRep::Integer(0)),
            environment: Some(PyRep::Tuple(Vec::new())),
            start_time: Some(SimFileTime::from_ticks(1_335_000_000_000_000_00)),
            duration: Some(PyRep::Real(0.0)),
            repeat: Some(PyRep::Integer(0)),
            error: Some(PyRep::None),
            actual_stop_time: Some(Some(SimFileTime::from_ticks(1_335_000_050_000_000_00))),
        }
        .into_args()
        .len(),
        11
    );
    let wall_sample = WallFileTime::from_ticks(1_335_000_000_000_000_00);
    let sim_sample = SimFileTime::from_ticks(1_334_997_000_000_000_00);
    assert_eq!(
        wall_sample.into_notify_arg(),
        PyRep::Integer(1_335_000_000_000_000_00)
    );
    assert_eq!(
        sim_sample.into_notify_arg(),
        PyRep::Integer(1_334_997_000_000_000_00)
    );

    use super::notify_registry::{OnMoonScanComplete, OnRemoveMoonProbe, OnScanNoMaterials};
    assert_eq!(OnMoonScanComplete::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        OnMoonScanComplete {
            moon_id: 40_000_001,
            results: PyRep::Dict(vec![(PyRep::Integer(45_490), PyRep::Real(0.25))]),
        }
        .into_args()
        .len(),
        2
    );
    assert_eq!(OnRemoveMoonProbe::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        OnRemoveMoonProbe { probe_id: 1 }.into_args(),
        vec![PyRep::Integer(1)]
    );
    assert_eq!(OnScanNoMaterials::DEFAULT_ID_TYPE, "charid");
    assert_eq!(
        OnScanNoMaterials {
            moon_id: 40_000_001,
        }
        .into_args(),
        vec![PyRep::Integer(40_000_001)]
    );
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
        "sp6_lifecycle.rs",
        "sp6_lifecycle/drone_return.rs",
        "services/combat_weapons.rs",
        "services/doomsday_aoe_ops.rs",
        "services/tactical_modules_superweapon_ops.rs",
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
        // Wave 5D Batch 5 (Refs #932)
        "sp6_lifecycle/cluster_tick.rs",
        "services/wallet_ledger.rs",
        "services/tourney_return_ops.rs",
        "services/tourney_arena.rs",
        "services/structure_profiles_rpc.rs",
        "services/space_warp_jump_helpers.rs",
        "services/space_warp_jump.rs",
        "services/space_entity_ops.rs",
        "services/space_cyno_filaments.rs",
        "services/serverops_cluster.rs",
        "services/repair_service.rs",
        "services/pve_hacking_sites.rs",
        "services/pve_expeditions.rs",
        "services/pve_crab_beacons_ops.rs",
        "services/pve_abyssal_lifecycle.rs",
        "services/pi_orbitals.rs",
        "services/pi_orbital_registry.rs",
        "services/pi_colonies.rs",
        "services/nav_warp_interdiction.rs",
        "services/macho_cluster_mgmt.rs",
        "services/insurgency_complex_ops_ext.rs",
        "services/industry_pi.rs",
        "services/fitting_ship_bays.rs",
        "services/fitting_containers_ships.rs",
        "services/fitting_cycles_overheat.rs",
        "services/fitting_dogma_im.rs",
        "services/fitting_dogma_ops.rs",
        "services/fitting_insurance_repair.rs",
        "services/fitting_mining_lasers.rs",
        "services/fitting_module_cycles.rs",
        "services/fitting_module_cycles_helpers.rs",
        "services/fitting_module_cycles_pending_actions.rs",
        "services/fitting_saved_apply.rs",
        "services/fitting_saved_multifit.rs",
        "services/fitting_scanners_superweapons.rs",
        "services/fitting_ship_bays_store_ops.rs",
        "services/fitting_ships_mastery.rs",
        "services/contracts_service.rs",
        "services/combat_npc_police.rs",
        "services/character_energy.rs",
        "services/character_clones_implants.rs",
        "services/campaigns_milestones_playtime.rs",
        "services/air_npe_nodegraph_pocket.rs",
        // RFC-3 C-8 Disjoint Batch (Refs #932)
        "esi/character_routes.rs",
        "esi/universe_routes.rs",
        "router/binding.rs",
        "services/agents_mission_rewards.rs",
        "services/character_clone_jump_rules.rs",
        "services/character_skills.rs",
        "services/combat_missiles_aoe.rs",
        "services/combat_wrecks_loot_ops.rs",
        "services/crate_loot_service.rs",
        "services/implant_plugging.rs",
        "services/mining_cycle.rs",
        "services/mobile_deployables_ext.rs",
        "services/operations_survey_handlers.rs",
        "services/pi_helpers.rs",
        "services/pve_special_sites_combat.rs",
        "services/pve_special_sites_ops.rs",
        "services/quasar_cosmetics_live.rs",
        "services/shipcaster_ops.rs",
        "services/shipcaster_ops_starter.rs",
        "services/sov_gameplay_entosis.rs",
        "services/sov_gameplay_entosis_capture.rs",
        "services/sov_gameplay_skyhook.rs",
        "services/space_entity_mining.rs",
        "services/structure_acl_defense_rules.rs",
        "services/structure_lifecycle_cargo_fuel.rs",
        "services/structure_profile_services.rs",
        // Final Disjoint Sweep (Refs #932)
        "combat/death_pipeline.rs",
        "router/context.rs",
        "router/session_transition.rs",
        "services/character_gm_tools.rs",
        "services/inventory_fitting.rs",
        "services/space_nav_travel.rs",
        "services/tactical_modules.rs",
    ];
    for rel in zero_raw_files {
        let count = report.per_file_raw_notify.get(rel).copied().unwrap_or(0);
        assert_eq!(
            count, 0,
            "migrated file {rel} must have 0 raw untyped notify calls, found {count}"
        );
    }

    let dynamic_helper_files = [
        "services/auth_cache.rs",
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

#[test]
fn ballpark_marked_notifications_enforce_scene_field_and_registry_set() {
    use super::notify_registry::{
        is_ballpark_scoped_notification, OnDamageMessage, OnDamageMessages, OnDamageStateChange,
        OnDroneStateChange, OnSlimItemChange, OnSpecialFX, OnTarget, OnTargetOBO,
        OutboundSceneScope, SlimItemKeyVal, SpaceShipSlimFields,
        BALLPARK_SCOPED_NOTIFICATION_EVENTS,
    };
    use std::collections::BTreeSet;

    let expected: BTreeSet<&'static str> = [
        "OnDamageStateChange",
        "OnDamageMessage",
        "OnDamageMessages",
        "OnSlimItemChange",
        "OnTarget",
        "OnTargetOBO",
        "OnDroneStateChange",
        "OnSpecialFX",
    ]
    .into_iter()
    .collect();
    let exported: BTreeSet<&'static str> =
        BALLPARK_SCOPED_NOTIFICATION_EVENTS.iter().copied().collect();
    assert_eq!(exported, expected);
    assert_eq!(BALLPARK_SCOPED_NOTIFICATION_EVENTS.len(), expected.len());

    let registry_marked: BTreeSet<&'static str> = all_registered_notifications()
        .iter()
        .filter(|entry| entry.is_ballpark_scoped)
        .map(|entry| entry.event)
        .collect();
    assert_eq!(registry_marked, expected);
    for entry in all_registered_notifications() {
        assert_eq!(
            entry.is_ballpark_scoped,
            is_ballpark_scoped_notification(entry.event),
            "registry flag disagrees with is_ballpark_scoped_notification for {}",
            entry.event
        );
    }

    let scene = BallparkScene::try_with_epoch(30_000_142, 77).expect("non-zero scene");
    let expected_scope = Some(OutboundSceneScope::space(Some(30_000_142), Some(77)));

    assert!(OnDamageStateChange::IS_BALLPARK_SCOPED);
    let dmg_state = OnDamageStateChange {
        scene,
        ship_id: 100_000_010,
        damage_state: PyRep::None,
    };
    assert_eq!(dmg_state.scene_scope(), expected_scope);
    assert_eq!(dmg_state.into_args().len(), 2);

    assert!(OnDamageMessage::IS_BALLPARK_SCOPED);
    let dmg_msg = OnDamageMessage {
        scene,
        damage_messages_args: PyRep::Dict(Vec::new()),
    };
    assert_eq!(dmg_msg.scene_scope(), expected_scope);
    assert_eq!(dmg_msg.into_args().len(), 1);

    assert!(OnDamageMessages::IS_BALLPARK_SCOPED);
    let dmg_msgs = OnDamageMessages {
        scene,
        dmgmsgs: Vec::new(),
    };
    assert_eq!(dmg_msgs.scene_scope(), expected_scope);
    assert_eq!(dmg_msgs.into_args().len(), 1);

    assert!(OnSlimItemChange::IS_BALLPARK_SCOPED);
    let slim = OnSlimItemChange {
        scene,
        old_slim: PyRep::Integer(100_000_001),
        new_slim: SlimItemKeyVal::for_space_ship(SpaceShipSlimFields {
            item_id: 100_000_001,
            type_id: 587,
            group_id: 25,
            category_id: 6,
            owner_id: 90_000_001,
            char_id: Some(90_000_001),
            corp_id: Some(98_000_001),
            alliance_id: None,
            name: "Wolfpack".into(),
        }),
    };
    assert_eq!(slim.scene_scope(), expected_scope);
    assert_eq!(slim.into_args().len(), 2);

    assert!(OnTarget::IS_BALLPARK_SCOPED);
    let target = OnTarget {
        scene,
        arg0: PyRep::String("add".into()),
        arg1: PyRep::Integer(100_000_001),
        reason: None,
    };
    assert_eq!(target.scene_scope(), expected_scope);
    assert_eq!(target.into_args().len(), 2);

    assert!(OnTargetOBO::IS_BALLPARK_SCOPED);
    let target_obo = OnTargetOBO {
        scene,
        what: "clear".into(),
        sid: 100_000_001,
        tid: None,
        reason: None,
    };
    assert_eq!(target_obo.scene_scope(), expected_scope);
    assert_eq!(target_obo.into_args().len(), 4);

    assert!(OnDroneStateChange::IS_BALLPARK_SCOPED);
    let drone = OnDroneStateChange {
        scene,
        drone_id: 100_000_020,
        owner_id: 90_000_001,
        controller_id: 100_000_010,
        activity_state: 0,
        drone_type_id: 2454,
        controller_owner_id: 90_000_001,
        target_id: None,
    };
    assert_eq!(drone.scene_scope(), expected_scope);
    assert_eq!(drone.into_args().len(), 7);

    assert!(OnSpecialFX::IS_BALLPARK_SCOPED);
    let fx = OnSpecialFX {
        scene,
        ship_id: 100_000_010,
        module_id: Some(100_000_020),
        module_type_id: Some(483),
        target_id: None,
        other_type_id: None,
        guid: "effects.Laser".to_string(),
        is_offensive: false,
        start: 0,
        active: 0,
        duration: 0.0,
        repeat: 0,
        start_time: SimFileTime::from_ticks(1_335_000_000_000_000_00),
        time_from_start: 0.0,
        graphic_info: None,
    };
    assert_eq!(fx.scene_scope(), expected_scope);
    assert_eq!(fx.into_args().len(), 14);

    assert!(!OnMailSent::IS_BALLPARK_SCOPED);
    assert!(!is_ballpark_scoped_notification(OnMailSent::EVENT));
    let mail = OnMailSent {
        message_id: 9001,
        sender_id: 90_000_001,
        sent_date: WallFileTime::from_ticks(1_335_000_000_000_000_00),
        to_character_ids: vec![90_000_002],
        to_list_id: None,
        to_corp_or_alliance_id: None,
        title: "hi".into(),
        status_mask: 0,
    };
    assert_eq!(mail.scene_scope(), None);

    let reg_ctx = super::ServerContext::new();
    let target_args = vec![PyRep::String("lost".into()), PyRep::Integer(100_000_010)];
    assert!(
        reg_ctx
            .emit_registered_to_char(90_000_001, "OnTarget", "charid", target_args.clone())
            .is_none(),
        "emit_registered_to_char must return None without panicking for @ballpark event"
    );
    assert!(
        super::notify_registry::build_pending_registered_notification(
            90_000_001,
            None,
            "OnTarget",
            "charid",
            target_args,
            0,
        )
        .is_none(),
        "build_pending_registered_notification must return None without panicking for @ballpark event"
    );
}

#[test]
fn notify_registry_timestamps_exhaustive_guard() {
    const REGISTRY_SRC: &str = include_str!("notify_registry.rs");
    const FILETIME_MIN_TICKS: i64 = 100_000_000_000_000_000;
    const NUMERIC_DURATION_EXCLUSIONS: &[&str] = &[
        "time_ms",
        "time_left",
        "capture_time",
        "time_from_start",
        "timer_type",
    ];

    fn match_delim(s: &str, open_idx: usize, open_c: char, close_c: char) -> Result<(&str, usize), String> {
        let bytes = s.as_bytes();
        let ob = open_c as u8;
        let cb = close_c as u8;
        let mut depth = 0usize;
        let mut i = open_idx;
        let mut in_str = false;
        let mut in_line_comment = false;
        while i < bytes.len() {
            let b = bytes[i];
            if in_line_comment {
                if b == b'\n' {
                    in_line_comment = false;
                }
            } else if in_str {
                if b == b'\\' {
                    i += 1;
                } else if b == b'"' {
                    in_str = false;
                }
            } else if i + 1 < bytes.len() && b == b'/' && bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 1;
            } else if b == b'"' {
                in_str = true;
            } else if b == ob {
                depth += 1;
            } else if b == cb {
                depth -= 1;
                if depth == 0 {
                    return Ok((&s[open_idx + 1..i], i + 1));
                }
            }
            i += 1;
        }
        Err(format!("unclosed delimiter `{open_c}` at byte offset {open_idx}"))
    }

    fn split_top_level_commas(s: &str) -> Vec<&str> {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut start = 0usize;
        let mut depth = 0usize;
        let mut in_str = false;
        let mut in_line_comment = false;
        let mut i = 0usize;
        while i < bytes.len() {
            let b = bytes[i];
            if in_line_comment {
                if b == b'\n' {
                    in_line_comment = false;
                }
            } else if in_str {
                if b == b'\\' {
                    i += 1;
                } else if b == b'"' {
                    in_str = false;
                }
            } else if i + 1 < bytes.len() && b == b'/' && bytes[i + 1] == b'/' {
                in_line_comment = true;
                i += 1;
            } else if b == b'"' {
                in_str = true;
            } else if matches!(b, b'(' | b'[' | b'{') {
                depth += 1;
            } else if matches!(b, b')' | b']' | b'}') {
                depth = depth.saturating_sub(1);
            } else if b == b',' && depth == 0 {
                let part = s[start..i].trim();
                if !part.is_empty() {
                    out.push(part);
                }
                start = i + 1;
            }
            i += 1;
        }
        let tail = s[start..].trim();
        if !tail.is_empty() {
            out.push(tail);
        }
        out
    }

    fn split_macro_entries(block: &str) -> Vec<(String, &str)> {
        let mut items = Vec::new();
        let mut pos = 0usize;
        while let Some(rel) = block[pos..].find("=>") {
            let idx = pos + rel;
            let line_start = block[..idx].rfind('\n').map_or(0, |n| n + 1);
            let prefix = block[line_start..idx].trim();
            if prefix.starts_with("//") {
                pos = block[idx..].find('\n').map_or(block.len(), |n| idx + n + 1);
                continue;
            }
            let struct_name = prefix
                .split_whitespace()
                .last()
                .expect("notification struct identifier before `=>`")
                .to_string();
            let Some(brace_rel) = block[idx..].find('{') else {
                break;
            };
            let brace_idx = idx + brace_rel;
            let (body, end) = match_delim(block, brace_idx, '{', '}').expect("balanced entry body");
            items.push((struct_name, body));
            pos = end;
        }
        items
    }

    fn parse_sample_filetime_integer(arg: &str) -> Option<i64> {
        let compact: String = arg.chars().filter(|c| !c.is_whitespace()).collect();
        let rest = compact.strip_prefix("eve_proto::").unwrap_or(&compact);
        let inner = rest.strip_prefix("PyRep::Integer(")?.strip_suffix(')')?;
        let inner = inner.strip_suffix("i64").unwrap_or(inner);
        let digits: String = inner.chars().filter(|&c| c != '_').collect();
        let val = digits.parse::<i64>().ok()?;
        (val >= FILETIME_MIN_TICKS).then_some(val)
    }

    fn is_suspicious_timestamp_field_name(name: &str) -> bool {
        if NUMERIC_DURATION_EXCLUSIONS.contains(&name) {
            return false;
        }
        name.ends_with("_time")
            || name.ends_with("_date")
            || name == "timestamp"
            || name.ends_with("_timestamp")
            || name == "created"
            || name.ends_with("_created")
            || name == "deadline"
            || name.ends_with("_deadline")
            || name == "last_incident"
            || name == "time"
    }

    let src_bytes = REGISTRY_SRC.as_bytes();
    let mut entries = Vec::new();
    for (macro_name, open_c, close_c) in [
        ("define_notifications!", '{', '}'),
        ("define_notification_item!", '(', ')'),
    ] {
        for (idx, _) in REGISTRY_SRC.match_indices(macro_name) {
            if idx > 0 && src_bytes[idx - 1] != b'\n' {
                continue;
            }
            let after = idx + macro_name.len();
            let open_rel = REGISTRY_SRC[after..]
                .find(open_c)
                .expect("macro opening delimiter");
            let (content, _) =
                match_delim(REGISTRY_SRC, after + open_rel, open_c, close_c).expect("balanced macro invocation");
            entries.extend(split_macro_entries(content));
        }
    }
    assert!(
        entries.len() >= 500,
        "expected >= 500 parsed notification entries from notify_registry.rs, found {}",
        entries.len()
    );

    let mut violations = Vec::new();
    for (struct_name, body) in entries {
        let header_slice = body.find("samples:").map_or(body, |s| &body[..s]);
        let mut fields: Vec<(String, String)> = Vec::new();
        for section in ["fields:", "optional:"] {
            if let Some(sidx) = header_slice.find(section) {
                let b_rel = header_slice[sidx + section.len()..]
                    .find('{')
                    .expect("fields/optional opening `{`");
                let (sec_body, _) =
                    match_delim(header_slice, sidx + section.len() + b_rel, '{', '}')
                        .expect("balanced fields/optional block");
                for raw_field in split_top_level_commas(sec_body) {
                    if let Some((fn_raw, ft_raw)) = raw_field.split_once(':') {
                        let fname = fn_raw.trim().to_string();
                        let ftype: String = ft_raw.chars().filter(|c| !c.is_whitespace()).collect();
                        fields.push((fname, ftype));
                    }
                }
            }
        }

        let mut filetime_sample_indices = std::collections::BTreeSet::new();
        if let Some(sidx) = body.find("samples:") {
            if let Some(ov_rel) = body[sidx..].find("vec![") {
                let (outer_vec, _) = match_delim(body, sidx + ov_rel + 4, '[', ']')
                    .expect("balanced samples outer vec");
                let mut p = 0usize;
                while let Some(iv_rel) = outer_vec[p..].find("vec![") {
                    let (sample_vec, end) = match_delim(outer_vec, p + iv_rel + 4, '[', ']')
                        .expect("balanced sample inner vec");
                    for (idx, arg) in split_top_level_commas(sample_vec).into_iter().enumerate() {
                        if parse_sample_filetime_integer(arg).is_some() {
                            filetime_sample_indices.insert(idx);
                        }
                    }
                    p = end;
                }
            }
        }

        for (idx, (fname, ftype)) in fields.iter().enumerate() {
            let is_bare_untyped = matches!(
                ftype.as_str(),
                "i64" | "Option<i64>" | "PyRep" | "Option<PyRep>"
            );
            if !is_bare_untyped {
                continue;
            }
            let by_sample = filetime_sample_indices.contains(&idx);
            let by_name = is_suspicious_timestamp_field_name(fname);
            if by_sample || by_name {
                violations.push(format!(
                    "{struct_name}.{fname}: {ftype} (filetime_sample={by_sample}, timestamp_name={by_name})"
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "untyped FILETIME/timestamp notification fields found ({}):\n{}",
        violations.len(),
        violations.join("\n")
    );
}
