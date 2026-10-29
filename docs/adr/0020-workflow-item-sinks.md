# ADR 0020: Durable workflow item exports

Status: accepted for the desktop workflow implementation

A requested `sinks.items` export writes extracted site fields beside the downloads. It must not lose or duplicate records when enqueue or append succeeds immediately before a crash. An existing user's file must never become an output merely because it has the expected name.

The plan hash covers the sink request. Approval facts disclose JSONL output, the generated filename, extracted-field contents, record-mode URL redaction and a 64 MiB total bound. The sink has a 1 MiB per-record bound and an 8 MiB redacted metadata bound per atomic discovery document. Exceeding the document bound pauses before committing any of that document’s items. URL redaction occurs before metadata enters the durable discovery intent. Extracted text remains untrusted data; reports and agent responses contain only progress.

Discovery commits metadata and a monotonic ordinal with its checkpoint and job intent. Successful admission or skip atomically creates one outcome and removes the intent. A recovered engine admission resolves the same intent once. Batches flush after 128 outcomes, about 1 MiB of field data or the end of the admission outbox. This bounds per-batch state without rewriting the growing export file.

The append receipt binds retained root, destination and file identities, the committed byte offset, record count and BLAKE3. Output creation is exclusive. Recovery verifies the complete bounded committed prefix and requires any additional bytes to exactly match a prefix of the pending batch. It appends only missing bytes, syncs the file, then commits the cursor and clears outcomes. It never truncates or replaces data. A crash between initial exclusive creation and the first identity receipt fails closed; the person must preserve or move that uncertain file before retry.

Current run state, exact approval/hash, requester activity, session authority and kill switch are checked before filesystem creation, between bounded prefix reads, immediately before append and before committing the receipt. Pause, cancel or revocation leaves outcomes durable for a later authorized retry. Directory/file identity changes, modified bytes, capacity exhaustion and storage failures preserve existing files and pause the run.

Windows denies other write/delete opens while the append handle is retained. Unix retained capabilities provide containment, not isolation from another process running as the same user. A concurrent Unix writer can change an already-hashed prefix after the metadata check while this append changes mtime; the returned digest then represents expected logical bytes, not an atomic snapshot of the final file. The next prefix verification detects ordinary changes. No atomic isolation claim is made. The five-second hash deadline is cooperative between reads and cannot interrupt a blocked filesystem call. Windows directory entry sync has no portable power-loss guarantee.

Regression coverage includes all partial-tail crash positions, complete-tail replay, same-size/same-mtime prefix edits, replaced file/directory identities, unrelated initial files, atomic admission/skip outcomes, restart, URL canaries, ordinal recovery and authority loss before append. Rendered pages and continuation reviews are separate changes.
