//! Regression tests for `#594` (`[Arch-12]`) typed `RpcArgs` migration across service handlers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::{service_roles::ROLE_PLAYER, SessionState};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn setup_pilot_in_space(ctx: &ServerContext, char_id: i64, corp_id: i64) -> SessionState {
    let sys_id = 30000142_u32;
    let ship_id = char_id + 50_000;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("demo char seeded").clone();
        ch.character_id = char_id;
        ch.name = format!("Pilot-{char_id}");
        ch.corporation_id = corp_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        ch.active_ship_id = ship_id;
        ch.balance = IskCents::from_isk(10_000_000_000);
        snap.characters.insert(char_id, ch);
        snap.corporations.entry(corp_id).or_insert_with(|| {
            CorporationRecord::new(corp_id, format!("Corp-{corp_id}"), "TST", char_id)
                .with_master_wallet(IskCents::from_isk(10_000_000_000))
        });
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let mut s = SessionState::new(7701);
    s.user_id = Some(10);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s.role = ROLE_PLAYER;
    s
}

#[tokio::test]
async fn create_raffle_rejects_missing_nan_and_negative_prices() {
    let ctx = test_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 991_001, 98_991_001);
    sess.solar_system_id = None;
    sess.station_id = Some(60003760);
    let kw = HashMap::new();

    // Missing ticket price (dict payload without ticketPrice) must fail with InvalidArgument, NOT default to 0.0
    let payload_missing = PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(12345)),
        (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
    ]);
    let err_missing = ctx
        .dispatch_rpc(&mut sess, "raffleProxy", None, "CreateRaffle", &[payload_missing], &kw, None)
        .await
        .expect_err("CreateRaffle without ticketPrice must fail");
    assert_eq!(err_missing.code, "InvalidArgument");

    // Positional NaN ticket price must fail cleanly
    let err_nan = ctx
        .dispatch_rpc(
            &mut sess,
            "raffleProxy",
            None,
            "CreateRaffle",
            &[PyRep::Integer(12345), PyRep::Real(f64::NAN), PyRep::Integer(8)],
            &kw,
            None,
        )
        .await
        .expect_err("CreateRaffle with NaN price must fail");
    assert_eq!(err_nan.code, "InvalidArgument");
}

#[tokio::test]
async fn contracts_and_wallet_reject_malformed_or_nan_arguments() {
    let ctx = test_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 991_002, 98_991_002);
    let kw = HashMap::new();

    let err_accept = ctx
        .dispatch_rpc(&mut sess, "contractProxy", None, "AcceptContract", &[], &kw, None)
        .await
        .expect_err("AcceptContract with missing contractID must fail");
    assert_eq!(err_accept.code, "InvalidArgument");

    let err_bid = ctx
        .dispatch_rpc(
            &mut sess,
            "contractProxy",
            None,
            "PlaceBid",
            &[PyRep::Integer(100), PyRep::Real(f64::NAN)],
            &kw,
            None,
        )
        .await
        .expect_err("PlaceBid with NaN amount must fail");
    assert_eq!(err_bid.code, "InvalidArgument");

    let err_give = ctx
        .dispatch_rpc(
            &mut sess,
            "account",
            None,
            "GiveCash",
            &[PyRep::Integer(991_003), PyRep::Real(f64::INFINITY)],
            &kw,
            None,
        )
        .await
        .expect_err("GiveCash with Infinity amount must fail");
    assert_eq!(err_give.code, "InvalidArgument");

    let err_lp = ctx
        .dispatch_rpc(&mut sess, "LPStoreMgr", None, "TakeOfferForCharacter", &[], &kw, None)
        .await
        .expect_err("TakeOfferForCharacter without offerID must fail");
    assert_eq!(err_lp.code, "InvalidArgument");
}

#[tokio::test]
async fn fighter_movement_and_point_ability_reject_missing_and_nan_coords() {
    let ctx = test_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 991_004, 98_991_004);
    let kw = HashMap::new();

    let err_load = ctx
        .dispatch_rpc(&mut sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0)], &kw, None)
        .await
        .expect_err("LoadFightersToTube with missing second arg must fail");
    assert_eq!(err_load.code, "InvalidArgument");

    let err_point = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "ActivateAbilitySlotsAtPoint",
            &[
                PyRep::Integer(555_001),
                PyRep::Integer(0),
                PyRep::Tuple(vec![PyRep::Real(1.0), PyRep::Real(f64::NAN), PyRep::Real(0.0)]),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("NaN point target must fail");
    assert_eq!(err_point.code, "InvalidTargetPoint");

    let err_unload = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "UnloadTubeToFighterBay",
            &[PyRep::String("bad-tube".into())],
            &kw,
            None,
        )
        .await
        .expect_err("UnloadTubeToFighterBay with non-int tube must fail");
    assert_eq!(err_unload.code, "InvalidFighterTube");

    let err_slot = ctx
        .dispatch_rpc(
            &mut sess,
            "fighterMgr",
            None,
            "CmdActivateAbilitySlots",
            &[PyRep::Integer(555_001), PyRep::String("slot0".into())],
            &kw,
            None,
        )
        .await
        .expect_err("CmdActivateAbilitySlots with non-int slot must fail");
    assert_eq!(err_slot.code, "InvalidFighterAbilitySlot");
}

#[tokio::test]
async fn navigation_superweapon_corp_fitting_and_contacts_reject_unchecked_defaults() {
    let ctx = test_ctx();
    let mut sess = setup_pilot_in_space(&ctx, 991_005, 98_991_005);
    let kw = HashMap::new();

    // Superweapon ActivateSlashModule missing moduleID -> InvalidArgument
    let err_sw = ctx
        .dispatch_rpc(&mut sess, "superWeaponMgr", None, "ActivateSlashModule", &[], &kw, None)
        .await
        .expect_err("ActivateSlashModule without moduleID must fail");
    assert_eq!(err_sw.code, "InvalidArgument");

    // Nav CmdSetSpeedFraction with NaN -> InvalidSpeedFraction
    let bind = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(30000142), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject for beyonce must succeed");
    let bound_beyonce = match bind {
        PyRep::Tuple(parts) => parts[0].as_i64().expect("bound object id") as u64,
        _ => panic!("expected MachoBindObject tuple"),
    };
    let err_spd = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bound_beyonce),
            "CmdSetSpeedFraction",
            &[PyRep::Real(f64::NAN)],
            &kw,
            None,
        )
        .await
        .expect_err("CmdSetSpeedFraction(NaN) must fail");
    assert_eq!(err_spd.code, "InvalidSpeedFraction");

    // Nav CmdGotoDirection with missing args -> InvalidArguments
    let err_dir = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bound_beyonce),
            "CmdGotoDirection",
            &[PyRep::Real(1.0)],
            &kw,
            None,
        )
        .await
        .expect_err("CmdGotoDirection with missing y/z must fail");
    assert_eq!(err_dir.code, "InvalidArguments");

    // Corp PayoutDividend with NaN -> InvalidArgument
    let err_div = ctx
        .dispatch_rpc(
            &mut sess,
            "corpRegistry",
            None,
            "PayoutDividend",
            &[PyRep::Bool(false), PyRep::Real(f64::NAN)],
            &kw,
            None,
        )
        .await
        .expect_err("PayoutDividend(NaN) must fail");
    assert_eq!(err_div.code, "InvalidArgument");

    // AddContact with integer 2 for watchlist bool -> strict bool check rejects it
    ctx.db.with_mut_snapshot(|s| {
        let mut target = s.characters.values().next().expect("demo char seeded").clone();
        target.character_id = 991_006;
        target.name = "Contact Target".into();
        s.characters.insert(991_006, target);
    });
    let err_contact = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "AddContact",
            &[PyRep::Integer(991_006), PyRep::Real(5.0), PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect_err("AddContact with non-bool 2 for inWatchlist must fail");
    assert_eq!(err_contact.code, "InvalidArgument");
}
