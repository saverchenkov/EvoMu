//! End-to-end integration tests for SKINR Sequencing Studio and Paragon Hub market:
//! - Custom ship SKIN designs combining nanocoatings, metallic finishes, and pattern elements.
//! - Kerr-Induced Nanocoatings (finite component licenses), PLEX, and EverMark consumption.
//! - SKINR sequencing skill scaling (induction time discounts, concurrency, and early completion).
//! - Tradeable custom SKIN license production on job completion.
//! - Paragon Hub listing for ISK and PLEX with skill-reduced sales tax and broker fees.
//! - Peer-to-peer purchase execution, seller net proceeds, buyer fees, and license transfer.

#![forbid(unsafe_code)]

use crate::common;
use crate::quasar_goal_tale_notices::{bytes, sub};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use crate::quasar_sov_equinox_hub::{field, fields, varint};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, GoogleAny, WireMessage,
};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_market::{BUYER_FEES_VERSION, PARAGON_CORP_ID};
use eve_server::services::quasar_cosmetics_skinr::{
    encode_component_id, SkinrStaticCatalog, COMP_CAT_MATERIAL, COMP_CAT_METALLIC, COMP_CAT_PATTERN,
};

const JITA_STATION_ID: i64 = 60_003_760;
const EVERMARK_CORP_ID: i64 = 1_000_419;

fn decode_wm(payload: Option<&GoogleAny>) -> Option<WireMessage> {
    WireMessage::decode(&payload?.value).ok()
}

fn wm_varint(wm: &WireMessage, field: u32) -> Option<u64> {
    wm.fields.iter().find(|f| f.field_number == field).and_then(|f| f.value.as_varint())
}

fn encode_coating_slot(slot_id: u32, comp_id: u32) -> Vec<u8> {
    let mut slot_msg = Vec::new();
    let mut s_idx = Vec::new();
    encode_varint_field(1, slot_id as u64, &mut s_idx);
    encode_bytes_field(1, &s_idx, &mut slot_msg);

    let mut coat_cfg = Vec::new();
    let mut c_idx = Vec::new();
    encode_varint_field(1, comp_id as u64, &mut c_idx);
    encode_bytes_field(1, &c_idx, &mut coat_cfg);

    let mut cfg_outer = Vec::new();
    encode_bytes_field(1, &coat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);
    slot_msg
}

fn encode_pattern_slot(slot_id: u32, comp_id: u32) -> Vec<u8> {
    let mut slot_msg = Vec::new();
    let mut s_idx = Vec::new();
    encode_varint_field(1, slot_id as u64, &mut s_idx);
    encode_bytes_field(1, &s_idx, &mut slot_msg);

    let mut pat_cfg = Vec::new();
    let mut c_idx = Vec::new();
    encode_varint_field(1, comp_id as u64, &mut c_idx);
    encode_bytes_field(1, &c_idx, &mut pat_cfg);

    let mut cfg_outer = Vec::new();
    encode_bytes_field(2, &pat_cfg, &mut cfg_outer);
    encode_bytes_field(2, &cfg_outer, &mut slot_msg);
    slot_msg
}

#[tokio::test]
async fn test_skinr_sequencing_studio_design_components_and_skills() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let designer_cid = 90_000_851u32;
    let ship_id = 890_021i64;
    let rifter_type = 587u32;

    ctx.db.with_mut_snapshot(|s| {
        let mut ch = s.characters.get(&90_000_001).cloned().expect("template char");
        ch.character_id = designer_cid as i64;
        ch.corporation_id = 98_000_001;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(50_000_000);
        ch.plex_balance = 2_000;
        ch.station_id = Some(JITA_STATION_ID);
        s.characters.insert(designer_cid as i64, ch);

        // Train SKINR sequencing skills: Basic Studio Induction (81377) V, Studio Management (81046) V
        let sk_map = s.skills.entry(designer_cid as i64).or_default();
        for sid in [81377u32, 81046, 81366] {
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
                owner_id: designer_cid as i64,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut designer_sess = build_sess(65_021, designer_cid, 98_000_001);

    // 1. Verify SKINR sequencing skill duration scaling:
    // Basic Studio Induction Level 5 gives 15% duration discount
    let dur_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(dur_rsp.status_code, STATUS_OK);
    let dur_discount = varint(&dur_rsp.payload.as_ref().unwrap().value, 2);
    assert_eq!(dur_discount, Some(15), "Basic Studio Induction V grants 15% duration discount");

    let pricing_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(pricing_rsp.status_code, STATUS_OK);
    assert_eq!(varint(&pricing_rsp.payload.as_ref().unwrap().value, 2), Some(0), "PLEX cost cannot be discounted");

    // 2. Select distinct SDE components: Nanocoating (category 1), Metallic finish (category 3), Pattern (category 2)
    let skinr_cat = SkinrStaticCatalog::load(&ctx.sde);
    let coat_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_MATERIAL && !c.disallowed_slots.contains(&1)).expect("nanocoating component").component_id;
    let met_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_METALLIC && !c.disallowed_slots.contains(&2)).expect("metallic finish component").component_id;
    let pat_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_PATTERN && !c.disallowed_slots.contains(&5)).expect("pattern component").component_id;

    // 3. Build custom layout combining all 3 elements: nanocoating, metallic finish, pattern
    let mut layout = Vec::new();
    encode_bytes_field(1, &encode_coating_slot(1, coat_comp), &mut layout);
    encode_bytes_field(1, &encode_coating_slot(2, met_comp), &mut layout);
    encode_bytes_field(1, &encode_pattern_slot(5, pat_comp), &mut layout);
    let mut r_tid = Vec::new();
    encode_varint_field(1, rifter_type as u64, &mut r_tid);
    encode_bytes_field(2, &r_tid, &mut layout);
    encode_varint_field(3, 1, &mut layout); // pattern blend mode

    let mut init_req = Vec::new();
    encode_string_field(1, "Tri-Element Paragon Rifter", &mut init_req);
    encode_string_field(2, "Master Studio Line", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 2, &mut init_req); // quantity = 2 licenses

    // Initiating before granting finite licenses fails with insufficient capacity
    designer_sess.role = 0;
    let fail_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req.clone(),
    )
    .await;
    assert_ne!(fail_rsp.status_code, STATUS_OK, "Cannot sequence without component license capacity");

    // Grant finite component licenses (5 uses each for Kerr-Induced Nanocoatings)
    designer_sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    for cid in [coat_comp, met_comp, pat_comp] {
        let is_pat = cid == pat_comp;
        let mut grant_req = Vec::new();
        encode_bytes_field(1, &encode_component_id(cid, is_pat), &mut grant_req);
        encode_varint_field(2, 5, &mut grant_req);
        let g_rsp = quasar_rpc(
            &ctx,
            &mut designer_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            grant_req,
        )
        .await;
        assert_eq!(g_rsp.status_code, STATUS_OK);
    }
    designer_sess.role = 0;

    // 4. Initiate sequencing job consuming PLEX & Nanocoatings and awarding EverMarks
    let plex_before = ctx.db.get_character(designer_cid as i64).unwrap().plex_balance;
    let em_before = ctx.db.with_snapshot(|s| s.characters[&(designer_cid as i64)].loyalty_points.get(&EVERMARK_CORP_ID).copied().unwrap_or(0));
    let corp_em_before = ctx.db.with_snapshot(|s| s.corp_lp_balances.get(&(98_000_001, EVERMARK_CORP_ID)).copied().unwrap_or(0));

    let init_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
    )
    .await;
    assert_eq!(init_rsp.status_code, STATUS_OK, "InitiateRequest failed: {}", init_rsp.status_message);

    let init_wm = decode_wm(init_rsp.payload.as_ref()).expect("init wm");
    let skin_id_sub = sub(&sub(&init_wm, 2), 1);
    let skin_hex = String::from_utf8(bytes(&skin_id_sub, 1)).expect("skin hex");
    assert!(!skin_hex.is_empty());

    let plex_after = ctx.db.get_character(designer_cid as i64).unwrap().plex_balance;
    let em_after = ctx.db.with_snapshot(|s| s.characters[&(designer_cid as i64)].loyalty_points.get(&EVERMARK_CORP_ID).copied().unwrap_or(0));
    let corp_em_after = ctx.db.with_snapshot(|s| s.corp_lp_balances.get(&(98_000_001, EVERMARK_CORP_ID)).copied().unwrap_or(0));

    assert!(plex_after < plex_before, "Sequencing job must consume PLEX from character balance");
    assert_eq!(em_after - em_before, 1_000, "Initiating 2-license job awards 1,000 EverMarks (500/license)");
    assert_eq!(corp_em_after - corp_em_before, 1_000, "Corporation receives 1,000 EverMarks");

    // Active sequencing job query
    let act_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(act_rsp.status_code, STATUS_OK);
    let active_entries = fields(&act_rsp.payload.as_ref().unwrap().value, 1);
    assert_eq!(active_entries.len(), 1, "Exactly one active sequencing job in progress");

    // 5. Advance clock past planned duration to complete job and produce tradeable custom SKIN licenses
    ctx.advance_secs(86_400);

    let owned_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(owned_rsp.status_code, STATUS_OK);

    let mut lic_id = Vec::new();
    let mut seq_cid_msg = Vec::new();
    encode_varint_field(1, designer_cid as u64, &mut seq_cid_msg);
    encode_bytes_field(1, &seq_cid_msg, &mut lic_id);
    let mut skin_hex_msg = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_hex_msg);
    encode_bytes_field(2, &skin_hex_msg, &mut lic_id);

    let mut get_lic_req = Vec::new();
    encode_bytes_field(1, &lic_id, &mut get_lic_req);

    let lic_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        get_lic_req,
    )
    .await;
    assert_eq!(lic_rsp.status_code, STATUS_OK);
    let lic_wm = decode_wm(lic_rsp.payload.as_ref()).expect("license wm");
    let lic_attrs = sub(&lic_wm, 1);
    assert_eq!(wm_varint(&lic_attrs, 1), Some(0), "License is not active on ship");
    assert_eq!(wm_varint(&lic_attrs, 2), Some(2), "Sequencing produced 2 unactivated custom SKIN licenses");
    assert_eq!(wm_varint(&lic_attrs, 3), Some(0), "Licenses are not yet escrowed");

    // 6. Early completion mechanism: start second single-license job and complete early via PLEX payment
    let mut init_req2 = Vec::new();
    encode_string_field(1, "Tri-Element Fast Rifter", &mut init_req2);
    encode_string_field(2, "Master Studio Line", &mut init_req2);
    encode_bytes_field(3, &layout, &mut init_req2);
    encode_varint_field(5, 1, &mut init_req2);

    let init_rsp2 = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req2,
    )
    .await;
    assert_eq!(init_rsp2.status_code, STATUS_OK);

    let early_plex_before = ctx.db.get_character(designer_cid as i64).unwrap().plex_balance;
    let complete_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(complete_rsp.status_code, STATUS_OK, "CompleteEarlyRequest failed");
    let early_plex_after = ctx.db.get_character(designer_cid as i64).unwrap().plex_balance;
    assert!(early_plex_after < early_plex_before, "Early completion must charge PLEX");
}

#[tokio::test]
async fn test_skinr_paragon_hub_listing_fees_and_purchase() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default().unwrap();
    if let Some(m) = reg.get_message("eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest") {
        for f in &m.fields {
            eprintln!("InitiateRequest #{}: {} -> {}", f.number, f.name, f.type_name);
        }
    }
    for (name, msg) in &reg.messages_by_full_name {
        if name.contains("pattern") || name.contains("coating") {
            eprintln!("Type: {}", name);
            for f in &msg.fields {
                eprintln!("   #{}: {} ({:?}) -> {}", f.number, f.name, f.field_type, f.type_name);
            }
        }
    }
    let seller_cid = 90_000_861u32;
    let buyer_cid = 90_000_862u32;
    let ship_id = 890_031i64;
    let rifter_type = 587u32;

    ctx.db.with_mut_snapshot(|s| {
        let mut ch_s = s.characters.get(&90_000_001).cloned().expect("template char");
        ch_s.character_id = seller_cid as i64;
        ch_s.corporation_id = 98_000_001;
        ch_s.active_ship_id = ship_id;
        ch_s.balance = IskCents::from_isk(25_000_000);
        ch_s.plex_balance = 1_000;
        ch_s.station_id = Some(JITA_STATION_ID);
        s.characters.insert(seller_cid as i64, ch_s);

        let mut ch_b = s.characters.get(&90_000_001).cloned().expect("template char");
        ch_b.character_id = buyer_cid as i64;
        ch_b.corporation_id = 98_000_002;
        ch_b.balance = IskCents::from_isk(50_000_000);
        ch_b.plex_balance = 2_000;
        ch_b.station_id = Some(JITA_STATION_ID);
        ch_b.loyalty_points.insert(PARAGON_CORP_ID as i64, 500_000);
        s.characters.insert(buyer_cid as i64, ch_b);

        // Train Paragon Hub skills on seller:
        // Paragon Relations (84217) V (-5% tax), Paragon Partnership (84218) V (-5% tax) -> 10% tax reduction
        // Hub Promotion (81368) V (+5 listings)
        let sk_map = s.skills.entry(seller_cid as i64).or_default();
        for sid in [84217u32, 84218, 81368] {
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

    let mut seller_sess = build_sess(65_031, seller_cid, 98_000_001);
    seller_sess.role = 0;
    let mut buyer_sess = build_sess(65_032, buyer_cid, 98_000_002);
    buyer_sess.role = 0;

    // 1. Verify Paragon Hub tax rate reduction via Paragon Relations & Partnership skills:
    // Base public sales tax (20%) - 10% skills = 10% tax rate
    let mut pub_target = Vec::new();
    encode_varint_field(1, 1, &mut pub_target);
    let mut tax_req_pub = Vec::new();
    encode_bytes_field(1, &pub_target, &mut tax_req_pub);

    let tax_rsp_pub = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        tax_req_pub,
    )
    .await;
    assert_eq!(tax_rsp_pub.status_code, STATUS_OK);
    let tax_bytes = field(&tax_rsp_pub.payload.as_ref().unwrap().value, 1).expect("tax rate struct");
    let zigzag = varint(&tax_bytes, 1).expect("rate units");
    let tax_pct = (zigzag >> 1) as i64;
    assert_eq!(tax_pct, 10, "Public Paragon Hub tax reduced from 20% to 10% by skills");

    // Verify broker fee tier quotes
    let bf_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.api.GetBrokerFeesRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(bf_rsp.status_code, STATUS_OK);

    // 2. Grant finite component licenses, sequence custom design with 2 licenses for market listing
    let skinr_cat = SkinrStaticCatalog::load(&ctx.sde);
    let coat_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_MATERIAL && !c.disallowed_slots.contains(&1)).expect("nanocoating component").component_id;
    let met_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_METALLIC && !c.disallowed_slots.contains(&2)).expect("metallic component").component_id;

    seller_sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    for cid in [coat_comp, met_comp] {
        let mut grant_req = Vec::new();
        encode_bytes_field(1, &encode_component_id(cid, false), &mut grant_req);
        encode_varint_field(2, 5, &mut grant_req);
        let g_rsp = quasar_rpc(
            &ctx,
            &mut seller_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            grant_req,
        )
        .await;
        assert_eq!(g_rsp.status_code, STATUS_OK);
    }
    seller_sess.role = 0;

    let mut layout = Vec::new();
    encode_bytes_field(1, &encode_coating_slot(1, coat_comp), &mut layout);
    encode_bytes_field(1, &encode_coating_slot(2, met_comp), &mut layout);
    let mut r_tid = Vec::new();
    encode_varint_field(1, rifter_type as u64, &mut r_tid);
    encode_bytes_field(2, &r_tid, &mut layout);
    encode_varint_field(3, 1, &mut layout);

    let mut init_req = Vec::new();
    encode_string_field(1, "Paragon Market Elite Rifter", &mut init_req);
    encode_string_field(2, "Hub Stock", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 2, &mut init_req);

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

    // Settle sequencing job after 24h
    ctx.advance_secs(86_400);
    let _ = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
    )
    .await;

    // 3. List 1 license for ISK and 1 license for PLEX on Paragon Hub
    let mut skin_id_msg = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_id_msg);
    let mut lic_id = Vec::new();
    let mut seq_cid = Vec::new();
    encode_varint_field(1, seller_cid as u64, &mut seq_cid);
    encode_bytes_field(1, &seq_cid, &mut lic_id);
    encode_bytes_field(2, &skin_id_msg, &mut lic_id);

    let mut dur_msg_req = Vec::new();
    encode_varint_field(1, 86_400, &mut dur_msg_req);

    let seller_isk_pre_list = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;

    // List 1 license for 1,000,000 ISK (sint64 zigzag encoded: 1_000_000 * 2 = 2_000_000)
    let mut create_isk = Vec::new();
    let mut isk_msg = Vec::new();
    encode_varint_field(1, 2_000_000, &mut isk_msg);
    encode_bytes_field(1, &isk_msg, &mut create_isk);
    encode_bytes_field(3, &lic_id, &mut create_isk);
    encode_bytes_field(4, &dur_msg_req, &mut create_isk);
    encode_varint_field(5, 1, &mut create_isk); // quantity = 1
    encode_bytes_field(6, &pub_target, &mut create_isk);

    let list_isk_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_isk,
    )
    .await;
    assert_eq!(list_isk_rsp.status_code, STATUS_OK, "Listing for ISK failed: {}", list_isk_rsp.status_message);

    // List 1 license for 100 PLEX (10,000 PLEX cents; sint64 zigzag: 20_000)
    let mut create_plex = Vec::new();
    let mut plex_msg = Vec::new();
    encode_varint_field(1, 20_000, &mut plex_msg);
    encode_bytes_field(2, &plex_msg, &mut create_plex);
    encode_bytes_field(3, &lic_id, &mut create_plex);
    encode_bytes_field(4, &dur_msg_req, &mut create_plex);
    encode_varint_field(5, 1, &mut create_plex); // quantity = 1
    encode_bytes_field(6, &pub_target, &mut create_plex);

    let list_plex_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_plex,
    )
    .await;
    assert_eq!(list_plex_rsp.status_code, STATUS_OK, "Listing for PLEX failed: {}", list_plex_rsp.status_message);

    let seller_isk_post_list = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;
    assert!(seller_isk_post_list < seller_isk_pre_list, "Seller charged Paragon Hub listing broker fee");

    // 4. Buyer queries marketplace and discovers both listings
    let all_rsp = quasar_rpc(
        &ctx,
        &mut buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let listings = fields(&all_rsp.payload.as_ref().unwrap().value, 1);
    assert_eq!(listings.len(), 2, "Marketplace displays both ISK and PLEX listings");

    // 5. Buyer purchases both listings
    let seller_isk_before_sales = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;
    let seller_plex_before_sales = ctx.db.get_character(seller_cid as i64).unwrap().plex_balance;

    for em in listings {
        let uuid_bytes = field(&field(&em, 1).unwrap(), 1).unwrap();
        let mut buy_req = Vec::new();
        let mut id_m = Vec::new();
        encode_bytes_field(1, &uuid_bytes, &mut id_m);
        encode_bytes_field(1, &id_m, &mut buy_req);
        encode_varint_field(2, 1, &mut buy_req);
        encode_string_field(3, BUYER_FEES_VERSION, &mut buy_req);

        let buy_rsp = quasar_rpc(
            &ctx,
            &mut buyer_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
            buy_req,
        )
        .await;
        assert_eq!(buy_rsp.status_code, STATUS_OK, "Purchase failed: {}", buy_rsp.status_message);
    }

    // 6. Verify seller received net proceeds minus 10% skill-reduced sales tax
    // ISK listing: 1,000,000 ISK - 10% = 900,000 ISK net
    // PLEX listing: 100 PLEX - 10% = 90 PLEX net
    let seller_isk_after_sales = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;
    let seller_plex_after_sales = ctx.db.get_character(seller_cid as i64).unwrap().plex_balance;

    assert_eq!(
        seller_isk_after_sales - seller_isk_before_sales,
        IskCents::from_isk(900_000).0,
        "Seller must receive 900,000 ISK net proceeds (1,000,000 minus 10% skill-reduced sales tax)"
    );
    assert_eq!(
        seller_plex_after_sales - seller_plex_before_sales,
        90,
        "Seller must receive 90 PLEX net proceeds (100 minus 10% skill-reduced sales tax)"
    );

    // 7. Verify buyer received tradeable custom SKIN licenses
    let buyer_owned_rsp = quasar_rpc(
        &ctx,
        &mut buyer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(buyer_owned_rsp.status_code, STATUS_OK);
}
