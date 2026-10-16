"use strict";
// Local design simulation only: no network calls, cookies, daemon credentials or real decisions.
const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => [...document.querySelectorAll(selector)];
const jobs = [
  {
    id: "sample-1",
    name: "climate-reference-2026.zip",
    site: "data.example.org",
    owner: "Research assistant",
    channel: "MCP · run 03",
    state: "downloading",
    saved: 63,
    size: "1.8 GB",
    rate: "12.4 MB/s",
    eta: "54 sec left",
    queue: "Main",
    destination: "Downloads/Climate reference",
    note: "4 parallel ranges. Saved progress will survive a restart.",
    hash: null,
  },
  {
    id: "sample-2",
    name: "ubuntu-26.04-desktop.iso",
    site: "releases.example.org",
    owner: "You",
    channel: "Browser handoff",
    state: "downloading",
    saved: 38,
    size: "6.2 GB",
    rate: "8.7 MB/s",
    eta: "7 min left",
    queue: "Main",
    destination: "Downloads/Images",
    note: "Rescued after the browser connection was interrupted.",
    hash: null,
  },
  {
    id: "sample-3",
    name: "annual-report.pdf",
    site: "portal.example.org",
    owner: "Research assistant",
    channel: "MCP · run 03",
    state: "awaiting_approval",
    saved: 0,
    size: "18 MB",
    rate: "—",
    eta: "Waiting for you",
    queue: "Main",
    destination: "Downloads/Climate reference",
    note: "Needs your permission to use this website login for one file.",
    hash: null,
  },
  {
    id: "sample-4",
    name: "historical-observations.csv",
    site: "archive.example.org",
    owner: "Archive workflow",
    channel: "Plan · run 02",
    state: "queued",
    saved: 0,
    size: "420 MB",
    rate: "—",
    eta: "Starts 00:30",
    queue: "Night",
    destination: "Downloads/Archive",
    note: "Waiting for the Night queue. The schedule is a design proposal.",
    hash: null,
  },
  {
    id: "sample-5",
    name: "regional-tables.parquet",
    site: "data.example.org",
    owner: "Research assistant",
    channel: "MCP · run 03",
    state: "failed",
    saved: 71,
    size: "760 MB",
    rate: "—",
    eta: "Link expired",
    queue: "Main",
    destination: "Downloads/Climate reference",
    note: "The signed link expired. Request a fresh link from the website, then retry. Saved bytes stay on disk.",
    hash: null,
  },
  {
    id: "sample-6",
    name: "source-notes.pdf",
    site: "research.example.org",
    owner: "You",
    channel: "Pasted link",
    state: "completed",
    saved: 100,
    size: "4.2 MB",
    rate: "—",
    eta: "Verified",
    queue: "Main",
    destination: "Downloads/Climate reference",
    note: "Sample receipt: downloaded, SHA-256 verified, no transformation.",
    hash: "8c27b25e94a0c2aa64ce31ba10adc04a1fc0244b7e7ab16e894e8eb725aa0d63",
  },
];
let currentView = "downloads";
let selected = jobs[0].id;
let filter = "all";
let stopped = false;
let approvalPending = true;
let revision = 1;
let links = [];
let queue = "Main";
let reviewTimer;
let reviewReadyAt = 0;
const checked = new Set();
const stateNames = {
  downloading: "Downloading",
  queued: "Queued",
  paused: "Paused",
  failed: "Link expired",
  completed: "Completed",
  awaiting_approval: "Needs approval",
  canceled: "Canceled",
};
const headings = {
  downloads: [
    "THE DOWNLOAD DESK",
    "Everything in motion.",
    "One place for the files you asked for, and the ones your agents are bringing home.",
  ],
  inbox: [
    "THE LINK INBOX",
    "Collect now. Decide next.",
    "A familiar staging area for pasted links, browser rescues and agent suggestions.",
  ],
  runs: [
    "AGENT ORCHESTRATION",
    "A plan you can inspect.",
    "See the sources, steps, costs and limits before an agent starts moving files.",
  ],
  approvals: [
    "YOUR PERMISSION",
    "The next move is yours.",
    "Clear facts, a named requester, and a decision with a precise scope.",
  ],
  destinations: ["CLOUD DELIVERY", "Choose the path, then the destination.", "Review account scope, source disclosure and verification before a file leaves your machine."],
  queues: [
    "YOUR CONNECTION, YOUR SCHEDULE",
    "Make room for the right work.",
    "Keep urgent files moving, and let large collections wait for a better moment.",
  ],
};
function element(tag, text, cls) {
  const node = document.createElement(tag);
  if (text != null) node.textContent = text;
  if (cls) node.className = cls;
  return node;
}
function message(text) {
  $("#message").textContent = text;
  $("#message").hidden = false;
}
function setView(view) {
  currentView = view;
  $$("[data-panel]").forEach((node) => {
    node.hidden = node.dataset.panel !== view;
  });
  $$("[data-view]").forEach((node) => {
    if (node.dataset.view === view) node.setAttribute("aria-current", "page");
    else node.removeAttribute("aria-current");
  });
  const [eyebrow, title, description] = headings[view];
  $("#page-eyebrow").textContent = eyebrow;
  $("#page-title").textContent = title;
  $("#page-description").textContent = description;
  $("#add-links").hidden = view === "inbox";
  $("#message").hidden = true;
  if (view === "approvals") renderApproval();
  if (view === "destinations") renderCloud();
}
function visibleJobs() {
  const query = $("#search").value.toLowerCase();
  return jobs
    .filter((job) =>
      [job.name, job.site, job.owner, job.queue]
        .join(" ")
        .toLowerCase()
        .includes(query),
    )
    .filter(
      (job) =>
        filter === "all" ||
        (filter === "active" &&
          ["downloading", "queued"].includes(job.state)) ||
        (filter === "attention" &&
          ["failed", "awaiting_approval"].includes(job.state)) ||
        (filter === "completed" && job.state === "completed"),
    );
}
function renderJobs() {
  const body = $("#jobs");
  body.replaceChildren();
  const visible = visibleJobs();
  for (const job of visible) {
    const row = document.createElement("tr");
    row.classList.toggle("selected", job.id === selected);
    const pick = document.createElement("input");
    pick.type = "checkbox";
    pick.checked = checked.has(job.id);
    pick.setAttribute("aria-label", `Select ${job.name}`);
    pick.addEventListener("click", (event) => event.stopPropagation());
    pick.addEventListener("change", () => {
      pick.checked ? checked.add(job.id) : checked.delete(job.id);
      renderSelection();
    });
    const first = document.createElement("td");
    first.append(pick);
    row.append(first);
    const file = document.createElement("td");
    const name = element("strong", job.name);
    file.append(name, element("span", job.site, "file-source"));
    row.append(file);
    const owner = document.createElement("td");
    owner.append(
      element("div", job.owner, "owner"),
      element("small", job.channel, "file-source"),
    );
    row.append(owner);
    const state = document.createElement("td");
    state.append(element("span", stateNames[job.state], `state ${job.state}`));
    row.append(state);
    const saved = element("td", null, "saved");
    const progress = document.createElement("progress");
    progress.max = 100;
    progress.value = job.saved;
    progress.setAttribute("aria-label", `${job.saved}% saved`);
    saved.append(progress, `${job.saved}% · ${job.size}`);
    row.append(saved);
    const speed = element("td", null, "speed");
    speed.append(job.rate, element("small", job.eta));
    row.append(speed);
    row.append(element("td", job.queue));
    row.tabIndex = 0;
    row.setAttribute("aria-label", `${job.name}, ${stateNames[job.state]}`);
    const inspect = () => {
      selected = job.id;
      renderJobs();
    };
    row.addEventListener("click", inspect);
    row.addEventListener("keydown", (event) => {
      if (event.key === "Enter") inspect();
    });
    body.append(row);
  }
  if (!visible.length) {
    const row = document.createElement("tr");
    const cell = element("td", "No downloads match this view.", "no-jobs");
    cell.colSpan = 7;
    row.append(cell);
    body.append(row);
  }
  $("#download-count").textContent = jobs.length;
  $("#active-count").replaceChildren(
    String(jobs.filter((j) => j.state === "downloading").length),
    element("small", " downloads"),
  );
  $("#attention-count").replaceChildren(
    String(
      jobs.filter((j) => ["awaiting_approval", "failed"].includes(j.state))
        .length,
    ),
    element("small", " items"),
  );
  renderSelection();
  renderInspector();
}
function renderSelection() {
  $("#selection-count").textContent = `${checked.size} selected`;
  $$("[data-batch]").forEach((b) => {
    b.disabled =
      checked.size === 0 || (stopped && b.dataset.batch === "resume");
  });
  const visible = visibleJobs();
  $("#select-all").checked =
    visible.length > 0 && visible.every((j) => checked.has(j.id));
}
function renderInspector() {
  const job = jobs.find((j) => j.id === selected);
  if (!job) return;
  const node = $("#inspector");
  node.replaceChildren(
    element("p", "SELECTED DOWNLOAD", "eyebrow"),
    element("h2", job.name),
    element("p", job.note),
  );
  const segment = element("div", null, "segment-map");
  segment.setAttribute(
    "aria-label",
    `${job.saved}% saved (illustrative segment map)`,
  );
  for (let i = 0; i < 12; i++)
    segment.append(
      element(
        "span",
        null,
        i < Math.floor((job.saved / 100) * 12) ? "saved" : "",
      ),
    );
  node.append(segment, element("small", "Saved parts · illustrative", "muted"));
  const facts = document.createElement("dl");
  for (const [label, value] of [
    ["Asked by", `${job.owner} · ${job.channel}`],
    ["Website", job.site],
    ["Save to", job.destination],
    [
      "Permission",
      job.id === "sample-3"
        ? approvalPending
          ? "Pending · once requested"
          : "Decision recorded in simulation"
        : "Public URL · no website login",
    ],
    [
      "Receipt",
      job.hash ? `SHA-256 ${job.hash}` : "Available after verification",
    ],
  ])
    facts.append(element("dt", label), element("dd", value));
  node.append(facts);
  if (job.state === "awaiting_approval") {
    const review = element("button", "Review request");
    review.addEventListener("click", openReview);
    node.append(review);
  }
  if (job.state === "failed") {
    const retry = element("button", "Retry after refreshing link");
    retry.disabled = stopped;
    retry.addEventListener("click", () => {
      job.state = "queued";
      job.eta = "Waiting for a fresh URL";
      message(
        "Simulation: retry queued. A real client must obtain and validate a fresh URL first.",
      );
      renderJobs();
    });
    node.append(retry);
  }
  if (job.hash) {
    const receipt = element("button", "View provenance receipt");
    receipt.addEventListener("click", () =>
      message(
        `Sample receipt: ${job.name} came from ${job.site}, requested by ${job.owner}, without conversion. SHA-256: ${job.hash}. No real file was downloaded.`,
      ),
    );
    node.append(receipt);
  }
}
function renderApproval() {
  const card = $("#approval-card");
  card.replaceChildren();
  $("#approval-count").textContent = approvalPending ? "1" : "0";
  if (!approvalPending) {
    card.append(element("p", "No requests waiting in this simulation."));
    return;
  }
  const row = element("div", null, "approval-row");
  const facts = document.createElement("div");
  facts.append(
    element("strong", "portal.example.org"),
    element("p", "Research assistant · 1 file · 18 MB · once"),
    element("small", `Revision ${revision} · example code 17M-4F1`, "muted"),
  );
  const button = element("button", "Review request");
  button.disabled = stopped;
  button.addEventListener("click", openReview);
  row.append(facts, button);
  card.append(row);
}
function openReview() {
  if (stopped) {
    message(
      "Release the global stop before reviewing a new request. Jobs and permissions remain stopped.",
    );
    return;
  }
  if (!approvalPending) {
    setView("approvals");
    return;
  }
  $("#confirm-code").value = "";
  $("#approve-demo").disabled = true;
  $("#approval-revision").textContent = revision;
  $("#review-hint").textContent = "Take a moment to read the request.";
  $("#approval-dialog").showModal();
  $("#review-title").tabIndex = -1;
  $("#review-title").focus();
  reviewReadyAt = performance.now() + 750;
  clearTimeout(reviewTimer);
  reviewTimer = setTimeout(updateReview, 760);
}
function updateReview() {
  const code = $("#confirm-code")
    .value.toUpperCase()
    .replace(/[ -]/g, "")
    .replace(/O/g, "0")
    .replace(/[IL]/g, "1");
  $("#approve-demo").disabled =
    stopped || performance.now() < reviewReadyAt || code !== "17M4F1";
  $("#review-hint").textContent =
    performance.now() < reviewReadyAt
      ? "Take a moment to read the request."
      : code === "17M4F1"
        ? "Ready to record a simulated decision."
        : "Type 17M-4F1 to continue. You can deny without a code.";
}
function decide(approved) {
  if (approved && $("#approve-demo").disabled) return;
  approvalPending = false;
  const job = jobs.find((j) => j.id === "sample-3");
  job.state = approved ? "queued" : "canceled";
  job.eta = approved ? "Waiting for Main queue" : "Denied by you";
  $("#approval-dialog").close();
  renderApproval();
  renderJobs();
  message(
    approved
      ? "Simulation: permission recorded for this one file. A production decision must pass the native trust channel and match the current request hash."
      : "Simulation: request denied. No permission was granted.",
  );
}
$$("[data-view]").forEach((button) =>
  button.addEventListener("click", () => setView(button.dataset.view)),
);
$("#add-links").addEventListener("click", () => {
  setView("inbox");
  $("#link-input").focus();
});
$("#search").addEventListener("input", renderJobs);
$$("[data-filter]").forEach((button) =>
  button.addEventListener("click", () => {
    filter = button.dataset.filter;
    $$("[data-filter]").forEach((b) =>
      b.setAttribute("aria-pressed", String(b === button)),
    );
    renderJobs();
  }),
);
$("#select-all").addEventListener("change", (event) => {
  visibleJobs().forEach((j) =>
    event.target.checked ? checked.add(j.id) : checked.delete(j.id),
  );
  renderJobs();
});
$$("[data-batch]").forEach((button) =>
  button.addEventListener("click", () => {
    let changed = 0;
    for (const job of jobs.filter((j) => checked.has(j.id))) {
      if (
        button.dataset.batch === "pause" &&
        ["queued", "downloading"].includes(job.state)
      ) {
        job.state = "paused";
        job.rate = "—";
        job.eta = "Paused by you";
        changed++;
      }
      if (
        button.dataset.batch === "resume" &&
        !stopped &&
        ["paused", "failed"].includes(job.state)
      ) {
        job.state = "queued";
        job.rate = "—";
        job.eta = "Waiting for queue";
        changed++;
      }
      if (
        button.dataset.batch === "cancel" &&
        !["completed", "canceled"].includes(job.state)
      ) {
        job.state = "canceled";
        job.rate = "—";
        job.eta = "Canceled by you";
        if (job.id === "sample-3") approvalPending = false;
        changed++;
      }
    }
    message(
      `Simulation: updated ${changed} download${changed === 1 ? "" : "s"}. Completed files remain available.`,
    );
    renderJobs();
    renderApproval();
  }),
);
$("#global-stop").addEventListener("click", () => {
  stopped = true;
  cloudPlanned = false;
  renderCloud();
  approvalPending = false;
  for (const job of jobs)
    if (!["completed", "canceled", "failed"].includes(job.state)) {
      job.state = "paused";
      job.rate = "—";
      job.eta = "Global stop";
    }
  $("#workspace-state").textContent = "Zeus work stopped · remote imports may continue";
  $("#workspace-dot").style.background = "#9e4526";
  $("#release-stop").hidden = false;
  if ($("#approval-dialog").open) $("#approval-dialog").close();
  renderJobs();
  renderApproval();
  message(
    "Simulation: Zeus work paused, permissions revoked and pending approvals canceled. An accepted provider fetch may continue remotely. Releasing the stop will not resume work or restore permissions.",
  );
});
$("#release-stop").addEventListener("click", () => {
  stopped = false;
  renderCloud();
  $("#release-stop").hidden = true;
  $("#workspace-state").textContent =
    "Ready · jobs stay paused until you resume them";
  $("#workspace-dot").style.background = "#56702f";
  renderJobs();
  message(
    "Simulation: stop released. Resume each queue or download deliberately.",
  );
});
$("#inspect-links").addEventListener("click", () => {
  const unique = new Set();
  let duplicates = 0;
  let invalid = 0;
  links = [];
  for (const line of $("#link-input")
    .value.split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean)) {
    try {
      const url = new URL(line);
      if (
        !["https:", "http:"].includes(url.protocol) ||
        url.username ||
        url.password
      )
        throw new Error();
      if (unique.has(url.href)) {
        duplicates++;
        continue;
      }
      unique.add(url.href);
      links.push({
        url,
        name: decodeURIComponent(
          url.pathname.split("/").filter(Boolean).at(-1) || "download",
        ),
      });
    } catch {
      invalid++;
    }
  }
  const preview = $("#link-preview");
  preview.classList.toggle("empty", links.length === 0);
  preview.replaceChildren();
  for (const link of links) {
    const item = element("div", null, "link-item");
    item.append(
      element("strong", link.name),
      element("small", `${link.url.origin} · size unknown`),
    );
    preview.append(item);
  }
  if (!links.length) preview.textContent = "No valid HTTP or HTTPS links.";
  $("#link-total").textContent = links.length;
  $("#inbox-count").textContent = links.length;
  $("#queue-links").disabled = !links.length || stopped;
  message(
    `Preview: ${links.length} unique links, ${duplicates} duplicates skipped, ${invalid} invalid or credential-bearing links rejected. No websites contacted.`,
  );
});
$("#queue-links").addEventListener("click", () => {
  if (stopped) return;
  const packageName = $("#package-name").value.trim() || "New collection";
  for (const link of links)
    jobs.unshift({
      id: `sample-new-${jobs.length}`,
      name: link.name,
      site: link.url.hostname,
      owner: "You",
      channel: `Package · ${packageName}`,
      state: "queued",
      saved: 0,
      size: "?",
      rate: "—",
      eta: "Size not known yet",
      queue: $("#inbox-queue").value,
      destination: `Downloads/${packageName}`,
      note: "Simulated URL import. The real daemon must validate its URL, destination and policy before downloading.",
      hash: null,
    });
  links = [];
  $("#inbox-count").textContent = "0";
  $("#queue-links").disabled = true;
  setView("downloads");
  renderJobs();
  message(
    "Simulation: package added to the queue. No requests sent to websites.",
  );
});
$("#save-budget").addEventListener("click", () => {
  const inputs = ["files", "gb", "minutes", "parallel"].map((k) =>
    $(`#budget-${k}`),
  );
  if (inputs.some((i) => !i.reportValidity())) return;
  revision++;
  approvalPending = true;
  renderApproval();
  $("#budget-summary").textContent =
    `Stop after ${inputs[0].value} files, ${inputs[1].value} GB or ${inputs[2].value} minutes. Download at most ${inputs[3].value} at a time.`;
  message(
    `Simulation: preview changed to revision ${revision}. Previous review decisions cannot apply to an amended plan.`,
  );
});
$$("[data-queue]").forEach((button) =>
  button.addEventListener("click", () => {
    queue = button.dataset.queue;
    $("#queue-title").textContent = `${queue} queue`;
    $$("[data-queue]").forEach((b) =>
      b.setAttribute("aria-pressed", String(b === button)),
    );
    $("#queue-scheduled").checked = queue === "Night";
    $("#queue-parallel").value = queue === "Main" ? "2" : "1";
  }),
);
$("#save-queue").addEventListener("click", () => {
  if (
    !$("#queue-parallel").reportValidity() ||
    !$("#queue-speed").reportValidity()
  )
    return;
  message(
    `Simulation: ${queue} queue preview saved. ${$("#queue-scheduled").checked ? `Daily ${$("#queue-start").value}–${$("#queue-end").value}, Asia/Dhaka.` : "No daily schedule."} A production scheduler is still required.`,
  );
});
$("#review-run").addEventListener("click", () => {
  setView("approvals");
  message(
    "The sample permission request below illustrates the trust surface. It does not authorize this entire plan.",
  );
});
$("#confirm-code").addEventListener("input", updateReview);
$("#approve-demo").addEventListener("click", () => decide(true));
$("#deny-demo").addEventListener("click", () => decide(false));
$("#approval-dialog").addEventListener("close", () =>
  clearTimeout(reviewTimer),
);
renderJobs();
renderApproval();

// Cloud destination design simulation. Capability examples are explicit, never a live account check.
const cloudModes = {
  staged_upload: {
    title: "Download, verify, then upload",
    detail: "Keeps a complete staged file. Verify its local SHA-256 before uploading; requires space for the full file.",
    exposure: "The destination provider receives file bytes. The original source URL is not handed to the provider.",
    assurance: "Example assurance: local SHA-256 checked before upload. Completion must state whether a matching remote SHA-256 was returned or only remote size; a receipt alone is insufficient.",
  },
  stream_upload: {
    title: "Relay with bounded buffering",
    detail: "Reads from the source and uploads with bounded memory. No full local copy; restarts and provider limits must be checked before choosing this mode.",
    exposure: "The destination provider receives file bytes while Zeus reads them. The original source URL is not handed to the provider.",
    assurance: "Example assurance: local SHA-256 is available only after reading the entire stream. A source mismatch may be discovered after upload has begun. Remote SHA-256 and remote size are separate evidence.",
  },
  remote_import: {
    title: "Provider fetches the source URL",
    detail: "Only providers advertising URL-fetch support can use this mode. The provider performs the download; Zeus does not read the file bytes.",
    exposure: "The full source URL is disclosed to the destination provider. A separate approval must cover that disclosure. Browser cookies and session material are never forwarded.",
    assurance: "Example assurance: no local hash. A known expected SHA-256 can be compared with a provider-reported SHA-256; otherwise the evidence is remote size only. Accepted / in progress does not mean completed.",
  },
};
const cloudAccounts = {
  drive: { label: "Google Drive · Research (sample)", folder: "Research/Climate reference", fetch: false },
  yandex: { label: "Yandex Disk · Archive (sample)", folder: "disk:/Archive", fetch: true },
  rclone: { label: "Configured rclone remote (sample)", folder: "zeus-archive/Research", fetch: false },
  baidu: { label: "Baidu Netdisk · App folder (sample)", folder: "/apps/Zeus/Research", fetch: false, blockManifest: true },
};
let cloudRevision = 1;
let cloudPlanned = false;
function cloudSelection() {
  return { account: cloudAccounts[$("#cloud-account").value], mode: $("#cloud-mode").value,
    source: $("#cloud-source").value, folder: $("#cloud-folder").value.trim(), name: $("#cloud-name").value.trim(),
    remoteOptIn: $("#cloud-remote-optin").checked, blockManifest: $("#cloud-block-manifest").checked };
}
function cloudRestriction(selection) {
  if (!selection.name || /[\\/:?#\x00-\x1f]/.test(selection.name) || [".", ".."].includes(selection.name)) return "Choose a filename only, inside the account’s configured folder.";
  if (selection.mode === "remote_import" && !selection.account.fetch) return "This sample account does not advertise provider URL fetch. Choose another mode explicitly; no automatic fallback is allowed.";
  if (selection.mode === "remote_import" && selection.source !== "public") return "Provider fetch requires a public source URL. Local files and browser-session sources cannot be disclosed as a URL-fetch request.";
  if (selection.mode === "remote_import" && !selection.remoteOptIn) return "Explicitly accept the provider’s cancellation and byte-limit restrictions before previewing this mode.";
  if (selection.mode === "stream_upload" && selection.account.blockManifest && !selection.blockManifest) return "Baidu streaming needs a verified manifest of 4 MiB block MD5 values before the first upload. Choose staged mode explicitly if it is unavailable.";
  if (selection.mode === "stream_upload" && selection.source !== "public") return "This relay example accepts public URLs only. Use staged delivery for a completed local download; browser-session relay is not advertised.";
  if (selection.source === "session") return "Browser-session cloud delivery requires separately bound source and destination permission; this capability is not advertised by the preview.";
  return null;
}
function renderCloud() {
  const selection = cloudSelection();
  const mode = cloudModes[selection.mode];
  const restriction = cloudRestriction(selection);
  $("#cloud-capability").textContent = restriction || `Sample capability: ${mode.detail}`;
  $("#cloud-exposure").textContent = mode.exposure;
  $("#cloud-plan").disabled = Boolean(restriction) || stopped;
  $("#cloud-example").disabled = Boolean(restriction) || !cloudPlanned || stopped;
  const facts = $("#cloud-facts");
  facts.replaceChildren();
  for (const [label, value] of [["Account", selection.account.label], ["Folder", selection.folder || "Not chosen"], ["Object", `${selection.name}.zeus-cld_SAMPLE`], ["Mode", mode.title], ["Limits", selection.mode === "remote_import" ? "2 GB expected · 30 min local lifetime · provider cap and cancellation unavailable" : "1 sample file · at most 2 GB · 30 minutes"], ["Source URL", selection.mode === "remote_import" ? "Full source URL disclosed to provider" : "Not disclosed to destination provider"], ["Revision", String(cloudRevision)], ["Permission", "Not granted · simulation only"]]) {
    facts.append(element("dt", label), element("dd", value));
  }
  $("#cloud-assurance").textContent = mode.assurance;
  if (!cloudPlanned) {
    $("#cloud-progress").replaceChildren();
    $("#cloud-review-state").textContent = "Review the current facts. Nothing is authorized or submitted.";
  }
}
function invalidateCloud() {
  cloudRevision++;
  cloudPlanned = false;
  renderCloud();
}
for (const id of ["cloud-account", "cloud-source", "cloud-mode", "cloud-name", "cloud-remote-optin", "cloud-block-manifest"]) $("#" + id).addEventListener(id === "cloud-name" ? "input" : "change", () => {
  $("#cloud-folder").value = cloudAccounts[$("#cloud-account").value].folder;
  invalidateCloud();
});
$("#cloud-plan").addEventListener("click", () => {
  if (stopped || cloudRestriction(cloudSelection())) return;
  cloudPlanned = true;
  renderCloud();
  $("#cloud-review-state").textContent = `Sample plan revision ${cloudRevision} is ready for human review. This preview cannot request or record real cloud permission.`;
});
$("#cloud-example").addEventListener("click", () => {
  const selection = cloudSelection();
  if (stopped || !cloudPlanned || cloudRestriction(selection)) return;
  const stages = selection.mode === "remote_import"
    ? [["Source download by Zeus", null, "Not performed"], ["Provider operation", null, "Provider says in progress · byte count unknown"]]
    : selection.mode === "stream_upload"
      ? [["Source read", 63, "63% read through the bounded relay"], ["Upload", 58, "58% acknowledged by the sample provider"]]
      : [["Download", 100, "Complete sample staging file"], ["Local SHA-256", 100, "Verified before upload"], ["Upload", 35, "35% acknowledged by the sample provider"]];
  const progress = $("#cloud-progress");
  progress.replaceChildren();
  for (const [label, percent, detail] of stages) {
    const row = element("div", null, "cloud-progress-row");
    row.append(element("strong", label), element("span", detail));
    if (percent !== null) {
      const bar = element("progress"); bar.max = 100; bar.value = percent; bar.setAttribute("aria-label", `${label}: ${percent}% (sample)`); row.append(bar);
    }
    progress.append(row);
  }
  message("Sample progress only. No credentials, source bytes or cloud requests were sent.");
});
renderCloud();
