use super::*;
use crate::accounts::{base64url_encode_no_pad, sha256_bytes};
use crate::durable_domain_types::sorted_set_serde;
use crate::persistence::WorldStateWalDelta;
use crate::sqlite_store::corp_crimewatch_store::canonical_fp;
use crate::sqlite_store::fingerprints::{fnv1a_hasher, fp_sp6_lifecycle};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hasher;
use std::path::PathBuf;

fn load_test_sde() -> eve_sde::SdeRegistry {
    match std::env::var_os("EVOMU_SDE_PATH") {
        Some(p) => eve_sde::SdeRegistry::open(p).expect("open sde"),
        None => eve_sde::SdeRegistry::with_canonical_seed(),
    }
}

fn temp_db_path(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "evodb_err_wal_{}_{}_{}.sqlite",
        tag,
        std::process::id(),
        nanos
    ))
}

struct BytesSliceRef<'a>(&'a [u8]);

impl Serialize for BytesSliceRef<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

#[derive(Serialize)]
struct UnitStructSample;

#[derive(Serialize)]
struct NewtypeStructSample(u16);

#[derive(Serialize)]
struct TupleStructSample(u8, char);

#[derive(Serialize)]
enum EnumSample {
    Unit,
    Newtype(i8),
    Tuple(i16, f32),
    Struct { flag: bool, val: i32 },
}

#[derive(Serialize)]
struct AllSerdePrimitivesSample<'a> {
    b: bool,
    i8v: i8,
    i16v: i16,
    i32v: i32,
    i64v: i64,
    u8v: u8,
    u16v: u16,
    u32v: u32,
    u64v: u64,
    f32v: f32,
    f64_pos: f64,
    f64_zero: f64,
    ch: char,
    s: &'a str,
    raw: BytesSliceRef<'a>,
    none_v: Option<u8>,
    some_v: Option<u8>,
    unit_v: (),
    unit_s: UnitStructSample,
    e_unit: EnumSample,
    nt_s: NewtypeStructSample,
    e_nt: EnumSample,
    seq_v: Vec<u16>,
    tup_v: (u8, bool),
    tup_s: TupleStructSample,
    e_tup: EnumSample,
    map_v: BTreeMap<u8, &'a str>,
    e_str: EnumSample,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct SortedSetContainer {
    #[serde(with = "sorted_set_serde")]
    ids: HashSet<i64>,
}

#[test]
fn canonical_fp_all_primitives_and_sorted_set_serde() {
    let mut map_v = BTreeMap::new();
    map_v.insert(1, "alpha");
    map_v.insert(2, "beta");

    let sample = AllSerdePrimitivesSample {
        b: true,
        i8v: -7,
        i16v: -320,
        i32v: -99_000,
        i64v: -1_000_000_000,
        u8v: 250,
        u16v: 65_000,
        u32v: 4_000_000,
        u64v: 9_000_000_000,
        f32v: 3.5,
        f64_pos: 12.25,
        f64_zero: -0.0,
        ch: 'Z',
        s: "evomu",
        raw: BytesSliceRef(&[1, 2, 3, 4]),
        none_v: None,
        some_v: Some(99),
        unit_v: (),
        unit_s: UnitStructSample,
        e_unit: EnumSample::Unit,
        nt_s: NewtypeStructSample(42),
        e_nt: EnumSample::Newtype(-5),
        seq_v: vec![10, 20, 30],
        tup_v: (8, false),
        tup_s: TupleStructSample(3, 'A'),
        e_tup: EnumSample::Tuple(-11, 2.25),
        map_v,
        e_str: EnumSample::Struct {
            flag: true,
            val: 777,
        },
    };
    let h1 = canonical_fp(&sample);
    let h2 = canonical_fp(&sample);
    assert_eq!(h1, h2);
    assert_ne!(h1, 0);

    let set_wrap = SortedSetContainer {
        ids: [30, 10, 20].into_iter().collect(),
    };
    let encoded = serde_json::to_string(&set_wrap).unwrap();
    assert_eq!(encoded, "{\"ids\":[10,20,30]}");
    let decoded: SortedSetContainer = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, set_wrap);

    let mut std_h = fnv1a_hasher();
    Hasher::write(&mut std_h, b"abc");
    assert_ne!(Hasher::finish(&std_h), 0);

    let mut sp6 = Sp6PersistedState {
        missions_awaiting_downtime_respawn: vec![(4001, 7)],
        mission_initial_npc_counts: HashMap::from([("30000142:7".into(), 3)]),
        mission_initial_npc_types: HashMap::from([("30000142:7".into(), vec![587, 588])]),
        ..Sp6PersistedState::default()
    };
    let fp_a = fp_sp6_lifecycle(&sp6);
    sp6.mission_initial_npc_types.get_mut("30000142:7").unwrap().push(589);
    assert_ne!(fp_a, fp_sp6_lifecycle(&sp6));
}

#[test]
fn wal_delta_from_diff_apply_to_and_corrupt_wal_handling() {
    let mut old = WorldStateSnapshot::default();
    old.items.insert(
        1,
        InventoryItemRecord {
            item_id: 1,
            owner_id: 100,
            location_id: 60003760,
            type_id: 34,
            quantity: 100,
            flag: InventoryFlag::Hangar,
            ..InventoryItemRecord::default()
        },
    );
    old.items.insert(
        2,
        InventoryItemRecord {
            item_id: 2,
            owner_id: 100,
            location_id: 60003760,
            type_id: 35,
            quantity: 50,
            flag: InventoryFlag::Hangar,
            ..InventoryItemRecord::default()
        },
    );
    old.market_orders.insert(
        10,
        MarketOrderRecord {
            order_id: 10,
            owner_id: 100,
            station_id: 60003760,
            region_id: 10000002,
            type_id: 34,
            is_bid: false,
            price: IskCents(500),
            vol_entered: 100,
            vol_remaining: 100,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents(0),
        },
    );
    old.market_orders.insert(
        11,
        MarketOrderRecord {
            order_id: 11,
            owner_id: 100,
            station_id: 60003760,
            region_id: 10000002,
            type_id: 35,
            is_bid: true,
            price: IskCents(400),
            vol_entered: 20,
            vol_remaining: 20,
            min_volume: 1,
            is_npc: false,
            escrow: IskCents(8000),
        },
    );
    old.append_journal(100, "Bounty", IskCents(1000), IskCents(1000), "rat");
    old.contracts.insert(
        20,
        ContractRecord {
            contract_id: 20,
            issuer_id: 100,
            issuer_corp_id: 200,
            start_station_id: 60003760,
            end_station_id: 60003760,
            contract_type: "Courier".into(),
            status: "Outstanding".into(),
            price: IskCents(500),
            reward: IskCents(100),
            collateral: IskCents(1000),
            ..ContractRecord::default()
        },
    );
    old.system_states.insert(30000142, SystemDynamicState::default());
    old.committed_idempotency_keys.insert((100, "op-1".into()));

    let mut new = old.clone();
    new.wal_seq = 5;
    new.next_entity_id = 999;
    new.items.get_mut(&1).unwrap().quantity = 75;
    new.items.remove(&2);
    new.market_orders.get_mut(&10).unwrap().vol_remaining = 60;
    new.market_orders.remove(&11);
    new.append_journal(100, "Market", IskCents(250), IskCents(1250), "sale");
    new.contracts.get_mut(&20).unwrap().status = "InProgress".into();
    new.system_states.get_mut(&30000142).unwrap().dbs_multiplier = 1.25;
    new.committed_idempotency_keys.insert((100, "op-2".into()));

    // Non-trailing corrupt WAL line and duplicate seq return errors
    let snap_path = temp_db_path("bad_mid_wal");
    let wal_path = WorldRepository::wal_path_for(&snap_path);
    std::fs::write(&snap_path, serde_json::to_vec(&old).unwrap()).unwrap();
    let d_line = serde_json::to_string(&WorldStateWalDelta::from_diff(&old, &new)).unwrap();
    std::fs::write(&wal_path, format!("{{bad_json_line}}\n{d_line}\n")).unwrap();
    let r1 = WorldRepository::new_empty();
    assert!(r1.load_snapshot_from_file(&snap_path).is_err());

    std::fs::write(&wal_path, format!("{d_line}\n{d_line}\n")).unwrap();
    let r_dup = WorldRepository::new_empty();
    assert!(r_dup.load_snapshot_from_file(&snap_path).is_err());

    // Valid single-entry WAL applies all delta fields
    std::fs::write(&wal_path, format!("{d_line}\n")).unwrap();
    let r2 = WorldRepository::new_empty();
    r2.load_snapshot_from_file(&snap_path).unwrap();
    r2.with_snapshot(|applied| {
        assert_eq!(applied.wal_seq, 5);
        assert_eq!(applied.items.get(&1).unwrap().quantity, 75);
        assert!(!applied.items.contains_key(&2));
        assert_eq!(applied.market_orders.get(&10).unwrap().vol_remaining, 60);
        assert!(!applied.market_orders.contains_key(&11));
        assert_eq!(applied.wallet_journal.len(), 2);
        assert_eq!(applied.contracts.get(&20).unwrap().status, "InProgress");
        assert_eq!(applied.system_states.get(&30000142).unwrap().dbs_multiplier, 1.25);
        assert!(applied.committed_idempotency_keys.contains(&(100, "op-2".into())));
    });

    let _ = std::fs::remove_file(&snap_path);
    let _ = std::fs::remove_file(&wal_path);
}

#[test]
fn inventory_snapshot_counters_and_sso_token_revocation_edges() {
    assert_eq!(crate::snapshot_ext::unix_secs_to_date_string(0), "1970-01-01");
    assert_eq!(
        crate::snapshot_ext::unix_secs_to_date_string(1_700_000_000),
        "2023-11-14"
    );

    let path = temp_db_path("inv_snap_sso");
    let _ = std::fs::remove_file(&path);
    let repo = WorldRepository::open_sqlite(&path).expect("open sqlite");
    let uid = repo.provision_player_account("pilot_err", "pw");
    let uid2 = repo.provision_player_account("other_pilot", "pw");

    assert!(repo
        .ban_account_by_id_or_username("unknown_nobody_xyz", 9_999_999_999, "bad")
        .is_err());

    let sde = load_test_sde();
    let school = sde.schools.values().find(|s| s.race_id == 1).unwrap().school_id;
    let ch1 = repo
        .create_character(uid, "InvEdge", 1, 1, 11, school, &sde)
        .unwrap();
    let ch2 = repo
        .create_character(uid2, "Stranger", 1, 1, 11, school, &sde)
        .unwrap();
    let ch_id = ch1.character_id;
    let other_id = ch2.character_id;
    let ship_id = ch1.active_ship_id;

    repo.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            -7_900_000_000_025,
            OwnerNoteRecord {
                note_id: -7_900_000_000_025,
                character_id: -79,
                item_id: 0,
                label: "notif".into(),
                note: "b".into(),
                timestamp_unix: 0,
            },
        );
        snap.owner_notes.insert(
            780_000_040,
            OwnerNoteRecord {
                note_id: 780_000_040,
                character_id: -78,
                item_id: 0,
                label: "rep".into(),
                note: "b".into(),
                timestamp_unix: 0,
            },
        );
        snap.reconcile_id_counters();
        assert!(snap.next_notification_id >= 500_000);
        assert!(snap.next_report_id >= 700_000);

        // Move ch1 + ship into space and verify colocated ship checks
        let sys_id = snap.characters[&ch_id].solar_system_id;
        snap.characters.get_mut(&ch_id).unwrap().station_id = None;
        snap.items.get_mut(&ship_id).unwrap().location_id = sys_id as i64;
        assert!(snap.is_ship_colocated_with_character(ch_id, ship_id, None));
        assert!(!snap.is_ship_colocated_with_character(other_id, ship_id, None));
        assert!(!snap.is_ship_colocated_with_character(999_999, ship_id, None));

        // Consumable checks: locked, active ship, in-space ship cargo, not consumable
        let locked_it = InventoryItemRecord {
            item_id: 5010,
            owner_id: ch_id,
            location_id: 60003760,
            type_id: 34,
            quantity: 10,
            flag: InventoryFlag::Locked,
            ..InventoryItemRecord::default()
        };
        assert!(!snap.is_item_consumable_by(&locked_it, ch_id, Some(60003760), Some(ship_id)));
        assert!(!snap.is_item_consumable_by(&snap.items[&ship_id], ch_id, None, Some(ship_id)));

        let cargo_it = InventoryItemRecord {
            item_id: 5011,
            owner_id: ch_id,
            location_id: ship_id,
            type_id: 34,
            quantity: 10,
            flag: InventoryFlag::Cargo,
            ..InventoryItemRecord::default()
        };
        assert!(snap.is_item_consumable_by(&cargo_it, ch_id, None, Some(ship_id)));
        let nowhere_it = InventoryItemRecord {
            item_id: 5012,
            owner_id: ch_id,
            location_id: 60008494,
            type_id: 34,
            quantity: 10,
            flag: InventoryFlag::Hangar,
            ..InventoryItemRecord::default()
        };
        assert!(!snap.is_item_consumable_by(&nowhere_it, ch_id, None, Some(ship_id)));

        // Corp contract credit when for_corp with non-empty wallet divisions
        snap.corporations.insert(
            7701,
            CorporationRecord {
                corporation_id: 7701,
                master_wallet: IskCents(1000),
                wallet_divisions: vec![CorpWalletDivisionRecord {
                    division_id: 1,
                    name: "Master".into(),
                    balance: IskCents(1000),
                }],
                ..CorporationRecord::default()
            },
        );
        let corp_c = ContractRecord {
            contract_id: 8801,
            issuer_id: ch_id,
            issuer_corp_id: 7701,
            for_corp: true,
            ..ContractRecord::default()
        };
        snap.credit_contract_issuer(&corp_c, IskCents(2500));
        assert_eq!(snap.corporations[&7701].wallet_divisions[0].balance, IskCents(3500));

        // Expired ItemExchange contract with deposit blob & escrowed items
        snap.items.insert(
            5020,
            InventoryItemRecord {
                item_id: 5020,
                owner_id: ch_id,
                location_id: 8802,
                type_id: 34,
                quantity: 5,
                flag: InventoryFlag::Locked,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            5021,
            InventoryItemRecord {
                item_id: 5021,
                owner_id: ch_id,
                location_id: 8802,
                type_id: 35,
                quantity: 5,
                flag: InventoryFlag::Locked,
                ..InventoryItemRecord::default()
            },
        );
        snap.contracts.insert(
            8802,
            ContractRecord {
                contract_id: 8802,
                issuer_id: ch_id,
                start_station_id: 60003760,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                expiration_unix: 100,
                reward: IskCents(100),
                item_ids: vec![5020],
                ..ContractRecord::default()
            },
        );
        snap.service_state_blobs.insert("contract_deposit:8802".into(), "400".into());
        assert_eq!(snap.resolve_expired_contract(8802, 999_999), Some("Expired"));
        assert_eq!(snap.items[&5020].location_id, 60003760);
        assert_eq!(snap.items[&5021].location_id, 60003760);
        assert_eq!(snap.items[&5021].flag, InventoryFlag::Hangar);

        // Fail courier contract with deposit blob
        snap.contracts.insert(
            8803,
            ContractRecord {
                contract_id: 8803,
                issuer_id: ch_id,
                acceptor_id: Some(other_id),
                start_station_id: 60003760,
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                reward: IskCents(100),
                collateral: IskCents(500),
                ..ContractRecord::default()
            },
        );
        snap.service_state_blobs.insert("contract_deposit:8803".into(), "200".into());
        assert!(snap.fail_courier_contract(8803, 999_999));

        // Items for stack/split/delete error coverage + nested container trash
        snap.items.insert(
            5030,
            InventoryItemRecord {
                item_id: 5030,
                owner_id: ch_id,
                location_id: 60003760,
                type_id: 34,
                quantity: 100,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            5031,
            InventoryItemRecord {
                item_id: 5031,
                owner_id: ch_id,
                location_id: 60003760,
                type_id: 35,
                quantity: 10,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            5032,
            InventoryItemRecord {
                item_id: 5032,
                owner_id: ch_id,
                location_id: 60008494,
                type_id: 34,
                quantity: 10,
                ..InventoryItemRecord::default()
            },
        );
        snap.items.insert(
            5033,
            InventoryItemRecord {
                item_id: 5033,
                owner_id: ch_id,
                location_id: 5030,
                type_id: 36,
                quantity: 4,
                ..InventoryItemRecord::default()
            },
        );
        snap.item_custom_names.insert(5030, "courier_cargo:8803".into());
    });

    // Inventory validation & nested container deletion
    assert!(repo.add_or_stack_item(ch_id, 60003760, InventoryFlag::Hangar, 34, 0).is_err());
    assert!(repo.split_stack(ch_id, 5030, 0).is_err());
    assert!(repo.delete_item(ch_id, ship_id).is_err());
    assert!(repo.delete_item(other_id, 5030).is_err());
    assert!(repo.stack_items(other_id, 5030, 5031).is_err());
    assert!(repo.stack_items(ch_id, 5030, 5031).is_err());
    assert!(repo.stack_items(ch_id, 5030, 5032).is_err());
    repo.delete_item(ch_id, 5030).unwrap();
    assert!(repo.get_item(5033).is_none());

    // Revoking SSO access token directly via revoke_sso_refresh_token
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
    let code = repo
        .authorize_pkce(
            "pilot_err",
            "pw",
            ch_id,
            "evomu-launcher",
            "eveauth://callback",
            vec![],
            &challenge,
            "S256",
            2000,
        )
        .unwrap();
    let sso = repo
        .exchange_pkce_code(&code, "evomu-launcher", verifier, 2001)
        .unwrap();
    assert!(repo.revoke_sso_refresh_token(&sso.access_token));
    assert!(!repo.revoke_sso_refresh_token("not_a_token"));
    assert!(repo.verify_sso_access_token(&sso.access_token, 2002).is_err());

    let _ = std::fs::remove_file(&path);
}
