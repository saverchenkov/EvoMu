//! Live Mobile Depot lifecycle (`feat:modules.mobile-depot-deployment-and-refitting`) with player
//! RPCs only: market-bought depot and ammo, deployment via `posMgr.AnchorStructure`, real weapon
//! fire (`dogmaIM.Activate`), owner refitting within 2,500 m, and the wiki:Mobile_structures rules:
//! targetable and damageable during the 60 s anchoring period, reinforcement for 2 days once
//! anchored and shot down to 25% shield, invulnerable while reinforced.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{buy, fund_pilots, login, move_item, rpc, train_skills, travel, warp_to, JITA_STATION, PILOT_1, PILOT_2};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables as depots;
use eve_server::world_seeder::seed_full_market_catalog;

const MOBILE_DEPOT: u32 = 33_474;
const EMP_S: u32 = 185;
const SHIELD_EXTENDER: u32 = 377;
const AUTOCANNON: i64 = 100_000_006; // PILOT_2's fitted 200mm AutoCannon II (demo seed)

fn depot_state(ctx: &ServerContext, depot: i64) -> depots::MobileDeployableRecord {
    ctx.db.with_snapshot(|s| depots::read_deployable_state_snap(s).deployables.get(&depot).cloned()).expect("deployable record")
}

#[tokio::test]
async fn test_live_mobile_depot_anchoring_refit_perimeter_and_25pct_reinforcement() {
    // parity: feat:modules.mobile-depot-deployment-and-refitting
    eve_testkit::attest(&["feat:modules.mobile-depot-deployment-and-refitting"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 1_000_000_000.0), (PILOT_2, 1_000_000_000.0)]).await;
    let mut owner = login(&ctx, 8921, "demo", PILOT_1).await;
    let mut gunner = login(&ctx, 8922, "other", PILOT_2).await;
    train_skills(&ctx, &mut owner, &[SHIELD_EXTENDER], true).await;

    let (g_ship, o_ship) = (gunner.ship_id.unwrap(), owner.ship_id.unwrap());
    let ammo = buy(&ctx, &mut gunner, EMP_S, 1_000).await[0];
    move_item(&ctx, &mut gunner, ammo, g_ship, 5).await;
    rpc(&ctx, &mut gunner, "dogmaIM", "LoadAmmo", &[PyRep::Integer(g_ship), PyRep::List(vec![PyRep::Integer(AUTOCANNON)]), PyRep::List(vec![PyRep::Integer(ammo)])])
        .await
        .expect("LoadAmmo");
    let depot = buy(&ctx, &mut owner, MOBILE_DEPOT, 1).await[0];
    let refit_mod = buy(&ctx, &mut owner, SHIELD_EXTENDER, 1).await[0];
    move_item(&ctx, &mut owner, depot, o_ship, 5).await;
    move_item(&ctx, &mut owner, refit_mod, o_ship, 5).await;

    // Both fly out to a safe spot 100 km off the Jita sun.
    let sun = ctx.sde.celestials_in_system(30_000_142).into_iter().find(|c| c.group_id == 6).expect("sun").celestial_id;
    for sess in [&mut owner, &mut gunner] {
        travel(&ctx, sess, 30_000_142).await;
        warp_to(&ctx, sess, sun, 100_000.0).await;
    }
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let o_pos = sys.get_ball_snapshot(o_ship).await.unwrap().unwrap().1.position;
    let dep_pos = o_pos + glam::DVec3::new(500.0, 0.0, 0.0);
    let dep = rpc(&ctx, &mut owner, "posMgr", "AnchorStructure", &[PyRep::Integer(depot), PyRep::Tuple(vec![PyRep::Real(dep_pos.x), PyRep::Real(dep_pos.y), PyRep::Real(dep_pos.z)])])
        .await
        .expect("deploy Mobile Depot");
    assert_eq!(dep.dict_get("anchoringDelaySeconds").and_then(|v| v.as_f64()), Some(60.0), "60 s anchoring");

    // During anchoring the depot can be targeted and damaged (no invulnerability).
    let shield0 = sys.get_combat_state(depot).await.unwrap().unwrap().shield_hp;
    rpc(&ctx, &mut gunner, "dogmaIM", "AddTarget", &[PyRep::Integer(depot)]).await.expect("lock anchoring depot");
    let _ = sys.step_manual(12.0).await;
    rpc(&ctx, &mut gunner, "dogmaIM", "Activate", &[PyRep::Integer(AUTOCANNON), PyRep::String("projectileFired".into()), PyRep::Integer(depot)])
        .await
        .expect("fire on anchoring depot");
    let _ = sys.step_manual(3.0).await;
    let _ = rpc(&ctx, &mut gunner, "dogmaIM", "Deactivate", &[PyRep::Integer(AUTOCANNON), PyRep::String("projectileFired".into())]).await;
    assert!(sys.get_combat_state(depot).await.unwrap().unwrap().shield_hp < shield0, "anchoring depot takes damage");
    assert!(!depot_state(&ctx, depot).anchor_completed);

    // Anchoring completes after 60 s; the owner refits within 2,500 m.
    ctx.advance_secs(61);
    let _ = sys.step_manual(1.0).await;
    assert!(depot_state(&ctx, depot).anchor_completed, "anchored after 60 s");
    rpc(&ctx, &mut owner, "invbroker", "Add", &[PyRep::Integer(refit_mod), PyRep::Integer(o_ship), PyRep::Integer(19)])
        .await
        .expect("refit mid slot next to depot");
    rpc(&ctx, &mut owner, "invbroker", "Add", &[PyRep::Integer(refit_mod), PyRep::Integer(o_ship), PyRep::Integer(5)])
        .await
        .expect("unfit back to cargo next to depot");

    // Shooting the anchored depot down to 25% shield starts a 2-day reinforcement; no further damage.
    let max_shield = sys.get_combat_state(depot).await.unwrap().unwrap().max_shield_hp;
    ctx.advance_secs(15);
    rpc(&ctx, &mut gunner, "dogmaIM", "Activate", &[PyRep::Integer(AUTOCANNON), PyRep::String("projectileFired".into()), PyRep::Integer(depot)])
        .await
        .expect("open fire on anchored depot");
    let mut reinforced_at = None;
    for _ in 0..600 {
        let _ = sys.step_manual(5.0).await;
        ctx.advance_secs(5);
        if depot_state(&ctx, depot).reinforced_until_unix.is_some() {
            reinforced_at = Some(ctx.now_unix());
            break;
        }
    }
    let reinforced_at = reinforced_at.expect("depot entered reinforcement");
    let until = depot_state(&ctx, depot).reinforced_until_unix.unwrap();
    assert!(until >= reinforced_at + 2 * 86_400 - 10 && until <= reinforced_at + 2 * 86_400, "2-day reinforcement timer");
    let held = sys.get_combat_state(depot).await.unwrap().unwrap();
    assert!((held.shield_hp - 0.25 * max_shield).abs() < 1.0, "shield held at 25% ({} of {max_shield})", held.shield_hp);
    for _ in 0..12 {
        let _ = sys.step_manual(5.0).await;
        ctx.advance_secs(5);
    }
    let still = sys.get_combat_state(depot).await.unwrap().unwrap();
    assert!(still.shield_hp >= held.shield_hp && still.armor_hp >= held.armor_hp, "invulnerable while reinforced (only passive regen)");
    assert_eq!(still.structure_hp, held.structure_hp);
    let _ = rpc(&ctx, &mut gunner, "dogmaIM", "Deactivate", &[PyRep::Integer(AUTOCANNON), PyRep::String("projectileFired".into())]).await;

    // Leaving the 2,500 m fitting perimeter blocks refitting.
    warp_to(&ctx, &mut owner, JITA_STATION, 0.0).await;
    let far = rpc(&ctx, &mut owner, "invbroker", "Add", &[PyRep::Integer(refit_mod), PyRep::Integer(o_ship), PyRep::Integer(19)]).await;
    assert!(far.is_err(), "no refitting beyond 2,500 m of the depot");
}
