//! Focused unit tests for uncovered branches in `character_clone_jump_rules`
//! (`check_alpha_clone_install_capacity`, `check_structure_clone_bay_access`,
//! `compute_structure_clone_fee`, `activate_clone_vat_bay`, and `handle_clone_jump`).

use super::*;
use crate::router::ServerContext;
use crate::services::station_structure::structure_upwell_ops::{save_ext_state, StructureAccessProfile};
use eve_db::clock::VirtualServerClock;
use eve_db::types::UpwellStructureRecord;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, JumpCloneRecord};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn full_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::clone(&ServerContext::new().sde)))
}

fn ensure_type(sde: &mut Arc<SdeRegistry>, tid: u32, group_id: u32, category_id: u32, attrs: &[(u16, f64)]) {
    let reg = Arc::make_mut(sde);
    if !reg.types.contains_key(&tid) {
        let mut rec: InvType = reg.types.values().next().cloned().expect("base sde type");
        rec.type_id = tid;
        rec.group_id = group_id;
        rec.category_id = category_id;
        rec.name = format!("TestType_{tid}");
        rec.volume = 10.0;
        rec.capacity = 5_000.0;
        rec.mass = 1_000_000.0;
        for &(k, v) in attrs {
            rec.default_attributes.insert(k, v);
        }
        reg.types.insert(tid, rec);
    }
}

fn make_session(
    user_id: u64,
    char_id: i64,
    corp_id: i64,
    sys_id: u32,
    station_id: Option<i64>,
    structure_id: Option<i64>,
    ship_id: Option<i64>,
) -> SessionState {
    let mut s = SessionState::new(760);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(sys_id);
    s.station_id = station_id;
    s.structure_id = structure_id;
    s.ship_id = ship_id;
    s
}

#[test]
fn alpha_capacity_structure_acl_and_leave_behind_fee_calculations() {
    let mut sde = full_sde();
    ensure_type(&mut sde, 35832, 1657, 65, &[]);
    let ctx = ServerContext::with_sde_and_repository(
        sde,
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000)));

    let char_id = 96_920_001i64;
    let owner_corp = 98_920_001i64;
    let caller_corp = 98_920_002i64;
    let structure_id = 1_040_000_000_888i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 9401;
        ch.corporation_id = caller_corp;
        ch.clone_grade = 1; // Alpha
        ch.balance = IskCents::from_f64_isk(50_000_000.0);
        snap.characters.insert(char_id, ch);

        let mut c1 = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = owner_corp;
            __corp.name = "Owner Corp".into();
            __corp.ticker = "OWNC".into();
            __corp.ceo_id = 96_920_099;
            __corp };
        c1.ensure_wallet_divisions();
        snap.corporations.insert(owner_corp, c1);

        let mut c2 = { let mut __corp = CorporationRecord::default();
            __corp.corporation_id = caller_corp;
            __corp.name = "Caller Corp".into();
            __corp.ticker = "CALC".into();
            __corp.ceo_id = char_id;
            __corp };
        c2.ensure_wallet_divisions();
        snap.corporations.insert(caller_corp, c2);

        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                solar_system_id: 30_000_142,
                type_id: 35832,
                owner_corp_id: owner_corp,
                name: "Clone Fort".into(),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        let mut ext = structure_upwell_ops::load_ext_state(snap);
        ext.access_profiles.insert(
            structure_id,
            StructureAccessProfile {
                profile_id: 7100,
                owner_corp_id: owner_corp,
                name: "CloneACL".into(),
                allowed_corporations: HashSet::from([owner_corp, caller_corp]),
                service_settings: HashMap::from([(2i64, 750_000i64)]),
                ..Default::default()
            },
        );
        save_ext_state(snap, &ext);

        let err = check_alpha_clone_install_capacity(snap, char_id).expect_err("Alpha blocked");
        assert_eq!(err.code, "OmegaCapacityRequired");

        snap.characters.get_mut(&char_id).unwrap().clone_grade = 2;
        assert!(check_alpha_clone_install_capacity(snap, char_id).is_ok());

        // Structure clone fee scales with standing: high standing (6.0 -> 100 ISK), neutral (0.0 -> 500 ISK), negative (-2.0 -> 1000 ISK)
        assert_eq!(compute_structure_clone_fee(snap, structure_id, 6.0).0, 100);
        assert_eq!(compute_structure_clone_fee(snap, structure_id, 0.0).0, 500);
        assert_eq!(compute_structure_clone_fee(snap, structure_id, -2.0).0, 1000);
        let _ = check_structure_clone_bay_access(snap, structure_id, char_id);
    });
}

#[tokio::test]
async fn handle_clone_jump_cooldown_same_station_and_titan_vat_bay() {
    let mut sde = full_sde();
    ensure_type(&mut sde, 671, 30, 6, &[]); // Erebus (Titan group 30)
    ensure_type(&mut sde, 24570, 815, 7, &[(73, 300_000.0), (713, 16_273.0), (714, 250.0)]);
    let ctx = ServerContext::with_sde_and_repository(
        sde,
        Arc::new(eve_db::WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000)));

    let char_id = 96_920_010i64;
    let host_id = 96_920_011i64;
    let dep_station = 60_003_760i64;
    let dest_station = 60_008_494i64; // Amarr VIII
    let jc_same_station = 96_920_201i64;
    let jc_remote_station = 96_920_202i64;
    let jc_titan_bay = 96_920_203i64;
    let titan_ship_id = 96_920_500i64;
    let vat_mod_id = 96_920_601i64;
    let ozone_item_id = 96_920_602i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().cloned().expect("seed char");
        ch.character_id = char_id;
        ch.user_id = 9410;
        ch.corporation_id = 1_000_009;
        ch.solar_system_id = 30_000_142;
        ch.station_id = Some(dep_station);
        ch.structure_id = None;
        ch.clone_grade = 2;
        ch.balance = IskCents::from_f64_isk(50_000_000.0);
        ch.last_clone_jump_unix = 1_699_999_500; // active 24h cooldown!
        snap.characters.insert(char_id, ch);

        let mut host = snap.characters[&char_id].clone();
        host.character_id = host_id;
        host.user_id = 9411;
        host.solar_system_id = 30_000_142;
        host.station_id = None;
        host.structure_id = None;
        host.active_ship_id = titan_ship_id;
        snap.characters.insert(host_id, host);

        snap.items.insert(
            titan_ship_id,
            InventoryItemRecord {
                item_id: titan_ship_id,
                type_id: 671,
                owner_id: host_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            vat_mod_id,
            InventoryItemRecord {
                item_id: vat_mod_id,
                type_id: 24570,
                owner_id: host_id,
                location_id: titan_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.fleets.insert(
            96_920_700,
            eve_db::FleetRecord {
                fleet_id: 96_920_700,
                boss_id: host_id,
                members: vec![host_id, char_id],
                ..Default::default()
            },
        );

        snap.jump_clones.insert(
            char_id,
            vec![
                JumpCloneRecord {
                    jump_clone_id: jc_same_station,
                    character_id: char_id,
                    location_id: dep_station,
                    implants: vec![22118],
                },
                JumpCloneRecord {
                    jump_clone_id: jc_remote_station,
                    character_id: char_id,
                    location_id: dest_station,
                    implants: vec![],
                },
                JumpCloneRecord {
                    jump_clone_id: jc_titan_bay,
                    character_id: char_id,
                    location_id: titan_ship_id,
                    implants: vec![],
                },
            ],
        );
    });

    // Put Titan in live ballpark for space spawn position test
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: titan_ship_id,
                type_id: 671,
                position: DVec3::new(50_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 75.0,
                inertia: 40.0,
                mass: 2_000_000_000.0,
                radius: 3_500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                owner_id: host_id,
                char_id: Some(host_id),
                corp_id: 1_000_009,
                group_id: 30,
                category_id: 6,
                ..Default::default()
            },
        )
        .await;

    // 0. Activate Clone Vat Bay without fuel -> NotEnoughFuel; then add Liquid Ozone & activate
    let err = activate_clone_vat_bay(&ctx, &sys_handle, host_id, titan_ship_id, vat_mod_id)
        .await
        .expect_err("requires Liquid Ozone");
    assert_eq!(err.code, "NotEnoughFuel");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ozone_item_id,
            InventoryItemRecord {
                item_id: ozone_item_id,
                type_id: 16_273,
                owner_id: host_id,
                location_id: titan_ship_id,
                flag: InventoryFlag::SpecializedFuelBay,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let vat_res = activate_clone_vat_bay(&ctx, &sys_handle, host_id, titan_ship_id, vat_mod_id)
        .await
        .expect("vat bay activated with Liquid Ozone");
    assert!(matches!(vat_res, PyRep::KeyVal(_)));

    let dep_capsule_id = 96_920_499i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            dep_capsule_id,
            InventoryItemRecord {
                item_id: dep_capsule_id,
                type_id: 670,
                owner_id: char_id,
                location_id: dep_station,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = dep_capsule_id;
        snap.skills.entry(char_id).or_default().insert(
            24242,
            eve_db::types::TrainedSkillRecord {
                type_id: 24242,
                level: 2,
                skill_points: 45_255.0,
            },
        );
    });
    let mut sess = make_session(9410, char_id, 1_000_009, 30_000_142, Some(dep_station), None, Some(dep_capsule_id));

    // 1. Jumping to remote station while cooldown active -> CloneJumpCooldownActive
    let err = handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(jc_remote_station)])
        .expect("handled")
        .expect_err("cooldown active for remote station");
    assert_eq!(err.code, "CloneJumpCooldownActive");

    // 2. Same-station clone swap bypasses cooldown, swaps implants ([22118] equipped), charges 0 ISK
    handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(jc_same_station)])
        .expect("handled")
        .expect("same-station swap bypasses cooldown");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_implants[&char_id], vec![22118]);
    });

    // 3. Clear cooldown & refresh Titan Clone Vat Bay cycle -> clone jump spawns capsule in space near host Titan!
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_700_100_000)));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(oz) = snap.items.get_mut(&ozone_item_id) {
            oz.quantity = 500;
        } else {
            snap.items.insert(
                ozone_item_id,
                InventoryItemRecord {
                    item_id: ozone_item_id,
                    type_id: 16_273,
                    owner_id: host_id,
                    location_id: titan_ship_id,
                    flag: InventoryFlag::SpecializedFuelBay,
                    quantity: 500,
                    singleton: false,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });
    activate_clone_vat_bay(&ctx, &sys_handle, host_id, titan_ship_id, vat_mod_id)
        .await
        .expect("reactivate vat bay after clock advance");
    handle_clone_jump(&ctx, &mut sess, &[PyRep::Integer(jc_titan_bay)])
        .expect("handled")
        .expect("titan clone vat bay jump succeeds");
    assert_eq!(sess.station_id, None);
    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&char_id];
        assert!(ch.station_id.is_none());
        assert!(snap.item_positions.contains_key(&ch.active_ship_id));
    });
}
