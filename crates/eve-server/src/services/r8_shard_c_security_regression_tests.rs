//! Adversarial regression tests for Round-8 Shard C security/mechanics issues:
//! `#503` (Quasar live corp/role/admin auth), `#522` (Bookmark & ESI folder ACLs),
//! `#525` (Skyhook V24 planet-scaled reagent yield), `#526` (Sov Hub startup fuel enforcement).

#![forbid(unsafe_code)]

use crate::esi::char_social_routes::dispatch_char_social_route_with_claims;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN};
use crate::router::ServerContext;
use crate::services::quasar_corp_domain;
use crate::services::quasar_social_freelance;
use crate::services::quasar_sov_hub;
use crate::services::quasar_sov_skyhook::{
    planet_hourly_reagent_yield, register_query_handlers, TYPE_ORBITAL_SKYHOOK,
};
use crate::services::quasar_vendors_and_war_services;
use crate::services::sov_gameplay::sov_gameplay_skyhook::advance_skyhook_reagents_in_snap;
use crate::services::sov_services::{TYPE_SOVEREIGNTY_HUB, TYPE_SUPERIONIC_ICE};
use eve_db::{
    corp_roles, BookmarkFolderRecord, BookmarkRecord, CorporationRecord, IskCents, SsoTokenClaims,
    UpwellStructureRecord, WorldRepository,
};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ctx() -> ServerContext {
    ServerContext::with_sde_and_repository(shared_sde(), Arc::new(WorldRepository::with_demo_seed()))
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
}

trait TestRouterExt {
    fn dispatch(&self, ctx: &AuthoritativeContext, req: &GoogleAny) -> Result<GoogleAny, (u32, String)>;
    fn dispatch_dyn(
        &self,
        ctx: &AuthoritativeContext,
        type_name: &str,
        msg: &DynamicQuasarMessage,
    ) -> Result<GoogleAny, (u32, String)>;
}

impl TestRouterExt for PublicGatewayRouter {
    fn dispatch(&self, ctx: &AuthoritativeContext, req: &GoogleAny) -> Result<GoogleAny, (u32, String)> {
        let norm = req.normalized_type_name();
        let handler = self.handlers.read().unwrap().get(norm).cloned().expect("handler");
        let dyn_msg = self
            .descriptors
            .decode_dynamic_or_wire(norm, &req.value)
            .unwrap_or_else(|_| crate::services::quasar_char_skill_services::decode_wire_dynamic(norm, &req.value));
        handler(ctx, req, &dyn_msg)
    }

    fn dispatch_dyn(
        &self,
        ctx: &AuthoritativeContext,
        type_name: &str,
        msg: &DynamicQuasarMessage,
    ) -> Result<GoogleAny, (u32, String)> {
        let req = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: Vec::new(),
        };
        let handler = self.handlers.read().unwrap().get(type_name).cloned().expect("handler");
        handler(ctx, &req, msg)
    }
}

fn auth_ctx(cid: i64, corp_id: i64, role_mask: u64) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(1),
        Some(cid as u32),
        Some(corp_id as u32),
        role_mask,
        "tranquility",
        vec![1; 16],
        1_700_000_000,
    )
}

fn id_bytes(field: u32, val: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, val, &mut inner);
    let mut outer = Vec::new();
    encode_bytes_field(field, &inner, &mut outer);
    outer
}

#[test]
fn r8_503_quasar_corp_freelance_skyhook_vendor_auth_and_stale_role_rejection() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    quasar_corp_domain::register(&router, Arc::clone(&db));
    quasar_social_freelance::register(&router, Arc::clone(&db));
    register_query_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    quasar_vendors_and_war_services::register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    for admin_type in [
        "eve.tournament.arena.api.CreateRequest",
        "eve.tournament.arena.api.DeleteRequest",
        "eve.vendors.steam.user.api.CreateRequest",
        "eve.vendors.fusionauth.api.LinkUserRequest",
        "eve.vendors.payletter.GetUserIdentityVerificationReceiptRequest",
        "eve.vendors.epic.api.DeliverEntitlementRequest",
        "eve.internal.debug.PingRequest",
    ] {
        assert!(PublicGatewayRouter::requires_admin_role(admin_type), "{admin_type} must require admin role");
    }

    let (ceo_id, stripped_member, proj_mgr_no_take) = (90_000_001_i64, 90_000_002, 90_000_003);
    let (corp_id, other_corp_id) = (98_503_001_i64, 98_503_002_i64);

    db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&ceo_id).cloned().unwrap();
        for cid in [ceo_id, stripped_member, proj_mgr_no_take] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            ceo_id,
            master_wallet: IskCents::from_isk(100_000_000),
            ..Default::default()
        };
        corp.members.extend([ceo_id, stripped_member, proj_mgr_no_take]);
        corp.member_roles.insert(stripped_member, 0);
        corp.member_roles.insert(proj_mgr_no_take, corp_roles::ROLE_PROJECT_MANAGER);
        snap.corporations.insert(corp_id, corp);
        snap.corporations.insert(other_corp_id, CorporationRecord { corporation_id: other_corp_id, ceo_id: 90_999_999, ..Default::default() });
    });

    let forged_ctx = auth_ctx(stripped_member, corp_id, u64::MAX);
    let corp_req_bytes = id_bytes(1, corp_id as u64);
    for restricted_type in [
        "eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest",
        "eve.corporation.alsclogs.GetRequest",
        "eve.corporation.industry.job.GetActiveRequest",
        "eve.corporation.industry.blueprints.GetRequest",
        "eve.corporation.members.roles.auditlog.GetRequest",
        "eve.corporation.market.entry.GetEntriesRequest",
        "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest",
    ] {
        let any_req = GoogleAny { type_url: format!("type.googleapis.com/{restricted_type}"), value: corp_req_bytes.clone() };
        assert_eq!(router.dispatch(&forged_ctx, &any_req).unwrap_err().0, STATUS_FORBIDDEN);
    }

    assert_eq!(
        router.dispatch(&forged_ctx, &GoogleAny { type_url: "type.googleapis.com/eve.tournament.arena.api.CreateRequest".into(), value: Vec::new() }).unwrap_err().0,
        STATUS_FORBIDDEN
    );

    let pm_ctx = auth_ctx(proj_mgr_no_take, corp_id, u64::MAX);
    let mut create_msg = DynamicQuasarMessage::new("eve_public.freelance.project.api.CreateRequest");
    create_msg.fields.insert(1, DynamicValue::String("Escrow Drain".into()));
    create_msg.fields.insert(3, DynamicValue::Int64(10));
    let mut method_cfg = DynamicQuasarMessage::new("Cfg");
    method_cfg.fields.insert(1, DynamicValue::String("DestroyCapsuleerShips".into()));
    create_msg.fields.insert(6, DynamicValue::Message(Box::new(method_cfg)));
    let mut cur_msg = DynamicQuasarMessage::new("Currency");
    cur_msg.fields.insert(1, DynamicValue::Int64(1_000_000));
    let mut reward_msg = DynamicQuasarMessage::new("Rew");
    reward_msg.fields.insert(1, DynamicValue::Message(Box::new(cur_msg)));
    create_msg.fields.insert(10, DynamicValue::Message(Box::new(reward_msg)));
    assert_eq!(
        router.dispatch_dyn(&pm_ctx, "eve_public.freelance.project.api.CreateRequest", &create_msg).unwrap_err().0,
        STATUS_FORBIDDEN
    );

    let ceo_ctx = auth_ctx(ceo_id, corp_id, 0);
    assert!(router.dispatch_dyn(&ceo_ctx, "eve_public.freelance.project.api.CreateRequest", &create_msg).is_ok());
    let proj_uuid = db.with_snapshot(|s| {
        let (&pid, _) = s.corp_projects.iter().next().expect("created freelance project");
        crate::services::social_corp_fleet::social_corp_projects::freelance_uuid(1_700_000_000, pid)
    });
    let mut mgr_get_msg = DynamicQuasarMessage::new("GetForManagerRequest");
    let mut uuid_msg = DynamicQuasarMessage::new("Uuid");
    uuid_msg.fields.insert(1, DynamicValue::Bytes(proj_uuid.to_vec()));
    mgr_get_msg.fields.insert(1, DynamicValue::Message(Box::new(uuid_msg)));
    assert_eq!(
        router.dispatch_dyn(&forged_ctx, "eve_public.freelance.project.api.GetForManagerRequest", &mgr_get_msg).unwrap_err().0,
        STATUS_FORBIDDEN
    );

    for ok_type in [
        "eve.corporation.accounting.wallet.GetAllDivisionBalanceRequest",
        "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest",
    ] {
        assert!(router.dispatch(&ceo_ctx, &GoogleAny { type_url: format!("type.googleapis.com/{ok_type}"), value: corp_req_bytes.clone() }).is_ok());
    }
}

#[tokio::test]
async fn r8_522_bookmark_rpcs_and_esi_enforce_access_group_folder_acls() {
    let ctx = make_ctx();
    let (corp_id, other_corp_id) = (98_522_001_i64, 98_522_002_i64);
    let (admin_cid, viewer_cid, zero_acl_cid, outsider_cid) = (90_000_001_i64, 90_000_002, 90_000_003, 90_000_004);
    let (admin_gid, view_gid, locked_fid, secret_bm_id) = (522_10_i64, 522_11, 522_20, 522_30);

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&admin_cid).cloned().unwrap();
        for (cid, c_corp) in [(admin_cid, corp_id), (viewer_cid, corp_id), (zero_acl_cid, corp_id), (outsider_cid, other_corp_id)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.corporation_id = c_corp;
            ch.solar_system_id = 30_000_142;
            ch.station_id = Some(60_003_760);
            snap.characters.insert(cid, ch);
        }
        let mut corp = CorporationRecord { corporation_id: corp_id, ceo_id: admin_cid, ..Default::default() };
        corp.members.extend([admin_cid, viewer_cid, zero_acl_cid]);
        snap.corporations.insert(corp_id, corp);
        snap.corporations.insert(other_corp_id, CorporationRecord { corporation_id: other_corp_id, ceo_id: outsider_cid, ..Default::default() });
        snap.access_groups.insert(admin_gid, vec![admin_cid]);
        snap.access_groups.insert(view_gid, vec![viewer_cid]);
        snap.bookmark_folders.insert(
            locked_fid,
            BookmarkFolderRecord {
                folder_id: locked_fid,
                owner_id: admin_cid,
                folder_name: "BlackOps Staging".into(),
                is_shared: true,
                corporation_id: Some(corp_id),
                admin_group_id: Some(admin_gid),
                view_group_id: Some(view_gid),
                ..Default::default()
            },
        );
        snap.bookmarks.insert(
            secret_bm_id,
            BookmarkRecord {
                bookmark_id: secret_bm_id,
                owner_id: admin_cid,
                label: "Titan Safe".into(),
                solar_system_id: 30_000_142,
                x: 1000.0,
                y: 2000.0,
                z: 3000.0,
                item_id: None,
                memo: "classified".into(),
                folder_id: Some(locked_fid),
                is_shared: true,
            },
        );
    });

    let kw = HashMap::new();
    let mut zero_sess = SessionState { user_id: Some(3), character_id: Some(zero_acl_cid), corporation_id: Some(corp_id), solar_system_id: Some(30_000_142), station_id: Some(60_003_760), ..Default::default() };
    let mut viewer_sess = SessionState { user_id: Some(2), character_id: Some(viewer_cid), corporation_id: Some(corp_id), solar_system_id: Some(30_000_142), station_id: Some(60_003_760), ..Default::default() };

    let z_flds = ctx.dispatch_rpc(&mut zero_sess, "bookmark", None, "GetFolders", &[], &kw, None).await.unwrap();
    let z_bms = ctx.dispatch_rpc(&mut zero_sess, "bookmark", None, "GetBookmarks", &[], &kw, None).await.unwrap();
    assert!(!format!("{z_flds:?}").contains("BlackOps Staging"));
    assert!(!format!("{z_bms:?}").contains("Titan Safe"));
    assert!(ctx.dispatch_rpc(&mut zero_sess, "bookmark", None, "AddToKnownFolders", &[PyRep::Integer(locked_fid), PyRep::Bool(true)], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut zero_sess, "bookmark", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(secret_bm_id)]), PyRep::None], &kw, None).await.is_err());
    assert!(ctx.dispatch_rpc(&mut zero_sess, "bookmark", None, "BookmarkLocation", &[PyRep::String("Injected".into()), PyRep::String(String::new()), PyRep::Integer(locked_fid)], &kw, None).await.is_err());

    let v_bms = ctx.dispatch_rpc(&mut viewer_sess, "bookmark", None, "GetBookmarks", &[], &kw, None).await.unwrap();
    assert!(format!("{v_bms:?}").contains("Titan Safe"));
    assert!(ctx.dispatch_rpc(&mut viewer_sess, "bookmark", None, "AddToKnownFolders", &[PyRep::Integer(locked_fid), PyRep::Bool(true)], &kw, None).await.is_ok());
    let copied = ctx.dispatch_rpc(&mut viewer_sess, "bookmark", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(secret_bm_id)]), PyRep::None], &kw, None).await.expect("viewer copy");
    let copied_bm_id = match copied.dict_get("bookmarkIDs") {
        Some(PyRep::List(v)) => v[0].as_i64().unwrap(),
        other => panic!("unexpected bookmarkIDs: {other:?}"),
    };
    assert!(ctx.dispatch_rpc(&mut viewer_sess, "bookmark", None, "MoveBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(copied_bm_id)]), PyRep::Integer(locked_fid)], &kw, None).await.is_err());

    let mut p_corp = HashMap::new();
    p_corp.insert("corporation_id".into(), corp_id.to_string());
    let empty_q = HashMap::new();
    let mk_claims = |cid: i64| SsoTokenClaims {
        sub: format!("CHARACTER:EVE:{cid}"),
        name: "Test".into(),
        owner: "owner".into(),
        user_id: 1,
        character_id: cid,
        scp: vec!["esi-bookmarks.read_corporation_bookmarks.v1".into()],
        iss: "evomu".into(),
        aud: vec!["EVE Online".into()],
        iat: 1_700_000_000,
        exp: 1_900_000_000,
        jti: "jti".into(),
    };

    assert_eq!(
        dispatch_char_social_route_with_claims(&ctx, "GET", "/corporations/{corporation_id}/bookmarks", &p_corp, &empty_q, Some(&mk_claims(outsider_cid)), b"").unwrap().status_code,
        403
    );
    for ep in ["/corporations/{corporation_id}/bookmarks", "/corporations/{corporation_id}/bookmarks/folders"] {
        let z_res = dispatch_char_social_route_with_claims(&ctx, "GET", ep, &p_corp, &empty_q, Some(&mk_claims(zero_acl_cid)), b"").unwrap();
        assert_eq!(z_res.body.unwrap(), serde_json::json!([]));
    }
    let v_res = dispatch_char_social_route_with_claims(&ctx, "GET", "/corporations/{corporation_id}/bookmarks", &p_corp, &empty_q, Some(&mk_claims(viewer_cid)), b"").unwrap();
    let v_body = v_res.body.unwrap();
    let v_arr = v_body.as_array().unwrap();
    assert_eq!(v_arr.len(), 1);
    assert_eq!(v_arr[0]["bookmark_id"], secret_bm_id);
}

#[test]
fn r8_525_526_skyhook_planet_scaled_harvest_and_sov_hub_startup_fuel_enforcement() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    register_query_handlers(&router, Arc::clone(&db), Arc::clone(&sde));
    quasar_sov_hub::register_handlers(&router, Arc::clone(&db), Arc::clone(&sde));

    let (cid, corp_id) = (90_000_001_i64, 98_525_001_i64);
    let (sys_id, planet_id) = sde
        .solar_systems
        .keys()
        .find_map(|&sys| {
            sde.celestials_in_system(sys)
                .into_iter()
                .find(|c| matches!(c.type_id, 12 | 2015))
                .map(|c| (sys, c.celestial_id))
        })
        .expect("Ice or Lava planet in SDE");
    let (skyhook_id, hub_id, t0) = (79_525_001_i64, 79_526_001_i64, 1_700_000_000_u64);

    let expected_hourly = planet_hourly_reagent_yield(&sde, Some(planet_id), Some(sys_id), TYPE_SUPERIONIC_ICE);
    assert!((300..=1_800).contains(&expected_hourly));

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
            ch.solar_system_id = sys_id;
        }
        let mut corp = CorporationRecord { corporation_id: corp_id, ceo_id: cid, ..Default::default() };
        corp.members.insert(cid);
        corp.member_roles.insert(cid, corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_STATION_MANAGER);
        snap.corporations.insert(corp_id, corp);
        snap.structures.insert(
            skyhook_id,
            UpwellStructureRecord { structure_id: skyhook_id, type_id: TYPE_ORBITAL_SKYHOOK, owner_corp_id: corp_id, solar_system_id: sys_id, name: "Equinox Skyhook".into(), state: "shield_vulnerable".into(), ..Default::default() },
        );
        snap.sov_runtime.skyhook_planets.insert(skyhook_id, planet_id);
        snap.sov_runtime.skyhook_active.insert(skyhook_id, true);
        snap.sov_runtime.skyhook_reagent_bays.insert(skyhook_id, (TYPE_SUPERIONIC_ICE, 0, 0, t0));
        snap.sov_runtime.skyhook_reagents.insert(skyhook_id, (TYPE_SUPERIONIC_ICE, 0));

        let (_, sec, insec) = advance_skyhook_reagents_in_snap(snap, &sde, skyhook_id, t0 + 7_200).unwrap();
        assert_eq!(sec + insec, expected_hourly * 2);
        assert!(sec + insec >= 600);

        snap.structures.insert(
            hub_id,
            UpwellStructureRecord { structure_id: hub_id, type_id: TYPE_SOVEREIGNTY_HUB, owner_corp_id: corp_id, solar_system_id: sys_id, name: "Sov Hub".into(), state: "shield_vulnerable".into(), ..Default::default() },
        );
    });

    let (&upg_type_id, def) = sde
        .types
        .keys()
        .filter_map(|tid| quasar_sov_hub::upgrade_definition(&sde, *tid).map(|d| (tid, d)))
        .find(|(_, d)| d.fuel_startup > 0 && d.fuel_per_hour > 0)
        .expect("fueled sov upgrade def");

    let seed_hub = |fuel_qty: u32| {
        db.with_mut_snapshot(|snap| {
            let mut fuel = BTreeMap::new();
            fuel.insert(def.fuel_type, fuel_qty);
            snap.sov_runtime.sov_hubs.insert(
                hub_id,
                eve_db::PersistedSovHubData {
                    hub_id,
                    upgrades: vec![eve_db::PersistedHubUpgrade { type_id: upg_type_id, online: false, low_power: false }],
                    fuel,
                    workforce: Default::default(),
                    last_updated_unix: 0,
                },
            );
        });
    };
    seed_hub(def.fuel_per_hour);

    let mgr_ctx = auth_ctx(cid, corp_id, corp_roles::ROLE_STATION_MANAGER);
    let mut hub_id_msg = DynamicQuasarMessage::new("Id");
    hub_id_msg.fields.insert(1, DynamicValue::Int64(hub_id));
    let mut tid_msg = DynamicQuasarMessage::new("Tid");
    tid_msg.fields.insert(1, DynamicValue::Int64(upg_type_id as i64));
    let mut cfg_item = DynamicQuasarMessage::new("Cfg");
    cfg_item.fields.insert(1, DynamicValue::Message(Box::new(tid_msg)));
    cfg_item.fields.insert(2, DynamicValue::Bool(true));
    let mut proc_msg = DynamicQuasarMessage::new("ProcessConfigurationRequest");
    proc_msg.fields.insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(cfg_item))]));
    proc_msg.fields.insert(3, DynamicValue::Message(Box::new(hub_id_msg)));

    let bypass_err = router
        .dispatch_dyn(&mgr_ctx, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", &proc_msg)
        .expect_err("onlining upgrade without fuel_startup must be rejected");
    assert_eq!(bypass_err.0, STATUS_BAD_REQUEST);

    seed_hub(def.fuel_startup + def.fuel_per_hour);
    router
        .dispatch_dyn(&mgr_ctx, "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest", &proc_msg)
        .expect("onlining upgrade with sufficient startup + hourly fuel succeeds");
    let remaining = db.with_snapshot(|s| s.sov_runtime.sov_hubs[&hub_id].fuel.get(&def.fuel_type).copied().unwrap_or(0));
    assert_eq!(remaining, def.fuel_per_hour);
}
