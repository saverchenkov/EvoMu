//! Unit and mutation-killing tests for `durable_domain_types`.

use super::*;

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
