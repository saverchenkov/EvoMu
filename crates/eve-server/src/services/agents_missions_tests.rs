//! Unit and regression tests for mission archetype classification and Paragon Heraldry / IRIS missions.

#![forbid(unsafe_code)]

use super::*;
use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::lp_store_catalog::PARAGON_CORP_ID;
use eve_db::{ActiveMissionRecord, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const PARAGON_IRIS_AGENT_ID: i64 = 3_020_034;
const PARAGON_IRIS_MISSION_17013_ID: u32 = 17_013;

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        if let Some(env_path) = std::env::var_os("EVOMU_SDE_PATH").filter(|v| !v.is_empty()) {
            let p = std::path::PathBuf::from(env_path);
            if p.exists() {
                return Arc::new(SdeRegistry::open(&p).expect("open EVOMU_SDE_PATH sqlite"));
            }
        }
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                return Arc::new(SdeRegistry::open(&candidate).expect("open evomu_sde.sqlite"));
            }
            if !dir.pop() {
                break;
            }
        }
        Arc::new(SdeRegistry::with_canonical_seed())
    }))
}

fn test_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    )
    .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

fn bound_agent(char_id: i64, agent_id: i64) -> BoundObjectEntry {
    BoundObjectEntry {
        bound_id: 1,
        session_id: 1,
        user_id: 1,
        character_id: char_id,
        service: "agentMgr".into(),
        object_id: agent_id,
        oid_str: format!("N=1:{agent_id}"),
        ref_id: 1,
    }
}

fn docked_session(char_id: i64, sys_id: u32, station_id: Option<i64>) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(sys_id),
        station_id,
        ship_id: Some(100_000_001),
        ..Default::default()
    }
}

fn move_demo_character(ctx: &ServerContext, char_id: i64, sys_id: u32, station_id: Option<i64>) {
    ctx.db.with_mut_snapshot(|snap| {
        let ch = snap
            .characters
            .get_mut(&char_id)
            .expect("demo character must exist in WorldRepository::with_demo_seed()");
        ch.solar_system_id = sys_id;
        ch.station_id = station_id;
    });
}

fn distribution_career_progress(ctx: &ServerContext, char_id: i64) -> u64 {
    ctx.quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(char_id as u32)
        .iter()
        .filter(|g| g.def.activity_kind == "distribution_missions")
        .map(|g| g.progress)
        .max()
        .unwrap_or(0)
}

#[test]
fn classify_mission_archetype_excludes_heraldry_trade_dc_and_rnd_hangar_turnin_from_courier() {
    let ctx = test_ctx();
    let iris = ctx
        .sde
        .agents
        .get(&PARAGON_IRIS_AGENT_ID)
        .expect("Paragon IRIS agent 3020034 must exist in SDE");
    let heraldry_tmpl = ctx
        .sde
        .missions
        .get(&PARAGON_IRIS_MISSION_17013_ID)
        .expect("Paragon Heraldry mission 17013 must exist in SDE");
    assert!(
        heraldry_tmpl.is_courier,
        "SDE mission 17013 sets is_courier=1, making runtime exclusion essential"
    );

    let heraldry_arch = classify_mission_archetype(Some(heraldry_tmpl), None, Some(iris));
    assert!(heraldry_arch.is_heraldry);
    assert!(!heraldry_arch.is_courier);
    assert!(!heraldry_arch.is_trade);
    assert!(!heraldry_arch.is_data_center);
    assert!(!heraldry_arch.is_kill);

    let trade_tmpl = ctx
        .sde
        .missions
        .values()
        .find(|m| {
            m.is_courier
                && m.content_template
                    .as_deref()
                    .is_some_and(|c| c.contains("Trade"))
        })
        .expect("SDE must contain a Trade mission with is_courier=1");
    let trade_arch = classify_mission_archetype(Some(trade_tmpl), None, None);
    assert!(trade_arch.is_trade);
    assert!(!trade_arch.is_courier);

    let dc_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 5)
        .expect("SDE must contain a Data Center agent");
    let dc_arch = classify_mission_archetype(Some(trade_tmpl), None, Some(dc_agent));
    assert!(dc_arch.is_data_center);
    assert!(!dc_arch.is_trade);
    assert!(!dc_arch.is_courier);

    let rnd_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 4)
        .expect("SDE must contain an R&D agent");
    let courier_tmpl = ctx
        .sde
        .missions
        .values()
        .find(|m| {
            m.is_courier
                && m.content_template
                    .as_deref()
                    .is_some_and(|c| c.contains("BasicCourier"))
        })
        .expect("SDE must contain a BasicCourier mission");
    let now = ctx.now_unix();
    let rnd_hangar_meta = MissionTimingAndCargoMeta {
        mission_id: courier_tmpl.mission_id,
        agent_id: rnd_agent.agent_id,
        offered_at_unix: now,
        offer_expiry_unix: now + 86_400,
        accepted_at_unix: Some(now),
        accept_expiry_unix: Some(now + 604_800),
        bonus_expiry_unix: Some(now + 3_600),
        completed_at_unix: None,
        required_delivery_type_id: Some(34),
        required_delivery_qty: 20,
        granted_courier_item_id: None,
        granted_courier_qty: 0,
        is_anomic_burner: false,
        chosen_branch: None,
        destination_station_id: None,
        origin_station_id: rnd_agent.station_id,
        offered_level: Some(1),
    };
    let rnd_hangar_arch =
        classify_mission_archetype(Some(courier_tmpl), Some(&rnd_hangar_meta), Some(rnd_agent));
    assert!(!rnd_hangar_arch.is_courier);

    let mut rnd_dest_meta = rnd_hangar_meta;
    rnd_dest_meta.destination_station_id = rnd_agent.station_id;
    let rnd_dest_arch =
        classify_mission_archetype(Some(courier_tmpl), Some(&rnd_dest_meta), Some(rnd_agent));
    assert!(rnd_dest_arch.is_courier);

    let normal_arch = classify_mission_archetype(Some(courier_tmpl), None, Some(iris));
    assert!(normal_arch.is_courier);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paragon_iris_heraldry_offer_accept_complete_lifecycle_succeeds_without_courier_cargo() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let iris = ctx
        .sde
        .agents
        .get(&PARAGON_IRIS_AGENT_ID)
        .expect("Paragon IRIS agent 3020034 must exist in SDE")
        .clone();
    let sys_id = iris.solar_system_id.expect("IRIS solar system");
    let st_id = iris.station_id.expect("IRIS station");
    move_demo_character(&ctx, char_id, sys_id, Some(st_id));

    let mut sess = docked_session(char_id, sys_id, Some(st_id));
    let bound = bound_agent(char_id, iris.agent_id);
    let kw = HashMap::new();

    // 1. Request mission offer via DoAction(2).
    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2)],
        &kw,
    )
    .await
    .expect("DoAction(2) must be handled")
    .expect("DoAction(2) must succeed for Paragon IRIS");

    let (offered_mid, offered_meta) = ctx.db.with_snapshot(|snap| {
        let m = snap
            .active_missions
            .values()
            .find(|m| m.character_id == char_id && m.agent_id == iris.agent_id)
            .expect("DoAction(2) must create an active mission for IRIS")
            .clone();
        let meta = load_mission_meta(snap, char_id, m.mission_id)
            .expect("DoAction(2) must persist mission metadata");
        (m.mission_id, meta)
    });
    let tmpl = ctx
        .sde
        .missions
        .get(&offered_mid)
        .expect("Offered mission template must exist in SDE");
    assert!(
        tmpl.content_template
            .as_deref()
            .unwrap_or_default()
            .contains("Heraldry"),
        "IRIS must offer a Heraldry mission, got mid={offered_mid}"
    );
    assert_eq!(offered_meta.required_delivery_type_id, None);
    assert_eq!(offered_meta.destination_station_id, None);

    // 2. Journal details must not classify the Heraldry mission as Courier.
    let journal = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "GetMyJournalDetails",
        &[],
        &kw,
    )
    .await
    .expect("GetMyJournalDetails must be handled")
    .expect("GetMyJournalDetails must succeed");
    let journal_dbg = format!("{journal:?}");
    assert!(
        !journal_dbg.contains("UI/Agents/MissionTypes/Courier"),
        "Heraldry journal entry must not use Courier hint: {journal_dbg}"
    );

    let briefing = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "GetMissionBriefingInfo",
        &[PyRep::Integer(iris.agent_id)],
        &kw,
    )
    .await
    .expect("GetMissionBriefingInfo must be handled")
    .expect("GetMissionBriefingInfo must succeed");
    let briefing_dbg = format!("{briefing:?}");
    assert!(
        !briefing_dbg.contains("Courier") && !briefing_dbg.contains("Transport"),
        "Heraldry briefing must not use Courier objectives: {briefing_dbg}"
    );

    // 3. Accept mission via DoAction(3): must succeed without InvalidMissionLocation or courier cargo.
    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .expect("DoAction(3) must be handled")
    .expect("DoAction(3) accept must succeed for Paragon Heraldry");

    ctx.db.with_snapshot(|snap| {
        let m = &snap.active_missions[&(char_id, offered_mid)];
        assert_eq!(m.state, "Accepted");
        let meta = load_mission_meta(snap, char_id, offered_mid).unwrap();
        assert_eq!(meta.granted_courier_item_id, None);
        assert!(
            !snap
                .item_custom_names
                .values()
                .any(|name| name.starts_with(&format!("courier_cargo:{char_id}:"))),
            "Heraldry accept must not grant any courier_cargo item"
        );
        assert!(
            snap.asteroids.is_empty(),
            "Heraldry accept must not spawn mining asteroids"
        );
    });

    // 4. Complete mission via DoAction(6): must credit Paragon LP and avoid distribution career goals.
    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .expect("DoAction(6) must be handled")
    .expect("DoAction(6) complete must succeed for Paragon Heraldry");

    ctx.db.with_snapshot(|snap| {
        let m = &snap.active_missions[&(char_id, offered_mid)];
        assert_eq!(m.state, "Completed");
        let paragon_lp = snap.characters[&char_id]
            .loyalty_points
            .get(&PARAGON_CORP_ID)
            .copied()
            .unwrap_or(0);
        assert!(paragon_lp > 0, "Paragon IRIS mission must credit Paragon LP");
    });
    assert_eq!(
        distribution_career_progress(&ctx, char_id),
        0,
        "Completing a Paragon Heraldry mission must not increment distribution_missions career goals"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paragon_iris_mission_17013_accept_and_complete_succeeds() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let iris = ctx
        .sde
        .agents
        .get(&PARAGON_IRIS_AGENT_ID)
        .expect("Paragon IRIS agent 3020034 must exist in SDE")
        .clone();
    let m17013 = ctx
        .sde
        .missions
        .get(&PARAGON_IRIS_MISSION_17013_ID)
        .expect("SDE mission 17013 must exist");
    assert!(m17013.is_courier);

    let sys_id = iris.solar_system_id.expect("IRIS solar system");
    let st_id = iris.station_id.expect("IRIS station");
    move_demo_character(&ctx, char_id, sys_id, Some(st_id));

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, PARAGON_IRIS_MISSION_17013_ID),
            ActiveMissionRecord {
                mission_id: PARAGON_IRIS_MISSION_17013_ID,
                character_id: char_id,
                agent_id: iris.agent_id,
                solar_system_id: sys_id,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: PARAGON_IRIS_MISSION_17013_ID,
                agent_id: iris.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: None,
                accept_expiry_unix: None,
                bonus_expiry_unix: None,
                completed_at_unix: None,
                required_delivery_type_id: None,
                required_delivery_qty: 0,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: None,
                origin_station_id: Some(st_id),
                offered_level: Some(iris.level),
            },
        );
    });

    let mut sess = docked_session(char_id, sys_id, Some(st_id));
    let bound = bound_agent(char_id, iris.agent_id);
    let kw = HashMap::new();

    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Mission 17013 accept must not return InvalidMissionLocation");

    handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .unwrap()
    .expect("Mission 17013 complete must succeed");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.active_missions[&(char_id, PARAGON_IRIS_MISSION_17013_ID)].state,
            "Completed"
        );
        let paragon_lp = snap.characters[&char_id]
            .loyalty_points
            .get(&PARAGON_CORP_ID)
            .copied()
            .unwrap_or(0);
        assert!(paragon_lp > 0);
    });
    assert_eq!(distribution_career_progress(&ctx, char_id), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heraldry_colocation_and_basic_courier_cargo_contract_hold() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let iris = ctx
        .sde
        .agents
        .get(&PARAGON_IRIS_AGENT_ID)
        .expect("Paragon IRIS agent 3020034 must exist in SDE")
        .clone();
    let sys_id = iris.solar_system_id.expect("IRIS solar system");
    let iris_st_id = iris.station_id.expect("IRIS station");
    let wrong_st_id = ctx
        .sde
        .stations
        .keys()
        .copied()
        .find(|&sid| sid != iris_st_id)
        .expect("SDE must contain a second station");

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, PARAGON_IRIS_MISSION_17013_ID),
            ActiveMissionRecord {
                mission_id: PARAGON_IRIS_MISSION_17013_ID,
                character_id: char_id,
                agent_id: iris.agent_id,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: PARAGON_IRIS_MISSION_17013_ID,
                agent_id: iris.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: Some(now),
                accept_expiry_unix: Some(now + 604_800),
                bonus_expiry_unix: Some(now + 3_600),
                completed_at_unix: None,
                required_delivery_type_id: None,
                required_delivery_qty: 0,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: None,
                origin_station_id: Some(iris_st_id),
                offered_level: Some(iris.level),
            },
        );
    });

    // Completing Heraldry mission at a non-agent station must still fail with AgentNotCoLocated.
    move_demo_character(&ctx, char_id, sys_id, Some(wrong_st_id));
    let mut wrong_sess = docked_session(char_id, sys_id, Some(wrong_st_id));
    let bound_iris = bound_agent(char_id, iris.agent_id);
    let kw = HashMap::new();
    let err = handle_with_bound(
        &ctx,
        &mut wrong_sess,
        Some(&bound_iris),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("Completing Heraldry mission away from IRIS station must fail");
    assert_eq!(err.code, "AgentNotCoLocated");

    // True BasicCourier missions must still require required_delivery_type_id and grant cargo.
    let courier_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 2 && a.division_id == 22 && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .expect("SDE must contain a distribution courier agent")
        .clone();
    let c_sys = courier_agent.solar_system_id.unwrap();
    let c_st = courier_agent.station_id.unwrap();
    let courier_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            m.is_courier
                && m.content_template
                    .as_deref()
                    .is_some_and(|c| c.contains("BasicCourier"))
        })
        .min_by_key(|m| m.mission_id)
        .expect("SDE must contain a BasicCourier mission")
        .clone();
    let c_mid = courier_tmpl.mission_id;

    move_demo_character(&ctx, char_id, c_sys, Some(c_st));
    let mut c_sess = docked_session(char_id, c_sys, Some(c_st));
    let bound_c = bound_agent(char_id, courier_agent.agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, c_mid),
            ActiveMissionRecord {
                mission_id: c_mid,
                character_id: char_id,
                agent_id: courier_agent.agent_id,
                solar_system_id: c_sys,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        save_mission_meta(
            snap,
            char_id,
            &MissionTimingAndCargoMeta {
                mission_id: c_mid,
                agent_id: courier_agent.agent_id,
                offered_at_unix: now,
                offer_expiry_unix: now + 86_400,
                accepted_at_unix: None,
                accept_expiry_unix: None,
                bonus_expiry_unix: None,
                completed_at_unix: None,
                required_delivery_type_id: None,
                required_delivery_qty: 0,
                granted_courier_item_id: None,
                granted_courier_qty: 0,
                is_anomic_burner: false,
                chosen_branch: None,
                destination_station_id: Some(c_st),
                origin_station_id: Some(c_st),
                offered_level: Some(courier_agent.level),
            },
        );
    });

    let missing_cargo_err = handle_with_bound(
        &ctx,
        &mut c_sess,
        Some(&bound_c),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("True BasicCourier mission without required_delivery_type_id must error");
    assert_eq!(missing_cargo_err.code, "InvalidMissionLocation");

    ctx.db.with_mut_snapshot(|snap| {
        let mut meta = load_mission_meta(snap, char_id, c_mid).unwrap();
        meta.required_delivery_type_id = Some(34);
        meta.required_delivery_qty = 5;
        save_mission_meta(snap, char_id, &meta);
    });
    handle_with_bound(
        &ctx,
        &mut c_sess,
        Some(&bound_c),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3)],
        &kw,
    )
    .await
    .unwrap()
    .expect("BasicCourier mission with required_delivery_type_id must accept cleanly");

    ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, c_mid).unwrap();
        let granted_id = meta
            .granted_courier_item_id
            .expect("BasicCourier accept must grant courier cargo item");
        assert_eq!(
            snap.item_custom_names.get(&granted_id).map(String::as_str),
            Some(format!("courier_cargo:{char_id}:{c_mid}").as_str())
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn classify_rnd_turnin_without_meta_and_complete_without_courier_cargo() {
    let ctx = test_ctx();
    let rnd_ag = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 4 && a.solar_system_id.is_some() && a.station_id.is_some())
        .min_by_key(|a| a.agent_id)
        .cloned()
        .expect("SDE must contain an R&D agent with station");
    let courier_tmpl = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            let ct = m.content_template.as_deref().unwrap_or_default();
            m.is_courier && !ct.contains("Trade") && !ct.contains("Heraldry")
        })
        .min_by_key(|m| m.mission_id)
        .cloned()
        .expect("SDE must contain a BasicCourier mission");

    let arch_without_meta = classify_mission_archetype(Some(&courier_tmpl), None, Some(&rnd_ag));
    assert!(arch_without_meta.is_rnd_turnin);
    assert!(!arch_without_meta.is_courier);

    let char_id = 90_000_001_i64;
    let rnd_sys = rnd_ag.solar_system_id.unwrap();
    let rnd_st = rnd_ag.station_id.unwrap();
    move_demo_character(&ctx, char_id, rnd_sys, Some(rnd_st));
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        snap.active_missions.insert(
            (char_id, courier_tmpl.mission_id),
            ActiveMissionRecord {
                mission_id: courier_tmpl.mission_id,
                character_id: char_id,
                agent_id: rnd_ag.agent_id,
                solar_system_id: rnd_sys,
                state: "Accepted".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
        // Intentionally do NOT insert MissionTimingAndCargoMeta (meta_opt == None) to cover #1368 F-3.
        snap.items.insert(
            990_701,
            eve_db::InventoryItemRecord {
                item_id: 990_701,
                type_id: 34,
                owner_id: char_id,
                location_id: rnd_st,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: rnd_tritanium_required_qty(rnd_ag.level) as i32,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let mut sess = docked_session(char_id, rnd_sys, Some(rnd_st));
    let bound = bound_agent(char_id, rnd_ag.agent_id);
    let kw = HashMap::new();
    let res = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(6),
            PyRep::Integer(rnd_ag.agent_id),
            PyRep::Integer(courier_tmpl.mission_id as i64),
        ],
        &kw,
    )
    .await
    .expect("DoAction(6) must be handled")
    .expect("R&D turn-in completion without meta must not demand courier cargo");
    assert_eq!(res.dict_get("state"), Some(&PyRep::String("Completed".into())));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fw_initial_offer_target_sys_courier_origin_sys_and_epic_arc_v24_briefing_regressions() {
    let mut ctx = test_ctx();
    let char_id = 90_000_001_i64;

    // Clone SDE with deterministic fixtures covering #1362 (BF-1204-01, BF-1204-02) and #1368 (F-1).
    let mut sde = (*ctx.sde).clone();
    let home_sys_tmpl = sde
        .solar_systems
        .get(&30_000_142)
        .or_else(|| sde.solar_systems.values().next())
        .cloned()
        .expect("SDE must contain at least one solar system");
    let home_sys = home_sys_tmpl.solar_system_id;
    let home_sys_name = home_sys_tmpl.name.clone();

    let mut warzone_sys = home_sys_tmpl.clone();
    warzone_sys.solar_system_id = 30_002_062;
    warzone_sys.name = "Onnamon".into();
    sde.solar_systems.entry(30_002_062).or_insert(warzone_sys);
    let warzone_sys_name = sde.solar_systems[&30_002_062].name.clone();
    assert_ne!(warzone_sys_name, home_sys_name);

    let dest_sys_id = 30_002_097_u32;
    if dest_sys_id != home_sys {
        let mut dest_sys = home_sys_tmpl.clone();
        dest_sys.solar_system_id = dest_sys_id;
        dest_sys.name = "Mehatoor".into();
        sde.solar_systems.entry(dest_sys_id).or_insert(dest_sys);
    }
    let dest_sys_name = sde.solar_systems[&dest_sys_id].name.clone();
    assert_ne!(home_sys_name, dest_sys_name);

    let st_tmpl = sde
        .stations
        .values()
        .find(|s| s.solar_system_id == home_sys)
        .or_else(|| sde.stations.values().next())
        .cloned()
        .expect("SDE must contain at least one station");
    let mut origin_st = st_tmpl.clone();
    origin_st.station_id = 69_990_001;
    origin_st.solar_system_id = home_sys;
    origin_st.name = "Origin Hub".into();
    sde.stations.insert(origin_st.station_id, origin_st.clone());

    let mut dest_st = st_tmpl;
    dest_st.station_id = 69_990_002;
    dest_st.solar_system_id = dest_sys_id;
    dest_st.name = "Dest Hub".into();
    sde.stations.insert(dest_st.station_id, dest_st.clone());

    let fw_agent_id = 3_999_181_i64;
    let fw_corp_id = 1_000_181_i64;
    sde.agents.insert(
        fw_agent_id,
        eve_sde::AgentInfo {
            agent_id: fw_agent_id,
            name: "FDU Agent".into(),
            corporation_id: fw_corp_id,
            station_id: Some(origin_st.station_id),
            solar_system_id: Some(home_sys),
            level: 1,
            division_id: 24,
            agent_type_id: 9,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );

    let talk_agent_id = 3_999_182_i64;
    sde.agents.insert(
        talk_agent_id,
        eve_sde::AgentInfo {
            agent_id: talk_agent_id,
            name: "Regular Contact".into(),
            corporation_id: 1_000_009,
            station_id: Some(origin_st.station_id),
            solar_system_id: Some(home_sys),
            level: 2,
            division_id: 24,
            agent_type_id: 2,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );

    let epic_agent_id = 3_999_183_i64;
    sde.agents.insert(
        epic_agent_id,
        eve_sde::AgentInfo {
            agent_id: epic_agent_id,
            name: "Sister Alitura".into(),
            corporation_id: 1_000_130,
            station_id: Some(origin_st.station_id),
            solar_system_id: Some(home_sys),
            level: 1,
            division_id: 24,
            agent_type_id: 10,
            is_locator: false,
            research_skills: Vec::new(),
        },
    );

    sde.epic_arcs.insert(
        29,
        eve_sde::EpicArcTemplate {
            epic_arc_id: 29,
            name: "The Blood-Stained Stars".into(),
            faction_id: Some(500_003),
            restart_interval: Some(90),
            missions_count: 50,
        },
    );

    let fw_kill_mid = 1_u32;
    let talk_mid = 999_881_u32;
    let epic_mid = 14_117_u32;
    let courier_mid = 999_882_u32;
    sde.missions.insert(
        fw_kill_mid,
        eve_sde::MissionTemplate {
            mission_id: fw_kill_mid,
            title: "FW Strike".into(),
            is_courier: false,
            lp_alpha: 100,
            lp_omega: 100,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicKillMission".into()),
            dungeon_id: None,
        },
    );
    sde.missions.insert(
        talk_mid,
        eve_sde::MissionTemplate {
            mission_id: talk_mid,
            title: "Meet Contact".into(),
            is_courier: false,
            lp_alpha: 50,
            lp_omega: 50,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_TalkToAgentMission".into()),
            dungeon_id: None,
        },
    );
    sde.missions.insert(
        epic_mid,
        eve_sde::MissionTemplate {
            mission_id: epic_mid,
            title: "A Beacon of Hope".into(),
            is_courier: false,
            lp_alpha: 50,
            lp_omega: 50,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_EpicArcTalkToAgent".into()),
            dungeon_id: None,
        },
    );
    sde.missions.insert(
        courier_mid,
        eve_sde::MissionTemplate {
            mission_id: courier_mid,
            title: "Cross-System Haul".into(),
            is_courier: true,
            lp_alpha: 50,
            lp_omega: 50,
            has_standing_rewards: true,
            content_template: Some("agent.missionTemplatizedContent_BasicCourierMission".into()),
            dungeon_id: None,
        },
    );

    let fw_fac_id = sde
        .factions
        .values()
        .find(|f| f.militia_corporation_id == Some(fw_corp_id))
        .map(|f| f.faction_id as i64);
    ctx.sde = Arc::new(sde);

    // 1. BF-1204-01: Epic Arc briefing uses V24 arc name; non-Epic-Arc TalkToAgent never emits "Epic Arc".
    let epic_brief =
        build_mission_briefing_text(&ctx, epic_agent_id, epic_mid, "A Beacon of Hope", home_sys, None);
    assert!(
        epic_brief.contains("The Blood-Stained Stars") && !epic_brief.contains("Epic Arc 4"),
        "Epic Arc briefing must resolve V24 arc name: {epic_brief}"
    );
    let non_epic_talk_brief =
        build_mission_briefing_text(&ctx, talk_agent_id, talk_mid, "Meet Contact", home_sys, None);
    assert!(
        non_epic_talk_brief.starts_with("Level 2 mission liaison")
            && !non_epic_talk_brief.contains("Epic Arc"),
        "non-Epic-Arc TalkToAgent briefing must not emit Epic Arc prefix: {non_epic_talk_brief}"
    );

    // 2. BF-1204-02 (courier origin system): uses origin_station's solar system even when sys_id == dest_sys.
    let courier_meta = crate::services::agents_missions::agents_standings_arcs::MissionTimingAndCargoMeta {
        mission_id: courier_mid,
        agent_id: talk_agent_id,
        offered_at_unix: 100,
        offer_expiry_unix: 200,
        accepted_at_unix: None,
        accept_expiry_unix: None,
        bonus_expiry_unix: None,
        completed_at_unix: None,
        required_delivery_type_id: Some(3814),
        required_delivery_qty: 10,
        granted_courier_item_id: None,
        granted_courier_qty: 0,
        is_anomic_burner: false,
        chosen_branch: None,
        destination_station_id: Some(dest_st.station_id),
        origin_station_id: Some(origin_st.station_id),
        offered_level: Some(2),
    };
    let courier_brief = build_mission_briefing_text(
        &ctx,
        talk_agent_id,
        courier_mid,
        "Cross-System Haul",
        dest_sys_id,
        Some(&courier_meta),
    );
    assert!(
        courier_brief.contains(&format!("from Origin Hub ({home_sys_name}) to Dest Hub ({dest_sys_name})")),
        "Courier briefing must label origin station with origin system name: {courier_brief}"
    );

    // 3. BF-1204-02 (FW DoAction(2)): initial offer resolves encounter/objectives using warzone target_sys.
    move_demo_character(&ctx, char_id, home_sys, Some(origin_st.station_id));
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.retain(|(cid, _), _| *cid != char_id);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.war_faction_id = fw_fac_id;
            ch.standings.insert(fw_agent_id, 10.0);
        }
    });
    let mut sess = docked_session(char_id, home_sys, Some(origin_st.station_id));
    let bound = bound_agent(char_id, fw_agent_id);
    let kw = HashMap::new();
    let offer = handle_with_bound(
        &ctx,
        &mut sess,
        Some(&bound),
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(fw_agent_id)],
        &kw,
    )
    .await
    .expect("FW DoAction(2) must be handled")
    .expect("FW DoAction(2) must succeed");

    let offered_mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    assert_eq!(offered_mid, fw_kill_mid);
    let objectives = offer.dict_get("objectives").and_then(PyRep::as_str).unwrap_or("");
    assert!(
        objectives.contains(&warzone_sys_name) && !objectives.contains(&home_sys_name),
        "FW initial offer objectives must name warzone system {warzone_sys_name} instead of home system {home_sys_name}: {objectives}"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.active_missions.get(&(char_id, offered_mid)).map(|m| m.solar_system_id),
            Some(30_002_062)
        );
    });
}
