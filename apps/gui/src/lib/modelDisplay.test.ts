import assert from "node:assert/strict";
import { test } from "node:test";
import type { AccountQuota } from "@bindings/AccountQuota";
import { modelDisplay } from "./modelDisplay.ts";

const account = (provider: string, id: string, name: string) => ({
  account: { provider, id }, models: [{ id: "opus", name, description: "CLI description" }],
} as AccountQuota);

test("CLI names resolve aliases while keeping provider and account boundaries", () => {
  const accounts = [account("cursor", "cursor", "Cursor Opus"), account("claude", "work", "Opus 5.5")];
  assert.equal(modelDisplay("claude", "opus", accounts).label, "Opus 5.5");
  assert.equal(modelDisplay("claude", "opus", accounts, "work").description, "CLI description");
  assert.equal(modelDisplay("claude", "opus", accounts, "missing").label, "Opus");
  assert.equal(modelDisplay("claude", "claude-opus-5", accounts).label, "Opus 5");
  const mixed = [...accounts, account("claude", "personal", "Opus 5")];
  assert.equal(modelDisplay("claude", "opus", mixed).label, "Opus");
  assert.equal(modelDisplay("claude", "opus", mixed, "personal").label, "Opus 5");
});
