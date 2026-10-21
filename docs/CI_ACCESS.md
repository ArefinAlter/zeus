# CI access to private zeus-core

Configured on 2026-10-21 with the user's approval. The app workflow uses a repository-scoped, read-only Ed25519 deploy key instead of a personal access token.

| Setting | Value |
|---|---|
| Repository granting read access | `ArefinAlter/zeus-core` |
| Deploy key | `167128912` — `zeus Actions read-only core checkout 2026-10-21` |
| Public fingerprint | `SHA256:JorAV4ZR1Ip4fUJtLIH9Fg+u2095kEVxpzNzJoWyUgU` |
| Repository storing the private half | `ArefinAlter/zeus` |
| Actions secret | `ZEUS_CORE_SSH_KEY` |
| Workflow | [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) |

GitHub reports `read_only: true`. An isolated SSH probe using only this key, with host-key verification, read core's `main` at `a94655cdef51e452f6f582fb4be6193ec4a1db1c` on port 22. The private key was encrypted into the repository's Actions secret through GitHub CLI stdin, then its temporary local files were deleted. No private key is committed or retained as a local backup.

The ref-resolution job checks out the compatible pinned core revision with `actions/checkout@v5`, then uses its SSH origin to look for the matching branch. It executes only the inline lookup, retains credentials for that job, and relies on checkout's post-job cleanup. A missing branch selects the compatible pin; authentication or transport errors fail the job. Every build/check job uses `persist-credentials: false`, so the checkout credential is removed before repository code runs. All seven private-core checkout steps use the same secret with strict host-key checking. Fork pull requests do not receive the secret.

These settings follow the official [checkout SSH inputs and credential cleanup](https://github.com/actions/checkout/blob/v5/README.md#usage). GitHub documents the [single-repository deploy-key scope and read-only setting](https://docs.github.com/en/authentication/connecting-to-github-with-ssh/managing-deploy-keys), and publishes the [SSH host fingerprints](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints) used for verification.

Rotation replaces this pair: generate a new key, add its public half as a read-only core deploy key, replace the app Actions secret, verify read access, then remove the old deploy key. Update the ID/fingerprint above. To revoke access, remove the core deploy key and delete the app secret. Deploy keys do not expire automatically; deleting the creating user's access is not a substitute for removing this key.

Validation includes live private-repository SSH access, the server's read-only key metadata, all seven checkout configurations, Bash syntax and five real-Git resolver scenarios: main, a matching branch, a missing branch, an explicit ref and an inaccessible remote. Product code is unchanged. Hosted runner results remain separate from this local verification; the previously reported account billing/spending-limit restriction for core/extension Actions is unresolved.
