# Client Update Protocol

When CCP updates the Tranquility EVE Online client in `SharedCache` (`tq/EVE.app` on macOS or `tq/` on Windows), `eve-server`, the Quasar descriptor registry, the SDE database, and the Playtester must advance without breaking in-flight reviews or interrupting live verification.

## Detection and Classification

Run the client-update inspector whenever `SharedCache` changes (also checked automatically every 30 minutes by the Supervisor's `flow_status.py`):

```bash
python3 .agents/skills/evomu-adversarial-review/scripts/client_update.py
```

The inspector compares the installed `SharedCache` (`start.ini`, `eveMachoNetVersion.pyj`, `resfileindex.txt`, `code.ccp`, `carbon.json`, and `bin64/_destiny.so`) against `crates/eve-machonet/src/lib.rs`, `data/quasar_descriptor_manifest.json`, `docs/index/staticdata_catalog.json`, and `crates/eve-destiny/tests/fixtures/ccp_golden_vectors.json`, classifying the update into one of three tiers:

| Tier | Trigger | Live Client Impact | Required Tracks |
| :--- | :--- | :--- | :--- |
| **Tier 1 — Build Bump** | `build` incremented; `version`, `codename`, and `machoVersion` unchanged | Newer client connects cleanly (`client.build >= server.build`); SDE/Quasar sweep tests fail only if manifests drift | Single `hotfix/client-<codename>-<build>` track updating `V24_BOOT_BUILD` and any drifted Quasar/SDE manifests |
| **Tier 2 — Point Release** | `version` or `codename` bumped (e.g. `V24.01 -> V24.02`), `machoVersion` unchanged | **Immediate P0 handshake rejection** (`HANDSHAKE_INCOMPATIBLERELEASE` / `HANDSHAKE_INCOMPATIBLEVERSION`) on new client launches | **Track 1** (Handshake + Version Constants + Quasar Descriptors) -> Playtester Cutover -> **Track 2** (SDE + Static Catalog + Golden Fixtures) |
| **Tier 3 — Protocol or Runtime Change** | `machoVersion` changed, `GPS.pyj` handshake altered, `carbon_destiny` bumped, or existing RPC/notification names removed in `code.ccp` | Wire framing, crypto/handshake, RPC dispatch, or physics divergence | Full bytecode diff of `GPS.pyj` / `eveMachoNetVersion.pyj`, `rpc_names.py` audit, and `client_expectations` re-extraction before Track 1 & Track 2 |

## Playtester Suspension During Update

When a Tier 2 or Tier 3 update lands in `SharedCache` before `main` is updated:

- **Suspend the Playtester daemon:** Stop `playtester.py` / `exefile` while the server update tracks build, review, and land (freeing CPU/memory and avoiding testing against a superseded client build).
- **Resume after `main` updates:** Once the server update lands on `origin/main`, merge `origin/main`, rebuild `eve-server`, stage the updated `SharedCache` client (`cryptoPack = Placebo` in `common.ini`), and relaunch the Playtester daemon on the new client build.

## Isolated Worktree Invariant for Local Client Data

`data/quasar_descriptors.sqlite` and `data/evomu_sde.sqlite` are local, gitignored client-derived databases. In-flight `run_review.py` snapshots for other branches symlink fallback files from `<main checkout>/data/`.

- **Never overwrite `<main checkout>/data/quasar_descriptors.sqlite` or `<main checkout>/data/evomu_sde.sqlite` while unmerged tracks are in review against the old manifests.** Doing so breaks every concurrent review with `ManifestMismatch` or SDE table count failures.
- **Generate inside the update worktree's own `data/` directory:** `run_review.py` (`LOCAL_DATA_FILES`) prefers `<worktree>/data/<file>` over `<main checkout>/data/<file>`, so generating `<wt>/data/quasar_descriptors.sqlite` or `<wt>/data/evomu_sde.sqlite` lets the update track pass both Rule 6 and `run_review.py` in complete isolation.
- **Atomic swap at merge time:** Right when merging the reviewed update track onto `main` (before running `preflight.py` on `main`), atomically replace `<main checkout>/data/<file>` (`mv -f <wt>/data/<file>.tmp <main>/data/<file>` or copy + `os.replace`).

## Track Execution Sequence

Keep each track under the 60-file / 8,000-line cap (Rule 9). Do not mix protocol/Quasar updates and SDE/golden-fixture updates in the same branch when both changed.

### Track 1: Handshake, Version Constants, Quasar Registry, and README (`hotfix/client-<ver>-protocol-quasar`)

1. **Single-source version constants (`crates/eve-machonet/src/lib.rs`) and `README.md`:**
   - Update `V24_MACHO_VERSION`, `V24_VERSION_NUMBER`, `V24_BOOT_BUILD`, and `V24_CODENAME_AT_REGION` from `start.ini` and `eveMachoNetVersion.pyj`.
   - Update `README.md` (supported client release and build number in the intro, Prerequisites, Testing strategy, and Repository layout diagram) so `README.md` matches the exact `codename` and `build` supported on `main` (verified mechanically by `client_update.py`).
   - Ensure `eve-server` (`auth_cache.rs`, `macho_cluster_mgmt.rs`, Quasar client-version metadata) and handshake/auth tests reference `eve_machonet` constants rather than hardcoded build or version literals.
2. **Quasar descriptors (`crates/eve-proto` & `data/quasar_descriptor_manifest.json`):**
   - Extract from the new `code.ccp` into the worktree's own `data/` directory:
     ```bash
     python3 tools/quasar-import/extract_quasar_descriptors.py \
       --sqlite "$WT/data/quasar_descriptors.sqlite" \
       --manifest "$WT/data/quasar_descriptor_manifest.json"
     ```
   - Update `EXPECTED_QUASAR_DOMAIN_COUNT` and `EXPECTED_QUASAR_FILE_COUNT` in `crates/eve-proto/src/quasar.rs` (and any test count assertions).
3. **Identifier regression check:**
   - Run `python3 .agents/skills/evomu-adversarial-review/scripts/metrics/rpc_names.py` against the updated `code.ccp` to confirm `NOT_IN_CLIENT == 0` across all server RPCs and notifications.
4. **Verify, review, and atomic swap:**
   - Run Rule 6 on `eve-machonet`, `eve-proto`, and `eve-server`, pass `run_review.py`, atomically copy `$WT/data/quasar_descriptors.sqlite` into `<main>/data/quasar_descriptors.sqlite`, merge to `main`, push `origin/main`, and notify the Playtester to cut over to the new client build.

### Track 2: SDE Static Data, Catalog, and Differential Fixtures (`feat/client-<ver>-sde-catalog`)

1. **Sync `docs/index/staticdata_catalog.json` and `tools/sde-import/import_sde.py`:**
   - Refresh `docs/index/staticdata_catalog.json` from `resfileindex.txt` (`python3 .agents/skills/evomu-adversarial-review/scripts/client_update.py --sync-catalog "$WT/docs/index/staticdata_catalog.json"`), adding any new `res:/staticdata/*` or `res:/localizationfsd/*` entries.
   - Ensure `tools/sde-import/import_sde.py` reads `client_build` and `client_release` dynamically from `start.ini` (and registers schemas/loaders if new `.fsdbinary` or `.static` tables need domain exposure).
2. **Rebuild `evomu_sde.sqlite` and `sde_import_manifest.json` in the worktree:**
   ```bash
   python3 tools/sde-import/import_sde.py \
     --out-db "$WT/data/evomu_sde.sqlite" \
     --manifest "$WT/data/sde_import_manifest.json" \
     --force-extract
   ```
3. **Update SDE & `dialogs.static` contract tests:**
   - Update resource/row count assertions in `crates/eve-sde/tests/*` (`client_static_resources.rs`, `sde_sweep.rs`, `sde_localization_fsd.rs`, etc.) from the generated `sde_import_manifest.json` (never from memory).
   - Verify `crates/eve-testkit/src/client_contract.rs` (`UserErrorKey` vs `dialogs.static`) passes cleanly.
4. **Differential Destiny & Dogma check (when `_destiny.so` or `dogma/*.pyj` changed):**
   - If `carbon_destiny` or `dogma/*.pyj` changed, capture `<codename>-<build>` fixtures with `python3 tools/generate_golden_fixtures.py` and inspect `python3 tools/generate_golden_fixtures.py --diff <old_key> <new_key>` (using a separate mechanical fixture branch with `increment-size: allow` in claims if the JSON diff exceeds 8,000 lines).
5. **Verify, review, and atomic swap:**
   - Run Rule 6 on `eve-sde`, `eve-dogma`, `eve-destiny`, `eve-testkit`, and `eve-server`, pass `run_review.py`, atomically copy `$WT/data/evomu_sde.sqlite` into `<main>/data/evomu_sde.sqlite`, merge to `main`, and push `origin/main`.

### Track 3: Client Surface Re-Indexing (When New Features Ship)

When `client_update.py` reports newly added `RemoteSvc`/moniker RPCs, `__notifyevents__`, or Quasar domains that are client-reachable:
- Run `tools/eve-indexer/index_server_logic.py` and `.agents/skills/evomu-adversarial-review/scripts/client_expectations/extractor.py` to refresh the catalog and inspect new client call sites before scheduling feature parity tracks.
