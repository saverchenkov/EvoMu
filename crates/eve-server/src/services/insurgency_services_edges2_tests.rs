#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

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

fn auth_sess(char_id: i64, sys_id: Option<u32>) -> SessionState {
    let mut s = SessionState::new(8401);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = sys_id;
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rpc_auth_filtering_and_ice_heist_instances() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut unauth = SessionState::new(8402);

    for (svc, meth) in [
        ("insurgencySolarsystem", "GetAllVisibleCampaigns"),
        ("insurgencySolarsystem", "GetLocalCampaignClientSnapshot"),
        ("dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances"),
        ("dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances"),
    ] {
        let err = handle(&ctx, &mut unauth, svc, meth, &[], &kw)
            .await
            .expect("dispatched")
            .unwrap_err();
        assert_eq!(err.code, "Unauthorized");
    }

    assert!(handle(
        &ctx,
        &mut unauth,
        "insurgencySolarsystem",
        "GetPirateInsurgencyInstances",
        &[],
        &kw,
    )
    .await
    .is_none());

    init_insurgency_campaign(&ctx, 920_001, 500_010, 500_001, 30_002_537, vec![30_002_537]);
    let ih1 = spawn_ice_heist(&ctx, 30_002_537);
    let ih2 = spawn_ice_heist(&ctx, 30_002_537);
    assert_ne!(ih1, ih2);

    let mut sess = auth_sess(90_000_001, Some(30_002_537));
    let filtered = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyIceHeistInstances",
        &[PyRep::Integer(30_002_537)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    match filtered {
        PyRep::List(rows) => assert_eq!(rows.len(), 2),
        other => panic!("expected List, got {other:?}"),
    }

    sess.solar_system_id = None;
    let all = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyIceHeistInstances",
        &[PyRep::None],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    match all {
        PyRep::List(rows) => assert_eq!(rows.len(), 2),
        other => panic!("expected List, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn campaign_lifecycle_fob_invulnerability_corruption_and_fw_capture_tick() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_id = 30_002_537_u32;

    emit_campaign_started(&ctx, sys_id, 999_999);
    emit_campaign_updated(&ctx, sys_id, 999_999, 0);

    let camp_id = 920_002_i64;
    init_insurgency_campaign(&ctx, camp_id, 500_010, 500_001, sys_id, vec![sys_id]);
    let fob_id = unified_fob_id(camp_id);

    with_insurgency_runtime(&ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&camp_id) {
            c.forecasting_end_unix = now - 10;
            c.active_end_unix = now + 10_000;
        }
        rt.fob_armor_reinforced_until.insert(camp_id, now + 1_200);
    });
    assert!(ctx.db.with_snapshot(|s| is_fob_invulnerable_in_snap(s, camp_id, fob_id, now)));
    assert!(is_insurgency_stronghold_invulnerable(&ctx, sys_id, fob_id));

    with_insurgency_runtime(&ctx, |rt| {
        rt.fob_armor_reinforced_until.insert(camp_id, now - 10);
    });
    assert!(!is_insurgency_stronghold_invulnerable(&ctx, sys_id, fob_id));

    add_system_corruption(&ctx, camp_id, sys_id, 30_000);
    assert!(insurgency_system_corruption_stage(&ctx, sys_id) >= 1);

    end_insurgency_campaign(&ctx, camp_id);
    assert_eq!(insurgency_system_corruption_stage(&ctx, sys_id), 0);
    end_insurgency_campaign(&ctx, camp_id);

    let camp2 = 920_003_i64;
    init_insurgency_campaign(&ctx, camp2, 500_010, 500_001, sys_id, vec![sys_id]);
    let fob2 = unified_fob_id(camp2);
    let _ = ensure_stronghold_in_space(&ctx, camp2).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(st) = snap.structures.get_mut(&fob2) {
            st.state = "Destroyed".into();
        }
        ensure_unified_pirate_fob_in_snap(snap, Some(&ctx.sde), camp2);
        assert!(is_fob_invulnerable_in_snap(snap, camp2, fob2, now));
        assert_eq!(match_fob_campaign_for_ball_in_snap(snap, sys_id, fob2), Some(camp2));
    });

    let cid = 90_000_001_i64;
    let ship_id = 9_600_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 670,
                owner_id: cid,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.system_states.entry(sys_id).or_default().solar_system_id = sys_id;
    });
    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(Ball {
        id: ship_id,
        type_id: 670,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }).await;
    assert!(!step_system_insurgency_capture_tick(&ctx, sys_id, ship_id, 500_001).await);
    assert!(!step_system_insurgency_capture_tick(&ctx, 30_000_142, ship_id, 500_001).await);

    let mut st_rec = UpwellStructureRecord::default();
    st_rec.structure_id = fob2;
    st_rec.type_id = PIRATE_STRONGHOLD_GURISTAS_TYPE;
    st_rec.solar_system_id = sys_id;
    st_rec.state = "ShieldVulnerable".into();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(fob2, st_rec);
    });
    step_system_insurgency_tick(&ctx, sys_id).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn add_system_corruption_tuple_stronghold_notify_and_instance_queries() {
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let sys_id = 30_002_537_u32;
    let camp_id = 921_001_i64;
    let cid = 90_000_001_i64;
    let kw = HashMap::new();
    let mut sess = auth_sess(cid, Some(sys_id));

    init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, sys_id, vec![sys_id]);
    with_insurgency_runtime(&ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&camp_id) {
            c.forecasting_end_unix = now - 60;
            c.active_end_unix = now + 3_600;
            c.winddown_end_unix = now + 7_200;
        }
    });

    // 1. Assert exact (points, stage) tuple from `add_system_corruption` (600 pts -> stage 3; != 0, 1, -1)
    let (pts, stage) = add_system_corruption(&ctx, camp_id, sys_id, 600);
    assert_eq!((pts, stage), (600, 3));

    // 2. Seed distinct insurgency & ice-heist instances and verify exact `instanceID` list results
    with_insurgency_runtime(&ctx, |rt| {
        rt.insurgency_instances.push(eve_db::PersistedInsurgencySiteRecord {
            instance_id: 965_432,
            solar_system_id: sys_id,
            site_type: "MiningOpAmbush".into(),
            name: "Exact Insurgency Site".into(),
            corruption_reward: 250.0,
            completed: false,
        });
        rt.ice_heist_instances.push(eve_db::PersistedIceHeistRecord {
            instance_id: 865_432,
            solar_system_id: sys_id,
            pirate_tankers_saved: 1,
            empire_tankers_saved: 2,
            completed: false,
        });
    });

    let ins_rep = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyInstances",
        &[PyRep::Integer(i64::from(sys_id))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(ins_rows) = ins_rep else {
        panic!("expected List from GetPirateInsurgencyInstances");
    };
    assert!(ins_rows
        .iter()
        .any(|r| r.dict_get("instanceID") == Some(&PyRep::Integer(965_432))));

    let ice_rep = handle(
        &ctx,
        &mut sess,
        "dungeonInstanceCacheMgr",
        "GetPirateInsurgencyIceHeistInstances",
        &[PyRep::Integer(i64::from(sys_id))],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(ice_rows) = ice_rep else {
        panic!("expected List from GetPirateInsurgencyIceHeistInstances");
    };
    assert!(ice_rows
        .iter()
        .any(|r| r.dict_get("instanceID") == Some(&PyRep::Integer(865_432))));

    // 3. `notify_stronghold_destruction` actually dispatches MachoNet + Quasar notifications
    let _ = ctx.drain_notifications_for_char(cid);
    notify_stronghold_destruction(
        &ctx.db,
        Some(&ctx.quasar_gateway),
        Some(&ctx.pending_notifications),
        Some(&ctx.notify_tx),
        &StrongholdDestructionInfo {
            campaign_id: camp_id,
            solar_system_id: sys_id,
            killer_char_id: Some(cid),
            old_suppression: 100,
            new_suppression: 350,
            new_suppression_stage: 2,
        },
    );
    let notes = ctx.drain_notifications_for_char(cid);
    assert!(!notes.is_empty(), "notify_stronghold_destruction must queue MachoNet notices");
}
