# AGENTS.md

## Code Review Rules

- V0 proofs are frozen historical artifacts retained for posterity only.
- Never propose modifying V0 proof generation or verification.
- Do not report unchanged V0 behavior as a finding or blocker in current pull requests.
- Review proof changes against V1 and later unless the user explicitly requests legacy analysis.

## Versioned Implementations

- Never conflate V0 and V1 implementations. If an operation has two versions, give each version its own function (for example, `operation_v0` and `operation_v1`) and select between them in a version dispatcher.
- Keep version-specific behavior out of shared implementation bodies so changes to one version cannot alter the other version's historical behavior.
