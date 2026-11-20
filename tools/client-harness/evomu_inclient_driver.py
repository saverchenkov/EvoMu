# -*- coding: utf-8 -*-
"""EvoMu In-Client Journey Driver (Python 2.7 Stackless inside real `exefile`).

Loaded via `/tools=evomu` (`<staged_build>/tools/startup/evomu.py`) inside the
installed EVE Online client. Executes user journeys in `client_journeys.json` over
the real client's MachoNet TCP connection, Quasar Public Gateway HTTP/2 gRPC
channel (`EVE_PUBLIC_GATEWAY_ADDRESS`), ESI/SSO HTTP server, and client
services while lazily coordinating with Client 2 over a local IPC socket to
`run_journeys` for multi-client journeys.
"""

try:
    import __builtin__
except ImportError:
    import builtins as __builtin__
import json
import os
import sys
import time
import traceback
try:
    import urllib2 as _urllib
except ImportError:
    import urllib.request as _urllib

from evomu_inclient_helpers import (
    _INT_TYPES,
    IpcClient,
    _advance_clock,
    _assert_ui_rendered,
    _atomic_write_json,
    _capture_inclient_screenshot,
    _cleanup_corp_structures,
    _dock_station,
    _ensure_docked_jita,
    _ensure_home_ship,
    _ensure_view,
    _extract_id,
    _get_beyonce,
    _get_field,
    _install_rpc_recorder,
    _leave_active_fleet,
    _live_session_attr,
    _reset_baseline_state,
    _resign_player_corp,
    _sleep_ms,
    _sleep_sim_ms,
    _snapshot_ship_fitted_item_ids,
    _sync_quasar_gateway,
    _trace,
    _try_slash,
    _undock_ship,
    _wait_for_session,
    _wait_session_attr,
    _wait_view_settled,
    _wait_warp_fx_settled,
    _warp_animated,
    _warp_full,
)

sys.dont_write_bytecode = True

_STARTED = False

_C2_JOURNEYS = set(
    [
        "local_chat",
        "multiplayer_presence",
        "market_trade",
        "contracts",
        "fleet",
        "mail_notifications",
    ]
)


def _emit_client_marker(cfg, tag, jid, extra=""):
    line = "[%s] id=%s%s" % (tag, jid, (" " + extra) if extra else "")
    _trace(cfg, line)
    try:
        sys.stdout.write(line + "\n")
        sys.stdout.flush()
    except Exception:
        pass
    try:
        import logmodule
        general = getattr(logmodule, "general", None)
        log_fn = getattr(general, "Log", None) if general is not None else None
        if callable(log_fn):
            lvl = getattr(logmodule, "LGNOTICE", getattr(logmodule, "LGINFO", 1))
            try:
                log_fn(line, lvl)
            except TypeError:
                log_fn(line)
    except Exception:
        pass


def _find_hangar_item(sm, station_id, type_id, exclude_id=0, min_qty=1, reverse=True):
    _, items = sm.RemoteSvc("invbroker").MachoBindObject(station_id, ("List", (), {}))
    seq = list(reversed(list(items or []))) if reverse else list(items or [])
    for it in seq:
        if int(_get_field(it, "typeID", 0)) != int(type_id):
            continue
        iid = int(_get_field(it, "itemID", 0))
        qty = int(_get_field(it, "quantity", 1) or 1)
        if exclude_id and iid == int(exclude_id):
            continue
        if qty >= int(min_qty):
            return iid, it
    return 0, None


def _require_ipc_ok(ipc, payload, context="c2"):
    resp = ipc.request(payload)
    if not isinstance(resp, dict) or resp.get("ok") is not True:
        err = resp.get("error") if isinstance(resp, dict) else repr(resp)
        raise RuntimeError(
            "%s (%s) failed: %s" % (payload.get("cmd", "ipc"), context, err or resp)
        )
    return resp


def _board_station_ship(sm, ship_item_id, assemble=True):
    if assemble:
        sm.RemoteSvc("ship").AssembleShip([int(ship_item_id)])
    sm.RemoteSvc("timeControl").ClearSessionTimer()
    sm.RemoteSvc("ship").Board(int(ship_item_id))
    _wait_session_attr("shipid", int(ship_item_id), timeout_sec=3.0, raise_on_timeout=True)
    _sleep_ms(200)
    _wait_view_settled(sm)
    sm.RemoteSvc("timeControl").ClearSessionTimer()


def _journey_launch_login(ctx):
    sm = ctx["sm"]
    char_id = ctx["char_id"]
    ship_id = int(_live_session_attr("shipid", ctx["primary_ship_id"]) or ctx["primary_ship_id"])
    chars_list = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
    srv_time = sm.RemoteSvc("machoNet").GetTime()
    pgw = _sync_quasar_gateway(sm, char_id) or sm.GetService("publicGatewaySvc")
    ctx["pgw"] = pgw
    pgw_avail = bool(pgw.is_available()) if pgw is not None and hasattr(pgw, "is_available") else False
    passed = bool(
        char_id
        and chars_list
        and srv_time
        and any(int(_get_field(c, "characterID", 0)) == int(char_id) for c in chars_list)
    )
    return passed, {
        "user": ctx["user"],
        "char_id": char_id,
        "ship_id": ship_id,
        "characters_loaded": len(chars_list or []),
        "server_time": srv_time,
        "public_gateway_available": pgw_avail,
    }


def _journey_create_character(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    char_id = ctx["char_id"]
    created_ids = []
    active_extra_ids = []
    suffix = str(int(time.time()) % 100000)
    try:
        for bld, anc, sch, label in [
            (2, 7, 17, "Caldari"),
            (5, 1, 11, "Amarr"),
            (7, 13, 20, "Gallente"),
            (3, 19, 14, "Minmatar"),
        ]:
            cur_chars = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
            if len(cur_chars) >= 3 and active_extra_ids:
                old_cid = active_extra_ids.pop(0)
                sm.RemoteSvc("charUnboundMgr").PrepareCharacterForDelete(old_cid)
                _advance_clock(sm, ipc, 36001)
                sm.RemoteSvc("charUnboundMgr").DeleteCharacter(old_cid)
            cname = "Evo%s%s" % (label[:3], suffix)
            if sm.RemoteSvc("charUnboundMgr").ValidateNameEx(cname):
                cid = sm.RemoteSvc("charUnboundMgr").CreateCharacterWithDoll(
                    cname, bld, 0, anc, {"hair": "short"}, {"poseID": 1}, sch
                )
                created_ids.append(int(cid))
                active_extra_ids.append(int(cid))
        all_chars = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
    finally:
        if int(_live_session_attr("charid", 0) or 0) != int(char_id):
            sm.GetService("sessionMgr").PerformSessionChange(
                "charsel", sm.RemoteSvc("charUnboundMgr").SelectCharacterID, char_id
            )
            _ensure_view(sm, "hangar")
        _sync_quasar_gateway(sm, char_id)
    passed = len(created_ids) == 4 and len(all_chars) >= 2
    return passed, {
        "created_char_ids": created_ids,
        "total_chars": len(all_chars or []),
        "active_char_id": char_id,
    }


def _journey_station_ui(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    char_id = ctx["char_id"]
    ship_id = int(_live_session_attr("shipid", ctx["primary_ship_id"]) or ctx["primary_ship_id"])
    _ensure_docked_jita(sm, ipc)
    _assert_ui_rendered(sm, "hangar")
    _, hangar_items = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
    _, ship_dogma = sm.RemoteSvc("dogmaIM").MachoBindObject(ship_id, ("GetShipAttributes", (), {}))
    mkt_groups = sm.RemoteSvc("marketProxy").GetMarketGroups()
    facs = sm.RemoteSvc("facilityManager").GetFacilities()
    sm.RemoteSvc("charMgr").GetPublicInfo3(char_id)
    skills_map = sm.RemoteSvc("skillMgr2").GetSkills()
    wallet_bal = sm.RemoteSvc("account").GetCashBalance(0)
    sm.RemoteSvc("agentMgr").GetMyJournalDetails()
    corp_id_cur = int(_live_session_attr("corpid", ctx["baseline_corp_id"]) or ctx["baseline_corp_id"])
    corp_info = sm.RemoteSvc("corpRegistry").GetCorporation(corp_id_cur)
    sm.RemoteSvc("mailMgr").GetLabels()
    sm.RemoteSvc("notificationMgr").GetUnprocessed()
    sm.RemoteSvc("calendarMgr").GetEventList(1, 2030)
    nes_offers = sm.RemoteSvc("storeManager").get_offers()
    passed = hangar_items is not None and mkt_groups is not None and corp_info is not None
    return passed, {
        "hangar_items": len(hangar_items or []),
        "ship_dogma_attrs": len(ship_dogma or {}),
        "market_groups": len(mkt_groups or []),
        "facilities": len(facs or []),
        "skills": len(skills_map or {}),
        "wallet_balance": wallet_bal,
        "corp_name": _get_field(corp_info, "corporationName", corp_id_cur),
        "nes_offers": len(nes_offers or []),
    }


def _journey_fit_ship(ctx):
    sm = ctx["sm"]
    char_id = ctx["char_id"]
    ship_id = int(_live_session_attr("shipid", ctx["primary_ship_id"]) or ctx["primary_ship_id"])
    _try_slash(sm, "/giveisk 50000000")
    sm.RemoteSvc("marketProxy").PlaceBuyOrder(60003760, 484, 100000.0, 2, -1, 1, 90, False)
    sm.RemoteSvc("marketProxy").PlaceBuyOrder(60003760, 178, 1000.0, 40, -1, 1, 90, False)
    ammo_id, _ = _find_hangar_item(sm, 60003760, 178, reverse=False)
    sm.RemoteSvc("charFittingMgr").SaveFitting(
        char_id,
        {"name": "Rifter Fit", "shipTypeID": 587, "fitData": [(484, 27, 1)]},
    )
    saved_fits = sm.RemoteSvc("charFittingMgr").GetFittings(char_id)
    _try_slash(sm, "/createitem 587 1")
    hull_id, _ = _find_hangar_item(sm, 60003760, 587, exclude_id=ship_id, reverse=False)
    if hull_id:
        sm.RemoteSvc("ship").AssembleShip([hull_id])
        sm.RemoteSvc("repackagingSvc").RepackageItems([(hull_id, 60003760, 4)])
    passed = len(saved_fits or []) >= 1 and bool(hull_id)
    return passed, {
        "ammo_id": ammo_id or None,
        "saved_fittings": len(saved_fits or []),
        "hull_id": hull_id or None,
    }


def _journey_undock_navigate(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    _undock_ship(sm)
    beyonce_jita = _get_beyonce(sm, 30000142)
    _assert_ui_rendered(sm, "inflight")
    bm_res = sm.RemoteSvc("accessGroupBookmarkMgr").BookmarkStaticLocation(
        "Jita Planet", 40009077, "Jita I", None
    )
    beyonce_jita.CmdFollowBall(40009077, 1500.0)
    beyonce_jita.CmdOrbit(40009077, 5000.0)
    beyonce_jita.CmdAlignTo(40009077)
    beyonce_jita.CmdSetSpeedFraction(1.0)
    beyonce_jita.CmdStop()
    warp_anim = _warp_animated(sm, beyonce_jita, 40009077)
    beyonce_jita.CmdStop()
    route_sys = [
        (30000142, 50001249, 30000144),
        (30000144, 50002184, 30000139),
        (30000139, 50000547, 30000141),
    ]
    jumped_systems = []
    for cur_sys, gate_id, next_sys in route_sys:
        _wait_view_settled(sm)
        b_cur = _get_beyonce(sm, cur_sys)
        _warp_full(sm, b_cur, gate_id)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        sm.RemoteSvc("beyonce").CmdStargateJump(gate_id)
        _wait_session_attr("solarsystemid2", next_sys, timeout_sec=3.0, raise_on_timeout=True)
        _wait_view_settled(sm)
        actual_sys = int(_live_session_attr("solarsystemid2", 0) or 0)
        if actual_sys != int(next_sys):
            raise RuntimeError(
                "Stargate jump %d landed in %r instead of %r" % (gate_id, actual_sys, next_sys)
            )
        jumped_systems.append(actual_sys)
    for cur_sys, gate_id, next_sys, need_warp in [
        (30000141, 50001004, 30000139, False),
        (30000139, 50000549, 30000144, True),
        (30000144, 50002185, 30000142, True),
    ]:
        _wait_view_settled(sm)
        if need_warp:
            b_cur = _get_beyonce(sm, cur_sys)
            _warp_full(sm, b_cur, gate_id)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        sm.RemoteSvc("beyonce").CmdStargateJump(gate_id)
        _wait_session_attr("solarsystemid2", next_sys, timeout_sec=3.0, raise_on_timeout=True)
        _wait_view_settled(sm)
    beyonce_jita2 = _get_beyonce(sm, 30000142)
    _warp_full(sm, beyonce_jita2, 60003760)
    _ensure_docked_jita(sm, ipc)
    _assert_ui_rendered(sm, "hangar")
    passed = len(jumped_systems) == 3 and bm_res is not None and bool(warp_anim)
    return passed, {
        "jumped_systems": jumped_systems,
        "bookmark_created": bm_res is not None,
        "warp_telemetry": warp_anim,
    }


def _journey_local_chat(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    char2_id = ctx["char2_id"]
    sys_chans = sm.RemoteSvc("XmppChatMgr").ResyncSystemChannelAccess(30000142)
    owned_chan = sm.RemoteSvc("XmppChatMgr").CreatePlayerOwnedChannel(
        "SquadChannel", "Tactical Channel"
    )
    cid_chan = str(_get_field(owned_chan, "channelID", 1))
    sm.RemoteSvc("XmppChatMgr").AddCharacterToChannel(cid_chan, char2_id, 1)
    chat_c2 = _require_ipc_ok(
        ipc,
        {"cmd": "test_chat_with_c2", "channel_id": cid_chan, "msg": "Hello from client 1"},
        context="local_chat",
    )
    passed = sys_chans is not None and bool(chat_c2.get("ok", False))
    return passed, {
        "system_channels": len(sys_chans or []),
        "channel_id": cid_chan,
        "char2_id": char2_id,
        "c2_chat_ok": bool(chat_c2.get("ok", False)),
    }


def _journey_multiplayer_presence(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    char_id = ctx["char_id"]
    char2_id = ctx["char2_id"]
    ship_id = int(_live_session_attr("shipid", ctx["primary_ship_id"]) or ctx["primary_ship_id"])
    _undock_ship(sm)
    _require_ipc_ok(ipc, {"cmd": "c2_undock_jita"}, context="multiplayer_presence")
    beyonce_jita, _ = sm.RemoteSvc("beyonce").MachoBindObject(30000142, None)
    beyonce_jita.CmdSetSpeedFraction(0.5)
    c2_vis = _require_ipc_ok(
        ipc,
        {"cmd": "c2_check_ball_and_dock", "target_ball_id": ship_id},
        context="multiplayer_presence",
    )
    beyonce_jita.CmdStop()
    _advance_clock(sm, ipc, 12)
    _dock_station(sm, 60003760)
    _try_slash(sm, "/createitem 34 50")
    trit_id, _ = _find_hangar_item(sm, 60003760, 34, reverse=False)
    c2_trade_res = None
    if trit_id:
        t_res = sm.RemoteSvc("trademgr").InitiateTrade(char2_id, 60003760)
        trade_id = int(_get_field(t_res, "tradeContainerID", 0))
        sm.RemoteSvc("trademgr").AddItem(trade_id, trit_id)
        sm.RemoteSvc("trademgr").OfferMoney(trade_id, 5000.0)
        sm.RemoteSvc("trademgr").MakeOffer(trade_id)
        c2_trade_res = _require_ipc_ok(
            ipc,
            {"cmd": "c2_complete_trade", "char1_id": char_id, "trade_id": trade_id},
            context="multiplayer_presence",
        )
    passed = bool(
        c2_vis.get("saw_ship", False)
        and trit_id
        and c2_trade_res
        and c2_trade_res.get("ok")
    )
    return passed, {
        "ship_id": ship_id,
        "c2_saw_ship": bool(c2_vis.get("saw_ship", False)),
        "c2_destiny_updates": int(c2_vis.get("updates", 0)),
        "trit_item_id": trit_id or None,
        "trade_ok": bool(c2_trade_res and c2_trade_res.get("ok")),
    }


def _journey_skills_training(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    char_id = ctx["char_id"]
    sm.GetService("shipTree")
    for sk_id, lvl in ((3300, 5), (3402, 5), (3411, 5), (3301, 0), (3302, 0)):
        _try_slash(sm, "/giveskill %d %d" % (sk_id, lvl))
    bound_skill, _ = sm.RemoteSvc("skillMgr2").MachoBindObject(char_id, None)
    bound_skill.SaveSkillQueue([(3301, 1), (3302, 1)])
    bound_skill.GetSkillQueue()
    _advance_clock(sm, ipc, 3600)
    _try_slash(sm, "/createitem 40520 1")
    _try_slash(sm, "/createitem 40519 1")
    inj_id, _ = _find_hangar_item(sm, 60003760, 40520, reverse=True)
    if inj_id:
        sm.RemoteSvc("skillMgr2").InjectSkillpoints(inj_id)
    plan_res = sm.RemoteSvc("skillMgr2").GetSkillChangesForISIS()
    sm.RemoteSvc("skillMgr2").RespecCharacter(20, 22, 21, 18, 18)
    _try_slash(sm, "/createitem 9899 1")
    imp_id, _ = _find_hangar_item(sm, 60003760, 9899, reverse=False)
    if imp_id:
        sm.RemoteSvc("dogmaIM").InjectImplant(imp_id)
    passed = plan_res is not None
    return passed, {
        "injector_item_id": inj_id or None,
        "implant_item_id": imp_id or None,
        "isis_plan_ok": plan_res is not None,
    }


def _journey_market_trade(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    market_tid = 19658
    sell_mkt_id = int(_get_field(_try_slash(sm, "/createitem %d 200" % market_tid), "itemID", 0) or 0)
    if not sell_mkt_id:
        sell_mkt_id, _ = _find_hangar_item(sm, 60003760, market_tid, min_qty=50, reverse=True)
    placed_orders = sm.RemoteSvc("marketProxy").PlaceMultiSellOrder(
        [{"stationID": 60003760, "typeID": market_tid, "itemID": sell_mkt_id, "price": 50.0, "quantity": 20}],
        False,
        90,
    )
    sm.RemoteSvc("timeControl").AdvanceClock(305)
    orders = sm.RemoteSvc("marketProxy").GetCharOrders()
    sell_orders = [
        o for o in (orders or [])
        if int(_get_field(o, "typeID", 0)) == market_tid and not bool(_get_field(o, "bid", False))
    ]
    target_orders = sell_orders or list(placed_orders or []) or list(orders or [])
    modified_oid = None
    if target_orders:
        modified_oid = int(_get_field(target_orders[-1], "orderID", 0))
        sm.RemoteSvc("marketProxy").ModifyCharOrder(modified_oid, 55.0, False, True, False)
    _require_ipc_ok(
        ipc,
        {"cmd": "c2_buy_tritanium", "type_id": market_tid, "price": 100.0, "qty": 5},
        context="market_trade",
    )
    txs = sm.RemoteSvc("marketProxy").CharGetTransactions()
    hist = sm.RemoteSvc("marketProxy").GetOldPriceHistory(market_tid)
    has_mkt_tx = any(int(_get_field(tx, "typeID", 0)) == market_tid for tx in (txs or []))
    passed = bool(orders is not None and has_mkt_tx and hist is not None and len(hist) > 0)
    return passed, {
        "type_id": market_tid,
        "modified_order_id": modified_oid,
        "transactions_count": len(txs or []),
        "history_rows": len(hist or []),
    }


def _journey_contracts(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    char_id = ctx["char_id"]
    char2_id = ctx["char2_id"]
    it_ex = int(_get_field(_try_slash(sm, "/createitem 34 20"), "itemID", 0) or 0)
    it_auc = int(_get_field(_try_slash(sm, "/createitem 34 20"), "itemID", 0) or 0)
    it_cour = int(_get_field(_try_slash(sm, "/createitem 34 20"), "itemID", 0) or 0)
    if not (it_ex and it_auc and it_cour):
        fallback_trit, _ = _find_hangar_item(sm, 60003760, 34, min_qty=5, reverse=True)
        it_ex = it_ex or fallback_trit
        it_auc = it_auc or fallback_trit
        it_cour = it_cour or fallback_trit
    c_ex = sm.RemoteSvc("contractProxy").CreateContract(
        1, 0, char2_id, 1440, 1, 60003760, 60003760, 25000.0, 0.0, 0.0, "Item Exchange", "", [(it_ex, 5)]
    )
    c_auc = sm.RemoteSvc("contractProxy").CreateContract(
        2, 0, 0, 1440, 1, 60003760, 60003760, 10000.0, 50000.0, 0.0, "Auction", "", [(it_auc, 5)]
    )
    c_cour = sm.RemoteSvc("contractProxy").CreateContract(
        3, 0, char2_id, 1440, 1, 60003760, 60003760, 0.0, 15000.0, 5000.0, "Courier", "", [(it_cour, 5)]
    )
    c_ex_id = int(_get_field(c_ex, "contractID", c_ex))
    c_auc_id = int(_get_field(c_auc, "contractID", c_auc))
    c_cour_id = int(_get_field(c_cour, "contractID", c_cour))
    _require_ipc_ok(
        ipc,
        {
            "cmd": "c2_process_contracts",
            "c_ex": c_ex_id,
            "c_auc": c_auc_id,
            "c_cour": c_cour_id,
            "char1_id": char_id,
        },
        context="contracts",
    )
    passed = bool(c_ex_id and c_auc_id and c_cour_id)
    return passed, {
        "exchange_contract_id": c_ex_id,
        "auction_contract_id": c_auc_id,
        "courier_contract_id": c_cour_id,
    }


def _journey_mining_loop(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    primary_ship_id = ctx["primary_ship_id"]
    for sk in (3327, 3328, 3329, 3330, 3331, 3386, 32918):
        _try_slash(sm, "/giveskill %d 5" % sk)
    _try_slash(sm, "/createitem 32880 1")
    _try_slash(sm, "/createitem 483 1")
    venture_id, _ = _find_hangar_item(sm, 60003760, 32880, reverse=True)
    miner_id, _ = _find_hangar_item(sm, 60003760, 483, reverse=True)
    rock_id = 0
    mine_rep = None
    ore_id = None
    reproc_quotes = None
    reproc_res = None
    body_failed = False
    try:
        if venture_id:
            _board_station_ship(sm, venture_id, assemble=True)
            if miner_id:
                inv_v, _ = sm.RemoteSvc("invbroker").MachoBindObject(venture_id, None)
                inv_v.Add(miner_id, 60003760, flag=27)
                dogma_v, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(venture_id, None)
                dogma_v.SetModuleOnline(venture_id, miner_id)
        _undock_ship(sm)
        active_mine_ship = venture_id or primary_ship_id
        bp_mine = getattr(sm.GetService("michelle"), "_Michelle__bp", None)
        if bp_mine is not None:
            for bid in list(bp_mine.balls.keys()):
                slim_m = bp_mine.GetInvItem(bid)
                if int(bid) != active_mine_ship and slim_m is not None and int(getattr(slim_m, "categoryID", 0) or 0) == 25:
                    rock_id = int(bid)
                    break
        if not rock_id:
            b_jita_mine = _get_beyonce(sm, 30000142)
            _warp_full(sm, b_jita_mine, 50001249)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("beyonce").CmdStargateJump(50001249)
            _wait_session_attr("solarsystemid2", 30000144, timeout_sec=3.0, raise_on_timeout=True)
            b_perim_mine = _get_beyonce(sm, 30000144)
            _ensure_view(sm, "inflight")
            _wait_view_settled(sm)
            bp_perim = getattr(sm.GetService("michelle"), "_Michelle__bp", None)
            belt_id = 40009232
            if bp_perim is not None:
                for bid in list(bp_perim.balls.keys()):
                    slim_b = bp_perim.GetInvItem(bid)
                    if slim_b is not None and int(getattr(slim_b, "groupID", 0) or 0) == 9:
                        belt_id = int(bid)
                        break
            _warp_full(sm, b_perim_mine, belt_id)
            _wait_view_settled(sm)
            bp_mine = getattr(sm.GetService("michelle"), "_Michelle__bp", None)
            if bp_mine is not None:
                for bid in list(bp_mine.balls.keys()):
                    slim_m = bp_mine.GetInvItem(bid)
                    if int(bid) != active_mine_ship and slim_m is not None and int(getattr(slim_m, "categoryID", 0) or 0) == 25:
                        rock_id = int(bid)
                        break
        if not rock_id:
            raise RuntimeError("No asteroid rock found in Jita or Perimeter belt")
        sm.RemoteSvc("dogmaIM").AddTarget(rock_id)
        if venture_id and miner_id:
            dogma_mine, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(venture_id, None)
            mine_rep = dogma_mine.Activate(miner_id, "mining", rock_id)
        _advance_clock(sm, ipc, 65)
        if venture_id and miner_id and mine_rep is not None:
            dogma_mine, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(venture_id, None)
            dogma_mine.Deactivate(miner_id)
        if int(_live_session_attr("solarsystemid2", 30000142) or 30000142) == 30000144:
            b_perim_ret = _get_beyonce(sm, 30000144)
            _warp_full(sm, b_perim_ret, 50002185)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("beyonce").CmdStargateJump(50002185)
            _wait_session_attr("solarsystemid2", 30000142, timeout_sec=3.0, raise_on_timeout=True)
            _ensure_view(sm, "inflight")
            _wait_view_settled(sm)
            b_jita_ret = _get_beyonce(sm, 30000142)
            _warp_full(sm, b_jita_ret, 60003760)
        _dock_station(sm, 60003760)
        if venture_id:
            inv_hangar, _ = sm.RemoteSvc("invbroker").MachoBindObject(60003760, None)
            _, ship_ore_items = sm.RemoteSvc("invbroker").MachoBindObject(venture_id, ("List", (), {}))
            for it in (ship_ore_items or []):
                if int(_get_field(it, "typeID", 0)) in (1228, 1230):
                    inv_hangar.Add(int(_get_field(it, "itemID", 0)), venture_id)
        _, inv_ore = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        for it in (inv_ore or []):
            if int(_get_field(it, "typeID", 0)) in (1228, 1230) and int(_get_field(it, "quantity", 0)) >= 100:
                ore_id = int(_get_field(it, "itemID", 0))
                break
        if not ore_id:
            _try_slash(sm, "/createitem 1228 200")
            ore_id, _ = _find_hangar_item(sm, 60003760, 1228, min_qty=100, reverse=False)
        if not ore_id:
            raise RuntimeError("No ore available for reprocessing")
        reproc_svc, _ = sm.RemoteSvc("reprocessingSvc").MachoBindObject(60003760, None)
        reproc_quotes = reproc_svc.GetQuotes([ore_id], 60003760)
        reproc_res = reproc_svc.Reprocess([ore_id], 60003760)
    except Exception:
        body_failed = True
        raise
    finally:
        cleanup_errors = []
        try:
            if int(_live_session_attr("solarsystemid2", 30000142) or 30000142) == 30000144:
                b_perim_ret = _get_beyonce(sm, 30000144)
                _warp_full(sm, b_perim_ret, 50002185)
                sm.RemoteSvc("timeControl").ClearSessionTimer()
                sm.RemoteSvc("beyonce").CmdStargateJump(50002185)
                _wait_session_attr("solarsystemid2", 30000142, timeout_sec=3.0, raise_on_timeout=True)
                _ensure_view(sm, "inflight")
                _wait_view_settled(sm)
            if int(_live_session_attr("stationid", 0) or 0) != 60003760:
                if not int(_live_session_attr("stationid", 0) or 0):
                    b_jita_ret = _get_beyonce(sm, 30000142)
                    _warp_full(sm, b_jita_ret, 60003760)
                    _dock_station(sm, 60003760)
        except Exception as exc:
            cleanup_errors.append("return_to_jita: %s" % exc)
        try:
            _ensure_docked_jita(sm, ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_docked_jita: %s" % exc)
        try:
            _ensure_home_ship(sm, primary_ship_id, ipc=ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_home_ship: %s" % exc)
        if not body_failed and cleanup_errors:
            raise RuntimeError("mining_loop cleanup failed: %s" % "; ".join(cleanup_errors))
    passed = bool(
        venture_id
        and miner_id
        and rock_id
        and mine_rep is not None
        and ore_id
        and reproc_quotes is not None
        and reproc_res is not None
    )
    return passed, {
        "venture_id": venture_id or None,
        "miner_id": miner_id or None,
        "rock_id": rock_id or None,
        "ore_id": ore_id or None,
        "reprocess_ok": reproc_res is not None,
    }


def _journey_pve_combat(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    ship_id = int(_live_session_attr("shipid", ctx["primary_ship_id"]) or ctx["primary_ship_id"])
    _undock_ship(sm)
    rat_id = 0
    body_failed = False
    try:
        rat_rep = _try_slash(sm, "/spawnnpc 10035")
        rat_id = int(
            _get_field(rat_rep, "npcID", _get_field(rat_rep, "ballID", _get_field(rat_rep, "itemID", 0))) or 0
        )
        if rat_id <= 0:
            raise RuntimeError("Failed to spawn PvE NPC rat (rep=%r)" % (rat_rep,))
        sm.RemoteSvc("dogmaIM").AddTarget(rat_id)
        _try_slash(sm, "/despawn %d" % rat_id)
        _advance_clock(sm, ipc, 2)
    except Exception:
        body_failed = True
        if rat_id > 0:
            _try_slash(sm, "/despawn %d" % rat_id)
        raise
    finally:
        cleanup_errors = []
        try:
            _dock_station(sm, 60003760)
        except Exception as exc:
            cleanup_errors.append("dock_station: %s" % exc)
        if not body_failed and cleanup_errors:
            raise RuntimeError("pve_combat cleanup failed: %s" % "; ".join(cleanup_errors))
    passed = rat_id > 0
    return passed, {
        "ship_id": ship_id,
        "npc_id": rat_id,
    }


def _journey_missions(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    agents_rows = sm.RemoteSvc("agentMgr").GetAgents()
    sec_ag_id = None
    sec_ag_station_id = 0
    sec_ag_sys_id = 0
    for r in (agents_rows or []):
        if int(_get_field(r, "level", 0)) == 1 and int(_get_field(r, "solarSystemID", 0)) == 30000142:
            sec_ag_id = int(_get_field(r, "agentID", 0))
            sec_ag_station_id = int(_get_field(r, "stationID", 0) or (60015169 if sec_ag_id == 3020034 else 0))
            sec_ag_sys_id = int(_get_field(r, "solarSystemID", 0) or 0)
            break
    if not sec_ag_id and agents_rows:
        sec_ag_id = int(_get_field(agents_rows[0], "agentID", 0))
        sec_ag_station_id = int(_get_field(agents_rows[0], "stationID", 0) or (60015169 if sec_ag_id == 3020034 else 0))
        sec_ag_sys_id = int(_get_field(agents_rows[0], "solarSystemID", 0) or 0)
    ag_action = None
    lp_offers = None
    moved_ag_station = False
    body_failed = False
    try:
        if sec_ag_station_id and sec_ag_station_id != 60003760 and sec_ag_sys_id == 30000142:
            _undock_ship(sm)
            moved_ag_station = True
            b_ag = _get_beyonce(sm, 30000142)
            _warp_full(sm, b_ag, sec_ag_station_id)
            _dock_station(sm, sec_ag_station_id)
        if sec_ag_id:
            bound_ag, _ = sm.RemoteSvc("agentMgr").MachoBindObject(sec_ag_id, None)
            ag_action = bound_ag.DoAction(2)
        lp_offers = sm.RemoteSvc("LPStoreMgr").GetAvailableOffersFromCorp(1000035)
    except Exception:
        body_failed = True
        raise
    finally:
        if moved_ag_station:
            cleanup_errors = []
            try:
                if int(_live_session_attr("stationid", 0) or 0) != 60003760:
                    if int(_live_session_attr("stationid", 0) or 0):
                        _undock_ship(sm)
                    b_ag_ret = _get_beyonce(sm, 30000142)
                    _warp_full(sm, b_ag_ret, 60003760)
                    _dock_station(sm, 60003760)
            except Exception as exc:
                cleanup_errors.append("return_to_jita: %s" % exc)
            try:
                _ensure_docked_jita(sm, ipc)
            except Exception as exc:
                cleanup_errors.append("ensure_docked_jita: %s" % exc)
            if not body_failed and cleanup_errors:
                raise RuntimeError("missions cleanup failed: %s" % "; ".join(cleanup_errors))
    passed = bool(agents_rows and len(agents_rows) > 0 and sec_ag_id and ag_action is not None and lp_offers is not None)
    return passed, {
        "agents_count": len(agents_rows or []),
        "agent_id": sec_ag_id,
        "agent_station_id": sec_ag_station_id,
        "lp_offers_count": len(lp_offers or []),
    }


def _journey_exploration(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    ship_id = int(_live_session_attr("shipid", ctx["primary_ship_id"]) or ctx["primary_ship_id"])
    for sk_exp in (3402, 3412, 3413, 3426, 3432, 13278, 21718, 25810, 25811, 25812):
        _try_slash(sm, "/giveskill %d 5" % sk_exp)
    _try_slash(sm, "/createitem 22175 1")
    _try_slash(sm, "/createitem 17938 1")
    _try_slash(sm, "/createitem 30013 8")
    _, inv_exp = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
    da_id, launcher_id, probe_id = 0, 0, 0
    for it in (inv_exp or []):
        t_exp = int(_get_field(it, "typeID", 0))
        i_exp = int(_get_field(it, "itemID", 0))
        if t_exp == 22175 and not da_id:
            da_id = i_exp
        elif t_exp == 17938 and not launcher_id:
            launcher_id = i_exp
        elif t_exp == 30013 and not probe_id:
            probe_id = i_exp
    inv_ship_exp, _ = sm.RemoteSvc("invbroker").MachoBindObject(ship_id, None)
    if da_id:
        inv_ship_exp.Add(da_id, 60003760, flag=19)
    if launcher_id:
        inv_ship_exp.Add(launcher_id, 60003760, flag=27)
    if probe_id:
        inv_ship_exp.Add(probe_id, 60003760, flag=27)
    if launcher_id:
        dogma_dock_exp, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(ship_id, None)
        dogma_dock_exp.SetModuleOnline(ship_id, launcher_id)
    _undock_ship(sm)
    dscan_res = sm.RemoteSvc("scanSvc").ConeScan(360.0, 21474836475000.0, [1.0, 0.0, 0.0])
    dogma_exp, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(ship_id, None)
    launch_rep = dogma_exp.Activate(launcher_id, "default")
    probe_pids = list(_get_field(launch_rep, "probeIDs", []) or [])
    sm.RemoteSvc("scanMgr").RequestScans(
        {int(pid): {"pos": (-32000000.0, 28000000.0, -10000000.0), "scanRangeAU": 0.25} for pid in probe_pids},
        1,
    )
    _advance_clock(sm, ipc, 15)
    scan_res = sm.RemoteSvc("scanMgr").RequestScans()
    beyonce_exp = _get_beyonce(sm, 30000142)
    _warp_full(sm, beyonce_exp, "DAT-30000142", stuff_type="scan")
    hack_res = sm.RemoteSvc("hackingMgr").StartNewGameInstance(1870014200)
    sm.RemoteSvc("hackingMgr").ClickedOnTile(1)
    _advance_clock(sm, ipc, 5)
    _warp_full(sm, beyonce_exp, 60003760)
    _dock_station(sm, 60003760)
    passed = bool(dscan_res is not None and len(probe_pids) > 0 and scan_res is not None and hack_res is not None)
    return passed, {
        "dscan_count": len(dscan_res) if isinstance(dscan_res, (list, tuple)) else 1,
        "probes_launched": len(probe_pids),
        "scan_results": len(scan_res) if isinstance(scan_res, (list, tuple)) else 1,
        "hacking_ok": hack_res is not None,
    }


def _journey_wormholes_filaments(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    cfg = ctx["cfg"]
    char_id = ctx["char_id"]
    ship_id = int(_live_session_attr("shipid", ctx["primary_ship_id"]) or ctx["primary_ship_id"])
    _try_slash(sm, "/createitem 53977 1")
    _try_slash(sm, "/createitem 56131 3")
    _, inv_fil = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
    nj_id, aby_id = 0, 0
    for it in (inv_fil or []):
        tid = int(_get_field(it, "typeID", 0))
        iid = int(_get_field(it, "itemID", 0))
        if tid == 53977 and not nj_id:
            nj_id = iid
        elif tid == 56131 and not aby_id:
            aby_id = iid
    if nj_id or aby_id:
        inv_ship_fil, _ = sm.RemoteSvc("invbroker").MachoBindObject(ship_id, None)
        if nj_id:
            inv_ship_fil.Add(nj_id, 60003760)
        if aby_id:
            inv_ship_fil.Add(aby_id, 60003760)
    dest_sys = 30000142
    aby_ent = None
    fil_res = None
    body_failed = False
    try:
        _undock_ship(sm)
        _trace(cfg, "j15 undocked")
        beyonce_jita = _get_beyonce(sm, 30000142)
        _warp_full(sm, beyonce_jita, 40009082)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        sm.RemoteSvc("timeControl").AdvanceClock(65)
        fid_wh = int(_extract_id(sm.RemoteSvc("fleetMgr").CreateFleet()))
        fl_wh, _ = sm.RemoteSvc("fleetObjectHandler").MachoBindObject(fid_wh, None)
        sm.GetService("fleet").fleet = fl_wh
        _trace(cfg, "j15 calling AbyssalEntranceDeployment")
        aby_ent = sm.RemoteSvc("abyssalMgr").AbyssalEntranceDeployment(aby_id)
        trace_id = int(_get_field(aby_ent, "traceBallID", _extract_id(aby_ent)))
        _wait_view_settled(sm)
        _trace(cfg, "j15 calling AbyssalEntranceGateActivation(%d)" % trace_id)
        enter_rep = sm.RemoteSvc("abyssalMgr").AbyssalEntranceGateActivation(trace_id)
        pocket_id = int(_get_field(enter_rep, "pocketID", _extract_id(enter_rep)))
        target_aby_sys = int(_get_field(enter_rep, "solarSystemID", 32000001) or 32000001)
        room_npcs = list(_get_field(enter_rep, "npcBallIDs", []) or [])
        for npc_bid in room_npcs:
            _try_slash(sm, "/despawn %d" % int(npc_bid))
        room_npcs = []
        _trace(cfg, "j15 waiting abyss session %d" % target_aby_sys)
        _wait_session_attr("solarsystemid2", target_aby_sys, timeout_sec=5.0, raise_on_timeout=True)
        _sync_quasar_gateway(sm, char_id)
        _wait_view_settled(sm)
        _trace(cfg, "j15 calling ClientIsReady")
        sm.RemoteSvc("abyssalMgr").ClientIsReady()
        beyonce_rm = _get_beyonce(sm, target_aby_sys)
        for rm in (1, 2, 3):
            for npc_bid in room_npcs:
                _try_slash(sm, "/despawn %d" % int(npc_bid))
            _try_slash(sm, "/heal")
            gate_id = 1895000000 + pocket_id * 10 + rm
            for _ in range(3):
                beyonce_rm.CmdSetSpeedFraction(1.0)
                beyonce_rm.CmdFollowBall(gate_id, 500.0)
                sm.RemoteSvc("timeControl").AdvanceClock(220)
                try:
                    _trace(cfg, "j15 room %d gate %d" % (rm, gate_id))
                    if rm < 3:
                        gate_rep = sm.RemoteSvc("abyssalMgr").AbyssalGateActivation(gate_id)
                        room_npcs = list(_get_field(gate_rep, "npcBallIDs", []) or [])
                    else:
                        _wait_view_settled(sm)
                        sm.RemoteSvc("abyssalMgr").AbyssalEndGateActivation(gate_id)
                    break
                except Exception as exc:
                    if "GateOutOfRange" not in str(exc):
                        raise
                    _try_slash(sm, "/heal")
        _trace(cfg, "j15 waiting return jita")
        _wait_session_attr("solarsystemid2", 30000142, timeout_sec=5.0, raise_on_timeout=True)
        _sync_quasar_gateway(sm, char_id)
        _wait_view_settled(sm)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        sm.RemoteSvc("timeControl").AdvanceClock(65)
        _trace(cfg, "j15 calling ActivateRandomJumpFilament(%d)" % nj_id)
        fil_res = sm.RemoteSvc("randomJumpMgr").ActivateRandomJumpFilament(nj_id)
        dest_sys = int(_get_field(fil_res, "solarSystemID", _live_session_attr("solarsystemid2", 30000142)) or 30000142)
        _wait_session_attr("solarsystemid2", dest_sys, timeout_sec=5.0, raise_on_timeout=True)
        _wait_view_settled(sm)
    except Exception:
        body_failed = True
        raise
    finally:
        cleanup_errors = []
        try:
            _leave_active_fleet(sm)
        except Exception as exc:
            cleanup_errors.append("leave_fleet: %s" % exc)
        try:
            sm.RemoteSvc("timeControl").ClearSessionTimer()
        except Exception as exc:
            cleanup_errors.append("clear_session_timer: %s" % exc)
        try:
            if int(_live_session_attr("solarsystemid2", 30000142) or 30000142) != 30000142:
                sm.RemoteSvc("slash").SlashCmd("/tr 60003760")
                _wait_session_attr("solarsystemid2", 30000142, timeout_sec=5.0, raise_on_timeout=True)
                _wait_view_settled(sm)
            elif not _live_session_attr("stationid", None):
                beyonce_jita2 = _get_beyonce(sm, 30000142)
                _warp_full(sm, beyonce_jita2, 60003760)
        except Exception as exc:
            cleanup_errors.append("return_to_jita: %s" % exc)
        try:
            _ensure_docked_jita(sm, ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_docked_jita: %s" % exc)
        if not body_failed and cleanup_errors:
            raise RuntimeError("wormholes_filaments cleanup failed: %s" % "; ".join(cleanup_errors))
    passed = bool(fil_res and aby_ent)
    return passed, {
        "needlejack_dest_sys": dest_sys,
        "abyssal_deployed": aby_ent is not None,
    }


def _journey_ship_loss(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    primary_ship_id = ctx["primary_ship_id"]
    _try_slash(sm, "/giveskill 3327 5")
    _try_slash(sm, "/giveskill 3329 5")
    _try_slash(sm, "/createitem 587 1")
    sac_ship, _ = _find_hangar_item(sm, 60003760, 587, exclude_id=primary_ship_id, reverse=True)
    if sac_ship:
        _board_station_ship(sm, sac_ship, assemble=True)
    curr_ship = int(_live_session_attr("shipid", sac_ship or primary_ship_id) or (sac_ship or primary_ship_id))
    ins_res = None
    pod_ship = 0
    kms = []
    reboarded_ship = 0
    try:
        ins_res = sm.RemoteSvc("insuranceSvc").InsureShip(curr_ship, 1.0)
        sm.RemoteSvc("insuranceSvc").GetContracts()
        _undock_ship(sm)
        sm.RemoteSvc("ship").SelfDestruct()
        sm.RemoteSvc("timeControl").AdvanceClock(125)
        _sleep_ms(250)
        _wait_view_settled(sm)
        pod_ship = int(_live_session_attr("shipid", 0) or 0)
        kms = sm.RemoteSvc("charMgr").GetRecentShipKillsAndLosses(10, 0)
        _dock_station(sm, 60003760)
    finally:
        _ensure_docked_jita(sm, ipc)
        if primary_ship_id and curr_ship != primary_ship_id:
            try:
                reboarded_ship = _ensure_home_ship(sm, primary_ship_id, ipc=ipc)
            except Exception:
                reboarded_ship = 0
        if not reboarded_ship or reboarded_ship == pod_ship:
            _try_slash(sm, "/createitem 587 1")
            new_ship, _ = _find_hangar_item(sm, 60003760, 587, exclude_id=pod_ship, reverse=True)
            if new_ship:
                _board_station_ship(sm, new_ship, assemble=True)
                ctx["primary_ship_id"] = new_ship
                ctx["baseline"]["ship_id"] = new_ship
                reboarded_ship = new_ship
    passed = ins_res is not None and pod_ship != curr_ship and len(kms or []) >= 1
    return passed, {
        "lost_ship_id": curr_ship,
        "pod_ship_id": pod_ship,
        "killmails": len(kms or []),
        "restored_ship_id": reboarded_ship,
    }


def _journey_clones(ctx):
    sm = ctx["sm"]
    _try_slash(sm, "/giveskill 24242 2")
    _try_slash(sm, "/giveskill 3411 2")
    jc_res = sm.RemoteSvc("jumpCloneSvc").InstallCloneInStation()
    clone_state = sm.RemoteSvc("jumpCloneSvc").GetCloneState()
    sm.RemoteSvc("home_station").set_home_station(60003760)
    passed = jc_res is not None and clone_state is not None
    return passed, {
        "jump_clone_installed": jc_res is not None,
        "home_station_id": 60003760,
    }


def _journey_industry(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    _try_slash(sm, "/giveskill 3380 5")
    _try_slash(sm, "/giveskill 3388 5")
    _try_slash(sm, "/createitem 691 1")
    _try_slash(sm, "/createitem 34 50000")
    _try_slash(sm, "/createitem 35 20000")
    _try_slash(sm, "/createitem 36 10000")
    _try_slash(sm, "/createitem 37 5000")
    bpo_id, _ = _find_hangar_item(sm, 60003760, 691, reverse=False)
    job_res = None
    jid_ind = None
    if bpo_id:
        job_res = sm.RemoteSvc("industryManager").InstallJob(
            {
                "facilityID": 60003760,
                "activityID": 1,
                "blueprintID": bpo_id,
                "blueprintTypeID": 691,
                "productTypeID": 587,
                "runs": 1,
                "inputLocationID": 60003760,
                "outputLocationID": 60003760,
            }
        )
        dur_ind = int(
            _get_field(job_res, "duration", 0)
            or _get_field(job_res, "durationSeconds", 0)
            or 10800
        )
        adv_ind = max(14400, dur_ind + 600)
        try:
            _advance_clock(sm, ipc, adv_ind)
        except Exception:
            sm.RemoteSvc("timeControl").AdvanceClock(adv_ind)
        jid_ind = _extract_id(job_res)
        sm.RemoteSvc("industryManager").CompleteJob(jid_ind)
    passed = job_res is not None
    return passed, {
        "blueprint_id": bpo_id or None,
        "job_id": jid_ind,
    }


def _journey_planetary(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    primary_ship_id = ctx["primary_ship_id"]
    _try_slash(sm, "/giveskill 2505 4")
    _try_slash(sm, "/giveskill 3342 5")
    _try_slash(sm, "/giveskill 20342 1")
    _try_slash(sm, "/giveskill 20526 1")
    _try_slash(sm, "/createitem 20185 1")
    _try_slash(sm, "/createitem 2524 1")
    hauler_id, _ = _find_hangar_item(sm, 60003760, 20185, reverse=False)
    cc_id, _ = _find_hangar_item(sm, 60003760, 2524, reverse=False)
    pinfo = None
    body_failed = False
    try:
        active_pi_ship = primary_ship_id
        if hauler_id:
            _board_station_ship(sm, hauler_id, assemble=True)
            active_pi_ship = hauler_id
        if cc_id and active_pi_ship:
            inv_ship_pi, _ = sm.RemoteSvc("invbroker").MachoBindObject(active_pi_ship, None)
            inv_ship_pi.Add(cc_id, 60003760)
        _undock_ship(sm)
        sm.RemoteSvc("planetMgr").UserUpdateNetwork(
            40009077,
            [(1, 1, 2524, 0.25, 0.5), (1, 2, 2409, 0.26, 0.52), (3, 1, 2, 0), (5, 1, 2, 1)],
        )
        _advance_clock(sm, ipc, 1800)
        pinfo = sm.RemoteSvc("planetMgr").GetPlanetResourceInfo(40009077)
        _dock_station(sm, 60003760)
    except Exception:
        body_failed = True
        raise
    finally:
        cleanup_errors = []
        try:
            _ensure_docked_jita(sm, ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_docked_jita: %s" % exc)
        try:
            _ensure_home_ship(sm, primary_ship_id, ipc=ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_home_ship: %s" % exc)
        if not body_failed and cleanup_errors:
            raise RuntimeError("planetary cleanup failed: %s" % "; ".join(cleanup_errors))
    passed = pinfo is not None
    return passed, {
        "planet_id": 40009077,
        "hauler_id": hauler_id or None,
        "command_center_id": cc_id or None,
        "resource_info_ok": pinfo is not None,
    }


def _journey_corporation(ctx):
    sm = ctx["sm"]
    baseline_corp_id = ctx["baseline_corp_id"]
    _try_slash(sm, "/giveskill 3363 2")
    _try_slash(sm, "/giveskill 11584 2")
    _try_slash(sm, "/giveisk 1500000000")
    sm.RemoteSvc("timeControl").ClearSessionTimer()
    corp_id = 0
    ali_id = 0
    office_res = None
    corp_info = None
    ali_info = None
    wars_rep = None
    try:
        corp_res = sm.RemoteSvc("corpRegistry").AddCorporation(
            "Alliance Builders", "EVAB", "Player corporation", 10.0, False
        )
        corp_id = int(_extract_id(corp_res, ("corporationID", "corpID", "id")))
        _wait_session_attr("corpid", corp_id, timeout_sec=3.0)
        sm.RemoteSvc("account").GiveCash(corp_id, 1200000000.0, "Fund Alliance Builders")
        office_res = sm.RemoteSvc("officeManager").RentOffice(60003760, 1)
        ali_res = sm.RemoteSvc("corpRegistry").CreateAlliance(
            "Vanguard Alliance", "EVVA", "Vanguard", ""
        )
        ali_id = int(_extract_id(ali_res, ("allianceID", "id")))
        _wait_session_attr("allianceid", ali_id, timeout_sec=3.0, raise_on_timeout=True)
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        corp_info = sm.RemoteSvc("corpRegistry").GetCorporation(corp_id)
        ali_info = sm.RemoteSvc("allianceRegistry").GetAlliance(ali_id)
        wars_rep = sm.RemoteSvc("warsInfoMgr").GetWarsByOwners([corp_id, ali_id])
    finally:
        _resign_player_corp(sm, baseline_corp_id, ipc=ctx.get("ipc"))
    passed = (
        corp_id > 0
        and ali_id > 0
        and corp_info is not None
        and ali_info is not None
        and wars_rep is not None
    )
    return passed, {
        "corp_id": corp_id,
        "office_rented": office_res is not None,
        "alliance_id": ali_id,
    }


def _journey_fleet(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    char2_id = ctx["char2_id"]
    _leave_active_fleet(sm)
    fleet_svc = sm.GetService("fleet")
    fleet_id = 0
    motd = None
    try:
        fleet_id = int(_extract_id(sm.RemoteSvc("fleetMgr").CreateFleet()))
        _wait_session_attr("fleetid", fleet_id, timeout_sec=3.0)
        fl_handler, _ = sm.RemoteSvc("fleetObjectHandler").MachoBindObject(fleet_id, None)
        fleet_svc.fleet = fl_handler
        fl_handler.Init(fleet_svc.GetMyShipTypeID(), None, None)
        fleet_svc.InitFleet()
        sm.RemoteSvc("fleetMgr").Invite(char2_id, None, None, 4)
        _require_ipc_ok(ipc, {"cmd": "c2_accept_fleet", "fleet_id": fleet_id}, context="fleet")
        fleet_svc.InitFleet()
        fl_handler.SetMotdEx("Fleet Operations")
        motd = fl_handler.GetMotd()
        _undock_ship(sm)
        sm.RemoteSvc("fleetMgr").SendBroadcast("Target", 0, None, None)
        _advance_clock(sm, ipc, 12)
        _dock_station(sm, 60003760)
    finally:
        _ensure_docked_jita(sm, ipc)
        _leave_active_fleet(sm)
    passed = fleet_id > 0 and motd is not None
    return passed, {
        "fleet_id": fleet_id,
        "motd": motd,
        "char2_id": char2_id,
    }


def _journey_structures(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    primary_ship_id = ctx["primary_ship_id"]
    baseline_corp_id = ctx["baseline_corp_id"]
    struct_id = None
    c_id = int(_live_session_attr("corpid", 0) or 0)
    body_failed = False
    try:
        if c_id < 98000000:
            _try_slash(sm, "/giveskill 3363 2")
            _try_slash(sm, "/giveskill 11584 2")
            _try_slash(sm, "/giveisk 500000000")
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            c_res = sm.RemoteSvc("corpRegistry").AddCorporation(
                "Structure Builders", "EVSB", "Structure corporation", 10.0, False
            )
            c_id = int(_extract_id(c_res, ("corporationID", "corpID", "id")))
            _wait_session_attr("corpid", c_id, timeout_sec=3.0, raise_on_timeout=True)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
        _, inv_ships = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        cur_ship = None
        for it in (inv_ships or []):
            if int(_get_field(it, "itemID", 0)) == primary_ship_id:
                cur_ship = it
                break
        active_st_ship = primary_ship_id
        if cur_ship is None or int(_get_field(cur_ship, "typeID", 0)) != 20185:
            _try_slash(sm, "/giveskill 20342 1")
            _try_slash(sm, "/giveskill 20526 1")
            _try_slash(sm, "/giveskill 3342 5")
            _try_slash(sm, "/giveskill 3328 5")
            _try_slash(sm, "/createitem 20185 1")
            hauler_st, _ = _find_hangar_item(sm, 60003760, 20185, reverse=False)
            if hauler_st:
                _board_station_ship(sm, hauler_st, assemble=True)
                active_st_ship = hauler_st
        _try_slash(sm, "/createitem 35832 1")
        cit_id, _ = _find_hangar_item(sm, 60003760, 35832, reverse=False)
        if cit_id:
            sm.GetService("invCache").GetInventoryFromId(active_st_ship).Add(cit_id, 60003760)
        _undock_ship(sm)
        if cit_id:
            b_jita_st = _get_beyonce(sm, 30000142)
            _warp_full(sm, b_jita_st, 50001249)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("beyonce").CmdStargateJump(50001249)
            _wait_session_attr("solarsystemid2", 30000144, timeout_sec=3.0)
            b_perim = _get_beyonce(sm, 30000144)
            _ensure_view(sm, "inflight")
            _wait_view_settled(sm)
            _warp_full(sm, b_perim, 50002183)
            sm.RemoteSvc("slash").SlashCmd("/heal")
            b_perim.CmdWarpToStuff("item", 40009248, 0.0)
            sm.RemoteSvc("timeControl").AdvanceClock(112)
            bm_mid = sm.RemoteSvc("accessGroupBookmarkMgr").BookmarkStaticLocation(
                "Perim MidSafe", active_st_ship, "Perim", None
            )
            _sleep_sim_ms(350)
            sm.RemoteSvc("timeControl").AdvanceClock(240)
            _sleep_sim_ms(1500)
            _wait_warp_fx_settled(sm, timeout_sec=4.0)
            _warp_full(sm, b_perim, int(bm_mid[0]), stuff_type="bookmark")
            sm.RemoteSvc("timeControl").AdvanceClock(604860)
            anc_res = sm.RemoteSvc("structureDeployment").Anchor(cit_id, "Station Citadel")
            struct_id = int(_extract_id(anc_res) or cit_id)
            core_rep = _try_slash(sm, "/load me 56201 1")
            core_id = int(_get_field(core_rep, "itemID", 0) or 0)
            if core_id <= 0:
                raise RuntimeError(
                    "Failed to spawn Quantum Core for structure %d (rep=%r)" % (struct_id, core_rep)
                )
            inv_st, _ = sm.RemoteSvc("invbroker").MachoBindObject(struct_id, None)
            inv_st.Add(core_id, active_st_ship, flag=180)
            for _ in range(4):
                _try_slash(sm, "/upwelltimer %d" % struct_id)
            sm.RemoteSvc("timeControl").AdvanceClock(900)
            _sleep_ms(200)
            sm.RemoteSvc("structureDirectory").GetMyCorporationStructures()
            sm.RemoteSvc("structureDirectory").GetMyDockableStructures(30000144)
            try:
                sm.RemoteSvc("timeControl").ClearSessionTimer()
                sm.RemoteSvc("structureDocking").Dock(struct_id)
                _wait_session_attr("structureid", struct_id, timeout_sec=3.0, raise_on_timeout=True)
                sm.RemoteSvc("structureControl").TakeControl(struct_id)
                sm.RemoteSvc("structureControl").GetStructurePilot(struct_id)
                sm.RemoteSvc("structureControl").ReleaseControl(struct_id)
                sm.RemoteSvc("timeControl").ClearSessionTimer()
                sm.RemoteSvc("structureDocking").Undock(struct_id)
                _wait_session_attr("structureid", None, timeout_sec=3.0, raise_on_timeout=True)
            except Exception:
                if _live_session_attr("structureid", None) is not None:
                    sm.RemoteSvc("timeControl").ClearSessionTimer()
                    sm.RemoteSvc("structureDocking").Undock(struct_id)
                    _wait_session_attr("structureid", None, timeout_sec=3.0, raise_on_timeout=True)
                raise
            _cleanup_corp_structures(sm, ipc=ipc, structure_ids=(struct_id,))
            _warp_full(sm, b_perim, 50002185)
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("beyonce").CmdStargateJump(50002185)
            _wait_session_attr("solarsystemid2", 30000142, timeout_sec=3.0, raise_on_timeout=True)
            b_jita_back = _get_beyonce(sm, 30000142)
            _ensure_view(sm, "inflight")
            _wait_view_settled(sm)
            _warp_full(sm, b_jita_back, 60003760)
    except Exception:
        body_failed = True
        raise
    finally:
        cleanup_errors = []
        try:
            if struct_id or int(_live_session_attr("corpid", 0) or 0) >= 98000000:
                _cleanup_corp_structures(sm, ipc=ipc, structure_ids=(struct_id,) if struct_id else None)
        except Exception as exc:
            cleanup_errors.append("cleanup_corp_structures: %s" % exc)
        try:
            if int(_live_session_attr("solarsystemid2", 30000142) or 30000142) != 30000142:
                _try_slash(sm, "/tr 60003760")
                _wait_session_attr("solarsystemid2", 30000142, timeout_sec=3.0, raise_on_timeout=True)
        except Exception as exc:
            cleanup_errors.append("return_to_jita: %s" % exc)
        try:
            _ensure_docked_jita(sm, ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_docked_jita: %s" % exc)
        try:
            _ensure_home_ship(sm, primary_ship_id, ipc=ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_home_ship: %s" % exc)
        try:
            _resign_player_corp(sm, baseline_corp_id, ipc=ipc, structure_ids=(struct_id,) if struct_id else None)
        except Exception as exc:
            cleanup_errors.append("resign_player_corp: %s" % exc)
        if not body_failed and cleanup_errors:
            raise RuntimeError("structures cleanup failed: %s" % "; ".join(cleanup_errors))
    passed = struct_id is not None
    return passed, {
        "structure_id": struct_id,
        "corp_id": c_id,
    }


def _journey_sovereignty_fw(ctx):
    sm = ctx["sm"]
    sm.RemoteSvc("facWarMgr").JoinFactionAsCharacter(500001)
    _wait_session_attr("warfactionid", 500001, timeout_sec=3.0)
    fw_stats = sm.RemoteSvc("facWarMgr").GetStats_Character()
    fw_occ = sm.RemoteSvc("fwWarzoneSolarsystem").GetLocalOccupationState(30000142)
    fw_warzones = sm.RemoteSvc("fwWarzoneSolarsystem").GetAllWarzones()
    fw_states = sm.RemoteSvc("fwWarzoneSolarsystem").GetAllWarzonesOccupationStates()
    passed = fw_stats is not None and fw_occ is not None and bool(fw_warzones) and fw_states is not None
    return passed, {
        "faction_id": 500001,
        "warzones_count": len(fw_warzones) if isinstance(fw_warzones, (list, tuple, dict)) else 1,
    }


def _journey_daily_goals_events(ctx):
    sm = ctx["sm"]
    char_id = ctx["char_id"]
    pgw = _sync_quasar_gateway(sm, char_id) or ctx.get("pgw") or sm.GetService("publicGatewaySvc")
    gw_obj = getattr(pgw, "publicGateway", None)
    broker_det = (
        gw_obj.get_request_broker_details()
        if gw_obj is not None and hasattr(gw_obj, "get_request_broker_details")
        else {}
    )
    from dailygoals.client.goalMessenger import DailyGoalMessenger

    goals_cur = DailyGoalMessenger.get_instance(pgw).get_current_goals()
    goal_prog = sm.RemoteSvc("seasonManager").get_character_progress_by_character_id(2)
    ops_cat = sm.RemoteSvc("operationsManager").can_character_play_the_tutorial()
    pgw_avail = bool(pgw.is_available()) if pgw is not None and hasattr(pgw, "is_available") else False
    passed = bool(pgw_avail and ops_cat is not None and goals_cur and goal_prog is not None)
    return passed, {
        "public_gateway_available": pgw_avail,
        "broker_details_present": bool(broker_det),
        "goals_present": bool(goals_cur),
        "season_progress_ok": goal_prog is not None,
    }


def _journey_cosmetics_store(ctx):
    sm = ctx["sm"]
    ipc = ctx.get("ipc")
    primary_ship_id = ctx["primary_ship_id"]
    fc_res = sm.RemoteSvc("FastCheckoutService").GetOffersForUser()
    fc_packs = fc_res.get("offers", []) if isinstance(fc_res, dict) else getattr(fc_res, "offers", [])
    fc_pack = fc_packs[0] if fc_packs else {"offerId": 1, "price": 0.0}
    fc_free_ok = len(fc_packs) >= 1 and float(_get_field(fc_pack, "price", 1.0)) == 0.0
    fc_offer_id = int(_get_field(fc_pack, "offerId", 1) or 1)
    fc_buy = sm.RemoteSvc("FastCheckoutService").BuyOffer(fc_offer_id, 1)
    fc_status = _get_field(fc_buy, "status", "")
    new_plex_bal = int(_get_field(fc_buy, "newPlexBalance", 0) or 0)
    nes_offers = sm.RemoteSvc("storeManager").get_offers()
    nes_offer = next(
        (o for o in nes_offers if any(int(_get_field(p, "typeID", 0) or 0) > 0 for p in (_get_field(o, "products", []) or []))),
        nes_offers[1] if len(nes_offers) > 1 else nes_offers[0],
    )
    nes_offer_id = int(_get_field(nes_offer, "offerID", 1) or 1)
    nes_price = int(_get_field(nes_offer, "price", 0) or 0)
    can_buy_nes = sm.RemoteSvc("storeManager").can_purchase_offer(nes_offer_id, "PLX", 1)
    nes_buy = sm.RemoteSvc("storeManager").buy_offer(nes_offer_id, "PLX", 1)
    delivered_id = int(_get_field(nes_buy, "deliveredItemID", 0) or 0)
    plex_rem = int(_get_field(nes_buy, "plexRemaining", -1))

    cos_ship_id = primary_ship_id
    skins = []
    body_failed = False
    try:
        _, inv_cos = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
        ship_type_id = 0
        for it in (inv_cos or []):
            if int(_get_field(it, "itemID", 0)) == primary_ship_id:
                ship_type_id = int(_get_field(it, "typeID", 0))
                break
        if ship_type_id != 587:
            _try_slash(sm, "/giveskill 3327 1")
            _try_slash(sm, "/giveskill 3329 1")
            _try_slash(sm, "/createitem 587 1")
            _, inv_cos2 = sm.RemoteSvc("invbroker").MachoBindObject(60003760, ("List", (), {}))
            rifter_id = None
            for it in (inv_cos2 or []):
                if int(_get_field(it, "typeID", 0)) == 587 and int(_get_field(it, "flagID", 0)) == 4:
                    rifter_id = int(_get_field(it, "itemID", 0))
                    if int(_get_field(it, "singleton", 0)) != 1:
                        sm.RemoteSvc("ship").AssembleShip([rifter_id])
                    break
            if rifter_id:
                _board_station_ship(sm, rifter_id, assemble=False)
                cos_ship_id = rifter_id
        sm.RemoteSvc("shipCosmeticsMgr").GiveSkin(170)
        sm.RemoteSvc("shipCosmeticsMgr").ApplySkinToShip(cos_ship_id, 170)
        skins = sm.RemoteSvc("shipCosmeticsMgr").GetLicencedSkins()
    except Exception:
        body_failed = True
        raise
    finally:
        if cos_ship_id != primary_ship_id:
            try:
                _ensure_home_ship(sm, primary_ship_id, ipc=ipc)
            except Exception:
                if not body_failed:
                    raise
    passed = (
        fc_free_ok
        and fc_status == "COMPLETED"
        and new_plex_bal >= 50100
        and can_buy_nes == True
        and delivered_id > 0
        and plex_rem == new_plex_bal - nes_price
        and isinstance(skins, list)
        and len(skins) >= 1
    )
    return passed, {
        "fast_checkout_offer_id": fc_offer_id,
        "new_plex_balance": new_plex_bal,
        "nes_offer_id": nes_offer_id,
        "nes_price": nes_price,
        "delivered_item_id": delivered_id,
        "plex_remaining": plex_rem,
        "skinned_ship_id": cos_ship_id,
        "skins_count": len(skins) if isinstance(skins, list) else -1,
    }


def _journey_mail_notifications(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    char2_id = ctx["char2_id"]
    mail_id = sm.RemoteSvc("mailMgr").SendMail(
        [char2_id], None, None, "Client Mail", "Sent from in-client journey driver", False, False
    )
    lbl_id = sm.RemoteSvc("mailMgr").CreateLabel("Important", 16776960)
    cal_start_ft = int(sm.RemoteSvc("machoNet").GetTime()) + 36000000000
    cal_id = sm.RemoteSvc("calendarMgr").CreatePersonalEvent(
        cal_start_ft, 60, "Fleet Briefing", "Jita 4-4", 1, []
    )
    _require_ipc_ok(
        ipc,
        {"cmd": "c2_read_mail", "mail_id": int(mail_id) if isinstance(mail_id, _INT_TYPES) else 1},
        context="mail_notifications",
    )
    passed = mail_id is not None and cal_id is not None
    return passed, {
        "mail_id": mail_id,
        "label_id": lbl_id,
        "calendar_event_id": cal_id,
        "char2_id": char2_id,
    }


def _journey_esi_external(ctx):
    esi_port = ctx["esi_port"]
    esi_req = _urllib.Request("http://127.0.0.1:%d/latest/status" % esi_port)
    esi_resp = _urllib.urlopen(esi_req, timeout=5.0)
    esi_body = esi_resp.read()
    if bytes is not str and isinstance(esi_body, bytes):
        esi_body = esi_body.decode("utf-8", "replace")
    status_code = esi_resp.getcode()
    passed = status_code == 200 and "players" in esi_body
    return passed, {
        "http_status": status_code,
        "body_excerpt": esi_body.strip()[:120],
    }


def _journey_downtime_persistence(ctx):
    sm = ctx["sm"]
    _try_slash(sm, "/downtime")
    _sleep_ms(200)
    bal_after_dt = sm.RemoteSvc("account").GetCashBalance(0)
    passed = bal_after_dt is not None
    return passed, {
        "balance_after_downtime": bal_after_dt,
    }


def _journey_gm_tools(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    ship_id = int(_live_session_attr("shipid", ctx["primary_ship_id"]) or ctx["primary_ship_id"])
    try:
        tr_rep = _try_slash(sm, "/tr 30000142")
        _wait_session_attr("stationid", None, timeout_sec=3.0)
        _wait_view_settled(sm)
        heal_rep = _try_slash(sm, "/heal")
        setattr_rep = _try_slash(sm, "/setattr %d 164 25" % ship_id)
    finally:
        _ensure_docked_jita(sm, ipc)
    passed = tr_rep is not None and heal_rep is not None and setattr_rep is not None
    return passed, {
        "ship_id": ship_id,
        "tr_ok": tr_rep is not None,
        "heal_ok": heal_rep is not None,
        "setattr_ok": setattr_rep is not None,
    }


def _journey_titans(ctx):
    sm = ctx["sm"]
    ipc = ctx["ipc"]
    primary_ship_id = ctx["primary_ship_id"]
    for sk_id in (3347, 24563, 24562, 3433, 20533, 21610):
        _try_slash(sm, "/giveskill %d 5" % sk_id)
    titan_id = 0
    dd_ok = False
    body_failed = False
    try:
        sm.RemoteSvc("timeControl").ClearSessionTimer()
        _try_slash(sm, "/tr 30004759")
        _wait_session_attr("solarsystemid2", 30004759, timeout_sec=5.0, raise_on_timeout=True)
        _ensure_view(sm, "inflight")
        _wait_view_settled(sm)
        sp_tit = _try_slash(sm, "/spawn 11567")
        titan_id = int(_get_field(sp_tit, "spawnedID", _get_field(sp_tit, "itemID", 0)) or 0)
        if titan_id > 0:
            sm.RemoteSvc("timeControl").ClearSessionTimer()
            sm.RemoteSvc("ship").Board(titan_id)
            _wait_session_attr("shipid", titan_id, timeout_sec=3.0, raise_on_timeout=True)
            dd_fit = _try_slash(sm, "/fit me 24550")
            dd_mod_id = int(_get_field(dd_fit, "itemID", 0) or 0)
            _try_slash(sm, "/fit me 43658")
            _try_slash(sm, "/load me 16274 50000")
            _try_slash(sm, "/load me 16275 5000")
            _try_slash(sm, "/heal")
            sm.RemoteSvc("crimewatch").SetSafetyLevel(0)
            sp_cap = _try_slash(sm, "/spawn 19720")
            cap_tgt_id = int(_get_field(sp_cap, "spawnedID", _get_field(sp_cap, "itemID", 0)) or 0)
            if cap_tgt_id > 0 and dd_mod_id > 0:
                sm.RemoteSvc("dogmaIM").AddTarget(cap_tgt_id)
                dogma_tit, _ = sm.RemoteSvc("dogmaIM").MachoBindObject(titan_id, None)
                dd_res = dogma_tit.Activate(dd_mod_id, "superWeapon", cap_tgt_id, 0)
                dd_ok = dd_res is not None
                sm.RemoteSvc("timeControl").AdvanceClock(310)
                _try_slash(sm, "/despawn %d" % cap_tgt_id)
    except Exception:
        body_failed = True
        raise
    finally:
        cleanup_errors = []
        try:
            sm.RemoteSvc("timeControl").ClearSessionTimer()
        except Exception as exc:
            cleanup_errors.append("ClearSessionTimer: %s" % exc)
        try:
            _try_slash(sm, "/tr 60003760")
            _wait_session_attr("solarsystemid2", 30000142, timeout_sec=5.0, raise_on_timeout=True)
        except Exception as exc:
            cleanup_errors.append("return_to_jita: %s" % exc)
        try:
            _ensure_docked_jita(sm, ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_docked_jita: %s" % exc)
        try:
            _ensure_home_ship(sm, primary_ship_id, ipc=ipc)
        except Exception as exc:
            cleanup_errors.append("ensure_home_ship: %s" % exc)
        if not body_failed and cleanup_errors:
            raise RuntimeError("titans cleanup failed: %s" % "; ".join(cleanup_errors))
    passed = titan_id > 0 and dd_ok
    return passed, {
        "titan_id": titan_id,
        "doomsday_fired": dd_ok,
    }


_JOURNEY_HANDLERS = [
    ("launch_login", _journey_launch_login),
    ("create_character", _journey_create_character),
    ("station_ui", _journey_station_ui),
    ("fit_ship", _journey_fit_ship),
    ("undock_navigate", _journey_undock_navigate),
    ("local_chat", _journey_local_chat),
    ("multiplayer_presence", _journey_multiplayer_presence),
    ("skills_training", _journey_skills_training),
    ("market_trade", _journey_market_trade),
    ("contracts", _journey_contracts),
    ("mining_loop", _journey_mining_loop),
    ("pve_combat", _journey_pve_combat),
    ("missions", _journey_missions),
    ("exploration", _journey_exploration),
    ("wormholes_filaments", _journey_wormholes_filaments),
    ("ship_loss", _journey_ship_loss),
    ("clones", _journey_clones),
    ("industry", _journey_industry),
    ("planetary", _journey_planetary),
    ("corporation", _journey_corporation),
    ("fleet", _journey_fleet),
    ("structures", _journey_structures),
    ("sovereignty_fw", _journey_sovereignty_fw),
    ("daily_goals_events", _journey_daily_goals_events),
    ("cosmetics_store", _journey_cosmetics_store),
    ("mail_notifications", _journey_mail_notifications),
    ("esi_external", _journey_esi_external),
    ("downtime_persistence", _journey_downtime_persistence),
    ("gm_tools", _journey_gm_tools),
    ("titans", _journey_titans),
]


def _format_evidence(passed, rpcs_sent, metrics, shot_path, tb_str):
    parts = ["rpcs=%d" % len(rpcs_sent or [])]
    if rpcs_sent:
        rpc_names = []
        for entry in rpcs_sent[:6]:
            svc = entry.get("service", "")
            meth = entry.get("method", "")
            rpc_names.append("%s.%s" % (svc, meth))
        if len(rpcs_sent) > 6:
            rpc_names.append("...")
        parts.append("calls=[%s]" % ", ".join(rpc_names))
    if metrics:
        parts.append("metrics=%s" % json.dumps(metrics, sort_keys=True))
    if shot_path:
        parts.append("screenshot=%s" % shot_path)
    if not passed and tb_str:
        last_line = tb_str.strip().splitlines()[-1] if tb_str.strip() else "error"
        parts.append("error=%s" % last_line)
    return "; ".join(parts)


def _execute_all_journeys(cfg, ipc):
    sm = __builtin__.sm
    user = str(cfg.get("user", "demo"))
    password = str(cfg.get("password", "demo-secret"))
    target_char = int(cfg.get("char_id", 90000001))
    esi_port = int(cfg.get("esi_port", 26004))

    char_id = _wait_for_session(sm, user, password, target_char, timeout_sec=120.0)
    ipc.notify({"cmd": "client1_authenticated", "char_id": char_id})
    sess = __builtin__.session
    ship_id = int(getattr(sess, "shipid", 0) or 0)
    if not ship_id:
        chars = sm.RemoteSvc("charUnboundMgr").GetCharactersToSelect()
        if chars:
            ship_id = int(_get_field(chars[0], "shipID", 0) or 0)
    primary_ship_id = ship_id
    home_station_id = int(getattr(sess, "stationid", None) or 60003760)
    home_system_id = int(getattr(sess, "solarsystemid2", None) or 30000142)
    cur_corp_id = int(getattr(sess, "corpid", 0) or 0)
    baseline_corp_id = cur_corp_id if 0 < cur_corp_id < 98000000 else 0
    baseline_wf = getattr(sess, "warfactionid", None)
    baseline_fitted_ids = (
        sorted(_snapshot_ship_fitted_item_ids(sm, primary_ship_id))
        if primary_ship_id
        else []
    )

    outcomes = {}
    pgw = _sync_quasar_gateway(sm, char_id) or sm.GetService("publicGatewaySvc")
    only_set = set(cfg["only_journeys"]) if cfg.get("only_journeys") else None
    rpc_sink = _install_rpc_recorder(sm)

    ctx = {
        "sm": sm,
        "ipc": ipc,
        "cfg": cfg,
        "user": user,
        "password": password,
        "char_id": char_id,
        "primary_ship_id": primary_ship_id,
        "home_station_id": home_station_id,
        "home_system_id": home_system_id,
        "baseline_corp_id": baseline_corp_id,
        "baseline": {
            "char_id": char_id,
            "ship_id": primary_ship_id,
            "station_id": home_station_id,
            "system_id": home_system_id,
            "corp_id": baseline_corp_id,
            "warfaction_id": baseline_wf,
            "fitted_item_ids": baseline_fitted_ids,
        },
        "esi_port": esi_port,
        "pgw": pgw,
        "char2_id": None,
    }

    def ensure_c2_ready():
        if ctx["char2_id"] is None:
            c2_ready = _require_ipc_ok(
                ipc,
                {"cmd": "ensure_client2_ready"},
                context="ensure_c2_ready",
            )
            raw_cid = c2_ready.get("char2_id")
            if (
                isinstance(raw_cid, bool)
                or not isinstance(raw_cid, _INT_TYPES)
                or int(raw_cid) <= 0
            ):
                raise RuntimeError(
                    "ensure_client2_ready returned invalid char2_id=%r" % (raw_cid,)
                )
            ctx["char2_id"] = int(raw_cid)
        return ctx["char2_id"]

    ctx["ensure_c2_ready"] = ensure_c2_ready

    out_dir = cfg.get("out_dir")
    if out_dir and not os.path.isdir(out_dir):
        os.makedirs(out_dir)
    outcomes_path = os.path.join(out_dir, "real_client_outcomes.json") if out_dir else ""

    for jid, handler in _JOURNEY_HANDLERS:
        if only_set is not None and jid not in only_set:
            continue

        pre_reset_tb = None
        try:
            _reset_baseline_state(sm, ipc, ctx["baseline"], char_id=ctx["char_id"])
        except Exception:
            pre_reset_tb = traceback.format_exc()

        ipc.notify({"cmd": "journey_start", "id": jid})
        _emit_client_marker(cfg, "EVOMU_JOURNEY_START", jid)

        if pre_reset_tb is not None:
            _trace(cfg, "PRE-RESET EXCEPTION before %s:\n%s" % (jid, pre_reset_tb))
            try:
                sys.stdout.write("[client-1] journey=%s pre-reset exception:\n%s\n" % (jid, pre_reset_tb))
                sys.stdout.flush()
            except Exception:
                pass
            shot_path = None
            if not cfg.get("silent", False):
                shot_path = _capture_inclient_screenshot(out_dir, jid, ipc=ipc)
            metrics = {"pre_reset_failed": True}
            entry = {
                "passed": False,
                "bridge_verified": False,
                "real_client_verified": False,
                "rpcs_sent": [],
                "metrics": metrics,
                "screenshot": shot_path,
                "traceback": pre_reset_tb,
                "evidence": _format_evidence(False, [], metrics, shot_path, pre_reset_tb),
            }
            outcomes[jid] = entry
            if outcomes_path:
                _atomic_write_json(outcomes_path, outcomes)
            ipc.notify({"cmd": "journey_progress", "id": jid, "entry": entry})
            _emit_client_marker(cfg, "EVOMU_JOURNEY_END", jid, "passed=False rpcs=0 pre_reset_failed=True")
            ipc.notify({"cmd": "journey_end", "id": jid, "passed": False})
            continue

        j_rpcs = []
        rpc_sink[0] = j_rpcs
        passed = False
        metrics = {}
        tb_str = None
        shot_path = None
        try:
            if jid in _C2_JOURNEYS:
                ensure_c2_ready()
            _sync_quasar_gateway(sm, char_id, raise_on_error=True)
            res = handler(ctx)
            if isinstance(res, tuple):
                passed, metrics = bool(res[0]), (res[1] if len(res) > 1 and isinstance(res[1], dict) else {})
            else:
                passed = bool(res)
        except Exception:
            passed = False
            tb_str = traceback.format_exc()
            _trace(cfg, "EXCEPTION in %s:\n%s" % (jid, tb_str))
            try:
                sys.stdout.write("[client-1] journey=%s exception:\n%s\n" % (jid, tb_str))
                sys.stdout.flush()
            except Exception:
                pass
        finally:
            rpc_sink[0] = None

        if not cfg.get("silent", False):
            shot_path = _capture_inclient_screenshot(out_dir, jid, ipc=ipc)

        try:
            _reset_baseline_state(sm, ipc, ctx["baseline"], char_id=ctx["char_id"])
        except Exception:
            post_tb = traceback.format_exc()
            _trace(cfg, "POST-RESET EXCEPTION after %s:\n%s" % (jid, post_tb))
            try:
                sys.stdout.write("[client-1] journey=%s post-reset exception:\n%s\n" % (jid, post_tb))
                sys.stdout.flush()
            except Exception:
                pass
            passed = False
            metrics["post_reset_failed"] = True
            tb_str = ("%s\n%s" % (tb_str, post_tb)) if tb_str else post_tb

        entry = {
            "passed": bool(passed),
            "bridge_verified": bool(passed),
            "real_client_verified": bool(passed),
            "rpcs_sent": list(j_rpcs),
            "metrics": metrics,
            "screenshot": shot_path,
            "traceback": tb_str,
            "evidence": _format_evidence(bool(passed), j_rpcs, metrics, shot_path, tb_str),
        }
        outcomes[jid] = entry
        if outcomes_path:
            _atomic_write_json(outcomes_path, outcomes)
        ipc.notify({"cmd": "journey_progress", "id": jid, "entry": entry})
        _trace(cfg, "done %s: passed=%r" % (jid, bool(passed)))
        try:
            sys.stdout.write("[client-1] done %s: passed=%r\n" % (jid, bool(passed)))
            sys.stdout.flush()
        except Exception:
            pass

        _emit_client_marker(cfg, "EVOMU_JOURNEY_END", jid, "passed=%r rpcs=%d" % (bool(passed), len(j_rpcs)))
        ipc.notify({"cmd": "journey_end", "id": jid, "passed": bool(passed)})

    if outcomes_path:
        _atomic_write_json(outcomes_path, outcomes)
    return outcomes


def run_from_startup():
    cfg_path = os.environ.get("EVOMU_INCLIENT_CFG", "")
    if not cfg_path or not os.path.exists(cfg_path):
        return
    cfg = json.load(open(cfg_path, "r"))
    ipc = IpcClient(cfg.get("ipc_port", 0))
    try:
        for _ in range(120):
            sm = getattr(__builtin__, "sm", None)
            if sm is not None and hasattr(sm, "IsServiceRunning") and sm.IsServiceRunning("machoNet"):
                break
            _sleep_ms(250)
        try:
            import evomu_site
            evomu_site._HOOKED_LOGMODULE = False
            evomu_site._install_logmodule_hook()
        except Exception:
            pass
        try:
            import logging
            class _TraceErrHandler(logging.Handler):
                def emit(self, record):
                    if record.levelno >= logging.ERROR or record.exc_info:
                        msg = self.format(record)
                        _trace(cfg, "PYLOG_ERR %s: %s" % (record.name, msg[:800]))
            _teh = _TraceErrHandler()
            logging.getLogger().addHandler(_teh)
        except Exception:
            pass
        outcomes = _execute_all_journeys(cfg, ipc)
        ipc.notify({"cmd": "driver_finished", "count": len(outcomes)})
    except Exception:
        tb = traceback.format_exc()
        sys.stderr.write("[evomu_inclient_driver] FAILURE:\n%s\n" % tb)
        sys.stderr.flush()
        ipc.notify({"cmd": "driver_failed", "error": tb[-1500:]})
    finally:
        ipc.close()
        try:
            import bluepy
            bluepy.Terminate(0)
        except Exception:
            os._exit(0)


def schedule_startup():
    global _STARTED
    if _STARTED:
        return
    _STARTED = True
    try:
        import uthread
        uthread.new(run_from_startup)
    except Exception:
        import threading
        t = threading.Thread(target=run_from_startup)
        t.setDaemon(True)
        t.start()
