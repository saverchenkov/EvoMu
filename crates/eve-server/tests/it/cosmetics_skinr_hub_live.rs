//! Live Quasar integration tests for SKINR Sequencing Studio and Paragon Hub:
//! - Custom ship SKIN design persistence and component consumption
//! - Sequencing studio job initiation (`InitiateRequest`) and early completion (`CompleteEarlyRequest`)
//! - Paragon Hub skill scaling: tax reduction (84217, 84218, 84220) and listing capacity (81368, 81369)
//! - Paragon Hub listing creation, broker fees, escrow, purchase execution, and net tax settlement
//!
//! Parity tags:
//! - `feat:cosmetics.skinr-designer`
//! - `feat:cosmetics.skinr-sequencing-studio`
//! - `feat:cosmetics.skinr-paragon-hub-listing`
//! - `feat:cosmetics.skinr-market`
//! - `feat:economy.paragon-hub`

#![forbid(unsafe_code)]

use crate::common;
use crate::quasar_goal_tale_notices::{bytes, sub};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use crate::quasar_sov_equinox_hub::{field, fields, varint};
use eve_db::IskCents;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_market::{BUYER_FEES_VERSION, PARAGON_CORP_ID};
use eve_server::services::quasar_cosmetics_skinr::{
    encode_component_id, SkinrStaticCatalog, COMP_CAT_MATERIAL, COMP_CAT_METALLIC, COMP_CAT_PATTERN,
};

async fn deposit_evermarks_quasar(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    amount: u64,
) {
    let mut dep_req = Vec::new();
    let mut op_id_m = Vec::new();
    encode_bytes_field(1, b"paragon-em-op-01", &mut op_id_m);
    let mut cid_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut cid_msg);
    let mut issuer_msg = Vec::new();
    encode_varint_field(1, PARAGON_CORP_ID as u64, &mut issuer_msg);
    let mut wallet_id = Vec::new();
    encode_bytes_field(1, &cid_msg, &mut wallet_id);
    encode_bytes_field(3, &issuer_msg, &mut wallet_id);
    let mut curr_m = Vec::new();
    encode_varint_field(1, amount, &mut curr_m);
    let mut op_attr = Vec::new();
    encode_bytes_field(1, &wallet_id, &mut op_attr);
    encode_bytes_field(2, &curr_m, &mut op_attr);
    encode_bytes_field(1, &op_id_m, &mut dep_req);
    encode_bytes_field(2, &op_attr, &mut dep_req);

    let prev_role = sess.role;
    sess.role = service_roles::ROLE_GM_DEFAULT;
    let req = EvePublicRequest {
        issued: Some(eve_proto::quasar::types::GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: b"corr-em-live".to_vec(),
        payload: Some(GoogleAny {
            type_url: format!(
                "{}eve.loyaltypoints.assetholding.operation.api.DepositRequest",
                TYPE_GOOGLEAPIS_PREFIX
            ),
            value: dep_req,
        }),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"em-live-app".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let rsp = ctx
        .quasar_gateway
        .handle_public_request(sess, &req, None, false, ctx.now_unix());
    sess.role = prev_role;
    assert_eq!(rsp.status_code, STATUS_OK, "DepositRequest must succeed");
}

fn decode_wm(payload: Option<&GoogleAny>) -> Option<WireMessage> {
    WireMessage::decode(&payload?.value).ok()
}

fn wm_varint(wm: &WireMessage, field_num: u32) -> Option<u64> {
    wm.fields.iter().find(|f| f.field_number == field_num).and_then(|f| f.value.as_varint())
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
async fn test_cosmetics_skinr_designer_and_sequencing_studio_live() {
    // parity: feat:cosmetics.skinr-designer
    // parity: feat:cosmetics.skinr-sequencing-studio
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-designer",
        "feat:cosmetics.skinr-sequencing-studio",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let designer_cid = 90_000_901u32;
    let rifter_type = 587u32;

    ctx.db.provision_gm_account("gm_designer", "gm-pass", designer_cid as i64, "GM Designer");

    let mut designer_sess = build_sess(65_101, designer_cid, 1_000_006);
    designer_sess.role = service_roles::ROLE_GM_DEFAULT;
    for sid in [81377u32, 81370, 81366] {
        common::slash_cmd(&ctx, &mut designer_sess, &format!("/giveskill {sid} 5")).await;
    }
    designer_sess.role = 0;

    // 1. Verify sequencing tier durations and pricing via Quasar RPC
    let dur_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
        Vec::new(),
    ).await;
    assert_eq!(dur_rsp.status_code, STATUS_OK);
    let dur_discount = varint(&dur_rsp.payload.as_ref().unwrap().value, 2);
    assert_eq!(dur_discount, Some(15), "Basic Studio Induction V grants 15% duration discount");

    let pricing_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        Vec::new(),
    ).await;
    assert_eq!(pricing_rsp.status_code, STATUS_OK);
    assert_eq!(varint(&pricing_rsp.payload.as_ref().unwrap().value, 2), Some(0), "PLEX cost cannot be discounted");

    // 2. Select distinct SDE components and build custom layout
    let skinr_cat = SkinrStaticCatalog::load(&ctx.sde);
    let coat_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_MATERIAL && !c.disallowed_slots.contains(&1)).expect("nanocoating").component_id;
    let met_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_METALLIC && !c.disallowed_slots.contains(&2)).expect("metallic").component_id;
    let pat_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_PATTERN && !c.disallowed_slots.contains(&5)).expect("pattern").component_id;

    let mut layout = Vec::new();
    encode_bytes_field(1, &encode_coating_slot(1, coat_comp), &mut layout);
    encode_bytes_field(1, &encode_coating_slot(2, met_comp), &mut layout);
    encode_bytes_field(1, &encode_pattern_slot(5, pat_comp), &mut layout);
    let mut r_tid = Vec::new();
    encode_varint_field(1, rifter_type as u64, &mut r_tid);
    encode_bytes_field(2, &r_tid, &mut layout);
    encode_varint_field(3, 1, &mut layout);

    let mut init_req = Vec::new();
    encode_string_field(1, "Live Custom Paragon Rifter", &mut init_req);
    encode_string_field(2, "Master Studio Sequencing", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 2, &mut init_req); // 2 licenses

    // Initiating without component licenses fails
    designer_sess.role = 0;
    let fail_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req.clone(),
    ).await;
    assert_ne!(fail_rsp.status_code, STATUS_OK, "Cannot sequence without component licenses");

    // Grant finite component licenses via GM admin RPC
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
        ).await;
        assert_eq!(g_rsp.status_code, STATUS_OK);
    }
    designer_sess.role = 0;

    // 3. Initiate sequencing job: consumes PLEX, creates job, and produces job UUID + skin hex
    let plex_before = ctx.db.get_character(designer_cid as i64).unwrap().plex_balance;
    let init_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
    ).await;
    assert_eq!(init_rsp.status_code, STATUS_OK, "InitiateRequest failed: {}", init_rsp.status_message);

    let init_wm = decode_wm(init_rsp.payload.as_ref()).expect("init wm");
    let job_id_sub = sub(&init_wm, 1);
    let job_uuid = bytes(&job_id_sub, 1);
    assert_eq!(job_uuid.len(), 16, "Must receive valid 16-byte job UUID");

    let skin_id_sub = sub(&sub(&init_wm, 2), 1);
    let skin_hex = String::from_utf8(bytes(&skin_id_sub, 1)).expect("skin hex");
    assert!(!skin_hex.is_empty(), "Must receive valid skin hex");

    let plex_after_init = ctx.db.get_character(designer_cid as i64).unwrap().plex_balance;
    assert!(plex_after_init < plex_before, "Sequencing job must consume PLEX");

    // Verify active sequencing job via GetAllActiveRequest
    let act_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
    ).await;
    assert_eq!(act_rsp.status_code, STATUS_OK);
    let active_entries = fields(&act_rsp.payload.as_ref().unwrap().value, 1);
    assert_eq!(active_entries.len(), 1, "Exactly one active sequencing job in progress");

    // 4. Complete early via CompleteEarlyRequest through Quasar message
    let mut job_uuid_inner = Vec::new();
    encode_bytes_field(1, &job_uuid, &mut job_uuid_inner);
    let mut comp_req = Vec::new();
    encode_bytes_field(1, &job_uuid_inner, &mut comp_req);

    let comp_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
        comp_req,
    ).await;
    assert_eq!(comp_rsp.status_code, STATUS_OK, "CompleteEarlyRequest failed: {}", comp_rsp.status_message);

    let plex_after_comp = ctx.db.get_character(designer_cid as i64).unwrap().plex_balance;
    assert!(plex_after_comp < plex_after_init, "Early completion must charge PLEX");

    // Active sequencing jobs should now be 0
    let act_post_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        Vec::new(),
    ).await;
    assert_eq!(act_post_rsp.status_code, STATUS_OK);
    let active_post = fields(&act_post_rsp.payload.as_ref().unwrap().value, 1);
    assert_eq!(active_post.len(), 0, "No active sequencing jobs remain after early completion");

    // 5. Verify custom design persistence via GetRequest
    let mut get_skin_req = Vec::new();
    let mut hex_bytes = Vec::new();
    encode_string_field(1, &skin_hex, &mut hex_bytes);
    encode_bytes_field(1, &hex_bytes, &mut get_skin_req);
    let get_skin_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest",
        get_skin_req,
    ).await;
    assert_eq!(get_skin_rsp.status_code, STATUS_OK);
    let skin_wm = decode_wm(get_skin_rsp.payload.as_ref()).expect("skin wm");
    let name_val = String::from_utf8(bytes(&sub(&skin_wm, 1), 1)).unwrap_or_default();
    assert_eq!(name_val, "Live Custom Paragon Rifter");

    // 6. Verify tradeable custom SKIN licenses produced via GetRequest
    let mut lic_id = Vec::new();
    let mut seq_cid_msg = Vec::new();
    encode_varint_field(1, designer_cid as u64, &mut seq_cid_msg);
    encode_bytes_field(1, &seq_cid_msg, &mut lic_id);
    encode_bytes_field(2, &hex_bytes, &mut lic_id);

    let mut get_lic_req = Vec::new();
    encode_bytes_field(1, &lic_id, &mut get_lic_req);
    let lic_rsp = quasar_rpc(
        &ctx,
        &mut designer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        get_lic_req,
    ).await;
    assert_eq!(lic_rsp.status_code, STATUS_OK);
    let lic_wm = decode_wm(lic_rsp.payload.as_ref()).expect("lic wm");
    let lic_attrs = sub(&lic_wm, 1);
    assert_eq!(wm_varint(&lic_attrs, 1), Some(0), "License is not active on ship");
    assert_eq!(wm_varint(&lic_attrs, 2), Some(2), "Sequencing produced 2 unactivated custom SKIN licenses");
    assert_eq!(wm_varint(&lic_attrs, 3), Some(0), "Licenses are not yet escrowed");
}

#[tokio::test]
async fn test_cosmetics_skinr_paragon_hub_market_and_skills_live() {
    // parity: feat:cosmetics.skinr-market
    // parity: feat:cosmetics.skinr-paragon-hub-listing
    // parity: feat:economy.paragon-hub
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-market",
        "feat:cosmetics.skinr-paragon-hub-listing",
        "feat:economy.paragon-hub",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let seller_cid = 90_000_911u32;
    let buyer_cid = 90_000_912u32;
    let rifter_type = 587u32;

    ctx.db.provision_gm_account("gm_seller", "gm-pass", seller_cid as i64, "GM Seller");
    ctx.db.provision_gm_account("gm_buyer", "gm-pass", buyer_cid as i64, "GM Buyer");

    let mut seller_sess = build_sess(65_111, seller_cid, 1_000_006);
    seller_sess.role = 0;
    let mut buyer_sess = build_sess(65_112, buyer_cid, 1_000_006);
    buyer_sess.role = 0;

    deposit_evermarks_quasar(&ctx, &mut buyer_sess, buyer_cid as i64, 500_000).await;

    // 1. Skill scaling: verify public tax rate before and after Paragon skill training
    let mut pub_target = Vec::new();
    encode_varint_field(1, 1, &mut pub_target);
    let mut tax_req_pub = Vec::new();
    encode_bytes_field(1, &pub_target, &mut tax_req_pub);

    let tax_rsp_untrained = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        tax_req_pub.clone(),
    ).await;
    assert_eq!(tax_rsp_untrained.status_code, STATUS_OK);
    let tax_bytes_0 = field(&tax_rsp_untrained.payload.as_ref().unwrap().value, 1).expect("tax rate struct");
    let zigzag_0 = varint(&tax_bytes_0, 1).expect("rate units");
    let tax_pct_0 = (zigzag_0 >> 1) as i64;
    assert_eq!(tax_pct_0, 20, "Base public Paragon Hub tax must be 20% without skills");

    // Train Paragon tax reduction skills:
    // 84217 Paragon Relations (V -> -5%), 84218 Paragon Partnership (V -> -5%), 84220 Executive Paragon Partnership (V -> -5%)
    // Total reduction = 15%, reducing public tax down to 5%!
    // Also train 81368 Hub Promotion (V -> +5) and 81369 Hub Algorithm Optimization (V -> +25)
    seller_sess.role = service_roles::ROLE_GM_DEFAULT;
    for sid in [84217u32, 84218, 84220, 81368, 81369] {
        common::slash_cmd(&ctx, &mut seller_sess, &format!("/giveskill {sid} 5")).await;
    }
    seller_sess.role = 0;

    let tax_rsp_trained = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.api.GetTaxRateRequest",
        tax_req_pub,
    ).await;
    assert_eq!(tax_rsp_trained.status_code, STATUS_OK);
    let tax_bytes_1 = field(&tax_rsp_trained.payload.as_ref().unwrap().value, 1).expect("tax rate struct");
    let zigzag_1 = varint(&tax_bytes_1, 1).expect("rate units");
    let tax_pct_1 = (zigzag_1 >> 1) as i64;
    assert_eq!(tax_pct_1, 5, "Public Paragon Hub tax must be reduced from 20% down to 5% with Paragon skills");

    // 2. Prepare custom design and 10 licenses for testing listings & capacity expansion
    let skinr_cat = SkinrStaticCatalog::load(&ctx.sde);
    let coat_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_MATERIAL && !c.disallowed_slots.contains(&1)).expect("nanocoating").component_id;
    let met_comp = skinr_cat.components.values().find(|c| c.category == COMP_CAT_METALLIC && !c.disallowed_slots.contains(&2)).expect("metallic").component_id;

    seller_sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    for cid in [coat_comp, met_comp] {
        let mut grant_req = Vec::new();
        encode_bytes_field(1, &encode_component_id(cid, false), &mut grant_req);
        encode_varint_field(2, 20, &mut grant_req);
        let g_rsp = quasar_rpc(
            &ctx,
            &mut seller_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
            grant_req,
        ).await;
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
    encode_string_field(1, "Live Paragon Market Hub Rifter", &mut init_req);
    encode_string_field(2, "Hub Stock Live", &mut init_req);
    encode_bytes_field(3, &layout, &mut init_req);
    encode_varint_field(5, 10, &mut init_req); // 10 licenses

    let init_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        init_req,
    ).await;
    assert_eq!(init_rsp.status_code, STATUS_OK);
    let init_wm = decode_wm(init_rsp.payload.as_ref()).expect("init wm");
    let skin_id_sub = sub(&sub(&init_wm, 2), 1);
    let skin_hex = String::from_utf8(bytes(&skin_id_sub, 1)).expect("skin hex");

    // Complete job early via CompleteEarlyRequest
    let comp_rsp = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
        Vec::new(),
    ).await;
    assert_eq!(comp_rsp.status_code, STATUS_OK);

    // 3. Create 6 distinct listings to verify listing limit > 5 enabled by Hub Promotion & Hub Algorithm Optimization
    // Without skills, max active listings is 5. With 81368 and 81369 trained, capacity is 35.
    let mut skin_id_msg = Vec::new();
    encode_string_field(1, &skin_hex, &mut skin_id_msg);
    let mut lic_id = Vec::new();
    let mut seq_cid = Vec::new();
    encode_varint_field(1, seller_cid as u64, &mut seq_cid);
    encode_bytes_field(1, &seq_cid, &mut lic_id);
    encode_bytes_field(2, &skin_id_msg, &mut lic_id);

    let mut dur_msg = Vec::new();
    encode_varint_field(1, 86_400, &mut dur_msg);

    let seller_isk_before_listings = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;

    for i in 1..=6 {
        // Price: 1,000,000 ISK (sint64 zigzag = 2_000_000)
        let mut isk_val = Vec::new();
        encode_varint_field(1, 2_000_000, &mut isk_val);
        let mut create_req = Vec::new();
        encode_bytes_field(1, &isk_val, &mut create_req);
        encode_bytes_field(3, &lic_id, &mut create_req);
        encode_bytes_field(4, &dur_msg, &mut create_req);
        encode_varint_field(5, 1, &mut create_req);
        encode_bytes_field(6, &pub_target, &mut create_req);

        let list_rsp = quasar_rpc(
            &ctx,
            &mut seller_sess,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            create_req,
        ).await;
        assert_eq!(list_rsp.status_code, STATUS_OK, "Listing #{i} must succeed with expanded capacity: {}", list_rsp.status_message);
    }

    let seller_isk_after_listings = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;
    assert!(seller_isk_after_listings < seller_isk_before_listings, "Broker fees must be debited on listing creation");

    // Verify active owned listings query via GetAllOwnedRequest
    let rsp_owned = quasar_rpc(
        &ctx,
        &mut seller_sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
        Vec::new(),
    ).await;
    assert_eq!(rsp_owned.status_code, STATUS_OK);
    let owned_listings = fields(&rsp_owned.payload.as_ref().unwrap().value, 1);
    assert_eq!(owned_listings.len(), 6, "Must show all 6 active listings on Paragon Hub");

    // 4. Buyer queries marketplace via GetAllRequest
    let all_rsp = quasar_rpc(
        &ctx,
        &mut buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.GetAllRequest",
        Vec::new(),
    ).await;
    assert_eq!(all_rsp.status_code, STATUS_OK);
    let market_listings = fields(&all_rsp.payload.as_ref().unwrap().value, 1);
    assert_eq!(market_listings.len(), 6, "Marketplace displays all 6 public listings");

    // 5. Buyer purchases 1 listing
    let first_entry = &market_listings[0];
    let listing_uuid = field(&field(first_entry, 1).unwrap(), 1).unwrap();

    let buyer_isk_pre = ctx.db.get_character(buyer_cid as i64).unwrap().balance.0;
    let seller_isk_pre_purchase = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;

    let mut buy_req = Vec::new();
    let mut id_m = Vec::new();
    encode_bytes_field(1, &listing_uuid, &mut id_m);
    encode_bytes_field(1, &id_m, &mut buy_req);
    encode_varint_field(2, 1, &mut buy_req);
    encode_string_field(3, BUYER_FEES_VERSION, &mut buy_req);

    let buy_rsp = quasar_rpc(
        &ctx,
        &mut buyer_sess,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        buy_req,
    ).await;
    assert_eq!(buy_rsp.status_code, STATUS_OK, "Purchase failed: {}", buy_rsp.status_message);

    // 6. Verify ISK settlement and 5% tax reduction:
    // Listing price = 1,000,000 ISK.
    // Public sales tax with Paragon skills = 5% (50,000 ISK).
    // Net seller proceeds = 1,000,000 - 50,000 = 950,000 ISK (95,000,000 ISK cents).
    let buyer_isk_post = ctx.db.get_character(buyer_cid as i64).unwrap().balance.0;
    let seller_isk_post_purchase = ctx.db.get_character(seller_cid as i64).unwrap().balance.0;

    assert_eq!(
        buyer_isk_pre - buyer_isk_post,
        IskCents::from_isk(1_000_000).0,
        "Buyer balance must be debited 1,000,000 ISK"
    );
    assert_eq!(
        seller_isk_post_purchase - seller_isk_pre_purchase,
        IskCents::from_isk(950_000).0,
        "Seller must receive 950,000 ISK net proceeds (1,000,000 minus 5% skill-reduced sales tax)"
    );

    // 7. Verify buyer received tradeable custom SKIN license via GetRequest
    let mut buyer_lic_id = Vec::new();
    let mut b_cid_msg = Vec::new();
    encode_varint_field(1, buyer_cid as u64, &mut b_cid_msg);
    encode_bytes_field(1, &b_cid_msg, &mut buyer_lic_id);
    encode_bytes_field(2, &skin_id_msg, &mut buyer_lic_id);

    let mut get_lic_req = Vec::new();
    encode_bytes_field(1, &buyer_lic_id, &mut get_lic_req);

    let buyer_lic_rsp = quasar_rpc(
        &ctx,
        &mut buyer_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        get_lic_req,
    ).await;
    assert_eq!(buyer_lic_rsp.status_code, STATUS_OK);
    let buyer_lic_wm = decode_wm(buyer_lic_rsp.payload.as_ref()).expect("buyer lic wm");
    let buyer_lic_attrs = sub(&buyer_lic_wm, 1);
    assert_eq!(wm_varint(&buyer_lic_attrs, 2), Some(1), "Buyer now owns 1 unactivated custom SKIN license");
}
