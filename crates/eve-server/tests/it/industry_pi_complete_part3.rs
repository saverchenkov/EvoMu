//! Industry, Research, and Reactions (`E-4`) complete parity integration tests (Part 3).

use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
    UpwellStructureRecord,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const RIFTER_BP_TYPE_ID: TypeId = 691;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn seed_item(
    snap: &mut eve_db::WorldStateSnapshot,
    type_id: TypeId,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
    is_bpc: bool,
    runs: Option<i32>,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        InventoryItemRecord {
            item_id,
            type_id,
            owner_id,
            location_id,
            flag,
            quantity,
            singleton,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: is_bpc,
            blueprint_runs_remaining: runs,
        },
    );
    item_id
}

#[tokio::test]
async fn test_p0_issues_18_and_19_industry_auth_and_pi_simulation() {
    const TRITANIUM_TYPE_ID: TypeId = 34;
    const JITA_PLANET_ID: i64 = 40_009_082;
    const GAS_COMMAND_CENTER_TYPE_ID: TypeId = 2533;
    const GAS_EXTRACTOR_TYPE_ID: TypeId = 2534;
    const WATER_P1_TYPE_ID: TypeId = 3645;
    const AQUEOUS_LIQUIDS_P0_TYPE_ID: TypeId = 2268;

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();
    let char_ceo = 90_000_001;
    let char_line = 90_000_002;
    let corp_id = 98_000_777;
    let victim_corp_id = 98_000_888;

    let mut sess_ceo = setup_pilot(&ctx, 18191, "demo", "demo-secret", char_ceo).await;
    let mut sess_line = setup_pilot(&ctx, 18192, "other", "other-secret", char_line).await;
    sess_ceo.corporation_id = Some(corp_id);
    sess_line.corporation_id = Some(corp_id);
    sess_line.corp_role = 0;

    let (victim_bpo_id, corp_bpo_id, struct_id) = ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_ceo, char_line] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = corp_id;
                ch.station_id = Some(JITA_STATION_ID);
                ch.solar_system_id = JITA_SOLAR_SYSTEM_ID;
                ch.balance = IskCents::from_isk(100_000_000);
            }
            let sk = snap.skills.entry(cid).or_default();
            for stid in [3380, 3387, 3402, 3403, 3406, 2495, 2505, 13279] {
                sk.insert(
                    stid,
                    TrainedSkillRecord {
                        type_id: stid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
        snap.corporations.insert(
            corp_id,
            CorporationRecord {
                corporation_id: corp_id,
                name: "P0 Auth Corp".into(),
                ticker: "P0AC".into(),
                ceo_id: char_ceo,
                master_wallet: IskCents::from_isk(100_000_000),
                members: [char_ceo, char_line].into_iter().collect(),
                member_roles: HashMap::from([(char_ceo, u64::MAX), (char_line, 0)]),
                ..Default::default()
            },
        );
        snap.corporations.insert(
            victim_corp_id,
            CorporationRecord {
                corporation_id: victim_corp_id,
                name: "Victim Corp".into(),
                ticker: "VICT".into(),
                ceo_id: 90_000_999,
                master_wallet: IskCents::from_isk(100_000_000),
                ..Default::default()
            },
        );
        let vbpo = seed_item(snap, RIFTER_BP_TYPE_ID, victim_corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG1, 1, true, false, None);
        let _ = seed_item(snap, TRITANIUM_TYPE_ID, victim_corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG1, 500_000, false, false, None);
        let cbpo = seed_item(snap, RIFTER_BP_TYPE_ID, corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG3, 1, true, false, None);
        let _ = seed_item(snap, TRITANIUM_TYPE_ID, corp_id, JITA_STATION_ID, InventoryFlag::CorpSAG3, 500_000, false, false, None);
        snap.next_entity_id += 1;
        let st_id = snap.next_entity_id;
        snap.structures.insert(
            st_id,
            UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35825,
                owner_corp_id: corp_id,
                solar_system_id: JITA_SOLAR_SYSTEM_ID,
                name: "Corp Raitaru".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 5_000.0,
                armor_hp: 5_000.0,
                hull_hp: 5_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        (vbpo, cbpo, st_id)
    });

    // Issue #18: Spoofed foreign ownerID must be rejected
    let spoof_err = ctx
        .dispatch_rpc(
            &mut sess_ceo,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(victim_bpo_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(1)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
                ("ownerID", PyRep::Integer(victim_corp_id)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Foreign ownerID must be rejected");
    assert_eq!(spoof_err.code, "OwnerAccessDenied");

    // Issue #18: Zero-role corp member cannot install corp job or set facility taxes
    let no_role_job_err = ctx
        .dispatch_rpc(
            &mut sess_line,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(corp_bpo_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(4)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
            ])],
            &kw,
            None,
        )
        .await
        .expect_err("Zero-role member cannot install corp job");
    assert_eq!(no_role_job_err.code, "CrpAccessDenied");

    let no_role_tax_err = ctx
        .dispatch_rpc(
            &mut sess_line,
            "facilityManager",
            None,
            "SetFacilityTaxes",
            &[PyRep::Integer(struct_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.03))])],
            &kw,
            None,
        )
        .await
        .expect_err("Zero-role member cannot set facility taxes");
    assert_eq!(no_role_tax_err.code, "InsufficientCorpRoles");

    // Grant Factory Manager & Station Manager -> job install and tax update succeed, and CorpSAG3 flag is restored on cancel
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&corp_id) {
            c.member_roles.insert(
                char_line,
                eve_db::corp_roles::ROLE_FACTORY_MANAGER | eve_db::corp_roles::ROLE_STATION_MANAGER,
            );
        }
    });
    ctx.dispatch_rpc(
        &mut sess_line,
        "facilityManager",
        None,
        "SetFacilityTaxes",
        &[PyRep::Integer(struct_id), PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Real(0.03))])],
        &kw,
        None,
    )
    .await
    .expect("Station Manager can set facility taxes");

    let corp_job_rep = ctx
        .dispatch_rpc(
            &mut sess_line,
            "industryManager",
            None,
            "InstallJob",
            &[PyRep::keyval([
                ("blueprintID", PyRep::Integer(corp_bpo_id)),
                ("blueprintTypeID", PyRep::Integer(RIFTER_BP_TYPE_ID as i64)),
                ("facilityID", PyRep::Integer(JITA_STATION_ID)),
                ("activityID", PyRep::Integer(4)),
                ("runs", PyRep::Integer(1)),
                ("useCorp", PyRep::Bool(true)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("Factory Manager can install corp job");
    let corp_job_id = corp_job_rep.dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess_line, "industryManager", None, "CancelJob", &[PyRep::Integer(corp_job_id)], &kw, None)
        .await
        .expect("Cancel corp job");
    let restored_flag = ctx.db.with_snapshot(|snap| snap.items[&corp_bpo_id].flag);
    assert_eq!(restored_flag, InventoryFlag::CorpSAG3, "Corporate blueprint division flag must be restored");

    // Issue #19: StartTicking with 0 elapsed seconds does not fabricate extraction; UserLaunchCommodities enforces system check and planet container
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.planetary_colonies.insert(
            (char_ceo, JITA_PLANET_ID),
            eve_db::PlanetaryColonyRecord {
                planet_id: JITA_PLANET_ID,
                owner_id: char_ceo,
                solar_system_id: JITA_SOLAR_SYSTEM_ID,
                command_center_level: 3,
                pins_count: 2,
                extracted_commodities: HashMap::new(),
                last_extracted_unix: now_u,
            },
        );
        let mut net = eve_server::services::industry_pi::load_pi_network(snap, char_ceo, JITA_PLANET_ID, 3);
        net.last_simulated_unix = now_u;
        net.pins = vec![
            eve_server::services::industry_pi::PiPinState {
                pin_id: 701,
                type_id: GAS_COMMAND_CENTER_TYPE_ID,
                contents: HashMap::from([(WATER_P1_TYPE_ID, 100)]),
                ..Default::default()
            },
            eve_server::services::industry_pi::PiPinState {
                pin_id: 702,
                type_id: GAS_EXTRACTOR_TYPE_ID,
                extractor_resource_type_id: Some(AQUEOUS_LIQUIDS_P0_TYPE_ID),
                extractor_cycle_seconds: 300,
                extractor_head_count: 4,
                ..Default::default()
            },
        ];
        eve_server::services::industry_pi::save_pi_network(snap, char_ceo, JITA_PLANET_ID, &net);
    });

    for _ in 0..5 {
        ctx.dispatch_rpc(&mut sess_ceo, "planetMgr", None, "StartTicking", &[PyRep::Integer(JITA_PLANET_ID)], &kw, None)
            .await
            .expect("StartTicking at 0 elapsed seconds");
    }
    let extracted_after_zero_time = ctx.db.with_snapshot(|snap| {
        let net = eve_server::services::industry_pi::load_pi_network(snap, char_ceo, JITA_PLANET_ID, 3);
        net.pins.iter().map(|p| p.contents.get(&AQUEOUS_LIQUIDS_P0_TYPE_ID).copied().unwrap_or(0)).sum::<u64>()
    });
    assert_eq!(extracted_after_zero_time, 0, "StartTicking must not fabricate extraction when 0 real seconds elapsed");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_ceo) {
            ch.solar_system_id = 30_000_001;
        }
    });
    sess_ceo.solar_system_id = Some(30_000_001);
    let remote_launch_err = ctx
        .dispatch_rpc(
            &mut sess_ceo,
            "planetMgr",
            None,
            "UserLaunchCommodities",
            &[PyRep::Integer(JITA_PLANET_ID), PyRep::Integer(WATER_P1_TYPE_ID as i64), PyRep::Integer(50), PyRep::String("CustomsOffice".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Remote system UserLaunchCommodities must fail");
    assert_eq!(remote_launch_err.code, "ColonyNotInCurrentSystem");
}
