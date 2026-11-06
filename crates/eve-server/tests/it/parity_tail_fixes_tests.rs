//! Non-GM SQLite-persisted E2E tests for HyperNet `raffleMgr.character_Seeding`
//! player hangar-item seeding and V24 SDE-grounded Paragon Hub (`economy.paragon-hub`)
//! cosmetic marketplace & SKINR sequencing studio skills, fees, and caps.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    AccountRecord, CharacterRecord, InventoryFlag, InventoryItemRecord, IskCents, NeuralAttributes,
    TrainedSkillRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag64,
    EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{STATUS_BAD_REQUEST, STATUS_OK, STATUS_PRECONDITION_FAILED};
use eve_server::router::ServerContext;
use eve_server::services::hypernet_raffle_ops::{compute_required_hypercores, HYPERCORE_TYPE_ID};
use eve_server::services::quasar_cosmetics_market::PARAGON_CORP_ID;
use std::collections::HashMap;
use std::path::PathBuf;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const RIFTER_TYPE: u32 = 587;
const MERLIN_TYPE: u32 = 603;

fn temp_sqlite_path(tag: &str) -> PathBuf {
    let ns = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("evomu_tail_fixes_{tag}_{}_{ns}.sqlite", std::process::id()))
}

fn cleanup_sqlite(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

fn player_session(sid: u64, uid: u64, cid: i64, corp_id: i64, station_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(JITA_SYSTEM_ID);
    s.station_id = station_id;
    s.role = service_roles::ROLE_PLAYER_DEFAULT;
    s
}

async fn mrpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn seed_hangar(
    ctx: &ServerContext,
    sess: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    mrpc(ctx, sess, "raffleMgr", "character_Seeding", args).await
}

async fn qrpc(ctx: &ServerContext, sess: &mut SessionState, type_name: &str, value: Vec<u8>) -> EvePublicResponse {
    let now = ctx.now_unix();
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: now as i64, nanos: 0 }),
        correlation_uuid: vec![0x42; 16],
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"), value }),
        external_origin: "tail-fixes-e2e".into(),
        application_instance_uuid: vec![0x24; 16],
        authoritative_context: None,
        trace_context: None,
    };
    let rep = mrpc(ctx, sess, "publicGatewaySvc", "SendRequest", &[PyRep::Bytes(req.encode_proto())])
        .await
        .expect("publicGatewaySvc.SendRequest");
    let raw = rep.dict_get("response_proto").and_then(|v| match v {
        PyRep::Bytes(b) => Some(b.clone()),
        _ => None,
    }).expect("response_proto bytes");
    EvePublicResponse::decode_proto(&raw).expect("decode EvePublicResponse")
}

fn seed_non_gm_pilot(
    db: &WorldRepository,
    cid: i64,
    uid: u64,
    name: &str,
    corp_id: i64,
    isk_cents: i64,
    plex: i64,
    evermarks: i64,
) -> i64 {
    let ship_id = cid * 10 + 1;
    db.with_mut_snapshot(|s| {
        let uname = format!("user_{uid}");
        s.accounts.entry(uname.clone()).or_insert_with(|| AccountRecord {
            user_id: uid, username: uname, password_token: "hash".into(),
            role: service_roles::ROLE_PLAYER_DEFAULT, banned_until_unix: None, ban_reason: None,
        });
        s.account_plex_vault.insert(uid, plex);
        let mut lp = HashMap::new();
        lp.insert(PARAGON_CORP_ID as i64, evermarks);
        s.characters.insert(cid, CharacterRecord {
            character_id: cid, user_id: uid, name: name.into(), corporation_id: corp_id,
            alliance_id: None, war_faction_id: None, station_id: Some(JITA_STATION_ID),
            structure_id: None, solar_system_id: JITA_SYSTEM_ID, constellation_id: 20_000_020,
            region_id: 10_000_002, active_ship_id: ship_id, balance: IskCents(isk_cents),
            plex_balance: 0, loyalty_points: lp, standings: HashMap::new(), security_status: 0.5,
            clone_grade: 2, home_station_id: JITA_STATION_ID, race_id: 1, bloodline_id: 1,
            ancestry_id: 11, school_id: 17, neural: NeuralAttributes::default(),
            skill_points: 5_000_000.0, activity_status: None, free_skill_points: 0.0,
            last_clone_jump_unix: 0, jump_fatigue_minutes: 0.0, jump_cooldown_until_unix: 0,
            jump_fatigue_until_unix: 0, description: String::new(), title: String::new(),
            gender: 1, create_date_unix: 1_704_067_200, fleet_id: None,
            autopilot_waypoints: Vec::new(), version: 1, system_epoch: 1,
        });
        s.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: RIFTER_TYPE, owner_id: cid, location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });
    ship_id
}

fn put_seq(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_uuid(field: u32, uuid: &[u8; 16], out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_isk(field: u32, units: i64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, encode_zigzag64(units), &mut b);
    encode_varint_field(2, 0, &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_plex_cents(field: u32, cents: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, encode_zigzag64(cents as i64), &mut b);
    encode_bytes_field(field, &b, out);
}

fn put_seconds(field: u32, secs: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, secs, &mut b);
    encode_bytes_field(field, &b, out);
}

fn decode_sint_units(msg_bytes: &[u8]) -> i64 {
    let wm = WireMessage::decode(msg_bytes).expect("decode money/rate msg");
    let raw = wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()).unwrap_or(0);
    ((raw >> 1) as i64) ^ -((raw & 1) as i64)
}

fn extract_uuid_list(raw: &[u8], entry_field: u32) -> Vec<[u8; 16]> {
    let wm = WireMessage::decode(raw).expect("decode list msg");
    wm.fields.iter().filter(|f| f.field_number == entry_field).filter_map(|f| {
        let entry = WireMessage::decode(f.value.as_bytes()?).ok()?;
        let id_m = WireMessage::decode(entry.fields.iter().find(|x| x.field_number == 1)?.value.as_bytes()?).ok()?;
        id_m.fields.iter().find(|x| x.field_number == 1)?.value.as_bytes()?.try_into().ok()
    }).collect()
}

/// Exercises non-GM (`ROLE_PLAYER`) `raffleMgr.character_Seeding` hangar-item seeding,
/// negative validation, `raffleProxy` ticket purchase/payout/claim, and SQLite reload persistence.
// parity: rpc:raffleMgr.QA_SeedRaffles, rpc:raffleProxy.CreateRaffle, rpc:raffleProxy.BuyRandomTickets
// parity: rpc:raffleProxy.AwardItem, rpc:raffleProxy.Grab, feat:wallet.hypernet-relay
// parity: feat:wallet.hypernet-hypercore-sizing-expiry, notify:OnRaffleCreatedServer
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_gm_raffle_mgr_character_seeding_hangar_e2e_and_sqlite_reload() {
    eve_testkit::attest(&[
        "rpc:raffleMgr.QA_SeedRaffles", "rpc:raffleProxy.CreateRaffle",
        "rpc:raffleProxy.BuyRandomTickets", "rpc:raffleProxy.AwardItem",
        "rpc:raffleProxy.Grab", "feat:wallet.hypernet-relay",
        "feat:wallet.hypernet-hypercore-sizing-expiry", "notify:OnRaffleCreatedServer",
    ]);

    let db_path = temp_sqlite_path("raffle_char_seeding");
    let (seller_cid, buyer_cid) = (2_120_000_101i64, 2_120_000_102i64);
    let (item_rifter, item_merlin, item_stacked, item_cores) = (995_100_001i64, 995_100_002i64, 995_100_003i64, 995_100_004i64);
    let (raffle_1_id, raffle_2_id, seller_bal_after, buyer_bal_after, remaining_cores_expected);

    {
        let ctx = ServerContext::from_sqlite(&db_path).expect("open sqlite");
        common::pin_clock(&ctx);

        let active_ship_id = seed_non_gm_pilot(&ctx.db, seller_cid, 7101, "HangarRaffleHost", 98_000_101, 10_000_000_00, 0, 0);
        seed_non_gm_pilot(&ctx.db, buyer_cid, 7102, "HangarRaffleBuyer", 98_000_102, 50_000_000_00, 0, 0);

        ctx.db.with_mut_snapshot(|s| {
            for (iid, tid, qty, singleton) in [
                (item_rifter, RIFTER_TYPE, 1, true),
                (item_merlin, MERLIN_TYPE, 1, true),
                (item_stacked, RIFTER_TYPE, 2, false),
            ] {
                s.items.insert(iid, InventoryItemRecord {
                    item_id: iid, type_id: tid, owner_id: seller_cid, location_id: JITA_STATION_ID,
                    flag: InventoryFlag::Hangar, quantity: qty, singleton, version: 1, ..Default::default()
                });
            }
        });

        let mut seller = player_session(801, 7101, seller_cid, 98_000_101, Some(JITA_STATION_ID));
        let mut buyer = player_session(802, 7102, buyer_cid, 98_000_102, Some(JITA_STATION_ID));
        assert!(!seller.has_gm_role() && !buyer.has_gm_role());
        let _ = ctx.db.acquire_online_lock(7101, 801, Some(seller_cid), true, ctx.now_unix());

        // 1. Negative checks under non-GM ROLE_PLAYER
        let cross_err = seed_hangar(&ctx, &mut seller, &[PyRep::Integer(buyer_cid), PyRep::List(vec![PyRep::Integer(item_rifter)])]).await.unwrap_err();
        assert_eq!(cross_err.message, "NotAuthorized");

        seller.station_id = None;
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&seller_cid).unwrap().station_id = None);
        let undock_err = seed_hangar(&ctx, &mut seller, &[PyRep::List(vec![PyRep::Integer(item_rifter)])]).await.unwrap_err();
        assert_eq!(undock_err.message, "CharacterNotDocked");

        seller.structure_id = Some(1_045_000_001_000);
        ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&seller_cid).unwrap().structure_id = Some(1_045_000_001_000));
        let struct_err = seed_hangar(&ctx, &mut seller, &[PyRep::List(vec![PyRep::Integer(item_rifter)])]).await.unwrap_err();
        assert_eq!(struct_err.code, "InvalidRaffleLocation");
        seller.structure_id = None;
        seller.station_id = Some(JITA_STATION_ID);
        ctx.db.with_mut_snapshot(|s| {
            let ch = s.characters.get_mut(&seller_cid).unwrap();
            ch.structure_id = None;
            ch.station_id = Some(JITA_STATION_ID);
        });

        for bad_item in [item_stacked, active_ship_id] {
            let item_err = seed_hangar(&ctx, &mut seller, &[PyRep::List(vec![PyRep::Integer(bad_item)])]).await.unwrap_err();
            assert_eq!(item_err.code, "InvalidRaffleItem");
        }
        let tc_err = seed_hangar(&ctx, &mut seller, &[PyRep::List(vec![PyRep::Integer(item_rifter)]), PyRep::Integer(12)]).await.unwrap_err();
        assert_eq!(tc_err.code, "InvalidTicketCount");
        let core_err = seed_hangar(&ctx, &mut seller, &[PyRep::List(vec![PyRep::Integer(item_rifter), PyRep::Integer(item_merlin)]), PyRep::Integer(16), PyRep::Real(25_000.0)]).await.unwrap_err();
        assert_eq!(core_err.code, "InsufficientHyperCores");
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.items[&item_rifter].owner_id, seller_cid);
            assert_eq!(s.items[&item_rifter].flag, InventoryFlag::Hangar);
        });

        // 2. Grant HyperCores in station hangar and seed 2 real hangar items as non-GM ROLE_PLAYER
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(item_cores, InventoryItemRecord {
                item_id: item_cores, type_id: HYPERCORE_TYPE_ID, owner_id: seller_cid, location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar, quantity: 20, singleton: false, version: 1, ..Default::default()
            });
        });

        let seeded = mrpc(&ctx, &mut seller, "raffleMgr", "character_Seeding", &[
            PyRep::List(vec![PyRep::Integer(item_rifter), PyRep::Integer(item_merlin)]),
            PyRep::Integer(16), PyRep::Real(25_000.0),
        ]).await.expect("non-GM character_Seeding hangar items");
        let PyRep::List(seeded_list) = seeded else { panic!("expected List of seeded raffles") };
        assert_eq!(seeded_list.len(), 2);
        raffle_1_id = seeded_list[0].dict_get("raffle_id").and_then(PyRep::as_i64).unwrap();
        raffle_2_id = seeded_list[1].dict_get("raffle_id").and_then(PyRep::as_i64).unwrap();
        assert_ne!(raffle_1_id, raffle_2_id);
        assert_eq!(seeded_list[0].dict_get("type_id").and_then(PyRep::as_i64), Some(RIFTER_TYPE as i64));
        assert_eq!(seeded_list[1].dict_get("type_id").and_then(PyRep::as_i64), Some(MERLIN_TYPE as i64));
        assert_eq!(seeded_list[0].dict_get("owner_id").and_then(PyRep::as_i64), Some(seller_cid));
        assert_eq!(seeded_list[0].dict_get("ticket_count").and_then(PyRep::as_i64), Some(16));
        assert_eq!(seeded_list[0].dict_get("ticket_price").and_then(PyRep::as_f64), Some(25_000.0));

        let cores_used_total: i32 = seeded_list.iter().map(|r| r.dict_get("hypercores_used").and_then(PyRep::as_i64).unwrap() as i32).sum();
        assert!(cores_used_total >= compute_required_hypercores(16.0 * 25_000.0, 16).unwrap() * 2);
        remaining_cores_expected = 20 - cores_used_total;

        let created_notices = ctx.drain_notifications_for_char(seller_cid).iter().filter(|n| n.event == "OnRaffleCreatedServer").count();
        assert_eq!(created_notices, 2, "must emit OnRaffleCreatedServer per seeded item");

        ctx.db.with_snapshot(|s| {
            for iid in [item_rifter, item_merlin] {
                assert_eq!(s.items[&iid].owner_id, -52_568);
                assert_eq!(s.items[&iid].flag, InventoryFlag::Locked);
            }
            assert_eq!(s.items[&item_cores].quantity, remaining_cores_expected);
        });

        // 3. Second non-GM player buys all 16 tickets on Raffle 1 -> wins & claims item
        mrpc(&ctx, &mut buyer, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle_1_id), PyRep::Integer(16)]).await.expect("BuyRandomTickets");
        let claimed = mrpc(&ctx, &mut buyer, "raffleProxy", "AwardItem", &[PyRep::Integer(raffle_1_id)]).await.expect("AwardItem");
        assert_eq!(claimed.as_i64(), Some(item_rifter));

        seller_bal_after = ctx.db.with_snapshot(|s| s.characters[&seller_cid].balance.0);
        buyer_bal_after = ctx.db.with_snapshot(|s| s.characters[&buyer_cid].balance.0);
        assert_eq!(seller_bal_after, 10_380_000_00);
        assert_eq!(buyer_bal_after, 49_600_000_00);
    }

    // 4. Reload fresh ServerContext from SQLite and verify items, HyperCores, wallets & active Raffle 2 persist
    {
        let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen sqlite");
        let mut buyer2 = player_session(812, 7102, buyer_cid, 98_000_102, Some(JITA_STATION_ID));

        let PyRep::List(active_raffles) = mrpc(&ctx2, &mut buyer2, "raffleProxy", "Grab", &[]).await.expect("Grab") else { panic!("expected List") };
        assert_eq!(active_raffles.len(), 1);
        assert_eq!(active_raffles[0].dict_get("raffle_id").and_then(PyRep::as_i64), Some(raffle_2_id));
        assert_eq!(active_raffles[0].dict_get("type_id").and_then(PyRep::as_i64), Some(MERLIN_TYPE as i64));

        ctx2.db.with_snapshot(|s| {
            assert_eq!((s.items[&item_rifter].owner_id, s.items[&item_rifter].flag), (buyer_cid, InventoryFlag::Hangar));
            assert_eq!((s.items[&item_merlin].owner_id, s.items[&item_merlin].flag), (-52_568, InventoryFlag::Locked));
            assert_eq!(s.items[&item_cores].quantity, remaining_cores_expected);
            assert_eq!((s.characters[&seller_cid].balance.0, s.characters[&buyer_cid].balance.0), (seller_bal_after, buyer_bal_after));
        });
    }
    cleanup_sqlite(&db_path);
}

/// Exercises non-GM (`ROLE_PLAYER`) Paragon Hub (`economy.paragon-hub`) & SKINR sequencing
/// grounded in V24 SDE Group 4734 skills and `ship_skin_design_components.fsdbinary`.
// parity: feat:cosmetics.skinr-market, feat:cosmetics.skinr-paragon-hub-listing
// parity: feat:cosmetics.skinr-sequencing-studio, feat:cosmetics.skinr-designer
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj
// parity: quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_gm_paragon_hub_and_skinr_sde_grounded_e2e_and_sqlite_reload() {
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-market", "feat:cosmetics.skinr-paragon-hub-listing",
        "feat:cosmetics.skinr-sequencing-studio", "feat:cosmetics.skinr-designer",
        "quasar:eveProto/generated/eve_public/cosmetic/market/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/component/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/market/skin/listing/api/api_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/api/requests_pb2.pyj",
        "quasar:eveProto/generated/eve_public/cosmetic/ship/skin/thirdparty/sequencing/job/api/requests_pb2.pyj",
    ]);

    let db_path = temp_sqlite_path("paragon_hub_skinr");
    let (seller_cid, buyer_cid, shared_corp) = (2_120_000_201i64, 2_120_000_202i64, 98_000_201i64);
    let (skin_hex_saved, seller_isk_final, buyer_isk_final, seller_plex_final, buyer_plex_final, buyer_em_final);

    {
        let ctx = ServerContext::from_sqlite(&db_path).expect("open sqlite");
        common::pin_clock(&ctx);

        seed_non_gm_pilot(&ctx.db, seller_cid, 7201, "ParagonDesigner", shared_corp, 5_000_000_000_00, 50_000, 50_000);
        seed_non_gm_pilot(&ctx.db, buyer_cid, 7202, "ParagonCollector", shared_corp, 80_000_000_00, 5_000, 50_000);

        let mut seller = player_session(821, 7201, seller_cid, shared_corp, Some(JITA_STATION_ID));
        let mut buyer = player_session(822, 7202, buyer_cid, shared_corp, Some(JITA_STATION_ID));
        assert!(!seller.has_gm_role() && !buyer.has_gm_role());

        // 1. Untrained Paragon Hub tax is 20% Public; train SDE Group 4734 skills -> 5% Public tax, 30% Induction, 150 runs/job, 6 listing slots
        let untax_rsp = qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetTaxRateRequest", Vec::new()).await;
        let untax_wm = WireMessage::decode(&untax_rsp.payload.unwrap().value).unwrap();
        assert_eq!(decode_sint_units(untax_wm.fields[0].value.as_bytes().unwrap()), 20);

        ctx.db.with_mut_snapshot(|s| {
            let sk = s.skills.entry(seller_cid).or_default();
            for (sid, lvl) in [(84217u32, 5u8), (84218, 5), (84220, 5), (81377, 5), (81372, 5), (81375, 5), (81364, 3), (81368, 1)] {
                sk.insert(sid, TrainedSkillRecord { type_id: sid, level: lvl, skill_points: 256_000.0 });
            }
        });

        let tax_wm = WireMessage::decode(&qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.api.GetTaxRateRequest", Vec::new()).await.payload.unwrap().value).unwrap();
        assert_eq!(decode_sint_units(tax_wm.fields[0].value.as_bytes().unwrap()), 5);
        let dur_wm = WireMessage::decode(&qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest", Vec::new()).await.payload.unwrap().value).unwrap();
        assert_eq!(dur_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(30));

        // 2. Non-GM component purchase from SDE-seeded Paragon Hub + hangar item ConsumeRequest (zero GM grants)
        let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
        let (&coat_item_tid, &(coat_comp_id, uses_per_item)) = skinr_cat.item_type_to_component.iter().find(|(_, &(cid, uses))| {
            uses > 0 && skinr_cat.components.get(&cid).is_some_and(|c| !c.is_pattern() && !c.disallowed_slots.contains(&1))
        }).expect("SDE coating component item");

        let comps_wm = WireMessage::decode(&qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await.payload.unwrap().value).unwrap();
        let target_comp_uuid: [u8; 16] = comps_wm.fields.iter().filter(|f| f.field_number == 1).find_map(|f| {
            let entry = WireMessage::decode(f.value.as_bytes()?).ok()?;
            let id_m = WireMessage::decode(entry.fields.iter().find(|x| x.field_number == 1)?.value.as_bytes()?).ok()?;
            let attrs = WireMessage::decode(entry.fields.iter().find(|x| x.field_number == 2)?.value.as_bytes()?).ok()?;
            let has_isk = attrs.fields.iter().any(|x| x.field_number == 3);
            let bundle = attrs.fields.iter().find(|x| x.field_number == 6).and_then(|x| x.value.as_varint()).unwrap_or(0);
            let tid_m = WireMessage::decode(attrs.fields.iter().find(|x| x.field_number == 5)?.value.as_bytes()?).ok()?;
            let tid = tid_m.fields.iter().find(|x| x.field_number == 1)?.value.as_varint()? as u32;
            (has_isk && bundle == 1 && tid == coat_item_tid).then(|| id_m.fields[0].value.as_bytes().unwrap().try_into().unwrap())
        }).expect("seeded ISK listing for SDE coating component");

        let need_items = ((120 + uses_per_item as u64 - 1) / (uses_per_item as u64)).max(1);
        let mut buy_comp = Vec::new();
        put_uuid(1, &target_comp_uuid, &mut buy_comp);
        encode_varint_field(2, need_items, &mut buy_comp);
        assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest", buy_comp).await.status_code, STATUS_OK);

        let purchased_item_id = ctx.db.with_snapshot(|s| {
            s.items.values().find(|i| i.owner_id == seller_cid && i.type_id == coat_item_tid && i.flag == InventoryFlag::Hangar).unwrap().item_id
        });
        let mut consume_req = Vec::new();
        put_seq(1, purchased_item_id as u64, &mut consume_req);
        encode_varint_field(2, need_items, &mut consume_req);
        assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest", consume_req).await.status_code, STATUS_OK);

        // 3. Sequence 120 SKINs in one job (enabled by Batch V + Mass III = 150 cap; 151 rejected) & CompleteEarly
        let (mut coat_id_m, mut coat_cfg, mut slot_cfg, mut slot_m, mut layout) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, coat_comp_id as u64, &mut coat_id_m);
        encode_bytes_field(1, &coat_id_m, &mut coat_cfg);
        encode_bytes_field(1, &coat_cfg, &mut slot_cfg);
        put_seq(1, 1, &mut slot_m);
        encode_bytes_field(2, &slot_cfg, &mut slot_m);
        encode_bytes_field(1, &slot_m, &mut layout);
        put_seq(2, RIFTER_TYPE as u64, &mut layout);
        encode_varint_field(3, 1, &mut layout);

        let mk_init = |qty: u64| {
            let mut r = Vec::new();
            encode_string_field(1, "Grounded Batch Rifter", &mut r);
            encode_string_field(2, "Paragon Grounded", &mut r);
            encode_bytes_field(3, &layout, &mut r);
            encode_varint_field(5, qty, &mut r);
            r
        };
        assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", mk_init(151)).await.status_code, STATUS_BAD_REQUEST);
        let init_rsp = qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", mk_init(120)).await;
        assert_eq!(init_rsp.status_code, STATUS_OK, "120-run batch failed: {}", init_rsp.status_message);
        let init_wm = WireMessage::decode(&init_rsp.payload.unwrap().value).unwrap();
        let job_uuid: [u8; 16] = WireMessage::decode(init_wm.fields[0].value.as_bytes().unwrap()).unwrap().fields[0].value.as_bytes().unwrap().try_into().unwrap();
        let attrs_wm = WireMessage::decode(init_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
        let sid_wm = WireMessage::decode(attrs_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
        skin_hex_saved = std::str::from_utf8(sid_wm.fields[0].value.as_bytes().unwrap()).unwrap().to_string();

        let plex_pre_early = ctx.db.with_snapshot(|s| s.unified_plex_balance(seller_cid));
        let mut early_req = Vec::new();
        put_uuid(1, &job_uuid, &mut early_req);
        assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", early_req).await.status_code, STATUS_OK);
        assert!(ctx.db.with_snapshot(|s| s.unified_plex_balance(seller_cid)) < plex_pre_early);

        // 4. Verify Public (1,000 * tier) vs Corporation-branded (2,500 * tier) EverMark buyer fees & 6-slot cap
        let (mut skin_id_m, mut corp_target) = (Vec::new(), Vec::new());
        encode_string_field(1, &skin_hex_saved, &mut skin_id_m);
        put_seq(2, shared_corp as u64, &mut corp_target);

        let mut bf_pub_req = Vec::new();
        encode_bytes_field(2, &skin_id_m, &mut bf_pub_req);
        let bf_pub_wm = WireMessage::decode(&qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetBuyerFeeRequest", bf_pub_req).await.payload.unwrap().value).unwrap();
        let pub_em_fee = WireMessage::decode(WireMessage::decode(bf_pub_wm.fields[0].value.as_bytes().unwrap()).unwrap().fields[0].value.as_bytes().unwrap()).unwrap().fields.iter().find(|f| f.field_number == 2).unwrap().value.as_varint().unwrap() as i64;

        let mut bf_corp_req = Vec::new();
        encode_bytes_field(1, &corp_target, &mut bf_corp_req);
        encode_bytes_field(2, &skin_id_m, &mut bf_corp_req);
        let bf_corp_wm = WireMessage::decode(&qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.api.GetBuyerFeeRequest", bf_corp_req).await.payload.unwrap().value).unwrap();
        let corp_em_fee = WireMessage::decode(WireMessage::decode(bf_corp_wm.fields[0].value.as_bytes().unwrap()).unwrap().fields[0].value.as_bytes().unwrap()).unwrap().fields.iter().find(|f| f.field_number == 2).unwrap().value.as_varint().unwrap() as i64;
        assert_eq!(corp_em_fee * 2, pub_em_fee * 5, "Branded buyer fee must be 2.5x public tier fee");

        let mut lic_id = Vec::new();
        put_seq(1, seller_cid as u64, &mut lic_id);
        encode_bytes_field(2, &skin_id_m, &mut lic_id);

        let mut list_pub_isk = Vec::new();
        put_isk(1, 2_000_000, &mut list_pub_isk);
        encode_bytes_field(3, &lic_id, &mut list_pub_isk);
        put_seconds(4, 86_400, &mut list_pub_isk);
        encode_varint_field(5, 1, &mut list_pub_isk);
        assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", list_pub_isk.clone()).await.status_code, STATUS_OK);

        let mut list_corp_plex = Vec::new();
        put_plex_cents(2, 10_000, &mut list_corp_plex);
        encode_bytes_field(3, &lic_id, &mut list_corp_plex);
        put_seconds(4, 86_400, &mut list_corp_plex);
        encode_varint_field(5, 1, &mut list_corp_plex);
        encode_bytes_field(6, &corp_target, &mut list_corp_plex);
        assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", list_corp_plex).await.status_code, STATUS_OK);

        for _ in 0..4 {
            assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", list_pub_isk.clone()).await.status_code, STATUS_OK);
        }
        assert_eq!(qrpc(&ctx, &mut seller, "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest", list_pub_isk).await.status_code, STATUS_PRECONDITION_FAILED);

        let all_rsp = qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new()).await;
        let all_wm = WireMessage::decode(&all_rsp.payload.unwrap().value).unwrap();
        let entries: Vec<([u8; 16], bool)> = all_wm.fields.iter().filter(|f| f.field_number == 1).map(|f| {
            let entry = WireMessage::decode(f.value.as_bytes().unwrap()).unwrap();
            let id_m = WireMessage::decode(entry.fields.iter().find(|x| x.field_number == 1).unwrap().value.as_bytes().unwrap()).unwrap();
            let u: [u8; 16] = id_m.fields[0].value.as_bytes().unwrap().try_into().unwrap();
            let attrs = WireMessage::decode(entry.fields.iter().find(|x| x.field_number == 2).unwrap().value.as_bytes().unwrap()).unwrap();
            let branded = attrs.fields.iter().find(|x| x.field_number == 10).and_then(|x| x.value.as_varint()).unwrap_or(0) != 0;
            (u, branded)
        }).collect();
        assert_eq!(entries.len(), 6);
        let pub_isk_uuid = entries.iter().find(|x| !x.1).unwrap().0;
        let corp_plex_uuid = entries.iter().find(|x| x.1).unwrap().0;

        ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&buyer_cid).unwrap().loyalty_points.insert(PARAGON_CORP_ID as i64, 0); });
        let mut try_buy = Vec::new();
        put_uuid(1, &pub_isk_uuid, &mut try_buy);
        encode_varint_field(2, 1, &mut try_buy);
        assert_eq!(qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", try_buy).await.status_code, STATUS_PRECONDITION_FAILED);
        ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&buyer_cid).unwrap().loyalty_points.insert(PARAGON_CORP_ID as i64, 50_000); });

        let seller_isk_pre = ctx.db.with_snapshot(|s| s.characters[&seller_cid].balance.0);
        let seller_plex_pre = ctx.db.with_snapshot(|s| s.unified_plex_balance(seller_cid));
        for u in [&pub_isk_uuid, &corp_plex_uuid] {
            let mut b = Vec::new();
            put_uuid(1, u, &mut b);
            encode_varint_field(2, 1, &mut b);
            assert_eq!(qrpc(&ctx, &mut buyer, "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest", b).await.status_code, STATUS_OK);
        }
        seller_isk_final = ctx.db.with_snapshot(|s| s.characters[&seller_cid].balance.0);
        buyer_isk_final = ctx.db.with_snapshot(|s| s.characters[&buyer_cid].balance.0);
        seller_plex_final = ctx.db.with_snapshot(|s| s.unified_plex_balance(seller_cid));
        buyer_plex_final = ctx.db.with_snapshot(|s| s.unified_plex_balance(buyer_cid));
        buyer_em_final = ctx.db.with_snapshot(|s| s.characters[&buyer_cid].loyalty_points[&(PARAGON_CORP_ID as i64)]);

        assert_eq!(seller_isk_final - seller_isk_pre, 1_900_000_00, "5% Public seller tax after 84217/84218/84220 V");
        assert_eq!(seller_plex_final - seller_plex_pre, 90, "10% Corp seller tax on 100 PLEX sale");
        assert_eq!(buyer_plex_final, 4_900);
        assert_eq!(buyer_em_final, 50_000 - pub_em_fee - corp_em_fee);
    }

    // 5. Reload fresh ServerContext from SQLite and verify listings, SKIN licenses & balances persist
    {
        let ctx2 = ServerContext::from_sqlite(&db_path).expect("reopen sqlite");
        let mut buyer2 = player_session(832, 7202, buyer_cid, shared_corp, Some(JITA_STATION_ID));

        let all_after = qrpc(&ctx2, &mut buyer2, "eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new()).await;
        assert_eq!(extract_uuid_list(&all_after.payload.unwrap().value, 1).len(), 4);

        let owned_after = qrpc(&ctx2, &mut buyer2, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest", Vec::new()).await;
        let owned_wm = WireMessage::decode(&owned_after.payload.unwrap().value).unwrap();
        assert_eq!(owned_wm.fields.iter().filter(|f| f.field_number == 1).count(), 1);
        assert!(String::from_utf8_lossy(&owned_wm.encode()).contains(&skin_hex_saved));

        ctx2.db.with_snapshot(|s| {
            assert_eq!((s.characters[&seller_cid].balance.0, s.characters[&buyer_cid].balance.0), (seller_isk_final, buyer_isk_final));
            assert_eq!((s.unified_plex_balance(seller_cid), s.unified_plex_balance(buyer_cid)), (seller_plex_final, buyer_plex_final));
            assert_eq!(s.characters[&buyer_cid].loyalty_points[&(PARAGON_CORP_ID as i64)], buyer_em_final);
        });
    }
    cleanup_sqlite(&db_path);
}
