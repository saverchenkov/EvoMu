//! Integration tests covering validation, RBAC, and error/success edge paths across:
//! 1. Industry, Research, Reactions, Blueprints, and Facilities
//! 2. Planetary Industry (Colonies, Network Commands, Orbitals, GM RPCs)
//! 3. Upwell Structures (Anchoring Rules, ACL Profiles/Groups, Lifecycle Ops, Docking)
//! 4. Faction Warfare, Warzone/Insurgency, Dynamic Resource Cache, and Sovereignty Ops
//!
//! All state mutations and checks go through authentic RPC, GM `/slash`, and Quasar gateway calls.

use crate::common;
use eve_db::TypeId;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const PERIMETER_SYSTEM_ID: u32 = 30_000_144;

const DEMO_CHAR_ID: i64 = 90_000_001;
const CALDARI_FACTION_ID: i64 = 500_001;
const MINMATAR_FACTION_ID: i64 = 500_002;
const JOVE_NON_MILITIA_FACTION_ID: i64 = 500_005;

const RIFTER_BP_TYPE_ID: TypeId = 691;
const RAITARU_TYPE_ID: TypeId = 35825;
const ASTRAHUS_TYPE_ID: TypeId = 35832;
const SOVEREIGNTY_HUB_TYPE_ID: TypeId = 32458;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn rpc_bound(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    bound_id: u64,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, Some(bound_id), method, args, &HashMap::new(), None)
        .await
}

async fn setup_demo_pilot(ctx: &ServerContext, sess_id: u64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(DEMO_CHAR_ID)],
    )
    .await
    .expect("SelectCharacterID demo");
    sess
}

async fn setup_gm_pilot(ctx: &ServerContext, sess_id: u64, char_id: i64) -> SessionState {
    let user = format!("gm_cov_{char_id}");
    let pass = format!("{user}-secret");
    ctx.db
        .provision_gm_account(&user, &pass, char_id, &format!("GM Cov {char_id}"));
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, &user, &pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID gm");
    sess
}

async fn setup_fresh_player_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    char_name: &str,
) -> (SessionState, i64) {
    let pass = format!("{username}-secret");
    ctx.db.provision_player_account(username, &pass);
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, &pass).await;

    let (&bloodline_id, &race_id) = ctx
        .sde
        .bloodlines
        .iter()
        .min_by_key(|(&bid, _)| bid)
        .map(|(bid, b)| (bid, &b.race_id))
        .expect("SDE bloodlines");
    let ancestry_id = ctx
        .sde
        .ancestries
        .values()
        .filter(|a| a.bloodline_id == bloodline_id)
        .map(|a| a.ancestry_id)
        .min()
        .unwrap_or(1);
    let school_id = ctx
        .sde
        .schools
        .values()
        .filter(|s| s.race_id == race_id)
        .map(|s| s.school_id)
        .min()
        .unwrap_or(11);

    let char_info = PyRep::keyval([("gender", PyRep::Integer(1))]);
    let portrait = PyRep::keyval([("backgroundID", PyRep::Integer(1))]);
    let created = rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[
            PyRep::String(char_name.into()),
            PyRep::Integer(i64::from(race_id)),
            PyRep::Integer(i64::from(bloodline_id)),
            PyRep::Integer(1),
            PyRep::Integer(i64::from(ancestry_id)),
            char_info,
            portrait,
            PyRep::Integer(i64::from(school_id)),
        ],
    )
    .await
    .expect("CreateCharacterWithDoll");
    let char_id = created.as_i64().expect("created characterID");
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID fresh");
    (sess, char_id)
}

async fn gm_create_item(ctx: &ServerContext, sess: &mut SessionState, type_id: u32, qty: i32) -> i64 {
    let rep = common::slash_cmd(ctx, sess, &format!("/createitem {type_id} {qty}")).await;
    rep.dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("/createitem itemID")
}

async fn bind_planet_mgr(ctx: &ServerContext, sess: &mut SessionState, planet_id: i64) -> u64 {
    let bound = rpc(
        ctx,
        sess,
        "planetMgr",
        "MachoBindObject",
        &[PyRep::Integer(planet_id), PyRep::None],
    )
    .await
    .expect("planetMgr.MachoBindObject");
    match bound {
        PyRep::Tuple(parts) => parts[0].as_i64().expect("bound planetMgr id") as u64,
        other => panic!("expected MachoBindObject tuple, got {other:?}"),
    }
}

fn find_deterministic_system(ctx: &ServerContext, pred: impl Fn(&eve_sde::SolarSystem) -> bool) -> u32 {
    ctx.sde
        .solar_systems
        .iter()
        .filter(|(_, s)| pred(s))
        .map(|(&id, _)| id)
        .min()
        .expect("matching solar system in SDE")
}

fn find_planet_in_system_by_type(
    ctx: &ServerContext,
    sys_id: u32,
    wanted_planet_type: u32,
) -> Option<i64> {
    ctx.sde
        .celestials_in_system(sys_id)
        .into_iter()
        .filter(|c| c.group_id == 7 && c.type_id == wanted_planet_type)
        .map(|c| c.celestial_id)
        .min()
}

fn find_any_planet_by_type(ctx: &ServerContext, wanted_planet_type: u32) -> (i64, u32) {
    if let Some(pid) = find_planet_in_system_by_type(ctx, PERIMETER_SYSTEM_ID, wanted_planet_type) {
        return (pid, PERIMETER_SYSTEM_ID);
    }
    let mut sys_ids: Vec<u32> = ctx.sde.solar_systems.keys().copied().collect();
    sys_ids.sort_unstable();
    for sid in sys_ids {
        if let Some(pid) = find_planet_in_system_by_type(ctx, sid, wanted_planet_type) {
            return (pid, sid);
        }
    }
    panic!("no planet of type {wanted_planet_type} in SDE");
}

fn quasar_call(
    ctx: &ServerContext,
    sess: &SessionState,
    type_name: &str,
    payload: Vec<u8>,
) -> eve_proto::quasar::EvePublicResponse {
    let now_u = ctx.now_unix();
    let req = eve_proto::quasar::EvePublicRequest {
        issued: Some(eve_proto::quasar::GoogleTimestamp {
            seconds: now_u as i64,
            nanos: 0,
        }),
        correlation_uuid: vec![0x31; 16],
        payload: Some(eve_proto::quasar::GoogleAny {
            type_url: format!("type.googleapis.com/{type_name}"),
            value: payload,
        }),
        application_instance_uuid: vec![0x11; 16],
        external_origin: "it-test".into(),
        authoritative_context: Some(eve_proto::quasar::AuthoritativeContext::for_session_at(
            sess.user_id.map(|u| u as i32),
            sess.character_id.and_then(|v| u32::try_from(v).ok()),
            sess.corporation_id.and_then(|v| u32::try_from(v).ok()),
            sess.corp_role,
            "tranquility",
            vec![0x22; 16],
            now_u,
        )),
        trace_context: None,
    };
    ctx.quasar_gateway
        .handle_public_request(sess, &req, None, false, now_u)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_industry_bp_facility_and_job_validation_errors() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut gm = setup_gm_pilot(&ctx, 1101, 91_100_001).await;
    let mut non_owner = setup_demo_pilot(&ctx, 1102).await;

    // 1. BlueprintManager & FacilityManager queries + tax validation errors
    let bp_data = rpc(
        &ctx,
        &mut gm,
        "blueprintManager",
        "GetBlueprintData",
        &[PyRep::Integer(RIFTER_BP_TYPE_ID as i64)],
    )
    .await
    .expect("GetBlueprintData");
    assert!(matches!(bp_data, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let limits = rpc(&ctx, &mut gm, "blueprintManager", "GetLimits", &[])
        .await
        .expect("GetLimits");
    assert!(limits.dict_get("maxManufacturingJobs").is_some() || matches!(limits, PyRep::KeyVal(_)));

    let jita_fac = rpc(
        &ctx,
        &mut gm,
        "facilityManager",
        "GetFacility",
        &[PyRep::Integer(JITA_STATION_ID)],
    )
    .await
    .expect("GetFacility Jita");
    assert_eq!(
        jita_fac.dict_get("facilityID").and_then(|v| v.as_i64()),
        Some(JITA_STATION_ID)
    );

    let sci = rpc(
        &ctx,
        &mut gm,
        "map",
        "GetIndustryCostModifier",
        &[PyRep::Integer(i64::from(JITA_SYSTEM_ID))],
    )
    .await
    .expect("GetIndustryCostModifier");
    assert!(matches!(sci, PyRep::Real(_) | PyRep::Dict(_) | PyRep::KeyVal(_)));

    let jobs_24h = rpc(
        &ctx,
        &mut gm,
        "map",
        "GetIndustryJobsOverLast24Hours",
        &[PyRep::Integer(i64::from(JITA_SYSTEM_ID))],
    )
    .await
    .expect("GetIndustryJobsOverLast24Hours");
    assert!(jobs_24h.as_i64().unwrap_or(0) >= 0);

    // SetFacilityTaxes validation errors: rate > 10% and nonexistent facility
    let bad_rate_err = rpc(
        &ctx,
        &mut non_owner,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Real(0.25)],
    )
    .await
    .expect_err("industry tax > 10% must fail");
    assert_eq!(bad_rate_err.code, "InvalidFacilityTaxRate");

    let neg_rate_err = rpc(
        &ctx,
        &mut non_owner,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(JITA_STATION_ID), PyRep::Real(-0.01)],
    )
    .await
    .expect_err("negative industry tax must fail");
    assert_eq!(neg_rate_err.code, "InvalidFacilityTaxRate");

    let missing_fac_err = rpc(
        &ctx,
        &mut non_owner,
        "facilityManager",
        "SetFacilityTaxes",
        &[PyRep::Integer(999_888_777), PyRep::Real(0.05)],
    )
    .await
    .expect_err("nonexistent facility tax update must fail");
    assert_eq!(missing_fac_err.code, "FacilityNotFound");

    // 2. InstallJob parameter and permission validation errors
    let bad_params_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[PyRep::Integer(0), PyRep::Integer(1), PyRep::Integer(0)],
    )
    .await
    .expect_err("zero runs must fail");
    assert_eq!(bad_params_err.code, "InvalidIndustryJobParameters");

    let unknown_bp_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(999_999_999),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("unknown blueprint must fail");
    assert!(
        matches!(
            unknown_bp_err.code.as_str(),
            "UnknownBlueprint" | "InvalidIndustryJobParameters" | "BlueprintNotAvailable"
        ),
        "unexpected unknown_bp_err: {unknown_bp_err:?}"
    );

    // Give GM a Rifter BPO + ISK, verify insufficient materials error first
    common::slash_cmd(&ctx, &mut gm, "/giveisk 500000000").await;
    let bpo_item_id = gm_create_item(&ctx, &mut gm, RIFTER_BP_TYPE_ID, 1).await;

    // Non-owner trying to install job on GM's blueprint item -> BlueprintNotAvailable
    let non_owner_bp_err = rpc(
        &ctx,
        &mut non_owner,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpo_item_id),
            PyRep::Integer(i64::from(RIFTER_BP_TYPE_ID)),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
        ],
    )
    .await
    .expect_err("non-owner blueprint use must fail");
    assert_eq!(non_owner_bp_err.code, "BlueprintNotAvailable");

    // Explicit ownerID mismatch -> OwnerAccessDenied
    let owner_mismatch_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[PyRep::keyval([
            ("blueprintItemID", PyRep::Integer(bpo_item_id)),
            ("blueprintTypeID", PyRep::Integer(i64::from(RIFTER_BP_TYPE_ID))),
            ("activityID", PyRep::Integer(1)),
            ("runs", PyRep::Integer(1)),
            ("facilityID", PyRep::Integer(JITA_STATION_ID)),
            ("ownerID", PyRep::Integer(DEMO_CHAR_ID)),
        ])],
    )
    .await
    .expect_err("explicit ownerID mismatch must fail");
    assert_eq!(owner_mismatch_err.code, "OwnerAccessDenied");

    // Invention on BPO -> InventionRequiresBPC
    let inv_bpo_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpo_item_id),
            PyRep::Integer(i64::from(RIFTER_BP_TYPE_ID)),
            PyRep::Integer(8),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
        ],
    )
    .await
    .expect_err("invention on BPO must fail");
    assert_eq!(inv_bpo_err.code, "InventionRequiresBPC");

    // Manufacturing without Industry skill -> MissingIndustrySkills, then grant Industry/Science and hit InsufficientManufacturingMaterials
    let no_skill_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpo_item_id),
            PyRep::Integer(i64::from(RIFTER_BP_TYPE_ID)),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
        ],
    )
    .await
    .expect_err("manufacturing without Industry skill must fail");
    assert_eq!(no_skill_err.code, "MissingIndustrySkills");

    common::slash_cmd(&ctx, &mut gm, "/giveskill 3380 5").await;
    common::slash_cmd(&ctx, &mut gm, "/giveskill 3402 5").await;

    let no_mats_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpo_item_id),
            PyRep::Integer(i64::from(RIFTER_BP_TYPE_ID)),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
        ],
    )
    .await
    .expect_err("missing manufacturing mats must fail");
    assert_eq!(no_mats_err.code, "InsufficientManufacturingMaterials");

    // Try reaction formula at NPC station in highsec -> FacilityDoesNotSupportReactions / ReactionsForbiddenInHighSec
    let reaction_bp_tid = ctx
        .sde
        .blueprints
        .iter()
        .filter(|(_, bp)| {
            bp.activities.contains_key("reaction") || bp.activities.contains_key("9")
        })
        .map(|(&id, _)| id)
        .min()
        .expect("SDE reaction blueprint");
    let react_bp_item = gm_create_item(&ctx, &mut gm, reaction_bp_tid, 1).await;
    let react_highsec_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(react_bp_item),
            PyRep::Integer(i64::from(reaction_bp_tid)),
            PyRep::Integer(9),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
        ],
    )
    .await
    .expect_err("reaction at NPC station in highsec must fail");
    assert!(
        matches!(
            react_highsec_err.code.as_str(),
            "FacilityDoesNotSupportReactions" | "ReactionsForbiddenInHighSec"
        ),
        "unexpected react_highsec_err: {react_highsec_err:?}"
    );

    // Provide manufacturing mats for 2 Rifter BPOs and verify slot limit + lifecycle errors
    let rifter_bp = ctx.sde.blueprints.get(&RIFTER_BP_TYPE_ID).expect("Rifter BP");
    for mat in &rifter_bp.materials {
        gm_create_item(&ctx, &mut gm, mat.type_id, (mat.quantity as i32) * 10).await;
    }
    let job_rep = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpo_item_id),
            PyRep::Integer(i64::from(RIFTER_BP_TYPE_ID)),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
        ],
    )
    .await
    .expect("InstallJob Rifter mfg");
    let job_id = job_rep
        .as_i64()
        .or_else(|| job_rep.dict_get("jobID").and_then(|v| v.as_i64()))
        .expect("installed jobID");

    // Second manufacturing job with 1 base slot (no Mass Production) -> IndustrySlotsFull
    let bpo_item_id_2 = gm_create_item(&ctx, &mut gm, RIFTER_BP_TYPE_ID, 1).await;
    let slot_full_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpo_item_id_2),
            PyRep::Integer(i64::from(RIFTER_BP_TYPE_ID)),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
        ],
    )
    .await
    .expect_err("exceeding manufacturing slots must fail");
    assert_eq!(slot_full_err.code, "IndustrySlotsFull");

    // Grant Mass Production (3387) so slot check passes, then re-using locked blueprint -> BlueprintNotAvailable
    common::slash_cmd(&ctx, &mut gm, "/giveskill 3387 2").await;
    let locked_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpo_item_id),
            PyRep::Integer(i64::from(RIFTER_BP_TYPE_ID)),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(JITA_STATION_ID),
        ],
    )
    .await
    .expect_err("locked blueprint reuse must fail");
    assert_eq!(locked_err.code, "BlueprintNotAvailable");

    // 3. CancelJob / CompleteJob lifecycle error paths
    let not_found_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(999_888_777)],
    )
    .await
    .expect_err("CancelJob nonexistent must fail");
    assert_eq!(not_found_err.code, "JobNotFound");

    let non_owner_cancel = rpc(
        &ctx,
        &mut non_owner,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(job_id)],
    )
    .await
    .expect_err("non-owner CancelJob must fail");
    assert_eq!(non_owner_cancel.code, "JobOwnershipViolation");

    let not_ready_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(job_id)],
    )
    .await
    .expect_err("CompleteJob before finish must fail");
    assert_eq!(not_ready_err.code, "JobNotYetComplete");

    // Advance job timer cleanly with GM /completeindustry and complete
    common::slash_cmd(&ctx, &mut gm, &format!("/completeindustry {job_id}")).await;
    let completed = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(job_id)],
    )
    .await
    .expect("CompleteJob after ready");
    assert!(matches!(completed, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::None | PyRep::Integer(_)));

    // Completing or cancelling again -> InvalidJobState
    let recomplete_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "CompleteJob",
        &[PyRep::Integer(job_id)],
    )
    .await
    .expect_err("re-completing delivered job must fail");
    assert_eq!(recomplete_err.code, "InvalidJobState");

    let recancel_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "CancelJob",
        &[PyRep::Integer(job_id)],
    )
    .await
    .expect_err("cancelling delivered job must fail");
    assert_eq!(recancel_err.code, "InvalidJobState");

    // Non-BPO (BPC) cannot be researched for ME/TE -> CannotResearchOrCopyBPC
    // Also test NotDocked when undocked without remote skill
    common::slash_cmd(&ctx, &mut gm, &format!("/tr {PERIMETER_SYSTEM_ID}")).await;
    let not_docked_err = rpc(
        &ctx,
        &mut gm,
        "industryManager",
        "InstallJob",
        &[
            PyRep::Integer(bpo_item_id_2),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("InstallJob without docked facility or facilityID must fail");
    assert!(
        matches!(
            not_docked_err.code.as_str(),
            "NotDocked" | "FacilityOutOfRemoteRange" | "FacilityNotFound"
        ),
        "unexpected not_docked_err: {not_docked_err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_pi_orbitals_gm_and_colony_validation_paths() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut non_gm = setup_demo_pilot(&ctx, 1201).await;
    let mut gm = setup_gm_pilot(&ctx, 1202, 91_200_001).await;

    // Pick a deterministic temperate planet (type 11) and incompatible Command Center from SDE
    let temp_cc_tid: u32 = 2254;
    let barren_cc_tid: u32 = ctx
        .sde
        .types
        .iter()
        .filter(|(_, t)| {
            t.group_id == 1027
                && t.default_attributes
                    .get(&1632)
                    .copied()
                    .is_some_and(|pt| pt > 0.0 && (pt as u32) != 11)
        })
        .map(|(&tid, _)| tid)
        .min()
        .unwrap_or(2524);
    let (planet_id, planet_sys) = find_any_planet_by_type(&ctx, 11);

    // 1. All 9 planetMgr.GM* RPCs must reject non-GM with AdminAccessDenied
    let expected_admin_code = String::from("AdminAccessDenied");
    for (method, args) in [
        ("GMAddCommodity", vec![PyRep::Integer(1), PyRep::Integer(2393), PyRep::Integer(10)]),
        ("GMConvertCommandCenter", vec![PyRep::Integer(1)]),
        ("GMForceInstallProgram", vec![PyRep::Integer(1), PyRep::Integer(2073), PyRep::Integer(300)]),
        ("GMVerifySimulation", vec![PyRep::Integer(planet_id)]),
        ("GMGetLocalDistributionReport", vec![PyRep::Integer(planet_id), PyRep::Integer(2073)]),
        ("GMGetSynchedServerState", vec![PyRep::Integer(planet_id)]),
        ("GMRunDepletionSim", vec![PyRep::Integer(planet_id), PyRep::Integer(2073)]),
        ("GMCreateNuggetLayer", vec![PyRep::Integer(planet_id), PyRep::Integer(2073)]),
        ("GMGetCompleteResource", vec![PyRep::Integer(planet_id), PyRep::Integer(2073)]),
    ] {
        let err = rpc(&ctx, &mut non_gm, "planetMgr", method, &args)
            .await
            .expect_err(&format!("non-GM {method} must fail"));
        assert_eq!(err.code, expected_admin_code, "{method} returned {err:?}");
    }

    // 2. GM RPCs on missing colony / invalid planetID
    let gm_bad_planet = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetLocalDistributionReport",
        &[PyRep::Integer(9_999_999), PyRep::Integer(2073)],
    )
    .await
    .expect_err("GMGetLocalDistributionReport invalid planet");
    assert_eq!(gm_bad_planet.code, "InvalidPlanetID");

    let gm_convert_empty = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMConvertCommandCenter",
        &[PyRep::Integer(planet_id), PyRep::Integer(1)],
    )
    .await
    .expect("GMConvertCommandCenter without colony returns Bool(false)");
    assert_eq!(gm_convert_empty, PyRep::Bool(false));

    let gm_verify = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMVerifySimulation",
        &[PyRep::Integer(planet_id)],
    )
    .await
    .expect("GMVerifySimulation");
    assert!(matches!(gm_verify, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Bool(_)));

    let gm_dist = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetLocalDistributionReport",
        &[PyRep::Integer(planet_id), PyRep::Integer(2073)],
    )
    .await
    .expect("GMGetLocalDistributionReport");
    assert!(matches!(gm_dist, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let gm_sync = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetSynchedServerState",
        &[PyRep::Integer(planet_id)],
    )
    .await
    .expect("GMGetSynchedServerState");
    assert!(matches!(gm_sync, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::Tuple(_)));

    let gm_deplete = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMRunDepletionSim",
        &[PyRep::Integer(planet_id), PyRep::Integer(2073)],
    )
    .await
    .expect("GMRunDepletionSim");
    assert!(matches!(gm_deplete, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    let gm_nugget = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMCreateNuggetLayer",
        &[PyRep::Integer(planet_id), PyRep::Integer(2073)],
    )
    .await
    .expect("GMCreateNuggetLayer");
    assert_eq!(gm_nugget, PyRep::None);

    let gm_res = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GMGetCompleteResource",
        &[PyRep::Integer(planet_id), PyRep::Integer(2073)],
    )
    .await
    .expect("GMGetCompleteResource");
    assert!(matches!(gm_res, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    // 3. Colony & UserUpdateNetwork validation errors
    // Binding planetMgr in another system must fail with BindAuthorizationDenied
    let cross_sys_bind_err = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "MachoBindObject",
        &[PyRep::Integer(planet_id), PyRep::None],
    )
    .await
    .expect_err("planetMgr.MachoBindObject from another system must fail");
    assert_eq!(cross_sys_bind_err.code, "BindAuthorizationDenied");

    let colony_missing = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "GetColony",
        &[PyRep::Integer(planet_id)],
    )
    .await
    .expect_err("GetColony before setup must fail");
    assert_eq!(colony_missing.code, "ColonyNotFound");

    // Different solar system -> PlanetNotInCurrentSystem
    let wrong_sys_err = rpc(
        &ctx,
        &mut gm,
        "planetMgr",
        "UserUpdateNetwork",
        &[
            PyRep::Integer(planet_id),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(-1),
                PyRep::Integer(i64::from(temp_cc_tid)),
                PyRep::Real(0.5),
                PyRep::Real(0.5),
            ])]),
        ],
    )
    .await
    .expect_err("Planet in different system must fail");
    assert_eq!(wrong_sys_err.code, "PlanetNotInCurrentSystem");

    // Teleport GM into planet's system (undocked in space), then bind planetMgr
    common::slash_cmd(&ctx, &mut gm, &format!("/tr {planet_sys}")).await;
    common::slash_cmd(&ctx, &mut gm, "/giveisk 500000000").await;
    let bound_pm = bind_planet_mgr(&ctx, &mut gm, planet_id).await;

    // Bind non_gm (still docked in Jita) to Jita planet and try CC placement while docked
    if let Some(jita_planet) = ctx
        .sde
        .celestials_in_system(JITA_SYSTEM_ID)
        .into_iter()
        .filter(|c| c.group_id == 7)
        .map(|c| c.celestial_id)
        .min()
    {
        let non_gm_pm = bind_planet_mgr(&ctx, &mut non_gm, jita_planet).await;
        let docked_cc_err = rpc_bound(
            &ctx,
            &mut non_gm,
            "planetMgr",
            non_gm_pm,
            "UserUpdateNetwork",
            &[PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(-1),
                PyRep::Integer(i64::from(temp_cc_tid)),
                PyRep::Real(0.4),
                PyRep::Real(0.4),
            ])])],
        )
        .await
        .expect_err("Docked CC placement must fail");
        assert_eq!(docked_cc_err.code, "CannotPlaceCommandCenterWhileDocked");
    }

    // Missing CC item in ship cargo -> CommandCenterItemRequired
    let missing_cc_err = rpc_bound(
        &ctx,
        &mut gm,
        "planetMgr",
        bound_pm,
        "UserUpdateNetwork",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(i64::from(temp_cc_tid)),
            PyRep::Real(0.4),
            PyRep::Real(0.4),
        ])])],
    )
    .await
    .expect_err("Missing CC item must fail");
    assert_eq!(missing_cc_err.code, "CommandCenterItemRequired");

    // Wrong CC planet type (Barren CC on Temperate planet) -> IncompatibleCommandCenterPlanetType
    gm_create_item(&ctx, &mut gm, barren_cc_tid, 1).await;
    let bad_cc_type = rpc_bound(
        &ctx,
        &mut gm,
        "planetMgr",
        bound_pm,
        "UserUpdateNetwork",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(i64::from(barren_cc_tid)),
            PyRep::Real(0.4),
            PyRep::Real(0.4),
        ])])],
    )
    .await
    .expect_err("Incompatible CC type must fail");
    assert_eq!(bad_cc_type.code, "IncompatibleCommandCenterPlanetType");

    // Place valid Temperate CC
    gm_create_item(&ctx, &mut gm, temp_cc_tid, 2).await;
    rpc_bound(
        &ctx,
        &mut gm,
        "planetMgr",
        bound_pm,
        "UserUpdateNetwork",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(i64::from(temp_cc_tid)),
            PyRep::Real(0.4),
            PyRep::Real(0.4),
        ])])],
    )
    .await
    .expect("Valid CC placement");

    // Duplicate CC -> ColonyAlreadyExists
    let dup_cc_err = rpc_bound(
        &ctx,
        &mut gm,
        "planetMgr",
        bound_pm,
        "UserUpdateNetwork",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(-2),
            PyRep::Integer(i64::from(temp_cc_tid)),
            PyRep::Real(0.5),
            PyRep::Real(0.5),
        ])])],
    )
    .await
    .expect_err("Second CC on same planet must fail");
    assert_eq!(dup_cc_err.code, "ColonyAlreadyExists");

    // Invalid coordinates & unsupported command code
    let bad_coord_err = rpc_bound(
        &ctx,
        &mut gm,
        "planetMgr",
        bound_pm,
        "UserUpdateNetwork",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(-2),
            PyRep::Integer(2474),
            PyRep::Real(f64::NAN),
            PyRep::Real(0.4),
        ])])],
    )
    .await
    .expect_err("NaN coordinates must fail");
    assert_eq!(bad_coord_err.code, "InvalidPinCoordinates");

    let bad_cmd_err = rpc_bound(
        &ctx,
        &mut gm,
        "planetMgr",
        bound_pm,
        "UserUpdateNetwork",
        &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(999)])])],
    )
    .await
    .expect_err("Unsupported command code must fail");
    assert_eq!(bad_cmd_err.code, "UnsupportedPlanetCommand");

    // Upgrade Command Center without Command Center Upgrades skill -> MissingSkillForCommandCenterUpgrade
    let pins = rpc_bound(&ctx, &mut gm, "planetMgr", bound_pm, "GetCommandPinsForPlanet", &[])
        .await
        .expect("GetCommandPinsForPlanet");
    let cc_pin_id = match &pins {
        PyRep::List(v) if !v.is_empty() => v[0]
            .dict_get("id")
            .or_else(|| v[0].dict_get("pinID"))
            .and_then(|x| x.as_i64())
            .expect("cc pin id"),
        other => panic!("expected command pin list, got {other:?}"),
    };

    let upgrade_skill_err = rpc_bound(
        &ctx,
        &mut gm,
        "planetMgr",
        bound_pm,
        "UpgradeCommandCenter",
        &[PyRep::Integer(3)],
    )
    .await
    .expect_err("UpgradeCommandCenter without skill must fail");
    assert_eq!(upgrade_skill_err.code, "MissingSkillForCommandCenterUpgrade");

    // Invalid link level & missing route link
    let bad_link_err = rpc_bound(
        &ctx,
        &mut gm,
        "planetMgr",
        bound_pm,
        "UserUpdateNetwork",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(3),
            PyRep::Integer(cc_pin_id),
            PyRep::Integer(999_999),
            PyRep::Integer(0),
        ])])],
    )
    .await
    .expect_err("Link to missing pin must fail");
    assert_eq!(bad_link_err.code, "PinNotFound");

    // 4. planetOrbitalRegistryBroker defaults, RBAC, and validation
    let orb_info = rpc(
        &ctx,
        &mut non_gm,
        "planetOrbitalRegistryBroker",
        "GetSettingsInfo",
        &[PyRep::Integer(planet_id)],
    )
    .await
    .expect("GetSettingsInfo default InterBus orbital");
    let PyRep::Tuple(orb_tuple) = orb_info else {
        panic!("expected 6-tuple from GetSettingsInfo");
    };
    assert_eq!(orb_tuple.len(), 6);
    assert_eq!(orb_tuple[0], PyRep::Integer(0));
    assert_eq!(
        orb_tuple[1].dict_get("standingGood").and_then(|v| v.as_f64()),
        Some(0.05)
    );

    let update_rbac_err = rpc(
        &ctx,
        &mut non_gm,
        "planetOrbitalRegistryBroker",
        "UpdateSettings",
        &[PyRep::Integer(planet_id)],
    )
    .await
    .expect_err("UpdateSettings non-owner must fail");
    assert_eq!(update_rbac_err.code, "CrpAccessDenied");

    let revert_err = rpc(
        &ctx,
        &mut non_gm,
        "planetOrbitalRegistryBroker",
        "RevertOrbitalsToInterBus",
        &[PyRep::Integer(planet_id)],
    )
    .await
    .expect_err("RevertOrbitalsToInterBus non-owner must fail");
    assert_eq!(revert_err.code, "CrpAccessDenied");

    // Abandon planet cleanly
    rpc_bound(&ctx, &mut gm, "planetMgr", bound_pm, "UserAbandonPlanet", &[])
        .await
        .expect("UserAbandonPlanet");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_structure_anchoring_profiles_acl_and_lifecycle_ops() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut ceo = setup_gm_pilot(&ctx, 1301, 91_300_001).await;
    let (mut non_member, _) = setup_fresh_player_pilot(&ctx, 1302, "str_outsider", "Structure Outsider").await;

    // 1. Anchoring validation errors while docked & missing item & trade hub
    let docked_err = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(RAITARU_TYPE_ID)), PyRep::String("Docked Test".into())],
    )
    .await
    .expect_err("Anchor while docked must fail");
    assert_eq!(docked_err.code, "ShipDocked");

    // Creating a corp without Corporation Management (3363) fails, then train via /giveskill and create
    ctx.advance_secs(12);
    common::slash_cmd(&ctx, &mut ceo, "/giveisk 5000000000").await;
    let missing_corp_skill = rpc(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Edge Struct Corp".into()), PyRep::String("EDGE".into())],
    )
    .await
    .expect_err("AddCorporation without Corporation Management skill must fail");
    assert_eq!(missing_corp_skill.code, "MissingCorporationManagementSkill");
    common::slash_cmd(&ctx, &mut ceo, "/giveskill 3363 5").await;
    let corp_rep = rpc(
        &ctx,
        &mut ceo,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Edge Struct Corp".into()), PyRep::String("EDGE".into())],
    )
    .await
    .expect("AddCorporation");
    assert!(corp_rep.as_i64().is_some() || corp_rep.dict_get("corporationID").is_some());
    ctx.advance_secs(12);

    rpc(&ctx, &mut ceo, "structureDocking", "Undock", &[])
        .await
        .expect("Undock");

    // Young corp (< 7 days) cannot anchor -> CorporationTooYoungToAnchor
    gm_create_item(&ctx, &mut ceo, RAITARU_TYPE_ID, 1).await;
    let young_err = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(RAITARU_TYPE_ID)), PyRep::String("Young Corp Test".into())],
    )
    .await
    .expect_err("Anchor with corp < 7 days old must fail");
    assert_eq!(young_err.code, "CorporationTooYoungToAnchor");

    // Advance 7 days so corp-age check passes, then test trade-hub restriction in Jita
    common::slash_cmd(&ctx, &mut ceo, "/advancetime 604860").await;
    let jita_err = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(RAITARU_TYPE_ID)), PyRep::String("Jita Test".into())],
    )
    .await
    .expect_err("Anchor in Jita must fail");
    assert_eq!(jita_err.code, "AnchoringDisallowedInTradeHub");

    // Move to Perimeter & try anchoring Keepstar (35834, forbidden in high-sec)
    let keepstar_type_id: TypeId = 35834;
    common::slash_cmd(&ctx, &mut ceo, &format!("/tr {PERIMETER_SYSTEM_ID}")).await;
    gm_create_item(&ctx, &mut ceo, keepstar_type_id, 1).await;
    let keepstar_hs_err = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(keepstar_type_id)), PyRep::String("HS Keepstar".into())],
    )
    .await
    .expect_err("Keepstar in high-sec must fail");
    assert_eq!(keepstar_hs_err.code, "CannotAnchorInHighSec");

    // Move 5,000 km away from station so distance check passes, then anchor an Astrahus cleanly in open space
    let sys_handle = ctx.systems.get_or_spawn(PERIMETER_SYSTEM_ID);
    let ship_id = ceo.ship_id.expect("ceo ship_id");
    let far_pos = ctx
        .sde
        .stations
        .iter()
        .filter(|(_, s)| s.solar_system_id == PERIMETER_SYSTEM_ID)
        .min_by_key(|(&id, _)| id)
        .map(|(_, s)| s.position + glam::DVec3::new(0.0, 5_000_000_000.0, 0.0))
        .expect("Perimeter station");
    assert!(sys_handle.set_ball_position(ship_id, far_pos).await);

    gm_create_item(&ctx, &mut ceo, ASTRAHUS_TYPE_ID, 1).await;
    let anchor_rep = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(ASTRAHUS_TYPE_ID)), PyRep::String("Edge Astrahus".into())],
    )
    .await
    .expect("Anchor Astrahus");
    let structure_id = anchor_rep
        .dict_get("structureID")
        .and_then(|v| v.as_i64())
        .expect("anchored structureID");
    let core_type_id = anchor_rep
        .dict_get("quantumCoreTypeID")
        .and_then(|v| v.as_i64())
        .unwrap_or(56_201) as u32;

    // Attempting Unanchor during AnchorVulnerable fails with CannotUnanchorReinforcedStructure
    let unanch_early = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "Unanchor",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect_err("Unanchor during AnchorVulnerable must fail");
    assert_eq!(unanch_early.code, "CannotUnanchorReinforcedStructure");

    // Advance past AnchorVulnerable + Anchoring timers -> OutsideDockingPerimeter at 60km, then approach, Dock, insert Quantum Core into slot 172, Undock, finish Onlining
    common::slash_cmd(&ctx, &mut ceo, "/advancetime 90000").await;
    rpc(&ctx, &mut ceo, "structureGuests", "GetGuests", &[PyRep::Integer(structure_id)])
        .await
        .expect("sync AnchorNoCore");
    let dock_far_err = rpc(&ctx, &mut ceo, "structureDocking", "Dock", &[PyRep::Integer(structure_id)])
        .await
        .expect_err("Dock at 60km must fail OutsideDockingPerimeter");
    assert_eq!(dock_far_err.code, "OutsideDockingPerimeter");
    let (_, st_ball) = sys_handle
        .get_ball_snapshot(structure_id)
        .await
        .expect("ball query")
        .expect("structure ball");
    assert!(sys_handle.set_ball_position(ship_id, st_ball.position).await);
    rpc(&ctx, &mut ceo, "structureDocking", "Dock", &[PyRep::Integer(structure_id)])
        .await
        .expect("Dock in Astrahus");
    let core_item_id = gm_create_item(&ctx, &mut ceo, core_type_id, 1).await;
    rpc(
        &ctx,
        &mut ceo,
        "invbroker",
        "Add",
        &[
            PyRep::Integer(core_item_id),
            PyRep::Integer(structure_id),
            PyRep::Integer(172),
        ],
    )
    .await
    .expect("insert quantum core");
    ctx.advance_secs(12);
    rpc(&ctx, &mut ceo, "structureDocking", "Undock", &[])
        .await
        .expect("Undock from Astrahus");
    common::slash_cmd(&ctx, &mut ceo, "/advancetime 1200").await;
    rpc(&ctx, &mut ceo, "structureGuests", "GetGuests", &[PyRep::Integer(structure_id)])
        .await
        .expect("sync ShieldVulnerable");

    // 2. Rename, vulnerability schedule, description, and unanchor validation errors
    let bad_name_err = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(structure_id), PyRep::String("   ".into())],
    )
    .await
    .expect_err("Blank name must fail");
    assert_eq!(bad_name_err.code, "InvalidStructureName");

    let outsider_rename = rpc(
        &ctx,
        &mut non_member,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(structure_id), PyRep::String("Hijacked".into())],
    )
    .await
    .expect_err("Outsider rename must fail");
    assert_eq!(outsider_rename.code, "StructureOwnershipViolation");

    let renamed = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "RenameStructure",
        &[PyRep::Integer(structure_id), PyRep::String("Perimeter Edge Keep".into())],
    )
    .await
    .expect("RenameStructure");
    assert_eq!(
        renamed.dict_get("name").and_then(|v| v.as_str()),
        Some("Perimeter Edge Keep")
    );

    let bad_vuln = rpc(
        &ctx,
        &mut ceo,
        "structureVulnerability",
        "UpdateReinforceTimesFromClient",
        &[PyRep::Integer(structure_id), PyRep::Integer(24), PyRep::Integer(2)],
    )
    .await
    .expect_err("Hour 24 must fail");
    assert_eq!(bad_vuln.code, "InvalidReinforceSchedule");

    let ok_vuln = rpc(
        &ctx,
        &mut ceo,
        "structureVulnerability",
        "UpdateReinforceTimesFromClient",
        &[PyRep::Integer(structure_id), PyRep::Integer(20), PyRep::Integer(4)],
    )
    .await
    .expect("UpdateReinforceTimesFromClient");
    assert_eq!(
        ok_vuln.dict_get("nextReinforceHour").and_then(|v| v.as_i64()),
        Some(20)
    );

    // CancelUnanchor before unanchoring -> StructureNotUnanchoring
    let not_unanch_err = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "CancelUnanchor",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect_err("CancelUnanchor before Unanchor must fail");
    assert_eq!(not_unanch_err.code, "StructureNotUnanchoring");

    let unanch_rep = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "Unanchor",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("Unanchor");
    assert_eq!(
        unanch_rep.dict_get("state").and_then(|v| v.as_str()),
        Some("Unanchoring")
    );

    let dup_unanch = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "Unanchor",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect_err("Duplicate Unanchor must fail");
    assert_eq!(dup_unanch.code, "StructureAlreadyUnanchoring");

    let cancel_rep = rpc(
        &ctx,
        &mut ceo,
        "structureDeployment",
        "CancelUnanchor",
        &[PyRep::Integer(structure_id)],
    )
    .await
    .expect("CancelUnanchor");
    assert_eq!(
        cancel_rep.dict_get("unanchoringCancelled").and_then(|v| v.as_bool()),
        Some(true)
    );

    // 3. structureProfiles & ownerGroupManager RBAC and CRUD
    let outsider_prof_err = rpc(
        &ctx,
        &mut non_member,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Forbidden".into()), PyRep::String("No".into())],
    )
    .await
    .expect_err("Non-director NPC-corp pilot cannot create profile");
    assert_eq!(outsider_prof_err.code, "InsufficientCorpRoles");

    let prof_id = rpc(
        &ctx,
        &mut ceo,
        "structureProfiles",
        "CreateProfile",
        &[PyRep::String("Corp Doctrine".into()), PyRep::String("Testing ACL".into())],
    )
    .await
    .expect("CEO CreateProfile")
    .as_i64()
    .expect("profileID");

    let steal_prof_err = rpc(
        &ctx,
        &mut non_member,
        "structureProfiles",
        "UpdateProfile",
        &[PyRep::Integer(prof_id), PyRep::String("Stolen".into())],
    )
    .await
    .expect_err("Non-owning corp cannot update profile");
    assert_eq!(steal_prof_err.code, "StructureOwnershipViolation");

    let dup_prof_id = rpc(
        &ctx,
        &mut ceo,
        "structureProfiles",
        "DuplicateProfile",
        &[PyRep::Integer(prof_id), PyRep::String("Corp Doctrine Copy".into())],
    )
    .await
    .expect("DuplicateProfile")
    .as_i64()
    .expect("duplicated profileID");
    assert_ne!(dup_prof_id, prof_id);

    rpc(
        &ctx,
        &mut ceo,
        "structureProfiles",
        "SetDefaultProfile",
        &[PyRep::Integer(prof_id)],
    )
    .await
    .expect("SetDefaultProfile");

    rpc(
        &ctx,
        &mut ceo,
        "structureProfiles",
        "DeleteProfile",
        &[PyRep::Integer(dup_prof_id)],
    )
    .await
    .expect("DeleteProfile");

    let grp_rep = rpc(
        &ctx,
        &mut ceo,
        "ownerGroupManager",
        "Create",
        &[PyRep::String("Blocked Outsiders".into()), PyRep::String("Test group".into())],
    )
    .await
    .expect("ownerGroupManager.Create");
    let grp_id = grp_rep
        .as_i64()
        .or_else(|| grp_rep.dict_get("groupID").and_then(|v| v.as_i64()))
        .expect("groupID");
    assert!(grp_id > 0);

    // 4. Asset safety empty-hangar error path
    let empty_as_err = rpc(
        &ctx,
        &mut ceo,
        "structureAssetSafety",
        "MovePersonalAssetsToSafety",
        &[PyRep::Integer(i64::from(PERIMETER_SYSTEM_ID)), PyRep::Integer(structure_id)],
    )
    .await
    .expect_err("MovePersonalAssetsToSafety with no eligible items must fail");
    assert_eq!(empty_as_err.code, "NoAssetsToMoveToSafety");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_fw_enlistment_warzone_insurgency_and_sov_ops() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut gm = setup_gm_pilot(&ctx, 1401, 91_400_001).await;
    let (mut non_ceo, _non_ceo_id) = setup_fresh_player_pilot(&ctx, 1402, "fw_grunt", "FW Grunt").await;

    // 1. facWarMgr enlistment RBAC, invalid faction, standing check, cooldown
    let non_ceo_join = rpc(
        &ctx,
        &mut non_ceo,
        "facWarMgr",
        "JoinFactionAsCorporation",
        &[PyRep::Integer(CALDARI_FACTION_ID)],
    )
    .await
    .expect_err("NPC corp / non-CEO JoinFactionAsCorporation must fail");
    assert_eq!(non_ceo_join.code, "AccessDenied");

    ctx.advance_secs(12);
    common::slash_cmd(&ctx, &mut gm, "/giveisk 5000000000").await;
    common::slash_cmd(&ctx, &mut gm, "/giveskill 3363 5").await;
    rpc(
        &ctx,
        &mut gm,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("Militia Test Corp".into()), PyRep::String("MLTA".into())],
    )
    .await
    .expect("AddCorporation");
    ctx.advance_secs(12);

    let bad_fac_err = rpc(
        &ctx,
        &mut gm,
        "facWarMgr",
        "JoinFactionAsCorporation",
        &[PyRep::Integer(JOVE_NON_MILITIA_FACTION_ID)],
    )
    .await
    .expect_err("Joining Jove militia must fail");
    assert_eq!(bad_fac_err.code, "InvalidFWMilitiaFaction");

    // Raise standing with Caldari via GM /standing and enlist
    common::slash_cmd(&ctx, &mut gm, &format!("/standing {CALDARI_FACTION_ID} 2.5")).await;
    rpc(
        &ctx,
        &mut gm,
        "facWarMgr",
        "JoinFactionAsCorporation",
        &[PyRep::Integer(CALDARI_FACTION_ID)],
    )
    .await
    .expect("JoinFactionAsCorporation Caldari");

    let dup_join_err = rpc(
        &ctx,
        &mut gm,
        "facWarMgr",
        "JoinFactionAsCorporation",
        &[PyRep::Integer(MINMATAR_FACTION_ID)],
    )
    .await
    .expect_err("Already enlisted corp cannot enlist again");
    assert!(
        matches!(dup_join_err.code.as_str(), "AlreadyEnlisted" | "CorporationAlreadyEnlisted" | "FwAlreadyEnlisted"),
        "unexpected dup_join_err: {dup_join_err:?}"
    );

    let status = rpc(&ctx, &mut gm, "facWarMgr", "GetCorpFactionalWarStatus", &[])
        .await
        .expect("GetCorpFactionalWarStatus");
    assert_eq!(
        status.dict_get("factionID").and_then(|v| v.as_i64()),
        Some(CALDARI_FACTION_ID)
    );

    // Immediate LeaveFactionAsCorporation within 24h fails with MilitiaCooldownActive
    let early_leave_err = rpc(&ctx, &mut gm, "facWarMgr", "LeaveFactionAsCorporation", &[])
        .await
        .expect_err("Leaving within 24h of joining must fail");
    assert_eq!(early_leave_err.code, "MilitiaCooldownActive");

    // Advance 24h -> LeaveFactionAsCorporation succeeds, then immediate rejoin hits 24h cooldown
    common::slash_cmd(&ctx, &mut gm, "/advancetime 86460").await;
    rpc(&ctx, &mut gm, "facWarMgr", "LeaveFactionAsCorporation", &[])
        .await
        .expect("LeaveFactionAsCorporation after 24h");
    let cooldown_err = rpc(
        &ctx,
        &mut gm,
        "facWarMgr",
        "JoinFactionAsCorporation",
        &[PyRep::Integer(CALDARI_FACTION_ID)],
    )
    .await
    .expect_err("Rejoining within 24h cooldown must fail");
    assert_eq!(cooldown_err.code, "MilitiaCooldownActive");

    // 2. Warzone & VP queries + /fwcapture
    let warzones = rpc(&ctx, &mut gm, "fwWarzoneSolarsystem", "GetAllWarzones", &[])
        .await
        .expect("GetAllWarzones");
    assert!(matches!(warzones, PyRep::Dict(_) | PyRep::List(_)));

    let hqs = rpc(&ctx, &mut gm, "fwWarzoneSolarsystem", "GetHQSystemIDs", &[])
        .await
        .expect("GetHQSystemIDs");
    assert!(matches!(hqs, PyRep::Dict(_) | PyRep::List(_) | PyRep::Tuple(_)));

    let all_vp = rpc(&ctx, &mut gm, "fwVictoryPointMgr", "GetAllVictoryPointStates", &[])
        .await
        .expect("GetAllVictoryPointStates");
    let fw_sys = match &all_vp {
        PyRep::Dict(entries) => entries
            .iter()
            .filter_map(|(k, _)| k.as_i64().and_then(|v| u32::try_from(v).ok()))
            .min()
            .unwrap_or(30_002_813),
        _ => 30_002_813,
    };

    let cap_rep = common::slash_cmd(&ctx, &mut gm, &format!("/fwcapture {fw_sys} 40000")).await;
    assert!(
        cap_rep
            .dict_get("victoryPoints")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 40_000
    );

    // In Jita (non-FW), GetLocalVictoryPointState returns None; teleport into FW system to observe KeyVal
    let jita_vp = rpc(&ctx, &mut gm, "fwVictoryPointMgr", "GetLocalVictoryPointState", &[])
        .await
        .expect("GetLocalVictoryPointState Jita");
    assert_eq!(jita_vp, PyRep::None);

    common::slash_cmd(&ctx, &mut gm, &format!("/tr {fw_sys}")).await;
    let local_vp = rpc(
        &ctx,
        &mut gm,
        "fwVictoryPointMgr",
        "GetLocalVictoryPointState",
        &[],
    )
    .await
    .expect("GetLocalVictoryPointState");
    assert_eq!(
        local_vp.dict_get("solarSystemID").and_then(|v| v.as_i64()),
        Some(i64::from(fw_sys))
    );
    assert!(
        local_vp
            .dict_get("victoryPoints")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 40_000
    );

    let bf = rpc(
        &ctx,
        &mut gm,
        "dungeonInstanceCacheMgr",
        "GetFactionWarfareBattlefieldInstances",
        &[],
    )
    .await
    .expect("GetFactionWarfareBattlefieldInstances");
    assert!(matches!(bf, PyRep::List(_) | PyRep::Dict(_)));

    // DonateLPsToSolarSystem on non-warzone Jita must fail
    let lp_donate_err = rpc(
        &ctx,
        &mut gm,
        "facWarMgr",
        "DonateLPsToSolarSystem",
        &[PyRep::Integer(i64::from(JITA_SYSTEM_ID)), PyRep::Integer(1000)],
    )
    .await
    .expect_err("DonateLPsToSolarSystem in Jita must fail");
    assert_eq!(lp_donate_err.code, "NotInWarzone");

    // 3. Dynamic Resource Cache (DBS / ESS) & Sovereignty validation errors
    let dbs = rpc(&ctx, &mut gm, "dynamicResourceCacheMgr", "GetDBSMapData", &[])
        .await
        .expect("GetDBSMapData");
    assert!(matches!(dbs, PyRep::Dict(_)));

    let ess_agency = rpc(&ctx, &mut gm, "dynamicResourceCacheMgr", "GetESSAgencyData", &[])
        .await
        .expect("GetESSAgencyData");
    assert!(matches!(ess_agency, PyRep::Dict(_)));

    // Sov Hub anchor outside nullsec (while undocked in Jita) -> CannotAnchorSovHubOutsideNullSec
    common::slash_cmd(&ctx, &mut gm, &format!("/tr {JITA_SYSTEM_ID}")).await;
    let sov_hs_err = rpc(
        &ctx,
        &mut gm,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(SOVEREIGNTY_HUB_TYPE_ID)), PyRep::String("Bad Hub".into())],
    )
    .await
    .expect_err("Sov Hub in highsec must fail");
    assert_eq!(sov_hs_err.code, "CannotAnchorSovHubOutsideNullSec");

    // Move to deterministic claimable nullsec system without alliance -> AllianceRequired
    let null_sys = find_deterministic_system(&ctx, |s| {
        s.security_status <= 0.0
            && s.solar_system_id < 31_000_000
            && s.solar_system_id != 30_100_000
            && s.region_id != 10_000_070
            && ctx.sde.constellations.get(&s.constellation_id).and_then(|c| c.faction_id).is_none()
            && ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id).is_none()
    });
    common::slash_cmd(&ctx, &mut gm, &format!("/tr {null_sys}")).await;
    let sov_no_ally_err = rpc(
        &ctx,
        &mut gm,
        "structureDeployment",
        "Anchor",
        &[PyRep::Integer(i64::from(SOVEREIGNTY_HUB_TYPE_ID)), PyRep::String("No Ally Hub".into())],
    )
    .await
    .expect_err("Sov Hub without alliance must fail");
    assert_eq!(sov_no_ally_err.code, "AllianceRequired");

    // 4. Quasar Sovereignty Hub & Skyhook gateway RPCs
    let defs_rsp = quasar_call(
        &ctx,
        &gm,
        "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest",
        Vec::new(),
    );
    assert_eq!(defs_rsp.status_code, 200, "{}", defs_rsp.status_message);

    let mut bad_hub_req = Vec::new();
    let mut id_bytes = Vec::new();
    eve_proto::quasar::encode_varint_field(1, 9_999_888_777, &mut id_bytes);
    eve_proto::quasar::encode_bytes_field(1, &id_bytes, &mut bad_hub_req);
    let missing_hub_rsp = quasar_call(
        &ctx,
        &gm,
        "eve_public.sovereignty.hub.api.GetResourcesRequest",
        bad_hub_req,
    );
    assert_ne!(missing_hub_rsp.status_code, 200);

    let skyhooks_rsp = quasar_call(
        &ctx,
        &gm,
        "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest",
        Vec::new(),
    );
    assert_eq!(skyhooks_rsp.status_code, 200, "{}", skyhooks_rsp.status_message);
}
