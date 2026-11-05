use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::{CorporationRecord, IskCents};

fn setup_corp_projects_ctx() -> (ServerContext, SessionState, i64, i64) {
    let (ctx, mut sess, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let corp_id = 98_000_020_i64;
    sess.corporation_id = Some(corp_id);
    sess.corp_role = u64::MAX;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
            ch.balance = IskCents::from_isk(10_000_000);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "Project Corp".into(),
            ticker: "PRJC".into(),
            ceo_id: char_id,
            alliance_id: None,
            tax_rate: 0.05,
            master_wallet: IskCents::from_isk(100_000_000),
            members: vec![char_id].into_iter().collect(),
            member_roles: [(char_id, corp_roles::ROLE_DIRECTOR)].into_iter().collect(),
            hq_station_id: Some(60_003_760),
            ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(corp_id, corp);
    });

    (ctx, sess, char_id, corp_id)
}

#[test]
fn test_uuid_roundtrips_and_parse_goal_contribution_config() {
    let pid = 4242_i64;
    let g_uuid = corp_goal_uuid(pid);
    assert_eq!(goal_uuid_to_project_id(&g_uuid), Some(pid));
    assert_eq!(freelance_uuid_to_project_id(&g_uuid), None);

    let f_uuid = freelance_uuid(1_700_000_000, pid);
    assert_eq!(freelance_uuid_to_project_id(&f_uuid), Some(pid));
    assert_eq!(goal_uuid_to_project_id(&f_uuid), None);

    assert_eq!(parse_goal_contribution_config(None), ("Any".into(), None));
    let mut cfg_msg = DynamicQuasarMessage::new("test.Config");
    let mut sub = DynamicQuasarMessage::new("test.Sub");
    sub.fields.insert(1, DynamicValue::Uint64(34));
    cfg_msg.fields.insert(5, DynamicValue::Message(Box::new(sub))); // 5 -> MineMaterials
    assert_eq!(
        parse_goal_contribution_config(Some(&DynamicValue::Message(Box::new(cfg_msg.clone())))),
        ("MineMaterials".into(), Some(34))
    );

    let mut create_req = DynamicQuasarMessage::new("test.CreateReq");
    create_req.fields.insert(4, DynamicValue::Message(Box::new(cfg_msg)));
    create_req.fields.insert(7, DynamicValue::Uint64(5_000));
    let opts = parse_goal_create_options(&create_req, 100).unwrap();
    assert_eq!(opts.matcher_type, "MineMaterials");
    assert_eq!(opts.filter_id, Some(34));
    assert_eq!(opts.isk_per_unit, IskCents::from_isk(5_000));
    assert_eq!(opts.escrow, IskCents::from_isk(500_000));
}

#[test]
fn test_create_activity_redeem_edit_and_close_corp_goal_and_freelance() {
    let (ctx, mut sess, char_id, corp_id) = setup_corp_projects_ctx();
    let now = ctx.now_unix();

    // 1. Create a Corporation Goal project
    let (goal_pid, _uuid) = create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: char_id,
            name: "Destroy Pirates".into(),
            description: "Kill 10 pirate NPCs".into(),
            target_quantity: 10,
            initial_quantity: 0,
            escrow: IskCents::from_isk(1_000_000),
            isk_per_unit: IskCents::from_isk(100_000),
            wallet_division: 1,
            matcher_type: "DestroyNPCShips".into(),
            is_freelance: false,
            auto_payout_on_contribution: false,
            max_per_character: Some(10),
            expires_unix: Some(now + 3600),
            filter_id: None,
            batch_size: 1,
            now_unix: now,
            min_project_id: 101,
        },
    )
    .expect("corp goal creation should succeed");

    // Record server activity (5 kills)
    record_server_project_activity(&ctx, char_id, &["DestroyNPCShips"], 5, None);
    let cur = ctx
        .db
        .with_snapshot(|s| s.corp_projects.get(&goal_pid).map(|p| p.current_quantity));
    assert_eq!(cur, Some(5));

    // Redeem corp goal reward for char
    let redeemed = redeem_all_corp_goal_rewards_for_char(&ctx.db, char_id, now).unwrap();
    assert_eq!(redeemed, vec![(goal_pid, 500_000)]);

    // Edit project
    let edited = handle_edit_project(
        &ctx,
        &mut sess,
        &[
            PyRep::Integer(goal_pid),
            PyRep::String("Destroy Elite Pirates".into()),
            PyRep::Integer((now + 7200) as i64),
            PyRep::Integer(20),
        ],
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        edited.dict_get("name").and_then(|v| v.as_str()),
        Some("Destroy Elite Pirates")
    );

    // Close project and refund remaining 500k escrow
    let closed = close_or_delete_canonical_project(&ctx.db, goal_pid, false, now);
    assert_eq!(closed, Some((corp_id, IskCents::from_isk(500_000))));

    // 2. Create a Freelance project, commit participant, record activity, and redeem
    let (fl_pid, _) = create_canonical_project(
        &ctx.db,
        CanonicalProjectSpec {
            corp_id,
            creator_id: char_id,
            name: "Freelance Mining".into(),
            description: "Mine Veldspar".into(),
            target_quantity: 100,
            initial_quantity: 0,
            escrow: IskCents::from_isk(1_000_000),
            isk_per_unit: IskCents::from_isk(10_000),
            wallet_division: 1,
            matcher_type: "MineMaterials".into(),
            is_freelance: true,
            auto_payout_on_contribution: false,
            max_per_character: None,
            expires_unix: Some(now + 10),
            filter_id: Some(1228),
            batch_size: 10,
            now_unix: now,
            min_project_id: 201,
        },
    )
    .expect("freelance creation should succeed");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(det) = snap.alliance_war_state.project_details.get_mut(&fl_pid) {
            det.committed_participants.push(char_id);
        }
    });
    record_server_project_activity(&ctx, char_id, &["MineMaterials"], 20, Some(1228));
    let (batches, isk) = redeem_freelance_reward_for_char(&ctx.db, fl_pid, char_id, now).unwrap();
    assert_eq!(batches, 2);
    assert_eq!(isk, 200_000);

    // Sweep expiry after expires_unix
    let expired = ctx
        .db
        .with_mut_snapshot(|snap| sweep_expired_corp_projects_in_snap(snap, now + 20));
    assert!(expired.contains(&(fl_pid, corp_id)));
}
