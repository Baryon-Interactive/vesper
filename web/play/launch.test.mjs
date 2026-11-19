import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { expect, test } from "vitest";

const read = (name) => readFileSync(new URL(`./${name}`, import.meta.url), "utf8");

// Run play/sw.js with a stub VesperCore whose verified listing is `store`; return
// the worker's reply to the launch page's prepare request.
async function prepare(store) {
  const handlers = {};
  const worker = {
    importScripts: () => {},
    VesperCore: {
      setProgress: () => {},
      resetStats: () => {},
      openBuild: async () => ({ summary: { version: "1", files: [], launch: null }, channelName: "stable", store }),
    },
    caches: { open: async () => ({ put: async () => {}, match: async () => undefined }) },
    Response: class {},
    URL,
    console,
  };
  worker.self = { addEventListener: (type, handler) => (handlers[type] = handler), location: { origin: "https://play.example" } };
  runInNewContext(read("sw.js"), worker);
  let reply;
  let work;
  handlers.message({
    data: { type: "vesper-prepare", appId: "no-doors-allowed", channel: "stable", id: "1" },
    source: { postMessage: (message) => (reply = message) },
    waitUntil: (promise) => (work = promise),
  });
  await work;
  return reply;
}

// Run play/launch.js against a stub page whose worker answers with `ready`;
// return the game frame's src.
async function launchFrameSrc(search, ready) {
  const listeners = [];
  let frame = null;
  const registration = {
    update: async () => {},
    installing: null,
    waiting: null,
    active: {
      postMessage: (message) =>
        queueMicrotask(() => listeners.forEach((listener) => listener({ data: { ...ready, id: message.id } }))),
    },
  };
  const page = {
    location: { search, hostname: "play.example" },
    document: {
      referrer: "",
      getElementById: () => ({}),
      createElement: () => ({ addEventListener: () => {} }),
      body: { appendChild: (element) => (frame = element) },
    },
    navigator: {
      serviceWorker: {
        register: async () => registration,
        ready: Promise.resolve(registration),
        addEventListener: (_type, listener) => listeners.push(listener),
        removeEventListener: () => {},
      },
    },
    fetch: async () => ({ ok: true, json: async () => ({}) }),
    URLSearchParams,
    setTimeout,
    console,
  };
  page.window = page;
  runInNewContext(read("launch.js"), page);
  for (let i = 0; i < 100 && !frame; i++) await new Promise((resolve) => setTimeout(resolve, 1));
  expect(frame).not.toBeNull();
  return frame.src;
}

const invite = `?app=no-doors-allowed&channel=stable&code=AB12&name=a%20b%26c&newrun=1&lobby=wss%3A%2F%2Fother.example&long=${"x".repeat(257)}`;

test("the worker reports the listing's play_params and drops keys that break the rule", async () => {
  const reply = await prepare({ play_params: ["code", "name", "long", "app", "a b", 7] });
  expect(reply).toMatchObject({ type: "vesper-ready", entry: "index.html", playParams: ["code", "name", "long"] });
  expect((await prepare({})).playParams).toEqual([]);
});

test("the launch page forwards only the game's play_params to its frame", async () => {
  const ready = await prepare({ play_params: ["code", "name", "long"] });
  expect(await launchFrameSrc(invite, ready)).toBe("/run/no-doors-allowed/index.html?code=AB12&name=a+b%26c");
  expect(await launchFrameSrc("?app=no-doors-allowed&channel=stable", ready)).toBe("/run/no-doors-allowed/index.html");
});

test("the launch page forwards nothing for a game without play_params", async () => {
  expect(await launchFrameSrc(invite, await prepare({}))).toBe("/run/no-doors-allowed/index.html");
});
