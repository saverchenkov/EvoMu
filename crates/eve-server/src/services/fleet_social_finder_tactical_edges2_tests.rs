//! Additional edge-case and branch-coverage unit tests for `fleet_social_finder_tactical.rs`.

#![forbid(unsafe_code)]

use crate::services::account_services::account_services_tests::PyRepTestExt;
use crate::services::fleet_social::fleet_social_finder_tactical::{
    handle_finder_and_tactical, handle_fleet_watchlist_ops, handle_get_fleet_setups,
    handle_get_formation_settings, handle_set_fleet_formation_settings,
};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    seed_multi_member_fleet, test_ctx_and_clock,
};
use crate::services::fleet_social::handle;
use eve_db::{FleetRole, InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

fn mk_sess(
    char_id: i64,
    fleet_id: Option<i64>,
    sys_id: Option<u32>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.fleet_id = fleet_id;
    s.solar_system_id = sys_id;
    s.ship_id = ship_id;
    s
}

#[tokio::test]
async fn formations_watchlist_setups_and_hp_ratio_fallback_edges() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_400_000);
    let (fleet_id, _boss_sess) = seed_multi_member_fleet(&ctx).await;
    let (boss_id, wc_id, sc_id, mem_id) = (90_000_001_i64, 90_000_002_i64, 90_000_003_i64, 90_000_004_i64);
    let mut boss_s = mk_sess(boss_id, Some(fleet_id), Some(30_000_142), None);
    let mut wc_s = mk_sess(wc_id, Some(fleet_id), Some(30_000_142), None);
    let mut no_fleet_s = mk_sess(90_999_001, None, Some(30_000_142), None);
    let kw = HashMap::new();

    // GetFormationSettings default -> (0, 0.0, 10_000.0)
    let def_form = handle_get_formation_settings(&ctx, boss_id);
    assert_eq!(def_form.dict_get("formationID"), Some(&PyRep::Integer(0)));
    assert_eq!(
        def_form.dict_get("formationSize"),
        Some(&PyRep::Real(10_000.0))
    );

    // SetFleetFormationSettings via kwargs + clamping negative/excess IDs
    let mut form_kw = HashMap::new();
    form_kw.insert("formation".into(), PyRep::String("Diamond".into()));
    let _ = handle_set_fleet_formation_settings(
        &ctx,
        boss_id,
        &[
            PyRep::Integer(99),
            PyRep::Real(2_500.0),
            PyRep::Real(12_000.0),
        ],
        &form_kw,
    );
    // kwargs takes priority -> Diamond == 4
    let after_form = handle_get_formation_settings(&ctx, boss_id);
    assert_eq!(after_form.dict_get("formationID"), Some(&PyRep::Integer(4)));

    // RegisterForDamageUpdates when NOT in fleet returns {characterID, watched: false} rather than NotInFleet
    let not_in = handle_fleet_watchlist_ops(
        &ctx,
        &mut no_fleet_s,
        "RegisterForDamageUpdates",
        &[PyRep::Integer(mem_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(not_in.dict_get("watched"), Some(&PyRep::Bool(false)));

    // Empty GetWatchlist, AddToWatchlist with single integer, dynamic_attributes ship HP fallback
    let empty_wl = handle_fleet_watchlist_ops(&ctx, &mut boss_s, "GetWatchlist", &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(empty_wl.as_list().map(|l| l.len()), Some(0));

    let mem_ship_id = 77_500_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&mem_id) {
            ch.active_ship_id = mem_ship_id;
        }
        let mut ship = InventoryItemRecord {
            item_id: mem_ship_id,
            type_id: 587,
            owner_id: mem_id,
            location_id: 30_000_142,
            flag: InventoryFlag::None,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        ship.dynamic_attributes.insert(264, 0.5);
        snap.items.insert(mem_ship_id, ship);
    });

    assert!(handle_fleet_watchlist_ops(
        &ctx,
        &mut boss_s,
        "AddToWatchlist",
        &[PyRep::Integer(mem_id)]
    )
    .await
    .unwrap()
    .is_ok());
    let pop_wl = handle_fleet_watchlist_ops(&ctx, &mut boss_s, "GetWatchlist", &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pop_wl.as_list().map(|l| l.len()), Some(1));

    // RemoveFromWatchlist leaves fav_ids clean and returns watched: false
    let rm = handle_fleet_watchlist_ops(
        &ctx,
        &mut boss_s,
        "RemoveFromWatchlist",
        &[PyRep::Integer(mem_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rm.dict_get("watched"), Some(&PyRep::Bool(false)));

    // StoreFleetSetup inheriting fleet properties + LoadFleetSetup fallback when setup missing (as FC/boss)
    assert!(handle(
        &ctx,
        &mut boss_s,
        "fleetObjectHandler",
        "StoreFleetSetup",
        &[PyRep::String("InheritSetup".into())],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    let setups = handle_get_fleet_setups(&ctx, boss_id);
    assert!(setups.as_dict().is_some_and(|d| !d.is_empty()));

    // Non-existent setup loads fallback MOTD & creates wings/squads for FleetCommander (`wc_s` if FleetCommander or `boss_s`)
    assert!(handle(
        &ctx,
        &mut boss_s,
        "fleetObjectHandler",
        "LoadFleetSetup",
        &[PyRep::String("DoesNotExistSetup".into())],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // Error cases: LoadFleetSetup when NotInFleet and FleetNotFound (for wc_s with stale fleet_id)
    assert_eq!(
        handle(
            &ctx,
            &mut no_fleet_s,
            "fleetObjectHandler",
            "LoadFleetSetup",
            &[PyRep::String("X".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NotInFleet"
    );
    wc_s.fleet_id = Some(99_999_999);
    assert_eq!(
        handle(
            &ctx,
            &mut wc_s,
            "fleetObjectHandler",
            "LoadFleetSetup",
            &[PyRep::String("X".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetNotFound"
    );
    let _ = (sc_id, mem_id);
}

#[tokio::test]
async fn finder_adverts_join_requests_reconnect_and_unhandled_dispatch() {
    let (ctx, _clock) = test_ctx_and_clock(1_775_400_000);
    let (fleet_id, _boss_sess) = seed_multi_member_fleet(&ctx).await;
    let (boss_id, wc_id, sc_id, mem_id) = (90_000_001_i64, 90_000_002_i64, 90_000_003_i64, 90_000_004_i64);
    let outsider_id = 90_999_500_i64;
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let mut base = snap.characters[&boss_id].clone();
        base.character_id = outsider_id;
        base.user_id = outsider_id as u64;
        base.corporation_id = 98_555_001;
        base.alliance_id = Some(99_555_001);
        snap.characters.insert(outsider_id, base);
    });

    let mut boss_s = mk_sess(boss_id, Some(fleet_id), None, None); // solar_system_id == None fallback
    let mut mem_s = mk_sess(mem_id, Some(fleet_id), Some(30_000_142), None);
    let mut out_s = mk_sess(outsider_id, None, Some(30_000_142), None);

    // Sibling fleetProxy method handled elsewhere returns None
    assert!(
        handle_finder_and_tactical(&ctx, &mut boss_s, "fleetProxy", "LeaveFleet", &[], &kw)
            .await
            .is_none()
    );

    // AddFleetFinderAdvert with solar_system_id == None falls back to char's system, with inviteScope=3 (Public) & autoAccept=true
    let mut ad_kw = HashMap::new();
    ad_kw.insert(
        "fleetName".into(),
        PyRep::String("Public Auto Fleet".into()),
    );
    ad_kw.insert("inviteScope".into(), PyRep::Integer(3));
    ad_kw.insert("autoAccept".into(), PyRep::Bool(true));
    assert!(handle_finder_and_tactical(
        &ctx,
        &mut boss_s,
        "fleetProxy",
        "AddFleetFinderAdvert",
        &[],
        &ad_kw
    )
    .await
    .unwrap()
    .is_ok());

    // UpdateAdvertInfo via diff keyval (`membergroupsToAddToAllowed`, `publicToAddToAllowed`, `fleetName`)
    let diff_kv = PyRep::keyval([
        (
            "membergroupsToAddToAllowed",
            PyRep::List(vec![PyRep::Integer(98_555_001)]),
        ),
        (
            "publicToAddToAllowed",
            PyRep::List(vec![PyRep::Integer(99_555_001)]),
        ),
    ]);
    assert!(handle_finder_and_tactical(
        &ctx,
        &mut boss_s,
        "fleetProxy",
        "UpdateAdvertInfo",
        &[diff_kv],
        &HashMap::from([("fleetName".into(), PyRep::String("Renamed Fleet".into()))]),
    )
    .await
    .unwrap()
    .is_ok());

    // UpdateAdvertAllowedEntities subtracting disallowed entities
    let allowed_kv = PyRep::keyval([
        (
            "publicAllowedEntities",
            PyRep::List(vec![PyRep::Integer(99_555_001), PyRep::Integer(98_555_001)]),
        ),
        (
            "publicDisallowedEntities",
            PyRep::List(vec![PyRep::Integer(98_555_001)]),
        ),
    ]);
    assert!(handle_finder_and_tactical(
        &ctx,
        &mut boss_s,
        "fleetProxy",
        "UpdateAdvertAllowedEntities",
        &[allowed_kv],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // GetAvailableFleetAds visible to outsider matching alliance 99_555_001
    let ads = handle_finder_and_tactical(
        &ctx,
        &mut out_s,
        "fleetProxy",
        "GetAvailableFleetAds",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ads.as_dict().map(|d| d.len()), Some(1));

    // ApplyToJoinFleet error cases: AlreadyInFleet, InvalidArgument, FleetNotFound, then auto_join success!
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut mem_s,
            "fleetProxy",
            "ApplyToJoinFleet",
            &[PyRep::Integer(fleet_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AlreadyInFleet"
    );
    assert_eq!(
        handle_finder_and_tactical(&ctx, &mut out_s, "fleetProxy", "ApplyToJoinFleet", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut out_s,
            "fleetProxy",
            "ApplyToJoinFleet",
            &[PyRep::Integer(999_888_777)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetNotFound"
    );
    let join_rep = handle_finder_and_tactical(
        &ctx,
        &mut out_s,
        "fleetProxy",
        "ApplyToJoinFleet",
        &[PyRep::Integer(fleet_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(join_rep.dict_get("joined"), Some(&PyRep::Bool(true)));
    assert_eq!(out_s.fleet_id, Some(fleet_id));

    // GetFleetID & Reconnect (in fleet vs not in fleet)
    assert_eq!(
        handle_finder_and_tactical(&ctx, &mut out_s, "fleetObjectHandler", "GetFleetID", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(fleet_id)
    );
    let reconn = handle_finder_and_tactical(
        &ctx,
        &mut out_s,
        "fleetObjectHandler",
        "Reconnect",
        &[],
        &kw
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reconn.dict_get("fleetID"), Some(&PyRep::Integer(fleet_id)));
    let mut solo_s = mk_sess(90_999_600, None, Some(30_000_142), None);
    let solo_reconn = handle_finder_and_tactical(
        &ctx,
        &mut solo_s,
        "fleetObjectHandler",
        "Reconnect",
        &[],
        &kw
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(solo_reconn.dict_get("fleetID"), Some(&PyRep::None));

    // GetJoinRequests / AcceptJoinRequest / RejectJoinRequest validation errors + manual request flow
    ctx.fleet_extras
        .lock()
        .unwrap()
        .adverts
        .get_mut(&fleet_id)
        .unwrap()
        .auto_join = false;
    let applicant_id = 90_999_700_i64;
    let mut app_s = mk_sess(applicant_id, None, Some(30_000_142), None);
    ctx.db.with_mut_snapshot(|snap| {
        let mut base = snap.characters[&boss_id].clone();
        base.character_id = applicant_id;
        base.user_id = applicant_id as u64;
        base.alliance_id = Some(99_555_001);
        snap.characters.insert(applicant_id, base);
    });
    handle_finder_and_tactical(
        &ctx,
        &mut app_s,
        "fleetProxy",
        "ApplyToJoinFleet",
        &[PyRep::Integer(fleet_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let reqs = handle_finder_and_tactical(
        &ctx,
        &mut boss_s,
        "fleetObjectHandler",
        "GetJoinRequests",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(reqs.as_dict().map(|d| d.len()), Some(1));

    // AcceptJoinRequest with occupied FleetCommander slot -> PositionOccupied
    let occ_err = handle_finder_and_tactical(
        &ctx,
        &mut boss_s,
        "fleetObjectHandler",
        "AcceptJoinRequest",
        &[
            PyRep::Integer(applicant_id),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(occ_err.code, "PositionOccupied");

    // RejectJoinRequest then re-reject -> FleetJoinRequestNotFound
    let rej = handle_finder_and_tactical(
        &ctx,
        &mut boss_s,
        "fleetObjectHandler",
        "RejectJoinRequest",
        &[PyRep::Integer(applicant_id)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rej.dict_get("characterID"), Some(&PyRep::Integer(applicant_id)));
    assert_eq!(rej.dict_get("rejected"), Some(&PyRep::Bool(true)));
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut boss_s,
            "fleetObjectHandler",
            "RejectJoinRequest",
            &[PyRep::Integer(applicant_id)],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetJoinRequestNotFound"
    );

    // Stale fleet_id in GetJoinRequests clears session.fleet_id & returns FleetNotFound
    let mut stale_s = mk_sess(boss_id, Some(88_888_888), Some(30_000_142), None);
    assert_eq!(
        handle_finder_and_tactical(
            &ctx,
            &mut stale_s,
            "fleetObjectHandler",
            "GetJoinRequests",
            &[],
            &kw
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetNotFound"
    );
    assert_eq!(stale_s.fleet_id, None);
    let _ = (wc_id, sc_id, FleetRole::SquadMember);

    // Direct checks for `leader_keyval`, `advert_to_dict`, `require_fleet_advert_auth`, and `sync_fleet_advert_snapshot_to_db`
    let kv_rep = super::leader_keyval(boss_id, Some((98_000_001, Some(99_000_001), 2.5)));
    assert_eq!(kv_rep.dict_get("charID"), Some(&PyRep::Integer(boss_id)));
    assert_eq!(kv_rep.dict_get("allianceID"), Some(&PyRep::Integer(99_000_001)));

    let ad = crate::services::fleet_social::fleet_social_fleets::FleetAdvertState {
        fleet_id,
        leader_id: boss_id,
        fleet_name: "Test Fleet".into(),
        description: "Desc".into(),
        solar_system_id: 30_000_142,
        invite_scope: 3,
        auto_join: false,
        min_standing: 1.5,
        min_security: -2.0,
        allowed_entities: [90_000_002, 90_000_001].into_iter().collect(),
        _created_unix: 1_700_000_000,
    };
    let ad_rep = super::advert_to_dict(&ad, 4, None);
    assert_eq!(ad_rep.dict_get("fleetID"), Some(&PyRep::Integer(fleet_id)));
    assert_eq!(ad_rep.dict_get("joinNeedsApproval"), Some(&PyRep::Bool(true)));

    assert_eq!(super::require_fleet_advert_auth(&ctx, &boss_s).unwrap(), (boss_id, fleet_id));
    ctx.fleet_extras.lock().unwrap().adverts.insert(fleet_id, ad);
    super::sync_fleet_advert_snapshot_to_db(&ctx, fleet_id);
    assert!(ctx
        .db
        .with_snapshot(|s| s.fleet_and_social_ops.fleet_adverts.contains_key(&fleet_id)));
    ctx.fleet_extras.lock().unwrap().adverts.remove(&fleet_id);
    super::sync_fleet_advert_snapshot_to_db(&ctx, fleet_id);
    assert!(ctx
        .db
        .with_snapshot(|s| !s.fleet_and_social_ops.fleet_adverts.contains_key(&fleet_id)));
}
