//! Unit tests for `pve_homefront_sites`.

use super::*;
use crate::router::ServerContext;
use crate::services::pve_special_sites::with_pve_special_reg;
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
    session.character_id = Some(90_050_001);
    session.solar_system_id = Some(hs_sys);

    // GetHomefrontSiteInstances must remain strictly read-only on an uninitialized snapshot
    // while deterministically returning hub & Exordium sites and 0 for non-hub systems.
    let cold_res = handle_get_homefront_site_instances(&ctx, &session, &[])
        .expect("cold highsec homefront query succeeds read-only");
    let PyRep::Dict(ref cold_outer) = cold_res else {
        panic!("expected cold outer Dict, got {cold_res:?}");
    };
    let (_, PyRep::List(ref cold_entries)) = cold_outer[0] else {
        panic!("expected cold system list in outer dict, got {cold_outer:?}");
    };
    assert_eq!(cold_entries.len(), 2, "cold hub GetHomefrontSiteInstances must read-only compute 2 entries");
    let cold_pairs: Vec<(i64, i64)> = cold_entries
        .iter()
        .map(|e| {
            (
                e.dict_get("dungeonID").and_then(|v| v.as_i64()).unwrap(),
                e.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap(),
            )
        })
        .collect();

    if sde.solar_systems.contains_key(&30_000_144) {
        let cold_perim = handle_get_homefront_site_instances(&ctx, &session, &[PyRep::Integer(30_000_144)])
            .expect("cold non-hub highsec query succeeds read-only");
        let PyRep::Dict(ref p_outer) = cold_perim else { panic!(); };
        let (_, PyRep::List(ref p_entries)) = p_outer[0] else { panic!(); };
        assert!(p_entries.is_empty(), "non-hub highsec Perimeter must return 0 cold entries");
    }

    let exordium_sys_opt = sde
        .schools
        .values()
        .find(|sc| {
            crate::services::pve_special_sites_ops::is_homefront_exordium_system(
                &sde,
                sc.solar_system_id,
            )
        })
        .map(|sc| sc.solar_system_id);
    let cold_exordium_pairs: Option<Vec<(i64, i64)>> = exordium_sys_opt.map(|ex_sys| {
        let ex_res = handle_get_homefront_site_instances(
            &ctx,
            &session,
            &[PyRep::Integer(i64::from(ex_sys))],
        )
        .expect("cold Exordium query succeeds read-only");
        let PyRep::Dict(ref ex_outer) = ex_res else { panic!(); };
        let (_, PyRep::List(ref ex_entries)) = ex_outer[0] else { panic!(); };
        assert_eq!(ex_entries.len(), 2, "cold Exordium system must return 2 deterministic entries");
        ex_entries
            .iter()
            .map(|e| {
                (
                    e.dict_get("dungeonID").and_then(|v| v.as_i64()).unwrap(),
                    e.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap(),
                )
            })
            .collect()
    });

    with_pve_special_reg(&ctx, |reg| {
        assert!(!reg.initialized, "GetHomefrontSiteInstances must not initialize special-sites registry");
        assert!(!reg.homefront_sites.contains_key(&hs_sys));
        if let Some(ex_sys) = exordium_sys_opt {
            assert!(!reg.homefront_sites.contains_key(&ex_sys));
        }
    });

    crate::services::pve_special_sites::ensure_special_sites_initialized(&ctx);
    let res = handle_get_homefront_site_instances(&ctx, &session, &[])
        .expect("highsec homefront query succeeds after initialization");
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
        assert!(matches!(sites[0].optimal_fleet_size, 3 | 5));
        let warm_pairs: Vec<(i64, i64)> = sites
            .iter()
            .map(|s| (i64::from(s.dungeon_id), s.instance_id))
            .collect();
        assert_eq!(cold_pairs, warm_pairs, "cold read must match warm deterministic seeding");
        if let (Some(ex_sys), Some(ex_cold)) = (exordium_sys_opt, cold_exordium_pairs.as_ref()) {
            let ex_sites = reg.homefront_sites.get(&ex_sys).expect("Exordium seeded by init_homefront_sites");
            let ex_warm: Vec<(i64, i64)> = ex_sites
                .iter()
                .map(|s| (i64::from(s.dungeon_id), s.instance_id))
                .collect();
            assert_eq!(ex_cold, &ex_warm, "cold Exordium read must match warm seeding");
        }
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
            let uname = format!("hf_pilot_{user_id}");
            snap.accounts
                .entry(uname.clone())
                .or_insert_with(|| eve_db::AccountRecord {
                    user_id,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                });
            let mut ch = template_ch;
            ch.character_id = char_id;
            ch.user_id = user_id;
            ch.name = format!("HF Pilot {char_id}");
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
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
            let mut hi_idx = 0usize;
            let mut med_idx = 0usize;
            let hi_flags = [
                eve_db::InventoryFlag::HiSlot0,
                eve_db::InventoryFlag::HiSlot1,
                eve_db::InventoryFlag::HiSlot2,
                eve_db::InventoryFlag::HiSlot3,
                eve_db::InventoryFlag::HiSlot4,
                eve_db::InventoryFlag::HiSlot5,
                eve_db::InventoryFlag::HiSlot6,
                eve_db::InventoryFlag::HiSlot7,
            ];
            let med_flags = [
                eve_db::InventoryFlag::MedSlot0,
                eve_db::InventoryFlag::MedSlot1,
                eve_db::InventoryFlag::MedSlot2,
                eve_db::InventoryFlag::MedSlot3,
                eve_db::InventoryFlag::MedSlot4,
                eve_db::InventoryFlag::MedSlot5,
                eve_db::InventoryFlag::MedSlot6,
                eve_db::InventoryFlag::MedSlot7,
            ];
            let needs_salvage_skill = fitted_modules.contains(&25_861);
            if needs_salvage_skill {
                snap.skills.entry(char_id).or_default().insert(
                    25_863,
                    eve_db::TrainedSkillRecord {
                        type_id: 25_863,
                        level: 4,
                        skill_points: 45_255.0,
                    },
                );
            }
            for (idx, &mod_tid) in fitted_modules.iter().enumerate() {
                let mid = ship_id * 100 + idx as i64 + 1;
                let is_mid_slot = matches!(mod_tid, 22_175 | 22_586 | 442 | 440);
                let flag = if is_mid_slot {
                    let f = med_flags[med_idx.min(7)];
                    med_idx += 1;
                    f
                } else {
                    let f = hi_flags[hi_idx.min(7)];
                    hi_idx += 1;
                    f
                };
                snap.items.insert(
                    mid,
                    InventoryItemRecord {
                        item_id: mid,
                        type_id: mod_tid,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
            }
        });
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
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
                },
                ctx.build_type_combat_state(ship_type_id),
            )
            .await;
        ctx.db
            .acquire_online_lock(user_id, user_id, Some(char_id), false, ctx.now_unix())
            .expect("register online session for homefront pilot");
        let mut sess = SessionState::new(user_id);
        sess.user_id = Some(user_id);
        sess.character_id = Some(char_id);
        sess.solar_system_id = Some(sys_id);
        sess.ship_id = Some(ship_id);
        sess
    }

    async fn enter_hf_gate_at_site(
        ctx: &ServerContext,
        sess: &SessionState,
        instance_id: i64,
    ) -> Result<crate::services::pve_special_sites_ops::HomefrontShipEligibility, BridgeRpcError> {
        if let Some(st) = get_homefront_runtime_state(ctx, instance_id) {
            if let Some(sid) = sess.ship_id.filter(|&id| id > 0) {
                let sys_h = ctx.systems.get_or_spawn(st.solar_system_id);
                let target_pos = st.site_position + DVec3::new(400.0, 0.0, 0.0);
                if sys_h.get_ball_snapshot(sid).await.ok().flatten().is_none() {
                    let _ = sys_h
                        .spawn_ball(Ball {
                            id: sid,
                            type_id: 621,
                            position: target_pos,
                            velocity: DVec3::ZERO,
                            max_velocity: 300.0,
                            inertia: 1.0,
                            mass: 1_000_000.0,
                            radius: 50.0,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        })
                        .await;
                } else {
                    let _ = sys_h.set_ball_position(sid, target_pos).await;
                }
            }
        }
        activate_homefront_acceleration_gate(ctx, sess, instance_id).await
    }

    #[tokio::test]
    async fn homefront_multi_role_lifecycle_across_all_five_core_and_three_pilot_archetypes() {
        let ctx = fresh_homefront_ctx();
        let sys_id: SolarSystemId = 30_000_142; // Jita (Caldari non-Exordium hi-sec)
        assert!(!is_homefront_exordium_system(&ctx.sde, sys_id));

        let dps1 = seed_hf_pilot(&ctx, 7101, 910_001, 810_001, 621, sys_id, &[]).await; // Caracal
        let dps2 = seed_hf_pilot(&ctx, 7102, 910_002, 810_002, 16_236, sys_id, &[25_861, 22_175, 533]).await; // Coercer: Salvager I + Data Analyzer I + Small Energy Neut
        let logi = seed_hf_pilot(&ctx, 7103, 910_003, 810_003, 597, sys_id, &[3586, 529, 442]).await; // Punisher logi + Remote Cap + Cargo Scanner
        let venture = seed_hf_pilot(&ctx, 7104, 910_004, 810_004, 32_880, sys_id, &[483, 22_175]).await; // Venture + Miner I + Data Analyzer I
        let tayra = seed_hf_pilot(&ctx, 7105, 910_005, 810_005, 649, sys_id, &[]).await; // Tayra hauler (7,300 m3 >= 3,500 m3)
        let pioneer_miner_cruiser =
            seed_hf_pilot(&ctx, 7106, 910_006, 810_006, 621, sys_id, &[483]).await; // Caracal + Miner I

        // 1. Raid (10347): 5 pilots (DPS + Tackler + Logi + Hauler)
        let raid_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_347).expect("spawn Raid");
        assert_eq!(raid_state.blueprint.archetype, HomefrontArchetype::Raid);
        assert_eq!(raid_state.blueprint.base_pilot_isk, 15_000_000);
        for s in [&dps1, &dps2, &logi, &venture, &tayra] {
            enter_hf_gate_at_site(&ctx, s, raid_state.instance_id)
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

        // 2. Emergency Aid (10238): sustain protectee over 600s (n=2 -> (4/5)^3 * 15M = 7,680,000 each -> 15,360,000)
        let aid_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_238).expect("spawn Emergency Aid");
        assert_eq!(aid_state.blueprint.archetype, HomefrontArchetype::EmergencyAid);
        enter_hf_gate_at_site(&ctx, &dps1, aid_state.instance_id).await.unwrap();
        enter_hf_gate_at_site(&ctx, &logi, aid_state.instance_id).await.unwrap();
        record_homefront_combat_and_logistics(&ctx, &dps1, aid_state.instance_id, 3_500.0, 0.0).unwrap();
        emergency_aid_advance_defense(&ctx, &logi, aid_state.instance_id, 8_000.0, 12_000.0, 300).unwrap();
        let aid_mid = emergency_aid_advance_defense(&ctx, &logi, aid_state.instance_id, 6_000.0, 4_000.0, 300).unwrap();
        assert!(aid_mid.is_objective_complete());
        let aid_pay = complete_homefront_operation_and_pay(&ctx, &dps1, aid_state.instance_id)
            .await
            .expect("complete Emergency Aid");
        assert_eq!(aid_pay.total_fleet_isk, 15_360_000);

        // 3. Metaliminal Meteoroid (10312): 5,000 units Mutanite (Venture + fitted Caracal) with residue (n=2 -> 6,144,000 * 2 = 12,288,000)
        let met_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_312).expect("spawn Metaliminal");
        assert_eq!(met_state.blueprint.archetype, HomefrontArchetype::MetaliminalMeteoroid);
        assert_eq!(met_state.blueprint.base_pilot_isk, 12_000_000);
        enter_hf_gate_at_site(&ctx, &venture, met_state.instance_id).await.unwrap();
        enter_hf_gate_at_site(&ctx, &pioneer_miner_cruiser, met_state.instance_id).await.unwrap();
        metaliminal_mine_mutanite(&ctx, &venture, met_state.instance_id, 2_500, 500).unwrap();
        let met_done = metaliminal_mine_mutanite(&ctx, &pioneer_miner_cruiser, met_state.instance_id, 2_000, 0).unwrap();
        assert!(matches!(
            met_done.progress,
            HomefrontArchetypeProgress::MetaliminalMeteoroid { remaining_units: 0, total_residue_units: 500, ore_mined_by_fleet: 4_500, .. }
        ));
        let met_pay = complete_homefront_operation_and_pay(&ctx, &venture, met_state.instance_id)
            .await
            .expect("complete Metaliminal");
        assert_eq!(met_pay.total_fleet_isk, 12_288_000);

        // 4. Dread Assault (10320): 3 cycles x 30,000 GJ cap -> 90s Siege volleys -> enemy HQ destroyed (n=2 -> 15,360,000)
        let dread_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_320).expect("spawn Dread Assault");
        assert_eq!(dread_state.blueprint.archetype, HomefrontArchetype::DreadAssault);
        enter_hf_gate_at_site(&ctx, &logi, dread_state.instance_id).await.unwrap();
        enter_hf_gate_at_site(&ctx, &dps1, dread_state.instance_id).await.unwrap();
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
        assert_eq!(dread_pay.total_fleet_isk, 15_360_000);

        // 5. Abyssal Artifact Recovery (10346): 9 waves + warp bubble disperse/respawn + (4/5)^3 scaling (n=2 -> 69,120,000)
        let aar_probe = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn Abyssal probe");
        enter_hf_gate_at_site(&ctx, &venture, aar_probe.instance_id).await.unwrap();
        let probe_bubble_id = 1_897_800_000 + aar_probe.instance_id * 100 + 9;
        assert!(
            sys_h.remove_warp_bubble(probe_bubble_id).await.unwrap(),
            "AAR gate entry must spawn a live WarpBubble in SystemHandle"
        );

        let aar_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn Abyssal Artifact");
        assert_eq!(aar_state.blueprint.archetype, HomefrontArchetype::AbyssalArtifactRecovery);
        assert_eq!(aar_state.blueprint.base_pilot_isk, 67_500_000);
        enter_hf_gate_at_site(&ctx, &venture, aar_state.instance_id).await.unwrap();
        enter_hf_gate_at_site(&ctx, &dps1, aar_state.instance_id).await.unwrap();
        let v_cid = venture.character_id.unwrap();
        let d_cid = dps1.character_id.unwrap();
        let (v_bal_before, v_j_before, d_bal_before, d_j_before) = ctx.db.with_snapshot(|snap| {
            let vj = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == v_cid && j.entry_type == "CorporateRewardPayout")
                .count();
            let dj = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == d_cid && j.entry_type == "CorporateRewardPayout")
                .count();
            (snap.characters[&v_cid].balance, vj, snap.characters[&d_cid].balance, dj)
        });
        let _ = ctx.drain_notifications_for_char(v_cid);
        let _ = ctx.drain_notifications_for_char(d_cid);
        let bubble_id = 1_897_800_000 + aar_state.instance_id * 100 + 9;
        for w in 1..=9_u8 {
            let mid = abyssal_clear_wave(&ctx, &venture, aar_state.instance_id, 324, 0.0, w % 2 == 1)
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
        // Per-wave AAR payouts must credit wallets, journal (9 non-zero rows), and OnAccountChange BEFORE settlement.
        let (v_bal_pre_settle, v_j_pre_settle, d_bal_pre_settle, d_j_pre_settle) = ctx.db.with_snapshot(|snap| {
            let vj: Vec<_> = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == v_cid && j.entry_type == "CorporateRewardPayout")
                .skip(v_j_before)
                .collect();
            let dj: Vec<_> = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == d_cid && j.entry_type == "CorporateRewardPayout")
                .skip(d_j_before)
                .collect();
            assert_eq!(vj.len(), 9, "Venture must receive 9 per-wave AAR payouts");
            assert_eq!(dj.len(), 9, "DPS1 must receive 9 per-wave AAR payouts");
            assert!(vj.iter().all(|j| j.amount > IskCents(0)), "no zero-ISK AAR journal entries");
            assert!(dj.iter().all(|j| j.amount > IskCents(0)), "no zero-ISK AAR journal entries");
            (snap.characters[&v_cid].balance, vj.len(), snap.characters[&d_cid].balance, dj.len())
        });
        assert_eq!(
            v_bal_pre_settle,
            v_bal_before.checked_add(IskCents::from_isk(34_560_000)).unwrap()
        );
        assert_eq!(
            d_bal_pre_settle,
            d_bal_before.checked_add(IskCents::from_isk(34_560_000)).unwrap()
        );
        assert!(ctx.drain_notifications_for_char(v_cid).iter().any(|n| n.event == "OnAccountChange"));
        assert!(ctx.drain_notifications_for_char(d_cid).iter().any(|n| n.event == "OnAccountChange"));

        let aar_pay = complete_homefront_operation_and_pay(&ctx, &dps1, aar_state.instance_id)
            .await
            .expect("complete AAR");
        assert_eq!(aar_pay.total_fleet_isk, 69_120_000);
        // Settlement must grant LP without double-paying already-paid AAR waves or emitting OnAccountChange again.
        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.characters[&v_cid].balance, v_bal_pre_settle);
            assert_eq!(snap.characters[&d_cid].balance, d_bal_pre_settle);
            let vj_after = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == v_cid && j.entry_type == "CorporateRewardPayout")
                .count();
            let dj_after = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == d_cid && j.entry_type == "CorporateRewardPayout")
                .count();
            assert_eq!(vj_after, v_j_before + v_j_pre_settle);
            assert_eq!(dj_after, d_j_before + d_j_pre_settle);
        });
        assert!(!ctx.drain_notifications_for_char(v_cid).iter().any(|n| n.event == "OnAccountChange"));
        assert!(!ctx.drain_notifications_for_char(d_cid).iter().any(|n| n.event == "OnAccountChange"));
        assert!(
            !sys_h.remove_warp_bubble(bubble_id).await.unwrap(),
            "AAR completion must already remove warp bubble from SystemHandle"
        );

        // 6. Suspicious Signal (10309): hack 3 arrays then burn 350k relay HP inside vulnerability window (n=2 -> 15,360,000)
        let sig_state = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_309).expect("spawn Suspicious Signal");
        assert_eq!(sig_state.blueprint.archetype, HomefrontArchetype::SuspiciousSignal);
        enter_hf_gate_at_site(&ctx, &venture, sig_state.instance_id).await.unwrap();
        enter_hf_gate_at_site(&ctx, &dps1, sig_state.instance_id).await.unwrap();
        for idx in 0..3 {
            suspicious_signal_hack_array(&ctx, &venture, sig_state.instance_id, idx).unwrap();
        }
        suspicious_signal_attack_relay(&ctx, &dps1, sig_state.instance_id, 350_000.0, 45).unwrap();
        let sig_pay = complete_homefront_operation_and_pay(&ctx, &dps1, sig_state.instance_id)
            .await
            .expect("complete Suspicious Signal");
        assert_eq!(sig_pay.total_fleet_isk, 15_360_000);

        // 7-9. Three-pilot operations (10713 Salvage Research, 10714 Stabilize Rift, 10705 Traffic Stop)
        // Solo (n=1 on opt=3): 12M * (4/5)^2 = 7,680,000
        let sal = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_713).unwrap();
        assert_eq!(sal.blueprint.optimal_fleet_size, 3);
        enter_hf_gate_at_site(&ctx, &dps2, sal.instance_id).await.unwrap(); // Coercer T1 dessy
        salvage_research_process_wreck(&ctx, &dps2, sal.instance_id, 30, true).unwrap();
        assert_eq!(complete_homefront_operation_and_pay(&ctx, &dps2, sal.instance_id).await.unwrap().total_fleet_isk, 7_680_000);

        let rift = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_714).unwrap();
        enter_hf_gate_at_site(&ctx, &dps2, rift.instance_id).await.unwrap();
        stabilize_rift_neutralize_arrays(&ctx, &dps2, rift.instance_id, [0.9; 5], 60).unwrap();
        assert_eq!(complete_homefront_operation_and_pay(&ctx, &dps2, rift.instance_id).await.unwrap().total_fleet_isk, 7_680_000);

        let traf = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_705).unwrap();
        enter_hf_gate_at_site(&ctx, &logi, traf.instance_id).await.unwrap();
        traffic_stop_scan_and_intercept(&ctx, &logi, traf.instance_id, 6, 1, 6_000.0).unwrap();
        assert_eq!(
            complete_homefront_operation_and_pay(&ctx, &logi, traf.instance_id)
                .await
                .expect_err("Traffic Stop must require 9-minute (540s) survival timer")
                .code,
            "ObjectivesIncomplete"
        );
        ctx.db.with_mut_snapshot(|snap| {
            if let HomefrontProgressRecord::TrafficStop {
                survival_elapsed_secs,
                required_survival_secs,
                ..
            } = &mut snap.dungeon_runtime.homefront_runtimes.get_mut(&traf.instance_id).unwrap().progress
            {
                *survival_elapsed_secs = *required_survival_secs;
            }
        });
        assert_eq!(complete_homefront_operation_and_pay(&ctx, &logi, traf.instance_id).await.unwrap().total_fleet_isk, 7_680_000);

        // Optimal 3-pilot Traffic Stop (n=3 on opt=3) -> full 36,000,000 ISK pool after 9-min timer
        for sid in [810_002_i64, 810_003, 810_004] {
            let _ = sys_h.try_update_combat_state(sid, |cs| {
                cs.shield_hp = 5_000_000.0;
                cs.max_shield_hp = 5_000_000.0;
                cs.armor_hp = 5_000_000.0;
                cs.max_armor_hp = 5_000_000.0;
                cs.structure_hp = 5_000_000.0;
                cs.max_structure_hp = 5_000_000.0;
            });
        }
        let traf3 = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_705).unwrap();
        for s in [&dps2, &logi, &venture] {
            enter_hf_gate_at_site(&ctx, s, traf3.instance_id).await.unwrap();
            record_homefront_combat_and_logistics(&ctx, s, traf3.instance_id, 2_000.0, 0.0).unwrap();
        }
        traffic_stop_scan_and_intercept(&ctx, &logi, traf3.instance_id, 6, 0, 2_000.0).unwrap();
        sys_h.step_manual(539.0).await.unwrap();
        ctx.advance_secs(539);
        tick_homefront_operations_sync(&ctx);
        assert!(!get_homefront_runtime_record(&ctx, traf3.instance_id).unwrap().completed);
        sys_h.step_manual(1.0).await.unwrap();
        ctx.advance_secs(1);
        tick_homefront_operations_sync(&ctx);
        assert!(get_homefront_runtime_record(&ctx, traf3.instance_id).unwrap().completed);

        // Traffic Stop live smuggler escape & warp scramble prevention:
        // Scrambled smugglers stay pinned across checkpoints; unscrambled smugglers escape & fail at 5.
        let traf_esc = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_705).unwrap();
        enter_hf_gate_at_site(&ctx, &logi, traf_esc.instance_id).await.unwrap();
        let first_smuggler = match &get_homefront_runtime_record(&ctx, traf_esc.instance_id).unwrap().progress {
            HomefrontProgressRecord::TrafficStop { smuggler_ball_ids, .. } => smuggler_ball_ids[0],
            other => panic!("expected TrafficStop, got {other:?}"),
        };
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        sys_h.set_warp_scramble_strength(first_smuggler, 2).await.unwrap();
        for dt_step in [90_u64, 75, 75, 75, 75] {
            sys_h.step_manual(dt_step as f64).await.unwrap();
            ctx.advance_secs(dt_step);
            tick_homefront_operations_sync(&ctx);
        }
        let esc_rec = get_homefront_runtime_record(&ctx, traf_esc.instance_id).unwrap();
        assert!(esc_rec.failed, "5 unscrambled smuggler escapes must fail Traffic Stop");
        match &esc_rec.progress {
            HomefrontProgressRecord::TrafficStop { smugglers_escaped, smuggler_ball_ids, .. } => {
                assert_eq!(*smugglers_escaped, 5);
                assert!(smuggler_ball_ids.contains(&first_smuggler), "scrambled smuggler must remain pinned while others escaped");
            }
            other => panic!("expected TrafficStop, got {other:?}"),
        }
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
            enter_hf_gate_at_site(&ctx, &s, raid.instance_id).await.unwrap();
            contributors.push(s);
        }
        // Pilot 13 enters gate but remains idle (zero contribution -> leecher exclusion)
        let leecher = seed_hf_pilot(&ctx, 7299, 920_099, 820_099, 597, sys_id, &[]).await;
        enter_hf_gate_at_site(&ctx, &leecher, raid.instance_id).await.unwrap();

        for _ in 0..4 {
            raid_tackle_and_destroy_hauler(&ctx, &contributors[1], raid.instance_id, true, 3_000.0).unwrap();
            raid_deliver_cargo_to_freighter(&ctx, &contributors[0], raid.instance_id).unwrap();
        }
        for c in &contributors[2..] {
            record_homefront_combat_and_logistics(&ctx, c, raid.instance_id, 1_500.0, 0.0).unwrap();
        }

        // Concurrent completion calls (TOCTOU guard): only ONE succeeds, zero double-payout.
        let (res_a, res_b) = tokio::join!(
            complete_homefront_operation_and_pay(&ctx, &contributors[0], raid.instance_id),
            complete_homefront_operation_and_pay(&ctx, &contributors[1], raid.instance_id),
        );
        let (summary, dup_err) = match (res_a, res_b) {
            (Ok(s), Err(e)) | (Err(e), Ok(s)) => (s, e),
            other => panic!("expected exactly one Ok and one Err on concurrent settlement, got {other:?}"),
        };
        assert_eq!(dup_err.code, "SiteAlreadyCompleted");
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

    #[tokio::test]
    async fn homefront_sqlite_reload_mid_operation_and_isolated_server_contexts() {
        let sde = super::shared_sde();
        let sys_id: SolarSystemId = 30_000_142;
        let tmp_db = std::env::temp_dir().join(format!(
            "evomu_hf_reload_{}_{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&tmp_db);

        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("open sqlite repo1"));
        repo1.with_mut_snapshot(|snap| {
            let mut ch = template_ch.clone();
            ch.character_id = 90_000_001;
            snap.characters.insert(90_000_001, ch);
        });
        let ctx1 = ServerContext::with_sde_and_repository(sde.clone(), repo1.clone());
        ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

        let tayra = seed_hf_pilot(&ctx1, 7501, 950_001, 850_001, 649, sys_id, &[]).await;
        let raid = spawn_homefront_site_for_dungeon(&ctx1, sys_id, 10_347).unwrap();
        enter_hf_gate_at_site(&ctx1, &tayra, raid.instance_id).await.unwrap();
        for _ in 0..2 {
            raid_tackle_and_destroy_hauler(&ctx1, &tayra, raid.instance_id, true, 3_000.0).unwrap();
            raid_deliver_cargo_to_freighter(&ctx1, &tayra, raid.instance_id).unwrap();
        }

        // Completely separate ServerContext must NOT share any static OnceLock state
        let isolated_ctx = fresh_homefront_ctx();
        assert!(
            get_homefront_runtime_state(&isolated_ctx, raid.instance_id).is_none(),
            "unrelated ServerContext must not see ctx1 Homefront state"
        );
        drop(ctx1);
        drop(repo1);

        // Reopen SQLite into fresh ServerContext mid-operation and finish remaining 2 deliveries
        let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo2"));
        let ctx2 = ServerContext::with_sde_and_repository(sde, repo2);
        ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_120)));

        let reloaded = get_homefront_runtime_state(&ctx2, raid.instance_id)
            .expect("SQLite reload must preserve active HomefrontRuntimeRecord");
        assert!(reloaded.started);
        assert!(matches!(
            reloaded.progress,
            HomefrontArchetypeProgress::Raid { cargo_delivered: 2, required_deliveries: 4, .. }
        ));
        for _ in 0..2 {
            raid_tackle_and_destroy_hauler(&ctx2, &tayra, raid.instance_id, true, 3_000.0).unwrap();
            raid_deliver_cargo_to_freighter(&ctx2, &tayra, raid.instance_id).unwrap();
        }
        let settled = complete_homefront_operation_and_pay(&ctx2, &tayra, raid.instance_id)
            .await
            .expect("complete Raid after SQLite reload");
        // (4/5)^4 * 15,000,000 = 6,144,000
        assert_eq!(settled.total_fleet_isk, 6_144_000);
        let _ = std::fs::remove_file(&tmp_db);
    }

    #[tokio::test]
    async fn homefront_aar_skipped_wave_recovery_and_evacuation_window() {
        use crate::services::pve_homefront_sites::{
            get_homefront_runtime_state, tick_homefront_operations_sync,
        };
        use eve_db::HomefrontProgressRecord;

        let ctx = fresh_homefront_ctx();
        let sys_id = 30_000_142_u32;
        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let pilot = seed_hf_pilot(&ctx, 7601, 960_001, 860_001, 32_880, sys_id, &[483]).await;
        let _ = sys_h.try_update_combat_state(860_001, |cs| {
            cs.shield_hp = 5_000_000.0;
            cs.max_shield_hp = 5_000_000.0;
            cs.armor_hp = 5_000_000.0;
            cs.max_armor_hp = 5_000_000.0;
            cs.structure_hp = 5_000_000.0;
            cs.max_structure_hp = 5_000_000.0;
        });

        // Part A: 60s AAR evacuation window counts down after setup_remaining_secs reaches 0,
        // then spawns Basileus (77_702) and Abyssal Artifact Cache (77_727) into SystemHandle.
        let aar1 = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn AAR #1");
        enter_hf_gate_at_site(&ctx, &pilot, aar1.instance_id)
            .await
            .expect("enter AAR #1");
        sys_h.step_manual(90.0).await.unwrap();
        ctx.advance_secs(90);
        tick_homefront_operations_sync(&ctx);
        for _ in 1..=9_u8 {
            abyssal_clear_wave(&ctx, &pilot, aar1.instance_id, 324, 2_500.0, true)
                .await
                .expect("clear AAR wave");
        }
        ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&aar1.instance_id];
            match &rec.progress {
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    evacuation_active,
                    extraction_window_secs,
                    basileus_spawned,
                    waves_paid_mask,
                    ..
                } => {
                    assert!(*evacuation_active);
                    assert_eq!(*extraction_window_secs, 60);
                    assert!(!*basileus_spawned);
                    assert_eq!(*waves_paid_mask, 0x01FF);
                }
                other => panic!("expected AAR record, got {other:?}"),
            }
        });
        sys_h.step_manual(30.0).await.unwrap();
        ctx.advance_secs(30);
        tick_homefront_operations_sync(&ctx);
        assert!(matches!(
            get_homefront_runtime_state(&ctx, aar1.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::AbyssalArtifactRecovery {
                extraction_window_secs: 30,
                ..
            }
        ));
        sys_h.step_manual(30.0).await.unwrap();
        ctx.advance_secs(30);
        tick_homefront_operations_sync(&ctx);
        let (basileus_id_opt, cache_id_opt) = ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&aar1.instance_id];
            match &rec.progress {
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    basileus_spawned,
                    cache_ball_id,
                    extraction_window_secs,
                    ..
                } => {
                    assert!(*basileus_spawned);
                    assert_eq!(*extraction_window_secs, 0);
                    (
                        rec.spawned_ball_ids.first().copied(),
                        *cache_ball_id,
                    )
                }
                other => panic!("expected AAR record, got {other:?}"),
            }
        });
        assert!(basileus_id_opt.is_some());
        let cache_id = cache_id_opt.expect("Abyssal Cache ball ID must be recorded");
        let cache_ball = sys_h
            .get_ball_snapshot(cache_id)
            .await
            .unwrap()
            .expect("Abyssal Cache ball must exist in SystemHandle")
            .1;
        assert_eq!(cache_ball.type_id, 77_727);
        let has_basileus = {
            let view = sys_h.view();
            view.balls.values().any(|b| b.type_id == 77_702)
        };
        assert!(has_basileus, "Basileus (77_702) must spawn when extraction window expires");
        let pay1 = complete_homefront_operation_and_pay(&ctx, &pilot, aar1.instance_id)
            .await
            .expect("complete AAR #1");
        assert_eq!(pay1.total_fleet_isk, 27_648_000);

        // Part B: Wave timeout with unmined Mutanite must FAIL immediately (both Wave 2 and Wave 9)
        // and completion settlement must NEVER pay retroactive skipped-wave ISK (`(0, 1)`).
        let pilot2 = seed_hf_pilot(&ctx, 7602, 960_002, 860_002, 32_880, sys_id, &[483]).await;
        let _ = sys_h.try_update_combat_state(860_002, |cs| {
            cs.shield_hp = 5_000_000.0;
            cs.max_shield_hp = 5_000_000.0;
            cs.armor_hp = 5_000_000.0;
            cs.max_armor_hp = 5_000_000.0;
            cs.structure_hp = 5_000_000.0;
            cs.max_structure_hp = 5_000_000.0;
        });
        let aar2 = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn AAR #2");
        enter_hf_gate_at_site(&ctx, &pilot2, aar2.instance_id)
            .await
            .expect("enter AAR #2");
        abyssal_clear_wave(&ctx, &pilot2, aar2.instance_id, 324, 2_500.0, true)
            .await
            .expect("wave 1 pays bit 0 (2,048,000 ISK)");
        let (setup2, wave2_limit) = ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&aar2.instance_id];
            match &rec.progress {
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    current_wave,
                    setup_remaining_secs,
                    wave_time_remaining_secs,
                    mutanite_units_remaining,
                    ..
                } => {
                    assert_eq!(*current_wave, 2);
                    assert!(*mutanite_units_remaining > 0);
                    (*setup_remaining_secs, *wave_time_remaining_secs)
                }
                other => panic!("expected AAR record, got {other:?}"),
            }
        });
        let _ = ctx.drain_notifications_for_char(960_002);
        if setup2 > 0 {
            sys_h.step_manual(setup2 as f64).await.unwrap();
            ctx.advance_secs(setup2);
            tick_homefront_operations_sync(&ctx);
        }
        sys_h.step_manual(wave2_limit as f64).await.unwrap();
        ctx.advance_secs(wave2_limit);
        tick_homefront_operations_sync(&ctx);
        let aar2_after = get_homefront_runtime_record(&ctx, aar2.instance_id).unwrap();
        assert!(aar2_after.failed, "AAR Wave 2 timeout with unmined Mutanite must fail immediately");
        assert!(!aar2_after.completed);
        assert!(
            aar2_after
                .failure_reason
                .as_deref()
                .is_some_and(|r| r.contains("wave 2 timed out")),
            "unexpected failure reason: {:?}",
            aar2_after.failure_reason
        );
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.characters[&960_002].balance,
                IskCents::from_isk(3_048_000),
                "timed-out AAR must retain only Wave 1 live payout (2,048,000) and pay zero extra"
            );
        });

        // Wave 9 timeout must ALSO fail immediately, and final completion pays 0 retroactive ISK.
        let pilot3 = seed_hf_pilot(&ctx, 7603, 960_003, 860_003, 32_880, sys_id, &[483]).await;
        let aar3 = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn AAR #3");
        enter_hf_gate_at_site(&ctx, &pilot3, aar3.instance_id)
            .await
            .expect("enter AAR #3");
        for _ in 1..=8_u8 {
            abyssal_clear_wave(&ctx, &pilot3, aar3.instance_id, 324, 2_500.0, true)
                .await
                .expect("waves 1..8 cleared live");
        }
        let (setup9, wave9_limit) = ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&aar3.instance_id];
            match &rec.progress {
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    current_wave,
                    setup_remaining_secs,
                    wave_time_remaining_secs,
                    mutanite_units_remaining,
                    ..
                } => {
                    assert_eq!(*current_wave, 9);
                    assert!(*mutanite_units_remaining > 0);
                    (*setup_remaining_secs, *wave_time_remaining_secs)
                }
                other => panic!("expected AAR record, got {other:?}"),
            }
        });
        if setup9 > 0 {
            sys_h.step_manual(setup9 as f64).await.unwrap();
            ctx.advance_secs(setup9);
            tick_homefront_operations_sync(&ctx);
        }
        sys_h.step_manual(wave9_limit as f64).await.unwrap();
        ctx.advance_secs(wave9_limit);
        tick_homefront_operations_sync(&ctx);
        let aar3_after = get_homefront_runtime_record(&ctx, aar3.instance_id).unwrap();
        assert!(aar3_after.failed, "AAR Wave 9 timeout with unmined Mutanite must fail immediately");
        assert!(!aar3_after.completed);
        assert!(
            aar3_after
                .failure_reason
                .as_deref()
                .is_some_and(|r| r.contains("wave 9 timed out")),
            "unexpected failure reason: {:?}",
            aar3_after.failure_reason
        );

        let pilot4 = seed_hf_pilot(&ctx, 7604, 960_004, 860_004, 32_880, sys_id, &[483]).await;
        let aar4 = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn AAR #4");
        enter_hf_gate_at_site(&ctx, &pilot4, aar4.instance_id)
            .await
            .expect("enter AAR #4");
        abyssal_clear_wave(&ctx, &pilot4, aar4.instance_id, 324, 2_500.0, true)
            .await
            .expect("wave 1 paid");
        ctx.db.with_mut_snapshot(|snap| {
            let rec = snap.dungeon_runtime.homefront_runtimes.get_mut(&aar4.instance_id).unwrap();
            if let HomefrontProgressRecord::AbyssalArtifactRecovery {
                current_wave,
                mutanite_units_remaining,
                wave_hostiles_remaining,
                warp_bubble_active,
                evacuation_active,
                basileus_spawned,
                cache_ball_id,
                ..
            } = &mut rec.progress
            {
                *current_wave = 9;
                *mutanite_units_remaining = 0;
                *wave_hostiles_remaining = 0;
                *warp_bubble_active = false;
                *evacuation_active = true;
                *basileus_spawned = true;
                *cache_ball_id = None;
            }
        });
        let _ = ctx.drain_notifications_for_char(960_004);
        let _pay4 = complete_homefront_operation_and_pay(&ctx, &pilot4, aar4.instance_id)
            .await
            .expect("AAR completion settles");
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.characters[&960_004].balance,
                IskCents::from_isk(3_048_000),
                "AAR completion must NEVER pay retroactive ISK for skipped/unmined waves"
            );
        });
    }

    #[tokio::test]
    async fn homefront_r14_tidi_sim_clock_and_cold_system_timer_behaviour() {
        use crate::services::pve_homefront_sites::{
            get_homefront_runtime_record, tick_homefront_operations_sync,
        };
        use crate::services::serverops_cluster::set_system_tidi_factor;
        use eve_db::{HomefrontBlueprintRecord, HomefrontProgressRecord, HomefrontRuntimeRecord};

        let ctx = fresh_homefront_ctx();
        let sys_id: SolarSystemId = 30_000_142;

        // 1. Cold system (`get_handle(sys_id).is_none()`): ticks must not wake the system or advance `elapsed_sim_secs`.
        let cold_inst_id = 990_001_i64;
        let now0 = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.dungeon_runtime.homefront_runtimes.insert(
                cold_inst_id,
                HomefrontRuntimeRecord {
                    blueprint: HomefrontBlueprintRecord {
                        dungeon_id: 10_347,
                        dungeon_name: "ColdRaid".into(),
                        time_limit_secs: 900,
                        ..Default::default()
                    },
                    solar_system_id: sys_id,
                    instance_id: cold_inst_id,
                    started: true,
                    started_at_sec: now0,
                    last_tick_sec: now0,
                    expires_at_sec: now0 + 900,
                    sim_clock_initialized: true,
                    progress: HomefrontProgressRecord::default(),
                    ..Default::default()
                },
            );
        });
        assert!(ctx.systems.get_handle(sys_id).is_none());
        for _ in 0..3 {
            ctx.advance_secs(500);
            tick_homefront_operations_sync(&ctx);
            assert!(
                ctx.systems.get_handle(sys_id).is_none(),
                "multiple cold Homefront ticks across >time_limit_secs wall advance must never get_or_spawn a cold solar system"
            );
            let cold_after = get_homefront_runtime_record(&ctx, cold_inst_id).unwrap();
            assert_eq!(cold_after.elapsed_sim_secs, 0.0);
            assert!(!cold_after.failed);
            assert!(cold_after.sim_clock_initialized);
        }

        // Cold expiration despawn must fail cleanly without waking the cold solar system (`despawn_homefront_balls_db`).
        ctx.db.with_mut_snapshot(|snap| {
            let rec = snap
                .dungeon_runtime
                .homefront_runtimes
                .get_mut(&cold_inst_id)
                .unwrap();
            rec.elapsed_sim_secs = rec.blueprint.time_limit_secs as f64;
            rec.spawned_ball_ids.push(99_990_001);
        });
        tick_homefront_operations_sync(&ctx);
        assert!(
            ctx.systems.get_handle(sys_id).is_none(),
            "despawn_homefront_balls_db must never wake a cold solar system"
        );
        let cold_expired = get_homefront_runtime_record(&ctx, cold_inst_id).unwrap();
        assert!(cold_expired.failed);

        // 2. Live site at TiDi = 0.25: 100 wall seconds advance `elapsed_sim_secs` by ~25s, then expire cleanly on remaining sim time.
        let pilot = seed_hf_pilot(&ctx, 7701, 970_101, 870_101, 649, sys_id, &[]).await;
        let raid = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_347).unwrap();
        enter_hf_gate_at_site(&ctx, &pilot, raid.instance_id)
            .await
            .expect("enter Raid site");
        let sys_h = ctx.systems.get_handle(sys_id).expect("awake system after gate entry");
        let applied = set_system_tidi_factor(&ctx, sys_id, 0.25);
        assert!((applied - 0.25).abs() < 1e-9);

        for _ in 0..100 {
            sys_h.step_manual(1.0).await.unwrap();
            ctx.advance_secs(1);
            tick_homefront_operations_sync(&ctx);
        }

        let mid = get_homefront_runtime_record(&ctx, raid.instance_id).unwrap();
        assert!(
            (mid.elapsed_sim_secs - 25.0).abs() < 1e-6,
            "100 wall ticks at TiDi=0.25 must advance elapsed_sim_secs by ~25.0s, got {}",
            mid.elapsed_sim_secs
        );
        assert!(!mid.failed);
        let expected_rem = mid.blueprint.time_limit_secs.saturating_sub(25);
        assert_eq!(mid.expires_at_sec, ctx.now_unix() + expected_rem);
        match &mid.progress {
            HomefrontProgressRecord::Raid { haulers_escaped, .. } => {
                assert_eq!(
                    *haulers_escaped, 0,
                    "Raid 90s sim escape timer must not fire at 25s sim elapsed (100s wall)"
                );
            }
            other => panic!("expected Raid progress, got {other:?}"),
        }

        // Advance remaining sim time up to 1s before limit -> still active; final 4 wall secs (+1s sim) -> expires cleanly.
        let rem_sim_before_last = (mid.blueprint.time_limit_secs as f64) - mid.elapsed_sim_secs - 1.0;
        let wall_before_last = rem_sim_before_last * 4.0;
        sys_h.step_manual(wall_before_last).await.unwrap();
        ctx.advance_secs(wall_before_last.round() as u64);
        tick_homefront_operations_sync(&ctx);
        assert!(
            !get_homefront_runtime_record(&ctx, raid.instance_id)
                .unwrap()
                .failed,
            "Homefront site must remain active with 1 sim second remaining"
        );

        sys_h.step_manual(4.0).await.unwrap();
        ctx.advance_secs(4);
        tick_homefront_operations_sync(&ctx);
        let expired = get_homefront_runtime_record(&ctx, raid.instance_id).unwrap();
        assert!(
            expired.failed,
            "Homefront site must expire cleanly once elapsed_sim_secs reaches time_limit_secs"
        );
        assert!(
            expired.elapsed_sim_secs + 1e-9 >= expired.blueprint.time_limit_secs as f64,
            "elapsed_sim_secs ({}) must reach time_limit_secs ({})",
            expired.elapsed_sim_secs,
            expired.blueprint.time_limit_secs,
        );
        assert_eq!(
            record_homefront_combat_and_logistics(&ctx, &pilot, raid.instance_id, 500.0, 0.0)
                .unwrap_err()
                .code,
            "OperationTimedOut"
        );
    }

    #[tokio::test]
    async fn homefront_rt36_sqlite_restart_preserves_progress_sim_clock_and_rebuilds_pocket() {
        use crate::services::pve_homefront_sites::{
            ensure_homefront_outer_entries_sync, get_homefront_runtime_record,
            on_homefront_entity_destroyed_core, tick_homefront_operations_sync,
        };
        use eve_db::HomefrontProgressRecord;

        let sde = super::shared_sde();
        let sys_id: SolarSystemId = 30_000_142;
        let tmp_db = std::env::temp_dir().join(format!(
            "evomu_hf_rt36_{}_{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&tmp_db);

        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("open sqlite repo1"));
        repo1.with_mut_snapshot(|snap| {
            let mut ch = template_ch.clone();
            ch.character_id = 90_000_001;
            snap.characters.insert(90_000_001, ch);
        });
        let ctx1 = ServerContext::with_sde_and_repository(sde.clone(), repo1.clone());
        ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

        let pilot_id = 950_901_i64;
        let tayra = seed_hf_pilot(&ctx1, 7702, pilot_id, 850_901, 649, sys_id, &[]).await;
        let raid = spawn_homefront_site_for_dungeon(&ctx1, sys_id, 10_347).unwrap();
        enter_hf_gate_at_site(&ctx1, &tayra, raid.instance_id)
            .await
            .expect("enter Raid site before restart");
        record_homefront_combat_and_logistics(&ctx1, &tayra, raid.instance_id, 7_500.0, 0.0)
            .expect("record partial damage");

        let sys_h1 = ctx1.systems.get_or_spawn(sys_id);
        sys_h1.step_manual(120.0).await.unwrap();
        ctx1.advance_secs(120);
        tick_homefront_operations_sync(&ctx1);

        let h_live = match &get_homefront_runtime_record(&ctx1, raid.instance_id)
            .unwrap()
            .progress
        {
            HomefrontProgressRecord::Raid {
                active_hauler_ball_id: Some(hid),
                ..
            } => *hid,
            other => panic!("expected active Raid hauler, got {other:?}"),
        };
        sys_h1.set_warp_scramble_strength(h_live, 3).await.unwrap();
        let _ = on_homefront_entity_destroyed_core(
            &ctx1.systems,
            &ctx1.db,
            &ctx1.sde,
            sys_id,
            h_live,
            Some(pilot_id),
        )
        .await;

        let pre_restart = get_homefront_runtime_record(&ctx1, raid.instance_id).unwrap();
        assert!((pre_restart.elapsed_sim_secs - 120.0).abs() < 1e-6);
        let time_limit = pre_restart.blueprint.time_limit_secs;
        let (pre_freighter, pre_depot, pre_hauler2, pre_crate) = match &pre_restart.progress {
            HomefrontProgressRecord::Raid {
                freighter_ball_id,
                delivery_depot_ball_id,
                active_hauler_ball_id: Some(h2),
                dropped_crate_ball_ids,
                haulers_destroyed,
                cargo_dropped,
                ..
            } => {
                assert_eq!(*haulers_destroyed, 1);
                assert_eq!(*cargo_dropped, 1);
                (
                    *freighter_ball_id,
                    *delivery_depot_ball_id,
                    *h2,
                    dropped_crate_ball_ids[0],
                )
            }
            other => panic!("expected Raid progress pre-restart, got {other:?}"),
        };
        let pre_damage = pre_restart.contributions[&pilot_id].damage_dealt;

        drop(sys_h1);
        drop(ctx1);
        drop(repo1);

        // Simulate server downtime of +3,600s (> time_limit_secs) and reopen SQLite into fresh context.
        let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo2"));
        let ctx2 = ServerContext::with_sde_and_repository(sde, repo2);
        ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
            1_775_400_120 + 3_600.max(time_limit + 600),
        )));

        // Cold ticks must neither fail the site nor advance `elapsed_sim_secs`.
        tick_homefront_operations_sync(&ctx2);
        assert!(ctx2.systems.get_handle(sys_id).is_none());
        let cold_reopened = get_homefront_runtime_record(&ctx2, raid.instance_id).unwrap();
        assert!(!cold_reopened.failed);
        assert!((cold_reopened.elapsed_sim_secs - 120.0).abs() < 1e-6);

        // Wake the system and run outer-entry + tick reconciliation.
        let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
        ensure_homefront_outer_entries_sync(&ctx2, Some(sys_id), Some(raid.instance_id));
        tick_homefront_operations_sync(&ctx2);

        let post_wake = get_homefront_runtime_record(&ctx2, raid.instance_id).unwrap();
        assert!(
            !post_wake.failed,
            "RT-36 wake after >time_limit_secs wall downtime must not fail active Homefront site"
        );
        assert!(
            (post_wake.elapsed_sim_secs - 120.0).abs() < 1e-6,
            "RT-36 must preserve elapsed_sim_secs across restart (got {})",
            post_wake.elapsed_sim_secs
        );
        assert_eq!(post_wake.last_sim_elapsed_secs, Some(0.0));
        assert!(post_wake.entered_pilots.contains_key(&pilot_id));
        assert_eq!(post_wake.contributions[&pilot_id].damage_dealt, pre_damage);
        match &post_wake.progress {
            HomefrontProgressRecord::Raid {
                haulers_destroyed,
                cargo_dropped,
                ..
            } => {
                assert_eq!(*haulers_destroyed, 1);
                assert_eq!(*cargo_dropped, 1);
            }
            other => panic!("expected Raid progress after wake, got {other:?}"),
        }

        assert!(
            ctx2.db.with_snapshot(|s| s.items.contains_key(&pre_crate)),
            "RT-36 persisted crate {pre_crate} must remain in snap.items after SQLite restart (dropped_crate_ball_ids={:?})",
            match &post_wake.progress {
                HomefrontProgressRecord::Raid { dropped_crate_ball_ids, .. } => dropped_crate_ball_ids.clone(),
                _ => Vec::new(),
            }
        );
        let view2 = sys_h2.view();
        for expected_ball in [
            post_wake.outer_beacon_ball_id,
            post_wake.outer_gate_ball_id,
            pre_freighter,
            pre_depot,
            pre_hauler2,
            pre_crate,
        ] {
            assert!(
                view2.balls.contains_key(&expected_ball),
                "RT-36 wake must rebuild missing Homefront ball {expected_ball}"
            );
        }
        let depot_cs = view2
            .combat_states
            .get(&pre_depot)
            .expect("reconciled Raid delivery depot must have CombatState");
        assert!(
            depot_cs.is_tethered,
            "RT-36 wake must preserve Raid delivery depot is_tethered invulnerability"
        );
        assert!(
            (depot_cs.structure_hp - 5_000.0).abs() < 1e-6,
            "RT-36 wake must preserve Raid delivery depot 5,000 HP (got {})",
            depot_cs.structure_hp
        );
        assert!(
            ctx2.systems.is_special_site_ball_spawned(sys_id, pre_depot),
            "RT-36 wake must claim Raid delivery depot as special-site ball"
        );
        assert!(
            ctx2.systems.is_special_site_ball_spawned(sys_id, pre_crate),
            "RT-36 wake must claim persisted Raid crate as special-site ball"
        );

        // BF-HF-DECAY-01: Live clock advance + background cluster tick after >=3,600s restart must
        // never sweep active Homefront Raid depot, dropped crate, or crate mission cargo.
        ctx2.advance_secs(1);
        crate::sp6_lifecycle::tick_background_cluster(&ctx2).await;
        ctx2.db.with_snapshot(|s| {
            assert!(
                s.items.contains_key(&pre_depot),
                "Raid delivery depot {pre_depot} must survive live background tick after >=3,600s restart"
            );
            assert!(
                s.items.contains_key(&pre_crate),
                "Dropped Raid crate {pre_crate} must survive live background tick after >=3,600s restart"
            );
            assert!(
                s.items.values().any(|it| {
                    it.location_id == pre_crate
                        && it.type_id == HOMEFRONT_RAID_CRATE_TYPE_ID
                        && it.quantity > 0
                }),
                "Mission cargo inside dropped Raid crate {pre_crate} must survive background tick"
            );
        });
        let view_post_bg = sys_h2.view();
        assert!(view_post_bg.balls.contains_key(&pre_depot));
        assert!(view_post_bg.balls.contains_key(&pre_crate));

        drop(sys_h2);
        drop(ctx2);
        let _ = std::fs::remove_file(&tmp_db);
    }

    #[tokio::test]
    async fn homefront_gate_activation_timeout_commits_failure_and_despawns_balls() {
        use crate::services::pve_homefront_sites::get_homefront_runtime_record;

        let ctx = fresh_homefront_ctx();
        let sys_id: SolarSystemId = 30_000_142;
        let pilot1 = seed_hf_pilot(&ctx, 7711, 970_201, 870_201, 649, sys_id, &[]).await;
        let pilot2 = seed_hf_pilot(&ctx, 7712, 970_202, 870_202, 621, sys_id, &[]).await;
        let raid = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_347).unwrap();

        enter_hf_gate_at_site(&ctx, &pilot1, raid.instance_id)
            .await
            .expect("first pilot enters site before timeout");
        let pre_timeout = get_homefront_runtime_record(&ctx, raid.instance_id).unwrap();
        assert!(!pre_timeout.spawned_ball_ids.is_empty());

        let sys_h = ctx.systems.get_handle(sys_id).expect("awake system");
        let past_limit = pre_timeout.blueprint.time_limit_secs as f64 + 10.0;
        sys_h.step_manual(past_limit).await.unwrap();
        assert!(
            !get_homefront_runtime_record(&ctx, raid.instance_id).unwrap().failed,
            "precondition: site must still be un-failed before gate activation"
        );

        // Second pilot triggers timeout check directly inside activate_homefront_acceleration_gate
        // without running tick_homefront_operations_sync first.
        let gate_err = enter_hf_gate_at_site(&ctx, &pilot2, raid.instance_id)
            .await
            .expect_err("expired gate activation must return OperationTimedOut");
        assert_eq!(gate_err.code, "OperationTimedOut");

        let post_timeout = get_homefront_runtime_record(&ctx, raid.instance_id).unwrap();
        assert!(
            post_timeout.failed,
            "OperationTimedOut from activate_homefront_acceleration_gate must commit rec.failed = true (not roll back)"
        );
        assert!(post_timeout.failure_reason.is_some());
        assert!(
            (post_timeout.elapsed_sim_secs - pre_timeout.blueprint.time_limit_secs as f64).abs()
                < 1e-6,
            "elapsed_sim_secs must commit at time_limit_secs ({}), got {}",
            pre_timeout.blueprint.time_limit_secs,
            post_timeout.elapsed_sim_secs
        );

        let view = sys_h.view();
        for bid in pre_timeout
            .spawned_ball_ids
            .iter()
            .copied()
            .chain([pre_timeout.outer_beacon_ball_id, pre_timeout.outer_gate_ball_id])
        {
            assert!(
                !view.balls.contains_key(&bid),
                "timed-out gate rejection must despawn Homefront ball {bid}"
            );
        }
    }

    #[tokio::test]
    async fn homefront_live_reconciliation_preserves_elapsed_sim_time_on_non_restarted_system() {
        use crate::services::pve_homefront_sites::{
            get_homefront_runtime_record, raid_tackle_and_destroy_hauler,
            tick_homefront_operations_sync,
        };
        use eve_db::HomefrontProgressRecord;

        let ctx = fresh_homefront_ctx();
        let sys_id: SolarSystemId = 30_000_142;
        let pilot = seed_hf_pilot(&ctx, 7721, 970_301, 870_301, 649, sys_id, &[]).await;
        let raid = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_347).unwrap();
        enter_hf_gate_at_site(&ctx, &pilot, raid.instance_id)
            .await
            .expect("enter Raid site");

        let sys_h = ctx.systems.get_handle(sys_id).expect("awake system");
        sys_h.step_manual(20.0).await.unwrap();
        ctx.advance_secs(20);
        tick_homefront_operations_sync(&ctx);

        let mid = get_homefront_runtime_record(&ctx, raid.instance_id).unwrap();
        assert!((mid.elapsed_sim_secs - 20.0).abs() < 1e-6);
        assert_eq!(mid.last_sim_elapsed_secs, Some(20.0));

        // Step live sim clock without restarting, then allocate a replacement Raid hauler in DB
        // without spawning its ball so the next tick exercises live pocket reconciliation first.
        sys_h.step_manual(15.0).await.unwrap();
        raid_tackle_and_destroy_hauler(&ctx, &pilot, raid.instance_id, true, 1_000.0)
            .expect("raid_tackle_and_destroy_hauler advances active_hauler_ball_id");
        let new_hauler_id = match &get_homefront_runtime_record(&ctx, raid.instance_id)
            .unwrap()
            .progress
        {
            HomefrontProgressRecord::Raid {
                active_hauler_ball_id: Some(hid),
                ..
            } => *hid,
            other => panic!("expected replacement hauler id, got {other:?}"),
        };
        assert!(!sys_h.view().balls.contains_key(&new_hauler_id));

        tick_homefront_operations_sync(&ctx);

        let post = get_homefront_runtime_record(&ctx, raid.instance_id).unwrap();
        assert!(
            (post.elapsed_sim_secs - 35.0).abs() < 1e-6,
            "live non-restarted reconciliation must NOT erase elapsed sim delta (expected 35.0, got {})",
            post.elapsed_sim_secs
        );
        assert_eq!(post.last_sim_elapsed_secs, Some(35.0));
        assert!(
            sys_h.view().balls.contains_key(&new_hauler_id),
            "reconciliation must spawn the missing replacement Raid hauler ball"
        );
    }

    #[tokio::test]
    async fn homefront_rt36_rebuilds_aar_basileus_when_cache_destroyed_before_restart() {
        use crate::services::pve_homefront_sites::{
            ensure_homefront_outer_entries_sync, get_homefront_runtime_record,
            on_homefront_entity_destroyed_core, tick_homefront_operations_sync,
        };
        use eve_db::HomefrontProgressRecord;

        let sde = super::shared_sde();
        let sys_id: SolarSystemId = 30_000_142;
        let tmp_db = std::env::temp_dir().join(format!(
            "evomu_hf_rt36_basileus_{}_{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&tmp_db);

        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("open sqlite repo1"));
        repo1.with_mut_snapshot(|snap| {
            let mut ch = template_ch.clone();
            ch.character_id = 90_000_001;
            snap.characters.insert(90_000_001, ch);
        });
        let ctx1 = ServerContext::with_sde_and_repository(sde.clone(), repo1.clone());
        ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

        let pilot_id = 970_302_i64;
        let pilot = seed_hf_pilot(&ctx1, 7722, pilot_id, 870_302, 32_880, sys_id, &[483]).await;
        let sys_h1 = ctx1.systems.get_or_spawn(sys_id);
        let _ = sys_h1.try_update_combat_state(870_302, |cs| {
            cs.shield_hp = 5_000_000.0;
            cs.max_shield_hp = 5_000_000.0;
            cs.armor_hp = 5_000_000.0;
            cs.max_armor_hp = 5_000_000.0;
            cs.structure_hp = 5_000_000.0;
            cs.max_structure_hp = 5_000_000.0;
        });

        let aar = spawn_homefront_site_for_dungeon(&ctx1, sys_id, 10_346).unwrap();
        enter_hf_gate_at_site(&ctx1, &pilot, aar.instance_id)
            .await
            .expect("enter AAR site");
        sys_h1.step_manual(90.0).await.unwrap();
        ctx1.advance_secs(90);
        tick_homefront_operations_sync(&ctx1);
        for _ in 1..=9_u8 {
            abyssal_clear_wave(&ctx1, &pilot, aar.instance_id, 324, 2_500.0, true)
                .await
                .expect("clear AAR wave");
        }
        sys_h1.step_manual(60.0).await.unwrap();
        ctx1.advance_secs(60);
        tick_homefront_operations_sync(&ctx1);

        let (basileus_id, cache_id) = match &get_homefront_runtime_record(&ctx1, aar.instance_id)
            .unwrap()
            .progress
        {
            HomefrontProgressRecord::AbyssalArtifactRecovery {
                basileus_spawned: true,
                basileus_ball_id: Some(bid),
                cache_ball_id: Some(cid),
                ..
            } => (*bid, *cid),
            other => panic!("expected live AAR Basileus and Cache, got {other:?}"),
        };
        let _ = on_homefront_entity_destroyed_core(
            &ctx1.systems,
            &ctx1.db,
            &ctx1.sde,
            sys_id,
            cache_id,
            Some(pilot_id),
        )
        .await;
        let pre_restart = get_homefront_runtime_record(&ctx1, aar.instance_id).unwrap();
        assert!(!pre_restart.completed && !pre_restart.failed);
        let pre_elapsed = pre_restart.elapsed_sim_secs;

        drop(sys_h1);
        drop(ctx1);
        drop(repo1);

        let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo2"));
        let ctx2 = ServerContext::with_sde_and_repository(sde, repo2);
        ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_402_000)));
        // Verify stale actor-epoch cursor is cleared on SQLite load.
        assert_eq!(
            get_homefront_runtime_record(&ctx2, aar.instance_id)
                .unwrap()
                .last_sim_elapsed_secs,
            None
        );

        let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
        // Advance new actor sim clock slightly before Homefront ticks or reconciles.
        sys_h2.step_manual(5.0).await.unwrap();
        ensure_homefront_outer_entries_sync(&ctx2, Some(sys_id), Some(aar.instance_id));

        assert!(
            sys_h2.view().balls.contains_key(&basileus_id),
            "RT-36 must rebuild AAR Basileus {basileus_id} after restart even when Cache was destroyed"
        );
        tick_homefront_operations_sync(&ctx2);
        let post_wake = get_homefront_runtime_record(&ctx2, aar.instance_id).unwrap();
        assert!(
            (post_wake.elapsed_sim_secs - pre_elapsed).abs() < 1e-6,
            "re-anchoring against fresh post-restart actor epoch must preserve elapsed_sim_secs ({pre_elapsed}), got {}",
            post_wake.elapsed_sim_secs
        );
        assert_eq!(post_wake.last_sim_elapsed_secs, Some(5.0));

        drop(sys_h2);
        drop(ctx2);
        let _ = std::fs::remove_file(&tmp_db);
    }

    #[tokio::test]
    async fn homefront_rt36_prunes_dead_boss_and_escorts_across_restart() {
        use crate::services::pve_homefront_sites::{
            ensure_homefront_outer_entries_sync, get_homefront_runtime_record,
            on_homefront_entity_destroyed_core, tick_homefront_operations_sync,
        };

        let sde = super::shared_sde();
        let sys_id: SolarSystemId = 30_000_142;
        let tmp_db = std::env::temp_dir().join(format!(
            "evomu_hf_rt36_npcs_{}_{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&tmp_db);

        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("open sqlite repo1"));
        repo1.with_mut_snapshot(|snap| {
            let mut ch = template_ch.clone();
            ch.character_id = 90_000_001;
            snap.characters.insert(90_000_001, ch);
        });
        let ctx1 = ServerContext::with_sde_and_repository(sde.clone(), repo1.clone());
        ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

        let pilot_id = 970_303_i64;
        let pilot = seed_hf_pilot(&ctx1, 7723, pilot_id, 870_303, 649, sys_id, &[]).await;
        let raid = spawn_homefront_site_for_dungeon(&ctx1, sys_id, 10_347).unwrap();
        enter_hf_gate_at_site(&ctx1, &pilot, raid.instance_id)
            .await
            .expect("enter Raid site");

        let sys_h1 = ctx1.systems.get_handle(sys_id).expect("awake system");
        let view1 = sys_h1.view();
        let pre_rec = get_homefront_runtime_record(&ctx1, raid.instance_id).unwrap();
        let boss_id = pre_rec.dispatch_boss_ball_id;
        let base_obj = 1_897_800_000 + raid.instance_id.saturating_mul(100);
        let escorts: Vec<i64> = (20_i64..30)
            .map(|o| base_obj + o)
            .filter(|id| view1.balls.contains_key(id))
            .collect();
        assert!(view1.balls.contains_key(&boss_id));
        assert!(!escorts.is_empty());
        let dead_escort = escorts[0];

        let _ = on_homefront_entity_destroyed_core(
            &ctx1.systems,
            &ctx1.db,
            &ctx1.sde,
            sys_id,
            boss_id,
            Some(pilot_id),
        )
        .await;
        let _ = on_homefront_entity_destroyed_core(
            &ctx1.systems,
            &ctx1.db,
            &ctx1.sde,
            sys_id,
            dead_escort,
            Some(pilot_id),
        )
        .await;
        let post_kill = get_homefront_runtime_record(&ctx1, raid.instance_id).unwrap();
        assert!(!post_kill.spawned_ball_ids.contains(&boss_id));
        assert!(!post_kill.spawned_ball_ids.contains(&dead_escort));

        drop(sys_h1);
        drop(ctx1);
        drop(repo1);

        let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo2"));
        let ctx2 = ServerContext::with_sde_and_repository(sde, repo2);
        ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_500)));

        let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
        ensure_homefront_outer_entries_sync(&ctx2, Some(sys_id), Some(raid.instance_id));
        tick_homefront_operations_sync(&ctx2);

        let view2 = sys_h2.view();
        assert!(
            !view2.balls.contains_key(&boss_id),
            "RT-36 must NOT resurrect killed dispatch commander {boss_id}"
        );
        assert!(
            !view2.balls.contains_key(&dead_escort),
            "RT-36 must NOT resurrect killed escort NPC {dead_escort}"
        );
        for &surviving_escort in escorts.iter().skip(1) {
            assert!(
                view2.balls.contains_key(&surviving_escort),
                "RT-36 must rebuild surviving escort NPC {surviving_escort}"
            );
        }

        drop(sys_h2);
        drop(ctx2);
        let _ = std::fs::remove_file(&tmp_db);
    }

    #[tokio::test]
    async fn homefront_rt36_salvage_wrecks_survive_7200s_background_decay() {
        use crate::services::pve_homefront_sites::get_homefront_runtime_record;
        use eve_db::HomefrontProgressRecord;

        let ctx = fresh_homefront_ctx();
        let sys_id: SolarSystemId = 30_000_142;
        let coercer =
            seed_hf_pilot(&ctx, 7724, 970_304, 870_304, 16_236, sys_id, &[25_861, 22_175, 533])
                .await;
        let sal = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_713).unwrap();
        enter_hf_gate_at_site(&ctx, &coercer, sal.instance_id)
            .await
            .expect("enter SalvageResearch site");

        let wreck_ids = match &get_homefront_runtime_record(&ctx, sal.instance_id)
            .unwrap()
            .progress
        {
            HomefrontProgressRecord::SalvageResearch { wreck_ball_ids, .. } => {
                wreck_ball_ids.clone()
            }
            other => panic!("expected SalvageResearch progress, got {other:?}"),
        };
        assert!(!wreck_ids.is_empty());

        // Advance >=7,200s wall time (WRECK_DECAY_LIFETIME_SECS) and run background cluster tick;
        // without sim-clock expiry, all active Homefront salvage wrecks must remain intact.
        ctx.advance_secs(7_500);
        crate::sp6_lifecycle::tick_background_cluster(&ctx).await;

        let post = get_homefront_runtime_record(&ctx, sal.instance_id).unwrap();
        assert!(!post.failed);
        let sys_h = ctx.systems.get_handle(sys_id).expect("awake system");
        let view = sys_h.view();
        ctx.db.with_snapshot(|s| {
            for wid in &wreck_ids {
                assert!(
                    s.items.contains_key(wid),
                    "active Homefront salvage wreck {wid} must survive >=7,200s on_advance_secs_internal"
                );
                assert!(
                    view.balls.contains_key(wid),
                    "active Homefront salvage wreck ball {wid} must survive >=7,200s background tick"
                );
            }
        });
    }

    #[tokio::test]
    async fn homefront_r14_freshly_entered_zero_elapsed_survives_sqlite_restart_and_cold_ticks() {
        use crate::services::pve_homefront_sites::{
            get_homefront_runtime_record, tick_homefront_operations_sync,
        };

        let sde = super::shared_sde();
        let sys_id: SolarSystemId = 30_000_142;
        let tmp_db = std::env::temp_dir().join(format!(
            "evomu_hf_r14_zero_cold_{}_{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&tmp_db);

        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("open sqlite repo1"));
        repo1.with_mut_snapshot(|snap| {
            let mut ch = template_ch.clone();
            ch.character_id = 90_000_001;
            snap.characters.insert(90_000_001, ch);
        });
        let ctx1 = ServerContext::with_sde_and_repository(sde.clone(), repo1.clone());
        ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

        let pilot = seed_hf_pilot(&ctx1, 7725, 950_902, 850_902, 649, sys_id, &[]).await;
        let raid = spawn_homefront_site_for_dungeon(&ctx1, sys_id, 10_347).unwrap();
        enter_hf_gate_at_site(&ctx1, &pilot, raid.instance_id)
            .await
            .expect("enter Raid site cleanly");

        // Run a zero-sim-delta tick before restart so last_tick_sec > started_at_sec while elapsed_sim_secs == 0.0.
        ctx1.advance_secs(5);
        tick_homefront_operations_sync(&ctx1);
        let pre = get_homefront_runtime_record(&ctx1, raid.instance_id).unwrap();
        assert_eq!(pre.elapsed_sim_secs, 0.0);
        assert!(pre.sim_clock_initialized);
        assert!(!pre.failed);
        let time_limit = pre.blueprint.time_limit_secs;

        drop(ctx1);
        drop(repo1);

        // Restart SQLite with > time_limit_secs downtime while solar system remains cold.
        let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo2"));
        let ctx2 = ServerContext::with_sde_and_repository(sde, repo2);
        ctx2.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
            1_775_400_005 + time_limit + 600,
        )));

        let loaded = get_homefront_runtime_record(&ctx2, raid.instance_id).unwrap();
        assert_eq!(loaded.elapsed_sim_secs, 0.0);
        assert_eq!(loaded.last_sim_elapsed_secs, None);
        assert!(loaded.sim_clock_initialized);
        assert!(!loaded.failed);

        // Multiple cold ticks + active-pilot RPC must neither charge downtime, fail the site, nor wake the cold system.
        for delta in [0, 5, 10] {
            if delta > 0 {
                ctx2.advance_secs(delta);
            }
            tick_homefront_operations_sync(&ctx2);
            assert!(ctx2.systems.get_handle(sys_id).is_none());
            let after_cold = get_homefront_runtime_record(&ctx2, raid.instance_id).unwrap();
            assert_eq!(after_cold.elapsed_sim_secs, 0.0);
            assert!(!after_cold.failed);
        }
        record_homefront_combat_and_logistics(&ctx2, &pilot, raid.instance_id, 50.0, 0.0)
            .expect("cold pilot RPC must not misclassify restarted elapsed_sim_secs == 0.0 as expired");
        assert!(ctx2.systems.get_handle(sys_id).is_none());

        // Waking the system and stepping 20s resumes normal sim-clock progression cleanly from 0.0s.
        let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
        tick_homefront_operations_sync(&ctx2);
        sys_h2.step_manual(20.0).await.unwrap();
        ctx2.advance_secs(20);
        tick_homefront_operations_sync(&ctx2);

        let post_wake = get_homefront_runtime_record(&ctx2, raid.instance_id).unwrap();
        assert!(!post_wake.failed);
        assert!(
            (post_wake.elapsed_sim_secs - 20.0).abs() < 1e-6,
            "expected 20.0s sim time after wake, got {}",
            post_wake.elapsed_sim_secs
        );

        drop(sys_h2);
        drop(ctx2);
        let _ = std::fs::remove_file(&tmp_db);
    }

    #[tokio::test]
    async fn homefront_rt36_salvage_werpost_kill_does_not_resurrect_boss_across_ticks_or_restart() {
        use crate::services::pve_homefront_sites::{
            ensure_homefront_outer_entries_sync, get_homefront_runtime_record,
            on_homefront_entity_destroyed_core, tick_homefront_operations_sync,
        };
        use eve_db::HomefrontProgressRecord;

        let sde = super::shared_sde();
        let sys_id: SolarSystemId = 30_000_142;
        let tmp_db = std::env::temp_dir().join(format!(
            "evomu_hf_werpost_boss_{}_{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&tmp_db);

        let template_ch = WorldRepository::with_demo_seed()
            .with_snapshot(|s| s.characters[&90_000_001].clone());
        let repo1 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("open sqlite repo1"));
        repo1.with_mut_snapshot(|snap| {
            let mut ch = template_ch.clone();
            ch.character_id = 90_000_001;
            snap.characters.insert(90_000_001, ch);
        });
        let ctx1 = ServerContext::with_sde_and_repository(sde.clone(), repo1.clone());
        ctx1.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));

        let coercer =
            seed_hf_pilot(&ctx1, 7726, 950_903, 850_903, 16_236, sys_id, &[25_861, 22_175, 533])
                .await;
        let sal = spawn_homefront_site_for_dungeon(&ctx1, sys_id, 10_713).unwrap();
        enter_hf_gate_at_site(&ctx1, &coercer, sal.instance_id)
            .await
            .expect("enter SalvageResearch site");

        let pre = get_homefront_runtime_record(&ctx1, sal.instance_id).unwrap();
        let boss_id = pre.dispatch_boss_ball_id;
        let dead_wreck_id = match &pre.progress {
            HomefrontProgressRecord::SalvageResearch { wreck_ball_ids, .. } => wreck_ball_ids[0],
            other => panic!("expected SalvageResearch progress, got {other:?}"),
        };
        assert!(pre.spawned_ball_ids.contains(&boss_id));
        let sys_h1 = ctx1.systems.get_handle(sys_id).expect("awake system");
        let boss_hp = {
            let v = sys_h1.view();
            let cs = &v.combat_states[&boss_id];
            cs.shield_hp + cs.armor_hp + cs.structure_hp
        };

        // Activate Werpost via SalvageResearch RPC and step until Werpost destroys dispatch_boss_ball_id.
        salvage_research_process_wreck(&ctx1, &coercer, sal.instance_id, 0, true)
            .expect("activate Werpost");
        let kill_secs = ((boss_hp / 350.0).ceil() as u64).max(1) + 2;
        for _ in 0..kill_secs {
            sys_h1.step_manual(1.0).await.unwrap();
            ctx1.advance_secs(1);
            tick_homefront_operations_sync(&ctx1);
        }

        let after_kill = get_homefront_runtime_record(&ctx1, sal.instance_id).unwrap();
        assert!(!after_kill.failed);
        assert!(
            !after_kill.spawned_ball_ids.contains(&boss_id),
            "Werpost-killed dispatch_boss_ball_id {boss_id} must be pruned from spawned_ball_ids"
        );
        assert!(
            !sys_h1.view().balls.contains_key(&boss_id),
            "Werpost-killed dispatch_boss_ball_id {boss_id} must not remain in space"
        );

        // 5 additional 1 Hz ticks must never resurrect the dead boss.
        for _ in 0..5 {
            sys_h1.step_manual(1.0).await.unwrap();
            ctx1.advance_secs(1);
            tick_homefront_operations_sync(&ctx1);
            assert!(
                !sys_h1.view().balls.contains_key(&boss_id),
                "Werpost-killed dispatch_boss_ball_id {boss_id} must not resurrect on subsequent 1 Hz ticks"
            );
            let tick_rec = get_homefront_runtime_record(&ctx1, sal.instance_id).unwrap();
            assert!(!tick_rec.spawned_ball_ids.contains(&boss_id));
        }

        // Destroying a Salvage wreck via entity destruction also prunes wreck_ball_ids (no restart respawn).
        on_homefront_entity_destroyed_core(
            &ctx1.systems,
            &ctx1.db,
            &ctx1.sde,
            sys_id,
            dead_wreck_id,
            None,
        )
        .await;
        sys_h1.try_remove_destroyed_ball(dead_wreck_id);

        drop(sys_h1);
        drop(ctx1);
        drop(repo1);

        // Restart SQLite, wake solar system, and reconcile pocket: neither boss nor destroyed wreck resurrects.
        let repo2 = Arc::new(WorldRepository::open_sqlite(&tmp_db).expect("reopen sqlite repo2"));
        let ctx2 = ServerContext::with_sde_and_repository(sde, repo2);
        let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
        ensure_homefront_outer_entries_sync(&ctx2, Some(sys_id), Some(sal.instance_id));
        tick_homefront_operations_sync(&ctx2);

        let reopened = get_homefront_runtime_record(&ctx2, sal.instance_id).unwrap();
        assert!(!reopened.spawned_ball_ids.contains(&boss_id));
        match &reopened.progress {
            HomefrontProgressRecord::SalvageResearch { wreck_ball_ids, .. } => {
                assert!(!wreck_ball_ids.contains(&dead_wreck_id));
            }
            other => panic!("expected SalvageResearch after restart, got {other:?}"),
        }
        let view2 = sys_h2.view();
        assert!(!view2.balls.contains_key(&boss_id));
        assert!(!view2.balls.contains_key(&dead_wreck_id));

        drop(sys_h2);
        drop(ctx2);
        let _ = std::fs::remove_file(&tmp_db);
    }
}
