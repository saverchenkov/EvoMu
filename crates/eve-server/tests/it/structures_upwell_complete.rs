//! Integration tests for the Upwell structure services in `structure_upwell_ops`:
//! manual asset safety, structure descriptions, deliveries, in-space cargo drop-off,
//! structure guests and hangar view state, plus untagged regression coverage for
//! unanchor/cancel, rename, control and the vulnerability schedule.

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn kv_entry<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.get(key),
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    kv_entry(rep, key).and_then(|v| v.as_i64()).unwrap_or(0)
}

fn kv_f64(rep: &PyRep, key: &str) -> f64 {
    kv_entry(rep, key).and_then(|v| v.as_f64()).unwrap_or(0.0)
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login(ctx: &ServerContext, sess_id: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    rpc(ctx, &mut sess, "authentication", "Login", &[PyRep::String(user.into()), PyRep::String(pass.into())]).await.expect("Login");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    sess
}

async fn login_with_corp(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let mut sess = login(ctx, sess_id, "demo", "demo-secret", char_id).await;
    rpc(ctx, &mut sess, "corpRegistry", "AddCorporation", &[PyRep::String("Upwell Sovereign Holdings".into()), PyRep::String("UPWL".into())])
        .await
        .expect("AddCorporation");
    sess
}

fn grant_item(ctx: &ServerContext, owner_id: i64, location_id: i64, type_id: u32, flag: InventoryFlag, quantity: i32) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton: quantity == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    })
}

/// Undocks, flies 5,000,000 km off the station (anchoring needs 1,000 km clearance), anchors an
/// Astrahus (SDE type 35832), runs the anchoring timers, inserts its quantum core and lets it
/// finish onlining. Returns the structure ID with the pilot undocked next to it.
async fn undock_and_anchor(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, name: &str) -> i64 {
    use eve_server::services::station_structure::structure_lifecycle as life;
    rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    let ship_id = sess.ship_id.expect("ship_id");
    let sys = sess.solar_system_id.expect("system");
    let station = ctx.sde.stations.values().filter(|s| s.solar_system_id == sys).map(|s| s.position).next().expect("station");
    assert!(ctx.systems.get_or_spawn(sys).set_ball_position(ship_id, station + glam::DVec3::new(0.0, 5.0e9, 0.0)).await);
    grant_item(ctx, char_id, ship_id, 35832, InventoryFlag::Cargo, 1);
    let anchored = rpc(ctx, sess, "structureDeployment", "Anchor", &[PyRep::Integer(35832), PyRep::String(name.into())]).await.expect("Anchor");
    let id = kv_i64(&anchored, "structureID");
    assert!(id > 0);
    ctx.advance_virtual_clock(life::ANCHOR_VULNERABLE_SECS + life::ANCHORING_SECS);
    rpc(ctx, sess, "structureDocking", "Dock", &[PyRep::Integer(id)]).await.expect("Dock");
    let core = grant_item(ctx, char_id, ship_id, 56201, InventoryFlag::Cargo, 1);
    rpc(ctx, sess, "invbroker", "Add", &[PyRep::Integer(core), PyRep::Integer(id), PyRep::Integer(4)]).await.expect("insert core");
    rpc(ctx, sess, "structureDocking", "Undock", &[]).await.expect("Undock");
    ctx.advance_virtual_clock(life::ONLINING_SECS);
    rpc(ctx, sess, "structureGuests", "GetGuests", &[PyRep::Integer(id)]).await.expect("sync");
    ctx.db.with_snapshot(|snap| assert_eq!(snap.structures.get(&id).map(|s| s.state.clone()).as_deref(), Some(life::STATE_SHIELD_VULNERABLE)));
    id
}

fn balance(ctx: &ServerContext, char_id: i64) -> eve_db::IskCents {
    ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.balance).expect("character"))
}

#[tokio::test]
async fn test_manual_asset_safety_personal_and_corp_with_delay_and_fee() {
    // parity: feat:structures.asset-safety-manual
    eve_testkit::attest(&["feat:structures.asset-safety-manual"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = login_with_corp(&ctx, 3001, char_id).await;
    let struct_id = undock_and_anchor(&ctx, &mut sess, char_id, "Safety Astrahus").await;
    let sys_id = i64::from(sess.solar_system_id.expect("system"));
    let corp_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.corporation_id).expect("corp"));
    let station_id = 60_003_760;
    assert!(ctx.sde.stations.contains_key(&station_id), "destination station must exist in the SDE");

    // Nothing to move yet.
    let empty = rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(sys_id), PyRep::Integer(struct_id)])
        .await
        .expect_err("no personal assets");
    assert_eq!(empty.code, "NoAssetsToMoveToSafety");

    // Personal assets.
    let personal_item = grant_item(&ctx, char_id, struct_id, 34, InventoryFlag::Hangar, 1_000);
    let personal = rpc(&ctx, &mut sess, "structureAssetSafety", "MovePersonalAssetsToSafety", &[PyRep::Integer(sys_id), PyRep::Integer(struct_id)])
        .await
        .expect("MovePersonalAssetsToSafety");
    let personal_wrap = kv_i64(&personal, "wrapID");
    assert_eq!(kv_i64(&personal, "ownerID"), char_id);
    assert_eq!(kv_i64(&personal, "itemCount"), 1);
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&personal_item).expect("item");
        assert_eq!(it.location_id, personal_wrap);
        assert_eq!(it.flag, InventoryFlag::Locked);
    });

    // Corporation assets.
    let corp_item = grant_item(&ctx, corp_id, struct_id, 35, InventoryFlag::Hangar, 400);
    let corp = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveCorpAssetsToSafety", &[PyRep::Integer(sys_id), PyRep::Integer(struct_id)])
        .await
        .expect("MoveCorpAssetsToSafety");
    let corp_wrap = kv_i64(&corp, "wrapID");
    assert_eq!(kv_i64(&corp, "ownerID"), corp_id);
    assert_ne!(corp_wrap, personal_wrap);

    let mine = rpc(&ctx, &mut sess, "structureAssetSafety", "GetItemsInSafetyForCharacter", &[]).await.expect("GetItemsInSafetyForCharacter");
    let PyRep::List(mine_rows) = &mine else { panic!("list expected") };
    assert_eq!(mine_rows.len(), 1);
    assert_eq!(kv_i64(&mine_rows[0], "assetWrapID"), personal_wrap);
    let corp_rows = rpc(&ctx, &mut sess, "structureAssetSafety", "GetItemsInSafetyForCorp", &[]).await.expect("GetItemsInSafetyForCorp");
    let PyRep::List(corp_rows) = &corp_rows else { panic!("list expected") };
    assert_eq!(corp_rows.len(), 1);
    assert_eq!(kv_i64(&corp_rows[0], "assetWrapID"), corp_wrap);

    let names = rpc(&ctx, &mut sess, "structureAssetSafety", "GetWrapNames", &[PyRep::List(vec![PyRep::Integer(personal_wrap)])]).await.expect("GetWrapNames");
    let PyRep::Dict(name_entries) = &names else { panic!("dict expected") };
    assert_eq!(name_entries.len(), 1);
    assert_eq!(name_entries[0].0.as_i64(), Some(personal_wrap));
    assert_eq!(name_entries[0].1.as_str(), Some("Safety Astrahus"));

    // Delivery is refused before the 5-day delay elapses.
    let early = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(personal_wrap), PyRep::Integer(station_id)])
        .await
        .expect_err("5-day delay");
    assert_eq!(early.code, "AssetSafetyCooldownActive");
    ctx.advance_virtual_clock(5 * 86_400 - 120);
    let still_early = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(personal_wrap), PyRep::Integer(station_id)])
        .await
        .expect_err("still inside the 5-day delay");
    assert_eq!(still_early.code, "AssetSafetyCooldownActive");
    ctx.advance_virtual_clock(180);

    // After 5 days: delivered for 0.5% (same-system recovery within 20 days) of the SDE-derived estimated value.
    let trit_price = ctx.sde.types.get(&34).map(|t| t.base_price).expect("Tritanium in SDE");
    let expected_fee = eve_db::IskCents::from_f64_isk(trit_price * 1_000.0 * 0.005);
    let before = balance(&ctx, char_id);
    let delivered = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(personal_wrap), PyRep::Integer(station_id)])
        .await
        .expect("MoveSafetyWrapToStructure");
    assert_eq!(kv_i64(&delivered, "destinationID"), station_id);
    assert!((kv_f64(&delivered, "feePaidISK") - expected_fee.to_wire_f64()).abs() < 0.01);
    assert_eq!(balance(&ctx, char_id), before.checked_sub(expected_fee).expect("fee <= balance"));
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&personal_item).expect("item");
        assert_eq!(it.location_id, station_id);
        assert_eq!(it.flag, InventoryFlag::Deliveries);
    });
    let again = rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(personal_wrap), PyRep::Integer(station_id)])
        .await
        .expect_err("already delivered");
    assert_eq!(again.code, "AssetSafetyWrapAlreadyDelivered");

    // Corp wrap delivers too.
    rpc(&ctx, &mut sess, "structureAssetSafety", "MoveSafetyWrapToStructure", &[PyRep::Integer(corp_wrap), PyRep::Integer(station_id)]).await.expect("deliver corp wrap");
    ctx.db.with_snapshot(|snap| assert_eq!(snap.items.get(&corp_item).expect("corp item").location_id, station_id));
}

#[tokio::test]
async fn test_structure_description_deliveries_cargo_dropoff_guests_and_hangar_view() {
    // parity: feat:structures.structure-description, feat:structures.deliveries, feat:structures.cargo-dropoff, feat:structures.guests, feat:structures.hangar-view
    eve_testkit::attest(&[
        "feat:structures.structure-description",
        "feat:structures.deliveries",
        "feat:structures.cargo-dropoff",
        "feat:structures.guests",
        "feat:structures.hangar-view",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let other_id = 90_000_002;
    let mut sess = login_with_corp(&ctx, 3002, char_id).await;
    let mut other = login(&ctx, 3003, "other", "other-secret", other_id).await;
    let struct_id = undock_and_anchor(&ctx, &mut sess, char_id, "Trade Hub Astrahus").await;
    let ship_id = sess.ship_id.expect("ship");

    // Hangar view requires being docked in a structure.
    let not_docked = rpc(&ctx, &mut sess, "structureHangarViewMgr", "GetMyHangarViewState", &[]).await.expect_err("not docked");
    assert_eq!(not_docked.code, "NotDockedInStructure");

    // Cargo drop-off from space into the structure's deliveries.
    let ore = grant_item(&ctx, char_id, ship_id, 1230, InventoryFlag::Cargo, 50);
    let not_mine = grant_item(&ctx, other_id, 60_003_760, 34, InventoryFlag::Hangar, 1);
    let bad = rpc(&ctx, &mut sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(struct_id), PyRep::List(vec![PyRep::Integer(not_mine)])])
        .await
        .expect_err("item not in cargo");
    assert_eq!(bad.code, "ItemNotInShipCargo");
    rpc(&ctx, &mut sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(struct_id), PyRep::List(vec![PyRep::Integer(ore)])]).await.expect("DropOffItems");
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&ore).expect("ore");
        assert_eq!(it.location_id, struct_id);
        assert_eq!(it.flag, InventoryFlag::Deliveries);
        assert_eq!(it.owner_id, char_id);
    });

    // Dock and check guests + hangar view.
    rpc(&ctx, &mut sess, "structureDocking", "Dock", &[PyRep::Integer(struct_id)]).await.expect("Dock");
    let docked_drop = rpc(&ctx, &mut sess, "structureCargoDelivery", "DropOffItems", &[PyRep::Integer(struct_id), PyRep::List(vec![PyRep::Integer(ore)])])
        .await
        .expect_err("drop-off is an in-space action");
    assert_eq!(docked_drop.code, "MustBeInSpace");
    let guests = rpc(&ctx, &mut other, "structureGuests", "GetGuests", &[PyRep::Integer(struct_id)]).await.expect("GetGuests");
    let PyRep::List(guest_rows) = &guests else { panic!("list expected") };
    let guest_ids: Vec<i64> = guest_rows.iter().filter_map(|g| if let PyRep::Tuple(t) = g { t.first().and_then(|v| v.as_i64()) } else { None }).collect();
    assert_eq!(guest_ids, vec![char_id], "only the docked character is a guest");

    let view = rpc(&ctx, &mut sess, "structureHangarViewMgr", "GetMyHangarViewState", &[]).await.expect("GetMyHangarViewState");
    assert_eq!(kv_i64(&view, "structureID"), struct_id);
    assert_eq!(kv_i64(&view, "structureTypeID"), 35832);
    assert_eq!(kv_i64(&view, "shipID"), ship_id);

    // Deliveries: send an item in the structure hangar to another character's deliveries.
    let gift = grant_item(&ctx, char_id, struct_id, 34, InventoryFlag::Hangar, 100);
    let no_recipient = rpc(&ctx, &mut sess, "structureDeliveries", "Deliver", &[PyRep::Integer(struct_id), PyRep::Integer(gift), PyRep::Integer(1)])
        .await
        .expect_err("unknown recipient");
    assert_eq!(no_recipient.code, "RecipientNotFound");
    rpc(&ctx, &mut sess, "structureDeliveries", "Deliver", &[PyRep::Integer(struct_id), PyRep::Integer(gift), PyRep::Integer(other_id)]).await.expect("Deliver");
    ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&gift).expect("gift");
        assert_eq!(it.owner_id, other_id);
        assert_eq!(it.location_id, struct_id);
        assert_eq!(it.flag, InventoryFlag::Deliveries);
    });
    let resend = rpc(&ctx, &mut sess, "structureDeliveries", "Deliver", &[PyRep::Integer(struct_id), PyRep::Integer(gift), PyRep::Integer(other_id)])
        .await
        .expect_err("no longer owned by sender");
    assert_eq!(resend.code, "ItemNotInStructureHangar");

    // Description: owner sets, anyone reads; outsiders cannot set.
    let initial = rpc(&ctx, &mut other, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(struct_id)]).await.expect("Get empty");
    assert_eq!(initial, PyRep::None);
    rpc(&ctx, &mut sess, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(struct_id), PyRep::String("Open market, low tax".into())])
        .await
        .expect("SetStructureDescription");
    let read = rpc(&ctx, &mut other, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(struct_id)]).await.expect("GetStructureDescription");
    assert_eq!(read.as_str(), Some("Open market, low tax"));
    let denied = rpc(&ctx, &mut other, "structureDirectory", "SetStructureDescription", &[PyRep::Integer(struct_id), PyRep::String("vandalism".into())])
        .await
        .expect_err("outsider cannot set description");
    assert_eq!(denied.code, "StructureOwnershipViolation");
    let persisted = rpc(&ctx, &mut sess, "structureDirectory", "GetStructureDescription", &[PyRep::Integer(struct_id)]).await.expect("Get persisted");
    assert_eq!(persisted.as_str(), Some("Open market, low tax"));
}

#[tokio::test]
async fn test_structure_unanchor_rename_control_and_schedule_regressions() {
    // parity: feat:structures.rename, feat:structures.vulnerability-schedule, feat:structures.structure-control
    eve_testkit::attest(&["feat:structures.rename", "feat:structures.vulnerability-schedule", "feat:structures.structure-control"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = login_with_corp(&ctx, 3004, char_id).await;
    let struct_id = undock_and_anchor(&ctx, &mut sess, char_id, "Regression Astrahus").await;

    let cancel_idle = rpc(&ctx, &mut sess, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(struct_id)]).await.expect_err("not unanchoring");
    assert_eq!(cancel_idle.code, "StructureNotUnanchoring");
    let started = rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(struct_id)]).await.expect("Unanchor");
    assert_eq!(kv_i64(&started, "unanchorCompleteUnix"), ctx.now_unix() as i64 + 7 * 86_400);
    let twice = rpc(&ctx, &mut sess, "structureDeployment", "Unanchor", &[PyRep::Integer(struct_id)]).await.expect_err("already unanchoring");
    assert_eq!(twice.code, "StructureAlreadyUnanchoring");
    rpc(&ctx, &mut sess, "structureDeployment", "CancelUnanchor", &[PyRep::Integer(struct_id)]).await.expect("CancelUnanchor");

    let blank = rpc(&ctx, &mut sess, "structureDeployment", "RenameStructure", &[PyRep::Integer(struct_id), PyRep::String("   ".into())]).await.expect_err("blank");
    assert_eq!(blank.code, "InvalidStructureName");
    rpc(&ctx, &mut sess, "structureDeployment", "RenameStructure", &[PyRep::Integer(struct_id), PyRep::String("Aegis Prime".into())]).await.expect("rename");
    ctx.db.with_snapshot(|snap| assert_eq!(snap.structures.get(&struct_id).expect("structure").name, "Aegis Prime"));

    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.expect("pilot"), PyRep::None);
    sess.structure_id = Some(struct_id);
    sess.station_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.structure_id = Some(struct_id);
            ch.station_id = None;
        }
    });
    rpc(&ctx, &mut sess, "structureControl", "TakeControl", &[PyRep::Integer(struct_id)]).await.expect("TakeControl");
    assert_eq!(rpc(&ctx, &mut sess, "structureControl", "GetStructurePilot", &[PyRep::Integer(struct_id)]).await.expect("pilot"), PyRep::Integer(char_id));
    rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(struct_id)]).await.expect("ReleaseControl");
    let release_again = rpc(&ctx, &mut sess, "structureControl", "ReleaseControl", &[PyRep::Integer(struct_id)]).await.expect_err("not controlling");
    assert_eq!(release_again.code, "NotControllingStructure");

    let bad = rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(struct_id), PyRep::Integer(29), PyRep::Integer(2)])
        .await
        .expect_err("hour 29");
    assert_eq!(bad.code, "InvalidReinforceSchedule");
    let sched = rpc(&ctx, &mut sess, "structureVulnerability", "UpdateReinforceTimesFromClient", &[PyRep::Integer(struct_id), PyRep::Integer(19), PyRep::Integer(4)])
        .await
        .expect("schedule");
    assert_eq!(kv_i64(&sched, "applyAfterUnix"), ctx.now_unix() as i64 + 86_400);
}
