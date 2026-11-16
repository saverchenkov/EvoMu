//! Unit tests for `gm_soak_commands` (`#1088`).

use super::*;
use eve_db::WorldRepository;
use eve_sde::{InvType, RaceInfo, SdeRegistry, TypeMaterialEntry};
use std::sync::Arc;

fn make_test_inv_type(
    type_id: u32,
    group_id: u32,
    category_id: u32,
    name: &str,
    mass: f64,
    volume: f64,
    capacity: f64,
    radius: f64,
    base_price: f64,
    race_id: Option<u32>,
    faction_id: Option<u32>,
) -> InvType {
    InvType {
        type_id,
        group_id,
        category_id,
        name: name.into(),
        mass,
        volume,
        capacity,
        radius,
        base_price,
        published: true,
        portion_size: 1,
        market_group_id: None,
        race_id,
        faction_id,
        variation_parent_type_id: None,
        description_id: None,
        icon_id: None,
        graphic_id: None,
        sound_id: None,
        meta_group_id: None,
        meta_level: Some(0),
        tech_level: Some(1),
        default_effect_id: None,
        description: String::new(),
        effect_ids: Vec::new(),
        default_attributes: HashMap::new(),
    }
}

fn make_test_ctx() -> (ServerContext, i64) {
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.types.entry(29_768).or_insert_with(|| {
        make_test_inv_type(
            29_768,
            673,
            11,
            "Independent Osprey",
            11_000_000.0,
            100_000.0,
            400.0,
            120.0,
            0.0,
            Some(1),
            Some(500_021),
        )
    });
    sde.types.entry(587).or_insert_with(|| {
        make_test_inv_type(
            587,
            25,
            6,
            "Rifter",
            1_067_000.0,
            27_289.0,
            135.0,
            42.0,
            0.0,
            Some(2),
            None,
        )
    });
    sde.types.entry(34).or_insert_with(|| {
        make_test_inv_type(
            34,
            18,
            4,
            "Tritanium",
            0.0,
            0.01,
            0.0,
            0.0,
            5.0,
            None,
            None,
        )
    });
    sde.type_materials.insert(
        587,
        vec![TypeMaterialEntry {
            material_type_id: 34,
            quantity: 20_000,
        }],
    );
    sde.races.entry(1).or_insert(RaceInfo {
        race_id: 1,
        name: "Caldari".into(),
        corvette_type_id: Some(601),
        starter_skills: HashMap::from([(3300, 3), (3327, 2)]),
        description: String::new(),
    });
    let repo = Arc::new(WorldRepository::with_demo_seed());
    let cid = 90_000_001_i64;
    repo.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.race_id = 1;
        }
    });
    (ServerContext::with_sde_and_repository(Arc::new(sde), repo), cid)
}

#[test]
fn test_deterministic_soak_seed_ring_and_deadwarp_anchor() {
    let s1 = deterministic_logi_soak_seed(30_000_142, 90_000_001, 100_001, 5);
    let s2 = deterministic_logi_soak_seed(30_000_142, 90_000_001, 100_001, 5);
    let s3 = deterministic_logi_soak_seed(30_000_142, 90_000_001, 100_001, 6);
    assert_eq!(s1, s2);
    assert_ne!(s1, s3);

    let center = DVec3::new(1_000.0, 2_000.0, 3_000.0);
    let ring_a = ring_positions_around(center, 4, SOAK_LOGI_ORBIT_RANGE_M, s1);
    let ring_b = ring_positions_around(center, 4, SOAK_LOGI_ORBIT_RANGE_M, s1);
    assert_eq!(ring_a, ring_b);
    assert_eq!(ring_a.len(), 4);
    for p in &ring_a {
        let horiz = DVec3::new(p.x - center.x, 0.0, p.z - center.z).length();
        assert!((horiz - SOAK_LOGI_ORBIT_RANGE_M).abs() < 1e-3);
        assert!((p.y - center.y).abs() <= 500.0);
    }

    let anchor_a = deterministic_deadwarp_anchor(center, 30_000_142, 100_001);
    let anchor_b = deterministic_deadwarp_anchor(center, 30_000_142, 100_001);
    assert_eq!(anchor_a, anchor_b);
    let dist_au = anchor_a.distance(center) / crate::services::space_destiny::AU_IN_METERS;
    assert!((dist_au - DEADWARP_DISTANCE_AU).abs() < 1e-6);
}

#[test]
fn test_derive_sde_wreck_loot_is_deterministic_and_uses_sde_materials() {
    let (ctx, _) = make_test_ctx();
    let loot_a = derive_sde_wreck_loot(&ctx.sde, 587, DEFAULT_WRECK_TYPE_ID, 42);
    let loot_b = derive_sde_wreck_loot(&ctx.sde, 587, DEFAULT_WRECK_TYPE_ID, 42);
    assert_eq!(loot_a, loot_b);
    assert!(
        loot_a.iter().any(|&(tid, qty)| tid == 34 && qty >= 1),
        "expected Tritanium derived from SDE type_materials: {loot_a:?}"
    );
}

#[tokio::test]
async fn test_gm_soak_argument_validation_and_session_dump() {
    let (ctx, cid) = make_test_ctx();
    let mut sess = SessionState::new(777_001);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.solar_system_id = Some(30_000_142);

    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/soak").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/soak logi 0").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/soak logi 99").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/deadwarp bogus").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/sysjunkclear extra").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/spawnwreck").await,
        Some(Err(e)) if e.code == "InvalidArguments"
    ));
    assert!(matches!(
        handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/spawnwreck 99999999").await,
        Some(Err(e)) if e.code == "TypeNotFound"
    ));
    assert!(matches!(
        handle_session_cmd(&ctx, &sess, cid, "/session status"),
        Err(e) if e.code == "InvalidArguments"
    ));

    let dump = handle_session_cmd(&ctx, &sess, cid, "/session").expect("/session dump");
    assert!(dump.dict_get("sessionDict").is_some());
    assert!(dump.dict_get("wireState").is_some());
    assert!(dump.dict_get("entries").is_some());
    assert!(dump.dict_get("destinyStamps").is_some());

    let back = handle_gm_soak_slash_cmd(&ctx, &sess, cid, "/backintime")
        .await
        .expect("matched")
        .expect("/backintime");
    assert_eq!(back.dict_get("queueLength").and_then(|v| v.as_i64()), Some(0));
    ctx.db.with_snapshot(|snap| {
        let sk = snap.skills.get(&cid).expect("starter skills");
        let race_starter = &ctx.sde.races.get(&1).expect("Caldari race").starter_skills;
        let expected_count = race_starter.values().filter(|&&lvl| lvl > 0).count();
        assert!(expected_count > 0);
        assert_eq!(sk.len(), expected_count);
        for (&tid, &lvl) in race_starter {
            if lvl > 0 {
                assert_eq!(sk.get(&tid).map(|s| s.level), Some(lvl));
            }
        }
        assert!(snap.skill_queues.get(&cid).is_none_or(|q| q.is_empty()));
    });
}
