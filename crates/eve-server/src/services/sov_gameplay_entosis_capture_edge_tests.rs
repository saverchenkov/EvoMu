//! Comprehensive edge, error, and happy-path unit tests for `sov_gameplay_entosis_capture`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
use crate::services::tactical_modules::{db_ptr_key, ActiveEntosisLinkState};
use eve_db::durable_domain_types::PersistedHubUpgrade;
use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;

async fn spawn_pos_ball(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3) {
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball(Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("spawn ball");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn register_entosis_target_guards_and_existing_progress_preservation() {
    let ctx = test_ctx();
    let hub_id = 1_085_000_001_i64;
    let sys_id = 30_000_142_u32;
    let no_ally_char = 90_500_001_i64;
    let corp_ally_char = 90_500_002_i64;
    let def_char = 90_500_003_i64;
    let att_ally = 99_500_001_i64;
    let def_ally = 99_500_002_i64;
    let corp_id = 98_500_001_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut base = snap.characters.values().next().unwrap().clone();
        base.character_id = no_ally_char;
        base.alliance_id = None;
        base.corporation_id = 98_500_099;
        snap.characters.insert(no_ally_char, base.clone());

        base.character_id = corp_ally_char;
        base.alliance_id = None;
        base.corporation_id = corp_id;
        snap.characters.insert(corp_ally_char, base.clone());

        base.character_id = def_char;
        base.alliance_id = Some(def_ally);
        base.corporation_id = 98_500_002;
        snap.characters.insert(def_char, base);

        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.name = "Alliance Corp".into(); __corp.ticker = "ACRP".into(); __corp.alliance_id = Some(att_ally); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(def_ally);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_500_002,
                solar_system_id: sys_id,
                name: "Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });

    // 1. Unknown char -> no-op
    register_entosis_capture_target(&ctx, hub_id, 100, 999_999_999, now + 300);
    // 2. Char with no alliance -> no-op
    register_entosis_capture_target(&ctx, hub_id, 100, no_ally_char, now + 300);
    // 3. Defender alliance == holder -> no-op
    register_entosis_capture_target(&ctx, hub_id, 100, def_char, now + 300);
    ctx.db.with_snapshot(|s| {
        assert!(!s.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });

    // 4. Corp-derived alliance registers target
    register_entosis_capture_target(&ctx, hub_id, 501, corp_ally_char, now + 300);
    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.sov_runtime.entosis_capture_progress.get(&hub_id).copied(),
            Some((corp_ally_char, att_ally, 501, 0.0, now + 300))
        );
    });

    // 5. Existing entry with accrued == 0 and ship_id == 0 gets overwritten;
    //    Existing entry with accrued > 0 and opposing alliance preserves controlling side;
    //    Existing entry with accrued > 0 and same alliance + ship_id == 0 takes over ship & max(warmup)
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (corp_ally_char, att_ally, 0, 0.0, now + 100));
    });
    register_entosis_capture_target(&ctx, hub_id, 502, corp_ally_char, now + 200);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.sov_runtime.entosis_capture_progress[&hub_id].2, 502);
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (def_char, def_ally, 0, 120.0, now + 400));
    });
    // Opposing alliance does NOT overwrite partial progress controlling side
    register_entosis_capture_target(&ctx, hub_id, 503, corp_ally_char, now + 500);
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.sov_runtime.entosis_capture_progress[&hub_id].1, def_ally);
        assert_eq!(s.sov_runtime.entosis_capture_progress[&hub_id].2, 0);
    });

    // Same alliance with ship_id == 0 takes over ship and takes max warmup timestamp
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (corp_ally_char, att_ally, 0, 120.0, now + 400));
    });
    register_entosis_capture_target(&ctx, hub_id, 504, corp_ally_char, now + 500);
    ctx.db.with_snapshot(|s| {
        assert_eq!(
            s.sov_runtime.entosis_capture_progress[&hub_id],
            (corp_ally_char, att_ally, 504, 120.0, now + 500)
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tick_entosis_decapture_defender_counter_range_stront_and_defender_win() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let hub_id = 1_085_000_100_i64;
    let destroyed_hub_id = 1_085_000_101_i64;
    let node_id = 1_085_000_102_i64;
    let att1_char = 90_500_010_i64;
    let att2_char = 90_500_011_i64;
    let def_char = 90_500_012_i64;
    let att1_ally = 99_500_010_i64;
    let att2_ally = 99_500_011_i64;
    let def_ally = 99_500_012_i64;
    let att1_ship = 1_085_000_201_i64;
    let att2_ship = 1_085_000_202_i64;
    let def_ship = 1_085_000_203_i64;
    let now = ctx.now_unix();

    // Empty tick is a safe no-op
    tick_entosis_captures(&ctx, now + 10).await;

    ctx.db.with_mut_snapshot(|snap| {
        let mut base = snap.characters.values().next().unwrap().clone();
        for (cid, ally) in [(att1_char, att1_ally), (att2_char, att2_ally), (def_char, def_ally)] {
            base.character_id = cid;
            base.alliance_id = Some(ally);
            base.corporation_id = 98_500_010 + (cid % 10);
            snap.characters.insert(cid, base.clone());
        }
        snap.system_states.entry(sys_id).or_default().sov_alliance_id = Some(def_ally);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_500_012,
                solar_system_id: sys_id,
                name: "Live Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            destroyed_hub_id,
            UpwellStructureRecord {
                structure_id: destroyed_hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_500_012,
                solar_system_id: sys_id,
                name: "Destroyed Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
        let hub = snap.sov_runtime.sov_hubs.entry(hub_id).or_default();
        hub.hub_id = hub_id;
        hub.upgrades.push(PersistedHubUpgrade {
            type_id: UPGRADE_CYNOSURAL_NAVIGATION,
            online: true,
            low_power: false,
        });

        // Orphan/destroyed target gets pruned
        snap.sov_runtime
            .entosis_capture_progress
            .insert(destroyed_hub_id, (att1_char, att1_ally, 0, 50.0, now));
        // Hub starts with 100s progress held by att1_ally
        snap.sov_runtime
            .entosis_capture_progress
            .insert(hub_id, (att1_char, att1_ally, att1_ship, 100.0, now));
    });

    spawn_pos_ball(&ctx, sys_id, hub_id, TYPE_SOVEREIGNTY_HUB, DVec3::ZERO).await;
    spawn_pos_ball(&ctx, sys_id, att1_ship, 587, DVec3::new(10_000.0, 0.0, 0.0)).await;
    spawn_pos_ball(&ctx, sys_id, att2_ship, 587, DVec3::new(10_000.0, 0.0, 0.0)).await;
    spawn_pos_ball(&ctx, sys_id, def_ship, 587, DVec3::new(10_000.0, 0.0, 0.0)).await;

    let db_key = db_ptr_key(&ctx.db);

    // 1. Out-of-range ship link is removed; destroyed_hub_id is pruned; now_unix <= last_ts skips tick
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, att1_ship),
            ActiveEntosisLinkState {
                module_id: 77_001,
                char_id: att1_char,
                ship_id: att1_ship,
                side_id: att1_ally,
                target_id: hub_id,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(77_001);
    });
    ctx.systems
        .get_or_spawn(sys_id)
        .set_ball_position(att1_ship, DVec3::new(500_000.0, 0.0, 0.0))
        .await;
    // Tick at `now` -> removes out-of-range link & destroyed hub, skips progress since now <= last_ts
    tick_entosis_captures(&ctx, now).await;
    ctx.db.with_snapshot(|s| {
        assert!(!s.sov_runtime.entosis_capture_progress.contains_key(&destroyed_hub_id));
        assert!(!s.module_active_states.contains(&77_001));
    });
    assert!(!ctx
        .tactical_state
        .read()
        .unwrap()
        .entosis_active_links
        .contains_key(&(db_key, att1_ship)));

    // 2. Opposing attacker (att2_ally) partial de-capture, then full de-capture & flip to att2_ally
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, att2_ship),
            ActiveEntosisLinkState {
                module_id: 77_002,
                char_id: att2_char,
                ship_id: att2_ship,
                side_id: att2_ally,
                target_id: hub_id,
                cycle_duration_s: 300.0,
                started_unix: now,
                expires_unix: now + 300,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }
    // +40s -> 100 - 40 = 60s remaining for att1_ally
    tick_entosis_captures(&ctx, now + 40).await;
    ctx.db.with_snapshot(|s| {
        let p = s.sov_runtime.entosis_capture_progress[&hub_id];
        assert_eq!(p.1, att1_ally);
        assert!((p.3 - 60.0).abs() < 1e-6);
    });

    // +80s (dt=80 > 60) -> de-captures 60s to 0, flips side to att2_ally with 20s accrued!
    tick_entosis_captures(&ctx, now + 120).await;
    ctx.db.with_snapshot(|s| {
        let p = s.sov_runtime.entosis_capture_progress[&hub_id];
        assert_eq!(p.1, att2_ally);
        assert_eq!(p.2, att2_ship);
        assert!((p.3 - 20.0).abs() < 1e-6);
    });

    // 3. Cycle expires WITHOUT strontium at now+300 -> stront_alive == false, caps eff_dt at (exp_u - last_ts) = 180s -> 20+180 = 200s, removes link!
    tick_entosis_captures(&ctx, now + 350).await;
    assert!(!ctx
        .tactical_state
        .read()
        .unwrap()
        .entosis_active_links
        .contains_key(&(db_key, att2_ship)));
    ctx.db.with_snapshot(|s| {
        let p = s.sov_runtime.entosis_capture_progress[&hub_id];
        assert_eq!(p.2, 0);
        assert!((p.3 - 200.0).abs() < 1e-6);
    });

    // 4. Hub defender counter-entosis uncontested: partial countdown then complete removal of entry & defender link
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, def_ship),
            ActiveEntosisLinkState {
                module_id: 77_003,
                char_id: def_char,
                ship_id: def_ship,
                side_id: def_ally,
                target_id: hub_id,
                cycle_duration_s: 300.0,
                started_unix: now + 350,
                expires_unix: now + 650,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }
    tick_entosis_captures(&ctx, now + 450).await;
    ctx.db.with_snapshot(|s| {
        let p = s.sov_runtime.entosis_capture_progress[&hub_id];
        assert!((p.3 - 100.0).abs() < 1e-6);
    });
    tick_entosis_captures(&ctx, now + 600).await;
    ctx.db.with_snapshot(|s| {
        assert!(!s.sov_runtime.entosis_capture_progress.contains_key(&hub_id));
    });
    assert!(!ctx
        .tactical_state
        .read()
        .unwrap()
        .entosis_active_links
        .contains_key(&(db_key, def_ship)));

    // 5. Command Node capture by DEFENDER -> defender_won == true: restores Hub Online, keeps sov_alliance_id & upgrades!
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&hub_id).unwrap().state = "HullReinforced".into();
        snap.structures.get_mut(&hub_id).unwrap().reinforced_until_unix = now + 10_000;
        snap.sov_runtime.command_nodes.insert(node_id, (hub_id, sys_id, 0));
        snap.sov_runtime.command_node_scores.insert(hub_id, vec![(def_ally, 0.98)]);
        snap.sov_runtime
            .entosis_capture_progress
            .insert(node_id, (def_char, def_ally, def_ship, 5_000.0, now + 600));
        snap.items.insert(
            1_085_000_299,
            InventoryItemRecord {
                item_id: 1_085_000_299,
                type_id: 16_275,
                owner_id: def_char,
                location_id: def_ship,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                ..Default::default()
            },
        );
    });
    spawn_pos_ball(&ctx, sys_id, node_id, TYPE_STRUCTURE_COMMAND_NODE, DVec3::new(15_000.0, 0.0, 0.0)).await;
    {
        let mut tst = ctx.tactical_state.write().unwrap();
        tst.entosis_active_links.insert(
            (db_key, def_ship),
            ActiveEntosisLinkState {
                module_id: 77_003,
                char_id: def_char,
                ship_id: def_ship,
                side_id: def_ally,
                target_id: node_id,
                cycle_duration_s: 300.0,
                started_unix: now + 600,
                expires_unix: now + 900,
                cycle_index: 1,
                is_capital: false,
            },
        );
    }
    let _ = ctx.drain_notifications_for_char(def_char);
    tick_entosis_captures(&ctx, now + 620).await;
    ctx.db.with_snapshot(|s| {
        let st = &s.structures[&hub_id];
        assert_eq!(st.state, "Online");
        assert_eq!(st.reinforced_until_unix, 0);
        assert_eq!(s.system_states[&sys_id].sov_alliance_id, Some(def_ally));
        assert!(s.sov_runtime.sov_hubs.contains_key(&hub_id));
        assert!(!s.sov_runtime.command_nodes.contains_key(&node_id));
        assert!(!s.sov_runtime.command_node_scores.contains_key(&hub_id));
    });
}
