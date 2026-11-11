//! Unit and mutation-killing tests for `durable_domain_types`.

use super::*;
use crate::{
    AllianceSovConfig, DeploymentRecord, DungeonRuntimeStore, IncursionSiteEntry,
    IncursionSiteWaveMeta, MmjuSpoolEntry, MobileDeployableRuntimeState, PlayerProjectState,
};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TupleMapWrapper {
    #[serde(with = "tuple_map_serde")]
    map: HashMap<(i64, u32), String>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SortedSetWrapper {
    #[serde(with = "sorted_set_serde")]
    set: HashSet<i64>,
}

#[test]
fn tuple_map_serde_sorts_keys_and_roundtrips() {
    let mut map = HashMap::new();
    map.insert((20, 2), "twenty-two".to_string());
    map.insert((10, 5), "ten-five".to_string());
    map.insert((10, 1), "ten-one".to_string());
    let wrapper = TupleMapWrapper { map };

    let json = serde_json::to_string(&wrapper).expect("serialize TupleMapWrapper");
    assert_eq!(
        json,
        r#"{"map":[[[10,1],"ten-one"],[[10,5],"ten-five"],[[20,2],"twenty-two"]]}"#
    );

    let decoded: TupleMapWrapper =
        serde_json::from_str(&json).expect("deserialize TupleMapWrapper");
    assert_eq!(decoded, wrapper);
    assert!(serde_json::from_str::<TupleMapWrapper>(r#"{"map":"invalid"}"#).is_err());
}

#[test]
fn sorted_set_serde_sorts_elements_and_roundtrips() {
    let mut set = HashSet::new();
    set.insert(42);
    set.insert(7);
    set.insert(19);
    let wrapper = SortedSetWrapper { set };

    let json = serde_json::to_string(&wrapper).expect("serialize SortedSetWrapper");
    assert_eq!(json, r#"{"set":[7,19,42]}"#);

    let decoded: SortedSetWrapper =
        serde_json::from_str(&json).expect("deserialize SortedSetWrapper");
    assert_eq!(decoded, wrapper);
    assert!(serde_json::from_str::<SortedSetWrapper>(r#"{"set":"invalid"}"#).is_err());
}

#[test]
fn persisted_crimewatch_state_timer_boundaries() {
    let cw = PersistedCrimeWatchState {
        character_id: 90_000_001,
        weapon_timer_until_unix: 100,
        pvp_timer_until_unix: 200,
        suspect_flag_until_unix: 300,
        criminal_flag_until_unix: 400,
        pending_concord_arrival: Some((30_000_142, 106, 100_000_001, 90_000_002)),
    };

    // Strict `>` boundary checks (kills `>=`, `<`, `==` mutants)
    assert!(cw.is_weapon_timer_active(99));
    assert!(!cw.is_weapon_timer_active(100));
    assert!(!cw.is_weapon_timer_active(101));

    assert!(cw.is_pvp_active(199));
    assert!(!cw.is_pvp_active(200));
    assert!(!cw.is_pvp_active(201));

    assert!(cw.is_suspect(299));
    assert!(!cw.is_suspect(300));
    assert!(!cw.is_suspect(301));

    assert!(cw.is_criminal(399));
    assert!(!cw.is_criminal(400));
    assert!(!cw.is_criminal(401));
}

#[test]
fn durable_domain_types_serde_defaults_and_roundtrips() {
    let gov: CorpGovState = serde_json::from_str("{}").expect("empty CorpGovState");
    assert_eq!(gov, CorpGovState::default());

    let mut war_state = PersistedWarState {
        next_negotiation_id: 5,
        negotiations: vec![PersistedWarNegotiation {
            id: 1,
            war_id: 10,
            kind: PersistedNegKind::Surrender,
            from_corp: 98_000_001,
            isk: IskCents::from_isk(1_000_000),
            message: "Peace".to_string(),
            status: "Pending".to_string(),
            effective_unix: None,
        }],
        peace_treaties: vec![PersistedPeaceTreaty {
            war_id: 10,
            corp_a: 98_000_001,
            corp_b: 98_000_002,
            expiry_unix: 1_700_000_000,
        }],
        ..Default::default()
    };
    war_state.wars.insert(
        10,
        PersistedWarMeta {
            war_hq_id: Some(1_000_000_000_001),
            open_for_allies: true,
            finish_at_unix: Some(1_700_086_400),
        },
    );
    war_state
        .mutual_invites
        .insert((98_000_002, 98_000_001), 1_700_000_000);
    war_state
        .mutual_invites
        .insert((98_000_001, 98_000_002), 1_700_000_001);
    let war_json = serde_json::to_string(&war_state).expect("serialize PersistedWarState");
    let war_back: PersistedWarState =
        serde_json::from_str(&war_json).expect("deserialize PersistedWarState");
    assert_eq!(war_back, war_state);

    let sov_hub = PersistedSovHubData {
        hub_id: 1_000_000_000_100,
        upgrades: vec![PersistedHubUpgrade {
            type_id: 81_619,
            online: true,
            low_power: false,
        }],
        fuel: BTreeMap::from([(81_143, 500)]),
        workforce: PersistedWorkforceMode::Export {
            destination: Some(30_000_142),
            amount: 1_200,
        },
        last_updated_unix: 1_700_000_000,
    };
    let sov_json = serde_json::to_string(&sov_hub).expect("serialize PersistedSovHubData");
    let sov_back: PersistedSovHubData =
        serde_json::from_str(&sov_json).expect("deserialize PersistedSovHubData");
    assert_eq!(sov_back, sov_hub);
    assert_eq!(
        PersistedWorkforceMode::default(),
        PersistedWorkforceMode::Inactive
    );

    let mut combat_cw = PersistedCombatCrimewatchState::default();
    combat_cw
        .duel_challenges
        .insert((90_000_002, 90_000_001), 1_700_000_100);
    combat_cw.detailed_killmails.insert(
        77,
        DetailedKillmail {
            kill_id: 77,
            kill_time_unix: 1_700_000_050,
            solar_system_id: 30_000_142,
            victim_character_id: 90_000_001,
            victim_corporation_id: 98_000_001,
            victim_alliance_id: None,
            victim_ship_type_id: 587,
            victim_damage_taken: 1_250.0,
            victim_position: [1.0, 2.0, 3.0],
            attackers: vec![KillmailAttackerEntry {
                character_id: 90_000_002,
                corporation_id: 98_000_002,
                alliance_id: Some(99_000_001),
                ship_type_id: 587,
                weapon_type_id: 2_889,
                damage_done: 1_250.0,
                final_blow: true,
                security_status: 0.5,
            }],
            dropped_items: vec![KillmailItemEntry {
                type_id: 185,
                flag: 5,
                qty_dropped: 100,
                qty_destroyed: 0,
                singleton: false,
            }],
            destroyed_items: Vec::new(),
            total_value_isk: 500_000.0,
            killmail_hash: "abc123".to_string(),
        },
    );
    let cw_json = serde_json::to_string(&combat_cw).expect("serialize combat_cw");
    let cw_back: PersistedCombatCrimewatchState =
        serde_json::from_str(&cw_json).expect("deserialize combat_cw");
    assert_eq!(cw_back, combat_cw);
}

#[test]
fn empty_collections_negative_sorting_and_remaining_durable_domain_roundtrips() {
    // Empty tuple_map_serde and negative sorted_set_serde ordering
    let empty_map = TupleMapWrapper { map: HashMap::new() };
    let empty_map_json = serde_json::to_string(&empty_map).unwrap();
    assert_eq!(empty_map_json, r#"{"map":[]}"#);
    assert_eq!(serde_json::from_str::<TupleMapWrapper>(&empty_map_json).unwrap(), empty_map);

    let neg_set = SortedSetWrapper {
        set: HashSet::from([5, -10, 0, -2]),
    };
    let neg_set_json = serde_json::to_string(&neg_set).unwrap();
    assert_eq!(neg_set_json, r#"{"set":[-10,-2,0,5]}"#);
    assert_eq!(serde_json::from_str::<SortedSetWrapper>(&neg_set_json).unwrap(), neg_set);

    // Defaults from "{}" for all container structs
    assert_eq!(
        serde_json::from_str::<PersistedAllianceMeta>("{}").unwrap(),
        PersistedAllianceMeta::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedProjectDetails>("{}").unwrap(),
        PersistedProjectDetails::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedAllianceWarState>("{}").unwrap(),
        PersistedAllianceWarState::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedWarMeta>("{}").unwrap(),
        PersistedWarMeta::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedWarState>("{}").unwrap(),
        PersistedWarState::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedFwState>("{}").unwrap(),
        PersistedFwState::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedSovState>("{}").unwrap(),
        PersistedSovState::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedInsurgencyState>("{}").unwrap(),
        PersistedInsurgencyState::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedCombatCrimewatchState>("{}").unwrap(),
        PersistedCombatCrimewatchState::default()
    );
    assert_eq!(
        serde_json::from_str::<PersistedCrimeWatchState>(
            r#"{"character_id":1,"weapon_timer_until_unix":0,"pvp_timer_until_unix":0,"suspect_flag_until_unix":0,"criminal_flag_until_unix":0,"pending_concord_arrival":null}"#
        )
        .unwrap(),
        PersistedCrimeWatchState {
            character_id: 1,
            ..Default::default()
        }
    );

    // PersistedWorkforceMode variants + PersistedSovState + MercenaryDen/Activity
    for mode in [
        PersistedWorkforceMode::Inactive,
        PersistedWorkforceMode::Import {
            sources: vec![30_000_142, 30_000_144],
        },
        PersistedWorkforceMode::Export {
            destination: None,
            amount: 500,
        },
        PersistedWorkforceMode::Transit,
    ] {
        let j = serde_json::to_string(&mode).unwrap();
        let back: PersistedWorkforceMode = serde_json::from_str(&j).unwrap();
        assert_eq!(back, mode);
    }

    let mut sov_state = PersistedSovState::default();
    sov_state.skyhook_active.insert(1_000_000_001, false);
    sov_state.sov_upgrade_combat_npcs.insert(200_000_005);
    sov_state.mercenary_dens.insert(
        50,
        PersistedMercenaryDen {
            den_id: 50,
            type_id: 85_230,
            owner_char_id: 90_001,
            skyhook_id: 1_000_000_001,
            solar_system_id: 30_000_142,
            planet_id: 40_009_087,
            enabled: true,
            cargo_extraction_enabled: false,
            development_level: 2,
            anarchy_level: 1,
            infomorphs: 40,
            simulated_at_unix: 1_700_000_000,
            next_activity_generation_unix: 1_700_010_000,
        },
    );
    sov_state.mercenary_activities.insert(
        "deadbeef".into(),
        PersistedMercenaryActivity {
            uuid: vec![0xde, 0xad, 0xbe, 0xef],
            den_id: 50,
            solar_system_id: 30_000_142,
            dungeon_id: 123,
            started: true,
            started_by: 90_001,
            expiry_unix: 1_700_020_000,
            development_impact: -1,
            anarchy_impact: 2,
            infomorph_bonus: 15,
        },
    );
    let sov_json = serde_json::to_string(&sov_state).unwrap();
    assert_eq!(serde_json::from_str::<PersistedSovState>(&sov_json).unwrap(), sov_state);

    // PersistedFwState, PersistedInsurgencyState, PersistedAllianceWarState, Ally neg kind, bounty ticks
    let mut fw = PersistedFwState::default();
    fw.battlefield_instances.push(PersistedBattlefieldRecord {
        instance_id: 1,
        solar_system_id: 30_002_813,
        faction_id: 500_001,
        name: "Caldari Battlefield".into(),
        capture_progress: 0.75,
        completed: false,
    });
    fw.stat_events.push(PersistedFwStatEvent {
        timestamp_unix: 1_700_000_000,
        character_id: 90_001,
        corporation_id: 98_001,
        alliance_id: None,
        faction_id: 500_001,
        kills: 1,
        victory_points: 20,
    });
    let fw_json = serde_json::to_string(&fw).unwrap();
    assert_eq!(serde_json::from_str::<PersistedFwState>(&fw_json).unwrap(), fw);

    let mut ins = PersistedInsurgencyState::default();
    ins.campaigns.insert(
        1,
        PersistedInsurgencyCampaign {
            campaign_id: 1,
            pirate_faction_id: 500_010,
            target_faction_id: 500_001,
            fob_solar_system_id: 30_002_813,
            systems: vec![30_002_813, 30_002_904],
            created_at_unix: 100,
            forecasting_end_unix: 200,
            active_end_unix: 300,
            winddown_end_unix: 400,
            corruption_points: HashMap::from([(30_002_813, 50)]),
            suppression_points: HashMap::from([(30_002_813, 25)]),
        },
    );
    ins.insurgency_instances.push(PersistedInsurgencySiteRecord {
        instance_id: 2,
        solar_system_id: 30_002_813,
        site_type: "Ambush".into(),
        name: "Pirate Ambush".into(),
        corruption_reward: 1.5,
        completed: true,
    });
    ins.ice_heist_instances.push(PersistedIceHeistRecord {
        instance_id: 3,
        solar_system_id: 30_002_813,
        pirate_tankers_saved: 2,
        empire_tankers_saved: 1,
        completed: false,
    });
    let ins_json = serde_json::to_string(&ins).unwrap();
    assert_eq!(serde_json::from_str::<PersistedInsurgencyState>(&ins_json).unwrap(), ins);

    let ally_neg = PersistedWarNegotiation {
        id: 2,
        war_id: 10,
        kind: PersistedNegKind::Ally,
        from_corp: 98_003,
        isk: IskCents::from_isk(5_000_000),
        message: "Assist".into(),
        status: "Accepted".into(),
        effective_unix: Some(1_700_000_500),
    };
    let ally_json = serde_json::to_string(&ally_neg).unwrap();
    assert_eq!(serde_json::from_str::<PersistedWarNegotiation>(&ally_json).unwrap(), ally_neg);

    let mut combat = PersistedCombatCrimewatchState::default();
    combat.pending_bounty_ticks.insert((30_000_142, 90_001), 125_000.0);
    combat.bounty_tick_next_flush_unix.insert((30_000_142, 90_001), 1_700_001_200);
    combat.pending_bounty_tick_started_unix.insert((30_000_142, 90_001), 1_700_000_000);
    let combat_json = serde_json::to_string(&combat).unwrap();
    assert_eq!(
        serde_json::from_str::<PersistedCombatCrimewatchState>(&combat_json).unwrap(),
        combat
    );

    let notif = SocialContactNotificationRecord::default();
    let notif_json = serde_json::to_string(&notif).unwrap();
    assert_eq!(
        serde_json::from_str::<SocialContactNotificationRecord>(&notif_json).unwrap(),
        notif
    );

    // MobileDeployableRuntimeState deserialize_mmju_spools (map numeric key, map non-numeric key, seq, invalid scalar)
    let spool_a = MmjuSpoolEntry {
        unit_id: 7001,
        ship_id: 8001,
        char_id: 9001,
        solar_system_id: 30_000_142,
        started_unix: 100,
        complete_at_unix: 105,
        direction: [1.0, 0.0, 0.0],
    };
    let spool_b = MmjuSpoolEntry {
        unit_id: 7002,
        ship_id: 8002,
        char_id: 9002,
        solar_system_id: 30_000_142,
        started_unix: 200,
        complete_at_unix: 205,
        direction: [0.0, 1.0, 0.0],
    };
    let map_json = serde_json::json!({
        "mmju_spools": {
            "101": spool_a,
            "non_numeric_key": spool_b,
        }
    })
    .to_string();
    let md_map: MobileDeployableRuntimeState = serde_json::from_str(&map_json).unwrap();
    assert_eq!(md_map.mmju_spools.get(&101), Some(&spool_a));
    assert_eq!(md_map.mmju_spools.get(&7002), Some(&spool_b));

    let seq_json = serde_json::json!({
        "mmju_spools": [spool_a, spool_b]
    })
    .to_string();
    let md_seq: MobileDeployableRuntimeState = serde_json::from_str(&seq_json).unwrap();
    assert_eq!(md_seq.mmju_spools.get(&8001), Some(&spool_a));
    assert_eq!(md_seq.mmju_spools.get(&8002), Some(&spool_b));

    assert!(serde_json::from_str::<MobileDeployableRuntimeState>(r#"{"mmju_spools":123}"#).is_err());

    // Serde field defaults across service_domains (PlayerProjectState, pve_sites, quasar_and_social)
    let pps: PlayerProjectState = serde_json::from_str(
        r#"{"rank":1,"experience":0.0,"accuracy":0.0,"score":0.0,"tasks_submitted":0,"analysis_kredits":0,"tutorial_level":1,"tutorial_complete":false,"current_task_index":0,"bonus_samples_left":10}"#,
    )
    .unwrap();
    assert_eq!(pps.max_bonus_samples, 10);
    assert_eq!(pps.project_id, "mcgill-polyseg");

    let wave_meta: IncursionSiteWaveMeta = serde_json::from_str("{}").unwrap();
    assert_eq!(wave_meta.current_wave, 1);
    assert_eq!(wave_meta.total_waves, 3);

    let inc_site: IncursionSiteEntry = serde_json::from_str(
        r#"{"site_id":"S1","dungeon_id":1,"dungeon_name":"Vanguard","solar_system_id":30000142,"tier":"Vanguard","min_pilots":5,"max_optimal_pilots":10,"base_isk":1000,"base_concord_lp":100,"influence_delta":0.01,"locked_until_low_influence":false,"completed_count":0,"boss_npc_ball_id":999}"#,
    )
    .unwrap();
    assert_eq!(inc_site.boss_type_id, 2933);

    let d_store: DungeonRuntimeStore = serde_json::from_str("{}").unwrap();
    assert_eq!(d_store.next_instance_id, 950_001);

    let dep: DeploymentRecord = serde_json::from_str(
        r#"{"id":1,"name":"D","short_description":"SD","content_group_id":2,"enabled":true,"current_content_count":0}"#,
    )
    .unwrap();
    assert!(dep.no_region_limit);

    let sov_cfg: AllianceSovConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(sov_cfg.current_prime_hour, 18);
    assert_eq!(sov_cfg.new_prime_hour, 18);
}
