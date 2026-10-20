/** Exact identity is important for signed links; never normalize their query strings. */
export function uniqueLinks(text) {
  return [...new Set(text.split(/\r?\n/).map(line => line.trim()).filter(Boolean))];
}

export function rowId(section, row) {
  if (section === "runs") return row.run_id;
  if (section === "files") return `${row.root}\u0000${row.path}`;
  return row.id;
}

export function filterRows(rows, query) {
  const term = query.trim().toLocaleLowerCase();
  return term ? rows.filter(row => Object.values(row).some(value =>
    typeof value === "string" && value.toLocaleLowerCase().includes(term))) : rows;
}

export function reconcileSelection(section, selected, rows) {
  const available = new Set(rows.map(row => rowId(section, row)));
  return new Set([...selected].filter(id => available.has(id)));
}

const allowed = {
  pause: new Set(["queued", "probing", "downloading", "retry_scheduled"]),
  resume: new Set(["paused"]),
  retry: new Set(["failed"]),
  cancel: new Set(["queued", "probing", "awaiting_approval", "awaiting_session", "downloading", "paused", "retry_scheduled", "verifying"]),
};

export function canControl(action, selectedRows) {
  return selectedRows.length > 0 && selectedRows.every(row => allowed[action]?.has(row.state));
}

export function formatBytes(value) {
  if (value === null || value === undefined) return "Unknown";
  if (!Number.isFinite(value) || value < 0) return "Unknown";
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let amount = value;
  let unit = 0;
  while (amount >= 1024 && unit < units.length - 1) { amount /= 1024; unit++; }
  return `${amount.toLocaleString(undefined, { maximumFractionDigits: unit ? 1 : 0 })} ${units[unit]}`;
}
