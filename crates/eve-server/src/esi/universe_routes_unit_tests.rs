//! Unit and mutation-killing tests for `esi::universe_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn make_claims(char_id: i64) -> SsoTokenClaims {
    SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{char_id}"),
        name: "NavTester".to_string(),
        owner: "hash".to_string(),
        user_id: 1,
        character_id: char_id,
        scp: Vec::new(),
        iss: "evomu".to_string(),
        aud: vec!["EVE Online".to_string()],
        iat: 1_700_000_000,
        exp: 1_800_000_000,
        jti: "jti-1".to_string(),
    }
}

#[test]
fn jump_adjacency_cache_and_structure_acl_access_cover_all_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let adj1 = cached_base_jump_adjacency(&ctx);
    let adj2 = cached_base_jump_adjacency(&ctx);
    assert!(Arc::ptr_eq(&adj1, &adj2));
    assert!(!adj1.get(&30_000_142).unwrap().is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        let mut st = snap.structures.get(&1_055_000_001).unwrap().clone();
        // Same corp -> allowed
        assert!(character_has_structure_acl_access(snap, char_id, &st));

        // Destroyed structure -> denied
        st.state = "Destroyed".to_string();
        assert!(!character_has_structure_acl_access(snap, char_id, &st));
        st.state = "ShieldVulnerable".to_string();

        // Unknown character -> denied
        assert!(!character_has_structure_acl_access(snap, 99_999_999, &st));

        // Explicitly in structure_docking_acl_denied -> denied even for same corp
        snap.structure_docking_acl_denied
            .entry(st.structure_id)
            .or_default()
            .insert(char_id);
        assert!(!character_has_structure_acl_access(snap, char_id, &st));
        snap.structure_docking_acl_denied.clear();

        // Different corp, negative standing -> denied
        st.owner_corp_id = 1_000_125;
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_125, -1.5);
        }
        assert!(!character_has_structure_acl_access(snap, char_id, &st));

        // Neutral standing, no active war -> allowed
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(1_000_125, 0.0);
        }
        assert!(character_has_structure_acl_access(snap, char_id, &st));

        // Active war between pilot corp and owner corp -> denied
        snap.wars.insert(
            999,
            eve_db::WarRecord {
                war_id: 999,
                aggressor_corp_id: corp_id,
                defender_corp_id: 1_000_125,
                started_unix: 1_700_000_000,
                mutual: false,
                active: true,
            },
        );
        assert!(!character_has_structure_acl_access(snap, char_id, &st));
    });
}

#[test]
fn dispatch_route_planner_and_character_search_cover_flags_avoid_waypoints_and_strict() {
    let (ctx, _token, char_id, _corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let p_same = params(&[("origin", "30000142"), ("destination", "30000142")]);
    let p_hop = params(&[("origin", "30000142"), ("destination", "30000144")]);

    // Invalid flag -> 400; invalid avoid -> 400; invalid connection -> 400; unknown system -> 404
    assert_eq!(
        dispatch_universe_and_nav(
            &ctx,
            "GET",
            "/route/{origin}/{destination}",
            &p_hop,
            &params(&[("flag", "warp")]),
            None,
            1,
            b""
        )
        .status_code,
        400
    );
    assert_eq!(
        dispatch_universe_and_nav(
            &ctx,
            "GET",
            "/route/{origin}/{destination}",
            &p_hop,
            &params(&[("avoid", "bad")]),
            None,
            1,
            b""
        )
        .status_code,
        400
    );
    assert_eq!(
        dispatch_universe_and_nav(
            &ctx,
            "GET",
            "/route/{origin}/{destination}",
            &p_hop,
            &params(&[("connections", "badpair")]),
            None,
            1,
            b""
        )
        .status_code,
        400
    );
    assert_eq!(
        dispatch_universe_and_nav(
            &ctx,
            "GET",
            "/route/{origin}/{destination}",
            &params(&[("origin", "30000142"), ("destination", "99999999")]),
            &empty_q,
            None,
            1,
            b""
        )
        .status_code,
        404
    );

    // Same system avoided -> 404; same system not avoided -> [origin]
    assert_eq!(
        dispatch_universe_and_nav(
            &ctx,
            "GET",
            "/route/{origin}/{destination}",
            &p_same,
            &params(&[("avoid", "30000142")]),
            None,
            1,
            b""
        )
        .status_code,
        404
    );
    let same_ok = dispatch_universe_and_nav(
        &ctx,
        "GET",
        "/route/{origin}/{destination}",
        &p_same,
        &empty_q,
        None,
        1,
        b"",
    );
    assert_eq!(same_ok.status_code, 200);
    assert_eq!(same_ok.body, Some(json!([30_000_142])));

    // Shortest, secure, insecure, waypoints, connections
    for q in [
        params(&[("flag", "shortest")]),
        params(&[("flag", "secure")]),
        params(&[("flag", "insecure")]),
        params(&[("waypoints", "30000144")]),
        params(&[("connections", "30000142|30000144")]),
    ] {
        let res = dispatch_universe_and_nav(
            &ctx,
            "GET",
            "/route/{origin}/{destination}",
            &p_hop,
            &q,
            None,
            1,
            b"",
        );
        assert_eq!(res.status_code, 200);
        assert!(!res.body.as_ref().unwrap().as_array().unwrap().is_empty());
    }

    // Character search: invalid category -> 400; too short -> 400; strict vs substring -> 200
    let p_char = params(&[("character_id", &char_id.to_string())]);
    let search_param = "search";
    assert_eq!(
        dispatch_universe_and_nav(
            &ctx,
            "GET",
            "/characters/{character_id}/search",
            &p_char,
            &params(&[("categories", "spaceship"), (search_param, "Jita")]),
            None,
            1,
            b""
        )
        .status_code,
        400
    );
    let search_ok = dispatch_universe_and_nav(
        &ctx,
        "GET",
        "/characters/{character_id}/search",
        &p_char,
        &params(&[
            ("categories", "solar_system,inventory_type,character,corporation,alliance,station,structure"),
            (search_param, "Jita"),
            ("strict", "true"),
        ]),
        Some(&make_claims(char_id)),
        1,
        b"",
    );
    assert_eq!(search_ok.status_code, 200);
    assert!(search_ok.body.as_ref().unwrap()["solar_system"]
        .as_array()
        .unwrap()
        .contains(&json!(30_000_142)));
}

#[test]
fn dispatch_universe_catalog_names_ids_structures_and_autopilot_waypoint() {
    let (ctx, _token, char_id, _corp_id, planet_id) = seed_full_esi_context();
    let claims = make_claims(char_id);
    let empty_q = HashMap::new();

    for (path, p) in [
        ("/universe/categories", params(&[])),
        ("/universe/categories/{category_id}", params(&[("category_id", "6")])),
        ("/universe/groups", params(&[])),
        ("/universe/groups/{group_id}", params(&[("group_id", "25")])),
        ("/universe/types", params(&[])),
        ("/universe/types/{type_id}", params(&[("type_id", "587")])),
        ("/universe/regions", params(&[])),
        ("/universe/regions/{region_id}", params(&[("region_id", "10000002")])),
        ("/universe/constellations", params(&[])),
        (
            "/universe/constellations/{constellation_id}",
            params(&[("constellation_id", "20000020")]),
        ),
        ("/universe/systems", params(&[])),
        ("/universe/systems/{system_id}", params(&[("system_id", "30000142")])),
        (
            "/universe/planets/{planet_id}",
            params(&[("planet_id", &planet_id.to_string())]),
        ),
        ("/universe/structures", params(&[])),
        (
            "/universe/structures/{structure_id}",
            params(&[("structure_id", "1055000001")]),
        ),
        ("/universe/factions", params(&[])),
        ("/universe/bloodlines", params(&[])),
        ("/universe/ancestries", params(&[])),
    ] {
        let res = dispatch_universe_and_nav(&ctx, "GET", path, &p, &empty_q, Some(&claims), 1, b"");
        assert_eq!(res.status_code, 200, "failed on {path}");
    }

    // POST /universe/names and POST /universe/ids
    let names_res = dispatch_universe_and_nav(
        &ctx,
        "POST",
        "/universe/names",
        &empty_q,
        &empty_q,
        None,
        1,
        br"[30000142, 587, 90000001, 98000001, 99000001]",
    );
    assert_eq!(names_res.status_code, 200);
    assert_eq!(names_res.body.as_ref().unwrap().as_array().unwrap().len(), 5);

    let ids_res = dispatch_universe_and_nav(
        &ctx,
        "POST",
        "/universe/ids",
        &empty_q,
        &empty_q,
        None,
        1,
        br#"["Jita", "Rifter", "Test Corporation"]"#,
    );
    assert_eq!(ids_res.status_code, 200);
    assert!(ids_res.body.as_ref().unwrap().get("systems").is_some());

    // POST /ui/autopilot/waypoint
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    let _ = ctx
        .db
        .acquire_online_lock(uid, 999, Some(char_id), false, ctx.now_unix());
    let wp_res = dispatch_universe_and_nav(
        &ctx,
        "POST",
        "/ui/autopilot/waypoint",
        &empty_q,
        &params(&[
            ("destination_id", "30000142"),
            ("add_to_beginning", "true"),
            ("clear_other_waypoints", "true"),
        ]),
        Some(&claims),
        1,
        b"",
    );
    assert_eq!(wp_res.status_code, 204);
}

#[test]
fn private_structure_is_hidden_from_public_list_detail_and_search() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let outsider_id = 90_000_099_i64;
    let private_sid = 1_055_000_099_i64;
    let public_sid = 1_055_000_100_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = outsider_id;
        ch.name = "Neutral Outsider".into();
        ch.corporation_id = 98_999_888;
        ch.alliance_id = None;
        ch.standings.clear();
        snap.characters.insert(outsider_id, ch);
        for (sid, name, corp_only) in [
            (private_sid, "Private Keepstar", true),
            (public_sid, "Public Freeport Keepstar", false),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_834,
                    owner_corp_id: corp_id,
                    solar_system_id: 30_000_142,
                    name: name.into(),
                    position: [10_000.0, 20_000.0, 30_000.0],
                    state: "ShieldVulnerable".into(),
                    quantum_core_installed: true,
                    fuel_blocks: 500,
                    ..Default::default()
                },
            );
            let mut ext =
                crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
            ext.access_profiles.insert(
                sid,
                crate::services::station_structure::structure_upwell_ops::StructureAccessProfile {
                    corp_only,
                    ..Default::default()
                },
            );
            crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
        }
    });

    let empty_q = HashMap::new();
    let list_res =
        dispatch_universe_and_nav(&ctx, "GET", "/universe/structures", &empty_q, &empty_q, None, 1, b"");
    assert_eq!(list_res.status_code, 200);
    let ids = list_res.body.unwrap();
    let arr = ids.as_array().unwrap();
    assert!(!arr.contains(&json!(private_sid)));
    assert!(arr.contains(&json!(public_sid)));

    let outsider_claims = make_claims(outsider_id);
    let priv_detail = dispatch_universe_and_nav(
        &ctx,
        "GET",
        "/universe/structures/{structure_id}",
        &params(&[("structure_id", &private_sid.to_string())]),
        &empty_q,
        Some(&outsider_claims),
        1,
        b"",
    );
    assert_eq!(priv_detail.status_code, 403);

    let owner_claims = make_claims(char_id);
    let owner_detail = dispatch_universe_and_nav(
        &ctx,
        "GET",
        "/universe/structures/{structure_id}",
        &params(&[("structure_id", &private_sid.to_string())]),
        &empty_q,
        Some(&owner_claims),
        1,
        b"",
    );
    assert_eq!(owner_detail.status_code, 200);

    let search_query = "Keepstar";
    let search_outsider = dispatch_universe_and_nav(
        &ctx,
        "GET",
        "/characters/{character_id}/search",
        &params(&[("character_id", &outsider_id.to_string())]),
        &params(&[("categories", "structure"), ("search", search_query)]),
        Some(&outsider_claims),
        1,
        b"",
    );
    assert_eq!(search_outsider.status_code, 200);
    let outsider_hits = search_outsider.body.unwrap()["structure"]
        .as_array()
        .cloned()
        .unwrap();
    assert!(!outsider_hits.contains(&json!(private_sid)));
    assert!(outsider_hits.contains(&json!(public_sid)));

    let search_owner = dispatch_universe_and_nav(
        &ctx,
        "GET",
        "/characters/{character_id}/search",
        &params(&[("character_id", &char_id.to_string())]),
        &params(&[("categories", "structure"), ("search", search_query)]),
        Some(&owner_claims),
        1,
        b"",
    );
    assert_eq!(search_owner.status_code, 200);
    let owner_hits = search_owner.body.unwrap()["structure"]
        .as_array()
        .cloned()
        .unwrap();
    assert!(owner_hits.contains(&json!(private_sid)));
    assert!(owner_hits.contains(&json!(public_sid)));
}
