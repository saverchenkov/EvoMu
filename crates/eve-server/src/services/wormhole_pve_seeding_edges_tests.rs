//! Edge-case behavioral unit tests for `wormhole_pve_seeding`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::test_ctx;
use eve_db::clock::VirtualServerClock;
use eve_db::{ServerClock, WorldRepository};
use eve_sde::InvType;
use glam::DVec3;
use std::sync::Arc;

fn make_wh_ctx() -> (ServerContext, Arc<VirtualServerClock>) {
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_785_000_000));
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(test_ctx().sde.clone(), db);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sleeper_sentry_helpers_cooldowns_loot_drop_and_special_wh_seeding() {
    let (ctx, clock) = make_wh_ctx();
    let wh_sys = *ctx
        .sde
        .solar_systems
        .keys()
        .find(|&&sid| {
            let (cls, _, sh, _) = system_wormhole_profile_sde(&ctx.sde, sid);
            cls == 3 && !sh
        })
        .unwrap_or(&31_001_001);

    // 1. Helpers
    assert_eq!(sleeper_sentry_type_for_class(1), 30_460);
    assert_eq!(sleeper_sentry_type_for_class(2), 30_460);
    assert_eq!(sleeper_sentry_type_for_class(3), 30_461);
    assert_eq!(sleeper_sentry_type_for_class(4), 30_461);
    assert_eq!(sleeper_sentry_type_for_class(5), 30_462);
    assert!(is_sleeper_sentry_type(30_460));
    assert!(is_sleeper_sentry_type(30_461));
    assert!(is_sleeper_sentry_type(30_462));
    assert!(!is_sleeper_sentry_type(587));
    assert_eq!(capital_escalation_class_for_group(485), Some(true));
    assert_eq!(capital_escalation_class_for_group(4594), Some(true));
    assert_eq!(capital_escalation_class_for_group(547), Some(false));
    assert_eq!(capital_escalation_class_for_group(1538), Some(false));
    assert_eq!(capital_escalation_class_for_group(25), None);
    assert!(is_any_capital_group(30));
    assert!(is_any_capital_group(659));
    assert!(!is_any_capital_group(25));

    // 2. build_sleeper_sentry_entity + spawn_sleeper_sentry_tower with real SDE 30_460 / 30_461 / 30_462
    let center = DVec3::new(10_000.0, 20_000.0, 30_000.0);
    for (cls, expected_tid) in [(1_u8, 30_460_u32), (3, 30_461), (6, 30_462)] {
        let (b, _cs, ent) = build_sleeper_sentry_entity(&ctx.sde, wh_sys, 5_000 + i64::from(cls), expected_tid, center, cls);
        assert_eq!(b.type_id, expected_tid);
        assert_eq!(ent.archetype, "SleeperSentry");
        assert_eq!(ent.sleeper_wh_class, cls);
    }
    let tower_id = spawn_sleeper_sentry_tower(&ctx, wh_sys, 3, center).await;
    let sys_handle = ctx.systems.get_or_spawn(wh_sys);
    let (_, tower_ball) = sys_handle.get_ball_snapshot(tower_id).await.unwrap().expect("tower ball");
    assert_eq!(tower_ball.position, center + DVec3::new(3_000.0, 0.0, 2_500.0));
    assert_eq!(tower_ball.type_id, 30_461);
    assert!(ctx.combat_ephemeral.read().unwrap().npc_entities.contains_key(&tower_id));

    // 3. can_sentry_fire_and_record: first fires, immediate both-small fails, advance either triggers true
    assert!(can_sentry_fire_and_record(&ctx, tower_id, 10.0, 100.0));
    assert!(!can_sentry_fire_and_record(&ctx, tower_id, 10.0, 105.0));
    clock.advance_secs(11);
    assert!(can_sentry_fire_and_record(&ctx, tower_id, 10.0, 105.0));

    // 4. drop_site_loot_into_space creates wreck/container 3293 + ball + child loot item
    let drop_item_id = drop_site_loot_into_space(&ctx, wh_sys, 90_900_001, [100.0, 200.0, 300.0], 34, 42).await;
    ctx.db.with_snapshot(|snap| {
        let loot = snap.items.get(&drop_item_id).expect("loot item");
        assert_eq!(loot.type_id, 34);
        assert_eq!(loot.quantity, 42);
        let cont = snap.items.get(&loot.location_id).expect("container");
        assert_eq!(cont.type_id, 3293);
    });

    // 5. seed_special_wormhole_pve_sites across non-shattered (false), Drifter, C13, and shattered systems
    assert!(!seed_special_wormhole_pve_sites(&ctx, 30_000_142).await);

    let mut tested_drifter = false;
    let mut tested_c13 = false;
    let mut tested_shattered_classes = Vec::new();
    for &sid in ctx.sde.solar_systems.keys() {
        let (cls, _, is_shattered, _) = system_wormhole_profile_sde(&ctx.sde, sid);
        if (14..=18).contains(&cls) && !tested_drifter {
            assert!(seed_special_wormhole_pve_sites(&ctx, sid).await);
            assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&sid).is_none()));
            tested_drifter = true;
        } else if cls == 13 && !tested_c13 {
            assert!(seed_special_wormhole_pve_sites(&ctx, sid).await);
            assert!(ctx.db.with_snapshot(|s| {
                s.cosmic_signatures.get(&sid).is_some_and(|l| l.len() == 4)
                    && s.cosmic_anomalies.get(&sid).is_some_and(|l| l.len() == 5)
            }));
            tested_c13 = true;
        } else if (1..=6).contains(&cls) && is_shattered && !tested_shattered_classes.contains(&cls) {
            assert!(seed_special_wormhole_pve_sites(&ctx, sid).await);
            // Re-seeding is idempotent wrt cosmic_anomalies/cosmic_signatures deduplication
            assert!(seed_special_wormhole_pve_sites(&ctx, sid).await);
            assert!(ctx.db.with_snapshot(|s| {
                s.cosmic_anomalies
                    .get(&sid)
                    .is_some_and(|l| l.iter().any(|a| a.anomaly_type == "Ore") && l.iter().any(|a| a.anomaly_type == "IceBelt"))
            }));
            tested_shattered_classes.push(cls);
        }
    }
    assert!(tested_drifter && tested_c13 && !tested_shattered_classes.is_empty());
}

#[test]
fn build_specialized_npc_components_archetypes_attributes_and_fallbacks() {
    let mut custom_sde = (*test_ctx().sde).clone();
    let base_type = custom_sde.types.values().next().unwrap().clone();
    // CloneSoldier Trainer, Recruiter, Transporter, Negotiator without group-1206 fallback confusion + custom HP-less NPC + full-attr NPC + Aroused Hypnosian
    let make_npc = |tid: u32, name: &str, grp: u32, faction: Option<u32>, attrs: Vec<(u16, f64)>| {
        InvType {
            type_id: tid,
            group_id: grp,
            category_id: 11,
            name: name.into(),
            published: true,
            faction_id: faction,
            default_attributes: attrs.into_iter().collect(),
            ..base_type.clone()
        }
    };

    let full_attr_tid = 9_700_001_u32;
    let zero_hp_tid = 9_700_002_u32;
    let clone_trainer = 9_700_003_u32;
    let clone_recruiter = 9_700_004_u32;
    let clone_transporter = 9_700_005_u32;
    let clone_negotiator = 9_700_006_u32;
    let aroused_hyp = 9_700_007_u32;
    let mut custom_drifter = make_npc(9_700_008, "Drifter Cruiser", 1000, None, vec![(265, 500.0)]);
    custom_drifter.group_id = 1000; // != 1956 -> overshield 4000

    custom_sde.types.insert(
        full_attr_tid,
        make_npc(
            full_attr_tid,
            "Full Attr NPC",
            1000,
            None,
            vec![
                (37, 420.0),
                (54, 22_000.0),
                (481, 123_456.0),
                (64, 2.5),
                (114, 10.0),
                (116, 20.0),
                (117, 30.0),
                (118, 40.0),
                (20, -45.0),
                (514, 12_000.0),
                (105, 3.0),
                (103, 18_000.0),
                (97, 80.0),
                (1460, 150.0),
                (1455, 250.0),
                (1464, 35_000.0),
                (9, 1_500.0),
            ],
        ),
    );
    custom_sde.types.insert(zero_hp_tid, make_npc(zero_hp_tid, "Zero HP", 1000, None, vec![(9, 0.0)]));
    custom_sde.types.insert(clone_trainer, make_npc(clone_trainer, "Angel Clone Soldier Trainer", 1000, None, vec![]));
    custom_sde.types.insert(clone_recruiter, make_npc(clone_recruiter, "Angel Clone Soldier Recruiter", 1000, None, vec![]));
    custom_sde.types.insert(clone_transporter, make_npc(clone_transporter, "Angel Clone Soldier Transporter", 1000, None, vec![]));
    custom_sde.types.insert(clone_negotiator, make_npc(clone_negotiator, "Angel Clone Soldier Negotiator", 1000, None, vec![]));
    custom_sde.types.insert(aroused_hyp, make_npc(aroused_hyp, "Aroused Hypnosian Sleeper", 1000, None, vec![(265, 500.0)]));
    custom_sde.types.insert(9_700_008, custom_drifter);

    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(custom_sde), db);
    let wh_c3 = *ctx
        .sde
        .solar_systems
        .keys()
        .find(|&&sid| system_wormhole_profile_sde(&ctx.sde, sid).0 == 3)
        .unwrap_or(&31_001_001);

    // 1. Full attrs with Navy valid spawn_group_id vs out-of-range spawn_group_id
    let (b1, cs1, ent1) = build_specialized_npc_components(&ctx, 30_000_142, 1, full_attr_tid, "Navy", 500_002, DVec3::ZERO);
    assert_eq!(b1.max_velocity, 420.0);
    assert_eq!(ent1.faction_id, 500_002);
    assert_eq!(ent1.bounty_isk, 123_456.0);
    assert_eq!(ent1.damage_vector.em, 25.0);
    assert_eq!(ent1.damage_vector.explosive, 50.0);
    assert_eq!(ent1.damage_vector.kinetic, 75.0);
    assert_eq!(ent1.damage_vector.thermal, 100.0);
    assert_eq!(ent1.remote_shield_rep, 150.0);
    assert_eq!(ent1.remote_armor_rep, 250.0);
    assert_eq!(ent1.remote_rep_range, 35_000.0);
    assert_eq!(ent1.sleeper_wh_class, 7);
    assert!(cs1.max_structure_hp >= 1_500.0);

    let (_, _, ent_navy_def) = build_specialized_npc_components(&ctx, 30_000_142, 2, zero_hp_tid, "FactionPolice", 999, DVec3::ZERO);
    assert_eq!(ent_navy_def.faction_id, 500_001);

    // 2. Zero HP fallback fills 2000/2000/2000; Wormhole C3 system resolves sleeper_wh_class = 3
    let (_, cs_zero, ent_zero) = build_specialized_npc_components(&ctx, wh_c3, 3, zero_hp_tid, "TransmissionRelay", 0, DVec3::ZERO);
    assert_eq!(cs_zero.max_shield_hp, 2_000.0);
    assert_eq!(cs_zero.max_armor_hp, 2_000.0);
    assert_eq!(cs_zero.max_structure_hp, 2_000.0);
    assert_eq!(ent_zero.sleeper_wh_class, 3);
    assert_eq!(ent_zero.optimal_range, 0.0);
    assert_eq!(ent_zero.neut_amount, 0.0);

    // 3. Special type 79_507 zero ewar
    let (_, _, ent_79507) = build_specialized_npc_components(&ctx, 30_000_142, 4, 79_507, "Pirate", 0, DVec3::ZERO);
    assert_eq!(ent_79507.web_speed_factor, 0.0);
    assert_eq!(ent_79507.scram_strength, 0.0);

    // 4. CloneSoldier tiers & ewar
    let (_, _, e_tr) = build_specialized_npc_components(&ctx, 30_000_142, 5, clone_trainer, "CloneSoldier", 0, DVec3::ZERO);
    let (_, _, e_rec) = build_specialized_npc_components(&ctx, 30_000_142, 6, clone_recruiter, "CloneSoldier", 0, DVec3::ZERO);
    let (_, _, e_tp) = build_specialized_npc_components(&ctx, 30_000_142, 7, clone_transporter, "CloneSoldier", 0, DVec3::ZERO);
    let (_, _, e_neg) = build_specialized_npc_components(&ctx, 30_000_142, 8, clone_negotiator, "CloneSoldier", 0, DVec3::ZERO);
    assert!(e_tr.tag_drop_type_id.is_some());
    assert!(e_rec.tag_drop_type_id.is_some());
    assert_eq!(e_tp.web_speed_factor, -60.0);
    assert!(e_neg.scram_strength >= 2.0);
    assert_eq!(e_neg.scram_range, 24_000.0);

    // 5. Aroused PochvenHypnosianSleeper shield 5000, Drifter overshield 4000
    let (_, cs_hyp, _) = build_specialized_npc_components(&ctx, 30_000_142, 9, aroused_hyp, "PochvenHypnosianSleeper", 0, DVec3::ZERO);
    assert_eq!(cs_hyp.max_shield_hp, 5_000.0);

    let (_, _, e_drifter) = build_specialized_npc_components(&ctx, 30_000_142, 10, 9_700_008, "Drifter", 0, DVec3::ZERO);
    assert_eq!(e_drifter.drifter_overshield_hp, 4_000.0);
}

#[tokio::test]
async fn spawn_sleeper_sentry_tower_and_drop_site_loot_into_space_core_work() {
    let ctx = ServerContext::with_repository(Arc::new(WorldRepository::with_demo_seed()));
    let sys_id = 31_000_005_u32;
    let sentry_bid = spawn_sleeper_sentry_tower(&ctx, sys_id, 3, DVec3::new(1_000.0, 0.0, 0.0)).await;
    assert!(sentry_bid > 0);
    assert!(ctx.combat_ephemeral.read().unwrap().npc_entities.contains_key(&sentry_bid));

    let loot_id = drop_site_loot_into_space_core(
        &ctx.db,
        &ctx.systems,
        sys_id,
        90_000_001,
        [500.0, 0.0, 0.0],
        34,
        25,
    )
    .await;
    ctx.db.with_snapshot(|snap| {
        let loot_item = &snap.items[&loot_id];
        assert_eq!(loot_item.type_id, 34);
        assert_eq!(loot_item.quantity, 25);
        assert_eq!(snap.items[&loot_item.location_id].type_id, 3293);
    });
}
