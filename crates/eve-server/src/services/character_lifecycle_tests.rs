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
    for bad_role in [0u64, eve_machonet::service_roles::ROLE_QA] {
        sess.role = bad_role;
        assert_eq!(
            handle(&ctx, &mut sess, "charUnboundMgr", "GetQAStarterSystemIDs", &[], &kw)
                .unwrap()
                .unwrap_err()
                .code,
            "InsufficientRole"
        );
        assert_eq!(
            handle(&ctx, &mut sess, "charUnboundMgr", "ToggleValidation", &[], &kw)
                .unwrap()
                .unwrap_err()
                .code,
            "InsufficientRole"
        );
    }
    sess.role = eve_machonet::service_roles::ROLE_GMH;
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
    sess.role = 0;

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
    assert_eq!(sess.gender_id, Some(1));
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
        snap.cluster_and_runtime.resculpt_pending.insert(cid);
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
    assert_eq!(sess.bloodline_id, Some(valid_bl));
    assert_eq!(sess.race_id, Some(ch_race));

    // Updating another character's gender while active on a different character must not leak into session.
    let mut other_char_sess = auth_session(uid, 90_000_002, corp_id, 30_000_142);
    assert_eq!(
        handle(
            &ctx,
            &mut other_char_sess,
            "charUnboundMgr",
            "UpdateCharacterGender",
            &[PyRep::Integer(cid), PyRep::Integer(0)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(other_char_sess.gender_id, None);
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&cid].gender), 0);

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
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = player_corp;
                __corp.name = "TestPlayerCorp".into();
                __corp.ceo_id = sac_cid;
                __corp.members = members;
                __corp },
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
            .or_insert_with(|| { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp })
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
    if let PyRep::Rowset { columns, rows } = &kills {
        assert_eq!(rows[0].len(), columns.len());
        assert_eq!(
            rows[0][3],
            PyRep::Integer(unix_to_filetime(1_700_000_000))
        );
        for required in [
            "warID",
            "iskLost",
            "iskDestroyed",
            "bountyClaimed",
            "loyaltyPoints",
            "killRightSupplied",
        ] {
            let idx = columns
                .iter()
                .position(|c| c == required)
                .unwrap_or_else(|| panic!("missing KillReportWnd column {required}"));
            assert_eq!(rows[0][idx], PyRep::None);
        }
    } else {
        panic!("expected Rowset from GetRecentShipKillsAndLosses");
    }

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

#[test]
fn char_unbound_mgr_toggle_validation_persists_per_user_state() {
    let ctx = test_ctx();
    let mut sess = auth_session(42, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    sess.role = eve_machonet::service_roles::ROLE_GML;
    let first = handle(&ctx, &mut sess, "charUnboundMgr", "ToggleValidation", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(first, PyRep::Bool(true));

    let second = handle(&ctx, &mut sess, "charUnboundMgr", "ToggleValidation", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(second, PyRep::Bool(false));

    let third = handle(&ctx, &mut sess, "charUnboundMgr", "ToggleValidation", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(third, PyRep::Bool(true));

    let persisted = ctx.db.with_snapshot(|s| {
        load_store(s)
            .validation_toggled_by_user
            .get(&42)
            .copied()
    });
    assert_eq!(persisted, Some(true));
}

#[test]
fn biomass_prepare_blocked_when_ship_keyed_self_destruct_active() {
    let ctx = test_ctx();
    let char_id = 90_000_002_i64;
    let ship_id = 990_001_001_i64;
    let mut sess = auth_session(1, char_id, 98_000_001, 30_000_142);
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.user_id = 1;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
        }
        snap.sp6_lifecycle.self_destructs.insert(
            ship_id,
            eve_db::SelfDestructPersistedEntry {
                ship_id,
                char_id,
                solar_system_id: 30_000_142,
                started_unix: now_u,
                detonate_at_unix: now_u + 120,
            },
        );
    });

    let err = handle(
        &ctx,
        &mut sess,
        "charUnboundMgr",
        "PrepareCharacterForDelete",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CharacterInSpace");

    ctx.db.with_mut_snapshot(|snap| {
        let test_corp_id = 98_777_123_i64;
        let ceo_id = 90_777_001_i64;
        let dir_id = 90_777_002_i64;
        let pm_id = 90_777_003_i64;
        let member_id = 90_777_004_i64;
        assert!(!super::has_corp_director_or_personnel_manager_role(
            snap, ceo_id, test_corp_id
        ));
        let mut corp = eve_db::CorporationRecord::default();
        corp.corporation_id = test_corp_id;
        corp.name = "RoleCheckCorp".into();
        corp.ticker = "RLCK".into();
        corp.ceo_id = ceo_id;
        corp.member_roles
            .insert(dir_id, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(test_corp_id, corp);
        snap.corp_role_assignments.insert(
            (test_corp_id, pm_id),
            eve_db::CorpMemberRoleAssignment {
                character_id: pm_id,
                roles: eve_db::corp_roles::ROLE_PERSONNEL_MANAGER,
                ..Default::default()
            },
        );
        assert!(super::has_corp_director_or_personnel_manager_role(
            snap, ceo_id, test_corp_id
        ));
        assert!(super::has_corp_director_or_personnel_manager_role(
            snap, dir_id, test_corp_id
        ));
        assert!(super::has_corp_director_or_personnel_manager_role(
            snap, pm_id, test_corp_id
        ));
        assert!(!super::has_corp_director_or_personnel_manager_role(
            snap, member_id, test_corp_id
        ));
    });
}

#[test]
fn char_mgr_combat_log_resolves_war_id_and_isk_values() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let victim_cid = 90_000_001_i64;
    let attacker_cid = 90_000_002_i64;
    let agg_corp = 98_100_001_i64;
    let def_corp = 98_100_002_i64;
    let ally_corp = 98_100_003_i64;
    let other_corp = 98_100_004_i64;
    let mut sess = auth_session(uid, victim_cid, def_corp, 30_000_142);
    let kw = HashMap::new();

    let (priced_tid, unit_price) = ctx
        .sde
        .types
        .iter()
        .find(|(_, t)| t.base_price >= 1_000.0)
        .map(|(&tid, t)| (tid, t.base_price))
        .expect("SDE has at least one type with positive base_price");
    let total_val = unit_price * 4.0;

    ctx.db.with_mut_snapshot(|snap| {
        // Active war 701: agg_corp vs def_corp (started at 1000, active indefinitely)
        snap.wars.insert(
            701,
            eve_db::WarRecord {
                war_id: 701,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: 1_000,
                active: true,
            },
        );
        // Accepted defender ally effective at 1_500
        snap.war_state.negotiations.push(eve_db::PersistedWarNegotiation {
            id: 1,
            war_id: 701,
            from_corp: ally_corp,
            kind: eve_db::PersistedNegKind::Ally,
            isk: IskCents::from_isk(0),
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(1_500),
        });
        // Finished war 702: agg_corp vs other_corp (ended at 1_200)
        snap.wars.insert(
            702,
            eve_db::WarRecord {
                war_id: 702,
                aggressor_corp_id: agg_corp,
                defender_corp_id: other_corp,
                mutual: false,
                started_unix: 500,
                active: false,
            },
        );
        snap.war_state.wars.insert(
            702,
            eve_db::PersistedWarMeta {
                finish_at_unix: Some(1_200),
                ..Default::default()
            },
        );

        // Kill 1 (ts 2_000): victim in def_corp vs attacker in agg_corp -> warID 701, dropped qty 1 -> iskDestroyed = 3x unit_price
        snap.killmails.insert(
            990_001,
            KillmailRecord {
                kill_id: 990_001,
                victim_character_id: victim_cid,
                attacker_character_id: attacker_cid,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: total_val,
                killed_unix: 2_000,
                victim_corp_id: def_corp,
                attacker_corp_id: agg_corp,
                ..Default::default()
            },
        );
        snap.combat_crimewatch_state.detailed_killmails.insert(
            990_001,
            eve_db::DetailedKillmail {
                kill_id: 990_001,
                kill_time_unix: 2_000,
                solar_system_id: 30_000_142,
                victim_character_id: victim_cid,
                victim_corporation_id: def_corp,
                victim_ship_type_id: 587,
                dropped_items: vec![eve_db::KillmailItemEntry {
                    type_id: priced_tid,
                    flag: 5,
                    qty_dropped: 1,
                    qty_destroyed: 0,
                    singleton: false,
                }],
                total_value_isk: total_val,
                ..Default::default()
            },
        );

        // Kill 2 (ts 1_800): accepted ally (ally_corp) killed agg_corp -> warID 701, no DetailedKillmail -> iskDestroyed None
        snap.killmails.insert(
            990_002,
            KillmailRecord {
                kill_id: 990_002,
                victim_character_id: attacker_cid,
                attacker_character_id: victim_cid,
                solar_system_id: 30_000_142,
                ship_type_id: 588,
                total_value_isk: 500_000.0,
                killed_unix: 1_800,
                victim_corp_id: agg_corp,
                attacker_corp_id: ally_corp,
                ..Default::default()
            },
        );

        // Kill 3 (ts 1_400): after war 702 finish_at_unix (1_200) -> warID None
        snap.killmails.insert(
            990_003,
            KillmailRecord {
                kill_id: 990_003,
                victim_character_id: victim_cid,
                attacker_character_id: attacker_cid,
                solar_system_id: 30_000_142,
                ship_type_id: 589,
                total_value_isk: 0.0,
                killed_unix: 1_400,
                victim_corp_id: other_corp,
                attacker_corp_id: agg_corp,
                ..Default::default()
            },
        );
    });

    let rep = handle(
        &ctx,
        &mut sess,
        "charMgr",
        "GetRecentShipKillsAndLosses",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Rowset { columns, rows } = rep else {
        panic!("expected Rowset");
    };
    let col = |name: &str| columns.iter().position(|c| c == name).expect(name);
    let kid_i = col("killID");
    let war_i = col("warID");
    let lost_i = col("iskLost");
    let dest_i = col("iskDestroyed");
    let bounty_i = col("bountyClaimed");
    let lp_i = col("loyaltyPoints");
    let kr_i = col("killRightSupplied");

    assert_eq!(rows.len(), 3);
    // Most recent first: 990_001 (ts 2_000)
    assert_eq!(rows[0][kid_i], PyRep::Integer(990_001));
    assert_eq!(rows[0][war_i], PyRep::Integer(701));
    assert_eq!(rows[0][lost_i], PyRep::Real(total_val));
    assert_eq!(rows[0][dest_i], PyRep::Real(total_val - unit_price));
    assert_eq!(rows[0][bounty_i], PyRep::None);
    assert_eq!(rows[0][lp_i], PyRep::None);
    assert_eq!(rows[0][kr_i], PyRep::None);

    // 990_002 (ts 1_800): accepted ally kill in war 701, no detailed killmail breakdown
    assert_eq!(rows[1][kid_i], PyRep::Integer(990_002));
    assert_eq!(rows[1][war_i], PyRep::Integer(701));
    assert_eq!(rows[1][lost_i], PyRep::Real(500_000.0));
    assert_eq!(rows[1][dest_i], PyRep::None);

    // 990_003 (ts 1_400): after war 702 finished at 1_200, zero iskLost
    assert_eq!(rows[2][kid_i], PyRep::Integer(990_003));
    assert_eq!(rows[2][war_i], PyRep::None);
    assert_eq!(rows[2][lost_i], PyRep::None);
    assert_eq!(rows[2][dest_i], PyRep::None);
}

#[test]
fn char_unbound_mgr_gm_gates_deny_player_and_qa_without_mutation() {
    use eve_machonet::service_roles::{
        ROLE_ADMIN, ROLE_GMH, ROLE_GML, ROLE_GMS, ROLE_PLAYER_DEFAULT, ROLE_QA,
    };

    let ctx = test_ctx();
    let mut sess = auth_session(88, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();
    let before = ctx.db.snapshot_tables();

    let expected_err_code = "InsufficientRole";
    for bad_role in [0u64, ROLE_PLAYER_DEFAULT, ROLE_QA] {
        sess.role = bad_role;
        for method in ["GetQAStarterSystemIDs", "ToggleValidation"] {
            let err = handle(&ctx, &mut sess, "charUnboundMgr", method, &[], &kw)
                .unwrap()
                .expect_err(&format!("charUnboundMgr.{method} must reject role {bad_role:#x}"));
            assert_eq!(err.code, expected_err_code);
        }
    }
    assert_eq!(ctx.db.snapshot_tables(), before);
    let stored_before = ctx
        .db
        .with_snapshot(|s| load_store(s).validation_toggled_by_user.get(&88).copied());
    assert_eq!(stored_before, None);

    for ok_role in [ROLE_GMH, ROLE_GML, ROLE_GMS, ROLE_ADMIN] {
        sess.role = ok_role;
        let systems = handle(&ctx, &mut sess, "charUnboundMgr", "GetQAStarterSystemIDs", &[], &kw)
            .unwrap()
            .unwrap();
        assert_eq!(systems.as_list().map(|l| l.len()), Some(4));
        assert!(matches!(
            handle(&ctx, &mut sess, "charUnboundMgr", "ToggleValidation", &[], &kw)
                .unwrap()
                .unwrap(),
            PyRep::Bool(_)
        ));
    }
}

#[test]
fn char_mgr_combat_log_excludes_expired_ally_kills() {
    let ctx = test_ctx();
    let uid = 1_u64;
    let victim_cid = 90_000_001_i64;
    let attacker_cid = 90_000_002_i64;
    let agg_corp = 98_110_001_i64;
    let def_corp = 98_110_002_i64;
    let ally_corp = 98_110_003_i64;
    let mut sess = auth_session(uid, victim_cid, ally_corp, 30_000_142);
    let kw = HashMap::new();

    let eff = 1_500_u64;
    let exp = 2_100_u64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            705,
            eve_db::WarRecord {
                war_id: 705,
                aggressor_corp_id: agg_corp,
                defender_corp_id: def_corp,
                mutual: false,
                started_unix: 1_000,
                active: true,
            },
        );
        snap.war_state.negotiations.push(eve_db::PersistedWarNegotiation {
            id: 51,
            war_id: 705,
            from_corp: ally_corp,
            kind: eve_db::PersistedNegKind::Ally,
            isk: eve_db::IskCents::from_isk(0),
            message: String::new(),
            status: "Accepted".into(),
            effective_unix: Some(eff),
        });
        snap.war_state.ally_expires_unix.insert((705, ally_corp), exp);

        for (kill_id, killed_unix) in [
            (990_101_i64, eff - 1),
            (990_102_i64, exp - 1),
            (990_103_i64, exp),
            (990_104_i64, exp + 50),
        ] {
            snap.killmails.insert(
                kill_id,
                KillmailRecord {
                    kill_id,
                    victim_character_id: victim_cid,
                    attacker_character_id: attacker_cid,
                    solar_system_id: 30_000_142,
                    ship_type_id: 587,
                    total_value_isk: 250_000.0,
                    killed_unix,
                    victim_corp_id: ally_corp,
                    attacker_corp_id: agg_corp,
                    ..Default::default()
                },
            );
        }
    });

    let rep = handle(
        &ctx,
        &mut sess,
        "charMgr",
        "GetRecentShipKillsAndLosses",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Rowset { columns, rows } = rep else {
        panic!("expected Rowset");
    };
    let col = |name: &str| columns.iter().position(|c| c == name).expect(name);
    let kid_i = col("killID");
    let war_i = col("warID");

    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0][kid_i], PyRep::Integer(990_104));
    assert_eq!(rows[0][war_i], PyRep::None);
    assert_eq!(rows[1][kid_i], PyRep::Integer(990_103));
    assert_eq!(rows[1][war_i], PyRep::None);
    assert_eq!(rows[2][kid_i], PyRep::Integer(990_102));
    assert_eq!(rows[2][war_i], PyRep::Integer(705));
    assert_eq!(rows[3][kid_i], PyRep::Integer(990_101));
    assert_eq!(rows[3][war_i], PyRep::None);
}
