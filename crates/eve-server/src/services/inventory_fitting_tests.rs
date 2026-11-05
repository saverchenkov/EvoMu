//! Unit tests for `inventory_fitting` (`csprng_unit_f64`, `invbroker.GetItemDescriptor`, `invbroker.List`, `invbroker.ListByFlags`, `invbroker.Add` for cargo & module fitting, and fallback routing).

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[tokio::test]
async fn inventory_fitting_get_item_descriptor_list_and_add_module_and_cargo() {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    let u = csprng_unit_f64(&ctx);
    assert!((0.0..1.0).contains(&u));

    let char_id = 96_500_001i64;
    let station_id = 60_003_760i64;
    let ship_id = 96_500_100i64;
    let mod_id = 96_500_200i64;
    let cargo_id = 96_500_300i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 1;
        ch.name = "InvFitPilot".into();
        ch.corporation_id = 1_000_009;
        ch.station_id = Some(station_id);
        ch.solar_system_id = 30_000_142;
        ch.active_ship_id = ship_id;
        snap.characters.insert(char_id, ch);

        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: 484, // 125mm Gatling AutoCannon I
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            cargo_id,
            InventoryItemRecord {
                item_id: cargo_id,
                type_id: 34, // Tritanium
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        for &sk in &[3300u32, 3302, 3318, 3392, 3426] {
            snap.skills.entry(char_id).or_default().insert(
                sk,
                TrainedSkillRecord {
                    type_id: sk,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.corporation_id = Some(1_000_009);
    session.station_id = Some(station_id);
    session.solar_system_id = Some(30_000_142);
    session.ship_id = Some(ship_id);

    // 1. GetItemDescriptor
    let desc = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "GetItemDescriptor",
        &[],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("GetItemDescriptor ok");
    assert_ne!(desc, PyRep::None);

    // 2. List & ListByFlags
    let list_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "List",
        &[PyRep::Integer(InventoryFlag::Hangar as i64)],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("List ok");
    assert_ne!(list_res, PyRep::None);

    let list_flags_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "ListByFlags",
        &[PyRep::List(vec![PyRep::Integer(InventoryFlag::Hangar as i64)])],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("ListByFlags ok");
    assert_ne!(list_flags_res, PyRep::None);

    // 3. Add module to HiSlot0 (flag 27)
    let fit_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(mod_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::HiSlot0 as i64),
        ],
        &HashMap::new(),
    )
    .await
    .expect("handled")
    .expect("Add module ok");
    assert_ne!(fit_res, PyRep::None);
    let mod_loc = ctx
        .db
        .with_snapshot(|s| s.items.get(&mod_id).map(|i| (i.location_id, i.flag)));
    assert_eq!(mod_loc, Some((ship_id, InventoryFlag::HiSlot0)));

    // 4. Add split stack of Tritanium to ship Cargo (flag 5)
    let mut kwargs = HashMap::new();
    kwargs.insert("qty".into(), PyRep::Integer(200));
    let cargo_res = handle(
        &ctx,
        &mut session,
        None,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(cargo_id),
            PyRep::Integer(ship_id),
            PyRep::Integer(InventoryFlag::Cargo as i64),
        ],
        &kwargs,
    )
    .await
    .expect("handled")
    .expect("Add cargo split ok");
    assert_ne!(cargo_res, PyRep::None);
    let rem_qty = ctx
        .db
        .with_snapshot(|s| s.items.get(&cargo_id).map(|i| i.quantity));
    assert_eq!(rem_qty, Some(300));
}
