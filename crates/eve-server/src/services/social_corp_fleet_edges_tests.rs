use super::*;
use crate::services::social_corp_fleet_extra_unit_tests::{call_social, rs_rows, setup_sandbox};
use crate::services::wallet_services_ext::{CorpBillRecord, bill_note_id, save_json_note};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, OwnerNoteRecord, SystemDynamicState,
    UpwellStructureRecord, WarRecord,
};
use eve_db::durable_domain_types::{PersistedHubUpgrade, PersistedSovHubData};
use std::collections::HashMap;

fn set_clock_unix(ctx: &ServerContext, ts: u64) {
    ctx.set_clock(std::sync::Arc::new(
        eve_db::VirtualServerClock::with_fixed_base(ts),
    ));
}

async fn call_social_kw(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
    kwargs: HashMap<String, PyRep>,
    bound_obj_id: Option<i64>,
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, sess, svc, method, &args, &kwargs, bound_obj_id)
        .await
        .expect("social_corp_fleet method should dispatch")
}

async fn create_test_alliance(
    ctx: &ServerContext,
    exec_sess: &mut SessionState,
    name: &str,
    short: &str,
) -> i64 {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&98_100_001) {
            c.set_division_balance(1, IskCents::from_isk(5_000_000_000));
            for d in &mut c.wallet_divisions {
                if d.division_id == 1 {
                    d.balance = IskCents::from_isk(5_000_000_000);
                }
            }
        }
    });
    let res = call_social(
        ctx,
        exec_sess,
        "corpRegistry",
        "CreateAlliance",
        vec![
            PyRep::String(name.into()),
            PyRep::String(short.into()),
            PyRep::String("Test alliance".into()),
            PyRep::String("https://alliance.example".into()),
        ],
    )
    .await
    .expect("CreateAlliance succeeds");
    let aid = find_keyval_field(&res, "allianceID")
        .and_then(|v| v.as_i64())
        .expect("alliance id");
    exec_sess.alliance_id = Some(aid);
    aid
}

fn find_keyval_field<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries.iter().find_map(|(k, v)| {
            if k == key {
                Some(v)
            } else {
                None
            }
        }),
        PyRep::Object { args, .. } => match args.as_ref() {
            PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
                if k.as_str() == Some(key) {
                    Some(v)
                } else {
                    None
                }
            }),
            _ => None,
        },
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
            if k.as_str() == Some(key) {
                Some(v)
            } else {
                None
            }
        }),
        _ => None,
    }
}

#[tokio::test]
async fn alliance_timers_executor_rotation_closure_and_app_rejection() {
    let (ctx, mut exec_sess, mut member_sess) = setup_sandbox();
    let aid = create_test_alliance(&ctx, &mut exec_sess, "Edge Rotate Alliance", "ERA").await;

    // Member corp submits an application
    call_social(
        &ctx,
        &mut member_sess,
        "corpRegistry",
        "ApplyToJoinAlliance",
        vec![
            PyRep::Integer(aid),
            PyRep::String("Joining for edge tests".into()),
        ],
    )
    .await
    .expect("ApplyToJoinAlliance");

    // Non-existent application error
    let err = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "UpdateApplication",
        vec![PyRep::Integer(98_999_999), PyRep::String("Accepted".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "ApplicationNotFound");

    // Reject via kwarg "state" = "declined", then re-apply and accept via integer arg
    call_social_kw(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "UpdateApplication",
        vec![PyRep::Integer(98_100_002)],
        HashMap::from([("state".into(), PyRep::String("declined".into()))]),
        Some(aid),
    )
    .await
    .expect("reject via kwarg");

    call_social(
        &ctx,
        &mut member_sess,
        "corpRegistry",
        "ApplyToJoinAlliance",
        vec![
            PyRep::Integer(aid),
            PyRep::String("Second try".into()),
        ],
    )
    .await
    .expect("re-apply");

    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "UpdateApplication",
        vec![
            PyRep::Integer(98_100_002),
            PyRep::String("Please".into()),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("accept application");

    // Advance 24h so member corp 98_100_002 officially joins on sync_all_alliance_timers
    set_clock_unix(&ctx, 1_700_000_000 + 86_401);
    sync_all_alliance_timers(&ctx);
    ctx.db.with_snapshot(|snap| {
        let a = snap.alliances.get(&aid).expect("alliance exists");
        assert!(a.member_corp_ids.contains(&98_100_002));
    });

    // Executor corp (98_100_001) leaves via DeleteMember -> after 24h executor rotates to 98_100_002
    let leave_res = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "DeleteMember",
        vec![PyRep::Integer(98_100_001)],
    )
    .await
    .expect("DeleteMember own corp");
    assert_eq!(
        find_keyval_field(&leave_res, "delaySeconds").and_then(|v| v.as_i64()),
        Some(86_400)
    );

    set_clock_unix(&ctx, 1_700_000_000 + 86_401 * 2);
    sync_all_alliance_timers(&ctx);
    ctx.db.with_snapshot(|snap| {
        let a = snap.alliances.get(&aid).expect("alliance still open");
        assert_eq!(a.executor_corp_id, 98_100_002);
        assert!(!a.member_corp_ids.contains(&98_100_001));
    });
    exec_sess.alliance_id = None;
    member_sess.alliance_id = Some(aid);

    // Last member leaves -> alliance closes and is removed from snap.alliances
    call_social(
        &ctx,
        &mut member_sess,
        "allianceRegistry",
        "DeleteMember",
        vec![PyRep::Integer(98_100_002)],
    )
    .await
    .expect("last DeleteMember");
    set_clock_unix(&ctx, 1_700_000_000 + 86_401 * 3);
    sync_all_alliance_timers(&ctx);
    assert!(!sync_alliance_timers(&ctx, aid));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.alliances.contains_key(&aid));
    });
}

#[tokio::test]
async fn alliance_info_days_dictatorial_kick_and_delete_member_guards() {
    let (ctx, mut exec_sess, mut member_sess) = setup_sandbox();

    // Outside alliance: GetDaysInAlliance & DeclareExecutorSupport error
    assert_eq!(
        call_social(&ctx, &mut exec_sess, "allianceRegistry", "GetDaysInAlliance", vec![])
            .await
            .unwrap_err()
            .code,
        "NotInAlliance"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "DeclareExecutorSupport",
            vec![PyRep::Integer(98_100_001)],
        )
        .await
        .unwrap_err()
        .code,
        "NotInAlliance"
    );

    let aid = create_test_alliance(&ctx, &mut exec_sess, "Guard Alliance", "GRD").await;

    // GetAlliancePublicInfo via bound_obj_id with empty args + missing AllianceNotFound
    let pub_info = call_social_kw(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "GetAlliancePublicInfo",
        vec![],
        HashMap::new(),
        Some(aid),
    )
    .await
    .expect("bound GetAlliancePublicInfo");
    assert_eq!(
        find_keyval_field(&pub_info, "allianceID").and_then(|v| v.as_i64()),
        Some(aid)
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GetAlliancePublicInfo",
            vec![PyRep::Integer(99_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "AllianceNotFound"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GetAlliance",
            vec![PyRep::Integer(99_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "AllianceNotFound"
    );

    // GetDaysInAlliance falls back to 0 when employment_history is empty, then computes delta
    with_alliance_war_state(&ctx, |st| {
        st.alliances.entry(aid).or_default().employment_history.clear();
    });
    assert_eq!(
        call_social(&ctx, &mut exec_sess, "allianceRegistry", "GetDaysInAlliance", vec![])
            .await
            .expect("days 0")
            .as_i64(),
        Some(0)
    );
    with_alliance_war_state(&ctx, |st| {
        st.alliances
            .entry(aid)
            .or_default()
            .employment_history
            .push((98_100_001, 1_700_000_000, None));
    });
    set_clock_unix(&ctx, 1_700_000_000 + 86_400 * 5);
    assert_eq!(
        call_social(&ctx, &mut exec_sess, "allianceRegistry", "GetDaysInAlliance", vec![])
            .await
            .expect("days 5")
            .as_i64(),
        Some(5)
    );

    // Dictatorial UpdateAlliance blocks DeclareExecutorSupport
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "UpdateAlliance",
        vec![
            PyRep::String("Dictatorial rules".into()),
            PyRep::String("https://dict.example".into()),
            PyRep::Integer(0),
            PyRep::Bool(true),
        ],
    )
    .await
    .expect("set dictatorial");
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "DeclareExecutorSupport",
            vec![PyRep::Integer(98_100_001)],
        )
        .await
        .unwrap_err()
        .code,
        "AllianceIsDictatorial"
    );

    // Join member corp 98_100_002 for KickMember / DeleteMember checks
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(a) = snap.alliances.get_mut(&aid) {
            a.member_corp_ids.insert(98_100_002);
        }
        if let Some(c) = snap.corporations.get_mut(&98_100_002) {
            c.alliance_id = Some(aid);
        }
    });
    member_sess.alliance_id = Some(aid);

    // DeleteMember on another corp -> CorpRoleDenied
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "DeleteMember",
            vec![PyRep::Integer(98_100_002)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // KickMember by non-executor corp -> NotAllianceExecutor
    assert_eq!(
        call_social(
            &ctx,
            &mut member_sess,
            "allianceRegistry",
            "KickMember",
            vec![PyRep::Integer(98_100_002)],
        )
        .await
        .unwrap_err()
        .code,
        "NotAllianceExecutor"
    );

    // KickMember by non-director in executor corp -> CorpRoleDenied
    let mut line_sess = exec_sess.clone();
    line_sess.character_id = Some(91_000_003);
    line_sess.corp_role = 0;
    assert_eq!(
        call_social(
            &ctx,
            &mut line_sess,
            "allianceRegistry",
            "KickMember",
            vec![PyRep::Integer(98_100_002)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Active war prevents KickMember -> CannotLeaveAllianceDuringWar
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            777_001,
            WarRecord {
                war_id: 777_001,
                aggressor_corp_id: 98_100_002,
                defender_corp_id: 98_900_999,
                mutual: false,
                started_unix: 1_700_000_000,
                active: true,
            },
        );
    });
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "KickMember",
            vec![PyRep::Integer(98_100_002)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotLeaveAllianceDuringWar"
    );
}

#[tokio::test]
async fn alliance_contacts_labels_and_bulletins_edge_paths() {
    let (ctx, mut exec_sess, _member_sess) = setup_sandbox();
    let aid = create_test_alliance(&ctx, &mut exec_sess, "Contact Label Ally", "CLA").await;
    let other_aid = aid + 1;
    ctx.db.with_mut_snapshot(|snap| {
        snap.alliances.insert(
            other_aid,
            eve_db::AllianceRecord {
                alliance_id: other_aid,
                name: "Other Ally".into(),
                short_name: "OTH".into(),
                executor_corp_id: 98_100_002,
                member_corp_ids: [98_100_002].into_iter().collect(),
            },
        );
    });

    // InvalidStanding
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "AddAllianceContact",
            vec![PyRep::Integer(other_aid), PyRep::Real(12.5)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidStanding"
    );

    // Classify alliance contact, NPC corp contact, character contact, and EditAllianceContact (prev = Some)
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "AddAllianceContact",
        vec![PyRep::Integer(other_aid), PyRep::Real(5.0)],
    )
    .await
    .expect("add alliance contact");
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "AddAllianceContact",
        vec![PyRep::Integer(1_000_035), PyRep::Real(2.5)],
    )
    .await
    .expect("add npc corp contact");
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "AddAllianceContact",
        vec![PyRep::Integer(91_000_002), PyRep::Real(-5.0)],
    )
    .await
    .expect("add char contact");
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "EditAllianceContact",
        vec![
            PyRep::Integer(91_000_002),
            PyRep::Real(10.0),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("edit existing contact");

    // Labels: CreateLabel, AssignLabels (swapped order + auto-create missing contact), RemoveLabels (tuple), DeleteLabel
    let label_rep = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "CreateLabel",
        vec![PyRep::String("CoreOps".into()), PyRep::Integer(2)],
    )
    .await
    .expect("CreateLabel");
    let mask = find_keyval_field(&label_rep, "labelID")
        .and_then(|v| v.as_i64())
        .expect("label mask");

    // Swapped order: (labelID < 1_000_000, contactID >= 1_000_000) for contact not yet added
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "AssignLabels",
        vec![PyRep::Integer(mask), PyRep::Integer(91_000_777)],
    )
    .await
    .expect("AssignLabels swapped & auto-create");

    // Tuple form for AssignLabels and RemoveLabels
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "AssignLabels",
        vec![PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Integer(other_aid)]),
            PyRep::Integer(mask),
        ])],
    )
    .await
    .expect("AssignLabels tuple");

    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "RemoveLabels",
        vec![PyRep::Tuple(vec![
            PyRep::Integer(mask),
            PyRep::Integer(other_aid),
        ])],
    )
    .await
    .expect("RemoveLabels tuple swapped");

    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "DeleteLabel",
        vec![PyRep::Integer(mask)],
    )
    .await
    .expect("DeleteLabel clears remaining mask");

    ctx.db.with_snapshot(|snap| {
        let list = snap.alliance_contacts.get(&aid).expect("alliance contacts");
        let c = list
            .iter()
            .find(|c| c.contact_id == 91_000_777)
            .expect("auto-created contact");
        assert!(c.label_ids.is_empty());
    });

    // Bulletins: empty title error, missing update error, and all 4 UpdateBulletinOrder forms
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "AddBulletin",
            vec![PyRep::String("".into()), PyRep::String("body".into())],
        )
        .await
        .unwrap_err()
        .code,
        "BulletinTitleRequired"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "UpdateBulletin",
            vec![
                PyRep::Integer(999_888),
                PyRep::String("Missing".into()),
                PyRep::String("Body".into()),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "BulletinNotFound"
    );

    let b1_rep = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("First".into()), PyRep::String("Body1".into())],
    )
    .await
    .expect("b1");
    let b1 = find_keyval_field(&b1_rep, "bulletinID")
        .and_then(|v| v.as_i64())
        .unwrap();
    let b2_rep = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "AddBulletin",
        vec![PyRep::String("Second".into()), PyRep::String("Body2".into())],
    )
    .await
    .expect("b2");
    let b2 = find_keyval_field(&b2_rep, "bulletinID")
        .and_then(|v| v.as_i64())
        .unwrap();

    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "UpdateBulletin",
        vec![
            PyRep::Integer(b1),
            PyRep::String("First Updated".into()),
            PyRep::String("Body1 Updated".into()),
        ],
    )
    .await
    .expect("UpdateBulletin");

    // Empty UpdateBulletinOrder -> InvalidArgument
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "UpdateBulletinOrder",
            vec![],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    // Tuple-pair form
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "UpdateBulletinOrder",
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![PyRep::Integer(b2), PyRep::Integer(0)]),
            PyRep::Tuple(vec![PyRep::Integer(b1), PyRep::Integer(1)]),
        ])],
    )
    .await
    .expect("UpdateBulletinOrder pairs");
    // List-of-ints form
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "UpdateBulletinOrder",
        vec![PyRep::List(vec![PyRep::Integer(b1), PyRep::Integer(b2)])],
    )
    .await
    .expect("UpdateBulletinOrder int list");
    // Varargs form
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "UpdateBulletinOrder",
        vec![PyRep::Integer(b2), PyRep::Integer(b1)],
    )
    .await
    .expect("UpdateBulletinOrder varargs");
}

#[tokio::test]
async fn alliance_sovereignty_capital_prime_hour_and_structures_info() {
    let (ctx, mut exec_sess, _member_sess) = setup_sandbox();
    let aid = create_test_alliance(&ctx, &mut exec_sess, "Sov Alliance", "SOV").await;
    let valid_sys = *ctx.sde.solar_systems.keys().next().expect("sde solar system") as i64;

    // SetCapitalSystem errors
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "SetCapitalSystem",
            vec![PyRep::Integer(0)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "SetCapitalSystem",
            vec![PyRep::Integer(99_999_999)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidSolarSystem"
    );

    // No pending transition -> NoCapitalSystemTransition
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "CancelCapitalSystemTransition",
            vec![],
        )
        .await
        .unwrap_err()
        .code,
        "NoCapitalSystemTransition"
    );

    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "SetCapitalSystem",
        vec![PyRep::Integer(valid_sys)],
    )
    .await
    .expect("SetCapitalSystem");
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "CancelCapitalSystemTransition",
        vec![],
    )
    .await
    .expect("CancelCapitalSystemTransition");

    // SetPrimeHour errors & success + GetPrimeTimeInfo
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "SetPrimeHour",
            vec![],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "SetPrimeHour",
            vec![PyRep::Integer(25)],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidPrimeHour"
    );
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "SetPrimeHour",
        vec![PyRep::Integer(14)],
    )
    .await
    .expect("SetPrimeHour 14");
    let prime = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "GetPrimeTimeInfo",
        vec![],
    )
    .await
    .expect("GetPrimeTimeInfo");
    assert!(find_keyval_field(&prime, "newPrimeHour").is_some());

    // GetAllianceSovereigntyStructuresInfo with SovHub / IHUB / TCU + command nodes + vuln window
    let hub_id = 880_000_001;
    let ihub_id = 880_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        snap.system_states.insert(
            valid_sys as u32,
            SystemDynamicState {
                solar_system_id: valid_sys as u32,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                ess_cycle_started_unix: 0,
                ess_main_bank_contributors: HashMap::new(),
                sov_alliance_id: Some(aid),
                military_index: 3,
                industrial_index: 2,
                strategic_index: 4,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 81_826, // Sovereignty Hub
                owner_corp_id: 98_100_001,
                solar_system_id: valid_sys as u32,
                name: "Alliance SovHub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: false,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 1_700_050_000,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.structures.insert(
            ihub_id,
            UpwellStructureRecord {
                structure_id: ihub_id,
                type_id: 32_458, // Infrastructure Hub
                owner_corp_id: 98_100_001,
                solar_system_id: valid_sys as u32,
                name: "Alliance IHub".into(),
                position: [1000.0, 0.0, 0.0],
                state: "ArmorReinforced".into(),
                quantum_core_installed: false,
                fuel_blocks: 0,
                shield_hp: 0.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        snap.sov_runtime.sov_hubs.insert(
            hub_id,
            PersistedSovHubData {
                hub_id,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 82_001,
                    online: true,
                    low_power: false,
                }],
                fuel: Default::default(),
                workforce: Default::default(),
                last_updated_unix: 1_700_000_000,
            },
        );
        snap.sov_runtime
            .adm_vulnerability_windows
            .insert(hub_id, (1_700_010_000, 1_700_020_000));
        snap.sov_runtime
            .command_nodes
            .insert(990_001, (hub_id, valid_sys as u32, 0));
        snap.sov_runtime
            .command_node_scores
            .insert(hub_id, vec![(aid, 0.72)]);
    });

    let info = call_social_kw(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "GetAllianceSovereigntyStructuresInfo",
        vec![],
        HashMap::new(),
        Some(aid),
    )
    .await
    .expect("GetAllianceSovereigntyStructuresInfo");
    match &info {
        PyRep::List(items) => assert!(items.len() >= 2),
        PyRep::Tuple(items) => assert!(items.len() >= 2),
        other => panic!("unexpected sov structures rep: {other:?}"),
    }
}

#[tokio::test]
async fn alliance_bills_balance_lists_pay_and_gm_force_accept() {
    let (ctx, mut exec_sess, mut member_sess) = setup_sandbox();
    let aid = create_test_alliance(&ctx, &mut exec_sess, "Bill Ally", "BIL").await;

    // Seed division 1 & division 2 balances on debtor corp 98_100_001
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.corporations.get_mut(&98_100_001) {
            c.set_division_balance(1, IskCents::from_isk(100_000));
            c.set_division_balance(1, IskCents::from_isk(100_000));
            c.set_division_balance(2, IskCents::from_isk(50_000));
        }
        // Corrupt note for bill 9003
        snap.owner_notes.insert(
            bill_note_id(9003),
            OwnerNoteRecord {
                note_id: bill_note_id(9003),
                character_id: -71,
                item_id: 0,
                label: "bill:9003".into(),
                note: "{not valid json".into(),
                timestamp_unix: 1_700_000_000,
            },
        );
    });

    let bill1 = CorpBillRecord {
        bill_id: 9001,
        bill_type_id: 1,
        debtor_id: aid,
        creditor_id: 98_100_002,
        amount_cents: IskCents::from_isk(25_000).0,
        due_unix: 1_700_100_000,
        interest: 0.0,
        external_id: 0,
        external_id2: 0,
        paid: false,
        paid_by_division: None,
    };
    let bill2 = CorpBillRecord {
        bill_id: 9002,
        bill_type_id: 2,
        debtor_id: 98_100_001,
        creditor_id: aid,
        amount_cents: IskCents::from_isk(10_000).0,
        due_unix: 1_700_100_000,
        interest: 0.0,
        external_id: 0,
        external_id2: 0,
        paid: true,
        paid_by_division: Some(1),
    };
    let bill_other = CorpBillRecord {
        bill_id: 9004,
        bill_type_id: 1,
        debtor_id: 98_999_888,
        creditor_id: 98_100_002,
        amount_cents: IskCents::from_isk(999_999).0,
        due_unix: 1_700_100_000,
        interest: 0.0,
        external_id: 0,
        external_id2: 0,
        paid: false,
        paid_by_division: None,
    };
    ctx.db.with_mut_snapshot(|snap| {
        save_json_note(
            snap,
            bill_note_id(9001),
            -71,
            9001,
            "bill:9001".into(),
            &bill1,
            1_700_000_000,
        );
        save_json_note(
            snap,
            bill_note_id(9002),
            -71,
            9002,
            "bill:9002".into(),
            &bill2,
            1_700_000_000,
        );
        save_json_note(
            snap,
            bill_note_id(9004),
            -71,
            9004,
            "bill:9004".into(),
            &bill_other,
            1_700_000_000,
        );
    });

    // GetBillBalance: specific unpaid bill, paid bill -> 0.0, corrupt -> BillNotFound, missing -> BillNotFound, other -> AccessDenied, aggregate
    let bal1 = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "GetBillBalance",
        vec![PyRep::Integer(9001)],
    )
    .await
    .expect("bal 9001");
    assert_eq!(bal1, PyRep::Real(25_000.0));

    let bal_paid = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "GetBillBalance",
        vec![PyRep::Integer(9002)],
    )
    .await
    .expect("paid bal");
    assert_eq!(bal_paid, PyRep::Real(0.0));

    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GetBillBalance",
            vec![PyRep::Integer(9003)],
        )
        .await
        .unwrap_err()
        .code,
        "BillNotFound"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GetBillBalance",
            vec![PyRep::Integer(9999)],
        )
        .await
        .unwrap_err()
        .code,
        "BillNotFound"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GetBillBalance",
            vec![PyRep::Integer(9004)],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    let agg = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "GetBillBalance",
        vec![],
    )
    .await
    .expect("aggregate balance");
    assert_eq!(agg, PyRep::Real(25_000.0));

    // GetBills & GetBillsReceivable
    let bills = call_social(&ctx, &mut exec_sess, "allianceRegistry", "GetBills", vec![])
        .await
        .expect("GetBills");
    assert!(!rs_rows(&bills).is_empty());
    let recv = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "GetBillsReceivable",
        vec![],
    )
    .await
    .expect("GetBillsReceivable");
    assert!(!rs_rows(&recv).is_empty());

    // PayBill error paths: InvalidArgument, BillNotFound, BillAlreadyPaid, AccessDenied, NotEnoughMoney
    assert_eq!(
        call_social(&ctx, &mut exec_sess, "allianceRegistry", "PayBill", vec![])
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "PayBill",
            vec![PyRep::Integer(9999), PyRep::Integer(1000)],
        )
        .await
        .unwrap_err()
        .code,
        "BillNotFound"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "PayBill",
            vec![PyRep::Integer(9002), PyRep::Integer(1000)],
        )
        .await
        .unwrap_err()
        .code,
        "BillAlreadyPaid"
    );
    // Non-director/non-accountant in executor corp -> CorpRoleDenied; bill owned by another entity -> AccessDenied
    let mut non_acct = exec_sess.clone();
    non_acct.character_id = Some(91_000_003);
    non_acct.corp_role = 0;
    assert_eq!(
        call_social(
            &ctx,
            &mut non_acct,
            "allianceRegistry",
            "PayBill",
            vec![PyRep::Integer(9001), PyRep::Integer(1000)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "PayBill",
            vec![PyRep::Integer(9004), PyRep::Integer(1000)],
        )
        .await
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    // Division 3 has 0 ISK -> NotEnoughMoney
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "PayBill",
            vec![PyRep::Integer(9001), PyRep::Integer(1002)],
        )
        .await
        .unwrap_err()
        .code,
        "NotEnoughMoney"
    );

    // Pay bill 9001 from division 2 (accountKey 1001)
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "PayBill",
        vec![PyRep::Integer(9001), PyRep::Integer(1001)],
    )
    .await
    .expect("PayBill from division 2");
    ctx.db.with_snapshot(|snap| {
        let debtor = snap.corporations.get(&98_100_001).unwrap();
        assert_eq!(debtor.division_balance(2), IskCents::from_isk(25_000));
    });

    // Pay another bill from division 1 (accountKey 1000) to hit master_wallet branch
    let bill5 = CorpBillRecord {
        bill_id: 9005,
        bill_type_id: 1,
        debtor_id: aid,
        creditor_id: 98_100_002,
        amount_cents: IskCents::from_isk(10_000).0,
        due_unix: 1_700_100_000,
        interest: 0.0,
        external_id: 0,
        external_id2: 0,
        paid: false,
        paid_by_division: None,
    };
    ctx.db.with_mut_snapshot(|snap| {
        save_json_note(
            snap,
            bill_note_id(9005),
            -71,
            9005,
            "bill:9005".into(),
            &bill5,
            1_700_000_000,
        );
    });
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "PayBill",
        vec![PyRep::Integer(9005), PyRep::Integer(1000)],
    )
    .await
    .expect("PayBill from division 1");

    // GMForceAcceptApplication: GMRoleRequired, InvalidArgument, AllianceNotFound, CorpNotFound, ApplicationNotFound, and success
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![PyRep::Integer(98_100_002)],
        )
        .await
        .unwrap_err()
        .code,
        "GMRoleRequired"
    );
    exec_sess.role = 0xFFFF_FFFF_FFFF_FFFF;
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![],
        )
        .await
        .unwrap_err()
        .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_social_kw(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![PyRep::Integer(98_100_002)],
            HashMap::new(),
            Some(99_999_999),
        )
        .await
        .unwrap_err()
        .code,
        "AllianceNotFound"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![PyRep::Integer(98_999_777)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpNotFound"
    );
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "allianceRegistry",
            "GMForceAcceptApplication",
            vec![PyRep::Integer(98_100_002)],
        )
        .await
        .unwrap_err()
        .code,
        "ApplicationNotFound"
    );

    call_social(
        &ctx,
        &mut member_sess,
        "corpRegistry",
        "ApplyToJoinAlliance",
        vec![PyRep::Integer(aid), PyRep::String("GM force".into())],
    )
    .await
    .expect("apply for GM force");
    call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "GMForceAcceptApplication",
        vec![PyRep::Integer(98_100_002)],
    )
    .await
    .expect("GMForceAcceptApplication succeeds");
}

#[tokio::test]
async fn invbroker_deliver_to_corp_hangar_and_member_edge_cases() {
    let (ctx, mut exec_sess, _member_sess) = setup_sandbox();
    let station_id = 60_003_760_i64;
    exec_sess.station_id = None;

    // NotDocked
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![
                PyRep::Integer(station_id),
                PyRep::Integer(98_100_001),
                PyRep::List(vec![PyRep::Integer(1)]),
                PyRep::Integer(116),
                PyRep::Integer(station_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "NotDocked"
    );

    exec_sess.station_id = Some(station_id);
    exec_sess.ship_id = Some(700_001);

    // Empty items -> ItemNotFound; unknown item -> ItemNotFound
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![
                PyRep::Integer(station_id),
                PyRep::Integer(98_100_001),
                PyRep::List(vec![]),
                PyRep::Integer(116),
                PyRep::Integer(station_id),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotFound"
    );

    // Seed items for various error guards + valid office-folder deliveries
    let office_id = 700_100;
    let container_id = 700_101;
    let courier_item_id = 700_102;
    let inside_container_id = 700_103;
    let fitted_item_id = 700_104;
    let foreign_item_id = 700_105;
    let elsewhere_item_id = 700_106;
    let div_item_id = 700_107;
    let personal_item_id = 700_108;
    let member_delivery_item_id = 700_109;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            700_001,
            InventoryItemRecord {
                item_id: 700_001,
                type_id: 587,
                owner_id: 91_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            office_id,
            InventoryItemRecord {
                item_id: office_id,
                type_id: 27, // Office folder
                owner_id: 98_100_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            container_id,
            InventoryItemRecord {
                item_id: container_id,
                type_id: 17_366,
                owner_id: 91_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            inside_container_id,
            InventoryItemRecord {
                item_id: inside_container_id,
                type_id: 34,
                owner_id: 91_000_001,
                location_id: container_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            courier_item_id,
            InventoryItemRecord {
                item_id: courier_item_id,
                type_id: 34,
                owner_id: 91_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10,
                singleton: false,
                ..Default::default()
            },
        );
        snap.item_custom_names
            .insert(courier_item_id, "courier_cargo:55".into());
        snap.items.insert(
            fitted_item_id,
            InventoryItemRecord {
                item_id: fitted_item_id,
                type_id: 3170,
                owner_id: 91_000_001,
                location_id: 700_001,
                flag: InventoryFlag::LoSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            foreign_item_id,
            InventoryItemRecord {
                item_id: foreign_item_id,
                type_id: 34,
                owner_id: 91_000_002,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            elsewhere_item_id,
            InventoryItemRecord {
                item_id: elsewhere_item_id,
                type_id: 34,
                owner_id: 91_000_001,
                location_id: station_id + 1,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            div_item_id,
            InventoryItemRecord {
                item_id: div_item_id,
                type_id: 34,
                owner_id: 98_100_001,
                location_id: office_id,
                flag: InventoryFlag::CorpSAG2,
                quantity: 20,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            personal_item_id,
            InventoryItemRecord {
                item_id: personal_item_id,
                type_id: 34,
                owner_id: 91_000_001,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 15,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            member_delivery_item_id,
            InventoryItemRecord {
                item_id: member_delivery_item_id,
                type_id: 34,
                owner_id: 98_100_001,
                location_id: office_id,
                flag: InventoryFlag::CorpSAG1,
                quantity: 30,
                singleton: false,
                ..Default::default()
            },
        );
    });

    // CorporationNotFound (using existing item and unknown corp in session)
    let mut bad_corp_sess = exec_sess.clone();
    bad_corp_sess.corporation_id = Some(98_888_999);
    assert_eq!(
        call_social(
            &ctx,
            &mut bad_corp_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![
                PyRep::List(vec![PyRep::Integer(personal_item_id)]),
                PyRep::Integer(116),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );

    // DeliverToCorpMember: TargetNotInCorporation (using existing item)
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpMember",
            vec![
                PyRep::List(vec![PyRep::Integer(personal_item_id)]),
                PyRep::Integer(91_000_002),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "TargetNotInCorporation"
    );

    // CannotDeliverActiveShip
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::Integer(700_001), PyRep::Integer(116)],
        )
        .await
        .unwrap_err()
        .code,
        "CannotDeliverActiveShip"
    );

    // Fitted/locked item -> ItemCannotBeTransferred
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::List(vec![PyRep::Integer(fitted_item_id)]), PyRep::Integer(116)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemCannotBeTransferred"
    );

    // courier_cargo item -> ItemCannotBeTransferred
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::List(vec![PyRep::Integer(courier_item_id)]), PyRep::Integer(116)],
        )
        .await
        .unwrap_err()
        .code,
        "ItemCannotBeTransferred"
    );

    // Item inside non-office container -> ItemInsideContainer
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![
                PyRep::List(vec![PyRep::Integer(inside_container_id)]),
                PyRep::Integer(116),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ItemInsideContainer"
    );

    // ItemNotOwned
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![
                PyRep::List(vec![PyRep::Integer(foreign_item_id)]),
                PyRep::Integer(116),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotOwned"
    );

    // ItemNotAtLocation
    assert_eq!(
        call_social(
            &ctx,
            &mut exec_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![
                PyRep::List(vec![PyRep::Integer(elsewhere_item_id)]),
                PyRep::Integer(116),
            ],
        )
        .await
        .unwrap_err()
        .code,
        "ItemNotAtLocation"
    );

    // Corp division item without take/trader role -> CorpRoleDenied
    let mut no_role_sess = exec_sess.clone();
    no_role_sess.character_id = Some(91_000_003);
    no_role_sess.corp_role = 0;
    assert_eq!(
        call_social(
            &ctx,
            &mut no_role_sess,
            "invbroker",
            "DeliverToCorpHangar",
            vec![PyRep::List(vec![PyRep::Integer(div_item_id)]), PyRep::Integer(116)],
        )
        .await
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Success: DeliverToCorpHangar on personal item & corp division item from office folder
    call_social(
        &ctx,
        &mut exec_sess,
        "invbroker",
        "DeliverToCorpHangar",
        vec![
            PyRep::List(vec![
                PyRep::Integer(personal_item_id),
                PyRep::Integer(div_item_id),
            ]),
            PyRep::Integer(118),
        ],
    )
    .await
    .expect("DeliverToCorpHangar succeeds");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&personal_item_id].owner_id, 98_100_001);
        assert_eq!(snap.items[&personal_item_id].flag, InventoryFlag::CorpSAG3);
        assert_eq!(snap.items[&div_item_id].flag, InventoryFlag::CorpSAG3);
    });

    // Success: DeliverToCorpMember from office folder to corp member 91_000_003
    call_social(
        &ctx,
        &mut exec_sess,
        "invbroker",
        "DeliverToCorpMember",
        vec![
            PyRep::List(vec![PyRep::Integer(member_delivery_item_id)]),
            PyRep::Integer(91_000_003),
        ],
    )
    .await
    .expect("DeliverToCorpMember succeeds");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&member_delivery_item_id].owner_id, 91_000_003);
        assert_eq!(snap.items[&member_delivery_item_id].flag, InventoryFlag::Deliveries);
    });
}

#[tokio::test]
async fn alliance_contacts_five_arg_bool_signature_extracts_label_mask() {
    let (ctx, mut exec_sess, _member_sess) = setup_sandbox();
    let aid = create_test_alliance(&ctx, &mut exec_sess, "FiveArg Ally", "FVA").await;
    let target_char_id = 91_000_778_i64;

    // V24 5-arg signature: (contactID, standing, inWatchlist: Bool, notify: Bool, labelMask)
    let added = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "AddAllianceContact",
        vec![
            PyRep::Integer(target_char_id),
            PyRep::Real(5.0),
            PyRep::Bool(true),
            PyRep::Bool(false),
            PyRep::Integer(3),
        ],
    )
    .await
    .expect("5-arg AddAllianceContact with Bool at args[2] succeeds");
    assert_eq!(find_keyval_field(&added, "labelMask"), Some(&PyRep::Integer(3)));
    ctx.db.with_snapshot(|snap| {
        let rec = snap
            .alliance_contacts
            .get(&aid)
            .and_then(|v| v.iter().find(|c| c.contact_id == target_char_id))
            .expect("alliance contact recorded");
        assert!((rec.standing - 5.0).abs() < 1e-6);
        assert_eq!(rec.label_ids, vec![1, 2]);
    });

    // EditAllianceContact with Bool at args[2] and missing/negative labelMask falls back cleanly to 0
    let edited_missing = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "EditAllianceContact",
        vec![
            PyRep::Integer(target_char_id),
            PyRep::Real(-2.5),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("3-arg EditAllianceContact with Bool at args[2] succeeds");
    assert_eq!(find_keyval_field(&edited_missing, "labelMask"), Some(&PyRep::Integer(0)));

    let edited_neg = call_social(
        &ctx,
        &mut exec_sess,
        "allianceRegistry",
        "EditAllianceContact",
        vec![
            PyRep::Integer(target_char_id),
            PyRep::Real(10.0),
            PyRep::Bool(true),
            PyRep::Bool(true),
            PyRep::Integer(-5),
        ],
    )
    .await
    .expect("5-arg EditAllianceContact with negative labelMask clamps to 0");
    assert_eq!(find_keyval_field(&edited_neg, "labelMask"), Some(&PyRep::Integer(0)));
    ctx.db.with_snapshot(|snap| {
        let rec = snap
            .alliance_contacts
            .get(&aid)
            .and_then(|v| v.iter().find(|c| c.contact_id == target_char_id))
            .expect("edited alliance contact recorded");
        assert!((rec.standing - 10.0).abs() < 1e-6);
        assert!(rec.label_ids.is_empty());
    });
}
