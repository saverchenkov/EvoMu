//! Unit tests for `sov_fw_insurgency` (Sov prospecting deposits, site seeding, FW direct enlistment, Sov info RPCs).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::sov_services::{TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB};
use eve_db::UpwellStructureRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "sov_fw_insurgency_extra_unit_tests.rs"]
mod sov_fw_insurgency_extra_unit_tests;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_prospecting_deposit_respawn_and_site_seeding() {
    let ctx = test_ctx();
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_000_142);

    let pos = derive_system_site_position_from_sde(&ctx, null_sys, 2, 1.5);
    assert!(pos.iter().all(|v| v.is_finite()));

    // Seed an active SovOre anomaly + depleted asteroid -> schedules respawn, then respawns when due
    let base_id = 1_950_000_100_i64;
    let ast_id = base_id + 1;
    let ore_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 25)
        .map(|t| t.type_id)
        .unwrap_or(1230);

    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_prospecting_levels.insert(null_sys, 2);
        snap.cosmic_anomalies
            .entry(null_sys)
            .or_default()
            .push(eve_db::CosmicAnomalyRecord {
                anomaly_id: "SOV-ORE-001".to_string(),
                solar_system_id: null_sys,
                name: "Sov Prospecting Array".to_string(),
                anomaly_type: "SovOre".to_string(),
                position: pos,
                signal_strength: 1.0,
                ore_types: vec![ore_tid],
                remaining_volume: 0.0,
                created_unix: ctx.now_unix().saturating_sub(40_000),
                lifetime_seconds: 86_400,
                cleared: false,
            });
        snap.asteroids.insert(
            ast_id,
            eve_db::AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: null_sys,
                belt_id: 0,
                type_id: ore_tid,
                position: pos.into(),
                radius: 250.0,
                quantity: 0,
                initial_quantity: 10_000,
            },
        );
    });

    // First tick with 0 asteroids remaining marks SovOre anomaly cleared = true
    tick_sov_prospecting_deposit_respawn(&ctx, null_sys).await;
    ctx.db.with_snapshot(|snap| {
        let anoms = &snap.cosmic_anomalies[&null_sys];
        assert!(anoms.iter().any(|a| a.cleared));
    });

    // seed_system_mining_sites_from_sde seeds belts/anomalies/prospecting sites
    seed_system_mining_sites_from_sde(&ctx, null_sys).await;
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.cosmic_anomalies.get(&null_sys).unwrap().is_empty());
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_direct_enlistment_and_sov_mgr_info_rpcs() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    let ally_id = 99_000_033_i64;
    let hub_id = 1_095_000_001_i64;
    let skyhook_id = 1_095_000_002_i64;
    let kw = HashMap::new();

    let (corp_id, militia_faction) = ctx.db.with_mut_snapshot(|snap| {
        let cid = snap.characters[&char_id].corporation_id;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = None;
            ch.corporation_id = 1_000_009; // NPC corp for direct enlistment
        }
        if let Some(c) = snap.corporations.get_mut(&cid) {
            c.ceo_id = char_id;
            c.alliance_id = Some(ally_id);
        }
        snap.alliances.insert(
            ally_id,
            eve_db::AllianceRecord {
                alliance_id: ally_id,
                name: "Sov Test Alliance".to_string(),
                short_name: "SOVT".to_string(),
                executor_corp_id: cid,
                ..Default::default()
            },
        );
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.sov_alliance_id = Some(ally_id);
        st.military_index = 2;
        st.industrial_index = 1;
        st.strategic_index = 1;

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: cid,
                solar_system_id: sys_id,
                name: "Info Sov Hub".to_string(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: cid,
                solar_system_id: sys_id,
                name: "Info Skyhook".to_string(),
                position: [10_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".to_string(),
                ..Default::default()
            },
        );
        snap.sov_runtime
            .skyhook_reagents
            .insert(skyhook_id, (81143, 300));
        let fac = ctx
            .sde
            .factions
            .values()
            .find(|f| f.militia_corporation_id.is_some())
            .map(|f| i64::from(f.faction_id))
            .unwrap_or(500_001);
        (cid, fac)
    });

    let mut sess = SessionState::new(9550);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(1_000_009);
    sess.solar_system_id = Some(sys_id);

    // Invalid faction rejected
    let bad_fac = handle(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(999_999)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_fac.code, "InvalidWarFaction");

    // Valid faction enlistment succeeds & updates session
    let join_ok = handle(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(militia_faction)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        join_ok.dict_get("factionID").and_then(PyRep::as_i64),
        Some(militia_faction)
    );
    assert_eq!(
        join_ok.dict_get("enlisted").and_then(PyRep::as_bool),
        Some(true)
    );
    assert_eq!(sess.war_faction_id, Some(militia_faction));

    // Re-enlisting fails with FwAlreadyEnlisted
    let dup_join = handle(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(militia_faction)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(dup_join.code, "FwAlreadyEnlisted");

    // AcquireSkyhooks while docked fails
    sess.station_id = Some(60_003_760);
    let docked_err = handle(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(docked_err.code, "ShipDocked");
    sess.station_id = None;

    // AcquireSkyhooks in space returns skyhook summary
    let sky_info = handle(
        &ctx,
        &mut sess,
        "sovMgr",
        "AcquireSkyhooks",
        &[PyRep::Integer(i64::from(sys_id))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        sky_info.dict_get("skyhookID").and_then(PyRep::as_i64),
        Some(id) if id == skyhook_id || id == hub_id
    ));
    assert_eq!(
        sky_info.dict_get("skyhookOnline").and_then(PyRep::as_bool),
        Some(true)
    );

    // GetSystemSovereigntyInfo
    let sov_info = handle(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSystemSovereigntyInfo",
        &[PyRep::Integer(i64::from(sys_id))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        sov_info.dict_get("allianceID").and_then(PyRep::as_i64),
        Some(ally_id)
    );
    assert_eq!(
        sov_info.dict_get("corporationID").and_then(PyRep::as_i64),
        Some(corp_id)
    );
    assert_eq!(
        sov_info.dict_get("infrastructureHubID").and_then(PyRep::as_i64),
        Some(hub_id)
    );

    // GetInfrastructureHubInfo
    let hub_info = handle(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetInfrastructureHubInfo",
        &[PyRep::Integer(i64::from(sys_id))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        hub_info.dict_get("hubID").and_then(PyRep::as_i64),
        Some(hub_id)
    );
    assert_eq!(
        hub_info.dict_get("militaryIndex").and_then(PyRep::as_i64),
        Some(2)
    );
    assert_eq!(
        hub_info.dict_get("online").and_then(PyRep::as_bool),
        Some(true)
    );

    // GetSovStructuresInfoForSolarSystem
    let structs = handle(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(i64::from(sys_id))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(structs.row_count(), Some(2));

    // GetSovStructuresInfoForLocalSolarSystem
    let local_structs = handle(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovStructuresInfoForLocalSolarSystem",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        local_structs.dict_get("local").and_then(PyRep::as_bool),
        Some(true)
    );
    assert_eq!(
        local_structs
            .dict_get("infrastructureHubID")
            .and_then(PyRep::as_i64),
        Some(hub_id)
    );

    // Reject unknown service or unknown method cleanly
    assert!(handle(&ctx, &mut sess, "", "", &[], &kw)
        .await
        .is_none());
    assert!(handle(&ctx, &mut sess, "sovMgr", "", &[], &kw)
        .await
        .is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sov_fw_insurgency_join_faction_guards_and_sov_structures_branches() {
    let ctx = test_ctx();
    let null_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| crate::services::sov_gameplay::is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759);
    let char_id = 90_000_001_i64;
    let ship_id = 100_999_001_i64;
    let hub_id = 1_099_000_001_i64;
    let kw = HashMap::new();
    let now = ctx.now_unix();

    let (militia_fid, opp_fid) = {
        let mut fids: Vec<i64> = ctx
            .sde
            .factions
            .values()
            .filter(|f| f.militia_corporation_id.is_some())
            .map(|f| i64::from(f.faction_id))
            .collect();
        fids.sort_unstable();
        (fids[0], fids[1])
    };

    // Seed character in player corp with cooldown, low standing, corp restriction -> hit all JoinFactionAsCharacter guards, then join cleanly
    let corp_id = ctx.db.with_mut_snapshot(|snap| {
        let cid = 98_920_001_i64;
        snap.corporations.insert(
            cid,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = cid; __corp.ceo_id = char_id; __corp.name = "FwJoinGuardCorp".into(); __corp.ticker = "FJGC".into(); __corp.ensure_wallet_divisions(); __corp },
        );
        let ch = snap.characters.get_mut(&char_id).unwrap();
        ch.corporation_id = cid;
        ch.war_faction_id = None;
        ch.standings.insert(militia_fid, -5.0);
        snap.structures.insert(hub_id, UpwellStructureRecord {
            structure_id: hub_id,
            type_id: TYPE_SOVEREIGNTY_HUB,
            owner_corp_id: cid,
            solar_system_id: null_sys,
            name: "Null Hub".into(),
            position: [1_000.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            shield_hp: 0.0,
            armor_hp: 0.0,
            hull_hp: 0.0,
            ..Default::default()
        });
        snap.sov_runtime.adm_vulnerability_windows.insert(hub_id, (3_600, 7_200));
        snap.sov_campaign_scores.insert(hub_id, eve_db::snapshot_ext::SovCampaignScoreRecord {
            structure_id: hub_id,
            attackers_score: 0.25,
            defender_score: 0.75,
        });
        cid
    });

    let mut sess = SessionState::new(9201);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    // InvalidWarFaction (missing arg & non-militia faction)
    assert_eq!(handle(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[], &kw).await.unwrap().unwrap_err().code, "InvalidWarFaction");
    assert_eq!(handle(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_021)], &kw).await.unwrap().unwrap_err().code, "InvalidWarFaction");

    // Restricted corp factions guard (FwFactionNotAllowedByCorp), then FwStandingTooLow, then FwAlreadyEnlisted
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(militia_fid, 1.0);
    });
    let mut sess_corp = sess.clone();
    sess_corp.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    let _ = handle(
        &ctx,
        &mut sess_corp,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![PyRep::Integer(opp_fid)])],
        &kw,
    )
    .await;
    assert_eq!(
        handle(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(militia_fid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FwFactionNotAllowedByCorp"
    );
    let _ = handle(
        &ctx,
        &mut sess_corp,
        "fwCharacterEnlistmentMgr",
        "SetMyCorpAllowedEnlistmentFactions",
        &[PyRep::List(vec![PyRep::Integer(militia_fid), PyRep::Integer(opp_fid)])],
        &kw,
    )
    .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(militia_fid, -5.0);
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(opp_fid);
    });
    assert_eq!(handle(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(militia_fid)], &kw).await.unwrap().unwrap_err().code, "FwAlreadyEnlisted");

    // FwStandingTooLow then success (and triggers militia corp join)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = None;
    });
    assert_eq!(handle(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(militia_fid)], &kw).await.unwrap().unwrap_err().code, "FwStandingTooLow");

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.insert(militia_fid, 1.0);
        snap.corporations.remove(&corp_id);
    });
    let joined = handle(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(militia_fid)], &kw).await.unwrap().unwrap();
    assert_eq!(joined.dict_get("enlisted"), Some(&PyRep::Bool(true)));
    ctx.db.with_mut_snapshot(|snap| {
        let new_cid = snap.characters[&char_id].corporation_id;
        sess.corporation_id = Some(new_cid);
        if let Some(st) = snap.structures.get_mut(&hub_id) {
            st.owner_corp_id = new_cid;
        }
    });

    // AcquireSkyhooks guards: docked, NotNullSecSystem, OutsideSovStructureProximity
    sess.station_id = Some(60_003_760);
    assert_eq!(handle(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[PyRep::Integer(i64::from(null_sys))], &kw).await.unwrap().unwrap_err().code, "ShipDocked");
    sess.station_id = None;
    assert_eq!(handle(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[PyRep::Integer(30_000_142)], &kw).await.unwrap().unwrap_err().code, "NotNullSecSystem");

    let sys_h = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_h.spawn_ball(eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        position: glam::DVec3::new(500_000.0, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1.0e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }).await;
    assert_eq!(handle(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[PyRep::Integer(i64::from(null_sys))], &kw).await.unwrap().unwrap_err().code, "OutsideSovStructureProximity");

    // GetSovStructuresInfoForSolarSystem: invalid sys, no solar system, valid with ADM window & campaign score & zero fractions
    assert_eq!(handle(&ctx, &mut sess, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[PyRep::Integer(99_999_999)], &kw).await.unwrap().unwrap_err().code, "InvalidSolarSystemID");
    let mut nosys = sess.clone();
    nosys.solar_system_id = None;
    assert_eq!(handle(&ctx, &mut nosys, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[], &kw).await.unwrap().unwrap_err().code, "NoSolarSystem");
    assert_eq!(handle(&ctx, &mut nosys, "sovMgr", "GetSovStructuresInfoForLocalSolarSystem", &[], &kw).await.unwrap().unwrap_err().code, "NoSolarSystem");

    let info_res = handle(&ctx, &mut sess, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[PyRep::Integer(i64::from(null_sys))], &kw).await.unwrap().unwrap();
    let PyRep::List(items) = info_res else { panic!("expected List"); };
    assert!(!items.is_empty());
    assert_eq!(items[0].dict_get("shieldFraction"), Some(&PyRep::Real(0.0)));
    assert_eq!(items[0].dict_get("campaignEventType"), Some(&PyRep::Integer(i64::from(TYPE_SOVEREIGNTY_HUB))));

    // Deposit respawn: install prospecting upgrade in SubSystemSlot0 on hub + cleared SovOre -> executes full respawn!
    let upg_tid = crate::services::mining_sites::SOV_PROSPECTING_DEPOSITS[0].upgrade_type_id;
    ctx.db.with_mut_snapshot(|snap| {
        let hub_id = snap.structures.values().find(|s| s.solar_system_id == null_sys).map(|s| s.structure_id).unwrap();
        snap.items.insert(888_444_001, eve_db::InventoryItemRecord {
            item_id: 888_444_001,
            type_id: upg_tid,
            owner_id: corp_id,
            location_id: hub_id,
            flag: eve_db::InventoryFlag::SubSystemSlot0,
            quantity: 1,
            ..Default::default()
        });
        snap.asteroids.retain(|_, a| a.solar_system_id != null_sys);
        let list = snap.cosmic_anomalies.entry(null_sys).or_default();
        list.retain(|a| a.anomaly_type != "SovOre");
        list.push(eve_db::CosmicAnomalyRecord {
            anomaly_id: "SOV-RESPAWN-1".into(),
            solar_system_id: null_sys,
            anomaly_type: "SovOre".into(),
            name: "Cleared SovOre".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![],
            remaining_volume: 0.0,
            created_unix: now.saturating_sub(1_000_000_000),
            lifetime_seconds: 1,
            cleared: true,
        });
    });
    tick_sov_prospecting_deposit_respawn(&ctx, null_sys).await;
    assert!(ctx.db.with_snapshot(|s| s.cosmic_anomalies.get(&null_sys).is_some_and(|l| l.iter().any(|a| a.anomaly_type == "SovOre" && !a.cleared))));

    // Also test seed_system_mining_sites_from_sde in LowSec The Forge (triggers both Ore Signature & Gas site + already_seeded guard)
    if let Some(forge_low) = ctx.sde.solar_systems.values().find(|s| s.region_id == 10_000_002 && s.security_status > 0.0 && s.security_status < 0.45).map(|s| s.solar_system_id) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.cosmic_anomalies.remove(&forge_low);
            snap.cosmic_signatures.remove(&forge_low);
        });
        seed_system_mining_sites_from_sde(&ctx, forge_low).await;
        assert!(ctx.db.with_snapshot(|s| s.cosmic_signatures.get(&forge_low).is_some_and(|l| l.iter().any(|x| x.sig_group == "Gas"))));
        seed_system_mining_sites_from_sde(&ctx, forge_low).await;
    }
}

