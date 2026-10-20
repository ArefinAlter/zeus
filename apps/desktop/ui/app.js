import { uniqueLinks, rowId, filterRows, reconcileSelection, canControl, formatBytes } from "./model.mjs";

const $ = id => document.getElementById(id);
const invoke = window.__TAURI__?.core?.invoke;
const views = {
  jobs: ["Downloads", "Durable progress, current state and the files coming home.", "Selected controls apply only when every selected download supports the action. Cancel deletes unfinished partial data; completed files remain."],
  inbox: ["Link inbox", "Collect links, preview the destinations, then start the reviewed batch.", ""],
  runs: ["Workflow runs", "Reviewed plans, discovery progress and download outcomes.", "Workflow planning and controls are available through the Zeus CLI and MCP tools. Counts are the daemon's durable report fields."],
  files: ["Files", "Files visible inside your configured default write root.", "This page lists files in the default root. It does not scan your device or build a search index. File operations remain available through the Zeus CLI."],
  artifacts: ["Artifacts", "Content identity, integrity and provenance identifiers.", "Artifact metadata is reduced to identity and integrity facts for this view. Use `zeus artifacts get` and `zeus artifacts lineage` for complete receipts."],
  approvals: ["Approvals", "Requests raised by this restricted desktop identity.", "Review requests with `zeus approvals list` and the interactive CLI approval flow. This desktop client cannot decide approvals. The daemon-wide pending count can include requests outside this identity's view."],
};
const columns = {
  jobs: [["filename", "File / source"], ["state", "State"], ["bytes_done", "Committed"], ["bytes_total", "Total"], ["rate_ewma", "Rate"], ["source_kind", "Requester"]],
  runs: [["run_id", "Run"], ["status", "State"], ["pages_fetched", "Pages"], ["files_completed", "Completed"], ["files_failed", "Failed"], ["bytes_done", "Committed"]],
  files: [["path", "File"], ["size_bytes", "Size"], ["modified_at", "Modified"], ["quarantined", "Quarantined"]],
  artifacts: [["name", "Artifact"], ["state", "Record"], ["size_bytes", "Size"], ["media_type", "Type"], ["quarantined", "Quarantined"]],
  approvals: [["id", "Approval"], ["kind", "Kind"], ["state", "State"], ["expires_at", "Expires"]],
};
const state = { view: "jobs", connected: false, busy: false, refreshing: false, generation: null, snapshot: null, rows: [], selected: new Set(), focused: null, cursor: null, next: null, review: null, reviewKey: null, keys: new Map(), viewRevision: 0 };

function message(text, failed = false) {
  $("notice").textContent = text;
  $("notice").hidden = !text;
  $("notice").classList.toggle("failed", failed);
}

function errorText(error) {
  return error && typeof error.message === "string" ? error.message : "The local connection is unavailable. Start the Zeus daemon, then reconnect.";
}

function authority(permission) {
  return state.connected && !state.busy && state.snapshot?.permissions.includes(permission) && state.snapshot?.capabilities.includes("idempotency");
}

function updateActions() {
  const selectedRows = state.rows.filter(row => state.selected.has(rowId(state.view, row)));
  $("selection-count").textContent = `${selectedRows.length} selected`;
  document.querySelectorAll("[data-action]").forEach(button => {
    button.disabled = !authority("jobs.control") || !canControl(button.dataset.action, selectedRows);
    button.title = button.disabled ? "Connect, then select downloads that all support this action." : "";
  });
  $("preview").disabled = !authority("jobs.write") || !uniqueLinks($("links").value).length;
  $("enqueue").disabled = !authority("jobs.write") || !state.review?.ready;
  $("links").disabled = state.busy;
  $("destination").disabled = state.busy;
  $("next-page").disabled = !state.connected || state.busy || !state.next;
  $("first-page").disabled = !state.connected || state.busy || !state.cursor;
}

function cellValue(key, value) {
  if (["bytes_done", "bytes_total", "size_bytes"].includes(key)) return formatBytes(value);
  if (key === "rate_ewma") return value ? `${formatBytes(value)}/s` : "—";
  if (typeof value === "boolean") return value ? "Yes" : "No";
  if (value === null || value === undefined || value === "") return "—";
  if (typeof value === "object") return JSON.stringify(value, null, 2);
  return ["state", "status", "source_kind", "kind"].includes(key) ? String(value).replaceAll("_", " ") : String(value);
}

function detail(row) {
  state.focused = rowId(state.view, row);
  $("detail-title").textContent = row.filename || row.name || row.path || row.run_id || row.id || "Item";
  $("detail").replaceChildren();
  for (const [key, value] of Object.entries(row)) {
    if (value === null || value === undefined) continue;
    const term = document.createElement("dt"); term.textContent = key.replaceAll("_", " ");
    const definition = document.createElement("dd"); definition.textContent = cellValue(key, value);
    $("detail").append(term, definition);
  }
}

function renderRows() {
  if (state.view === "inbox") return;
  const focusKey = document.activeElement?.dataset?.focusKey;
  const visible = filterRows(state.rows, $("search").value);
  const row = document.createElement("tr");
  if (state.view === "jobs") { const th = document.createElement("th"); th.textContent = "Select"; row.append(th); }
  for (const [, title] of columns[state.view]) { const th = document.createElement("th"); th.scope = "col"; th.textContent = title; row.append(th); }
  $("table-head").replaceChildren(row);
  $("rows").replaceChildren();
  for (const item of visible) {
    const tr = document.createElement("tr");
    const id = rowId(state.view, item);
    tr.classList.toggle("selected", state.selected.has(id));
    if (state.view === "jobs") {
      const td = document.createElement("td"); const checkbox = document.createElement("input");
      checkbox.type = "checkbox"; checkbox.checked = state.selected.has(id);
      checkbox.dataset.focusKey = `select:${id}`;
      checkbox.setAttribute("aria-label", `Select ${item.filename || item.id}`);
      checkbox.addEventListener("change", () => { checkbox.checked ? state.selected.add(id) : state.selected.delete(id); tr.classList.toggle("selected", checkbox.checked); updateActions(); });
      td.append(checkbox); tr.append(td);
    }
    columns[state.view].forEach(([key], index) => {
      const td = document.createElement("td");
      if (index === 0) {
        const button = document.createElement("button"); button.className = "row-name";
        button.dataset.focusKey = `inspect:${id}`;
        button.textContent = item[key] || item.url_redacted || item.id || item.run_id || "Unnamed";
        button.addEventListener("click", () => detail(item)); td.append(button);
        if (state.view === "jobs") { const source = document.createElement("small"); source.textContent = item.url_redacted; td.append(source); }
      } else {
        td.textContent = cellValue(key, item[key]);
        if (key === "state" || key === "status") td.className = `state state-${item[key]}`;
      }
      tr.append(td);
    });
    $("rows").append(tr);
  }
  $("count").textContent = `${visible.length} of ${state.rows.length} loaded records`;
  $("empty").hidden = visible.length > 0;
  $("empty").textContent = state.connected ? (state.rows.length ? "No records match this page filter." : "No records on this page.") : "Connect to load this workspace.";
  const focused = state.rows.find(item => rowId(state.view, item) === state.focused);
  if (focused) detail(focused);
  else { $("detail-title").textContent = "Choose an item"; $("detail").replaceChildren(); state.focused = null; }
  if (focusKey) [...$("rows").querySelectorAll("[data-focus-key]")].find(element => element.dataset.focusKey === focusKey)?.focus({ preventScroll: true });
  updateActions();
}

function unavailable(view, snapshot) {
  const requirements = { jobs: ["jobs.read"], runs: ["workflows.plan", "workflows"], files: ["files.read"], artifacts: ["files.read", "artifacts"], approvals: ["approvals.read"], inbox: ["jobs.write", "idempotency"] };
  const [permission, capability] = requirements[view];
  if (!snapshot.permissions.includes(permission)) return "This desktop identity does not have permission to open this page.";
  if (capability && !snapshot.capabilities.includes(capability)) return "This daemon does not offer the feature required for this page.";
  return null;
}

function renderHealth(snapshot) {
  $("journal").textContent = snapshot.journal_ok ? "Writable" : "Needs attention";
  $("browser").textContent = snapshot.extension_connected ? "Connected" : "Not connected";
  $("pending").textContent = String(snapshot.pending_approvals);
  $("version").textContent = snapshot.daemon_version;
  $("sequence").textContent = `Updated ${new Date().toLocaleTimeString()}`;
  $("connection").textContent = snapshot.kill_switch_engaged ? "Connected · stop switch engaged" : "Connected to your local daemon";
  $("connection-dot").classList.add("online");
}

async function refresh() {
  if (!invoke || state.refreshing || state.busy) return;
  state.refreshing = true;
  const revision = state.viewRevision;
  const view = state.view;
  try {
    const snapshot = await invoke("workspace_snapshot");
    if (state.generation !== null && snapshot.generation !== state.generation) invalidateReview();
    state.generation = snapshot.generation;
    state.snapshot = snapshot;
    state.connected = true;
    renderHealth(snapshot);
    const reason = unavailable(view, snapshot);
    if (reason) {
      if (revision === state.viewRevision) { state.rows = []; state.next = null; renderRows(); message(reason); }
      return;
    }
    if (view !== "inbox") {
      const page = await invoke("workspace_page", { section: view, cursor: state.cursor });
      if (revision !== state.viewRevision) return;
      if (page.generation !== state.generation) throw { message: "The daemon connection changed. Refresh to load a current page." };
      state.rows = page.items; state.next = page.next_cursor;
      for (const [identity, attempt] of view === "jobs" ? state.keys : []) {
        const current = state.rows.find(row => row.id === attempt.jobId);
        if (!current || `${current.updated_at}:${current.state}` !== attempt.fingerprint) state.keys.delete(identity);
      }
      state.selected = reconcileSelection(view, state.selected, state.rows);
      renderRows();
    }
  } catch (error) {
    state.connected = false;
    $("connection").textContent = "Connection unavailable · changes disabled";
    $("connection-dot").classList.remove("online");
    message(errorText(error), true);
  } finally {
    state.refreshing = false;
    updateActions();
    if (revision !== state.viewRevision) queueMicrotask(refresh);
  }
}

function navigate(view) {
  if (!views[view] || state.busy) return;
  state.view = view; state.cursor = null; state.next = null; state.rows = []; state.selected.clear(); state.focused = null; state.viewRevision++;
  document.querySelectorAll("[data-view]").forEach(button => { if (button.dataset.view === view) button.setAttribute("aria-current", "page"); else button.removeAttribute("aria-current"); });
  $("title").textContent = views[view][0]; $("description").textContent = views[view][1]; $("page-note").textContent = views[view][2];
  $("inbox-panel").hidden = view !== "inbox"; $("list-panel").hidden = view === "inbox"; $("job-actions").hidden = view !== "jobs";
  $("search").value = ""; message(""); renderRows(); updateActions(); refresh();
}

function invalidateReview() {
  state.review = null; state.reviewKey = null;
  $("review-items").replaceChildren(); $("review-description").textContent = "Preview the current links before starting them.";
  $("link-count").textContent = `${uniqueLinks($("links").value).length} unique links`;
  updateActions();
}

async function preview() {
  if (!authority("jobs.write")) return;
  state.busy = true; updateActions(); message("");
  try {
    const result = await invoke("preview_links", { urls: uniqueLinks($("links").value), destination: $("destination").value || null, generation: state.generation });
    state.review = result; state.reviewKey = crypto.randomUUID();
    $("review-items").replaceChildren();
    for (const item of result.items) {
      const article = document.createElement("article"); article.className = "review-item";
      for (const [label, value] of [["Source", item.url_redacted], ["File", item.filename], ["Destination", item.destination], ["Approval", item.approval_needed ? "Human review required" : "No approval indicated"], ["Findings", item.error_code || item.warnings?.join(", ")]]) {
        if (!value) continue;
        const line = document.createElement("p"); const name = document.createElement("strong"); name.textContent = `${label}: `;
        line.append(name, document.createTextNode(String(value))); article.append(line);
      }
      $("review-items").append(article);
    }
    $("review-description").textContent = result.ready ? `${result.items.length} downloads reviewed. This preview expires after 10 minutes or a reconnect.` : "The daemon rejected part of this batch. Correct the input and preview again.";
  } catch (error) { state.connected = false; message(errorText(error), true); }
  finally { state.busy = false; updateActions(); }
}

async function enqueue() {
  if (!authority("jobs.write") || !state.review?.ready) return;
  state.busy = true; updateActions(); message("");
  try {
    const receipt = await invoke("enqueue_review", { reviewId: state.review.review_id, idempotencyKey: state.reviewKey, generation: state.generation });
    $("receipt-card").hidden = false; $("receipt").textContent = JSON.stringify(receipt, null, 2);
    const rejected = receipt.jobs.filter(job => job.error_code).length;
    message(rejected ? `${receipt.jobs.length - rejected} downloads accepted; ${rejected} rejected. Review the receipt before submitting another batch.` : `${receipt.jobs.length} downloads accepted. Their current state is on Downloads.`);
    state.review = null; state.reviewKey = null; $("links").value = ""; invalidateReview();
  } catch (error) { state.connected = false; message(`${errorText(error)} The current review keeps its retry key.`, true); }
  finally { state.busy = false; updateActions(); }
}

async function control(action) {
  const selected = state.rows.filter(row => state.selected.has(row.id));
  if (!authority("jobs.control") || !canControl(action, selected)) return;
  state.busy = true; updateActions(); const receipts = [];
  try {
    if (action === "cancel") {
      const dialog = $("cancel-dialog");
      $("cancel-description").textContent = `${selected.length} unfinished download(s) will stop and their partial data will be deleted. Completed files remain.`;
      const confirmed = new Promise(resolve => dialog.addEventListener("close", () => resolve(dialog.returnValue === "confirm"), { once: true }));
      dialog.returnValue = "back";
      dialog.showModal();
      if (!await confirmed) return;
    }
    for (const row of selected) {
      const identity = `${row.id}:${action}`;
      if (!state.keys.has(identity)) state.keys.set(identity, { key: crypto.randomUUID(), jobId: row.id, fingerprint: `${row.updated_at}:${row.state}` });
      const receipt = await invoke("control_job", { jobId: row.id, action, idempotencyKey: state.keys.get(identity).key, generation: state.generation });
      receipts.push(receipt); state.keys.delete(identity);
    }
    message(`${receipts.length} download(s) updated.`);
  } catch (error) { state.connected = false; message(`${receipts.length} download(s) updated before the operation stopped. ${errorText(error)}`, true); }
  finally { state.busy = false; updateActions(); refresh(); }
}

document.querySelectorAll("[data-view]").forEach(button => button.addEventListener("click", () => navigate(button.dataset.view)));
document.querySelectorAll("[data-action]").forEach(button => button.addEventListener("click", () => control(button.dataset.action)));
$("add-links").addEventListener("click", () => { navigate("inbox"); $("links").focus(); });
$("search").addEventListener("input", renderRows);
$("refresh").addEventListener("click", refresh);
$("links").addEventListener("input", invalidateReview);
$("destination").addEventListener("input", invalidateReview);
$("preview").addEventListener("click", preview);
$("enqueue").addEventListener("click", enqueue);
$("next-page").addEventListener("click", () => { state.cursor = state.next; state.selected.clear(); state.viewRevision++; refresh(); });
$("first-page").addEventListener("click", () => { state.cursor = null; state.selected.clear(); state.viewRevision++; refresh(); });
document.addEventListener("visibilitychange", () => { if (!document.hidden) refresh(); });
if (!invoke) message("Open this workspace through the Zeus desktop executable. A regular browser has no daemon bridge.", true);
else { refresh(); setInterval(() => { if (!document.hidden) refresh(); }, 2000); }
updateActions();
