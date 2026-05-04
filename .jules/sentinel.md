## 2024-03-27 - [Input Validation and Resource Limits]
**Vulnerability:** Missing input validation for plugin parameters (allowing NaN/Infinity) and lack of resource limits (unbounded block size and drain time).
**Learning:** CLI applications that process external data or use plugin systems must strictly validate all user-provided inputs, including numeric parameters, to prevent unexpected behavior or resource exhaustion.
**Prevention:** Implement strict range checks and sanity checks for all CLI arguments and configuration file inputs.

## 2025-01-24 - [Partial Validation and Empty Chain DoS]
**Vulnerability:** Input validation for plugin parameters was missing for YAML mapping formats, and an empty plugin chain (possible via YAML) caused an index-out-of-bounds panic.
**Learning:** Security checks must be applied at all entry points (CLI and Config) and after all loading logic is complete, rather than just in one branch of the initialization.
**Prevention:** Centralize core validation logic so it applies regardless of the input source.
