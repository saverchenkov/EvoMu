//! Targeted unit and mutation-killing tests for `characters.rs`.

use super::*;

fn test_sde() -> SdeRegistry {
    let mut sde = SdeRegistry::default();
    sde.races.insert(
        1,
        eve_sde::RaceInfo {
            race_id: 1,
            name: "Caldari".into(),
            corvette_type_id: Some(601),
            starter_skills: HashMap::from([(3300, 1)]),
            description: String::new(),
        },
    );
    sde.bloodlines.insert(
        1,
        eve_sde::BloodlineInfo {
            bloodline_id: 1,
            race_id: 1,
            name: "Deteis".into(),
            corporation_id: 1_000_006,
            ship_type_id: Some(601),
            charisma: 19,
            intelligence: 20,
            memory: 20,
            perception: 20,
            willpower: 20,
            description: String::new(),
        },
    );
    sde.ancestries.insert(
        1,
        eve_sde::AncestryInfo {
            ancestry_id: 1,
            bloodline_id: 1,
            name: "Merchandisers".into(),
            charisma: 0,
            intelligence: 0,
            memory: 0,
            perception: 0,
            willpower: 0,
            description: String::new(),
            short_description: String::new(),
        },
    );
    sde.ancestries.insert(
        64,
        eve_sde::AncestryInfo {
            ancestry_id: 64,
            bloodline_id: 0,
            name: "Undefined".into(),
            charisma: 0,
            intelligence: 0,
            memory: 0,
            perception: 0,
            willpower: 0,
            description: String::new(),
            short_description: String::new(),
        },
    );
    sde.schools.insert(
        17,
        eve_sde::SchoolInfo {
            school_id: 17,
            race_id: 1,
            corporation_id: 1_000_044,
            station_id: 60_015_157,
            solar_system_id: 30_000_141,
            name: "School of Applied Knowledge".into(),
        },
    );
    let jita_sta = eve_sde::StationInfo {
        station_id: 60_003_760,
        solar_system_id: 30_000_142,
        constellation_id: 20_000_020,
        region_id: 10_000_002,
        station_type_id: 1529,
        name: "Jita IV - Moon 4".into(),
        position: Default::default(),
        owner_corporation_id: 1_000_035,
        owner_faction_id: None,
        operation_id: 1,
        reprocessing_efficiency: 0.5,
        reprocessing_stations_take: 0.05,
        is_conquerable: false,
        orbit_id: None,
        has_clone_bay: true,
    };
    sde.stations.insert(60_003_760, jita_sta.clone());
    sde.solar_systems.insert(
        30_000_142,
        eve_sde::SolarSystem {
            solar_system_id: 30_000_142,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            name: "Jita".into(),
            security_status: 0.9,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: vec![jita_sta],
            wormhole_class_id: None,
        },
    );
    sde.solar_systems.insert(
        30_000_144,
        eve_sde::SolarSystem {
            solar_system_id: 30_000_144,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            name: "Perimeter".into(),
            security_status: 0.9,
            radius: 1.0e12,
            galactic_pos_ly: [0.0; 3],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );
    sde
}

#[test]
fn character_creation_validation_lineage_and_account_cap() {
    let sde = test_sde();
    let repo = WorldRepository::with_demo_seed();
    let uid = repo.provision_player_account("char_creator", "pass");

    // Name length < 3 rejected
    assert!(repo.create_character(uid, "ab", 1, 1, 1, 17, &sde).is_err());
    // Unknown race / bloodline / ancestry / school or mismatched lineage rejected
    assert!(repo.create_character(uid, "ValidName", 999, 1, 1, 17, &sde).is_err());
    assert!(repo.create_character(uid, "ValidName", 1, 999, 1, 17, &sde).is_err());
    assert!(repo.create_character(uid, "ValidName", 1, 1, 999, 17, &sde).is_err());
    assert!(repo.create_character(uid, "ValidName", 1, 1, 1, 999, &sde).is_err());

    // Create 3 characters (including ancestry_id == 64 undefined ancestry fallback & qa_starter_system_id)
    let c1 = repo
        .create_character_with_system(uid, "Pilot One", 1, 1, 64, 17, &sde, Some(30_000_142))
        .expect("create c1");
    assert_eq!(c1.user_id, uid);
    assert_eq!(c1.solar_system_id, 30_000_142);
    assert!(c1.skill_points > 0.0);

    // Case-insensitive duplicate name rejected
    assert!(repo.create_character(uid, "pilot one", 1, 1, 1, 17, &sde).is_err());

    let c2 = repo.create_character(uid, "Pilot Two", 1, 1, 1, 17, &sde).expect("create c2");
    let c3 = repo.create_character(uid, "Pilot Three", 1, 1, 1, 17, &sde).expect("create c3");

    // 4th character on same account rejected (cap = 3)
    assert!(repo.create_character(uid, "Pilot Four", 1, 1, 1, 17, &sde).is_err());

    let list = repo.get_characters_for_user(uid);
    assert_eq!(list.len(), 3);
    assert_eq!(list[0].character_id, c1.character_id);
    assert_eq!(list[1].character_id, c2.character_id);
    assert_eq!(list[2].character_id, c3.character_id);

    assert_eq!(repo.get_character(c1.character_id).unwrap().name, "Pilot One");
    assert_eq!(repo.get_character(999_999_999), Err(DbError::CharacterNotFound(999_999_999)));
    assert_eq!(
        repo.select_character_for_user(uid + 1, c1.character_id),
        Err(DbError::CharacterOwnershipViolation {
            user_id: uid + 1,
            character_id: c1.character_id,
        })
    );
}

#[test]
fn undock_dock_respawn_and_solar_system_transitions() {
    let sde = test_sde();
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;
    let ch0 = repo.get_character(cid).unwrap();
    let ship_id = ch0.active_ship_id;

    // Locked or non-singleton active ship cannot undock
    repo.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::Locked;
    });
    assert_eq!(repo.undock_character(1, cid), Err(DbError::ItemLocked(ship_id)));
    repo.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().flag = InventoryFlag::Hangar;
    });

    // Undock succeeds and moves ship into space
    let (st_id, ch_space, ship_space) = repo.undock_character(1, cid).unwrap();
    assert_eq!(st_id, 60_003_760);
    assert!(ch_space.station_id.is_none());
    assert_eq!(ship_space.location_id, 30_000_142);
    assert_eq!(ship_space.flag, InventoryFlag::None);

    // Second undock while in space returns NotDocked
    assert_eq!(repo.undock_character(1, cid), Err(DbError::NotDocked(cid)));

    // set_character_solar_system updates character and active ship location
    let jumped = repo.set_character_solar_system(cid, 30_000_144, &sde).unwrap();
    assert_eq!(jumped.solar_system_id, 30_000_144);
    assert_eq!(repo.get_item(ship_id).unwrap().location_id, 30_000_144);

    // rollback_undock restores station_id and ship Hangar location
    repo.rollback_undock(cid, 60_003_760, ship_id);
    assert_eq!(repo.get_character(cid).unwrap().station_id, Some(60_003_760));
    assert_eq!(repo.get_item(ship_id).unwrap().flag, InventoryFlag::Hangar);

    // Respawn reuses an existing Capsule (type_id 670) at home station if one already exists
    let pre_capsule_id = 200_555_001_i64;
    repo.with_mut_snapshot(|snap| {
        snap.items.insert(
            pre_capsule_id,
            InventoryItemRecord {
                item_id: pre_capsule_id,
                type_id: 670,
                owner_id: cid,
                location_id: 60_003_760,
                flag: InventoryFlag::Unlocked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..InventoryItemRecord::default()
            },
        );
    });
    let (respawned, cap) = repo.respawn_character_at_home_station(cid, ship_id, &sde).unwrap();
    assert_eq!(cap.item_id, pre_capsule_id);
    assert_eq!(cap.flag, InventoryFlag::Hangar);
    assert_eq!(respawned.active_ship_id, pre_capsule_id);
    assert!(repo.get_item(ship_id).is_none());
}

#[test]
fn neural_remap_bounds_cooldown_and_mining_ledger_with_waste_and_observers() {
    let repo = WorldRepository::with_demo_seed();
    let cid = 90_000_001_i64;

    // Out-of-bounds (< 17 or > 27) and sum != 99 rejected
    assert!(repo.remap_neural_attributes(cid, 16, 20, 21, 21, 21, 1_700_000_000).is_err());
    assert!(repo.remap_neural_attributes(cid, 28, 17, 18, 18, 18, 1_700_000_000).is_err());
    assert!(repo.remap_neural_attributes(cid, 20, 20, 20, 20, 20, 1_700_000_000).is_err());

    // First timed remap sets yearly cooldown (365 * 86_400 = 31_536_000s)
    let r1 = repo.remap_neural_attributes(cid, 17, 27, 21, 17, 17, 1_700_000_000).unwrap();
    assert_eq!(r1.neural.intelligence, 27);
    assert_eq!(r1.neural.remap_cooldown_until_unix, Some(1_700_000_000 + 31_536_000));

    // Remap during cooldown without bonus remaps fails
    repo.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().neural.bonus_remaps = 0;
    });
    assert!(repo.remap_neural_attributes(cid, 18, 26, 21, 17, 17, 1_700_000_100).is_err());

    // With bonus_remaps = 1, remap during cooldown succeeds and decrements bonus_remaps
    repo.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&cid).unwrap().neural.bonus_remaps = 1;
    });
    let r2 = repo.remap_neural_attributes(cid, 18, 26, 21, 17, 17, 1_700_000_100).unwrap();
    assert_eq!(r2.neural.bonus_remaps, 0);
    assert_eq!(r2.neural.intelligence, 26);

    // Mining ledger with waste does not leak system-wide to arbitrary structures,
    // while explicit observer entries record scoped structure volume.
    let obs_id = 1_000_000_000_999_i64;
    repo.with_mut_snapshot(|snap| {
        snap.structures.insert(
            obs_id,
            UpwellStructureRecord {
                structure_id: obs_id,
                type_id: 35835,
                owner_corp_id: 1_000_009,
                solar_system_id: 30_000_142,
                name: "Athanor".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                ..UpwellStructureRecord::default()
            },
        );
    });
    repo.record_mining_ledger_entry(cid, 30_000_142, 1230, 500, 1_704_067_200).unwrap();
    repo.record_mining_ledger_entry_with_waste(cid, 30_000_142, 1228, 300, 120, 1_704_067_260).unwrap();

    let entries = repo.get_mining_ledger(cid);
    assert_eq!(entries.len(), 2);
    let with_waste = repo.get_mining_ledger_with_waste(cid);
    assert_eq!(with_waste[0].1, 0);
    assert_eq!(with_waste[1].1, 120);

    assert!(repo.get_observers_with_mining_events().is_empty());
    assert!(repo.get_observer_mining_ledger(obs_id).is_empty());

    repo.record_observer_mining_ledger_entry(obs_id, cid, 45492, 300, 1_704_067_260).unwrap();
    assert_eq!(repo.get_observers_with_mining_events(), vec![obs_id]);
    assert_eq!(repo.get_observer_mining_ledger(obs_id).len(), 1);
}
