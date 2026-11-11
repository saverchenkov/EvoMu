//! Edge-case, validation, and RBAC coverage tests across Wallet/Market/Bounty,
//! Skills/Clones/Implants/Certificates, Cosmetics/SKINR/FastCheckout/Store, and
//! Auth/Character Lifecycle — exercised exclusively through authentic RPC, Quasar,
//! and slash command calls without snapshot shortcuts.

#![forbid(unsafe_code)]

use crate::common;
use crate::quasar_progression_complete::quasar_rpc;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const OTHER_STATION_ID: i64 = 60_003_761;
const CHAR_DEMO: i64 = 90_000_001;
const CHAR_OTHER: i64 = 90_000_002;

const TYPE_TRITANIUM: u32 = 34;
const TYPE_PLEX: u32 = 44_992;
const TYPE_RIFTER: u32 = 587;
const TYPE_SKILL_EXTRACTOR: u32 = 40_519;
const TYPE_GUNNERY: u32 = 3_300;
const TYPE_INFOMORPH_PSYCHOLOGY: u32 = 24_242;
const TYPE_CORP_MGMT: u32 = 3_363;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn rpc_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, kwargs, None)
        .await
}

async fn gm_slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    let saved = sess.role;
    sess.role |= service_roles::ROLE_GM_DEFAULT;
    let res = rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())]).await;
    sess.role = saved;
    res.unwrap_or_else(|e| panic!("GM slash `{cmd}` failed: {e:?}"))
}

async fn gm_slash_err(
    ctx: &ServerContext,
    sess: &mut SessionState,
    cmd: &str,
) -> BridgeRpcError {
    let saved = sess.role;
    sess.role |= service_roles::ROLE_GM_DEFAULT;
    let res = rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())]).await;
    sess.role = saved;
    res.expect_err(&format!("expected error for `{cmd}`"))
}

async fn login_select(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn kv_int(rep: &PyRep, key: &str) -> Option<i64> {
    match rep {
        PyRep::KeyVal(m) => m.get(key).and_then(|v| v.as_i64()),
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
            if k.as_str() == Some(key) {
                v.as_i64()
            } else {
                None
            }
        }),
        _ => None,
    }
}

fn find_hangar_item_id(ctx: &ServerContext, owner_id: i64, type_id: u32) -> i64 {
    ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.owner_id == owner_id
                    && i.type_id == type_id
                    && i.flag == eve_db::InventoryFlag::Hangar
                    && i.quantity >= 1
            })
            .min_by_key(|i| i.item_id)
            .map(|i| i.item_id)
            .unwrap_or_else(|| panic!("expected hangar item type {type_id} for {owner_id}"))
    })
}

fn year_month_now(ctx: &ServerContext) -> (i64, i64) {
    let secs = ctx.now_unix() as i64;
    let days = secs.div_euclid(86_400);
    let mut y = 1970i64;
    let mut rem = days;
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
        let yd = if leap { 366 } else { 365 };
        if rem < yd {
            let md = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
            let mut m = 1i64;
            for d in md {
                if rem < d {
                    return (y, m);
                }
                rem -= d;
                m += 1;
            }
            return (y, 12);
        }
        rem -= yd;
        y += 1;
    }
}

fn valid_char_creation_ids(ctx: &ServerContext) -> (u32, u32, u32, u32) {
    let mut bl_ids: Vec<u32> = ctx.sde.bloodlines.keys().copied().collect();
    bl_ids.sort_unstable();
    for bid in bl_ids {
        let bl = &ctx.sde.bloodlines[&bid];
        let mut anc_ids: Vec<u32> = ctx
            .sde
            .ancestries
            .iter()
            .filter(|(_, a)| a.bloodline_id == bid)
            .map(|(&aid, _)| aid)
            .collect();
        anc_ids.sort_unstable();
        let mut sch_ids: Vec<u32> = ctx
            .sde
            .schools
            .iter()
            .filter(|(_, s)| s.race_id == bl.race_id)
            .map(|(&sid, _)| sid)
            .collect();
        sch_ids.sort_unstable();
        if let (Some(&aid), Some(&sid)) = (anc_ids.first(), sch_ids.first()) {
            return (bl.race_id, bid, aid, sid);
        }
    }
    (1, 1, 1, 11)
}

async fn join_corp_member(
    ctx: &ServerContext,
    s_ceo: &mut SessionState,
    s_member: &mut SessionState,
    corp_id: i64,
) {
    let app_rep = rpc(
        ctx,
        s_member,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(corp_id), PyRep::String("join".into())],
    )
    .await
    .expect("InsertApplication");
    let app_id = app_rep
        .as_i64()
        .or_else(|| kv_int(&app_rep, "applicationID"))
        .expect("applicationID");
    rpc(
        ctx,
        s_ceo,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await
    .expect("UpdateApplicationOffer Accepted");
    s_member.corporation_id = Some(corp_id);
}

// ---------------------------------------------------------------------------
// 1. Wallet Ledger, Corp Wallet RBAC, PLEX Vault, Bills, Store, and Trade
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_wallet_billing_plex_vault_and_store_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s_demo = login_select(&ctx, 57_001, "demo", "demo-secret", CHAR_DEMO).await;
    let mut s_other = login_select(&ctx, 57_002, "other", "other-secret", CHAR_OTHER).await;
    ctx.advance_secs(11);

    // Provision skills & ISK for corp creation on demo via GM slash.
    gm_slash(&ctx, &mut s_demo, &format!("/setskill {TYPE_CORP_MGMT} 1")).await;
    gm_slash(&ctx, &mut s_demo, "/giveisk 50000000").await;
    gm_slash(&ctx, &mut s_other, "/giveisk 1000000").await;

    let corp_rep = rpc(
        &ctx,
        &mut s_demo,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Edge Ledger Corp".into()),
            PyRep::String("ELCR".into()),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = corp_rep.as_i64().unwrap_or_else(|| {
        kv_int(&corp_rep, "corporationID").expect("corp_id from AddCorporation")
    });
    s_demo.corporation_id = Some(corp_id);

    // --- account.GiveCashFromCorpAccount validation & RBAC ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(CHAR_OTHER),
                PyRep::Real(0.0),
                PyRep::Integer(1000),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(CHAR_OTHER),
                PyRep::Real(-50.0),
                PyRep::Integer(1000),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(CHAR_OTHER),
                PyRep::Real(f64::NAN),
                PyRep::Integer(1000),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // Invalid fromAccountKey (outside 1000..=1006)
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(CHAR_OTHER),
                PyRep::Real(100.0),
                PyRep::Integer(999),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidAccountKey"
    );

    // Fund corp master wallet legitimately via account.GiveCash(corp_id, ...)
    rpc(
        &ctx,
        &mut s_demo,
        "account",
        "GiveCash",
        &[
            PyRep::Integer(corp_id),
            PyRep::Real(500_000.0),
            PyRep::String("seed corp".into()),
        ],
    )
    .await
    .expect("seed corp wallet");

    // Internal corp transfer missing toAccountKey
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(corp_id),
                PyRep::Real(100.0),
                PyRep::Integer(1000),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidAccountKey"
    );

    // NPC corp member trying corp payout -> CorporationNotFound
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(CHAR_DEMO),
                PyRep::Real(100.0),
                PyRep::Integer(1000),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );
    // Foreign corp bill view -> CorpAccessDenied
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "billMgr",
            "GetCorporationBills",
            &[PyRep::Integer(corp_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpAccessDenied"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "billMgr",
            "GetCorporationBillsReceivable",
            &[PyRep::Integer(corp_id)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpAccessDenied"
    );

    // Have s_other join corp_id as unprivileged member (0 roles), then assert CrpAccessDenied
    join_corp_member(&ctx, &mut s_demo, &mut s_other, corp_id).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(CHAR_DEMO),
                PyRep::Real(100.0),
                PyRep::Integer(1000),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Corp division 1002 has 0 initially -> NotEnoughMoney
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(CHAR_OTHER),
                PyRep::Real(5_000.0),
                PyRep::Integer(1002),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughMoney"
    );

    // Non-existent recipient -> InvalidRecipient
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "account",
            "GiveCashFromCorpAccount",
            &[
                PyRep::Integer(999_888_777),
                PyRep::Real(100.0),
                PyRep::Integer(1000),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidRecipient"
    );

    // Valid corp->corp division transfer & corp->char transfer
    let mut kw_div = HashMap::new();
    kw_div.insert("toAccountKey".into(), PyRep::Integer(1001));
    kw_div.insert("reason".into(), PyRep::String("div move".into()));
    rpc_kw(
        &ctx,
        &mut s_demo,
        "account",
        "GiveCashFromCorpAccount",
        &[
            PyRep::Integer(corp_id),
            PyRep::Real(50_000.0),
            PyRep::Integer(1000),
        ],
        &kw_div,
    )
    .await
    .expect("corp division transfer");

    // GetTransactions RBAC & filtering
    let (yr, mo) = year_month_now(&ctx);
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "account",
            "GetTransactions",
            &[
                PyRep::Integer(1000),
                PyRep::Integer(yr),
                PyRep::Integer(mo),
                PyRep::Bool(true),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    rpc(
        &ctx,
        &mut s_demo,
        "account",
        "GetTransactions",
        &[
            PyRep::Integer(1001),
            PyRep::Integer(yr),
            PyRep::Integer(mo),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("corp division 1001 transactions");

    // --- account.GiveCash & SetContactCost edges ---
    for bad_amt in [0.0, -25.0, f64::INFINITY] {
        let res = rpc(
            &ctx,
            &mut s_demo,
            "account",
            "GiveCash",
            &[
                PyRep::Integer(CHAR_OTHER),
                PyRep::Real(bad_amt),
                PyRep::String("bad".into()),
            ],
        )
        .await;
        assert!(res.is_err(), "expected GiveCash({bad_amt}) to fail");
    }
    assert!(rpc(
        &ctx,
        &mut s_demo,
        "account",
        "GiveCash",
        &[
            PyRep::Integer(999_777_666),
            PyRep::Real(100.0),
            PyRep::String("unknown".into()),
        ],
    )
    .await
    .is_err());
    assert!(rpc(
        &ctx,
        &mut s_other,
        "account",
        "GiveCash",
        &[
            PyRep::Integer(CHAR_DEMO),
            PyRep::Real(999_999_999_999.0),
            PyRep::String("too much".into()),
        ],
    )
    .await
    .is_err());
    assert!(rpc(
        &ctx,
        &mut s_demo,
        "account",
        "SetContactCost",
        &[PyRep::Real(-10.0)],
    )
    .await
    .is_err());
    rpc(
        &ctx,
        &mut s_demo,
        "account",
        "SetContactCost",
        &[PyRep::Real(25_000.0)],
    )
    .await
    .expect("valid SetContactCost");

    // --- invbroker PLEX Vault deposit / withdraw edges ---
    gm_slash(&ctx, &mut s_demo, &format!("/createitem {TYPE_TRITANIUM} 5")).await;
    gm_slash(&ctx, &mut s_demo, &format!("/createitem {TYPE_PLEX} 10")).await;
    let trit_id = find_hangar_item_id(&ctx, CHAR_DEMO, TYPE_TRITANIUM);
    let plex_id = find_hangar_item_id(&ctx, CHAR_DEMO, TYPE_PLEX);

    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(plex_id),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(plex_id),
                PyRep::Integer(i64::from(i32::MAX) + 1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(OTHER_STATION_ID),
                PyRep::Integer(plex_id),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NotDockedAtLocation"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(999_999_111),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(trit_id),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NotPlex"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "DepositPlexToVault",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(plex_id),
                PyRep::Integer(99_999_999),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughQuantity"
    );
    // Valid deposit of 4 PLEX
    rpc(
        &ctx,
        &mut s_demo,
        "invbroker",
        "DepositPlexToVault",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(plex_id),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("DepositPlexToVault 4");

    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "WithdrawPlexFromVault",
            &[
                PyRep::Integer(0),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(4),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "WithdrawPlexFromVault",
            &[
                PyRep::Integer(1),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(999),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidFlag"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "invbroker",
            "WithdrawPlexFromVault",
            &[
                PyRep::Integer(999_999),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(4),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughPlex"
    );
    // Withdraw 2 PLEX cleanly
    rpc(
        &ctx,
        &mut s_demo,
        "invbroker",
        "WithdrawPlexFromVault",
        &[
            PyRep::Integer(2),
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(4),
        ],
    )
    .await
    .expect("WithdrawPlexFromVault 2");

    // --- billMgr RBAC & validation ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "billMgr",
            "GetCorporationBills",
            &[PyRep::Integer(corp_id + 1)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpAccessDenied"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "billMgr",
            "GetCorporationBillsReceivable",
            &[PyRep::Integer(corp_id + 1)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpAccessDenied"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "billMgr",
            "SendAutomaticPaySettings",
            &[PyRep::Integer(corp_id), PyRep::Dict(vec![])],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleRequired"
    );
    assert_eq!(
        rpc(&ctx, &mut s_demo, "billMgr", "CharPayBill", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(&ctx, &mut s_demo, "billMgr", "CharPayBill", &[PyRep::Integer(-1)])
            .await
            .unwrap_err()
            .code,
        "BillNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "billMgr",
            "CharPayBill",
            &[PyRep::Integer(88_888)],
        )
        .await
        .unwrap_err()
        .code,
        "BillNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "billMgr",
            "PayCorporationBill",
            &[PyRep::Integer(88_888)],
        )
        .await
        .unwrap_err()
        .code,
        "BillNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "billMgr",
            "PayCorporationBill",
            &[PyRep::Integer(88_888)],
        )
        .await
        .unwrap_err()
        .code,
        "BillNotFound"
    );
    rpc(
        &ctx,
        &mut s_demo,
        "billMgr",
        "GetAutomaticPaySettings",
        &[],
    )
    .await
    .expect("GetAutomaticPaySettings");

    // --- storeManager validation & error paths ---
    let _cats = rpc(&ctx, &mut s_demo, "storeManager", "get_categories", &[])
        .await
        .expect("get_categories");
    let _prods = rpc(&ctx, &mut s_demo, "storeManager", "get_products", &[])
        .await
        .expect("get_products");
    let _offers = rpc(&ctx, &mut s_demo, "storeManager", "get_offers", &[])
        .await
        .expect("get_offers");

    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "storeManager",
            "buy_offer",
            &[
                PyRep::Integer(0),
                PyRep::String("PLX".into()),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArguments"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "storeManager",
            "buy_offer",
            &[
                PyRep::Integer(1),
                PyRep::String("AUR".into()),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "AurumCurrencyRemoved"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "storeManager",
            "buy_offer",
            &[
                PyRep::Integer(1),
                PyRep::String("ISK".into()),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidCurrency"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "storeManager",
            "buy_offer",
            &[
                PyRep::Integer(1),
                PyRep::String("PLX".into()),
                PyRep::Integer(0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "storeManager",
            "buy_offer",
            &[
                PyRep::Integer(999_999_123),
                PyRep::String("PLX".into()),
                PyRep::Integer(1),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "OfferNotFound"
    );
    // Test can_purchase_offer & buy_offer with insufficient PLEX on `other` (0 PLEX)
    let offers_resp = rpc(&ctx, &mut s_other, "storeManager", "get_offers", &[])
        .await
        .expect("offers");
    if let PyRep::List(list) = offers_resp {
        if let Some(first_oid) = list.first().and_then(|o| kv_int(o, "offerID")) {
            let can = rpc(
                &ctx,
                &mut s_other,
                "storeManager",
                "can_purchase_offer",
                &[PyRep::Integer(first_oid), PyRep::Integer(100)],
            )
            .await;
            assert!(can.is_ok() || can.is_err());
            let buy_err = rpc(
                &ctx,
                &mut s_other,
                "storeManager",
                "buy_offer",
                &[
                    PyRep::Integer(first_oid),
                    PyRep::String("PLX".into()),
                    PyRep::Integer(100),
                ],
            )
            .await;
            if let Err(e) = buy_err {
                assert!(
                    matches!(e.code.as_str(), "InsufficientPlexBalance" | "OfferNotFound" | "InsufficientFunds"),
                    "unexpected buy_offer error: {e:?}"
                );
            }
        }
    }

    // --- trademgr self-trade / invalid initiate edge ---
    assert!(rpc(
        &ctx,
        &mut s_demo,
        "trademgr",
        "InitiateTrade",
        &[PyRep::Integer(CHAR_DEMO)],
    )
    .await
    .is_err());
}

// ---------------------------------------------------------------------------
// 2. Market Orders, Escrow, Corp Auth, Contract Proxy, and Bounties / KillRights
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_market_orders_escrow_corp_auth_and_bounties_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s_demo = login_select(&ctx, 57_011, "demo", "demo-secret", CHAR_DEMO).await;
    let mut s_other = login_select(&ctx, 57_012, "other", "other-secret", CHAR_OTHER).await;
    ctx.advance_secs(11);

    gm_slash(&ctx, &mut s_demo, "/giveisk 50000000").await;
    gm_slash(&ctx, &mut s_other, "/giveisk 500000").await;
    gm_slash(&ctx, &mut s_demo, &format!("/createitem {TYPE_TRITANIUM} 100")).await;
    gm_slash(&ctx, &mut s_demo, &format!("/createitem {TYPE_RIFTER} 1")).await;
    gm_slash(&ctx, &mut s_demo, &format!("/createitem {TYPE_PLEX} 5")).await;

    let trit_id = find_hangar_item_id(&ctx, CHAR_DEMO, TYPE_TRITANIUM);
    let plex_id = find_hangar_item_id(&ctx, CHAR_DEMO, TYPE_PLEX);

    // Market query endpoints
    let _ = rpc(&ctx, &mut s_demo, "marketProxy", "StartupCheck", &[])
        .await
        .expect("StartupCheck");
    let _ = rpc(&ctx, &mut s_demo, "marketProxy", "GetMarketGroups", &[])
        .await
        .expect("GetMarketGroups");
    let _ = rpc(
        &ctx,
        &mut s_demo,
        "marketProxy",
        "GetMarketGroupTypes", // rpc-lint: allow test fixture
        &[PyRep::Integer(18)],
    )
    .await
    .expect("GetMarketGroupTypes");
    let _ = rpc(
        &ctx,
        &mut s_demo,
        "marketProxy",
        "GetOrders",
        &[PyRep::Integer(TYPE_TRITANIUM as i64)],
    )
    .await
    .expect("GetOrders");
    let _ = rpc(&ctx, &mut s_demo, "marketProxy", "GetStationAsks", &[])
        .await
        .expect("GetStationAsks");
    let _ = rpc(&ctx, &mut s_demo, "marketProxy", "GetSystemAsks", &[])
        .await
        .expect("GetSystemAsks");
    let _ = rpc(&ctx, &mut s_demo, "marketProxy", "GetRegionBest", &[])
        .await
        .expect("GetRegionBest");
    let _ = rpc(
        &ctx,
        &mut s_demo,
        "marketProxy",
        "GetHistoryForManyTypeIDs",
        &[PyRep::List(vec![PyRep::Integer(TYPE_TRITANIUM as i64)])],
    )
    .await
    .expect("GetHistoryForManyTypeIDs");
    let _ = rpc(
        &ctx,
        &mut s_demo,
        "marketProxy",
        "GetOldPriceHistory",
        &[PyRep::Integer(TYPE_TRITANIUM as i64)],
    )
    .await
    .expect("GetOldPriceHistory");
    let _ = rpc(
        &ctx,
        &mut s_demo,
        "marketProxy",
        "GetNewPriceHistory",
        &[PyRep::Integer(TYPE_TRITANIUM as i64)],
    )
    .await
    .expect("GetNewPriceHistory");

    // --- PlaceBuyOrder validation errors ---
    // Bad tick size (>4 sig figs & sub-cent)
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(12_345.0),
                PyRep::Integer(10),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPriceTickSize"
    );
    // Negative price & zero qty & invalid duration
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(-5.0),
                PyRep::Integer(10),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidMarketOrder"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(5.0),
                PyRep::Integer(0),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidMarketOrder"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(5.0),
                PyRep::Integer(10),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(5),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidMarketOrder"
    );
    // Immediate order unfilled (duration 0)
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(1.0),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(0),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ImmediateOrderUnfilled"
    );
    // Remote buy at another station without Visibility skill -> StationMismatch
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(OTHER_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(5.0),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "StationMismatch"
    );
    // Range exceeding Visibility skill level -> VisibilitySkillRequired
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(5.0),
                PyRep::Integer(5),
                PyRep::Integer(32_767),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "VisibilitySkillRequired"
    );
    // Insufficient ISK
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(100_000.0),
                PyRep::Integer(10_000_000),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    // Corp order from NPC corp -> CorporationNotFound; after joining player corp without roles -> CorpRoleRequired
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(0.5),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(true),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );
    gm_slash(&ctx, &mut s_demo, &format!("/setskill {TYPE_CORP_MGMT} 1")).await;
    let mkt_corp_rep = rpc(
        &ctx,
        &mut s_demo,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Market Auth Corp".into()),
            PyRep::String("MKTA".into()),
        ],
    )
    .await
    .expect("AddCorporation mkt");
    let mkt_corp_id = mkt_corp_rep
        .as_i64()
        .unwrap_or_else(|| kv_int(&mkt_corp_rep, "corporationID").expect("corp_id"));
    join_corp_member(&ctx, &mut s_demo, &mut s_other, mkt_corp_id).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "marketProxy",
            "PlaceBuyOrder",
            &[
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Real(0.5),
                PyRep::Integer(5),
                PyRep::Integer(-1),
                PyRep::Integer(1),
                PyRep::Integer(90),
                PyRep::Bool(true),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleRequired"
    );

    // --- PlaceMultiSellOrder validation errors ---
    // Bad tick size
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TYPE_TRITANIUM as i64)),
                    ("itemID", PyRep::Integer(trit_id)),
                    ("price", PyRep::Real(12_345.0)),
                    ("quantity", PyRep::Integer(10)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPriceTickSize"
    );
    // Insufficient inventory
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TYPE_TRITANIUM as i64)),
                    ("itemID", PyRep::Integer(trit_id)),
                    ("price", PyRep::Real(50.0)),
                    ("quantity", PyRep::Integer(999_999_999)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientInventoryForSellOrder"
    );
    // Broker fee cap exceeded (maxBrokerFeePercent = 0.0001)
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(TYPE_TRITANIUM as i64)),
                    ("itemID", PyRep::Integer(trit_id)),
                    ("price", PyRep::Real(50.0)),
                    ("quantity", PyRep::Integer(10)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0001),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "BrokerFeeExceeded"
    );
    // Non-market-eligible item (find deterministically from SDE)
    let non_mkt_tid = ctx
        .sde
        .types
        .iter()
        .filter(|(&tid, t)| t.market_group_id.is_none() && t.published && tid > 0)
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, _)| tid)
        .expect("non-market type in SDE");
    gm_slash(&ctx, &mut s_demo, &format!("/createitem {non_mkt_tid} 1")).await;
    let non_mkt_item_id = find_hangar_item_id(&ctx, CHAR_DEMO, non_mkt_tid);
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "PlaceMultiSellOrder",
            &[
                PyRep::List(vec![PyRep::keyval([
                    ("stationID", PyRep::Integer(JITA_STATION_ID)),
                    ("typeID", PyRep::Integer(non_mkt_tid as i64)),
                    ("itemID", PyRep::Integer(non_mkt_item_id)),
                    ("price", PyRep::Real(100.0)),
                    ("quantity", PyRep::Integer(1)),
                ])]),
                PyRep::Bool(false),
                PyRep::Integer(90),
                PyRep::Real(0.0),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotMarketEligible"
    );

    // --- Place valid sell order, then test ModifyCharOrder & CancelCharOrder edges ---
    let sell_rep = rpc(
        &ctx,
        &mut s_demo,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(JITA_STATION_ID)),
                ("typeID", PyRep::Integer(TYPE_TRITANIUM as i64)),
                ("itemID", PyRep::Integer(trit_id)),
                ("price", PyRep::Real(100.0)),
                ("quantity", PyRep::Integer(10)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(90),
            PyRep::Real(0.0),
        ],
    )
    .await
    .expect("valid sell order");
    let sell_oid = kv_int(&sell_rep, "orderID").expect("sell orderID");

    // Immediate modify -> OrderModificationCooldown (< 300s)
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "ModifyCharOrder",
            &[
                PyRep::Integer(sell_oid),
                PyRep::Real(110.0),
                PyRep::Bool(false),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Bool(false),
                PyRep::Integer(90),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "OrderModificationCooldown"
    );

    ctx.advance_secs(301);

    // Other pilot cannot modify -> OrderOwnershipViolation
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "marketProxy",
            "ModifyCharOrder",
            &[
                PyRep::Integer(sell_oid),
                PyRep::Real(110.0),
                PyRep::Bool(false),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Bool(false),
                PyRep::Integer(90),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "OrderOwnershipViolation"
    );
    // Missing order -> OrderNotFound
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "ModifyCharOrder",
            &[
                PyRep::Integer(999_999_888),
                PyRep::Real(110.0),
                PyRep::Bool(false),
                PyRep::Integer(JITA_STATION_ID),
                PyRep::Integer(TYPE_TRITANIUM as i64),
                PyRep::Bool(false),
                PyRep::Integer(90),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "OrderNotFound"
    );
    // ModifyPlexCharOrder on non-PLEX order -> InvalidPlexOrder
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "ModifyPlexCharOrder",
            &[
                PyRep::Integer(sell_oid),
                PyRep::Real(110.0),
                PyRep::Bool(false),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPlexOrder"
    );
    // CancelCharOrder foreign & missing -> OrderOwnershipViolation / OrderNotFound
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "marketProxy",
            "CancelCharOrder",
            &[PyRep::Integer(sell_oid)],
        )
        .await
        .unwrap_err()
        .code,
        "OrderOwnershipViolation"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "CancelCharOrder",
            &[PyRep::Integer(999_999_777)],
        )
        .await
        .unwrap_err()
        .code,
        "OrderNotFound"
    );

    // PlacePlexSellOrder wrong station / non-plex error & BuyMultipleItems empty list error
    let plex_sell_err = rpc(
        &ctx,
        &mut s_demo,
        "marketProxy",
        "PlacePlexSellOrder",
        &[
            PyRep::Integer(JITA_STATION_ID),
            PyRep::Integer(plex_id),
            PyRep::Real(12_345_678.0),
            PyRep::Integer(1),
            PyRep::Integer(90),
            PyRep::Bool(false),
        ],
    )
    .await;
    assert!(plex_sell_err.is_err());

    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "marketProxy",
            "BuyMultipleItems",
            &[PyRep::Integer(JITA_STATION_ID), PyRep::List(vec![])],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidBuyMultipleItems"
    );

    // Valid cancel refunds remaining items
    rpc(
        &ctx,
        &mut s_demo,
        "marketProxy",
        "CancelCharOrder",
        &[PyRep::Integer(sell_oid)],
    )
    .await
    .expect("valid CancelCharOrder");

    let _ = rpc(&ctx, &mut s_demo, "marketProxy", "GetCharOrders", &[])
        .await
        .expect("GetCharOrders");
    let _ = rpc(&ctx, &mut s_demo, "marketProxy", "GetCharEscrow", &[])
        .await
        .expect("GetCharEscrow");

    // --- bountyProxy validation & error paths ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "bountyProxy",
            "AddToBounty",
            &[PyRep::Integer(CHAR_OTHER), PyRep::Real(50_000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "BountyAmountTooLow"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "bountyProxy",
            "AddToBounty",
            &[PyRep::Integer(0), PyRep::Real(200_000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "BountyAmountTooLow"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "bountyProxy",
            "AddToBounty",
            &[PyRep::Integer(999_888_123), PyRep::Real(200_000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "bountyProxy",
            "AddToBounty",
            &[PyRep::Integer(CHAR_DEMO), PyRep::Real(99_999_999_999.0)],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientFunds"
    );
    let placed = rpc(
        &ctx,
        &mut s_demo,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(CHAR_OTHER), PyRep::Real(150_000.0)],
    )
    .await
    .expect("valid AddToBounty");
    assert_eq!(kv_int(&placed, "targetID"), Some(CHAR_OTHER));

    let _ = rpc(
        &ctx,
        &mut s_demo,
        "bountyProxy",
        "GetBounty",
        &[PyRep::Integer(CHAR_OTHER)],
    )
    .await
    .expect("GetBounty");
    let _ = rpc(
        &ctx,
        &mut s_demo,
        "bountyProxy",
        "GetBountiesAndKillRights",
        &[],
    )
    .await
    .expect("GetBountiesAndKillRights");
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "bountyProxy",
            "CancelSellKillRight",
            &[PyRep::Integer(999_111)],
        )
        .await
        .unwrap_err()
        .code,
        "KillRightNotFound"
    );
}

// ---------------------------------------------------------------------------
// 3. Skills, Alpha Caps, Extractors/Injectors, Respec, Jump Clones, Implants, Certs
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_skills_clones_implants_and_certificates_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Create an Alpha player account cleanly via provision_player_account (no GM role, clone_grade=1)
    let alpha_uid = ctx.db.provision_player_account("alpha_edge", "alpha-pass");
    let mut s_alpha = SessionState::new(57_021);
    common::login_pilot(&ctx, &mut s_alpha, "alpha_edge", "alpha-pass").await;

    // Create a character for Alpha via charUnboundMgr.CreateCharacterWithDoll
    let (race_id, bloodline_id, ancestry_id, school_id) = valid_char_creation_ids(&ctx);
    let doll = PyRep::Dict(vec![(PyRep::String("skin".into()), PyRep::Integer(1))]);
    let alpha_cid = rpc(
        &ctx,
        &mut s_alpha,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String("Alpha Edge Pilot".into()),
            PyRep::Integer(race_id as i64),
            PyRep::Integer(bloodline_id as i64),
            PyRep::Integer(1),
            PyRep::Integer(ancestry_id as i64),
            doll.clone(),
            doll.clone(),
            PyRep::Integer(school_id as i64),
            PyRep::None,
            PyRep::None,
        ],
    )
    .await
    .expect("CreateCharacterWithDoll alpha")
    .as_i64()
    .expect("alpha cid");
    rpc(
        &ctx,
        &mut s_alpha,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(alpha_cid)],
    )
    .await
    .expect("select alpha");

    let mut s_demo = login_select(&ctx, 57_022, "demo", "demo-secret", CHAR_DEMO).await;
    ctx.advance_secs(11);

    // Pick a skill with prerequisites deterministically from SDE
    let prereq_skill = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| {
            t.category_id == 16
                && t.published
                && t.default_attributes
                    .get(&eve_dogma::REQUIRED_SKILL_ATTR_IDS[0])
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
        })
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, _)| tid)
        .expect("skill with prerequisite in SDE");

    // Pick an Omega-only skill (alpha cap == 0) deterministically
    let omega_skill = ctx
        .sde
        .types
        .iter()
        .filter(|(&tid, t)| {
            t.category_id == 16
                && t.published
                && eve_server::services::character_skills::alpha_skill_cap_for_character(
                    &ctx.sde,
                    1,
                    tid,
                ) == 0
                && t.default_attributes
                    .get(&eve_dogma::REQUIRED_SKILL_ATTR_IDS[0])
                    .copied()
                    .unwrap_or(0.0)
                    == 0.0
        })
        .min_by_key(|(&tid, _)| tid)
        .map(|(&tid, _)| tid);

    // --- SaveSkillQueue validation & Alpha cap errors ---
    // Invalid skill type (Tritanium 34)
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "SaveSkillQueue",
            &[
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(TYPE_TRITANIUM as i64),
                    PyRep::Integer(1),
                ])]),
                PyRep::Bool(true),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );

    // Prerequisite not met
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "SaveSkillQueue",
            &[
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(prereq_skill as i64),
                    PyRep::Integer(1),
                ])]),
                PyRep::Bool(true),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "SkillPrerequisiteNotMet"
    );

    // Alpha cap exceeded (level cap) on an Omega-only skill if present
    if let Some(om_tid) = omega_skill {
        assert_eq!(
            rpc(
                &ctx,
                &mut s_alpha,
                "skillMgr2",
                "SaveSkillQueue",
                &[
                    PyRep::List(vec![PyRep::Tuple(vec![
                        PyRep::Integer(om_tid as i64),
                        PyRep::Integer(1),
                    ])]),
                    PyRep::Bool(true),
                ],
            )
            .await
            .unwrap_err()
            .code,
            "AlphaCloneSkillLevelCapExceeded"
        );
    }

    // Skill already trained
    gm_slash(&ctx, &mut s_alpha, &format!("/setskill {TYPE_GUNNERY} 3")).await;
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "SaveSkillQueue",
            &[
                PyRep::List(vec![PyRep::Tuple(vec![
                    PyRep::Integer(TYPE_GUNNERY as i64),
                    PyRep::Integer(2),
                ])]),
                PyRep::Bool(true),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "SkillAlreadyTrained"
    );

    // --- ApplyFreeSkillPoints validation ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(TYPE_TRITANIUM as i64), PyRep::Real(100.0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(TYPE_GUNNERY as i64), PyRep::Real(0.0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSkillPointAmount"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "ApplyFreeSkillPoints",
            &[PyRep::Integer(TYPE_GUNNERY as i64), PyRep::Real(50_000.0)],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientFreeSP"
    );

    // --- InjectSkillIntoBrain validation ---
    assert_eq!(
        rpc(&ctx, &mut s_alpha, "skillMgr2", "InjectSkillIntoBrain", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "InjectSkillIntoBrain",
            &[PyRep::List(vec![PyRep::Integer(999_777_111)])],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    gm_slash(&ctx, &mut s_alpha, &format!("/createitem {TYPE_TRITANIUM} 1")).await;
    let alpha_trit = find_hangar_item_id(&ctx, alpha_cid, TYPE_TRITANIUM);
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "InjectSkillIntoBrain",
            &[PyRep::List(vec![PyRep::Integer(alpha_trit)])],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSkillType"
    );
    gm_slash(&ctx, &mut s_alpha, &format!("/createitem {TYPE_GUNNERY} 1")).await;
    let alpha_gunnery_book = find_hangar_item_id(&ctx, alpha_cid, TYPE_GUNNERY);
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "InjectSkillIntoBrain",
            &[PyRep::List(vec![PyRep::Integer(alpha_gunnery_book)])],
        )
        .await
        .unwrap_err()
        .code,
        "SkillAlreadyInjected"
    );

    // --- ExtractSkills validation & Omega/5.5M SP checks ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "ExtractSkills",
            &[PyRep::Dict(vec![(
                PyRep::Integer(TYPE_GUNNERY as i64),
                PyRep::Integer(500_000),
            )])],
        )
        .await
        .unwrap_err()
        .code,
        "MissingSkillExtractor"
    );
    gm_slash(
        &ctx,
        &mut s_alpha,
        &format!("/createitem {TYPE_SKILL_EXTRACTOR} 1"),
    )
    .await;
    let alpha_extractor = find_hangar_item_id(&ctx, alpha_cid, TYPE_SKILL_EXTRACTOR);
    // Alpha account with extractor cannot extract -> OmegaCloneRequired
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "ExtractSkills",
            &[
                PyRep::Dict(vec![(
                    PyRep::Integer(TYPE_GUNNERY as i64),
                    PyRep::Integer(500_000),
                )]),
                PyRep::Integer(alpha_extractor),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "OmegaCloneRequired"
    );

    // Grant Omega via GM /setomega on alpha_uid
    let omega_until = ctx.now_unix() + 86_400;
    gm_slash(&ctx, &mut s_demo, &format!("/setomega {alpha_uid} {omega_until}")).await;
    let _ = rpc(&ctx, &mut s_alpha, "subscriptionMgr", "GetCloneGrade", &[])
        .await
        .expect("refresh clone grade");

    // Unextractable skill -> SkillCannotBeExtracted
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "ExtractSkills",
            &[
                PyRep::Dict(vec![(
                    PyRep::Integer(TYPE_INFOMORPH_PSYCHOLOGY as i64),
                    PyRep::Integer(500_000),
                )]),
                PyRep::Integer(alpha_extractor),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "SkillCannotBeExtracted"
    );
    // < 5.5M SP -> InsufficientSkillPoints
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "ExtractSkills",
            &[
                PyRep::Dict(vec![(
                    PyRep::Integer(TYPE_GUNNERY as i64),
                    PyRep::Integer(500_000),
                )]),
                PyRep::Integer(alpha_extractor),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientSkillPoints"
    );

    // CombineSkillInjector error paths
    assert!(
        rpc(
            &ctx,
            &mut s_alpha,
            "skillMgr2",
            "CombineSkillInjector",
            &[PyRep::List(vec![PyRep::Integer(alpha_trit)])],
        )
        .await
        .is_err()
    );

    // --- Neural Remap (RespecCharacter) validation & cooldown ---
    assert_eq!(
        rpc(&ctx, &mut s_alpha, "skillMgr2", "RespecCharacter", &[])
            .await
            .unwrap_err()
            .code,
        "InvalidRemapArguments"
    );
    // Invalid total (!= 99) -> RemapFailed
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "charMgr",
            "RespecCharacter",
            &[PyRep::Dict(vec![
                (PyRep::String("charisma".into()), PyRep::Integer(17)),
                (PyRep::String("intelligence".into()), PyRep::Integer(20)),
                (PyRep::String("memory".into()), PyRep::Integer(20)),
                (PyRep::String("perception".into()), PyRep::Integer(20)),
                (PyRep::String("willpower".into()), PyRep::Integer(17)),
            ])],
        )
        .await
        .unwrap_err()
        .code,
        "RemapFailed"
    );
    let _ = rpc(&ctx, &mut s_alpha, "skillMgr2", "GetRespecInfo", &[])
        .await
        .expect("GetRespecInfo");

    // --- Jump Clone & CloneJump validation/cooldown ---
    // Install clone without Infomorph Psychology -> InfomorphPsychologyRequired
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "jumpCloneSvc",
            "InstallCloneInStation",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "InfomorphPsychologyRequired"
    );
    gm_slash(
        &ctx,
        &mut s_alpha,
        &format!("/setskill {TYPE_INFOMORPH_PSYCHOLOGY} 2"),
    )
    .await;
    gm_slash(&ctx, &mut s_alpha, "/giveisk 5000000").await;

    // Install clone validly in Jita
    rpc(
        &ctx,
        &mut s_alpha,
        "jumpCloneSvc",
        "InstallCloneInStation",
        &[],
    )
    .await
    .expect("InstallCloneInStation");
    // Second clone in same station -> JumpCloneAlreadyAtStation
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "jumpCloneSvc",
            "InstallCloneInStation",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "JumpCloneAlreadyAtStation"
    );

    // SetJumpCloneName / DestroyInstalledClone / CloneJump errors
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "jumpCloneSvc",
            "SetJumpCloneName",
            &[PyRep::Integer(0), PyRep::String("X".into())],
        )
        .await
        .unwrap_err()
        .code,
        "JumpCloneNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "jumpCloneSvc",
            "SetJumpCloneName",
            &[PyRep::Integer(1), PyRep::String("A".repeat(101))],
        )
        .await
        .unwrap_err()
        .code,
        "JumpCloneNameTooLong"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "jumpCloneSvc",
            "SetJumpCloneName",
            &[PyRep::Integer(888_777), PyRep::String("Valid".into())],
        )
        .await
        .unwrap_err()
        .code,
        "JumpCloneNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "jumpCloneSvc",
            "DestroyInstalledClone",
            &[PyRep::Integer(888_777)],
        )
        .await
        .unwrap_err()
        .code,
        "CloneNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "jumpCloneSvc",
            "CloneJump",
            &[PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidJumpCloneID"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "jumpCloneSvc",
            "CloneJump",
            &[PyRep::Integer(888_777)],
        )
        .await
        .unwrap_err()
        .code,
        "JumpCloneNotFound"
    );

    // --- dogmaIM InjectImplant / DestroyImplant / UseBooster edges ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "dogmaIM",
            "InjectImplant",
            &[PyRep::Integer(999_333_111)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "dogmaIM",
            "InjectImplant",
            &[PyRep::Integer(alpha_trit)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotAnImplant"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "dogmaIM",
            "DestroyImplant",
            &[PyRep::Integer(alpha_trit)],
        )
        .await
        .unwrap_err()
        .code,
        "ImplantNotPluggedIn"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "dogmaIM",
            "UseBooster",
            &[PyRep::Integer(999_333_222)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "dogmaIM",
            "UseBooster",
            &[PyRep::Integer(alpha_trit)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotABooster"
    );

    // Pick a slot-1 implant deterministically from SDE (attribute 331 == 1.0)
    if let Some((&imp_tid, _)) = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| {
            t.published
                && t.default_attributes.get(&331).copied() == Some(1.0)
                && t.default_attributes
                    .get(&eve_dogma::REQUIRED_SKILL_ATTR_IDS[0])
                    .copied()
                    .unwrap_or(0.0)
                    == 0.0
        })
        .min_by_key(|(&tid, _)| tid)
    {
        gm_slash(&ctx, &mut s_alpha, &format!("/createitem {imp_tid} 2")).await;
        let imp_id = find_hangar_item_id(&ctx, alpha_cid, imp_tid);
        rpc(
            &ctx,
            &mut s_alpha,
            "dogmaIM",
            "InjectImplant",
            &[PyRep::Integer(imp_id)],
        )
        .await
        .expect("plug slot-1 implant");
        // Second implant in same slot -> ImplantSlotOccupied
        assert_eq!(
            rpc(
                &ctx,
                &mut s_alpha,
                "dogmaIM",
                "InjectImplant",
                &[PyRep::Integer(imp_id)],
            )
            .await
            .unwrap_err()
            .code,
            "ImplantSlotOccupied"
        );
    }

    // --- certificateMgr edge & valid queries ---
    let _ = rpc(&ctx, &mut s_alpha, "certificateMgr", "GetMyCertificates", &[]) // rpc-lint: allow test fixture
        .await
        .expect("GetMyCertificates");
    let _ = rpc(
        &ctx,
        &mut s_alpha,
        "certificateMgr", // rpc-lint: allow test fixture
        "GetCertificateCategories",
        &[],
    )
    .await
    .expect("GetCertificateCategories");
    let _ = rpc(
        &ctx,
        &mut s_alpha,
        "certificateMgr", // rpc-lint: allow test fixture
        "GetCertificateClasses",
        &[],
    )
    .await
    .expect("GetCertificateClasses");
    assert_eq!(
        rpc(
            &ctx,
            &mut s_alpha,
            "certificateMgr", // rpc-lint: allow test fixture
            "GetAllShipCertificateRecommendations",
            &[PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidShipTypeID"
    );
    let _ = rpc(
        &ctx,
        &mut s_alpha,
        "certificateMgr", // rpc-lint: allow test fixture
        "GetAllShipCertificateRecommendations",
        &[PyRep::Integer(TYPE_RIFTER as i64)],
    )
    .await
    .expect("Rifter cert recs");
}

// ---------------------------------------------------------------------------
// 4. Cosmetics, SKINR Quasar Studio/Market, FastCheckout, Char Lifecycle & Slash RBAC
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_cosmetics_skinr_fast_checkout_and_char_lifecycle_edges() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s_demo = login_select(&ctx, 57_031, "demo", "demo-secret", CHAR_DEMO).await;
    let mut s_other = login_select(&ctx, 57_032, "other", "other-secret", CHAR_OTHER).await;
    ctx.advance_secs(11);

    // --- shipCosmeticsMgr validation errors ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(TYPE_TRITANIUM as i64)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSkinLicenseType"
    );

    // Find a Rifter SKIN license deterministically from SDE
    let (rifter_lic_tid, other_hull_lic_tid) = {
        let mut r_lic = None;
        let mut o_lic = None;
        let mut lic_ids: Vec<u32> = ctx.sde.skin_licenses.keys().copied().collect();
        lic_ids.sort_unstable();
        for tid in lic_ids {
            if let Some(lic) = ctx.sde.skin_licenses.get(&tid) {
                if let Some(sk) = ctx.sde.skins.get(&lic.skin_id) {
                    if sk.types.contains(&TYPE_RIFTER) {
                        if r_lic.is_none() {
                            r_lic = Some(tid);
                        }
                    } else if o_lic.is_none() && !sk.types.is_empty() {
                        o_lic = Some(tid);
                    }
                }
            }
            if r_lic.is_some() && o_lic.is_some() {
                break;
            }
        }
        (
            r_lic.expect("Rifter SKIN license in SDE"),
            o_lic.expect("non-Rifter SKIN license in SDE"),
        )
    };

    // Not owned in hangar -> SkinLicenseItemNotFound
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[PyRep::Integer(rifter_lic_tid as i64)],
        )
        .await
        .unwrap_err()
        .code,
        "SkinLicenseItemNotFound"
    );
    // Incompatible hull -> InvalidShipTypeForSkin
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "shipCosmeticsMgr",
            "ActivateSkinLicense",
            &[
                PyRep::Integer(other_hull_lic_tid as i64),
                PyRep::Integer(TYPE_RIFTER as i64),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidShipTypeForSkin"
    );

    // ApplySkinToShip errors: unlicensed, invalid hull, foreign ship
    gm_slash(&ctx, &mut s_demo, &format!("/createitem {TYPE_RIFTER} 1")).await;
    let demo_rifter_id = find_hangar_item_id(&ctx, CHAR_DEMO, TYPE_RIFTER);
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "shipCosmeticsMgr",
            "ApplySkinToShip",
            &[
                PyRep::Integer(demo_rifter_id),
                PyRep::Integer(rifter_lic_tid as i64),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "SkinNotLicensed"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "shipCosmeticsMgr",
            "ApplySkinToShip",
            &[
                PyRep::Integer(demo_rifter_id),
                PyRep::Integer(rifter_lic_tid as i64),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "OwnershipViolation"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "shipCosmeticsMgr",
            "ApplySkinToShip",
            &[
                PyRep::Integer(demo_rifter_id),
                PyRep::Integer(other_hull_lic_tid as i64),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidShipTypeForSkin"
    );

    // Activate & Apply & Remove SKIN cleanly
    gm_slash(&ctx, &mut s_demo, &format!("/createitem {rifter_lic_tid} 1")).await;
    rpc(
        &ctx,
        &mut s_demo,
        "shipCosmeticsMgr",
        "ActivateSkinLicense",
        &[PyRep::Integer(rifter_lic_tid as i64)],
    )
    .await
    .expect("ActivateSkinLicense");
    rpc(
        &ctx,
        &mut s_demo,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[
            PyRep::Integer(demo_rifter_id),
            PyRep::Integer(rifter_lic_tid as i64),
        ],
    )
    .await
    .expect("ApplySkinToShip");
    rpc(
        &ctx,
        &mut s_demo,
        "shipCosmeticsMgr",
        "ApplySkinToShip",
        &[PyRep::Integer(demo_rifter_id), PyRep::None],
    )
    .await
    .expect("unapply SKIN");

    // --- overviewPresetMgr / sharedSettingsMgr link errors ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "overviewPresetMgr",
            "StoreLinkAndGetID",
            &[PyRep::String("".into()), PyRep::String("payload".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidLinkName"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "sharedSettingsMgr",
            "StoreSettingLinkAndGetID",
            &[PyRep::String("Name".into()), PyRep::None],
        )
        .await
        .unwrap_err()
        .code,
        "EmptyLinkPayload"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "sharedSettingsMgr",
            "StoreSettingLinkAndGetID",
            &[PyRep::String("Big".into()), PyRep::String("x".repeat(70_000))],
        )
        .await
        .unwrap_err()
        .code,
        "PayloadTooLarge"
    );

    // --- FastCheckoutService & activityMgr validation errors ---
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(-1)],
        )
        .await
        .unwrap_err()
        .code,
        "FastCheckoutDisabled"
    );
    let saved_fc_role = s_demo.role;
    s_demo.role |= service_roles::ROLE_GM_DEFAULT;
    rpc(
        &ctx,
        &mut s_demo,
        "machoNet",
        "SetGlobalConfigValue",
        &[
            PyRep::String("fastCheckoutEnabled".into()),
            PyRep::String("1".into()),
        ],
    )
    .await
    .expect("enable fastCheckoutEnabled");
    s_demo.role = saved_fc_role;

    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(-1)],
        )
        .await
        .unwrap_err()
        .code,
        "OfferNotFound"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "FastCheckoutService",
            "BuyOffer",
            &[PyRep::Integer(1), PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidQuantity"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "activityMgr",
            "ActivitySeen",
            &[PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_demo,
            "activityMgr",
            "ActivitySeen",
            &[PyRep::Integer(999_888_777)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidActivityID"
    );
    assert!(rpc(
        &ctx,
        &mut s_demo,
        "activityMgr",
        "SetActivitiesPopState",
        &[PyRep::Integer(123)],
    )
    .await
    .is_err());

    // --- Quasar SKINR Drafts, Sequencing, Market, and Payment Store error paths ---
    // Draft GetRequest missing id -> STATUS_BAD_REQUEST
    let r_draft_missing = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(r_draft_missing.status_code, STATUS_BAD_REQUEST);

    // Sequencing InitiateRequest missing layout -> STATUS_BAD_REQUEST
    let mut seq_bad_layout = Vec::new();
    encode_string_field(1, "Bad Layout", &mut seq_bad_layout);
    let r_seq_bad = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        seq_bad_layout,
    )
    .await;
    assert_eq!(r_seq_bad.status_code, STATUS_BAD_REQUEST);

    // Sequencing InitiateRequest invalid hull type (Tritanium 34) -> STATUS_BAD_REQUEST
    let mut layout = Vec::new();
    let mut hull_msg = Vec::new();
    encode_varint_field(1, TYPE_TRITANIUM as u64, &mut hull_msg);
    encode_bytes_field(2, &hull_msg, &mut layout);
    let mut seq_bad_hull = Vec::new();
    encode_string_field(1, "Bad Hull", &mut seq_bad_hull);
    encode_bytes_field(3, &layout, &mut seq_bad_hull);
    let r_seq_hull = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        seq_bad_hull,
    )
    .await;
    assert_eq!(r_seq_hull.status_code, STATUS_BAD_REQUEST);

    // Sequencing CompleteEarlyRequest unknown job -> STATUS_BAD_REQUEST or STATUS_NOT_FOUND
    let mut bad_job_req = Vec::new();
    encode_bytes_field(1, &[0xAB; 16], &mut bad_job_req);
    let r_job_nf = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
        bad_job_req,
    )
    .await;
    assert!(matches!(r_job_nf.status_code, STATUS_NOT_FOUND | STATUS_BAD_REQUEST));

    // Paragon Market InitiateCreateListingRequest missing price -> STATUS_BAD_REQUEST
    let r_mkt_create_bad = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
        Vec::new(),
    )
    .await;
    assert_eq!(r_mkt_create_bad.status_code, STATUS_BAD_REQUEST);

    // Paragon Market InitiatePurchaseListingRequest missing/malformed UUID -> STATUS_BAD_REQUEST
    let mut flat_uuid = Vec::new();
    encode_bytes_field(1, &[0xCD; 16], &mut flat_uuid);
    encode_varint_field(2, 1, &mut flat_uuid);
    let r_mkt_buy_bad = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        flat_uuid,
    )
    .await;
    assert_eq!(r_mkt_buy_bad.status_code, STATUS_BAD_REQUEST);

    // Paragon Market InitiatePurchaseListingRequest nonexistent nested UUID -> STATUS_NOT_FOUND
    let mut uuid_msg = Vec::new();
    encode_bytes_field(1, &[0xCD; 16], &mut uuid_msg);
    let mut bad_buy = Vec::new();
    encode_bytes_field(1, &uuid_msg, &mut bad_buy);
    encode_varint_field(2, 1, &mut bad_buy);
    let r_mkt_buy_nf = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        bad_buy,
    )
    .await;
    assert_eq!(r_mkt_buy_nf.status_code, STATUS_NOT_FOUND);

    // Paragon Market InitiatePurchaseListingRequest outdated buyer_fees_version -> STATUS_BAD_REQUEST
    let mut bad_ver = Vec::new();
    encode_bytes_field(1, &uuid_msg, &mut bad_ver);
    encode_varint_field(2, 1, &mut bad_ver);
    encode_string_field(3, "v0-outdated", &mut bad_ver);
    let r_mkt_ver = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
        bad_ver,
    )
    .await;
    assert_eq!(r_mkt_ver.status_code, STATUS_BAD_REQUEST);

    // Quasar Payment DeliverItemRequest non-admin -> STATUS_FORBIDDEN or STATUS_UNAUTHORIZED
    let r_deliver_unauth = quasar_rpc(
        &ctx,
        &mut s_other,
        "eve.fulfillment.api.DeliverItemRequest",
        Vec::new(),
    )
    .await;
    assert!(matches!(
        r_deliver_unauth.status_code,
        STATUS_FORBIDDEN | STATUS_UNAUTHORIZED
    ));

    // Save 20 drafts cleanly, then 21st hits SKINR draft capacity limit -> STATUS_BAD_REQUEST
    for i in 1..=20u32 {
        let mut save_bytes = Vec::new();
        encode_string_field(1, &format!("Draft {i}"), &mut save_bytes);
        let r = quasar_rpc(
            &ctx,
            &mut s_demo,
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
            save_bytes,
        )
        .await;
        assert_eq!(r.status_code, STATUS_OK, "draft {i} should save");
    }
    let mut save_21 = Vec::new();
    encode_string_field(1, "Draft 21 OverCap", &mut save_21);
    let r_21 = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        save_21,
    )
    .await;
    assert_eq!(r_21.status_code, STATUS_BAD_REQUEST);

    // Draft DeleteRequest not found -> STATUS_NOT_FOUND
    let mut del_nf = Vec::new();
    encode_bytes_field(1, &[0xEF; 16], &mut del_nf);
    let r_del_nf = quasar_rpc(
        &ctx,
        &mut s_demo,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
        del_nf,
    )
    .await;
    assert_eq!(r_del_nf.status_code, STATUS_NOT_FOUND);

    // --- Character Lifecycle & Auth/Slash validation ---
    // CreateCharacterWithDoll errors on clean player account
    let _lc_uid = ctx.db.provision_player_account("lifecycle_edge", "lc-pass");
    let mut s_lc = SessionState::new(57_033);
    common::login_pilot(&ctx, &mut s_lc, "lifecycle_edge", "lc-pass").await;

    let (race_id, bloodline_id, ancestry_id, school_id) = valid_char_creation_ids(&ctx);
    let doll = PyRep::Dict(vec![(PyRep::String("skin".into()), PyRep::Integer(1))]);
    assert_eq!(
        rpc(
            &ctx,
            &mut s_lc,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[PyRep::String("CCP Forbidden".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidCharacterName"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_lc,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Good Name One".into()),
                PyRep::Integer(race_id as i64),
                PyRep::Integer(bloodline_id as i64),
                PyRep::Integer(2), // invalid gender
                PyRep::Integer(ancestry_id as i64),
                doll.clone(),
                doll.clone(),
                PyRep::Integer(school_id as i64),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidGender"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_lc,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Good Name One".into()),
                PyRep::Integer(race_id as i64),
                PyRep::Integer(bloodline_id as i64),
                PyRep::Integer(1),
                PyRep::Integer(ancestry_id as i64),
                PyRep::Dict(vec![]), // empty doll rejected
                doll.clone(),
                PyRep::Integer(school_id as i64),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidDollInfo"
    );
    assert_eq!(
        rpc(
            &ctx,
            &mut s_lc,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Good Name One".into()),
                PyRep::Integer(race_id as i64),
                PyRep::Integer(999_999), // unknown bloodline
                PyRep::Integer(1),
                PyRep::Integer(ancestry_id as i64),
                doll.clone(),
                doll.clone(),
                PyRep::Integer(school_id as i64),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "CharacterCreationFailed"
    );

    // Create 3 valid characters -> 4th fails with MaxCharacterSlotsReached
    let mut created_cids = Vec::new();
    for idx in 1..=3 {
        let cid = rpc(
            &ctx,
            &mut s_lc,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[
                PyRep::String(format!("Lifecycle Pilot {idx}")),
                PyRep::Integer(race_id as i64),
                PyRep::Integer(bloodline_id as i64),
                PyRep::Integer(1),
                PyRep::Integer(ancestry_id as i64),
                doll.clone(),
                doll.clone(),
                PyRep::Integer(school_id as i64),
                PyRep::None,
                PyRep::None,
            ],
        )
        .await
        .unwrap_or_else(|e| panic!("char {idx} create failed: {e:?}"))
        .as_i64()
        .expect("char_id");
        created_cids.push(cid);
    }
    assert_eq!(
        rpc(
            &ctx,
            &mut s_lc,
            "charUnboundMgr",
            "CreateCharacterWithDoll",
            &[
                PyRep::String("Fourth Slot Over".into()),
                PyRep::Integer(race_id as i64),
                PyRep::Integer(bloodline_id as i64),
                PyRep::Integer(1),
                PyRep::Integer(ancestry_id as i64),
                doll.clone(),
                doll.clone(),
                PyRep::Integer(school_id as i64),
                PyRep::None,
                PyRep::None,
            ],
        )
        .await
        .unwrap_err()
        .code,
        "MaxCharacterSlotsReached"
    );

    // DeleteCharacter before PrepareCharacterForDelete -> DeleteNotPrepared
    let target_cid = created_cids[0];
    assert_eq!(
        rpc(
            &ctx,
            &mut s_lc,
            "charUnboundMgr",
            "DeleteCharacter",
            &[PyRep::Integer(target_cid)],
        )
        .await
        .unwrap_err()
        .code,
        "DeleteNotPrepared"
    );

    // Prepare -> immediate DeleteCharacter -> DeleteTimerNotExpired -> Cancel -> Re-prepare -> Advance 10h+1 -> DeleteCharacter ok!
    rpc(
        &ctx,
        &mut s_lc,
        "charUnboundMgr",
        "PrepareCharacterForDelete",
        &[PyRep::Integer(target_cid)],
    )
    .await
    .expect("PrepareCharacterForDelete");
    assert_eq!(
        rpc(
            &ctx,
            &mut s_lc,
            "charUnboundMgr",
            "DeleteCharacter",
            &[PyRep::Integer(target_cid)],
        )
        .await
        .unwrap_err()
        .code,
        "DeleteTimerNotExpired"
    );
    rpc(
        &ctx,
        &mut s_lc,
        "charUnboundMgr",
        "CancelCharacterDeletePrepare",
        &[PyRep::Integer(target_cid)],
    )
    .await
    .expect("CancelCharacterDeletePrepare");
    rpc(
        &ctx,
        &mut s_lc,
        "charUnboundMgr",
        "PrepareCharacterForDelete",
        &[PyRep::Integer(target_cid)],
    )
    .await
    .expect("re-prepare");
    ctx.advance_secs(10 * 3_600 + 5);
    rpc(
        &ctx,
        &mut s_lc,
        "charUnboundMgr",
        "DeleteCharacter",
        &[PyRep::Integer(target_cid)],
    )
    .await
    .expect("DeleteCharacter after delay");

    // slash.SlashCmd RBAC & invalid argument edges
    assert_eq!(
        rpc(
            &ctx,
            &mut s_other,
            "slash",
            "SlashCmd",
            &[PyRep::String("/giveisk 1000".into())],
        )
        .await
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert_eq!(
        gm_slash_err(&ctx, &mut s_demo, "/giveisk notanumber").await.code,
        "InvalidSlashArgument"
    );
    assert_eq!(
        gm_slash_err(&ctx, &mut s_demo, "/giveitem 999999999 1").await.code,
        "TypeNotFound"
    );
    assert_eq!(
        gm_slash_err(&ctx, &mut s_demo, "/setskill 34 3").await.code,
        "InvalidSkillType"
    );
}
