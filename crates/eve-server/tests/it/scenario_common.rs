//! Shared [`ScenarioWorldBackend`] implementation backed by live [`ServerContext`] wire sessions (`#1540`).

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;

use eve_db::{InventoryFlag, InventoryItemRecord, MarketOrderRecord, WorldRepository};
use eve_proto::{IskCents, PyRep};
use eve_sde::{SdeRegistry, TypeId};
use eve_server::router::client_wire::ClientWireSession;
use eve_server::router::ServerContext;
use eve_testkit::legal_fixture::FixtureSessionView;
use eve_testkit::scenario::ScenarioWorldBackend;

use crate::common::{self, ServerWireTransport};

/// Drives [`eve_testkit::scenario::ScenarioBuilder`] against an in-process [`ServerContext`] over `ClientWireSession`.
pub struct ServerScenarioBackend<'c> {
    pub ctx: &'c ServerContext,
    next_sid: u64,
}

impl<'c> ServerScenarioBackend<'c> {
    pub fn new(ctx: &'c ServerContext) -> Self {
        Self { ctx, next_sid: 950_000 }
    }

    pub fn from_test_ctx(test_ctx: &'c eve_server::router::TestContext) -> Self {
        Self::new(test_ctx.server())
    }

    /// Creates a backend with RPC rate-budget enforcement disabled via [`TestContext::disable_rpc_rate_budget`]
    /// for multi-actor sweeps exceeding per-session burst limits.
    pub fn with_rpc_rate_budget_disabled(test_ctx: &'c eve_server::router::TestContext) -> Self {
        test_ctx.disable_rpc_rate_budget();
        Self::from_test_ctx(test_ctx)
    }
}

async fn login_gm_treasury<'c>(ctx: &'c ServerContext, gm_sid: u64) -> ClientWireSession<'c> {
    common::ensure_quasar_tap_hook_installed();
    ctx.db.provision_gm_account("isd_treasury", "isd_treasury-secret", 90_000_099, "ISD Treasury");
    let mut gm = ClientWireSession::new(ctx, gm_sid);
    let empty = HashMap::new();
    gm.call_wire("authentication", None, "Login", &[PyRep::String("isd_treasury".into()), PyRep::String("isd_treasury-secret".into())], &empty)
        .await.rpc_result.expect("GM treasury Login");
    gm.call_wire("charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_099)], &empty)
        .await.rpc_result.expect("GM treasury SelectCharacterID");
    gm
}

impl<'c> ScenarioWorldBackend for ServerScenarioBackend<'c> {
    type Transport = ServerWireTransport<'c>;

    fn sde(&self) -> &SdeRegistry { &self.ctx.sde }
    fn db(&self) -> &WorldRepository { &self.ctx.db }

    fn provision_player_account(&self, username: &str, password: &str) -> u64 {
        self.ctx.db.provision_player_account(username, password)
    }

    fn next_session_id(&mut self) -> u64 {
        self.next_sid += 1;
        self.next_sid
    }

    fn create_actor_transport(&mut self, session_id: u64) -> Self::Transport {
        common::ensure_quasar_tap_hook_installed();
        ServerWireTransport { inner: ClientWireSession::new(self.ctx, session_id) }
    }

    fn actor_session_view(&self, transport: &Self::Transport) -> FixtureSessionView {
        let s = &transport.inner.session;
        FixtureSessionView {
            session_id: s.session_id,
            user_id: s.user_id,
            character_id: s.character_id,
            corporation_id: s.corporation_id,
            alliance_id: s.alliance_id,
            station_id: s.station_id,
            structure_id: s.structure_id,
            solar_system_id: s.solar_system_id,
            ship_id: s.ship_id,
            corp_role: s.corp_role,
            fleet_id: s.fleet_id,
            role: s.role,
            now_unix: Some(self.ctx.now_unix()),
        }
    }

    fn sync_actor_session(&mut self, transport: &mut Self::Transport) {
        self.ctx.sync_session(&mut transport.inner.session);
    }

    fn fund_character_isk<'a>(&'a mut self, char_id: i64, amount_isk: f64) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let ctx = self.ctx;
        let gm_sid = self.next_session_id();
        Box::pin(async move {
            let mut gm = login_gm_treasury(ctx, gm_sid).await;
            let empty = HashMap::new();
            gm.call_wire("slash", None, "SlashCmd", &[PyRep::String("/giveisk 900000000000".into())], &empty)
                .await.rpc_result.expect("GM treasury /giveisk");
            gm.call_wire("account", None, "GiveCash", &[PyRep::Integer(char_id), PyRep::Real(amount_isk), PyRep::String("scenario-grant".into())], &empty)
                .await.rpc_result.expect("GM treasury GiveCash");
        })
    }

    fn upgrade_actor_to_omega<'a>(&'a mut self, transport: &'a mut Self::Transport) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        let ctx = self.ctx;
        let gm_sid = self.next_session_id();
        let uid = transport.inner.session.user_id.expect("authenticated session for omega upgrade");
        Box::pin(async move {
            let mut gm = login_gm_treasury(ctx, gm_sid).await;
            let until = ctx.now_unix().saturating_add(36_500 * 86_400);
            gm.call_wire("slash", None, "SlashCmd", &[PyRep::String(format!("/setomega {uid} {until}"))], &HashMap::new())
                .await.rpc_result.expect("GM /setomega");
            ctx.sync_session(&mut transport.inner.session);
        })
    }

    fn inject_unavailable_skillbook_id(&mut self, char_id: i64, station_id: i64, skill_id: TypeId) -> i64 {
        self.ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let item_id = snap.next_entity_id;
            snap.items.insert(item_id, InventoryItemRecord {
                item_id, type_id: skill_id, owner_id: char_id, location_id: station_id,
                flag: InventoryFlag::Hangar, quantity: 1, singleton: false, ..Default::default()
            });
            item_id
        })
    }

    fn ensure_station_market_liquidity(&mut self, station_id: i64, requirements: &[(TypeId, i64)]) {
        let mut needed: BTreeMap<TypeId, i64> = BTreeMap::new();
        for &(tid, qty) in requirements {
            *needed.entry(tid).or_insert(0) += qty.max(1);
        }
        let station_meta = self.ctx.sde.solar_systems.values()
            .find_map(|sys| sys.stations.iter().find(|st| st.station_id == station_id))
            .map(|st| (st.region_id, st.owner_corporation_id))
            .unwrap_or((10_000_002, 1_000_125));
        self.ctx.db.with_mut_snapshot(|snap| {
            for (tid, req_qty) in needed {
                let avail: i64 = snap.market_orders.values()
                    .filter(|o| !o.is_bid && o.station_id == station_id && o.type_id == tid && o.vol_remaining > 0)
                    .map(|o| i64::from(o.vol_remaining))
                    .sum();
                if avail >= req_qty {
                    continue;
                }
                let price_isk = self.ctx.sde.types.get(&tid)
                    .map(|t| t.base_price)
                    .filter(|&p| p.is_finite() && p >= 10.0)
                    .unwrap_or(10_000.0)
                    .ceil() as i64;
                let vol = (req_qty - avail).clamp(1_000, 100_000) as i32;
                snap.next_entity_id += 1;
                let order_id = snap.next_entity_id;
                snap.market_orders.insert(order_id, MarketOrderRecord {
                    order_id, owner_id: station_meta.1, station_id, region_id: station_meta.0,
                    type_id: tid, is_bid: false, price: IskCents::from_isk(price_isk),
                    vol_entered: vol, vol_remaining: vol, min_volume: 1, is_npc: true, escrow: IskCents(0),
                });
            }
        });
    }

    fn ship_in_space_ballpark<'a>(&'a mut self, solar_system_id: u32, ship_id: i64) -> Pin<Box<dyn Future<Output = Option<bool>> + Send + 'a>> {
        let ctx = self.ctx;
        Box::pin(async move {
            let Some(handle) = ctx.systems.get_handle(solar_system_id) else {
                return Some(false);
            };
            Some(handle.get_ball_snapshot(ship_id).await.ok().flatten().is_some())
        })
    }

    fn advance_secs(&self, secs: u64) {
        self.ctx.advance_secs(secs);
    }
}
