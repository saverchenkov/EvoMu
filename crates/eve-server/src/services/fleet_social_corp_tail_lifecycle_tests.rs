//! Unit tests for `fleet_social_corp_tail` member kick, auto-kick queues, bloodline NPC corp, and corp closure.

use super::super::*;
use crate::services::fleet_social::fleet_social_corps::with_corp_gov;
use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use eve_db::{InventoryFlag, InventoryItemRecord, WarRecord};

#[test]
fn kick_out_members_auto_kick_on_dock_and_downtime_execution() {
    let ctx = test_ctx_with_clock(1_800_025_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);

    // Bloodline default NPC corp lookup
    ctx.db.with_snapshot(|snap| {
        let npc = bloodline_default_npc_corp(&ctx, snap, 90_000_002);
        assert!(npc > 1_000_000);
    });

    // Drop title & roles on 90_000_002 and advance 24h so member can be kicked
    handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(90_000_002)),
            ("roles", PyRep::Integer(0)),
            ("titleMask", PyRep::Integer(0)),
        ])],
    )
    .unwrap()
    .unwrap();
    ctx.advance_secs(86_405);

    // Put 90_000_002 in space -> KickOutMember queues pending_auto_kick instead of immediate removal!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let q_res = handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(90_000_002)])
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(q_kv) = q_res else { panic!() };
    assert_eq!(q_kv.get("queuedAutoKick").unwrap(), &PyRep::Bool(true));
    assert_eq!(q_kv.get("removed").unwrap(), &PyRep::Bool(false));

    // When 90_000_002 docks at Jita 4-4, execute_pending_auto_kicks_for_docked_char moves them to bloodline NPC corp!
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.station_id = Some(60_003_760);
        }
    });
    execute_pending_auto_kicks_for_docked_char(&ctx, &mut mem_sess, 90_000_002);
    let new_corp_m2 = mem_sess.corporation_id.unwrap_or(0);
    assert_ne!(new_corp_m2, 98_000_001);
    assert!(new_corp_m2 >= 1_000_000);

    // Queue 90_000_003 in corp 98_000_002 and verify execute_pending_auto_kicks_on_downtime executes it
    with_corp_gov(&ctx, |g| {
        g.entry(98_000_002)
            .or_default()
            .pending_auto_kicks
            .insert(90_000_003, ctx.now_unix());
    });
    let dt_count = execute_pending_auto_kicks_on_downtime(&ctx);
    assert_eq!(dt_count, 1);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.corporations.get(&98_000_002).unwrap().members.contains(&90_000_003));
    });
}

#[test]
fn last_member_resign_closes_corporation_and_handles_active_war_blocker() {
    let ctx = test_ctx_with_clock(1_800_026_000);
    let (mut ceo_sess, _mem_sess) = seed_player_corps(&ctx);

    // Kick 90_000_002 while docked so CEO is last remaining member of 98_000_001
    let immediate = handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(90_000_002)])
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(im_kv) = immediate else { panic!() };
    assert_eq!(im_kv.get("removed").unwrap(), &PyRep::Bool(true));

    // Seed an active war + an office item on 98_000_001
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            555_001,
            WarRecord {
                war_id: 555_001,
                aggressor_corp_id: 98_000_001,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: 1_800_000_000,
                active: true,
            },
        );
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: 27,
                owner_id: 98_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    with_corp_gov(&ctx, |g| {
        g.entry(98_000_001).or_default().offices.insert(60_003_760, 777_001);
    });

    // Active war prevents closing corp when last member resigns
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[])
            .unwrap()
            .unwrap_err()
            .code,
        "ActiveWarsPreventClosure"
    );

    // Deactivate war -> CEO resign closes corp 98_000_001 and removes office 777_001
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.get_mut(&555_001).unwrap().active = false;
    });
    let closed_res = handle_kick_out_members(&ctx, &mut ceo_sess, &[])
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(cl_kv) = closed_res else { panic!() };
    assert_eq!(cl_kv.get("corporationClosed").unwrap(), &PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.corporations.contains_key(&98_000_001));
        assert!(!snap.items.contains_key(&777_001));
    });
}
