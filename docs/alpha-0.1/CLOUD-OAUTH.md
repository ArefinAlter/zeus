# Native cloud sign-in

Zeus can connect explicitly registered Google Drive, Yandex Disk and Baidu Netdisk applications without asking the person to obtain and paste short-lived access tokens. Run these commands from the installed, trusted CLI on the desktop that owns the daemon:

```text
zeus cloud auth login
zeus cloud auth status research
zeus cloud auth refresh research
zeus cloud auth disconnect research
zeus cloud auth disconnect research --revoke-remote
```

Each change opens the controlling terminal, reviews the selected account and asks for confirmation. App passwords use hidden terminal input. Secrets are never accepted through flags, redirected stdin or agent calls. The sign-in command prints a private provider URL directly to that terminal; open it in the system browser. It never reads an existing browser profile or discovers an account automatically. A provider application registration and the account owner's consent are required. They are not supplied by a successful build.

| Provider | Application setup | Implemented flow | Lifecycle |
| --- | --- | --- | --- |
| Google Drive | Register a native **Desktop app** and enable Drive API. Enter its client ID and any issued app password. | PKCE S256 and state, with an ephemeral `http://127.0.0.1:<port>/oauth/callback` receiver owned by the CLI. Requests offline access and fresh consent. | Refreshes near expiry; replaces rotated refresh tokens. Remote revocation is optional and applies to the app/project's granted scopes, potentially affecting other connections. |
| Yandex Disk | Register the app with the required Disk permissions and an exact `http://127.0.0.1:<port>/oauth/callback` URI. Enter its ID and app password. Run `zeus cloud auth login --redirect-port <port>`, or enter that registered port when prompted. | PKCE S256 and state, with the configured loopback port and a stable account device ID/name. | Refreshes near expiry. Optional remote revocation targets this device token. |
| Baidu Netdisk | Register an eligible application with Netdisk access. Enter its application key/ID and app password. | Official device-code endpoint; the person visits its verification page and enters the displayed user code. Polling enforces the provider interval, expiry and slow-down responses. | Refreshes near expiry. Remote revocation is unavailable in this implementation; revoke access on the provider's account page when required. |
| rclone remote | Authorize a dedicated protected rclone config using rclone's own supported provider setup. | `zeus cloud accounts configure`; exact executable/config hashes are pinned. | Authorization remains managed by that selected rclone configuration. Native Zeus OAuth does not parse or change it. |

Google defaults to `drive.file`. This scope covers files/folders the app is authorized to use; entering an arbitrary existing folder ID does not grant access to it. A broader full-Drive choice is explicit and can require provider verification. Yandex requests Disk read/write, and Baidu requests `basic netdisk`. The provider consent page shows the final permissions. Provider-side app verification, test-user restrictions, application eligibility, account quotas and refresh issuance remain external acceptance gates.

The CLI reviews the exact destination root and maximum bytes per delivery before starting sign-in. Completing OAuth creates or updates that configured scope at its reviewed revision. It does not approve an upload. Each delivery still requires its own content/destination approval. Refreshing the same authorized account leaves its scope revision unchanged. Credentials are resolved again after staged source preparation, immediately before upload. An upload that itself outlasts a provider access token can still require the adapter's checkpoint reconciliation and a reviewed retry; token refresh never authorizes blind replay of a remote write. Replacing its registration, account credential or destination goes through configuration and increments the revision, invalidating prior plans.

Access tokens, refresh tokens and optional application passwords are stored together in the selected OS keyring. New key names bind the durable journal store identity and account alias, so independent installations/state stores cannot overwrite or delete each other's same-named accounts. Existing stored key references remain readable. OAuth account records contain only the application ID/scopes, device identity, scope revision and cleanup/dispatch state. Public account/status views, logs and audit records exclude tokens, polling credentials and authorization URLs. If the keyring refuses the credential, account authority is not published; Zeus does not fall back to a plaintext file.

A browser callback must have the exact loopback host, port, path and state. The listener accepts one bound response and ignores unrelated requests. Ctrl-C cancels sign-in; unfinished flows expire after at most ten minutes and do not survive a daemon restart. Codes are exchanged at most once. A lost exchange response requires a new sign-in.

Regular cloud work refreshes credentials shortly before expiry under a per-account lock. An expired access token with usable refresh material reports `refresh_required`, rather than asking for a new sign-in. Before dispatch, Zeus persists refresh uncertainty because providers can rotate one-use refresh material. A crash, lost response or failed keyring replacement never causes an automatic replay of that potentially consumed credential. Status reports `sign_in_required`; reconnect through a newly reviewed sign-in. Explicit refresh also has a durable intent receipt, so retrying that same intent does not dispatch another refresh.

Disconnect disables local authority and stops local workers first. It separately reports confirmed, unsupported or uncertain remote revocation. It cannot stop a provider operation that lacks cancellation. Failed keyring deletion stays recorded as `local_cleanup_pending`; repeat a newly reviewed local disconnect once the keyring is available to retry cleanup. A saved remote result remains stable on same-intent retry, including interruption uncertainty.

Hermetic tests cover the HTTP contracts, PKCE vector, callback forgery/rebinding, cancellation, token redaction, keyring failure, concurrent refresh, restart uncertainty and receipt replay. They use fake credentials and isolated loopback servers. Live provider sign-in and a real OS keyring credential round trip require explicit account-owner acceptance and are not claimed by those tests.

Provider contracts reviewed on 2026-10-28:

- [Google installed application OAuth](https://developers.google.com/identity/protocols/oauth2/native-app).
- [Yandex authorization codes and PKCE](https://www.yandex.com/dev/id/doc/en/codes/code-url), [refresh](https://www.yandex.com/dev/id/doc/en/tokens/refresh-client), and [device token revocation](https://yandex.com/dev/id/doc/en/tokens/token-invalidate).
- [Baidu's official SDK authorization methods](https://github.com/baidu-netdisk/mcp/blob/main/src/baidu-netdisk/openapi_client/api/auth_api.py) and [OAuth documentation](https://openauth.baidu.com/doc/doc.html).

## Development verification, 28 October 2026

Windows x86-64 MSVC checks on the coordinated alpha tree passed:

| Check | Result |
| --- | --- |
| `cargo test --release -p zeus-cloud --all-features` | 26 unit and 6 subprocess tests |
| `cargo test --release -p zeus-api` | 89 unit and 8 IPC tests |
| `cargo test --release -p zeus-daemon cloud::tests` | 31 tests, including 11 OAuth lifecycle cases |
| `cargo test --release -p zeus-cli cloud_oauth::tests` | 2 callback/loopback tests |
| Strict release Clippy for cloud/API/daemon/CLI, all targets/features | Passed with warnings denied |
| Generated API and normative cloud/inbox schema synchronization | Passed |
| Desktop and extension TypeScript checks; extension contract checks | Passed |
| Extension conformance | 30 tests |

These are development protocol/contract results. The full installed CLI/desktop trust channel, real provider applications/accounts and actual OS-keyring credential round trips remain separate acceptance gates. Subsequent roadmap changes require their own regression checks.
