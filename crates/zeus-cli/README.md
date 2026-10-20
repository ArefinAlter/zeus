# Zeus CLI

URL-list intake validates the complete batch before enqueueing:

```text
zeus get --input-file urls.txt --dry-run
zeus get -i urls.txt --idempotency-key reports-1 --wait
zeus artifacts ls --limit 20 --state active --json
zeus artifacts get art_REPLACE_WITH_ID --json
zeus artifacts lineage art_REPLACE_WITH_ID --direction ancestors --max-depth 8
zeus artifacts ingest /absolute/permitted/root/report.pdf --idempotency-key report-1
```

`urls.txt` is a UTF-8 regular file of at most 1 MiB: one HTTP(S) URL per line, blank lines and `#` comments ignored. Positional URLs come first; exact duplicates are removed without rewriting signed queries. Each request accepts 1–50 unique URLs. Stdin input and recursive mirroring are not implemented.

Artifact queries preserve public records and pagination. `ls` accepts SHA-256/BLAKE3, producing job/run and state filters; pass the returned `next_cursor` as `--cursor` with the same filters. `--ndjson` prints list records. Ingest uses daemon containment and hashing, never moves the source and refuses `--dry-run`; repeat its key only with identical input.

In `zeus tui`, `/` searches loaded jobs by name, redacted URL or state. Enter finishes typing; Escape clears search; Ctrl-C exits. Search text cannot trigger pause/resume/cancel shortcuts. Selection follows job identity when snapshots reorder, and the table scrolls to keep the selected job visible. This is a live queue filter, not a filesystem index.

## Cloud delivery

`zeus cloud accounts configure` opens the controlling terminal to register a destination. The daemon independently verifies the human terminal and pinned CLI executable. Native provider credentials are entered with echo disabled and stored in the OS keyring; there are no secret command-line flags or stdin credential inputs. The setup command does not perform OAuth sign-in or refresh native access credentials. Obtain the appropriate access credential separately, or use an explicitly configured rclone executable, dedicated config file and remote. No account is auto-discovered.

Rclone setup pins the exact executable and dedicated configuration file by SHA-256. The adapter checks those fingerprints before every subprocess launch. A legitimate OAuth refresh that rewrites the configuration file, an executable upgrade, or any other file change causes later launches to refuse the old pins. Zeus does not refresh pins automatically. Run `zeus cloud accounts configure` again in the human terminal, review the intended remote and folder, and save the new account revision. Previous plans and delivery permissions remain bound to their old revision; create and review a new plan when the account revision changes. Native access-credential replacement follows the same reviewed configuration path; automatic native OAuth refresh is not implemented.

```text
zeus cloud accounts configure
zeus cloud accounts list
zeus cloud status
zeus cloud plan https://example.org/report.zip --account research --remote-name report.zip --mode staged_upload --size-bytes 1048576 --max-bytes 1048576 --max-seconds 1800 --idempotency-key research-plan-1
zeus cloud submit clp_REPLACE_WITH_PLAN_ID --plan-hash REPLACE_WITH_RETURNED_HASH --idempotency-key research-submit-1
zeus approvals list
zeus approvals approve ap_REPLACE_WITH_APPROVAL_ID
zeus cloud get cld_REPLACE_WITH_DELIVERY_ID
zeus cloud control cld_REPLACE_WITH_DELIVERY_ID pause
zeus cloud control cld_REPLACE_WITH_DELIVERY_ID resume
zeus cloud control cld_REPLACE_WITH_DELIVERY_ID cancel
zeus cloud accounts revoke research
```

These are syntax examples; replace the source, account and IDs with your reviewed values. `cloud submit` only requests a human approval. The existing terminal approval flow binds the current request hash and facts. Setting `--dry-run` on a mutation refuses the call; use `cloud plan` to preview instead.

The exact modes are `staged_upload`, `stream_upload` and `remote_import`. A completed download can be used with `--job job_...` instead of a URL and `--size-bytes`. The configured account fixes the destination folder; `--remote-name` is a filename, and the returned plan shows the unique `.zeus-<delivery_id>` suffix. Reuse an idempotency key only with identical inputs.

A URL requires its exact size. `--sha256` can bind an independently known source hash. Baidu streaming additionally needs `--block-md5` with comma-separated, precomputed 4 MiB block MD5 values, or `--block-md5-file` with one value per line (up to 8192) for manifests too large for a command line. Read the plan's capability result; Zeus never chooses another mode automatically. Provider URL fetch discloses the full source URL and may require `--allow-unbounded-remote`, acknowledging that the provider can exceed the local cap or continue after cancellation. No browser-session credentials are forwarded.

`cloud list`, `get` and `status` support `--json`; lists also support `--ndjson`. Delivery views distinguish awaiting approval, local execution, remote pending, completion, failure and remote cancellation uncertainty. Completion evidence identifies local/remote SHA-256 comparison or size-only verification. Provider acceptance alone is not completion. Account revocation invalidates local authority but does not promise remote OAuth revocation, deletion or cancellation.

Plan and delivery ownership is tied to the authenticated daemon credential. Processes sharing an agent credential share that owner namespace; it is not isolation between individual assistants using the same credential. A retry key identifies an intent but never renews an expired plan, extends its permission or changes its account revision.

`max_bytes` bounds the delivered object size on each local transfer leg. It is not a total network billing quota: staged and streamed deliveries carry both download and upload traffic, and protocol overhead or resumed attempts add traffic. Status reports provider-confirmed bytes; it does not count every byte read from the source.
