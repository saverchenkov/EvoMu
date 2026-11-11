//! Cross-protocol parity unit tests verifying MachoNet RPCs, Quasar handlers, and ESI HTTP
//! routes execute identical domain validation, state transitions, and authorization checks (`#593`).

#![forbid(unsafe_code)]

use crate::domain::{corp_governance, fleets, mail, market_acl};
use crate::esi::char_social_routes::dispatch_char_social_route;
use crate::esi::character_mail_routes::dispatch_character_mail_routes;
use crate::esi::corp_governance_routes::dispatch_corp_governance_route;
use crate::esi::economy_routes::dispatch_economy_route;
use crate::esi::fleet_routes::dispatch_fleet_routes;
use crate::router::ServerContext;
use crate::services::quasar_career_goals::default_career_milestones;
use eve_db::{
    corp_roles, snapshot_ext::MarketOrderMeta, CorpMemberRoleAssignment, InventoryFlag, IskCents,
    MarketOrderRecord, SsoTokenClaims, UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn params(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn make_session(char_id: i64, corp_id: i64) -> SessionState {
    let mut s = SessionState::new(710);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.region_id = Some(10_000_002);
    s
}

fn make_claims(char_id: i64) -> SsoTokenClaims {
    SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{char_id}"),
        name: "ParityTester".to_string(),
        owner: "hash".to_string(),
        user_id: 1,
        character_id: char_id,
        scp: Vec::new(),
        iss: "evomu".to_string(),
        aud: vec!["EVE Online".to_string()],
        iat: 1_700_000_000,
        exp: 1_800_000_000,
        jti: "parity-jti".to_string(),
    }
}

#[tokio::test]
async fn parity_fleets_kick_bound_object_eviction_and_boss_guard() {
    let ctx = test_ctx();
    let boss_id = 90_000_001;
    let wingman_id = 90_000_002;
    let mut boss_sess = make_session(boss_id, 98_000_001);
    let mut wing_sess = make_session(wingman_id, 98_000_001);
    let kw: HashMap<String, PyRep> = HashMap::new();

    let created = ctx
        .dispatch_rpc(
            &mut boss_sess,
            "fleetObjectHandler",
            None,
            "CreateFleet",
            &[],
            &kw,
            None,
        )
        .await
        .expect("MachoNet CreateFleet");
    let fleet_id = created.as_i64().unwrap_or_else(|| boss_sess.fleet_id.unwrap());

    // Invite via ESI, accept via MachoNet.
    let empty = HashMap::new();
    let boss_claims = make_claims(boss_id);
    let inv_body = serde_json::to_vec(&json!({
        "character_id": wingman_id,
        "role": "squad_member",
    }))
    .unwrap();
    let inv_res = dispatch_fleet_routes(
        &ctx,
        "POST",
        "/fleets/{fleet_id}/members",
        &params(&[("fleet_id", &fleet_id.to_string())]),
        &empty,
        Some(&boss_claims),
        &inv_body,
    )
    .unwrap();
    assert_eq!(inv_res.status_code, 204);

    let _accepted = ctx
        .dispatch_rpc(
            &mut wing_sess,
            "fleetMgr",
            None,
            "AcceptInvite",
            &[PyRep::Integer(fleet_id), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("MachoNet AcceptInvite");
    assert_eq!(
        fleets::get_character_fleet(&ctx, wingman_id).unwrap().fleet_id,
        fleet_id
    );

    // Subordinate cannot kick the fleet boss via either MachoNet or ESI (`CannotKickBoss`).
    let boss_kick_rpc = ctx
        .dispatch_rpc(
            &mut wing_sess,
            "fleetMgr",
            None,
            "KickMember",
            &[PyRep::Integer(boss_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(boss_kick_rpc.unwrap_err().code, "CannotKickBoss");

    let wing_claims = make_claims(wingman_id);
    let boss_kick_esi = dispatch_fleet_routes(
        &ctx,
        "DELETE",
        "/fleets/{fleet_id}/members/{member_id}",
        &params(&[
            ("fleet_id", &fleet_id.to_string()),
            ("member_id", &boss_id.to_string()),
        ]),
        &empty,
        Some(&wing_claims),
        &[],
    )
    .unwrap();
    assert_eq!(boss_kick_esi.status_code, 403);

    // Kicking wingman via ESI clears fleet state and domain lookup matches across protocols.
    let kick_res = dispatch_fleet_routes(
        &ctx,
        "DELETE",
        "/fleets/{fleet_id}/members/{member_id}",
        &params(&[
            ("fleet_id", &fleet_id.to_string()),
            ("member_id", &wingman_id.to_string()),
        ]),
        &empty,
        Some(&boss_claims),
        &[],
    )
    .unwrap();
    assert_eq!(kick_res.status_code, 204);
    assert!(fleets::get_character_fleet(&ctx, wingman_id).is_err());
}

#[tokio::test]
async fn parity_mail_cspa_and_corp_mail_role_enforcement() {
    let ctx = test_ctx();
    let corp_id = 98_099_100;
    let ceo_id = 91_000_001;
    let officer_id = 91_000_002;
    let stranger_id = 91_000_003;

    ctx.db.with_mut_snapshot(|snap| {
        let mut tpl = snap.characters[&90_000_001].clone();
        tpl.balance = IskCents::from_isk(1_000_000);
        for (id, cid) in [(ceo_id, corp_id), (officer_id, corp_id), (stranger_id, 1_000_009)] {
            let mut ch = tpl.clone();
            ch.character_id = id;
            ch.corporation_id = cid;
            snap.characters.insert(id, ch);
        }
        let mut corp = snap.corporations.values().next().unwrap().clone();
        corp.corporation_id = corp_id;
        corp.name = "Parity Corp".into();
        corp.ticker = "PRTY".into();
        corp.ceo_id = ceo_id;
        corp.alliance_id = None;
        corp.members = HashSet::from([ceo_id, officer_id]);
        corp.member_roles.clear();
        snap.corporations.insert(corp_id, corp);

        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .restrict_corp_mails = true;
        // Grant officer Communication Officer via title only (not raw corp.member_roles), verifying both
        // MachoNet `SendMail` and ESI `POST /mail` honor unified `effective_corp_role_breakdown`.
        let gov = snap.corp_governance.get_mut(&corp_id).unwrap();
        gov.titles
            .insert(1, ("Comms".into(), corp_roles::ROLE_CHAT_MANAGER));
        gov.member_titles.insert(officer_id, 1);
        snap.character_cspa_charges
            .insert(stranger_id, IskCents::from_isk(5_000));
    });

    // CSPA parity across character_cspa_charges via domain + ESI `/cspa`.
    let empty = HashMap::new();
    let cspa_esi = dispatch_char_social_route(
        &ctx,
        "POST",
        "/characters/{character_id}/cspa",
        &params(&[("character_id", &ceo_id.to_string())]),
        &empty,
        &serde_json::to_vec(&json!([stranger_id])).unwrap(),
    )
    .unwrap();
    assert_eq!(cspa_esi.status_code, 201);
    assert_eq!(cspa_esi.body.unwrap(), json!(5_000.0));

    let fee = mail::calculate_cspa_fee(
        &ctx,
        &mail::CspaCalculationInput {
            caller_id: ceo_id,
            target_ids: &[stranger_id],
            default_charge: IskCents(0),
            exempt_on_same_alliance: false,
            exempt_min_standing: -10.0,
        },
    )
    .unwrap();
    assert_eq!(fee, IskCents::from_isk(5_000));

    // Officer authorized via title-derived ROLE_CHAT_MANAGER succeeds on both ESI & MachoNet.
    let mail_body = serde_json::to_vec(&json!({
        "approved_cost": 0,
        "subject": "ESI Corp Mail",
        "body": "Body",
        "recipients": [{"recipient_id": corp_id, "recipient_type": "corporation"}],
    }))
    .unwrap();
    let officer_claims = make_claims(officer_id);
    let esi_mail_res = dispatch_character_mail_routes(
        &ctx,
        "POST",
        "/characters/{character_id}/mail",
        &params(&[("character_id", &officer_id.to_string())]),
        &empty,
        Some(&officer_claims),
        1,
        &mail_body,
    )
    .unwrap();
    assert_eq!(esi_mail_res.status_code, 201);

    let mut officer_sess = make_session(officer_id, corp_id);
    let rpc_mail_id = ctx
        .dispatch_rpc(
            &mut officer_sess,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::List(vec![]),
                PyRep::None,
                PyRep::Integer(corp_id),
                PyRep::String("RPC Corp Mail".into()),
                PyRep::String("Body".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Title-authorized officer must succeed via MachoNet SendMail");
    assert!(rpc_mail_id.as_i64().unwrap() > 0);
}

#[test]
fn parity_market_structure_acl_and_contract_region() {
    let ctx = test_ctx();
    let owner_corp = 98_099_200;
    let owner_char = 92_000_001;
    let outsider_char = 92_000_002;
    let structure_id = 1_000_999_200_001;

    ctx.db.with_mut_snapshot(|snap| {
        let mut tpl = snap.characters[&90_000_001].clone();
        tpl.character_id = owner_char;
        tpl.corporation_id = owner_corp;
        snap.characters.insert(owner_char, tpl.clone());

        tpl.character_id = outsider_char;
        tpl.corporation_id = 98_099_201;
        tpl.standings.insert(owner_corp, -5.0);
        snap.characters.insert(outsider_char, tpl);

        snap.structures.insert(
            structure_id,
            UpwellStructureRecord {
                structure_id,
                name: "Parity Fortizar".into(),
                type_id: 35_833,
                owner_corp_id: owner_corp,
                solar_system_id: 30_000_142,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                hull_hp: 1.0,
                armor_hp: 1.0,
                shield_hp: 1.0,
                reinforced_until_unix: 0,
                fuel_blocks: 100,
                quantum_core_installed: true,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        let mut hub = snap.items.values().next().unwrap().clone();
        hub.item_id = 8_888_999_001;
        hub.type_id = 35_892;
        hub.owner_id = owner_corp;
        hub.location_id = structure_id;
        hub.flag = InventoryFlag::ServiceSlot0;
        hub.singleton = true;
        hub.quantity = 1;
        hub.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(hub.item_id, hub);
        snap.structure_docking_acl_denied
            .entry(structure_id)
            .or_default()
            .insert(outsider_char);
        snap.market_orders.insert(
            9_999_200,
            MarketOrderRecord {
                order_id: 9_999_200,
                type_id: 34,
                station_id: structure_id,
                region_id: 10_000_002,
                owner_id: owner_char,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                escrow: IskCents(0),
                is_npc: false,
            },
        );
        snap.market_order_meta.insert(
            9_999_200,
            MarketOrderMeta {
                issued_unix: 1_700_000_000,
                duration_days: 90,
                range: "station".into(),
                issued_by: Some(owner_char),
                wallet_division: Some(1000),
                is_corporation: false,
                state: None,
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        // Quasar market visibility + direct verify
        assert!(market_acl::can_caller_view_market_location(
            snap,
            owner_char,
            structure_id
        ));
        assert!(!market_acl::can_caller_view_market_location(
            snap,
            outsider_char,
            structure_id
        ));
        assert!(market_acl::verify_structure_market_query_access(
            Some(&ctx),
            snap,
            outsider_char,
            structure_id,
            true
        )
        .is_err());

        // Contract region resolution for Upwell structure in Jita (30000142 -> region 10000002)
        assert!(market_acl::contract_in_region(
            &ctx.sde,
            snap,
            structure_id,
            10_000_002
        ));
        assert!(!market_acl::contract_in_region(
            &ctx.sde,
            snap,
            structure_id,
            10_000_043
        ));
    });

    // ESI `/markets/structures/{structure_id}` parity
    let empty = HashMap::new();
    let outsider_claims = make_claims(outsider_char);
    let esi_denied = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/structures/{structure_id}",
        &params(&[("structure_id", &structure_id.to_string())]),
        &empty,
        Some(&outsider_claims),
        1,
    )
    .unwrap();
    assert_eq!(esi_denied.status_code, 403);

    let owner_claims = make_claims(owner_char);
    let esi_allowed = dispatch_economy_route(
        &ctx,
        "GET",
        "/markets/structures/{structure_id}",
        &params(&[("structure_id", &structure_id.to_string())]),
        &empty,
        Some(&owner_claims),
        1,
    )
    .unwrap();
    assert_eq!(esi_allowed.status_code, 200);
    assert_eq!(esi_allowed.body.unwrap().as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn parity_career_goal_idempotency_and_corp_governance() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let corp_id = 98_099_300_i64;

    // 1. Cross-protocol career milestone idempotency between Quasar and MachoNet `seasonManager`.
    let milestones = default_career_milestones();
    let explorer_ms = milestones
        .iter()
        .find(|m| m.career_id == Some(1))
        .expect("explorer milestone");
    let goals = ctx
        .quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(char_id as u32);
    for g in goals.iter().filter(|g| g.def.career_id == 1) {
        let _ = ctx.quasar_gateway.progression.career_goals.add_progress(
            char_id as u32,
            &g.uuid,
            g.def.target,
            1_700_000_000,
        );
    }
    let cp = ctx
        .quasar_gateway
        .progression
        .career_goals
        .career_points_for_character(char_id as u32, explorer_ms.career_id);
    assert!(
        cp >= explorer_ms.required_points,
        "Explorer CP {cp} must reach milestone threshold {}",
        explorer_ms.required_points
    );

    let claimed_quasar = ctx
        .quasar_gateway
        .progression
        .career_goals
        .claim_milestone_reward(char_id as u32, explorer_ms.milestone_id, &ctx.db);
    assert!(claimed_quasar.is_ok(), "first claim via Quasar succeeds");

    let mut sess = make_session(char_id, corp_id);
    let dup_rpc = ctx
        .dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "claim_goal_reward",
            &[PyRep::Integer(i64::from(explorer_ms.milestone_id))],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(
        dup_rpc.unwrap_err().code,
        "GoalAlreadyClaimed",
        "MachoNet must reject second claim of Quasar-claimed milestone"
    );

    // 2. Unified corp governance: pending 24h role maturation + member limit parity.
    let now = 2_000_000_000_u64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = snap.corporations.values().next().unwrap().clone();
        corp.corporation_id = corp_id;
        corp.name = "Gov Parity Corp".into();
        corp.ticker = "GOVP".into();
        corp.ceo_id = char_id;
        corp.alliance_id = None;
        corp.members = HashSet::from([char_id, 90_000_002]);
        corp.member_roles.clear();
        snap.corporations.insert(corp_id, corp);
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .pending_role_grants
            .insert(90_000_002, (corp_roles::ROLE_DIRECTOR, now + 86_400));
        snap.corp_role_assignments.insert(
            (corp_id, 90_000_002),
            CorpMemberRoleAssignment {
                roles: corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_DIRECTOR,
                ..Default::default()
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let (_, before_roles) =
            corp_governance::effective_corp_member_roles(snap, corp_id, 90_000_002, 0, now);
        assert_eq!(before_roles & corp_roles::ROLE_DIRECTOR, 0);
        assert_ne!(before_roles & corp_roles::ROLE_ACCOUNTANT, 0);

        let after_eff = corp_governance::effective_corp_role_breakdown(
            snap,
            corp_id,
            90_000_002,
            now + 86_400,
        );
        assert_ne!(after_eff.roles & corp_roles::ROLE_DIRECTOR, 0);

        let domain_limit =
            corp_governance::resolve_corp_member_limit(Some(&ctx.sde), snap, corp_id).unwrap();
        let empty = HashMap::new();
        let esi_limit_res = dispatch_corp_governance_route(
            &ctx,
            "GET",
            "/corporations/{corporation_id}/members/limit",
            &params(&[("corporation_id", &corp_id.to_string())]),
            &empty,
        )
        .unwrap();
        assert_eq!(esi_limit_res.status_code, 200);
        assert_eq!(esi_limit_res.body.unwrap(), json!(domain_limit));
    });
}

#[test]
fn domain_market_acl_visibility_contracts_and_region_resolution() {
    let ctx = test_ctx();
    let owner_corp = 98_000_001_i64;
    let ally_corp = 98_000_701_i64;
    let enemy_corp = 98_000_702_i64;
    let neutral_corp = 98_000_703_i64;
    let shared_alliance = 99_000_701_i64;

    let same_corp_char = 90_000_001_i64;
    let ally_char = 90_700_001_i64;
    let both_none_char = 90_700_002_i64;
    let war_char = 90_700_003_i64;
    let neutral_char = 90_700_004_i64;
    let neg_standing_char = 90_700_005_i64;

    let online_st_id = 1_055_700_001_i64;
    let destroyed_st_id = 1_055_700_002_i64;
    let offline_st_id = 1_055_700_003_i64;
    let no_alliance_corp = 98_000_704_i64;
    let no_alliance_st_id = 1_055_700_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&owner_corp) {
            c.alliance_id = Some(shared_alliance);
        }
        let mut corp_tpl = snap.corporations[&owner_corp].clone();
        corp_tpl.corporation_id = ally_corp;
        corp_tpl.alliance_id = Some(shared_alliance);
        snap.corporations.insert(ally_corp, corp_tpl.clone());

        corp_tpl.corporation_id = enemy_corp;
        corp_tpl.alliance_id = None;
        snap.corporations.insert(enemy_corp, corp_tpl.clone());

        corp_tpl.corporation_id = neutral_corp;
        snap.corporations.insert(neutral_corp, corp_tpl.clone());

        corp_tpl.corporation_id = no_alliance_corp;
        snap.corporations.insert(no_alliance_corp, corp_tpl);

        let mut ch_tpl = snap.characters[&same_corp_char].clone();
        ch_tpl.character_id = ally_char;
        ch_tpl.corporation_id = ally_corp;
        ch_tpl.alliance_id = Some(shared_alliance);
        snap.characters.insert(ally_char, ch_tpl.clone());

        ch_tpl.character_id = both_none_char;
        ch_tpl.corporation_id = neutral_corp;
        ch_tpl.alliance_id = None;
        snap.characters.insert(both_none_char, ch_tpl.clone());

        ch_tpl.character_id = war_char;
        ch_tpl.corporation_id = enemy_corp;
        ch_tpl.alliance_id = None;
        snap.characters.insert(war_char, ch_tpl.clone());

        ch_tpl.character_id = neutral_char;
        ch_tpl.corporation_id = neutral_corp;
        ch_tpl.alliance_id = None;
        snap.characters.insert(neutral_char, ch_tpl.clone());

        ch_tpl.character_id = neg_standing_char;
        ch_tpl.corporation_id = neutral_corp;
        ch_tpl.standings.insert(owner_corp, -0.1);
        snap.characters.insert(neg_standing_char, ch_tpl);

        let mk_st = |id: i64, owner: i64, state: &str| UpwellStructureRecord {
            structure_id: id,
            type_id: 35_834, // Keepstar
            owner_corp_id: owner,
            solar_system_id: 30_000_142,
            name: format!("ACL St {id}"),
            position: [15_000_000_000.0, 0.0, 0.0],
            state: state.into(),
            quantum_core_installed: true,
            fuel_blocks: 500,
            shield_hp: 100_000.0,
            armor_hp: 100_000.0,
            hull_hp: 100_000.0,
            reinforced_until_unix: 0,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        };
        let online_st = mk_st(online_st_id, owner_corp, "ShieldVulnerable");
        let destroyed_st = mk_st(destroyed_st_id, owner_corp, "Destroyed");
        let mut offline_st = mk_st(offline_st_id, owner_corp, "ShieldVulnerable");
        offline_st.fuel_blocks = 0;
        let no_alliance_st = mk_st(no_alliance_st_id, no_alliance_corp, "ShieldVulnerable");
        snap.structures.insert(online_st_id, online_st);
        snap.structures.insert(destroyed_st_id, destroyed_st);
        snap.structures.insert(offline_st_id, offline_st);
        snap.structures.insert(no_alliance_st_id, no_alliance_st);

        // Online Standup Market Hub (35892) + Manufacturing Plant (35878) in online_st; also in destroyed_st.
        let mut srv_item = snap.items.values().next().unwrap().clone();
        srv_item.item_id = 100_777_001;
        srv_item.type_id = 35_892;
        srv_item.location_id = online_st_id;
        srv_item.flag = InventoryFlag::ServiceSlot0;
        srv_item.quantity = 1;
        snap.items.insert(srv_item.item_id, srv_item.clone());

        srv_item.item_id = 100_777_002;
        srv_item.type_id = 35_878;
        srv_item.flag = InventoryFlag::ServiceSlot1;
        snap.items.insert(srv_item.item_id, srv_item.clone());

        srv_item.item_id = 100_777_003;
        srv_item.location_id = destroyed_st_id;
        snap.items.insert(srv_item.item_id, srv_item);

        snap.wars.insert(
            77_001,
            eve_db::WarRecord {
                war_id: 77_001,
                aggressor_corp_id: enemy_corp,
                defender_corp_id: owner_corp,
                mutual: false,
                started_unix: 100,
                active: true,
            },
        );
        // Active reverse war + inactive war on another war_id
        snap.wars.insert(
            77_002,
            eve_db::WarRecord {
                war_id: 77_002,
                aggressor_corp_id: owner_corp,
                defender_corp_id: neutral_corp,
                mutual: false,
                started_unix: 100,
                active: false,
            },
        );
    });

    ctx.db.with_snapshot(|snap| {
        let st = &snap.structures[&online_st_id];
        let destroyed = &snap.structures[&destroyed_st_id];
        let no_alliance_st = &snap.structures[&no_alliance_st_id];
        assert!(!market_acl::character_has_structure_acl_access(snap, same_corp_char, destroyed));
        assert!(!market_acl::character_has_structure_acl_access(snap, 999_999_999, st));
        assert!(market_acl::character_has_structure_acl_access(snap, same_corp_char, st));
        assert!(market_acl::character_has_structure_acl_access(snap, ally_char, st));
        assert!(!market_acl::character_has_structure_acl_access(snap, neg_standing_char, st));
        assert!(!market_acl::character_has_structure_acl_access(snap, war_char, st));
        // Neutral char has 0.0 standing and inactive war -> allowed; both alliance_id == None still not short-circuited by alliance.
        assert!(market_acl::character_has_structure_acl_access(
            snap,
            both_none_char,
            no_alliance_st
        ));
        assert!(!market_acl::can_caller_view_structure_acl(
            snap,
            same_corp_char,
            destroyed_st_id,
            None
        ));

        assert!(market_acl::verify_structure_market_query_access(
            Some(&ctx),
            snap,
            same_corp_char,
            60_003_760,
            false
        )
        .is_ok());
        assert!(market_acl::verify_structure_market_query_access(
            Some(&ctx),
            snap,
            same_corp_char,
            60_003_760,
            true
        )
        .is_err());
        assert!(market_acl::verify_structure_market_query_access(
            Some(&ctx),
            snap,
            same_corp_char,
            offline_st_id,
            true
        )
        .is_err());
        assert!(market_acl::verify_structure_market_query_access(
            Some(&ctx),
            snap,
            war_char,
            online_st_id,
            true
        )
        .is_err());

        assert_eq!(market_acl::primary_online_industry_activity(snap, st), Some(1));
        assert_eq!(market_acl::primary_online_industry_activity(snap, destroyed), None);
        assert_eq!(
            market_acl::primary_online_industry_activity(snap, &snap.structures[&offline_st_id]),
            None
        );

        let npc_station = ctx
            .sde
            .stations
            .values()
            .next()
            .expect("NPC station in SDE");
        assert!(market_acl::contract_in_region(
            &ctx.sde,
            snap,
            npc_station.station_id,
            npc_station.region_id
        ));
        assert!(!market_acl::contract_in_region(
            &ctx.sde,
            snap,
            npc_station.station_id,
            npc_station.region_id + 1
        ));
        let st_reg = ctx.sde.solar_systems[&30_000_142].region_id;
        assert!(market_acl::contract_in_region(&ctx.sde, snap, online_st_id, st_reg));
        assert!(!market_acl::contract_in_region(&ctx.sde, snap, 888_777_666, st_reg));

        let mut c = eve_db::ContractRecord {
            contract_id: 91_001,
            issuer_id: same_corp_char,
            acceptor_id: None,
            contract_type: "ItemExchange".into(),
            status: "Outstanding".into(),
            start_station_id: 60_003_760,
            end_station_id: 60_003_760,
            price: IskCents(0),
            reward: IskCents(0),
            collateral: IskCents(0),
            item_ids: Vec::new(),
            expiration_unix: 2_000_000_000,
            issuer_corp_id: owner_corp,
            assignee_id: 0,
            availability: "Private".into(),
            buyout: IskCents(0),
            current_bid: IskCents(0),
            current_bidder_id: Some(neutral_char),
            requested_items: Vec::new(),
            bid_history: vec![(war_char, IskCents(100), 1_700_000_000)],
            date_issued_unix: 1_700_000_000,
            title: "Private".into(),
            for_corp: true,
            num_days: 1,
        };
        assert!(market_acl::is_contract_visible_to_character(&c, same_corp_char, ally_corp, None));
        assert!(!market_acl::is_contract_visible_to_character(&c, neutral_char, neutral_corp, None));
        c.availability = "Corporation".into();
        assert!(market_acl::is_contract_visible_to_character(&c, neutral_char, owner_corp, None));
        assert!(!market_acl::is_contract_visible_to_character(&c, neutral_char, neutral_corp, None));
        c.assignee_id = shared_alliance;
        assert!(market_acl::is_contract_visible_to_character(
            &c,
            ally_char,
            ally_corp,
            Some(shared_alliance)
        ));
        assert!(!market_acl::is_contract_visible_to_character(&c, neutral_char, neutral_corp, None));

        // for_corp: true means issuer_id alone is not treated as personal party unless another field matches.
        assert!(!market_acl::is_character_party_to_contract(snap, same_corp_char, &c));
        assert!(market_acl::is_character_party_to_contract(snap, neutral_char, &c));
        assert!(market_acl::is_character_party_to_contract(snap, war_char, &c));
        assert!(market_acl::is_character_party_to_contract(snap, ally_char, &c));
    });
}

#[test]
fn domain_corp_governance_career_goals_mail_cspa_and_fleets_branches() {
    use crate::domain::career_goals;
    use crate::services::quasar_career_goals::CareerGoalManager;
    use eve_db::{CorpGovState, TrainedSkillRecord};

    let ctx = test_ctx();
    let corp_id = 98_000_001_i64;
    let ceo_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    let outside_corp = 98_000_801_i64;
    let outside_char = 90_800_001_i64;
    let missing_corp = 98_999_999_i64;

    let skill_191_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.default_attributes.get(&191).copied().is_some_and(|v| v > 0.0))
        .expect("skill type with dogma attr 191")
        .type_id;
    let skill_191_per_lvl = ctx.sde.types[&skill_191_id].default_attributes[&191].round() as i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = snap.corporations[&corp_id].clone();
        corp.ceo_id = ceo_id;
        corp.members.insert(member_id);
        corp.member_roles.clear();
        snap.corporations.insert(corp_id, corp.clone());

        corp.corporation_id = outside_corp;
        corp.ceo_id = outside_char;
        snap.corporations.insert(outside_corp, corp);

        let mut ch = snap.characters[&ceo_id].clone();
        ch.character_id = outside_char;
        ch.corporation_id = outside_corp;
        ch.alliance_id = None;
        ch.balance = IskCents::from_isk(5_000);
        snap.characters.insert(outside_char, ch);

        if let Some(c) = snap.characters.get_mut(&member_id) {
            c.corporation_id = corp_id;
        }
        snap.character_cspa_charges
            .insert(outside_char, IskCents::from_isk(1_500));

        // Grant member pending title role maturing at t=2000 and immediate HQ diplomat role via assignment
        let mut gov = CorpGovState::default();
        gov.restrict_corp_mails = true;
        gov.titles
            .insert(1, ("Officer".into(), corp_roles::ROLE_DIRECTOR));
        gov.member_titles.insert(member_id, 1);
        gov.pending_title_role_grants
            .insert(member_id, (corp_roles::ROLE_DIRECTOR, 2_000));
        snap.corp_governance.insert(corp_id, gov);

        snap.corp_role_assignments.insert(
            (corp_id, member_id),
            CorpMemberRoleAssignment {
                character_id: member_id,
                roles_at_hq: corp_roles::ROLE_DIPLOMAT,
                grantable_roles_at_base: corp_roles::ROLE_ACCOUNTANT,
                ..Default::default()
            },
        );

        snap.corp_member_limits.remove(&corp_id);
        snap.skills.insert(
            ceo_id,
            HashMap::from([(
                skill_191_id,
                TrainedSkillRecord {
                    type_id: skill_191_id,
                    level: 3,
                    skill_points: 8_000.0,
                },
            )]),
        );
    });

    // 1. corp_governance: missing corp, pending title maturity, any_active_roles, can_send_corp_mail, member limits
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(
            corp_governance::effective_corp_role_breakdown(snap, missing_corp, member_id, 1_000),
            corp_governance::EffectiveCorpRoles::default()
        );
        assert_eq!(
            corp_governance::effective_corp_member_roles(snap, missing_corp, member_id, 0xAB, 1_000),
            (false, 0xAB)
        );
        let unmatured =
            corp_governance::effective_corp_role_breakdown(snap, corp_id, member_id, 1_500);
        assert_eq!(unmatured.roles & corp_roles::ROLE_DIRECTOR, 0);
        assert_ne!(unmatured.unmatured_mask & corp_roles::ROLE_DIRECTOR, 0);
        assert_ne!(unmatured.any_active_roles() & corp_roles::ROLE_DIPLOMAT, 0);
        assert_ne!(unmatured.any_grantable_roles() & corp_roles::ROLE_ACCOUNTANT, 0);

        let matured =
            corp_governance::effective_corp_role_breakdown(snap, corp_id, member_id, 2_000);
        assert_ne!(matured.roles & corp_roles::ROLE_DIRECTOR, 0);
        assert_eq!(matured.unmatured_mask, 0);

        assert!(corp_governance::can_send_corp_mail(snap, corp_id, member_id, 1_500));
        // Clear assignments & titles; restricted rejects, unrestricted allows
        snap.corp_role_assignments.clear();
        snap.corp_governance
            .get_mut(&corp_id)
            .unwrap()
            .member_titles
            .clear();
        assert!(!corp_governance::can_send_corp_mail(snap, corp_id, member_id, 1_500));
        assert!(corp_governance::can_send_corp_mail(snap, corp_id, ceo_id, 1_500));
        snap.corp_governance.get_mut(&corp_id).unwrap().restrict_corp_mails = false;
        assert!(corp_governance::can_send_corp_mail(snap, corp_id, member_id, 1_500));

        assert_eq!(corp_governance::resolve_corp_member_limit(None, snap, missing_corp), None);
        assert_eq!(corp_governance::resolve_corp_member_limit(None, snap, corp_id), Some(10));
        assert_eq!(
            corp_governance::resolve_corp_member_limit(Some(&ctx.sde), snap, corp_id),
            Some((skill_191_per_lvl * 3).max(10))
        );
        snap.corp_governance.get_mut(&corp_id).unwrap().member_limit = Some(42);
        assert_eq!(corp_governance::resolve_corp_member_limit(Some(&ctx.sde), snap, corp_id), Some(42));
        snap.corp_member_limits.insert(corp_id, 77);
        assert_eq!(corp_governance::resolve_corp_member_limit(Some(&ctx.sde), snap, corp_id), Some(77));

        let mask = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT;
        let names: Vec<String> = corp_governance::decode_corp_roles(mask)
            .into_iter()
            .map(str::to_lowercase)
            .collect();
        assert_eq!(corp_governance::encode_esi_corp_roles(&names), mask);
    });

    // 2. career_goals: credit, overflow, missing char, milestone + SDE fallback, claim incomplete & double claim
    let milestones = default_career_milestones();
    let mgr = CareerGoalManager::new(Some(Arc::clone(&ctx.db)));
    let sde_goal_id = ctx.sde.career_goals.keys().copied().next().unwrap_or(999_999);
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(
            career_goals::credit_character_reward_in_snap(snap, 999_888_777, IskCents(1), 0.0, 0, None)
                .expect_err("missing character")
                .code,
            "CharacterNotFound"
        );
        let sp_before = snap.characters[&member_id].free_skill_points;
        let (bal, sp) = career_goals::credit_character_reward_in_snap(
            snap,
            member_id,
            IskCents::from_isk(250),
            500.0,
            75,
            Some(("mission_reward", "Career Bonus".into(), Some((10, ceo_id, 1_700_000_123)))),
        )
        .expect("credit reward");
        assert!(bal.0 >= IskCents::from_isk(250).0);
        assert!((sp - (sp_before + 500.0)).abs() < 1e-6);
        assert_eq!(snap.characters[&member_id].loyalty_points[&1_000_419], 75);
        assert_eq!(snap.corp_lp_balances[&(corp_id, 1_000_419)], 75);

        snap.characters.get_mut(&member_id).unwrap().balance = IskCents(i64::MAX);
        assert_eq!(
            career_goals::credit_character_reward_in_snap(snap, member_id, IskCents(1), 0.0, 0, None)
                .expect_err("overflow")
                .code,
            "BalanceOverflow"
        );

        let m_id = milestones[0].milestone_id;
        assert!(career_goals::resolve_career_goal_or_milestone(&ctx.sde, &mgr, &milestones, ceo_id, m_id)
            .unwrap()
            .career_points_progress
            .is_some());
        if ctx.sde.career_goals.contains_key(&sde_goal_id) && !milestones.iter().any(|m| m.milestone_id == sde_goal_id) {
            assert!(career_goals::resolve_career_goal_or_milestone(&ctx.sde, &mgr, &milestones, ceo_id, sde_goal_id)
                .unwrap()
                .career_points_progress
                .is_none());
        }
        assert!(career_goals::resolve_career_goal_or_milestone(&ctx.sde, &mgr, &milestones, ceo_id, 999_888_777).is_none());

        assert_eq!(
            career_goals::claim_career_goal_or_milestone_in_snap(snap, &mgr, ceo_id, m_id, 5, 10, 100.0)
                .expect_err("incomplete")
                .code,
            "GoalProgressIncomplete"
        );
        assert_eq!(
            career_goals::claim_career_goal_or_milestone_in_snap(snap, &mgr, 999_888_777, m_id, 10, 10, 100.0)
                .expect_err("missing char")
                .code,
            "CharacterNotFound"
        );
        let new_sp = career_goals::claim_career_goal_or_milestone_in_snap(
            snap, &mgr, ceo_id, m_id, 10, 10, 100.0,
        )
        .expect("claim ok");
        assert!(new_sp >= 100.0);
        assert_eq!(
            career_goals::claim_career_goal_or_milestone_in_snap(snap, &mgr, ceo_id, m_id, 10, 10, 100.0)
                .expect_err("double claim")
                .code,
            "GoalAlreadyClaimed"
        );
    });

    // 3. mail::charge_cspa_action: debit & insufficient funds guard
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&ceo_id).unwrap().balance = IskCents::from_isk(2_000);
    });
    let paid = mail::charge_cspa_action(
        &ctx,
        &mail::CspaCalculationInput {
            caller_id: ceo_id,
            target_ids: &[outside_char],
            default_charge: IskCents(0),
            exempt_on_same_alliance: false,
            exempt_min_standing: -10.0,
        },
    )
    .expect("cspa debit succeeds");
    assert_eq!(paid, IskCents::from_isk(1_500));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&ceo_id].balance),
        IskCents::from_isk(500)
    );
    let insuf = mail::charge_cspa_action(
        &ctx,
        &mail::CspaCalculationInput {
            caller_id: ceo_id,
            target_ids: &[outside_char],
            default_charge: IskCents(0),
            exempt_on_same_alliance: false,
            exempt_min_standing: -10.0,
        },
    )
    .expect_err("second debit must fail InsufficientFundsForCSPA");
    assert_eq!(insuf.code, "InsufficientFundsForCSPA");
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&ceo_id].balance),
        IskCents::from_isk(500)
    );

    // 4. fleets helpers
    assert_eq!(fleets::role_to_i64(eve_db::FleetRole::WingCommander), 2);
    assert_eq!(fleets::i64_to_role(3), eve_db::FleetRole::SquadCommander);
    assert_eq!(fleets::role_name_str(eve_db::FleetRole::FleetCommander), "Fleet Commander");
}

#[test]
fn domain_career_goals_reward_resolve_and_claim_boundaries() {
    use crate::domain::career_goals::{
        claim_career_goal_or_milestone_in_snap, credit_character_reward_in_snap,
        resolve_career_goal_or_milestone,
    };

    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let zero_corp_char = 92_100_001_i64;
    let corp_id = 98_099_400_i64;
    let milestones = default_career_milestones();
    let ms = milestones.first().expect("at least one career milestone").clone();
    let mgr = &ctx.quasar_gateway.progression.career_goals;

    ctx.db.with_mut_snapshot(|snap| {
        let mut tpl = snap.characters[&char_id].clone();
        tpl.character_id = zero_corp_char;
        tpl.corporation_id = 0;
        tpl.balance = IskCents(100);
        tpl.free_skill_points = 10.0;
        tpl.loyalty_points.clear();
        snap.characters.insert(zero_corp_char, tpl);

        let err_missing = credit_character_reward_in_snap(snap, 999_888_777, IskCents(10), 0.0, 0, None)
            .unwrap_err();
        assert_eq!(err_missing.code, "CharacterNotFound");

        snap.characters.get_mut(&zero_corp_char).unwrap().balance = IskCents(i64::MAX);
        let err_ovf = credit_character_reward_in_snap(snap, zero_corp_char, IskCents(1), 0.0, 0, None)
            .unwrap_err();
        assert_eq!(err_ovf.code, "BalanceOverflow");
        snap.characters.get_mut(&zero_corp_char).unwrap().balance = IskCents(100);

        // sp_reward == 0.0 must not change SP; evermarks > 0 with corporation_id == 0 only credits character LP; None appends no journal
        let j_before = snap.wallet_journal.len();
        let (bal1, sp1) = credit_character_reward_in_snap(snap, zero_corp_char, IskCents(50), 0.0, 25, None)
            .unwrap();
        assert_eq!(bal1, IskCents(150));
        assert_eq!(sp1, 10.0);
        assert_eq!(snap.characters[&zero_corp_char].loyalty_points.get(&1_000_419).copied(), Some(25));
        assert!(snap.corp_lp_balances.get(&(0, 1_000_419)).is_none());
        assert_eq!(snap.wallet_journal.len(), j_before);

        // journal_meta with ledger_extra = None uses append_journal, and corporation_id > 0 credits corp_lp_balances
        snap.characters.get_mut(&zero_corp_char).unwrap().corporation_id = corp_id;
        let (bal2, sp2) = credit_character_reward_in_snap(
            snap,
            zero_corp_char,
            IskCents(200),
            500.0,
            15,
            Some(("CareerReward", "milestone".into(), None)),
        )
        .unwrap();
        assert_eq!(bal2, IskCents(350));
        assert_eq!(sp2, 510.0);
        assert_eq!(snap.corp_lp_balances.get(&(corp_id, 1_000_419)).copied(), Some(15));
        assert_eq!(snap.wallet_journal.len(), j_before + 1);
        assert_eq!(snap.wallet_journal.last().unwrap().reason, "milestone");

        // journal_meta with Some((ref_type, first_party, date_unix)) populates full meta
        let _ = credit_character_reward_in_snap(
            snap,
            zero_corp_char,
            IskCents(300),
            0.0,
            0,
            Some(("AgentMissionReward", "goal".into(), Some((85, 10_001, 1_700_000_123)))),
        )
        .unwrap();
        let entry_id = snap.wallet_journal.last().unwrap().entry_id;
        let meta = &snap.wallet_journal_meta[&entry_id];
        assert_eq!(meta.ref_type_id, 85);
        assert_eq!(meta.first_party_id, 10_001);
        assert_eq!(meta.second_party_id, zero_corp_char);
        assert_eq!(snap.wallet_journal_timestamps[&entry_id], 1_700_000_123);

        // resolve_career_goal_or_milestone: milestone arm vs SDE fallback vs None
        let resolved_ms = resolve_career_goal_or_milestone(&ctx.sde, mgr, &milestones, zero_corp_char, ms.milestone_id)
            .expect("milestone resolves");
        assert!(resolved_ms.career_points_progress.is_some());
        assert!(resolved_ms.required_progress >= 1);

        if let Some((&sde_gid, g_tmpl)) = ctx.sde.career_goals.iter().find(|(&gid, _)| !milestones.iter().any(|m| m.milestone_id == gid)) {
            let resolved_sde = resolve_career_goal_or_milestone(&ctx.sde, mgr, &milestones, zero_corp_char, sde_gid)
                .expect("sde goal resolves");
            assert_eq!(resolved_sde.career_points_progress, None);
            assert_eq!(resolved_sde.required_progress, g_tmpl.target_progress.max(1));
        }
        assert!(resolve_career_goal_or_milestone(&ctx.sde, mgr, &milestones, zero_corp_char, u32::MAX).is_none());

        // claim_career_goal_or_milestone_in_snap: < boundary, missing char, == boundary, duplicate guards
        let incomplete = claim_career_goal_or_milestone_in_snap(snap, mgr, zero_corp_char, ms.milestone_id, 9, 10, 100.0)
            .unwrap_err();
        assert_eq!(incomplete.code, "GoalProgressIncomplete");

        let no_char = claim_career_goal_or_milestone_in_snap(snap, mgr, 999_888_777, ms.milestone_id, 10, 10, 100.0)
            .unwrap_err();
        assert_eq!(no_char.code, "CharacterNotFound");

        let new_sp = claim_career_goal_or_milestone_in_snap(snap, mgr, zero_corp_char, ms.milestone_id, 10, 10, 250.0)
            .unwrap();
        assert!((new_sp - 760.0).abs() < 1e-6);

        let dup = claim_career_goal_or_milestone_in_snap(snap, mgr, zero_corp_char, ms.milestone_id, 10, 10, 250.0)
            .unwrap_err();
        assert_eq!(dup.code, "GoalAlreadyClaimed");

        // Non-milestone NPE goal duplicate guard
        let npe_gid = 99_999_u32;
        assert!(claim_career_goal_or_milestone_in_snap(snap, mgr, zero_corp_char, npe_gid, 5, 5, 50.0).is_ok());
        assert_eq!(
            claim_career_goal_or_milestone_in_snap(snap, mgr, zero_corp_char, npe_gid, 5, 5, 50.0)
                .unwrap_err()
                .code,
            "GoalAlreadyClaimed"
        );
    });
}

#[test]
fn domain_market_acl_character_structure_contract_and_primary_activity_boundaries() {
    use eve_db::{ContractRecord, WarRecord};

    let ctx = test_ctx();
    let owner_corp = 98_099_500_i64;
    let ally_corp = 98_099_501_i64;
    let neutral_corp = 98_099_502_i64;
    let war_corp = 98_099_503_i64;
    let alliance_id = 99_099_500_i64;

    let owner_char = 92_200_001_i64;
    let ally_char = 92_200_002_i64;
    let neg_standing_char = 92_200_003_i64;
    let neutral_zero_char = 92_200_004_i64;
    let war_char = 92_200_005_i64;
    let st_id = 1_000_999_500_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut corp_tpl = snap.corporations.values().next().cloned().expect("seeded corp in demo snapshot");
        corp_tpl.corporation_id = owner_corp;
        corp_tpl.alliance_id = Some(alliance_id);
        snap.corporations.insert(owner_corp, corp_tpl.clone());
        corp_tpl.corporation_id = neutral_corp;
        corp_tpl.alliance_id = None;
        snap.corporations.insert(neutral_corp, corp_tpl);

        let mut ch_tpl = snap.characters[&90_000_001].clone();
        ch_tpl.standings.clear();

        ch_tpl.character_id = owner_char;
        ch_tpl.corporation_id = owner_corp;
        ch_tpl.alliance_id = Some(alliance_id);
        snap.characters.insert(owner_char, ch_tpl.clone());

        ch_tpl.character_id = ally_char;
        ch_tpl.corporation_id = ally_corp;
        ch_tpl.alliance_id = Some(alliance_id);
        snap.characters.insert(ally_char, ch_tpl.clone());

        ch_tpl.character_id = neg_standing_char;
        ch_tpl.corporation_id = neutral_corp;
        ch_tpl.alliance_id = None;
        ch_tpl.standings.insert(owner_corp, -0.01);
        snap.characters.insert(neg_standing_char, ch_tpl.clone());

        ch_tpl.character_id = neutral_zero_char;
        ch_tpl.standings.insert(owner_corp, 0.0);
        snap.characters.insert(neutral_zero_char, ch_tpl.clone());

        ch_tpl.character_id = war_char;
        ch_tpl.corporation_id = war_corp;
        ch_tpl.standings.clear();
        snap.characters.insert(war_char, ch_tpl);

        snap.wars.insert(
            9_900_501,
            WarRecord {
                war_id: 9_900_501,
                aggressor_corp_id: owner_corp,
                defender_corp_id: war_corp,
                mutual: false,
                started_unix: 1,
                active: true,
            },
        );
        snap.wars.insert(
            9_900_502,
            WarRecord {
                war_id: 9_900_502,
                aggressor_corp_id: neutral_corp,
                defender_corp_id: owner_corp,
                mutual: false,
                started_unix: 1,
                active: false, // inactive war must NOT block neutral_zero_char!
            },
        );

        let st = UpwellStructureRecord {
            structure_id: st_id,
            name: "ACL Astrahus".into(),
            type_id: 35_832,
            owner_corp_id: owner_corp,
            solar_system_id: 30_000_142,
            position: [0.0, 0.0, 0.0],
            state: "ShieldVulnerable".into(),
            hull_hp: 1.0,
            armor_hp: 1.0,
            shield_hp: 1.0,
            reinforced_until_unix: 0,
            fuel_blocks: 50,
            quantum_core_installed: true,
            last_damage_tick: 0,
            last_damage_unix: 0,
            self_destruct_at_unix: None,
        };
        snap.structures.insert(st_id, st.clone());

        assert!(market_acl::character_has_structure_acl_access(snap, owner_char, &st));
        assert!(market_acl::character_has_structure_acl_access(snap, ally_char, &st));
        assert!(!market_acl::character_has_structure_acl_access(snap, neg_standing_char, &st));
        assert!(market_acl::character_has_structure_acl_access(snap, neutral_zero_char, &st));
        assert!(!market_acl::character_has_structure_acl_access(snap, war_char, &st));
        assert!(!market_acl::character_has_structure_acl_access(snap, 999_777_666, &st));

        // Deny override blocks even owner_char
        snap.structure_docking_acl_denied.entry(st_id).or_default().insert(owner_char);
        assert!(!market_acl::character_has_structure_acl_access(snap, owner_char, &st));
        snap.structure_docking_acl_denied.remove(&st_id);

        // Destroyed structure blocks access and industry service discovery
        let mut destroyed = st.clone();
        destroyed.state = "Destroyed".into();
        assert!(!market_acl::character_has_structure_acl_access(snap, owner_char, &destroyed));
        assert_eq!(market_acl::primary_online_industry_activity(snap, &destroyed), None);

        // Add online Composite Reactor service module -> primary_online_industry_activity returns Some(9)
        let mut mod_item = snap.items.values().next().unwrap().clone();
        mod_item.item_id = 8_888_999_501;
        mod_item.type_id = 35_894;
        mod_item.owner_id = owner_corp;
        mod_item.location_id = st_id;
        mod_item.flag = InventoryFlag::ServiceSlot0;
        mod_item.singleton = true;
        mod_item.quantity = 1;
        mod_item.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(mod_item.item_id, mod_item);
        assert_eq!(market_acl::primary_online_industry_activity(snap, &st), Some(9));

        // Missing structure behavior for verify_structure_market_query_access + can_caller_view_structure_acl
        assert!(market_acl::can_caller_view_structure_acl(snap, owner_char, 60_003_760, None));
        assert!(market_acl::verify_structure_market_query_access(None, snap, owner_char, 60_003_760, false).is_ok());
        assert_eq!(
            market_acl::verify_structure_market_query_access(None, snap, owner_char, 999_111_222_333, true)
                .unwrap_err()
                .code,
            "StructureNotFound"
        );

        // Contract visibility, party membership, and region checks
        let mut c = ContractRecord {
            contract_id: 500_001,
            issuer_id: owner_char,
            issuer_corp_id: owner_corp,
            availability: "".into(),
            assignee_id: 0,
            start_station_id: 60_003_760,
            ..Default::default()
        };
        assert!(market_acl::is_public_available_contract(&c));
        c.availability = "Personal".into();
        assert!(!market_acl::is_public_available_contract(&c));
        assert!(!market_acl::is_contract_visible_to_character(&c, neutral_zero_char, neutral_corp, None));
        assert!(market_acl::is_contract_visible_to_character(&c, owner_char, owner_corp, None));

        c.availability = "Corporation".into();
        assert!(market_acl::is_contract_visible_to_character(&c, neutral_zero_char, owner_corp, None));
        assert!(!market_acl::is_contract_visible_to_character(&c, neutral_zero_char, neutral_corp, None));

        c.assignee_id = alliance_id;
        assert!(market_acl::is_contract_visible_to_character(&c, ally_char, ally_corp, Some(alliance_id)));
        assert!(!market_acl::is_contract_visible_to_character(&c, neutral_zero_char, neutral_corp, None));

        // is_character_party_to_contract: for_corp == true with Private availability excludes issuer_id unless acceptor/bidder/assignee
        c.for_corp = true;
        c.availability = "Private".into();
        c.assignee_id = 0;
        assert!(!market_acl::is_character_party_to_contract(snap, owner_char, &c));
        c.for_corp = false;
        assert!(market_acl::is_character_party_to_contract(snap, owner_char, &c));
        c.for_corp = true;
        c.bid_history.push((war_char, IskCents(100), 10));
        assert!(market_acl::is_character_party_to_contract(snap, war_char, &c));
        c.current_bidder_id = Some(neutral_zero_char);
        assert!(market_acl::is_character_party_to_contract(snap, neutral_zero_char, &c));

        // NPC station contract_in_region + unknown location
        assert!(market_acl::contract_in_region(&ctx.sde, snap, 60_003_760, 10_000_002));
        assert!(!market_acl::contract_in_region(&ctx.sde, snap, 60_003_760, 10_000_043));
        assert!(!market_acl::contract_in_region(&ctx.sde, snap, 999_888_777_666, 10_000_002));
    });
}

#[test]
fn fleet_corp_governance_and_mail_cspa_helper_boundaries_kill_mutants() {
    use eve_db::durable_domain_types::CorpGovState;
    use eve_db::{
        FleetMemberRecord, FleetRecord, FleetRole, FleetSquadRecord, FleetWingRecord,
        SocialContactRecord,
    };

    // 1. fleets.rs role conversion, wing/squad management, and hierarchy permission matrix
    for (role, id, name, esi_pair) in [
        (
            FleetRole::FleetCommander,
            1,
            "Fleet Commander",
            ("fleet_commander", "Fleet Commander"),
        ),
        (
            FleetRole::WingCommander,
            2,
            "Wing Commander",
            ("wing_commander", "Wing Commander"),
        ),
        (
            FleetRole::SquadCommander,
            3,
            "Squad Commander",
            ("squad_commander", "Squad Commander"),
        ),
        (
            FleetRole::SquadMember,
            4,
            "Squad Member",
            ("squad_member", "Squad Member"),
        ),
    ] {
        assert_eq!(fleets::role_to_i64(role), id);
        assert_eq!(fleets::i64_to_role(id), role);
        assert_eq!(fleets::role_name_str(role), name);
        assert_eq!(fleets::fleet_role_to_esi_str(role), esi_pair);
        assert_eq!(fleets::parse_esi_fleet_role(esi_pair.0), Some(role));
    }
    assert_eq!(fleets::i64_to_role(99), FleetRole::SquadMember);
    assert_eq!(fleets::parse_esi_fleet_role("unknown"), None);

    let (boss, fc, wc1, wc2, sc1, sm1, sm2) = (101_i64, 102, 103, 104, 105, 106, 107);
    let mut f = FleetRecord {
        fleet_id: 500,
        boss_id: boss,
        wings: vec![10, 20],
        squads: vec![100, 200],
        members: vec![boss, fc, wc1, wc2, sc1, sm1],
        ..Default::default()
    };
    f.wing_details.insert(
        20,
        FleetWingRecord {
            wing_id: 20,
            name: "W20".into(),
            commander_id: None,
            squad_ids: vec![200],
        },
    );
    f.squad_details.insert(
        100,
        FleetSquadRecord {
            squad_id: 100,
            wing_id: 10,
            name: "S100".into(),
            commander_id: None,
        },
    );
    for (cid, role, wid, sid) in [
        (fc, FleetRole::FleetCommander, None, None),
        (wc1, FleetRole::WingCommander, Some(10), None),
        (wc2, FleetRole::WingCommander, Some(20), None),
        (sc1, FleetRole::SquadCommander, Some(10), Some(100)),
        (sm1, FleetRole::SquadMember, Some(10), Some(100)),
        (sm2, FleetRole::SquadMember, Some(20), Some(200)),
    ] {
        f.member_details.insert(
            cid,
            FleetMemberRecord {
                character_id: cid,
                role,
                wing_id: wid,
                squad_id: sid,
                takes_fleet_warp: true,
                ..Default::default()
            },
        );
    }

    assert!(fleets::is_fleet_member(&f, boss));
    assert!(fleets::is_fleet_member(&f, sm2)); // via member_details
    assert!(!fleets::is_fleet_member(&f, 999));
    assert!(fleets::is_fleet_boss_or_fc(&f, boss));
    assert!(fleets::is_fleet_boss_or_fc(&f, fc));
    assert!(!fleets::is_fleet_boss_or_fc(&f, wc1));
    assert_eq!(fleets::squad_parent_wing_id(&f, 100), Some(10));
    assert_eq!(fleets::squad_parent_wing_id(&f, 200), Some(20)); // via wing_details fallback
    assert_eq!(fleets::squad_parent_wing_id(&f, 999), None);

    assert!(fleets::can_manage_wing(&f, boss, 10));
    assert!(fleets::can_manage_wing(&f, fc, 10));
    assert!(fleets::can_manage_wing(&f, wc1, 10));
    assert!(!fleets::can_manage_wing(&f, wc1, 20));
    assert!(!fleets::can_manage_wing(&f, sc1, 10));

    assert!(fleets::can_manage_squad(&f, boss, 100));
    assert!(fleets::can_manage_squad(&f, fc, 100));
    assert!(fleets::can_manage_squad(&f, wc1, 100));
    assert!(!fleets::can_manage_squad(&f, wc1, 200));
    assert!(fleets::can_manage_squad(&f, sc1, 100));
    assert!(!fleets::can_manage_squad(&f, sc1, 200));
    assert!(!fleets::can_manage_squad(&f, sm1, 100));

    assert_eq!(
        fleets::member_effective_position(&f, boss),
        (FleetRole::FleetCommander, None, None)
    );
    assert_eq!(
        fleets::member_effective_position(&f, 888),
        (FleetRole::SquadMember, Some(10), Some(100))
    );

    // can_act_on_member & can_invite_member hierarchy rules
    assert!(!fleets::can_act_on_member(&f, fc, boss, None));
    assert!(fleets::can_act_on_member(&f, boss, fc, Some((FleetRole::FleetCommander, None, None))));
    assert!(fleets::can_act_on_member(&f, fc, wc1, Some((FleetRole::WingCommander, Some(10), None))));
    assert!(!fleets::can_act_on_member(&f, fc, wc1, Some((FleetRole::FleetCommander, None, None))));
    assert!(fleets::can_act_on_member(&f, wc1, sc1, Some((FleetRole::SquadMember, Some(10), Some(100)))));
    assert!(!fleets::can_act_on_member(&f, wc1, sm2, None));
    assert!(!fleets::can_act_on_member(&f, wc1, sm1, Some((FleetRole::SquadMember, Some(20), Some(200)))));
    assert!(fleets::can_act_on_member(&f, sc1, sm1, Some((FleetRole::SquadMember, None, Some(100)))));
    assert!(!fleets::can_act_on_member(&f, sc1, sm1, Some((FleetRole::SquadMember, Some(20), Some(100)))));
    assert!(!fleets::can_act_on_member(&f, sm1, sm1, None));

    assert!(fleets::can_invite_member(&f, boss, FleetRole::FleetCommander, None, None));
    assert!(fleets::can_invite_member(&f, fc, FleetRole::WingCommander, Some(10), None));
    assert!(!fleets::can_invite_member(&f, fc, FleetRole::FleetCommander, None, None));
    assert!(fleets::can_invite_member(&f, wc1, FleetRole::SquadCommander, Some(10), Some(100)));
    assert!(!fleets::can_invite_member(&f, wc1, FleetRole::SquadCommander, Some(20), Some(200)));
    assert!(fleets::can_invite_member(&f, sc1, FleetRole::SquadMember, None, Some(100)));
    assert!(!fleets::can_invite_member(&f, sc1, FleetRole::SquadMember, Some(20), Some(100)));
    assert!(!fleets::can_invite_member(&f, sm1, FleetRole::SquadMember, Some(10), Some(100)));

    // 2. corp_governance.rs EffectiveCorpRoles, pending stasis vs immediate roles, mail ACL, and member limits
    let eff_roles = corp_governance::EffectiveCorpRoles {
        is_ceo: false,
        roles: 1,
        roles_at_hq: 2,
        roles_at_base: 4,
        roles_at_other: 8,
        grantable_roles: 16,
        grantable_roles_at_hq: 32,
        grantable_roles_at_base: 64,
        grantable_roles_at_other: 128,
        unmatured_mask: 0,
    };
    assert_eq!(eff_roles.any_active_roles(), 1 | 2 | 4 | 8);
    assert_eq!(eff_roles.any_grantable_roles(), 16 | 32 | 64 | 128);

    let ctx = test_ctx();
    let corp_id = 98_000_001_i64;
    let ceo_id = 90_000_001_i64;
    let member_id = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        assert_eq!(
            corp_governance::effective_corp_member_roles(snap, 999_999_999, member_id, 42, 1_000),
            (false, 42)
        );
        assert_eq!(corp_governance::resolve_corp_member_limit(Some(&ctx.sde), snap, 999_999_999), None);

        if let Some(corp) = snap.corporations.get_mut(&corp_id) {
            corp.ceo_id = ceo_id;
            corp.member_roles
                .insert(member_id, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT);
        }
        let mut gov = CorpGovState::default();
        gov.pending_role_grants.insert(
            member_id,
            (corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT, 2_000),
        );
        gov.restrict_corp_mails = true;
        snap.corp_governance.insert(corp_id, gov);

        // Before maturity (t=1000): breakdown masks all pending roles; effective_corp_member_roles only masks delayed stasis roles (Director), keeping Accountant!
        let bd_before = corp_governance::effective_corp_role_breakdown(snap, corp_id, member_id, 1_000);
        assert_eq!(bd_before.roles, 0);
        let (_, mem_roles_before) =
            corp_governance::effective_corp_member_roles(snap, corp_id, member_id, 0, 1_000);
        assert_eq!(mem_roles_before, corp_roles::ROLE_ACCOUNTANT);
        assert!(!corp_governance::can_send_corp_mail(snap, corp_id, member_id, 1_000));
        assert!(corp_governance::can_send_corp_mail(snap, corp_id, ceo_id, 1_000));

        // At maturity (t=2000): Director matures -> can_send_corp_mail becomes true
        assert!(corp_governance::can_send_corp_mail(snap, corp_id, member_id, 2_000));

        // Member limit resolution order: corp_member_limits -> corp_governance.member_limit -> CEO skills -> 10
        snap.skills.remove(&ceo_id);
        assert_eq!(corp_governance::corp_member_limit_from_skills(&ctx.sde, snap, ceo_id), 10);
        assert_eq!(corp_governance::resolve_corp_member_limit(None, snap, corp_id), Some(10));
        snap.corp_governance.get_mut(&corp_id).unwrap().member_limit = Some(45);
        assert_eq!(corp_governance::resolve_corp_member_limit(Some(&ctx.sde), snap, corp_id), Some(45));
        snap.corp_member_limits.insert(corp_id, 80);
        assert_eq!(corp_governance::resolve_corp_member_limit(Some(&ctx.sde), snap, corp_id), Some(80));
    });

    // 3. mail.rs calculate_cspa_fee & charge_cspa_action across exemptions and insufficient funds
    let stranger_id = 90_000_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&ceo_id].clone();
        ch.character_id = stranger_id;
        ch.corporation_id = 98_999_888;
        ch.alliance_id = Some(99_111_222);
        snap.characters.insert(stranger_id, ch);
        snap.characters.get_mut(&ceo_id).unwrap().alliance_id = Some(99_111_222);
        snap.characters.get_mut(&ceo_id).unwrap().balance = IskCents(500_000);
        snap.character_cspa_charges.insert(stranger_id, IskCents(295_000));
    });

    // Same-alliance exemption: exempt when exempt_on_same_alliance=true, charged when false
    let fee_exempt_ally = mail::calculate_cspa_fee(
        &ctx,
        &mail::CspaCalculationInput {
            caller_id: ceo_id,
            target_ids: &[stranger_id, ceo_id, 0],
            exempt_min_standing: 0.0,
            exempt_on_same_alliance: true,
            default_charge: IskCents(100_000),
        },
    )
    .unwrap();
    assert_eq!(fee_exempt_ally, IskCents(0));

    let fee_charged = mail::charge_cspa_action(
        &ctx,
        &mail::CspaCalculationInput {
            caller_id: ceo_id,
            target_ids: &[stranger_id],
            exempt_min_standing: 0.0,
            exempt_on_same_alliance: false,
            default_charge: IskCents(100_000),
        },
    )
    .unwrap();
    assert_eq!(fee_charged, IskCents(295_000));
    assert_eq!(
        ctx.db.with_snapshot(|s| s.characters[&ceo_id].balance),
        IskCents(205_000)
    );

    // Second charge exceeds remaining 205_000 -> InsufficientFundsForCSPA
    assert_eq!(
        mail::charge_cspa_action(
            &ctx,
            &mail::CspaCalculationInput {
                caller_id: ceo_id,
                target_ids: &[stranger_id],
                exempt_min_standing: 0.0,
                exempt_on_same_alliance: false,
                default_charge: IskCents(100_000),
            },
        )
        .unwrap_err()
        .code,
        "InsufficientFundsForCSPA"
    );

    // Friendly contact standing > exempt_min_standing exempts CSPA unless blocked
    ctx.db.with_mut_snapshot(|snap| {
        snap.char_contacts.insert(
            stranger_id,
            vec![SocialContactRecord {
                contact_id: ceo_id,
                contact_type: "character".into(),
                standing: 5.0,
                is_watched: false,
                is_blocked: false,
                label_ids: Vec::new(),
            }],
        );
    });
    assert_eq!(
        mail::calculate_cspa_fee(
            &ctx,
            &mail::CspaCalculationInput {
                caller_id: ceo_id,
                target_ids: &[stranger_id],
                exempt_min_standing: 0.0,
                exempt_on_same_alliance: false,
                default_charge: IskCents(100_000),
            },
        )
        .unwrap(),
        IskCents(0)
    );
}
