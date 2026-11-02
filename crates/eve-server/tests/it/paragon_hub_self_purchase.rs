//! Negative regression test for Paragon Hub SKIN self-purchase ISK minting (B23-P0-SELFBUY-MINT).
use crate::quasar_goal_tale_notices::{bytes, sub};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, WireMessage};
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_market::PARAGON_CORP_ID;

fn player_sess(sid: u64, cid: u32, corp: u32) -> SessionState {
    let mut s = build_sess(sid, cid, corp);
    s.user_id = Some(sid);
    s.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;
    s
}

#[tokio::test]
async fn test_b23_paragon_hub_skin_self_purchase_rejected() {
    let ctx = ServerContext::new();
    let seller_cid: u32 = 95_230_901;
    let corp_id: u32 = 98_230_901;
    ctx.db.with_mut_snapshot(|s| {
        let template = s.characters[&90_000_001].clone();
        let ch = s.characters.entry(seller_cid as i64).or_insert_with(|| template);
        ch.character_id = seller_cid as i64;
        ch.corporation_id = corp_id as i64;
        ch.balance = IskCents::from_isk(50_000_000);
        ch.loyalty_points.insert(PARAGON_CORP_ID as i64, 100_000);
        ch.station_id = Some(60_003_760);
    });
    let mut seller = player_sess(952_901, seller_cid, corp_id);
    let mut seller_gm = build_sess(952_909, seller_cid, corp_id);

    let skin_hex = "b2300901000000000000000000000001";
    let mut grant_req = Vec::new();
    let mut skin_id = Vec::new();
    encode_string_field(1, skin_hex, &mut skin_id);
    encode_bytes_field(1, &skin_id, &mut grant_req);
    encode_varint_field(2, 1, &mut grant_req);
    let rsp_grant = quasar_rpc(
        &ctx,
        &mut seller_gm,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
        grant_req,
    ).await;
    assert_eq!(rsp_grant.status_code, STATUS_OK);

    let mut price_isk = Vec::new();
    encode_varint_field(1, 10_000_000 * 2, &mut price_isk); // sint64 zigzag of 10_000_000
    let mut lic_id_msg = Vec::new();
    let mut seller_seq = Vec::new();
    encode_varint_field(1, seller_cid as u64, &mut seller_seq);
    encode_bytes_field(1, &seller_seq, &mut lic_id_msg);
    encode_bytes_field(2, &skin_id, &mut lic_id_msg);
    let mut dur_msg = Vec::new();
    encode_varint_field(1, 86_400, &mut dur_msg);
    let mut target_msg = Vec::new();
    encode_varint_field(1, 1, &mut target_msg);
    let mut create_req = Vec::new();
    encode_bytes_field(1, &price_isk, &mut create_req);
    encode_bytes_field(3, &lic_id_msg, &mut create_req);
    encode_bytes_field(4, &dur_msg, &mut create_req);
    encode_varint_field(5, 1, &mut create_req);
    encode_bytes_field(6, &target_msg, &mut create_req);
    let rsp_create = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        create_req,
    ).await;
    assert_eq!(rsp_create.status_code, STATUS_OK);

    let rsp_owned = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.GetAllOwnedRequest",
        Vec::new(),
    ).await;
    assert_eq!(rsp_owned.status_code, STATUS_OK);
    let wm_owned = WireMessage::decode(&rsp_owned.payload.as_ref().unwrap().value).expect("decode owned");
    let owned_listing = sub(&wm_owned, 1);
    let listing_uuid = bytes(&sub(&owned_listing, 1), 1);

    let (bal_before, journal_len_before) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&(seller_cid as i64)].balance,
            s.wallet_journal.iter().filter(|e| e.character_id == seller_cid as i64).count(),
        )
    });

    let mut uuid_msg = Vec::new();
    encode_bytes_field(1, &listing_uuid, &mut uuid_msg);
    let mut buy_req = Vec::new();
    encode_bytes_field(1, &uuid_msg, &mut buy_req);
    encode_varint_field(2, 1, &mut buy_req);
    let rsp_self_buy = quasar_rpc(
        &ctx,
        &mut seller,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        buy_req,
    ).await;
    assert_eq!(rsp_self_buy.status_code, 400);

    let (bal_after, journal_len_after) = ctx.db.with_snapshot(|s| {
        (
            s.characters[&(seller_cid as i64)].balance,
            s.wallet_journal.iter().filter(|e| e.character_id == seller_cid as i64).count(),
        )
    });
    assert_eq!(bal_after, bal_before, "self-purchase must not alter ISK balance");
    assert_eq!(journal_len_after, journal_len_before, "self-purchase must not append wallet journal entries");
}
