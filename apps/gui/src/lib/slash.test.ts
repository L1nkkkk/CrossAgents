import assert from "node:assert/strict";
import { test } from "node:test";
import { matchCommands, type SlashGroup } from "./slash.ts";

const groups: SlashGroup[] = [
  {
    key: "crossagents",
    label: "CrossAgents 指令",
    commands: [
      { key: "compact", name: "compact", title: "压缩上下文", group: "crossagents" },
      { key: "model", name: "model", title: "换模型", group: "crossagents" },
    ],
  },
  {
    key: "agent",
    label: "Nova 自带的指令",
    commands: [{ key: "agent/review", name: "review", title: "审查改动", group: "agent" }],
  },
];

test("nothing typed lists every command, group by group", () => {
  assert.deepEqual(
    matchCommands(groups, "").map((c) => c.name),
    ["compact", "model", "review"],
  );
});

test("a name matches as a prefix, and names rank before titles", () => {
  assert.deepEqual(
    matchCommands(groups, "m").map((c) => c.name),
    ["model", "compact"],
  );
});

test("the title is searchable too", () => {
  assert.deepEqual(
    matchCommands(groups, "压缩").map((c) => c.name),
    ["compact"],
  );
});

test("no match is empty: the text is a message", () => {
  assert.deepEqual(matchCommands(groups, "zzz"), []);
});

test("matching ignores case and the space after the slash", () => {
  assert.deepEqual(
    matchCommands(groups, " REV ").map((c) => c.name),
    ["review"],
  );
});
