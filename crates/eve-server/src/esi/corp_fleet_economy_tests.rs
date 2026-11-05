//! Unit tests for ESI corporations, alliances, fleets, wallets, markets, contracts, industry, mining, and PI.

use super::{EsiHttpRequest, EsiRouter, OpenApiSpec};
use crate::router::ServerContext;
use eve_db::{
    AllianceRecord, ContractRecord, FleetRecord, IndustryJobRecord, InventoryFlag,
    InventoryItemRecord, IskCents, StarbaseDetailSettings,
};
use eve_sde::SdeRegistry;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn seed_corp_and_economy_context() -> (Arc<ServerContext>, String, i64, i64, i64, i64) {
    let ctx = if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let c = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&c.sde));
        c
    };

    let char_id = 90_000_001_i64;
    let corp_id = 98_000_001_i64;
    let alliance_id = 99_000_001_i64;
    let fleet_id = 7_700_001_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.alliance_id = Some(alliance_id);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = char_id;
            corp.alliance_id = Some(alliance_id);
            corp.members.insert(char_id);
            corp.member_roles.insert(char_id, !0_u64);
        }
        snap.alliances.insert(
            alliance_id,
            AllianceRecord {
                alliance_id,
                name: "Test Alliance".into(),
                short_name: "TSTA".into(),
                executor_corp_id: corp_id,
                member_corp_ids: HashSet::from([corp_id]),
            },
        );
        snap.alliance_lifecycle_meta.insert(
            alliance_id,
            eve_db::AllianceLifecycleMeta {
                creator_id: char_id,
                creator_corporation_id: corp_id,
                date_founded_unix: now,
                faction_id: None,
            },
        );
        snap.items.insert(
            1_080_000_001,
            InventoryItemRecord {
                item_id: 1_080_000_001,
                type_id: 12235,
                owner_id: corp_id,
                location_id: 30_000_142,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: Default::default(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.item_positions
            .insert(1_080_000_001, [10_000.0, 0.0, 0.0]);
        snap.starbase_settings
            .insert(1_080_000_001, StarbaseDetailSettings::default());
        snap.fleets.insert(
            fleet_id,
            FleetRecord {
                fleet_id,
                boss_id: char_id,
                members: vec![char_id],
                wings: Vec::new(),
                squads: Vec::new(),
                free_move: false,
                is_registered: false,
                wing_details: Default::default(),
                squad_details: Default::default(),
                member_details: Default::default(),
                invites: Default::default(),
                motd: "Welcome".into(),
            },
        );
        snap.industry_jobs.insert(
            66_001,
            IndustryJobRecord {
                job_id: 66_001,
                installer_id: char_id,
                facility_id: 60_003_760,
                blueprint_item_id: 100_000_001,
                blueprint_type_id: 687,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 600,
                start_unix: now,
                blueprint_location_id: 60_003_760,
                output_location_id: 60_003_760,
                cost: Some(1_000.0),
                licensed_runs: Some(10),
                probability: Some(1.0),
                completed_unix: None,
                completed_character_id: None,
            },
        );
        snap.contracts.insert(
            88_001,
            ContractRecord {
                contract_id: 88_001,
                issuer_id: char_id,
                issuer_corp_id: corp_id,
                contract_type: "Auction".into(),
                status: "Outstanding".into(),
                availability: "Public".into(),
                title: "Public Rifter".into(),
                start_station_id: 60_003_760,
                end_station_id: 60_003_760,
                price: IskCents::from_isk(500_000),
                date_issued_unix: now,
                expiration_unix: now + 86_400,
                num_days: 7,
                ..Default::default()
            },
        );
    });

    let scopes: BTreeSet<String> = OpenApiSpec::pinned()
        .operations
        .iter()
        .flat_map(|op| op.required_scopes.iter().cloned())
        .collect();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            char_id,
            "evomu-cli",
            "http://localhost/callback",
            scopes.into_iter().collect(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "S256",
            now,
        )
        .unwrap();
    let token = ctx
        .db
        .exchange_pkce_code(
            &code,
            "evomu-cli",
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
            now,
        )
        .unwrap()
        .access_token;

    (Arc::new(ctx), token, char_id, corp_id, alliance_id, fleet_id)
}

fn call_json(router: &EsiRouter, method: &str, path: &str, token: &str, body: Option<Value>) -> (u16, Value) {
    let mut req = EsiHttpRequest::new(method, path).with_bearer_token(token);
    if let Some(b) = body {
        req = req
            .with_header("Content-Type", "application/json")
            .with_body(serde_json::to_vec(&b).unwrap());
    }
    let resp = router.handle_request(&req);
    let val = if resp.body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&resp.body).unwrap_or(Value::Null)
    };
    (resp.status_code, val)
}

#[test]
fn esi_corporations_alliances_and_fleets_crud() {
    let (ctx, token, cid, corpid, aid, fid) = seed_corp_and_economy_context();
    let router = EsiRouter::new(Arc::clone(&ctx));

    for path in [
        "/latest/alliances/",
        &format!("/latest/alliances/{aid}/"),
        &format!("/latest/alliances/{aid}/corporations/"),
        &format!("/latest/alliances/{aid}/icons/"),
        &format!("/latest/alliances/{aid}/contacts/"),
        &format!("/latest/alliances/{aid}/contacts/labels/"),
        "/latest/corporations/npccorps/",
        &format!("/latest/corporations/{corpid}/"),
        &format!("/latest/corporations/{corpid}/alliancehistory/"),
        &format!("/latest/corporations/{corpid}/blueprints/"),
        &format!("/latest/corporations/{corpid}/containers/logs/"),
        &format!("/latest/corporations/{corpid}/divisions/"),
        &format!("/latest/corporations/{corpid}/facilities/"),
        &format!("/latest/corporations/{corpid}/icons/"),
        &format!("/latest/corporations/{corpid}/medals/"),
        &format!("/latest/corporations/{corpid}/medals/issued/"),
        &format!("/latest/corporations/{corpid}/members/"),
        &format!("/latest/corporations/{corpid}/members/limit/"),
        &format!("/latest/corporations/{corpid}/members/titles/"),
        &format!("/latest/corporations/{corpid}/membertracking/"),
        &format!("/latest/corporations/{corpid}/projects/"),
        &format!("/latest/corporations/{corpid}/roles/"),
        &format!("/latest/corporations/{corpid}/roles/history/"),
        &format!("/latest/corporations/{corpid}/shareholders/"),
        &format!("/latest/corporations/{corpid}/standings/"),
        &format!("/latest/corporations/{corpid}/starbases/"),
        &format!("/latest/corporations/{corpid}/starbases/1080000001/?system_id=30000142"),
        &format!("/latest/corporations/{corpid}/structures/"),
        &format!("/latest/corporations/{corpid}/titles/"),
        &format!("/latest/corporations/{corpid}/bookmarks/"),
        &format!("/latest/corporations/{corpid}/bookmarks/folders/"),
        &format!("/latest/corporations/{corpid}/contacts/"),
        &format!("/latest/corporations/{corpid}/contacts/labels/"),
    ] {
        let (st, _) = call_json(&router, "GET", path, &token, None);
        assert_eq!(st, 200, "GET {path}");
    }

    // Fleet full lifecycle: info, update MOTD, create/rename/delete wing & squad, invite/move/kick member
    assert_eq!(call_json(&router, "GET", &format!("/latest/characters/{cid}/fleet/"), &token, None).0, 200);
    assert_eq!(call_json(&router, "GET", &format!("/latest/fleets/{fid}/"), &token, None).0, 200);
    assert_eq!(
        call_json(
            &router,
            "PUT",
            &format!("/latest/fleets/{fid}/"),
            &token,
            Some(json!({"motd": "New MOTD", "is_free_move": true})),
        )
        .0,
        204
    );
    let (st_w, w_val) = call_json(&router, "POST", &format!("/latest/fleets/{fid}/wings/"), &token, None);
    assert_eq!(st_w, 201);
    let wid = w_val["wing_id"].as_i64().unwrap();
    assert_eq!(
        call_json(
            &router,
            "PUT",
            &format!("/latest/fleets/{fid}/wings/{wid}/"),
            &token,
            Some(json!({"name": "Hammer"})),
        )
        .0,
        204
    );
    let (st_s, s_val) = call_json(
        &router,
        "POST",
        &format!("/latest/fleets/{fid}/wings/{wid}/squads/"),
        &token,
        None,
    );
    assert_eq!(st_s, 201);
    let sid = s_val["squad_id"].as_i64().unwrap();
    assert_eq!(
        call_json(
            &router,
            "PUT",
            &format!("/latest/fleets/{fid}/squads/{sid}/"),
            &token,
            Some(json!({"name": "Alpha"})),
        )
        .0,
        204
    );
    assert_eq!(
        call_json(
            &router,
            "POST",
            &format!("/latest/fleets/{fid}/members/"),
            &token,
            Some(json!({"character_id": 90_000_002, "role": "squad_member", "wing_id": wid, "squad_id": sid})),
        )
        .0,
        204
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(f) = snap.fleets.get_mut(&fid) {
            f.invites.remove(&90_000_002);
            if !f.members.contains(&90_000_002) {
                f.members.push(90_000_002);
            }
            f.member_details.insert(
                90_000_002,
                eve_db::FleetMemberRecord {
                    character_id: 90_000_002,
                    role: eve_db::FleetRole::SquadMember,
                    wing_id: Some(wid),
                    squad_id: Some(sid),
                    takes_fleet_warp: true,
                    accepts_regroup: true,
                    accepts_conduit_jumps: true,
                    job: 0,
                    role_name: "Squad Member".into(),
                    join_time_unix: ctx.now_unix(),
                },
            );
        }
    });
    assert_eq!(
        call_json(
            &router,
            "PUT",
            &format!("/latest/fleets/{fid}/members/90000002/"),
            &token,
            Some(json!({"role": "squad_commander", "wing_id": wid, "squad_id": sid})),
        )
        .0,
        204
    );
    assert_eq!(call_json(&router, "GET", &format!("/latest/fleets/{fid}/members/"), &token, None).0, 200);
    assert_eq!(call_json(&router, "GET", &format!("/latest/fleets/{fid}/wings/"), &token, None).0, 200);
    assert_eq!(
        call_json(
            &router,
            "DELETE",
            &format!("/latest/fleets/{fid}/members/90000002/"),
            &token,
            None,
        )
        .0,
        204
    );
    assert_eq!(
        call_json(&router, "DELETE", &format!("/latest/fleets/{fid}/squads/{sid}/"), &token, None).0,
        204
    );
    assert_eq!(
        call_json(&router, "DELETE", &format!("/latest/fleets/{fid}/wings/{wid}/"), &token, None).0,
        204
    );

    // UI window open endpoints
    let _ = ctx.db.acquire_online_lock(1, 101, Some(cid), true, ctx.now_unix());
    let existing_contract_id = ctx
        .db
        .with_snapshot(|snap| snap.contracts.keys().copied().next())
        .unwrap_or(88_001);
    assert_eq!(
        call_json(
            &router,
            "POST",
            &format!("/latest/ui/openwindow/contract/?contract_id={existing_contract_id}"),
            &token,
            None
        )
        .0,
        204
    );
    assert_eq!(call_json(&router, "POST", "/latest/ui/openwindow/information/?target_id=90000001", &token, None).0, 204);
    assert_eq!(call_json(&router, "POST", "/latest/ui/openwindow/marketdetails/?type_id=34", &token, None).0, 204);
    assert_eq!(
        call_json(
            &router,
            "POST",
            "/latest/ui/openwindow/newmail/",
            &token,
            Some(json!({"subject": "Hi", "body": "Body", "recipients": [90_000_002]})),
        )
        .0,
        204
    );
}

#[test]
fn esi_wallets_markets_contracts_industry_and_mining_endpoints() {
    let (ctx, token, cid, corpid, _, _) = seed_corp_and_economy_context();
    let router = EsiRouter::new(Arc::clone(&ctx));

    for path in [
        &format!("/latest/characters/{cid}/wallet/"),
        &format!("/latest/characters/{cid}/wallet/journal/"),
        &format!("/latest/characters/{cid}/wallet/transactions/"),
        &format!("/latest/characters/{cid}/loyalty/points/"),
        &format!("/latest/loyalty/stores/{corpid}/offers/"),
        &format!("/latest/corporations/{corpid}/wallets/"),
        &format!("/latest/corporations/{corpid}/wallets/1/journal/"),
        &format!("/latest/corporations/{corpid}/wallets/1/transactions/"),
        "/latest/markets/groups/",
        "/latest/markets/groups/4/",
        "/latest/markets/prices/",
        "/latest/markets/10000002/orders/?order_type=all",
        "/latest/markets/10000002/history/?type_id=34",
        "/latest/markets/10000002/types/",
        &format!("/latest/characters/{cid}/orders/"),
        &format!("/latest/characters/{cid}/orders/history/"),
        &format!("/latest/corporations/{corpid}/orders/"),
        &format!("/latest/corporations/{corpid}/orders/history/"),
        "/latest/contracts/public/10000002/",
        "/latest/contracts/public/bids/88001/",
        "/latest/contracts/public/items/88001/",
        &format!("/latest/characters/{cid}/contracts/"),
        &format!("/latest/characters/{cid}/contracts/88001/bids/"),
        &format!("/latest/characters/{cid}/contracts/88001/items/"),
        &format!("/latest/corporations/{corpid}/contracts/"),
        "/latest/industry/facilities/",
        "/latest/industry/systems/",
        &format!("/latest/characters/{cid}/industry/jobs/"),
        &format!("/latest/corporations/{corpid}/industry/jobs/"),
        &format!("/latest/characters/{cid}/mining/"),
        &format!("/latest/corporation/{corpid}/mining/extractions/"),
        &format!("/latest/corporation/{corpid}/mining/observers/"),
        &format!("/latest/characters/{cid}/planets/"),
        &format!("/latest/corporations/{corpid}/customs_offices/"),
    ] {
        let (st, _) = call_json(&router, "GET", path, &token, None);
        assert_eq!(st, 200, "GET {path}");
    }
}
