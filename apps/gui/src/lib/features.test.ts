import assert from "node:assert/strict";
import { test } from "node:test";
import {
  FEATURES,
  WITHOUT_LIST,
  atLeast,
  currentState,
  featureOf,
  forgetHub,
  hiddenByHub,
  missingText,
  noteForbidden,
  noteHub,
  offeredOf,
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
  // 设备: the list and one device's revoke.
  assert.equal(featureOf("/v1/devices"), "devices");
  assert.equal(featureOf("/v1/devices/0193-abc"), "devices");
  // Everything else belongs to no feature and is never gated.
  assert.equal(featureOf("/v1/info"), undefined);
  assert.equal(featureOf("/v1/runs/abc"), undefined);
  assert.equal(featureOf("/v1/workspaces"), undefined);
  assert.equal(featureOf("/v1/agents/abc/files/tree?path="), undefined);
});

test("versions compare by their numbers", () => {
  assert.equal(atLeast("0.2.6", "0.2.6"), true);
  assert.equal(atLeast("v0.2.6", "0.2.6"), true);
  assert.equal(atLeast("0.2.7", "0.2.6"), true);
  assert.equal(atLeast("0.3.0", "0.2.6"), true);
  assert.equal(atLeast("1.0.0", "0.2.6"), true);
  assert.equal(atLeast("0.2.10", "0.2.9"), true);
  assert.equal(atLeast("0.2.5", "0.2.6"), false);
  assert.equal(atLeast("0.1.9", "0.2.6"), false);
  assert.equal(atLeast("0.2.6-dev", "0.2.6"), true);
  assert.equal(atLeast(undefined, "0.2.6"), false);
  assert.equal(atLeast("", "0.2.6"), false);
});

test("a 0.2.6 hub without a list still has the Git page and 排障记录", () => {
  // The list arrived after 0.2.6, so such a hub sends none; both pages were already in 0.2.6.
  const state: FeatureState = { known: true, version: "0.2.6", listed: null, refused: {} };
  assert.deepEqual(offeredOf(state), ["git", "lessons"]);
  assert.deepEqual(supportOf(state, "git"), { known: true, show: true, disabled: false });
  assert.deepEqual(supportOf(state, "lessons"), { known: true, show: true, disabled: false });
  // 工作组, 应用到原目录 and 设备 came later and are never inferred.
  assert.deepEqual(supportOf(state, "work"), { known: true, show: false, disabled: false });
  assert.deepEqual(supportOf(state, "apply"), { known: true, show: false, disabled: false });
  assert.deepEqual(supportOf(state, "devices"), { known: true, show: false, disabled: false });
  assert.deepEqual(hiddenByHub(state), ["work", "apply", "devices"]);
});

test("a 0.2.5 hub without a list has none of the entries", () => {
  const state: FeatureState = { known: true, version: "0.2.5", listed: null, refused: {} };
  for (const feature of FEATURES) {
    assert.deepEqual(supportOf(state, feature), { known: true, show: false, disabled: false });
  }
  assert.deepEqual(hiddenByHub(state), [...FEATURES]);
});

test("a list wins over the version: the hub's own answer is what counts", () => {
  // A 0.2.6 hub that does list something: follow the list, even when it disagrees with the table.
  const state: FeatureState = { known: true, version: "0.2.6", listed: ["work"], refused: {} };
  assert.deepEqual(offeredOf(state), ["work"]);
  assert.deepEqual(supportOf(state, "work"), { known: true, show: true, disabled: false });
  assert.deepEqual(supportOf(state, "git"), { known: true, show: false, disabled: false });
  // A 0.2.5 hub that lists everything: the list still decides.
  const older: FeatureState = { known: true, version: "0.2.5", listed: [...FEATURES], refused: {} };
  assert.deepEqual(offeredOf(older), [...FEATURES]);
  assert.deepEqual(hiddenByHub(older), []);
  // No version at all and no list: nothing can be inferred, so nothing shows.
  assert.deepEqual(offeredOf({ known: true, listed: null, refused: {} }), []);
});

test("before the hub answers, nothing is shown yet", () => {
  assert.deepEqual(supportOf({ known: false, listed: null, refused: {} }, "git"), { known: false, show: false, disabled: false });
  assert.deepEqual(hiddenByHub({ known: false, listed: null, refused: {} }), []);
});

test("a listed entry shows; one the hub left out stays hidden", () => {
  const state: FeatureState = { known: true, version: "0.2.6", listed: ["git", "apply"], refused: {} };
  assert.deepEqual(supportOf(state, "git"), { known: true, show: true, disabled: false });
  assert.deepEqual(supportOf(state, "work"), { known: true, show: false, disabled: false });
  assert.deepEqual(hiddenByHub(state), ["lessons", "work", "devices"]);
});

test("a hub that does not list 设备 has no devices page", () => {
  // 设备 came after 0.2.6 and is never inferred from the version: without it in the list, the
  // settings entry does not appear, whatever the version says.
  const listed: FeatureState = { known: true, version: "0.3.0", listed: ["git", "lessons", "work", "apply"], refused: {} };
  assert.deepEqual(supportOf(listed, "devices"), { known: true, show: false, disabled: false });
  assert.deepEqual(hiddenByHub(listed), ["devices"]);
  const inferred: FeatureState = { known: true, version: "0.9.0", listed: null, refused: {} };
  assert.deepEqual(supportOf(inferred, "devices"), { known: true, show: false, disabled: false });
});

test("a 403 keeps the entry, greyed, quoting the hub", () => {
  const state: FeatureState = { known: true, version: "0.2.6", listed: [...FEATURES], refused: { git: "需要 read 级别" } };
  assert.deepEqual(supportOf(state, "git"), { known: true, show: true, disabled: true, reason: "需要 read 级别" });
  // Another entry is unaffected.
  assert.deepEqual(supportOf(state, "apply"), { known: true, show: true, disabled: false });
  // An inferred entry can be refused too, and is then greyed like a listed one.
  const inferred: FeatureState = { known: true, version: "0.2.6", listed: null, refused: { git: "只能读" } };
  assert.deepEqual(supportOf(inferred, "git"), { known: true, show: true, disabled: true, reason: "只能读" });
});

test("the version tooltip names what a hub leaves out, and says the version was the evidence", () => {
  // 0.2.6: only the two later entries are hidden.
  const recent: FeatureState = { known: true, version: "0.2.6", listed: null, refused: {} };
  assert.match(missingText(recent), /按版本 0\.2\.6 推断/);
  assert.match(missingText(recent), /工作组、应用到原目录/);
  assert.doesNotMatch(missingText(recent), /Git 页/);
  // 0.2.5: all five.
  const older: FeatureState = { known: true, version: "0.2.5", listed: null, refused: {} };
  assert.match(missingText(older), /按版本 0\.2\.5 推断/);
  assert.match(missingText(older), /Git 页、排障记录、工作组、应用到原目录、设备/);
  // A newer hub that has some of them says which, without calling itself old.
  const partial: FeatureState = { known: true, version: "0.3.0", listed: ["git"], refused: {} };
  assert.match(missingText(partial), /排障记录、工作组、应用到原目录、设备/);
  assert.doesNotMatch(missingText(partial), /旧版/);
  assert.equal(missingText({ known: true, version: "0.2.6", listed: [...FEATURES], refused: {} }), "");
  // Before the hub answers there is nothing to say.
  assert.equal(missingText({ known: false, listed: null, refused: {} }), "");
});

test("the table is the one place inference is spelled out", () => {
  assert.deepEqual(WITHOUT_LIST, [{ since: "0.2.6", features: ["git", "lessons"] }]);
});

test("the store follows /v1/info and 403s, and a new connection forgets refusals", () => {
  forgetHub();
  assert.equal(currentState().known, false);

  noteHub(["git", "lessons", "unknown-name"], "0.2.6");
  assert.deepEqual(currentState().listed, ["git", "lessons"]);

  noteForbidden("/v1/git/status?path=x", "这个凭据只能读");
  assert.deepEqual(supportOf(currentState(), "git"), { known: true, show: true, disabled: true, reason: "这个凭据只能读" });
  // A feature the hub did not list is hidden anyway, so its refusal is not kept.
  noteForbidden("/v1/works", "没这个权限");
  assert.equal(currentState().refused.work, undefined);

  noteHub([...FEATURES], "0.2.6");
  assert.deepEqual(currentState().refused, {});

  // A hub from before the list: its version decides, and an inferred entry's 403 is kept.
  noteHub(undefined, "0.2.6");
  assert.equal(currentState().listed, null);
  assert.deepEqual(supportOf(currentState(), "git"), { known: true, show: true, disabled: false });
  assert.deepEqual(supportOf(currentState(), "work"), { known: true, show: false, disabled: false });
  noteForbidden("/v1/git/status?path=x", "这一项也要读权限");
  assert.equal(currentState().refused.git, "这一项也要读权限");
  // A refusal for an entry this version does not have is not kept.
  noteForbidden("/v1/works", "谁");
  assert.equal(currentState().refused.work, undefined);

  // A 0.2.5 hub without a list: nothing inferred, so no refusals are kept either.
  noteHub(undefined, "0.2.5");
  assert.deepEqual(currentState().refused, {});
  noteForbidden("/v1/git/status?path=x", "谁");
  assert.deepEqual(currentState().refused, {});
});
