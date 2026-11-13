//! Unit tests for `fleet_social_corp_tail` role updates, delayed hangar grants, blockers, and vote creation.

#[cfg(test)]
#[path = "fleet_social_corp_tail_lifecycle_tests.rs"]
mod fleet_social_corp_tail_lifecycle_tests;

use super::*;
use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use eve_db::corp_roles;

#[test]
fn corp_role_updates_cover_all_payload_shapes_delayed_hangars_and_title_blockers() {
    let ctx = test_ctx_with_clock(1_800_020_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);

    // Seed a role-granting corp title #1 and an office in corp_governance
    super::super::fleet_social_corps::with_corp_gov(&ctx, |g| {
        let st = g.entry(98_000_001).or_default();
        st.titles.insert(
            1,
            ("Quartermaster".into(), corp_roles::ROLE_ACCOUNTANT),
        );
    });

    // Empty args -> InvalidArgument; negative mask -> CrpAccessDenied; non-director caller -> CorpRoleDenied
    assert_eq!(
        handle_update_members(&ctx, &mut ceo_sess, "UpdateMembers", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut ceo_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_002)),
                ("roles", PyRep::Integer(-1)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut mem_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_002)),
                ("roles", PyRep::Integer(0)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Non-CEO director cannot toggle ROLE_DIRECTOR on another member
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98_000_001)
            .unwrap()
            .member_roles
            .insert(90_000_002, corp_roles::ROLE_DIRECTOR);
    });
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut mem_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_002)),
                ("roles", PyRep::Integer(0)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Reset 90_000_002 to 0 roles, then grant immediate role + delayed hangar role + scoped roles + title mask via Dict/KeyVal
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98_000_001)
            .unwrap()
            .member_roles
            .insert(90_000_002, 0);
    });
    let delayed_bit = corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK.trailing_zeros();
    let combined_roles = corp_roles::ROLE_PERSONNEL_MANAGER | (1u64 << delayed_bit);
    let res = handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMembers",
        &[PyRep::List(vec![PyRep::keyval([
            ("characterID", PyRep::Integer(90_000_002)),
            ("roles", PyRep::Integer(combined_roles as i64)),
            ("grantableRoles", PyRep::Integer(corp_roles::ROLE_DIPLOMAT as i64)),
            ("rolesAtHQ", PyRep::Integer(combined_roles as i64)),
            ("grantableRolesAtHQ", PyRep::Integer(0)),
            ("rolesAtBase", PyRep::Integer(corp_roles::ROLE_AUDITOR as i64)),
            ("grantableRolesAtBase", PyRep::Integer(0)),
            ("rolesAtOther", PyRep::Integer(0)),
            ("grantableRolesAtOther", PyRep::Integer(0)),
            ("divisionID", PyRep::Integer(2)),
            ("squadronID", PyRep::Integer(3)),
            ("baseID", PyRep::Integer(60_003_760)),
            ("titleMask", PyRep::Integer(1)),
        ])])],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(u_kv) = res else { panic!() };
    assert_eq!(u_kv.get("characterID").unwrap().as_i64().unwrap(), 90_000_002);
    assert_eq!(u_kv.get("delayed24h").unwrap().as_bool(), Some(true));
    assert_eq!(u_kv.get("updatedCount").unwrap().as_i64(), Some(1));

    // member_kick_blocker catches active roles first
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            member_kick_blocker(snap, 98_000_001, 90_000_002, 1_800_020_010),
            Some("MemberHasRoles")
        );
    });

    // Advance clock past 24h so pending hangar roles mature on next read/promote
    ctx.advance_secs(86_405);
    super::super::fleet_social_corps::promote_due_pending_corp_grants(&ctx, 98_000_001);
    ctx.db.with_snapshot(|snap| {
        let eff = snap.corporations.get(&98_000_001).unwrap().member_roles.get(&90_000_002).copied().unwrap_or(0);
        assert_eq!(eff & (1u64 << delayed_bit), 1u64 << delayed_bit);
    });

    // Positional tuple/row formats (4-elem short row, 14-elem named row, numeric row) also work
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[
            PyRep::Integer(90_000_002),
            PyRep::String("Pilot".into()),
            PyRep::Integer(0),
            PyRep::Integer(1),
        ],
    )
    .unwrap()
    .is_ok());
    // Clear scoped roles too -> now only title #1 (ROLE_ACCOUNTANT) remains -> MemberHasRoleGrantingTitle
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(90_000_002)),
            ("roles", PyRep::Integer(0)),
            ("grantableRoles", PyRep::Integer(0)),
            ("rolesAtHQ", PyRep::Integer(0)),
            ("rolesAtBase", PyRep::Integer(0)),
            ("rolesAtOther", PyRep::Integer(0)),
            ("titleMask", PyRep::Integer(1)),
        ])],
    )
    .unwrap()
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            member_kick_blocker(snap, 98_000_001, 90_000_002, ctx.now_unix()),
            Some("MemberHasRoleGrantingTitle")
        );
    });
}

#[test]
fn insert_vote_case_covers_ceo_election_and_share_creation_branches() {
    let ctx = test_ctx_with_clock(1_800_021_000);
    let _ = seed_player_corps(&ctx);

    // Seed shares for 90_000_002 so member can propose CEO election (vote_type=0)
    super::super::fleet_social_corps::with_corp_gov(&ctx, |g| {
        let st = g.entry(98_000_001).or_default();
        st.shares.insert(90_000_002, 500);
        st.company_shares = 1_000;
    });

    // Non-member rejected
    assert_eq!(
        handle_insert_vote_case(
            &ctx,
            98_000_001,
            90_000_999,
            1_800_021_000,
            &[PyRep::String("Vote".into())],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Shareholder 90_000_002 proposes CEO election with candidate 90_000_001
    let ceo_vote = handle_insert_vote_case(
        &ctx,
        98_000_001,
        90_000_002,
        1_800_021_000,
        &[
            PyRep::String("New CEO".into()),
            PyRep::String("Elect Prime".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(90_000_001)]),
        ],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(cv_kv) = ceo_vote else { panic!() };
    assert_eq!(cv_kv.get("voteType").unwrap().as_i64().unwrap(), 0);
    assert_eq!(cv_kv.get("parameter").unwrap().as_i64().unwrap(), 90_000_001);

    // Non-director shareholder cannot create share issuance vote (vote_type=5); CEO can!
    assert_eq!(
        handle_insert_vote_case(
            &ctx,
            98_000_001,
            90_000_002,
            1_800_021_000,
            &[
                PyRep::String("Mint".into()),
                PyRep::String("Issue 100 shares".into()),
                PyRep::Integer(5),
                PyRep::Integer(100),
            ],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    let share_vote = handle_insert_vote_case(
        &ctx,
        98_000_001,
        90_000_001,
        1_800_021_000,
        &[
            PyRep::String("Mint".into()),
            PyRep::String("Issue 250 shares".into()),
            PyRep::Integer(5),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("Issue 250".into()), PyRep::Integer(250)])]),
        ],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(sv_kv) = share_vote else { panic!() };
    assert_eq!(sv_kv.get("voteType").unwrap().as_i64().unwrap(), 5);
    assert_eq!(sv_kv.get("parameter").unwrap().as_i64().unwrap(), 250);
}

#[test]
fn aggression_settings_serializes_v24_object_and_filetime_ticks_gh794() {
    let ctx = test_ctx_with_clock(1_800_022_000);
    let (mut ceo_sess, _mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001_i64;
    let ceo_id = 90_000_001_i64;
    let now_unix = 1_800_022_000_u64;

    let init_agg = super::super::fleet_social_corp_tail_dispatch::handle_corp_gov_tail(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetAggressionSettings",
        &[PyRep::Integer(corp_id)],
        corp_id,
        ceo_id,
        now_unix,
    )
    .unwrap()
    .unwrap();
    let PyRep::Object { type_name, args } = &init_agg else {
        panic!("expected AggressionSettings PyRep::Object, got {init_agg:?}");
    };
    assert_eq!(
        type_name,
        "crimewatch.corp_aggression.settings.AggressionSettings"
    );
    assert_eq!(args.dict_get("_enableAfter"), Some(&PyRep::None));
    assert_eq!(args.dict_get("_disableAfter"), Some(&PyRep::Integer(0)));

    let encoded = eve_proto::marshal::encode(&init_agg);
    let decoded = eve_proto::marshal::decode(&encoded).expect("decode AggressionSettings");
    assert!(matches!(
        &decoded,
        PyRep::Object { type_name, .. }
            if type_name == "crimewatch.corp_aggression.settings.AggressionSettings"
    ));

    let reg_agg = super::super::fleet_social_corp_tail_dispatch::handle_corp_gov_tail(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(true)],
        corp_id,
        ceo_id,
        now_unix,
    )
    .unwrap()
    .unwrap();
    let expected_ft = crate::sp6_lifecycle::unix_to_filetime(now_unix + 86_400);
    assert_eq!(
        reg_agg.dict_get("_enableAfter"),
        Some(&PyRep::Integer(expected_ft))
    );
    assert_eq!(reg_agg.dict_get("_disableAfter"), Some(&PyRep::Integer(0)));
}

#[test]
fn issue_886_closing_player_corp_liquidates_wallets_orders_and_office_items() {
    let base_ctx = test_ctx_with_clock(1_800_023_000);
    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_issue_886_{}_{}",
        std::process::id(),
        1_800_023_000
    ));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("create temp sqlite dir");
    let db_path = tmp_dir.join("world.sqlite");

    let repo1 = std::sync::Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("open sqlite"));
    let ctx = crate::router::ServerContext::with_sde_and_repository(
        std::sync::Arc::clone(&base_ctx.sde),
        repo1,
    );
    ctx.set_clock(std::sync::Arc::new(eve_db::VirtualServerClock::with_fixed_base(
        1_800_023_000,
    )));
    ctx.seed_demo_world_fixtures();
    let (mut ceo_sess, _mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001_i64;
    let ceo_id = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == ceo_id as u64) {
            let uname = format!("u{}", ceo_id as u64);
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: ceo_id as u64,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(ceo_id as u64, ceo_sess.session_id, Some(ceo_id), true, 1_800_023_000)
        .expect("acquire ceo online lock");
    let station_id = 60_003_760_i64;
    let office_id = 1_000_886_001_i64;
    let box_in_office_id = 1_000_886_002_i64;
    let nested_loot_id = 1_000_886_003_i64;
    let structure_id = 1_000_886_050_i64;
    let struct_mod_id = 1_000_886_051_i64;
    let contract_id = 886_070_i64;
    let contract_item_id = 1_000_886_052_i64;
    let buy_order_id = 886_001_i64;
    let sell_order_id = 886_002_i64;

    ceo_sess.station_id = Some(station_id);
    ceo_sess.structure_id = None;
    super::super::fleet_social_corps::with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.offices.insert(station_id, office_id);
    });

    let initial_ceo_isk = ctx.db.with_mut_snapshot(|snap| {
        // Leave only CEO in corporation so removing CEO closes the corporation.
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.members.retain(|&m| m == ceo_id);
        corp.member_roles.retain(|&m, _| m == ceo_id);
        for d in 1..=7 {
            corp.set_division_balance(d, eve_db::IskCents(0));
        }
        corp.set_division_balance(1, eve_db::IskCents::from_isk(250_000));
        corp.set_division_balance(2, eve_db::IskCents::from_isk(150_000));

        snap.items.insert(
            office_id,
            eve_db::InventoryItemRecord {
                item_id: office_id,
                type_id: 27, // Office
                owner_id: corp_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            box_in_office_id,
            eve_db::InventoryItemRecord {
                item_id: box_in_office_id,
                type_id: 3293, // Medium Standard Container
                owner_id: corp_id,
                location_id: office_id,
                flag: eve_db::InventoryFlag::CorpSAG1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            nested_loot_id,
            eve_db::InventoryItemRecord {
                item_id: nested_loot_id,
                type_id: 34, // Tritanium
                owner_id: corp_id,
                location_id: box_in_office_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
        snap.market_orders.insert(
            buy_order_id,
            eve_db::MarketOrderRecord {
                order_id: buy_order_id,
                owner_id: corp_id,
                station_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: eve_db::IskCents::from_isk(5),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: eve_db::IskCents::from_isk(500),
            },
        );
        snap.market_orders.insert(
            sell_order_id,
            eve_db::MarketOrderRecord {
                order_id: sell_order_id,
                owner_id: corp_id,
                station_id,
                region_id: 10_000_002,
                type_id: 35, // Pyerite
                is_bid: false,
                price: eve_db::IskCents::from_isk(10),
                vol_entered: 40,
                vol_remaining: 40,
                min_volume: 1,
                is_npc: false,
                escrow: eve_db::IskCents(0),
            },
        );
        // Active owned structure blocks solo-CEO closure
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Corp Astrahus".into(),
                state: "ShieldVulnerable".into(),
                position: [10_000.0, 0.0, 0.0],
                quantum_core_installed: false,
                fuel_blocks: 10,
                ..Default::default()
            },
        );
        snap.items.insert(
            struct_mod_id,
            eve_db::InventoryItemRecord {
                item_id: struct_mod_id,
                type_id: 35888,
                owner_id: corp_id,
                location_id: structure_id,
                flag: eve_db::InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // Open for_corp contract + locked item
        snap.contracts.insert(
            contract_id,
            eve_db::ContractRecord {
                contract_id,
                issuer_id: ceo_id,
                issuer_corp_id: corp_id,
                for_corp: true,
                contract_type: "ItemExchange".into(),
                status: "Outstanding".into(),
                start_station_id: station_id,
                end_station_id: station_id,
                item_ids: vec![contract_item_id],
                availability: "Public".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            contract_item_id,
            eve_db::InventoryItemRecord {
                item_id: contract_item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: eve_db::InventoryFlag::Locked,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.characters[&ceo_id].balance.to_f64_isk()
    });

    // 1. Active owned Upwell structure blocks closure
    let st_err = handle_kick_out_members(&ctx, &mut ceo_sess, &[])
        .expect("matched")
        .expect_err("active structure must block closure");
    assert_eq!(st_err.code, "ActiveStructuresPreventClosure");

    // Mark structure destroyed -> open for_corp contract blocks closure
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&structure_id).unwrap().state = "Destroyed".into();
    });
    let ct_err = handle_kick_out_members(&ctx, &mut ceo_sess, &[])
        .expect("matched")
        .expect_err("open for_corp contract must block closure");
    assert_eq!(ct_err.code, "OpenCorpContractsOrJobsPreventClosure");

    // Mark contract Completed while keeping Locked contract_item_id and structure-slotted struct_mod_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.contracts.get_mut(&contract_id).unwrap().status = "Completed".into();
        snap.contracts.get_mut(&contract_id).unwrap().item_ids.clear();
    });
    let _ = ctx.drain_notifications_for_char(ceo_id);

    // 2. Solo-CEO resignation succeeds via handle_kick_out_members and emits OnAccountChange + OnItemChange
    let close_rep = handle_kick_out_members(&ctx, &mut ceo_sess, &[])
        .expect("matched")
        .expect("corporation closure should succeed");
    assert_eq!(close_rep.dict_get("corporationClosed"), Some(&PyRep::Bool(true)));

    let notifs = ctx.drain_notifications_for_char(ceo_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnAccountChange"),
        "expected OnAccountChange notification for CEO liquidation sweep"
    );
    assert!(
        notifs.iter().any(|n| n.event == "OnItemChange"),
        "expected OnItemChange notifications for Deliveries items and removed office"
    );

    // Excluded structure-slotted and Locked items must not be re-homed into personal CEO assets
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.items[&struct_mod_id].owner_id, corp_id,
            "structure-slotted item must stay excluded from blind corp re-homing"
        );
        assert_eq!(
            snap.items[&contract_item_id].owner_id, corp_id,
            "Locked item must stay excluded from blind corp re-homing"
        );
    });

    // 3. Force a failed try_with_mut_snapshot rollback AFTER closure (B6-P0-01 regression check):
    //    must NOT resurrect corp_id, office_id, or wipe corporation_liquidation journal / duplicate ISK.
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.balance = eve_db::IskCents(0);
        }
        Err("forced rollback after corp closure".to_string())
    });

    let assert_closed_state = |snap: &eve_db::WorldStateSnapshot, phase: &str| {
        assert!(
            !snap.corporations.contains_key(&corp_id),
            "{phase}: closed corporation must remain absent"
        );
        assert!(
            !snap.market_orders.contains_key(&buy_order_id),
            "{phase}: corp buy order must remain deleted"
        );
        assert!(
            !snap.market_orders.contains_key(&sell_order_id),
            "{phase}: corp sell order must remain deleted"
        );
        assert!(
            !snap.items.contains_key(&office_id),
            "{phase}: office folder item must remain deleted"
        );
        // 250k + 150k division balances + 500 buy escrow swept to closing CEO
        let final_ceo_isk = snap.characters[&ceo_id].balance.to_f64_isk();
        assert!(
            (final_ceo_isk - (initial_ceo_isk + 400_500.0)).abs() < 1e-6,
            "{phase}: expected CEO wallet to hold exact 400,500 ISK liquidation sweep, got {final_ceo_isk} (was {initial_ceo_isk})"
        );
        assert!(
            snap.wallet_journal
                .iter()
                .any(|j| j.character_id == ceo_id && j.entry_type == "corporation_liquidation"),
            "{phase}: corporation_liquidation journal entry must persist"
        );
        let box_item = &snap.items[&box_in_office_id];
        assert_eq!(box_item.owner_id, ceo_id);
        assert_eq!(box_item.location_id, station_id);
        assert_eq!(box_item.flag, eve_db::InventoryFlag::Deliveries);
        let nested = &snap.items[&nested_loot_id];
        assert_eq!(
            nested.owner_id, ceo_id,
            "{phase}: nested items inside corporate office container must keep CEO ownership"
        );
        assert_eq!(nested.location_id, box_in_office_id);
        let returned_sell = snap.items.values().find(|i| {
            i.owner_id == ceo_id
                && i.location_id == station_id
                && i.flag == eve_db::InventoryFlag::Deliveries
                && i.type_id == 35
                && i.quantity == 40
        });
        assert!(
            returned_sell.is_some(),
            "{phase}: open corporate sell order volume must persist in CEO Deliveries"
        );
    };

    ctx.db.with_snapshot(|snap| assert_closed_state(snap, "post-rollback"));
    drop(ctx);

    // 4. Reopen WorldRepository from SQLite to confirm durable persistence across restart
    let repo2 = std::sync::Arc::new(eve_db::WorldRepository::open_sqlite(&db_path).expect("reopen sqlite"));
    let ctx2 = crate::router::ServerContext::with_sde_and_repository(base_ctx.sde, repo2);
    ctx2.db.with_snapshot(|snap| assert_closed_state(snap, "post-sqlite-reopen"));
    drop(ctx2);
    let _ = std::fs::remove_dir_all(&tmp_dir);
}

