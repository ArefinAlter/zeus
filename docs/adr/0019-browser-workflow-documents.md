# 0019: Explicit browser authority for workflow documents

Status: accepted and implemented for plain documents on `feature/workflow-browser-pages`; absent from 0.1.0-alpha.1. Installed-browser acceptance remains pending.

The native workflow HTTP transport resolves and pins vetted public peers. An extension fetch uses the browser's network stack and cannot provide equivalent peer pinning. Authenticated document acquisition therefore needs an explicit browser execution mode, separately reviewed by a person, in addition to session authority. Native transport failures never select this mode automatically.

The additive `session.page` capability returns bounded document data through native messaging. A request identifies the exact browser profile, grant, HTTPS origin and owned plan or approved run. Plan sampling requires a live site grant and exact browser-mode review; run execution requires its workflow grant and the browser mode in its reviewed plan facts. A once/job grant cannot authorize a page read. The extension does not export cookies or other credential material for this route.

Both sides reject redirects, IP literals, unqualified/local hostnames and a different profile or subject. The daemon checks allowed domains, public DNS, robots rules and pacing immediately before dispatch. It rechecks requester, approval, grant, original limits and run state while waiting and before accepting a reply. The extension independently checks its own human consent, browser host permission, deadline and body bound, and cancels on revoked access or disconnect. Documents are limited to 512 KiB decoded, a 25-second request deadline, two concurrent reads and a small content-type/header allowlist. Private browsing has no ordinary-profile fallback.

The browser resolves and connects under its own network policy. A daemon preflight cannot prove that the browser subsequently uses the same DNS answer, including after DNS rebinding. The review must say that native peer pinning is unavailable for this mode. This residual boundary is explicit; the native public-page path retains its pinned-peer rules. No browser fetch is presented as equivalent to a native pinned connection.

Browser content remains untrusted data. Request/reply bodies, query strings and credential values are not logged. Raw payloads have private Debug implementations and bounded temporary storage; capability negotiation, subject checks and output bounds are tested independently of page content.

The implementation uses the browser's documented extension-origin fetch and host-permission model. Render/scroll support additionally requires the scripting permission and exact host permission; it must receive its own capability and bounded reviewed effects before it is advertised. [Chrome extension network requests](https://developer.chrome.com/docs/extensions/develop/concepts/network-requests), [Chrome scripting API](https://developer.chrome.com/docs/extensions/reference/api/scripting).

Validation will record unit, protocol, daemon and installed-browser evidence separately. A mocked fetch does not establish that a live subscription or remote website accepts an account.

The established verified interactive CLI/TUI approval path remains available. Initial preview review selects a single connected attested profile; ambiguity fails closed. The normalized plan then stores that exact profile in its hash and approval facts. Workflow grants cannot move to another profile, even if another extension connection also saw the approval. The extension independently accepts terminal-issued consent only for an approval it previously displayed, and separately requires browser host access. Unpacked/development extension identities are not permitted to decide approvals.

Installed-browser acceptance still requires a person to install/connect the extension, grant browser host access, display the approval there, and decide the exact review in the verified terminal. The automated broker and extension fixtures do not establish that this human-installed path has been exercised. No store identity or signing credential is assumed.
