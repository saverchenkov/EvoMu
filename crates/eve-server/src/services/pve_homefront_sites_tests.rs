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

    // GetHomefrontSiteInstances must remain strictly read-only on an uninitialized snapshot.
    let cold_res = handle_get_homefront_site_instances(&ctx, &session, &[])
        .expect("cold highsec homefront query succeeds read-only");
    let PyRep::Dict(ref cold_outer) = cold_res else {
        panic!("expected cold outer Dict, got {cold_res:?}");
    };
    let (_, PyRep::List(ref cold_entries)) = cold_outer[0] else {
        panic!("expected cold system list in outer dict, got {cold_outer:?}");
    };
    assert!(cold_entries.is_empty(), "cold GetHomefrontSiteInstances must not seed on read");
    with_pve_special_reg(&ctx, |reg| {
        assert!(!reg.initialized, "GetHomefrontSiteInstances must not initialize special-sites registry");
        assert!(!reg.homefront_sites.contains_key(&hs_sys));
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
        assert_eq!(complete_homefront_operation_and_pay(&ctx, &logi, traf.instance_id).await.unwrap().total_fleet_isk, 7_680_000);

        // Optimal 3-pilot Traffic Stop (n=3 on opt=3) -> full 36,000,000 ISK pool
        let traf3 = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_705).unwrap();
        for s in [&dps2, &logi, &venture] {
            enter_hf_gate_at_site(&ctx, s, traf3.instance_id).await.unwrap();
            record_homefront_combat_and_logistics(&ctx, s, traf3.instance_id, 2_000.0, 0.0).unwrap();
        }
        traffic_stop_scan_and_intercept(&ctx, &logi, traf3.instance_id, 6, 0, 2_000.0).unwrap();
        let traf3_pay = complete_homefront_operation_and_pay(&ctx, &logi, traf3.instance_id).await.unwrap();
        assert_eq!(traf3_pay.eligible_pilot_count, 3);
        assert_eq!(traf3_pay.total_fleet_isk, 36_000_000);
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
        ctx.advance_secs(30);
        tick_homefront_operations_sync(&ctx);
        assert!(matches!(
            get_homefront_runtime_state(&ctx, aar1.instance_id).unwrap().progress,
            HomefrontArchetypeProgress::AbyssalArtifactRecovery {
                extraction_window_secs: 30,
                ..
            }
        ));
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

        // Part B: Skipped wave 2 (bit 1 unset while bits 0,2..=8 are set => mask 0x01FD) recovers
        // exact Wave 2 solo share (2,048,000 ISK) at settlement without double-paying paid waves.
        let pilot2 = seed_hf_pilot(&ctx, 7602, 960_002, 860_002, 32_880, sys_id, &[483]).await;
        let aar2 = spawn_homefront_site_for_dungeon(&ctx, sys_id, 10_346).expect("spawn AAR #2");
        enter_hf_gate_at_site(&ctx, &pilot2, aar2.instance_id)
            .await
            .expect("enter AAR #2");
        abyssal_clear_wave(&ctx, &pilot2, aar2.instance_id, 324, 2_500.0, true)
            .await
            .expect("wave 1 pays bit 0");
        ctx.db.with_mut_snapshot(|snap| {
            let rec = snap
                .dungeon_runtime
                .homefront_runtimes
                .get_mut(&aar2.instance_id)
                .unwrap();
            if let HomefrontProgressRecord::AbyssalArtifactRecovery {
                current_wave,
                waves_paid,
                ..
            } = &mut rec.progress
            {
                *current_wave = 3;
                *waves_paid = 2;
            }
        });
        for _ in 3..=9_u8 {
            abyssal_clear_wave(&ctx, &pilot2, aar2.instance_id, 324, 2_500.0, true)
                .await
                .expect("waves 3..9 pay bits 2..=8");
        }
        ctx.db.with_snapshot(|snap| {
            let rec = &snap.dungeon_runtime.homefront_runtimes[&aar2.instance_id];
            match &rec.progress {
                HomefrontProgressRecord::AbyssalArtifactRecovery { waves_paid_mask, .. } => {
                    assert_eq!(*waves_paid_mask, 0x01FD);
                }
                other => panic!("expected AAR record, got {other:?}"),
            }
            // Live payouts so far: 27,648,000 - 2,048,000 (Wave 2) = 25,600,000 across 8 rows.
            assert_eq!(snap.characters[&960_002].balance, IskCents::from_isk(26_600_000));
            let rows: Vec<_> = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == 960_002 && j.entry_type == "CorporateRewardPayout")
                .collect();
            assert_eq!(rows.len(), 8);
            assert!(rows.iter().all(|j| j.amount > IskCents(0)));
        });
        let _ = ctx.drain_notifications_for_char(960_002);
        let pay2 = complete_homefront_operation_and_pay(&ctx, &pilot2, aar2.instance_id)
            .await
            .expect("complete AAR #2 recovering skipped wave 2");
        assert_eq!(pay2.total_fleet_isk, 27_648_000);
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                snap.characters[&960_002].balance,
                IskCents::from_isk(28_648_000),
                "settlement must pay exact skipped Wave 2 share (2,048,000 ISK)"
            );
            let rows: Vec<_> = snap
                .wallet_journal
                .iter()
                .filter(|j| j.character_id == 960_002 && j.entry_type == "CorporateRewardPayout")
                .collect();
            assert_eq!(rows.len(), 9);
            assert!(rows.iter().all(|j| j.amount > IskCents(0)));
        });
        assert!(ctx
            .drain_notifications_for_char(960_002)
            .iter()
            .any(|n| n.event == "OnAccountChange"));
    }
}
