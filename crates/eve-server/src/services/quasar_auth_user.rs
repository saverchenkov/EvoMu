//! `quasar_auth_user` — Quasar Identity, Auth, User, Device, Legal, PII, CDN & Localization Services (`Q-3`).

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_INTERNAL_ERROR, STATUS_NOT_FOUND,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuasarIdentityStore {
    pub plex_balances: HashMap<u64, i64>,
    pub omega_subscriptions: HashMap<u64, u64>,
    pub user_emails: HashMap<u64, String>,
    pub user_tags: HashMap<u64, Vec<String>>,
    pub accepted_agreements: HashMap<u64, Vec<String>>,
    pub redeemed_codes: HashSet<String>,
    pub user_entitlements: HashMap<u64, Vec<String>>,
    pub payment_tokens: HashMap<u64, Vec<String>>,
    pub billing_schedules: HashMap<u64, Vec<String>>,
}

static IDENTITY_STORE: OnceLock<Arc<RwLock<QuasarIdentityStore>>> = OnceLock::new();

pub fn get_identity_store() -> Arc<RwLock<QuasarIdentityStore>> {
    IDENTITY_STORE
        .get_or_init(|| {
            let mut store = QuasarIdentityStore::default();
            store.plex_balances.insert(1, 1000);
            store.plex_balances.insert(1001, 500);
            store.user_emails.insert(1, "pilot1@evomu.local".to_string());
            store.user_tags.insert(1, vec!["alpha_clone".to_string(), "verified".to_string()]);
            store.user_entitlements.insert(1, vec!["standard_pilot".to_string()]);
            Arc::new(RwLock::new(store))
        })
        .clone()
}

pub fn build_generic_response(
    descriptors: &QuasarDescriptorRegistry,
    expected_rsp_name: &str,
    seed: u64,
) -> Result<GoogleAny, (u32, String)> {
    let desc = descriptors
        .get_message(expected_rsp_name)
        .ok_or_else(|| (STATUS_NOT_FOUND, format!("Unknown response type '{expected_rsp_name}'")))?;

    let sample = descriptors
        .instantiate_populated_sample(&desc.full_name, seed)
        .map_err(|e| (STATUS_INTERNAL_ERROR, format!("Failed to instantiate sample: {e}")))?;

    let bytes = descriptors
        .encode_dynamic(&sample)
        .map_err(|e| (STATUS_INTERNAL_ERROR, format!("Failed to encode sample: {e}")))?;

    Ok(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{}", desc.full_name),
        value: bytes,
    })
}

pub const GROUP_1_REQUEST_PAIRS: &[(&str, &str)] = &[
    ("eve.cdn.api.GetActiveURLSigningKeyRequest", "eve.cdn.api.GetActiveURLSigningKeyResponse"),
    ("eve.cdn.api.SignURLRequest", "eve.cdn.api.SignURLResponse"),
    ("eve.legal.agreement.api.GetLatestRequest", "eve.legal.agreement.api.GetLatestResponse"),
    ("eve.localization.cerberus.api.GetLocalizedTextRequest", "eve.localization.cerberus.api.GetLocalizedTextResponse"),
    ("eve.localization.message.api.GetLocalizedTextRequest", "eve.localization.message.api.GetLocalizedTextResponse"),
    ("eve.pii.ipaddress.GetRequest", "eve.pii.ipaddress.GetResponse"),
    ("eve.pii.user.GetRequest", "eve.pii.user.GetResponse"),
    ("eve.user.GetByCharacterRequest", "eve.user.GetByCharacterResponse"),
    ("eve.user.GetByNameRequest", "eve.user.GetByNameResponse"),
    ("eve.user.GetConnectionHistoryRequest", "eve.user.GetConnectionHistoryResponse"),
    ("eve.user.GetCreateDateRequest", "eve.user.GetCreateDateResponse"),
    ("eve.user.GetOnlineCharactersRequest", "eve.user.GetOnlineCharactersResponse"),
    ("eve.user.GetRequest", "eve.user.GetResponse"),
    ("eve.user.IsAdministratorRequest", "eve.user.IsAdministratorResponse"),
    ("eve.user.IsBannedRequest", "eve.user.IsBannedResponse"),
    ("eve.user.LockCharacterOwnershipRequest", "eve.user.LockCharacterOwnershipResponse"),
    ("eve.user.LogOutUserRequest", "eve.user.LogOutUserResponse"),
    ("eve.user.LogoutUserAlertDialogRequest", "eve.user.LogoutUserAlertDialogResponse"),
    ("eve.user.ScramblePasswordRequest", "eve.user.ScramblePasswordResponse"),
    ("eve.user.api.BanRequest", "eve.user.api.BanResponse"),
    ("eve.user.api.CreateDefaultContactsRequest", "eve.user.api.CreateDefaultContactsResponse"),
    ("eve.user.api.CreateRequest", "eve.user.api.CreateResponse"),
    ("eve.user.api.DeletePersonalDataRequest", "eve.user.api.DeletePersonalDataResponse"),
    ("eve.user.api.GetAllByEmailRequest", "eve.user.api.GetAllByEmailResponse"),
    ("eve.user.api.GetContactDetailsRequest", "eve.user.api.GetContactDetailsResponse"),
    ("eve.user.api.GetDetailsRequest", "eve.user.api.GetDetailsResponse"),
    ("eve.user.api.GetHistoryRequest", "eve.user.api.GetHistoryResponse"),
    ("eve.user.api.admin.SetEmailAddressRequest", "eve.user.api.admin.SetEmailAddressResponse"),
    ("eve.user.ban.BanRequest", "eve.user.ban.BanResponse"),
    ("eve.user.ban.GetBanRequest", "eve.user.ban.GetBanResponse"),
    ("eve.user.ban.MassPermaBanUserRequest", "eve.user.ban.BanResponse"),
    ("eve.user.ban.MassTempBanUserRequest", "eve.user.ban.BanResponse"),
    ("eve.user.ban.PermaBanUserRequest", "eve.user.ban.BanResponse"),
    ("eve.user.ban.SendBanMailRequest", "eve.user.ban.SendBanMailResponse"),
    ("eve.user.ban.TempBanUserRequest", "eve.user.ban.BanResponse"),
    ("eve.user.botting.GetReportersRequest", "eve.user.botting.GetReportersResponse"),
    ("eve.user.character.ChangeOwnershipRequest", "eve.user.character.GetCharactersResponse"),
    ("eve.user.character.GetCharactersRequest", "eve.user.character.GetCharactersResponse"),
    ("eve.user.character.api.GetDetailsRequest", "eve.user.character.api.GetDetailsResponse"),
    ("eve.user.character.transfer.GetAvailabilityRequest", "eve.user.character.transfer.GetAvailabilityResponse"),
    ("eve.user.character.transfer.GetPendingByUserRequest", "eve.user.character.transfer.GetPendingByUserResponse"),
    ("eve.user.character.transfer.QueueRequest", "eve.user.character.transfer.QueueResponse"),
    ("eve.user.character.transfer.ValidateRequest", "eve.user.character.transfer.ValidateResponse"),
    ("eve.user.character_training.GetSlotsRequest", "eve.user.character_training.GetSlotsResponse"),
    ("eve.user.email.api.SendBotReportingRequest", "eve.user.email.api.SendBotReportingResponse"),
    ("eve.user.email.api.SendCharacterTransferReceiverRequest", "eve.user.email.api.SendCharacterTransferReceiverResponse"),
    ("eve.user.email.api.SendCharacterTransferSenderRequest", "eve.user.email.api.SendCharacterTransferSenderResponse"),
    ("eve.user.email.api.SendChargebackBanRequest", "eve.user.email.api.SendChargebackBanResponse"),
    ("eve.user.email.api.SendEmailChangeNotificationRequest", "eve.user.email.api.SendEmailChangeNotificationResponse"),
    ("eve.user.email.api.SendEmailVerificationRequest", "eve.user.email.api.SendEmailVerificationResponse"),
    ("eve.user.email.api.SendPaymentFailedRequest", "eve.user.email.api.SendPaymentFailedResponse"),
    ("eve.user.email.api.SendPaymentSucceededRequest", "eve.user.email.api.SendPaymentSucceededResponse"),
    ("eve.user.email.api.SendPaymentSucceededWithTaxRequest", "eve.user.email.api.SendPaymentSucceededWithTaxResponse"),
    ("eve.user.email.api.SendResetPasswordRequest", "eve.user.email.api.SendResetPasswordResponse"),
    ("eve.user.email.api.SendResetPasswordSendUsernamesRequest", "eve.user.email.api.SendResetPasswordSendUsernamesResponse"),
    ("eve.user.email.api.SendSubscriptionRenewalFailedRequest", "eve.user.email.api.SendSubscriptionRenewalFailedResponse"),
    ("eve.user.email.api.SendSubscriptionRenewalReminderForTaxRequest", "eve.user.email.api.SendSubscriptionRenewalReminderForTaxResponse"),
    ("eve.user.email.api.SendSubscriptionRenewalSuccessRequest", "eve.user.email.api.SendSubscriptionRenewalSuccessResponse"),
    ("eve.user.email.api.SendSubscriptionRenewalSuccessWithTaxRequest", "eve.user.email.api.SendSubscriptionRenewalSuccessWithTaxResponse"),
    ("eve.user.email.api.SendSubscriptonRenewalFailedRequest", "eve.user.email.api.SendSubscriptonRenewalFailedResponse"),
    ("eve.user.email.api.SendTwoFactorAuthenticationRequest", "eve.user.email.api.SendTwoFactorAuthenticationResponse"),
    ("eve.user.email.api.SendVerifyEmailChangeRequest", "eve.user.email.api.SendVerifyEmailChangeResponse"),
    ("eve.user.email.api.SendVerifyNewEmailRequest", "eve.user.email.api.SendVerifyNewEmailResponse"),
    ("eve.user.email.api.SendWelcomeRequest", "eve.user.email.api.SendWelcomeResponse"),
    ("eve.user.entitlement.GetEntitlementsRequest", "eve.user.entitlement.GetEntitlementsResponse"),
    ("eve.user.entitlement.api.AddOmegatimeRequest", "eve.user.entitlement.api.AddOmegatimeResponse"),
    ("eve.user.entitlement.api.GetAllForUserRequest", "eve.user.entitlement.api.GetAllForUserResponse"),
    ("eve.user.entitlement.api.GrantRequest", "eve.user.entitlement.api.GrantResponse"),
    ("eve.user.gdpr.PurgeUserDataRequest", "eve.user.gdpr.PurgeUserDataResponse"),
    ("eve.user.legal.agreement.api.AcceptRequest", "eve.user.legal.agreement.api.AcceptResponse"),
    ("eve.user.legal.agreement.api.GetAgreementsOfTypeRequest", "eve.user.legal.agreement.api.GetAgreementsOfTypeResponse"),
    ("eve.user.legal.agreement.api.GetAgreementsRequest", "eve.user.legal.agreement.api.GetAgreementsResponse"),
    ("eve.user.licence.api.GetAllForUserRequest", "eve.user.licence.api.GetAllForUserResponse"),
    ("eve.user.licence.api.GrantRequest", "eve.user.licence.api.GrantResponse"),
    ("eve.user.licence.api.SetRequest", "eve.user.licence.api.SetResponse"),
    ("eve.user.license.api.GetAllByExpiryRequest", "eve.user.license.api.GetAllByExpiryResponse"),
    ("eve.user.license.api.GetAllForUserRequest", "eve.user.license.api.GetAllForUserResponse"),
    ("eve.user.license.api.GetRequest", "eve.user.license.api.GetResponse"),
    ("eve.user.license.api.GrantRequest", "eve.user.license.api.GrantResponse"),
    ("eve.user.license.api.SetExpiryDateRequest", "eve.user.license.api.SetExpiryDateResponse"),
    ("eve.user.license.api.admin.UpsertWithExpiryRequest", "eve.user.license.api.admin.UpsertWithExpiryResponse"),
    ("eve.user.note.AddRequest", "eve.user.note.AddResponse"),
    ("eve.user.note.DeleteRequest", "eve.user.note.DeleteResponse"),
    ("eve.user.note.EditRequest", "eve.user.note.EditResponse"),
    ("eve.user.note.GetNotesByUserRequest", "eve.user.note.GetNotesByUserResponse"),
    ("eve.user.note.GetRequest", "eve.user.note.GetResponse"),
    ("eve.user.penalty.CheckIsUserPenalizedRequest", "eve.user.penalty.CheckIsUserPenalizedResponse"),
    ("eve.user.recruitment.api.AcceptInviteRequest", "eve.user.recruitment.api.AcceptInviteResponse"),
    ("eve.user.recruitment.api.CreateInviteRequest", "eve.user.recruitment.api.CreateInviteResponse"),
    ("eve.user.recruitment.api.GetFriendsRequest", "eve.user.recruitment.api.GetFriendsResponse"),
    ("eve.user.recruitment.api.GetMilestoneRewardsRequest", "eve.user.recruitment.api.GetMilestoneRewardsResponse"),
    ("eve.user.recruitment.api.GetRecruiterByFriendRequest", "eve.user.recruitment.api.GetRecruiterByFriendResponse"),
    ("eve.user.recruitment.api.GetRecruiterByTokenRequest", "eve.user.recruitment.api.GetRecruiterByTokenResponse"),
    ("eve.user.recruitment.api.GetTokenRequest", "eve.user.recruitment.api.GetTokenResponse"),
    ("eve.user.redemption.GetRedeemableTokensRequest", "eve.user.redemption.GetRedeemableTokensResponse"),
    ("eve.user.redemption.InsertRedeemableTokenRequest", "eve.user.redemption.InsertRedeemableTokenResponse"),
    ("eve.user.redemption.RedeemTokensRequest", "eve.user.redemption.RedeemTokensResponse"),
    ("eve.user.subscription.GetStateRequest", "eve.user.subscription.GetStateResponse"),
    ("eve.user.subscription.api.GetActiveRequest", "eve.user.subscription.api.GetActiveResponse"),
    ("eve.user.subscription.api.GetAllForUserRequest", "eve.user.subscription.api.GetAllForUserResponse"),
    ("eve.user.subscription.api.GetAllLogsRequest", "eve.user.subscription.api.GetAllLogsResponse"),
    ("eve.user.subscription.api.GetLogRequest", "eve.user.subscription.api.GetLogResponse"),
    ("eve.user.subscription.api.GetRequest", "eve.user.subscription.api.GetResponse"),
    ("eve.user.subscription.api.GetStateRequest", "eve.user.subscription.api.GetStateResponse"),
    ("eve.user.subscription.api.SubscriptionRevokeRequest", "eve.user.subscription.api.SubscriptionRevokeResponse"),
    ("eve.user.subscription.api.admin.CancelRequest", "eve.user.subscription.api.admin.CancelResponse"),
    ("eve.user.subscription.api.admin.RescheduleRequest", "eve.user.subscription.api.admin.RescheduleResponse"),
    ("eve.user.tag.AddRequest", "eve.user.tag.GetResponse"),
    ("eve.user.tag.GetAllRequest", "eve.user.tag.GetAllResponse"),
    ("eve.user.tag.GetRequest", "eve.user.tag.GetResponse"),
    ("eve.user.tag.api.AddRequest", "eve.user.tag.api.AddResponse"),
    ("eve.user.tag.api.RemoveRequest", "eve.user.tag.api.RemoveResponse"),
    ("eve.user.twofactor.api.GetAuthenticatorEnabledRequest", "eve.user.twofactor.api.GetAuthenticatorEnabledResponse"),
    ("eve.user.twofactor.api.admin.DisableAuthenticatorRequest", "eve.user.twofactor.api.admin.DisableAuthenticatorResponse"),
    ("eve_public.auth.AuthenticateUsernameRequest", "eve_public.auth.AuthenticateUsernameResponse"),
    ("eve_public.auth.crypto.rsa.GetPublicKeyRequest", "eve_public.auth.crypto.rsa.GetPublicKeyResponse"),
    ("eve_public.auth.steam.AuthenticateSteamRequest", "eve_public.auth.steam.AuthenticateSteamResponse"),
    ("eve_public.user.license.api.GetRequest", "eve_public.user.license.api.GetResponse"),
];

pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let descriptors = Arc::clone(&router.descriptors);
    let store = get_identity_store();

    // 1. Specific custom handlers with live database/tenancy semantics

    // eve_public.auth.AuthenticateUsernameRequest -> eve_public.auth.AuthenticateUsernameResponse
    let db_auth = Arc::clone(&db);
    router.register_handler(
        "eve_public.auth.AuthenticateUsernameRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let mut username = "anonymous_pilot".to_string();
            if let Some(DynamicValue::Message(creds)) = dyn_msg.get(1) {
                if let Some(DynamicValue::String(u)) = creds.get(1) {
                    if !u.is_empty() {
                        username = u.clone();
                    }
                }
            }
            let _ = db_auth.find_user_id_by_username(&username);
            let mut token_result = Vec::new();
            encode_string_field(1, &format!("evomu-auth-token-{username}"), &mut token_result);
            encode_varint_field(2, 0, &mut token_result); // is_pwned_password = false

            let mut out = Vec::new();
            encode_bytes_field(1, &token_result, &mut out); // field 1: login_successful
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.auth.AuthenticateUsernameResponse"),
                value: out,
            })
        }),
    );

    // eve.user.character.GetCharactersRequest -> eve.user.character.GetCharactersResponse
    let db_chars = Arc::clone(&db);
    router.register_handler(
        "eve.user.character.GetCharactersRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = if let Some(DynamicValue::Message(uident)) = dyn_msg.get(1) {
                if let Some(DynamicValue::Int32(id)) = uident.get(1) {
                    *id as u64
                } else {
                    ctx.effective_user_id().unwrap_or(1) as u64
                }
            } else {
                ctx.effective_user_id().unwrap_or(1) as u64
            };
            let chars = db_chars.get_characters_for_user(uid);
            let mut out = Vec::new();
            for ch in chars {
                let mut cident = Vec::new();
                encode_varint_field(1, ch.character_id as u64, &mut cident);
                encode_bytes_field(1, &cident, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.user.character.GetCharactersResponse"),
                value: out,
            })
        }),
    );

    // eve.user.api.GetDetailsRequest -> eve.user.api.GetDetailsResponse
    router.register_handler(
        "eve.user.api.GetDetailsRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1);
            let mut user_attr = Vec::new();
            encode_string_field(1, &format!("user_{uid}"), &mut user_attr);
            let mut out = Vec::new();
            encode_bytes_field(1, &user_attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.user.api.GetDetailsResponse"),
                value: out,
            })
        }),
    );

    // eve.user.api.admin.SetEmailAddressRequest -> SetEmailAddressResponse
    let store_email_set = Arc::clone(&store);
    router.register_handler(
        "eve.user.api.admin.SetEmailAddressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            if let Some(DynamicValue::String(new_email)) = dyn_msg.get(2) {
                store_email_set
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .user_emails
                    .insert(uid, new_email.clone());
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.user.api.admin.SetEmailAddressResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve.user.email.api.SendWelcomeRequest -> SendWelcomeResponse
    let store_email_welcome = Arc::clone(&store);
    router.register_handler(
        "eve.user.email.api.SendWelcomeRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let email = store_email_welcome
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .user_emails
                .get(&uid)
                .cloned()
                .unwrap_or_else(|| format!("pilot{uid}@evomu.local"));
            let mut out = Vec::new();
            encode_string_field(1, &email, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.user.email.api.SendWelcomeResponse"),
                value: out,
            })
        }),
    );

    // eve.user.license.api.GrantRequest -> enqueues eve_public.user.license.api.ActivatedNotice
    let store_lic_grant = Arc::clone(&store);
    let router_pending = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.user.license.api.GrantRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let expiry = ctx.received.as_ref().map(|t| t.seconds as u64).unwrap_or(0) + 30 * 86400;
            store_lic_grant
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .omega_subscriptions
                .insert(uid, expiry);

            // Publish license activation notice to user
            let mut notice_bytes = Vec::new();
            encode_string_field(1, "omega", &mut notice_bytes);
            router_pending
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.ActivatedNotice"),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.user.license.api.GrantResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve.cdn.api.SignURLRequest -> SignURLResponse
    router.register_handler(
        "eve.cdn.api.SignURLRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let url = if let Some(DynamicValue::String(u)) = dyn_msg.get(1) {
                u.clone()
            } else {
                "https://cdn.eveonline.com/asset/v1.png".to_string()
            };
            let mut out = Vec::new();
            encode_string_field(1, &format!("{url}?token=evomu-sig-valid"), &mut out);
            encode_string_field(3, "evomu-cdn-key-1", &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cdn.api.SignURLResponse"),
                value: out,
            })
        }),
    );

    // eve.cdn.api.GetActiveURLSigningKeyRequest -> GetActiveURLSigningKeyResponse
    router.register_handler(
        "eve.cdn.api.GetActiveURLSigningKeyRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_string_field(1, "evomu-cdn-key-1", &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cdn.api.GetActiveURLSigningKeyResponse"),
                value: out,
            })
        }),
    );

    // eve.legal.agreement.api.GetLatestRequest -> GetLatestResponse
    router.register_handler(
        "eve.legal.agreement.api.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut doc = Vec::new();
            encode_string_field(1, "EVE Online End User License Agreement V24.01", &mut doc);
            let mut out = Vec::new();
            encode_bytes_field(1, &doc, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.legal.agreement.api.GetLatestResponse"),
                value: out,
            })
        }),
    );

    // eve.user.legal.agreement.api.AcceptRequest -> AcceptResponse
    router.register_handler(
        "eve.user.legal.agreement.api.AcceptRequest",
        Arc::new(move |ctx, any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let agreement_id = if !any.value.is_empty() {
                String::from_utf8_lossy(&any.value).into_owned()
            } else {
                "eula_v24".to_string()
            };
            get_identity_store()
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .accepted_agreements
                .entry(uid)
                .or_default()
                .push(agreement_id);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.user.legal.agreement.api.AcceptResponse"),
                value: vec![],
            })
        }),
    );

    // eve.localization.message.api.GetLocalizedTextRequest -> GetLocalizedTextResponse
    router.register_handler(
        "eve.localization.message.api.GetLocalizedTextRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_string_field(1, "Welcome to EVE Online Tranquility", &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.localization.message.api.GetLocalizedTextResponse"),
                value: out,
            })
        }),
    );

    // 2. Register all other Group 1 request pairs using descriptors
    for &(req_type, rsp_type) in GROUP_1_REQUEST_PAIRS {
        let req_norm = QuasarDescriptorRegistry::normalize_type_name(req_type).to_string();
        let rsp_norm = QuasarDescriptorRegistry::normalize_type_name(rsp_type).to_string();
        let desc_arc = Arc::clone(&descriptors);
        let rsp_name_clone = rsp_norm.clone();

        // Skip if already registered with custom logic above
        if matches!(
            req_norm.as_str(),
            "eve_public.auth.AuthenticateUsernameRequest"
                | "eve.user.character.GetCharactersRequest"
                | "eve.user.api.GetDetailsRequest"
                | "eve.user.api.admin.SetEmailAddressRequest"
                | "eve.user.email.api.SendWelcomeRequest"
                | "eve.user.license.api.GrantRequest"
                | "eve.cdn.api.SignURLRequest"
                | "eve.cdn.api.GetActiveURLSigningKeyRequest"
                | "eve.legal.agreement.api.GetLatestRequest"
                | "eve.user.legal.agreement.api.AcceptRequest"
                | "eve.localization.message.api.GetLocalizedTextRequest"
        ) {
            continue;
        }

        router.register_handler(
            &req_norm,
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_generic_response(&desc_arc, &rsp_name_clone, 0x1001)
            }),
        );
    }
}
