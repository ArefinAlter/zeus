# 09 — Model providers, privacy and spend

Status: Draft 1, 2026-10-16. Implements the decisions in [ADR 0008](../adr/0008-byok-provider-ladder.md) and [model architecture](../ARCHITECTURE.md). The trust boundary, actor definitions, secret handling and human authorization rules are those of [00](00-conventions.md), [05](05-daemon-api-v1.md) and [07](07-approvals-trust-surface-and-consent.md).

## 1. Purpose and authority

A model MAY suggest a plan, classify metadata, suggest a filename or extract fields. Its output MUST NOT itself authorize a download, session grant, write, conversion, shell command or payment. Workflow execution MUST pass the ordinary validation, dry run, budgets and human approval in [08](08-workflow-plan-v0.md). Extracted page/file text and all model output are untrusted data. Model adapters MUST NOT register tools or execute tool calls.

A new installation MUST have model access disabled. Configuring a provider MUST NOT send a probe, discover models, download a model or otherwise incur inference charges. Configuration MUST identify the intended provider, endpoint and model explicitly. Configuration and consent are distinct: stored credentials do not authorize content sharing.

`ModelRuntime` is the daemon entry point. Low-level `LlmProvider::complete` is an adapter interface whose caller is responsible for runtime consent and accounting; production frontends MUST NOT call it directly.

## 2. Provider selection

The first configured, permitted rung wins:

| Rung | Provider | Current adapter |
|---|---|---|
| 1 | Host agent | Supplied by the embedding MCP host; Zeus performs no independent inference. |
| 2 | MCP sampling | Supplied by the connected host only when supported; never a requirement for background execution. |
| 3 | Sign in with ChatGPT | Dynamic OSS registration, validated OIDC identity, separate account registrations, serialized refresh and streamed public Responses API. |
| 4 | User's explicitly configured CLI | Reserved provider kind; constrained process runner remains required before enabling. |
| 5 | OpenRouter PKCE | Loopback S256 sign-in and compatible chat adapter. |
| 6 | User API keys | Native OpenAI Responses, OpenAI-compatible chat, Anthropic Messages. |
| 6 | Local models | Ollama chat, llama.cpp and LM Studio compatible chat. |

A selected request MUST NOT silently fall back or retry after dispatch: a timeout may already have incurred cost. The user MAY explicitly issue a new request, with a new reservation. Provider/model replacement MUST be visible in configuration.

The OpenAI adapter uses `/v1/responses`, `store:false`, a bounded `max_output_tokens` and text aggregated across all message items. It MUST reject incomplete/refused responses. Compatible chat uses `max_tokens`; compatibility is explicit rather than inferred from a server name. Anthropic uses the user's API key, the `2023-06-01` API version, top-level system instructions and `/v1/messages`. Ollama uses `/api/chat`, `stream:false` and `options.num_predict`.

Adapters MUST bound requests to 128 messages and 256 KiB of message text, responses to 2 MiB, and generation to 65,536 tokens. Deadlines are 1–300 seconds, with no redirect following. Error messages MUST NOT include HTTP bodies, request URLs containing secrets or credentials. Remote endpoints MUST use HTTPS; literal loopback HTTP is permitted for explicitly configured local services and hermetic tests. A local provider MUST use a literal loopback address and MUST NOT use a configured Ollama cloud model. A user-controlled local service can itself proxy remotely; this residual trust is outside network address validation and MUST be clear in local-provider setup.

## 3. Context and privacy

The `metadata_only` default permits names, sizes, types, redacted URLs and structural metadata. It MUST NOT include document bytes, extracted text, page bodies, cookies, credentials or authorization headers. `local_only` applies the same content restriction and MUST select only a local provider; host agents and sampling are excluded because their inference locality is not known. `content` permits limited excerpts only after explicit consent for the particular task.

Callers MUST declare whether a request contains file/page content. `RequestAuthorization` binds that declaration, task-specific content consent and the maximum request spend to the call. A content-bearing request requires both `privacy=content` and `content_opt_in=true`. Setting a global privacy preference MUST NOT manufacture task consent. Prompt builders MUST derive these declarations from typed inputs, not from model statements.

Workflow drafting is the first production caller (05 section 9.7, `workflows.plan` with `draft`). A person at a verified terminal sets the request ceiling. The prompt carries the person's goal and text-free outlines of the page samples: element names, ids, classes, attribute names and link shapes (no query, fragment or file name), JSON key paths with value types, never text nodes or values. It is therefore declared `contains_content: false` under `metadata_only`. The response schema is exactly the plan's `extract` member, so an answer that adds budgets, domains or sinks fails validation (`llm.schema_violation`), and the daemon assembles the rest of the plan from fixed conservative defaults. Content excerpts for drafting are not offered.

Every tier applies secret redaction before dispatch. URL userinfo, query strings and fragments, sensitive header values and all bearer tokens are removed. Redaction is a secondary defense, not permission to include arbitrary credential stores in a prompt. Prompt and response bodies MUST NOT be persisted in the model-state database or included in audit events. No provider adapter may read another application's token store or extract consumer subscription tokens.

Structured response schemas MUST be locally validated before dispatch and after parsing. External schema resolution is forbidden; schema size is at most 64 KiB and nesting at most 32. A malformed, truncated, refused or nonconforming result is `schema_violation`, never a partially accepted plan.

## 4. Authentication

Secrets MUST live in a `SecretStore`. The production backend uses Windows Credential Manager, macOS Keychain or Secret Service through `keyring`. Missing entries are distinct from backend failures; unavailable keyrings fail closed. Values and debug output MUST be redacted. Plaintext configuration MUST NOT contain keys or tokens. An encrypted-file fallback is not yet implemented and MUST NOT silently degrade to plaintext.

OpenRouter sign-in MUST bind an OS-selected loopback port, generate an S256 verifier, independent random state and random callback path, and expire after five minutes. The callback MUST be HTTP GET with the expected Host/path, a single matching state and a single nonempty code. Foreign origins, duplicate parameters and oversized headers are rejected. The exchange uses the documented `/api/v1/auth/keys` endpoint, a 30-second deadline and no redirects; `code`, `code_verifier` and `code_challenge_method:S256` are submitted once. The resulting API key is saved before showing success. The authorization URL is displayed/opened only by a trusted human frontend; the daemon never launches a browser spontaneously.

Official OpenAI documentation now describes dynamic OSS registration using an installation host ID and `dynamic_agent_client`; the server issues the durable account-bound client ID. The adapter validates the ID token against published signing keys, binds issuer/audience/nonce and the original account subject, retains granted scopes and identity, and rotates each profile token bundle in the keyring. Its single-owner account database and refresh mutex serialize rotating refreshes. Discovery and JWKS URLs are restricted to the configured issuer authority, whose production value is fixed to `https://auth.openai.com`; provider metadata cannot redirect credentials to another host. The installation host UUID and issued client registration are durable before the code exchange. Separate registrations stay separate, even when display emails match. A BYOK Responses adapter is a separate authentication mode.

ChatGPT inference MUST use the public `/v1/responses` endpoint with `store:false` and `stream:true`. A partial text delta is never success: the reader must observe `response.completed`, and must reject failed, incomplete or interrupted streams. Account-specific model listing is an explicit user action. Sign-out attempts remote session revocation, clears local credentials, and reports whether remote revocation was confirmed; no remote confirmation is fabricated after a network failure.

## 5. Spend accounting

All amounts are integer micro-USD. Daily and monthly periods are UTC. Zero caps prohibit paid work. Clock rollback MUST NOT reset totals.

Before network dispatch, `RuntimeStore` atomically and durably charges a conservative ceiling against both caps. Concurrent requests therefore cannot each spend the same remaining balance. The request-specific maximum is checked first. Reservations survive cancellation, ambiguous network failures and process crashes. A successful response can refund only the difference between its reservation and validated usage cost, and only in the original periods; it cannot refund a newer day/month. On startup, abandoned reservation identities are retired while their costs remain charged.

Remote adapters require explicit pricing ceilings and a model input-context bound. The reservation covers the full configured input limit and the requested output limit, including reasoning output tokens. Token costs round upward. Prices are configuration, not hardcoded claims about current vendor pricing. Provider-side billing, rate changes, fees or a dishonest server can exceed configured estimates; monetary caps are exact for the local ledger under those configured ceilings, not a guarantee about a third-party invoice. A response exceeding its quote records the observed overrun and latches model execution off until a human submits explicit adapter pricing through `llm.configure`. That review requires all earlier requests to have settled and never resets accumulated spend; ordinary settings changes do not release the latch. Unknown usage keeps the entire reservation. Provider-side account limits SHOULD complement Zeus's limits.

Spend state MUST be committed with immediate durability before dispatch. A storage failure refuses the request. Changing caps MUST preserve already accumulated spend. Lowering a cap below current spend prevents additional paid requests until a new period or a subsequent explicit configuration change.

## 6. Signed policy

Policy uses DSSE with payload type `application/vnd.zeus.policy+json;version=1` and Ed25519 verification against provisioned public keys. Sequence numbers MUST never decrease. Replaying the same sequence is allowed only for the same payload; a changed payload at the same sequence is rejected. The highest accepted sequence and envelope are stored atomically. Envelope size is at most 256 KiB, with at most 16 signatures.

Schema version, signature, sequence, issue/expiry times and minimum application version are checked. Expiry must follow issue time. `all_models`, `all_remote_models` and provider-name kill switches are enforced before selection. User-disabled providers cannot be re-enabled by remote policy. Expired or absent policy permits explicitly configured API keys and local models; it disables policy-dependent OAuth/CLI rungs. Host-provided execution remains governed by the host connection and privacy tier.

This build deliberately has no production pinned keys. It MUST NOT claim remote policy is trusted until release owners provision real keys. A user cannot replace trusted keys through `llm.configure`; a verified distributor/release mechanism must provision them. Distribution polling, release-key custody and rotation are not implemented by declaring the verification types.

## 7. API and user experience

`llm.status` reports ordered rungs, configured state, policy status/reason, privacy preference and durable daily/monthly spending. It never returns secrets. `llm.configure` is human-only and accepts optional enabled state, adapter configuration, privacy preference, experimental opt-in, spend caps and one provider API key. An adapter declares `kind`, complete method `endpoint`, `dialect`, `model`, `pricing` and `timeout_seconds`; all fields are non-secret. Credentials travel once over the authenticated local pipe into the keyring.

The daemon owns `llm.auth.start`, `llm.auth.status`, `llm.auth.cancel` and `llm.auth.sign_out`. Each requires a verified human channel. Start returns the authorization URL only to the initiating credential; status and cancellation are bound to that credential. Flow URLs/states must never enter logs. Each flow expires after five minutes, at most eight are retained, and dropping/canceling a pending flow closes its listener. Account-picker identities are exposed only on this human auth surface, not the general rung-status method. Sign-in does not automatically enable model execution or select a paid model.

The auth UI MUST show the provider, actual model, destination, privacy tier, request/day/month ceilings and whether policy/keyring setup prevents use. It MUST make metadata-only/local choices easy to find and require task-level confirmation before excerpts leave the machine. ChatGPT account consent, OpenRouter consent and Zeus download/session approval are separate scopes and MUST remain separate controls.

## 8. Verification and research

Hermetic tests cover native Responses payloads and multi-item output, streamed terminal success, Anthropic system/key handling, Ollama locality, redaction, JSON Schema rejection, opaque errors, loopback PKCE exchange, wrong-state rejection, JWT signature/issuer/audience/nonce binding, serialized refresh rotation, keyring generation failures, retryable sign-out cleanup, durable spend/refunds/restart, cost-quote violations and signed-policy rollback/equivocation. They use local mock servers and test-only signing keys. OS credential-store behavior and real vendor/browser sign-in still require platform acceptance tests.

Primary documentation consulted on 2026-10-16:

- [OpenAI text generation and Responses](https://developers.openai.com/api/docs/guides/text).
- [OpenAI OSS registration and sign-in](https://developers.openai.com/siwc/token-sharing-open-source/sign-in) and [account sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions).
- [OpenRouter S256 PKCE flow and loopback callbacks](https://openrouter.ai/docs/guides/overview/auth/oauth).
- [Anthropic API overview](https://platform.claude.com/docs/en/api/overview) and [Messages reference](https://platform.claude.com/docs/en/api/messages/create).
- [Ollama chat endpoint](https://docs.ollama.com/api/chat).

## Open issues

- Complete the constrained CLI delegation provider and real host/sampling adapters at their respective connection boundaries.
- Provision and distribute production policy keys with a documented release rotation procedure; exercise offline expiry and key rollover acceptance tests.
- Add encrypted fallback for platforms without an available keyring, with an explicit unlock flow and no plaintext fallback.
- Validate real Chrome/Edge/Firefox callback UX and OS keyrings on supported platforms; live vendor tests require user-owned accounts and explicit spend authorization.
- Reconcile actual invoices against configured tariff ceilings and expose ambiguous charges to the user without automatically refunding uncertain work.
