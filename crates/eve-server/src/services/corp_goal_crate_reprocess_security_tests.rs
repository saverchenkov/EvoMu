//! Security and mechanics regression tests for Round-7 Track D Priority-1 fixes (`#461`, `#463`).

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use crate::services::crate_loot_service::handle_crate_service;
use crate::services::social_corp_fleet::social_corp_projects::{
    CanonicalProjectSpec, create_canonical_project, record_server_project_activity,
    redeem_corp_goal_reward_for_char,
};
use eve_db::{
    ContractRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
};
use eve_proto::PyRep;
use eve_proto::quasar::{EvePublicRequest, GoogleAny, TYPE_GOOGLEAPIS_PREFIX};
use std::sync::Arc;

/// `#461`: Rejects substring-matched non-crate items (`Crate`/`Box`/`Cache`/`Bundle`/`Package`)
/// with `sde.type_materials` and blocks opening or claiming crates locked in contract escrow or another station.
#[test]
fn r7_461_crate_loot_blocks_substring_reprocessing_and_escrow_or_location_bypass() {
    let mut ctx = test_ctx();
    let cid = 90_000_001_i64;
    let station_a = 60_003_760_i64;
    let station_b = 60_008_494_i64;
    let mut sess = auth_session(1, cid, 98_000_001, 30_000_142);
    sess.station_id = Some(station_a);

    // Rename a real SDE type with `type_materials` (not in `crates.fsdbinary`) to match all substring keywords.
    let fake_crate_tid = ctx
        .sde
        .type_materials
        .keys()
        .copied()
        .find(|tid| *tid == 587)
        .unwrap_or_else(|| *ctx.sde.type_materials.keys().next().unwrap());
    Arc::make_mut(&mut ctx.sde).types.entry(fake_crate_tid).and_modify(|t| {
        t.name = "Smuggler Salvage Cache Crate Box Bundle Package".into();
        t.category_id = 6;
        t.group_id = 25;
    });

    // Locate a genuine fixed-loot crate in the SDE to verify escrow and remote-station rejection.
    let mut genuine_tid = None;
    for (&tid, _) in ctx.sde.types.iter() {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                940_099,
                InventoryItemRecord {
                    item_id: 940_099,
                    type_id: tid,
                    owner_id: cid,
                    location_id: station_a,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    ..Default::default()
                },
            );
        });
        if handle_crate_service(&ctx, &sess, "GetCrateLocation", &[PyRep::Integer(940_099)])
            .is_some_and(|r| r.is_ok())
        {
            genuine_tid = Some(tid);
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.remove(&940_099);
            });
            break;
        }
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.remove(&940_099);
        });
    }
    let genuine_tid = genuine_tid.expect("SDE must contain at least one valid fixed-loot crate");

    let (fake_item_id, escrow_crate_id, remote_crate_id, stack_remote_id) =
        (940_001_i64, 940_002_i64, 940_003_i64, 940_004_i64);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.station_id = Some(station_a);
        }
        snap.items.insert(
            fake_item_id,
            InventoryItemRecord {
                item_id: fake_item_id,
                type_id: fake_crate_tid,
                owner_id: cid,
                location_id: station_a,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            escrow_crate_id,
            InventoryItemRecord {
                item_id: escrow_crate_id,
                type_id: genuine_tid,
                owner_id: cid,
                location_id: station_a,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.contracts.insert(
            880_001,
            ContractRecord {
                contract_id: 880_001,
                issuer_id: cid,
                status: "Outstanding".into(),
                start_station_id: station_a,
                end_station_id: station_a,
                item_ids: vec![escrow_crate_id],
                ..Default::default()
            },
        );
        snap.items.insert(
            remote_crate_id,
            InventoryItemRecord {
                item_id: remote_crate_id,
                type_id: genuine_tid,
                owner_id: cid,
                location_id: station_b,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            stack_remote_id,
            InventoryItemRecord {
                item_id: stack_remote_id,
                type_id: genuine_tid,
                owner_id: cid,
                location_id: station_b,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                ..Default::default()
            },
        );
    });

    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(fake_item_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateType"
    );
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimLootFromCrateStack",
            &[PyRep::Integer(fake_item_id), PyRep::Integer(1)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateType"
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(escrow_crate_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateLocation"
    );
    assert_eq!(
        handle_crate_service(&ctx, &sess, "OpenCrate", &[PyRep::Integer(remote_crate_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidCrateLocation"
    );
    assert_eq!(
        handle_crate_service(
            &ctx,
            &sess,
            "ClaimLootFromCrateStack",
            &[PyRep::Integer(stack_remote_id), PyRep::Integer(2)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidCrateLocation"
    );
}

/// `#463`: Allows incremental partial reward redemptions without permanent lockout
/// and removes the artificial 10,000 ISK/unit ceiling on corporation goal reward pools.
#[test]
fn r7_463_corp_goal_incremental_redemption_and_uncapped_unit_payout() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let corp_id = 98_000_001_i64;
    let worker_id = 90_000_001_i64;
    let sess = auth_session(1, worker_id, corp_id, 30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "Uncapped Goal Corp".into();
                __corp.ceo_id = worker_id;
                __corp.wallet_divisions = CorporationRecord::default_wallet_divisions(IskCents::from_isk(
                    100_000_000,
                ));
                let __mw = IskCents::from_isk(100_000_000);
                if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); }
                __corp },
        );
    });

    // 50,000,000 ISK pool over target_quantity = 100 -> 500,000 ISK/unit (above old 10,000 ISK cap).
    let (pid, _uuid) = create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: worker_id,
            name: "High-Value Bounty Drive".into(),
            description: "Uncapped pool rate test".into(),
            target_quantity: 100,
            initial_quantity: 0,
            escrow: IskCents::from_isk(50_000_000),
            isk_per_unit: IskCents::ZERO,
            wallet_division: 1,
            matcher_type: "kill_npc".into(),
            is_freelance: false,
            auto_payout_on_contribution: false,
            max_per_character: None,
            expires_unix: Some(now + 3_600),
            filter_id: None,
            batch_size: 1,
            now_unix: now,
            min_project_id: 400,
        },
    )
    .unwrap();

    let get_rewards_req = EvePublicRequest {
        correlation_uuid: vec![15; 16],
        payload: Some(GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetMineWithRewardsRequest"
            ),
            value: Vec::new(),
        }),
        ..Default::default()
    };

    // Contribution batch 1: 3 units -> 3 * 500,000 = 1,500,000 ISK (not capped at 30,000 ISK).
    record_server_project_activity(&ctx, worker_id, &["kill_npc"], 3, None);
    let r1 = ctx
        .quasar_gateway
        .handle_public_request(&sess, &get_rewards_req, None, false, now);
    assert_eq!(r1.status_code, 200);
    assert!(!r1.payload.as_ref().unwrap().value.is_empty());

    let payout1_isk = redeem_corp_goal_reward_for_char(&ctx.db, pid, worker_id, now).unwrap();
    assert_eq!(payout1_isk, 1_500_000);

    // Contribution batch 2 after first partial redemption: 4 more units -> must NOT be locked out!
    record_server_project_activity(&ctx, worker_id, &["kill_npc"], 4, None);
    let r2 = ctx
        .quasar_gateway
        .handle_public_request(&sess, &get_rewards_req, None, false, now);
    assert_eq!(r2.status_code, 200);
    assert!(!r2.payload.as_ref().unwrap().value.is_empty());

    let payout2_isk = redeem_corp_goal_reward_for_char(&ctx.db, pid, worker_id, now).unwrap();
    assert_eq!(payout2_isk, 2_000_000);
}
