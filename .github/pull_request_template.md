## What and why

<!-- One or two sentences: the user-visible or contract-visible change, and the reason. -->

## Contract impact

- [ ] No change to a spec, schema, wire type or public API
- [ ] Spec and schema updated in this PR (`docs/spec`), `npm run validate` passes
- [ ] ADR added or updated (`docs/adr`) for a hard-to-reverse decision

## Security checklist

- [ ] No secret, cookie, token or real personal data in code, logs, tests or fixtures
- [ ] No new path from an agent-kind client to approvals, grants or session material
- [ ] Untrusted strings from servers or files stay labelled as data
- [ ] Not applicable (docs or refactor only)

## Verification

- [ ] `cargo fmt --all`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] Tests added or updated

## Known gaps

<!-- Anything that returns NotImplemented or is deliberately deferred. -->

Signed-off-by is present on every commit (DCO).
