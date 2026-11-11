//! Focused coverage and mutation-killing unit tests for `lp_store_catalog`.

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::PyRepTestExt;
use eve_db::durable_domain_types::CorpGovState;
use eve_db::snapshot_ext::{
    CorpMemberRoleAssignment, LoyaltyStoreOfferItemRecord, LoyaltyStoreOfferRecord,
};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, IskCents, WorldRepository, WorldStateSnapshot,
};
use eve_machonet::SessionState;
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

fn mk_inv_type(
    type_id: u32,
    group_id: u32,
    meta_group_id: Option<u32>,
    faction_id: Option<u32>,
    mass: f64,
    published: bool,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id: 6,
        name: format!("Type-{type_id}"),
        mass,
        volume: 1.0,
        capacity: 0.0,
        radius: 10.0,
        base_price: 100.0,
        published,
        portion_size: 1,
        market_group_id: None,
        race_id: None,
        faction_id,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id,
        meta_level: None,
        tech_level: None,
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

#[test]
fn emblem_evermark_pricing_all_ship_groups_factions_mass_and_alliance_tier() {
    // 1. Legacy helper `paragon_emblem_evermark_cost` exhaustive tier + fallback + alliance check
    for &(gid, expected_corp) in &[
        (25, 750),
        (31, 750),
        (237, 750),
        (324, 1_500),
        (1534, 1_500),
        (26, 2_500),
        (28, 2_500),
        (463, 2_500),
        (358, 4_000),
        (1972, 4_000),
        (27, 7_500),
        (513, 7_500),
        (941, 7_500),
        (485, 15_000),
        (4594, 15_000),
        (30, 15_000),
        (99_999, 2_500),
    ] {
        assert_eq!(paragon_emblem_evermark_cost(gid, 1), expected_corp);
        assert_eq!(paragon_emblem_evermark_cost(gid, 2), (expected_corp * 5) / 3);
    }

    // 2. Build synthetic SDE types for base, meta_group==4 faction, empire faction, all 9 pirate factions, and mass variants
    let mut sde = SdeRegistry::default();
    let t_base = 10_001_u32;
    let t_meta4 = 10_002_u32;
    let t_empire = 10_003_u32;
    sde.types
        .insert(t_base, mk_inv_type(t_base, 25, Some(1), None, 1_000.0, true));
    sde.types
        .insert(t_meta4, mk_inv_type(t_meta4, 25, Some(4), None, 1_000.0, true));
    sde.types.insert(
        t_empire,
        mk_inv_type(t_empire, 25, Some(1), Some(500_001), 1_000.0, true),
    );

    let pirate_factions = [
        500_010_u32, 500_011, 500_012, 500_018, 500_019, 500_024, 500_026, 500_028, 500_029,
    ];
    for (i, &pf) in pirate_factions.iter().enumerate() {
        let tid = 10_100 + i as u32;
        sde.types
            .insert(tid, mk_inv_type(tid, 25, Some(1), Some(pf), 1_000.0, true));
        assert_eq!(emblem_evermark_price(&sde, tid, 25, 1), 6_000);
        assert_eq!(emblem_evermark_price(&sde, tid, 25, 2), 7_000);
    }
    let t_pirate = 10_100_u32;

    // Missing ship_type_id falls back cleanly (meta_group=1, faction=None, mass=0.0)
    assert_eq!(emblem_evermark_price(&sde, 888_888, 25, 1), 5_000);
    // Both meta_group == 4 (without faction_id) and non-pirate faction_id trigger `is_faction`
    assert_eq!(emblem_evermark_price(&sde, t_meta4, 25, 1), 5_500);
    assert_eq!(emblem_evermark_price(&sde, t_empire, 25, 1), 5_500);

    // Flat ship groups
    for &(gid, price) in &[
        (31, 2_500),
        (237, 2_500),
        (325, 3_500),
        (324, 6_500),
        (830, 6_500),
        (831, 6_500),
        (834, 6_500),
        (893, 6_500),
        (1283, 6_500),
        (1527, 6_500),
        (541, 9_000),
        (1534, 9_000),
        (1305, 9_000),
        (28, 7_500),
        (380, 9_000),
        (1202, 9_000),
        (358, 11_500),
        (832, 11_500),
        (833, 11_500),
        (894, 11_500),
        (906, 11_500),
        (963, 11_500),
        (1972, 11_500),
        (463, 11_000),
        (543, 11_500),
        (540, 14_000),
        (898, 16_500),
        (900, 16_500),
        (883, 21_000),
        (513, 17_500),
        (902, 17_500),
        (4594, 18_500),
        (77_777, 5_000),
    ] {
        assert_eq!(emblem_evermark_price(&sde, t_base, gid, 1), price, "group {gid}");
        assert_eq!(emblem_evermark_price(&sde, t_base, gid, 2), price + 1_000, "group {gid} alliance");
    }

    // 3-way (pirate / faction / base) groups
    for &(gid, p_pir, p_fac, p_base) in &[
        (25, 6_000, 5_500, 5_000),
        (420, 8_500, 8_000, 7_500),
        (26, 11_000, 10_500, 10_000),
        (419, 13_500, 13_000, 12_500),
        (1201, 13_500, 13_000, 12_500),
        (27, 16_000, 15_500, 15_000),
    ] {
        assert_eq!(emblem_evermark_price(&sde, t_pirate, gid, 1), p_pir);
        assert_eq!(emblem_evermark_price(&sde, t_empire, gid, 1), p_fac);
        assert_eq!(emblem_evermark_price(&sde, t_base, gid, 1), p_base);
    }

    // 2-way (faction / base) capital groups + supercarrier group 659
    for &(gid, p_fac, p_base) in &[
        (485, 18_000, 17_500),
        (1538, 20_500, 20_000),
        (547, 21_500, 21_000),
        (659, 23_500, 22_500),
        (30, 26_000, 25_000),
    ] {
        assert_eq!(emblem_evermark_price(&sde, t_empire, gid, 1), p_fac);
        assert_eq!(emblem_evermark_price(&sde, t_base, gid, 1), p_base);
    }
    assert_eq!(emblem_evermark_price(&sde, t_pirate, 659, 1), 23_500);

    // Group 941 (Industrial Command Ship): Porpoise 42244 or 0 < mass < 50_000_000 -> 16_000, else 18_500
    sde.types
        .insert(42_244, mk_inv_type(42_244, 941, Some(1), None, 0.0, true));
    sde.types
        .insert(10_500, mk_inv_type(10_500, 941, Some(1), None, 0.0, true));
    sde.types.insert(
        10_501,
        mk_inv_type(10_501, 941, Some(1), None, 49_999_999.0, true),
    );
    sde.types.insert(
        10_502,
        mk_inv_type(10_502, 941, Some(1), None, 50_000_000.0, true),
    );
    assert_eq!(emblem_evermark_price(&sde, 42_244, 941, 1), 16_000);
    assert_eq!(emblem_evermark_price(&sde, 10_500, 941, 1), 18_500);
    assert_eq!(emblem_evermark_price(&sde, 10_501, 941, 1), 16_000);
    assert_eq!(emblem_evermark_price(&sde, 10_502, 941, 1), 18_500);
}

#[test]
fn esi_sde_filtering_paragon_catalog_legacy_fallback_and_pyrep_serialization() {
    let empty_sde = SdeRegistry::default();
    assert!(esi_sde_lp_offers(&empty_sde, -999_999).is_empty());

    // Find an imported ESI offer with at least one required item to test `&&` filtering in `esi_sde_lp_offers`
    let sample_with_req = esi_all_lp_offers(&empty_sde)
        .into_iter()
        .find(|o| !o.req_items.is_empty())
        .expect("ESI catalog has offers with req_items");

    let mut partial_sde = SdeRegistry::default();
    partial_sde.types.insert(
        sample_with_req.type_id,
        mk_inv_type(sample_with_req.type_id, 25, None, None, 1.0, true),
    );
    // Missing required item type -> excluded
    assert!(!esi_sde_lp_offers(&partial_sde, sample_with_req.corp_id)
        .iter()
        .any(|o| o.offer_id == sample_with_req.offer_id));

    // Only required items present, main type_id missing -> excluded
    let mut req_only_sde = SdeRegistry::default();
    for &(rt, _) in &sample_with_req.req_items {
        req_only_sde
            .types
            .insert(rt, mk_inv_type(rt, 25, None, None, 1.0, true));
    }
    assert!(!esi_sde_lp_offers(&req_only_sde, sample_with_req.corp_id)
        .iter()
        .any(|o| o.offer_id == sample_with_req.offer_id));

    // Both main type_id and all required item types present -> included
    for &(rt, _) in &sample_with_req.req_items {
        partial_sde
            .types
            .insert(rt, mk_inv_type(rt, 25, None, None, 1.0, true));
    }
    assert!(esi_sde_lp_offers(&partial_sde, sample_with_req.corp_id)
        .iter()
        .any(|o| o.offer_id == sample_with_req.offer_id));

    // Full SDE Paragon catalog load + deduplication in `get_corp_lp_offers_with_sde`
    let real_sde = shared_sde();
    let paragon_sde = paragon_sde_evermark_offers(&real_sde);
    assert!(!paragon_sde.is_empty());
    assert!(paragon_sde.windows(2).all(|w| w[0].offer_id <= w[1].offer_id));

    let db = WorldRepository::with_demo_seed();
    db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.remove(LP_STORE_OFFERS_KEY);
        snap.loyalty_store_offers.remove(&PARAGON_CORP_ID);
        let merged = get_corp_lp_offers_with_sde(&real_sde, snap, PARAGON_CORP_ID);
        assert!(merged.len() >= paragon_sde.len());

        // Invalid JSON in service_state_blobs falls back to empty Vec
        snap.service_state_blobs
            .insert(LP_STORE_OFFERS_KEY.into(), "{not_json".into());
        assert!(load_lp_store_offers(snap).is_empty());

        // Legacy `loyalty_store_offers` conversion (isk_cost * 100, ak_cost Some & None, required_items)
        let custom_corp = 1_000_999_i64;
        snap.loyalty_store_offers.insert(
            custom_corp,
            vec![
                LoyaltyStoreOfferRecord {
                    offer_id: 11,
                    type_id: 34,
                    quantity: 5,
                    lp_cost: 100,
                    isk_cost: 250,
                    ak_cost: None,
                    required_items: vec![LoyaltyStoreOfferItemRecord {
                        type_id: 35,
                        quantity: 3,
                    }],
                },
                LoyaltyStoreOfferRecord {
                    offer_id: 12,
                    type_id: 36,
                    quantity: 1,
                    lp_cost: 400,
                    isk_cost: 0,
                    ak_cost: Some(9),
                    required_items: Vec::new(),
                },
            ],
        );
        let legacy = get_corp_lp_offers(snap, custom_corp);
        assert_eq!(legacy.len(), 2);
        assert_eq!(legacy[0].isk_cost, IskCents(25_000));
        assert_eq!(legacy[0].ak_cost, 0);
        assert_eq!(legacy[0].req_items, vec![(35, 3)]);
        assert_eq!(legacy[1].ak_cost, 9);

        // `save_corp_lp_balances` replaces only target corp and preserves other corps
        snap.corp_lp_balances.insert((98_000_001, CONCORD_CORP_ID), 500);
        snap.corp_lp_balances.insert((98_000_002, CONCORD_CORP_ID), 999);
        let mut new_bals = HashMap::new();
        new_bals.insert(PARAGON_CORP_ID, 777);
        save_corp_lp_balances(snap, 98_000_001, &new_bals);
        assert_eq!(load_corp_lp_balances(snap, 98_000_001).get(&CONCORD_CORP_ID), None);
        assert_eq!(load_corp_lp_balances(snap, 98_000_001).get(&PARAGON_CORP_ID), Some(&777));
        assert_eq!(load_corp_lp_balances(snap, 98_000_002).get(&CONCORD_CORP_ID), Some(&999));
    });

    // `offer_to_pyrep` with negative ak_cost clamped to 0 and min_standing == None omitted
    let offer_no_st = LPOffer {
        offer_id: 77,
        corp_id: CONCORD_CORP_ID,
        type_id: 34,
        quantity: 2,
        lp_cost: 50,
        isk_cost: IskCents(12_500),
        req_items: vec![(35, 4)],
        ak_cost: -5,
        min_standing: None,
    };
    let rep = offer_to_pyrep(&offer_no_st);
    assert_eq!(rep.dict_get("akCost").and_then(|v| v.as_i64()), Some(0));
    assert!(rep.dict_get("minStanding").is_none());
    let req_list = rep.dict_get("reqItems").and_then(|v| v.as_list()).unwrap();
    assert_eq!(req_list.len(), 1);
    assert_eq!(req_list[0], PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(4)]));
}

#[test]
fn check_corp_lp_role_and_hangar_flag_authorization_matrix() {
    let mut snap = WorldStateSnapshot::default();
    let corp_id = 98_500_001_i64;
    let ceo_id = 90_500_001_i64;
    let member_id = 90_500_002_i64;

    // 1. Missing corporation record falls back to session.corp_role
    let mut sess = SessionState::default();
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 1_000)
            .unwrap_err()
            .code,
        "InsufficientCorporationRole"
    );
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 1_000).unwrap(),
        (false, corp_roles::ROLE_DIRECTOR)
    );

    // 2. Insert corporation: CEO gets (true, u64::MAX); non-director member rejected
    let mut corp = { let mut __corp = CorporationRecord::default();
        __corp.corporation_id = corp_id;
        __corp.ceo_id = ceo_id;
        __corp.name = "Test Corp".into();
        __corp.ticker = "TSTC".into();
        __corp };
    corp.member_roles.insert(member_id, 0);
    snap.corporations.insert(corp_id, corp);
    sess.corp_role = 0;

    assert_eq!(
        check_corp_lp_role(&snap, &sess, ceo_id, corp_id, 1_000).unwrap(),
        (true, u64::MAX)
    );
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 1_000)
            .unwrap_err()
            .code,
        "InsufficientCorporationRole"
    );

    // 3. `corp_role_assignments` + `pending_role_grants` maturation boundary at `matured_at == 2_000`
    snap.corp_role_assignments.insert(
        (corp_id, member_id),
        CorpMemberRoleAssignment {
            character_id: member_id,
            roles: corp_roles::ROLE_DIRECTOR,
            ..Default::default()
        },
    );
    let mut gov = CorpGovState::default();
    gov.pending_role_grants
        .insert(member_id, (corp_roles::ROLE_DIRECTOR, 2_000));
    snap.corp_governance.insert(corp_id, gov);

    // Before maturation (1_999): pending bit is stripped -> denied
    assert!(check_corp_lp_role(&snap, &sess, member_id, corp_id, 1_999).is_err());
    // Exact maturation boundary (2_000): pending bit is granted -> allowed
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 2_000).unwrap(),
        (false, corp_roles::ROLE_DIRECTOR)
    );

    // 4. Title-based Director grant: title ID boundaries (0, 1, 64, 65) + `pending_title_role_grants` boundary
    snap.corp_role_assignments.clear();
    {
        let gov_mut = snap.corp_governance.get_mut(&corp_id).unwrap();
        gov_mut.pending_role_grants.clear();
        gov_mut.titles.insert(0, ("OutLow".into(), corp_roles::ROLE_DIRECTOR));
        gov_mut.titles.insert(65, ("OutHigh".into(), corp_roles::ROLE_DIRECTOR));
        gov_mut.titles.insert(1, ("FirstTitle".into(), 0));
        gov_mut.titles.insert(64, ("LastTitle".into(), corp_roles::ROLE_DIRECTOR));
        gov_mut.member_titles.insert(member_id, 1_u64 << 0);
    }
    assert!(check_corp_lp_role(&snap, &sess, member_id, corp_id, 3_000).is_err());

    // Grant bit 63 (tid=64) with unmatured title grant maturing at 3_000
    {
        let gov_mut = snap.corp_governance.get_mut(&corp_id).unwrap();
        gov_mut.member_titles.insert(member_id, 1_u64 << 63);
        gov_mut
            .pending_title_role_grants
            .insert(member_id, (corp_roles::ROLE_DIRECTOR, 3_000));
    }
    assert!(check_corp_lp_role(&snap, &sess, member_id, corp_id, 2_999).is_err());
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 3_000).unwrap(),
        (false, corp_roles::ROLE_DIRECTOR)
    );

    // 5. `corp_lp_flag_take_authorized` complete flag & role matrix
    let valid_corp_flags = [
        InventoryFlag::Hangar,
        InventoryFlag::CorpDeliveries,
        InventoryFlag::CorpSAG1,
        InventoryFlag::CorpSAG2,
        InventoryFlag::CorpSAG3,
        InventoryFlag::CorpSAG4,
        InventoryFlag::CorpSAG5,
        InventoryFlag::CorpSAG6,
        InventoryFlag::CorpSAG7,
    ];
    for flag in valid_corp_flags {
        assert!(corp_lp_flag_take_authorized(true, 0, flag));
        assert!(corp_lp_flag_take_authorized(false, corp_roles::ROLE_DIRECTOR, flag));
        assert!(!corp_lp_flag_take_authorized(false, 0, flag));
    }
    assert!(!corp_lp_flag_take_authorized(true, u64::MAX, InventoryFlag::Cargo));
    assert!(!corp_lp_flag_take_authorized(false, corp_roles::ROLE_DIRECTOR, InventoryFlag::Cargo));

    let sags = [
        InventoryFlag::CorpSAG1,
        InventoryFlag::CorpSAG2,
        InventoryFlag::CorpSAG3,
        InventoryFlag::CorpSAG4,
        InventoryFlag::CorpSAG5,
        InventoryFlag::CorpSAG6,
        InventoryFlag::CorpSAG7,
    ];
    for (idx, flag) in sags.into_iter().enumerate() {
        let right_role = corp_roles::ROLE_HANGAR_CAN_TAKE[idx];
        let wrong_role = corp_roles::ROLE_HANGAR_CAN_TAKE[(idx + 1) % 7];
        assert!(corp_lp_flag_take_authorized(false, right_role, flag));
        assert!(!corp_lp_flag_take_authorized(false, wrong_role, flag));
    }
    assert!(corp_lp_flag_take_authorized(
        false,
        corp_roles::ROLE_DELIVERIES_HANGAR_TAKE,
        InventoryFlag::CorpDeliveries
    ));
    assert!(!corp_lp_flag_take_authorized(
        false,
        corp_roles::ROLE_DELIVERIES_HANGAR_TAKE,
        InventoryFlag::Hangar
    ));
}

#[test]
fn lp_store_offers_roundtrip_pyrep_and_role_bitmask_kill_mutants() {
    let mut snap = WorldStateSnapshot::default();
    let offer_with_standing = LPOffer {
        offer_id: 901,
        corp_id: CONCORD_CORP_ID,
        type_id: 34,
        quantity: 25,
        lp_cost: 5_000,
        isk_cost: IskCents(125_050),
        req_items: vec![(35, 10), (36, 2)],
        ak_cost: 42,
        min_standing: Some(2.75),
    };
    let offer_no_standing = LPOffer {
        offer_id: 902,
        corp_id: PARAGON_CORP_ID,
        type_id: 37,
        quantity: 1,
        lp_cost: 100,
        isk_cost: IskCents(0),
        req_items: Vec::new(),
        ak_cost: -9,
        min_standing: None,
    };

    // Persist via save_lp_store_offers and reload via load_lp_store_offers + get_corp_lp_offers
    save_lp_store_offers(&mut snap, &[offer_with_standing.clone(), offer_no_standing.clone()]);
    let loaded = load_lp_store_offers(&snap);
    assert_eq!(loaded.len(), 2);
    assert_eq!(loaded[0].offer_id, 901);
    assert_eq!(loaded[0].corp_id, CONCORD_CORP_ID);
    assert_eq!(loaded[0].ak_cost, 42);
    assert_eq!(loaded[1].offer_id, 902);
    assert_eq!(get_corp_lp_offers(&snap, CONCORD_CORP_ID).len(), 1);

    // Exact offer_to_pyrep field verification including minStanding
    let rep = offer_to_pyrep(&offer_with_standing);
    assert_eq!(rep.dict_get("offerID").and_then(|v| v.as_i64()), Some(901));
    assert_eq!(rep.dict_get("corpID").and_then(|v| v.as_i64()), Some(CONCORD_CORP_ID));
    assert_eq!(rep.dict_get("typeID").and_then(|v| v.as_i64()), Some(34));
    assert_eq!(rep.dict_get("quantity").and_then(|v| v.as_i64()), Some(25));
    assert_eq!(rep.dict_get("lpCost").and_then(|v| v.as_i64()), Some(5_000));
    assert_eq!(rep.dict_get("iskCost").and_then(|v| v.as_f64()), Some(1_250.5));
    assert_eq!(rep.dict_get("akCost").and_then(|v| v.as_i64()), Some(42));
    assert_eq!(rep.dict_get("minStanding").and_then(|v| v.as_f64()), Some(2.75));
    let rep_no_st = offer_to_pyrep(&offer_no_standing);
    assert_eq!(rep_no_st.dict_get("akCost").and_then(|v| v.as_i64()), Some(0));
    assert!(rep_no_st.dict_get("minStanding").is_none());

    // Role checks with multi-bit masks to kill `== -> !=`, `& -> |`, `& -> ^`, and return tuple mutants
    let corp_id = 98_600_001_i64;
    let ceo_id = 90_600_001_i64;
    let member_id = 90_600_002_i64;
    let mut sess = SessionState::default();

    // Missing corporation fallback: non-director bit (ROLE_ACCOUNTANT) must still be denied
    sess.corp_role = corp_roles::ROLE_ACCOUNTANT;
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 1_000)
            .unwrap_err()
            .code,
        "InsufficientCorporationRole"
    );
    let multi_dir = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT;
    sess.corp_role = multi_dir;
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 1_000).unwrap(),
        (false, multi_dir)
    );

    // Existing corporation: CEO returns (true, u64::MAX); non-director w/ ACCOUNTANT rejected;
    // director w/ ACCOUNTANT returns exact (false, multi_dir)
    let mut corp = CorporationRecord::default();
    corp.corporation_id = corp_id;
    corp.ceo_id = ceo_id;
    corp.name = "LP Mutant Corp".into();
    corp.ticker = "LPMC".into();
    corp.member_roles.insert(member_id, corp_roles::ROLE_ACCOUNTANT);
    snap.corporations.insert(corp_id, corp);
    sess.corp_role = 0;

    assert_eq!(
        check_corp_lp_role(&snap, &sess, ceo_id, corp_id, 1_000).unwrap(),
        (true, u64::MAX)
    );
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 1_000)
            .unwrap_err()
            .code,
        "InsufficientCorporationRole"
    );

    snap.corporations
        .get_mut(&corp_id)
        .unwrap()
        .member_roles
        .insert(member_id, multi_dir);
    assert_eq!(
        check_corp_lp_role(&snap, &sess, member_id, corp_id, 1_000).unwrap(),
        (false, multi_dir)
    );
}
