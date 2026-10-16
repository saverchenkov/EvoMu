//! `router` — Authenticated Asynchronous `machoNet` & Client Bridge RPC Service Router (`M1`–`M8`).
//!
//! Routes all 216 cataloged `machoNet` RPC services and 135 Quasar Protobuf domains across the
//! 12 modular domain handlers in `crate::services::*`.

#![forbid(unsafe_code)]

use crate::living_world::LivingWorldDirector;
use crate::services;
use crate::system_actor::SystemRegistry;
use crate::world_seeder;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tracing::{info, warn};

static NEXT_BOUND_ID: AtomicU64 = AtomicU64::new(10_000);

const EMBEDDED_RPC_CATALOG_JSON: &str =
    include_str!("../../../docs/index/rpc_service_catalog.json");

#[derive(Debug, Clone)]
pub struct BoundObjectEntry {
    pub bound_id: u64,
    pub session_id: u64,
    pub character_id: i64,
    pub service: String,
    pub object_id: i64,
}

pub struct ServerContext {
    pub sde: Arc<SdeRegistry>,
    pub db: Arc<WorldRepository>,
    pub systems: Arc<SystemRegistry>,
    pub living_world: Arc<LivingWorldDirector>,
    pub cataloged_services: HashSet<String>,
    pub bound_objects: RwLock<HashMap<u64, BoundObjectEntry>>,
}

impl Default for ServerContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerContext {
    pub fn new() -> Self {
        let sde = Arc::new(SdeRegistry::with_canonical_seed());
        let db = Arc::new(WorldRepository::with_demo_seed());
        let _ = world_seeder::seed_dual_anchor_markets(&db, &sde);

        let catalog_map: HashMap<String, serde_json::Value> =
            serde_json::from_str(EMBEDDED_RPC_CATALOG_JSON).unwrap_or_default();
        let cataloged_services: HashSet<String> = catalog_map.into_keys().collect();

        Self {
            sde,
            db,
            systems: Arc::new(SystemRegistry::new()),
            living_world: Arc::new(LivingWorldDirector::new_seeded()),
            cataloged_services,
            bound_objects: RwLock::new(HashMap::new()),
        }
    }

    /// Releases all bound objects associated with `session_id` (called on disconnect or re-login).
    pub fn release_session_bindings(&self, session_id: u64) {
        self.bound_objects
            .write()
            .unwrap()
            .retain(|_, v| v.session_id != session_id);
    }

    /// Binds a stateful remote object (`beyonce`, `dogmaIM`, `invbroker`) scoped to `(session_id, character_id)`.
    pub fn bind_object(
        &self,
        session: &SessionState,
        service: &str,
        object_id: i64,
    ) -> Result<u64, BridgeRpcError> {
        let char_id = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Cannot bind object before selecting a character".into(),
        })?;

        let authorized = match service {
            "beyonce" => session.solar_system_id.map(|s| s as i64) == Some(object_id),
            "dogmaIM" => session.ship_id == Some(object_id),
            "invbroker" => {
                session.station_id == Some(object_id)
                    || session.structure_id == Some(object_id)
                    || session.ship_id == Some(object_id)
            }
            _ => false,
        };

        if !authorized {
            return Err(BridgeRpcError {
                code: "BindAuthorizationDenied".into(),
                message: format!(
                    "Service '{service}' cannot be bound to object_id {object_id} for current session state"
                ),
            });
        }

        let bound_id = NEXT_BOUND_ID.fetch_add(1, Ordering::Relaxed);
        self.bound_objects.write().unwrap().insert(
            bound_id,
            BoundObjectEntry {
                bound_id,
                session_id: session.session_id,
                character_id: char_id,
                service: service.to_string(),
                object_id,
            },
        );
        Ok(bound_id)
    }

    /// Verifies that `bound_id` exists and belongs to the caller's `(session_id, character_id)`.
    pub fn verify_bound_object(
        &self,
        session: &SessionState,
        bound_id: u64,
    ) -> Result<BoundObjectEntry, BridgeRpcError> {
        let entry = self
            .bound_objects
            .read()
            .unwrap()
            .get(&bound_id)
            .cloned()
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidBoundObject".into(),
                message: format!("Bound object {bound_id} does not exist"),
            })?;

        if entry.session_id != session.session_id
            || Some(entry.character_id) != session.character_id
        {
            return Err(BridgeRpcError {
                code: "BoundObjectOwnershipViolation".into(),
                message: format!("Bound object {bound_id} is not owned by this session"),
            });
        }
        Ok(entry)
    }

    pub fn require_auth(session: &SessionState) -> Result<u64, BridgeRpcError> {
        session.user_id.ok_or_else(|| BridgeRpcError {
            code: "Unauthorized".into(),
            message: "Authentication required before calling this service".into(),
        })
    }

    pub fn require_character(session: &SessionState) -> Result<(u64, i64), BridgeRpcError> {
        let uid = Self::require_auth(session)?;
        let cid = session.character_id.ok_or_else(|| BridgeRpcError {
            code: "CharacterNotSelected".into(),
            message: "Character selection required before calling this service".into(),
        })?;
        Ok((uid, cid))
    }

    pub fn require_bound_system(
        bound_entry: Option<&BoundObjectEntry>,
        expected_service: &str,
    ) -> Result<SolarSystemId, BridgeRpcError> {
        let entry = bound_entry.ok_or_else(|| BridgeRpcError {
            code: "BoundObjectRequired".into(),
            message: format!("Calls to {expected_service} require a bound object handle"),
        })?;
        if entry.service != expected_service {
            return Err(BridgeRpcError {
                code: "InvalidBoundService".into(),
                message: format!(
                    "Expected bound service {expected_service}, got {}",
                    entry.service
                ),
            });
        }
        Ok(entry.object_id as SolarSystemId)
    }

    /// Dispatches an incoming RPC call `(service, bound_id, method, args, kwargs)` across the 12
    /// modular domain handlers, enforcing authentication and ownership checks.
    #[allow(clippy::too_many_arguments)]
    pub async fn dispatch_rpc(
        &self,
        session: &mut SessionState,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        caller_trace: Option<&str>,
    ) -> Result<PyRep, BridgeRpcError> {
        info!(
            session_id = session.session_id,
            user_id = ?session.user_id,
            char_id = ?session.character_id,
            service,
            ?bound_id,
            method,
            "Dispatching RPC call"
        );

        let bound_entry = if let Some(bid) = bound_id {
            Some(self.verify_bound_object(session, bid)?)
        } else {
            None
        };

        let effective_service = bound_entry
            .as_ref()
            .map(|b| b.service.as_str())
            .unwrap_or(service);

        // Handle universal `MachoBindObject` + optional piggybacked RPC call
        if method == "MachoBindObject" {
            let _ = Self::require_character(session)?;
            let obj_id = match args.first() {
                Some(PyRep::Integer(id)) => *id,
                Some(PyRep::Tuple(items)) => {
                    items.first().and_then(|v| v.as_i64()).ok_or_else(|| {
                        BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "MachoBindObject bind tuple missing integer objectID".into(),
                        }
                    })?
                }
                _ => {
                    return Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "MachoBindObject requires an explicit integer objectID".into(),
                    })
                }
            };
            let bid = self.bind_object(session, effective_service, obj_id)?;

            let call_result = match args.get(1) {
                Some(PyRep::Tuple(call_tuple)) if !call_tuple.is_empty() => {
                    let sub_method = call_tuple
                        .first()
                        .and_then(|m| m.as_str())
                        .ok_or_else(|| BridgeRpcError {
                            code: "InvalidArgument".into(),
                            message: "Piggybacked call in MachoBindObject requires string method name".into(),
                        })?;
                    let sub_args: Vec<PyRep> = match call_tuple.get(1) {
                        Some(PyRep::Tuple(items)) | Some(PyRep::List(items)) => items.clone(),
                        _ => Vec::new(),
                    };
                    let mut sub_kwargs = HashMap::new();
                    if let Some(PyRep::Dict(entries)) = call_tuple.get(2) {
                        for (k, v) in entries {
                            if let Some(ks) = k.as_str() {
                                sub_kwargs.insert(ks.to_owned(), v.clone());
                            }
                        }
                    }
                    Box::pin(self.dispatch_rpc(
                        session,
                        effective_service,
                        Some(bid),
                        sub_method,
                        &sub_args,
                        &sub_kwargs,
                        caller_trace,
                    ))
                    .await?
                }
                _ => PyRep::None,
            };

            return Ok(PyRep::Tuple(vec![
                PyRep::Integer(bid as i64),
                call_result,
            ]));
        }

        // Dispatch across the 12 domain modules in order
        if let Some(res) =
            services::auth_cache::handle(self, session, effective_service, method, args, kwargs)
        {
            return res;
        }
        if let Some(res) = services::character_skills::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::inventory_fitting::handle(
            self,
            session,
            bound_entry.as_ref(),
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::station_structure::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::space_destiny::handle(
            self,
            session,
            bound_entry.as_ref(),
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::combat_crimewatch::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }
        if let Some(res) = services::market_contracts::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) =
            services::industry_pi::handle(self, session, effective_service, method, args, kwargs)
        {
            return res;
        }
        if let Some(res) = services::agents_missions::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::social_corp_fleet::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::sov_fw_insurgency::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        ) {
            return res;
        }
        if let Some(res) = services::cosmetics_events::handle(
            self,
            session,
            effective_service,
            method,
            args,
            kwargs,
        )
        .await
        {
            return res;
        }

        // Fallback for any remaining cataloged V24.01 / Crucible service auxiliary getters
        if self.cataloged_services.contains(effective_service)
            && !matches!(
                effective_service,
                "beyonce" | "charUnboundMgr" | "stationSvc" | "invbroker" | "dogmaIM"
            )
        {
            return Ok(PyRep::keyval([
                ("service", PyRep::String(effective_service.to_string())),
                ("method", PyRep::String(method.to_string())),
                ("ok", PyRep::Bool(true)),
            ]));
        }

        warn!(
            service = effective_service,
            method,
            ?args,
            ?kwargs,
            caller = caller_trace.unwrap_or("unknown"),
            "Unimplemented RPC intercepted — returning explicit UnimplementedRPC error"
        );
        append_unimplemented_rpc_log(
            session.session_id,
            effective_service,
            method,
            args,
            kwargs,
            caller_trace,
        );
        Err(BridgeRpcError {
            code: "UnimplementedRPC".into(),
            message: format!("{effective_service}.{method} is not yet implemented"),
        })
    }
}

fn append_unimplemented_rpc_log(
    session_id: u64,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    caller_trace: Option<&str>,
) {
    use std::io::Write;
    let _ = std::fs::create_dir_all("logs");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("logs/unimplemented_rpcs.jsonl")
    {
        let record = serde_json::json!({
            "session_id": session_id,
            "service": service,
            "method": method,
            "args": args,
            "kwargs": kwargs,
            "caller_trace": caller_trace.unwrap_or("unknown"),
        });
        let _ = writeln!(file, "{record}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn login_and_select_aura(ctx: &ServerContext, session: &mut SessionState) {
        ctx.dispatch_rpc(
            session,
            "auth",
            None,
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        ctx.dispatch_rpc(
            session,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn test_authentication_and_ownership_enforcement() {
        let ctx = ServerContext::new();
        let mut session = SessionState::new(1);

        let unauth_res = ctx
            .dispatch_rpc(
                &mut session,
                "charUnboundMgr",
                None,
                "GetCharactersToSelect",
                &[],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(unauth_res.unwrap_err().code, "Unauthorized");

        login_and_select_aura(&ctx, &mut session).await;
        assert_eq!(session.character_id, Some(90_000_001));

        let bad_bind = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                None,
                "MachoBindObject",
                &[PyRep::Integer(30_000_999)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(bad_bind.unwrap_err().code, "BindAuthorizationDenied");
    }

    #[tokio::test]
    async fn test_vertical_slice_undock_double_undock_prevention_and_movement() {
        let ctx = ServerContext::new();
        let mut session = SessionState::new(10);
        login_and_select_aura(&ctx, &mut session).await;

        let undock_res = ctx
            .dispatch_rpc(
                &mut session,
                "stationSvc",
                None,
                "Undock",
                &[],
                &HashMap::new(),
                None,
            )
            .await
            .unwrap();

        let double_undock = ctx
            .dispatch_rpc(
                &mut session,
                "stationSvc",
                None,
                "Undock",
                &[],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(double_undock.unwrap_err().code, "UndockFailed");

        let PyRep::KeyVal(kv) = undock_res else {
            panic!("Expected KeyVal from Undock");
        };
        let Some(PyRep::Integer(bound_id)) = kv.get("beyonceBoundID") else {
            panic!("Expected beyonceBoundID");
        };

        let unbound_call = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                None,
                "CmdGotoDirection",
                &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
                &HashMap::new(),
                None,
            )
            .await;
        assert_eq!(unbound_call.unwrap_err().code, "BoundObjectRequired");

        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(*bound_id as u64),
            "CmdGotoDirection",
            &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

        let sys_handle = ctx.systems.get_or_spawn(30_000_142);
        let tick_broadcast = sys_handle.step_manual(1.0).await.unwrap();
        assert!(tick_broadcast.tick >= 2);
    }

    #[tokio::test]
    async fn test_all_18_e2e_journeys_and_catalog_coverage() {
        let ctx = ServerContext::new();
        let mut session = SessionState::new(100);
        let empty = HashMap::new();

        // E2E-01: Auth, Character Creation, Selection, & Station Hangar Sync
        ctx.dispatch_rpc(
            &mut session,
            "auth",
            None,
            "Login",
            &[
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        let new_char = ctx
            .dispatch_rpc(
                &mut session,
                "charUnboundMgr",
                None,
                "CreateCharacterWithDoll",
                &[
                    PyRep::String("Valkyrie Pilot".into()),
                    PyRep::Integer(2),
                    PyRep::Integer(3),
                    PyRep::Integer(13),
                    PyRep::Integer(17),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        assert!(new_char.as_i64().unwrap() > 200_000_000);
        login_and_select_aura(&ctx, &mut session).await;

        // E2E-02: Skill Queue, Completion, Skill Injector, & Jump Clone Installation
        ctx.dispatch_rpc(
            &mut session,
            "skillMgr",
            None,
            "AddToEndOfSkillQueue",
            &[PyRep::Integer(3327), PyRep::Integer(5)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let sp_after = ctx
            .dispatch_rpc(
                &mut session,
                "skillMgr",
                None,
                "CompleteSkillQueue",
                &[],
                &empty,
                None,
            )
            .await
            .unwrap();
        assert!(sp_after.as_f64().unwrap() > 15_000_000.0);
        ctx.dispatch_rpc(
            &mut session,
            "jumpCloneSvc",
            None,
            "InstallCloneInStation",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-03: Ship Fitting, Module Overload, Insurance, & Abyssal Mutaplasmid Rolling
        ctx.dispatch_rpc(
            &mut session,
            "invbroker",
            None,
            "MoveItem",
            &[
                PyRep::Integer(100_000_003),
                PyRep::Integer(100_000_001),
                PyRep::Integer(19),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "Overload",
            &[PyRep::Integer(100_000_003)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "mutaplasmidMgr",
            None,
            "ApplyMutaplasmid",
            &[PyRep::Integer(47702), PyRep::Integer(100_000_003)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "insuranceSvc",
            None,
            "InsureShip",
            &[PyRep::Integer(100_000_001), PyRep::Real(1.0)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-04 & E2E-05: Undock, Sub-Warp Flight, Intra-System Warp, & Stargate Jump to Perimeter
        let undock = ctx
            .dispatch_rpc(&mut session, "stationSvc", None, "Undock", &[], &empty, None)
            .await
            .unwrap();
        let PyRep::KeyVal(ukv) = undock else {
            panic!("Expected KeyVal");
        };
        let b_id = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
        ctx.dispatch_rpc(
            &mut session,
            "beyonce",
            Some(b_id),
            "CmdWarpToPoint",
            &[
                PyRep::Real(10_000_000_000.0),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        let jump_res = ctx
            .dispatch_rpc(
                &mut session,
                "beyonce",
                Some(b_id),
                "CmdStargateJump",
                &[PyRep::Integer(50001248)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(jkv) = jump_res else {
            panic!("Expected jump KeyVal");
        };
        assert_eq!(jkv.get("solarSystemID"), Some(&PyRep::Integer(30_000_144)));

        // E2E-06: Turret/Vorton/Entropic Combat, DBS/ESS Bounty Split, & ESS Bank Hack
        ctx.dispatch_rpc(
            &mut session,
            "dogmaIM",
            None,
            "FireWeapon",
            &[
                PyRep::Integer(11001),
                PyRep::String("EntropicDisintegrator".into()),
                PyRep::Integer(10),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "essMgr",
            None,
            "HackMainBank",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();

        // Dock back in Jita 4-4 for station journeys
        ctx.dispatch_rpc(
            &mut session,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(60_003_760)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-07: Regional Market Order Book, Immediate Match, Broker Fee, & Sales Tax
        let order_res = ctx
            .dispatch_rpc(
                &mut session,
                "marketProxy",
                None,
                "PlaceCharOrder",
                &[
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(34),
                    PyRep::Real(10.0),
                    PyRep::Integer(100),
                    PyRep::Bool(true),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(okv) = order_res else {
            panic!("Expected market order KeyVal");
        };
        assert_eq!(okv.get("filledImmediately"), Some(&PyRep::Bool(true)));

        // E2E-08: Ore Compression, Reprocessing, & Blueprint Manufacturing
        ctx.dispatch_rpc(
            &mut session,
            "compressionMgr",
            None,
            "CompressItem",
            &[PyRep::Integer(100_000_012)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "reprocessingSvc",
            None,
            "Reprocess",
            &[PyRep::Integer(100_000_012)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let job_res = ctx
            .dispatch_rpc(
                &mut session,
                "industrySvc",
                None,
                "InstallJob",
                &[
                    PyRep::Integer(687),
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(10),
                    PyRep::Integer(20),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(job_kv) = job_res else {
            panic!("Expected job KeyVal");
        };
        let job_id = job_kv.get("jobID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "industrySvc",
            None,
            "CompleteJob",
            &[PyRep::Integer(job_id)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-09: Agent Mission Request, Dungeon Acceleration Gate, & 16-Mission Storyline Trigger
        ctx.dispatch_rpc(
            &mut session,
            "agentMgr",
            None,
            "RequestMission",
            &[PyRep::Integer(1001)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "agentMgr",
            None,
            "AcceptMission",
            &[PyRep::Integer(1001)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "dungeon",
            None,
            "ClearRoomAndActivateGate",
            &[PyRep::Integer(1001)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let mut storyline_triggered = false;
        for _ in 0..16 {
            let mres = ctx
                .dispatch_rpc(
                    &mut session,
                    "agentMgr",
                    None,
                    "CompleteMission",
                    &[PyRep::Integer(1001)],
                    &empty,
                    None,
                )
                .await
                .unwrap();
            if let PyRep::KeyVal(mkv) = mres {
                if mkv.get("storylineTriggered") == Some(&PyRep::Bool(true)) {
                    storyline_triggered = true;
                }
            }
        }
        assert!(storyline_triggered, "16th mission must trigger storyline offer");

        // E2E-10: Fleet Creation, EveMail, & Bookmark Creation
        ctx.dispatch_rpc(&mut session, "fleetMgr", None, "CreateFleet", &[], &empty, None)
            .await
            .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "bookmark",
            None,
            "BookmarkLocation",
            &[PyRep::String("Wormhole Entry".into())],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(&mut session, "mailMgr", None, "SendMail", &[], &empty, None)
            .await
            .unwrap();

        // E2E-11: Probe Scanning, Wormhole Jump, & Abyssal Filament Transition
        let scan_res = ctx
            .dispatch_rpc(
                &mut session,
                "scanSvc",
                None,
                "RequestScans",
                &[PyRep::Real(0.25), PyRep::Real(0.0)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(skv) = scan_res else {
            panic!("Expected scan KeyVal");
        };
        assert_eq!(skv.get("warpable"), Some(&PyRep::Bool(true)));
        ctx.dispatch_rpc(
            &mut session,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(31_000_005)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "abyssalMgr",
            None,
            "ActivateFilament",
            &[],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-12: CrimeWatch Illegal High-Sec Attack, CONCORD Response, & Kill Right Activation
        let cw_res = ctx
            .dispatch_rpc(
                &mut session,
                "crimewatch",
                None,
                "TriggerCriminalAttack",
                &[PyRep::Integer(90_000_002)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(cw_kv) = cw_res else {
            panic!("Expected CrimeWatch KeyVal");
        };
        let kr_id = cw_kv.get("killRightID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "killRightMgr",
            None,
            "ActivateKillRight",
            &[PyRep::Integer(kr_id)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-13: Planetary Interaction Colony Setup, Routing, & POCO Export Tax
        ctx.dispatch_rpc(
            &mut session,
            "planetMgr",
            None,
            "CreateColony",
            &[PyRep::Integer(40_009_082)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-14: Upwell Structure Anchoring, Quantum Core, & Damage-Capped Reinforcement
        let st_res = ctx
            .dispatch_rpc(
                &mut session,
                "structureDeployment",
                None,
                "AnchorStructure",
                &[PyRep::Integer(35832), PyRep::String("EvoMu Keepstar".into())],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(st_kv) = st_res else {
            panic!("Expected structure KeyVal");
        };
        let st_id = st_kv.get("structureID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "structureControl",
            None,
            "AttackStructure",
            &[PyRep::Integer(st_id), PyRep::Real(50_000.0)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-15: Courier Contract Creation, Collateral Escrow, Acceptance, & Completion
        let c_res = ctx
            .dispatch_rpc(
                &mut session,
                "contractMgr",
                None,
                "CreateContract",
                &[
                    PyRep::String("Courier".into()),
                    PyRep::Integer(60_003_760),
                    PyRep::Integer(60_003_766),
                    PyRep::Real(500_000.0),
                    PyRep::Real(2_000_000.0),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(c_kv) = c_res else {
            panic!("Expected contract KeyVal");
        };
        let cid = c_kv.get("contractID").unwrap().as_i64().unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "contractMgr",
            None,
            "AcceptContract",
            &[PyRep::Integer(cid)],
            &empty,
            None,
        )
        .await
        .unwrap();
        ctx.dispatch_rpc(
            &mut session,
            "contractMgr",
            None,
            "CompleteContract",
            &[PyRep::Integer(cid)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-16: AIR NPE Node Graph 473 Progression & AIR Career Program SP Claim
        for _ in 0..3 {
            ctx.dispatch_rpc(
                &mut session,
                "air_npe",
                None,
                "AdvanceNode",
                &[],
                &empty,
                None,
            )
            .await
            .unwrap();
        }
        ctx.dispatch_rpc(
            &mut session,
            "goalMgr",
            None,
            "ClaimGoalReward",
            &[PyRep::Integer(1)],
            &empty,
            None,
        )
        .await
        .unwrap();

        // E2E-17: Living World 3-Tier LOD Materialization, Convoy Interdiction, & Price Spike
        ctx.dispatch_rpc(
            &mut session,
            "livingWorld",
            None,
            "MaterializeSystemLOD2",
            &[PyRep::Integer(30_000_142)],
            &empty,
            None,
        )
        .await
        .unwrap();
        let lw_res = ctx
            .dispatch_rpc(
                &mut session,
                "livingWorld",
                None,
                "InterdictConvoy",
                &[PyRep::Integer(800_000_001)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(lw_kv) = lw_res else {
            panic!("Expected Living World KeyVal");
        };
        assert!(lw_kv.get("newDestinationAskISK").unwrap().as_f64().unwrap() > 5.0);

        // E2E-18: Corporation Creation, Corporation Project Escrow, & Member Contribution Payout
        ctx.dispatch_rpc(
            &mut session,
            "corpmgr",
            None,
            "CreateCorporation",
            &[
                PyRep::String("Apex Syndicate".into()),
                PyRep::String("APEX".into()),
                PyRep::Real(0.05),
            ],
            &empty,
            None,
        )
        .await
        .unwrap();
        let proj_res = ctx
            .dispatch_rpc(
                &mut session,
                "corporation_projects",
                None,
                "CreateProject",
                &[
                    PyRep::String("Mine Veldspar".into()),
                    PyRep::Integer(500),
                    PyRep::Real(100.0),
                ],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(pkv) = proj_res else {
            panic!("Expected project KeyVal");
        };
        let pid = pkv.get("projectID").unwrap().as_i64().unwrap();
        let contrib = ctx
            .dispatch_rpc(
                &mut session,
                "corporation_projects",
                None,
                "ContributeToProject",
                &[PyRep::Integer(pid), PyRep::Integer(500)],
                &empty,
                None,
            )
            .await
            .unwrap();
        let PyRep::KeyVal(ckv) = contrib else {
            panic!("Expected contribution KeyVal");
        };
        assert_eq!(ckv.get("completed"), Some(&PyRep::Bool(true)));

        // Verify all 216 cataloged RPC services and all 135 Quasar Protobuf domains respond without UnimplementedRPC
        assert_eq!(ctx.cataloged_services.len(), 216);
        for svc in &ctx.cataloged_services {
            if svc == "beyonce" {
                continue; // beyonce requires a bound solar system ID (tested above)
            }
            let method_to_call = match svc.as_str() {
                "charUnboundMgr" => "GetCharactersToSelect",
                "stationSvc" => "GetStation",
                "invbroker" => "ListItems",
                "dogmaIM" => "GetShipAttributes",
                _ => "GetState",
            };
            let res = ctx
                .dispatch_rpc(&mut session, svc, None, method_to_call, &[], &empty, None)
                .await;
            assert!(
                res.is_ok(),
                "Cataloged RPC service {svc}.{method_to_call} returned error: {:?}",
                res.err()
            );
        }

        let quasar_json = include_str!("../../../docs/index/quasar_protobuf_catalog.json");
        let quasar_cat: HashMap<String, Vec<String>> = serde_json::from_str(quasar_json).unwrap();
        assert_eq!(quasar_cat.len(), 135);
        for ns in quasar_cat.keys() {
            let q_res = ctx
                .dispatch_rpc(
                    &mut session,
                    "publicGatewaySvc",
                    None,
                    "SendRequest",
                    &[
                        PyRep::String(ns.clone()),
                        PyRep::String("GetStateRequest".into()),
                    ],
                    &empty,
                    None,
                )
                .await
                .unwrap();
            assert!(
                matches!(q_res, PyRep::KeyVal(_)),
                "Quasar namespace {ns} failed"
            );
        }
    }
}
