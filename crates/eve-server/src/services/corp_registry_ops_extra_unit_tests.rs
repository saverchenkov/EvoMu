//! Extra branch-coverage and mutation-killing unit tests for `corp_registry_ops.rs`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item,
};
use crate::services::corp_registry_ops::*;
use eve_db::{
    CorporationRecord, FleetMemberRecord, FleetRecord, FleetRole, InventoryFlag,
    IskCents,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

pub(super) fn call_corp_reg(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, "corpRegistry", method, args, &kw)
        .expect("corpRegistry method handled")
}

pub(super) fn setup_ctx() -> (ServerContext, SessionState, i64, i64, i64, i64) {
    let (ctx, mut sess, ceo_id, ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_050_i64;
    let member_id = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    sess.corporation_id = Some(corp_id);
    sess.station_id = Some(station_id);
    sess.corp_role = u64::MAX;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&ceo_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        if let Some(ch) = snap.characters.get_mut(&member_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Vanguard Logistics Corp".into(),
            ticker: "VGLC".into(),
            ceo_id,
            master_wallet: IskCents::from_isk(50_000_000),
            members: [ceo_id, member_id].into_iter().collect(),
            member_roles: [(ceo_id, u64::MAX), (member_id, 0)].into_iter().collect(),
            hq_station_id: Some(station_id),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });
    (ctx, sess, ceo_id, member_id, corp_id, ship_id)
}

#[tokio::test]
async fn command_burst_dogma_activate_and_invbroker_hangar_branches() {
    let (ctx, mut sess, ceo_id, member_id, corp_id, _ship_id) = setup_ctx();
    let kw = HashMap::new();

    // corporationSvc.GetEmploymentRecord delegates to fleet_social::handle
    let emp = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "corporationSvc",
        "GetEmploymentRecord",
        &[PyRep::Integer(ceo_id)],
        &kw,
    )
    .await;
    assert!(emp.is_some());

    // Command burst fallback when session.fleet_id is set and ship_id = 0 (no ship fitting)
    let burst_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 1770)
        .map(|t| t.type_id)
        .unwrap_or(42_526);
    let mod_id = 940_001_i64;
    let charge_id = 940_002_i64;
    let fake_ship_id = 940_003_i64;
    let wingmate_ship = 940_004_i64;
    let fleet_id = 8001_i64;
    let sys_id = sess.solar_system_id.unwrap_or(30_000_142);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_fighter_item(
                mod_id,
                burst_type_id,
                ceo_id,
                fake_ship_id,
                InventoryFlag::HiSlot0,
                1,
            ),
        );
        snap.items.insert(
            charge_id,
            make_fighter_item(
                charge_id,
                42_832,
                ceo_id,
                fake_ship_id,
                InventoryFlag::HiSlot0,
                100,
            ),
        );
        if let Some(m) = snap.characters.get_mut(&member_id) {
            m.solar_system_id = sys_id;
            m.station_id = None;
            m.active_ship_id = wingmate_ship;
        }
        let mut fl = FleetRecord {
            fleet_id,
            boss_id: ceo_id,
            ..Default::default()
        };
        fl.members.push(ceo_id);
        fl.members.push(member_id);
        fl.member_details.insert(
            ceo_id,
            FleetMemberRecord {
                character_id: ceo_id,
                role: FleetRole::FleetCommander,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 2,
                role_name: "FC".into(),
                join_time_unix: 1000,
            },
        );
        fl.member_details.insert(
            member_id,
            FleetMemberRecord {
                character_id: member_id,
                role: FleetRole::SquadMember,
                wing_id: None,
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: "Member".into(),
                join_time_unix: 1000,
            },
        );
        snap.fleets.insert(fleet_id, fl);
    });

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball(Ball {
            id: fake_ship_id,
            type_id: 587,
            position: glam::DVec3::ZERO,
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys_h
        .spawn_ball(Ball {
            id: wingmate_ship,
            type_id: 587,
            position: glam::DVec3::new(5_000.0, 0.0, 0.0),
            velocity: glam::DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let mut burst_sess = sess.clone();
    burst_sess.fleet_id = Some(fleet_id);
    burst_sess.ship_id = Some(0);
    let docked_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut burst_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(docked_err.code, "ShipDocked");

    burst_sess.station_id = None;
    burst_sess.structure_id = None;
    let no_ship_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut burst_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_ship_err.code, "NoActiveShip");

    burst_sess.ship_id = Some(fake_ship_id);
    let burst_res = handle_corp_hangar_invbroker(
        &ctx,
        &mut burst_sess,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(burst_res.dict_get("active"), Some(&PyRep::Bool(true)));
    assert_eq!(
        burst_res
            .dict_get("affectedMemberIDs")
            .and_then(|v| v.as_list())
            .map(|l| l.len()),
        Some(2)
    );

    // Adversarial `#477` cases: offline, unfitted, contract-locked, foreign, and missing in-space ball.
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(mod_id, false);
    });
    assert_eq!(handle_corp_hangar_invbroker(&ctx, &mut burst_sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)], &kw).await.unwrap().unwrap_err().code, "ModuleOffline");
    ctx.db.with_mut_snapshot(|s| {
        s.module_online_states.insert(mod_id, true);
        s.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::Hangar;
    });
    assert_eq!(handle_corp_hangar_invbroker(&ctx, &mut burst_sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)], &kw).await.unwrap().unwrap_err().code, "ModuleNotFitted");
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&mod_id).unwrap().flag = InventoryFlag::HiSlot0;
        s.contracts.insert(
            77_901,
            eve_db::ContractRecord {
                contract_id: 77_901,
                issuer_id: ceo_id,
                status: "Outstanding".into(),
                item_ids: vec![mod_id],
                ..Default::default()
            },
        );
    });
    assert_eq!(handle_corp_hangar_invbroker(&ctx, &mut burst_sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)], &kw).await.unwrap().unwrap_err().code, "ModuleNotFitted");
    ctx.db.with_mut_snapshot(|s| {
        s.contracts.remove(&77_901);
        s.items.get_mut(&mod_id).unwrap().owner_id = member_id;
    });
    assert_eq!(handle_corp_hangar_invbroker(&ctx, &mut burst_sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)], &kw).await.unwrap().unwrap_err().code, "ModuleNotOwnedByShip");
    ctx.db.with_mut_snapshot(|s| s.items.get_mut(&mod_id).unwrap().owner_id = ceo_id);
    let _ = sys_h.remove_ball(fake_ship_id).await;
    assert_eq!(handle_corp_hangar_invbroker(&ctx, &mut burst_sess, None, "dogmaIM", "Activate", &[PyRep::Integer(mod_id)], &kw).await.unwrap().unwrap_err().code, "ShipNotInSpace");

    // invbroker.List error branches: InvalidLocation and BoundObjectOwnershipViolation
    let mut no_loc_sess = sess.clone();
    no_loc_sess.station_id = None;
    no_loc_sess.structure_id = None;
    no_loc_sess.ship_id = None;
    let mut flag_kw = HashMap::new();
    flag_kw.insert("flag".into(), PyRep::Integer(116));
    let no_loc_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut no_loc_sess,
        None,
        "invbroker",
        "List",
        &[],
        &flag_kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_loc_err.code, "InvalidLocation");

    let bound_other_station = BoundObjectEntry {
        bound_id: 1,
        session_id: sess.session_id,
        user_id: sess.user_id.unwrap_or(1),
        character_id: ceo_id,
        service: "invbroker".into(),
        object_id: 60_003_761,
        oid_str: "oid".into(),
        ref_id: 1,
    };
    let viol_err = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        Some(&bound_other_station),
        "invbroker",
        "List",
        &[PyRep::Integer(116)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(viol_err.code, "BoundObjectOwnershipViolation");

    // Seed BPC, singleton, and named items in division 1 (flag 116)
    let station_id = sess.station_id.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let mut bpc = make_fighter_item(940_010, 34, corp_id, station_id, InventoryFlag::CorpSAG1, 1);
        bpc.is_blueprint_copy = true;
        bpc.singleton = true;
        snap.items.insert(940_010, bpc);
        snap.item_custom_names.insert(940_010, "CustomBPC".into());
    });
    let listed = handle_corp_hangar_invbroker(
        &ctx,
        &mut sess,
        None,
        "invbroker",
        "List",
        &[],
        &flag_kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(listed, PyRep::ObjectEx { .. }));
}
