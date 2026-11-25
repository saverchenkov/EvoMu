//! Mailbox-backed test seam and `#[cfg(any(test, feature = "testkit"))]` accessors (`#1342`).
//!
//! Routes test inspections and mutations through `SystemCommand::TestAccess` so every
//! actor-owned field and derived [`super::SystemView`] snapshot advances deterministically in
//! actor-loop order without exposing unsynchronized external lock writes.

#![forbid(unsafe_code)]

use super::ball_ops::{
    call_actor_reply_with_deadline, dispatch_actor_sync_reply_with_deadline, ACTOR_CMD_TIMEOUT,
};
use super::{ActorExecCtx, SystemActorState, SystemCommand, SystemHandle, TestAccessOp};
use eve_destiny::Ballpark;
use std::sync::{Arc, Mutex};

pub(in crate::system_actor) fn handle_test_access_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::TestAccess { op, mutated, reply } => {
            op.run(exec_ctx.ballpark, exec_ctx.actor_st);
            if mutated {
                super::sync_ball_positions(
                    exec_ctx.ballpark,
                    exec_ctx.ball_positions_actor,
                    exec_ctx.scrambled_balls_actor,
                );
            }
            super::defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        other => Some(other),
    }
}

fn prepare_test_access_op<R: Send + 'static>(
    f: impl FnOnce(&mut Ballpark, &mut SystemActorState) -> R + Send + 'static,
) -> (Arc<Mutex<Option<R>>>, TestAccessOp) {
    let slot: Arc<Mutex<Option<R>>> = Arc::new(Mutex::new(None));
    let slot_cb = Arc::clone(&slot);
    let op = TestAccessOp::new(move |bp: &mut Ballpark, st: &mut SystemActorState| {
        let val = f(bp, st);
        *slot_cb.lock().unwrap_or_else(|e| e.into_inner()) = Some(val);
    });
    (slot, op)
}

fn take_test_access_result<R>(slot: Arc<Mutex<Option<R>>>) -> R {
    let mut guard = slot.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .take()
        .expect("SystemCommand::TestAccess executed without producing result")
}

async fn call_test_access_async<R: Send + 'static>(
    handle: &SystemHandle,
    mutated: bool,
    f: impl FnOnce(&mut Ballpark, &mut SystemActorState) -> R + Send + 'static,
) -> R {
    let (slot, op) = prepare_test_access_op(f);
    call_actor_reply_with_deadline(handle, "TestAccess", ACTOR_CMD_TIMEOUT, |reply| {
        SystemCommand::TestAccess { op, mutated, reply }
    })
    .await
    .unwrap_or_else(|err| panic!("SystemHandle::TestAccess async failed: {err}"));
    take_test_access_result(slot)
}

fn call_test_access_sync<R: Send + 'static>(
    handle: &SystemHandle,
    mutated: bool,
    f: impl FnOnce(&mut Ballpark, &mut SystemActorState) -> R + Send + 'static,
) -> R {
    let (slot, op) = prepare_test_access_op(f);
    dispatch_actor_sync_reply_with_deadline(
        handle,
        "TestAccess",
        ACTOR_CMD_TIMEOUT,
        op,
        |op, reply| SystemCommand::TestAccess { op, mutated, reply },
        |ctx, op| {
            op.run(ctx.ballpark, ctx.actor_st);
            if mutated {
                ctx.sync_positions();
            }
        },
    )
    .unwrap_or_else(|err| panic!("SystemHandle::TestAccess sync failed: {err}"));
    take_test_access_result(slot)
}

impl SystemHandle {
    /// Inspects actor-owned `Ballpark` state asynchronously through the actor mailbox.
    pub async fn test_with_ballpark_async<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Ballpark) -> R + Send + 'static,
    ) -> R {
        call_test_access_async(self, false, move |bp, _| f(bp)).await
    }

    /// Mutates actor-owned `Ballpark` state asynchronously through the actor mailbox and publishes
    /// an updated [`super::SystemView`] before resolving.
    pub async fn test_with_ballpark_mut_async<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Ballpark) -> R + Send + 'static,
    ) -> R {
        call_test_access_async(self, true, move |bp, _| f(bp)).await
    }

    /// Inspects actor-owned `SystemActorState` asynchronously through the actor mailbox.
    #[cfg(test)]
    pub(crate) async fn test_with_actor_state_async<R: Send + 'static>(
        &self,
        f: impl FnOnce(&SystemActorState) -> R + Send + 'static,
    ) -> R {
        call_test_access_async(self, false, move |_, st| f(st)).await
    }

    /// Mutates actor-owned `SystemActorState` asynchronously through the actor mailbox and
    /// publishes an updated [`super::SystemView`] before resolving.
    #[cfg(test)]
    pub(crate) async fn test_with_actor_state_mut_async<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut SystemActorState) -> R + Send + 'static,
    ) -> R {
        call_test_access_async(self, true, move |_, st| f(st)).await
    }

    /// Atomically mutates both `Ballpark` and `SystemActorState` asynchronously through the actor
    /// mailbox and publishes an updated [`super::SystemView`] before resolving.
    #[cfg(test)]
    pub(crate) async fn test_with_ballpark_and_state_mut_async<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Ballpark, &mut SystemActorState) -> R + Send + 'static,
    ) -> R {
        call_test_access_async(self, true, f).await
    }

    /// Inspects actor-owned `Ballpark` state synchronously through the bounded actor bridge.
    pub fn test_with_ballpark<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Ballpark) -> R + Send + 'static,
    ) -> R {
        call_test_access_sync(self, false, move |bp, _| f(bp))
    }

    /// Mutates actor-owned `Ballpark` state synchronously through the bounded actor bridge and
    /// publishes an updated [`super::SystemView`] before returning.
    pub fn test_with_ballpark_mut<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Ballpark) -> R + Send + 'static,
    ) -> R {
        call_test_access_sync(self, true, move |bp, _| f(bp))
    }

    /// Inspects actor-owned `SystemActorState` synchronously through the bounded actor bridge.
    #[cfg(test)]
    pub(crate) fn test_with_actor_state<R: Send + 'static>(
        &self,
        f: impl FnOnce(&SystemActorState) -> R + Send + 'static,
    ) -> R {
        call_test_access_sync(self, false, move |_, st| f(st))
    }

    /// Mutates actor-owned `SystemActorState` synchronously through the bounded actor bridge and
    /// publishes an updated [`super::SystemView`] before returning.
    #[cfg(test)]
    pub(crate) fn test_with_actor_state_mut<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut SystemActorState) -> R + Send + 'static,
    ) -> R {
        call_test_access_sync(self, true, move |_, st| f(st))
    }

    /// Atomically mutates both `Ballpark` and `SystemActorState` synchronously through the bounded
    /// actor bridge and publishes an updated [`super::SystemView`] before returning.
    #[cfg(test)]
    pub(crate) fn test_with_ballpark_and_state_mut<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Ballpark, &mut SystemActorState) -> R + Send + 'static,
    ) -> R {
        call_test_access_sync(self, true, f)
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        ActiveModuleCycle, ActiveNaniteRepair, ActorShipProtection, AsteroidState, SystemRegistry,
        WarpProvenance,
    };
    use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
    use eve_sde::SdeRegistry;
    use glam::DVec3;
    use std::sync::Arc;

    fn test_sde() -> Arc<SdeRegistry> {
        Arc::new(SdeRegistry::default())
    }

    fn mk_ship(id: i64, pos: DVec3, vel: DVec3, mode: MovementMode) -> Ball {
        Ball {
            id,
            type_id: 587,
            position: pos,
            velocity: vel,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: if vel.length_squared() > 0.0 { 1.0 } else { 0.0 },
            mode,
        }
    }

    #[tokio::test]
    async fn test_seam_async_accessors_work_on_default_current_thread_and_publish_views() {
        let reg = SystemRegistry::with_sde(test_sde());
        let sys = reg.get_or_spawn(30_000_142);
        let ship_id = 910_001_i64;
        let gate_id = 910_002_i64;

        let initial_bp_id = sys.test_with_ballpark_async(|bp| bp.solar_system_id).await;
        assert_eq!(initial_bp_id, 30_000_142);

        let before_cycles = sys
            .test_with_actor_state_async(|st| st.active_module_cycles.len())
            .await;
        assert_eq!(before_cycles, 0);

        sys.test_with_ballpark_mut_async(move |bp| {
            bp.add_ball(mk_ship(ship_id, DVec3::new(1_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));
            bp.init_stargate_activation_state(gate_id, 1);
            bp.set_stargate_activation_state(gate_id, 2, 1_500, 99_999);
        })
        .await;
        assert!(sys.view().contains_ball(ship_id));
        assert_eq!(
            sys.view()
                .stargate_activation_state(gate_id)
                .map(|s| s.activation_state),
            Some(2)
        );

        sys.test_with_actor_state_mut_async(move |st| {
            st.active_module_cycles.push(ActiveModuleCycle {
                ship_id,
                module_id: 77_001,
                module_type_id: 2048,
                effect_name: "online".into(),
                duration_s: 10.0,
                ..Default::default()
            });
        })
        .await;
        assert!(sys.view().has_active_module(ship_id));
        assert_eq!(sys.view().active_module_cycles.len(), 1);

        let count = sys
            .test_with_ballpark_and_state_mut_async(move |bp, st| {
                bp.combat_states.insert(
                    ship_id,
                    CombatState {
                        is_cloaked: true,
                        ..Default::default()
                    },
                );
                st.external_active_modules.insert((ship_id, 77_002));
                st.active_module_cycles.len() + st.external_active_modules.len()
            })
            .await;
        assert_eq!(count, 2);
        let view = sys.view();
        assert!(view.is_ball_cloaked(ship_id));
        assert!(view.external_active_modules.contains(&(ship_id, 77_002)));
    }

    #[test]
    fn test_seam_sync_accessors_work_off_runtime_and_current_thread_runtime() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("multi-thread test runtime");
        let reg = rt.block_on(async { SystemRegistry::with_sde(test_sde()) });
        let sys = rt.block_on(async { reg.get_or_spawn(30_000_142) });

        // Call sync test_with_* from non-Tokio thread (`#[test]` transient-runtime path).
        let ship_a = 910_101_i64;
        sys.test_with_ballpark_mut(move |bp| {
            bp.add_ball(mk_ship(ship_a, DVec3::new(50.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));
        });
        assert_eq!(
            sys.test_with_ballpark(move |bp| bp.balls.get(&ship_a).map(|b| b.position.x)),
            Some(50.0)
        );
        assert_eq!(sys.view().ball_position(ship_a).map(|p| p.x), Some(50.0));

        let cycles = sys.test_with_actor_state_mut(move |st| {
            st.active_module_cycles.push(ActiveModuleCycle {
                ship_id: ship_a,
                module_id: 88_001,
                duration_s: 5.0,
                ..Default::default()
            });
            st.active_module_cycles.len()
        });
        assert_eq!(cycles, 1);
        assert_eq!(
            sys.test_with_actor_state(|st| st.active_module_cycles.len()),
            1
        );
        assert_eq!(sys.view().active_module_cycles.len(), 1);

        let ship_b = 910_102_i64;
        sys.test_with_ballpark_and_state_mut(move |bp, st| {
            bp.add_ball(mk_ship(ship_b, DVec3::ZERO, DVec3::ZERO, MovementMode::Stop));
            st.warp_state.track_player_or_fleet_warp(ship_b, 90_000_001);
        });
        assert_eq!(sys.warp_provenance(ship_b), Some(WarpProvenance::PlayerOrFleetWarp));

        // Ensure current_thread direct bridge inside a local `current_thread` runtime also works
        // cleanly without deadlocking on the actor mailbox.
        let ct_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current_thread runtime");
        ct_rt.block_on(async {
            let ct_reg = SystemRegistry::with_sde(test_sde());
            let ct_sys = ct_reg.get_or_spawn(30_000_144);
            let ship_c = 910_103_i64;
            ct_sys.test_with_ballpark_and_state_mut(move |bp, st| {
                bp.add_ball(mk_ship(
                    ship_c,
                    DVec3::new(10.0, 20.0, 30.0),
                    DVec3::ZERO,
                    MovementMode::Stop,
                ));
                bp.ball_slim_meta.insert(
                    ship_c,
                    SlimItemMeta {
                        group_id: 25,
                        category_id: 6,
                        ..Default::default()
                    },
                );
                st.asteroids.insert(
                    910_200,
                    AsteroidState {
                        asteroid_id: 910_200,
                        belt_id: 1,
                        type_id: 1230,
                        initial_units: 500,
                        remaining_units: 500,
                        unit_volume: 0.1,
                        depleted: false,
                        respawn_at_tick: None,
                    },
                );
                st.protection_registry.insert(
                    ship_c,
                    ActorShipProtection {
                        has_active_cloak_cycle: true,
                        ..Default::default()
                    },
                );
                st.active_nanite_repairs.push(ActiveNaniteRepair {
                    char_id: 90_000_001,
                    ship_id: ship_c,
                    module_id: 88_002,
                    initial_damage_hp: 10.0,
                    paste_total_needed: 1,
                    paste_consumed_so_far: 1,
                    duration_s: 5.0,
                    elapsed_s: 0.0,
                });
            });
            assert!(ct_sys.view().contains_ball(ship_c));
            assert!(ct_sys.has_active_non_gate_cloak_sync(ship_c));
            assert!(ct_sys.has_active_nanite_repair_sync(ship_c, 88_002));
        });
    }

    #[tokio::test]
    async fn converted_pure_read_helpers_match_actor_state_and_acquire_zero_locks() {
        let sde = test_sde();
        let reg = SystemRegistry::with_sde(Arc::clone(&sde));
        let sys = reg.get_or_spawn(30_000_142);

        let ship_a = 920_001_i64;
        let ship_b = 920_002_i64;
        let target_ship = 920_003_i64;
        let gate_id = 920_010_i64;
        let ast_active = 920_020_i64;
        let ast_depleted = 920_021_i64;
        let mod_overload = 920_101_i64;
        let mod_mjd = 920_102_i64;
        let mod_ind = 920_103_i64;
        let mod_cyno = 920_104_i64;
        let mod_lance = 920_105_i64;
        let mod_ext = 920_106_i64;
        let mod_nanite = 920_107_i64;
        let warp_dest_a = DVec3::new(300_000.0, 0.0, 0.0);
        let warp_dest_b = DVec3::new(900_000.0, 0.0, 0.0);
        let proj = eve_dogma::ShipFittingEngine::from_sde(ship_a, 587, &sde)
            .to_projection(eve_dogma::FitRev(42), &sde);
        let proj_expected = proj.clone();

        sys.test_with_ballpark_and_state_mut_async(move |bp, st| {
            let mut ball_a = mk_ship(
                ship_a,
                DVec3::new(500.0, 0.0, 0.0),
                DVec3::new(120.0, -30.0, 0.0),
                MovementMode::new_warp(DVec3::new(500.0, 0.0, 0.0), warp_dest_a, 3.0),
            );
            ball_a.velocity = DVec3::new(120.0, -30.0, 0.0);
            bp.add_ball(ball_a);

            let mut ball_b = mk_ship(
                ship_b,
                DVec3::new(1_000.0, 0.0, 0.0),
                DVec3::ZERO,
                MovementMode::new_warp(DVec3::new(1_000.0, 0.0, 0.0), warp_dest_b, 3.0),
            );
            if let MovementMode::Warp { phase, .. } = &mut ball_b.mode {
                *phase = eve_destiny::WarpPhase::Cruising;
            }
            bp.add_ball(ball_b);
            bp.add_ball(mk_ship(target_ship, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));
            bp.combat_states.insert(
                ship_b,
                CombatState {
                    is_cloaked: true,
                    ..Default::default()
                },
            );
            bp.init_stargate_activation_state(gate_id, 1);
            bp.set_stargate_activation_state(gate_id, 2, 1_500, 123_456);

            st.fit_projections.insert(ship_a, proj);
            st.active_module_cycles.push(ActiveModuleCycle {
                ship_id: ship_a,
                module_id: mod_overload,
                module_type_id: 484,
                target_id: Some(target_ship),
                damage_vector: eve_destiny::DamageVector { em: 25.0, ..Default::default() },
                is_overloaded: true,
                pending_overloaded: Some(false),
                self_ewar_immune: true,
                duration_s: 10.0,
                ..Default::default()
            });
            st.active_module_cycles.push(ActiveModuleCycle {
                ship_id: ship_a,
                module_id: mod_mjd,
                module_type_id: 4383,
                mjd_jump_distance_m: Some(100_000.0),
                duration_s: 12.0,
                ..Default::default()
            });
            st.active_module_cycles.push(ActiveModuleCycle {
                ship_id: ship_a,
                module_id: mod_ind,
                module_type_id: 28583,
                target_id: Some(target_ship),
                remote_shield_amount: 10.0,
                duration_s: 60.0,
                ..Default::default()
            });
            st.active_module_cycles.push(ActiveModuleCycle {
                ship_id: ship_a,
                module_id: mod_cyno,
                effect_name: "cynosuralGeneration".into(),
                duration_s: 300.0,
                started_at_unix: 1_000,
                ..Default::default()
            });
            st.active_module_cycles.push(ActiveModuleCycle {
                ship_id: ship_a,
                module_id: mod_lance,
                effect_name: "superWeaponLanceWarmup".into(),
                duration_s: 15.0,
                elapsed_s: 2.0,
                started_at_unix: 1_000,
                repeat: 5,
                ..Default::default()
            });
            st.external_active_modules.insert((ship_b, mod_ext));
            st.active_nanite_repairs.push(ActiveNaniteRepair {
                char_id: 90_000_001,
                ship_id: ship_a,
                module_id: mod_nanite,
                initial_damage_hp: 20.0,
                paste_total_needed: 4,
                paste_consumed_so_far: 2,
                duration_s: 10.0,
                elapsed_s: 2.0,
            });
            st.pending_anchoring_disruptors.push(super::super::PendingAnchoringDisruptor {
                id: 920_099,
                type_id: 1947,
                position: DVec3::ZERO,
                radius: 20_000.0,
                remaining_s: 60.0,
            });
            st.warp_state.track_player_or_fleet_warp(ship_a, 90_000_001);
            st.warp_state.track_return_warp(ship_b);
            st.warp_state.pending_warp_finished.insert(ship_b);
            st.protection_registry.insert(
                ship_b,
                ActorShipProtection {
                    has_active_cloak_cycle: true,
                    ..Default::default()
                },
            );
            st.asteroids.insert(
                ast_active,
                AsteroidState {
                    asteroid_id: ast_active,
                    belt_id: 1,
                    type_id: 1230,
                    initial_units: 500,
                    remaining_units: 500,
                    unit_volume: 0.1,
                    depleted: false,
                    respawn_at_tick: None,
                },
            );
            st.asteroids.insert(
                ast_depleted,
                AsteroidState {
                    asteroid_id: ast_depleted,
                    belt_id: 1,
                    type_id: 1230,
                    initial_units: 500,
                    remaining_units: 100,
                    unit_volume: 0.1,
                    depleted: true,
                    respawn_at_tick: Some(9_999),
                },
            );
        })
        .await;

        // Holding BOTH ballpark and actor_state write locks proves every pure-read helper below
        // reads only lock-free `SystemView` (acquiring RANK_BALLPARK or RANK_ACTOR_STATE would deadlock/panic).
        let view_before = sys.view();
        {
            let bp_guard = sys.ballpark.write_untracked().unwrap();
            let _st_guard = sys.actor_state.write().unwrap();

            assert_eq!(sys.latest_fit_projection(ship_a), Some(proj_expected));
            assert!(sys.is_module_active_sync(ship_a, mod_overload));
            assert!(sys.is_module_active_sync(ship_b, mod_ext));
            assert!(!sys.is_hibernated());
            assert_eq!(sys.stargate_activation_state(gate_id).map(|s| s.activation_state), Some(2));
            assert!(sys.has_active_nanite_repair_sync(ship_a, mod_nanite));
            assert!(sys.has_active_module_cycle_sync(ship_a, mod_overload));
            assert!(sys.has_active_mjd_or_mjfg_spool_sync(ship_a));
            assert!(sys.is_module_mjd_or_mjfg_spooling_sync(ship_a, mod_mjd));
            // 3 dyn balls + (5 cycles * 2) + 1 disruptor + 1 nanite repair = 15
            assert_eq!(sys.active_entity_and_cycle_load(), 15);
            assert_eq!(sys.warp_provenance(ship_a), Some(WarpProvenance::PlayerOrFleetWarp));
            assert_eq!(sys.active_warp_char(ship_a), Some(90_000_001));
            assert!(sys.is_active_warp(ship_a));
            assert!(sys.is_return_warp(ship_b));
            assert!(sys.is_pending_warp_finished(ship_b));
            assert!(sys.has_any_active_nanite_repair_sync(ship_a));
            assert!(!sys.has_any_active_nanite_repair_sync(ship_b));
            assert!(sys.is_module_being_nanite_repaired_sync(ship_a, mod_nanite));
            assert!(sys.has_active_overloaded_cycle_sync(ship_a));
            assert!(!sys.has_active_overloaded_cycle_sync(ship_b));
            assert_eq!(sys.active_offensive_aggressors_on_sync(target_ship, None), vec![ship_a]);
            assert!(sys.has_active_industrial_core_sync(ship_a));
            assert_eq!(sys.get_ball_velocity_sync(ship_a), Some([120.0, -30.0, 0.0]));
            assert_eq!(
                sys.get_live_ball_kinematics_sync(ship_a),
                Some((DVec3::new(500.0, 0.0, 0.0), DVec3::new(120.0, -30.0, 0.0)))
            );
            assert!(sys.has_active_modules_or_cloak_sync(ship_a));
            assert!(sys.has_active_modules_or_cloak_sync(ship_b));
            assert!(sys.is_ball_warping_sync(ship_a));
            assert!(sys.is_ball_warping_sync(ship_b));
            assert_eq!(sys.committed_warp_destination_sync(ship_a), None);
            assert_eq!(sys.committed_warp_destination_sync(ship_b), Some(warp_dest_b));
            assert!(sys.is_ball_aligning_warp_sync(ship_a));
            assert!(!sys.is_ball_aligning_warp_sync(ship_b));
            // lance rem = (15 - 5) + 5 = 15 -> 1005 + 15 = 1020; ind rem = 60 -> 1065
            assert_eq!(sys.max_active_module_cycle_expiry_unix_sync(ship_a, 1_010, 1_005), 1_065);
            assert!(sys.has_active_non_gate_cloak_sync(ship_b));

            assert_eq!(sys.active_cycle_module_ids_for_ship(ship_a).len(), 5);
            let refs = sys.active_cycles_referencing_ball(target_ship, Some((ship_a, mod_ind)));
            assert_eq!(refs.len(), 1);
            assert_eq!(refs[0].module_id, mod_overload);
            assert!(sys.has_self_ewar_immune_cycle(ship_a));
            assert!(sys.has_active_module_cycle_on_target(ship_a, mod_overload, target_ship));
            assert_eq!(
                sys.active_cycle_overload_states_for_ship(ship_a).get(&mod_overload),
                Some(&(true, Some(false)))
            );
            assert!(sys.has_any_active_module_cycle(ship_a));
            assert!(!sys.has_any_active_module_cycle(target_ship));
            assert_eq!(
                sys.live_asteroid_remaining_units(&[ast_active, ast_depleted, 999_999]),
                vec![(ast_active, 500), (ast_depleted, 0)]
            );

            // `from_ballpark(&bp, None, ..., Some(&view_before))` preserves all newly mirrored fields.
            let preserved = super::super::SystemView::from_ballpark(&bp_guard, None, 1.0, Some(&view_before));
            assert_eq!(preserved.active_module_cycles.len(), 5);
            assert_eq!(preserved.external_active_modules.len(), 1);
            assert_eq!(preserved.active_nanite_repairs.len(), 1);
            assert_eq!(preserved.pending_anchoring_disruptor_count, 1);
            assert!(preserved.protection_registry.contains_key(&ship_b));
            assert!(preserved.fit_projections.contains_key(&ship_a));
            assert_eq!(preserved.asteroids.len(), 2);
            assert_eq!(preserved.stargate_activation_state(gate_id).map(|s| s.activation_state), Some(2));
        }
        assert!(eve_db::lock_rank::held_lock_ranks().is_empty());

        // Sync mutators that previously omitted publish_view now refresh view immediately.
        sys.sync_asteroid_quantity(ast_active, 0, 12_345);
        assert_eq!(sys.live_asteroid_remaining_units(&[ast_active]), vec![(ast_active, 0)]);
        assert_eq!(sys.cancel_nanite_repairs_for_ship_sync(ship_a).len(), 1);
        assert!(!sys.has_any_active_nanite_repair_sync(ship_a));

        // spawn_ball_with_meta_sync populates ballpark and actor base_core_strengths atomically.
        let drifter_id = 920_500_i64;
        let drifter_ball = mk_ship(drifter_id, DVec3::new(3_500.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop);
        let mut drifter_cs = CombatState {
            warp_core_strength: 3,
            ..Default::default()
        };
        drifter_cs.shield_hp = 100.0;
        let drifter_meta = SlimItemMeta {
            group_id: 1307,
            category_id: 11,
            owner_id: 500_024,
            ..Default::default()
        };
        let spawned_tick = sys
            .spawn_ball_with_meta_sync(drifter_ball, drifter_cs, drifter_meta)
            .expect("spawn_ball_with_meta_sync succeeds");
        assert!(spawned_tick == sys.view().current_tick);
        assert!(sys.view().contains_ball(drifter_id));
        assert_eq!(
            sys.test_with_actor_state_async(move |st| st.base_core_strengths.get(&drifter_id).copied())
                .await,
            Some(3)
        );
    }
}
