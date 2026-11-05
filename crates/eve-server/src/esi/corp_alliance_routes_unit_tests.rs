//! Unit and mutation-killing tests for `esi::corp_alliance_routes`.

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn body(res: &RouteExecutionResult) -> &Value {
    res.body.as_ref().expect("response body")
}

#[test]
fn parse_path_map_blueprint_flag_and_map_upwell_state_cover_all_arms() {
    let empty = HashMap::new();
    assert_eq!(
        parse_path_i64(&empty, "corporation_id")
            .expect_err("missing")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("corporation_id", "abc")]), "corporation_id")
            .expect_err("nan")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("corporation_id", "0")]), "corporation_id")
            .expect_err("zero")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("corporation_id", "-2")]), "corporation_id")
            .expect_err("neg")
            .status_code,
        400
    );
    assert_eq!(
        parse_path_i64(&params(&[("corporation_id", "98000001")]), "corporation_id").unwrap(),
        98_000_001
    );

    assert_eq!(map_blueprint_flag(InventoryFlag::Hangar), "CorpSAG1");
    assert_eq!(map_blueprint_flag(InventoryFlag::Cargo), "Cargo");
    assert_eq!(
        map_blueprint_flag(InventoryFlag::CorpDeliveries),
        "CorpDeliveries"
    );
    assert_eq!(
        map_blueprint_flag(InventoryFlag::Deliveries),
        "CorpDeliveries"
    );
    assert_eq!(map_blueprint_flag(InventoryFlag::Locked), "Locked");
    assert_eq!(map_blueprint_flag(InventoryFlag::CorpSAG2), "CorpSAG2");
    assert_eq!(map_blueprint_flag(InventoryFlag::CorpSAG3), "CorpSAG3");
    assert_eq!(map_blueprint_flag(InventoryFlag::CorpSAG4), "CorpSAG4");
    assert_eq!(map_blueprint_flag(InventoryFlag::CorpSAG5), "CorpSAG5");
    assert_eq!(map_blueprint_flag(InventoryFlag::CorpSAG6), "CorpSAG6");
    assert_eq!(map_blueprint_flag(InventoryFlag::CorpSAG7), "CorpSAG7");
    assert_eq!(map_blueprint_flag(InventoryFlag::Capsule), "CorpSAG1");

    for (raw, expected) in [
        ("AnchorVulnerable", "anchor_vulnerable"),
        ("Anchoring", "anchoring"),
        ("NoCore", "fitting_invulnerable"),
        ("FittingInvulnerable", "fitting_invulnerable"),
        ("Onlining", "onlining_vulnerable"),
        ("OnliningVulnerable", "onlining_vulnerable"),
        ("ShieldVulnerable", "shield_vulnerable"),
        ("ArmorReinforced", "armor_reinforce"),
        ("ArmorVulnerable", "armor_vulnerable"),
        ("HullReinforced", "hull_reinforce"),
        ("HullVulnerable", "hull_vulnerable"),
        ("Unanchored", "unanchored"),
        ("SomeOtherState", "shield_vulnerable"),
    ] {
        assert_eq!(map_upwell_state(raw), expected, "mismatch for {raw}");
    }
}

#[test]
fn alliance_corporations_icons_and_corporation_public_info_routes() {
    let (ctx, _token, _char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let p_ally = params(&[("alliance_id", "99000001")]);
    let p_missing_ally = params(&[("alliance_id", "99999999")]);

    let corps = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/alliances/{alliance_id}/corporations",
        &p_ally,
        &empty_q,
    )
    .unwrap();
    assert_eq!(corps.status_code, 200);
    assert!(body(&corps).as_array().unwrap().contains(&json!(corp_id)));

    let icons = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/alliances/{alliance_id}/icons",
        &p_ally,
        &empty_q,
    )
    .unwrap();
    assert_eq!(icons.status_code, 200);
    assert!(body(&icons)["px64x64"].as_str().unwrap().contains("99000001"));

    for path in [
        "/alliances/{alliance_id}/corporations",
        "/alliances/{alliance_id}/icons",
    ] {
        assert_eq!(
            dispatch_corp_alliance_route(&ctx, "GET", path, &p_missing_ally, &empty_q)
                .unwrap()
                .status_code,
            404
        );
    }

    let player_corp = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}",
        &params(&[("corporation_id", &corp_id.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(player_corp.status_code, 200);
    assert_eq!(body(&player_corp)["ticker"], "DEMO");
    assert_eq!(body(&player_corp)["alliance_id"], 99_000_001);

    let npc_corp = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}",
        &params(&[("corporation_id", "1000035")]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(npc_corp.status_code, 200);
    assert!(body(&npc_corp)["tax_rate"].is_number());

    assert_eq!(
        dispatch_corp_alliance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}",
            &params(&[("corporation_id", "88888888")]),
            &empty_q
        )
        .unwrap()
        .status_code,
        404
    );
}

#[test]
fn corporation_blueprints_facilities_members_roles_standings_starbases_and_structures() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let corp_str = corp_id.to_string();
    let p_corp = params(&[("corporation_id", &corp_str)]);
    let empty_q = HashMap::new();

    let tower_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 365)
        .map(|t| t.type_id)
        .unwrap_or(12235);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_001,
            eve_db::InventoryItemRecord {
                item_id: 880_001,
                type_id: 688,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::CorpSAG2,
                quantity: 1,
                singleton: true,
                dynamic_attributes: HashMap::from([(1112, 8.0), (1113, 16.0)]),
                is_blueprint_copy: true,
                blueprint_runs_remaining: Some(10),
                ..Default::default()
            },
        );
        snap.items.insert(
            880_002,
            eve_db::InventoryItemRecord {
                item_id: 880_002,
                type_id: tower_type_id,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(880_002, true);
    });

    for path in [
        "/corporations/{corporation_id}/blueprints",
        "/corporations/{corporation_id}/facilities",
        "/corporations/{corporation_id}/icons",
        "/corporations/{corporation_id}/members",
        "/corporations/{corporation_id}/membertracking",
        "/corporations/{corporation_id}/roles",
        "/corporations/{corporation_id}/standings",
        "/corporations/{corporation_id}/starbases",
        "/corporations/{corporation_id}/structures",
    ] {
        let res = dispatch_corp_alliance_route(&ctx, "GET", path, &p_corp, &empty_q).unwrap();
        assert_eq!(res.status_code, 200, "failed on {path}");
    }

    let bps = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/blueprints",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    let bp_row = body(&bps)
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["item_id"] == 880_001)
        .expect("corp blueprint present");
    assert_eq!(bp_row["location_flag"], "CorpSAG2");
    assert_eq!(bp_row["quantity"], -2);
    assert_eq!(bp_row["material_efficiency"], 8);
    assert_eq!(bp_row["time_efficiency"], 16);
    assert_eq!(bp_row["runs"], 10);

    let sbs = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/starbases",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    let sb_row = body(&sbs)
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["starbase_id"] == 880_002)
        .expect("starbase present");
    assert_eq!(sb_row["state"], "online");
    assert_eq!(sb_row["system_id"], 30_000_142);

    let roles = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/roles",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert!(body(&roles)
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["character_id"] == char_id));
}
