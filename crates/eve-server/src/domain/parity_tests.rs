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
