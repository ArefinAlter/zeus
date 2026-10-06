# ADR 0004: Extension-mediated sessions; never read cookie databases

| | |
|---|---|
| Status | Accepted |
| Date | 2026-10-05 |
| Specs affected | 02, 07 (planned), 12 (planned), 01 sections 4 and 5 |

## Context

Many downloads need the user's logged-in session. The known shortcuts are reading the browser's cookie database from disk, extracting cookies through the DevTools protocol, or impersonating browser TLS fingerprints. Each is a technique used by infostealers; tools that do it get flagged by antivirus and distrusted by users. Newer browser features (device-bound session credentials) also make exported cookies useless.

## Decision

1. Session material reaches Zeus **only** through the browser extension over native messaging (spec 02), for **one exact origin**, after a **human consent** recorded in the extension, with a lifetime of at most 15 minutes per lease.
2. Zeus **never** reads a browser cookie store, profile directory, `Login Data`, `Local State` or any browser-owned file, never attaches to a browser debugging port to read cookies, and ships no code that could. The repository has a CI check that greps for the relevant paths and APIs.
3. Cookies are held only in zeroizing memory for the lease, never written to the journal, events, audit log, logs, crash reports or LLM prompts (00 section 8). Grants record consent, not cookies.
4. When the origin refuses the daemon (anti-bot, device-bound sessions) the only recovery is the **in-browser fetch proxy**: the user's own browser fetches the byte ranges. Zeus never rotates identities, spoofs TLS fingerprints, solves captchas or otherwise evades a control (01 section 8.6).
5. Consent scopes are `once`, `site` (24 h), `workflow` (7 days, renewable) and `never` (01 section 5).

## Consequences

- Zeus can honestly say it does not look like, and does not behave like, an infostealer; the extension store review and antivirus story are tractable.
- Headless or browserless hosts have no path to authenticated downloads in the MVP (01 open issue O4).
- The extension is on the critical path for session features, so the native-messaging protocol is a first-class contract.

## Alternatives considered

- Read the cookie DB (what yt-dlp's `--cookies-from-browser` does): rejected for the reasons above; app-bound cookie encryption makes it increasingly fragile anyway.
- Automate a browser with CDP: kept only as a possible later transport (01 open issue O4), never for harvesting cookies.
- Ask the user to paste cookies: rejected, error-prone and encourages leaking them.
