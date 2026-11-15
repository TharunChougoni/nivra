import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { createApi } from "./api";
import { initializeTheme } from "./theme";
import { JSDOM } from "jsdom";

test("bundled theme initialization restores preferences without the retired asset route", () => {
  const dom = new JSDOM("<!doctype html><html data-theme='dark'></html>");
  try {
    initializeTheme(dom.window.document, () => ({ getItem: () => "light" }));
    assert.equal(dom.window.document.documentElement.dataset.theme, "light");
    initializeTheme(dom.window.document, () => ({ getItem: () => "invalid" }));
    assert.equal(dom.window.document.documentElement.dataset.theme, "dark");
    initializeTheme(dom.window.document, () => {
      throw new Error("storage denied");
    });
    assert.equal(dom.window.document.documentElement.dataset.theme, "dark");
    for (const path of ["../index.html", "../bundle/index.html"]) {
      const html = readFileSync(new URL(path, import.meta.url), "utf8");
      assert.equal(html.includes("/assets/theme.js"), false, path);
    }
    const main = readFileSync(new URL("./main.tsx", import.meta.url), "utf8");
    assert.ok(
      main.indexOf("initializeTheme(document") <
        main.indexOf("export function mountApp"),
    );
  } finally {
    dom.window.close();
  }
});
test("application renders external evidence as text and never injects raw HTML or evaluates code", () => {
  for (const file of readdirSync(new URL(".", import.meta.url)).filter(
    (v) => /\.(ts|tsx)$/.test(v) && !v.includes(".test."),
  )) {
    const source = readFileSync(new URL(file, import.meta.url), "utf8");
    assert.equal(source.includes("dangerouslySetInnerHTML"), false, file);
    assert.equal(source.includes(".innerHTML"), false, file);
    assert.equal(source.includes("eval("), false, file);
  }
});
test("ETag cache cannot be refilled by responses after the token changes", async () => {
  let token = "one";
  let done: ((response: Response) => void) | undefined;
  const api = createApi({
    getToken: () => token,
    fetchImpl: () =>
      new Promise((r) => {
        done = r;
      }),
  });
  const read = api.summary();
  token = "";
  api.clear();
  assert.ok(done);
  done(new Response('{"private":"fixture"}'));
  await assert.rejects(read, /session_changed/);
});
