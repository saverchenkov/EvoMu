//! Integration tests for character attributes, neural remaps, nanite repair paste,
//! and strip fitting (`feat:attributes.*`, `feat:fitting.*`).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, NeuralAttributes, TrainedSkillRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const JITA_STATION: i64 = 60_003_760;

// Item & skill constants
const SKILL_SCIENCE: u32 = 3402;
const SKILL_BIOLOGY: u32 = 3405;
const SKILL_NANITE_OPERATION: u32 = 28879;
const SKILL_NANITE_INTERFACING: u32 = 28880;
const ACCELERATOR_TYPE_ID: u32 = 49539;
const NANITE_PASTE_TYPE_ID: u32 = 28668;
const LASER_TYPE_ID: u32 = 3033;
const MWD_TYPE_ID: u32 = 434;
const CHARGE_TYPE_ID: u32 = 185;
const DRONE_TYPE_ID: u32 = 2454;
const RIG_TYPE_ID: u32 = 31716;
const SUBSYSTEM_TYPE_ID: u32 = 45603;

async fn login_and_select(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

#[tokio::test]
async fn test_booster_duration_extended_by_biology_and_expiry() {
    // parity: feat:attributes.booster-duration-biology
    eve_testkit::attest(&["feat:attributes.booster-duration-biology"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 88_001, PILOT_1).await;
    let kw = HashMap::new();

    let accel_id = 994_001_001;
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT_1).or_default();
        sk.insert(SKILL_SCIENCE, TrainedSkillRecord { type_id: SKILL_SCIENCE, level: 1, skill_points: 250.0 });
        // Biology level 5 (+20% per level -> 100% bonus -> 2.0x duration)
        sk.insert(SKILL_BIOLOGY, TrainedSkillRecord { type_id: SKILL_BIOLOGY, level: 5, skill_points: 256_000.0 });

        snap.items.insert(
            accel_id,
            InventoryItemRecord {
                item_id: accel_id,
                type_id: ACCELERATOR_TYPE_ID,
                owner_id: PILOT_1,
                location_id: JITA_STATION,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let attrs_before = ctx.db.with_snapshot(|s| {
        eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, PILOT_1)
    });

    let use_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "UseBooster", &[PyRep::Integer(accel_id)], &kw, None)
        .await
        .expect("UseBooster with Biology V");

    let dur_secs = use_rep.dict_get("durationSeconds").and_then(|v| v.as_i64()).unwrap_or(0);
    let base_dur_ms = ctx
        .sde
        .types
        .get(&ACCELERATOR_TYPE_ID)
        .and_then(|t| t.default_attributes.get(&330).copied())
        .unwrap_or(1_800_000.0);
    let base_dur_secs = (base_dur_ms / 1000.0).round() as i64;

    // Biology V doubles duration (2.0x)
    assert_eq!(dur_secs, base_dur_secs * 2, "Biology V must double duration to 2.0x");

    // Attribute bonus is active
    let bonus_int = ctx
        .sde
        .types
        .get(&ACCELERATOR_TYPE_ID)
        .and_then(|t| t.default_attributes.get(&176).or_else(|| t.default_attributes.get(&165)).copied())
        .unwrap_or(6.0);
    let attrs_active = ctx.db.with_snapshot(|s| {
        eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, PILOT_1)
    });
    assert!(
        (attrs_active.1 - (attrs_before.1 + bonus_int)).abs() < 1e-4,
        "Active booster must apply attribute bonus during extended duration"
    );

    // Mid-duration (before extended expiry): booster remains active
    ctx.advance_secs((dur_secs as u64) - 60);
    let attrs_mid = ctx.db.with_snapshot(|s| {
        eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, PILOT_1)
    });
    assert!(
        (attrs_mid.1 - (attrs_before.1 + bonus_int)).abs() < 1e-4,
        "Booster bonus must remain active before extended duration expires"
    );

    // Advance past extended expiry: booster expires and attribute bonus ends
    ctx.advance_secs(70);
    let attrs_expired = ctx.db.with_snapshot(|s| {
        eve_server::services::character_skills::effective_character_attributes(&ctx.sde, s, PILOT_1)
    });
    assert!(
        (attrs_expired.1 - attrs_before.1).abs() < 1e-4,
        "Attribute bonus must end once extended duration has elapsed"
    );
}

#[tokio::test]
async fn test_neural_remap_defaults_constraints_and_consumption_order() {
    // parity: feat:attributes.remap-defaults-and-order
    eve_testkit::attest(&["feat:attributes.remap-defaults-and-order"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 88_002, PILOT_1).await;
    let kw = HashMap::new();

    // Reset to default starting neural attributes
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&PILOT_1).unwrap().neural = NeuralAttributes::default();
    });

    // 1. Verify default starting attributes: Cha 19, Int/Mem/Per/Wil 20 (99 total, 14 remappable), 2 bonus remaps
    let ch = ctx.db.get_character(PILOT_1).unwrap();
    assert_eq!(ch.neural.charisma, 19);
    assert_eq!(ch.neural.intelligence, 20);
    assert_eq!(ch.neural.memory, 20);
    assert_eq!(ch.neural.perception, 20);
    assert_eq!(ch.neural.willpower, 20);
    assert_eq!(ch.neural.bonus_remaps, 2);
    assert_eq!(ch.neural.last_remap_unix, None);
    assert_eq!(ch.neural.remap_cooldown_until_unix, None);

    let respec_info = ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetRespecInfo", &[], &kw, None)
        .await
        .expect("GetRespecInfo");
    assert_eq!(respec_info.dict_get("bonusRemaps"), Some(&PyRep::Integer(2)));

    // 2. Constraints: each attribute must stay within 17..=27 and sum to 99
    // < 17 rejected
    assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(16), PyRep::Integer(21), PyRep::Integer(21), PyRep::Integer(21), PyRep::Integer(20)], &kw, None).await.is_err());
    // > 27 rejected
    assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(19), PyRep::Integer(28), PyRep::Integer(18), PyRep::Integer(17), PyRep::Integer(17)], &kw, None).await.is_err());
    // sum != 99 rejected
    assert!(ctx.dispatch_rpc(&mut sess, "charMgr", None, "RespecCharacter", &[PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20), PyRep::Integer(20)], &kw, None).await.is_err());

    // 3. Normal timed remap is consumed BEFORE bonus remaps
    let remap1 = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[PyRep::Integer(17), PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17)],
            &kw,
            None,
        )
        .await
        .expect("First neural remap (normal timed remap)");
    assert_eq!(
        remap1.dict_get("bonusRemaps"),
        Some(&PyRep::Integer(2)),
        "Normal timed remap must be consumed first without using bonus remaps"
    );
    assert_eq!(remap1.dict_get("intelligence"), Some(&PyRep::Integer(27)));

    // 4. Subsequent remaps while on cooldown consume bonus remaps in order
    let remap2 = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[PyRep::Integer(17), PyRep::Integer(26), PyRep::Integer(22), PyRep::Integer(17), PyRep::Integer(17)],
            &kw,
            None,
        )
        .await
        .expect("Second remap (consumes first bonus remap)");
    assert_eq!(remap2.dict_get("bonusRemaps"), Some(&PyRep::Integer(1)));

    let remap3 = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[PyRep::Integer(17), PyRep::Integer(25), PyRep::Integer(23), PyRep::Integer(17), PyRep::Integer(17)],
            &kw,
            None,
        )
        .await
        .expect("Third remap (consumes second bonus remap)");
    assert_eq!(remap3.dict_get("bonusRemaps"), Some(&PyRep::Integer(0)));

    // Fourth remap on cooldown with 0 bonus remaps is refused
    assert!(
        ctx.dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[PyRep::Integer(17), PyRep::Integer(24), PyRep::Integer(24), PyRep::Integer(17), PyRep::Integer(17)],
            &kw,
            None,
        )
        .await
        .is_err(),
        "Remap on cooldown with 0 bonus remaps must fail"
    );

    // 5. Normal remap replenishes after 1 year (365 days)
    ctx.advance_secs(365 * 86_400 + 10);
    let remap4 = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "RespecCharacter",
            &[PyRep::Integer(17), PyRep::Integer(27), PyRep::Integer(21), PyRep::Integer(17), PyRep::Integer(17)],
            &kw,
            None,
        )
        .await
        .expect("Normal remap replenishes after one year");
    assert_eq!(remap4.dict_get("intelligence"), Some(&PyRep::Integer(27)));
}

#[tokio::test]
async fn test_nanite_repair_paste_consumption_skill_scaling_and_burnout() {
    // parity: feat:fitting.nanite-repair-paste-consumption
    eve_testkit::attest(&["feat:fitting.nanite-repair-paste-consumption"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 88_003, PILOT_1).await;
    let kw = HashMap::new();

    // Undock into space
    ctx.dispatch_rpc(&mut sess, "ship", None, "Undock", &[], &kw, None).await.expect("Undock");
    let ship_id = sess.ship_id.expect("active ship in space");
    let sys_id = sess.solar_system_id.expect("solar system");
    let sys = ctx.systems.get_or_spawn(sys_id);

    let laser_1 = 994_002_001;
    let laser_2 = 994_002_002;
    let laser_burnt = 994_002_003;
    let paste_id = 994_002_004;

    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(PILOT_1).or_default();
        // Nanite Operation IV (reduces paste cost by 20%)
        sk.insert(SKILL_NANITE_OPERATION, TrainedSkillRecord { type_id: SKILL_NANITE_OPERATION, level: 4, skill_points: 135_765.0 });
        sk.insert(SKILL_NANITE_INTERFACING, TrainedSkillRecord { type_id: SKILL_NANITE_INTERFACING, level: 3, skill_points: 40_000.0 });

        // Damaged module 1 (15.0 HP damage, offline)
        snap.items.insert(
            laser_1,
            InventoryItemRecord {
                item_id: laser_1,
                type_id: LASER_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(3, 15.0), (9, 40.0)]),
                ..Default::default()
            },
        );

        // Damaged module 2 for StopRepairModule (10.0 HP damage)
        snap.items.insert(
            laser_2,
            InventoryItemRecord {
                item_id: laser_2,
                type_id: LASER_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(3, 10.0), (9, 40.0)]),
                ..Default::default()
            },
        );

        // Completely burnt-out module (damage == max HP == 40.0)
        snap.items.insert(
            laser_burnt,
            InventoryItemRecord {
                item_id: laser_burnt,
                type_id: LASER_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot2,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(3, 40.0), (9, 40.0)]),
                ..Default::default()
            },
        );

        // Nanite Repair Paste in ship cargo
        snap.items.insert(
            paste_id,
            InventoryItemRecord {
                item_id: paste_id,
                type_id: NANITE_PASTE_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let paste_before = ctx.db.with_snapshot(|s| s.items[&paste_id].quantity);

    // 1. RepairModule consumes Nanite Repair Paste from cargo hold
    let rep_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "RepairModule",
            &[PyRep::Integer(ship_id), PyRep::Integer(laser_1)],
            &kw,
            None,
        )
        .await
        .expect("RepairModule on damaged laser 1");
    let paste_consumed = rep_res.dict_get("pasteConsumed").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(paste_consumed >= 1, "Must consume at least 1 unit of paste");

    let paste_after = ctx.db.with_snapshot(|s| s.items[&paste_id].quantity);
    assert_eq!(
        paste_after,
        paste_before - (paste_consumed as i32),
        "Nanite Repair Paste must be deducted from cargo"
    );

    // Step physics to finish repair
    for _ in 0..12 {
        sys.step_manual(1.0).await.unwrap();
    }
    let dmg_after = ctx.db.with_snapshot(|s| {
        s.items[&laser_1].dynamic_attributes.get(&3).copied().unwrap_or(0.0)
    });
    assert_eq!(dmg_after, 0.0, "Completed repair must restore module damage to 0.0");

    // 2. StopRepairModule stops in-progress repair
    ctx.dispatch_rpc(
        &mut sess,
        "dogmaIM",
        None,
        "RepairModule",
        &[PyRep::Integer(ship_id), PyRep::Integer(laser_2)],
        &kw,
        None,
    )
    .await
    .expect("Start repair on laser 2");

    let stop_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "StopRepairModule",
            &[PyRep::Integer(ship_id), PyRep::Integer(laser_2)],
            &kw,
            None,
        )
        .await
        .expect("StopRepairModule on laser 2");
    assert_eq!(stop_rep.dict_get("stopped"), Some(&PyRep::Bool(true)));

    // 3. Completely burnt-out module must be refused with ModuleBurnedOut
    let burn_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "RepairModule",
            &[PyRep::Integer(ship_id), PyRep::Integer(laser_burnt)],
            &kw,
            None,
        )
        .await
        .expect_err("Repair on completely burnt-out module must fail");
    assert_eq!(burn_err.code, "ModuleBurnedOut");
}

#[tokio::test]
async fn test_strip_fitting_docked_modules_charges_drones_and_rig_exclusion() {
    // parity: feat:fitting.strip-fitting
    eve_testkit::attest(&["feat:fitting.strip-fitting"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut sess = login_and_select(&ctx, 88_004, PILOT_1).await;
    let kw = HashMap::new();
    let ship_id = sess.ship_id.expect("docked active ship");

    let mod_id = 994_003_001;
    let charge_id = 994_003_002;
    let drone_id = 994_003_003;
    let rig_id = 994_003_004;
    let subsys_id = 994_003_005;

    ctx.db.with_mut_snapshot(|snap| {
        // Fitted module in MedSlot0
        snap.items.insert(
            mod_id,
            InventoryItemRecord {
                item_id: mod_id,
                type_id: MWD_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Loaded charge in HiSlot0
        snap.items.insert(
            charge_id,
            InventoryItemRecord {
                item_id: charge_id,
                type_id: CHARGE_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Drone in DroneBay
        snap.items.insert(
            drone_id,
            InventoryItemRecord {
                item_id: drone_id,
                type_id: DRONE_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::DroneBay,
                quantity: 2,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        // Rig in RigSlot0 (must stay fitted!)
        snap.items.insert(
            rig_id,
            InventoryItemRecord {
                item_id: rig_id,
                type_id: RIG_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::RigSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // T3 Subsystem in SubSystemSlot0 (must stay fitted!)
        snap.items.insert(
            subsys_id,
            InventoryItemRecord {
                item_id: subsys_id,
                type_id: SUBSYSTEM_TYPE_ID,
                owner_id: PILOT_1,
                location_id: ship_id,
                flag: InventoryFlag::SubSystemSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let _ = ctx.drain_notifications_for_char(PILOT_1);

    // Call StripFitting while docked
    let strip_rep = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "StripFitting", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect("StripFitting docked");
    assert_eq!(strip_rep.dict_get("shipID"), Some(&PyRep::Integer(ship_id)));

    // Verify fitted module, charge, and drone moved to hangar
    ctx.db.with_snapshot(|s| {
        let m = s.items.get(&mod_id).expect("module in items");
        assert_eq!(m.flag, InventoryFlag::Hangar);
        assert_eq!(m.location_id, JITA_STATION);

        let c = s.items.get(&charge_id).expect("charge in items");
        assert_eq!(c.flag, InventoryFlag::Hangar);
        assert_eq!(c.location_id, JITA_STATION);

        let d = s.items.get(&drone_id).expect("drone in items");
        assert_eq!(d.flag, InventoryFlag::Hangar);
        assert_eq!(d.location_id, JITA_STATION);

        // Rigs and T3 Subsystems remain fitted!
        let r = s.items.get(&rig_id).expect("rig in items");
        assert_eq!(r.flag, InventoryFlag::RigSlot0, "Rigs must remain fitted");
        assert_eq!(r.location_id, ship_id);

        let sub = s.items.get(&subsys_id).expect("subsystem in items");
        assert_eq!(sub.flag, InventoryFlag::SubSystemSlot0, "T3 subsystems must remain fitted");
        assert_eq!(sub.location_id, ship_id);
    });

    // Verify OnItemsChanged and OnModuleUnfitted notifications pushed
    let notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert!(
        notifs.iter().any(|n| n.event == "OnItemsChanged"),
        "Must push OnItemsChanged batch notification"
    );
    assert!(
        notifs.iter().any(|n| n.event == "OnModuleUnfitted"),
        "Must push OnModuleUnfitted notifications"
    );

    // In space: StripFitting must be refused
    ctx.dispatch_rpc(&mut sess, "ship", None, "Undock", &[], &kw, None).await.expect("Undock");
    let space_err = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "StripFitting", &[PyRep::Integer(ship_id)], &kw, None)
        .await
        .expect_err("StripFitting in space must be refused");
    assert_eq!(space_err.code, "CannotStripFittingInSpace");
}
