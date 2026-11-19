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

// Run play/launch.js against a stub page. `workers` are the stub service
// workers, each `(message, answer) => void`; the first is active. Timers only
// run when the test fires them. Returns the page so a test can drive it.
function launchPage(search, workers) {
  const listeners = { message: [], controllerchange: [] };
  const timers = [];
  const page = { frame: null, status: {}, sent: [], timers };
  const answer = (message) => queueMicrotask(() => listeners.message.forEach((listener) => listener({ data: message })));
  const stub = (handle) => ({ postMessage: (message) => (page.sent.push(message), handle(message, answer)) });
  const registration = {
    update: async () => {},
    installing: null,
    waiting: null,
    active: stub(workers[0]),
    addEventListener: () => {},
    removeEventListener: () => {},
  };
  page.replaceWorker = (handle) => {
    registration.active = stub(handle);
    listeners.controllerchange.forEach((listener) => listener({}));
  };
  page.fireTimer = (ms) => {
    const index = timers.findIndex((timer) => timer.ms === ms && !timer.cleared);
    expect(index).toBeGreaterThanOrEqual(0);
    timers[index].cleared = true;
    timers[index].fn();
  };
  const context = {
    location: { search, hostname: "play.example" },
    document: {
      referrer: "",
      getElementById: () => page.status,
      createElement: () => ({ addEventListener: () => {} }),
      body: { appendChild: (element) => (page.frame = element) },
    },
    navigator: {
      serviceWorker: {
        register: async () => registration,
        ready: Promise.resolve(registration),
        controller: null,
        addEventListener: (type, listener) => listeners[type]?.push(listener),
        removeEventListener: (type, listener) => {
          const list = listeners[type] || [];
          if (list.includes(listener)) list.splice(list.indexOf(listener), 1);
        },
      },
    },
    fetch: async () => ({ ok: true, json: async () => ({}) }),
    URLSearchParams,
    setTimeout: (fn, ms) => timers.push({ fn, ms }),
    clearTimeout: (id) => id && timers[id - 1] && (timers[id - 1].cleared = true),
    console,
  };
  context.window = context;
  runInNewContext(read("launch.js"), context);
  return page;
}

const settle = () => new Promise((resolve) => setTimeout(resolve, 5));
const answers = (ready) => (message, answer) => answer({ ...ready, id: message.id });
const silent = () => {};

// Return the game frame's src once the page has made it.
async function launchFrameSrc(search, ready) {
  const page = launchPage(search, [answers(ready)]);
  for (let i = 0; i < 100 && !page.frame; i++) await settle();
  expect(page.frame).not.toBeNull();
  return page.frame.src;
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

test("the launch page asks a worker that replaces the one that never answered", async () => {
  const ready = await prepare({});
  const page = launchPage("?app=no-doors-allowed", [silent]);
  await settle();
  expect(page.sent).toHaveLength(1);
  page.replaceWorker(answers(ready));
  await settle();
  expect(page.sent).toHaveLength(2);
  expect(page.sent[1].id).not.toBe(page.sent[0].id);
  expect(page.frame?.src).toBe("/run/no-doors-allowed/index.html");
});

test("the launch page retries a silent worker once, then asks for a reload", async () => {
  const page = launchPage("?app=no-doors-allowed", [silent]);
  await settle();
  expect(page.sent).toHaveLength(1);
  page.fireTimer(45000);
  await settle();
  expect(page.sent).toHaveLength(2);
  page.fireTimer(45000);
  await settle();
  expect(page.frame).toBeNull();
  expect(page.status).toMatchObject({ className: "error", textContent: "Checking signatures took too long. Reload the page to try again." });
});

test("an answer to the first request still counts after a retry", async () => {
  const ready = await prepare({});
  let first = null;
  const page = launchPage("?app=no-doors-allowed", [(message, answer) => (first ??= () => answer({ ...ready, id: message.id }))]);
  await settle();
  page.fireTimer(45000);
  await settle();
  first();
  await settle();
  expect(page.frame?.src).toBe("/run/no-doors-allowed/index.html");
});
