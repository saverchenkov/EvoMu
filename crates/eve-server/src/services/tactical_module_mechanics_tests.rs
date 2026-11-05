#![forbid(unsafe_code)]

use super::*;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_sde::{InvType, SdeRegistry};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn custom_inv_type(
    sde: &SdeRegistry,
    type_id: u32,
    group_id: u32,
    category_id: u32,
    capacity: f64,
    name: &str,
    attrs: &[(u16, f64)],
) -> InvType {
    let mut t = sde.types.get(&587).expect("seed type").clone();
    t.type_id = type_id;
    t.group_id = group_id;
    t.category_id = category_id;
    t.capacity = capacity;
    t.name = name.into();
    t.default_attributes.clear();
    for &(k, v) in attrs {
        t.default_attributes.insert(k, v);
    }
    t
}

fn test_ctx_with_custom<F: FnOnce(&mut SdeRegistry)>(mutate_sde: F) -> ServerContext {
    let mut sde = (*shared_sde()).clone();
    mutate_sde(&mut sde);
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(Arc::new(sde), db)
}

#[test]
fn validate_module_fitting_constraints_grappler_and_max_group() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            999_801,
            custom_inv_type(sde, 999_801, 1672, 7, 0.0, "Heavy Stasis Grappler II", &[]),
        );
    });

    let char_id = 99_000_001;
    let frigate_id = 1_000_900_001;
    let bs_id = 1_000_900_002;
    let grappler_mod = ctx.sde.types.get(&999_801).unwrap().clone();

    let frigate_type = custom_inv_type(&ctx.sde, 587, 25, 6, 100.0, "Rifter", &[]);
    let bs_type = custom_inv_type(&ctx.sde, 641, 27, 6, 500.0, "Megathron", &[]);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bs_id,
            InventoryItemRecord {
                item_id: bs_id,
                type_id: 641,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_000_900_010,
            InventoryItemRecord {
                item_id: 1_000_900_010,
                type_id: 999_801,
                owner_id: char_id,
                location_id: bs_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        // Frigate cannot fit Heavy Stasis Grappler
        let err_hull = validate_module_fitting_constraints(&ctx, snap, frigate_id, 1_000_900_011, &grappler_mod, &frigate_type)
            .unwrap_err();
        assert_eq!(err_hull.code, "CanNotFitModuleToShip");

        // Second Heavy Stasis Grappler on Battleship hits MaxGroupFitted (1)
        let err_max = validate_module_fitting_constraints(&ctx, snap, bs_id, 1_000_900_012, &grappler_mod, &bs_type)
            .unwrap_err();
        assert_eq!(err_max.code, "MaxGroupFitted");
    });
}

#[test]
fn nanite_repair_paste_deduct_free_cargo_and_capped_refund() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            999_810,
            custom_inv_type(sde, 999_810, 25, 6, 5.0, "Small Cargo Test Hull", &[]),
        );
    });

    let char_id = 99_000_010;
    let ship_id = 1_000_900_101;
    let paste_id = 1_000_900_102;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("demo seed character");
        ch.character_id = char_id;
        ch.user_id = char_id as u64;
        ch.name = "Repair Pilot".into();
        snap.characters.insert(char_id, ch);
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 999_810,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: 28668, // Nanite Repair Paste (0.01 m3)
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 120,
                singleton: false,
                ..Default::default()
            },
        );

        let taken = deduct_paste_from_cargo_in_snap(snap, char_id, ship_id, 50);
        assert_eq!(taken, 50);
        assert_eq!(snap.items.get(&paste_id).unwrap().quantity, 70);

        let free_m3 = ship_free_cargo_m3_in_snap(snap, &ctx.sde, ship_id);
        assert!((free_m3 - 4.30).abs() < 1e-4);

        // Refunding 1,000 units (10.0 m3) caps cleanly at 430 units so cargo volume never exceeds 5.0 m3
        let refunded = refund_paste_to_cargo_capped_in_snap(snap, &ctx.sde, char_id, ship_id, 1_000);
        assert_eq!(refunded, 430);
        assert_eq!(snap.items.get(&paste_id).unwrap().quantity, 500);
    });
}

#[test]
fn thermodynamics_rack_heat_stepping_propagation_and_module_burnout() {
    let ctx = test_ctx_with_custom(|sde| {
        sde.types.insert(
            999_820,
            custom_inv_type(
                sde,
                999_820,
                55,
                7,
                0.0,
                "Hot Overloaded Autocannon",
                &[(1180, 0.25), (1211, 20.0), (40, 40.0)],
            ),
        );
    });

    let char_id = 99_000_020;
    let ship_id = 1_000_900_201;
    let mod1_id = 1_000_900_202;
    let mod2_id = 1_000_900_203;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: 30_002_505,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        for (mid, flag, dmg) in [(mod1_id, InventoryFlag::HiSlot0, 39.0), (mod2_id, InventoryFlag::HiSlot1, 39.0)] {
            let mut rec = InventoryItemRecord {
                item_id: mid,
                type_id: 999_820,
                owner_id: char_id,
                location_id: ship_id,
                flag,
                quantity: 1,
                singleton: true,
                ..Default::default()
            };
            rec.dynamic_attributes.insert(40, 40.0);
            rec.dynamic_attributes.insert(3, dmg); // already at 39/40 damage
            snap.items.insert(mid, rec);
        }
    });

    let db_opt = Some(Arc::clone(&ctx.db));
    let sde_opt = Some(Arc::clone(&ctx.sde));
    let eval = evaluate_ship_thermodynamics(ship_id, &sde_opt, &db_opt);
    assert!(eval.heat_gen_mult > 0.0);

    let mut cyc = ActiveModuleCycle {
        module_id: mod1_id,
        ship_id,
        owner_char_id: Some(char_id),
        module_type_id: 999_820,
        is_overloaded: true,
        rack_idx: 0,
        heat_damage_per_cycle: 20.0,
        duration_s: 5.0,
        ..Default::default()
    };

    let mut ship_rack_heat = HashMap::new();
    for _ in 0..10 {
        step_rack_heat_per_tick(&mut ship_rack_heat, &[cyc.clone()], &sde_opt, &db_opt, 1.0);
    }
    let hot_val = ship_rack_heat.get(&ship_id).map(|r| r[0]).unwrap_or(0.0);
    assert!(hot_val > 0.25);

    // Apply overheat damage with guaranteed-hit seeded RNG -> burns out high-slot modules!
    let rng = Some(Arc::new(crate::rng::MechanicRng::seeded(7)));
    let mut burned_out = false;
    let mut burned_others = Vec::new();
    for _ in 0..5 {
        apply_overheat_rack_and_damage_on_cycle(
            &cyc,
            &mut ship_rack_heat,
            &sde_opt,
            &db_opt,
            &rng,
            &mut burned_out,
            &mut burned_others,
        );
    }
    assert!(burned_out || !burned_others.is_empty());

    // Stop overloading -> rack heat dissipates on idle ticks
    cyc.is_overloaded = false;
    for _ in 0..15 {
        step_rack_heat_per_tick(&mut ship_rack_heat, &[], &sde_opt, &db_opt, 2.0);
    }
    let cooled_val = ship_rack_heat.get(&ship_id).map(|r| r[0]).unwrap_or(0.0);
    assert!(cooled_val < hot_val);
}

#[tokio::test]
async fn interdiction_probe_untargetable_check_and_ballpark_lifetime_despawn() {
    let ctx = test_ctx_with_custom(|_| {});
    let sys_id = 30_002_505;
    let probe_id = 1_000_900_301;
    let probe_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 548)
        .map(|t| t.type_id)
        .unwrap_or(22778);

    let sys = ctx.systems.get_or_spawn(sys_id);
    let pball = Ball {
        id: probe_id,
        type_id: probe_tid,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let meta = SlimItemMeta {
        group_id: 548,
        category_id: 8,
        owner_id: 99_000_030,
        char_id: Some(99_000_030),
        corp_id: 98_000_001,
        alliance_id: 0,
        security_status: 0.0,
        is_cloaked: false,
        ..Default::default()
    };
    sys.spawn_ball_with_meta(pball, CombatState::default(), meta)
        .await
        .expect("spawn probe");

    if ctx.sde.types.get(&probe_tid).is_some_and(|t| matches!(t.group_id, 548 | 4088)) {
        assert!(is_untargetable_interdiction_probe(&ctx, &sys, probe_id).await);
    }

    // Track probe lifetime in Ballpark and verify expiration despawns ball & bubble
    let db_opt = Some(Arc::clone(&ctx.db));
    let db_key = tactical_modules::db_ptr_key(&ctx.db);
    ctx.tactical_state.write().unwrap().active_interdiction_probes.insert(
        (db_key, probe_id),
        ActiveInterdictionProbe {
            probe_id,
            probe_type_id: probe_tid,
            solar_system_id: sys_id,
            remaining_s: 10.0,
            is_wubble: true,
        },
    );
    let mut bp = Ballpark::new(sys_id);
    bp.balls.insert(
        probe_id,
        Ball {
            id: probe_id,
            type_id: probe_tid,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    bp.add_warp_bubble(probe_id, DVec3::ZERO, 20_000.0);
    step_interdiction_probes_lifetime(&mut bp, &db_opt, &ctx.tactical_state, 5.0);
    assert!(bp.balls.contains_key(&probe_id));
    step_interdiction_probes_lifetime(&mut bp, &db_opt, &ctx.tactical_state, 6.0);
    assert!(!bp.balls.contains_key(&probe_id));
}
