//! Adversarial persistence and cache-coherency integration tests for:
//! 1. Cross-character 3rd-party SKIN entitlement redemption surviving live `SkinrCosmeticsState`
//!    mutations (`ActivateRequest`, `ApplyRequest`, `UnapplyRequest`, `ConsumeRequest`,
//!    `InitiateRequest` + `CompleteEarlyRequest`) and cold SQLite reloads.
//! 2. Revoked corporation and alliance ship hull emblems (`prune_revoked_logo_entitlement`)
//!    never resurrecting across subsequent live `CosmeticsLiveState` mutations (`palette.SetRequest`,
//!    structure `paintwork.license.IssueRequest` + `RevokeRequest`) or SQLite reloads.

#![forbid(unsafe_code)]

use crate::common;
use crate::fleet_boss_formations_corp_projects_live::quasar_call;
use crate::quasar_sov_equinox_hub::{field, fields};
use eve_db::{
    corp_roles, CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents,
    UpwellStructureRecord, WorldRepository,
};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, WireMessage};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::quasar_cosmetics_live::PARAGON_CORP_ID;
use eve_server::services::quasar_cosmetics_skinr::{
    SkinrStaticCatalog, COMP_CAT_MATERIAL, COMP_CAT_METALLIC,
};
use eve_server::services::quasar_entitlement_membership::encode_logo_identifier;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const RIFTER_TYPE_ID: u32 = 587;

fn shared_real_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| {
        SdeRegistry::open(SdeRegistry::default_db_path())
            .map(Arc::new)
            .unwrap_or_else(|_| Arc::new(SdeRegistry::with_canonical_seed()))
    }))
}

struct TempSqliteFile {
    path: PathBuf,
}

impl TempSqliteFile {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("evomu_{tag}_{}_{}.sqlite", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }
}

impl Drop for TempSqliteFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(format!("{}-wal", self.path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.path.display()));
    }
}

fn sess_for(db: &WorldRepository, sid: u64, cid: i64, role: u64, crole: u64) -> SessionState {
    let (uid, corp_id, ship_id) = db.with_snapshot(|s| {
        let c = &s.characters[&cid];
        (c.user_id, c.corporation_id, c.active_ship_id)
    });
    let mut s = SessionState::new(sid);
    s.user_id = Some(uid);
    s.character_id = Some(cid);
    s.corporation_id = Some(corp_id);
    s.ship_id = Some(ship_id);
    s.solar_system_id = Some(JITA_SYS);
    s.station_id = Some(JITA_STATION);
    s.role = role;
    s.corp_role = crole;
    s
}

fn encode_slot(slot_id: u32, comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let (mut s_idx, mut slot_msg, mut c_idx, mut cfg, mut outer) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, u64::from(slot_id), &mut s_idx);
    encode_bytes_field(1, &s_idx, &mut slot_msg);
    encode_varint_field(1, u64::from(comp_id), &mut c_idx);
    encode_bytes_field(1, &c_idx, &mut cfg);
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &cfg, &mut outer);
    encode_bytes_field(2, &outer, &mut slot_msg);
    slot_msg
}

fn encode_comp_id(comp_id: u32, is_pattern: bool) -> Vec<u8> {
    let (mut cid, mut out) = (Vec::new(), Vec::new());
    encode_varint_field(1, u64::from(comp_id), &mut cid);
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &cid, &mut out);
    out
}

fn encode_layout(ship_tid: u32, coat_id: u32, sec_id: u32) -> Vec<u8> {
    let (mut layout, mut hull) = (Vec::new(), Vec::new());
    encode_bytes_field(1, &encode_slot(1, coat_id, false), &mut layout);
    encode_bytes_field(1, &encode_slot(2, sec_id, false), &mut layout);
    encode_varint_field(1, u64::from(ship_tid), &mut hull);
    encode_bytes_field(2, &hull, &mut layout);
    encode_varint_field(3, 1, &mut layout);
    layout
}

fn encode_skin_lic_id(char_id: u32, skin_hex: &str) -> Vec<u8> {
    let (mut ch, mut sk, mut id) = (Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, u64::from(char_id), &mut ch);
    encode_string_field(1, skin_hex, &mut sk);
    encode_bytes_field(1, &ch, &mut id);
    encode_bytes_field(2, &sk, &mut id);
    id
}

fn create_and_deliver_skin_entitlement(
    ctx: &ServerContext,
    gm_sess: &SessionState,
    holder_cid: u32,
    skin_hex: &str,
    qty: u64,
) -> Vec<u8> {
    let (mut sk, mut sk_unit, mut unit_msg, mut create_req) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_string_field(1, skin_hex, &mut sk);
    encode_bytes_field(1, &sk, &mut sk_unit);
    encode_bytes_field(10, &sk_unit, &mut unit_msg);
    encode_bytes_field(4, &unit_msg, &mut create_req);
    let create_rsp = quasar_call(ctx, gm_sess, "eve.assetholding.asset.api.CreateRequest", create_req);
    assert_eq!(create_rsp.status_code, STATUS_OK);
    let asset_id_msg = field(&create_rsp.payload.unwrap().value, 1).expect("asset id");
    let asset_uuid = field(&asset_id_msg, 1).expect("asset uuid");

    let (mut ch, mut holder, mut asset, mut grp, mut ent_id) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    encode_varint_field(1, u64::from(holder_cid), &mut ch);
    encode_bytes_field(1, &ch, &mut holder);
    encode_bytes_field(1, &asset_uuid, &mut asset);
    encode_string_field(1, "skinr_reward", &mut grp);
    encode_bytes_field(1, &holder, &mut ent_id);
    encode_bytes_field(2, &asset, &mut ent_id);
    encode_bytes_field(3, &grp, &mut ent_id);

    let (mut entry, mut deliv_req) = (Vec::new(), Vec::new());
    encode_bytes_field(2, &ent_id, &mut entry);
    encode_varint_field(10, qty, &mut entry);
    encode_bytes_field(2, &entry, &mut deliv_req);
    let deliv_rsp = quasar_call(ctx, gm_sess, "eve.assetholding.entitlement.api.DirectDeliveryRequest", deliv_req);
    assert_eq!(deliv_rsp.status_code, STATUS_OK);
    ent_id
}

#[tokio::test]
async fn cross_character_redeemed_skin_survives_recipient_skinr_mutations_and_sqlite_reload() {
    // parity: feat:cosmetics.skinr-designer, feat:cosmetics.skinr-sequencing-studio, feat:quasar.cosmetic
    eve_testkit::attest(&[
        "feat:cosmetics.skinr-designer",
        "feat:cosmetics.skinr-sequencing-studio",
        "feat:quasar.cosmetic",
    ]);

    let tmp_db = TempSqliteFile::new("skinr_split_brain");
    let sde = shared_real_sde();
    let creator_id = 91_800_101_i64;
    let recipient_id = 91_800_102_i64;

    let cat = SkinrStaticCatalog::load(&sde);
    let coat = cat
        .components
        .values()
        .find(|c| c.category == COMP_CAT_MATERIAL && c.disallowed_slots.is_empty())
        .expect("material component")
        .component_id;
    let met = cat
        .components
        .values()
        .find(|c| c.category == COMP_CAT_METALLIC && c.disallowed_slots.is_empty())
        .expect("metallic component")
        .component_id;
    let consumable_item_tid = cat.item_type_to_component.keys().copied().next();

    let (creator_skin_hex, fresh_skin_hex) = {
        let db = Arc::new(WorldRepository::open_sqlite(&tmp_db.path).expect("open sqlite"));
        db.provision_gm_account("skinr_creator", "pw", creator_id, "Creator Pilot");
        db.provision_gm_account("skinr_recipient", "pw", recipient_id, "Recipient Pilot");
        let recipient_ship = db.with_snapshot(|s| s.characters[&recipient_id].active_ship_id);

        let consumable_item_id = 1_800_000_555_i64;
        if let Some(tid) = consumable_item_tid {
            db.with_mut_snapshot(|s| {
                s.items.insert(
                    consumable_item_id,
                    InventoryItemRecord {
                        item_id: consumable_item_id,
                        type_id: tid,
                        owner_id: recipient_id,
                        location_id: JITA_STATION,
                        flag: InventoryFlag::Hangar,
                        quantity: 2,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
            });
        }

        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        common::pin_clock(&ctx);

        let gm_creator = sess_for(&db, 8201, creator_id, service_roles::ROLE_GM_DEFAULT, 0);
        let creator_sess = sess_for(&db, 8202, creator_id, service_roles::ROLE_PLAYER_DEFAULT, 0);
        let gm_recipient = sess_for(&db, 8203, recipient_id, service_roles::ROLE_GM_DEFAULT, 0);
        let recipient_sess = sess_for(&db, 8204, recipient_id, service_roles::ROLE_PLAYER_DEFAULT, 0);

        for s in [&gm_creator, &gm_recipient] {
            for cid in [coat, met] {
                let mut req = Vec::new();
                encode_bytes_field(1, &encode_comp_id(cid, false), &mut req);
                encode_varint_field(2, 10, &mut req);
                assert_eq!(
                    quasar_call(
                        &ctx,
                        s,
                        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
                        req,
                    )
                    .status_code,
                    STATUS_OK
                );
            }
        }

        // Creator sequences a custom Rifter SKIN design while SkinrCosmeticsState is cached live.
        let mut init_req = Vec::new();
        encode_string_field(1, "Valkyrie Crimson Rifter", &mut init_req);
        encode_string_field(2, "Valkyrie Custom Line", &mut init_req);
        encode_bytes_field(3, &encode_layout(RIFTER_TYPE_ID, coat, met), &mut init_req);
        encode_varint_field(5, 1, &mut init_req);
        let init_rsp = quasar_call(
            &ctx,
            &creator_sess,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            init_req,
        );
        assert_eq!(init_rsp.status_code, STATUS_OK);
        let skin_id_bytes = field(&field(&init_rsp.payload.unwrap().value, 2).unwrap(), 1).unwrap();
        let creator_hex = String::from_utf8(field(&skin_id_bytes, 1).unwrap()).unwrap();
        assert_eq!(
            quasar_call(
                &ctx,
                &creator_sess,
                "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
                Vec::new(),
            )
            .status_code,
            STATUS_OK
        );

        // Deliver & redeem BOTH creator's custom SKIN (2 charges) AND a brand-new reward SKIN to Recipient.
        let fresh_hex = "redeemed_paragon_rifter_hex_01".to_string();
        for ent_id in [
            create_and_deliver_skin_entitlement(&ctx, &gm_creator, recipient_id as u32, &creator_hex, 2),
            create_and_deliver_skin_entitlement(&ctx, &gm_creator, recipient_id as u32, &fresh_hex, 1),
        ] {
            let mut redeem_req = Vec::new();
            encode_bytes_field(1, &ent_id, &mut redeem_req);
            assert_eq!(
                quasar_call(
                    &ctx,
                    &recipient_sess,
                    "eve_public.assetholding.entitlement.api.RedeemRequest",
                    redeem_req,
                )
                .status_code,
                STATUS_OK
            );
        }

        // Execute adversarial SkinrCosmeticsState mutations as Recipient (`state.save()`):
        let mut act_req = Vec::new();
        encode_bytes_field(1, &encode_skin_lic_id(recipient_id as u32, &creator_hex), &mut act_req);
        assert_eq!(
            quasar_call(&ctx, &recipient_sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest", act_req).status_code,
            STATUS_OK
        );

        let (mut ship_seq, mut apply_req) = (Vec::new(), Vec::new());
        encode_varint_field(1, recipient_ship as u64, &mut ship_seq);
        encode_bytes_field(1, &encode_skin_lic_id(recipient_id as u32, &creator_hex), &mut apply_req);
        encode_bytes_field(2, &ship_seq, &mut apply_req);
        for req_name in [
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyRequest",
            "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest",
        ] {
            assert_eq!(quasar_call(&ctx, &recipient_sess, req_name, apply_req.clone()).status_code, STATUS_OK);
        }

        if consumable_item_tid.is_some() {
            let (mut item_seq, mut consume_req) = (Vec::new(), Vec::new());
            encode_varint_field(1, consumable_item_id as u64, &mut item_seq);
            encode_bytes_field(1, &item_seq, &mut consume_req);
            encode_varint_field(2, 1, &mut consume_req);
            assert_eq!(
                quasar_call(&ctx, &recipient_sess, "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest", consume_req).status_code,
                STATUS_OK
            );
        }

        let mut rec_job = Vec::new();
        encode_string_field(1, "Recipient Cobalt Rifter", &mut rec_job);
        encode_string_field(2, "Cobalt Line", &mut rec_job);
        encode_bytes_field(3, &encode_layout(RIFTER_TYPE_ID, met, coat), &mut rec_job);
        encode_varint_field(5, 1, &mut rec_job);
        assert_eq!(
            quasar_call(&ctx, &recipient_sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest", rec_job).status_code,
            STATUS_OK
        );
        assert_eq!(
            quasar_call(&ctx, &recipient_sess, "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest", Vec::new()).status_code,
            STATUS_OK
        );

        (creator_hex, fresh_hex)
    };

    // Cold reload from SQLite into a brand-new WorldRepository + ServerContext and verify zero data loss.
    let reloaded_db = Arc::new(WorldRepository::open_sqlite(&tmp_db.path).expect("reopen sqlite"));
    let recipient_sess = sess_for(&reloaded_db, 8301, recipient_id, service_roles::ROLE_PLAYER_DEFAULT, 0);
    let reloaded_ctx = ServerContext::with_sde_and_repository(sde, reloaded_db);

    for hex in [&creator_skin_hex, &fresh_skin_hex] {
        let mut get_skin = Vec::new();
        encode_string_field(1, hex, &mut get_skin);
        assert_eq!(
            quasar_call(&reloaded_ctx, &recipient_sess, "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest", get_skin).status_code,
            STATUS_OK,
            "Redeemed SKIN design {hex} must survive recipient mutations and SQLite reload"
        );
        let mut get_lic = Vec::new();
        encode_bytes_field(1, &encode_skin_lic_id(recipient_id as u32, hex), &mut get_lic);
        assert_eq!(
            quasar_call(&reloaded_ctx, &recipient_sess, "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest", get_lic).status_code,
            STATUS_OK,
            "Redeemed SKIN license {hex} must survive recipient mutations and SQLite reload"
        );
    }
    let owned_rsp = quasar_call(
        &reloaded_ctx,
        &recipient_sess,
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        Vec::new(),
    );
    assert_eq!(owned_rsp.status_code, STATUS_OK);
    assert!(fields(&owned_rsp.payload.unwrap().value, 1).len() >= 3);
}

#[tokio::test]
async fn revoked_hull_emblems_never_resurrect_after_live_cosmetics_mutations_and_sqlite_reload() {
    // parity: feat:quasar.cosmetic, rpc:shipCosmeticsMgr.GetEnabledCosmetics
    eve_testkit::attest(&["feat:quasar.cosmetic"]);

    let tmp_db = TempSqliteFile::new("logo_revocation");
    let sde = shared_real_sde();
    let cid = 91_800_201_i64;
    let corp_id = 98_800_201_i64;
    let alliance_id = 99_800_201_i64;
    let struct_id = 1_030_800_201_001_i64;

    let ship_id = {
        let db = Arc::new(WorldRepository::open_sqlite(&tmp_db.path).expect("open sqlite"));
        db.provision_gm_account("emblem_director", "pw", cid, "Emblem Director");
        let ship_id = db.with_mut_snapshot(|s| {
            let c = s.characters.get_mut(&cid).unwrap();
            c.corporation_id = corp_id;
            c.alliance_id = Some(alliance_id);
            let sid = c.active_ship_id;
            s.corporations.insert(
                corp_id,
                CorporationRecord {
                    corporation_id: corp_id,
                    name: "Vanguard Cosmetics Corp".into(),
                    ticker: "VGCC".into(),
                    ceo_id: cid,
                    alliance_id: Some(alliance_id),
                    tax_rate: 0.05,
                    master_wallet: IskCents(1_000_000_000),
                    members: [cid].into_iter().collect(),
                    member_roles: [(cid, corp_roles::ROLE_DIRECTOR)].into_iter().collect(),
                    applications: HashMap::new(),
                    wallet_divisions: CorporationRecord::default_wallet_divisions(IskCents(1_000_000_000)),
                    description: String::new(),
                    url: String::new(),
                    hq_station_id: Some(JITA_STATION),
                },
            );
            s.structures.insert(
                struct_id,
                UpwellStructureRecord {
                    structure_id: struct_id,
                    type_id: 35832,
                    owner_corp_id: corp_id,
                    solar_system_id: JITA_SYS,
                    name: "Jita Paintwork Citadel".into(),
                    state: "ShieldVulnerable".into(),
                    position: [0.0; 3],
                    quantum_core_installed: true,
                    fuel_blocks: 100,
                    shield_hp: 1.0,
                    armor_hp: 1.0,
                    hull_hp: 1.0,
                    reinforced_until_unix: 0,
                    last_damage_tick: 0,
                    last_damage_unix: 0,
                    self_destruct_at_unix: None,
                },
            );
            s.corp_lp_balances.insert((corp_id, PARAGON_CORP_ID), 25_000);
            sid
        });

        let ctx = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db));
        common::pin_clock(&ctx);
        let mut director = sess_for(&db, 8501, cid, service_roles::ROLE_PLAYER_DEFAULT, corp_roles::ROLE_DIRECTOR);
        let gm = sess_for(&db, 8502, cid, service_roles::ROLE_GM_DEFAULT, corp_roles::ROLE_DIRECTOR);

        let mut grant_body = Vec::new();
        encode_bytes_field(1, &encode_logo_identifier(cid as u64, u64::from(RIFTER_TYPE_ID)), &mut grant_body);
        for req_name in [
            "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest",
            "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest",
        ] {
            assert_eq!(quasar_call(&ctx, &gm, req_name, grant_body.clone()).status_code, STATUS_OK);
        }

        let mut ship_seq = Vec::new();
        encode_varint_field(1, ship_id as u64, &mut ship_seq);
        for (slot_idx, attr_field) in [(0u64, 1u32), (1u64, 2u32)] {
            let (mut logo_id, mut attr, mut disp) = (Vec::new(), Vec::new(), Vec::new());
            encode_bytes_field(1, &ship_seq, &mut logo_id);
            encode_varint_field(2, slot_idx, &mut logo_id);
            encode_bytes_field(attr_field, &[], &mut attr);
            encode_bytes_field(1, &logo_id, &mut disp);
            encode_bytes_field(2, &attr, &mut disp);
            assert_eq!(
                quasar_call(&ctx, &director, "eve_public.cosmetic.ship.logo.DisplayRequest", disp).status_code,
                STATUS_OK
            );
        }

        let enabled_before = ctx
            .dispatch_rpc(&mut director, "shipCosmeticsMgr", None, "GetEnabledCosmetics", &[PyRep::Integer(ship_id)], &HashMap::new(), None)
            .await
            .expect("GetEnabledCosmetics");
        assert_eq!(
            enabled_before,
            PyRep::Dict(vec![(PyRep::Integer(0), PyRep::Integer(2)), (PyRep::Integer(1), PyRep::Integer(1))])
        );

        for req_name in [
            "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
            "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest",
        ] {
            assert_eq!(quasar_call(&ctx, &gm, req_name, grant_body.clone()).status_code, STATUS_OK);
        }
        assert_eq!(
            ctx.dispatch_rpc(&mut director, "shipCosmeticsMgr", None, "GetEnabledCosmetics", &[PyRep::Integer(ship_id)], &HashMap::new(), None)
                .await
                .unwrap(),
            PyRep::Dict(Vec::new())
        );

        // Mutate CosmeticsLiveState via palette.SetRequest + paintwork.license.IssueRequest + RevokeRequest.
        let (mut rgb, mut pal_attrs, mut pal_req) = (Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, 25, &mut rgb);
        encode_varint_field(2, 120, &mut rgb);
        encode_varint_field(3, 210, &mut rgb);
        encode_bytes_field(1, &rgb, &mut pal_attrs);
        encode_bytes_field(1, &pal_attrs, &mut pal_req);
        assert_eq!(
            quasar_call(&ctx, &director, "eve_public.cosmetic.corporation.palette.api.SetRequest", pal_req).status_code,
            STATUS_OK
        );

        let (mut slot1, mut slots_cfg, mut dur_msg, mut st_seq, mut issue_req) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
        encode_varint_field(1, 101, &mut slot1);
        encode_bytes_field(1, &slot1, &mut slots_cfg);
        encode_varint_field(1, 2_592_000, &mut dur_msg);
        encode_varint_field(1, struct_id as u64, &mut st_seq);
        encode_bytes_field(1, &slots_cfg, &mut issue_req);
        encode_bytes_field(2, &dur_msg, &mut issue_req);
        encode_bytes_field(3, &st_seq, &mut issue_req);
        let iss_rsp = quasar_call(&ctx, &director, "eve_public.cosmetic.structure.paintwork.license.api.IssueRequest", issue_req);
        assert_eq!(iss_rsp.status_code, STATUS_OK);
        let entry_wm = WireMessage::decode(
            WireMessage::decode(&iss_rsp.payload.unwrap().value).unwrap().fields[0].value.as_bytes().unwrap(),
        )
        .unwrap();
        let mut rev_paint = Vec::new();
        encode_bytes_field(1, entry_wm.fields[0].value.as_bytes().unwrap(), &mut rev_paint);
        assert_eq!(
            quasar_call(&ctx, &director, "eve_public.cosmetic.structure.paintwork.license.api.RevokeRequest", rev_paint).status_code,
            STATUS_OK
        );

        assert_eq!(
            ctx.dispatch_rpc(&mut director, "shipCosmeticsMgr", None, "GetEnabledCosmetics", &[PyRep::Integer(ship_id)], &HashMap::new(), None)
                .await
                .unwrap(),
            PyRep::Dict(Vec::new()),
            "Revoked hull emblems must never resurrect after subsequent CosmeticsLiveState saves"
        );
        ship_id
    };

    let reloaded_db = Arc::new(WorldRepository::open_sqlite(&tmp_db.path).expect("reopen sqlite"));
    let mut director = sess_for(&reloaded_db, 8601, cid, service_roles::ROLE_PLAYER_DEFAULT, corp_roles::ROLE_DIRECTOR);
    let reloaded_ctx = ServerContext::with_sde_and_repository(sde, reloaded_db);
    assert_eq!(
        reloaded_ctx
            .dispatch_rpc(&mut director, "shipCosmeticsMgr", None, "GetEnabledCosmetics", &[PyRep::Integer(ship_id)], &HashMap::new(), None)
            .await
            .unwrap(),
        PyRep::Dict(Vec::new())
    );
    assert_eq!(
        quasar_call(&reloaded_ctx, &director, "eve_public.cosmetic.corporation.palette.api.GetOwnRequest", Vec::new()).status_code,
        STATUS_OK
    );
}
