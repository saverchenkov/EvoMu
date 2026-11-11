use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use eve_db::{CorporationRecord, InventoryFlag, IskCents, KillmailRecord, MarketOrderRecord};
use std::collections::HashMap;

fn dict_i64(r: &PyRep, k: &str) -> Option<i64> {
    r.dict_get(k).and_then(|v| v.as_i64())
}

fn setup_corp_registry_ctx() -> (ServerContext, SessionState, i64, i64, i64) {
    let (ctx, mut sess, char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_010_i64;
    let station_id = 60_003_760_i64;
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.corp_role = u64::MAX;
    sess.corp_account_key = 1000;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        let mut corp = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = corp_id;
            __corp.name = "Apex Frontier Syndicate".into();
            __corp.ticker = "APEX".into();
            __corp.ceo_id = char_id;
            __corp.alliance_id = None;
            __corp.tax_rate = 0.05;
            __corp.members = vec![char_id].into_iter().collect();
            __corp.member_roles = [(char_id, u64::MAX)].into_iter().collect();
            __corp.hq_station_id = Some(station_id);
            __corp.set_division_balance(1, IskCents::from_isk(100_000_000));
            __corp };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });
    let _ = sys_id;
    (ctx, sess, char_id, corp_id, station_id)
}

#[tokio::test]
async fn test_handle_corp_hangar_invbroker_list_and_add() {
    let (ctx, mut sess, char_id, corp_id, station_id) = setup_corp_registry_ctx();
    let kwargs = HashMap::new();
    let corp_item_id = 910_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            corp_item_id,
            make_fighter_item(
                corp_item_id,
                34,
                corp_id,
                station_id,
                InventoryFlag::CorpSAG1,
                500,
            ),
        );
    });

    // CEO can query division 1 (flag 116)
    let listed = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(listed, PyRep::ObjectEx { .. }));

    // Non-CEO without role is denied on List and Add
    let member_id = 90_000_077_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.members.insert(member_id);
            c.member_roles.insert(member_id, 0);
        }
    });
    let mut member_sess = sess.clone();
    member_sess.character_id = Some(member_id);
    member_sess.corp_role = 0;

    let denied_list = handle_corp_hangar_invbroker(
        &ctx,
        &mut member_sess,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kwargs,
    )
    .await
    .unwrap();
    assert!(denied_list.is_err());

    let denied_add = handle_corp_hangar_invbroker(
        &ctx,
        &mut member_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(corp_item_id)],
        &kwargs,
    )
    .await
    .unwrap();
    assert!(denied_add.is_err());
    let _ = char_id;
}

#[test]
fn test_corp_registry_handle_methods_and_helpers() {
    let (ctx, mut sess, char_id, corp_id, station_id) = setup_corp_registry_ctx();
    let kwargs = HashMap::new();

    // Seed a killmail and a corporate market order
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(
            501,
            KillmailRecord {
                kill_id: 501,
                solar_system_id: 30_000_142,
                killed_unix: 1_700_000_000,
                victim_character_id: 90_000_099,
                victim_corp_id: corp_id,
                victim_alliance_id: None,
                ship_type_id: 600,
                attacker_character_id: char_id,
                attacker_corp_id: corp_id,
                attacker_alliance_id: None,
                total_value_isk: 10_000_000.0,
            },
        );
        snap.market_orders.insert(
            701,
            MarketOrderRecord {
                order_id: 701,
                type_id: 34,
                owner_id: corp_id,
                station_id,
                region_id: 10_000_002,
                price: IskCents::from_isk(5),
                vol_entered: 1000,
                vol_remaining: 1000,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents::ZERO,
            },
        );
    });

    // GetEveOwners
    let owners = handle(&ctx, &mut sess, "corpRegistry", "GetEveOwners", &[], &kwargs)
        .unwrap()
        .unwrap();
    assert!(!matches!(owners, PyRep::None));

    // GetRecentKills & GetRecentLosses
    let kills = handle(&ctx, &mut sess, "corpRegistry", "GetRecentKills", &[], &kwargs)
        .unwrap()
        .unwrap();
    assert!(!matches!(kills, PyRep::None));
    let losses = handle(&ctx, &mut sess, "corpRegistry", "GetRecentLosses", &[], &kwargs)
        .unwrap()
        .unwrap();
    assert!(!matches!(losses, PyRep::None));

    // GetSuggestedAllianceShortNames & GetSuggestedTickerNames
    let short_names = handle(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetSuggestedAllianceShortNames",
        &[PyRep::String("Northern Coalition Dot".into())],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(short_items) = short_names else {
        panic!("expected List from GetSuggestedAllianceShortNames");
    };
    assert!(!short_items.is_empty());
    assert!(short_items.iter().all(|item| {
        item.dict_get("shortName")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    }));

    let tickers = handle(
        &ctx,
        &mut sess,
        "corpRegistry",
        "GetSuggestedTickerNames",
        &[PyRep::String("Stellar Industrial Logistics".into())],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(tickers, PyRep::List(ref l) if !l.is_empty()));

    // SetAccountKey
    let set_key = handle(
        &ctx,
        &mut sess,
        "corpRegistry",
        "SetAccountKey",
        &[PyRep::Integer(1002)],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(set_key, PyRep::None);
    assert_eq!(sess.corp_account_key, 1002);

    // ExecuteActions (batch role update & legacy tuple format)
    let batch_exec = handle(
        &ctx,
        &mut sess,
        "corpRegistry",
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(char_id)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("add".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(128),
            ])]),
        ],
        &kwargs,
    )
    .unwrap()
    .unwrap();
    assert_eq!(batch_exec, PyRep::Bool(true));

    // Bind & UnBind
    let bound = handle(&ctx, &mut sess, "corpRegistry", "Bind", &[PyRep::Integer(corp_id)], &kwargs)
        .unwrap()
        .unwrap();
    let bid = bound.as_i64().unwrap();
    let unbound = handle(&ctx, &mut sess, "corpRegistry", "UnBind", &[PyRep::Integer(bid)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(unbound, PyRep::None);

    // Supplemental helpers
    let mkt = handle_get_corp_market_info(&ctx, &mut sess, &[PyRep::Integer(corp_id)])
        .unwrap()
        .unwrap();
    assert!(!matches!(mkt, PyRep::None));

    let info_win = handle_get_info_window_data_for_char(&ctx, &mut sess, &[PyRep::Integer(char_id)])
        .unwrap()
        .unwrap();
    assert_eq!(dict_i64(&info_win, "corporationID"), Some(corp_id));

    let member = handle_get_member(&ctx, &mut sess, &[PyRep::Integer(char_id)])
        .unwrap()
        .unwrap();
    assert_eq!(dict_i64(&member, "characterID"), Some(char_id));
    assert_eq!(dict_i64(&member, "blockRoles"), Some(0));
    assert_eq!(dict_i64(&member, "blocksRoles"), Some(0));

    let member_self = handle_get_member(&ctx, &mut sess, &[])
        .unwrap()
        .unwrap();
    assert_eq!(dict_i64(&member_self, "characterID"), Some(char_id));
    assert_eq!(dict_i64(&member_self, "blockRoles"), Some(0));
    assert_eq!(dict_i64(&member_self, "blocksRoles"), Some(0));

    let npc_char_id = 90_000_088_i64;
    let npc_corp_id = 1_000_006_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut ch_npc) = snap.characters.get(&char_id).cloned() {
            ch_npc.character_id = npc_char_id;
            ch_npc.user_id = npc_char_id as u64;
            ch_npc.name = "NPC Corp Pilot".into();
            ch_npc.corporation_id = npc_corp_id;
            snap.characters.insert(npc_char_id, ch_npc);
        }
    });
    let mut npc_sess = sess.clone();
    npc_sess.character_id = Some(npc_char_id);
    npc_sess.corporation_id = Some(npc_corp_id);
    npc_sess.corp_role = 0;
    let npc_member = handle_get_member(&ctx, &mut npc_sess, &[PyRep::Integer(npc_char_id)])
        .unwrap()
        .unwrap();
    assert_eq!(dict_i64(&npc_member, "characterID"), Some(npc_char_id));
    assert_eq!(dict_i64(&npc_member, "corporationID"), Some(npc_corp_id));
    assert_eq!(dict_i64(&npc_member, "blockRoles"), Some(0));
    assert_eq!(dict_i64(&npc_member, "blocksRoles"), Some(0));

    // #787: title_id_to_bit boundary and bitmask resolution + seed_default_corp_titles idempotency
    use eve_db::snapshot_ext::title_id_to_bit;
    assert_eq!(title_id_to_bit(i64::MIN), 0);
    assert_eq!(title_id_to_bit(-1), 0);
    assert_eq!(title_id_to_bit(0), 0);
    assert_eq!(title_id_to_bit(1), 1);
    assert_eq!(title_id_to_bit(2), 2);
    assert_eq!(title_id_to_bit(3), 4);
    assert_eq!(title_id_to_bit(5), 16);
    assert_eq!(title_id_to_bit(63), 1u64 << 62);
    assert_eq!(title_id_to_bit(64), 64);
    assert_eq!(title_id_to_bit(65), 0);
    assert_eq!(title_id_to_bit(100), 0);
    assert_eq!(title_id_to_bit(128), 128);
    assert_eq!(title_id_to_bit(32_768), 32_768);
    assert_eq!(title_id_to_bit(65_536), 65_536);

    ctx.db.with_mut_snapshot(|snap| {
        snap.seed_default_corp_titles(corp_id);
        let state = snap.corp_governance.entry(corp_id).or_default();
        state.titles.insert(4, ("Wing Commander".into(), 0));
        state.titles.insert(32_768, ("Fleet Admiral".into(), 0));
        state.member_titles.insert(char_id, 1 | 4 | 32_768);
        snap.seed_default_corp_titles(corp_id);
        assert_eq!(snap.corp_titles[&corp_id].len(), 16);
        assert_eq!(snap.corp_governance[&corp_id].titles[&4].0, "Wing Commander");
    });
    let titled_member = handle_get_member(&ctx, &mut sess, &[PyRep::Integer(char_id)])
        .unwrap()
        .unwrap();
    assert_eq!(
        titled_member.dict_get("title").and_then(PyRep::as_str),
        Some("Wing Commander, Fleet Admiral")
    );

    let tracking =
        handle_get_member_tracking_info(&ctx, &mut sess, "GetMemberTrackingInfo", &[])
            .unwrap()
            .unwrap();
    let PyRep::Rowset { columns: t_cols, rows: t_rows } = tracking else {
        panic!("expected Rowset from GetMemberTrackingInfo");
    };
    let idx = |name: &str| t_cols.iter().position(|c| c == name).expect(name);
    assert_eq!(t_rows.len(), 1);
    assert_eq!(t_rows[0][idx("lastOnline")].as_f64(), Some(-1.0));
    assert_eq!(t_rows[0][idx("online")].as_bool(), Some(true));
    assert_eq!(t_rows[0][idx("factionID")], PyRep::None);
    assert!(t_rows[0][idx("startDateTime")].as_i64().unwrap_or(0) >= 116_444_736_000_000_000);
    assert!(t_rows[0][idx("logonDateTime")].as_i64().unwrap_or(0) >= 116_444_736_000_000_000);

    // Offline member with prior login/logout and never-logged-in member
    let off_char_id = 90_000_002_i64;
    let never_char_id = 90_000_003_i64;
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(off_char_id);
            corp.members.insert(never_char_id);
        }
        if let Some(ch2) = snap.characters.get_mut(&off_char_id) {
            ch2.corporation_id = corp_id;
        }
        if let Some(ch3) = snap.characters.get_mut(&never_char_id) {
            ch3.corporation_id = corp_id;
        } else if let Some(mut ch3) = snap.characters.get(&char_id).cloned() {
            ch3.character_id = never_char_id;
            ch3.user_id = never_char_id as u64;
            ch3.name = "Never Logged In".into();
            ch3.corporation_id = corp_id;
            snap.characters.insert(never_char_id, ch3);
        }
        snap.character_login_stats.remove(&never_char_id);
        snap.character_login_stats.insert(
            off_char_id,
            eve_db::CharacterLoginStats {
                last_login_unix: now_u.saturating_sub(7200),
                last_logout_unix: now_u.saturating_sub(3600),
                ..Default::default()
            },
        );
    });
    let tracking_all =
        handle_get_member_tracking_info(&ctx, &mut sess, "GetMemberTrackingInfo", &[])
            .unwrap()
            .unwrap();
    let PyRep::Rowset { rows: all_rows, .. } = tracking_all else {
        panic!("expected Rowset");
    };
    let off_row = all_rows.iter().find(|r| r[0].as_i64() == Some(off_char_id)).unwrap();
    let off_hours = off_row[idx("lastOnline")].as_f64().unwrap();
    assert!((off_hours - 1.0).abs() < 0.01, "expected ~1.0 hours offline, got {off_hours}");
    assert!(off_row[idx("logoffDateTime")].as_i64().unwrap_or(0) >= 116_444_736_000_000_000);
    let never_row = all_rows.iter().find(|r| r[0].as_i64() == Some(never_char_id)).unwrap();
    assert_eq!(never_row[idx("lastOnline")], PyRep::None);
}

#[test]
fn set_account_key_noop_skips_session_change_timer() {
    let (ctx, mut sess, _char_id, _corp_id, _station_id) = setup_corp_registry_ctx();
    let kwargs = HashMap::new();

    // No-op SetAccountKey(1000) returns None without starting the 10s timer.
    let res1 = handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1000)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(res1, PyRep::None);
    assert_eq!(sess.corp_account_key, 1000);

    // Immediate actual change 1000 -> 1001 succeeds without SessionChangeInProgress.
    let res2 = handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(res2, PyRep::None);
    assert_eq!(sess.corp_account_key, 1001);

    // No-op repeat of current key (1001) during active timer also succeeds without triggering cooldown.
    let res3 = handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)], &kwargs)
        .unwrap()
        .unwrap();
    assert_eq!(res3, PyRep::None);

    // Changing to 1002 within 10s fails with SessionChangeInProgress.
    let err = handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1002)], &kwargs)
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "SessionChangeInProgress");
}

#[tokio::test]
async fn test_fleet_burst_fallback_all_branches() {
    use eve_db::{FleetMemberRecord, FleetRecord, FleetRole};
    let (ctx, sess, char_id, _corp_id, _station_id) = setup_corp_registry_ctx();

    // Unauthenticated session in fleet -> Some(Err(Unauthenticated))
    let mut unauth = SessionState::default();
    unauth.fleet_id = Some(9001);
    let res_unauth = corp_registry_burst_fallback::handle_fleet_burst_fallback(
        &ctx,
        &mut unauth,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(1234)],
    )
    .await;
    assert!(matches!(res_unauth, Some(Err(_))));

    // Active ship is a structure -> defers (None)
    let struct_id = 1_050_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832,
                owner_corp_id: 98_000_010,
                solar_system_id: 30_000_142,
                name: "Test Astrahus".into(),
                position: [0.0; 3],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
    });
    let mut struct_sess = sess.clone();
    struct_sess.fleet_id = Some(9001);
    struct_sess.ship_id = Some(struct_id);
    assert!(corp_registry_burst_fallback::handle_fleet_burst_fallback(
        &ctx,
        &mut struct_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(1234)],
    )
    .await
    .is_none());

    // Non-burst module returns None
    let non_burst_mod = 950_001_i64;
    let burst_mod = 950_002_i64;
    let charge_id = 950_003_i64;
    let ship_id = 950_010_i64;
    let other_ship = 950_011_i64;
    let far_member = 90_000_088_i64;
    let far_ship = 950_012_i64;
    let unspawned_member = 90_000_089_i64;
    let unspawned_ship = 950_013_i64;
    let burst_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1770)
        .map(|t| t.type_id)
        .unwrap_or(42_526);
    let sys_id = sess.solar_system_id.unwrap_or(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            non_burst_mod,
            make_fighter_item(non_burst_mod, 34, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
        snap.items.insert(
            burst_mod,
            make_fighter_item(burst_mod, burst_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            charge_id,
            make_fighter_item(charge_id, 42_832, char_id, ship_id, InventoryFlag::HiSlot0, 20),
        );
        snap.items.insert(
            other_ship,
            make_fighter_item(other_ship, 587, 90_000_999, sys_id as i64, InventoryFlag::None, 1),
        );
        let base_ch = snap.characters.get(&char_id).cloned().unwrap();
        for (mcid, msid) in [(far_member, far_ship), (unspawned_member, unspawned_ship)] {
            let mut ch = base_ch.clone();
            ch.character_id = mcid;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = msid;
            snap.characters.insert(mcid, ch);
        }
        let mut fl = FleetRecord {
            fleet_id: 9001,
            boss_id: char_id,
            ..Default::default()
        };
        fl.members.extend([char_id, far_member, unspawned_member]);
        for m in [char_id, far_member, unspawned_member] {
            fl.member_details.insert(
                m,
                FleetMemberRecord {
                    character_id: m,
                    role: if m == char_id { FleetRole::FleetCommander } else { FleetRole::SquadMember },
                    wing_id: None,
                    squad_id: None,
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "M".into(),
                    join_time_unix: 1000,
                },
            );
        }
        snap.fleets.insert(9001, fl);
    });

    let mut b_sess = sess.clone();
    b_sess.fleet_id = Some(9001);
    b_sess.station_id = None;
    b_sess.structure_id = None;
    b_sess.ship_id = Some(ship_id);

    // Non-burst module -> None
    assert!(corp_registry_burst_fallback::handle_fleet_burst_fallback(
        &ctx,
        &b_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(non_burst_mod)],
    )
    .await
    .is_none());

    // Missing solar system -> NoSolarSystem
    let mut no_sys = b_sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        corp_registry_burst_fallback::handle_fleet_burst_fallback(
            &ctx,
            &no_sys,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(burst_mod)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoSolarSystem"
    );

    // Ship ownership mismatch -> ShipOwnershipViolation
    let mut wrong_ship = b_sess.clone();
    wrong_ship.ship_id = Some(other_ship);
    assert_eq!(
        corp_registry_burst_fallback::handle_fleet_burst_fallback(
            &ctx,
            &wrong_ship,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(burst_mod)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipOwnershipViolation"
    );

    // Spawn booster ball and out-of-range fleetmate ball
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;
    let _ = sys_h
        .spawn_ball(eve_destiny::Ball {
            id: far_ship,
            type_id: 587,
            position: glam::DVec3::new(1_000_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        })
        .await;

    // Activate happy path with charge + far_member (out of range) + unspawned_member (not in ballpark)
    let act = corp_registry_burst_fallback::handle_fleet_burst_fallback(
        &ctx,
        &b_sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(burst_mod)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(act.dict_get("active"), Some(&PyRep::Bool(true)));
    assert_eq!(act.dict_get("chargeTypeID"), Some(&PyRep::Integer(42_832)));

    // Deactivate happy path
    let deact = corp_registry_burst_fallback::handle_fleet_burst_fallback(
        &ctx,
        &b_sess,
        "dogmaIM",
        "Deactivate",
        &[PyRep::Integer(burst_mod)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(deact.dict_get("active"), Some(&PyRep::Bool(false)));
    assert!(!ctx.db.with_snapshot(|s| s.module_active_states.contains(&burst_mod)));
}

#[tokio::test]
async fn test_corp_registry_ops_remaining_branches() {
    use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};

    let (ctx, mut sess, char_id, corp_id, station_id) = setup_corp_registry_ctx();
    let other_char_id = char_id + 99;
    let kw = HashMap::new();
    let _now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut other = snap.characters.get(&char_id).cloned().unwrap();
        other.character_id = other_char_id;
        other.name = "Corp Member 2".into();
        other.corporation_id = corp_id;
        other.bloodline_id = 19; // maps to type_id 34574
        snap.characters.insert(other_char_id, other);
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.members.insert(other_char_id);
            corp.member_roles.insert(other_char_id, corp_roles::ROLE_HANGAR_CAN_QUERY[0]);
        }
        snap.items.insert(
            777_000_001,
            InventoryItemRecord {
                item_id: 777_000_001,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            777_000_002,
            InventoryItemRecord {
                item_id: 777_000_002,
                type_id: 34,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // 1. handle_corp_hangar_invbroker: List without location -> InvalidLocation; wrong docked bound loc -> BoundObjectOwnershipViolation; Add item not in corp hangar -> None; Add by non-CEO without take role -> CorpRoleDenied
    let mut no_loc_sess = sess.clone();
    no_loc_sess.station_id = None;
    no_loc_sess.structure_id = None;
    no_loc_sess.ship_id = None;
    let err_no_loc = handle_corp_hangar_invbroker(
        &ctx,
        &mut no_loc_sess,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_loc.code, "InvalidLocation");

    let mut wrong_dock_sess = sess.clone();
    wrong_dock_sess.station_id = Some(station_id + 55);
    wrong_dock_sess.corporation_id = None; // covers DB fallback
    let bound_other_station = crate::router::BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "invbroker".into(),
        object_id: station_id,
        oid_str: String::new(),
        ref_id: 1,
    };
    let err_bound = handle_corp_hangar_invbroker(
        &ctx,
        &mut wrong_dock_sess,
        Some(&bound_other_station),
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bound.code, "BoundObjectOwnershipViolation");

    // Add: personal item defers (None)
    assert!(
        handle_corp_hangar_invbroker(
            &ctx,
            &mut sess,
            None,
            "invbroker",
            "Add",
            &[PyRep::Integer(777_000_002)],
            &kw,
        )
        .await
        .is_none()
    );
    // Add: non-CEO member trying to take corp SAG1 item without ROLE_HANGAR_CAN_TAKE[0] -> CorpRoleDenied
    let mut non_ceo_sess = sess.clone();
    non_ceo_sess.character_id = Some(other_char_id);
    non_ceo_sess.corporation_id = None; // covers fallback in Add
    let err_take = handle_corp_hangar_invbroker(
        &ctx,
        &mut non_ceo_sess,
        None,
        "invbroker",
        "Add",
        &[PyRep::Integer(777_000_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_take.code, "CorpRoleDenied");

    // 2. GetEveOwners: foreign corp denied, non-corp fallback, bloodlines
    assert_eq!(
        handle(&ctx, &mut sess, "corpRegistry", "GetEveOwners", &[PyRep::Integer(98_999_999)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    let mut sess_no_corp = sess.clone();
    sess_no_corp.corporation_id = None;
    // Test multiple bloodlines + character not in corporations map
    ctx.db.with_mut_snapshot(|snap| {
        for bl in [2u32, 3, 4, 5, 6, 7, 8, 11, 12, 13, 14] {
            let cid = char_id + 1000 + bl as i64;
            let mut c = snap.characters.get(&char_id).cloned().unwrap();
            c.character_id = cid;
            c.corporation_id = 98_765_432; // corp not inserted in snap.corporations
            c.bloodline_id = bl;
            snap.characters.insert(cid, c);
        }
    });
    let mut ghost_corp_sess = sess.clone();
    ghost_corp_sess.character_id = Some(char_id + 1002);
    ghost_corp_sess.corporation_id = None;
    let owners_ghost = handle(&ctx, &mut ghost_corp_sess, "corpRegistry", "GetEveOwners", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(owners_ghost, PyRep::Rowset { .. }));

    // 3. GetSuggestedAllianceShortNames & GetSuggestedTickerNames edge inputs
    let short_alli = handle(&ctx, &mut sess, "corpRegistry", "GetSuggestedAllianceShortNames", &[PyRep::String("Go".into())], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(short_alli, PyRep::List(v) if v == vec![PyRep::keyval([("shortName", PyRep::String("ALLI".into()))])]));
    let three_word_alli = handle(&ctx, &mut sess, "corpRegistry", "GetSuggestedAllianceShortNames", &[PyRep::String("Alpha Beta Gamma".into())], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(three_word_alli, PyRep::List(v) if v.len() == 2));

    let vowel_ticker = handle(&ctx, &mut sess, "corpRegistry", "GetSuggestedTickerNames", &[PyRep::String("AE".into())], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(vowel_ticker, PyRep::List(v) if !v.is_empty()));
    let empty_ticker = handle(&ctx, &mut sess, "corpRegistry", "GetSuggestedTickerNames", &[PyRep::String("   ".into())], &kw)
        .unwrap()
        .unwrap();
    assert!(matches!(empty_ticker, PyRep::List(v) if !v.is_empty()));

    // 4. SetAccountKey: session change timer conflict, non-existent corp non-1000 key denied, valid non-1000 changes session
    assert_eq!(
        handle(&ctx, &mut ghost_corp_sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    assert!(
        handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1001)], &kw)
            .unwrap()
            .is_ok()
    );
    // Immediate second call hits 10s SessionChangeInProgress timer
    assert_eq!(
        handle(&ctx, &mut sess, "corpRegistry", "SetAccountKey", &[PyRep::Integer(1002)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );

    // 5. ExecuteActions: v24 batch modes (int verbs, grantable/scoped roles, titleMask director check, baseID, divisionID, squadronID, accountKey, errors) and legacy corp actions
    // Grant a title with DIRECTOR and DELAYED role bits
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.titles.insert(1, ("TitleOne".into(), corp_roles::ROLE_DIRECTOR));
        gov.titles.insert(2, ("TitleDiv".into(), corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK));
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(other_char_id, corp_roles::ROLE_DIRECTOR);
        }
    });
    let members_arg = PyRep::List(vec![PyRep::Integer(other_char_id)]);
    // Integer verb 1 (add), 2 (remove), 3 (set) + negative val error + invalid action error + scoped & grantable roles
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "corpRegistry",
            "ExecuteActions",
            &[members_arg.clone(), PyRep::List(vec![PyRep::Integer(99)])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidParameter"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "corpRegistry",
            "ExecuteActions",
            &[members_arg.clone(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("badverb".into()), PyRep::String("roles".into()), PyRep::Integer(0)])])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidParameter"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "corpRegistry",
            "ExecuteActions",
            &[members_arg.clone(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(99), PyRep::String("roles".into()), PyRep::Integer(0)])])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidParameter"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "corpRegistry",
            "ExecuteActions",
            &[members_arg.clone(), PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("roles".into()), PyRep::Integer(-5)])])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    // Director trying to assign title 1 (which includes ROLE_DIRECTOR) to member -> CrpAccessDenied
    let mut dir_sess = sess.clone();
    dir_sess.character_id = Some(other_char_id);
    assert_eq!(
        handle(
            &ctx,
            &mut dir_sess,
            "corpRegistry",
            "ExecuteActions",
            &[
                PyRep::List(vec![PyRep::Integer(other_char_id)]),
                PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("set".into()), PyRep::String("titleMask".into()), PyRep::Integer(1)])]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    // CEO succeeds updating grantableRoles, rolesAtHQ/grantableRolesAtHQ, rolesAtBase/grantableRolesAtBase, rolesAtOther/grantableRolesAtOther, titleMask, accountKey, baseID, divisionID, squadronID
    let v24_all_ops = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("roles".into()), PyRep::Integer(corp_roles::ROLE_TRADER as i64)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("grantableRoles".into()), PyRep::Integer(corp_roles::ROLE_TRADER as i64)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("rolesAtHQ".into()), PyRep::Integer(corp_roles::ROLE_HANGAR_CAN_TAKE[0] as i64)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("grantableRolesAtHQ".into()), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("rolesAtBase".into()), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("grantableRolesAtBase".into()), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("rolesAtOther".into()), PyRep::Integer(4)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("grantableRolesAtOther".into()), PyRep::Integer(4)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("titleMask".into()), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("accountKey".into()), PyRep::Integer(1002)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("baseID".into()), PyRep::Integer(station_id)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("divisionID".into()), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::Integer(3), PyRep::String("squadronID".into()), PyRep::Integer(3)]),
        PyRep::Tuple(vec![PyRep::Integer(2), PyRep::String("unknownProp".into()), PyRep::Integer(0)]),
    ]);
    assert!(handle(&ctx, &mut sess, "corpRegistry", "ExecuteActions", &[members_arg.clone(), v24_all_ops], &kw).unwrap().is_ok());

    // Legacy ExecuteActions: SetAccountKey, SetTitle (including clearing title), SetNote, SetFriendlyFire, custom string action
    let legacy_ops = PyRep::List(vec![
        PyRep::Tuple(vec![PyRep::String("SetAccountKey".into()), PyRep::Integer(other_char_id), PyRep::Integer(1003)]),
        PyRep::Tuple(vec![PyRep::String("SetTitle".into()), PyRep::Integer(other_char_id), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::String("SetNote".into()), PyRep::Integer(other_char_id), PyRep::String("Lead".into())]),
        PyRep::Tuple(vec![PyRep::String("SetFriendlyFire".into()), PyRep::Integer(1)]),
        PyRep::Tuple(vec![PyRep::String("CustomOp".into())]),
        PyRep::String("RawActionLog".into()),
    ]);
    assert!(handle(&ctx, &mut sess, "corpRegistry", "ExecuteActions", &[PyRep::Integer(corp_id), legacy_ops], &kw).unwrap().is_ok());
    // Clear title on member
    assert!(
        handle(
            &ctx,
            &mut sess,
            "corpRegistry",
            "ExecuteActions",
            &[PyRep::Tuple(vec![PyRep::String("SetTitle".into()), PyRep::Integer(other_char_id), PyRep::Integer(0)])],
            &kw,
        )
        .unwrap()
        .is_ok()
    );
    // Director cannot grant ROLE_DIRECTOR title via legacy SetTitle -> CrpAccessDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .titles
            .insert(1, ("DirectorTitle".into(), corp_roles::ROLE_DIRECTOR));
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(other_char_id, corp_roles::ROLE_DIRECTOR);
        }
    });
    assert_eq!(
        handle(
            &ctx,
            &mut dir_sess,
            "corpRegistry",
            "ExecuteActions",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("SetTitle".into()),
                PyRep::Integer(other_char_id),
                PyRep::Integer(1),
            ])])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Bind & UnBind error paths
    let mut unauth = SessionState::default();
    assert!(handle(&ctx, &mut unauth, "corpRegistry", "Bind", &[], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut unauth, "corpRegistry", "UnBind", &[], &kw).unwrap().is_err());
    assert!(handle(&ctx, &mut sess, "corpRegistry", "UnBind", &[PyRep::Integer(999_888)], &kw).unwrap().is_err());

    // 6. handle_get_corp_market_info: CrpAccessDenied for foreign corp + sell/buy orders aggregation
    assert_eq!(
        handle_get_corp_market_info(&ctx, &mut sess, &[PyRep::Integer(98_999_999)])
            .unwrap()
            .unwrap_err()
            .code,
        "CrpAccessDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            5_000_001,
            MarketOrderRecord {
                order_id: 5_000_001,
                type_id: 34,
                station_id,
                region_id: 10_000_002,
                owner_id: corp_id,
                price: IskCents::from_isk(100),
                vol_remaining: 50,
                vol_entered: 50,
                min_volume: 1,
                is_bid: false,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            5_000_002,
            MarketOrderRecord {
                order_id: 5_000_002,
                type_id: 34,
                station_id,
                region_id: 10_000_002,
                owner_id: corp_id,
                price: IskCents::from_isk(80),
                vol_remaining: 25,
                vol_entered: 25,
                min_volume: 1,
                is_bid: true,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    let mkt = handle_get_corp_market_info(&ctx, &mut sess, &[PyRep::Integer(corp_id)])
        .unwrap()
        .unwrap();
    assert!(matches!(mkt, PyRep::Rowset { .. }));

    // 7. handle_get_info_window_data_for_char: CharacterNotFound, corp_id_override, active title slots
    assert_eq!(
        handle_get_info_window_data_for_char(&ctx, &mut sess, &[PyRep::Integer(888_777_666)])
            .unwrap()
            .unwrap_err()
            .code,
        "CharacterNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.member_titles.insert(char_id, 1);
    });
    let info_corp = handle_get_info_window_data_for_char(&ctx, &mut sess, &[PyRep::Integer(corp_id)])
        .unwrap()
        .unwrap();
    assert_eq!(info_corp.dict_get("corporationID"), Some(&PyRep::Integer(corp_id)));

    // 8. handle_get_member: corp_id >= 98M -> None, NotCorpMember error, pending_role_grants matured, titles
    assert!(handle_get_member(&ctx, &mut sess, &[PyRep::Integer(corp_id)]).is_none());
    assert_eq!(
        handle_get_member(&ctx, &mut sess, &[PyRep::Integer(90_888_777)])
            .unwrap()
            .unwrap_err()
            .code,
        "NotCorpMember"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.pending_role_grants.insert(other_char_id, (corp_roles::ROLE_TRADER, 1)); // already matured
        gov.member_titles.insert(other_char_id, 1);
    });
    let mem = handle_get_member(&ctx, &mut sess, &[PyRep::Integer(other_char_id)])
        .unwrap()
        .unwrap();
    assert_eq!(mem.dict_get("characterID"), Some(&PyRep::Integer(other_char_id)));
}
