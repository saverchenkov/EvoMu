//! Centralized global one-time startup seed orchestration (`#1542`).
//!
//! Global one-time seeds (store catalog, cosmetics market component listings,
//! shipcaster SDE launchers, legacy corporation recruitment ad migration, social
//! mailing-list hydration, and rookie overview preset/MOTD) execute once at
//! gateway / `ServerContext` startup rather than lazily inside RPC handlers or
//! state getters.
#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::quasar_cosmetics_market::{seed_component_listings_if_empty, BLOB_KEY};
use crate::services::quasar_cosmetics_market_types::CosmeticsMarketData;
use eve_db::WorldRepository;
use eve_sde::SdeRegistry;

/// Seeds gateway-owned global catalogs once before core Quasar handlers register.
///
/// Honors `suppress_auto_store_seed_for_db` when `ServerContext::from_config_with_static_data`
/// disables `seed_demo` (`#1542`). Never overwrites existing store catalogs or non-empty
/// persisted component listings.
pub(crate) fn seed_gateway_startup_globals(db: &WorldRepository, sde: &SdeRegistry) {
    if !crate::services::quasar_payment_store::is_auto_store_seed_suppressed_for(db) {
        db.with_mut_snapshot(|snap| {
            crate::services::quasar_payment_store_types::ensure_store_catalog_seeded(snap, sde);
        });
    }

    let mut data: CosmeticsMarketData = db.with_snapshot(|snap| {
        snap.quasar_state_blobs
            .get(BLOB_KEY)
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default()
    });
    if seed_component_listings_if_empty(&mut data, sde) {
        if let Ok(json) = serde_json::to_string(&data) {
            db.with_mut_snapshot(|snap| {
                snap.quasar_state_blobs.insert(BLOB_KEY.into(), json);
            });
        }
    }
}

/// Runs global one-time seeds and startup migrations once during `ServerContext` init.
///
/// Ordered deterministically after `hydrate_social_state_from_db` and
/// `hydrate_server_runtime_from_db` and skips DB write transactions when persisted
/// state is already populated (`#1542`).
pub(crate) fn run_startup_global_seeds(ctx: &ServerContext) {
    {
        let mut guard = ctx
            .cluster_misc_state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.ensure_sde_initialized(&ctx.sde);
    }

    let needs_legacy_rec_ad_migration = ctx.db.with_snapshot(|snap| {
        snap.fleet_and_social_ops.corp_recruitment_ads.is_empty()
            && snap
                .corp_governance
                .values()
                .any(|state| !state.recruitment_ads.is_empty())
    });
    if needs_legacy_rec_ad_migration {
        ctx.db.with_mut_snapshot(|snap| {
            crate::services::fleet_social::fleet_social_corp_gov::ensure_corp_rec_ads_populated_in_snap(
                snap,
            );
        });
    }

    crate::services::fleet_social::fleet_social_persistence::ensure_social_extras_mailing_lists_seeded(
        ctx,
    );
    crate::services::cosmetics_ui_ops::ensure_rookie_overview_seeded(ctx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quasar_gateway::STATUS_OK;
    use crate::services::cosmetics_ui_ops::{ROOKIE_HELP_CHANNEL_ID, ROOKIE_OVERVIEW_PRESET_NAME};
    use crate::services::quasar_cosmetics_market_types::ComponentListingRecord;
    use eve_db::{MailingListRecord, StoreCatalog, StoreCategory, StoreOffer, StoreProduct};
    use eve_machonet::service_roles::ROLE_GM_DEFAULT;
    use eve_machonet::SessionState;
    use eve_proto::quasar::EvePublicRequest;
    use eve_proto::{pb, PyRep};
    use std::collections::HashMap;
    use std::sync::Arc;

    fn admin_session(ctx: &ServerContext) -> SessionState {
        let (char_id, corp_id, user_id) = ctx
            .db
            .with_snapshot(|snap| {
                snap.characters
                    .iter()
                    .next()
                    .map(|(&cid, ch)| (cid, ch.corporation_id, ch.user_id))
            })
            .unwrap_or((90_000_001, 1_000_009, 1));
        SessionState {
            session_id: 7_701,
            user_id: Some(user_id),
            character_id: Some(char_id),
            corporation_id: Some(corp_id),
            solar_system_id: Some(30_000_142),
            role: ROLE_GM_DEFAULT,
            corp_role: u64::MAX,
            ..Default::default()
        }
    }

    #[test]
    fn startup_global_seeds_populate_fresh_server_and_preserve_custom_state_on_restart() {
        let ctx = ServerContext::new();

        // Fresh startup already seeded store, component listings, SDE launchers, and rookie overview.
        assert!(ctx.db.with_snapshot(|snap| {
            snap.quasar_commerce
                .nes_store_catalog
                .as_ref()
                .is_some_and(|c| !c.offers.is_empty())
        }));
        assert!(ctx.db.with_snapshot(|snap| {
            snap.quasar_state_blobs
                .get(BLOB_KEY)
                .and_then(|json| serde_json::from_str::<CosmeticsMarketData>(json).ok())
                .is_some_and(|mkt| !mkt.component_listings.is_empty())
        }));
        {
            let guard = ctx.cluster_misc_state.lock().unwrap();
            assert!(guard.sde_initialized);
            assert!(!guard.launchers.is_empty());
            assert!(!guard.starter_launchers.is_empty());
        }
        assert!(ctx.db.with_snapshot(|snap| {
            snap.chat_channels
                .get(ROOKIE_HELP_CHANNEL_ID)
                .is_some_and(|c| c.motd.contains(ROOKIE_OVERVIEW_PRESET_NAME))
        }));

        // Re-running startup seeds on an already-seeded DB performs zero write commits.
        let commits_before = ctx.db.tx_commit_count();
        let epoch_before = ctx.db.state_epoch();
        run_startup_global_seeds(&ctx);
        assert_eq!(ctx.db.tx_commit_count(), commits_before);
        assert_eq!(ctx.db.state_epoch(), epoch_before);

        // Customize persisted store catalog, component listings, rookie_help MOTD, mailing list,
        // and add a legacy corp recruitment ad before restarting on the same WorldRepository.
        let custom_motd = "Custom Rookie Help MOTD";
        let custom_listing_key = "custom-listing-restart-preserve".to_string();
        let custom_corp_id = 9_800_123_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.quasar_commerce.nes_store_catalog = Some(StoreCatalog {
                categories: vec![StoreCategory {
                    id: 99,
                    name: "Custom Cat".into(),
                    description: "Persisted".into(),
                }],
                products: vec![StoreProduct {
                    id: 901,
                    name: "Custom Prod".into(),
                    description: "Persisted".into(),
                    type_id: 34,
                    quantity: 1,
                }],
                offers: vec![StoreOffer {
                    id: 9001,
                    name: "Custom Offer".into(),
                    description: "Persisted".into(),
                    category_id: 99,
                    price_plex: 123,
                    product_ids: vec![901],
                    targeted: false,
                    ..Default::default()
                }],
            });

            let mut mkt = CosmeticsMarketData::default();
            mkt.component_listings.insert(
                custom_listing_key.clone(),
                ComponentListingRecord {
                    uuid: [7u8; 16],
                    sale_end_unix: 2_000_000_000,
                    price_isk: Some(55_000),
                    price_plex_cents: None,
                    component_item_type_id: 34,
                    bundle_size: 1,
                    discount_percent: 0,
                },
            );
            snap.quasar_state_blobs.insert(
                BLOB_KEY.into(),
                serde_json::to_string(&mkt).expect("serialize custom cosmetics market"),
            );

            if let Some(chan) = snap.chat_channels.get_mut(ROOKIE_HELP_CHANNEL_ID) {
                chan.motd = custom_motd.to_string();
            }

            snap.fleet_and_social_ops.corp_recruitment_ads.clear();
            snap.corp_governance
                .entry(custom_corp_id)
                .or_default()
                .recruitment_ads
                .push((4242, "Legacy Startup Ad".into(), "Migrated once".into(), 1, 90_000_001));

            snap.mailing_lists.insert(
                7_100_001,
                MailingListRecord {
                    list_id: 7_100_001,
                    name: "Startup Synced List".into(),
                    owner_id: 90_000_001,
                    ..Default::default()
                },
            );
        });

        let ctx_restarted =
            ServerContext::with_sde_and_repository(Arc::clone(&ctx.sde), Arc::clone(&ctx.db));

        ctx_restarted.db.with_snapshot(|snap| {
            let cat = snap.quasar_commerce.nes_store_catalog.as_ref().unwrap();
            assert!(cat
                .offers
                .iter()
                .any(|o| o.id == 9001 && o.name == "Custom Offer"));
            assert!(!cat.offers.iter().any(|o| o.id == 1002));

            let mkt: CosmeticsMarketData = serde_json::from_str(
                snap.quasar_state_blobs.get(BLOB_KEY).expect("market blob"),
            )
            .unwrap();
            assert_eq!(mkt.component_listings.len(), 1);
            assert!(mkt.component_listings.contains_key(&custom_listing_key));

            assert_eq!(
                snap.chat_channels
                    .get(ROOKIE_HELP_CHANNEL_ID)
                    .map(|c| c.motd.as_str()),
                Some(custom_motd)
            );

            assert_eq!(snap.fleet_and_social_ops.corp_recruitment_ads.len(), 1);
            assert_eq!(snap.fleet_and_social_ops.corp_recruitment_ads[0].ad_id, 4242);
            assert_eq!(
                snap.fleet_and_social_ops.corp_recruitment_ads[0].corp_id,
                custom_corp_id
            );
        });

        let se = ctx_restarted.social_extras.lock().unwrap();
        assert!(se.mailing_lists.contains_key(&7_100_001));
    }

    #[tokio::test]
    async fn read_rpcs_and_misc_reads_do_not_lazily_seed_or_mutate_snapshot() {
        let ctx = ServerContext::new();
        let mut sess = admin_session(&ctx);
        let kw = HashMap::new();
        let now_unix = 1_700_000_000;

        let commits_before = ctx.db.tx_commit_count();
        let epoch_before = ctx.db.state_epoch();

        let store_req = EvePublicRequest {
            payload: Some(
                pb::message("eve_public.store.offer.api.GetAllRequest")
                    .to_any()
                    .expect("store GetAllRequest any"),
            ),
            ..Default::default()
        };
        let store_rsp = ctx
            .quasar_gateway
            .handle_public_request(&sess, &store_req, None, false, now_unix);
        assert_eq!(store_rsp.status_code, STATUS_OK);

        let public_comp_req = EvePublicRequest {
            payload: Some(
                pb::message("eve_public.cosmetic.market.component.listing.api.GetAllRequest")
                    .to_any()
                    .expect("public component GetAllRequest any"),
            ),
            ..Default::default()
        };
        let public_comp_rsp = ctx.quasar_gateway.handle_public_request(
            &sess,
            &public_comp_req,
            None,
            false,
            now_unix,
        );
        assert_eq!(public_comp_rsp.status_code, STATUS_OK);

        let admin_comp_req = EvePublicRequest {
            payload: Some(
                pb::message("eve.cosmetic.market.component.listing.api.GetAllRequest")
                    .to_any()
                    .expect("admin component GetAllRequest any"),
            ),
            ..Default::default()
        };
        let admin_comp_rsp = ctx.quasar_gateway.handle_public_request(
            &sess,
            &admin_comp_req,
            None,
            false,
            now_unix,
        );
        assert_eq!(admin_comp_rsp.status_code, STATUS_OK);

        let corp_id = sess.corporation_id.unwrap_or(1_000_009);
        assert!(ctx
            .dispatch_rpc(
                &mut sess,
                "corpRecProxy",
                None,
                "GetRecruitmentAdsByCorpID",
                &[PyRep::Integer(corp_id)],
                &kw,
                None,
            )
            .await
            .is_ok());
        assert!(ctx
            .dispatch_rpc(&mut sess, "mailMgr", None, "GetMailHeaders", &[], &kw, None)
            .await
            .is_ok());
        assert!(ctx
            .dispatch_rpc(&mut sess, "mailMgr", None, "SyncMail", &[], &kw, None)
            .await
            .is_ok());
        assert!(ctx
            .dispatch_rpc(
                &mut sess,
                "mailingListsMgr",
                None,
                "GetJoinedLists",
                &[],
                &kw,
                None,
            )
            .await
            .is_ok());
        let rookie_preset_id: i64 = ctx
            .db
            .with_snapshot(|snap| {
                let motd = &snap.chat_channels.get(ROOKIE_HELP_CHANNEL_ID)?.motd;
                let after = motd.split("//").nth(1)?;
                let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                digits.parse::<i64>().ok()
            })
            .expect("rookie preset link id in MOTD");
        assert!(ctx
            .dispatch_rpc(
                &mut sess,
                "overviewPresetMgr",
                None,
                "GetStoredPreset",
                &[PyRep::Integer(rookie_preset_id)],
                &kw,
                None,
            )
            .await
            .is_ok());
        let launchers_count =
            crate::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
                st.launchers.len()
            });
        assert!(launchers_count > 0);

        assert_eq!(ctx.db.tx_commit_count(), commits_before);
        assert_eq!(ctx.db.state_epoch(), epoch_before);

        // Removing the cosmetics market blob after startup proves the component GetAll handler
        // reads cleanly and does NOT lazily re-seed the snapshot.
        ctx.db.with_mut_snapshot(|snap| {
            snap.quasar_state_blobs.remove(BLOB_KEY);
        });
        let commits_after_wipe = ctx.db.tx_commit_count();
        let after_wipe_rsp = ctx.quasar_gateway.handle_public_request(
            &sess,
            &admin_comp_req,
            None,
            false,
            now_unix,
        );
        assert_eq!(after_wipe_rsp.status_code, STATUS_OK);
        assert_eq!(ctx.db.tx_commit_count(), commits_after_wipe);
        assert!(ctx
            .db
            .with_snapshot(|snap| !snap.quasar_state_blobs.contains_key(BLOB_KEY)));

        // Clearing runtime shipcaster state after startup proves `with_misc_state` does NOT lazily
        // re-run `ensure_sde_initialized`.
        *ctx.cluster_misc_state.lock().unwrap() = Default::default();
        let sde_init_after_clear =
            crate::services::cluster_macho_misc_services::with_misc_state(&ctx, |st| {
                st.sde_initialized
            });
        assert!(!sde_init_after_clear);
    }
}
