//! `quasar_payment_store` — Quasar Payment, PLEX, Store, Vendors, Fulfillment & ISK Services (`Q-3`).

use super::quasar_auth_user::{build_generic_response, get_identity_store};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

pub const GROUP_3_REQUEST_PAIRS: &[(&str, &str)] = &[
    ("eve.fulfillment.DeliverItemRequest", "eve.fulfillment.DeliverItemResponse"),
    ("eve.fulfillment.GMGemDepositRequest", "eve.fulfillment.GMGemDepositResponse"),
    ("eve.fulfillment.GMGemWithdrawalRequest", "eve.fulfillment.GMGemWithdrawalResponse"),
    ("eve.fulfillment.GMPlexDepositRequest", "eve.fulfillment.GMPlexDepositResponse"),
    ("eve.fulfillment.GMPlexWithdrawalRequest", "eve.fulfillment.GMPlexWithdrawalResponse"),
    ("eve.fulfillment.RewardDeliveryRequest", "eve.fulfillment.RewardDeliveryResponse"),
    ("eve.fulfillment.api.DeliverBlueprintCopyRequest", "eve.fulfillment.api.DeliverBlueprintCopyResponse"),
    ("eve.fulfillment.api.DeliverItemRequest", "eve.fulfillment.api.DeliverItemResponse"),
    ("eve.fulfillment.api.DeliverPurchaseRequest", "eve.fulfillment.api.DeliverPurchaseResponse"),
    ("eve.fulfillment.api.DeliverRewardRequest", "eve.fulfillment.api.DeliverRewardResponse"),
    ("eve.fulfillment.api.ValidatePurchaseRequest", "eve.fulfillment.api.ValidatePurchaseResponse"),
    ("eve.fulfillment.offer.api.GetRequest", "eve.fulfillment.offer.api.GetResponse"),
    ("eve.isk.wallet.api.TransferByOwnerRequest", "eve.isk.wallet.api.TransferByOwnerResponse"),
    ("eve.payment.admin.api.GetPersonalDataRequest", "eve.payment.admin.api.GetPersonalDataResponse"),
    ("eve.payment.admin.api.ProcessPaymentFilesRequest", "eve.payment.admin.api.ProcessPaymentFilesResponse"),
    ("eve.payment.api.GetAllForUserRequest", "eve.payment.api.GetAllForUserResponse"),
    ("eve.payment.api.GetDetailsRequest", "eve.payment.api.GetDetailsResponse"),
    ("eve.payment.api.GetRequest", "eve.payment.api.GetResponse"),
    ("eve.payment.api.RefundRequest", "eve.payment.api.RefundResponse"),
    ("eve.payment.api.admin.ProcessPaymentFilesRequest", "eve.payment.api.admin.ProcessPaymentFilesResponse"),
    ("eve.payment.code.api.RedeemRequest", "eve.payment.code.api.RedeemResponse"),
    ("eve.payment.code.api.admin.ExpireRequest", "eve.payment.code.api.admin.ExpireResponse"),
    ("eve.payment.code.api.admin.GetRequest", "eve.payment.code.api.admin.GetResponse"),
    ("eve.payment.country.api.GetAllRequest", "eve.payment.country.api.GetAllResponse"),
    ("eve.payment.customer.api.ChangeCountryRequest", "eve.payment.customer.api.ChangeCountryResponse"),
    ("eve.payment.customer.api.GetByUserRequest", "eve.payment.customer.api.GetByUserResponse"),
    ("eve.payment.customer.api.GetRequest", "eve.payment.customer.api.GetResponse"),
    ("eve.payment.purchase.api.AlipayRequest", "eve.payment.purchase.api.AlipayResponse"),
    ("eve.payment.purchase.api.CostRequest", "eve.payment.purchase.api.CostResponse"),
    ("eve.payment.purchase.api.CreateSepaMandateRequest", "eve.payment.purchase.api.CreateSepaMandateResponse"),
    ("eve.payment.purchase.api.CreditCardRequest", "eve.payment.purchase.api.CreditCardResponse"),
    ("eve.payment.purchase.api.IdealRequest", "eve.payment.purchase.api.IdealResponse"),
    ("eve.payment.purchase.api.PayPalRequest", "eve.payment.purchase.api.PayPalResponse"),
    ("eve.payment.purchase.api.PaySafeCardRequest", "eve.payment.purchase.api.PaySafeCardResponse"),
    ("eve.payment.purchase.api.ReceiptRequest", "eve.payment.purchase.api.ReceiptResponse"),
    ("eve.payment.purchase.api.SepaDirectDebitRequest", "eve.payment.purchase.api.SepaDirectDebitResponse"),
    ("eve.payment.purchase.api.SofortRequest", "eve.payment.purchase.api.SofortResponse"),
    ("eve.payment.purchase.api.SteamWalletRequest", "eve.payment.purchase.api.SteamWalletResponse"),
    ("eve.payment.purchase.api.TokenRequest", "eve.payment.purchase.api.TokenResponse"),
    ("eve.payment.purchase.api.WechatPayRequest", "eve.payment.purchase.api.WechatPayResponse"),
    ("eve.payment.token.api.DisableAllForUserRequest", "eve.payment.token.api.DisableAllForUserResponse"),
    ("eve.payment.token.api.GetAllForUserRequest", "eve.payment.token.api.GetAllForUserResponse"),
    ("eve.payment.token.api.GetQuickPayForUserRequest", "eve.payment.token.api.GetQuickPayForUserResponse"),
    ("eve.payment.token.api.GetRequest", "eve.payment.token.api.GetResponse"),
    ("eve.plex.vault.api.BalanceRequest", "eve.plex.vault.api.BalanceResponse"),
    ("eve.plex.vault.api.PurchaseRequest", "eve.plex.vault.api.PurchaseResponse"),
    ("eve.plex.vault.assetholding.operation.api.DepositRequest", "eve.plex.vault.assetholding.operation.api.DepositResponse"),
    ("eve.plex.vault.assetholding.operation.api.GetRequest", "eve.plex.vault.assetholding.operation.api.GetResponse"),
    ("eve.plex.vault.assetholding.operation.api.WithdrawRequest", "eve.plex.vault.assetholding.operation.api.WithdrawResponse"),
    ("eve.plex.vault.transaction.api.GetAllForUserRequest", "eve.plex.vault.transaction.api.GetAllForUserResponse"),
    ("eve.plex.vault.transaction.api.GetAllLoggedForUserRequest", "eve.plex.vault.transaction.api.GetAllLoggedForUserResponse"),
    ("eve.plex.vault.transaction.api.GetLogRequest", "eve.plex.vault.transaction.api.GetLogResponse"),
    ("eve.plex.vault.transaction.api.GetRequest", "eve.plex.vault.transaction.api.GetResponse"),
    ("eve.plex.vault.transaction.api.GetTopTransferersRequest", "eve.plex.vault.transaction.api.GetTopTransferersResponse"),
    ("eve.plex.vault.transaction.api.SearchRequest", "eve.plex.vault.transaction.api.SearchResponse"),
    ("eve.plex.vault.transaction.api.admin.DepositRequest", "eve.plex.vault.transaction.api.admin.DepositResponse"),
    ("eve.plex.vault.transaction.api.admin.GenerateTransactionRequest", "eve.plex.vault.transaction.api.admin.GenerateTransactionResponse"),
    ("eve.plex.vault.transaction.api.admin.ResetForUserRequest", "eve.plex.vault.transaction.api.admin.ResetForUserResponse"),
    ("eve.plex.vault.transaction.api.admin.WithdrawalRequest", "eve.plex.vault.transaction.api.admin.WithdrawalResponse"),
    ("eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceRequest", "eve.plex.vault.transaction.invoice.api.admin.RecordInvoiceResponse"),
    ("eve.store.admin.api.AddUsersToTargetGroupRequest", "eve.store.admin.api.AddUsersToTargetGroupResponse"),
    ("eve.store.admin.api.CreateCategoryRequest", "eve.store.admin.api.CreateCategoryResponse"),
    ("eve.store.admin.api.CreateLabelRequest", "eve.store.admin.api.CreateLabelResponse"),
    ("eve.store.admin.api.CreateOfferRequest", "eve.store.admin.api.CreateOfferResponse"),
    ("eve.store.admin.api.CreateProductRequest", "eve.store.admin.api.CreateProductResponse"),
    ("eve.store.admin.api.CreateSpecialOfferRequest", "eve.store.admin.api.CreateSpecialOfferResponse"),
    ("eve.store.admin.api.CreateTagRequest", "eve.store.admin.api.CreateTagResponse"),
    ("eve.store.admin.api.CreateTargetGroupRequest", "eve.store.admin.api.CreateTargetGroupResponse"),
    ("eve.store.admin.api.DeleteCategoryRequest", "eve.store.admin.api.DeleteCategoryResponse"),
    ("eve.store.admin.api.DeleteLabelRequest", "eve.store.admin.api.DeleteLabelResponse"),
    ("eve.store.admin.api.DeleteProductRequest", "eve.store.admin.api.DeleteProductResponse"),
    ("eve.store.admin.api.DeleteTagRequest", "eve.store.admin.api.DeleteTagResponse"),
    ("eve.store.admin.api.DeleteTargetGroupRequest", "eve.store.admin.api.DeleteTargetGroupResponse"),
    ("eve.store.admin.api.DeleteTranslationRequest", "eve.store.admin.api.DeleteTranslationResponse"),
    ("eve.store.admin.api.EditLabelRequest", "eve.store.admin.api.EditLabelResponse"),
    ("eve.store.admin.api.EditProductRequest", "eve.store.admin.api.EditProductResponse"),
    ("eve.store.admin.api.EditTagRequest", "eve.store.admin.api.EditTagResponse"),
    ("eve.store.admin.api.ExportOffersRequest", "eve.store.admin.api.ExportOffersResponse"),
    ("eve.store.admin.api.GetAllTranslationsRequest", "eve.store.admin.api.GetAllTranslationsResponse"),
    ("eve.store.admin.api.GetCurrenciesRequest", "eve.store.admin.api.GetCurrenciesResponse"),
    ("eve.store.admin.api.GetImportLogEntriesRequest", "eve.store.admin.api.GetImportLogEntriesResponse"),
    ("eve.store.admin.api.GetImportLogRequest", "eve.store.admin.api.GetImportLogResponse"),
    ("eve.store.admin.api.GetImportLogsRequest", "eve.store.admin.api.GetImportLogsResponse"),
    ("eve.store.admin.api.GetLabelRequest", "eve.store.admin.api.GetLabelResponse"),
    ("eve.store.admin.api.GetLabelsRequest", "eve.store.admin.api.GetLabelsResponse"),
    ("eve.store.admin.api.GetMassGiveLogEntriesRequest", "eve.store.admin.api.GetMassGiveLogEntriesResponse"),
    ("eve.store.admin.api.GetMassGiveLogRequest", "eve.store.admin.api.GetMassGiveLogResponse"),
    ("eve.store.admin.api.GetMassGiveLogsRequest", "eve.store.admin.api.GetMassGiveLogsResponse"),
    ("eve.store.admin.api.GetOfferRequest", "eve.store.admin.api.GetOfferResponse"),
    ("eve.store.admin.api.GetOffersWithMissingTranslationsRequest", "eve.store.admin.api.GetOffersWithMissingTranslationsResponse"),
    ("eve.store.admin.api.GetTagRequest", "eve.store.admin.api.GetTagResponse"),
    ("eve.store.admin.api.GetTagsRequest", "eve.store.admin.api.GetTagsResponse"),
    ("eve.store.admin.api.GetTargetGroupRequest", "eve.store.admin.api.GetTargetGroupResponse"),
    ("eve.store.admin.api.GetTranslationsRequest", "eve.store.admin.api.GetTranslationsResponse"),
    ("eve.store.admin.api.GetUnassignedTargetedOffersRequest", "eve.store.admin.api.GetUnassignedTargetedOffersResponse"),
    ("eve.store.admin.api.GiftOfferRequest", "eve.store.admin.api.GiftOfferResponse"),
    ("eve.store.admin.api.ImportOffersRequest", "eve.store.admin.api.ImportOffersResponse"),
    ("eve.store.admin.api.LoadMissingTranslationsRequest", "eve.store.admin.api.LoadMissingTranslationsResponse"),
    ("eve.store.admin.api.MassGiveOfferRequest", "eve.store.admin.api.MassGiveOfferResponse"),
    ("eve.store.admin.api.UpdateCategoryRequest", "eve.store.admin.api.UpdateCategoryResponse"),
    ("eve.store.admin.api.UpdateOfferRequest", "eve.store.admin.api.UpdateOfferResponse"),
    ("eve.store.admin.api.UpdateTargetGroupRequest", "eve.store.admin.api.UpdateTargetGroupResponse"),
    ("eve.store.admin.api.UpdateTranslationsRequest", "eve.store.admin.api.UpdateTranslationsResponse"),
    ("eve.store.api.DeliverCodeRedeemRequest", "eve.store.api.DeliverCodeRedeemResponse"),
    ("eve.store.api.DeliverPurchaseRequest", "eve.store.api.DeliverPurchaseResponse"),
    ("eve.store.api.DeliverRewardRequest", "eve.store.api.DeliverRewardResponse"),
    ("eve.store.api.ValidatePurchaseRequest", "eve.store.api.ValidatePurchaseResponse"),
    ("eve.store.category.api.GetAllRequest", "eve.store.category.api.GetAllResponse"),
    ("eve.store.category.api.GetRequest", "eve.store.category.api.GetResponse"),
    ("eve.store.offer.api.ClearCacheRequest", "eve.store.offer.api.ClearCacheResponse"),
    ("eve.store.offer.api.GetAllForCountryRequest", "eve.store.offer.api.GetAllForCountryResponse"),
    ("eve.store.offer.api.GetAllForUserRequest", "eve.store.offer.api.GetAllForUserResponse"),
    ("eve.store.offer.api.GetAllTargetedForUserRequest", "eve.store.offer.api.GetAllTargetedForUserResponse"),
    ("eve.store.offer.api.GetForCountryRequest", "eve.store.offer.api.GetForCountryResponse"),
    ("eve.store.offer.api.GetForCurrencyRequest", "eve.store.offer.api.GetForCurrencyResponse"),
    ("eve.store.offer.api.GetForUserRequest", "eve.store.offer.api.GetForUserResponse"),
    ("eve.store.offer.api.OfferSearchRequest", "eve.store.offer.api.OfferSearchResponse"),
    ("eve.store.product.api.GetAllRequest", "eve.store.product.api.GetAllResponse"),
    ("eve.store.product.api.GetLastPlexPurchaseDateRequest", "eve.store.product.api.GetLastPlexPurchaseDateResponse"),
    ("eve.store.product.api.GetRequest", "eve.store.product.api.GetResponse"),
    ("eve.store.product.api.ProductSearchRequest", "eve.store.product.api.ProductSearchResponse"),
    ("eve.store.sale.admin.DropFailedFulfillmentRequest", "eve.store.sale.admin.DropFailedFulfillmentResponse"),
    ("eve.store.sale.admin.RetryAllFailedFulfillmentsRequest", "eve.store.sale.admin.RetryAllFailedFulfillmentsResponse"),
    ("eve.store.sale.admin.RetryFailedFulfillmentRequest", "eve.store.sale.admin.RetryFailedFulfillmentResponse"),
    ("eve.store.sale.api.GetAllFailedFulfillmentsRequest", "eve.store.sale.api.GetAllFailedFulfillmentsResponse"),
    ("eve.store.sale.api.GetAllForUserRequest", "eve.store.sale.api.GetAllForUserResponse"),
    ("eve.store.sale.api.GetDelayedRequest", "eve.store.sale.api.GetDelayedResponse"),
    ("eve.store.sale.api.GetFailedFulfillmentRequest", "eve.store.sale.api.GetFailedFulfillmentResponse"),
    ("eve.store.sale.api.GetRequest", "eve.store.sale.api.GetResponse"),
    ("eve.store.sale.api.GetTopConsumersRequest", "eve.store.sale.api.GetTopConsumersResponse"),
    ("eve.store.sale.api.HandleDelayRequest", "eve.store.sale.api.HandleDelayResponse"),
    ("eve.store.tag.api.GetAllRequest", "eve.store.tag.api.GetAllResponse"),
    ("eve.store.tag.api.GetRequest", "eve.store.tag.api.GetResponse"),
    ("eve.store.targetaudience.definition.catalogue.version.api.GetLatestDownloadLinkRequest", "eve.store.targetaudience.definition.catalogue.version.api.GetLatestDownloadLinkResponse"),
    ("eve.store.targetaudience.definition.catalogue.version.api.ListAvailableRequest", "eve.store.targetaudience.definition.catalogue.version.api.ListAvailableResponse"),
    ("eve.store.targetaudience.definition.ruleset.version.api.ListAvailableRequest", "eve.store.targetaudience.definition.ruleset.version.api.ListAvailableResponse"),
    ("eve.store.targetaudience.definition.version.api.CancelScheduledChangeRequest", "eve.store.targetaudience.definition.version.api.CancelScheduledChangeResponse"),
    ("eve.store.targetaudience.definition.version.api.GetRequest", "eve.store.targetaudience.definition.version.api.GetResponse"),
    ("eve.store.targetaudience.definition.version.api.ListScheduledChangesRequest", "eve.store.targetaudience.definition.version.api.ListScheduledChangesResponse"),
    ("eve.store.targetaudience.definition.version.api.ScheduleForTenantRequest", "eve.store.targetaudience.definition.version.api.ScheduleForTenantResponse"),
    ("eve.store.targetaudience.definition.version.api.SetForDefaultRequest", "eve.store.targetaudience.definition.version.api.SetForDefaultResponse"),
    ("eve.store.targetaudience.definition.version.api.SetForTenantRequest", "eve.store.targetaudience.definition.version.api.SetForTenantResponse"),
    ("eve.store.targetaudience.definition.version.api.UnsetForTenantRequest", "eve.store.targetaudience.definition.version.api.UnsetForTenantResponse"),
    ("eve.store.targetaudience.offer.api.GetFulfillmentRequest", "eve.store.targetaudience.offer.api.GetFulfillmentResponse"),
    ("eve.store.targetaudience.offer.api.GetRequest", "eve.store.targetaudience.offer.api.GetResponse"),
    ("eve.store.targetaudience.offer.api.ListRequest", "eve.store.targetaudience.offer.api.ListResponse"),
    ("eve.store.targetaudience.offer.api.MarkAsPurchaseFailedRequest", "eve.store.targetaudience.offer.api.MarkAsPurchaseFailedResponse"),
    ("eve.store.targetaudience.offer.api.MarkAsPurchasedRequest", "eve.store.targetaudience.offer.api.MarkAsPurchasedResponse"),
    ("eve.store.targetaudience.offer.api.ValidateRequest", "eve.store.targetaudience.offer.api.ValidateResponse"),
    ("eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeRequest", "eve.store.targetaudience.offer.api.admin.BackfillPartnerCodeResponse"),
    ("eve.store.targetaudience.offer.api.admin.PromoteRequest", "eve.store.targetaudience.offer.api.admin.PromoteResponse"),
    ("eve.store.targetaudience.offer.api.admin.RetractRequest", "eve.store.targetaudience.offer.api.admin.RetractResponse"),
    ("eve.store.targetaudience.ruleset.api.ListConditionsRequest", "eve.store.targetaudience.ruleset.api.ListConditionsResponse"),
    ("eve.store.targetaudience.ruleset.api.ListTriggersRequest", "eve.store.targetaudience.ruleset.api.ListTriggersResponse"),
    ("eve.store.targetgroup.api.AddUserRequest", "eve.store.targetgroup.api.AddUserResponse"),
    ("eve.store.targetgroup.api.GetAllForUserRequest", "eve.store.targetgroup.api.GetAllForUserResponse"),
    ("eve.store.targetgroup.api.GetAllRequest", "eve.store.targetgroup.api.GetAllResponse"),
    ("eve.store.targetgroup.api.GetRequest", "eve.store.targetgroup.api.GetResponse"),
    ("eve.store.targetgroup.api.RemoveUsersRequest", "eve.store.targetgroup.api.RemoveUsersResponse"),
    ("eve.vendors.epic.api.DeliverEntitlementRequest", "eve.vendors.epic.api.DeliverEntitlementResponse"),
    ("eve.vendors.epic.api.GetClawbacksLastCheckedRequest", "eve.vendors.epic.api.GetClawbacksLastCheckedResponse"),
    ("eve.vendors.epic.api.GetClawbacksRequest", "eve.vendors.epic.api.GetClawbacksResponse"),
    ("eve.vendors.epic.api.GetEntitlementsRequest", "eve.vendors.epic.api.GetEntitlementsResponse"),
    ("eve.vendors.epic.api.GetQueueDelayEntriesRequest", "eve.vendors.epic.api.GetQueueDelayEntriesResponse"),
    ("eve.vendors.epic.api.GetUserByEpicUserIDRequest", "eve.vendors.epic.api.GetUserByEpicUserIDResponse"),
    ("eve.vendors.epic.api.GetUserByEveUserIDRequest", "eve.vendors.epic.api.GetUserByEveUserIDResponse"),
    ("eve.vendors.epic.api.PurgeQueueRequest", "eve.vendors.epic.api.PurgeQueueResponse"),
    ("eve.vendors.fusionauth.api.LinkUserRequest", "eve.vendors.fusionauth.api.LinkUserResponse"),
    ("eve.vendors.payletter.GetUserIdentityVerificationReceiptRequest", "eve.vendors.payletter.GetUserIdentityVerificationReceiptResponse"),
    ("eve.vendors.payletter.InitializeIdentityVerificationReceiptPairingRequest", "eve.vendors.payletter.InitializeIdentityVerificationReceiptPairingResponse"),
    ("eve.vendors.payletter.PairIdentityVerificationReceiptWithUserRequest", "eve.vendors.payletter.PairIdentityVerificationReceiptWithUserResponse"),
    ("eve.vendors.steam.queue.api.GetAllBySteamUserRequest", "eve.vendors.steam.queue.api.GetAllBySteamUserResponse"),
    ("eve.vendors.steam.queue.api.GetAllByTransactionRequest", "eve.vendors.steam.queue.api.GetAllByTransactionResponse"),
    ("eve.vendors.steam.user.api.CreateRequest", "eve.vendors.steam.user.api.CreateResponse"),
    ("eve.vendors.steam.user.api.GetByEveUserRequest", "eve.vendors.steam.user.api.GetByEveUserResponse"),
    ("eve.vendors.steam.user.api.GetRequest", "eve.vendors.steam.user.api.GetResponse"),
    ("eve.vendors.steam.user.api.admin.LinkRequest", "eve.vendors.steam.user.api.admin.LinkResponse"),
    ("eve_public.payment.code.api.RedeemRequest", "eve_public.payment.code.api.RedeemResponse"),
    ("eve_public.payment.purchase.api.CostRequest", "eve_public.payment.purchase.api.CostResponse"),
    ("eve_public.payment.purchase.api.TokenRequest", "eve_public.payment.purchase.api.TokenResponse"),
    ("eve_public.payment.schedule.api.GetActiveByTypeRequest", "eve_public.payment.schedule.api.GetActiveByTypeResponse"),
    ("eve_public.payment.schedule.api.GetRequest", "eve_public.payment.schedule.api.GetResponse"),
    ("eve_public.payment.token.api.DisableAllRequest", "eve_public.payment.token.api.DisableAllResponse"),
    ("eve_public.payment.token.api.GetQuickPayRequest", "eve_public.payment.token.api.GetQuickPayResponse"),
    ("eve_public.payment.token.api.GetRequest", "eve_public.payment.token.api.GetResponse"),
    ("eve_public.plex.vault.api.BalanceRequest", "eve_public.plex.vault.api.BalanceResponse"),
    ("eve_public.plex.vault.api.PurchaseRequest", "eve_public.plex.vault.api.PurchaseResponse"),
    ("eve_public.plex.vault.transaction.api.GetAllLoggedForUserRequest", "eve_public.plex.vault.transaction.api.GetAllLoggedForUserResponse"),
    ("eve_public.plex.vault.transaction.api.GetLogRequest", "eve_public.plex.vault.transaction.api.GetLogResponse"),
    ("eve_public.plex.vault.transaction.api.GetStatisticsRequest", "eve_public.plex.vault.transaction.api.GetStatisticsResponse"),
    ("eve_public.store.category.api.GetAllRequest", "eve_public.store.category.api.GetAllResponse"),
    ("eve_public.store.category.api.GetRequest", "eve_public.store.category.api.GetResponse"),
    ("eve_public.store.offer.api.GetAllByTagRequest", "eve_public.store.offer.api.GetAllByTagResponse"),
    ("eve_public.store.offer.api.GetAllRequest", "eve_public.store.offer.api.GetAllResponse"),
    ("eve_public.store.offer.api.GetAllTargetedRequest", "eve_public.store.offer.api.GetAllTargetedResponse"),
    ("eve_public.store.offer.api.GetRequest", "eve_public.store.offer.api.GetResponse"),
    ("eve_public.store.product.api.GetRequest", "eve_public.store.product.api.GetResponse"),
    ("eve_public.store.targetaudience.offer.api.GetRequest", "eve_public.store.targetaudience.offer.api.GetResponse"),
    ("eve_public.store.targetaudience.offer.api.ListRequest", "eve_public.store.targetaudience.offer.api.ListResponse"),
];

pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let descriptors = Arc::clone(&router.descriptors);
    let store = get_identity_store();

    // 1. Specific custom handlers with live commerce/PLEX/store semantics

    // eve_public.plex.vault.api.BalanceRequest -> eve_public.plex.vault.api.BalanceResponse
    let store_plex_bal = Arc::clone(&store);
    router.register_handler(
        "eve_public.plex.vault.api.BalanceRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let bal = store_plex_bal
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .plex_balances
                .get(&uid)
                .copied()
                .unwrap_or(500);

            let mut curr = Vec::new();
            encode_varint_field(1, (bal * 100) as u64, &mut curr); // field 1: total_in_cents

            let mut out = Vec::new();
            encode_bytes_field(1, &curr, &mut out); // field 1: balance
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceResponse"),
                value: out,
            })
        }),
    );

    // eve.plex.vault.assetholding.operation.api.DepositRequest -> updates balance and enqueues BalanceChangedNotice
    let store_plex_dep = Arc::clone(&store);
    let router_pending_dep = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.plex.vault.assetholding.operation.api.DepositRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let deposit_amount = if let Some(DynamicValue::Message(c)) = dyn_msg.get(3) {
                if let Some(DynamicValue::Int64(amt)) = c.get(1) {
                    *amt / 100
                } else if let Some(DynamicValue::Uint64(amt)) = c.get(1) {
                    (*amt / 100) as i64
                } else {
                    100
                }
            } else if let Some(DynamicValue::Message(c)) = dyn_msg.get(2) {
                if let Some(DynamicValue::Int64(amt)) = c.get(1) {
                    *amt / 100
                } else {
                    100
                }
            } else {
                100
            };

            let new_bal = {
                let mut guard = store_plex_dep.write().unwrap_or_else(|e| e.into_inner());
                let entry = guard.plex_balances.entry(uid).or_insert(500);
                *entry += deposit_amount;
                *entry
            };

            let mut notice_bytes = Vec::new();
            let mut curr = Vec::new();
            encode_varint_field(1, (new_bal * 100) as u64, &mut curr);
            encode_bytes_field(3, &curr, &mut notice_bytes); // resulting_balance

            router_pending_dep
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceChangedNotice"),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.plex.vault.assetholding.operation.api.DepositResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve.plex.vault.assetholding.operation.api.WithdrawRequest -> updates balance and enqueues BalanceChangedNotice
    let store_plex_with = Arc::clone(&store);
    let router_pending_with = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.plex.vault.assetholding.operation.api.WithdrawRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let withdraw_amount = if let Some(DynamicValue::Message(c)) = dyn_msg.get(3) {
                if let Some(DynamicValue::Int64(amt)) = c.get(1) {
                    *amt / 100
                } else if let Some(DynamicValue::Uint64(amt)) = c.get(1) {
                    (*amt / 100) as i64
                } else {
                    50
                }
            } else if let Some(DynamicValue::Message(c)) = dyn_msg.get(2) {
                if let Some(DynamicValue::Int64(amt)) = c.get(1) {
                    *amt / 100
                } else {
                    50
                }
            } else {
                50
            };

            let new_bal = {
                let mut guard = store_plex_with.write().unwrap_or_else(|e| e.into_inner());
                let entry = guard.plex_balances.entry(uid).or_insert(500);
                *entry = (*entry - withdraw_amount).max(0);
                *entry
            };

            let mut notice_bytes = Vec::new();
            let mut curr = Vec::new();
            encode_varint_field(1, (new_bal * 100) as u64, &mut curr);
            encode_bytes_field(3, &curr, &mut notice_bytes);

            router_pending_with
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceChangedNotice"),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.plex.vault.assetholding.operation.api.WithdrawResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve_public.store.category.api.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve_public.store.category.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            for cat_id in 1..=4 {
                let mut cat_bytes = Vec::new();
                encode_varint_field(1, cat_id, &mut cat_bytes);
                encode_bytes_field(1, &cat_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.category.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // eve_public.store.offer.api.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve_public.store.offer.api.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            for offer_id in 1001..=1005 {
                let mut off_bytes = Vec::new();
                encode_varint_field(1, offer_id, &mut off_bytes);
                encode_bytes_field(1, &off_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.store.offer.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // eve_public.payment.code.api.RedeemRequest -> RedeemResponse
    let store_redeem = Arc::clone(&store);
    router.register_handler(
        "eve_public.payment.code.api.RedeemRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let code = if let Some(DynamicValue::Message(m)) = dyn_msg.get(1) {
                if let Some(DynamicValue::String(s)) = m.get(1) {
                    s.clone()
                } else {
                    "PROMO-V24-START".to_string()
                }
            } else if let Some(DynamicValue::String(s)) = dyn_msg.get(1) {
                s.clone()
            } else {
                "PROMO-V24-START".to_string()
            };
            store_redeem
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .redeemed_codes
                .insert(code);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.code.api.RedeemResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve_public.payment.schedule.api.GetRequest -> enqueues SetNotice
    let router_pending_sched = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.payment.schedule.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1);
            let mut notice_bytes = Vec::new();
            encode_varint_field(1, 101, &mut notice_bytes);
            router_pending_sched
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.SetNotice"),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.GetResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve_public.payment.token.api.GetQuickPayRequest -> enqueues QuickPayAddedNotice
    let router_pending_tok = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.payment.token.api.GetQuickPayRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1);
            let mut notice_bytes = Vec::new();
            encode_string_field(1, "tok_quickpay_1", &mut notice_bytes);
            router_pending_tok
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.QuickPayAddedNotice"),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.token.api.GetQuickPayResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve.fulfillment.api.DeliverItemRequest -> DeliverItemResponse
    let db_fulfill = Arc::clone(&db);
    router.register_handler(
        "eve.fulfillment.api.DeliverItemRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let type_id = if let Some(DynamicValue::Message(item_type)) = dyn_msg.get(2) {
                if let Some(DynamicValue::Int32(t)) = item_type.get(1) {
                    *t as u32
                } else {
                    44992 // PLEX
                }
            } else {
                44992
            };
            let qty = if let Some(DynamicValue::Int32(q)) = dyn_msg.get(3) {
                *q
            } else {
                1
            };
            let target_cid = ctx
                .effective_character_id()
                .map(|cid| cid as i64)
                .unwrap_or_else(|| {
                    let chars = db_fulfill.get_characters_for_user(uid);
                    chars.first().map(|c| c.character_id).unwrap_or(1001)
                });
            let _ = db_fulfill.add_or_stack_item(
                target_cid,
                60003760, // station
                eve_db::InventoryFlag::Hangar,
                type_id,
                qty as i64,
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.fulfillment.api.DeliverItemResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve.isk.wallet.api.TransferByOwnerRequest -> TransferByOwnerResponse
    router.register_handler(
        "eve.isk.wallet.api.TransferByOwnerRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.isk.wallet.api.TransferByOwnerResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 2. Register all other Group 3 request pairs using descriptors
    for &(req_type, rsp_type) in GROUP_3_REQUEST_PAIRS {
        let req_norm = QuasarDescriptorRegistry::normalize_type_name(req_type).to_string();
        let rsp_norm = QuasarDescriptorRegistry::normalize_type_name(rsp_type).to_string();
        let desc_arc = Arc::clone(&descriptors);
        let rsp_name_clone = rsp_norm.clone();

        if matches!(
            req_norm.as_str(),
            "eve_public.plex.vault.api.BalanceRequest"
                | "eve.plex.vault.assetholding.operation.api.DepositRequest"
                | "eve.plex.vault.assetholding.operation.api.WithdrawRequest"
                | "eve_public.store.category.api.GetAllRequest"
                | "eve_public.store.offer.api.GetAllRequest"
                | "eve_public.payment.code.api.RedeemRequest"
                | "eve_public.payment.schedule.api.GetRequest"
                | "eve_public.payment.token.api.GetQuickPayRequest"
                | "eve.fulfillment.api.DeliverItemRequest"
                | "eve.isk.wallet.api.TransferByOwnerRequest"
        ) {
            continue;
        }

        router.register_handler(
            &req_norm,
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_generic_response(&desc_arc, &rsp_name_clone, 0x3001)
            }),
        );
    }
}
