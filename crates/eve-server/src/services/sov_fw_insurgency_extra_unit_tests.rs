//! Deep branch-coverage and mutation-killing unit tests for `sov_fw_insurgency`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mining_sites;
use crate::services::sov_fw_insurgency::*;
use eve_db::{
    CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn null_sec_sys(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && s.solar_system_id < 31_000_000)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759)
}

async fn call_sov_fw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    handle(ctx, sess, service, method, args, &kw)
        .await
        .expect("sov_fw_insurgency method should be handled")
}

#[tokio::test]
async fn derive_site_positions_and_sov_prospecting_full_respawn_with_upgrade() {
    let ctx = test_ctx();
    let sys_id = null_sec_sys(&ctx);

    // 1. All 4 slot permutations and fallback system without celestials produce finite AU-scaled vectors
    for slot in 0..4 {
        let p = derive_system_site_position_from_sde(&ctx, sys_id, slot, 2.0);
        let len = DVec3::from_array(p).length();
        assert!((len - 2.0 * AU_METERS).abs() < 1_000.0);
    }
    let fallback_p = derive_system_site_position_from_sde(&ctx, 99_999_999, 3, 1.0);
    assert!((DVec3::from_array(fallback_p).length() - AU_METERS).abs() < 1_000.0);

    // 2. Early return when system has no SovOre anomaly
    tick_sov_prospecting_deposit_respawn(&ctx, sys_id).await;

    // 3. Install a valid SOV_PROSPECTING_DEPOSITS upgrade in SubSystemSlot0 on a Sov Hub in sys_id,
    // and seed a cleared SovOre anomaly whose respawn timer has elapsed -> triggers full respawn!
    let upgrade_tid = mining_sites::SOV_PROSPECTING_DEPOSITS[0].upgrade_type_id;
    let hub_id = 1_096_000_001_i64;
    let upg_item_id = 1_096_000_002_i64;
    let sov_pos = derive_system_site_position_from_sde(&ctx, sys_id, 3, 5.0);
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Prospecting Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.items.insert(
            upg_item_id,
            InventoryItemRecord {
                item_id: upg_item_id,
                type_id: upgrade_tid,
                owner_id: 98_000_001,
                location_id: hub_id,
                flag: InventoryFlag::SubSystemSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        // First test clearing an uncleared SovOre anomaly + collapsing its matching signature
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "OLD-SOV-ORE".into(),
                solar_system_id: sys_id,
                name: "Depleted Deposit".into(),
                anomaly_type: "SovOre".into(),
                position: sov_pos,
                signal_strength: 1.0,
                ore_types: vec![1230],
                remaining_volume: 100.0,
                created_unix: 100,
                lifetime_seconds: 10,
                cleared: false,
            });
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: "OLD-SOV-ORE".into(),
                solar_system_id: sys_id,
                sig_group: "SovOre".into(),
                type_name: "Depleted Deposit".into(),
                position: sov_pos,
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: 100,
                lifetime_seconds: 10,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
    });

    // First tick marks OLD-SOV-ORE cleared and collapses signature (sets created_unix = now)
    tick_sov_prospecting_deposit_respawn(&ctx, sys_id).await;
    ctx.db.with_mut_snapshot(|snap| {
        let sig = &snap.cosmic_signatures[&sys_id][0];
        assert!(sig.collapsed);
        // Backdate created_unix so the deposit is ready to respawn on the next tick
        let anom = &mut snap.cosmic_anomalies.get_mut(&sys_id).unwrap()[0];
        assert!(anom.cleared);
        anom.created_unix = now.saturating_sub(200_000);
        anom.lifetime_seconds = 1;
    });

    // Second tick respawns a fresh uncleared SovOre anomaly, signature, and asteroids!
    tick_sov_prospecting_deposit_respawn(&ctx, sys_id).await;
    ctx.db.with_snapshot(|snap| {
        let anoms = &snap.cosmic_anomalies[&sys_id];
        assert_eq!(anoms.len(), 1);
        assert!(!anoms[0].cleared);
        assert!(anoms[0].anomaly_id.starts_with("DNG-"));
        assert!(snap
            .asteroids
            .values()
            .any(|a| a.solar_system_id == sys_id && a.quantity > 0));
    });

    // Seeding mining sites is idempotent on second invocation and handles unknown solar system
    seed_system_mining_sites_from_sde(&ctx, 99_999_999).await;
    seed_system_mining_sites_from_sde(&ctx, sys_id).await;
    let count_after_first = ctx
        .db
        .with_snapshot(|snap| snap.cosmic_signatures.get(&sys_id).map(|v| v.len()).unwrap_or(0));
    seed_system_mining_sites_from_sde(&ctx, sys_id).await;
    let count_after_second = ctx
        .db
        .with_snapshot(|snap| snap.cosmic_signatures.get(&sys_id).map(|v| v.len()).unwrap_or(0));
    assert_eq!(count_after_first, count_after_second);
}

#[tokio::test]
async fn fw_enlistment_cooldown_standings_corp_filter_and_player_corp_retention() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let player_corp_id = 98_000_001_i64;
    let sys_id = 30_000_142_u32;
    let fac_id = ctx
        .sde
        .factions
        .values()
        .find(|f| f.militia_corporation_id.is_some())
        .map(|f| i64::from(f.faction_id))
        .unwrap_or(500_001);

    let mut sess = SessionState::new(720);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(player_corp_id);
    sess.solar_system_id = Some(sys_id);

    // 1. Missing faction_id argument -> InvalidWarFaction
    let err = call_sov_fw(&ctx, &mut sess, "facWarMgr", "JoinFactionAsCharacter", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "InvalidWarFaction");

    // 2. Active enlistment cooldown -> FwEnlistmentCooldown
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = crate::services::fw_enlistment::load_store(snap);
        store.cooldown_until.insert(char_id, now + 3600);
        snap.item_custom_names.insert(
            crate::services::fw_enlistment::FW_ENLISTMENT_STORE_KEY,
            serde_json::to_string(&store).unwrap(),
        );
    });
    let err = call_sov_fw(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(fac_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "FwEnlistmentCooldown");

    // 3. Clear cooldown, set negative standing -> FwStandingTooLow
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = crate::services::fw_enlistment::load_store(snap);
        store.cooldown_until.clear();
        snap.item_custom_names.insert(
            crate::services::fw_enlistment::FW_ENLISTMENT_STORE_KEY,
            serde_json::to_string(&store).unwrap(),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = None;
            ch.corporation_id = player_corp_id;
            ch.standings.insert(fac_id, -0.5);
        }
    });
    let err = call_sov_fw(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(fac_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "FwStandingTooLow");

    // 4. Fix standing, restrict corp_allowed to empty set -> FwFactionNotAllowedByCorp
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(fac_id, 1.0);
        }
        let mut store = crate::services::fw_enlistment::load_store(snap);
        store.corp_allowed.insert(player_corp_id, Vec::new());
        snap.item_custom_names.insert(
            crate::services::fw_enlistment::FW_ENLISTMENT_STORE_KEY,
            serde_json::to_string(&store).unwrap(),
        );
    });
    let err = call_sov_fw(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(fac_id)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "FwFactionNotAllowedByCorp");

    // 5. Allow fac_id in corp_allowed while character is in player_corp_id -> enlists without changing corporation_id
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = crate::services::fw_enlistment::load_store(snap);
        store.corp_allowed.insert(player_corp_id, vec![fac_id]);
        snap.item_custom_names.insert(
            crate::services::fw_enlistment::FW_ENLISTMENT_STORE_KEY,
            serde_json::to_string(&store).unwrap(),
        );
    });
    let ok = call_sov_fw(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(fac_id)],
    )
    .await
    .unwrap();
    assert_eq!(ok.dict_get("enlisted").and_then(PyRep::as_bool), Some(true));
    assert_eq!(sess.corporation_id, Some(player_corp_id));
}

#[tokio::test]
async fn sov_mgr_skyhook_proximity_nullsec_checks_and_vuln_window_campaigns() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let hisec_sys = 30_000_142_u32;
    let null_sys = null_sec_sys(&ctx);
    let tcu_id = 1_097_000_001_i64;
    let hub_id = 1_097_000_002_i64;
    let ship_id = 100_000_001_i64;

    let mut sess = SessionState::new(721);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = None;

    // 1. NoSolarSystem when session.solar_system_id is None and no valid arg
    assert_eq!(call_sov_fw(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[]).await.unwrap_err().code, "NoSolarSystem");
    assert_eq!(call_sov_fw(&ctx, &mut sess, "sovMgr", "GetSystemSovereigntyInfo", &[]).await.unwrap_err().code, "NoSolarSystem");
    assert_eq!(call_sov_fw(&ctx, &mut sess, "sovMgr", "GetInfrastructureHubInfo", &[]).await.unwrap_err().code, "NoSolarSystem");
    assert_eq!(call_sov_fw(&ctx, &mut sess, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[]).await.unwrap_err().code, "NoSolarSystem");
    assert_eq!(call_sov_fw(&ctx, &mut sess, "sovMgr", "GetSovStructuresInfoForLocalSolarSystem", &[]).await.unwrap_err().code, "NoSolarSystem");

    // 2. InvalidSolarSystemID when explicit non-None arg is not a valid SDE solar system
    let err = call_sov_fw(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(999_999_999)],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "InvalidSolarSystemID");

    // 3. AcquireSkyhooks in high-sec fails with NotNullSecSystem
    sess.solar_system_id = Some(hisec_sys);
    let err = call_sov_fw(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotNullSecSystem");

    // 4. Seed friendly ShieldVulnerable structure at origin in null_sys and spawn ship at 100 km -> OutsideSovStructureProximity
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        let st = snap.system_states.entry(null_sys).or_default();
        st.solar_system_id = null_sys;
        st.fw_contested_pct = 45.0;

        snap.structures.insert(
            tcu_id,
            UpwellStructureRecord {
                structure_id: tcu_id,
                type_id: 32226,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "TCU".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                shield_hp: 0.0,
                armor_hp: 0.0,
                hull_hp: 0.0,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "IHub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                fuel_blocks: 0,
                ..Default::default()
            },
        );
        // Set wrapping vulnerability window on tcu_id (ev <= sv) and normal window on hub_id + campaign score
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(tcu_id, (72_000, 3_600));
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(hub_id, (1, 2));
        snap.sov_campaign_scores.insert(
            hub_id,
            eve_db::SovCampaignScoreRecord {
                structure_id: hub_id,
                attackers_score: 0.35,
                defender_score: 0.65,
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(null_sys);
    let _ = sys_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    let err = call_sov_fw(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[])
        .await
        .unwrap_err();
    assert_eq!(err.code, "OutsideSovStructureProximity");

    // Move ship within 10 km -> AcquireSkyhooks succeeds!
    let _ = sys_handle.try_set_ball_position(ship_id, DVec3::new(10_000.0, 0.0, 0.0));
    let sky_ok = call_sov_fw(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[])
        .await
        .unwrap();
    assert_eq!(sky_ok.dict_get("skyhookOnline").and_then(PyRep::as_bool), Some(true));

    // 5. GetSystemSovereigntyInfo reports contested=1 and distinct claimStructureID (TCU) & infrastructureHubID (IHub)
    let sov_info = call_sov_fw(&ctx, &mut sess, "sovMgr", "GetSystemSovereigntyInfo", &[])
        .await
        .unwrap();
    assert_eq!(sov_info.dict_get("contested").and_then(PyRep::as_i64), Some(1));
    assert_eq!(
        sov_info.dict_get("claimStructureID").and_then(PyRep::as_i64),
        Some(tcu_id)
    );
    assert_eq!(
        sov_info.dict_get("infrastructureHubID").and_then(PyRep::as_i64),
        Some(hub_id)
    );

    // 6. GetInfrastructureHubInfo reports online=false when fuel_blocks == 0
    let hub_info = call_sov_fw(&ctx, &mut sess, "sovMgr", "GetInfrastructureHubInfo", &[])
        .await
        .unwrap();
    assert_eq!(hub_info.dict_get("online").and_then(PyRep::as_bool), Some(false));

    // 7. GetSovStructuresInfoForSolarSystem with PyRep::None uses session solar system, includes zero HP fractions & campaign fields
    let structs_list = call_sov_fw(
        &ctx,
        &mut sess,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::None],
    )
    .await
    .unwrap();
    assert!(format!("{structs_list:?}").contains("campaignEventType"));
}

#[tokio::test]
async fn sov_fw_npc_militia_corp_swap_already_enlisted_docked_and_online_ihub_branches() {
    let ctx = test_ctx();
    let char_id = 90_000_002_i64;
    let npc_corp_id = 1_000_167_i64; // State War Academy (not in snap.corporations)
    let sys_id = null_sec_sys(&ctx);
    let fac_info = ctx
        .sde
        .factions
        .values()
        .find(|f| f.militia_corporation_id.is_some())
        .expect("SDE has militia faction");
    let fac_id = i64::from(fac_info.faction_id);
    let militia_corp_id = i64::from(fac_info.militia_corporation_id.unwrap());

    let mut sess = SessionState::new(722);
    sess.user_id = Some(1);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(npc_corp_id);
    sess.solar_system_id = Some(sys_id);

    // 1. Invalid non-militia faction -> InvalidWarFaction
    assert_eq!(
        call_sov_fw(
            &ctx,
            &mut sess,
            "facWarMgr",
            "JoinFactionAsCharacter",
            &[PyRep::Integer(999_999)]
        )
        .await
        .unwrap_err()
        .code,
        "InvalidWarFaction"
    );

    // 2. NPC-corp character joins militia faction -> swaps corporation_id to militia_corp_id & sets enlisted
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = npc_corp_id;
            ch.war_faction_id = None;
            ch.standings.insert(fac_id, 0.5);
        }
        snap.corporations.remove(&npc_corp_id);
    });
    let joined = call_sov_fw(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(fac_id)],
    )
    .await
    .unwrap();
    assert_eq!(joined.dict_get("factionID").and_then(PyRep::as_i64), Some(fac_id));
    assert_eq!(sess.corporation_id, Some(militia_corp_id));
    assert_eq!(sess.war_faction_id, Some(fac_id));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&char_id].corporation_id, militia_corp_id);
        assert_eq!(snap.characters[&char_id].war_faction_id, Some(fac_id));
    });

    // 3. Re-enlisting while already enlisted -> FwAlreadyEnlisted
    assert_eq!(
        call_sov_fw(
            &ctx,
            &mut sess,
            "facWarMgr",
            "JoinFactionAsCharacter",
            &[PyRep::Integer(fac_id)]
        )
        .await
        .unwrap_err()
        .code,
        "FwAlreadyEnlisted"
    );

    // 4. AcquireSkyhooks while docked -> ShipDocked; empty nullsec -> skyhookOnline=false
    sess.station_id = Some(60_003_760);
    assert_eq!(
        call_sov_fw(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );
    sess.station_id = None;
    let empty_sky = call_sov_fw(&ctx, &mut sess, "sovMgr", "AcquireSkyhooks", &[])
        .await
        .unwrap();
    assert_eq!(empty_sky.dict_get("skyhookOnline").and_then(PyRep::as_bool), Some(false));

    // 5. Online IHub (ShieldVulnerable + fuel_blocks > 0) reports online=true & system indices
    let hub_id = 1_098_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(sys_id).or_default();
        st.solar_system_id = sys_id;
        st.military_index = 3;
        st.industrial_index = 2;
        st.strategic_index = 4;
        snap.sov_prospecting_levels.insert(sys_id, 5);
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32458,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Online Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                fuel_blocks: 500,
                ..Default::default()
            },
        );
    });
    let hub_info = call_sov_fw(&ctx, &mut sess, "sovMgr", "GetInfrastructureHubInfo", &[])
        .await
        .unwrap();
    assert_eq!(hub_info.dict_get("online").and_then(PyRep::as_bool), Some(true));
    assert_eq!(hub_info.dict_get("militaryIndex").and_then(PyRep::as_i64), Some(3));
    assert_eq!(hub_info.dict_get("prospectingLevel").and_then(PyRep::as_i64), Some(5));
}
