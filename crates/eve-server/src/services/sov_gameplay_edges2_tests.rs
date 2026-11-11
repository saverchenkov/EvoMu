#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use crate::services::sov_services::TYPE_SOVEREIGNTY_HUB;
use eve_db::{
    AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, PersistedMercenaryDen,
    TrainedSkillRecord, UpwellStructureRecord, corp_roles,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashSet;
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

fn find_claimable_null(ctx: &ServerContext) -> u32 {
    ctx.sde
        .solar_systems
        .values()
        .find(|s| is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
        .map(|s| s.solar_system_id)
        .unwrap_or(30_004_759)
}

#[test]
fn claimable_nullsec_exclusions_hack_errors_and_aftermath_noop() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let corp_id = 98_910_001_i64;
    let ally_id = 99_910_001_i64;

    // Pochven & NPC null exclusions (if present in SDE)
    if let Some(pochven_sys) = ctx.sde.solar_systems.values().find(|s| s.region_id == 10_000_070) {
        assert!(!is_claimable_nullsec_system(&ctx.sde, pochven_sys.solar_system_id));
    }
    if let Some(npc_null_sys) = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.security_status < 0.0 && !is_claimable_nullsec_system(&ctx.sde, s.solar_system_id))
    {
        assert!(!is_claimable_nullsec_system(&ctx.sde, npc_null_sys.solar_system_id));
    }

    // Data hack edge cases: non-hub None, destroyed hub TargetDestroyed, friendly hub, missing hacking skill
    let data_mod = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 538 && !t.name.contains("Relic"))
        .cloned()
        .unwrap_or_else(|| ctx.sde.types.values().next().unwrap().clone());

    let hub_destroyed = 1_093_000_001_i64;
    let hub_friendly = 1_093_000_002_i64;
    let hub_hostile = 1_093_000_003_i64;
    let non_hub = 1_093_000_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(ally_id);
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.ceo_id = cid; __corp.alliance_id = Some(ally_id); __corp.name = "HackCorp".into(); __corp.ticker = "HKC".into(); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.corporations.insert(
            corp_id + 1,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id + 1; __corp.ceo_id = cid + 1; __corp.alliance_id = Some(ally_id); __corp.name = "AllyCorp".into(); __corp.ticker = "ALC".into(); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.skills.remove(&cid);

        for (sid, tid, owner, state) in [
            (hub_destroyed, TYPE_SOVEREIGNTY_HUB, corp_id + 99, "Destroyed"),
            (hub_friendly, TYPE_SOVEREIGNTY_HUB, corp_id + 1, "ShieldVulnerable"),
            (hub_hostile, TYPE_SOVEREIGNTY_HUB, corp_id + 99, "ShieldVulnerable"),
            (non_hub, 35_832, corp_id + 99, "ShieldVulnerable"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: tid,
                    owner_corp_id: owner,
                    solar_system_id: 30_000_142,
                    name: "HubTest".into(),
                    position: [0.0, 0.0, 0.0],
                    state: state.into(),
                    ..Default::default()
                },
            );
        }
    });

    assert!(try_handle_sov_hub_data_hack(&ctx, cid, non_hub, 1, &data_mod, 60.0, 20.0).is_none());
    assert_eq!(
        try_handle_sov_hub_data_hack(&ctx, cid, hub_destroyed, 1, &data_mod, 60.0, 20.0)
            .unwrap()
            .unwrap_err()
            .code,
        "TargetDestroyed"
    );
    assert_eq!(
        try_handle_sov_hub_data_hack(&ctx, cid, hub_friendly, 1, &data_mod, 60.0, 20.0)
            .unwrap()
            .unwrap_err()
            .code,
        "CannotHackFriendlySovHub"
    );
    assert_eq!(
        try_handle_sov_hub_data_hack(&ctx, cid, hub_hostile, 1, &data_mod, 60.0, 20.0)
            .unwrap()
            .unwrap_err()
            .code,
        "HackingSkillRequired"
    );

    // Archaeology (3408) fallback skill allows hack
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid).or_default().insert(
            3408,
            TrainedSkillRecord {
                type_id: 3408,
                level: 3,
                skill_points: 8_000.0,
            },
        );
    });
    assert!(try_handle_sov_hub_data_hack(&ctx, cid, hub_hostile, 1, &data_mod, 60.0, 20.0)
        .unwrap()
        .is_ok());

    // Aftermath non-suspect and non-reinforced Tenebrex does not create crimewatch state
    ctx.crimewatch.write().unwrap().states.clear();
    handle_normal_structure_damage_aftermath(
        &ctx,
        30_000_142,
        0.9,
        cid,
        999,
        TYPE_TENEBREX_CYNO_JAMMER,
        "ShieldVulnerable",
        false,
        ctx.now_unix(),
    );
    assert!(!ctx.crimewatch.read().unwrap().states.contains_key(&cid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mercden_component_guards_skyhook_auth_and_anchor_sov_hub_validation() {
    let ctx = test_ctx();
    let cid = 90_000_001_i64;
    let other_cid = 90_000_002_i64;
    let corp_id = 98_920_001_i64;
    let ally_id = 99_920_001_i64;
    let null_sys = find_claimable_null(&ctx);
    let ship_id = 1_094_000_001_i64;
    let den_id = 1_094_000_002_i64;
    let skyhook_id = 1_094_000_003_i64;
    let hub_item_id = 1_094_000_004_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.alliance_id = None;
            ch.solar_system_id = null_sys;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default(); __corp.corporation_id = corp_id; __corp.ceo_id = other_cid; __corp.alliance_id = None; __corp.name = "SovValCorp".into(); __corp.ticker = "SVC".into(); __corp.members = HashSet::from([cid, other_cid]); __corp.ensure_wallet_divisions(); __corp },
        );
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord {
                structure_id: skyhook_id,
                type_id: crate::services::sov_services::TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id + 55,
                solar_system_id: null_sys,
                name: "Skyhook".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142, // Wrong system relative to null_sys
                name: "Den".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: other_cid,
                skyhook_id,
                solar_system_id: null_sys,
                planet_id: 40_009_077,
                type_id: TYPE_MERCENARY_DEN,
                enabled: false,
                cargo_extraction_enabled: true,
                development_level: 1,
                anarchy_level: 0,
                simulated_at_unix: now,
                infomorphs: 0,
                next_activity_generation_unix: now + 3_600,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 648,
                owner_id: cid,
                location_id: i64::from(null_sys),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let mut sess = SessionState::new(8501);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(null_sys);
    sess.ship_id = Some(ship_id);

    // Mercenary Den component error guards: ShipNotInSpace -> spawn ship -> MercenaryDenNotFound / NotInSystem / OutOfRange / NotOwner / Reinforced / NoInfomorphs
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, null_sys, cid, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[])
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    let _ = ctx
        .systems
        .get_or_spawn(null_sys)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::new(50_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, null_sys, cid, ship_id, 9_999_999, "mercenaryDen", "extract_infomorphs", &[])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenNotFound"
    );
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, null_sys, cid, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenNotInSystem"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&den_id).unwrap().solar_system_id = null_sys;
    });
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, null_sys, cid, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenOutOfRange"
    );
    let _ = ctx.systems.get_or_spawn(null_sys).remove_ball(ship_id).await;
    let _ = ctx
        .systems
        .get_or_spawn(null_sys)
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 648,
            position: DVec3::new(100.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 1.0e6,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, null_sys, cid, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[])
            .await
            .unwrap_err()
            .code,
        "NotMercenaryDenOwner"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.get_mut(&den_id).unwrap().owner_char_id = cid;
    });
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, null_sys, cid, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[])
            .await
            .unwrap_err()
            .code,
        "MercenaryDenReinforced"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.get_mut(&den_id).unwrap().enabled = true;
    });
    assert_eq!(
        handle_mercden_component_call(&ctx, &sess, null_sys, cid, ship_id, den_id, "mercenaryDen", "extract_infomorphs", &[])
            .await
            .unwrap_err()
            .code,
        "NoInfomorphsAvailable"
    );

    // ConfirmSkyhookSovManagement: missing args, unauthorized role, then sov alliance director clamp (>100 -> 100)
    assert_eq!(
        handle_confirm_skyhook_sov_management(&ctx, &sess, &[]).await.unwrap_err().code,
        "InvalidArguments"
    );
    assert_eq!(
        handle_confirm_skyhook_sov_management(&ctx, &sess, &[PyRep::Integer(skyhook_id), PyRep::Integer(25)])
            .await
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        c.alliance_id = Some(ally_id);
        c.member_roles.insert(cid, corp_roles::ROLE_DIRECTOR);
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "SovAlly".into(),
                short_name: "SVA".into(),
                executor_corp_id: corp_id,
                ..Default::default()
            },
        );
        snap.system_states.entry(null_sys).or_default().sov_alliance_id = Some(ally_id);
    });
    sess.alliance_id = Some(ally_id);
    sess.corp_role = corp_roles::ROLE_DIRECTOR;
    let clamped = handle_confirm_skyhook_sov_management(&ctx, &sess, &[PyRep::Integer(skyhook_id), PyRep::Integer(150)])
        .await
        .unwrap();
    assert_eq!(clamped.dict_get("taxPercent").and_then(PyRep::as_i64), Some(100));

    // handle_anchor_sov_hub_rpc guards
    sess.station_id = Some(60_003_760);
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "ShipDocked");
    sess.station_id = None;

    sess.solar_system_id = None;
    assert_eq!(handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code, "NoSolarSystem");
    sess.solar_system_id = Some(30_000_142);
    assert_eq!(
        handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code,
        "CannotAnchorSovHubOutsideNullSec"
    );
    sess.solar_system_id = Some(null_sys);

    // Other alliance owns sov -> SovereigntyHeldByAnotherAlliance
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = Some(ally_id + 1);
    });
    assert_eq!(
        handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[]).await.unwrap_err().code,
        "SovereigntyHeldByAnotherAlliance"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.get_mut(&null_sys).unwrap().sov_alliance_id = Some(ally_id);
        snap.items.insert(
            hub_item_id,
            InventoryItemRecord {
                item_id: hub_item_id,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_id: other_cid,
                location_id: ship_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    assert_eq!(
        handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_item_id)])
            .await
            .unwrap_err()
            .code,
        "ItemOwnershipViolation"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&hub_item_id).unwrap().owner_id = cid;
    });
    assert_eq!(
        handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(hub_item_id)])
            .await
            .unwrap_err()
            .code,
        "ItemNotInCargo"
    );
    assert_eq!(
        handle_anchor_sov_hub_rpc(&ctx, &mut sess, &[PyRep::Integer(9_999_998)])
            .await
            .unwrap_err()
            .code,
        "MissingStructureItem"
    );

    // Direct check for `tick_mercenary_den_reinforcements_in_snap`: HullReinforced -> HullVulnerable -> ShieldVulnerable
    let den_id = 1_000_000_990_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: corp_id,
                solar_system_id: null_sys,
                name: "Merc Den".into(),
                position: [0.0; 3],
                state: "HullReinforced".into(),
                reinforced_until_unix: 1_000,
                ..Default::default()
            },
        );
        let pending = PendingNotices::default();
        super::tick_mercenary_den_reinforcements_in_snap(snap, &pending, 1_000);
        assert_eq!(snap.structures[&den_id].state, "HullVulnerable");
        assert_eq!(snap.structures[&den_id].reinforced_until_unix, 1_900);

        super::tick_mercenary_den_reinforcements_in_snap(snap, &pending, 1_900);
        assert_eq!(snap.structures[&den_id].state, "ShieldVulnerable");
        assert_eq!(snap.structures[&den_id].reinforced_until_unix, 0);
    });
}
