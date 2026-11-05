import assert from "node:assert/strict";
import { test } from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { ExtensionBoundary, safeElement } from "./boundary.ts";

test("a throwing render function breaks only its own block", () => {
  const errors: unknown[] = [];
  const handlers = { onDisable: () => {}, onError: (error: unknown) => errors.push(error) };
  const html = renderToStaticMarkup(
    createElement(
      "div",
      null,
      safeElement(
        "broken",
        () => {
          throw new Error("boom from a panel");
        },
        handlers,
      ),
      safeElement("fine", () => createElement("span", null, "still here"), handlers),
    ),
  );
  assert.match(html, /still here/);
  assert.match(html, /boom from a panel/);
  assert.match(html, /停用/);
  assert.deepEqual(errors, [new Error("boom from a panel")]);
});

test("the boundary turns a later render error into the same failure card", () => {
  const error = new Error("child threw");
  assert.deepEqual(ExtensionBoundary.getDerivedStateFromError(error), { error });
  const boundary = new ExtensionBoundary({ extension: "broken", handlers: { onDisable: () => {} }, children: null });
  boundary.state = { error };
  assert.match(renderToStaticMarkup(boundary.render() as never), /child threw/);
  assert.match(renderToStaticMarkup(boundary.render() as never), /停用/);
});
