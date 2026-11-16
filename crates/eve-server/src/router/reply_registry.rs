//! Typed V24 client reply registry (`RFC-3 C-9`, `#933`, `#1189`).
//!
//! Centralizes the shared [`ToClient`] trait so downstream service families encode
//! client-ready wire shapes directly instead of adding rewrite arms in `router/client_compat.rs`.

#![forbid(unsafe_code)]

pub use eve_proto::ToClient;

/// Converts a strongly-typed reply value implementing [`ToClient`] into its wire [`eve_proto::PyRep`].
pub fn into_client_reply<T: ToClient>(value: T) -> eve_proto::PyRep {
    value.into_client_rep()
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_proto::PyRep;

    struct SampleReply(i64);

    impl crate::router::ToClient for SampleReply {
        fn into_client_rep(self) -> PyRep {
            PyRep::keyval([("sampleID", PyRep::Integer(self.0))])
        }
    }

    #[test]
    fn into_client_reply_encodes_pyrep_and_custom_to_client_impl() {
        assert_eq!(into_client_reply(PyRep::Integer(42)), PyRep::Integer(42));
        let rep = into_client_reply(SampleReply(77));
        assert_eq!(rep.dict_get("sampleID").and_then(|v| v.as_i64()), Some(77));
    }
}
