//! Tests concurrent linking of ESS Main Bank and ESS Reserve Bank by distinct pilots.
//! Verifies that a reserve bank link does not overwrite or disrupt an active main bank link.

use std::collections::HashMap;
use std::sync::Arc;

use eve_destiny::{Ball, MovementMode};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;

const NULLSEC_SYS_ID: u32 = 30_004_759;
const PILOT_A: i64 = 100_001;
const PILOT_B: i64 = 100_002;
const SHIP_A: i64 = 910_001;
const SHIP_B: i64 = 910_002;
const ESS_BALL_ID: i64 = 940_010;
const ESS_BEACON_TYPE_ID: u32 = 32882;
const RIFTER_TYPE_ID: u32 = 587;
const KEY_15M_TYPE_ID: u32 = 57720;

fn kv_map(rep: &PyRep) -> HashMap<String, PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.clone(),
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) | PyRep::Token(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect(),
        _ => HashMap::new(),
    }
}

// parity: feat:pve.ess-reserve, feat:pve.ess-main-bank

#[tokio::test]
async fn test_ess_concurrent_main_and_reserve_links() {
    let ctx = Arc::new(ServerContext::new());
    let sys_handle = ctx.systems.get_or_spawn(NULLSEC_SYS_ID);

    let mut sess_a = eve_machonet::SessionState::new(3101);
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(PILOT_A);
    sess_a.solar_system_id = Some(NULLSEC_SYS_ID);
    sess_a.ship_id = Some(SHIP_A);

    let mut sess_b = eve_machonet::SessionState::new(3102);
    sess_b.user_id = Some(2);
    sess_b.character_id = Some(PILOT_B);
    sess_b.solar_system_id = Some(NULLSEC_SYS_ID);
    sess_b.ship_id = Some(SHIP_B);

    let (char_a, char_b) = ctx.db.with_snapshot(|snap| {
        let base = snap.characters.values().next().cloned().unwrap_or_else(|| {
            panic!("Expected at least one character in demo seed");
        });
        let mut a = base.clone();
        a.character_id = PILOT_A;
        a.user_id = 1;
        a.name = "Pilot A".into();
        a.active_ship_id = SHIP_A;
        a.solar_system_id = NULLSEC_SYS_ID;
        a.station_id = None;
        a.structure_id = None;
        a.balance = eve_db::IskCents(0);

        let mut b = base;
        b.character_id = PILOT_B;
        b.user_id = 2;
        b.name = "Pilot B".into();
        b.active_ship_id = SHIP_B;
        b.solar_system_id = NULLSEC_SYS_ID;
        b.station_id = None;
        b.structure_id = None;
        b.balance = eve_db::IskCents(0);

        (a, b)
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.insert(PILOT_A, char_a);
        snap.characters.insert(PILOT_B, char_b);
        snap.system_states.insert(
            NULLSEC_SYS_ID,
            eve_db::SystemDynamicState {
                solar_system_id: NULLSEC_SYS_ID,
                dbs_multiplier: 1.35,
                ess_main_bank: eve_db::IskCents::from_f64_isk(50_000_000.0),
                ess_reserve_bank: eve_db::IskCents::from_f64_isk(100_000_000.0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );

        for (sid, pid) in [(SHIP_A, PILOT_A), (SHIP_B, PILOT_B)] {
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: RIFTER_TYPE_ID,
                    owner_id: pid,
                    location_id: NULLSEC_SYS_ID as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }

        // Pilot B has a reserve key in cargo
        let key_item_id = 950_888;
        snap.items.insert(
            key_item_id,
            eve_db::InventoryItemRecord {
                item_id: key_item_id,
                type_id: KEY_15M_TYPE_ID,
                owner_id: PILOT_B,
                location_id: SHIP_B,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ESS_BALL_ID,
                type_id: ESS_BEACON_TYPE_ID,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000_000.0,
                radius: 500.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(ESS_BEACON_TYPE_ID),
        )
        .await
        .unwrap();

    for (sid, pos_x) in [(SHIP_A, 1_000.0), (SHIP_B, 2_000.0)] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id: sid,
                    type_id: RIFTER_TYPE_ID,
                    position: DVec3::new(pos_x, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 35.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(RIFTER_TYPE_ID),
            )
            .await
            .unwrap();
    }

    let kw = HashMap::new();

    // 1. Pilot A links to Main Bank
    let link_a_rep = ctx
        .dispatch_rpc(
            &mut sess_a,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("Pilot A AttemptLinkToMainBank start");
    let kv_a = kv_map(&link_a_rep);
    assert_eq!(kv_a.get("linkStartedOnly"), Some(&PyRep::Bool(true)));

    // Verify DB state: Pilot A is linked to Main Bank
    let main_linked_char = ctx.db.with_snapshot(|s| {
        s.system_states.get(&NULLSEC_SYS_ID).and_then(|st| st.ess_linked_character_id)
    });
    assert_eq!(main_linked_char, Some(PILOT_A));

    // 2. Pilot B links to Reserve Bank concurrently
    let link_b_rep = ctx
        .dispatch_rpc(
            &mut sess_b,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("Pilot B AttemptLinkToReserveBank start");
    let kv_b = kv_map(&link_b_rep);
    assert_eq!(kv_b.get("linkStartedOnly"), Some(&PyRep::Bool(true)));

    // Verify CRITICAL invariant: Pilot B's reserve link did NOT overwrite Pilot A's main bank link!
    let (main_linked_char_after, reserve_linked_char) = ctx.db.with_snapshot(|s| {
        let pulse_st = eve_server::services::space_destiny::get_ess_reserve_pulses_from_blobs(
            &s.service_state_blobs,
            NULLSEC_SYS_ID,
        );
        let st_opt = s.system_states.get(&NULLSEC_SYS_ID);
        (st_opt.and_then(|st| st.ess_linked_character_id), pulse_st.reserve_linked_character_id)
    });
    assert_eq!(main_linked_char_after, Some(PILOT_A), "Main bank link must still belong to Pilot A");
    assert_eq!(reserve_linked_char, Some(PILOT_B), "Reserve bank link must belong to Pilot B");

    // 3. Step 5 ticks
    for _ in 0..5 {
        sys_handle.step_manual(1.0).await.unwrap();
    }

    // 4. Pilot A completes Main Bank payout
    let complete_a = ctx
        .dispatch_rpc(
            &mut sess_a,
            "essMgr",
            None,
            "AttemptLinkToMainBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("Pilot A AttemptLinkToMainBank complete");
    let kv_comp_a = kv_map(&complete_a);
    assert_eq!(kv_comp_a.get("linkStartedOnly"), Some(&PyRep::Bool(false)));
    assert!(kv_comp_a.get("stolenISK").or_else(|| kv_comp_a.get("amount")).and_then(PyRep::as_f64).unwrap_or(0.0) > 0.0);

    // 5. Pilot B completes Reserve Bank payout
    let complete_b = ctx
        .dispatch_rpc(
            &mut sess_b,
            "essMgr",
            None,
            "AttemptLinkToReserveBank",
            &[PyRep::Integer(ESS_BALL_ID)],
            &kw,
            None,
        )
        .await
        .expect("Pilot B AttemptLinkToReserveBank complete");
    let kv_comp_b = kv_map(&complete_b);
    assert_eq!(kv_comp_b.get("linkStartedOnly"), Some(&PyRep::Bool(false)));
    let stolen_b = kv_comp_b.get("stolenISK").or_else(|| kv_comp_b.get("amount")).and_then(PyRep::as_f64).unwrap_or(0.0);
    // Payout for first pulse must be initial_reserve_bank / pulses (100,000,000 / 15 = 6,666,666.67) rather than the whole bank
    let expected_first_pulse = 100_000_000.0 / 15.0;
    assert!((stolen_b - expected_first_pulse).abs() < 1.0, "Payout must be reserve_bank / pulses (expected ~{expected_first_pulse}, got {stolen_b})");

    // Verify both pilots received ISK
    let (bal_a, bal_b, rem_reserve) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&PILOT_A).map(|c| c.balance.0).unwrap_or(0),
            s.characters.get(&PILOT_B).map(|c| c.balance.0).unwrap_or(0),
            s.system_states.get(&NULLSEC_SYS_ID).map(|st| st.ess_reserve_bank.0).unwrap_or(0),
        )
    });
    assert!(bal_a > 0, "Pilot A must have received Main Bank ISK");
    assert!((bal_b as f64 / 100.0 - expected_first_pulse).abs() < 1.0, "Pilot B must have received one pulse worth of Reserve Bank ISK");
    assert!(rem_reserve > 0, "Reserve bank must still have remaining pulses/funds");
}
