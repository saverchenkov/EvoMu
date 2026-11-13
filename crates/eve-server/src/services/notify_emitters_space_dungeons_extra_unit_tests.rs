//! Extra unit and mutation-killing tests for `notify_emitters_space_dungeons.rs`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::notify_emitters_space_dungeons::*;
use eve_db::clock::VirtualServerClock;
use eve_db::{FleetRecord, InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_dogma::{DogmaItemState, ProjectedEwarEffect};
use eve_proto::PyRep;
use eve_sde::{InvType, SdeRegistry};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx_with_two_online_pilots() -> (ServerContext, i64, i64) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let char1 = 90_000_001_i64;
    let char2 = 90_000_002_i64;
    db.with_mut_snapshot(|snap| {
        let mut c2 = snap.characters.get(&char1).unwrap().clone();
        c2.character_id = char2;
        c2.user_id = 2;
        c2.name = "Pilot Two".into();
        c2.active_ship_id = 100_000_002;
        snap.characters.insert(char2, c2);
    });
    db.acquire_online_lock(1, 501, Some(char1), true, 1_775_400_000)
        .expect("online lock 1");
    db.acquire_online_lock(2, 502, Some(char2), true, 1_775_400_000)
        .expect("online lock 2");
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_400_000)));
    (ctx, char1, char2)
}

#[test]
fn damage_tuple_custom_shield_tau_and_exact_clamping_boundaries() {
    let (mut ctx, char1, _) = make_ctx_with_two_online_pilots();
    let ship_id = 500_123_i64;
    let custom_tid = 999_479_u32;

    // Insert custom ship type with attr 479 (shieldRechargeRate) = 250_000.0 ms
    {
        let sde = Arc::make_mut(&mut ctx.sde);
        let mut rec: InvType = sde.types.values().next().cloned().unwrap();
        rec.type_id = custom_tid;
        rec.default_attributes.insert(479, 250_000.0);
        sde.types.insert(custom_tid, rec);
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: custom_tid,
                owner_id: char1,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Advance virtual server clock 30 days without sim-clock rebase; timestampFt must still
    // track real OS wallclock FILETIME ticks (`#990`).
    ctx.advance_secs_without_sim_clock_rebase(30 * 86_400);
    let (wall_s, wall_ns) = eve_db::clock::host_wall_unix_secs_and_nanos();
    let wall_ft = (wall_s as i64 + 11_644_473_600) * 10_000_000 + (wall_ns as i64 / 100);

    let tup = build_damage_state_tuple(&ctx, ship_id, -0.5, 0.75, 2.0);
    let PyRep::Tuple(outer) = tup else {
        panic!("expected outer tuple");
    };
    assert_eq!(outer.len(), 3);
    let PyRep::Tuple(ref shield_inner) = outer[0] else {
        panic!("expected inner shield tuple");
    };
    assert_eq!(shield_inner[0], PyRep::Real(0.0));
    assert_eq!(shield_inner[1], PyRep::Real(50_000.0));
    let tup_ft = shield_inner[2].as_i64().expect("timestampFt integer");
    assert!(
        (tup_ft - wall_ft).abs() < 50_000_000,
        "timestampFt must track real wallclock FILETIME within 5s even after virtual clock advance"
    );
    assert_eq!(outer[1], PyRep::Real(0.75));
    assert_eq!(outer[2], PyRep::Real(1.0));

    // Missing item falls back to 100_000.0 / 5.0 = 20_000.0 tau_ms
    let fallback_tup = build_damage_state_tuple(&ctx, 999_999_999, 0.25, 1.0, 0.0);
    let PyRep::Tuple(fb_outer) = fallback_tup else {
        panic!("expected tuple");
    };
    let PyRep::Tuple(ref fb_inner) = fb_outer[0] else {
        panic!("expected inner tuple");
    };
    assert_eq!(fb_inner[0], PyRep::Real(0.25));
    assert_eq!(fb_inner[1], PyRep::Real(20_000.0));
    assert_eq!(fb_outer[1], PyRep::Real(1.0));
    assert_eq!(fb_outer[2], PyRep::Real(0.0));
}

#[test]
fn ess_bank_links_escalation_fleet_filter_and_dungeon_exit_distance_boundary() {
    let (ctx, char1, _) = make_ctx_with_two_online_pilots();
    let sys_id = 30_000_142_u32;

    // 1. ESS bank link no-op branches: payout <= 0.0 when link_started_only == false, ReserveBank with link_started_only == false, unknown method
    let _ = ctx.drain_notifications_for_char(char1);
    emit_ess_bank_link_notifications(&ctx, sys_id, char1, "AttemptLinkToMainBank", false, 0.0);
    emit_ess_bank_link_notifications(&ctx, sys_id, char1, "AttemptLinkToReserveBank", false, 100.0);
    emit_ess_bank_link_notifications(&ctx, sys_id, char1, "OtherMethod", true, 100.0);
    assert!(ctx.drain_notifications_for_char(char1).is_empty());

    // 2. Escalation with notify_fleet == false vs notify_fleet == true when fleet_id is None
    emit_escalation_created_or_advanced(&ctx, char1, 101, 5001, false);
    let drained_no_fleet = ctx.drain_notifications_for_char(char1);
    assert_eq!(drained_no_fleet.len(), 2);
    assert!(!drained_no_fleet
        .iter()
        .any(|n| n.event == "OnFleetEscalatingPathMessage"));

    // 3. Dungeon progression categories: "DED" and "CombatSignature" -> OnEscalatingPathDungeonEntered; current_wave == 0 -> max(1)
    for cat in ["DED", "CombatSignature"] {
        emit_dungeon_progression_notifications(
            &ctx,
            char1,
            sys_id,
            cat,
            "Arch",
            700,
            800,
            3,
            [1.0, 2.0, 3.0],
            0,
            2,
            false,
            999_999,
        );
        let d = ctx.drain_notifications_for_char(char1);
        assert!(d.iter().any(|n| n.event == "OnEscalatingPathDungeonEntered"));
        assert!(!d.iter().any(|n| n.event == "OnDungeonCompleted"));
        let joined = d.iter().find(|n| n.event == "OnProgressionRoomJoined").unwrap();
        // room_objective_id = room_id (3) * 10 + current_wave.max(1) (1) = 31
        assert_eq!(joined.args[1], PyRep::Integer(31));
    }

    // 4. Exiting dungeon distance boundary: spawn site at [175_000, 25_000, -40_000]
    let _ = spawn_and_emit_cosmic_anomaly(&ctx, sys_id, 4444);
    let _ = ctx.drain_notifications_for_char(char1);

    // At 150_001m (> 150_000m): does NOT emit OnExitingDungeon
    emit_exiting_dungeon_on_warp_if_present(
        &ctx,
        char1,
        sys_id,
        [175_000.0 + 150_001.0, 25_000.0, -40_000.0],
    );
    assert!(ctx.drain_notifications_for_char(char1).is_empty());

    // At exact 150_000m boundary (<= 150_000m): emits OnExitingDungeon(4444)
    emit_exiting_dungeon_on_warp_if_present(
        &ctx,
        char1,
        sys_id,
        [175_000.0 + 150_000.0, 25_000.0, -40_000.0],
    );
    let exit_notifs = ctx.drain_notifications_for_char(char1);
    assert_eq!(exit_notifs.len(), 1);
    assert_eq!(exit_notifs[0].event, "OnExitingDungeon");
    assert_eq!(exit_notifs[0].args, vec![PyRep::Integer(4444)]);
}

#[test]
fn projected_ewar_flush_fallback_victim_notification_and_solar_system_fallback() {
    let (ctx, char1, char2) = make_ctx_with_two_online_pilots();
    let sys_id = 30_000_142_u32;
    let ship1 = 100_000_001_i64;
    let ship2 = 100_000_002_i64;
    let scram_mod_id = 940_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship1,
            InventoryItemRecord {
                item_id: ship1,
                type_id: 606,
                owner_id: char1,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship2,
            InventoryItemRecord {
                item_id: ship2,
                type_id: 606,
                owner_id: char2,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        let mut mod_rec = InventoryItemRecord {
            item_id: scram_mod_id,
            type_id: 447, // Warp Scrambler I (group 52)
            owner_id: char1,
            location_id: ship1,
            flag: InventoryFlag::MedSlot0,
            quantity: 1,
            singleton: true,
            ..Default::default()
        };
        mod_rec.dynamic_attributes.insert(999, 42.0);
        snap.items.insert(scram_mod_id, mod_rec);
    });

    // 1. When module_id == ship_id or ship_id <= 0, OnGodmaFlushLocationProfile is NOT emitted
    let _ = ctx.drain_notifications_for_char(char1);
    flush_module_profile_and_ewar_end(&ctx, char1, Some(sys_id), ship1, ship1);
    flush_module_profile_and_ewar_end(&ctx, char1, Some(sys_id), 0, scram_mod_id);
    assert!(ctx.drain_notifications_for_char(char1).is_empty());

    // 1b. flush_module_ewar_end emits OnEwarEnd WITHOUT OnGodmaFlushLocationProfile (GH #963),
    //     whereas flush_module_profile_and_ewar_end emits both when module_id != ship_id.
    ctx.active_ewar_starts
        .write()
        .unwrap()
        .insert((ship1, scram_mod_id), vec![(ship2, "warpScrambler")]);
    let _ = flush_module_ewar_end(&ctx, char1, Some(sys_id), ship1, scram_mod_id);
    let ewar_only_notifs = ctx.drain_notifications_for_char(char1);
    assert!(ewar_only_notifs.iter().any(|n| n.event == "OnEwarEnd"));
    assert!(
        !ewar_only_notifs.iter().any(|n| n.event == "OnGodmaFlushLocationProfile"),
        "flush_module_ewar_end must preserve still-fitted module in godma.invitems"
    );
    ctx.active_ewar_starts
        .write()
        .unwrap()
        .insert((ship1, scram_mod_id), vec![(ship2, "warpScrambler")]);
    let _ = flush_module_profile_and_ewar_end(&ctx, char1, Some(sys_id), ship1, scram_mod_id);
    let unfit_notifs = ctx.drain_notifications_for_char(char1);
    let flush_notif = unfit_notifs
        .iter()
        .find(|n| n.event == "OnGodmaFlushLocationProfile")
        .expect("flush_module_profile_and_ewar_end must emit OnGodmaFlushLocationProfile on unfit");
    assert_eq!(
        flush_notif.args,
        vec![
            PyRep::Integer(ship1),
            PyRep::Tuple(vec![
                PyRep::String("effect".into()),
                PyRep::Integer(scram_mod_id),
            ]),
        ]
    );
    assert!(unfit_notifs.iter().any(|n| n.event == "OnEwarEnd"));

    // 2. Populate projected_ewar and projected_ewar_sources (without active_ewar_starts)
    //    targeting ship2 (owned by char2), then call flush_ship_all_ewar_sync on victim ship2!
    let mut eff_mod = DogmaItemState::from_sde(scram_mod_id, 447, &ctx.sde);
    eff_mod.base_attributes.insert(103, 2.0); // warpScrambleStrength
    ctx.projected_ewar_sources
        .write()
        .unwrap()
        .insert(scram_mod_id, ship1);
    ctx.projected_ewar.write().unwrap().insert(
        ship2,
        vec![ProjectedEwarEffect {
            source_module_id: scram_mod_id,
            source_module_type_id: 447,
            charge_type_id: None,
            distance: 5_000.0,
            overloaded: false,
            effective_module: eff_mod,
        }],
    );

    let flushed_pairs = flush_ship_all_ewar_sync(&ctx, char2, Some(sys_id), ship2);
    assert_eq!(flushed_pairs, vec![(ship1, scram_mod_id)]);

    // Both attacker (char1) and victim (char2) must receive OnEwarEnd without purging scram_mod_id
    let d1 = ctx.drain_notifications_for_char(char1);
    let d2 = ctx.drain_notifications_for_char(char2);
    assert!(d1.iter().any(|n| n.event == "OnEwarEnd"));
    assert!(!d1.iter().any(|n| n.event == "OnGodmaFlushLocationProfile"));
    assert!(d2.iter().any(|n| n.event == "OnEwarEnd"));

    // 3. SolarSystem fallback when char_id == 0 and target_owner == 0 (NPC attacker -> NPC target)
    ctx.active_ewar_starts
        .write()
        .unwrap()
        .insert((800_001, 800_002), vec![(800_003, "webify")]);
    flush_module_profile_and_ewar_end(&ctx, 0, Some(sys_id), 800_001, 800_002);
    let sys_notifs = ctx.drain_notifications_for_char(char1);
    assert!(sys_notifs.iter().any(|n| n.event == "OnEwarEnd"));
}

#[test]
fn beacon_deployable_cloak_cancel_kill_notifications_and_jam_duration_clamping() {
    let (ctx, char1, char2) = make_ctx_with_two_online_pilots();
    let sys_id = 30_000_142_u32;
    let fid = 888_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            fid,
            FleetRecord {
                fleet_id: fid,
                boss_id: char1,
                members: vec![char1, char2],
                ..Default::default()
            },
        );
    });
    let _ = ctx.drain_notifications_for_char(char1);
    let _ = ctx.drain_notifications_for_char(char2);

    // 1. emit_fleet_jump_beacon_deployable_change: Some(0) is ignored; Some(fid) notifies fleet
    emit_fleet_jump_beacon_deployable_change(&ctx, Some(0), 555, sys_id, char1, true);
    assert!(ctx.drain_notifications_for_char(char1).is_empty());

    emit_fleet_jump_beacon_deployable_change(&ctx, Some(fid), 555, sys_id, char1, true);
    let beacon_n = ctx.drain_notifications_for_char(char1);
    assert_eq!(beacon_n.len(), 1);
    assert_eq!(beacon_n[0].event, "OnFleetJumpBeaconDeployableChange");
    assert_eq!(beacon_n[0].args[4], PyRep::Bool(true));
    let _ = ctx.drain_notifications_for_char(char2);

    // 2. emit_warp_cloak_or_invuln_cancelled selective boolean combinations
    emit_warp_cloak_or_invuln_cancelled(&ctx, char1, 100, false, false);
    assert!(ctx.drain_notifications_for_char(char1).is_empty());

    emit_warp_cloak_or_invuln_cancelled(&ctx, char1, 100, true, false);
    let c_only = ctx.drain_notifications_for_char(char1);
    assert_eq!(c_only.len(), 1);
    assert_eq!(c_only[0].event, "OnJumpCloakCancelled");

    emit_warp_cloak_or_invuln_cancelled(&ctx, char1, 100, false, true);
    let i_only = ctx.drain_notifications_for_char(char1);
    assert_eq!(i_only.len(), 1);
    assert_eq!(i_only[0].event, "OnInvulnCancelled");

    // 3. emit_kill_notifications: distinct victim (char1) and attacker (char2) notifies both
    emit_kill_notifications(&ctx, char1, char2);
    assert_eq!(ctx.drain_notifications_for_char(char1).len(), 1);
    assert_eq!(ctx.drain_notifications_for_char(char2).len(), 1);

    // Victim <= 0 notifies only attacker
    emit_kill_notifications(&ctx, 0, char2);
    assert!(ctx.drain_notifications_for_char(char1).is_empty());
    assert_eq!(ctx.drain_notifications_for_char(char2).len(), 1);

    // 4. emit_ecm_jam_landed with duration_s < 1.0 clamps durationMs to 1000.0
    emit_ecm_jam_landed(&ctx, sys_id, char1, 101, 202, 303, 0.2);
    let jam_n = ctx.drain_notifications_for_char(char1);
    assert_eq!(jam_n.len(), 1);
    assert_eq!(jam_n[0].event, "OnJamStart");
    assert_eq!(jam_n[0].args[5], PyRep::Real(1_000.0));
}
