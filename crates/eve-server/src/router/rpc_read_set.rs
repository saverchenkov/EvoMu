//! Central sorted declaration of read-only `(service, method)` RPC pairs (`#1194` Track 3B-4).
//!
//! Entries listed here promise that a successful call commits nothing to the world database
//! (`RpcAccess::Read`). Unlisted entries default to `RpcAccess::Write`. A module being edited
//! anyway may alternatively mark its entry with `.read_only()` in place, but declaring the same
//! pair in both places is rejected when [`crate::router::rpc_registry::RpcRegistry`] is built.

/// Sorted list of `(service, method)` RPC pairs verified to succeed without committing DB state.
pub const READ_ONLY_RPCS: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use eve_machonet::SessionState;
    use eve_proto::{BridgeRpcError, PyRep};

    use super::*;
    use crate::router::context::ServerContext;
    use crate::router::rpc_registry::{
        rpc_entry, rpc_entry_with_caller, RpcAccess, RpcCallerClass, RpcDispatchHandler, RpcEntry,
        RpcRegistry,
    };

    fn noop_handler(
        _ctx: &ServerContext,
        _session: &mut SessionState,
        _service: &str,
        _method: &str,
        _args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        Some(Ok(PyRep::None))
    }

    #[test]
    fn read_only_rpcs_list_is_sorted_and_unique_and_builds_default_registry() {
        for pair in READ_ONLY_RPCS.windows(2) {
            assert!(
                pair[0] < pair[1],
                "READ_ONLY_RPCS must be strictly sorted and unique; found {:?} before {:?}",
                pair[0],
                pair[1]
            );
        }

        let reg = RpcRegistry::build();
        let read_set: std::collections::HashSet<(&str, &str)> =
            READ_ONLY_RPCS.iter().copied().collect();
        for entry in reg.all_entries() {
            let expected = if read_set.contains(&(entry.service, entry.method)) {
                RpcAccess::Read
            } else {
                RpcAccess::Write
            };
            assert_eq!(
                entry.access, expected,
                "unexpected access for {}.{}",
                entry.service, entry.method
            );
        }
        for &(svc, meth) in READ_ONLY_RPCS {
            let entry = reg
                .lookup(svc, meth)
                .unwrap_or_else(|| panic!("missing compiled entry for {svc}.{meth}"));
            assert_eq!(entry.access, RpcAccess::Read);
        }
    }

    #[test]
    fn read_set_validation_rejects_missing_duplicate_and_double_declared_entries() {
        static TEST_ENTRIES: &[RpcEntry] = &[
            rpc_entry(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "ReadViaList",
                RpcDispatchHandler::Sync(noop_handler),
            ),
            rpc_entry_with_caller(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "ReadInPlace",
                RpcCallerClass::Account,
                RpcDispatchHandler::Sync(noop_handler),
            )
            .read_only(),
            rpc_entry(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "DefaultWrite",
                RpcDispatchHandler::Sync(noop_handler),
            ),
        ];

        // Listed pair comes back Read; in-place .read_only() comes back Read; unlisted is Write.
        let reg = RpcRegistry::try_from_slices_with_reads(
            &[TEST_ENTRIES],
            &[(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "ReadViaList",
            )],
        )
        .expect("valid read set should build");
        assert_eq!(
            reg.lookup("testSvc", "ReadViaList").unwrap().access, // rpc-lint: allow synthetic read-set validation fixture
            RpcAccess::Read
        );
        assert_eq!(
            reg.lookup("testSvc", "ReadInPlace").unwrap().access, // rpc-lint: allow synthetic read-set validation fixture
            RpcAccess::Read
        );
        assert_eq!(
            reg.lookup("testSvc", "DefaultWrite").unwrap().access, // rpc-lint: allow synthetic read-set validation fixture
            RpcAccess::Write
        );

        // Pair with no table entry fails validation.
        let missing_err = RpcRegistry::try_from_slices_with_reads(
            &[TEST_ENTRIES],
            &[(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "NoSuchEntry",
            )],
        )
        .expect_err("missing read-set entry must fail validation");
        assert!(
            missing_err.contains("testSvc.NoSuchEntry"),
            "unexpected error: {missing_err}"
        );

        // Pair listed twice in read_only fails validation.
        let dup_err = RpcRegistry::try_from_slices_with_reads(
            &[TEST_ENTRIES],
            &[
                (
                    "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                    "ReadViaList",
                ),
                (
                    "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                    "ReadViaList",
                ),
            ],
        )
        .expect_err("duplicate read-set entry must fail validation");
        assert!(
            dup_err.contains("testSvc.ReadViaList"),
            "unexpected error: {dup_err}"
        );

        // Pair marked both with .read_only() in place and in read_only fails validation.
        let both_err = RpcRegistry::try_from_slices_with_reads(
            &[TEST_ENTRIES],
            &[(
                "testSvc", // rpc-lint: allow synthetic read-set validation fixture
                "ReadInPlace",
            )],
        )
        .expect_err("doubly-declared read-only entry must fail validation");
        assert!(
            both_err.contains("testSvc.ReadInPlace"),
            "unexpected error: {both_err}"
        );
    }
}
