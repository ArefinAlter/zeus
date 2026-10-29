# Cloud delivery providers

This crate implements cloud writes behind explicit credentials and a durable private checkpoint
port. The daemon owns approvals, source URL/DNS policy, complete local verification before staged
upload, operation admission, account revision binding, and the kill switch.

| Provider | Download, verify, upload | Bounded stream | Provider fetches URL | Restart |
| --- | --- | --- | --- | --- |
| Google Drive v3 | Yes | Known size | Unavailable | Staged source can resume after querying confirmed offset |
| Yandex Disk | Yes | Known size | Native asynchronous import | Upload replay unavailable; reconcile unique target/import receipt |
| rclone configured remote | Yes | `rcat --size` | Unavailable | Upload replay unavailable; reconcile unique target |
| Baidu Netdisk | Native multipart; block manifest required | Requires known size and precomputed block hashes | Unavailable | Staged source rehashes and resends only provider-requested parts |

Google uses pre-generated file IDs, saved before session creation. Upload chunks are 1 MiB;
successful range acknowledgements are persisted. A partial acknowledgement stops a forward-only
stream; a caller with the original staged file may explicitly resume and rehash its prefix.

Yandex retrieves a signed upload target without forwarding OAuth to it. URL import retains the
provider's operation receipt, disables source redirects, and polls the returned operation on the
fixed API origin. It does **not** advertise provider-side cancellation or a hard imported-byte cap.
Stopping local polling does not stop Yandex's fetch. An import with an unknown final size must be
handled explicitly by the caller; this implementation requires an expected size to verify it.

Rclone supports the remotes supported by the explicitly configured installed executable, including
MEGA, S3, OneDrive, Dropbox and WebDAV. Feature discovery must succeed before an account is ready.
No shell or `copyurl` is used. It receives a known-size source over stdin and has no source replay
or full-file local fallback. Zeus uses 64 KiB source chunks, caps stdout/stderr at 64 KiB each,
sets bounded rclone buffer/concurrency options, kills/reaps on cancellation, and never returns
subprocess diagnostics. External backend memory is controlled by rclone, not an OS memory sandbox.

Every path-based provider uses `name.zeus-<operation_id>` under its configured existing folder.
The daemon must allocate an unpredictable operation ID once, persist the intent, and serialize
admission for that ID. Rclone checks the target and refuses an existing object or checkpoint replay.
Generic `rcat` has no atomic conditional-create API: an external writer racing that reserved random
path remains a limitation. There is no rename into a pre-existing user-selected name. Cleanup is
restricted to the operation's unique path and does not claim to cancel an active remote import.
Yandex and rclone cleanup also require a provider SHA-256 match to the known source digest; without this
evidence it returns `reconciliation_required`. A separate remote writer replacing an object between
the metadata check and deletion cannot be excluded by either provider's path-based API.

Receipts distinguish observed local SHA-256, provider SHA-256, and provider size. A size match is
never labeled a remote hash match. If the caller requires an expected hash for provider-only import
or recovery and the provider returns none, the operation fails with `remote_hash_unavailable`.
Hash or size failure can leave a partial/object at the provider; the saved checkpoint must remain
available for explicit reconciliation or cleanup.

`PrivateCheckpoint` contains upload or operation URLs and is only for ACL-protected daemon storage;
its Debug output is redacted. It must never be put in public API responses, audit records, model
inputs, or events. Tokens are nonserializable, zeroizing `SecretString` values, obtained explicitly
or through `CredentialStore`; the `os-keyring` feature provides account-scoped keyring access.
The `oauth` module implements native Google/Yandex PKCE authorization, Baidu device authorization,
bounded token refresh and supported remote revocation. The daemon binds these to human-reviewed
account scopes; `zeus cloud auth login` owns the controlling terminal and exact loopback receiver.
Access, refresh and optional app-password material share one private keyring object. Provider app
registration and real account consent remain external setup steps; no ambient credentials are read.
See [native cloud sign-in](../../docs/alpha-0.1/CLOUD-OAUTH.md). Rclone requires a trusted
executable and protected configuration. The daemon pins their hashes at human authorization;
the adapter rechecks configured pins before every subprocess launch. A same-user file swap between
the hash check and process open remains a local trust-boundary limitation. Credentials are not
copied into another persistent plaintext file.

Tests are loopback HTTP fixtures and an actual child-process fixture. Run
`cargo test -p zeus-cloud --all-features`; no account credentials or live cloud requests are used.
These tests prove protocol handling and bounded local behavior, not live provider acceptance.

Provider contracts checked 2026-10-16:

- [Drive resumable uploads](https://developers.google.com/workspace/drive/api/guides/manage-uploads)
  and [pre-generated file IDs](https://developers.google.com/workspace/drive/api/guides/create-file).
- [Yandex's official Java REST API declarations](https://github.com/yandex-disk/yandex-disk-restapi-java/blob/master/disk-restapi-sdk/src/main/java/com/yandex/disk/rest/retrofit/CloudApi.java)
  and [REST API overview](https://yandex.com/dev/disk/rest/).
- [rclone rcat limitations](https://rclone.org/commands/rclone_rcat/),
  [backend discovery](https://rclone.org/commands/rclone_backend/),
  [object metadata](https://rclone.org/commands/rclone_lsjson/), and
  [known-size rcat implementation](https://github.com/rclone/rclone/blob/master/fs/operations/operations.go).
- [Baidu's current pre-upload contract](https://pan.baidu.com/union/doc/基础网盘服务/上传/预上传/).
