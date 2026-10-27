//! Integration tests for corporation lifecycle, autokick queue, quitting, divisions, and NPC taxes.
//!
//! Parity rows:
//! - `feat:lifecycle.corp-kick-online-session`
//! - `feat:lifecycle.corp-autokick-queue`
//! - `feat:lifecycle.corp-join-leaves-old-corp`
//! - `feat:lifecycle.corp-invite-consent`
//! - `feat:lifecycle.corp-quit-cleanup`
//! - `feat:corporation.pending-auto-kick`
//! - `feat:corporation.ceo-resign`
//! - `feat:corporation.corp-division-names`
//! - `feat:corporation.npc-corp-placement-rules`
//! - `feat:corporation.npc-corp-tax-rate`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::common;
use eve_db::{corp_roles, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PILOT_3: i64 = 90_000_003;

async fn select_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    match rep {
        PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
        PyRep::Dict(entries) => entries
            .iter()
            .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
            .unwrap_or(PyRep::None),
        _ => PyRep::None,
    }
}

fn rowset_rows(rep: &PyRep) -> Vec<Vec<PyRep>> {
    match rep {
        PyRep::Rowset { rows, .. } => rows.clone(),
        _ => Vec::new(),
    }
}

#[tokio::test]
async fn test_corp_lifecycle_kick_join_invite_quit() {
    // parity: feat:lifecycle.corp-kick-online-session
    // parity: feat:lifecycle.corp-autokick-queue
    // parity: feat:lifecycle.corp-join-leaves-old-corp
    // parity: feat:lifecycle.corp-invite-consent
    // parity: feat:lifecycle.corp-quit-cleanup
    eve_testkit::attest(&[
        "feat:lifecycle.corp-kick-online-session",
        "feat:lifecycle.corp-autokick-queue",
        "feat:lifecycle.corp-join-leaves-old-corp",
        "feat:lifecycle.corp-invite-consent",
        "feat:lifecycle.corp-quit-cleanup",
    ]);

    let ctx = ServerContext::new();
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "third".to_string(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "third".to_string(),
                password_token: eve_db::WorldRepository::hash_password_token("third", "third-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
        ch3.character_id = PILOT_3;
        ch3.user_id = 3;
        ch3.name = "Third Pilot".into();
        ch3.balance = IskCents(0);
        ch3.station_id = Some(60003760);
        snap.characters.insert(PILOT_3, ch3);
    });

    let mut sess1 = select_pilot(&ctx, 62_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 62_002, "other", "other-secret", PILOT_2).await;
    let mut sess3 = select_pilot(&ctx, 62_003, "third", "third-secret", PILOT_3).await;

    // Pilot 1 creates player corporation
    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Vanguard Syndicate".into()),
            PyRep::String("VNGD".into()),
            PyRep::Real(0.05),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corporationID");

    // 1. feat:lifecycle.corp-invite-consent
    // InsertInvitation with accept_now=True must NOT auto-join character; must fail or require target acceptance
    let invite_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "InsertInvitation",
        &[
            PyRep::Integer(PILOT_2),
            PyRep::Bool(true),
        ],
    )
    .await;
    assert!(
        invite_res.is_err(),
        "InsertInvitation with accept_now=True must be rejected without target consent"
    );
    let p2_corp_before = ctx.db.with_snapshot(|s| s.characters.get(&PILOT_2).unwrap().corporation_id);
    assert_ne!(p2_corp_before, corp_id, "Target corp must not change upon invitation");

    // 2. feat:lifecycle.corp-join-leaves-old-corp
    // Pilot 2 submits application
    let app_rep = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "InsertApplication",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("Requesting to join Vanguard".into()),
        ],
    )
    .await
    .expect("InsertApplication");
    let app_id = field(&app_rep, "applicationID").as_i64().expect("app_id");

    // Undocked pilot cannot join corp
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });
    let accept_undocked = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await;
    assert!(
        accept_undocked.is_err(),
        "Accepting an undocked pilot must fail with MustBeDockedToJoinCorp"
    );

    // Dock pilot 2 and accept
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_2).unwrap().station_id = Some(60003760);
    });
    let accept_res = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
    )
    .await;
    assert!(accept_res.is_ok(), "Accepting docked pilot into corp must succeed");

    // Verify pilot 2 is now in new corp and removed from old corp
    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).expect("corp");
        assert!(corp.members.contains(&PILOT_2), "Pilot 2 must be in new corp members");
        let ch = s.characters.get(&PILOT_2).expect("pilot 2");
        assert_eq!(ch.corporation_id, corp_id, "DB character corporation must be updated");
    });
    ctx.sync_session(&mut sess2);
    assert_eq!(sess2.corporation_id, Some(corp_id), "Online session must update corporation_id");

    // 3. feat:lifecycle.corp-autokick-queue
    // Give pilot 2 director role
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(PILOT_2, corp_roles::ROLE_DIRECTOR);
    });
    let kick_with_roles = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await;
    assert!(kick_with_roles.is_err(), "Kicking member with active roles must fail");

    // Drop roles; 24h drop timer applies
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().member_roles.insert(PILOT_2, 0);
        s.corp_governance.entry(corp_id).or_default().role_drop_timestamps.insert(PILOT_2, now);
    });
    let kick_24h = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await;
    assert!(kick_24h.is_err(), "Kicking member within 24h of role drop must fail");

    // Advance past 24 hours (86,400s)
    ctx.advance_secs(86_405);
    // Put pilot 2 in space (undocked)
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
    });

    let kick_queued = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("KickOutMember queued in space");
    assert_eq!(field(&kick_queued, "queuedAutoKick"), PyRep::Bool(true), "Kick must be queued when member is in space");

    // Verify queued in pending_auto_kicks
    ctx.db.with_snapshot(|s| {
        let gov = s.corp_governance.get(&corp_id).expect("gov");
        assert!(gov.pending_auto_kicks.contains_key(&PILOT_2), "Member must be in pending_auto_kicks");
        let corp = s.corporations.get(&corp_id).expect("corp");
        assert!(corp.members.contains(&PILOT_2), "Member remains in corp until executed on dock/downtime");
    });

    // 4. feat:lifecycle.corp-kick-online-session
    // Pilot 2 docks at station -> pending auto-kick executes
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_2).unwrap().station_id = Some(60003760);
    });
    eve_server::services::fleet_social::execute_pending_auto_kicks_for_docked_char(&ctx, &mut sess2, PILOT_2);

    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).expect("corp");
        assert!(!corp.members.contains(&PILOT_2), "Pilot 2 must be removed from corp members");
        let ch = s.characters.get(&PILOT_2).expect("pilot 2");
        assert_ne!(ch.corporation_id, corp_id, "Character must be moved to NPC corp");
        let corp_chan = s.chat_channels.get(&format!("corpid:{corp_id}"));
        if let Some(chan) = corp_chan {
            assert!(!chan.members.contains(&PILOT_2), "Pilot 2 must be removed from corp chat channel");
        }
    });
    assert_ne!(sess2.corporation_id, Some(corp_id), "Session corporation must be NPC corp");
    assert_eq!(sess2.corp_role, 0, "Session corp_role must be 0");
    assert_eq!(sess2.alliance_id, None, "Session alliance_id must be None");

    // 5. feat:lifecycle.corp-quit-cleanup
    // Move Pilot 3 into player corp and assign title and account key
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|s| {
        let corp = s.corporations.get_mut(&corp_id).unwrap();
        corp.members.insert(PILOT_3);
        s.characters.get_mut(&PILOT_3).unwrap().corporation_id = corp_id;
        let gov = s.corp_governance.entry(corp_id).or_default();
        gov.member_titles.insert(PILOT_3, 1);
        gov.member_account_keys.insert(PILOT_3, 1000);
        if let Some(chan) = s.chat_channels.get_mut(&format!("corpid:{corp_id}")) {
            chan.members.insert(PILOT_3);
        }
    });
    sess3.corporation_id = Some(corp_id);
    ctx.advance_secs(12);

    // Pilot 3 self-quits
    let quit_rep = rpc(
        &ctx,
        &mut sess3,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_3)],
    )
    .await
    .expect("Pilot 3 self-quit");
    assert_eq!(field(&quit_rep, "removed"), PyRep::Bool(true));

    ctx.db.with_snapshot(|s| {
        let ch = s.characters.get(&PILOT_3).unwrap();
        assert_ne!(ch.corporation_id, corp_id, "Quit pilot must be moved out of player corp");
        let gov = s.corp_governance.get(&corp_id);
        if let Some(g) = gov {
            assert!(!g.member_titles.contains_key(&PILOT_3), "Titles must be cleaned up on quit");
            assert!(!g.member_account_keys.contains_key(&PILOT_3), "Account keys must be cleaned up on quit");
        }
        let chan = s.chat_channels.get(&format!("corpid:{corp_id}"));
        if let Some(c) = chan {
            assert!(!c.members.contains(&PILOT_3), "Must be removed from player corp chat");
        }
        let npc_chan = s.chat_channels.get(&format!("corpid:{}", ch.corporation_id));
        if let Some(nc) = npc_chan {
            assert!(nc.members.contains(&PILOT_3), "Must be added to NPC corp chat channel");
        }
    });
}

#[tokio::test]
async fn test_corp_governance_divisions_ceo_placement_tax() {
    // parity: feat:corporation.pending-auto-kick
    // parity: feat:corporation.ceo-resign
    // parity: feat:corporation.corp-division-names
    // parity: feat:corporation.npc-corp-placement-rules
    // parity: feat:corporation.npc-corp-tax-rate
    eve_testkit::attest(&[
        "feat:corporation.pending-auto-kick",
        "feat:corporation.ceo-resign",
        "feat:corporation.corp-division-names",
        "feat:corporation.npc-corp-placement-rules",
        "feat:corporation.npc-corp-tax-rate",
    ]);

    let ctx = ServerContext::new();
    let mut sess1 = select_pilot(&ctx, 62_101, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = select_pilot(&ctx, 62_102, "other", "other-secret", PILOT_2).await;

    let created = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[
            PyRep::String("Apex Operations".into()),
            PyRep::String("APEX".into()),
            PyRep::Real(0.08),
        ],
    )
    .await
    .expect("AddCorporation");
    let corp_id = field(&created, "corporationID").as_i64().expect("corp_id");

    // 1. feat:corporation.corp-division-names
    let update_div = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "UpdateDivisionNames",
        &[
            PyRep::Integer(1),
            PyRep::String("Fleet Reserves".into()),
            PyRep::String("Main Treasury".into()),
        ],
    )
    .await;
    assert!(update_div.is_ok(), "UpdateDivisionNames must succeed for Director/CEO");

    let div_names = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetDivisionNames",
        &[PyRep::Integer(corp_id)],
    )
    .await
    .expect("GetDivisionNames");
    assert_eq!(field(&div_names, "division1"), PyRep::String("Fleet Reserves".into()));
    assert_eq!(field(&div_names, "walletDivision1"), PyRep::String("Main Treasury".into()));

    // 2. feat:corporation.pending-auto-kick
    // Add Pilot 2 to corp, undock, and kick to queue auto-kick
    ctx.advance_secs(12);
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&corp_id).unwrap().members.insert(PILOT_2);
        let ch = s.characters.get_mut(&PILOT_2).unwrap();
        ch.corporation_id = corp_id;
        ch.station_id = None;
        ch.structure_id = None;
    });
    ctx.advance_secs(12);
    let _ = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "KickOutMember",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("KickOutMember queued");

    let pending_rep = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "GetPendingAutoKicks",
        &[],
    )
    .await
    .expect("GetPendingAutoKicks");
    let rows = rowset_rows(&pending_rep);
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(PILOT_2))), "Pilot 2 must appear in GetPendingAutoKicks");

    // Execute on downtime
    let dt_count = eve_server::services::fleet_social::execute_pending_auto_kicks_on_downtime(&ctx);
    assert!(dt_count >= 1, "Downtime auto-kick must execute queued removals");
    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).unwrap();
        assert!(!corp.members.contains(&PILOT_2), "Pilot 2 removed at downtime");
    });

    // 3. feat:corporation.ceo-resign
    let pot_ceos = rpc(&ctx, &mut sess1, "corpRegistry", "GetNumberOfPotentialCEOs", &[])
        .await
        .expect("GetNumberOfPotentialCEOs");
    assert!(pot_ceos.as_i64().is_some());

    let can_run = rpc(&ctx, &mut sess1, "voteManager", "CanRunForCEO", &[])
        .await
        .expect("CanRunForCEO");
    assert!(matches!(can_run, PyRep::Bool(_)));

    let resign = rpc(&ctx, &mut sess1, "corpRegistry", "ResignFromCEO", &[])
        .await
        .expect("ResignFromCEO");
    assert_eq!(field(&resign, "newCeoID"), PyRep::Integer(0));
    ctx.db.with_snapshot(|s| {
        let corp = s.corporations.get(&corp_id).unwrap();
        assert_eq!(corp.ceo_id, 0, "Corporation must enter CEO-vacant state");
    });

    // 4. feat:corporation.npc-corp-placement-rules
    // Cannot submit application to NPC corporation
    let npc_app = rpc(
        &ctx,
        &mut sess2,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(1_000_006), PyRep::String("Join NPC corp".into())],
    )
    .await;
    assert!(npc_app.is_err(), "Applying to NPC corporation must be rejected with CannotApplyToNpcCorporation");

    // 5. feat:corporation.npc-corp-tax-rate
    // Set Pilot 2 in default NPC corporation and test 11% NPC tax on bounty ticks
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&PILOT_2).unwrap().corporation_id = 1_000_006;
    });

    let sys_id = 30000142; // Jita (highsec)
    // Small bounty < 100k ISK: tax exempt threshold
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 50_000.0);
    });
    let (_, small_tax, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(
        &ctx,
        sys_id,
        PILOT_2,
        0.0,
    );
    assert_eq!(small_tax, 0.0, "Bounties under 100k ISK must be tax exempt in NPC corp");

    // Large bounty >= 100k ISK: 11% NPC corp tax applied
    eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
        rt.pending_bounty_ticks.insert((sys_id, PILOT_2), 1_000_000.0);
    });
    let (net_payout, _, _) = eve_server::services::combat_npc_encounters::flush_npc_bounty_tick(
        &ctx,
        sys_id,
        PILOT_2,
        0.0,
    );
    assert!((net_payout - 1_201_500.0).abs() < 1e-3, "Net payout must reflect 11% tax deduction (1.35M gross - 148.5k tax)");

    let tax_entry = ctx.db.with_snapshot(|s| {
        s.wallet_journal
            .iter()
            .find(|e| e.character_id == PILOT_2 && e.entry_type == "bounty_prize_corporation_tax")
            .cloned()
    });
    assert!(tax_entry.is_some(), "Wallet journal must record 11% NPC corp tax deduction");
    let tax_rec = tax_entry.unwrap();
    assert_eq!(tax_rec.amount.raw_cents(), -148_500_00, "11% of 1.35M modified bounty is 148,500 ISK");
}
