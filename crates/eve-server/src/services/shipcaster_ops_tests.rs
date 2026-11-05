use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::cluster_macho_misc_services::with_misc_state;
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

#[tokio::test]
async fn test_shipcaster_ops_pads_launchers_linking_disruption_and_rpcs() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let cid = 90_000_001;
    let kw = HashMap::new();

    assert!(handle(&ctx, &mut sess, "other", "GetAllLandingPads", &[], &kw).await.is_none());
    assert!(!is_capital_or_freighter_group(25));
    assert!(is_capital_or_freighter_group(547));

    // 1. active_linked_pad_for_launcher rotation across 0, 1, and multiple pads
    let launcher = LauncherRecord {
        item_id: 9001,
        type_id: 75900,
        solar_system_id: 30_002_537,
        faction_id: 500_001,
        linked_pad_ids: vec![101, 102],
        position: DVec3::ZERO,
    };
    let pad1 = LandingPadRecord {
        item_id: 101,
        type_id: 75928,
        built_type_id: 75928,
        solar_system_id: 30_002_538,
        faction_id: 500_001,
        is_built: true,
        link_timestamp: 100,
        is_disrupted: false,
        position: DVec3::ZERO,
        required_materials: BTreeMap::new(),
        contributed_materials: BTreeMap::new(),
    };
    let mut pad2 = pad1.clone();
    pad2.item_id = 102;
    assert!(active_linked_pad_for_launcher(&launcher, &[], 0).is_none());
    assert_eq!(
        active_linked_pad_for_launcher(&launcher, &[pad1.clone()], 0).map(|p| p.item_id),
        Some(101)
    );
    let p_t0 = active_linked_pad_for_launcher(&launcher, &[pad1.clone(), pad2.clone()], 0)
        .unwrap()
        .item_id;
    let p_t1 = active_linked_pad_for_launcher(
        &launcher,
        &[pad1.clone(), pad2.clone()],
        SHIPCASTER_TARGET_CYCLE_SECS,
    )
    .unwrap()
    .item_id;
    assert_ne!(p_t0, p_t1);

    // Seed pad1 into misc state so Empire landing pad queries & link/disrupt have a built 500_001 pad
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(pad1.clone());
    });

    // 2. GetAllLandingPads, GetFactionLandingPads, GetFactionsWithShipcaster
    let all_pads = handle(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(all_pads, PyRep::List(ref l) if !l.is_empty()));

    assert!(handle(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetFactionLandingPads", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(0)],
        &kw
    )
    .await
    .unwrap()
    .is_err());
    let fac_pads = handle(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(fac_pads, PyRep::List(ref l) if !l.is_empty()));

    let facs = handle(
        &ctx,
        &mut sess,
        "shipCasterLauncherMgr",
        "GetFactionsWithShipcaster",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(facs, PyRep::List(ref l) if l.len() == 4));

    let _ = active_target_pad_for_faction(&ctx, 500_001);

    // 3. Link & disrupt/destroy landing pads
    assert!(link_fw_landing_pad(&ctx, -999).is_err());
    let sample_pad_id = with_misc_state(&ctx, |st| {
        st.landing_pads
            .iter()
            .find(|p| p.faction_id == 500_001 && p.is_built)
            .map(|p| p.item_id)
            .unwrap()
    });
    let disrupted = disrupt_or_destroy_fw_landing_pad(&ctx, sample_pad_id, false)
        .await
        .unwrap();
    assert!(disrupted.is_disrupted);
    assert_eq!(disrupted.link_timestamp, 0);
    // Undisrupt so we can link again
    with_misc_state(&ctx, |st| {
        if let Some(p) = st.landing_pads.iter_mut().find(|p| p.item_id == sample_pad_id) {
            p.is_disrupted = false;
        }
    });
    let relinked = link_fw_landing_pad(&ctx, sample_pad_id).unwrap();
    assert!(relinked.link_timestamp > 0);

    // 4. CanCharacterJump & CmdJumpThroughShipcaster error branches
    let can_jump = handle(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(can_jump, PyRep::Bool(_)));

    sess.station_id = Some(60_003_760);
    assert!(handle(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(1)],
        &kw
    )
    .await
    .unwrap()
    .is_err());
    assert!(handle(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(1), PyRep::Integer(30_000_142), PyRep::Integer(1)],
        &kw
    )
    .await
    .unwrap()
    .is_err());

    sess.station_id = None;
    sess.ship_id = Some(999_001);
    sess.solar_system_id = Some(30_000_142);
    assert!(handle(&ctx, &mut sess, "shipcasterTravelMgr", "CmdJumpThroughShipcaster", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[PyRep::Integer(999_999_999), PyRep::Integer(30_000_142), PyRep::Integer(1)],
        &kw
    )
    .await
    .unwrap()
    .is_err());

    // 5. deploy_fw_beacon_constructor & deposit_items_from_ship validation
    assert!(deploy_fw_beacon_constructor(&ctx, &sess, 999_999, DVec3::ZERO)
        .await
        .is_err());
    assert!(contribute_fw_beacon_material(&ctx, &sess, sample_pad_id, 999_999, 0)
        .await
        .is_err());
    assert!(deposit_items_from_ship(&ctx, &sess, sample_pad_id, None)
        .await
        .is_err());
    let dep_empty = deposit_items_from_ship(
        &ctx,
        &sess,
        sample_pad_id,
        Some(&PyRep::Dict(vec![(PyRep::Integer(34), PyRep::Integer(5))])),
    )
    .await
    .unwrap();
    if let PyRep::Tuple(items) = dep_empty {
        assert_eq!(items.first(), Some(&PyRep::Bool(false)));
    } else {
        panic!("expected Tuple");
    }
    let _ = cid;
}
