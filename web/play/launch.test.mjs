import { readFileSync } from "node:fs";
import { runInNewContext } from "node:vm";
import { expect, test } from "vitest";

// Run play/launch.js against a stub page and service worker; return the game frame's src.
async function launchFrameSrc(search) {
  const source = readFileSync(new URL("./launch.js", import.meta.url), "utf8");
  const listeners = [];
  let frame = null;
  const registration = {
    update: async () => {},
    installing: null,
    waiting: null,
    active: {
      postMessage: (message) =>
        queueMicrotask(() =>
          listeners.forEach((listener) =>
            listener({ data: { type: "vesper-ready", id: message.id, entry: "index.html", version: "1", channel: "stable" } }),
          ),
        ),
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
  runInNewContext(source, page);
  for (let i = 0; i < 100 && !frame; i++) await new Promise((resolve) => setTimeout(resolve, 1));
  expect(frame).not.toBeNull();
  return frame.src;
}

test("launch page forwards non-reserved query keys to the game frame", async () => {
  expect(await launchFrameSrc("?app=no-doors-allowed&channel=stable&code=AB12&name=a%20b%26c")).toBe(
    "/run/no-doors-allowed/index.html?code=AB12&name=a+b%26c",
  );
  expect(await launchFrameSrc("?app=no-doors-allowed&channel=stable")).toBe("/run/no-doors-allowed/index.html");
});
