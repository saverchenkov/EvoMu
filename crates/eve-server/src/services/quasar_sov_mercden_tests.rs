//! Unit tests for `quasar_sov_mercden` (Mercenary Dens, Evolution/Infomorph Simulation, Mercenary Tactical Operations).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::{corp_roles, TrainedSkillRecord};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn id_msg(field: u32, id: i64) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Int64(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn mercenary_den_formulas_simulation_and_activity_completion() {
    assert_eq!(infomorph_cargo_capacity(1), 3_000);
    assert_eq!(stage_for_level(0), 1);
    assert_eq!(stage_for_level(19), 1);
    assert_eq!(stage_for_level(20), 2);
    assert_eq!(stage_for_level(40), 3);
    assert_eq!(stage_for_level(70), 4);
    assert_eq!(stage_for_level(100), 5);

    assert_eq!(infomorphs_per_hour(1), 65);
    assert_eq!(infomorphs_per_hour(2), 68);
    assert_eq!(infomorphs_per_hour(3), 71);
    assert_eq!(infomorphs_per_hour(4), 74);
    assert_eq!(infomorphs_per_hour(5), 78);

    assert_eq!(anarchy_workforce_disruption_pct(1), 0);
    assert_eq!(anarchy_workforce_disruption_pct(2), 0);
    assert_eq!(anarchy_workforce_disruption_pct(3), 10);
    assert_eq!(anarchy_workforce_disruption_pct(4), 20);
    assert_eq!(anarchy_workforce_disruption_pct(5), 40);

    assert_eq!(activity_capacity(1), 3);
    assert_eq!(activity_capacity(5), 7);

    let mut den = PersistedMercenaryDen {
        den_id: 91_000_001,
        owner_char_id: 90_000_001,
        skyhook_id: 79_000_001,
        solar_system_id: 30_000_142,
        planet_id: 40_009_080,
        type_id: 85_230,
        enabled: true,
        cargo_extraction_enabled: true,
        development_level: 18,
        anarchy_level: 10,
        infomorphs: 0,
        simulated_at_unix: 1_000,
        next_activity_generation_unix: 1_000,
    };

    // Simulate 24 hours: +5 development (18 -> 23, stage 1 -> 2), +5 anarchy (10 -> 15), 24*65 = 1560 infomorphs
    assert!(simulate_den(&mut den, 1_000 + EVOLUTION_UNIT_SECS));
    assert_eq!(den.development_level, 23);
    assert_eq!(den.anarchy_level, 15);
    assert_eq!(den.infomorphs, 24 * 65);
    assert!(!encode_evolution_simulation(&den).is_empty());

    let db = WorldRepository::with_demo_seed();
    let pending: PendingNotices = Arc::new( std::sync::RwLock::new(Vec::new()) );
    push_definitions_updated_notice(&pending, &den);

    let act = PersistedMercenaryActivity {
        uuid: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
        den_id: den.den_id,
        solar_system_id: den.solar_system_id,
        dungeon_id: 123,
        started: true,
        started_by: 90_000_001,
        expiry_unix: 5_000,
        development_impact: 10,
        anarchy_impact: -10,
        infomorph_bonus: 350,
    };
    push_activity_expiry_changed_notice(&pending, den.solar_system_id, &act, 4_000);

    db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.insert(den.den_id, den.clone());
        // Sub-max Development (23 < 100): completing MTO grants +10 Development (23 -> 33),
        // -10 Anarchy (15 -> 5), and 0 bonus Encrypted Infomorphs (`Mercenary_Tactical_Operation.md:24`).
        complete_mercenary_activity_in_snap(snap, &pending, None, &act, den.den_id, den.solar_system_id);
        let updated = &snap.sov_runtime.mercenary_dens[&den.den_id];
        assert_eq!(updated.development_level, 33);
        assert_eq!(updated.anarchy_level, 5);
        assert_eq!(updated.infomorphs, 24 * 65);
    });
}

#[test]
fn mercenary_den_mto_exclusivity_clamped_notice_deltas_and_refresh_template_impacts() {
    let reg = QuasarDescriptorRegistry::open_default().expect("open default descriptor DB");
    let db = WorldRepository::with_demo_seed();
    let pending: PendingNotices = Arc::new(std::sync::RwLock::new(Vec::new()));
    let sys_id = 30_000_142_u32;

    db.with_mut_snapshot(|snap| {
        let den_sub = PersistedMercenaryDen {
            den_id: 91_000_801,
            owner_char_id: 90_000_001,
            skyhook_id: 79_000_801,
            solar_system_id: sys_id,
            planet_id: 40_009_080,
            type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
            enabled: true,
            cargo_extraction_enabled: true,
            development_level: 95,
            anarchy_level: 3,
            infomorphs: 120,
            simulated_at_unix: 10_000,
            next_activity_generation_unix: 10_000,
        };
        let den_max = PersistedMercenaryDen {
            den_id: 91_000_802,
            development_level: MAX_LEVEL,
            anarchy_level: 25,
            infomorphs: 120,
            ..den_sub.clone()
        };
        snap.sov_runtime.mercenary_dens.insert(den_sub.den_id, den_sub.clone());
        snap.sov_runtime.mercenary_dens.insert(den_max.den_id, den_max.clone());

        // Generated templates reflect development_level < MAX_LEVEL vs == MAX_LEVEL
        refresh_activities(snap, &pending, den_sub.den_id, 10_000);
        refresh_activities(snap, &pending, den_max.den_id, 10_000);
        let sub_acts: Vec<_> = snap.sov_runtime.mercenary_activities.values().filter(|a| a.den_id == den_sub.den_id).collect();
        let max_acts: Vec<_> = snap.sov_runtime.mercenary_activities.values().filter(|a| a.den_id == den_max.den_id).collect();
        assert!(!sub_acts.is_empty());
        assert!(!max_acts.is_empty());
        assert!(sub_acts.iter().all(|a| a.development_impact == 10 && a.infomorph_bonus == 0 && a.anarchy_impact == -10));
        assert!(max_acts.iter().all(|a| a.development_impact == 0 && a.infomorph_bonus == 350 && a.anarchy_impact == -10));

        // 1. Sub-max completion with dev 95 -> 100 and anarchy 3 -> 0, unset impacts -> default +10 / -10 clamped to (+5, -3, 0 infomorphs)
        pending.write().unwrap().clear();
        let act_sub = PersistedMercenaryActivity {
            uuid: vec![0xD1; 16],
            den_id: den_sub.den_id,
            solar_system_id: sys_id,
            dungeon_id: 1,
            started: true,
            started_by: 90_000_001,
            expiry_unix: 15_000,
            development_impact: 0,
            anarchy_impact: 0,
            infomorph_bonus: 350,
        };
        complete_mercenary_activity_in_snap(snap, &pending, None, &act_sub, den_sub.den_id, sys_id);
        let d_sub_after = &snap.sov_runtime.mercenary_dens[&den_sub.den_id];
        assert_eq!(d_sub_after.development_level, MAX_LEVEL);
        assert_eq!(d_sub_after.anarchy_level, 0);
        assert_eq!(d_sub_after.infomorphs, 120, "sub-max MTO completion must grant 0 bonus infomorphs");
        let completed_desc = reg
            .get_message("eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice")
            .expect("CompletedNotice descriptor");
        let completed_field_num = |field_name: &str| {
            completed_desc
                .fields
                .iter()
                .find(|f| f.name == field_name)
                .unwrap_or_else(|| panic!("missing CompletedNotice field {field_name}"))
                .number
        };
        let as_i64 = |v: Option<&DynamicValue>| -> Option<i64> {
            match v? {
                DynamicValue::Int32(x) => Some(i64::from(*x)),
                DynamicValue::Int64(x) => Some(*x),
                DynamicValue::Uint32(x) => Some(i64::from(*x)),
                DynamicValue::Uint64(x) => i64::try_from(*x).ok(),
                _ => None,
            }
        };
        let dev_field = completed_field_num("development_impact");
        let anarchy_field = completed_field_num("anarchy_impact");
        let bonus_field = completed_field_num("infomorph_bonus");

        let n_sub = pending
            .read()
            .unwrap()
            .iter()
            .find(|(_, a)| a.type_url.ends_with("mercenaryden.activity.api.CompletedNotice"))
            .expect("CompletedNotice emitted for sub-max")
            .1
            .clone();
        let dec_sub = reg.decode_dynamic("eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice", &n_sub.value).unwrap();
        assert_eq!(as_i64(dec_sub.get(dev_field)), Some(5));
        assert_eq!(as_i64(dec_sub.get(anarchy_field)), Some(-3));
        assert_eq!(as_i64(dec_sub.get(bonus_field)), Some(0));

        // 2. Max-level completion (pre_dev == 100): grants +350 infomorphs, 0 dev delta, -10 anarchy delta
        pending.write().unwrap().clear();
        let act_max = PersistedMercenaryActivity {
            uuid: vec![0xD2; 16],
            den_id: den_max.den_id,
            solar_system_id: sys_id,
            dungeon_id: 1,
            started: true,
            started_by: 90_000_001,
            expiry_unix: 15_000,
            development_impact: 10,
            anarchy_impact: 0,
            infomorph_bonus: 0,
        };
        complete_mercenary_activity_in_snap(snap, &pending, None, &act_max, den_max.den_id, sys_id);
        let d_max_after = &snap.sov_runtime.mercenary_dens[&den_max.den_id];
        assert_eq!(d_max_after.development_level, MAX_LEVEL);
        assert_eq!(d_max_after.anarchy_level, 15);
        assert_eq!(d_max_after.infomorphs, 120 + 350);
        let n_max = pending
            .read()
            .unwrap()
            .iter()
            .find(|(_, a)| a.type_url.ends_with("mercenaryden.activity.api.CompletedNotice"))
            .expect("CompletedNotice emitted for max")
            .1
            .clone();
        let dec_max = reg.decode_dynamic("eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice", &n_max.value).unwrap();
        assert_eq!(as_i64(dec_max.get(dev_field)), Some(0));
        assert_eq!(as_i64(dec_max.get(anarchy_field)), Some(-10));
        assert_eq!(as_i64(dec_max.get(bonus_field)), Some(350));
    });
}

#[test]
fn mercenary_den_and_activity_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let den_id = 91_000_010_i64;
    let sys_id = 30_000_142_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.skills.entry(cid).or_default().insert(
            TYPE_MERCENARY_DEN_MANAGEMENT,
            TrainedSkillRecord {
                type_id: TYPE_MERCENARY_DEN_MANAGEMENT,
                level: 4,
                skill_points: 100_000.0,
            },
        );
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: cid,
                skyhook_id: 79_000_001,
                solar_system_id: sys_id,
                planet_id: 40_009_080,
                type_id: 85_230,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 25,
                anarchy_level: 15,
                infomorphs: 200,
                simulated_at_unix: 0,
                next_activity_generation_unix: 0,
            },
        );
    });

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    // GetAllOwned, GetMaximumForCharacter, GetAsOwner
    for (req, suffix) in [
        (
            "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
            "eve_public.sovereignty.mercenaryden.api.GetAllOwnedResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityResponse",
        ),
    ] {
        let r = handlers[req](&gm_ctx, &dummy_any, &id_msg(1, den_id)).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    // Admin ModifyDevelopmentLevel & ModifyAnarchyLevel
    let mut mod_lvl = id_msg(1, den_id);
    mod_lvl.fields.insert(2, DynamicValue::Int64(60));
    handlers["eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest"](
        &gm_ctx,
        &dummy_any,
        &mod_lvl,
    )
    .expect("ModifyDevelopmentLevelRequest");
    handlers["eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest"](
        &gm_ctx,
        &dummy_any,
        &mod_lvl,
    )
    .expect("ModifyAnarchyLevelRequest");

    let mut bad_lvl = id_msg(1, den_id);
    bad_lvl.fields.insert(2, DynamicValue::Int64(150));
    assert_eq!(
        handlers["eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest"](
            &gm_ctx,
            &dummy_any,
            &bad_lvl,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // Activity StartRequest
    let act_uuid = db.with_snapshot(|snap| {
        snap.sov_runtime
            .mercenary_activities
            .values()
            .next()
            .expect("generated activity")
            .uuid
            .clone()
    });
    let mut act_id_inner = DynamicQuasarMessage::new("ActId");
    act_id_inner.fields.insert(1, DynamicValue::Bytes(act_uuid));
    let mut start_msg = DynamicQuasarMessage::new("Start");
    start_msg
        .fields
        .insert(1, DynamicValue::Message(Box::new(act_id_inner)));

    let start_resp = handlers["eve_public.sovereignty.mercenaryden.activity.api.StartRequest"](
        &gm_ctx,
        &dummy_any,
        &start_msg,
    )
    .expect("StartRequest");
    assert!(start_resp
        .type_url
        .ends_with("eve_public.sovereignty.mercenaryden.activity.api.StartResponse"));

    // Starting same activity twice returns CONFLICT
    assert_eq!(
        handlers["eve_public.sovereignty.mercenaryden.activity.api.StartRequest"](
            &gm_ctx,
            &dummy_any,
            &start_msg,
        )
        .unwrap_err()
        .0,
        STATUS_CONFLICT
    );
}

#[tokio::test]
async fn tick_sov_gameplay_transitions_mercenary_den_reinforcement_and_shield_recovery() {
    let ctx = crate::router::ServerContext::new();
    let den_id = 1_090_888_001_i64;
    let now = 1_750_000_000_u64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            den_id,
            eve_db::UpwellStructureRecord {
                structure_id: den_id,
                type_id: crate::services::sov_gameplay::TYPE_MERCENARY_DEN,
                owner_corp_id: 98_000_001,
                solar_system_id: 30_000_142,
                name: "Reinforced Merc Den".into(),
                position: [0.0, 0.0, 0.0],
                state: "HullReinforced".into(),
                reinforced_until_unix: now + 30,
                shield_hp: 100.0,
                armor_hp: 100.0,
                hull_hp: 100.0,
                ..Default::default()
            },
        );
    });

    crate::services::sov_gameplay::tick_sov_gameplay(&ctx, now + 29).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.structures[&den_id].state, "HullReinforced");
    });

    // At exit of HullReinforced -> HullVulnerable
    crate::services::sov_gameplay::tick_sov_gameplay(&ctx, now + 30).await;
    ctx.db.with_snapshot(|snap| {
        let s = &snap.structures[&den_id];
        assert_eq!(s.state, "HullVulnerable");
        assert_eq!(s.reinforced_until_unix, now + 30 + 900);
    });

    // After 900s uncontested in HullVulnerable -> ShieldVulnerable
    crate::services::sov_gameplay::tick_sov_gameplay(&ctx, now + 30 + 900).await;
    ctx.db.with_snapshot(|snap| {
        let s = &snap.structures[&den_id];
        assert_eq!(s.state, "ShieldVulnerable");
        assert_eq!(s.reinforced_until_unix, 0);
    });
}

#[test]
fn mercenary_den_anarchy_evolution_admin_modify_and_complete_reconcile_workforce() {
    use crate::services::quasar_sov_hub::{
        compute_hub_resources, planet_yield_for_planet, star_power_for_system,
    };
    use crate::services::quasar_sov_workforce::{
        exported_amount, imports, systems_within_jumps, WORKFORCE_TRANSFER_MAX_JUMPS,
    };
    use crate::services::sov_gameplay::TYPE_MERCENARY_DEN;
    use crate::services::sov_services::{
        TYPE_MAGMATIC_GAS, TYPE_ORBITAL_SKYHOOK, TYPE_SOVEREIGNTY_HUB,
    };
    use eve_db::{
        corp_roles, PersistedHubUpgrade, PersistedMercenaryActivity, PersistedMercenaryDen,
        PersistedSovHubData, PersistedWorkforceMode, UpwellStructureRecord,
    };
    use eve_machonet::service_roles;
    use eve_proto::quasar::GoogleTimestamp;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    register_handlers(&router, Arc::clone(&db));

    let cid = 90_000_001_i64;
    let corp_id = 1_000_009_i64;
    let sys_a = 30_000_142_u32;
    let sys_b = *systems_within_jumps(&sde, sys_a, WORKFORCE_TRANSFER_MAX_JUMPS)
        .keys()
        .find(|&&s| s != sys_a && star_power_for_system(&sde, s) >= 400)
        .expect("neighbor system with >= 400 star power within jump range");
    let temp_cel = sde
        .celestials_in_system(sys_a)
        .into_iter()
        .find(|c| c.type_id == 11)
        .expect("Temperate planet in Jita");
    let wf_base = planet_yield_for_planet(temp_cel.type_id, temp_cel.radius).1;
    assert!(wf_base >= 5_100, "expected >= 5_100 base Temperate workforce, got {wf_base}");

    let hub_a = 78_700_001_i64;
    let hub_b = 78_700_002_i64;
    let skyhook_a = 78_700_010_i64;
    let den_id = 78_700_020_i64;
    let t0 = 1_800_000_000_u64;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_a;
        }
        snap.corporations.insert(corp_id, {
            let mut corp = eve_db::CorporationRecord::default();
            corp.corporation_id = corp_id;
            corp.ceo_id = cid;
            corp
        });
        snap.structures.insert(
            hub_a,
            UpwellStructureRecord {
                structure_id: hub_a,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_a,
                name: "Hub A".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            hub_b,
            UpwellStructureRecord {
                structure_id: hub_b,
                type_id: TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: corp_id,
                solar_system_id: sys_b,
                name: "Hub B".into(),
                state: "anchor_vulnerable".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            skyhook_a,
            UpwellStructureRecord {
                structure_id: skyhook_a,
                type_id: TYPE_ORBITAL_SKYHOOK,
                owner_corp_id: corp_id,
                solar_system_id: sys_a,
                name: "Temperate Skyhook".into(),
                state: "Online".into(),
                ..Default::default()
            },
        );
        snap.structures.insert(
            den_id,
            UpwellStructureRecord {
                structure_id: den_id,
                type_id: TYPE_MERCENARY_DEN,
                owner_corp_id: corp_id,
                solar_system_id: sys_a,
                name: "Mercenary Den".into(),
                state: "ShieldVulnerable".into(),
                ..Default::default()
            },
        );
        snap.sov_runtime.skyhook_planets.insert(skyhook_a, temp_cel.celestial_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_a, true);
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                owner_char_id: cid,
                skyhook_id: skyhook_a,
                solar_system_id: sys_a,
                planet_id: temp_cel.celestial_id,
                type_id: TYPE_MERCENARY_DEN,
                enabled: true,
                cargo_extraction_enabled: true,
                development_level: 20,
                anarchy_level: 35, // Stage 2 (0% disruption)
                infomorphs: 100,
                simulated_at_unix: t0,
                next_activity_generation_unix: t0 + ACTIVITY_GENERATION_SECS,
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_a,
            PersistedSovHubData {
                hub_id: hub_a,
                workforce: PersistedWorkforceMode::Export {
                    destination: Some(sys_b),
                    amount: 5_000,
                },
                last_updated_unix: t0,
                ..Default::default()
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_b,
            PersistedSovHubData {
                hub_id: hub_b,
                workforce: PersistedWorkforceMode::Import { sources: vec![sys_a] },
                upgrades: vec![
                    PersistedHubUpgrade {
                        type_id: 82_499, // 200p, 1_800w
                        online: true,
                        low_power: false,
                    },
                    PersistedHubUpgrade {
                        type_id: 87_815, // 200p, 2_700w, 7/hr Magmatic Gas
                        online: true,
                        low_power: false,
                    },
                ],
                fuel: [(TYPE_MAGMATIC_GAS, 200)].into_iter().collect(),
                last_updated_unix: t0,
                ..Default::default()
            },
        );
    });

    let take_notice_urls = || -> Vec<String> {
        let mut guard = router.pending_notices.write().unwrap();
        guard.drain(..).map(|(_, a)| a.type_url).collect()
    };

    // 1. tick_den_with_sde advances 1 day (+86_400s): Anarchy 35 -> 40 (Stage 2 -> Stage 3, -10% disruption).
    let expected_stage3_wf = wf_base - (wf_base * anarchy_workforce_disruption_pct(3)) / 100;
    assert_eq!(anarchy_workforce_disruption_pct(3), 10);
    assert!(expected_stage3_wf < 5_000 && expected_stage3_wf >= 4_500);
    let _ = take_notice_urls();
    db.with_mut_snapshot(|snap| {
        tick_den_with_sde(snap, Some(&sde), &router.pending_notices, den_id, t0 + EVOLUTION_UNIT_SECS);
        assert_eq!(stage_for_level(snap.sov_runtime.mercenary_dens[&den_id].anarchy_level), 3);
        assert_eq!(
            compute_hub_resources(snap, &sde, hub_a).unwrap().workforce_local,
            expected_stage3_wf
        );
        assert_eq!(exported_amount(snap, hub_a), expected_stage3_wf);
        assert_eq!(imports(snap, &sde, hub_b), vec![(sys_a, expected_stage3_wf)]);
        assert!(!snap.sov_runtime.sov_hubs[&hub_b].upgrades[1].low_power);
    });
    let tick_notices = take_notice_urls();
    assert!(tick_notices
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(tick_notices
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));

    // 2. GM ModifyAnarchyLevelRequest setting Stage 5 (level 100, -40% disruption) clamps Hub A export
    //    and starves Hub B's slot-1 upgrade (87_815) into low_power = true.
    let expected_stage5_wf = wf_base - (wf_base * anarchy_workforce_disruption_pct(5)) / 100;
    assert_eq!(anarchy_workforce_disruption_pct(5), 40);
    assert!(expected_stage5_wf >= 1_800 && expected_stage5_wf < 4_500);

    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask |= service_roles::ROLE_ADMIN;
    gm_ctx.received = Some(GoogleTimestamp::from_unix((t0 + EVOLUTION_UNIT_SECS) as i64, 0));

    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Int64(den_id));
    let mut modify_req = DynamicQuasarMessage::new("ModifyAnarchyLevelRequest");
    modify_req.fields.insert(1, DynamicValue::Message(Box::new(inner)));
    modify_req.fields.insert(2, DynamicValue::Int64(100));

    handlers["eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest"](
        &gm_ctx,
        &any,
        &modify_req,
    )
    .expect("admin ModifyAnarchyLevelRequest -> Stage 5");

    let mod_notices = take_notice_urls();
    assert!(mod_notices
        .iter()
        .any(|u| u.ends_with("mercenaryden.api.EvolutionStateChangedNotice")));
    assert!(mod_notices
        .iter()
        .any(|u| u.ends_with("workforce.api.StateChangedNotice")));
    assert!(mod_notices
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
    db.with_snapshot(|snap| {
        assert_eq!(
            compute_hub_resources(snap, &sde, hub_a).unwrap().workforce_local,
            expected_stage5_wf
        );
        assert_eq!(exported_amount(snap, hub_a), expected_stage5_wf);
        assert_eq!(imports(snap, &sde, hub_b), vec![(sys_a, expected_stage5_wf)]);
        assert!(!snap.sov_runtime.sov_hubs[&hub_b].upgrades[0].low_power);
        assert!(snap.sov_runtime.sov_hubs[&hub_b].upgrades[1].low_power);
    });

    // 3. complete_mercenary_activity_in_snap lowering Anarchy 100 -> 40 (Stage 5 -> Stage 3) restores
    //    Hub A local workforce and emits ResourcesSimulatedNotice.
    let act = PersistedMercenaryActivity {
        uuid: vec![0xA1; 16],
        den_id,
        solar_system_id: sys_a,
        dungeon_id: 12_367,
        started: true,
        started_by: cid,
        expiry_unix: t0 + EVOLUTION_UNIT_SECS + 3_600,
        development_impact: 5,
        anarchy_impact: -60,
        infomorph_bonus: 0,
    };
    db.with_mut_snapshot(|snap| {
        complete_mercenary_activity_in_snap(
            snap,
            &router.pending_notices,
            Some(&sde),
            &act,
            den_id,
            sys_a,
        );
        assert_eq!(snap.sov_runtime.mercenary_dens[&den_id].anarchy_level, 40);
        assert_eq!(
            compute_hub_resources(snap, &sde, hub_a).unwrap().workforce_local,
            expected_stage3_wf
        );
    });
    let mto_notices = take_notice_urls();
    assert!(mto_notices
        .iter()
        .any(|u| u.ends_with("mercenaryden.api.EvolutionStateChangedNotice")));
    assert!(mto_notices
        .iter()
        .any(|u| u.ends_with("hub.api.ResourcesSimulatedNotice")));
}
