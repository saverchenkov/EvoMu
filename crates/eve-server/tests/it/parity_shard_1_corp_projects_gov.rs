//! Live non-GM end-to-end tests for Parity Shard 1 Corporation Projects & Governance ESI:
//! - Multi-method Quasar Corp Projects (`DeliverItem`, `ManufactureItems`, `DestroyNPCShips`, `EarnLoyaltyPoints`)
//!   driven by real MachoNet gameplay (no `with_mut_snapshot`, no `record_server_project_activity`, no `/slash`)
//! - `GET /v1/corporations/{corporation_id}/projects/` (`feat:esi.corporation.projects`)
//! - Role-gated `SetStructureReinforceDefault` + new Raitaru adoption + `corpmgr.AuditMember` timestamps/issuer
//! - `GET /members/limit`, `/roles`, `/roles/history`, `/blueprints`, `/structures` + SQLite restart reload

#![forbid(unsafe_code)]

use crate::common;
use crate::corp_member_audit_reinforce_and_quit::found_corp_with_member;
use crate::fleet_boss_formations_corp_projects_live::{build_project_create_msg, quasar_call};
use crate::industry_live_support::{
    acquire, board_loaded_freighter, bom, buy, deploy_online_upwell, dock_station, fuel_cargo,
    fund_pilots, install, int, items_at, login, move_item, nearest_system, rpc, train_skills,
    travel, upwell_cargo, warp_to, JITA_STATION, PERIMETER_SYS, PILOT_1, PILOT_2,
};
use crate::parity_shard_1_support::{
    assert_etag_304, cleanup_sqlite_files, destroy_target_live, esi_call, issue_token, reload_repo,
    temp_db_path,
};
use eve_db::corp_roles;
use eve_machonet::SessionState;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_proto::PyRep;
use eve_server::esi::{EsiRouter, OpenApiSpec};
use eve_server::quasar_gateway::STATUS_FORBIDDEN;
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::load_mission_meta;
use eve_server::services::social_corp_fleet::social_corp_projects::goal_uuid_to_project_id;
use eve_server::services::station_structure::structure_lifecycle;
use eve_server::world_seeder::seed_full_market_catalog;
use serde_json::json;
use std::sync::Arc;

const RIFTER_BP: u32 = 691;
const RIFTER_PRODUCT: u32 = 587;
const TRITANIUM: u32 = 34;
const PYERITE: u32 = 35;
const RAITARU: u32 = 35_825;
const MFG_PLANT: u32 = 35_878;
const RESEARCH_LAB: u32 = 35_891;
const SMALL_AUTOCANNON: u32 = 486;

async fn complete_job(ctx: &ServerContext, sess: &mut SessionState, job: &PyRep) -> PyRep {
    let dur = int(job, "durationSeconds") as u64;
    ctx.advance_virtual_clock(dur + 2);
    rpc(ctx, sess, "industryManager", "CompleteJob", &[PyRep::Integer(int(job, "jobID"))])
        .await
        .expect("CompleteJob")
}

#[tokio::test]
async fn test_shard1_live_corp_projects_methods_audit_and_reinforce_default() {
    // parity: feat:corporation.corp-project-contribution-methods, feat:esi.corporation.projects
    // parity: feat:corporation.member-audit, feat:corporation.structure-reinforce-default
    eve_testkit::attest(&[
        "feat:corporation.corp-project-contribution-methods",
        "feat:esi.corporation.projects",
        "feat:corporation.member-audit",
        "feat:corporation.structure-reinforce-default",
    ]);

    let db_path = temp_db_path("corp_projects_gov");
    let corp_id;
    let pid_deliv;
    let pid_mfg;
    let pid_kill;
    let pid_lp;
    let raitaru_id;

    {
        let repo = reload_repo(&db_path);
        let ctx = Arc::new(ServerContext::with_repository(repo));
        common::pin_clock(&ctx);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 60_000_000_000.0), (PILOT_2, 2_000_000_000.0)]).await;
        let mut s1 = login(&ctx, 9421, "demo", PILOT_1).await;
        let mut s2 = login(&ctx, 9422, "other", PILOT_2).await;

        train_skills(
            &ctx,
            &mut s1,
            &[3300, 3301, 3380, 3387, 20_185, RAITARU, MFG_PLANT, SMALL_AUTOCANNON, RIFTER_BP],
            true,
        )
        .await;

        corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "PRJS").await;
        ctx.advance_virtual_clock(8 * 86_400);

        // 1. Structure Reinforce Default: non-director rejected; Director sets 19:00; newly anchored Raitaru adopts 19
        let denied = rpc(&ctx, &mut s2, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(19)])
            .await
            .expect_err("Non-director must be rejected");
        assert_eq!(denied.code, "CorpRoleDenied");

        rpc(&ctx, &mut s1, "corpRegistry", "SetStructureReinforceDefault", &[PyRep::Integer(19)])
            .await
            .expect("Director sets reinforce hour 19");
        assert_eq!(
            rpc(&ctx, &mut s2, "corpRegistry", "GetStructureReinforceDefault", &[])
                .await
                .expect("GetStructureReinforceDefault")
                .as_i64(),
            Some(19)
        );

        // 2. Member Audit: grant ROLE_STATION_MANAGER and verify issuer, target, roles & timestamps via AuditMember
        rpc(
            &ctx,
            &mut s1,
            "corpRegistry",
            "UpdateMember",
            &[
                PyRep::Integer(PILOT_2),
                PyRep::Integer(corp_roles::ROLE_STATION_MANAGER as i64),
            ],
        )
        .await
        .expect("Grant ROLE_STATION_MANAGER");

        let audit_res = rpc(&ctx, &mut s1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)])
            .await
            .expect("corpmgr.AuditMember");
        let PyRep::Rowset { columns, rows } = audit_res else {
            panic!("AuditMember must return Rowset");
        };
        let col = |name: &str| {
            columns
                .iter()
                .position(|c| c == name)
                .unwrap_or_else(|| panic!("missing column {name}"))
        };
        assert!(
            rows.iter().any(|r| {
                r[col("characterID")].as_i64() == Some(PILOT_2)
                    && r[col("issuerID")].as_i64() == Some(PILOT_1)
                    && r[col("changeTime")].as_i64().unwrap_or(0) > 0
                    && r[col("newRoles")].as_i64() == Some(corp_roles::ROLE_STATION_MANAGER as i64)
            }),
            "AuditMember rowset must record target, issuer, timestamp, and new role bitmask"
        );

        // 3. Anti-spoof: client cannot directly mutate project progress via SetCurrentProgressRequest
        let mut spoof_req = Vec::new();
        let mut fake_uuid = Vec::new();
        encode_bytes_field(1, &[0xCC; 16], &mut fake_uuid);
        encode_bytes_field(1, &fake_uuid, &mut spoof_req);
        encode_varint_field(3, 50, &mut spoof_req);
        assert_eq!(
            quasar_call(&ctx, &s2, "eve_public.goal.api.SetCurrentProgressRequest", spoof_req).status_code,
            STATUS_FORBIDDEN
        );

        // 4. Create 4 Quasar projects covering DeliverItem, ManufactureItems, DestroyNPCShips, and EarnLoyaltyPoints
        let deliv_rsp = quasar_call(
            &ctx,
            &s1,
            "eve_public.goal.api.CreateRequest",
            build_project_create_msg("Deliver Jita Tritanium", 250, "DeliverItem", TRITANIUM as i64),
        );
        pid_deliv = goal_uuid_to_project_id(&deliv_rsp.payload.unwrap().value[4..20]).unwrap();

        let mfg_rsp = quasar_call(
            &ctx,
            &s1,
            "eve_public.goal.api.CreateRequest",
            build_project_create_msg("Manufacture Rifter Frigate", 1, "ManufactureItems", RIFTER_PRODUCT as i64),
        );
        pid_mfg = goal_uuid_to_project_id(&mfg_rsp.payload.unwrap().value[4..20]).unwrap();

        let fw_sys = nearest_system(&ctx, PERIMETER_SYS, |sys| {
            ctx.db.with_snapshot(|s| s.fw_system_occupations.contains_key(&sys))
        });
        ctx.systems.get_or_spawn(fw_sys);
        eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
        let kill_rsp = quasar_call(
            &ctx,
            &s1,
            "eve_public.goal.api.CreateRequest",
            build_project_create_msg("Clear Warzone Pirates", 1, "DestroyNPCShips", i64::from(fw_sys)),
        );
        pid_kill = goal_uuid_to_project_id(&kill_rsp.payload.unwrap().value[4..20]).unwrap();

        let mut lp_req = Vec::new();
        let mut lp_cfg = Vec::new();
        let mut npc_corp_id = Vec::new();
        encode_varint_field(1, 1_000_035, &mut npc_corp_id);
        let mut lp_sub = Vec::new();
        encode_bytes_field(1, &npc_corp_id, &mut lp_sub);
        encode_string_field_local(1, "Earn Caldari Loyalty Points", &mut lp_req);
        encode_varint_field(3, 1, &mut lp_req);
        encode_bytes_field(29, &lp_sub, &mut lp_cfg);
        encode_bytes_field(4, &lp_cfg, &mut lp_req);
        let lp_rsp = quasar_call(&ctx, &s1, "eve_public.goal.api.CreateRequest", lp_req);
        pid_lp = goal_uuid_to_project_id(&lp_rsp.payload.unwrap().value[4..20]).unwrap();

        // A) DeliverItem: wrong item ignored, 250 Tritanium completes DeliverItem project
        let wrong_item = buy(&ctx, &mut s1, PYERITE, 10).await[0];
        rpc(&ctx, &mut s1, "invbroker", "DeliverToCorpHangar", &[PyRep::Integer(wrong_item), PyRep::Integer(1)])
            .await
            .unwrap();
        assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_deliv].current_quantity), 0);

        let trit_item = buy(&ctx, &mut s1, TRITANIUM, 250).await[0];
        rpc(&ctx, &mut s1, "invbroker", "DeliverToCorpHangar", &[PyRep::Integer(trit_item), PyRep::Integer(1)])
            .await
            .expect("Deliver 250 Tritanium");
        assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_deliv].current_quantity), 250);

        // B) ManufactureItems: buy Rifter BPO + BOM, install & complete Rifter job via live RPCs
        let bpo = buy(&ctx, &mut s1, RIFTER_BP, 1).await[0];
        for (m_tid, m_qty) in bom(&ctx, RIFTER_BP, 1) {
            acquire(&ctx, &mut s1, m_tid, m_qty).await;
        }
        let mfg_job = install(&ctx, &mut s1, bpo, JITA_STATION, 1, 1, &[]).await.expect("Install Rifter job");
        complete_job(&ctx, &mut s1, &mfg_job).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_mfg].current_quantity), 1);

        // C) Anchor Raitaru in Perimeter (verifies adopt default reinforceHour == 19!)
        let mut cargo = upwell_cargo(&ctx, RAITARU, &[MFG_PLANT]);
        cargo.push((SMALL_AUTOCANNON, 2));
        cargo.extend(fuel_cargo());
        let fr = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
        let gun_ids = items_at(&ctx, PILOT_1, fr, SMALL_AUTOCANNON);
        move_item(&ctx, &mut s1, gun_ids[0].0, JITA_STATION, 4).await;
        let (rid, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[MFG_PLANT], 1_500).await;
        raitaru_id = rid;
        let st_info = rpc(&ctx, &mut s1, "structureDirectory", "GetStructureInfo", &[PyRep::Integer(raitaru_id)])
            .await
            .expect("GetStructureInfo");
        assert_eq!(st_info.dict_get("reinforceHour").and_then(|v| v.as_i64()), Some(19));

        // D) DestroyNPCShips + EarnLoyaltyPoints: accept & run a Caldari L1 agent mission in space!
        dock_station(&ctx, &mut s1, JITA_STATION).await;
        train_skills(&ctx, &mut s1, &[3300, 3301, 3303, 3306, 3327, 11_083, 3413, 3426], true).await;
        let corvette = buy(&ctx, &mut s1, 587, 1).await[0];
        rpc(&ctx, &mut s1, "ship", "AssembleShip", &[PyRep::Integer(corvette)]).await.unwrap();
        rpc(&ctx, &mut s1, "ship", "Board", &[PyRep::Integer(corvette)]).await.unwrap();
        let turret = buy(&ctx, &mut s1, 2_881, 1).await[0];
        rpc(&ctx, &mut s1, "invbroker", "Add", &[PyRep::Integer(turret), PyRep::Integer(corvette), PyRep::Integer(27)])
            .await
            .unwrap();
        let emp_ammo = buy(&ctx, &mut s1, 185, 250).await[0];
        move_item(&ctx, &mut s1, emp_ammo, corvette, 5).await;
        rpc(&ctx, &mut s1, "dogmaIM", "LoadAmmo", &[PyRep::Integer(corvette), PyRep::List(vec![PyRep::Integer(turret)]), PyRep::List(vec![PyRep::Integer(emp_ammo)])])
            .await
            .expect("LoadAmmo on Rifter");

        let (&agent_id, agent_st) = ctx
            .sde
            .agents
            .iter()
            .filter(|(_, a)| {
                a.level == 1
                    && a.division_id == 22
                    && !matches!(a.agent_type_id, 4 | 5 | 7 | 8 | 10 | 12 | 13)
                    && a.station_id.is_some_and(|st| {
                        ctx.sde.stations.get(&st).is_some_and(|s| {
                            s.region_id == 10_000_002
                                && ctx.sde.solar_systems.get(&s.solar_system_id).is_some_and(|sys| sys.security_status >= 0.5)
                        })
                    })
            })
            .min_by_key(|(id, _)| *id)
            .map(|(id, a)| (id, a.station_id.unwrap()))
            .expect("High-sec Courier agent");
        if s1.station_id != Some(agent_st) {
            dock_station(&ctx, &mut s1, agent_st).await;
        }
        let offer = rpc(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
            .await
            .expect("Request mission");
        let mid = int(&offer, "missionID");
        rpc(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid)])
            .await
            .expect("Accept mission");
        if let Some(meta) = ctx.db.with_snapshot(|s| load_mission_meta(s, PILOT_1, mid as u32)) {
            if let Some(cid) = meta.granted_courier_item_id {
                move_item(&ctx, &mut s1, cid, corvette, 5).await;
            }
            if let Some(dest) = meta.destination_station_id.filter(|&d| d > 0 && d != s1.station_id.unwrap_or(0)) {
                dock_station(&ctx, &mut s1, dest).await;
            }
        }
        rpc(&ctx, &mut s1, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid)])
            .await
            .expect("Complete mission -> EarnLoyaltyPoints");
        assert!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_lp].current_quantity) >= 1);

        // Travel to FW warzone complex gate, activate acceleration gate, and destroy spawned NPC guard in live combat
        travel(&ctx, &mut s1, fw_sys).await;
        let _sys_h = ctx.systems.get_or_spawn(fw_sys);
        let gate_id = ctx.db.with_snapshot(|s| {
            let (&gid, _) = s
                .fw_runtime
                .fw_complexes
                .iter()
                .find(|(_, c)| c.0 == fw_sys && c.1 == "Novice" && !c.4)
                .unwrap();
            gid
        });
        warp_to(&ctx, &mut s1, gate_id, 0.0).await;
        rpc(&ctx, &mut s1, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(gate_id)])
            .await
            .expect("ActivateAccelerationGate");
        destroy_target_live(&ctx, &mut s1, fw_sys, corvette, &[turret], gate_id + 500_000_000).await;
        assert_eq!(ctx.db.with_snapshot(|s| s.corp_projects[&pid_kill].current_quantity), 1);

        // Query ESI GET /v1/corporations/{corp_id}/projects/
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();
        let tok_member = issue_token(&ctx, PILOT_2, &["esi-corporations.read_projects.v1"]);
        let proj_path = format!("/v1/corporations/{corp_id}/projects/");
        assert_etag_304(&router, &spec, &proj_path, Some(&tok_member));
        let (projs, _) = esi_call(&router, &spec, "GET", &proj_path, Some(&tok_member), None, 200);
        let arr = projs.as_array().unwrap();
        assert_eq!(arr.len(), 4);
        assert!(arr.iter().any(|p| p["project_id"] == pid_deliv && p["current_quantity"] == 250 && p["target_quantity"] == 250));
        assert!(arr.iter().any(|p| p["project_id"] == pid_mfg && p["current_quantity"] == 1 && p["target_quantity"] == 1));
        assert!(arr.iter().any(|p| p["project_id"] == pid_kill && p["current_quantity"] == 1 && p["target_quantity"] == 1));
        assert!(arr.iter().any(|p| p["project_id"] == pid_lp && p["current_quantity"] == 1 && p["target_quantity"] == 1));
    }

    // SQLite persistence check across server restart
    {
        let repo2 = reload_repo(&db_path);
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(Arc::clone(&ctx2));
        let spec2 = OpenApiSpec::pinned();
        let tok2 = issue_token(&ctx2, PILOT_2, &["esi-corporations.read_projects.v1", "esi-corporations.read_structures.v1"]);

        let (projs2, _) = esi_call(&router2, &spec2, "GET", &format!("/v1/corporations/{corp_id}/projects/"), Some(&tok2), None, 200);
        assert_eq!(projs2.as_array().unwrap().len(), 4);

        let (structs2, _) = esi_call(&router2, &spec2, "GET", &format!("/v1/corporations/{corp_id}/structures/"), Some(&tok2), None, 200);
        let st = structs2.as_array().unwrap().iter().find(|s| s["structure_id"] == raitaru_id).unwrap();
        assert_eq!(st["reinforce_hour"], json!(19));
    }

    cleanup_sqlite_files(&db_path);
}

fn encode_string_field_local(field_num: u32, s: &str, buf: &mut Vec<u8>) {
    encode_bytes_field(field_num, s.as_bytes(), buf);
}

#[tokio::test]
async fn test_shard1_live_corp_governance_members_roles_bps_and_structures() {
    // parity: esi:GET /corporations/{corporation_id}/members/limit, feat:esi.corporation.members
    // parity: esi:GET /corporations/{corporation_id}/roles, esi:GET /corporations/{corporation_id}/roles/history
    // parity: feat:esi.corporation.roles, esi:GET /corporations/{corporation_id}/blueprints
    // parity: feat:esi.corporation.blueprints, esi:GET /corporations/{corporation_id}/structures
    // parity: feat:esi.corporation.structures, feat:esi.corporations.structures-services-timers
    eve_testkit::attest(&[
        "esi:GET /corporations/{corporation_id}/members/limit",
        "feat:esi.corporation.members",
        "esi:GET /corporations/{corporation_id}/roles",
        "esi:GET /corporations/{corporation_id}/roles/history",
        "feat:esi.corporation.roles",
        "esi:GET /corporations/{corporation_id}/blueprints",
        "feat:esi.corporation.blueprints",
        "esi:GET /corporations/{corporation_id}/structures",
        "feat:esi.corporation.structures",
        "feat:esi.corporations.structures-services-timers",
    ]);

    let db_path = temp_db_path("corp_gov_esi");
    let corp_id;
    let raitaru_id;

    {
        let repo = reload_repo(&db_path);
        let ctx = Arc::new(ServerContext::with_repository(repo));
        common::pin_clock(&ctx);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 60_000_000_000.0), (PILOT_2, 1_000_000_000.0)]).await;

        let mut s1 = login(&ctx, 9431, "demo", PILOT_1).await;
        let mut s2 = login(&ctx, 9432, "other", PILOT_2).await;

        corp_id = found_corp_with_member(&ctx, &mut s1, &mut s2, "GOVN").await;
        let router = EsiRouter::new(Arc::clone(&ctx));
        let spec = OpenApiSpec::pinned();

        // Untrained CEO starts at base member limit 10; training Corporation Management (3363) L5 adds 100 -> 110
        let tok_ceo = issue_token(
            &ctx,
            PILOT_1,
            &[
                "esi-corporations.read_corporation_membership.v1",
                "esi-corporations.track_members.v1",
                "esi-corporations.read_blueprints.v1",
                "esi-corporations.read_structures.v1",
            ],
        );
        let (lim_base, _) = esi_call(&router, &spec, "GET", &format!("/v1/corporations/{corp_id}/members/limit/"), Some(&tok_ceo), None, 200);
        assert_eq!(lim_base, json!(20));

        train_skills(&ctx, &mut s1, &[3363, 3380, 3403, 3406, 20_185, RAITARU, MFG_PLANT], true).await;
        rpc(&ctx, &mut s1, "corpRegistry", "UpdateCorporationAbilities", &[])
            .await
            .expect("UpdateCorporationAbilities");
        let tok_ceo = issue_token(
            &ctx,
            PILOT_1,
            &[
                "esi-corporations.read_corporation_membership.v1",
                "esi-corporations.track_members.v1",
                "esi-corporations.read_blueprints.v1",
                "esi-corporations.read_structures.v1",
            ],
        );
        let (lim_skilled, _) = esi_call(&router, &spec, "GET", &format!("/v1/corporations/{corp_id}/members/limit/"), Some(&tok_ceo), None, 200);
        assert_eq!(lim_skilled, json!(100));

        // Grant extended divisional & operational roles (Trader + Hangar_Take_1 + Container_Take_2 + Account_Take_1)
        let role_mask = (corp_roles::ROLE_TRADER
            | corp_roles::ROLE_HANGAR_CAN_TAKE[0]
            | corp_roles::ROLE_CONTAINER_CAN_TAKE[1]
            | corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]
            | corp_roles::ROLE_STATION_MANAGER) as i64;
        rpc(
            &ctx,
            &mut s1,
            "corpRegistry",
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(PILOT_2)),
                ("roles", PyRep::Integer(role_mask)),
                ("titleMask", PyRep::Integer(0)),
            ])],
        )
        .await
        .expect("Grant divisional roles");
        ctx.advance_secs(86_405);
        let tok_ceo = issue_token(
            &ctx,
            PILOT_1,
            &[
                "esi-corporations.read_corporation_membership.v1",
                "esi-corporations.track_members.v1",
                "esi-corporations.read_blueprints.v1",
                "esi-corporations.read_structures.v1",
            ],
        );

        assert_etag_304(&router, &spec, &format!("/v1/corporations/{corp_id}/roles/"), Some(&tok_ceo));
        let (roles_json, _) = esi_call(&router, &spec, "GET", &format!("/v1/corporations/{corp_id}/roles/"), Some(&tok_ceo), None, 200);
        let p2_entry = roles_json.as_array().unwrap().iter().find(|r| r["character_id"] == PILOT_2).unwrap();
        let p2_roles = p2_entry["roles"].as_array().unwrap();
        assert!(p2_roles.contains(&json!("Trader")));
        assert!(p2_roles.contains(&json!("Hangar_Take_1")));
        assert!(p2_roles.contains(&json!("Container_Take_2")));
        assert!(p2_roles.contains(&json!("Account_Take_1")));
        assert!(p2_roles.contains(&json!("Station_Manager")));

        let (hist_json, _) = esi_call(&router, &spec, "GET", &format!("/v2/corporations/{corp_id}/roles/history/"), Some(&tok_ceo), None, 200);
        assert!(hist_json.as_array().unwrap().iter().any(|h| h["character_id"] == PILOT_2 && h["issuer_id"] == PILOT_1));

        // Buy Rifter BPO, place in CorpSAG1 of deployed Raitaru, research ME -> verify /v2/corporations/{corp_id}/blueprints/
        let mut cargo = upwell_cargo(&ctx, RAITARU, &[RESEARCH_LAB]);
        cargo.push((RIFTER_BP, 1));
        cargo.extend(fuel_cargo());
        let fr = board_loaded_freighter(&ctx, &mut s1, &cargo).await;
        let bp_item = items_at(&ctx, PILOT_1, fr, RIFTER_BP)[0].0;
        let (rid, _) = deploy_online_upwell(&ctx, &mut s1, PERIMETER_SYS, RAITARU, &[RESEARCH_LAB], 1_500).await;
        raitaru_id = rid;
        move_item(&ctx, &mut s1, bp_item, raitaru_id, 116).await;
        rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(corp_id), PyRep::Real(100_000_000.0), PyRep::String("rd".into())])
            .await
            .unwrap();

        let res_job = install(&ctx, &mut s1, bp_item, raitaru_id, 4, 1, &[("useCorp", PyRep::Bool(true))])
            .await
            .expect("Research corp BPO ME");
        complete_job(&ctx, &mut s1, &res_job).await;
        let tok_ceo = issue_token(
            &ctx,
            PILOT_1,
            &[
                "esi-corporations.read_corporation_membership.v1",
                "esi-corporations.track_members.v1",
                "esi-corporations.read_blueprints.v1",
                "esi-corporations.read_structures.v1",
            ],
        );

        assert_etag_304(&router, &spec, &format!("/v2/corporations/{corp_id}/blueprints/"), Some(&tok_ceo));
        let (bps_json, _) = esi_call(&router, &spec, "GET", &format!("/v2/corporations/{corp_id}/blueprints/"), Some(&tok_ceo), None, 200);
        let bp_row = bps_json.as_array().unwrap().iter().find(|b| b["item_id"] == bp_item).expect("corp BPO in ESI");
        assert_eq!(bp_row["material_efficiency"], json!(1));
        assert_eq!(bp_row["runs"], json!(-1));

        // Verify /v1/corporations/{corp_id}/structures/ returns live Upwell structure, service, fuel_expires & reinforce_hour
        assert_etag_304(&router, &spec, &format!("/v1/corporations/{corp_id}/structures/"), Some(&tok_ceo));
        let (structs_json, _) = esi_call(&router, &spec, "GET", &format!("/v1/corporations/{corp_id}/structures/"), Some(&tok_ceo), None, 200);
        let st_row = structs_json.as_array().unwrap().iter().find(|s| s["structure_id"] == raitaru_id).unwrap();
        assert_eq!(st_row["type_id"], json!(RAITARU));
        assert!(st_row["fuel_expires"].as_str().is_some());
        assert_eq!(st_row["services"][0]["state"], json!("online"));
        let _ = structure_lifecycle::STATE_SHIELD_VULNERABLE;
    }

    // Reopen SQLite repo and confirm persistence
    {
        let repo2 = reload_repo(&db_path);
        let ctx2 = Arc::new(ServerContext::with_repository(repo2));
        let router2 = EsiRouter::new(Arc::clone(&ctx2));
        let spec2 = OpenApiSpec::pinned();
        let tok2 = issue_token(
            &ctx2,
            PILOT_1,
            &[
                "esi-corporations.read_corporation_membership.v1",
                "esi-corporations.track_members.v1",
                "esi-corporations.read_blueprints.v1",
                "esi-corporations.read_structures.v1",
            ],
        );
        let (lim2, _) = esi_call(&router2, &spec2, "GET", &format!("/v1/corporations/{corp_id}/members/limit/"), Some(&tok2), None, 200);
        assert_eq!(lim2, json!(100));
        let (bps2, _) = esi_call(&router2, &spec2, "GET", &format!("/v2/corporations/{corp_id}/blueprints/"), Some(&tok2), None, 200);
        assert_eq!(bps2.as_array().unwrap()[0]["material_efficiency"], json!(1));
        let (st2, _) = esi_call(&router2, &spec2, "GET", &format!("/v1/corporations/{corp_id}/structures/"), Some(&tok2), None, 200);
        assert_eq!(st2.as_array().unwrap()[0]["structure_id"], json!(raitaru_id));
    }

    cleanup_sqlite_files(&db_path);
}
