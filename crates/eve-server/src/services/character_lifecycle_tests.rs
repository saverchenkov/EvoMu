//! Unit tests for `character_lifecycle` (`charUnboundMgr` and `charMgr`).

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use eve_db::{
    corp_roles, ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    KillmailRecord, MarketOrderRecord,
};
use eve_proto::PyRep;
use std::collections::{HashMap, HashSet};

#[test]
fn char_unbound_mgr_prepare_cancel_delete_bloodline_and_gender() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let mut sess = auth_session(uid, cid, corp_id, 30_000_142);
    let kw = HashMap::new();

    assert!(doomheim_corp_id(&ctx).is_some());
    assert_eq!(unix_to_filetime(0), 116_444_736_000_000_000);

    // GetNumCharacters, GetQAStarterSystemIDs, ToggleValidation
    let num_rep = handle(&ctx, &mut sess, "charUnboundMgr", "GetNumCharacters", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(num_rep, PyRep::Tuple(_)));
    let qa_sys = handle(&ctx, &mut sess, "charUnboundMgr", "GetQAStarterSystemIDs", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(qa_sys.as_list().map(|l| l.len()), Some(4));
    assert_eq!(
        handle(&ctx, &mut sess, "charUnboundMgr", "ToggleValidation", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    // UpdateCharacterGender & UpdateCharacterBloodline
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "UpdateCharacterGender",
            &[PyRep::Integer(cid), PyRep::Integer(1)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "UpdateCharacterBloodline",
            &[PyRep::Integer(cid), PyRep::Integer(999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidBloodline"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert(format!("resculpt_pending:{cid}"), "true".into());
    });
    let ch_race = ctx.db.with_snapshot(|s| s.characters[&cid].race_id);
    let valid_bl = *ctx.sde.bloodlines.iter().find(|(_, b)| b.race_id == ch_race).unwrap().0;
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "UpdateCharacterBloodline",
            &[PyRep::Integer(cid), PyRep::Integer(valid_bl as i64)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // Seed a sacrificial character on account 1 in a player corporation with an office, market order, and contract
    let sac_cid = 90_888_001_i64;
    let player_corp = 98_888_001_i64;
    let station_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&cid).unwrap().clone();
        ch.character_id = sac_cid;
        ch.user_id = uid;
        ch.name = "BiomassTarget".into();
        ch.corporation_id = player_corp;
        ch.station_id = Some(station_id);
        ch.home_station_id = station_id;
        ch.balance = IskCents(100_000_00);
        snap.characters.insert(sac_cid, ch);

        let mut members = HashSet::new();
        members.insert(cid);
        members.insert(sac_cid);
        snap.corporations.insert(
            player_corp,
            CorporationRecord {
                corporation_id: player_corp,
                name: "TestPlayerCorp".into(),
                ceo_id: sac_cid, // initially CEO to test CannotDeleteCorpCEO
                members,
                ..Default::default()
            },
        );
        // Corp office at station_id (type 27)
        snap.items.insert(
            910_001,
            InventoryItemRecord {
                item_id: 910_001,
                type_id: 27,
                owner_id: player_corp,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Personal asset & market order & contract
        snap.items.insert(
            910_002,
            InventoryItemRecord {
                item_id: 910_002,
                type_id: 34,
                owner_id: sac_cid,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                ..Default::default()
            },
        );
        snap.market_orders.insert(
            910_003,
            MarketOrderRecord {
                order_id: 910_003,
                owner_id: sac_cid,
                station_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents(500),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(50_000_00),
            },
        );
        snap.contracts.insert(
            910_004,
            ContractRecord {
                contract_id: 910_004,
                issuer_id: sac_cid,
                status: "Outstanding".into(),
                reward: IskCents(25_000_00),
                start_station_id: station_id,
                ..Default::default()
            },
        );
    });

    // CEO cannot prepare for delete
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "PrepareCharacterForDelete",
            &[PyRep::Integer(sac_cid)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotDeleteCorpCEO"
    );

    // Transfer CEO to cid
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&player_corp).unwrap().ceo_id = cid;
    });

    // Prepare, verify SelectCharacterID is locked, cancel, re-prepare, verify timer check, then delete
    let prep_rep = handle(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "PrepareCharacterForDelete",
        &[PyRep::Integer(sac_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(prep_rep.as_i64().unwrap() > 0);
    assert!(ctx.db.with_snapshot(|s| is_character_delete_pending(s, sac_cid)));
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "SelectCharacterID",
            &[PyRep::Integer(sac_cid)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterLocked"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "DeleteCharacter",
            &[PyRep::Integer(sac_cid)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DeleteTimerNotExpired"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "CancelCharacterDeletePrepare",
            &[PyRep::Integer(sac_cid)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charUnboundMgr",
            "DeleteCharacter",
            &[PyRep::Integer(sac_cid)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DeleteNotPrepared"
    );

    // Set prepared timestamp in the past and execute DeleteCharacter
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_store(snap);
        st.delete_prepared_unix.insert(sac_cid, 1);
        save_store(snap, &st);
    });
    let del_rep = handle(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "DeleteCharacter",
        &[PyRep::Integer(sac_cid)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(del_rep.dict_get("deleted"), Some(&PyRep::Bool(true)));

    let doomheim = doomheim_corp_id(&ctx).unwrap();
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&sac_cid).unwrap();
        assert_eq!(ch.user_id, 0);
        assert_eq!(ch.corporation_id, doomheim);
        assert_eq!(ch.balance.0, 0);
        // Asset 910_002 transferred to player_corp
        assert_eq!(snap.items.get(&910_002).unwrap().owner_id, player_corp);
        // Corp division 1 received 100k + 50k escrow + 25k reward = 175k ISK
        assert_eq!(
            snap.corporations
                .get(&player_corp)
                .unwrap()
                .division_balance(1)
                .0,
            175_000_00
        );
    });
}

#[test]
fn char_mgr_queries_roles_kills_activity_and_cohorts() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let corp_id = 98_000_001_i64;
    let mut sess = auth_session(uid, cid, corp_id, 30_000_142);
    let kw = HashMap::new();

    assert!(
        handle(&ctx, &mut sess, "charMgr", "GetCharacterCreationDate", &[], &kw)
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap()
            > 0
    );
    let hs = handle(&ctx, &mut sess, "charMgr", "GetHomeStationRow", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(hs.dict_get("stationID").is_some());

    let orgs = handle(
        &ctx,
        &mut sess,
        "charMgr",
        "GetOrganizationInfoForCharacters",
        &[PyRep::List(vec![PyRep::Integer(cid), PyRep::Integer(other_cid)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(orgs.as_dict().map(|d| d.len()), Some(2));

    // GetPrivateInfoOnCorpChange: own char succeeds, other char denied unless Director/Personnel Manager
    assert!(
        handle(&ctx, &mut sess, "charMgr", "GetPrivateInfoOnCorpChange", &[], &kw)
            .unwrap()
            .is_ok()
    );
    assert!(handle(
        &ctx,
        &mut sess,
        "charMgr",
        "GetPrivateInfoOnCorpChange",
        &[PyRep::Integer(other_cid)],
        &kw,
    )
    .unwrap()
    .is_err());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&cid) {
            c1.corporation_id = corp_id;
        }
        if let Some(c2) = snap.characters.get_mut(&other_cid) {
            c2.corporation_id = corp_id;
        }
        snap.corporations
            .entry(corp_id)
            .or_insert_with(|| CorporationRecord {
                corporation_id: corp_id,
                ..Default::default()
            })
            .member_roles
            .insert(cid, corp_roles::ROLE_DIRECTOR);
        snap.killmails.insert(
            7001,
            KillmailRecord {
                kill_id: 7001,
                victim_character_id: cid,
                attacker_character_id: other_cid,
                solar_system_id: 30_000_142,
                killed_unix: 1_700_000_000,
                ..Default::default()
            },
        );
    });

    assert!(handle(
        &ctx,
        &mut sess,
        "charMgr",
        "GetPrivateInfoOnCorpChange",
        &[PyRep::Integer(other_cid)],
        &kw,
    )
    .unwrap()
    .is_ok());

    let pub_info = handle(&ctx, &mut sess, "charMgr", "GetPublicInfo", &[PyRep::Integer(cid)], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(pub_info.dict_get("characterID"), Some(&PyRep::Integer(cid)));

    let kills = handle(
        &ctx,
        &mut sess,
        "charMgr",
        "GetRecentShipKillsAndLosses",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(kills.row_count(), Some(1));

    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "charMgr",
            "SetActivityStatus",
            &[PyRep::String("active".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    assert!(
        handle(&ctx, &mut sess, "charMgr", "GetCohortsForCharacter", &[], &kw)
            .unwrap()
            .is_ok()
    );
    assert!(handle(
        &ctx,
        &mut sess,
        "charMgr",
        "GetCohortsForCharacter",
        &[PyRep::Integer(other_cid)],
        &kw,
    )
    .unwrap()
    .is_err());
}
