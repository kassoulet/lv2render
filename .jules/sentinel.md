## 2024-03-27 - [Input Validation and Resource Limits]
**Vulnerability:** Missing input validation for plugin parameters (allowing NaN/Infinity) and lack of resource limits (unbounded block size and drain time).
**Learning:** CLI applications that process external data or use plugin systems must strictly validate all user-provided inputs, including numeric parameters, to prevent unexpected behavior or resource exhaustion.
**Prevention:** Implement strict range checks and sanity checks for all CLI arguments and configuration file inputs.
