import { test } from "node:test";
import { deepStrictEqual, match } from "node:assert";
import { extractToolImages } from "./toolimages.ts";

const url = "data:image/png;base64,YWJj";
test("extracts image_url and MCP blocks", () => {
  deepStrictEqual(extractToolImages(JSON.stringify([{ type: "image_url", image_url: { url } }, { type: "image", data: "YWJj", mimeType: "image/png" }])), { images: [url, url], text: "" });
});
test("extracts a bare data URL", () => deepStrictEqual(extractToolImages(url), { images: [url], text: "" }));
test("keeps mixed text", () => {
  const result = extractToolImages(JSON.stringify([{ type: "text", text: "hello" }, { type: "image_url", image_url: { url } }]));
  deepStrictEqual(result.images, [url]); match(result.text, /hello/);
});
test("preserves non-image data and malformed JSON", () => {
  for (const raw of ["data:text/plain;base64,YWJj", "[{broken"]) deepStrictEqual(extractToolImages(raw), { images: [], text: raw });
});
