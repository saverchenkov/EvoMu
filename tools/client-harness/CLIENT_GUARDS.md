# Client Behaviour Guards Registry (#1539)

Canonical documentation of every client-code behaviour guard in [`evomu_site.py`](evomu_site.py), synchronized 1:1 with [`guard_registry.py`](guard_registry.py).

## Operating Policy

1. **Hit recording**: Every guard wrapper records `{seq, guard, function, reason, time}` whenever it skips the client's method, returns after catching an exception, substitutes a fallback, or filters inputs.
2. **Classification & approval**:
   - `server-caused`: must reference an **open server issue** that will remove it.
   - `client-only`: requires a written case proving the failure does not depend on anything the server sends **and** repository-owner approval (`owner_approved: True`). **Zero guards have `client-only` approval today.**
3. **Off switch (`EVOMU_DISABLE_SERVER_GUARDS`)**:
   - Attestation journeys (`run_journeys` and `playtester.py --scenario`) set `EVOMU_DISABLE_SERVER_GUARDS=1` so **zero** `server-caused` guards are installed.
   - Continuous playtester (`playtester.py --continuous`) keeps guards ON and writes hit telemetry into `exec_result.json`, `run_summary.json`, and `memory.json`.
4. **Monotonic ratchets**:
   - Registered behaviour guards: `<= 15` (may only fall).
   - `BENIGN_USER_ERROR_MARKERS` in `playtester.py`: `<= 75` (may only fall).

## Classification Table (15 Guards)

| Guard ID | Installer | Classification | Open Issue | Named Issues | Server Fix Landed | Owner Approved | Client Targets |
| :--- | :--- | :--- | :--- | :--- | :--- | :--- | :--- |
| `camera_dock_guard` | `_install_camera_dock_guard` | `server-caused` | `#1190` | `#996, #1051, #1154` | `before_period` | `no` | `ViewStateService`, `SpaceCamera`, `TacticalCamera`, `Michelle`, `__LocationArrival` |
| `base_space_camera_guard` | `_install_base_space_camera_guard` | `server-caused` | `#1190` | `#1340` | `no` | `no` | `BaseSpaceCamera.ResetAnchorPos`, `UpdateAnchorOffset`, `GetTrackPosition` |
| `hangar_transition_guard` | `_install_hangar_transition_guard` | `server-caused` | `#1190` | `#1163` | `yes` | `no` | `SpaceToHangarTransition.StartTransition`, `EndTransition` |
| `hangar_view_teardown_guard` | `_install_hangar_view_teardown_guard` | `server-caused` | `#1190` | `#1154` | `before_period` | `no` | `HangarView.*` (scene clear, ship model replace, teardown, undock, skin, reload) |
| `inventory_transition_guard` | `_install_inventory_transition_guard` | `server-caused` | `#1190` | `#1267, #1290, #1301, #1333` | `yes` | `no` | `Inventory.*`, `InvContCapacityGauge.*`, `invCacheContainer.List` |
| `fitting_active_ship_guard` | `_install_fitting_active_ship_guard` | `server-caused` | `#1190` | `#1254, #1410, #1443` | `partial (#1254 yes; #1410 no)` | `no` | `FittingSvc`, `FittingWarnings`, `WarningIconCont`, `StatsPanel`, `DronePanel` |
| `base_hangar_behaviour_guard` | `_install_base_hangar_behaviour_guard` | `server-caused` | `#1190` | `#1286` | `no` | `no` | `BaseHangarShipBehaviour.SetShipDamage` |
| `modular_capital_hangar_guard` | `_install_modular_capital_hangar_guard` | `server-caused` | `#1190` | `#1286` | `no` | `no` | `ModularCapitalHangarShipBehaviour.ProcessSkinChange` |
| `modular_hangar_teardown_guard` | `_install_modular_hangar_teardown_guard` | `server-caused` | `#1190` | `#1218, #1286` | `partial (#1218 yes; #1286 no)` | `no` | `ModularHangarSceneController.CleanScene`, `ModularHangarShipBehaviour.ProcessSkinChange` |
| `wreck_rotation_guard` | `_install_wreck_rotation_guard` | `server-caused` | `#1190` | `#1217` | `yes` | `no` | `Wreck.Rotate`, `OnModelLoaded`, `SetRandomRotation`, `SpaceMgr` |
| `slots_container_sync_guard` | `_install_slots_container_sync_guard` | `server-caused` | `#1512` | `#1252` | `yes` | `no` | `SlotsContainer.OnModuleFitted`, `ProcessShipEffect`, `OnInvChange` |
| `sensor_suite_gfx_guard` | `_install_sensor_suite_gfx_guard` | `server-caused` | `#1190` | `#1292, #1293` | `no` | `no` | `SensorSuiteHandler.StartGfxSwipe`, `_GfxSwipeThread` |
| `modular_hangar_camera_guard` | `_install_modular_hangar_camera_guard` | `server-caused` | `#1190` | `#1318` | `no` | `no` | `ModularHangarCamera.*`, `ModularHangarCapitalCamera.*` |
| `skill_plan_panel_signal_guard` | `_install_skill_plan_panel_signal_guard` | `server-caused` | `#1190` | `#1332` | `no` | `no` | `SkillPlanPanel` signal slots and `Close` |
| `probe_tracker_guard` | `_install_probe_tracker_guard` | `server-caused` | `#1190` | `#1334` | `yes` | `no` | `ProbeTracker.OnProbesIdle`, `SetProbeDestination`, `UpdateProbeState` |

## Out-of-Scope Environment Adapters

These hooks adapt localhost XMPP TLS/port, flatbuffer CDN HTTP redirects, FSD typelist preloading, and traceback telemetry and are **not** disabled by `EVOMU_DISABLE_SERVER_GUARDS`:

- `_install_logmodule_hook`
- `_install_xmpp_plaintext_guard`
- `_install_xmpp_port_guard`
- `_install_flatbuffer_cdn_http_guard`
- `_install_fsd_typelist_preload_hook`
