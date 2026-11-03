//! E2E parity closure tests for Batch 4 remediated RPCs:
//! - `seasonalLoginCampaignManager`: `get_active_campaign`, `get_claim_history`, `claim_reward`
//! - `shipCasterLandingPadMgr`: `GetAllLandingPads`, `GetFactionLandingPads`
//! - `shipCasterLauncherMgr`: `GetFactionsWithShipcaster`
//! - `starterShipcasterTravelMgr`: `CanCharacterJump`, `CmdJumpThroughStarterShipcaster`
//! - `shipcasterTravelMgr`: `CmdJumpThroughShipcaster`
//! - `storeManager`: `get_categories`, `get_products`, `get_offers`, `can_purchase_offer`, `buy_offer`

use crate::common;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::cluster_macho_misc_services::{with_misc_state, LandingPadRecord};
use eve_server::services::cosmetics_ui_ops::{
    load_cosmetics_ui_state, save_cosmetics_ui_state, SeasonalCampaignSchedule,
};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        PyRep::KeyVal(map) => map.get(key),
        _ => None,
    }
}

#[tokio::test]
async fn test_batch4_seasonal_login_campaign_manager_rpcs() {
    // parity: rpc:seasonalLoginCampaignManager.get_active_campaign
    // parity: rpc:seasonalLoginCampaignManager.get_claim_history
    // parity: rpc:seasonalLoginCampaignManager.claim_reward
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut unauth = SessionState::new(94_001);
    assert!(
        rpc(&ctx, &mut unauth, "seasonalLoginCampaignManager", "get_active_campaign", &[])
            .await
            .is_err(),
        "unauthenticated get_active_campaign must fail"
    );
    assert!(
        rpc(&ctx, &mut unauth, "seasonalLoginCampaignManager", "get_claim_history", &[])
            .await
            .is_err(),
        "unauthenticated get_claim_history must fail"
    );
    assert!(
        rpc(&ctx, &mut unauth, "seasonalLoginCampaignManager", "claim_reward", &[])
            .await
            .is_err(),
        "unauthenticated claim_reward must fail"
    );

    let mut sess = SessionState::new(94_002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.active_seasonal_schedule = Some(SeasonalCampaignSchedule {
            campaign_id: 2323,
            start_unix: now - 3_600,
            end_unix: now + 10 * 86_400,
            turnover_secs: 11 * 3_600,
        });
        save_cosmetics_ui_state(snap, &ext);
    });

    let active = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_active_campaign", &[])
        .await
        .expect("get_active_campaign");
    let PyRep::Tuple(fields) = active else {
        panic!("expected Tuple from get_active_campaign");
    };
    assert_eq!(fields[0], PyRep::Integer(2323));
    assert!(fields[3].as_i64().unwrap_or(0) > 0);

    let hist_before = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
        .await
        .expect("get_claim_history before");
    assert_eq!(hist_before, PyRep::Dict(vec![]));

    let claimed = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect("claim_reward");
    assert_eq!(claimed, PyRep::Bool(true));

    let hist_after = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "get_claim_history", &[])
        .await
        .expect("get_claim_history after");
    let PyRep::Dict(entries) = hist_after else {
        panic!("expected Dict from get_claim_history");
    };
    assert_eq!(entries.len(), 1);

    let repeat_err = rpc(&ctx, &mut sess, "seasonalLoginCampaignManager", "claim_reward", &[])
        .await
        .expect_err("repeat claim on same day must fail");
    assert!(repeat_err
        .message
        .to_ascii_lowercase()
        .contains("already claimed"));
}

#[tokio::test]
async fn test_batch4_shipcaster_pads_launchers_and_travel_rpcs() {
    // parity: rpc:shipCasterLandingPadMgr.GetAllLandingPads
    // parity: rpc:shipCasterLandingPadMgr.GetFactionLandingPads
    // parity: rpc:shipCasterLauncherMgr.GetFactionsWithShipcaster
    // parity: rpc:starterShipcasterTravelMgr.CanCharacterJump
    // parity: rpc:starterShipcasterTravelMgr.CmdJumpThroughStarterShipcaster
    // parity: rpc:shipcasterTravelMgr.CmdJumpThroughShipcaster
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.advance_secs(600);

    let mut unauth = SessionState::new(94_010);
    assert!(
        rpc(&ctx, &mut unauth, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
            .await
            .is_err()
    );
    assert!(
        rpc(
            &ctx,
            &mut unauth,
            "shipCasterLandingPadMgr",
            "GetFactionLandingPads",
            &[PyRep::Integer(500001)]
        )
        .await
        .is_err()
    );
    assert!(
        rpc(&ctx, &mut unauth, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[])
            .await
            .is_err()
    );

    let mut sess = SessionState::new(94_011);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let bad_faction = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(0)],
    )
    .await
    .expect_err("non-positive factionID must fail");
    assert_eq!(bad_faction.code, "InvalidArguments");

    let factions = rpc(&ctx, &mut sess, "shipCasterLauncherMgr", "GetFactionsWithShipcaster", &[])
        .await
        .expect("GetFactionsWithShipcaster");
    let PyRep::List(faction_list) = factions else {
        panic!("expected List");
    };
    assert_eq!(faction_list.len(), 4);

    // Seed a built & linked Caldari landing pad in Onatoh (30002811)
    let now = ctx.now_unix();
    let pad_id = 109_000_001_i64;
    with_misc_state(&ctx, |st| {
        st.landing_pads.push(LandingPadRecord {
            item_id: pad_id,
            type_id: 75928,
            built_type_id: 75928,
            faction_id: 500001,
            solar_system_id: 30002811,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            is_built: true,
            is_disrupted: false,
            link_timestamp: now as i64,
            required_materials: BTreeMap::new(),
            contributed_materials: BTreeMap::new(),
        });
        if let Some(launcher) = st.launchers.iter_mut().find(|l| l.faction_id == 500001) {
            launcher.linked_pad_ids.push(pad_id);
        }
    });

    let all_pads = rpc(&ctx, &mut sess, "shipCasterLandingPadMgr", "GetAllLandingPads", &[])
        .await
        .expect("GetAllLandingPads");
    let PyRep::List(all_list) = all_pads else {
        panic!("expected List");
    };
    assert_eq!(all_list.len(), 1);

    let cal_pads = rpc(
        &ctx,
        &mut sess,
        "shipCasterLandingPadMgr",
        "GetFactionLandingPads",
        &[PyRep::Integer(500001)],
    )
    .await
    .expect("GetFactionLandingPads");
    let PyRep::List(cal_list) = cal_pads else {
        panic!("expected List");
    };
    assert_eq!(cal_list.len(), 1);
    assert_eq!(dict_get(&cal_list[0], "itemID"), Some(&PyRep::Integer(pad_id)));

    // Starter shipcaster eligibility & jump
    let starter = with_misc_state(&ctx, |st| st.starter_launchers.first().cloned())
        .expect("seeded starter launcher");
    let ship_id = sess.ship_id.unwrap_or(100_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.create_date_unix = now - 3_600;
            ch.solar_system_id = starter.solar_system_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.war_faction_id = Some(500001);
        }
    });
    sess.solar_system_id = Some(starter.solar_system_id);
    sess.station_id = None;
    sess.structure_id = None;

    let can_jump = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump");
    assert_eq!(can_jump, PyRep::Bool(true));

    let origin_sys = ctx.systems.get_or_spawn(starter.solar_system_id);
    let _ = origin_sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: starter.position + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await;

    ctx.advance_secs(15);
    rpc(
        &ctx,
        &mut sess,
        "starterShipcasterTravelMgr",
        "CmdJumpThroughStarterShipcaster",
        &[PyRep::Integer(starter.item_id)],
    )
    .await
    .expect("CmdJumpThroughStarterShipcaster");
    assert_eq!(sess.solar_system_id, Some(starter.staging_system_id));

    let can_jump_after = rpc(&ctx, &mut sess, "starterShipcasterTravelMgr", "CanCharacterJump", &[])
        .await
        .expect("CanCharacterJump after");
    assert_eq!(can_jump_after, PyRep::Bool(false));

    // Interstellar shipcaster jump from Caldari War HQ launcher to Onatoh pad
    ctx.advance_secs(30);
    let cal_launcher = with_misc_state(&ctx, |st| {
        st.launchers.iter().find(|l| l.faction_id == 500001).cloned()
    })
    .expect("Caldari launcher");
    let hq_sys = ctx.systems.get_or_spawn(cal_launcher.solar_system_id);
    let _ = hq_sys.remove_ball(ship_id).await;
    let _ = hq_sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 587,
                position: cal_launcher.position + DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, 587),
            ctx.build_ship_slim_meta(char_id, 587),
        )
        .await;
    let _ = hq_sys
        .spawn_ball_with_meta(
            Ball {
                id: cal_launcher.item_id,
                type_id: cal_launcher.type_id,
                position: cal_launcher.position,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e10,
                radius: 5_000.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
            ctx.build_ship_combat_state(0, cal_launcher.item_id, cal_launcher.type_id),
            SlimItemMeta::default(),
        )
        .await;
    sess.solar_system_id = Some(cal_launcher.solar_system_id);

    rpc(
        &ctx,
        &mut sess,
        "shipcasterTravelMgr",
        "CmdJumpThroughShipcaster",
        &[
            PyRep::Integer(cal_launcher.item_id),
            PyRep::Integer(30002811),
            PyRep::Integer(pad_id),
        ],
    )
    .await
    .expect("CmdJumpThroughShipcaster");
    assert_eq!(sess.solar_system_id, Some(30002811));
}

#[tokio::test]
async fn test_batch4_store_manager_rpcs() {
    // parity: rpc:storeManager.get_categories
    // parity: rpc:storeManager.get_products
    // parity: rpc:storeManager.get_offers
    // parity: rpc:storeManager.can_purchase_offer
    // parity: rpc:storeManager.buy_offer
    let ctx = ServerContext::new();
    common::seed_test_nes_store_catalog(&ctx);

    let mut unauth = SessionState::new(94_020);
    assert!(rpc(&ctx, &mut unauth, "storeManager", "get_categories", &[]).await.is_err());
    assert!(rpc(&ctx, &mut unauth, "storeManager", "get_products", &[]).await.is_err());
    assert!(rpc(&ctx, &mut unauth, "storeManager", "get_offers", &[]).await.is_err());
    assert!(
        rpc(&ctx, &mut unauth, "storeManager", "can_purchase_offer", &[PyRep::Integer(1002)])
            .await
            .is_err()
    );
    assert!(
        rpc(&ctx, &mut unauth, "storeManager", "buy_offer", &[PyRep::Integer(1002)])
            .await
            .is_err()
    );

    let mut sess = SessionState::new(94_021);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    let PyRep::List(cats) = rpc(&ctx, &mut sess, "storeManager", "get_categories", &[])
        .await
        .expect("get_categories")
    else {
        panic!("expected List");
    };
    assert!(!cats.is_empty());

    let PyRep::List(prods) = rpc(&ctx, &mut sess, "storeManager", "get_products", &[])
        .await
        .expect("get_products")
    else {
        panic!("expected List");
    };
    assert!(!prods.is_empty());

    let PyRep::List(offers) = rpc(&ctx, &mut sess, "storeManager", "get_offers", &[])
        .await
        .expect("get_offers")
    else {
        panic!("expected List");
    };
    assert!(!offers.is_empty());

    // Zero/negative quantity must be rejected
    let bad_qty = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(0)],
    )
    .await
    .expect_err("zero quantity must fail");
    assert_eq!(bad_qty.code, "InvalidQuantity");

    // AUR currency must be rejected
    let aur_err = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("AUR".into()), PyRep::Integer(1)],
    )
    .await
    .expect_err("AUR must fail");
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    ctx.db.with_mut_snapshot(|snap| {
        snap.adjust_plex_vault(char_id, 250).unwrap();
    });
    let can_buy = rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(1002)])
        .await
        .expect("can_purchase_offer");
    assert_eq!(can_buy, PyRep::Bool(true));

    let plex_before = ctx.db.with_snapshot(|snap| snap.plex_vault_balance(char_id));
    let buy_res = rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .expect("buy_offer");
    assert_eq!(dict_get(&buy_res, "success"), Some(&PyRep::Bool(true)));
    let plex_after = ctx.db.with_snapshot(|snap| snap.plex_vault_balance(char_id));
    assert_eq!(plex_before - plex_after, 110);

    let delivered = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|i| i.owner_id == char_id && i.flag == InventoryFlag::Hangar)
    });
    assert!(delivered, "purchased NES item must be delivered to hangar");
}
