use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;

#[test]
fn test_lp_store_catalog_helpers_and_persistence() {
    let (ctx, _session, _char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();

    let custom = LPOffer {
        offer_id: 888_111,
        corp_id: 1_000_125,
        type_id: 34,
        quantity: 10,
        lp_cost: 250,
        isk_cost: IskCents::from_isk(5_000),
        ak_cost: 5,
        req_items: vec![(35, 2)],
        min_standing: Some(2.5),
    };

    ctx.db.with_mut_snapshot(|snap| {
        register_lp_store_offer(snap, custom.clone());
        let loaded = load_lp_store_offers(snap);
        assert!(loaded.iter().any(|o| o.offer_id == 888_111 && o.ak_cost == 5));

        // Re-registering same offer_id replaces existing offer
        let mut updated = custom.clone();
        updated.lp_cost = 300;
        register_lp_store_offer(snap, updated);
        let loaded2 = load_lp_store_offers(snap);
        assert_eq!(
            loaded2.iter().find(|o| o.offer_id == 888_111).unwrap().lp_cost,
            300
        );

        let corp_offers = get_corp_lp_offers(snap, 1_000_125);
        assert!(corp_offers.iter().any(|o| o.offer_id == 888_111));

        let with_sde = get_corp_lp_offers_with_sde(&ctx.sde, snap, 1_000_125);
        assert!(with_sde.iter().any(|o| o.offer_id == 888_111));

        let mut bals = load_corp_lp_balances(snap, 98_000_001);
        bals.insert(1_000_125, 12_345);
        save_corp_lp_balances(snap, 98_000_001, &bals);
        let bals2 = load_corp_lp_balances(snap, 98_000_001);
        assert_eq!(bals2.get(&1_000_125), Some(&12_345));
    });

    // Test paragon_emblem_evermark_cost tiers
    assert_eq!(paragon_emblem_evermark_cost(25, 1), 750);
    assert_eq!(paragon_emblem_evermark_cost(25, 2), 1_250);
    assert_eq!(paragon_emblem_evermark_cost(420, 1), 1_500);
    assert_eq!(paragon_emblem_evermark_cost(26, 1), 2_500);
    assert_eq!(paragon_emblem_evermark_cost(419, 1), 4_000);
    assert_eq!(paragon_emblem_evermark_cost(27, 1), 7_500);
    assert_eq!(paragon_emblem_evermark_cost(30, 1), 15_000);

    // offer_to_pyrep
    let rep = offer_to_pyrep(&custom);
    assert_eq!(rep.dict_get("offerID").and_then(|v| v.as_i64()), Some(888_111));
    assert_eq!(rep.dict_get("akCost").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(rep.dict_get("minStanding").and_then(|v| v.as_f64()), Some(2.5));
    let Some(PyRep::List(req_items)) = rep.dict_get("reqItems") else {
        panic!("expected reqItems list");
    };
    assert_eq!(req_items.as_slice(), &[PyRep::Tuple(vec![PyRep::Integer(35), PyRep::Integer(2)])]);

    // esi_sde_lp_offers & esi_all_lp_offers with empty SDE types fallback
    let empty_sde = eve_sde::SdeRegistry::default();
    let sde_offers = esi_sde_lp_offers(&empty_sde, 1_000_125);
    assert!(!sde_offers.is_empty());
    let all_offers = esi_all_lp_offers(&empty_sde);
    assert!(!all_offers.is_empty());
}
