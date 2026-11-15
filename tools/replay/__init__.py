"""Virtual-clock harness trace capture, replay, and golden sequence verification."""

from .trace import (
    SCHEMA_VERSION,
    TraceRecorder,
    VirtualClock,
    extract_golden,
    load_trace,
    save_trace,
)
from .verify_trace import (
    check_destiny_invariants,
    compare_golden,
    verify_trace_data,
    verify_trace_file,
)

__all__ = [
    "SCHEMA_VERSION",
    "TraceRecorder",
    "VirtualClock",
    "check_destiny_invariants",
    "compare_golden",
    "extract_golden",
    "load_trace",
    "save_trace",
    "verify_trace_data",
    "verify_trace_file",
]
