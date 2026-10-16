# Local A2A bridge

Start the daemon with `zeus daemon install` and `zeus daemon start`, then run:

```text
zeus a2a --token-file /absolute/private/path/a2a-peers.json --port 4750
```

The token file contains `{"peers":[{"id":"research-assistant","token":"<64 lowercase hex characters from 32 cryptographically random bytes>"}]}`. Give each peer its own token, retain its `id` when rotating a token, and never reuse an IPC credential. Zeus protects this file with owner-only permissions (current user plus SYSTEM on Windows). A2A task ownership and idempotency live in `a2a-index.redb` under the daemon state directory. Only one bridge process can open that index.

The bridge uses a separate `a2a.token` daemon credential with agent authority. Existing installations gain it by rerunning `zeus daemon install`; existing credentials are preserved. This command does not enable an automatic service or expose the daemon over TCP.

Discovery is `GET http://127.0.0.1:4750/.well-known/agent-card.json`. RPC is `POST /a2a` with an exact `Host: 127.0.0.1:4750`, `Authorization: Bearer <peer token>`, `Content-Type: application/a2a+json`, `A2A-Version: 1.0` and `Content-Length`. The initial HTTP transport accepts one request per connection, closes the response, and rejects transfer coding, upgrades, duplicate headers and forwarded hosts. It caps headers at 16 KiB, bodies/responses at 1 MiB, concurrent connections at 32, header time at 5 seconds and body time at 10 seconds. Browser Origins are refused by default. It has no remote bind profile.

## Implemented profile

Methods: `SendMessage`, `GetTask`, `ListTasks`, `CancelTask`. The only advertised skill is `fetch_file`, for one public HTTP(S) URL. It accepts the following `NewJob` fields: `url`, `dest_dir`, `filename`, `expected_sha256`, `expected_size`, `priority`, `overwrite`, `max_connections`. The daemon enforces destination and overwrite policy. Cookies, session grants, free-text execution, caller-supplied ownership and arbitrary RPC forwarding are refused. No natural-language planner, streaming, push notifications, rescue skill, organization or conversion skill is advertised.

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "SendMessage",
  "params": {
    "message": {
      "messageId": "download-2026-001",
      "role": "ROLE_USER",
      "parts": [{"data": {"skill": "fetch_file", "job": {"url": "https://example.org/data.zip"}}}]
    },
    "configuration": {"returnImmediately": true},
    "metadata": {"idempotencyKey": "download-2026-001"}
  }
}
```

Save `result.task.id`; inspect/cancel with `{"id":"<task id>"}` in the respective method's `params`. `ListTasks` supports `contextId`, `status`, `pageSize` (1–100), `pageToken`, `includeArtifacts`, and `historyLength` (history is omitted). Returned cursors bind the peer and filter. `statusTimestampAfter` and multi-turn messages are not supported and return invalid parameters. Clients may reuse a context returned to the same peer.

Retries must retain both message identifier and idempotency key. The bridge commits their payload binding before enqueue, then uses a stable daemon idempotency key. It can recover an interrupted enqueue response after restart without creating another job. A changed payload under the same key is a conflict. Without `returnImmediately: true`, the bridge waits for a terminal/interrupted task up to 60 seconds; an operation that remains active returns an error instead of pretending to have completed. Poll or repeat the identical submission with `returnImmediately` to recover its task identifier.

Tasks always read current daemon state. Completion returns opaque identifiers, verification hashes, size and quarantine state; it never returns local paths, signed URLs, raw bytes or credentials. An approval-required task tells the peer to ask a human to use Zeus. No A2A message can supply that decision.

## Protocol pin and validation

Wire spelling follows the [A2A v1.0.0 specification](https://a2a-protocol.org/v1.0.0/specification/) and [its normative protobuf source](https://raw.githubusercontent.com/a2aproject/A2A/v1.0.0/specification/a2a.proto), retrieved on 2026-10-16. SHA-256 of that source: `4b74c0baa923ae0acb55474e548f1d6e5d3f83b80d757b65f8bf3e99a3c2257f`.

Hermetic tests exercise real framed HTTP submission, authentication before backend calls, Host/Origin/framing/size rejection, peer isolation, restart and lost-response deduplication, cancellation, human-only authority and secret exclusion. External SDK interoperability and all operating-system acceptance tests remain release gates; this implementation is a bounded local profile, not a claim of universal A2A interoperability.
