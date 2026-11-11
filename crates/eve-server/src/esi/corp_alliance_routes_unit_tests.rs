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

#[test]
fn corp_alliance_errors_standings_ceo_fallbacks_and_pagination() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_p = HashMap::new();
    let empty_q = HashMap::new();
    let bad_page = params(&[("page", "999")]);
    let p_missing_ally = params(&[("alliance_id", "99999998")]);
    let p_missing_corp = params(&[("corporation_id", "98999998")]);

    for path in [
        "/alliances/{alliance_id}/corporations",
        "/alliances/{alliance_id}/icons",
        "/corporations/{corporation_id}",
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
        assert_eq!(
            dispatch_corp_alliance_route(&ctx, "GET", path, &empty_p, &empty_q)
                .unwrap()
                .status_code,
            400,
            "missing param 400 expected on {path}"
        );
        let p_missing = if path.contains("alliance_id") {
            &p_missing_ally
        } else {
            &p_missing_corp
        };
        assert_eq!(
            dispatch_corp_alliance_route(&ctx, "GET", path, p_missing, &empty_q)
                .unwrap()
                .status_code,
            404,
            "404 expected on {path}"
        );
    }

    let agent_id = *ctx.sde.agents.keys().next().expect("sde agent");
    let faction_id = i64::from(*ctx.sde.factions.keys().next().expect("sde faction"));
    let solo_corp_id = 98_777_001_i64;
    let solo_ceo_id = 90_777_001_i64;
    let ally_exec_id = 99_777_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut c) = snap.characters.values().next().cloned() {
            c.character_id = solo_ceo_id;
            c.name = "Solo Ceo".to_string();
            c.corporation_id = corp_id;
            c.alliance_id = None;
            snap.characters.insert(solo_ceo_id, c);
        }
        snap.corporations.insert(
            solo_corp_id,
            { let mut __corp = eve_db::CorporationRecord::default(); __corp.corporation_id = solo_corp_id; __corp.name = "Solo Corp".to_string(); __corp.ticker = "SOLO".to_string(); __corp.ceo_id = solo_ceo_id; __corp.tax_rate = 0.05; __corp.hq_station_id = Some(60_003_760); __corp.alliance_id = None; __corp.members = Default::default(); __corp.member_roles = std::iter::once((solo_ceo_id, 1)).collect(); __corp.applications = Default::default(); __corp.wallet_divisions = eve_db::CorporationRecord::default_wallet_divisions(
                    eve_db::IskCents(0),
                ); __corp.description = String::new(); __corp.url = String::new(); let __mw = eve_db::IskCents(0); if __mw.0 != 0 { __corp.set_division_balance(1, __mw); } else { __corp.ensure_wallet_divisions(); } __corp },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            ally_exec_id,
            eve_db::AllianceRecord {
                alliance_id: ally_exec_id,
                name: "Exec Ally".to_string(),
                short_name: "EXEC".to_string(),
                executor_corp_id: solo_corp_id,
                member_corp_ids: std::iter::once(corp_id).collect(),
            },
        );
        if let Some(sc) = snap.corporations.get_mut(&solo_corp_id) {
            sc.alliance_id = Some(ally_exec_id);
        }
        snap.character_login_stats.insert(
            solo_ceo_id,
            eve_db::CharacterLoginStats {
                last_login_unix: 1_700_000_100,
                last_logout_unix: 1_700_000_200,
                logins_count: 3,
            },
        );
        snap.items.insert(
            880_101,
            eve_db::InventoryItemRecord {
                item_id: 880_101,
                type_id: 688,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 4,
                singleton: false,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        snap.standing_transactions.insert(
            char_id,
            vec![
                eve_db::StandingTransactionRecord {
                    event_id: 1,
                    from_id: agent_id,
                    to_id: corp_id,
                    modification: 2.5,
                    event_type_id: 1,
                },
                eve_db::StandingTransactionRecord {
                    event_id: 2,
                    from_id: faction_id,
                    to_id: corp_id,
                    modification: 1.25,
                    event_type_id: 1,
                },
                eve_db::StandingTransactionRecord {
                    event_id: 3,
                    from_id: 999_888_777,
                    to_id: corp_id,
                    modification: 1.0,
                    event_type_id: 1,
                },
            ],
        );
    });

    let ally_corps = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/alliances/{alliance_id}/corporations",
        &params(&[("alliance_id", &ally_exec_id.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert!(body(&ally_corps)
        .as_array()
        .unwrap()
        .contains(&json!(solo_corp_id)));

    let p_solo = params(&[("corporation_id", &solo_corp_id.to_string())]);
    assert!(body(
        &dispatch_corp_alliance_route(&ctx, "GET", "/corporations/{corporation_id}/members", &p_solo, &empty_q)
            .unwrap()
    )
    .as_array()
    .unwrap()
    .contains(&json!(solo_ceo_id)));

    let track = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/membertracking",
        &p_solo,
        &empty_q,
    )
    .unwrap();
    let row = body(&track).as_array().unwrap().first().unwrap();
    assert_eq!(row["character_id"], solo_ceo_id);
    assert_eq!(row["base_id"], 60_003_760);
    assert!(row.get("logon_date").is_some());
    assert!(row.get("logoff_date").is_some());

    assert!(body(
        &dispatch_corp_alliance_route(&ctx, "GET", "/corporations/{corporation_id}/roles", &p_solo, &empty_q)
            .unwrap()
    )
    .as_array()
    .unwrap()
    .iter()
    .any(|r| r["character_id"] == solo_ceo_id));

    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let bps = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/blueprints",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    let stacked = body(&bps)
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["item_id"] == 880_101)
        .unwrap();
    assert_eq!(stacked["quantity"], 4);

    let st = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/standings",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    let st_arr = body(&st).as_array().unwrap();
    assert!(st_arr.iter().any(|r| r["from_type"] == "agent"));
    assert!(st_arr.iter().any(|r| r["from_type"] == "faction"));
    assert!(!st_arr.iter().any(|r| r["from_id"] == 999_888_777));

    for path in [
        "/corporations/{corporation_id}/blueprints",
        "/corporations/{corporation_id}/standings",
        "/corporations/{corporation_id}/starbases",
        "/corporations/{corporation_id}/structures",
    ] {
        assert_eq!(
            dispatch_corp_alliance_route(&ctx, "GET", path, &p_corp, &bad_page)
                .unwrap()
                .status_code,
            404,
            "pagination 404 expected on {path}"
        );
    }
    let _ = char_id;
}

#[test]
fn corp_facilities_and_structures_cover_services_fuel_timers_and_reinforce() {
    let (ctx, _token, _char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);

    let fuel_groups =
        crate::services::station_structure::structure_lifecycle::service_fuel_groups(&ctx);
    let fuel_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| fuel_groups.contains(&t.group_id))
        .map(|t| t.type_id)
        .expect("fuel block type");
    let svc_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| {
            t.default_attributes.contains_key(
                &crate::services::station_structure::structure_lifecycle::ATTR_SERVICE_FUEL_PER_HOUR,
            )
        })
        .map(|t| t.type_id)
        .expect("service module type");

    let s1 = 1_000_088_001_i64;
    let s2 = 1_000_088_002_i64;
    let s3 = 1_000_088_003_i64;
    let svc_on_id = 881_001_i64;
    let svc_off_id = 881_002_i64;
    let fuel_item_id = 881_003_i64;

    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            s1,
            eve_db::UpwellStructureRecord {
                structure_id: s1,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Live Citadel".to_string(),
                state: "Anchoring".to_string(),
                quantum_core_installed: true,
                fuel_blocks: 0,
                reinforced_until_unix: now + 90_000,
                ..Default::default()
            },
        );
        snap.structures.insert(
            s2,
            eve_db::UpwellStructureRecord {
                structure_id: s2,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Anchor Vuln".to_string(),
                state: "AnchorVulnerable".to_string(),
                reinforced_until_unix: now + 10_000,
                ..Default::default()
            },
        );
        snap.structures.insert(
            s3,
            eve_db::UpwellStructureRecord {
                structure_id: s3,
                type_id: 35832,
                owner_corp_id: corp_id,
                solar_system_id: 30_000_142,
                name: "Onlining Vuln".to_string(),
                state: "OnliningVulnerable".to_string(),
                reinforced_until_unix: now + 20_000,
                ..Default::default()
            },
        );
        snap.items.insert(
            svc_on_id,
            eve_db::InventoryItemRecord {
                item_id: svc_on_id,
                type_id: svc_type_id,
                owner_id: corp_id,
                location_id: s1,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            svc_off_id,
            eve_db::InventoryItemRecord {
                item_id: svc_off_id,
                type_id: svc_type_id,
                owner_id: corp_id,
                location_id: s1,
                flag: InventoryFlag::ServiceSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            fuel_item_id,
            eve_db::InventoryItemRecord {
                item_id: fuel_item_id,
                type_id: fuel_type_id,
                owner_id: corp_id,
                location_id: s1,
                flag: InventoryFlag::StructureFuel,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(svc_on_id, true);
        snap.module_online_states.insert(svc_off_id, false);
        snap.structure_vulnerability_windows
            .insert(s1, (72_000, now + 500_000));
        snap.structure_vulnerability_windows.insert(s2, (14, 0));

        let mut ext =
            crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.structure_profile_id.insert(s1, 42);
        ext.unanchoring_until_unix.insert(s1, now + 600_000);
        ext.pending_reinforce_schedule
            .insert(s2, (20, 2, now + 700_000));
        ext.lifecycle.insert(
            s1,
            crate::services::station_structure::structure_upwell_ops::StructureLifecycleState {
                last_fuel_tick_unix: Some(now),
                online_modules: vec![svc_on_id],
                anchoring_duration_secs: Some(86_400),
                shield_fight_started_unix: Some(now.saturating_sub(60)),
                ..Default::default()
            },
        );
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });

    let fac = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/facilities",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert_eq!(fac.status_code, 200);
    assert!(body(&fac)
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["facility_id"] == s1 && f["type_id"] == 35832));

    let st_res = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/structures",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert_eq!(st_res.status_code, 200);
    let rows = body(&st_res).as_array().unwrap();
    let r1 = rows.iter().find(|r| r["structure_id"] == s1).unwrap();
    assert_eq!(r1["profile_id"], 42);
    assert!(r1.get("fuel_expires").is_some());
    assert!(r1.get("unanchors_at").is_some());
    assert!(r1.get("state_timer_start").is_some());
    assert!(r1.get("next_reinforce_apply").is_some());
    assert_eq!(r1["services"].as_array().unwrap().len(), 2);

    let r2 = rows.iter().find(|r| r["structure_id"] == s2).unwrap();
    assert_eq!(r2["reinforce_hour"], 14);
    assert_eq!(r2["next_reinforce_hour"], 20);
    assert!(r2.get("state_timer_start").is_some());

    let r3 = rows.iter().find(|r| r["structure_id"] == s3).unwrap();
    assert!(r3.get("state_timer_start").is_some());
}
