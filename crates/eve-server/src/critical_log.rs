//! Dedicated critical log marker reserved strictly for prevented client jolts (`GH #1084`).
//!
//! Nothing outside `outbound_sequencer`'s prevented-jolt rejection path may invoke this macro,
//! giving operators and the playtester a single unmistakable token across stderr, the log ring
//! buffer (`"CRT"`), and `FlightRecorder`.

#![forbid(unsafe_code)]

use std::io::Write;

const CRITICAL_BANNER_PREFIX: &str = "\x1b[1;31m[CRITICAL][PREVENTED_JOLT]\x1b[0m";

/// Records a prevented-jolt marker across stderr, the global log ring buffer (`"CRT"`), and
/// (when attached) [`crate::flight_recorder::FlightRecorder`].
#[doc(hidden)]
pub fn __emit_prevented_jolt_critical(
    session_id: u64,
    epoch: u32,
    batch_stamp: u32,
    trusted_stamp: u32,
    reason: &str,
    flight_recorder: Option<&crate::flight_recorder::FlightRecorder>,
) {
    let message = format!(
        "prevented client jolt: session={session_id} epoch={epoch} batch_stamp={batch_stamp} trusted_stamp={trusted_stamp} reason={reason}"
    );
    {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{CRITICAL_BANNER_PREFIX} {message}");
    }

    crate::log_ring_buffer::global_log_ring().record(
        "CRT",
        "outbound_sequencer::prevented_jolt",
        &message,
    );

    if let Some(fr) = flight_recorder {
        fr.record_prevented_jolt(session_id, epoch, batch_stamp, trusted_stamp, reason);
    }
}

/// Emits a prevented-jolt critical banner on stderr, a `"CRT"` ring-buffer entry, and a
/// `FlightRecorder` marker. Reserved strictly for prevented backstepping stamp jolts (`GH #1084`).
#[macro_export]
macro_rules! critical {
    (
        session_id = $session_id:expr,
        epoch = $epoch:expr,
        batch_stamp = $batch_stamp:expr,
        trusted_stamp = $trusted_stamp:expr,
        reason = $reason:expr,
        flight_recorder = $fr:expr $(,)?
    ) => {
        $crate::critical_log::__emit_prevented_jolt_critical(
            $session_id,
            $epoch,
            $batch_stamp,
            $trusted_stamp,
            $reason,
            $fr,
        )
    };
}
