## Issues
- #30 — feat(crds): add format/length constraints to PoutinePeer CRD schema fields
- #29 — refactor(crds): extract volume-rename helper in defaults.rs for_app
- #16 — test: add proptest property-based serde roundtrip tests to servarr-crds

## Scope
Improve CRD schema robustness and test coverage. This sprint tightens validation on PoutinePeer fields, extracts repeated patterns in schema defaults, and adds property-based serialization tests to catch schema bugs early. All three issues touch `servarr-crds` and share the goal of schema reliability.

## Acceptance Criteria
- [ ] PoutinePeer CRD schema enforces format/length constraints for applicable fields
- [ ] Repeated volume-rename logic extracted into reusable helper
- [ ] Property-based serde roundtrip tests cover servarr-crds types
- [ ] No existing tests regress
- [ ] All clippy warnings fixed

## Implementation Notes
- Start with #30 (schema constraints) to establish baseline
- Refactor helpers in #29 while adding constraints
- #16 (property tests) validates the schema changes don't break serialization
- Shared files: src/crds/poutine.rs, tests/
