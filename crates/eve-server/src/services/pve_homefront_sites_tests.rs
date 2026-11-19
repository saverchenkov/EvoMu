//! Unit tests for `pve_homefront_sites`.

use super::*;
use crate::router::ServerContext;
use eve_db::clock::VirtualServerClock;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn homefront_seeding_security_filtering_and_rpc_queries() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::new_empty());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

    let hs_sys = HOMEFRONT_SYSTEMS
        .iter()
        .copied()
        .find(|id| sde.solar_systems.contains_key(id))
        .unwrap_or(30_000_142);

    let mut session = SessionState::new(501);
    session.user_id = Some(1);
    session.character_id = Some(90_001);
    session.solar_system_id = Some(hs_sys);
    session.character_id = Some(90_050_001);
    session.solar_system_id = Some(hs_sys);

    let res = handle_get_homefront_site_instances(&ctx, &session, &[])
        .expect("highsec homefront query succeeds");
    let PyRep::Dict(ref outer) = res else {
        panic!("expected outer Dict, got {res:?}");
    };
    let (_, PyRep::List(ref entries)) = outer[0] else {
        panic!("expected system list in outer dict, got {outer:?}");
    };
    assert_eq!(entries.len(), 2);
    for entry in entries {
        assert!(entry.dict_get("dungeonID").and_then(|v| v.as_i64()).is_some());
        assert!(entry.dict_get("instanceID").and_then(|v| v.as_i64()).is_some());
        assert_eq!(entry.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(hs_sys as i64));
    }
    with_pve_special_reg(&ctx, |reg| {
        let sites = reg.homefront_sites.get(&hs_sys).expect("seeded homefront list");
        assert_eq!(sites.len(), 2);
        assert_ne!(sites[0].instance_id, sites[1].instance_id);
        assert_eq!(sites[0].optimal_fleet_size, 5);
    });

    with_pve_special_reg(&ctx, |reg| {
        ensure_homefront_sites_for_system_locked(&ctx, reg, 31_000_001);
        assert!(reg.homefront_sites.get(&31_000_001).is_some_and(|v| v.is_empty()));
        ensure_homefront_sites_for_system_locked(&ctx, reg, hs_sys);
    });

    assert_eq!(
        handle_get_homefront_site_instances(&ctx, &session, &[PyRep::String("bad".into())])
            .expect_err("non-int arg")
            .code,
        "UnknownSolarSystem"
    );
    assert_eq!(
        handle_get_homefront_site_instances(&ctx, &session, &[PyRep::Integer(99_999_999)])
            .expect_err("unknown system")
            .code,
        "UnknownSolarSystem"
    );

    let explicit = handle_get_homefront_site_instances(&ctx, &session, &[PyRep::Integer(31_000_001)])
        .expect("explicit wormhole system returns empty dict entry");
    assert!(matches!(explicit, PyRep::Dict(_)));

    let mut no_sys_session = SessionState::new(502);
    no_sys_session.user_id = Some(2);
    no_sys_session.character_id = Some(90_050_002);
    assert_eq!(
        handle_get_homefront_site_instances(&ctx, &no_sys_session, &[])
            .expect_err("no active solar system")
            .code,
        "NotInSpace"
    );
}

mod multi_role_unit_tests {
    use super::*;
    use crate::services::pve_homefront_sites::{
        abyssal_clear_wave, activate_homefront_acceleration_gate,
        complete_homefront_operation_and_pay, dread_assault_advance_siege,
        dread_assault_transfer_capacitor, emergency_aid_advance_defense,
        get_homefront_runtime_state, metaliminal_mine_mutanite, raid_deliver_cargo_to_freighter,
        raid_tackle_and_destroy_hauler, record_homefront_combat_and_logistics,
        salvage_research_process_wreck, spawn_homefront_site_for_dungeon,
        stabilize_rift_neutralize_arrays, suspicious_signal_attack_relay,
        suspicious_signal_hack_array, traffic_stop_scan_and_intercept,
        HomefrontArchetypeProgress,
    };
    use crate::services::pve_special_sites_ops::{
        is_homefront_exordium_system, HomefrontArchetype,
    };
    use eve_db::clock::VirtualServerClock;
    use eve_db::{InventoryItemRecord, IskCents, WorldRepository};
    use eve_destiny::{Ball, MovementMode};
    use eve_sde::{SolarSystemId, TypeId};
    use glam::DVec3;
    use std::sync::Arc;

    fn fresh_homefront_ctx() -> ServerContext {
        let sde = super::shared_sde();
        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        let db = Arc::new(WorldRepository::new_empty());
        db.with_mut_snapshot(|snap| {
            let mut ch = template_ch;
            ch.character_id = 90_000_001;
            snap.characters.insert(90_000_001, ch);
        });
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
        ctx
    }

    async fn seed_hf_pilot(
        ctx: &ServerContext,
        user_id: u64,
        char_id: i64,
        ship_id: i64,
        ship_type_id: TypeId,
        sys_id: SolarSystemId,
        fitted_modules: &[TypeId],
    ) -> SessionState {
        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = template_ch;
            ch.character_id = char_id;
            ch.user_id = user_id;
            ch.name = format!("HF Pilot {char_id}");
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_isk(1_000_000);
            ch.standings.clear();
            snap.wallet_journal.retain(|j| j.character_id != char_id);
            snap.characters.insert(char_id, ch);
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: ship_type_id,
                    owner_id: char_id,
                    location_id: char_id,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            for (idx, &mod_tid) in fitted_modules.iter().enumerate() {
                let mid = ship_id * 100 + idx as i64 + 1;
                snap.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: mod_tid,
                        owner_id: char_id,
                        location_id: ship_id,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
        });
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h
            .spawn_ball(Ball {
                id: ship_id,
                type_id: ship_type_id,
                position: DVec3::new(0.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            })
            .await;
        let mut sess = SessionState::new(user_id);
        sess.user_id = Some(user_id);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        sess
    }

    #[tokio::test]
    async fn homefront_multi_role_lifecycle_across_all_five_core_and_three_pilot_archetypes() {
        let ctx = fresh_homefront_ctx();
        let sys_id: SolarSystemId = 30_000_142; // Jita (Caldari non-Exordium hi-sec)
        assert!(!is_homefront_exordium_system(&ctx.sde, sys_id));

        let dps1 = seed_hf_pilot(&ctx, 7101, 910_001, 810_001, 621, sys_id, &[]).await; // Caracal
        let dps2 = seed_hf_pilot(&ctx, 7102, 910_002, 810_002, 16_236, sys_id, &[]).await; // Coercer
        let logi = seed_hf_pilot(&ctx, 7103, 910_003, 810_003, 597, sys_id, &[3586]).await; // Punisher logi
        let venture = seed_hf_pilot(&ctx, 7104, 910_004, 810_004, 32_880, sys_id, &[]).await; // Venture
        let tayra = seed_hf_pilot(&ctx, 7105, 910_005, 810_005, 649, sys_id, &[]).await; // Tayra hauler (7,300 m3 >= 3,500 m3)
        let pioneer_miner_cruiser =
            seed_hf_pilot(&ctx, 7106, 910_006, 810_006, 621, sys_id, &[483]).await; // Caracal + Miner I

        // 1. Raid (10347): 5 pilots (DPS + Tackler + Logi + Hauler)
        let raid_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_347).expect("spawn Raid");
        assert_eq!(raid_state.blueprint.archetype, HomefrontArchetype::Raid);
        assert_eq!(raid_state.blueprint.base_pilot_isk, 15_000_000);
        for s in [&dps1, &dps2, &logi, &venture, &tayra] {
            activate_homefront_acceleration_gate(&ctx, s, raid_state.instance_id)
                .await
                .expect("enter Raid gate");
        }
        let post_gate = get_homefront_runtime_state(&ctx, raid_state.instance_id).unwrap();
        assert!(!post_gate.spawned_ball_ids.is_empty());
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        for &bid in &post_gate.spawned_ball_ids {
            assert!(
                sys_h.get_ball_snapshot(bid).await.unwrap().is_some(),
                "spawned Homefront ball {bid} must exist in SystemActorHandle"
            );
        }
        record_homefront_combat_and_logistics(&ctx, &dps2, raid_state.instance_id, 2_400.0, 0.0).unwrap();
        record_homefront_combat_and_logistics(&ctx, &logi, raid_state.instance_id, 0.0, 1_800.0).unwrap();
        for _ in 0..4 {
            raid_tackle_and_destroy_hauler(&ctx, &dps1, raid_state.instance_id, true, 3_000.0).unwrap();
            raid_deliver_cargo_to_freighter(&ctx, &tayra, raid_state.instance_id).unwrap();
        }
        // Only 4 active contributors (venture didn't contribute yet) — give venture >= 1000 dmg too
        record_homefront_combat_and_logistics(&ctx, &venture, raid_state.instance_id, 1_200.0, 0.0).unwrap();
        let raid_summary = complete_homefront_operation_and_pay(&ctx, &dps1, raid_state.instance_id)
            .await
            .expect("complete Raid");
        assert_eq!(raid_summary.eligible_pilot_count, 5);
        assert_eq!(raid_summary.total_fleet_isk, 75_000_000);
        for &bid in &post_gate.spawned_ball_ids {
            assert!(
                sys_h.get_ball_snapshot(bid).await.unwrap().is_none(),
                "completed Homefront ball {bid} must despawn"
            );
        }
        ctx.db.with_snapshot(|snap| {
            let ch = &snap.characters[&910_005];
            assert_eq!(ch.balance, IskCents::from_isk(16_000_000));
            assert!(snap.wallet_journal.iter().any(|j| {
                j.character_id == 910_005
                    && j.entry_type == "CorporateRewardPayout"
                    && j.amount == IskCents::from_isk(15_000_000)
            }));
        });

        // 2. Emergency Aid (10238): sustain protectee over 600s
        let aid_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_238).expect("spawn Emergency Aid");
        assert_eq!(aid_state.blueprint.archetype, HomefrontArchetype::EmergencyAid);
        activate_homefront_acceleration_gate(&ctx, &dps1, aid_state.instance_id).await.unwrap();
        activate_homefront_acceleration_gate(&ctx, &logi, aid_state.instance_id).await.unwrap();
        record_homefront_combat_and_logistics(&ctx, &dps1, aid_state.instance_id, 3_500.0, 0.0).unwrap();
        emergency_aid_advance_defense(&ctx, &logi, aid_state.instance_id, 8_000.0, 12_000.0, 300).unwrap();
        let aid_mid = emergency_aid_advance_defense(&ctx, &logi, aid_state.instance_id, 6_000.0, 4_000.0, 300).unwrap();
        assert!(aid_mid.is_objective_complete());
        let aid_pay = complete_homefront_operation_and_pay(&ctx, &dps1, aid_state.instance_id)
            .await
            .expect("complete Emergency Aid");
        assert_eq!(aid_pay.total_fleet_isk, 30_000_000);

        // 3. Metaliminal Meteoroid (10312): 5,000 units Mutanite (Venture + fitted Caracal) with residue
        let met_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_312).expect("spawn Metaliminal");
        assert_eq!(met_state.blueprint.archetype, HomefrontArchetype::MetaliminalMeteoroid);
        assert_eq!(met_state.blueprint.base_pilot_isk, 12_000_000);
        activate_homefront_acceleration_gate(&ctx, &venture, met_state.instance_id).await.unwrap();
        activate_homefront_acceleration_gate(&ctx, &pioneer_miner_cruiser, met_state.instance_id).await.unwrap();
        metaliminal_mine_mutanite(&ctx, &venture, met_state.instance_id, 2_500, 500).unwrap();
        let met_done = metaliminal_mine_mutanite(&ctx, &pioneer_miner_cruiser, met_state.instance_id, 2_000, 0).unwrap();
        assert!(matches!(
            met_done.progress,
            HomefrontArchetypeProgress::MetaliminalMeteoroid { remaining_units: 0, total_residue_units: 500, ore_mined_by_fleet: 4_500, .. }
        ));
        let met_pay = complete_homefront_operation_and_pay(&ctx, &venture, met_state.instance_id)
            .await
            .expect("complete Metaliminal");
        assert_eq!(met_pay.total_fleet_isk, 24_000_000);

        // 4. Dread Assault (10320): 3 cycles x 30,000 GJ cap -> 90s Siege volleys -> enemy HQ destroyed
        let dread_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_320).expect("spawn Dread Assault");
        assert_eq!(dread_state.blueprint.archetype, HomefrontArchetype::DreadAssault);
        activate_homefront_acceleration_gate(&ctx, &logi, dread_state.instance_id).await.unwrap();
        activate_homefront_acceleration_gate(&ctx, &dps1, dread_state.instance_id).await.unwrap();
        record_homefront_combat_and_logistics(&ctx, &dps1, dread_state.instance_id, 4_000.0, 0.0).unwrap();
        for cycle in 1..=3_u8 {
            dread_assault_transfer_capacitor(&ctx, &logi, dread_state.instance_id, 35_000.0, 5_000.0).unwrap();
            let after_siege = dread_assault_advance_siege(&ctx, &dps1, dread_state.instance_id, 90).unwrap();
            if let HomefrontArchetypeProgress::DreadAssault { siege_cycles_completed, .. } = after_siege.progress {
                assert_eq!(siege_cycles_completed, cycle);
            }
        }
        let dread_pay = complete_homefront_operation_and_pay(&ctx, &logi, dread_state.instance_id)
            .await
            .expect("complete Dread Assault");
        assert_eq!(dread_pay.total_fleet_isk, 30_000_000);

        // 5. Abyssal Artifact Recovery (10346): 9 waves + warp bubble disperse/respawn + 67.5M payout
        let aar_probe = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn Abyssal probe");
        activate_homefront_acceleration_gate(&ctx, &venture, aar_probe.instance_id).await.unwrap();
        let probe_bubble_id = 1_897_800_000 + aar_probe.instance_id * 10 + 9;
        assert!(
            sys_h.remove_warp_bubble(probe_bubble_id).await.unwrap(),
            "AAR gate entry must spawn a live WarpBubble in SystemHandle"
        );

        let aar_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn Abyssal Artifact");
        assert_eq!(aar_state.blueprint.archetype, HomefrontArchetype::AbyssalArtifactRecovery);
        assert_eq!(aar_state.blueprint.base_pilot_isk, 67_500_000);
        activate_homefront_acceleration_gate(&ctx, &venture, aar_state.instance_id).await.unwrap();
        activate_homefront_acceleration_gate(&ctx, &dps1, aar_state.instance_id).await.unwrap();
        let bubble_id = 1_897_800_000 + aar_state.instance_id * 10 + 9;
        for w in 1..=9_u8 {
            let mid = abyssal_clear_wave(&ctx, &venture, aar_state.instance_id, 30, 0.0, w % 2 == 1)
                .await
                .expect("mine Mutanite to disperse warp bubble");
            assert!(matches!(
                mid.progress,
                HomefrontArchetypeProgress::AbyssalArtifactRecovery { warp_bubble_active: false, mutanite_units_remaining: 0, .. }
            ));
            let st = abyssal_clear_wave(&ctx, &dps1, aar_state.instance_id, 0, 2_500.0, false)
                .await
                .expect("DPS finishes AAR wave hostiles");
            if w < 9 {
                assert!(matches!(
                    st.progress,
                    HomefrontArchetypeProgress::AbyssalArtifactRecovery { warp_bubble_active: true, current_wave, .. } if current_wave == w + 1
                ), "wave {} must respawn warp bubble", w + 1);
            } else {
                assert!(st.is_objective_complete());
            }
        }
        let aar_pay = complete_homefront_operation_and_pay(&ctx, &dps1, aar_state.instance_id)
            .await
            .expect("complete AAR");
        assert_eq!(aar_pay.total_fleet_isk, 135_000_000);
        assert!(
            !sys_h.remove_warp_bubble(bubble_id).await.unwrap(),
            "AAR completion must already remove warp bubble from SystemHandle"
        );

        // 6. Suspicious Signal (10309): hack 3 arrays then burn 350k relay HP inside vulnerability window
        let sig_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_309).expect("spawn Suspicious Signal");
        assert_eq!(sig_state.blueprint.archetype, HomefrontArchetype::SuspiciousSignal);
        activate_homefront_acceleration_gate(&ctx, &venture, sig_state.instance_id).await.unwrap();
        activate_homefront_acceleration_gate(&ctx, &dps1, sig_state.instance_id).await.unwrap();
        for idx in 0..3 {
            suspicious_signal_hack_array(&ctx, &venture, sig_state.instance_id, idx).unwrap();
        }
        suspicious_signal_attack_relay(&ctx, &dps1, sig_state.instance_id, 350_000.0, 45).unwrap();
        let sig_pay = complete_homefront_operation_and_pay(&ctx, &dps1, sig_state.instance_id)
            .await
            .expect("complete Suspicious Signal");
        assert_eq!(sig_pay.total_fleet_isk, 30_000_000);

        // 7-9. Three-pilot operations (10713 Salvage Research, 10714 Stabilize Rift, 10705 Traffic Stop)
        let sal = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_713).unwrap();
        assert_eq!(sal.blueprint.optimal_fleet_size, 3);
        activate_homefront_acceleration_gate(&ctx, &dps2, sal.instance_id).await.unwrap(); // Coercer T1 dessy
        salvage_research_process_wreck(&ctx, &dps2, sal.instance_id, 30, true).unwrap();
        assert_eq!(complete_homefront_operation_and_pay(&ctx, &dps2, sal.instance_id).await.unwrap().total_fleet_isk, 12_000_000);

        let rift = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_714).unwrap();
        activate_homefront_acceleration_gate(&ctx, &dps2, rift.instance_id).await.unwrap();
        stabilize_rift_neutralize_arrays(&ctx, &dps2, rift.instance_id, [0.9; 5], 60).unwrap();
        assert_eq!(complete_homefront_operation_and_pay(&ctx, &dps2, rift.instance_id).await.unwrap().total_fleet_isk, 12_000_000);

        let traf = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_705).unwrap();
        activate_homefront_acceleration_gate(&ctx, &logi, traf.instance_id).await.unwrap();
        traffic_stop_scan_and_intercept(&ctx, &logi, traf.instance_id, 6, 1, 6_000.0).unwrap();
        assert_eq!(complete_homefront_operation_and_pay(&ctx, &logi, traf.instance_id).await.unwrap().total_fleet_isk, 12_000_000);
    }

    #[tokio::test]
    async fn homefront_five_pilot_role_cap_and_leecher_dilution_in_runtime() {
        let ctx = fresh_homefront_ctx();
        let sys_id: SolarSystemId = 30_000_142;
        let raid = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_347).unwrap();

        let mut contributors = Vec::new();
        for idx in 0..12_i64 {
            let ship_tid = if idx == 0 { 649 } else { 621 };
            let s = seed_hf_pilot(
                &ctx,
                7200 + idx as u64,
                920_000 + idx,
                820_000 + idx,
                ship_tid,
                sys_id,
                &[],
            )
            .await;
            activate_homefront_acceleration_gate(&ctx, &s, raid.instance_id).await.unwrap();
            contributors.push(s);
        }
        // Pilot 13 enters gate but remains idle (zero contribution -> leecher exclusion)
        let leecher = seed_hf_pilot(&ctx, 7299, 920_099, 820_099, 597, sys_id, &[]).await;
        activate_homefront_acceleration_gate(&ctx, &leecher, raid.instance_id).await.unwrap();

        for _ in 0..4 {
            raid_tackle_and_destroy_hauler(&ctx, &contributors[1], raid.instance_id, true, 3_000.0).unwrap();
            raid_deliver_cargo_to_freighter(&ctx, &contributors[0], raid.instance_id).unwrap();
        }
        for c in &contributors[2..] {
            record_homefront_combat_and_logistics(&ctx, c, raid.instance_id, 1_500.0, 0.0).unwrap();
        }

        let summary = complete_homefront_operation_and_pay(&ctx, &contributors[0], raid.instance_id)
            .await
            .expect("settle over-cap Raid");
        assert_eq!(summary.optimal_fleet_size, 5);
        assert_eq!(summary.max_payout_pilots, 10);
        assert_eq!(summary.eligible_pilot_count, 10);
        // Fleet pool MUST cap at 5 * 15M = 75M (diluted to 7.5M across first 10; pilots 11..12 + leecher get 0)
        assert_eq!(summary.total_fleet_isk, 75_000_000);
        let paid_count = summary.payouts.iter().filter(|p| p.isk == 7_500_000).count();
        let zero_count = summary.payouts.iter().filter(|p| !p.eligible && p.isk == 0).count();
        assert_eq!(paid_count, 10);
        assert_eq!(zero_count, 3);
        let leecher_payout = summary
            .payouts
            .iter()
            .find(|p| p.character_id == 920_099)
            .unwrap();
        assert!(!leecher_payout.eligible && leecher_payout.isk == 0 && leecher_payout.lp == 0);
    }
}
