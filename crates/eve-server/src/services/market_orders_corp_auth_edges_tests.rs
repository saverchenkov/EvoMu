//! Edge-case unit tests for `market_orders_corp_auth`:
//! location-aware role resolution (`HQ`, `base`, `other`), market order and accountant
//! spend role checks across divisions, and `is_item_sellable_on_market` guards.

use super::*;
use crate::services::station_structure::structure_upwell_ops::AssetSafetyWrapRecord;
use eve_db::{
    corp_roles, ContractRecord, CorpGovState, CorpMemberRoleAssignment,
    CorporationRecord, InventoryFlag, InventoryItemRecord, WorldRepository,
};
use std::collections::HashSet;

const CORP_ID: i64 = 98_000_010;
const HQ_STATION: i64 = 60_003_760;
const BASE_STATION: i64 = 60_004_588;
const OTHER_STATION: i64 = 60_008_494;

const CHAR_CEO: i64 = 90_001_001;
const CHAR_DIR: i64 = 90_001_002;
const CHAR_TRADER_DIV2: i64 = 90_001_003;
const CHAR_ACCT_DIV3: i64 = 90_001_004;
const CHAR_LOC_ROLES: i64 = 90_001_005;
const CHAR_PLAIN: i64 = 90_001_006;

fn build_snapshot() -> eve_db::WorldStateSnapshot {
    let db = WorldRepository::with_demo_seed();
    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            CORP_ID,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = CORP_ID; __corp.name = "Market Auth Corp".into(); __corp.ticker = "MAC".into(); __corp.ceo_id = CHAR_CEO; __corp.hq_station_id = Some(HQ_STATION); __corp.members = HashSet::from([
                    CHAR_CEO,
                    CHAR_DIR,
                    CHAR_TRADER_DIV2,
                    CHAR_ACCT_DIV3,
                    CHAR_LOC_ROLES,
                    CHAR_PLAIN,
                ]); __corp.member_roles = [
                    (CHAR_DIR, corp_roles::ROLE_DIRECTOR),
                    (
                        CHAR_TRADER_DIV2,
                        corp_roles::ROLE_TRADER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[1],
                    ),
                    (
                        CHAR_ACCT_DIV3,
                        corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
                    ),
                    (CHAR_PLAIN, 0),
                ]
                .into_iter()
                .collect(); __corp.ensure_wallet_divisions(); __corp },
        );
        let template_char = snap.characters.values().next().unwrap().clone();
        for cid in [
            CHAR_CEO,
            CHAR_DIR,
            CHAR_TRADER_DIV2,
            CHAR_ACCT_DIV3,
            CHAR_LOC_ROLES,
            CHAR_PLAIN,
        ] {
            let mut ch = template_char.clone();
            ch.character_id = cid;
            ch.corporation_id = CORP_ID;
            ch.station_id = Some(HQ_STATION);
            ch.home_station_id = HQ_STATION;
            snap.characters.insert(cid, ch);
        }
        snap.corp_role_assignments.insert(
            (CORP_ID, CHAR_LOC_ROLES),
            CorpMemberRoleAssignment {
                character_id: CHAR_LOC_ROLES,
                roles: corp_roles::ROLE_DELIVERIES_HANGAR_TAKE,
                roles_at_hq: corp_roles::ROLE_HANGAR_CAN_TAKE[0],
                roles_at_base: corp_roles::ROLE_HANGAR_CAN_TAKE[2],
                roles_at_other: corp_roles::ROLE_HANGAR_CAN_TAKE[5],
                grantable_roles: 0,
                grantable_roles_at_hq: 0,
                grantable_roles_at_base: 0,
                grantable_roles_at_other: 0,
            },
        );
        let mut gov = CorpGovState::default();
        gov.member_bases.insert(CHAR_LOC_ROLES, BASE_STATION);
        snap.corp_governance.insert(CORP_ID, gov);
    });
    db.with_snapshot(|s| s.clone())
}

#[test]
fn effective_corp_roles_at_station_covers_ceo_hq_base_other_and_missing_corp() {
    let mut snap = build_snapshot();
    let now = 1_700_000_000;

    // CEO gets full bitmask
    assert_eq!(
        effective_corp_roles_at_station_in_snap(&snap, CORP_ID, CHAR_CEO, HQ_STATION, now),
        (true, !0u64)
    );

    // Missing corp returns (false, eff.roles)
    assert_eq!(
        effective_corp_roles_at_station_in_snap(&snap, 98_999_999, CHAR_PLAIN, HQ_STATION, now),
        (false, 0)
    );

    // Location-specific roles at HQ, base, other, and station_id <= 0
    let (_, hq_roles) =
        effective_corp_roles_at_station_in_snap(&snap, CORP_ID, CHAR_LOC_ROLES, HQ_STATION, now);
    assert_ne!(hq_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[0], 0);
    assert_eq!(hq_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[2], 0);

    let (_, base_roles) =
        effective_corp_roles_at_station_in_snap(&snap, CORP_ID, CHAR_LOC_ROLES, BASE_STATION, now);
    assert_ne!(base_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[2], 0);
    assert_eq!(base_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[0], 0);

    let (_, other_roles) =
        effective_corp_roles_at_station_in_snap(&snap, CORP_ID, CHAR_LOC_ROLES, OTHER_STATION, now);
    assert_ne!(other_roles & corp_roles::ROLE_HANGAR_CAN_TAKE[5], 0);

    let (_, zero_station_roles) =
        effective_corp_roles_at_station_in_snap(&snap, CORP_ID, CHAR_LOC_ROLES, 0, now);
    assert_eq!(zero_station_roles, corp_roles::ROLE_DELIVERIES_HANGAR_TAKE);

    // Fallback HQ to character home/station when corp.hq_station_id is None
    snap.corporations.get_mut(&CORP_ID).unwrap().hq_station_id = None;
    let (_, fallback_hq) =
        effective_corp_roles_at_station_in_snap(&snap, CORP_ID, CHAR_LOC_ROLES, HQ_STATION, now);
    assert_ne!(fallback_hq & corp_roles::ROLE_HANGAR_CAN_TAKE[0], 0);
}

#[test]
fn require_corp_market_order_and_accountant_spend_roles() {
    let snap = build_snapshot();
    let now = 1_700_000_000;

    // Missing corporation
    assert_eq!(
        require_corp_market_order_role(&snap, 98_999_999, CHAR_CEO, 1000, now)
            .unwrap_err()
            .code,
        "CorporationNotFound"
    );
    assert_eq!(
        require_corp_accountant_spend_role(&snap, 98_999_999, CHAR_CEO, 1000, now, "Denied")
            .unwrap_err()
            .code,
        "CorporationNotFound"
    );

    // CEO and Director pass both
    assert!(require_corp_market_order_role(&snap, CORP_ID, CHAR_CEO, 1000, now).is_ok());
    assert!(require_corp_market_order_role(&snap, CORP_ID, CHAR_DIR, 1005, now).is_ok());
    assert!(
        require_corp_accountant_spend_role(&snap, CORP_ID, CHAR_CEO, 1000, now, "Denied").is_ok()
    );
    assert!(
        require_corp_accountant_spend_role(&snap, CORP_ID, CHAR_DIR, 1006, now, "Denied").is_ok()
    );

    // Trader + div 2 (account_key=1001) passes market order, fails div 1 (1000), fails accountant
    assert!(require_corp_market_order_role(&snap, CORP_ID, CHAR_TRADER_DIV2, 1001, now).is_ok());
    assert_eq!(
        require_corp_market_order_role(&snap, CORP_ID, CHAR_TRADER_DIV2, 1000, now)
            .unwrap_err()
            .code,
        "CorpRoleRequired"
    );
    assert_eq!(
        require_corp_accountant_spend_role(&snap, CORP_ID, CHAR_TRADER_DIV2, 1001, now, "BillDenied")
            .unwrap_err()
            .code,
        "BillDenied"
    );

    // Accountant + div 3 (account_key=1002) passes accountant spend, fails div 1 (999 clamped to 1000) & market order
    assert!(require_corp_accountant_spend_role(
        &snap,
        CORP_ID,
        CHAR_ACCT_DIV3,
        1002,
        now,
        "InsDenied"
    )
    .is_ok());
    assert_eq!(
        require_corp_accountant_spend_role(
            &snap,
            CORP_ID,
            CHAR_ACCT_DIV3,
            999,
            now,
            "InsDenied"
        )
        .unwrap_err()
        .code,
        "InsDenied"
    );
    assert_eq!(
        require_corp_market_order_role(&snap, CORP_ID, CHAR_ACCT_DIV3, 1002, now)
            .unwrap_err()
            .code,
        "CorpRoleRequired"
    );
}

#[test]
fn is_item_sellable_on_market_covers_guards_asset_safety_personal_and_corp_hangars() {
    let mut snap = build_snapshot();
    let mut ext = StructureExtendedState::default();
    let now = 1_700_000_000;

    let base_item = |item_id: i64, owner_id: i64, flag: InventoryFlag| InventoryItemRecord {
        item_id,
        type_id: 34, // Tritanium
        owner_id,
        location_id: HQ_STATION,
        flag,
        quantity: 100,
        singleton: false,
        ..Default::default()
    };

    // Flag/type blocks: Locked, Impounded, flag 36 (Skill), type 60 (Asset Safety Wrap), type 3468 (Plastic Wrap)
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &base_item(1, CHAR_PLAIN, InventoryFlag::Locked),
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &base_item(2, CHAR_PLAIN, InventoryFlag::Impounded),
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &base_item(3, CHAR_PLAIN, InventoryFlag::Skill),
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));
    let mut wrap_item = base_item(4, CHAR_PLAIN, InventoryFlag::Hangar);
    wrap_item.type_id = 60;
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &wrap_item,
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));
    wrap_item.type_id = 3_468;
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &wrap_item,
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));

    // Contract-locked item & structure-controller saved ship
    snap.contracts.insert(
        700,
        ContractRecord {
            contract_id: 700,
            status: "Outstanding".into(),
            item_ids: vec![10],
            ..Default::default()
        },
    );
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &base_item(10, CHAR_PLAIN, InventoryFlag::Hangar),
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));
    snap.item_custom_names.insert(
        -7_700_001,
        r#"{"active_pilot_by_structure":{"555":90001006},"previous_ship_by_structure":{"555":11}}"#
            .into(),
    );
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &base_item(11, CHAR_PLAIN, InventoryFlag::Hangar),
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));

    // Asset safety wrap blocks unpaid wrap_id / location_id / item_ids, but allows paid items
    ext.asset_safety_wraps.insert(
        800,
        AssetSafetyWrapRecord {
            wrap_id: 800,
            owner_id: CHAR_PLAIN,
            is_corp: false,
            source_structure_id: 1_030_000_001,
            source_structure_name: "Keep".into(),
            solar_system_id: 30_000_142,
            created_unix: now,
            deliverable_after_unix: now,
            delivered_to_id: Some(HQ_STATION),
            item_ids: vec![12],
            estimated_value_isk: 100.0,
            nearest_npc_station_id: Some(HQ_STATION),
            auto_delivered: true,
            retrieval_fee_paid: false,
        },
    );
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &base_item(800, CHAR_PLAIN, InventoryFlag::Hangar),
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));
    assert!(!is_item_sellable_on_market(
        &snap,
        &ext,
        &base_item(12, CHAR_PLAIN, InventoryFlag::Hangar),
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));
    ext.asset_safety_wraps.get_mut(&800).unwrap().retrieval_fee_paid = true;
    assert!(is_item_sellable_on_market(
        &snap,
        &ext,
        &base_item(12, CHAR_PLAIN, InventoryFlag::Hangar),
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));

    // Personal consumable item
    let personal = base_item(20, CHAR_PLAIN, InventoryFlag::Hangar);
    assert!(is_item_sellable_on_market(
        &snap,
        &ext,
        &personal,
        CHAR_PLAIN,
        CORP_ID,
        false,
        HQ_STATION,
        None,
        now
    ));

    // Corp item rejected when !use_corp, caller_corp_id == 0, wrong owner, or wrong station
    let corp_sag1 = base_item(21, CORP_ID, InventoryFlag::CorpSAG1);
    assert!(!is_item_sellable_on_market(
        &snap, &ext, &corp_sag1, CHAR_DIR, CORP_ID, false, HQ_STATION, None, now
    ));
    assert!(!is_item_sellable_on_market(
        &snap, &ext, &corp_sag1, CHAR_DIR, 0, true, HQ_STATION, None, now
    ));
    assert!(!is_item_sellable_on_market(
        &snap, &ext, &corp_sag1, CHAR_DIR, CORP_ID, true, OTHER_STATION, None, now
    ));

    // CorpDeliveries / Deliveries / Hangar / CorpSAG1..7 permissions
    let corp_deliv = base_item(22, CORP_ID, InventoryFlag::CorpDeliveries);
    let corp_sag3 = base_item(23, CORP_ID, InventoryFlag::CorpSAG3);
    let corp_sag6 = base_item(24, CORP_ID, InventoryFlag::CorpSAG6);
    let corp_cargo = base_item(25, CORP_ID, InventoryFlag::Cargo);

    // Director can sell any valid corp hangar/deliveries flag, but not Cargo
    assert!(is_item_sellable_on_market(
        &snap, &ext, &corp_deliv, CHAR_DIR, CORP_ID, true, HQ_STATION, None, now
    ));
    assert!(is_item_sellable_on_market(
        &snap, &ext, &corp_sag1, CHAR_DIR, CORP_ID, true, HQ_STATION, None, now
    ));
    assert!(is_item_sellable_on_market(
        &snap, &ext, &corp_sag3, CHAR_DIR, CORP_ID, true, HQ_STATION, None, now
    ));
    assert!(!is_item_sellable_on_market(
        &snap, &ext, &corp_cargo, CHAR_DIR, CORP_ID, true, HQ_STATION, None, now
    ));

    // CHAR_LOC_ROLES has Deliveries global, SAG1 at HQ, SAG3 at Base, SAG6 at Other
    assert!(is_item_sellable_on_market(
        &snap, &ext, &corp_deliv, CHAR_LOC_ROLES, CORP_ID, true, HQ_STATION, None, now
    ));
    assert!(is_item_sellable_on_market(
        &snap, &ext, &corp_sag1, CHAR_LOC_ROLES, CORP_ID, true, HQ_STATION, None, now
    ));
    assert!(!is_item_sellable_on_market(
        &snap, &ext, &corp_sag3, CHAR_LOC_ROLES, CORP_ID, true, HQ_STATION, None, now
    ));
    let mut sag3_at_base = corp_sag3.clone();
    sag3_at_base.location_id = BASE_STATION;
    assert!(is_item_sellable_on_market(
        &snap, &ext, &sag3_at_base, CHAR_LOC_ROLES, CORP_ID, true, BASE_STATION, None, now
    ));
    let mut sag6_at_other = corp_sag6.clone();
    sag6_at_other.location_id = OTHER_STATION;
    assert!(is_item_sellable_on_market(
        &snap, &ext, &sag6_at_other, CHAR_LOC_ROLES, CORP_ID, true, OTHER_STATION, None, now
    ));

    // Plain member cannot sell corp items
    assert!(!is_item_sellable_on_market(
        &snap, &ext, &corp_deliv, CHAR_PLAIN, CORP_ID, true, HQ_STATION, None, now
    ));
    assert!(!is_item_sellable_on_market(
        &snap, &ext, &corp_sag1, CHAR_PLAIN, CORP_ID, true, HQ_STATION, None, now
    ));

    // Direct check for `is_item_sellable_on_market_ignoring_contents`: non-empty container returns
    // `false` in `is_item_sellable_on_market` (`require_empty=true`) and `true` in `is_item_sellable_on_market_ignoring_contents` (`require_empty=false`).
    let mut non_empty_cont = base_item(950, CHAR_PLAIN, InventoryFlag::Hangar);
    non_empty_cont.singleton = true;
    snap.items.insert(950, non_empty_cont.clone());
    let mut inside_item = base_item(951, CHAR_PLAIN, InventoryFlag::Cargo);
    inside_item.location_id = 950;
    snap.items.insert(951, inside_item);
    assert!(!is_item_sellable_on_market(
        &snap, &ext, &non_empty_cont, CHAR_PLAIN, CORP_ID, false, HQ_STATION, None, now
    ));
    assert!(super::is_item_sellable_on_market_ignoring_contents(
        &snap, &ext, &non_empty_cont, CHAR_PLAIN, CORP_ID, false, HQ_STATION, None, now
    ));
    let foreign_item = base_item(952, CHAR_CEO, InventoryFlag::Hangar);
    assert!(!super::is_item_sellable_on_market_ignoring_contents(
        &snap, &ext, &foreign_item, CHAR_PLAIN, CORP_ID, false, HQ_STATION, None, now
    ));
}
