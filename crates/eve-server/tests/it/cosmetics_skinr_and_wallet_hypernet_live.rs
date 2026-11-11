//! Part 2 of end-to-end live integration tests: SKINR Designer & Sequencing Studio,
//! SKINR Paragon Hub Market (ISK & PLEX listings + skills), HyperNet Relay (all node sizes &
//! 72h expiry refund), EverMarks (gameplay accrual & Paragon emblem redemption), and New Eden Store.

#![forbid(unsafe_code)]

use crate::common;
use crate::corp_member_audit_reinforce_and_quit::found_corp_with_member;
use crate::fleet_boss_formations_corp_projects_live::quasar_call;
use crate::industry_live_support::{
    fund_pilots, int, login, owned_qty, rpc, train_skills, JITA_STATION, PILOT_1, PILOT_2,
};
use crate::industry_supercapital_tax_esi_e2e::dock_station_fast as dock_station;
use crate::quasar_progression_complete::build_sess;
use crate::quasar_sov_equinox_hub::{field, fields, varint};
use crate::wallet_hypernet_evermarks_store_live::seed_nes_store_via_quasar_admin;
use eve_db::IskCents;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_OK};
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_market::BUYER_FEES_VERSION;
use eve_server::services::quasar_cosmetics_skinr::{
    SkinrStaticCatalog, COMP_CAT_MATERIAL, COMP_CAT_METALLIC, COMP_CAT_PATTERN,
};
use std::sync::{Arc, OnceLock};

fn fast_demo_ctx() -> ServerContext {
    static BASE: OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> = OnceLock::new();
    let (sde, snap) = BASE.get_or_init(|| {
        let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
        let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
        let _ = eve_server::world_seeder::seed_dual_anchor_markets(&db, &sde);
        (sde, db.with_snapshot(Clone::clone))
    });
    let repo = Arc::new(eve_db::WorldRepository::new_empty());
    repo.with_mut_snapshot(|s| *s = snap.clone());
    ServerContext::with_sde_and_repository(Arc::clone(sde), repo)
        .with_seeded_rng(eve_server::rng::TEST_MODE_DEFAULT_SEED)
}

const RIFTER_TYPE_ID: u32 = 587;
const PLEX_TYPE_ID: u32 = 44_992;
const HYPERCORE_TYPE_ID: u32 = 52_568;
const PARAGON_CORP_ID: i64 = 1_000_419;
const PARAGON_STATION: i64 = 60_015_169;

fn sub_bytes(raw: &[u8], n: u32) -> Vec<u8> {
    field(raw, n).unwrap_or_default()
}

fn str_field(raw: &[u8], n: u32) -> String {
    String::from_utf8(sub_bytes(raw, n)).unwrap_or_default()
}

fn encode_slot(slot_id: u32, comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut slot_msg = Vec::new();
    let mut s_idx = Vec::new();
    encode_varint_field(1, u64::from(slot_id), &mut s_idx);
    encode_bytes_field(1, &s_idx, &mut slot_msg);

    let mut cfg = Vec::new();
    let mut c_idx = Vec::new();
    encode_varint_field(1, u64::from(comp_id), &mut c_idx);
    encode_bytes_field(1, &c_idx, &mut cfg);

    let mut outer = Vec::new();
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &cfg, &mut outer);
    encode_bytes_field(2, &outer, &mut slot_msg);
    slot_msg
}

fn encode_comp_id(comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut cid = Vec::new();
    encode_varint_field(1, u64::from(comp_id), &mut cid);
    let mut out = Vec::new();
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &cid, &mut out);
    out
}

fn encode_skinr_layout(ship_tid: u32, coat_id: u32, pat_id: Option<u32>, sec_id: u32) -> Vec<u8> {
    let mut layout = Vec::new();
    encode_bytes_field(1, &encode_slot(1, coat_id, false), &mut layout);
    encode_bytes_field(1, &encode_slot(2, sec_id, false), &mut layout);
    if let Some(pid) = pat_id {
        encode_bytes_field(1, &encode_slot(5, pid, true), &mut layout);
    }
    let mut hull = Vec::new();
    encode_varint_field(1, u64::from(ship_tid), &mut hull);
    encode_bytes_field(2, &hull, &mut layout);
    encode_varint_field(3, 1, &mut layout);
    layout
}

async fn deposit_plex(ctx: &ServerContext, sess: &mut SessionState, station_id: i64, qty: i64) {
    let prev_role = sess.role;
    sess.role = service_roles::ROLE_GM_DEFAULT;
    let kv = rpc(
        ctx,
        sess,
        "slash",
        "SlashCmd",
        &[PyRep::String(format!("/giveitem {PLEX_TYPE_ID} {qty}"))],
    )
    .await
    .expect("give PLEX");
    let item_id = int(&kv, "itemID");
    rpc(
        ctx,
        sess,
        "invbroker",
        "DepositPlexToVault",
        &[PyRep::Integer(station_id), PyRep::Integer(item_id), PyRep::Integer(qty)],
    )
    .await
    .expect("DepositPlexToVault");
    sess.role = prev_role;
}

#[tokio::test]
async fn test_live_skinr_designer_and_sequencing_studio() {
    // parity: feat:cosmetics.skinr-designer, feat:cosmetics.skinr-sequencing-studio
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-designer",
        "feat:cosmetics.skinr-sequencing-studio",
    ]);

    let ctx = fast_demo_ctx();
    common::pin_clock(&ctx);
    let designer_id = 90_000_921_i64;
    ctx.db.provision_gm_account("gm_skinr1", "gm", designer_id, "SKINR Designer");
    let mut s1 = build_sess(9201, designer_id as u32, 1_000_009);
    s1.solar_system_id = Some(30_000_142);
    s1.station_id = Some(JITA_STATION);
    s1.role = service_roles::ROLE_GM_DEFAULT;

    train_skills(&ctx, &mut s1, &[81_377, 81_372, 81_040, 81_046, 81_366, 81_375], true).await;

    let cat = SkinrStaticCatalog::load(&ctx.sde);
    let coat = cat.components.values().find(|c| c.category == COMP_CAT_MATERIAL && c.disallowed_slots.is_empty()).unwrap().component_id;
    let met = cat.components.values().find(|c| c.category == COMP_CAT_METALLIC && c.disallowed_slots.is_empty()).unwrap().component_id;
    let pat = cat.components.values().find(|c| c.category == COMP_CAT_PATTERN && c.disallowed_slots.is_empty()).unwrap().component_id;

    // 1. Grant finite component licenses via Quasar
    for (cid, is_pat) in [(coat, false), (met, false), (pat, true)] {
        let mut req = Vec::new();
        encode_bytes_field(1, &encode_comp_id(cid, is_pat), &mut req);
        encode_varint_field(2, 10, &mut req);
        let r = quasar_call(
            &ctx,
            &s1,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            req,
        );
        assert_eq!(r.status_code, STATUS_OK);
    }
    s1.role = 0;

    // 2. Designer negative check: mismatched coating component encoded in pattern slot fails
    let mut bad_layout = Vec::new();
    encode_bytes_field(1, &encode_slot(1, coat, true), &mut bad_layout);
    let mut bad_req = Vec::new();
    encode_string_field(1, "Bad Layout", &mut bad_req);
    encode_string_field(2, "Bad Line", &mut bad_req);
    encode_bytes_field(3, &bad_layout, &mut bad_req);
    encode_varint_field(5, 1, &mut bad_req);
    let bad_rsp = quasar_call(
        &ctx,
        &s1,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        bad_req,
    );
    assert_eq!(bad_rsp.status_code, STATUS_BAD_REQUEST);

    // 3. Initiate sequencing job #1 (natural timer completion via virtual clock advance)
    let plex_0 = ctx.db.get_character(designer_id).unwrap().plex_balance;
    let mut req1 = Vec::new();
    encode_string_field(1, "Live Tempest Rifter", &mut req1);
    encode_string_field(2, "Tempest Line", &mut req1);
    encode_bytes_field(3, &encode_skinr_layout(RIFTER_TYPE_ID, coat, Some(pat), met), &mut req1);
    encode_varint_field(5, 2, &mut req1);
    let rsp1 = quasar_call(
        &ctx,
        &s1,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        req1,
    );
    assert_eq!(rsp1.status_code, STATUS_OK);
    let raw1 = rsp1.payload.unwrap().value;
    let hex1 = str_field(&sub_bytes(&sub_bytes(&raw1, 2), 1), 1);
    assert!(ctx.db.get_character(designer_id).unwrap().plex_balance < plex_0);

    // Advance virtual clock beyond sequencing duration -> settles naturally into owned SKIN licenses
    ctx.advance_virtual_clock(30 * 86_400);
    let owned_rsp = quasar_call(
        &ctx,
        &s1,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
    );
    assert_eq!(owned_rsp.status_code, STATUS_OK);
    let entries = fields(&owned_rsp.payload.unwrap().value, 1);
    assert!(!entries.is_empty(), "Settled sequencing job must grant unactivated SKIN licenses");

    // Verify 3rd-party SKIN design metadata query
    let mut get_skin = Vec::new();
    encode_string_field(1, &hex1, &mut get_skin);
    let skin_rsp = quasar_call(
        &ctx,
        &s1,
        "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest",
        get_skin,
    );
    assert_eq!(skin_rsp.status_code, STATUS_OK);

    // 4. Initiate sequencing job #2 and complete early via CompleteEarlyRequest
    let mut req2 = Vec::new();
    encode_string_field(1, "Rush Aurora Rifter", &mut req2);
    encode_string_field(2, "Aurora Line", &mut req2);
    encode_bytes_field(3, &encode_skinr_layout(RIFTER_TYPE_ID, coat, None, met), &mut req2);
    encode_varint_field(5, 1, &mut req2);
    let rsp2 = quasar_call(
        &ctx,
        &s1,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        req2,
    );
    assert_eq!(rsp2.status_code, STATUS_OK);
    let early_rsp = quasar_call(
        &ctx,
        &s1,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
        Vec::new(),
    );
    assert_eq!(early_rsp.status_code, STATUS_OK);
}

#[tokio::test]
async fn test_live_skinr_paragon_hub_isk_plex_listings_and_market() {
    // parity: feat:cosmetics.skinr-market, feat:cosmetics.skinr-paragon-hub-listing
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-market",
        "feat:cosmetics.skinr-paragon-hub-listing",
    ]);

    let ctx = fast_demo_ctx();
    common::pin_clock(&ctx);
    let seller_id = 90_000_922_i64;
    let buyer_id = 90_000_923_i64;
    ctx.db.provision_gm_account("gm_skinr_s", "gm", seller_id, "SKINR Seller");
    ctx.db.provision_gm_account("gm_skinr_b", "gm", buyer_id, "SKINR Buyer");
    fund_pilots(&ctx, &[(seller_id, 5_000_000_000.0), (buyer_id, 5_000_000_000.0)]).await;
    let seller_uid = ctx.db.get_character(seller_id).unwrap().user_id;
    let buyer_uid = ctx.db.get_character(buyer_id).unwrap().user_id;
    let mut seller = build_sess(seller_uid, seller_id as u32, 1_000_009);
    seller.solar_system_id = Some(30_000_142);
    seller.station_id = Some(JITA_STATION);
    seller.role = service_roles::ROLE_GM_DEFAULT;

    let mut buyer = build_sess(buyer_uid, buyer_id as u32, 1_000_010);
    buyer.solar_system_id = Some(30_000_142);
    buyer.station_id = Some(JITA_STATION);
    buyer.role = service_roles::ROLE_GM_DEFAULT;

    // Earn >= 20,000 EverMarks for buyer via AIR Daily Goal completion + redemption (covers Paragon buyer fees)
    let mut day_idx = 0_u32;
    while ctx.db.with_snapshot(|s| s.characters[&buyer_id].loyalty_points.get(&PARAGON_CORP_ID).copied().unwrap_or(0)) < 20_000 {
        if day_idx > 0 {
            ctx.advance_virtual_clock(86_405);
        }
        day_idx += 1;
        let goals = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(buyer_id as u32);
        let active_regs: Vec<_> = goals.into_iter().filter(|g| g.template.category != 6).take(2).collect();
        for g in &active_regs {
            if g.template.category == 2 {
                eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, buyer_id, g.template.target, 30_000_142, 1230);
            } else {
                ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(buyer_id as u32, g.template.category, g.template.target, &ctx.quasar_gateway);
            }
            let mut u_m = Vec::new();
            encode_bytes_field(1, &g.uuid, &mut u_m);
            let mut claim_req = Vec::new();
            encode_bytes_field(1, &u_m, &mut claim_req);
            assert_eq!(quasar_call(&ctx, &buyer, "eve_public.dailygoal.api.RedeemRequest", claim_req).status_code, STATUS_OK);
        }
    }

    // 1. Train Paragon Hub broker skills (84217, 84218, 84220) and check GetTaxRateRequest (5%)
    train_skills(&ctx, &mut seller, &[84_217, 84_218, 84_220, 81_368, 81_369, 81_375], true).await;
    let tax_rsp = quasar_call(&ctx, &seller, "eve_public.cosmetic.market.api.GetTaxRateRequest", Vec::new());
    assert_eq!(tax_rsp.status_code, STATUS_OK);
    let tax_sub = sub_bytes(&tax_rsp.payload.unwrap().value, 1);
    let tax_raw = varint(&tax_sub, 1).unwrap_or(0) as i64;
    let tax_pct = (tax_raw >> 1) ^ -(tax_raw & 1);
    assert_eq!(tax_pct, 5, "Paragon skills must reduce broker tax from 10% to 5%, got {tax_pct}%");

    let cat = SkinrStaticCatalog::load(&ctx.sde);
    let coat = cat.components.values().find(|c| c.category == COMP_CAT_MATERIAL && c.disallowed_slots.is_empty()).unwrap().component_id;
    let met = cat.components.values().find(|c| c.category == COMP_CAT_METALLIC && c.disallowed_slots.is_empty()).unwrap().component_id;
    for cid in [coat, met] {
        let mut req = Vec::new();
        encode_bytes_field(1, &encode_comp_id(cid, false), &mut req);
        encode_varint_field(2, 12, &mut req);
        quasar_call(
            &ctx,
            &seller,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            req,
        );
    }
    seller.role = 0;
    buyer.role = 0;

    let mut seq_req = Vec::new();
    encode_string_field(1, "Paragon Market Skin", &mut seq_req);
    encode_string_field(2, "Market Line", &mut seq_req);
    encode_bytes_field(3, &encode_skinr_layout(RIFTER_TYPE_ID, coat, None, met), &mut seq_req);
    encode_varint_field(5, 4, &mut seq_req);
    let seq_rsp = quasar_call(
        &ctx,
        &seller,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        seq_req,
    );
    assert_eq!(seq_rsp.status_code, STATUS_OK);
    let raw_seq = seq_rsp.payload.unwrap().value;
    let skin_hex = str_field(&sub_bytes(&sub_bytes(&raw_seq, 2), 1), 1);
    let early_done = quasar_call(
        &ctx,
        &seller,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
        Vec::new(),
    );
    assert_eq!(early_done.status_code, STATUS_OK);

    let mut lic_id = Vec::new();
    let mut seq_cid = Vec::new();
    encode_varint_field(1, seller_id as u64, &mut seq_cid);
    let mut shex_m = Vec::new();
    encode_string_field(1, &skin_hex, &mut shex_m);
    encode_bytes_field(1, &seq_cid, &mut lic_id);
    encode_bytes_field(2, &shex_m, &mut lic_id);
    let mut dur_m = Vec::new();
    encode_varint_field(1, 86_400, &mut dur_m);

    // 2. Create Listing A (ISK = 1,000,000) and Listing B (ISK = 2,000,000)
    let mk_listing = |field_num: u32, price_units: u64| {
        let mut p_m = Vec::new();
        encode_varint_field(1, price_units, &mut p_m);
        let mut r = Vec::new();
        encode_bytes_field(field_num, &p_m, &mut r);
        encode_bytes_field(3, &lic_id, &mut r);
        encode_bytes_field(4, &dur_m, &mut r);
        encode_varint_field(5, 1, &mut r);
        r
    };
    let r_a = quasar_call(&ctx, &seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", mk_listing(1, 2_000_000));
    let r_b = quasar_call(&ctx, &seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", mk_listing(1, 4_000_000));
    assert_eq!(r_a.status_code, STATUS_OK);
    assert_eq!(r_b.status_code, STATUS_OK);

    // Cancel Listing B and re-list it in PLEX (50 PLEX = 5,000 plex-cents -> zigzag 10,000)
    let owned_rsp = quasar_call(&ctx, &seller, "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest", Vec::new());
    let owned = fields(&owned_rsp.payload.unwrap().value, 1);
    assert_eq!(owned.len(), 2);
    let uuid_b = field(&field(&owned[1], 1).unwrap(), 1).unwrap();
    let mut cancel_req = Vec::new();
    let mut cancel_id = Vec::new();
    encode_bytes_field(1, &uuid_b, &mut cancel_id);
    encode_bytes_field(1, &cancel_id, &mut cancel_req);
    assert_eq!(
        quasar_call(&ctx, &seller, "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest", cancel_req).status_code,
        STATUS_OK
    );

    let r_plex = quasar_call(&ctx, &seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", mk_listing(2, 10_000));
    assert_eq!(r_plex.status_code, STATUS_OK);

    // 3. Buyer purchases both ISK listing and PLEX listing
    let all_rsp = quasar_call(&ctx, &buyer, "eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new());
    let market_entries = fields(&all_rsp.payload.unwrap().value, 1);
    assert_eq!(market_entries.len(), 2);

    let buyer_isk_0 = ctx.db.get_character(buyer_id).unwrap().balance.0;
    let buyer_plex_0 = ctx.db.with_snapshot(|s| s.unified_plex_balance(buyer_id));
    for entry in &market_entries {
        let luuid = field(&field(entry, 1).unwrap(), 1).unwrap();
        let mut buy_req = Vec::new();
        let mut id_m = Vec::new();
        encode_bytes_field(1, &luuid, &mut id_m);
        encode_bytes_field(1, &id_m, &mut buy_req);
        encode_varint_field(2, 1, &mut buy_req);
        encode_string_field(3, BUYER_FEES_VERSION, &mut buy_req);
        assert_eq!(
            quasar_call(&ctx, &buyer, "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", buy_req).status_code,
            STATUS_OK
        );
    }
    assert_eq!(buyer_isk_0 - ctx.db.get_character(buyer_id).unwrap().balance.0, IskCents::from_isk(1_000_000).0);
    assert_eq!(buyer_plex_0 - ctx.db.with_snapshot(|s| s.unified_plex_balance(buyer_id)), 50);

    // Verify buyer owns both purchased SKIN licenses
    let mut buyer_lic_id = Vec::new();
    let mut buyer_cid_m = Vec::new();
    encode_varint_field(1, buyer_id as u64, &mut buyer_cid_m);
    encode_bytes_field(1, &buyer_cid_m, &mut buyer_lic_id);
    encode_bytes_field(2, &shex_m, &mut buyer_lic_id);
    let mut check_lic = Vec::new();
    encode_bytes_field(1, &buyer_lic_id, &mut check_lic);
    let buyer_lic_rsp = quasar_call(&ctx, &buyer, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", check_lic);
    assert_eq!(buyer_lic_rsp.status_code, STATUS_OK);
    assert_eq!(varint(&sub_bytes(&buyer_lic_rsp.payload.unwrap().value, 1), 2), Some(2));
}

#[tokio::test]
async fn test_live_hypernet_relay_node_sizing_and_72h_expiry_refund() {
    // parity: feat:wallet.hypernet-relay, feat:wallet.hypernet-hypercore-sizing-expiry
    eve_testkit::attest(&[
        "feat:wallet.hypernet-relay",
        "feat:wallet.hypernet-hypercore-sizing-expiry",
    ]);

    let ctx = fast_demo_ctx();
    common::pin_clock(&ctx);
    let mut s1 = login(&ctx, 9205, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 9206, "other", PILOT_2).await;

    s1.role = service_roles::ROLE_GM_DEFAULT;
    s2.role = service_roles::ROLE_GM_DEFAULT;
    rpc(&ctx, &mut s1, "slash", "SlashCmd", &[PyRep::String("/giveisk 500000000".into())]).await.unwrap();
    rpc(&ctx, &mut s2, "slash", "SlashCmd", &[PyRep::String("/giveisk 500000000".into())]).await.unwrap();
    rpc(&ctx, &mut s1, "slash", "SlashCmd", &[PyRep::String(format!("/giveitem {HYPERCORE_TYPE_ID} 2000"))]).await.unwrap();

    // 1. Negative check: invalid node count (10)
    let r_bad = rpc(&ctx, &mut s1, "slash", "SlashCmd", &[PyRep::String(format!("/giveitem {RIFTER_TYPE_ID} 1"))])
        .await
        .unwrap();
    s1.role = 0;
    s2.role = 0;
    let bad_nodes = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(int(&r_bad, "itemID"))),
            (PyRep::String("ticketCount".into()), PyRep::Integer(10)),
            (PyRep::String("ticketPrice".into()), PyRep::Real(100_000.0)),
        ])],
    )
    .await
    .expect_err("10-node raffle must fail");
    assert_eq!(bad_nodes.code, "InvalidRaffleTicketCount");

    // 2. Verify all 4 valid node counts (8, 16, 48, 512) consume HyperCores proportionally
    let mut core_deltas = Vec::new();
    let mut expiry_raffle_id = 0_i64;
    let mut expiry_ship_id = 0_i64;
    for &nodes in &[8_i64, 16, 48, 512] {
        s1.role = service_roles::ROLE_GM_DEFAULT;
        let s_kv = rpc(&ctx, &mut s1, "slash", "SlashCmd", &[PyRep::String(format!("/giveitem {RIFTER_TYPE_ID} 1"))])
            .await
            .unwrap();
        s1.role = 0;
        let sid = int(&s_kv, "itemID");
        let cores_pre = owned_qty(&ctx, PILOT_1, JITA_STATION, HYPERCORE_TYPE_ID);
        let created = rpc(
            &ctx,
            &mut s1,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Dict(vec![
                (PyRep::String("itemID".into()), PyRep::Integer(sid)),
                (PyRep::String("ticketCount".into()), PyRep::Integer(nodes)),
                (PyRep::String("ticketPrice".into()), PyRep::Real(50_000.0)),
                (PyRep::String("duration".into()), PyRep::Integer(259_200)),
            ])],
        )
        .await
        .expect("CreateRaffle valid node tier");
        let cores_post = owned_qty(&ctx, PILOT_1, JITA_STATION, HYPERCORE_TYPE_ID);
        core_deltas.push(cores_pre - cores_post);
        if nodes == 16 {
            expiry_raffle_id = int(&created, "raffleID");
            expiry_ship_id = sid;
        } else if nodes == 8 {
            let rid_8 = int(&created, "raffleID");
            let rifters_before = owned_qty(&ctx, PILOT_2, JITA_STATION, RIFTER_TYPE_ID);
            let tickets = rpc(
                &ctx,
                &mut s2,
                "raffleProxy",
                "BuyRandomTickets",
                &[PyRep::Integer(rid_8), PyRep::Integer(8)],
            )
            .await
            .expect("BuyRandomTickets 8/8");
            assert_eq!(int(&tickets, "winner_id"), PILOT_2);
            rpc(&ctx, &mut s2, "raffleProxy", "AwardItem", &[PyRep::Integer(rid_8)])
                .await
                .expect("AwardItem delivers escrowed Rifter to winner");
            assert_eq!(
                owned_qty(&ctx, PILOT_2, JITA_STATION, RIFTER_TYPE_ID) - rifters_before,
                1,
                "Winner receives escrowed Rifter"
            );
        }
    }
    assert!(core_deltas.iter().all(|&d| d > 0) && core_deltas[3] >= core_deltas[0], "HyperCore consumption scales: {core_deltas:?}");

    // 3. 72-hour expiry refund: buyer buys 4 tickets on 16-node raffle, clock advances > 72h -> buyer refunded ISK, creator gets item back
    let buyer_isk_pre = rpc(&ctx, &mut s2, "account", "GetCashBalance", &[]).await.unwrap().as_f64().unwrap();
    rpc(&ctx, &mut s2, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(expiry_raffle_id), PyRep::Integer(4)])
        .await
        .expect("Buy 4 tickets before expiry");
    ctx.advance_virtual_clock(259_205);
    let _ = rpc(&ctx, &mut s1, "raffleProxy", "GetActiveHistoricPrices", &[PyRep::Integer(RIFTER_TYPE_ID as i64)]).await;
    let _ = rpc(&ctx, &mut s1, "raffleProxy", "GetRaffle", &[PyRep::Integer(expiry_raffle_id)]).await;
    let buyer_isk_post = rpc(&ctx, &mut s2, "account", "GetCashBalance", &[]).await.unwrap().as_f64().unwrap();
    assert!((buyer_isk_post - buyer_isk_pre).abs() < 1.0, "Expired raffle must refund 100% of ticket ISK to buyer");
    let returned_loc = ctx.db.with_snapshot(|s| s.items.get(&expiry_ship_id).map(|i| (i.owner_id, i.location_id)));
    assert_eq!(returned_loc, Some((PILOT_1, JITA_STATION)), "Expired raffle must return escrowed item to creator");
}

#[tokio::test]
async fn test_live_wallet_evermarks_gameplay_accrual_and_nes_store() {
    // parity: feat:wallet.evermarks, feat:wallet.nes-store
    eve_testkit::attest(&["feat:wallet.evermarks", "feat:wallet.nes-store"]);

    let ctx = fast_demo_ctx();
    common::pin_clock(&ctx);
    fund_pilots(&ctx, &[(PILOT_1, 5_000_000_000.0), (PILOT_2, 100_000_000.0)]).await;
    let mut s1 = login(&ctx, 9207, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 9208, "other", PILOT_2).await;

    // 1. EverMarks: earn purely through AIR Daily Goals (500 char EM + 500 1:1 corp EM per goal)
    let corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "EMRK").await;
    let em_char_0 = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].loyalty_points.get(&PARAGON_CORP_ID).copied().unwrap_or(0));
    let em_corp_0 = ctx.db.with_snapshot(|s| s.corp_lp_balances.get(&(corp_id, PARAGON_CORP_ID)).copied().unwrap_or(0));

    for day in 0..5 {
        if day > 0 {
            ctx.advance_virtual_clock(86_405);
        }
        let goals = ctx.quasar_gateway.progression.daily_goals.get_or_init_goals(PILOT_1 as u32);
        let active_regs: Vec<_> = goals.into_iter().filter(|g| g.template.category != 6).take(2).collect();
        for g in &active_regs {
            if g.template.category == 2 {
                eve_server::services::space_mining_harvest::dispatch_mining_gameplay_events(&ctx, PILOT_1, g.template.target, 30_000_142, 1230);
            } else {
                ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(PILOT_1 as u32, g.template.category, g.template.target, &ctx.quasar_gateway);
            }
            let mut u_m = Vec::new();
            encode_bytes_field(1, &g.uuid, &mut u_m);
            let mut claim_req = Vec::new();
            encode_bytes_field(1, &u_m, &mut claim_req);
            let claim_rsp = quasar_call(&ctx, &s1, "eve_public.dailygoal.api.RedeemRequest", claim_req);
            assert_eq!(claim_rsp.status_code, STATUS_OK);
        }
    }

    let em_char_earned = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].loyalty_points.get(&PARAGON_CORP_ID).copied().unwrap_or(0));
    let em_corp_earned = ctx.db.with_snapshot(|s| s.corp_lp_balances.get(&(corp_id, PARAGON_CORP_ID)).copied().unwrap_or(0));
    assert_eq!(em_char_earned - em_char_0, 5_000, "Gameplay daily goals must grant 5,000 EverMarks");
    assert_eq!(em_corp_earned - em_corp_0, 5_000, "Corp must receive 1:1 matching 5,000 EverMarks");

    // Redeem Paragon ship emblem via LPStoreMgr at Paragon Hub station
    dock_station(&ctx, &mut s1, PARAGON_STATION).await;
    let PyRep::List(offers) = rpc(&ctx, &mut s1, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(PARAGON_CORP_ID)])
        .await
        .expect("Paragon LP store offers") else { panic!("expected offers list") };
    let emblem_offer = offers
        .iter()
        .find(|o| o.dict_get("lpCost").and_then(|v| v.as_i64()).is_some_and(|c| c > 0 && c <= 5_000))
        .expect("Paragon ship emblem offer <= 5000 EM");
    let eid = int(emblem_offer, "offerID");
    let ecost = int(emblem_offer, "lpCost");
    let etype = int(emblem_offer, "typeID");

    let buy_emblem = rpc(
        &ctx,
        &mut s1,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[PyRep::Integer(PARAGON_CORP_ID), PyRep::Integer(eid), PyRep::Integer(1)],
    )
    .await
    .expect("TakeOfferForCharacter Paragon emblem");
    assert_eq!(int(&buy_emblem, "deliveredTypeID"), etype);
    assert_eq!(int(&buy_emblem, "remainingLP"), em_char_earned - ecost);

    // 2. New Eden Store: create category, product, and offer via Quasar admin RPCs (no snapshot helper!)
    dock_station(&ctx, &mut s1, JITA_STATION).await;
    s1.role = service_roles::ROLE_GM_DEFAULT;
    seed_nes_store_via_quasar_admin(&ctx, &mut s1).await;
    s1.role = 0;

    let PyRep::List(nes_offers) = rpc(&ctx, &mut s1, "storeManager", "get_offers", &[]).await.unwrap() else {
        panic!("expected NES offers");
    };
    let nes_id = nes_offers
        .iter()
        .find(|o| o.dict_get("name").and_then(PyRep::as_str) == Some("Paragon Rifter Bundle"))
        .map(|o| int(o, "id"))
        .unwrap_or_else(|| int(nes_offers.last().unwrap(), "id"));
    assert_eq!(
        rpc(&ctx, &mut s1, "storeManager", "can_purchase_offer", &[PyRep::Integer(nes_id), PyRep::String("PLX".into()), PyRep::Integer(2)]).await.unwrap(),
        PyRep::Bool(false)
    );
    let aur_err = rpc(&ctx, &mut s1, "storeManager", "buy_offer", &[PyRep::Integer(nes_id), PyRep::String("AUR".into()), PyRep::Integer(1)])
        .await
        .expect_err("AUR removed");
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    deposit_plex(&ctx, &mut s1, JITA_STATION, 600).await;
    let bought = rpc(&ctx, &mut s1, "storeManager", "buy_offer", &[PyRep::Integer(nes_id), PyRep::String("PLX".into()), PyRep::Integer(1)])
        .await
        .expect("Purchase NES offer with PLEX");
    assert_eq!(int(&bought, "plexSpent"), 500);
    assert_eq!(int(&bought, "plexRemaining"), 600);
    assert!(int(&bought, "deliveredItemID") > 0);
}
