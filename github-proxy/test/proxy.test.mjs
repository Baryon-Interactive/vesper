import assert from "node:assert/strict";
import { test } from "node:test";
import { createProxy, githubUrl, parsePath } from "../src/index.js";

const CATALOGUE = "https://catalogue.example/index/";
const targets = {
  signed: {
    sources: [
      { id: "r2", kind: "r2", url_template: "https://cdn.example/game/{release}/{pack_id}", priority: 1 },
      { id: "github", kind: "github", url_template: "https://github.com/example/game-builds/releases/download/{release}/{pack_id}", priority: 0 },
    ],
  },
};

/** A fake fetch that serves the catalogue and records upstream requests. */
function fakeFetch(calls) {
  return async (url, init = {}) => {
    calls.push({ url: String(url), init });
    if (String(url) === `${CATALOGUE}apps/demo/targets.json`) return Response.json(targets);
    if (String(url).startsWith(CATALOGUE)) return new Response("missing", { status: 404 });
    const range = init.headers?.get("Range");
    return new Response(range ? "abcd" : "abcdefgh", {
      status: range ? 206 : 200,
      headers: {
        "Content-Type": "application/octet-stream",
        ...(range ? { "Content-Range": "bytes 0-3/8", "Content-Length": "4" } : { "Content-Length": "8" }),
        "Set-Cookie": "upstream=secret",
      },
    });
  };
}

test("paths must be three safe segments", () => {
  assert.deepEqual(parsePath("/demo/v1.0.0-web/pack-ab.vpack"), { appId: "demo", release: "v1.0.0-web", asset: "pack-ab.vpack" });
  assert.equal(parsePath("/demo/v1/../x"), null);
  assert.equal(parsePath("/demo/v1"), null);
  assert.equal(parsePath("/demo/v1/a%2Fb"), null);
  assert.equal(parsePath("/demo/../x"), null);
  assert.equal(parsePath("/demo/v1/%E0%A4%A"), null);
});

test("only github templates from the targets are used", () => {
  assert.equal(githubUrl(targets, "v1", "pack-a.vpack"), "https://github.com/example/game-builds/releases/download/v1/pack-a.vpack");
  const evil = { signed: { sources: [{ kind: "github", url_template: "https://evil.example/{release}/{pack_id}" }] } };
  assert.equal(githubUrl(evil, "v1", "a"), null);
  assert.equal(githubUrl({}, "v1", "a"), null);
});

test("ranges are streamed from the app's release with CORS", async () => {
  const calls = [];
  const proxy = createProxy({ fetchImpl: fakeFetch(calls) });
  const response = await proxy.fetch(
    new Request("https://proxy.example/demo/v1.0.0-web/pack-ab.vpack", { headers: { Range: "bytes=0-3", Cookie: "c=1" } }),
    { CATALOGUE },
  );
  assert.equal(response.status, 206);
  assert.equal(await response.text(), "abcd");
  assert.equal(response.headers.get("access-control-allow-origin"), "*");
  assert.match(response.headers.get("access-control-expose-headers"), /Content-Range/);
  assert.equal(response.headers.get("content-range"), "bytes 0-3/8");
  assert.equal(response.headers.get("set-cookie"), null);
  const upstream = calls.at(-1);
  assert.equal(upstream.url, "https://github.com/example/game-builds/releases/download/v1.0.0-web/pack-ab.vpack");
  assert.equal(upstream.init.headers.get("Range"), "bytes=0-3");
  assert.equal(upstream.init.headers.get("Cookie"), null);
});

test("unknown apps, bad paths and other methods are refused without contacting GitHub", async () => {
  const calls = [];
  const proxy = createProxy({ fetchImpl: fakeFetch(calls) });
  const status = async (url, init) => (await proxy.fetch(new Request(url, init), { CATALOGUE })).status;
  assert.equal(await status("https://proxy.example/other/v1/pack.vpack"), 404);
  assert.equal(await status("https://proxy.example/demo/v1/../pack.vpack"), 400);
  assert.equal(await status("https://proxy.example/demo/v1/pack.vpack?x=1"), 400);
  assert.equal(await status("https://proxy.example/demo/v1/pack.vpack", { method: "POST" }), 405);
  assert.equal(await status("https://proxy.example/demo/v1/pack.vpack", { method: "OPTIONS" }), 204);
  assert.ok(calls.every((c) => c.url.startsWith(CATALOGUE)));
});

test("targets are cached per app for five minutes", async () => {
  const calls = [];
  let clock = 0;
  const proxy = createProxy({ fetchImpl: fakeFetch(calls), now: () => clock });
  const get = () => proxy.fetch(new Request("https://proxy.example/demo/v1/pack.vpack"), { CATALOGUE });
  await get();
  await get();
  clock += 5 * 60 * 1000;
  await get();
  assert.equal(calls.filter((c) => c.url.endsWith("targets.json")).length, 2);
});
