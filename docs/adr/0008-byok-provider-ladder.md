# ADR 0008: Bring-your-own-key with a provider ladder

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |
| Specs affected | 09 (planned), 08 (planned) |

## Context

Some features need a language model: drafting workflow selectors, classifying files, suggesting names. Zeus is local-first and free; it must not resell tokens, must not make network calls the user did not cause, and must not become a way to launder consumer subscription credentials.

## Decision

1. **Default: no model access.** With no configuration Zeus calls no model. Metadata only (never file contents, cookies, tokens or signed-URL queries) reaches a model unless the user raises the privacy tier; redaction of secrets is always on.
2. **Ladder**, tried in order, first usable rung wins:
   1. the **host agent over MCP** (no Zeus-side model: the agent does the thinking and returns structured results);
   2. **MCP sampling** when the client supports it (opportunistic only);
   3. **Sign in with ChatGPT** (OpenAI's programme for open-source local apps: loopback redirect OAuth; facts to be verified in spec 09);
   4. **delegation to the user's own CLI** (for example `codex exec`) for narrow single-shot structured tasks;
   5. **OpenRouter** one-click OAuth with PKCE;
   6. a **raw API key** (OpenAI-compatible or Anthropic) or a **local model** (Ollama, llama.cpp, LM Studio).
3. **Never**: extracting, replaying or proxying Claude or Gemini consumer subscription tokens. Experimental or contested providers are individually switchable off through the signed policy manifest.
4. Credentials live in the OS keyring (encrypted file fallback), never in config files, logs or audit records. Spend is accounted in the daemon with hard caps per day and per month.
5. A **signed policy manifest** (DSSE-style envelope, ed25519, a pinned rotating key set, monotonic sequence number, expiry) can enable, mark experimental or disable providers and set a minimum Zeus version, so a provider whose terms change can be switched off without a release. Offline, the last verified manifest keeps applying until it expires, after which only user-configured API keys and local models remain.
6. The Rust-facing contract is the `LlmProvider` trait in `zeus-llm`; the daemon owns routing.

## Consequences

- Zero cost and zero data exposure by default; the strongest rung is whatever the user already pays for.
- Provider terms change quickly: the manifest and per-provider status make that manageable.

## Alternatives considered

- A Zeus-hosted proxy: contradicts local-first and no-telemetry, and creates a liability.
- A single provider: lock-in and a single point of policy failure.
