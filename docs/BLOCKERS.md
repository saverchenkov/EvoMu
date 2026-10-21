# Known Environmental Dependencies & Reproducibility Notes (`docs/BLOCKERS.md`)

## 1. V24.01 Client `.fsdbinary` C-Extension Loader (`tools/sde-import/cfsd_runner.c`)

- **Context (`D-1` / `D-2`):** EVE Online V24.01 (`build 3619879`) stores its `.fsdbinary` static data archives (`res:/staticdata/*.fsdbinary`) in CCP's binary FSD format, whose schema and offset-table decoders are compiled into per-archive native Python 2.7 C-extension shared objects (`cfsd*Loader.so`) inside `~/Library/Application Support/EVE Online/SharedCache/tq/EVE.app/Contents/Resources/build/bin64/`.
- **How `tools/sde-import` runs against a local client install:** [`tools/sde-import/cfsd_runner.c`](../tools/sde-import/cfsd_runner.c) (`lines 1-22`) is a minimal C host binary linked against the V24.01 client's embedded `libpython2.7.dylib` that initializes the client Python 2.7 interpreter so [`tools/sde-import/import_sde.py`](../tools/sde-import/import_sde.py) can invoke each archive's `Loader.load()` directly and populate the full local SQLite database (`data/evomu_sde.sqlite`, `213/213` resources across `42` tables) and [`data/sde_import_manifest.json`](../data/sde_import_manifest.json):
  - **Full `data/evomu_sde.sqlite` (`data/sde_import_manifest.json` `table_counts`):** `213` `sde_resources`, `48` `sde_categories`, `1,611` `sde_groups`, `53,288` `sde_types`, `2,871` `sde_dogma_attributes`, `3,427` `sde_dogma_effects`, `9,866` `sde_type_materials`, `5,100` `sde_blueprints`, `114` `sde_regions`, `1,184` `sde_constellations`, `8,490` `sde_solar_systems`, `13,978` `sde_stargates`, `6,989` `sde_jumps`, `5,210` `sde_stations`, `475,859` `sde_celestials`, `11` `sde_races`, `18` `sde_bloodlines`, `43` `sde_ancestries`, `23` `sde_schools`, `27` `sde_factions`, `283` `sde_npc_corporations`, `10,972` `sde_agents`, `2,114` `sde_market_groups`, `4` `sde_clone_grades`, `277` `sde_dbuff_collections`, `441` `sde_dynamic_item_attributes`, `21` `sde_epic_arcs`, `36` `sde_fighter_abilities`, `94` `sde_fighter_abilities_by_type`, `5,516` `sde_dungeons`, `2,892` `sde_missions`, `584` `sde_node_graphs`, `392` `sde_npc_spawn_groups`, `68` `sde_planet_schematics`, `11,877` `sde_skin_licenses`, `7,048` `sde_skins`, `11,238` `sde_space_components`, `69` `sde_station_operations`, `27` `sde_station_services`, `212` `sde_compressible_types`, `132` `sde_wormhole_types`, `3` `sde_meta`.
- **Runtime & Test Resolution (`D-2`):** [`SdeRegistry::default_db_path()`](../crates/eve-sde/src/lib.rs) and [`SdeRegistry::with_canonical_seed()`](../crates/eve-sde/src/lib.rs) resolve strictly to `data/evomu_sde.sqlite` (or `EVOMU_SDE_PATH`), failing loudly with [`SdeError::MissingDatabase`](../crates/eve-sde/src/lib.rs) when the local SDE database has not been built via `tools/sde-import`.

## 2. Legal, Licensing & Public Repository Rules (`D-1` / `AGENTS.md`)

- **No Client-Derived Data Checked into Git:** Per `AGENTS.md` ("Client-derived data and the public repository"), neither `libpython2.7.dylib`, any `cfsd*Loader.so` C extension, any raw `.fsdbinary` / `.static` archive, nor any SQLite database or JSON dump built from client data (`data/*.sqlite*`) is tracked in Git.
- **Local-Only `data/evomu_sde.sqlite` & Tracked Checksum Manifest:**
  - The full `data/evomu_sde.sqlite` database is **excluded from version control** (`.gitignore`: `/data/*.sqlite*`) and built locally from the operator's own V24.01 client installation via `tools/sde-import`.
  - The tracked [`data/sde_import_manifest.json`](../data/sde_import_manifest.json) contains only `table_counts` and per-resource file checksums (`res_path`, `cdn_relative_path`, `md5`, `byte_size`, `format`, `record_count`).

## 3. Real-client handshake encryption (`T-6`, `P-6`)

- **How the client is pointed at EvoMu (no client files changed):** `EVE.app/Contents/MacOS/EVE /server:127.0.0.1 /port:<macho_port> /login:<user>:<password>`. `utillib.GetServerName`/`GetServerPort` read `/server:`/`/port:`, and `connection.TryAutomaticLogin` uses `/login:` once `machoNet.GetServerStatus` reports OK. `tools/client-harness/run_journeys --launch-real-client` does this and always kills the client process group and `eve_crashmon` afterwards.
- **Blocker:** the installed client's `start.ini` sets `cryptoPack = CryptoAPI`, so `evecrypto.cryptoapi` encrypts the handshake session key with CCP's embedded RSA public key (`blue.crypto.GetSharedAsymmetricCipher`) and verifies the server's handshake function with it. After `"OK CC"` every handshake frame is AES-encrypted with a key only CCP's private key can recover, so no third-party server can read the login request. EvoMu implements the `Placebo` crypto pack.
- **Working local procedure (scratch only, nothing committed, `SharedCache` untouched):** copy the client `build/` directory byte-for-byte into a plain directory, add `common.ini` with `[main]` / `cryptoPack = Placebo` (`eveprefs.Init` merges `common.ini` over `start.ini`; this is the same client setting evemu_Crucible requires), and run that copy's `bin64/exefile /server:127.0.0.1 /port:<macho_port> /login:<user>:<password>` with the copy as working directory. Adding the file inside an `.app` copy breaks its code-signature seal (Gatekeeper refuses it), and symlinked copies fail the client's manifest/executable-path verification.
- **Open decision for the user:** whether `run_journeys` may build such a Placebo-configured copy automatically. Until then the harness launches the stock client, which stops at the CryptoAPI step.

## 4. Compressed SDE Archive Out of Date (Windows/Cross-Platform Fresh Checkout)

- **Context:** The tracked compressed SDE archive (`data/evomu_sde.sqlite.xz`, 16 MB) was created at commit `f23bd1d` before celestials data (stars, planets, moons, asteroid belts) was imported. Commit `5ea6e74` later added the `GET /universe/stars/{star_id}` ESI endpoint and the full celestials sweep, but the archive was never regenerated.
- **Symptoms:** Test `esi_universe_fw_complete::test_esi_universe_celestials_stations_structures_and_jumps_kills` fails with `Star 40000001 not found` because the unpacked SDE (84 MB) is missing celestials data that exists in the macOS agent's live SDE (~100 MB).
- **Root Cause:** The compressed archive predates the celestials import and needs to be regenerated from the full, current macOS SDE.
- **Impact:** 1 ESI test fails on Windows and any fresh checkout that unpacks from the archive. The macOS agent with the live SDE passes all tests.
- **Required Action:** User needs to regenerate `data/evomu_sde.sqlite.xz` from the current macOS SDE and commit it, OR mark the celestials test as `#[ignore]` until the archive is updated.
- **Status:** Awaiting user decision on whether to update the archive or skip the test on Windows.

## 5. Windows Review Gate Blocked by macOS Client Dependencies (B-5)

- **Context:** The mandatory review gate requires `preflight.py` to print `OK` (zero test failures) before `run_review.py` can execute. On Windows, 68 tests fail because they depend on macOS-only client data and tools that cannot be reproduced on Windows without violating the licensing constraints in BLOCKERS#2.
- **Failed Test Categories:**
  - `data/quasar_descriptors.sqlite` (macOS client extraction via B-1)
  - Raw client files under `~/Library/Application Support/EVE Online/` (macOS-only paths)
  - `cargo-fuzz` (requires macOS client for corpus generation)
  - Client harness tests (require real V24.01 client with Placebo crypto, B-2)
  - Stale SDE archive (B-3)
- **Impact:** Windows development is blocked from running reviews, which blocks all commits to `main` and `track/*` branches per AGENTS.md. Track agents can commit only to local `wip/*` branches until this is resolved.
- **Baseline:** 528 tests pass, 68 fail (stable baseline as of commit 53b099c). No regressions from macOS baseline.
- **Proposed Solution:** Platform-specific review gate exception that allows Windows reviews when:
  1. All failures match the known blockers list (data/tools from macOS client)
  2. Pass count is stable or improving (≥528 passed)
  3. RPC name checks pass (no invented client RPCs)
  4. All other early-gate checks pass (no P0 violations)
  5. Claims document acknowledges the platform limitation
- **Status:** Awaiting user decision on whether to implement platform-specific gate, or require all review work to happen on macOS.

## 6. ~~Python Preflight Unicode Encoding~~ [RESOLVED]

- **Context:** Windows console (cmd.exe, PowerShell) may use encodings like CP1252 or CP437 that don't support all Unicode characters. When `preflight.py` prints parity reports containing Unicode characters (file paths, test names), it crashed with `UnicodeEncodeError`.
- **Symptoms:** `UnicodeEncodeError: 'charmap' codec can't encode character '…' in position 1234: character maps to <undefined>`
- **Root Cause:** Python's `print()` uses `sys.stdout.encoding` (console encoding) which may not be UTF-8 on Windows.
- **Fix (201d08a):** Added `safe_print()` wrapper that catches `UnicodeEncodeError` and re-encodes with replacement characters (`errors='replace'`), allowing the script to complete successfully even when the console doesn't support UTF-8.
- **Impact:** Preflight now runs cleanly on Windows consoles without crashing.
- **Status:** RESOLVED in commit 201d08a (2025-01-22).
