//! `SO-5` dungeon instance cache: `dungeonInstanceCacheMgr.GetCombatAnomalyInstances`,
//! `GetOreAnomalyInstances`, `GetSignatureInstances` and `GetIceBeltInstances` against the real SDE.
//!
//! Ledger tags live inside each `#[tokio::test]` body next to its `eve_testkit::attest` call.

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_exploration::{
    advance_site_wave_or_complete, with_pve_exploration_reg,
};
use eve_server::services::mining_sites::displayed_security;
use eve_server::services::pve_dungeon_cache::{parse_dng_site_id, registry_instance_id};
use glam::DVec3;
use std::collections::{BTreeMap, BTreeSet, HashMap};

const HEK: u32 = 30_002_053; // displayed 0.8 high-sec
const TAMA: u32 = 30_002_813; // displayed 0.3 low-sec
const VFK: u32 = 30_002_904; // null-sec
const ARCHETYPE_ICE_BELT: u32 = 28;
const ARCHETYPE_ORE_ANOMALY: u32 = 27;

async fn login(ctx: &ServerContext, session_id: u64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

async fn in_space(ctx: &ServerContext, session_id: u64, sys: u32, ship_id: i64) -> SessionState {
    let mut sess = login(ctx, session_id).await;
    sess.solar_system_id = Some(sys);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: sys as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let _ = ctx
        .systems
        .get_or_spawn(sys)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 1.0,
            mass: 1_067_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    sess
}

/// Calls a `dungeonInstanceCacheMgr` method with no arguments (as the client does) and unpacks the
/// `{solarSystemID: {dungeonID: [instanceID]}}` reply.
async fn instances(
    ctx: &ServerContext,
    sess: &mut SessionState,
    method: &str,
) -> BTreeMap<u32, BTreeMap<u32, BTreeSet<i64>>> {
    let rep = ctx
        .dispatch_rpc(sess, "dungeonInstanceCacheMgr", None, method, &[], &HashMap::new(), None)
        .await
        .unwrap_or_else(|e| panic!("{method}: {e:?}"));
    let PyRep::Dict(outer) = rep else {
        panic!("{method} must return a dict keyed by solarSystemID, got {rep:?}");
    };
    let mut out = BTreeMap::new();
    for (k, v) in outer {
        let sys = k.as_i64().expect("solarSystemID key") as u32;
        let PyRep::Dict(inner) = v else {
            panic!("{method}: per-system value must be a dict keyed by dungeonID");
        };
        let mut per = BTreeMap::new();
        for (dk, dv) in inner {
            let did = dk.as_i64().expect("dungeonID key") as u32;
            let PyRep::List(ids) = dv else {
                panic!("{method}: instance list expected");
            };
            let set: BTreeSet<i64> = ids.iter().map(|i| i.as_i64().expect("instanceID")).collect();
            assert_eq!(set.len(), ids.len(), "{method}: duplicate instance IDs");
            per.insert(did, set);
        }
        out.insert(sys, per);
    }
    out
}

fn archetype(ctx: &ServerContext, did: u32) -> Option<u32> {
    ctx.sde.dungeons.get(&did).and_then(|d| d.archetype_id)
}

/// Independent re-read of the ice security ceiling from the SDE description text.
fn ice_ceiling(ctx: &ServerContext, type_id: u32) -> f64 {
    let desc = &ctx.sde.types.get(&type_id).expect("ice type in SDE").description;
    let at = desc.find("Available in").expect("ice description publishes a security band");
    let tail = &desc[at..];
    let close = tail.find("</color>").expect("colour-tagged security value");
    let open = tail[..close].rfind('>').expect("colour tag open");
    tail[open + 1..close].trim().parse().expect("numeric ceiling")
}

fn expected_registry(ctx: &ServerContext, sys: u32, probe: bool) -> BTreeMap<u32, BTreeSet<i64>> {
    with_pve_exploration_reg(ctx, |reg| {
        let mut out: BTreeMap<u32, BTreeSet<i64>> = BTreeMap::new();
        for s in reg.sites_by_system.get(&sys).into_iter().flatten() {
            if s.probe_required == probe && !s.cleared {
                out.entry(s.dungeon_id).or_default().insert(registry_instance_id(&s.site_id));
            }
        }
        out
    })
}

fn expected_anomalies(ctx: &ServerContext, sys: u32, kind: &str) -> BTreeMap<u32, BTreeSet<i64>> {
    ctx.db.with_snapshot(|snap| {
        let mut out: BTreeMap<u32, BTreeSet<i64>> = BTreeMap::new();
        for a in snap.cosmic_anomalies.get(&sys).into_iter().flatten() {
            if a.anomaly_type == kind && !a.cleared {
                let (d, i) = parse_dng_site_id(&a.anomaly_id).expect("DNG site id");
                out.entry(d).or_default().insert(i);
            }
        }
        out
    })
}

#[tokio::test]
async fn test_dungeon_instance_cache_returns_current_per_system_instances() {
    // parity: feat:pve.dungeon-instance-cache, rpc:dungeonInstanceCacheMgr.GetOreAnomalyInstances
    eve_testkit::attest(&["feat:pve.dungeon-instance-cache"]);

    let ctx = ServerContext::new();
    assert!(
        ctx.sde.dungeons.len() > 1000 && !ctx.sde.dungeon_asteroids.is_empty(),
        "real SDE (EVOMU_SDE_PATH) must be loaded"
    );

    // --- Combat anomalies: current, uncleared, overview-visible exploration sites of this system.
    let mut hek = in_space(&ctx, 9601, HEK, 760_000_001).await;
    let combat = instances(&ctx, &mut hek, "GetCombatAnomalyInstances").await;
    assert_eq!(combat.keys().copied().collect::<Vec<_>>(), vec![HEK], "only the session system");
    let hek_combat = combat[&HEK].clone();
    assert!(!hek_combat.is_empty(), "Hek has combat anomalies");
    assert_eq!(hek_combat, expected_registry(&ctx, HEK, false));
    for did in hek_combat.keys() {
        assert!(ctx.sde.dungeons.contains_key(did), "combat dungeon {did} is a real SDE dungeon");
    }

    // Complete one anomaly through the server's wave/complete path: it must drop out of the cache.
    let (victim_site, victim_did) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&HEK].iter().find(|s| !s.probe_required && !s.cleared).unwrap();
        (s.site_id.clone(), s.dungeon_id)
    });
    let victim_iid = registry_instance_id(&victim_site);
    assert!(hek_combat[&victim_did].contains(&victim_iid));
    let hek_sys = ctx.systems.get_or_spawn(HEK);
    for _ in 0..10 {
        let wave_bids: Vec<i64> = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system[&HEK].iter().find(|s| s.site_id == victim_site).map(|s| s.active_npc_ball_ids.clone()).unwrap_or_default()
        });
        for bid in wave_bids {
            let _ = hek_sys.remove_ball(bid).await;
        }
        advance_site_wave_or_complete(&ctx, &mut hek, HEK, &victim_site).await.expect("advance");
        let cleared = with_pve_exploration_reg(&ctx, |reg| {
            reg.sites_by_system[&HEK].iter().any(|s| s.site_id == victim_site && s.cleared)
        });
        if cleared {
            break;
        }
    }
    let combat_after = instances(&ctx, &mut hek, "GetCombatAnomalyInstances").await;
    assert!(
        !combat_after[&HEK].get(&victim_did).is_some_and(|ids| ids.contains(&victim_iid)),
        "completed anomaly {victim_site} must no longer be listed"
    );
    assert_eq!(combat_after[&HEK], expected_registry(&ctx, HEK, false));

    // --- Ore anomalies: SDE-seeded archetype-27 deposits of this system.
    let ore = instances(&ctx, &mut hek, "GetOreAnomalyInstances").await;
    assert_eq!(ore.keys().copied().collect::<Vec<_>>(), vec![HEK]);
    let hek_ore = ore[&HEK].clone();
    assert!(!hek_ore.is_empty(), "Hek (0.8) seeds a mid high-sec ore anomaly");
    assert_eq!(hek_ore, expected_anomalies(&ctx, HEK, "OreAnomaly"));
    for did in hek_ore.keys() {
        assert_eq!(archetype(&ctx, *did), Some(ARCHETYPE_ORE_ANOMALY), "ore dungeon {did}");
    }
    // Depleting the deposit (mining sets `cleared`) removes it from the cache.
    let (ore_did, ore_iid) = {
        let (d, ids) = hek_ore.iter().next().unwrap();
        (*d, *ids.iter().next().unwrap())
    };
    ctx.db.with_mut_snapshot(|snap| {
        for a in snap.cosmic_anomalies.get_mut(&HEK).unwrap() {
            if parse_dng_site_id(&a.anomaly_id) == Some((ore_did, ore_iid)) {
                a.cleared = true;
            }
        }
    });
    let ore_after = instances(&ctx, &mut hek, "GetOreAnomalyInstances").await;
    assert!(!ore_after[&HEK].get(&ore_did).is_some_and(|ids| ids.contains(&ore_iid)));

    // --- Signatures: probe-only exploration sites plus SDE-seeded ore/gas signatures.
    let sigs = instances(&ctx, &mut hek, "GetSignatureInstances").await;
    assert_eq!(sigs.keys().copied().collect::<Vec<_>>(), vec![HEK]);
    let mut expected_sigs = expected_registry(&ctx, HEK, true);
    assert!(!expected_sigs.is_empty(), "Hek has probe-only exploration signatures");
    ctx.db.with_snapshot(|snap| {
        for s in snap.cosmic_signatures.get(&HEK).into_iter().flatten() {
            if matches!(s.sig_group.as_str(), "Ore" | "Gas") && !s.collapsed {
                let (d, i) = parse_dng_site_id(&s.signature_id).unwrap();
                expected_sigs.entry(d).or_default().insert(i);
            }
        }
    });
    assert_eq!(sigs[&HEK], expected_sigs);
    assert!(
        sigs[&HEK].keys().any(|d| archetype(&ctx, *d) == Some(81)),
        "hidden ore signature (SDE archetype 81) is listed with the exploration signatures"
    );
    for did in sigs[&HEK].keys() {
        assert!(ctx.sde.dungeons.contains_key(did));
    }

    // --- Ice belts: archetype-28 SDE dungeons whose every ice type is allowed at the system's band.
    let mut checked_bands = Vec::new();
    for (sid, sys, ship) in [(9602u64, HEK, 760_000_002i64), (9603, TAMA, 760_000_003), (9604, VFK, 760_000_004)] {
        let mut s = in_space(&ctx, sid, sys, ship).await;
        let ice = instances(&ctx, &mut s, "GetIceBeltInstances").await;
        assert_eq!(ice.keys().copied().collect::<Vec<_>>(), vec![sys]);
        let belts = &ice[&sys];
        assert_eq!(belts.len(), 1, "one seeded ice belt per known-space system: {belts:?}");
        assert_eq!(belts, &expected_anomalies(&ctx, sys, "IceBelt"));
        let did = *belts.keys().next().unwrap();
        assert_eq!(archetype(&ctx, did), Some(ARCHETYPE_ICE_BELT), "ice dungeon {did}");
        let disp = displayed_security(ctx.sde.solar_systems[&sys].security_status);
        let ceilings: Vec<f64> = ctx.sde.dungeon_asteroids[&did]
            .iter()
            .map(|r| ice_ceiling(&ctx, r.type_id))
            .collect();
        assert!(ceilings.iter().all(|c| disp <= c + 1e-9), "{sys}: {did} ice allowed at {disp}");
        let tightest = ceilings.iter().copied().fold(f64::INFINITY, f64::min);
        checked_bands.push((sys, tightest));
        // Re-query is stable (idempotent seeding) and the instance is the same.
        let again = instances(&ctx, &mut s, "GetIceBeltInstances").await;
        assert_eq!(again, ice);
    }
    // High-sec belts carry only 1.0-ceiling ice, low-sec reaches 0.4 ice, null-sec 0.0 ice.
    assert_eq!(checked_bands, vec![(HEK, 1.0), (TAMA, 0.4), (VFK, 0.0)]);

    // Per-system: a Tama session sees Tama's instances, not Hek's.
    let mut tama = in_space(&ctx, 9605, TAMA, 760_000_005).await;
    let tama_combat = instances(&ctx, &mut tama, "GetCombatAnomalyInstances").await;
    assert_eq!(tama_combat.keys().copied().collect::<Vec<_>>(), vec![TAMA]);
    assert_eq!(tama_combat[&TAMA], expected_registry(&ctx, TAMA, false));
    let hek_ids: BTreeSet<i64> = hek_combat.values().flatten().copied().collect();
    assert!(tama_combat[&TAMA].values().flatten().all(|i| !hek_ids.contains(i)));

    // No character on the session -> error, never an empty fabricated reply.
    let mut anon = SessionState::new(9607);
    assert!(ctx
        .dispatch_rpc(&mut anon, "dungeonInstanceCacheMgr", None, "GetOreAnomalyInstances", &[], &HashMap::new(), None)
        .await
        .is_err());
}

#[tokio::test]
// parity: rpc:custom_agency_provider.warp_to_content
// parity: rpc:keeper.GotoRoom
// parity: rpc:keeper.PlayDungeon
async fn test_agency_warp_and_keeper_room_navigation() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let ship_id = 760_000_101i64;
    let mut sess = in_space(&ctx, 9608, HEK, ship_id).await;

    // 1. custom_agency_provider.warp_to_content
    // Retrieve content data in HEK
    let content_rep = ctx
        .dispatch_rpc(&mut sess, "custom_agency_provider", None, "get_content_data", &[], &kw, None)
        .await
        .expect("get_content_data");
    let PyRep::List(ref pieces) = content_rep else {
        panic!("Expected List of content pieces");
    };
    assert!(!pieces.is_empty(), "Seeded content pieces must exist in Hek");

    // Warp to first content piece
    let warp_rep = ctx
        .dispatch_rpc(&mut sess, "custom_agency_provider", None, "warp_to_content", &[pieces[0].clone()], &kw, None)
        .await
        .expect("warp_to_content");
    assert_eq!(warp_rep, PyRep::None);

    // Verify ship is in warp in Ballpark to the authentic server site position
    let hek_handle = ctx.systems.get_or_spawn(HEK);
    let (_, ship_ball) = hek_handle.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball in space");
    let target_iid = pieces[0].dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();
    let expected_pos = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system
            .get(&HEK)
            .and_then(|sites| {
                sites.iter().find(|s| eve_server::services::pve_dungeon_cache::registry_instance_id(&s.site_id) == target_iid)
            })
            .map(|s| glam::DVec3::from_array(s.position))
    }).expect("resolved server site position");

    match ship_ball.mode {
        MovementMode::Warp { destination, .. } => {
            assert_eq!(destination, expected_pos, "Warp destination must match server site position");
            assert_ne!(destination, glam::DVec3::new(250_000_000.0, 0.0, 250_000_000.0), "Must not use hardcoded fallback");
        }
        ref other => panic!("Ship must be in Warp mode, got {other:?}"),
    }

    // Attempting to warp to a non-existent site returns SiteNotFound
    let non_existent = PyRep::keyval([
        ("instanceID", PyRep::Integer(999_999_999)),
        ("dungeonID", PyRep::Integer(999_999)),
    ]);
    let not_found_err = ctx
        .dispatch_rpc(&mut sess, "custom_agency_provider", None, "warp_to_content", &[non_existent], &kw, None)
        .await
        .unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(not_found_err.code, "SiteNotFound");

    // 2. keeper.PlayDungeon and keeper.GotoRoom (requires GM role)
    sess.role = eve_db::service_roles::ROLE_GM_DEFAULT;

    let created_room = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "EditRoom",
            &[PyRep::Integer(43), PyRep::String("Room 1".into())],
            &kw,
            None,
        )
        .await
        .expect("EditRoom 1");
    let room1_id = created_room.dict_get("roomID").and_then(|v| v.as_i64()).expect("room1 ID");

    let created_room2 = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "EditRoom",
            &[PyRep::Integer(43), PyRep::String("Room 2".into())],
            &kw,
            None,
        )
        .await
        .expect("EditRoom 2");
    let room2_id = created_room2.dict_get("roomID").and_then(|v| v.as_i64()).expect("room2 ID");

    // PlayDungeon: starts playing dungeon 43 in room 1
    let play_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "PlayDungeon",
            &[PyRep::Integer(43), PyRep::Integer(room1_id)],
            &kw,
            None,
        )
        .await
        .expect("PlayDungeon");
    assert_eq!(play_rep.dict_get("dungeonID"), Some(&PyRep::Integer(43)));
    assert_eq!(play_rep.dict_get("roomID"), Some(&PyRep::Integer(room1_id)));

    // GotoRoom: transitions active dungeon to room 2
    let goto_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "GotoRoom",
            &[PyRep::Integer(room2_id)],
            &kw,
            None,
        )
        .await
        .expect("GotoRoom");
    assert_eq!(goto_rep.dict_get("dungeonID"), Some(&PyRep::Integer(43)));
    assert_eq!(goto_rep.dict_get("roomID"), Some(&PyRep::Integer(room2_id)));
    assert_eq!(goto_rep.dict_get("roomName"), Some(&PyRep::String("Room 2".into())));
}
