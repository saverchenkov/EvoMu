//! Unit and convergence tests for `router::delta_projection`.

use crate::router::delta_projection::*;
use crate::router::ServerContext;
use eve_db::snapshot_ext::MarketOrderMeta;
use eve_db::{
    ChangeSet, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    ShipFitInputsRecord, SkillQueueEntry, TrainedSkillRecord, WorldRepository, WorldStateSnapshot,
};
use eve_dogma::ShipFittingEngine;
use eve_machonet::MachoPacketType;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_testkit::shadow_client::ShadowClient;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    if let Some(sde) = SHARED_SDE.get() {
        return Arc::clone(sde);
    }
    let ctx = ServerContext::new();
    let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
    ctx.sde
}

fn capture_diff<F: FnOnce(&mut WorldStateSnapshot)>(
    db: &WorldRepository,
    seq: u64,
    mutate: F,
) -> (WorldStateSnapshot, WorldStateSnapshot, ChangeSet) {
    let before = db.with_snapshot(|s| s.clone());
    db.with_mut_snapshot(mutate);
    let after = db.with_snapshot(|s| s.clone());
    let changes = ChangeSet::diff(seq, &before, &after);
    (before, after, changes)
}

fn apply_projected_for_char(
    shadow: &mut ShadowClient,
    char_id: i64,
    corp_id: Option<i64>,
    notifs: &[ProjectedNotification],
) {
    for n in notifs {
        let matches = match n.target {
            ProjectedTarget::Character(cid) => cid == char_id,
            ProjectedTarget::Corporation(cid) => corp_id == Some(cid),
        };
        if matches {
            shadow.apply_notification(n.event, &n.args);
        }
    }
}

#[test]
fn item_insert_move_quantity_singleton_and_delete_converge_shadow_client() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let corp_id = 98_000_101_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 990_000_050_i64;
    let item_id = 990_000_555_i64;

    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord::new(corp_id, "Delta Corp", "DLTA", pilot_id),
        );
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.corporation_id = corp_id;
            ch.station_id = Some(station_id);
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut shadow = ShadowClient::from_world(pilot_id, &db, Some(&sde));
    shadow.assert_converged_with_world(&db, Some(&sde));

    let projector = DefaultDeltaProjection::new();
    let now_unix = 1_700_000_000_u64;

    // 1. Insert stack of 500 Tritanium.
    let (b1, a1, c1) = capture_diff(&db, 1, |snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                ..Default::default()
            },
        );
    });
    let n1 = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&b1),
        &a1,
        &c1,
        &sde,
        now_unix,
    ));
    let item_notifs: Vec<_> = n1.iter().filter(|n| n.event == "OnItemChange").collect();
    assert_eq!(item_notifs.len(), 1);
    assert_eq!(item_notifs[0].args.len(), 3);
    assert_eq!(
        item_notifs[0].args[1],
        PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]),
        "new item OnItemChange must carry ixLocationID=0 (#1026)"
    );
    assert_eq!(item_notifs[0].args[2], PyRep::None);
    apply_projected_for_char(&mut shadow, pilot_id, Some(corp_id), &n1);
    shadow.assert_converged_with_world(&db, Some(&sde));

    // 2. Move item to ship cargo + reduce quantity (500 -> 350).
    let (b2, a2, c2) = capture_diff(&db, 2, |snap| {
        let it = snap
            .items
            .get_mut(&item_id)
            .expect("inserted item must exist");
        it.location_id = ship_id;
        it.flag = InventoryFlag::Cargo;
        it.quantity = 350;
    });
    let n2 = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&b2),
        &a2,
        &c2,
        &sde,
        now_unix,
    ));
    let n2_item = n2
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("move/qty mutation must emit OnItemChange");
    let PyRep::Dict(ref delta_dict) = n2_item.args[1] else {
        panic!("expected OnItemChange delta dict");
    };
    assert!(delta_dict.contains(&(PyRep::Integer(3), PyRep::Integer(station_id))));
    assert!(delta_dict.contains(&(
        PyRep::Integer(4),
        PyRep::Integer(InventoryFlag::Hangar as i64)
    )));
    assert!(delta_dict.contains(&(PyRep::Integer(9), PyRep::Integer(500))));
    assert!(
        !delta_dict.iter().any(|(k, _)| k == &PyRep::Integer(5)),
        "ixQuantity (5) must never be emitted; ixStackSize (9) is canonical"
    );
    apply_projected_for_char(&mut shadow, pilot_id, Some(corp_id), &n2);
    shadow.assert_converged_with_world(&db, Some(&sde));

    // 3. Delete item -> locJunkyard (6) tombstone.
    let (b3, a3, c3) = capture_diff(&db, 3, |snap| {
        snap.items.remove(&item_id);
    });
    let n3 = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&b3),
        &a3,
        &c3,
        &sde,
        now_unix,
    ));
    let n3_item = n3
        .iter()
        .find(|n| n.event == "OnItemChange")
        .expect("item deletion must emit tombstone OnItemChange");
    assert_eq!(
        n3_item.args[0].dbrow_get("locationID"),
        Some(&PyRep::Integer(6))
    );
    assert_eq!(
        n3_item.args[0].dbrow_get("quantity"),
        Some(&PyRep::Integer(0))
    );
    apply_projected_for_char(&mut shadow, pilot_id, Some(corp_id), &n3);
    shadow.assert_converged_with_world(&db, Some(&sde));
}

#[test]
fn corp_hangar_routing_and_cross_owner_transfer_notify_both_scopes() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let corp_id = 98_000_102_i64;
    let station_id = 60_003_760_i64;
    let item_id = 990_000_777_i64;

    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord::new(corp_id, "Corp Hangar", "CHNG", pilot_id),
        );
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: corp_id,
                location_id: station_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let (before, after, changes) = capture_diff(&db, 1, |snap| {
        let it = snap
            .items
            .get_mut(&item_id)
            .expect("corp hangar item must exist");
        it.owner_id = pilot_id;
        it.flag = InventoryFlag::Hangar;
    });

    let projector = DefaultDeltaProjection::new();
    let notifs = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&before),
        &after,
        &changes,
        &sde,
        1_700_000_000,
    ));

    let targets: BTreeSet<ProjectedTarget> = notifs
        .iter()
        .filter(|n| n.event == "OnItemChange")
        .map(|n| n.target)
        .collect();
    assert_eq!(
        targets,
        BTreeSet::from([
            ProjectedTarget::Character(pilot_id),
            ProjectedTarget::Corporation(corp_id),
        ])
    );

    let char_notif = notifs
        .iter()
        .find(|n| n.target == ProjectedTarget::Character(pilot_id))
        .expect("character target OnItemChange must exist");
    assert_eq!(char_notif.id_type, "charid");
    let corp_notif = notifs
        .iter()
        .find(|n| n.target == ProjectedTarget::Corporation(corp_id))
        .expect("corporation target OnItemChange must exist");
    assert_eq!(corp_notif.id_type, "corpid");
    let PyRep::Dict(ref dict) = corp_notif.args[1] else {
        panic!("expected delta dict");
    };
    assert!(dict.contains(&(PyRep::Integer(2), PyRep::Integer(corp_id))));
    assert!(dict.contains(&(
        PyRep::Integer(4),
        PyRep::Integer(InventoryFlag::CorpSAG2 as i64)
    )));
}

#[test]
fn blueprint_insert_in_place_metadata_update_and_dedup_emit_on_blueprints_updated() {
    let sde = shared_sde();
    let bp_type_id = *sde
        .blueprints
        .keys()
        .next()
        .expect("SDE must include at least one blueprint type");
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
    let bp1_id = 990_001_001_i64;
    let bp2_id = 990_001_002_i64;

    let (b1, a1, c1) = capture_diff(&db, 1, |snap| {
        for bid in [bp1_id, bp2_id] {
            snap.items.insert(
                bid,
                InventoryItemRecord {
                    item_id: bid,
                    type_id: bp_type_id,
                    owner_id: pilot_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    is_blueprint_copy: true,
                    blueprint_runs_remaining: Some(10),
                    ..Default::default()
                },
            );
        }
    });

    let projector = DefaultDeltaProjection::new();
    let n1 = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&b1),
        &a1,
        &c1,
        &sde,
        1_700_000_000,
    ));
    let bp_notifs1: Vec<_> = n1
        .iter()
        .filter(|n| n.event == "OnBlueprintsUpdated")
        .collect();
    assert_eq!(
        bp_notifs1.len(),
        1,
        "two blueprints for same owner must dedup"
    );
    assert_eq!(bp_notifs1[0].id_type, "ownerID");
    assert_eq!(bp_notifs1[0].args, vec![PyRep::Integer(pilot_id)]);

    // In-place run decrement emits OnBlueprintsUpdated without redundant OnItemChange.
    let (b2, a2, c2) = capture_diff(&db, 2, |snap| {
        snap.items
            .get_mut(&bp1_id)
            .expect("bp1 must exist")
            .blueprint_runs_remaining = Some(9);
    });
    let n2 = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&b2),
        &a2,
        &c2,
        &sde,
        1_700_000_000,
    ));
    assert_eq!(n2.iter().filter(|n| n.event == "OnItemChange").count(), 0);
    assert_eq!(
        n2.iter()
            .filter(|n| n.event == "OnBlueprintsUpdated")
            .count(),
        1
    );
}

#[test]
fn personal_and_corp_division_wallets_project_on_account_change_and_converge_shadow() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let corp_id = 98_000_201_i64;

    db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::new(corp_id, "Wallet Corp", "WLTC", pilot_id);
        corp.set_division_balance(1, IskCents::from_isk(1_000_000));
        corp.set_division_balance(4, IskCents::from_isk(500_000));
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
    });

    let mut shadow = ShadowClient::from_world(pilot_id, &db, Some(&sde));
    shadow.assert_converged_with_world(&db, Some(&sde));

    let (before, after, changes) = capture_diff(&db, 1, |snap| {
        snap.characters
            .get_mut(&pilot_id)
            .expect("pilot must exist")
            .balance = IskCents::from_isk(12_345_678);
        let corp = snap
            .corporations
            .get_mut(&corp_id)
            .expect("corp must exist");
        corp.set_division_balance(1, IskCents::from_isk(2_250_000));
        corp.set_division_balance(4, IskCents::from_isk(875_000));
        corp.set_division_balance(7, IskCents::from_isk(42_000));
    });

    let projector = DefaultDeltaProjection::new();
    let notifs = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&before),
        &after,
        &changes,
        &sde,
        1_700_000_000,
    ));

    let acct_notifs: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnAccountChange")
        .collect();
    assert_eq!(acct_notifs.len(), 4);
    assert!(acct_notifs.iter().any(|n| {
        n.target == ProjectedTarget::Character(pilot_id)
            && n.id_type == "charid"
            && n.args
                == vec![
                    PyRep::String("cash".into()),
                    PyRep::Integer(pilot_id),
                    PyRep::Real(12_345_678.0),
                ]
    }));
    assert!(acct_notifs.iter().any(|n| {
        n.target == ProjectedTarget::Corporation(corp_id)
            && n.id_type == "corpid"
            && n.args
                == vec![
                    PyRep::String("cash".into()),
                    PyRep::Integer(corp_id),
                    PyRep::Real(2_250_000.0),
                ]
    }));
    assert!(acct_notifs.iter().any(|n| {
        n.target == ProjectedTarget::Corporation(corp_id)
            && n.id_type == "corpid"
            && n.args
                == vec![
                    PyRep::String("cash4".into()),
                    PyRep::Integer(corp_id),
                    PyRep::Real(875_000.0),
                ]
    }));
    assert!(acct_notifs.iter().any(|n| {
        n.target == ProjectedTarget::Corporation(corp_id)
            && n.id_type == "corpid"
            && n.args
                == vec![
                    PyRep::String("cash7".into()),
                    PyRep::Integer(corp_id),
                    PyRep::Real(42_000.0),
                ]
    }));

    apply_projected_for_char(&mut shadow, pilot_id, Some(corp_id), &notifs);
    shadow.assert_converged_with_world(&db, Some(&sde));
}

#[test]
fn delta_state_cache_projects_corp_wallets_and_paused_queues_without_before_snapshot() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let corp_id = 98_000_202_i64;

    db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::new(corp_id, "Cache Corp", "CCHC", pilot_id);
        corp.set_division_balance(3, IskCents::from_isk(100_000));
        snap.corporations.insert(corp_id, corp);
    });

    let cache = DeltaStateCache::new();
    db.with_snapshot(|snap| cache.seed_from_snapshot(snap));

    let (_before, after, changes) = capture_diff(&db, 1, |snap| {
        snap.corporations
            .get_mut(&corp_id)
            .expect("corp must exist")
            .set_division_balance(3, IskCents::from_isk(350_000));
        snap.paused_skill_queues.insert(pilot_id);
    });

    let projector = DefaultDeltaProjection::new();
    let notifs =
        cache.project_with_cache(&projector, None, &after, &changes, &sde, 1_700_000_000, &[]);

    assert!(notifs.iter().any(|n| {
        n.event == "OnAccountChange"
            && n.target == ProjectedTarget::Corporation(corp_id)
            && n.id_type == "corpid"
            && n.args
                == vec![
                    PyRep::String("cash3".into()),
                    PyRep::Integer(corp_id),
                    PyRep::Real(350_000.0),
                ]
    }));
    assert!(notifs.iter().any(|n| {
        n.event == "OnSkillQueuePausedServer"
            && n.target == ProjectedTarget::Character(pilot_id)
            && n.args.is_empty()
    }));
}

#[test]
fn skills_free_sp_and_skill_queue_project_v24_notifications() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let added_skill = 3300_u32;
    let removed_skill = 3301_u32;

    db.with_mut_snapshot(|snap| {
        snap.skills.entry(pilot_id).or_default().insert(
            removed_skill,
            TrainedSkillRecord {
                type_id: removed_skill,
                level: 2,
                skill_points: 1_415.0,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&pilot_id) {
            ch.free_skill_points = 50_000.0;
        }
    });

    let (before, after, changes) = capture_diff(&db, 1, |snap| {
        let sk = snap.skills.entry(pilot_id).or_default();
        sk.remove(&removed_skill);
        sk.insert(
            added_skill,
            TrainedSkillRecord {
                type_id: added_skill,
                level: 3,
                skill_points: 8_000.0,
            },
        );
        snap.characters
            .get_mut(&pilot_id)
            .expect("pilot must exist")
            .free_skill_points = 42_000.0;
        snap.skill_queues.insert(
            pilot_id,
            vec![SkillQueueEntry {
                type_id: added_skill,
                target_level: 4,
                start_sp: 8_000.0,
                end_sp: 45_255.0,
                duration_seconds: 3_600.0,
                started_at_unix: Some(1_700_000_000),
            }],
        );
    });

    let projector = DefaultDeltaProjection::new();
    let notifs = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&before),
        &after,
        &changes,
        &sde,
        1_700_000_000,
    ));

    let changed = notifs
        .iter()
        .find(|n| n.event == "OnServerSkillsChanged")
        .expect("OnServerSkillsChanged");
    assert_eq!(changed.args.len(), 3);
    let PyRep::Dict(ref changed_dict) = changed.args[0] else {
        panic!("expected dict");
    };
    assert_eq!(changed_dict[0].0, PyRep::Integer(i64::from(added_skill)));
    assert_eq!(
        changed_dict[0].1.dict_get("trainedSkillLevel"),
        Some(&PyRep::Integer(3))
    );

    let removed = notifs
        .iter()
        .find(|n| n.event == "OnServerSkillsRemoved")
        .expect("OnServerSkillsRemoved");
    assert_eq!(removed.args.len(), 2);
    let PyRep::Dict(ref rem_dict) = removed.args[0] else {
        panic!("expected dict");
    };
    assert_eq!(rem_dict[0].0, PyRep::Integer(i64::from(removed_skill)));

    let free_sp = notifs
        .iter()
        .find(|n| n.event == "OnFreeSkillPointsChanged")
        .expect("OnFreeSkillPointsChanged");
    assert_eq!(free_sp.args, vec![PyRep::Real(42_000.0)]);

    let sq = notifs
        .iter()
        .find(|n| n.event == "OnNewSkillQueueSaved")
        .expect("OnNewSkillQueueSaved");
    let PyRep::List(ref q_rows) = sq.args[0] else {
        panic!("expected queue list");
    };
    assert_eq!(q_rows.len(), 1);
    assert_eq!(
        q_rows[0].dict_get("trainingTypeID"),
        Some(&PyRep::Integer(i64::from(added_skill)))
    );
    assert_eq!(
        q_rows[0].dict_get("trainingToLevel"),
        Some(&PyRep::Integer(4))
    );
}

#[test]
fn market_orders_personal_and_corp_project_on_own_orders_changed_and_skip_npc() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let corp_id = 98_000_301_i64;
    let station_id = 60_003_760_i64;
    let char_order_id = 880_000_001_i64;
    let corp_order_id = 880_000_002_i64;
    let npc_order_id = 880_000_003_i64;

    db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            corp_id,
            CorporationRecord::new(corp_id, "Market Corp", "MRKT", pilot_id),
        );
        snap.market_orders.insert(
            corp_order_id,
            MarketOrderRecord {
                order_id: corp_order_id,
                owner_id: corp_id,
                station_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(5),
                vol_entered: 1000,
                vol_remaining: 400,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(2000),
            },
        );
        snap.market_order_meta.insert(
            corp_order_id,
            MarketOrderMeta {
                issued_unix: 1_699_900_000,
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(pilot_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: None,
            },
        );
    });

    let (before, after, changes) = capture_diff(&db, 1, |snap| {
        snap.market_orders.insert(
            char_order_id,
            MarketOrderRecord {
                order_id: char_order_id,
                owner_id: pilot_id,
                station_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(6),
                vol_entered: 500,
                vol_remaining: 500,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.insert(
            npc_order_id,
            MarketOrderRecord {
                order_id: npc_order_id,
                owner_id: 1_000_125,
                station_id,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(100),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: true,
                escrow: IskCents(0),
            },
        );
        snap.market_orders.remove(&corp_order_id);
        snap.market_order_meta.insert(
            corp_order_id,
            MarketOrderMeta {
                issued_unix: 1_699_900_000,
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(pilot_id),
                wallet_division: Some(1),
                is_corporation: true,
                state: Some("expired".into()),
            },
        );
    });

    let projector = DefaultDeltaProjection::new();
    let notifs = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&before),
        &after,
        &changes,
        &sde,
        1_700_000_000,
    ));
    let order_notifs: Vec<_> = notifs
        .iter()
        .filter(|n| n.event == "OnOwnOrdersChanged")
        .collect();
    assert_eq!(order_notifs.len(), 2, "NPC orders must be excluded");

    let char_n = order_notifs
        .iter()
        .find(|n| n.target == ProjectedTarget::Character(pilot_id))
        .expect("character order OnOwnOrdersChanged");
    assert_eq!(char_n.args[1], PyRep::String("Add".into()));
    assert_eq!(char_n.args[2], PyRep::Bool(false));

    let corp_n = order_notifs
        .iter()
        .find(|n| n.target == ProjectedTarget::Corporation(corp_id))
        .expect("corp order OnOwnOrdersChanged");
    assert_eq!(corp_n.id_type, "corpid");
    assert_eq!(corp_n.args[1], PyRep::String("Expiry".into()));
    assert_eq!(corp_n.args[2], PyRep::Bool(true));
}

#[test]
fn fit_godma_projection_and_custom_plex_hooks_emit_canonical_notifications() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let ship_id = 990_009_001_i64;
    let mod_id = 990_009_002_i64;

    let fit_before = ShipFittingEngine::from_sde(ship_id, 587, &sde);
    let snap_before = FitProjectionSnapshot::from_fitting_engine(&fit_before, &sde);

    let mut snap_after = snap_before.clone();
    snap_after
        .ship_attributes
        .insert(eve_sde::attr_ids::CPU_LOAD, 15.0);
    snap_after
        .module_attributes
        .insert(mod_id, BTreeMap::from([(2_u16, 0.0)]));
    let mut snap_before_mod = snap_before;
    snap_before_mod
        .module_attributes
        .insert(mod_id, BTreeMap::from([(2_u16, 1.0)]));

    let fit_diffs = [FitProjectionDiff {
        char_id: pilot_id,
        ship_id,
        before: snap_before_mod,
        after: snap_after,
    }];

    struct CapturePlexHook(Arc<Mutex<Vec<PlexDelta>>>);
    impl DeltaProjectionHook for CapturePlexHook {
        fn on_plex_delta(&self, delta: PlexDelta, _sink: &mut dyn DeltaSink) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(delta);
        }
    }

    let captured_plex = Arc::new(Mutex::new(Vec::new()));
    let mut projector = DefaultDeltaProjection::new();
    projector.add_hook(Box::new(CapturePlexHook(Arc::clone(&captured_plex))));

    let (before, after, changes) = capture_diff(&db, 1, |snap| {
        snap.characters
            .get_mut(&pilot_id)
            .expect("pilot must exist")
            .plex_balance = 500;
    });

    let notifs = projector.project_to_vec(
        &DeltaProjectionInput::new(Some(&before), &after, &changes, &sde, 1_700_000_000)
            .with_fit_diffs(&fit_diffs),
    );

    let godma = notifs
        .iter()
        .find(|n| n.event == "OnModuleAttributeChanges")
        .expect("OnModuleAttributeChanges must be projected from fit diff");
    let PyRep::List(ref entries) = godma.args[0] else {
        panic!("expected change tuple list");
    };
    assert!(entries.len() >= 2);
    assert!(entries.iter().any(|e| {
        matches!(
            e,
            PyRep::Tuple(t) if t.len() == 8
                && t[0] == PyRep::String("OnModuleAttributeChange".into())
                && t[2] == PyRep::Integer(mod_id)
                && t[3] == PyRep::Integer(2)
                && t[5] == PyRep::Real(0.0)
                && t[6] == PyRep::Real(1.0)
        )
    }));

    let plex_events = captured_plex.lock().unwrap_or_else(|e| e.into_inner());
    assert_eq!(plex_events.len(), 1);
    assert_eq!(plex_events[0].new_plex, 500);
}

#[test]
fn recipient_resolution_filters_offline_sessions_and_materializes_macho_packets() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let p1 = 90_000_001_i64;
    let p2 = 90_000_002_i64;
    let corp_id = 98_000_401_i64;

    let uid1 = db.with_snapshot(|s| s.characters[&p1].user_id);
    db.with_mut_snapshot(|snap| {
        let mut corp = CorporationRecord::new(corp_id, "Recipients Corp", "RCPS", p1);
        corp.members.insert(p1);
        corp.members.insert(p2);
        snap.corporations.insert(corp_id, corp);
        snap.characters
            .get_mut(&p1)
            .expect("p1 must exist")
            .corporation_id = corp_id;
        snap.characters
            .get_mut(&p2)
            .expect("p2 must exist")
            .corporation_id = corp_id;
    });

    // Only p1 is logged in via real WorldRepository online session lock; p2 stays offline.
    let _ = db.acquire_online_lock(uid1, 501, Some(p1), false, 1_700_000_000);

    let (before, after, changes) = capture_diff(&db, 1, |snap| {
        snap.corporations
            .get_mut(&corp_id)
            .expect("corp must exist")
            .set_division_balance(2, IskCents::from_isk(999_000));
    });

    let projector = DefaultDeltaProjection::new();
    let resolved = projector.project_and_resolve(
        &DeltaProjectionInput::new(Some(&before), &after, &changes, &sde, 1_700_000_000),
        &db,
    );
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].recipient_char_id, p1);
    assert_eq!(resolved[0].event, "OnAccountChange");
    assert_eq!(resolved[0].id_type, "corpid");

    let pending = materialize_pending_notifications(resolved, unix_to_filetime(1_700_000_000));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].packet.packet_type, MachoPacketType::Notification);
    assert!(!pending[0].marshaled_packet.is_empty());

    // Passing explicit online set covering both p1 and p2 resolves both, sorted & deduplicated.
    let both_online = BTreeSet::from([p1, p2]);
    let resolved_both = projector.project_and_resolve(
        &DeltaProjectionInput::new(Some(&before), &after, &changes, &sde, 1_700_000_000),
        &both_online,
    );
    let recipient_ids: Vec<i64> = resolved_both.iter().map(|r| r.recipient_char_id).collect();
    assert_eq!(recipient_ids, vec![p1, p2]);
}

#[test]
fn account_plex_vault_domain_rows_and_negative_no_op_mutations_behave_cleanly() {
    let sde = shared_sde();
    let db = WorldRepository::with_demo_seed();
    let pilot_id = 90_000_001_i64;
    let uid = db.with_snapshot(|s| s.characters[&pilot_id].user_id);
    let ship_id = 990_009_100_i64;
    let station_id = 60_003_760_i64;

    db.with_mut_snapshot(|snap| {
        snap.account_plex_vault.insert(uid, 250);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: pilot_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    struct CapturePlexHook(Arc<Mutex<Vec<PlexDelta>>>);
    impl DeltaProjectionHook for CapturePlexHook {
        fn on_plex_delta(&self, delta: PlexDelta, _sink: &mut dyn DeltaSink) {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(delta);
        }
    }

    let captured = Arc::new(Mutex::new(Vec::new()));
    let mut projector = DefaultDeltaProjection::new();
    projector.add_hook(Box::new(CapturePlexHook(Arc::clone(&captured))));

    let cache = DeltaStateCache::new();
    db.with_snapshot(|snap| cache.seed_from_snapshot(snap));

    // 1. Account PLEX vault change via domain_rows works both with before=Some and via DeltaStateCache (before=None).
    let (b_plex, a_plex, c_plex) = capture_diff(&db, 1, |snap| {
        snap.account_plex_vault.insert(uid, 750);
    });
    let _ = projector.project_to_vec(&DeltaProjectionInput::new(
        Some(&b_plex),
        &a_plex,
        &c_plex,
        &sde,
        1_700_000_000,
    ));
    let _ = cache.project_with_cache(&projector, None, &a_plex, &c_plex, &sde, 1_700_000_000, &[]);
    {
        let events = captured.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(events.len(), 2);
        for ev in events.iter() {
            assert_eq!(ev.char_id, None);
            assert_eq!(ev.user_id, uid);
            assert_eq!(ev.old_plex, 250);
            assert_eq!(ev.new_plex, 750);
        }
    }

    // 2. Negative: empty ChangeSet emits 0 notifications.
    let empty_changes = ChangeSet::diff(2, &a_plex, &a_plex);
    assert!(
        projector
            .project_to_vec(&DeltaProjectionInput::new(
                Some(&a_plex),
                &a_plex,
                &empty_changes,
                &sde,
                1_700_000_000,
            ))
            .is_empty()
    );

    // 3. Negative: ship_fit_inputs mutation (#861, sets d6_batch4) emits 0 spurious inventory/skill/wallet deltas.
    let (b_fit, a_fit, c_fit) = capture_diff(&db, 3, |snap| {
        snap.ship_fit_inputs.insert(
            ship_id,
            ShipFitInputsRecord {
                active_stance_type_id: Some(34562),
                last_stance_switch_ms: Some(1_700_000_000_000),
                overloaded_modules: BTreeSet::new(),
            },
        );
    });
    assert!(c_fit.cold_domains.d6_batch4);
    assert!(
        projector
            .project_to_vec(&DeltaProjectionInput::new(
                Some(&b_fit),
                &a_fit,
                &c_fit,
                &sde,
                1_700_000_000,
            ))
            .is_empty()
    );
    assert!(
        cache
            .project_with_cache(&projector, None, &a_fit, &c_fit, &sde, 1_700_000_000, &[])
            .is_empty()
    );

    // 4. Negative: NPC-owned item and non-structural item metadata updates (insured_payout_isk/version) emit 0 OnItemChange.
    let (b_meta, a_meta, c_meta) = capture_diff(&db, 4, |snap| {
        let ship = snap.items.get_mut(&ship_id).expect("ship must exist");
        ship.insured_payout_isk = 500_000.0;
        ship.version += 1;
        snap.items.insert(
            990_009_101,
            InventoryItemRecord {
                item_id: 990_009_101,
                type_id: 34,
                owner_id: 1_000_125, // NPC corp, neither player character nor player corp
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                ..Default::default()
            },
        );
    });
    assert!(
        projector
            .project_to_vec(&DeltaProjectionInput::new(
                Some(&b_meta),
                &a_meta,
                &c_meta,
                &sde,
                1_700_000_000,
            ))
            .is_empty()
    );

    // 5. Negative: unchanged FitProjectionSnapshot emits None (no OnModuleAttributeChanges).
    let fit = ShipFittingEngine::from_sde(ship_id, 587, &sde);
    let fit_snap = FitProjectionSnapshot::from_fitting_engine(&fit, &sde);
    assert!(
        project_fit_attribute_diff(
            pilot_id,
            ship_id,
            &fit_snap,
            &fit_snap,
            unix_to_filetime(1_700_000_000),
        )
        .is_none()
    );
}
