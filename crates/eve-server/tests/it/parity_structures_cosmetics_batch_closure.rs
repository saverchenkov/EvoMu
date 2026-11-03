//! End-to-end parity closure tests for the 11 remediated `feat:*` rows:
//! - `feat:wallet.nes-store`, `feat:wallet.evermarks`, `feat:wallet.hypernet-relay`
//! - `feat:structures.anchor-restrictions`, `feat:structures.profiles`,
//!   `feat:structures.defense-settings`, `feat:structures.structure-directory`
//! - `feat:cosmetics.skinr-designer`, `feat:cosmetics.skinr-market`
//! - `feat:serverops.daily-jobs`, `feat:serverops.time-dilation`

use crate::common;
use eve_db::{
    AsteroidRecord, CharacterRecord, ContractRecord, CorporationRecord, InventoryFlag,
    InventoryItemRecord, IskCents, NeuralAttributes, SocialContactRecord, SystemDynamicState,
    TrainedSkillRecord,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_skinr::COMP_CAT_PATTERN;
use std::collections::HashMap;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b).expect("decode EvePublicResponse");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

async fn quasar_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    type_name: &str,
    value: Vec<u8>,
) -> EvePublicResponse {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: format!("closure-test-{n:06}").into_bytes(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"closure-it-suite".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rep = ctx
        .dispatch_rpc(sess, "publicGatewaySvc", None, "SendRequest", &[PyRep::Bytes(req.encode_proto())], &HashMap::new(), None)
        .await
        .expect("publicGatewaySvc.SendRequest");
    extract_response_proto(&rep)
}

async fn call_rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn make_sess(sid: u64, uid: u64, cid: i64, corp_id: i64, sys_id: u32, station_id: Option<i64>, role: u64, corp_role: u64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s.role = role;
    s.corp_role = corp_role;
    s
}

fn register_char(ctx: &ServerContext, uid: u64, cid: i64, corp_id: i64, sys_id: u32, station_id: i64, isk: i64, plex: i64) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let ship_id = snap.next_entity_id;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.characters.insert(
            cid,
            CharacterRecord {
                activity_status: None, character_id: cid, user_id: uid, name: format!("Pilot {cid}"),
                corporation_id: corp_id, alliance_id: None, war_faction_id: None,
                station_id: Some(station_id), structure_id: None, solar_system_id: sys_id,
                constellation_id: 20_000_020, region_id: 10_000_002, active_ship_id: ship_id,
                balance: IskCents::from_isk(isk), plex_balance: plex,
                loyalty_points: HashMap::new(), standings: HashMap::new(), security_status: 0.0,
                clone_grade: 1, home_station_id: station_id, race_id: 1, bloodline_id: 1,
                ancestry_id: 1, school_id: 11,
                neural: NeuralAttributes::default(),
                skill_points: 0.0, free_skill_points: 0.0, last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0, jump_fatigue_until_unix: 0,
                description: String::new(), title: String::new(), gender: 1,
                create_date_unix: 1_700_000_000, fleet_id: None, autopilot_waypoints: Vec::new(),
                version: 1, system_epoch: 1,
            },
        );
        snap.account_plex_vault.insert(uid, plex);
        ship_id
    })
}

#[tokio::test]
async fn test_wallet_nes_evermarks_hypernet_closure() {
    // parity: feat:wallet.nes-store, feat:wallet.evermarks, feat:wallet.hypernet-relay
    eve_testkit::attest(&[
        "feat:wallet.nes-store",
        "feat:wallet.evermarks",
        "feat:wallet.hypernet-relay",
    ]);

    let ctx = ServerContext::new();
    let t0 = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0)));
    common::seed_test_nes_store_catalog(&ctx);

    let corp_id = 98_100_001_i64;
    let cid = 2_100_000_101_i64;
    let buyer_cid = 2_100_000_102_i64;
    let jita_station = 60_003_760_i64;
    register_char(&ctx, 501, cid, corp_id, 30_000_142, jita_station, 5_000_000_000, 1_000);
    register_char(&ctx, 502, buyer_cid, 1_000_009, 30_000_142, jita_station, 5_000_000_000, 100);
    let mut sess = make_sess(10, 501, cid, corp_id, 30_000_142, Some(jita_station), 0, 1);

    // 1. NES Store: catalog, Omega purchase, weekly 15,000 SP reward & Tuesday reset
    let offers = call_rpc(&ctx, &mut sess, "storeManager", "get_offers", &[]).await.expect("get_offers");
    assert!(matches!(offers, PyRep::List(ref xs) if !xs.is_empty()));
    let can_alpha = call_rpc(&ctx, &mut sess, "storeManager", "can_purchase_offer", &[PyRep::Integer(1005)])
        .await
        .expect("can_purchase_offer 1005");
    assert_eq!(can_alpha, PyRep::Bool(false));

    call_rpc(
        &ctx,
        &mut sess,
        "storeManager",
        "buy_offer",
        &[PyRep::Integer(1001), PyRep::String("PLX".into()), PyRep::Integer(1)],
    )
    .await
    .expect("buy_offer 1001");
    call_rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(1005)])
        .await
        .expect("buy_offer 1005");
    assert!(
        call_rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(1005)]).await.is_err(),
        "duplicate weekly SP claim must fail"
    );
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0 + 7 * 86_400)));
    call_rpc(&ctx, &mut sess, "storeManager", "buy_offer", &[PyRep::Integer(1005)])
        .await
        .expect("buy_offer 1005 after weekly reset");

    // 2. EverMarks: accrue via Quasar LP wallet (verifying 1-to-1 corp EverMark match) & spend in Paragon SDE LP store
    let mut admin_sess = make_sess(13, 501, cid, corp_id, 30_000_142, Some(jita_station), eve_machonet::service_roles::ROLE_GM_DEFAULT, 1);
    let mut dep_req = Vec::new();
    let mut op_id_m = Vec::new();
    encode_bytes_field(1, b"paragon-em-op-01", &mut op_id_m);
    let mut cid_msg = Vec::new();
    encode_varint_field(1, cid as u64, &mut cid_msg);
    let mut issuer_msg = Vec::new();
    encode_varint_field(1, 1_000_419, &mut issuer_msg);
    let mut wallet_id = Vec::new();
    encode_bytes_field(1, &cid_msg, &mut wallet_id);
    encode_bytes_field(3, &issuer_msg, &mut wallet_id);
    let mut curr_m = Vec::new();
    encode_varint_field(1, 50_000, &mut curr_m);
    let mut op_attr = Vec::new();
    encode_bytes_field(1, &wallet_id, &mut op_attr);
    encode_bytes_field(2, &curr_m, &mut op_attr);
    encode_bytes_field(1, &op_id_m, &mut dep_req);
    encode_bytes_field(2, &op_attr, &mut dep_req);
    let dep_rsp = quasar_rpc(&ctx, &mut admin_sess, "eve.loyaltypoints.assetholding.operation.api.DepositRequest", dep_req).await;
    assert_eq!(dep_rsp.status_code, STATUS_OK);

    let (char_em, corp_em) = ctx.db.with_snapshot(|s| {
        let cem = s.characters.get(&cid).and_then(|c| c.loyalty_points.get(&1_000_419).copied()).unwrap_or(0);
        let corpem = s.corp_lp_balances.get(&(corp_id, 1_000_419)).copied().unwrap_or(0);
        (cem, corpem)
    });
    assert_eq!(char_em, 50_000);
    assert_eq!(corp_em, 50_000, "corp must receive EverMarks 1-to-1");

    let paragon_offers = call_rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_419)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let first_offer_id = match &paragon_offers {
        PyRep::List(rows) => rows[0].dict_get("offerID").and_then(|v| v.as_i64()).expect("offerID"),
        _ => panic!("expected List of offers"),
    };
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = Some(60_015_169);
        }
    });
    sess.station_id = Some(60_015_169); // Jita VI - Paragon Fulfillment Center
    call_rpc(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(1_000_419), PyRep::Integer(first_offer_id), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter");
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = Some(jita_station);
        }
    });
    sess.station_id = Some(jita_station);
    let char_em_after = ctx.db.with_snapshot(|s| s.characters.get(&cid).and_then(|c| c.loyalty_points.get(&1_000_419).copied()).unwrap_or(0));
    assert!(char_em_after < char_em);

    // 3. HyperNet Relay: NPC station enforcement, HyperCore consumption, random ticket purchase, and winner delivery
    let rifter_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let rid = snap.next_entity_id;
        snap.items.insert(
            rid,
            InventoryItemRecord {
                item_id: rid,
                type_id: 587,
                owner_id: cid,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.next_entity_id += 1;
        let hid = snap.next_entity_id;
        snap.items.insert(
            hid,
            InventoryItemRecord {
                item_id: hid,
                type_id: 52_568,
                owner_id: cid,
                location_id: jita_station,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        rid
    });
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = None;
            ch.structure_id = Some(1_000_000_000_001);
        }
    });
    assert!(
        call_rpc(
            &ctx,
            &mut sess,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Integer(rifter_id), PyRep::Real(1_000_000.0), PyRep::Integer(8)],
        )
        .await
        .is_err()
    );
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&cid) {
            ch.station_id = Some(jita_station);
            ch.structure_id = None;
        }
    });
    let create_res = call_rpc(
        &ctx,
        &mut sess,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Integer(rifter_id), PyRep::Real(1_000_000.0), PyRep::Integer(8)],
    )
    .await
    .expect("CreateRaffle");
    let raffle_id = create_res.dict_get("raffleID").and_then(|v| v.as_i64()).expect("raffleID");
    let mut buyer_sess = make_sess(12, 502, buyer_cid, 1_000_009, 30_000_142, Some(jita_station), 0, 0);
    call_rpc(
        &ctx,
        &mut buyer_sess,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(8)],
    )
    .await
    .expect("BuyRandomTickets");
    call_rpc(&ctx, &mut buyer_sess, "raffleProxy", "AwardItem", &[PyRep::Integer(raffle_id)])
        .await
        .expect("AwardItem");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&rifter_id).map(|i| i.owner_id)), Some(buyer_cid));
}

#[tokio::test]
async fn test_structures_anchoring_profiles_defense_directory_closure() {
    // parity: feat:structures.anchor-restrictions, feat:structures.profiles, feat:structures.defense-settings, feat:structures.structure-directory
    eve_testkit::attest(&[
        "feat:structures.anchor-restrictions",
        "feat:structures.profiles",
        "feat:structures.defense-settings",
        "feat:structures.structure-directory",
    ]);

    let ctx = ServerContext::new();
    let t0 = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0)));

    let corp_id = 98_200_001_i64;
    let cid = 2_100_000_201_i64;
    let attacker_cid = 2_100_000_202_i64;
    let lowsec_sys = 30_002_718_u32;
    let jita_station = 60_003_760_i64;
    let ship_id = register_char(&ctx, 601, cid, corp_id, lowsec_sys, jita_station, 10_000_000_000, 0);
    register_char(&ctx, 602, attacker_cid, 1_000_009, lowsec_sys, jita_station, 1_000_000_000, 0);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = None;
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "Test Upwell Corp".into(),
                ticker: "TUPC".into(),
                ceo_id: cid,
                ..Default::default()
            },
        );
        snap.corp_governance.entry(corp_id).or_default().created_unix = t0 - 10 * 86_400;
        snap.next_entity_id += 1;
        let astra_item = snap.next_entity_id;
        snap.items.insert(
            astra_item,
            InventoryItemRecord {
                item_id: astra_item,
                type_id: 35_832,
                owner_id: cid,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = make_sess(20, 601, cid, corp_id, lowsec_sys, None, 0, 1);
    sess.ship_id = Some(ship_id);

    // 1. Structure Profiles + Defense Settings
    let profile_id = call_rpc(
        &ctx,
        &mut sess,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Doctrine Profile".into()), PyRep::String("Auto-defense".into())],
    )
    .await
    .expect("CreateProfile")
    .as_i64()
    .expect("profile_id");
    let defense_kv = PyRep::Tuple(vec![PyRep::Bool(true), PyRep::Bool(true), PyRep::Bool(true), PyRep::Real(-1.0), PyRep::Bool(false)]);
    let def_rows = PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1003), PyRep::Integer(0), defense_kv.clone()])]);
    call_rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(profile_id), def_rows])
        .await
        .expect("SaveProfileSettings");

    // 2. Anchor Restrictions: Jita rejected; lowsec Rancer succeeds
    let mut jita_sess = make_sess(21, 601, cid, corp_id, 30_000_142, None, 0, 1);
    jita_sess.ship_id = Some(ship_id);
    assert!(
        call_rpc(
            &ctx,
            &mut jita_sess,
            "structureDeployment",
            "Anchor",
            &[PyRep::Integer(35_832), PyRep::String("Illegal Jita Astra".into())],
        )
        .await
        .is_err()
    );

    let clear_pos = glam::DVec3::new(5.0e11, 0.0, 5.0e11);
    let _ = ctx
        .systems
        .get_or_spawn(lowsec_sys)
        .spawn_ball_with_combat(
            eve_destiny::Ball {
                id: ship_id,
                type_id: 587,
                position: clear_pos,
                velocity: glam::DVec3::ZERO,
                radius: 40.0,
                mass: 1.0e6,
                mode: eve_destiny::MovementMode::Stop,
                speed_fraction: 0.0,
                max_velocity: 300.0,
                inertia: 1.0,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    call_rpc(
        &ctx,
        &mut sess,
        "structureDeployment",
        "Anchor",
        &[
            PyRep::Integer(35_832),
            PyRep::List(vec![
                PyRep::Real(clear_pos.x + 60_000.0),
                PyRep::Real(clear_pos.y),
                PyRep::Real(clear_pos.z),
            ]),
            PyRep::Integer(profile_id),
            PyRep::Tuple(vec![PyRep::Integer(18), PyRep::Integer(2)]),
            PyRep::String("Rancer Fort".into()),
        ],
    )
    .await
    .expect("Anchor in lowsec");
    let (structure_id, st_type_id) = ctx.db.with_snapshot(|s| {
        s.structures
            .values()
            .find(|st| st.owner_corp_id == corp_id)
            .map(|st| (st.structure_id, st.type_id))
            .expect("anchored structure")
    });
    assert_eq!(st_type_id, 35_832);
    call_rpc(&ctx, &mut sess, "structureProfiles", "SaveProfileSettings", &[PyRep::Integer(structure_id), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1003), PyRep::Integer(0), defense_kv])])])
        .await
        .expect("SaveProfileSettings on structure");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&structure_id) {
            st.state = "Online".into();
        }
        snap.corp_contacts.entry(corp_id).or_default().push(SocialContactRecord {
            contact_id: attacker_cid,
            contact_type: "character".into(),
            standing: -5.0,
            is_watched: false,
            is_blocked: false,
            label_ids: Vec::new(),
        });
    });

    // 3. Defense Settings evaluation
    let (should_target, friendly_target) = ctx.db.with_snapshot(|snap| {
        (
            eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, structure_id, attacker_cid),
            eve_server::services::structure_acl_defense_rules::evaluate_auto_defense_target(&ctx, snap, structure_id, cid),
        )
    });
    assert_eq!((should_target, friendly_target), (true, false));

    // 4. Structure Directory
    let corp_structs = call_rpc(&ctx, &mut sess, "structureDirectory", "GetMyCorporationStructures", &[])
        .await
        .expect("GetMyCorporationStructures");
    let corp_count = match &corp_structs { PyRep::Dict(xs) => xs.iter().filter(|(k, _)| k.as_i64() == Some(structure_id)).count(), _ => 0 };
    assert_eq!(corp_count, 1);
    let map_data = call_rpc(&ctx, &mut sess, "structureDirectory", "GetStructureMapData", &[PyRep::Integer(lowsec_sys as i64)])
        .await
        .expect("GetStructureMapData");
    let map_count = match &map_data { PyRep::List(xs) => xs.len(), _ => 0 };
    assert_eq!(map_count, 1);
}

#[tokio::test]
async fn test_cosmetics_skinr_designer_and_market_closure() {
    // parity: feat:cosmetics.skinr-designer, feat:cosmetics.skinr-market
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-designer",
        "feat:cosmetics.skinr-market",
    ]);

    let ctx = ServerContext::new();
    let t0 = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0)));

    let seller_cid = 2_100_000_301_i64;
    let buyer_cid = 2_100_000_302_i64;
    let jita_station = 60_003_760_i64;
    register_char(&ctx, 701, seller_cid, 1_000_009, 30_000_142, jita_station, 5_000_000_000, 500);
    register_char(&ctx, 702, buyer_cid, 1_000_009, 30_000_142, jita_station, 5_000_000_000, 500);

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(seller_cid).or_default();
        sk.insert(84_217, TrainedSkillRecord { type_id: 84_217, level: 5, skill_points: 256_000.0 });
        sk.insert(81_377, TrainedSkillRecord { type_id: 81_377, level: 5, skill_points: 256_000.0 });
        if let Some(b) = snap.characters.get_mut(&buyer_cid) {
            b.loyalty_points.insert(1_000_419, 50_000);
        }
    });

    let mut admin_s = make_sess(30, 701, seller_cid, 1_000_009, 30_000_142, Some(jita_station), eve_machonet::service_roles::ROLE_GM_DEFAULT, 0);
    let mut seller_s = make_sess(31, 701, seller_cid, 1_000_009, 30_000_142, Some(jita_station), 0, 0);

    // 1. SKINR Designer: Tier 1 PLEX cost = 2500 cents (25 PLEX, 0% skill discount)
    let pricing_rsp = quasar_rpc(
        &ctx,
        &mut seller_s,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(pricing_rsp.status_code, STATUS_OK);
    let reg = QuasarDescriptorRegistry::open_default().expect("open_default");
    let p_any = pricing_rsp.payload.as_ref().unwrap();
    let p_dyn = reg.decode_dynamic(p_any.normalized_type_name(), &p_any.value).unwrap();
    assert_eq!(p_dyn.get(2), Some(&DynamicValue::Uint32(0)), "no skill discount on SKINR PLEX cost");

    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coat_id = skinr_cat
        .components
        .values()
        .find(|c| !c.is_pattern() && !c.disallowed_slots.contains(&1) && skinr_cat.compute_points_and_tier(&[c.component_id]).1 == 1)
        .expect("Tier 1 coating comp")
        .component_id;
    let mut grant_comp = Vec::new();
    let mut comp_m = Vec::new();
    encode_varint_field(1, coat_id as u64, &mut comp_m);
    let mut comp_id_outer = Vec::new();
    encode_bytes_field(1, &comp_m, &mut comp_id_outer);
    encode_bytes_field(1, &comp_id_outer, &mut grant_comp);
    encode_varint_field(2, 5, &mut grant_comp);
    assert_eq!(quasar_rpc(&ctx, &mut admin_s, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_comp).await.status_code, STATUS_OK);

    let (mut init_req, mut layout_m, mut slot_m, mut slot_id_m, mut cfg_m, mut coat_cfg_m, mut ship_m) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, "Closure SKIN", &mut init_req);
    encode_string_field(2, "Closure Line", &mut init_req);
    encode_varint_field(1, 1, &mut slot_id_m);
    encode_bytes_field(1, &comp_m, &mut coat_cfg_m);
    encode_bytes_field(1, &coat_cfg_m, &mut cfg_m);
    encode_bytes_field(1, &slot_id_m, &mut slot_m);
    encode_bytes_field(2, &cfg_m, &mut slot_m);
    encode_bytes_field(1, &slot_m, &mut layout_m);
    encode_varint_field(1, 587, &mut ship_m);
    encode_bytes_field(2, &ship_m, &mut layout_m);
    encode_varint_field(3, 1, &mut layout_m);
    encode_bytes_field(3, &layout_m, &mut init_req);
    encode_varint_field(5, 1, &mut init_req);

    let init_rsp = quasar_rpc(&ctx, &mut seller_s, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", init_req).await;
    assert_eq!(init_rsp.status_code, STATUS_OK);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.get(&seller_cid).map(|c| c.plex_balance)), Some(475), "Tier 1 SKINR sequencing must deduct 25 PLEX (500 -> 475)");
    let init_any = init_rsp.payload.as_ref().unwrap();
    let init_dyn = reg.decode_dynamic(init_any.normalized_type_name(), &init_any.value).unwrap();
    let job_uuid_bytes = match init_dyn.get(1) {
        Some(DynamicValue::Message(jm)) => match jm.get(1) {
            Some(DynamicValue::Bytes(b)) => b.clone(),
            _ => panic!("missing job uuid"),
        },
        _ => panic!("missing job id"),
    };
    let (mut comp_early_req, mut jid_m) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &job_uuid_bytes, &mut jid_m);
    encode_bytes_field(1, &jid_m, &mut comp_early_req);
    assert_eq!(quasar_rpc(&ctx, &mut seller_s, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", comp_early_req).await.status_code, STATUS_OK);

    // 2. SKINR Market: Paragon Relations V reduces 20% vendor tax to 15%
    let tax_rsp = quasar_rpc(&ctx, &mut seller_s, "eve_public.cosmetic.market.api.GetTaxRateRequest", Vec::new()).await;
    assert_eq!(tax_rsp.status_code, STATUS_OK);
    let tax_any = tax_rsp.payload.as_ref().unwrap();
    let tax_dyn = reg.decode_dynamic(tax_any.normalized_type_name(), &tax_any.value).unwrap();
    let tax_pct = match tax_dyn.get(1) {
        Some(DynamicValue::Message(tr)) => match tr.get(1) {
            Some(DynamicValue::Sint64(s)) => *s,
            _ => 0,
        },
        _ => 0,
    };
    assert_eq!(tax_pct, 15);
    let _ = COMP_CAT_PATTERN;
}

#[tokio::test]
async fn test_serverops_daily_jobs_and_time_dilation_closure() {
    // parity: feat:serverops.daily-jobs, feat:serverops.time-dilation
    eve_testkit::attest(&[
        "feat:serverops.daily-jobs",
        "feat:serverops.time-dilation",
    ]);

    let ctx = ServerContext::new();
    let t0 = 1_800_000_000_u64;
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(t0)));

    let sys_id = 30_002_718_u32;
    let cid = 2_100_000_401_i64;
    register_char(&ctx, 801, cid, 1_000_009, sys_id, 60_003_760, 1_000_000_000, 0);

    let (ast_id, contract_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let aid = snap.next_entity_id;
        snap.asteroids.insert(
            aid,
            AsteroidRecord {
                asteroid_id: aid,
                solar_system_id: sys_id,
                belt_id: 1,
                type_id: 1230,
                quantity: 0,
                initial_quantity: 10_000,
                position: (1.0e9, 0.0, 0.0),
                radius: 500.0,
            },
        );
        snap.next_entity_id += 1;
        let kid = snap.next_entity_id;
        snap.contracts.insert(
            kid,
            ContractRecord {
                contract_id: kid, issuer_id: cid, acceptor_id: None,
                contract_type: "ItemExchange".into(), status: "Outstanding".into(),
                start_station_id: 60_003_760, end_station_id: 60_003_760,
                price: IskCents::from_isk(1_000_000), reward: IskCents::ZERO,
                collateral: IskCents::ZERO, item_ids: Vec::new(),
                expiration_unix: t0 - 100, issuer_corp_id: 1_000_009,
                date_issued_unix: t0 - 10 * 86_400, ..Default::default()
            },
        );
        snap.system_states.insert(
            sys_id,
            SystemDynamicState {
                solar_system_id: sys_id, dbs_multiplier: 1.0,
                ess_main_bank: IskCents::ZERO, ess_reserve_bank: IskCents::ZERO,
                ess_linked_character_id: None, ess_link_started_tick: None,
                ess_cycle_started_unix: 0, ess_main_bank_contributors: HashMap::new(),
                sov_alliance_id: None, military_index: 3, industrial_index: 2, strategic_index: 1,
                fw_contested_pct: 0.0, insurgency_corruption_stage: 0, insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0, wormhole_destination_system_id: None,
                signature_position: None, fw_complex_position: None,
                fw_capture_seconds_elapsed: 0, fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0, last_sov_claim_tick: 0,
            },
        );
        (aid, kid)
    });

    let report = eve_server::services::serverops_cluster::execute_daily_maintenance_jobs(&ctx);
    assert!(report.get("replenishedAsteroids").copied().unwrap_or(0) >= 1);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.asteroids.get(&ast_id).map(|a| a.quantity), Some(10_000));
        assert_eq!(snap.contracts.get(&contract_id).map(|c| c.status.as_str()), Some("Expired"));
    });

    let _sys_handle = ctx.systems.get_or_spawn(sys_id);
    let mut gm_sess = make_sess(40, 801, cid, 1_000_009, sys_id, None, eve_machonet::service_roles::ROLE_GM_DEFAULT, 0);
    let tidi_rep = call_rpc(&ctx, &mut gm_sess, "slash", "SlashCmd", &[PyRep::String("/tidi 0.25".into())]).await.expect("/tidi command");
    assert_eq!(tidi_rep.as_f64(), Some(0.25));
    assert!((eve_server::services::serverops_cluster::get_system_tidi_factor(&ctx, sys_id) - 0.25).abs() < 1e-6);
}
