import test from "node:test";
import assert from "node:assert/strict";
import { uniqueLinks, reconcileSelection, canControl, filterRows } from "../ui/model.mjs";

test("signed links stay distinct while exact duplicates collapse", () => {
  assert.deepEqual(uniqueLinks(" https://example.org/a?sig=one\r\nhttps://example.org/a?sig=two\nhttps://example.org/a?sig=one\n"), ["https://example.org/a?sig=one", "https://example.org/a?sig=two"]);
});

test("selection follows identifiers through refresh reorder and removal", () => {
  const selected = new Set(["job_a", "job_b"]);
  assert.deepEqual([...reconcileSelection("jobs", selected, [{id:"job_b"}, {id:"job_c"}])], ["job_b"]);
  assert.deepEqual([...reconcileSelection("jobs", selected, [{id:"job_b"}, {id:"job_a"}])], ["job_a", "job_b"]);
});

test("mixed states cannot share an illegal batch control", () => {
  assert.equal(canControl("pause", [{state:"downloading"}, {state:"queued"}]), true);
  assert.equal(canControl("pause", [{state:"downloading"}, {state:"completed"}]), false);
  assert.equal(canControl("cancel", [{state:"finalizing"}]), false);
  assert.equal(canControl("retry", []), false);
  assert.equal(canControl("resume", [{state:"paused"}]), true);
});

test("search only filters loaded metadata and does not alter selection or state", () => {
  const rows = [{id:"job_a",filename:"Report.PDF",state:"paused"}, {id:"job_b",filename:"photo.jpg",state:"downloading"}];
  assert.deepEqual(filterRows(rows,"report"), [rows[0]]);
  assert.deepEqual(filterRows(rows,"p"), rows);
  assert.equal(rows[0].state, "paused");
});
