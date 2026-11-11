//! Edge-case and full-branch unit tests for `esi::corp_alliance_routes`.

#![forbid(unsafe_code)]

use super::*;
use crate::esi::full_endpoint_catalog_tests::seed_full_esi_context;
use crate::services::station_structure::structure_upwell_ops::{
    load_ext_state, save_ext_state, StructureLifecycleState,
};
use eve_db::snapshot_ext::{CharacterLoginStats, StructureEsiTimerMeta};
use eve_db::{
    AllianceRecord, InventoryFlag, InventoryItemRecord, StandingTransactionRecord,
    UpwellStructureRecord,
};

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
fn alliance_and_corporation_routes_cover_all_400_404_and_optional_field_branches() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let empty_q = HashMap::new();
    let p_bad_ally = params(&[("alliance_id", "bad")]);
    let p_bad_corp = params(&[("corporation_id", "bad")]);
    let p_miss_corp = params(&[("corporation_id", "99999999")]);

    for path in [
        "/alliances/{alliance_id}/corporations",
        "/alliances/{alliance_id}/icons",
    ] {
        assert_eq!(
            dispatch_corp_alliance_route(&ctx, "GET", path, &p_bad_ally, &empty_q)
                .unwrap()
                .status_code,
            400
        );
    }

    for path in [
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
            dispatch_corp_alliance_route(&ctx, "GET", path, &p_bad_corp, &empty_q)
                .unwrap()
                .status_code,
            400,
            "expected 400 for {path}"
        );
        assert_eq!(
            dispatch_corp_alliance_route(&ctx, "GET", path, &p_miss_corp, &empty_q)
                .unwrap()
                .status_code,
            404,
            "expected 404 for {path}"
        );
    }

    // Alliance where executor_corp_id is not in member_corp_ids
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            99_000_077,
            AllianceRecord {
                alliance_id: 99_000_077,
                name: "Exec Separate".into(),
                short_name: "EXSP".into(),
                executor_corp_id: corp_id,
                member_corp_ids: std::collections::HashSet::new(),
            },
        );
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.description = "Corp desc".into();
            corp.url = "https://corp.example".into();
            corp.hq_station_id = Some(60_003_760);
        }
    });
    let ally_corps = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/alliances/{alliance_id}/corporations",
        &params(&[("alliance_id", "99000077")]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&ally_corps), &json!([corp_id]));

    let corp_info = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}",
        &params(&[("corporation_id", &corp_id.to_string())]),
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&corp_info)["description"], "Corp desc");
    assert_eq!(body(&corp_info)["url"], "https://corp.example");
    assert_eq!(body(&corp_info)["home_station_id"], 60_003_760);

    // Also test corp without alliance/hq/desc/url
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut corp2) = snap.corporations.get(&corp_id).cloned() {
            corp2.corporation_id = 98_000_077;
            corp2.alliance_id = None;
            corp2.hq_station_id = None;
            corp2.description.clear();
            corp2.url.clear();
            corp2.members.remove(&corp2.ceo_id);
            snap.corporations.insert(98_000_077, corp2);
        }
    });
    let corp2_info = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}",
        &params(&[("corporation_id", "98000077")]),
        &empty_q,
    )
    .unwrap();
    assert!(body(&corp2_info).get("alliance_id").is_none());
    assert!(body(&corp2_info).get("description").is_none());

    // Members / membertracking when ceo_id is not in members set
    let mems2 = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/members",
        &params(&[("corporation_id", "98000077")]),
        &empty_q,
    )
    .unwrap();
    assert!(body(&mems2).as_array().unwrap().contains(&json!(char_id)));
}

#[test]
fn corp_blueprints_facilities_membertracking_roles_standings_starbases_and_structures() {
    let (ctx, _token, char_id, corp_id, _planet_id) = seed_full_esi_context();
    let p_corp = params(&[("corporation_id", &corp_id.to_string())]);
    let empty_q = HashMap::new();
    let now = ctx.now_unix();

    let bp_tid = ctx.sde.blueprints.keys().next().copied().unwrap_or(688);
    let tower_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 365)
        .map(|t| t.type_id)
        .unwrap_or(12235);
    let fuel_groups =
        crate::services::station_structure::structure_lifecycle::service_fuel_groups(&ctx);
    let fuel_tid = ctx
        .sde
        .types
        .values()
        .find(|t| fuel_groups.contains(&t.group_id))
        .map(|t| t.type_id)
        .unwrap_or(4247);
    let agent_id = ctx.sde.agents.keys().next().copied().unwrap_or(3_008_416);

    let st_anchoring = 1_055_000_901_i64;
    let st_anchor_vuln = 1_055_000_902_i64;
    let st_onlining = 1_055_000_903_i64;

    ctx.db.with_mut_snapshot(|snap| {
        // Singleton BPO & stacked BPO in Hangar/Cargo
        snap.items.insert(
            881_001,
            InventoryItemRecord {
                item_id: 881_001,
                type_id: bp_tid,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            881_002,
            InventoryItemRecord {
                item_id: 881_002,
                type_id: bp_tid,
                owner_id: corp_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Cargo,
                quantity: 4,
                singleton: false,
                is_blueprint_copy: false,
                ..Default::default()
            },
        );
        // Offline starbase in solar system + starbase not in solar system (skipped)
        snap.items.insert(
            881_003,
            InventoryItemRecord {
                item_id: 881_003,
                type_id: tower_tid,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            881_004,
            InventoryItemRecord {
                item_id: 881_004,
                type_id: tower_tid,
                owner_id: corp_id,
                location_id: 60_003_760, // station, not solar system -> skipped
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );

        // Character login stats + location fallback to solar_system_id
        snap.character_login_stats.insert(
            char_id,
            CharacterLoginStats {
                last_login_unix: now.saturating_sub(100),
                last_logout_unix: now.saturating_sub(50),
                logins_count: 5,
            },
        );
        if let Some(ch2) = snap.characters.get_mut(&90_000_002) {
            ch2.station_id = None;
            ch2.structure_id = None;
            ch2.solar_system_id = 30_000_142;
        }
        // Give char 90_000_002 zero corp roles (skipped in /roles) and give char_id full HQ/Base/Other roles
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.member_roles.insert(90_000_002, 0);
            corp.member_roles.insert(char_id, !0u64);
            corp.hq_station_id = Some(60_003_760);
        }

        // Standing transactions: agent, npc_corp, faction, unknown (skipped)
        snap.standing_transactions.entry(char_id).or_default().extend(
            vec![
                StandingTransactionRecord {
                    event_id: 1,
                    from_id: agent_id,
                    to_id: corp_id,
                    modification: 1.25,
                    event_type_id: 1,
                },
                StandingTransactionRecord {
                    event_id: 2,
                    from_id: 1_000_035,
                    to_id: corp_id,
                    modification: 2.0,
                    event_type_id: 1,
                },
                StandingTransactionRecord {
                    event_id: 3,
                    from_id: 500_001,
                    to_id: corp_id,
                    modification: 15.0, // clamped to 10.0
                    event_type_id: 1,
                },
                StandingTransactionRecord {
                    event_id: 4,
                    from_id: 999_888_777, // unknown -> skipped
                    to_id: corp_id,
                    modification: 1.0,
                    event_type_id: 1,
                },
            ],
        );

        // Structures in various states, fuel bays, online service modules, vulnerability windows, timers
        for (sid, state) in [
            (st_anchoring, "Anchoring"),
            (st_anchor_vuln, "AnchorVulnerable"),
            (st_onlining, "Onlining"),
        ] {
            snap.structures.insert(
                sid,
                UpwellStructureRecord {
                    structure_id: sid,
                    type_id: 35_832,
                    owner_corp_id: corp_id,
                    solar_system_id: 30_000_142,
                    name: format!("Struct {state}"),
                    position: [0.0, 0.0, 0.0],
                    state: state.into(),
                    quantum_core_installed: true,
                    fuel_blocks: if sid == st_onlining { 0 } else { 100 },
                    reinforced_until_unix: now + 3600,
                    ..Default::default()
                },
            );
        }
        // Fuel bay item for st_onlining + online and offline service modules
        snap.items.insert(
            881_010,
            InventoryItemRecord {
                item_id: 881_010,
                type_id: fuel_tid,
                owner_id: corp_id,
                location_id: st_onlining,
                flag: InventoryFlag::StructureFuel,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            881_011,
            InventoryItemRecord {
                item_id: 881_011,
                type_id: 35_892, // Standup Market Hub I
                owner_id: corp_id,
                location_id: st_onlining,
                flag: InventoryFlag::ServiceSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            881_012,
            InventoryItemRecord {
                item_id: 881_012,
                type_id: 35_892,
                owner_id: corp_id,
                location_id: st_onlining,
                flag: InventoryFlag::ServiceSlot1,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(881_011, true);
        snap.module_online_states.insert(881_012, false);

        snap.structure_vulnerability_windows
            .insert(st_onlining, (72_000, now + 86_400));
        snap.structure_esi_timer_meta.insert(
            st_anchor_vuln,
            StructureEsiTimerMeta {
                profile_id: 5,
                fuel_expires_unix: Some(now + 7200),
                state_timer_start_unix: Some(now - 600),
                state_timer_end_unix: Some(now + 3600),
                unanchors_at_unix: Some(now + 86_400),
                next_reinforce_hour: Some(19),
                next_reinforce_apply_unix: Some(now + 100_000),
            },
        );

        let mut ext = load_ext_state(snap);
        ext.lifecycle.insert(
            st_onlining,
            StructureLifecycleState {
                online_modules: vec![881_011],
                last_fuel_tick_unix: Some(now - 600),
                ..Default::default()
            },
        );
        ext.lifecycle.insert(
            st_anchoring,
            StructureLifecycleState {
                anchoring_duration_secs: Some(900),
                ..Default::default()
            },
        );
        ext.pending_reinforce_schedule
            .insert(st_anchoring, (21, 2, now + 86_400));
        ext.unanchoring_until_unix.insert(st_onlining, now + 50_000);
        save_ext_state(snap, &ext);
    });

    // Blueprints & pagination error
    let bps = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/blueprints",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert!(body(&bps)
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["item_id"] == 881_001 && b["quantity"] == -1));
    assert!(body(&bps)
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b["item_id"] == 881_002 && b["quantity"] == 4));
    assert_eq!(
        dispatch_corp_alliance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/blueprints",
            &p_corp,
            &params(&[("page", "999")]),
        )
        .unwrap()
        .status_code,
        404
    );

    // Facilities includes st_onlining (has online ServiceSlot0)
    let facs = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/facilities",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert!(body(&facs)
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["facility_id"] == st_onlining));

    // Membertracking logon/logoff & base_id
    let mt = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/membertracking",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    let m1 = body(&mt)
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["character_id"] == char_id)
        .unwrap();
    assert!(m1.get("logon_date").is_some());
    assert!(m1.get("logoff_date").is_some());
    assert_eq!(m1["base_id"], 60_003_760);

    // Roles skips 0-role member
    let roles = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/roles",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert!(!body(&roles)
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["character_id"] == 90_000_002));

    // Standings & pagination error
    let stds = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/standings",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert_eq!(body(&stds).as_array().unwrap().len(), 3);
    assert!(body(&stds)
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["from_type"] == "faction" && s["standing"] == 10.0));
    assert_eq!(
        dispatch_corp_alliance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/standings",
            &p_corp,
            &params(&[("page", "999")]),
        )
        .unwrap()
        .status_code,
        404
    );

    // Starbases offline & pagination error
    let sbs = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/starbases",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert!(body(&sbs)
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["starbase_id"] == 881_003 && s["state"] == "offline"));
    assert_eq!(
        dispatch_corp_alliance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/starbases",
            &p_corp,
            &params(&[("page", "999")]),
        )
        .unwrap()
        .status_code,
        404
    );

    // Structures with live anchoring, timers, fuel derivation, vuln windows, & pagination error
    let structs = dispatch_corp_alliance_route(
        &ctx,
        "GET",
        "/corporations/{corporation_id}/structures",
        &p_corp,
        &empty_q,
    )
    .unwrap();
    assert_eq!(structs.status_code, 200);
    let s_arr = body(&structs).as_array().unwrap();
    let s_on = s_arr
        .iter()
        .find(|s| s["structure_id"] == st_onlining)
        .unwrap();
    assert!(s_on.get("fuel_expires").is_some());
    assert!(s_on.get("unanchors_at").is_some());
    assert_eq!(s_on["reinforce_hour"], 20);
    assert_eq!(
        dispatch_corp_alliance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/structures",
            &p_corp,
            &params(&[("page", "999")]),
        )
        .unwrap()
        .status_code,
        404
    );
}
