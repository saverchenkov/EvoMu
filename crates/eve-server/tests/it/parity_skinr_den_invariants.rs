//! End-to-end integration tests for SKINR sequencing & Paragon Hub market,
//! Mercenary Den development/anarchy evolution & Skyhook workforce impact,
//! and atomic rollback / single-source-of-truth store invariants.

#![forbid(unsafe_code)]

use crate::common;
use crate::quasar_goal_tale_notices::{bytes, sub};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use crate::quasar_sov_equinox_hub::{
    field, fields, id_req, rpc as q_sov_rpc, structure, subscribe, varint,
};
use crate::sovereignty_equinox_gameplay::{
    drain_quasar, find_nullsec_planets, fit_subcap_blasters, lock_target_via_rpc, make_ball,
    open_sqlite_ctx, place_ship_ball, register_online_pilot, rpc_call, step_weapon_ticks,
    temp_sqlite_path, ALLIANCE, CORP, PILOT, PILOT_SHIP,
};
use eve_db::{
    corp_roles, AllianceRecord, InventoryFlag, InventoryItemRecord, IskCents, MiningLedgerRecord,
    TrainedSkillRecord, WarRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, WireMessage,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_market::PARAGON_CORP_ID;
use eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog;
use eve_server::services::quasar_sov_hub::compute_hub_resources;
use eve_server::services::quasar_sov_mercden::TYPE_MERCENARY_DEN_MANAGEMENT;
use eve_server::services::sov_gameplay::TYPE_MERCENARY_DEN;
use eve_server::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use glam::DVec3;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;

fn cleanup_sqlite_path(p: &PathBuf) {
    let _ = std::fs::remove_file(p);
    let _ = std::fs::remove_file(p.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(p.with_extension("sqlite-shm"));
}

async fn macho_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(session, service, None, method, args, &HashMap::new(), None)
        .await
}

fn decode_wm(payload: Option<&eve_proto::quasar::GoogleAny>) -> Option<WireMessage> {
    WireMessage::decode(&payload?.value).ok()
}

#[tokio::test]
async fn test_skinr_sequencing_studio_and_paragon_hub_listing() {
    // parity: feat:cosmetics.skinr-sequencing-studio
    // parity: feat:cosmetics.skinr-paragon-hub-listing
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-sequencing-studio",
        "feat:cosmetics.skinr-paragon-hub-listing",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let seller_cid = 90_000_831u32;
    let buyer_cid = 90_000_832u32;
    let ship_id = 890_011i64;
    let rifter_type = 587u32;

    ctx.db.with_mut_snapshot(|s| {
        let mut ch_s = s.characters.get(&90_000_001).cloned().expect("template char");
        ch_s.character_id = seller_cid as i64;
        ch_s.corporation_id = 98_000_001;
        ch_s.active_ship_id = ship_id;
        ch_s.balance = IskCents::from_isk(10_000_000);
        ch_s.plex_balance = 1_000;
        ch_s.station_id = Some(JITA_STATION_ID);
        s.characters.insert(seller_cid as i64, ch_s);

        let mut ch_b = s.characters.get(&90_000_001).cloned().expect("template char");
        ch_b.character_id = buyer_cid as i64;
        ch_b.corporation_id = 98_000_002;
        ch_b.balance = IskCents::from_isk(20_000_000);
        ch_b.plex_balance = 1_000;
        ch_b.station_id = Some(JITA_STATION_ID);
        ch_b.loyalty_points.insert(PARAGON_CORP_ID as i64, 100_000);
        s.characters.insert(buyer_cid as i64, ch_b);

        // Train SKINR sequencing skills + Paragon Hub skills on seller
        let sk_map = s.skills.entry(seller_cid as i64).or_default();
        for sid in [81044u32, 81377, 81046, 84217, 84218, 81368] {
            sk_map.insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }

        s.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: rifter_type,
                owner_id: seller_cid as i64,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut seller_sess = build_sess(65_016, seller_cid, 98_000_001);
    let mut buyer_sess = build_sess(65_017, buyer_cid, 98_000_002);
    buyer_sess.role = 0;

    // 1. Verify SKINR sequencing skills discount tier pricing & durations
    let pricing_rsp = quasar_rpc(&ctx, &mut seller_sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest", Vec::new()).await;
    assert_eq!(pricing_rsp.status_code, STATUS_OK);
    assert_eq!(varint(&pricing_rsp.payload.as_ref().unwrap().value, 2), Some(0));

    let dur_rsp = quasar_rpc(&ctx, &mut seller_sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest", Vec::new()).await;
    assert_eq!(dur_rsp.status_code, STATUS_OK);
    assert_eq!(varint(&dur_rsp.payload.as_ref().unwrap().value, 2), Some(15));

    // 2. Grant finite nanocoating component licenses and initiate sequencing job for 2 licenses
    let skinr_cat = SkinrStaticCatalog::load(&ctx.sde);
    let coat_id = skinr_cat.components.values().find(|c| !c.is_pattern() && !c.disallowed_slots.contains(&1)).expect("coating component").component_id;

    let mut grant_req = Vec::new();
    encode_bytes_field(1, &id_req(1, coat_id as u64), &mut grant_req);
    encode_varint_field(2, 5, &mut grant_req);
    let grant_rsp = quasar_rpc(&ctx, &mut seller_sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_req).await;
    assert_eq!(grant_rsp.status_code, STATUS_OK);
    seller_sess.role = 0;

    let mut slot_msg = Vec::new();
    let mut s_idx = Vec::new();
    encode_varint_field(1, 1, &mut s_idx);
    encode_bytes_field(1, &s_idx, &mut slot_msg);
    let mut coat_cfg = Vec::new();
    let mut c_idx = Vec::new();
    encode_varint_field(1, coat_id as u64, &mut c_idx);
    encode_bytes_field(1, &c_idx, &mut coat_cfg);
    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);

    let mut layout = Vec::new();
    encode_bytes_field(1, &slot_msg, &mut layout);
    let mut r_tid = Vec::new();
    encode_varint_field(1, rifter_type as u64, &mut r_tid);
    encode_bytes_field(2, &r_tid, &mut layout);
    encode_varint_field(3, 1, &mut layout);

    let mut init_req = Vec::new();
    encode_string_field(1, "Paragon Studio Rifter", &mut init_req);
    encode_string_field(2, "Studio Line", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 2, &mut init_req);

    let plex_before = ctx.db.get_character(seller_cid as i64).unwrap().plex_balance;
    let init_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
    )
    .await;
    assert_eq!(init_rsp.status_code, STATUS_OK);
    let init_wm = decode_wm(init_rsp.payload.as_ref()).expect("init wm");
    let skin_id_sub = sub(&sub(&init_wm, 2), 1);
    let skin_hex = String::from_utf8(bytes(&skin_id_sub, 1)).expect("skin hex");
    assert!(!skin_hex.is_empty());
    let plex_after = ctx.db.get_character(seller_cid as i64).unwrap().plex_balance;
    assert!(plex_after < plex_before, "sequencing job must consume PLEX");

    // Advance clock 24h so sequencing job completes and produces 2 tradeable SKIN licenses
    ctx.advance_secs(86_400);
    let owned_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(owned_rsp.status_code, STATUS_OK);

    // 3. Verify Paragon Hub tax rate reduction (20% - 10% from Paragon Relations V + Paragon Partnership V = 10%)
    let mut pub_target = Vec::new();
    encode_varint_field(1, 1, &mut pub_target);
    let mut tax_req = Vec::new();
    encode_bytes_field(1, &pub_target, &mut tax_req);
    let tax_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        tax_req,
    )
    .await;
    assert_eq!(tax_rsp.status_code, STATUS_OK);

    // 4. List 1 license for ISK and 1 license for PLEX on Paragon Hub, and have buyer purchase both
    let mut create_isk = Vec::new();
    let mut isk_msg = Vec::new();
    encode_varint_field(1, 1_000_000, &mut isk_msg);
    encode_bytes_field(1, &isk_msg, &mut create_isk);
    let mut skin_id_msg = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_id_msg);
    let mut lic_id = Vec::new();
    let mut seq_cid = Vec::new();
    encode_varint_field(1, seller_cid as u64, &mut seq_cid);
    encode_bytes_field(1, &seq_cid, &mut lic_id);
    encode_bytes_field(2, &skin_id_msg, &mut lic_id);
    encode_bytes_field(3, &lic_id, &mut create_isk);
    let mut dur_msg_req = Vec::new();
    encode_varint_field(1, 86_400, &mut dur_msg_req);
    encode_bytes_field(4, &dur_msg_req, &mut create_isk);
    encode_varint_field(5, 1, &mut create_isk);
    encode_bytes_field(6, &pub_target, &mut create_isk);

    let list_isk_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_isk,
    )
    .await;
    assert_eq!(list_isk_rsp.status_code, STATUS_OK, "{}", list_isk_rsp.status_message);

    let mut create_plex = Vec::new();
    let mut plex_msg = Vec::new();
    encode_varint_field(1, 10_000, &mut plex_msg);
    encode_bytes_field(2, &plex_msg, &mut create_plex);
    encode_bytes_field(3, &lic_id, &mut create_plex);
    encode_bytes_field(4, &dur_msg_req, &mut create_plex);
    encode_varint_field(5, 1, &mut create_plex);
    encode_bytes_field(6, &pub_target, &mut create_plex);

    let list_plex_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_plex,
    )
    .await;
    assert_eq!(list_plex_rsp.status_code, STATUS_OK, "{}", list_plex_rsp.status_message);

    let all_rsp = quasar_rpc(
        &ctx,
        &mut buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let entries = fields(&all_rsp.payload.as_ref().unwrap().value, 1);
    assert_eq!(entries.len(), 2);

    let seller_isk_before = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;
    let seller_plex_before = ctx.db.get_character(seller_cid as i64).unwrap().plex_balance;

    for em in entries {
        let uuid_bytes = field(&field(&em, 1).unwrap(), 1).unwrap();
        let mut buy_req = Vec::new();
        let mut id_m = Vec::new();
        encode_bytes_field(1, &uuid_bytes, &mut id_m);
        encode_bytes_field(1, &id_m, &mut buy_req);
        encode_varint_field(2, 1, &mut buy_req);
        encode_string_field(3, eve_server::services::quasar_cosmetics_market::BUYER_FEES_VERSION, &mut buy_req);
        let buy_rsp = quasar_rpc(
            &ctx,
            &mut buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_req,
        )
        .await;
        assert_eq!(buy_rsp.status_code, STATUS_OK, "{}", buy_rsp.status_message);
    }

    let seller_isk_after = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;
    let seller_plex_after = ctx.db.get_character(seller_cid as i64).unwrap().plex_balance;
    assert_eq!(
        seller_isk_after - seller_isk_before,
        IskCents::from_isk(450_000).0
    );
    assert_eq!(seller_plex_after - seller_plex_before, 45);
}

#[tokio::test]
async fn test_mercenary_den_development_anarchy_and_skyhook_workforce() {
    // parity: feat:sovereignty.mercenary-den-development-anarchy
    // parity: feat:sovereignty.mercenary-den-anarchy-evolution
    eve_testkit::attest(&[
        "feat:sovereignty.mercenary-den-development-anarchy",
        "feat:sovereignty.mercenary-den-anarchy-evolution",
    ]);

    let db_path = temp_sqlite_path("parity_mercden_evo");
    let ctx = open_sqlite_ctx(&db_path, true);
    let (null_sys, temp_planet, _, _, _) = find_nullsec_planets(&ctx);
    let (hub_id, skyhook_id) = (1_120_000_001i64, 1_120_000_002i64);
    let sh_pos = DVec3::new(200_000.0, 0.0, 0.0);

    let mut s_owner = register_online_pilot(
        &ctx,
        85_011,
        "denpilot",
        "secret",
        PILOT,
        CORP,
        Some(ALLIANCE),
        null_sys,
        PILOT_SHIP,
        19_722,
        sh_pos + DVec3::new(3_000.0, 0.0, 0.0),
        corp_roles::ROLE_DIRECTOR,
    )
    .await;
    let mut rx = subscribe(&ctx, &mut s_owner).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ALLIANCE);
        snap.structures
            .insert(hub_id, structure(hub_id, CORP, null_sys, TYPE_SOVEREIGNTY_HUB));
        snap.structures
            .insert(skyhook_id, structure(skyhook_id, CORP, null_sys, TYPE_ORBITAL_SKYHOOK));
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, temp_planet);
        snap.skills.entry(PILOT).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord {
                type_id: TYPE_MERCENARY_DEN_MANAGEMENT,
                level: 2,
                skill_points: 50_000.0,
            },
        );
        snap.items.insert(
            985_011,
            InventoryItemRecord {
                item_id: 985_011,
                type_id: TYPE_MERCENARY_DEN,
                owner_id: PILOT,
                location_id: PILOT_SHIP,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let _ = ctx.systems.get_or_spawn(null_sys).spawn_ball(make_ball(skyhook_id, TYPE_ORBITAL_SKYHOOK, sh_pos)).await;

    let base_wf = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    let den_id = rpc_call(&ctx, &mut s_owner, "structureDeployment", "Anchor", &[
        PyRep::Integer(TYPE_MERCENARY_DEN as i64),
        PyRep::Integer(skyhook_id),
        PyRep::Integer(temp_planet),
    ])
    .await
    .unwrap()
    .dict_get("structureID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let _ = drain_quasar(&mut rx);

    // Stage 1 (Dev 0): 65 IDK/hr, 0% workforce disruption
    ctx.advance_secs(3_600);
    let d_1h = q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let info_1h = field(&field(&d_1h, 4).unwrap(), 2).unwrap();
    assert_eq!(varint(&info_1h, 1), Some(65));
    assert_eq!(ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available), base_wf);

    // Advance to Day 8 (Dev 40, Anarchy 40 -> UI Anarchy level 2 / proto stage 3):
    // Den siphons 10% of host Skyhook's planetary workforce (150 of 1,500)
    ctx.advance_secs(8 * 86_400 - 3_600);
    let _ = q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest", id_req(1, den_id as u64)).await;
    let wf_day8 = ctx.db.with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available);
    assert_eq!(wf_day8 + 150, base_wf);

    // Fetch generated Mercenary Tactical Operations, start one, and complete it in space:
    // Raises Development (+10 -> 50) and lowers Anarchy (-10 -> 30, back to stage 2 = 0% disruption)
    let acts = q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest", id_req(1, den_id as u64))
        .await.payload.unwrap().value;
    let first_act = fields(&acts, 1).into_iter().next().unwrap();
    let mut start_req = Vec::new();
    encode_bytes_field(1, &field(&first_act, 1).unwrap(), &mut start_req);
    assert_eq!(
        q_sov_rpc(&ctx, &mut s_owner, "eve_public.sovereignty.mercenaryden.activity.api.StartRequest", start_req).await.status_code,
        STATUS_OK
    );
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let mto_ball = ctx
        .db
        .with_snapshot(|s| s.sov_runtime.mto_site_balls.keys().copied().next().unwrap());
    let mto_pos = ctx
        .systems
        .get_or_spawn(null_sys)
        .get_ball_snapshot(mto_ball)
        .await
        .unwrap()
        .unwrap()
        .1
        .position;
    fit_subcap_blasters(&ctx, PILOT, PILOT_SHIP, 985_200);
    place_ship_ball(
        &ctx,
        null_sys,
        PILOT,
        PILOT_SHIP,
        16_229,
        mto_pos + DVec3::new(1_500.0, 0.0, 0.0),
    )
    .await;
    lock_target_via_rpc(&ctx, &mut s_owner, null_sys, PILOT_SHIP, mto_ball).await;
    for round in 0..55 {
        for gun in (0..6).map(|g| 985_200i64 + g * 2) {
            ctx.set_invention_roll_override(Some(0.45));
            let _ = rpc_call(
                &ctx,
                &mut s_owner,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(gun), PyRep::Integer(mto_ball)],
            )
            .await;
            if ctx
                .db
                .with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball))
            {
                break;
            }
        }
        if ctx
            .db
            .with_snapshot(|s| !s.sov_runtime.mto_site_balls.contains_key(&mto_ball))
        {
            break;
        }
        if round % 10 == 9 {
            step_weapon_ticks(&ctx, null_sys).await;
        }
    }
    ctx.db.with_snapshot(|snap| {
        let den = snap.sov_runtime.mercenary_dens.get(&den_id).unwrap();
        assert_eq!(den.development_level, 50);
        assert_eq!(den.anarchy_level, 30);
    });
    assert_eq!(
        ctx.db
            .with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available),
        base_wf
    );

    // Advance to Day 16 (Anarchy 70 -> stage 4, -300 workforce), then destroy den and verify workforce restores
    ctx.advance_secs(8 * 86_400);
    let _ = q_sov_rpc(
        &ctx,
        &mut s_owner,
        "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
        id_req(1, den_id as u64),
    )
    .await;
    assert_eq!(
        ctx.db
            .with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available)
            + 300,
        base_wf
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.remove(&den_id);
    });
    assert_eq!(
        ctx.db
            .with_snapshot(|s| compute_hub_resources(s, &ctx.sde, hub_id).unwrap().workforce_available),
        base_wf
    );

    drop(ctx);
    cleanup_sqlite_path(&db_path);
}

#[tokio::test]
async fn test_atomic_rollback_all_stores_and_single_source_of_truth() {
    // parity: feat:invariant.atomic-rollback-all-stores
    // parity: feat:invariant.single-source-of-truth
    eve_testkit::attest(&[
        "feat:invariant.atomic-rollback-all-stores",
        "feat:invariant.single-source-of-truth",
    ]);

    let db_path = temp_sqlite_path("parity_invariants");

    {
        let repo = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
        let ctx = Arc::new(ServerContext::with_repository(repo));
        let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        ctx.db.with_mut_snapshot(|snap| *snap = demo);
        common::pin_clock(&ctx);

        let mut sess = SessionState::new(7210);
        sess.user_id = Some(1);
        macho_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
            .await
            .expect("SelectCharacterID");

        // 1. Atomic rollback across all stores on RPC error & randomized fault injection
        let bal_before = ctx.db.get_character(90_000_001).unwrap().balance;
        let plex_before = ctx.db.get_character(90_000_001).unwrap().plex_balance;
        let items_before = ctx.db.with_snapshot(|snap| snap.items.len());
        let ledger_before = ctx.db.with_snapshot(|snap| snap.mining_ledger.len());
        let contracts_before = ctx.db.with_snapshot(|snap| snap.contracts.len());

        let mut seed = 0xCAFE_BABE_1234_5678u64;
        for step in 0..12u64 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r: Result<(), String> = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                    ch.balance = IskCents((seed % 999_999) as i64);
                    ch.plex_balance = (seed % 500) as i64;
                }
                snap.items.insert(
                    999_777_000 + step as i64,
                    InventoryItemRecord {
                        item_id: 999_777_000 + step as i64,
                        type_id: 34,
                        owner_id: 90_000_001,
                        location_id: JITA_STATION_ID,
                        flag: InventoryFlag::Hangar,
                        quantity: (seed % 250 + 1) as i32,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
                snap.mining_ledger.push(MiningLedgerRecord {
                    character_id: 90_000_001,
                    solar_system_id: JITA_SYSTEM_ID,
                    type_id: 1230,
                    quantity: 500,
                    date: "2026-11-03".into(),
                    timestamp_unix: 1_800_000_000,
                });
                Err(format!("injected fault step {step}"))
            });
            assert!(r.is_err());
        }

        let rpc_fail = macho_rpc(
            &ctx,
            &mut sess,
            "contractProxy",
            "CreateContract",
            &[
                PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID), PyRep::Integer(JITA_STATION_ID),
                PyRep::Real(1_000.0), PyRep::Real(999_999_999_999.0), PyRep::Real(0.0),
                PyRep::String("Rollback Contract".into()), PyRep::String(String::new()),
                PyRep::List(vec![PyRep::Integer(100_000_003)]),
            ],
        )
        .await;
        assert!(rpc_fail.is_err());

        assert_eq!(ctx.db.get_character(90_000_001).unwrap().balance, bal_before);
        assert_eq!(ctx.db.get_character(90_000_001).unwrap().plex_balance, plex_before);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.items.len()), items_before);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.mining_ledger.len()), ledger_before);
        assert_eq!(ctx.db.with_snapshot(|snap| snap.contracts.len()), contracts_before);

        // 2. Single source of truth across Client, ESI, Quasar, and SQLite restart
        macho_rpc(&ctx, &mut sess, "charMgr", "AddContact", &[PyRep::Integer(90_000_002), PyRep::Real(10.0), PyRep::Bool(true)])
            .await
            .expect("AddContact");
        macho_rpc(&ctx, &mut sess, "invbroker", "SetLabel", &[PyRep::Integer(100_000_001), PyRep::String("SSOT Rifter".into())])
            .await
            .expect("SetLabel");
        macho_rpc(&ctx, &mut sess, "charMgr", "LogSettings", &[PyRep::Real(4_200.0)])
            .await
            .expect("LogSettings");

        ctx.db.with_mut_snapshot(|snap| {
            snap.alliances.insert(
                99_000_888,
                AllianceRecord {
                    alliance_id: 99_000_888,
                    name: "SSOT Parity Alliance".into(),
                    short_name: "SSOTP".into(),
                    executor_corp_id: 98_000_001,
                    member_corp_ids: [98_000_001].into_iter().collect(),
                },
            );
            snap.wars.insert(
                777_001,
                WarRecord {
                    war_id: 777_001,
                    aggressor_corp_id: 98_000_001,
                    defender_corp_id: 98_000_002,
                    started_unix: 1_800_086_400,
                    mutual: false,
                    active: true,
                },
            );
            if let Some(ch) = snap.characters.get(&90_000_001) {
                let uid = ch.user_id;
                snap.account_plex_vault.insert(uid, 777);
            }
        });

        let esi = EsiRouter::new(Arc::clone(&ctx));
        assert!(String::from_utf8_lossy(&esi.handle_request(&EsiHttpRequest::new("GET", "/v1/alliances/")).body).contains("99000888"));
        assert!(String::from_utf8_lossy(&esi.handle_request(&EsiHttpRequest::new("GET", "/v1/wars/")).body).contains("777001"));
    }

    // Reopen SQLite and verify all state matches across Client, ESI, and Snapshot
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = Arc::new(ServerContext::with_repository(repo2));
    let mut sess2 = SessionState::new(7211);
    sess2.user_id = Some(1);
    macho_rpc(&ctx2, &mut sess2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID after restart");

    let names_rep = macho_rpc(&ctx2, &mut sess2, "dogmaIM", "ItemGetInfo", &[PyRep::Integer(100_000_001)])
        .await
        .expect("ItemGetInfo");
    assert!(format!("{names_rep:?}").contains("SSOT Rifter"));
    let contacts_rep = macho_rpc(&ctx2, &mut sess2, "charMgr", "GetContactList", &[])
        .await
        .expect("GetContactList");
    assert!(format!("{contacts_rep:?}").contains("90000002"));
    let uid = ctx2.db.get_character(90_000_001).unwrap().user_id;
    assert_eq!(ctx2.db.with_snapshot(|s| s.account_plex_vault.get(&uid).copied()), Some(777));

    let esi2 = EsiRouter::new(Arc::clone(&ctx2));
    assert!(String::from_utf8_lossy(&esi2.handle_request(&EsiHttpRequest::new("GET", "/v1/alliances/")).body).contains("99000888"));
    assert!(String::from_utf8_lossy(&esi2.handle_request(&EsiHttpRequest::new("GET", "/v1/wars/")).body).contains("777001"));

    cleanup_sqlite_path(&db_path);
}
