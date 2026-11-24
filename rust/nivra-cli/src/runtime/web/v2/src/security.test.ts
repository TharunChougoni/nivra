import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { createApi } from "./api";

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
