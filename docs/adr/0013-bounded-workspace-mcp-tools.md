# ADR 0013: Bounded workspace tools and one generated MCP catalogue

Date: 2026-10-20
Status: implemented and merged for pre-alpha (2026-10-21); real-host acceptance and API freeze remain open

## Context

Spec 06's ten-tool reference catalogue occupied 23,991 of its 24,000-byte budget. Workflow, organizer and artifact daemon APIs already existed, but agents could not use them through MCP. Independently generated Rust and normative schemas also allowed the published tool definitions to drift.

## Decision

Raise the total compact UTF-8 catalogue budget to 40,960 bytes. Retain the 4,096-byte limit per tool and 320-character descriptions. Append three closed action sets: `zeus_workflow`, `zeus_files`, `zeus_artifacts`. Preserve the original ten names and order. Do not add arbitrary method dispatch, approval decisions, model drafting, conversion, retention or deletion.

Require an intent key on all three grouped tools, including reads. Forward it in the typed parameters of every mutating daemon method; read methods do not receive it. Each daemon service owns durable receipts. Planning publishes its receipt with the draft, and control publishes its receipt with the run checkpoint, so a replay cannot repeat an old pause or resume after a later action. Export binds an immutable snapshot; if a crash interrupts filesystem publication before its receipt completes, the same key fails with `api.conflict` and `reason: export_receipt_incomplete`, requiring inspection before a new export request. It never silently creates another file. Keyed model drafting remains unavailable and is excluded from MCP. Use conservative annotations for the most consequential action in each group. Human authority, ownership, permitted roots, capabilities and budgets remain daemon checks.

Embed the generated normative catalogue in the Rust binary. The Node check verifies the embedded file and stated digest; the Rust test compares actual rmcp definitions with that file and measures the complete wire array, including names, annotations and output schemas. Byte accounting uses UTF-8, not JavaScript string length.

Expose the workflow plan schema as `zeus://workflow/plan-schema`. Plan responses contain its URI, up to 2,048 UTF-8 bytes per page sample, and a truthful truncation flag. All validation still uses the daemon's original samples. Manifest results retain the complete on-disk receipt and omit inline content. Other oversized results fail explicitly at 32 KiB; mutation receipts are recoverable using the same key.

## Consequences

The measured catalogue is 31,473 bytes; its largest definition is 3,379 bytes. Grouping keeps discovery small but conservative annotations can cause a host to confirm harmless reads. Separate tools by risk class later if host acceptance warrants that cost. Do not relax daemon policy to improve host confirmation UX.

The new dependency is on the existing `zeus-workflow` workspace crate for its plan schema, without runtime features. No external dependency was introduced. This does not freeze API v1 or establish acceptance in a real MCP host.
