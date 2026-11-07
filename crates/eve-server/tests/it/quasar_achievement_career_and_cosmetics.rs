//! Parity closure integration test suite for:
//! 1. `feat:quasar.achievement`
//! 2. `feat:quasar.career`
//! 3. `feat:quasar.cosmetic`

use crate::common;
use crate::quasar_goal_tale_notices::{drain, subscribe};
use crate::quasar_progression_complete::{build_sess, quasar_rpc};
use eve_db::IskCents;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, WireMessage,
};
use eve_server::quasar_gateway::{
    STATUS_NOT_FOUND, STATUS_OK, STATUS_PRECONDITION_FAILED,
};
use eve_server::router::ServerContext;

fn seed_pilot(ctx: &ServerContext, cid: i64, corp_id: i64) {
    ctx.db.with_mut_snapshot(|s| {
        let template = s.characters[&90_000_001].clone();
        let ch = s.characters.entry(cid).or_insert_with(|| {
            let mut c = template;
            c.character_id = cid;
            c.corporation_id = corp_id;
            c
        });
        ch.balance = IskCents::from_isk(500_000);
        ch.free_skill_points = 0.0;
    });
}

fn make_ident_req(id: u32) -> Vec<u8> {
    let (mut req_body, mut id_bytes, mut uuid) = (Vec::new(), Vec::new(), [0u8; 16]);
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut req_body);
    req_body
}

fn make_uuid_req(uuid: &[u8; 16]) -> Vec<u8> {
    let (mut req_body, mut id_bytes) = (Vec::new(), Vec::new());
    encode_bytes_field(1, uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut req_body);
    req_body
}

fn make_skinr_save_request(name: &str, line: &str, slot_ids: &[u32], pattern_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string_field(1, name, &mut out);
    encode_string_field(2, line, &mut out);
    let mut layout_bytes = Vec::new();
    for sid in slot_ids {
        let (mut slot_msg, mut id_msg) = (Vec::new(), Vec::new());
        encode_varint_field(1, *sid as u64, &mut id_msg);
        encode_bytes_field(1, &id_msg, &mut slot_msg);
        encode_bytes_field(1, &slot_msg, &mut layout_bytes);
    }
    encode_varint_field(3, pattern_id as u64, &mut layout_bytes);
    encode_bytes_field(3, &layout_bytes, &mut out);
    out
}

fn seq_msg(id: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    b
}

fn comp_id_msg(comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &seq_msg(comp_id as u64), &mut b);
    b
}

fn comp_lic_id_msg(char_id: u32, comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, &seq_msg(char_id as u64), &mut b);
    encode_bytes_field(2, &comp_id_msg(comp_id, is_pattern), &mut b);
    b
}

fn make_admin_grant_char_and_title(char_id: u32, title_id: u32) -> Vec<u8> {
    let (mut out, mut char_msg, mut title_msg, mut uuid_bytes) = (Vec::new(), Vec::new(), Vec::new(), [0u8; 16]);
    encode_varint_field(1, char_id as u64, &mut char_msg);
    encode_bytes_field(1, &char_msg, &mut out);
    uuid_bytes[12..16].copy_from_slice(&title_id.to_be_bytes());
    encode_bytes_field(1, &uuid_bytes, &mut title_msg);
    encode_bytes_field(2, &title_msg, &mut out);
    out
}

#[tokio::test]
async fn test_quasar_achievement_progress() {

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let cid = 90_000_821u32;
    seed_pilot(&ctx, cid as i64, 98_000_001);
    let mut sess = build_sess(60_821, cid, 98_000_001);
    let mut rx = subscribe(&ctx, &mut sess).await;

    // 1. SDE-backed definitions: 60 enabled, 8 disabled excluded
    let ach_defs = &ctx.quasar_gateway.progression.achievements.definitions;
    assert_eq!(ach_defs.len(), 60, "Must load 60 enabled SDE achievements");
    for disabled_id in [3u32, 4, 14, 37, 51, 54, 56, 65] {
        assert!(
            !ach_defs.iter().any(|d| d.id == disabled_id),
            "Disabled achievement {disabled_id} excluded"
        );
    }
    let ach2 = ach_defs.iter().find(|d| d.id == 2).expect("achievement 2");
    assert_eq!(ach2.name, "Mine Ore");
    assert_eq!(ach2.target, 3);

    // 2. Query incomplete achievement #2 via GetRequest
    let r_get1 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ident_req(2)).await;
    assert_eq!(r_get1.status_code, STATUS_OK);
    let wm1 = WireMessage::decode(&r_get1.payload.expect("payload").value).expect("decode");
    assert!(wm1.fields.iter().all(|f| f.field_number != 1), "completed_date absent");
    assert_eq!(wm1.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(1));
    assert_eq!(wm1.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(0));

    // Disabled achievement query returns STATUS_NOT_FOUND
    let r_dis = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ident_req(3)).await;
    assert_eq!(r_dis.status_code, STATUS_NOT_FOUND);

    // 3. Category lookup GetRequest
    let r_cat = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.category.api.GetRequest", make_ident_req(1)).await;
    assert_eq!(r_cat.status_code, STATUS_OK);

    // 4. Showcase score query
    let r_score = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new()).await;
    assert_eq!(r_score.status_code, STATUS_OK);

    // 5. CDN Checkpoint metadata query
    let r_cdn = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.definition.api.cdn.GetLatestRequest", Vec::new()).await;
    assert_eq!(r_cdn.status_code, STATUS_OK);
    let wm_cdn = WireMessage::decode(&r_cdn.payload.expect("cdn payload").value).expect("decode cdn");
    let cp_field = wm_cdn.fields.iter().find(|f| f.field_number == 1).expect("checkpoint");
    let cp_wm = WireMessage::decode(cp_field.value.as_bytes().expect("bytes")).expect("decode cp");
    let url_str = cp_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten()).expect("url");
    assert!(url_str.contains("/cdn/achievements/v24.1.60/"));

    // 6. Admin AddProgressRequest & notice publication
    drain(&mut rx, "");
    let mut add_prog_req = make_ident_req(2);
    encode_varint_field(2, 1, &mut add_prog_req);
    let r_prog = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.AddProgressRequest", add_prog_req).await;
    assert_eq!(r_prog.status_code, STATUS_OK);

    let notices_prog = drain(&mut rx, "eve_public.achievement");
    assert!(notices_prog.iter().any(|(name, _)| name.ends_with("ProgressedNotice")), "ProgressedNotice emitted");

    // 7. Admin CompleteRequest & CompletedNotice publication
    let r_comp = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.CompleteRequest", make_ident_req(2)).await;
    assert_eq!(r_comp.status_code, STATUS_OK);

    let notices_comp = drain(&mut rx, "eve_public.achievement");
    assert!(notices_comp.iter().any(|(name, _)| name.ends_with("CompletedNotice")), "CompletedNotice emitted");

    // 8. Verify GetRequest returns completed_date set and not_completed absent
    let r_get2 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ident_req(2)).await;
    assert_eq!(r_get2.status_code, STATUS_OK);
    let wm2 = WireMessage::decode(&r_get2.payload.expect("payload").value).expect("decode");
    assert!(wm2.fields.iter().any(|f| f.field_number == 1), "completed_date present");
    assert!(wm2.fields.iter().all(|f| f.field_number != 2), "not_completed absent");
    assert_eq!(wm2.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(ach2.target));

    // 9. Reward queries & fail-closed ClaimRequest
    let r_unclaimed = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new()).await;
    assert_eq!(r_unclaimed.status_code, STATUS_OK);

    let mut claim_req = Vec::new();
    encode_varint_field(3, 1, &mut claim_req);
    let r_claim = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.reward.api.ClaimRequest", claim_req).await;
    assert_eq!(r_claim.status_code, STATUS_PRECONDITION_FAILED);

    // 10. Admin ResetRequest returns achievement to incomplete state
    let r_reset = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.admin.ResetRequest", make_ident_req(2)).await;
    assert_eq!(r_reset.status_code, STATUS_OK);

    let r_get3 = quasar_rpc(&ctx, &mut sess, "eve_public.achievement.api.GetRequest", make_ident_req(2)).await;
    assert_eq!(r_get3.status_code, STATUS_OK);
    let wm3 = WireMessage::decode(&r_get3.payload.expect("payload").value).expect("decode");
    assert_eq!(wm3.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(1));
}

#[tokio::test]
async fn test_quasar_career_goals() {

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let cid = 90_000_822u32;
    seed_pilot(&ctx, cid as i64, 98_000_001);
    let mut sess = build_sess(60_822, cid, 98_000_001);
    let mut rx = subscribe(&ctx, &mut sess).await;

    // 1. GetAllRequest returns all 196 goals and milestones
    let r_all = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetAllRequest", Vec::new()).await;
    assert_eq!(r_all.status_code, STATUS_OK);
    let r_all_payload = r_all.payload.expect("GetAllResponse payload");
    assert_eq!(r_all_payload.normalized_type_name(), "eve_public.career.goal.api.GetAllResponse");

    let wm_all = WireMessage::decode(&r_all_payload.value).expect("decode GetAllResponse");
    let goal_fields: Vec<_> = wm_all.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(goal_fields.len(), 196, "Must return 188 career goals + 8 milestones");

    // Verify first goal has 16-byte UUID Identifier
    let g1_bytes = goal_fields[0].value.as_bytes().expect("g1 bytes");
    let g1_wm = WireMessage::decode(g1_bytes).expect("decode g1");
    let id_field = g1_wm.fields.iter().find(|f| f.field_number == 1).expect("id field");
    let id_wm = WireMessage::decode(id_field.value.as_bytes().expect("id bytes")).expect("decode id");
    let uuid_bytes = id_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("uuid");
    assert_eq!(uuid_bytes.len(), 16, "Career goal identifier UUID must be 16 bytes");

    // 2. GetDefinitionsRequest returns all 196 definitions with attributes
    let r_defs = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.GetDefinitionsRequest", Vec::new()).await;
    assert_eq!(r_defs.status_code, STATUS_OK);
    let r_defs_payload = r_defs.payload.expect("GetDefinitionsResponse payload");
    let wm_defs = WireMessage::decode(&r_defs_payload.value).expect("decode GetDefinitionsResponse");
    let def_fields: Vec<_> = wm_defs.fields.iter().filter(|f| f.field_number == 1).collect();
    assert_eq!(def_fields.len(), 196, "Must return 196 goal definitions");

    // 3. Admin ProgressRequest emits ProgressedNotice
    drain(&mut rx, "");
    let enforcer_uuid = [0x43, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
    let mut prog_req = Vec::new();
    let mut id_inner = Vec::new();
    encode_bytes_field(1, &enforcer_uuid, &mut id_inner);
    encode_bytes_field(1, &id_inner, &mut prog_req);
    encode_varint_field(2, 5, &mut prog_req);

    let r_prog = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.ProgressRequest", prog_req).await;
    assert_eq!(r_prog.status_code, STATUS_OK);

    let notices_prog = drain(&mut rx, "eve_public.career");
    assert!(notices_prog.iter().any(|(name, _)| name.ends_with("ProgressedNotice")), "ProgressedNotice emitted");

    // 4. Admin CompleteRequest emits CompletedNotice
    let mut comp_req = Vec::new();
    let mut comp_id_inner = Vec::new();
    encode_bytes_field(1, &enforcer_uuid, &mut comp_id_inner);
    encode_bytes_field(1, &comp_id_inner, &mut comp_req);

    let r_comp = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.admin.CompleteRequest", comp_req).await;
    assert_eq!(r_comp.status_code, STATUS_OK);

    let notices_comp = drain(&mut rx, "eve_public.career");
    assert!(notices_comp.iter().any(|(name, _)| name.ends_with("CompletedNotice")), "CompletedNotice emitted");

    // 5. ClaimRewardRequest credits character balance and writes wallet journal
    let bal_before = ctx.db.with_snapshot(|s| s.characters.get(&(cid as i64)).unwrap().balance);
    let mut claim_req = Vec::new();
    let mut claim_id_inner = Vec::new();
    encode_bytes_field(1, &enforcer_uuid, &mut claim_id_inner);
    encode_bytes_field(1, &claim_id_inner, &mut claim_req);

    let r_claim = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.ClaimRewardRequest", claim_req).await;
    assert_eq!(r_claim.status_code, STATUS_OK);

    let (bal_after, has_journal, valid_meta) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&(cid as i64)).expect("character");
        let entry = snap.wallet_journal.iter().find(|j| {
            j.character_id == cid as i64 && j.entry_type == "CAREER_PROGRAM_REWARD"
        });
        let meta = entry.and_then(|e| snap.wallet_journal_meta.get(&e.entry_id));
        let valid = meta.is_some_and(|m| m.ref_type_id == 170 && m.first_party_id == 1_000_125);
        (ch.balance, entry.is_some(), valid)
    });
    assert!(bal_after > bal_before, "Balance must increase after claiming reward");
    assert!(has_journal, "WalletJournal CAREER_PROGRAM_REWARD must be recorded");
    assert!(valid_meta, "WalletJournalMeta ref_type_id 170 / CONCORD recorded");

    // 6. Complete remaining Enforcer goals and claim graduation milestone 3 (100,000 SP)
    let enforcer_goals = ctx.quasar_gateway.progression.career_goals.get_or_init_goals(cid);
    for g in enforcer_goals {
        if g.def.career_id == 3 && !g.completed {
            let _ = ctx.quasar_gateway.progression.career_goals.mark_completed(cid, &g.uuid, ctx.now_unix());
        }
    }

    let milestones = ctx.quasar_gateway.progression.career_goals.get_character_milestones(cid);
    let m3 = milestones.iter().find(|(m, _, _)| m.milestone_id == 3).expect("Enforcer milestone 3");
    assert!(m3.1, "Enforcer milestone reached");
    assert!(!m3.2, "Enforcer milestone not yet claimed");

    let sp_before = ctx.db.with_snapshot(|s| s.characters.get(&(cid as i64)).unwrap().free_skill_points);
    let mut ms_claim_req = Vec::new();
    let mut ms_id_inner = Vec::new();
    encode_bytes_field(
        1,
        &eve_server::services::quasar_career_goals::career_milestone_uuid(3),
        &mut ms_id_inner,
    );
    encode_bytes_field(1, &ms_id_inner, &mut ms_claim_req);

    let r_ms_claim = quasar_rpc(&ctx, &mut sess, "eve_public.career.goal.api.ClaimRewardRequest", ms_claim_req).await;
    assert_eq!(r_ms_claim.status_code, STATUS_OK);

    let sp_after = ctx.db.with_snapshot(|s| s.characters.get(&(cid as i64)).unwrap().free_skill_points);
    assert_eq!(sp_after - sp_before, 100_000.0, "Milestone claim awards 100,000 SP");

    let milestones_post = ctx.quasar_gateway.progression.career_goals.get_character_milestones(cid);
    let m3_post = milestones_post.iter().find(|(m, _, _)| m.milestone_id == 3).unwrap();
    assert!(m3_post.2, "Milestone 3 marked claimed");

    let has_ms_blob = ctx.db.with_snapshot(|snap| {
        snap.quasar_state_blobs.contains_key("quasar_career_milestones")
    });
    assert!(has_ms_blob, "Milestone claims persist to quasar_career_milestones blob");
}

#[tokio::test]
async fn test_quasar_cosmetic_catalog() {

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let cid = 90_000_823u32;
    seed_pilot(&ctx, cid as i64, 98_000_001);
    let mut sess = build_sess(60_823, cid, 98_000_001);
    let mut rx = subscribe(&ctx, &mut sess).await;

    // 1. Cosmetic Market queries: Broker fees, tax rate, and component/skin listings
    let mut pub_target = Vec::new();
    encode_varint_field(1, 1, &mut pub_target);
    let mut req_pub = Vec::new();
    encode_bytes_field(1, &pub_target, &mut req_pub);

    let r_bf = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.market.api.GetBrokerFeesRequest", req_pub.clone()).await;
    assert_eq!(r_bf.status_code, STATUS_OK);

    let r_tax = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.market.api.GetTaxRateRequest", req_pub).await;
    assert_eq!(r_tax.status_code, STATUS_OK);

    let r_comp_listings = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.market.component.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(r_comp_listings.status_code, STATUS_OK);

    let r_skin_listings = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new()).await;
    assert_eq!(r_skin_listings.status_code, STATUS_OK);

    // 2. SKINR sequencing pricing and active jobs
    let r_tier_pricing = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest", Vec::new()).await;
    assert_eq!(r_tier_pricing.status_code, STATUS_OK);

    let r_tier_dur = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest", Vec::new()).await;
    assert_eq!(r_tier_dur.status_code, STATUS_OK);

    let r_early = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest", Vec::new()).await;
    assert_eq!(r_early.status_code, STATUS_OK);

    let r_jobs = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest", Vec::new()).await;
    assert_eq!(r_jobs.status_code, STATUS_OK);

    // 3. SKINR design draft lifecycle (Capacity, Save, Get, GetAllSaved, Delete)
    let r_cap = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest", Vec::new()).await;
    assert_eq!(r_cap.status_code, STATUS_OK);

    let save_req = make_skinr_save_request("Closure Gold", "Closure Line", &[501, 502], 3);
    let r_save = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest", save_req).await;
    assert_eq!(r_save.status_code, STATUS_OK);
    let save_wm = WireMessage::decode(&r_save.payload.expect("SaveResponse").value).expect("decode save");
    let id_field = save_wm.fields.iter().find(|f| f.field_number == 1).expect("id field");
    let id_wm = WireMessage::decode(id_field.value.as_bytes().unwrap()).expect("decode id");
    let draft_uuid_bytes = id_wm.fields.iter().find(|f| f.field_number == 1).unwrap().value.as_bytes().unwrap();
    assert_eq!(draft_uuid_bytes.len(), 16);
    let mut draft_uuid = [0u8; 16];
    draft_uuid.copy_from_slice(draft_uuid_bytes);

    let r_get_draft = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", make_uuid_req(&draft_uuid)).await;
    assert_eq!(r_get_draft.status_code, STATUS_OK);

    let r_all_saved = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest", Vec::new()).await;
    assert_eq!(r_all_saved.status_code, STATUS_OK);

    let r_del = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest", make_uuid_req(&draft_uuid)).await;
    assert_eq!(r_del.status_code, STATUS_OK);

    let r_get_deleted = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest", make_uuid_req(&draft_uuid)).await;
    assert_eq!(r_get_deleted.status_code, STATUS_NOT_FOUND);

    // 4. SKINR Component licenses (GetOwned, Admin Grant, Get, Admin Revoke)
    let r_owned_comp = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GetOwnedRequest", Vec::new()).await;
    assert_eq!(r_owned_comp.status_code, STATUS_OK);

    let skinr_cat = eve_server::services::quasar_cosmetics_skinr::SkinrStaticCatalog::load(&ctx.sde);
    let coating_comp = skinr_cat.components.values().find(|c| !c.is_pattern()).expect("coating comp");
    let comp_id = coating_comp.component_id;

    drain(&mut rx, "");
    let mut grant_lic_req = Vec::new();
    encode_bytes_field(1, &comp_id_msg(comp_id, false), &mut grant_lic_req);
    encode_varint_field(2, 5, &mut grant_lic_req);

    let r_grant_lic = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest", grant_lic_req).await;
    assert_eq!(r_grant_lic.status_code, STATUS_OK);

    let comp_notices = drain(&mut rx, "eve_public.cosmetic");
    assert!(comp_notices.iter().any(|(name, _)| name.ends_with("GrantedNotice")), "GrantedNotice emitted");

    let mut get_lic_req = Vec::new();
    encode_bytes_field(1, &comp_lic_id_msg(cid, comp_id, false), &mut get_lic_req);
    let r_get_lic = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", get_lic_req.clone()).await;
    assert_eq!(r_get_lic.status_code, STATUS_OK);

    let r_rev_lic = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest", get_lic_req.clone()).await;
    assert_eq!(r_rev_lic.status_code, STATUS_OK);

    let r_get_revoked = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest", get_lic_req).await;
    assert_eq!(r_get_revoked.status_code, STATUS_NOT_FOUND);

    // 5. Character titles lifecycle (GetOwned, Admin Grant, Equip, GetEquipped)
    let r_titles_owned = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.character.title.api.GetOwnedRequest", Vec::new()).await;
    assert_eq!(r_titles_owned.status_code, STATUS_OK);

    drain(&mut rx, "");
    let title_id = 201u32;
    let grant_title_req = make_admin_grant_char_and_title(cid, title_id);
    let r_grant_title = quasar_rpc(&ctx, &mut sess, "eve.cosmetic.character.title.api.admin.GrantRequest", grant_title_req).await;
    assert_eq!(r_grant_title.status_code, STATUS_OK);

    let title_notices = drain(&mut rx, "eve");
    assert!(title_notices.iter().any(|(name, _)| name.ends_with("GrantedNotice")), "GrantedNotice emitted");

    let r_eq_title = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.character.title.api.EquipRequest", make_ident_req(title_id)).await;
    assert_eq!(r_eq_title.status_code, STATUS_OK);

    let equip_notices = drain(&mut rx, "eve_public.cosmetic");
    assert!(equip_notices.iter().any(|(name, _)| name.ends_with("SetNotice")), "SetNotice emitted");

    // 6. Custom skin license queries
    let r_skin_owned = quasar_rpc(&ctx, &mut sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest", Vec::new()).await;
    assert_eq!(r_skin_owned.status_code, STATUS_OK);
}
