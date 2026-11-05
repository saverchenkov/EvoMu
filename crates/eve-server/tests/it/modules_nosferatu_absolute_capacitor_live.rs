//! Live Energy Nosferatu transfer rule (`feat:modules.energy-nosferatu-relative-capacitor`) with
//! player RPCs only. wiki:Capacitor_warfare: a Nosferatu transfers only while the user's capacitor
//! is lower than the target's, measured in absolute GJ rather than percentage. The test builds the
//! discriminating case live: a Rifter at a higher capacitor percentage but fewer GJ than a
//! self-drained Rupture, and observes the cycle-end transfer jump on both ships.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{buy, fund_pilots, login, move_item, rpc, train_skills, travel, warp_to, PILOT_1, PILOT_2};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;

const NOS_S: u32 = 530; // Small Energy Nosferatu I (8 GJ / 2.5 s)
const NEUT_S: u32 = 533; // Small Energy Neutralizer I (45 GJ / 6 s, 45 GJ activation)
const RUPTURE: u32 = 629;
const MSB: u32 = 10_836; // Medium Shield Booster I (60 GJ / 3 s)

async fn cap(sys: &eve_server::system_actor::SystemHandle, ship: i64) -> (f64, f64) {
    let cs = sys.get_combat_state(ship).await.unwrap().expect("combat state");
    (cs.capacitor, cs.max_capacitor)
}

async fn activate(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, module: i64, effect: &str, target: Option<i64>) {
    let mut args = vec![PyRep::Integer(module), PyRep::String(effect.into())];
    args.extend(target.map(PyRep::Integer));
    rpc(ctx, sess, "dogmaIM", "Activate", &args).await.unwrap_or_else(|e| panic!("Activate {effect}: {e:?}"));
}

async fn deactivate(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, module: i64, effect: &str) {
    rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(module), PyRep::String(effect.into())]).await.unwrap_or_else(|e| panic!("Deactivate {effect}: {e:?}"));
}

#[tokio::test]
async fn test_live_nosferatu_transfers_by_absolute_not_percentage_capacitor() {
    // parity: feat:modules.energy-nosferatu-relative-capacitor
    eve_testkit::attest(&["feat:modules.energy-nosferatu-relative-capacitor"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 1_000_000_000.0), (PILOT_2, 1_000_000_000.0)]).await;
    let mut vamp = login(&ctx, 8931, "demo", PILOT_1).await;
    let mut donor = login(&ctx, 8932, "other", PILOT_2).await;
    train_skills(&ctx, &mut vamp, &[NOS_S, NEUT_S], true).await;
    train_skills(&ctx, &mut donor, &[RUPTURE, MSB], false).await;

    // Vampire: demo Rifter with a Small Neutralizer and Small Nosferatu (market-bought, fitted docked).
    let v_ship = vamp.ship_id.unwrap();
    let nos = buy(&ctx, &mut vamp, NOS_S, 1).await[0];
    let neut = buy(&ctx, &mut vamp, NEUT_S, 1).await[0];
    move_item(&ctx, &mut vamp, nos, v_ship, 28).await;
    move_item(&ctx, &mut vamp, neut, v_ship, 29).await;
    // Donor: market Rupture with a Medium Shield Booster to drain its own capacitor.
    let rupture = buy(&ctx, &mut donor, RUPTURE, 1).await[0];
    rpc(&ctx, &mut donor, "ship", "AssembleShip", &[PyRep::Integer(rupture)]).await.expect("AssembleShip Rupture");
    ctx.advance_secs(12);
    rpc(&ctx, &mut donor, "ship", "Board", &[PyRep::Integer(rupture)]).await.expect("Board Rupture");
    let msb = buy(&ctx, &mut donor, MSB, 1).await[0];
    move_item(&ctx, &mut donor, msb, rupture, 19).await;

    let sun = ctx.sde.celestials_in_system(30_000_142).into_iter().find(|c| c.group_id == 6).expect("sun").celestial_id;
    for sess in [&mut vamp, &mut donor] {
        travel(&ctx, sess, 30_000_142).await;
        warp_to(&ctx, sess, sun, 100_000.0).await;
    }
    ctx.advance_secs(15);
    let sys = ctx.systems.get_or_spawn(30_000_142);
    for _ in 0..15 {
        let _ = sys.step_manual(1.0).await;
    }
    rpc(&ctx, &mut vamp, "dogmaIM", "AddTarget", &[PyRep::Integer(rupture)]).await.expect("lock Rupture");
    let _ = sys.step_manual(12.0).await;

    // 1. The Rupture burns its own capacitor with the shield booster down to ~55%.
    activate(&ctx, &mut donor, msb, "shieldBoosting", None).await;
    for _ in 0..300 {
        let _ = sys.step_manual(1.0).await;
        let (c, m) = cap(&sys, rupture).await;
        if c / m < 0.58 {
            break;
        }
    }
    deactivate(&ctx, &mut donor, msb, "shieldBoosting").await;
    let _ = sys.step_manual(3.5).await;

    // 2. One neutralizer cycle gives the Rifter room while it stays at a higher percentage.
    activate(&ctx, &mut vamp, neut, "energyNeutralizerFalloff", Some(rupture)).await;
    let _ = sys.step_manual(1.0).await;
    deactivate(&ctx, &mut vamp, neut, "energyNeutralizerFalloff").await;
    let _ = sys.step_manual(6.0).await;
    let ((v_cap, v_max), (d_cap, d_max)) = (cap(&sys, v_ship).await, cap(&sys, rupture).await);
    assert!(v_cap < d_cap, "Rifter has fewer GJ ({v_cap:.1}) than the Rupture ({d_cap:.1})");
    assert!(v_cap / v_max > d_cap / d_max, "Rifter percentage {:.3} above Rupture {:.3}", v_cap / v_max, d_cap / d_max);
    assert!(v_max - v_cap > 8.0, "Rifter has room for a Nosferatu transfer");

    // 3. Nosferatu: at the cycle end both capacitors jump by the 8 GJ transfer (absolute rule);
    //    a percentage rule would block it because the Rifter's percentage is higher.
    activate(&ctx, &mut vamp, nos, "energyNosferatuFalloff", Some(rupture)).await;
    let (mut prev_v, mut prev_d) = (cap(&sys, v_ship).await.0, cap(&sys, rupture).await.0);
    let mut jump = None;
    for _ in 0..40 {
        let _ = sys.step_manual(0.1).await;
        let (now_v, now_d) = (cap(&sys, v_ship).await.0, cap(&sys, rupture).await.0);
        if now_v - prev_v > 6.0 && prev_d - now_d > 6.0 {
            jump = Some((now_v - prev_v, prev_d - now_d));
            break;
        }
        (prev_v, prev_d) = (now_v, now_d);
    }
    deactivate(&ctx, &mut vamp, nos, "energyNosferatuFalloff").await;
    let (gain, loss) = jump.expect("Nosferatu transferred capacitor from the higher-GJ, lower-percentage target");
    assert!((gain - 8.0).abs() < 1.0 && (loss - 8.0).abs() < 1.0, "8 GJ transfer (gain {gain:.2}, loss {loss:.2})");
}
