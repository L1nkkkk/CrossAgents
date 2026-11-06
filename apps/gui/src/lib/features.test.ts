import assert from "node:assert/strict";
import { test } from "node:test";
import {
  FEATURES,
  currentState,
  featureOf,
  forgetHub,
  hiddenByHub,
  missingText,
  noteForbidden,
  noteHub,
  supportOf,
  type FeatureState,
} from "./features.ts";

test("a request belongs to the entry it came from", () => {
  assert.equal(featureOf("/v1/git/branches?path=C%3A%2Fx"), "git");
  assert.equal(featureOf("/v1/git/commits/abc/diff?path=x"), "git");
  assert.equal(featureOf("/v1/works"), "work");
  assert.equal(featureOf("/v1/works/abc"), "work");
  assert.equal(featureOf("/v1/projects/a%2Fb/lessons?q=x"), "lessons");
  assert.equal(featureOf("/v1/lessons/abc"), "lessons");
  assert.equal(featureOf("/v1/runs/abc/apply"), "apply");
  assert.equal(featureOf("/v1/runs/abc/integration"), "apply");
  // Everything else belongs to no feature and is never gated.
  assert.equal(featureOf("/v1/info"), undefined);
  assert.equal(featureOf("/v1/runs/abc"), undefined);
  assert.equal(featureOf("/v1/workspaces"), undefined);
  assert.equal(featureOf("/v1/agents/abc/files/tree?path="), undefined);
});

test("an older hub, which sends no list, leaves every entry hidden", () => {
  for (const feature of FEATURES) {
    assert.deepEqual(supportOf({ known: true, listed: null, refused: {} }, feature), {
      known: true,
      show: false,
      disabled: false,
    });
  }
  assert.deepEqual(hiddenByHub({ known: true, listed: null, refused: {} }), [...FEATURES]);
});

test("before the hub answers, nothing is shown yet", () => {
  assert.deepEqual(supportOf({ known: false, listed: null, refused: {} }, "git"), { known: false, show: false, disabled: false });
  assert.deepEqual(hiddenByHub({ known: false, listed: null, refused: {} }), []);
});

test("a listed entry shows; one the hub left out stays hidden", () => {
  const state: FeatureState = { known: true, listed: ["git", "apply"], refused: {} };
  assert.deepEqual(supportOf(state, "git"), { known: true, show: true, disabled: false });
  assert.deepEqual(supportOf(state, "work"), { known: true, show: false, disabled: false });
  assert.deepEqual(hiddenByHub(state), ["lessons", "work"]);
});

test("a 403 keeps the entry, greyed, quoting the hub", () => {
  const state: FeatureState = { known: true, listed: [...FEATURES], refused: { git: "需要 read 级别" } };
  assert.deepEqual(supportOf(state, "git"), { known: true, show: true, disabled: true, reason: "需要 read 级别" });
  // Another entry is unaffected.
  assert.deepEqual(supportOf(state, "apply"), { known: true, show: true, disabled: false });
});

test("the version tooltip names what a hub leaves out, and why", () => {
  const old: FeatureState = { known: true, listed: null, refused: {} };
  assert.match(missingText(old), /旧版 hub/);
  assert.match(missingText(old), /Git 页、排障记录、工作组、应用到原目录/);
  // A newer hub that has some of them says which, without calling itself old.
  const partial: FeatureState = { known: true, listed: ["git"], refused: {} };
  assert.match(missingText(partial), /排障记录、工作组、应用到原目录/);
  assert.doesNotMatch(missingText(partial), /旧版/);
  assert.equal(missingText({ known: true, listed: [...FEATURES], refused: {} }), "");
  // Before the hub answers there is nothing to say.
  assert.equal(missingText({ known: false, listed: null, refused: {} }), "");
});

test("the store follows /v1/info and 403s, and a new connection forgets refusals", () => {
  forgetHub();
  assert.equal(currentState().known, false);

  noteHub(["git", "lessons", "unknown-name"]);
  assert.deepEqual(currentState().listed, ["git", "lessons"]);

  noteForbidden("/v1/git/status?path=x", "这个凭据只能读");
  assert.deepEqual(supportOf(currentState(), "git"), { known: true, show: true, disabled: true, reason: "这个凭据只能读" });
  // A feature the hub did not list is hidden anyway, so its refusal is not kept.
  noteForbidden("/v1/works", "没这个权限");
  assert.equal(currentState().refused.work, undefined);

  noteHub([...FEATURES]);
  assert.deepEqual(currentState().refused, {});

  // A hub from before the list: the same entries, hidden, and their 403s not kept.
  noteHub(undefined);
  assert.equal(currentState().listed, null);
  assert.deepEqual(supportOf(currentState(), "git"), { known: true, show: false, disabled: false });
  noteForbidden("/v1/git/status?path=x", "谁");
  assert.deepEqual(currentState().refused, {});
});
