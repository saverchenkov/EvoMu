//! Deep branch-coverage and mutation-killing unit tests for `fleet_social_corp_gov`
//! (shares, dividends, titles, CEO succession, vote cases, ballots, sanctioned actions, medals,
//! bulletins, recruitment ads, corporate contacts/labels) and `fleet_social_corp_tail`
//! (bloodline NPC placement, `member_kick_blocker` 24h cooldown boundary, docked & downtime auto-kick).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::fleet_social::fleet_social_corp_tail::{
    bloodline_default_npc_corp, execute_pending_auto_kicks_for_docked_char,
    execute_pending_auto_kicks_on_downtime, handle_kick_out_members,
    handle_update_members, member_kick_blocker,
};
use crate::services::fleet_social::handle as handle_social;
use eve_db::{corp_roles, CorporationRecord, IskCents, ServerClock, VirtualServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        let mut dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for _ in 0..5 {
            let candidate = dir.join("data/evomu_sde.sqlite");
            if candidate.exists() {
                return Arc::new(SdeRegistry::open(&candidate).expect("open evomu_sde.sqlite"));
            }
            if !dir.pop() {
                break;
            }
        }
        Arc::new(SdeRegistry::with_canonical_seed())
    }))
}

fn test_ctx_and_clock(ts: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let clock = Arc::new(VirtualServerClock::with_fixed_base(ts));
    let clock_dyn: Arc<dyn ServerClock> = clock.clone();
    db.set_clock(Arc::clone(&clock_dyn));
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(clock_dyn);
    (ctx, clock)
}

fn make_sess(char_id: i64, corp_id: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(8200 + (char_id as u64 % 400));
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(corp_id);
    s.solar_system_id = Some(30_000_142);
    s.station_id = Some(60_003_760);
    s.corp_role = role;
    s
}

async fn corp_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle_social(ctx, sess, svc, method, args, kwargs)
        .await
        .expect("corp call handled")
}

fn seed_corp_and_three_members(ctx: &ServerContext, now: u64) -> (i64, i64, i64, i64) {
    let corp_id = 98_500_001i64;
    let ceo_id = 90_000_001i64;
    let mem_a = 90_000_002i64;
    let mem_b = 90_000_003i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters[&ceo_id].clone();
        for &(cid, st) in &[(ceo_id, Some(60_003_760)), (mem_a, Some(60_003_760)), (mem_b, None)] {
            let mut ch = snap.characters.get(&cid).cloned().unwrap_or_else(|| {
                let mut t = base_ch.clone();
                t.character_id = cid;
                t.user_id = cid as u64;
                t.name = format!("Member-{cid}");
                t
            });
            ch.corporation_id = corp_id;
            ch.station_id = st;
            ch.structure_id = None;
            ch.balance = IskCents::from_isk(5_000_000);
            snap.characters.insert(cid, ch);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_id,
            name: "GovTest Corp".into(),
            ticker: "GOVT".into(),
            ceo_id,
            alliance_id: None,
            tax_rate: 0.1,
            master_wallet: IskCents::from_isk(50_000_000),
            members: HashSet::from([ceo_id, mem_a, mem_b]),
            member_roles: HashMap::from([(ceo_id, u64::MAX)]),
            applications: HashMap::new(),
            wallet_divisions: vec![],
            description: "Gov".into(),
            url: String::new(),
            hq_station_id: Some(60_003_760),
        };
        corp.ensure_wallet_divisions();
        corp.wallet_divisions[0].balance = IskCents::from_isk(50_000_000);
        snap.corporations.insert(corp_id, corp);

        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.shares.insert(corp_id, 1_000);
        gov.company_shares = 1_000;
        gov.created_unix = now;
    });
    (corp_id, ceo_id, mem_a, mem_b)
}

#[test]
fn corp_shares_dividends_titles_medals_bulletins_ads_and_contacts() {
    let now = 1_700_000_000u64;
    let (ctx, _) = test_ctx_and_clock(now);
    let (corp_id, ceo_id, mem_a, mem_b) = seed_corp_and_three_members(&ctx, now);
    let empty_kw = HashMap::new();

    let mut ceo_sess = make_sess(ceo_id, corp_id, u64::MAX);
    let mut mem_sess = make_sess(mem_a, corp_id, 0);

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // Shares: non-director denied, insufficient shares denied, valid MoveCompanyShares + MovePrivateShares
            assert_eq!(
                corp_call(&ctx, &mut mem_sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(mem_a), PyRep::Integer(10)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "CorpRoleDenied"
            );
            assert_eq!(
                corp_call(&ctx, &mut ceo_sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(mem_a), PyRep::Integer(9_999)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "InsufficientShares"
            );
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "MoveCompanyShares", &[PyRep::Integer(mem_a), PyRep::Integer(400)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut mem_sess, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(mem_b), PyRep::Integer(150)], &empty_kw).await.is_ok());
            assert_eq!(
                corp_call(&ctx, &mut mem_sess, "corpRegistry", "MovePrivateShares", &[PyRep::Integer(mem_b), PyRep::Integer(5_000)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "InsufficientShares"
            );
            assert!(corp_call(&ctx, &mut mem_sess, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(mem_a)], &empty_kw).await.is_ok());
            assert_eq!(
                corp_call(&ctx, &mut ceo_sess, "corpRegistry", "GetSharesByShareholder", &[PyRep::Integer(mem_a)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "CrpAccessDenied"
            );
            assert_eq!(
                corp_call(&ctx, &mut mem_sess, "corpRegistry", "GetSharesByShareholder", &[PyRep::Bool(true)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "CrpAccessDenied"
            );
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "GetMemberIDsWithMoreThanAvgShares", &[], &empty_kw).await.is_ok());

            // Dividends: non-director denied, insufficient wallet rejected, shareholders vs members payout
            assert_eq!(
                corp_call(&ctx, &mut mem_sess, "corpRegistry", "PayoutDividend", &[PyRep::Integer(1_000), PyRep::Integer(0)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "CorpRoleDenied"
            );
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "PayoutDividend", &[PyRep::Integer(100_000), PyRep::Integer(0)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "PayoutDividend", &[PyRep::Integer(90_000), PyRep::Integer(1)], &empty_kw).await.is_ok());

            // Titles: UpdateTitle, UpdateTitles, GetTitles, DeleteTitle
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Officer".into()), PyRep::Integer(2)], &empty_kw).await.is_ok());
            assert!(corp_call(
                &ctx,
                &mut ceo_sess,
                "corpRegistry",
                "UpdateTitles",
                &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(2), PyRep::String("Quartermaster".into()), PyRep::Integer(4)])])],
                &empty_kw,
            )
            .await
            .is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "GetTitles", &[], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "DeleteTitle", &[PyRep::Integer(2)], &empty_kw).await.is_ok());

            // Medals: CreateMedal, GiveMedalToCharacters, SetMedalStatus, GetAllCorpMedals, GetMedalDetails, GetMedalsReceived, GetRecipientsOfMedal, GetEmploymentRecord
            let m_rep = corp_call(
                &ctx,
                &mut ceo_sess,
                "corporationSvc",
                "CreateMedal",
                &[PyRep::String("Valor Cross".into()), PyRep::String("Frontline service".into())],
                &empty_kw,
            )
            .await
            .unwrap();
            let medal_id = m_rep.dict_get("medalID").and_then(|v| v.as_i64()).unwrap_or(1);
            assert!(corp_call(
                &ctx,
                &mut ceo_sess,
                "corporationSvc",
                "GiveMedalToCharacters",
                &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(mem_a)])],
                &empty_kw,
            )
            .await
            .is_ok());
            assert!(corp_call(&ctx, &mut mem_sess, "corporationSvc", "SetMedalStatus", &[PyRep::Integer(medal_id), PyRep::Integer(2)], &empty_kw).await.is_ok());
            for m in ["GetAllCorpMedals", "GetMedalDetails", "GetMedalsReceived", "GetRecipientsOfMedal", "GetEmploymentRecord"] {
                assert!(corp_call(&ctx, &mut ceo_sess, "corporationSvc", m, &[PyRep::Integer(medal_id)], &empty_kw).await.is_ok());
            }

            // Bulletins: AddBulletin, UpdateBulletinOrder, GetBulletins, DeleteBulletin
            let b_rep = corp_call(
                &ctx,
                &mut ceo_sess,
                "corpRegistry",
                "AddBulletin",
                &[PyRep::String("Fleet Ping".into()), PyRep::String("Form at Jita 4-4".into())],
                &empty_kw,
            )
            .await
            .unwrap();
            let bul_id = b_rep.dict_get("bulletinID").and_then(|v| v.as_i64()).unwrap_or(1);
            assert!(corp_call(
                &ctx,
                &mut ceo_sess,
                "corpRegistry",
                "UpdateBulletinOrder",
                &[PyRep::List(vec![PyRep::Integer(bul_id)])],
                &empty_kw,
            )
            .await
            .is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "GetBulletins", &[], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "DeleteBulletin", &[PyRep::Integer(bul_id)], &empty_kw).await.is_ok());

            // Recruitment Ads: Create, Update, GetRecruiters, GetRecruitmentAds*, SearchCorpAds, DeleteRecruitmentAd
            let ad_rep = corp_call(
                &ctx,
                &mut ceo_sess,
                "corpRecProxy",
                "CreateRecruitmentAd",
                &[PyRep::String("Small Gang PvP".into()), PyRep::String("Join us".into()), PyRep::Integer(3), PyRep::Integer(14), PyRep::Integer(1)],
                &empty_kw,
            )
            .await
            .unwrap();
            let ad_id = ad_rep.dict_get("adID").and_then(|v| v.as_i64()).unwrap();
            assert!(corp_call(
                &ctx,
                &mut ceo_sess,
                "corpRecProxy",
                "UpdateRecruitmentAd",
                &[PyRep::Integer(ad_id), PyRep::Integer(3), PyRep::String("Small Gang v2".into()), PyRep::String("Updated".into())],
                &empty_kw,
            )
            .await
            .is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRecProxy", "GetRecruiters", &[PyRep::Integer(corp_id), PyRep::Integer(ad_id)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRecProxy", "GetRecruitmentAdsForCorporation", &[], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRecProxy", "GetRecruitmentAdsByCorpID", &[PyRep::Integer(corp_id)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRecProxy", "GetRecruitmentAd", &[PyRep::Integer(ad_id)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRecProxy", "SearchCorpAds", &[PyRep::Integer(3), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(50)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRecProxy", "DeleteRecruitmentAd", &[PyRep::Integer(ad_id)], &empty_kw).await.is_ok());

            // Contacts & Labels (unknown owner rejected with OwnerNotFound, known corp 1_000_035 succeeds)
            assert_eq!(
                corp_call(&ctx, &mut ceo_sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(90_000_099), PyRep::Real(5.0)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "OwnerNotFound"
            );
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(1_000_035), PyRep::Real(5.0)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "EditCorporateContact", &[PyRep::Integer(1_000_035), PyRep::Real(-5.0)], &empty_kw).await.is_ok());
            let lbl_rep = corp_call(&ctx, &mut ceo_sess, "corpRegistry", "CreateLabel", &[PyRep::String("Hostiles".into())], &empty_kw).await.unwrap();
            let lbl_id = lbl_rep.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(1_000_035)]), PyRep::Integer(lbl_id)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "GetCorporateContacts", &[], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "GetLabels", &[], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(1_000_035)]), PyRep::Integer(lbl_id)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "DeleteLabel", &[PyRep::Integer(lbl_id)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "corpRegistry", "RemoveCorporateContacts", &[PyRep::Integer(1_000_035)], &empty_kw).await.is_ok());
        });
}

#[test]
fn votes_sanctions_ceo_election_kick_blockers_and_auto_kick_lifecycle() {
    let now = 1_700_000_000u64;
    let (ctx, clock) = test_ctx_and_clock(now);
    let (corp_id, ceo_id, mem_a, mem_b) = seed_corp_and_three_members(&ctx, now);
    let empty_kw = HashMap::new();

    // Grant mem_a & mem_b shares so shareholders vote
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.shares.insert(ceo_id, 500);
        gov.shares.insert(mem_a, 300);
        gov.shares.insert(mem_b, 200);
        gov.company_shares = 0;

        // Verify bloodline_default_npc_corp & member_kick_blocker 24h cooldown boundary
        let npc = bloodline_default_npc_corp(&ctx, snap, mem_a);
        assert!(npc > 0);
        assert_eq!(bloodline_default_npc_corp(&ctx, snap, 999_888_777), 1_000_006);

        // Role blocker
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(mem_a, corp_roles::ROLE_DIRECTOR);
        assert_eq!(member_kick_blocker(&snap, corp_id, mem_a, now), Some("MemberHasRoles"));
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.remove(&mem_a);

        // RoleDropCooldownActive exact 86_400s boundary: 86_399 blocked vs 86_400 cleared
        snap.corp_governance.get_mut(&corp_id).unwrap().role_drop_timestamps.insert(mem_a, now);
        assert_eq!(member_kick_blocker(&snap, corp_id, mem_a, now + 86_399), Some("RoleDropCooldownActive"));
        assert_eq!(member_kick_blocker(&snap, corp_id, mem_a, now + 86_400), None);
        snap.corp_governance.get_mut(&corp_id).unwrap().role_drop_timestamps.remove(&mem_a);
    });

    let mut ceo_sess = make_sess(ceo_id, corp_id, u64::MAX);
    let mut mem_a_sess = make_sess(mem_a, corp_id, 0);
    let mut non_sh_sess = make_sess(90_000_088, 1_000_035, 0);

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            // 1. Insert share-creation vote case (vote_type = 5) & CEO election (vote_type = 0)
            assert_eq!(
                corp_call(
                    &ctx,
                    &mut ceo_sess,
                    "voteManager",
                    "InsertVoteCase",
                    &[PyRep::String("Bad Shares".into()), PyRep::String("Zero".into()), PyRep::Integer(5), PyRep::Integer(0)],
                    &empty_kw,
                )
                .await
                .unwrap_err()
                .code,
                "InvalidArgument"
            );

            let shares_vc = corp_call(
                &ctx,
                &mut ceo_sess,
                "voteManager",
                "InsertVoteCase",
                &[PyRep::String("Issue 500 Shares".into()), PyRep::String("Expansion".into()), PyRep::Integer(5), PyRep::Integer(500)],
                &empty_kw,
            )
            .await
            .unwrap();
            let shares_cid = shares_vc.dict_get("voteCaseID").and_then(|v| v.as_i64()).unwrap();

            let ceo_vc = corp_call(
                &ctx,
                &mut ceo_sess,
                "voteManager",
                "InsertVoteCase",
                &[
                    PyRep::String("Elect MemA CEO".into()),
                    PyRep::String("Succession".into()),
                    PyRep::Integer(0),
                    PyRep::List(vec![PyRep::Integer(mem_a), PyRep::Integer(ceo_id)]),
                ],
                &empty_kw,
            )
            .await
            .unwrap();
            let ceo_cid = ceo_vc.dict_get("voteCaseID").and_then(|v| v.as_i64()).unwrap();

            // CanRunForCEO / CanViewVotes + non-shareholder InsertVote rejected
            assert!(corp_call(&ctx, &mut ceo_sess, "voteManager", "CanRunForCEO", &[], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "voteManager", "CanViewVotes", &[], &empty_kw).await.is_ok());
            assert_eq!(
                corp_call(&ctx, &mut non_sh_sess, "voteManager", "InsertVote", &[PyRep::Integer(shares_cid), PyRep::Integer(1)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "CorpAccessDenied"
            );

            // Cast votes while open -> VoteStillOpen before end_ts -> advance clock past 86_400 -> ActivateSanctionedAction succeeds!
            assert!(corp_call(&ctx, &mut ceo_sess, "voteManager", "InsertVote", &[PyRep::Integer(shares_cid), PyRep::Integer(1)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut mem_a_sess, "voteManager", "InsertVote", &[PyRep::Integer(ceo_cid), PyRep::Integer(1)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut ceo_sess, "voteManager", "InsertVote", &[PyRep::Integer(ceo_cid), PyRep::Integer(1)], &empty_kw).await.is_ok());

            for q in ["GetVotes", "GetVoteCase", "GetVoteCaseOptions", "GetVoteCasesByCorporation", "GetCorpSanctionedActions"] {
                assert!(corp_call(&ctx, &mut ceo_sess, "voteManager", q, &[PyRep::Integer(shares_cid)], &empty_kw).await.is_ok());
            }
            assert_eq!(
                corp_call(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(shares_cid)], &empty_kw)
                    .await
                    .unwrap_err()
                    .code,
                "VoteStillOpen"
            );

            // Advance virtual clock beyond voting window (now + 86_401)
            clock.advance_secs(86_401);
            assert!(corp_call(&ctx, &mut ceo_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(shares_cid)], &empty_kw).await.is_ok());
            assert!(corp_call(&ctx, &mut mem_a_sess, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(ceo_cid)], &empty_kw).await.is_ok());
        });

    // Verify new CEO is mem_a and shares increased by 500
    let (new_ceo, comp_shares) = ctx.db.with_snapshot(|s| {
        (
            s.corporations[&corp_id].ceo_id,
            s.corp_governance[&corp_id].company_shares,
        )
    });
    assert_eq!(new_ceo, mem_a);
    assert_eq!(comp_shares, 1500);

    // 2. handle_update_members + handle_kick_out_members (queued in-space kick + docked completion + downtime cleanup)
    assert!(handle_update_members(
        &ctx,
        &mut mem_a_sess,
        "UpdateMember",
        &[PyRep::Integer(ceo_id), PyRep::Integer(1), PyRep::Integer(0)],
    )
    .unwrap()
    .is_ok());

    // mem_b is in space (station_id = None) -> KickOutMembers queues into pending_auto_kicks
    assert!(handle_kick_out_members(&ctx, &mut mem_a_sess, &[PyRep::Integer(mem_b)])
        .unwrap()
        .is_ok());
    assert!(ctx.db.with_snapshot(|s| s.corp_governance[&corp_id].pending_auto_kicks.contains_key(&mem_b)));

    // Dock mem_b and run execute_pending_auto_kicks_for_docked_char -> expels to bloodline NPC corp
    let mut mem_b_sess = make_sess(mem_b, corp_id, 0);
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&mem_b).unwrap().station_id = Some(60_003_760);
    });
    execute_pending_auto_kicks_for_docked_char(&ctx, &mut mem_b_sess, mem_b);
    assert!(ctx.db.with_snapshot(|s| !s.corporations[&corp_id].members.contains(&mem_b)));

    // Queue stale entry & exercise execute_pending_auto_kicks_on_downtime
    ctx.db.with_mut_snapshot(|s| {
        s.corp_governance.get_mut(&corp_id).unwrap().pending_auto_kicks.insert(ceo_id, now);
    });
    assert_eq!(execute_pending_auto_kicks_on_downtime(&ctx), 1);
}
