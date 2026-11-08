import { beforeEach, describe, expect, it, vi } from "vitest";
import { env, SELF } from "cloudflare:test";
import { canonical, sha256Hex } from "../src/index";

const textEncoder = new TextEncoder();
const hex = (bytes: Uint8Array) =>
  [...bytes].map((value) => value.toString(16).padStart(2, "0")).join("");
const base64 = (bytes: Uint8Array) =>
  btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");
const now = () => Math.floor(Date.now() / 1000);

let rootPair: CryptoKeyPair;
let developerPair: CryptoKeyPair;
let developerPublicHex = "";

async function publicHex(key: CryptoKey) {
  return hex(new Uint8Array(await crypto.subtle.exportKey("raw", key)));
}

async function signedIndex(rootValid = true, app = "demo") {
  rootPair = await crypto.subtle.generateKey(
    { name: "Ed25519", namedCurve: "Ed25519" },
    true,
    ["sign", "verify"],
  );
  developerPair = await crypto.subtle.generateKey(
    { name: "Ed25519", namedCurve: "Ed25519" },
    true,
    ["sign", "verify"],
  );
  const rootPublic = await publicHex(rootPair.publicKey);
  developerPublicHex = await publicHex(developerPair.publicKey);
  const root = {
    keys: { root: rootPublic },
    roles: { root: { threshold: 1, keyids: ["root"] } },
    signatures: [] as unknown[],
  };
  const routes = {
    version: 1,
    expires: now() + 86400,
    apps: {
      [app]: { keys: [developerPublicHex], threshold: 1 },
    },
    roles: { root: { threshold: 1, keyids: ["root"] } },
    signatures: [] as unknown[],
  };
  const sign = async (value: Record<string, unknown>, valid: boolean) => {
    const unsigned = { ...value };
    delete unsigned.signatures;
    const data = textEncoder.encode(JSON.stringify(unsigned));
    const signature = await crypto.subtle.sign(
      "Ed25519",
      rootPair.privateKey,
      data,
    );
    return valid
      ? [{ keyid: "root", sig: base64(new Uint8Array(signature)) }]
      : [{ keyid: "wrong", sig: base64(new Uint8Array(signature)) }];
  };
  root.signatures = await sign(root, rootValid);
  routes.signatures = await sign(routes, rootValid);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/root.json")) {
        return new Response(JSON.stringify(root), { status: 200 });
      }
      if (url.endsWith("/routes.json")) {
        return new Response(JSON.stringify(routes), { status: 200 });
      }
      return new Response("not mocked", { status: 404 });
    }),
  );
}

async function request(path: string, init: RequestInit = {}, cookie?: string) {
  const headers = new Headers(init.headers);
  if (cookie) headers.set("cookie", cookie);
  return SELF.fetch(
    new Request(`https://gateway.test${path}`, { ...init, headers }),
  );
}

function sessionCookie(response: Response) {
  return response.headers.get("set-cookie")?.split(";")[0] ?? "";
}

async function login(email = "player@example.com") {
  const started = await request("/auth/email/start", {
    method: "POST",
    body: JSON.stringify({ email }),
  });
  const code = ((await started.json()) as { dev_code: string }).dev_code;
  const verified = await request("/auth/email/verify", {
    method: "POST",
    body: JSON.stringify({ email, code }),
  });
  return sessionCookie(verified);
}

async function developerRequest(
  path: string,
  method: string,
  body: unknown,
  requestId: string = crypto.randomUUID(),
  timestamp = String(now()),
  signatureOverride?: string,
) {
  const raw = JSON.stringify(body);
  const data = textEncoder.encode(
    canonical(method, path, timestamp, await sha256Hex(raw)),
  );
  const signature = await crypto.subtle.sign(
    "Ed25519",
    developerPair.privateKey,
    data,
  );
  return request(path, {
    method,
    body: raw,
    headers: {
      "x-vesper-timestamp": timestamp,
      "x-vesper-request-id": requestId,
      "x-vesper-signature": signatureOverride ?? base64(new Uint8Array(signature)),
    },
  });
}

beforeEach(async () => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  for (const table of [
    "sessions",
    "grants",
    "depot_keys",
    "redeem_codes",
    "webhook_events",
    "email_codes",
    "webhook_configs",
    "users",
    "apps",
  ]) {
    await env.DB.prepare(`DELETE FROM ${table}`).run();
  }
  await env.DB.prepare(
    "INSERT INTO apps(app_id,developer_pubkey,current_key_id,created_at) VALUES(?,?,?,?) ON CONFLICT(app_id) DO NOTHING",
  )
    .bind("demo", "00".repeat(32), 1, now())
    .run()
    .catch(() => undefined);
});

describe("gateway integration scenarios", () => {
  it("GitHub OAuth checks state and stores only a session hash", async () => {
    const start = await request("/auth/github/start", { redirect: "manual" });
    expect(start.status).toBe(302);
    const state = start.headers
      .get("set-cookie")
      ?.match(/oauth_state=([^;]+)/)?.[1];
    expect(state).toBeTruthy();
    const rejected = await request("/auth/github/callback?state=wrong&code=x", {
      headers: { cookie: `oauth_state=${state}` },
    });
    expect(rejected.status).toBe(400);

    vi.stubGlobal(
      "fetch",
      vi.fn(async (input: RequestInfo | URL) => {
        if (String(input).includes("access_token")) {
          return new Response(JSON.stringify({ access_token: "oauth-token" }));
        }
        return new Response(
          JSON.stringify({ id: 7, login: "octo", email: "o@example.com" }),
        );
      }),
    );
    const accepted = await request(
      `/auth/github/callback?state=${state}&code=ok`,
      { headers: { cookie: `oauth_state=${state}` } },
    );
    expect(accepted.status).toBe(200);
    const row = await env.DB.prepare("SELECT token_hash FROM sessions").first<{
      token_hash: string;
    }>();
    expect(row?.token_hash).toMatch(/^[0-9a-f]{64}$/);
    expect(row?.token_hash).not.toContain("oauth-token");
  });

  it("email codes count failures, expire, lock, and accept the correct code", async () => {
    const started = await request("/auth/email/start", {
      method: "POST",
      body: JSON.stringify({ email: "mail@example.com" }),
    });
    const code = ((await started.json()) as { dev_code: string }).dev_code;
    for (let index = 0; index < 5; index += 1) {
      const response = await request("/auth/email/verify", {
        method: "POST",
        body: JSON.stringify({ email: "mail@example.com", code: "000000" }),
      });
      expect(response.status).toBe(401);
    }
    const locked = await request("/auth/email/verify", {
      method: "POST",
      body: JSON.stringify({ email: "mail@example.com", code }),
    });
    expect(locked.status).toBe(401);

    const second = await request("/auth/email/start", {
      method: "POST",
      body: JSON.stringify({ email: "expire@example.com" }),
    });
    const expiredCode = ((await second.json()) as { dev_code: string })
      .dev_code;
    await env.DB.prepare("UPDATE email_codes SET expires_at=? WHERE email=?")
      .bind(now() - 1, "expire@example.com")
      .run();
    expect(
      (
        await request("/auth/email/verify", {
          method: "POST",
          body: JSON.stringify({
            email: "expire@example.com",
            code: expiredCode,
          }),
        })
      ).status,
    ).toBe(401);
    expect((await login("ok@example.com")).length).toBeGreaterThan(10);
    const stored = await env.DB.prepare(
      "SELECT code_hash FROM email_codes",
    ).all();
    expect(
      stored.results.every((row) =>
        /^[0-9a-f]{64}$/.test(String((row as { code_hash: string }).code_hash)),
      ),
    ).toBe(true);
  });

  it("developer codes redeem once and the player receives a grant", async () => {
    await signedIndex();
    const codes = await developerRequest("/dev/apps/demo/codes", "POST", {
      count: 1,
    });
    expect(codes.status).toBe(200);
    const code = ((await codes.json()) as { codes: string[] }).codes[0];
    const cookie = await login();
    const redeemed = await request(
      "/apps/demo/redeem",
      {
        method: "POST",
        body: JSON.stringify({ code }),
      },
      cookie,
    );
    expect(redeemed.status).toBe(200);
    const again = await request(
      "/apps/demo/redeem",
      {
        method: "POST",
        body: JSON.stringify({ code }),
      },
      cookie,
    );
    expect(again.status).toBe(400);
    const stored = await env.DB.prepare(
      "SELECT code_hash FROM redeem_codes",
    ).first<{ code_hash: string }>();
    expect(stored?.code_hash).toHaveLength(64);
    expect(stored?.code_hash).not.toContain(code);
  });

  it("key endpoint enforces session, grant, old key selection, and revoke", async () => {
    await signedIndex();
    const key1 = "11".repeat(32);
    const key2 = "22".repeat(32);
    await developerRequest("/dev/apps/demo/depot-key", "PUT", {
      key_id: 1,
      depot_key_hex: key1,
    });
    await developerRequest("/dev/apps/demo/depot-key", "PUT", {
      key_id: 2,
      depot_key_hex: key2,
    });
    expect((await request("/apps/demo/key", { method: "POST" })).status).toBe(
      401,
    );
    const cookie = await login("key@example.com");
    expect(
      (await request("/apps/demo/key", { method: "POST" }, cookie)).status,
    ).toBe(403);
    await env.DB.prepare(
      "INSERT INTO grants(user_id,app_id,channel,status,source,created_at) SELECT id,'demo','stable','active','test',? FROM users WHERE email=?",
    )
      .bind(now(), "key@example.com")
      .run();
    const current = await request("/apps/demo/key", { method: "POST" }, cookie);
    expect(
      ((await current.json()) as { depot_key_hex: string }).depot_key_hex,
    ).toBe(key2);
    const old = await request(
      "/apps/demo/key",
      {
        method: "POST",
        body: JSON.stringify({ key_id: 1 }),
      },
      cookie,
    );
    expect(
      ((await old.json()) as { depot_key_hex: string }).depot_key_hex,
    ).toBe(key1);
    const user = await env.DB.prepare("SELECT id FROM users WHERE email=?")
      .bind("key@example.com")
      .first<{ id: string }>();
    await developerRequest("/dev/apps/demo/grants/revoke", "POST", {
      user_id: user?.id,
      channel: "stable",
    });
    expect(
      (await request("/apps/demo/key", { method: "POST" }, cookie)).status,
    ).toBe(403);
  });

  it("Stripe rejects bad signatures, deduplicates events, and revokes refunds", async () => {
    await signedIndex();
    const cookie = await login("stripe@example.com");
    const user = await env.DB.prepare("SELECT id FROM users WHERE email=?")
      .bind("stripe@example.com")
      .first<{ id: string }>();
    await developerRequest("/dev/apps/demo/webhook", "PUT", {
      provider: "stripe",
      secret: "stripe-secret",
    });
    const event = JSON.stringify({
      id: "evt-1",
      type: "checkout.session.completed",
      data: { object: { metadata: { user_id: user?.id, channel: "stable" } } },
    });
    expect(
      (
        await request("/webhooks/stripe/demo", {
          method: "POST",
          body: event,
          headers: { "stripe-signature": "t=1,v1=bad" },
        })
      ).status,
    ).toBe(400);
    const timestamp = String(now());
    const key = await crypto.subtle.importKey(
      "raw",
      textEncoder.encode("stripe-secret"),
      { name: "HMAC", hash: "SHA-256" },
      false,
      ["sign"],
    );
    const staleMac = await crypto.subtle.sign(
      "HMAC",
      key,
      textEncoder.encode(`1.${event}`),
    );
    expect(
      (
        await request("/webhooks/stripe/demo", {
          method: "POST",
          body: event,
          headers: {
            "stripe-signature": `t=1,v1=${hex(new Uint8Array(staleMac))}`,
          },
        })
      ).status,
    ).toBe(400);
    const mac = await crypto.subtle.sign(
      "HMAC",
      key,
      textEncoder.encode(`${timestamp}.${event}`),
    );
    const signature = `t=${timestamp},v1=${hex(new Uint8Array(mac))}`;
    expect(
      (
        await request("/webhooks/stripe/demo", {
          method: "POST",
          body: event,
          headers: { "stripe-signature": signature },
        })
      ).status,
    ).toBe(200);
    expect(
      (
        await request("/webhooks/stripe/demo", {
          method: "POST",
          body: event,
          headers: { "stripe-signature": signature },
        })
      ).status,
    ).toBe(200);
    const refund = JSON.stringify({
      id: "evt-2",
      type: "charge.refunded",
      data: { object: { metadata: { user_id: user?.id } } },
    });
    const refundMac = await crypto.subtle.sign(
      "HMAC",
      key,
      textEncoder.encode(`${timestamp}.${refund}`),
    );
    await request("/webhooks/stripe/demo", {
      method: "POST",
      body: refund,
      headers: {
        "stripe-signature": `t=${timestamp},v1=${hex(new Uint8Array(refundMac))}`,
      },
    });
    const grant = await env.DB.prepare(
      "SELECT status FROM grants WHERE user_id=? AND app_id='demo'",
    )
      .bind(user?.id)
      .first<{ status: string }>();
    expect(grant?.status).toBe("revoked");
    expect(cookie).toContain("vesper_session=");
  });

  it("developer signatures reject wrong, stale, replayed, unknown, and bad-root requests", async () => {
    await signedIndex();
    const body = { key_id: 1, depot_key_hex: "33".repeat(32) };
    const good = await developerRequest(
      "/dev/apps/demo/depot-key",
      "PUT",
      body,
    );
    expect(good.status).toBe(200);
    const wrong = await developerRequest(
      "/dev/apps/demo/depot-key",
      "PUT",
      body,
      crypto.randomUUID(),
      String(now()),
      "bad-signature",
    );
    expect(wrong.status).toBe(401);
    const replay = await developerRequest(
      "/dev/apps/demo/depot-key",
      "PUT",
      body,
      "replayed",
    );
    expect(replay.status).toBe(200);
    const replayAgain = await developerRequest(
      "/dev/apps/demo/depot-key",
      "PUT",
      body,
      "replayed",
    );
    expect(replayAgain.status).toBe(401);
    expect(
      (await developerRequest("/dev/apps/unknown/depot-key", "PUT", body))
        .status,
    ).toBe(401);
    expect(
      (
        await developerRequest(
          "/dev/apps/demo/depot-key",
          "PUT",
          body,
          "stale",
          String(now() - 301),
        )
      ).status,
    ).toBe(401);
    await signedIndex(false);
    expect(
      (await developerRequest("/dev/apps/demo/depot-key", "PUT", body)).status,
    ).toBe(401);
  });

  it("DepotKeys remain ciphertext in D1", async () => {
    await signedIndex();
    const plain = "44".repeat(32);
    await developerRequest("/dev/apps/demo/depot-key", "PUT", {
      key_id: 1,
      depot_key_hex: plain,
    });
    const row = await env.DB.prepare(
      "SELECT encrypted_key FROM depot_keys WHERE app_id='demo'",
    ).first<{ encrypted_key: string }>();
    expect(row?.encrypted_key).toBeTruthy();
    expect(row?.encrypted_key).not.toContain(plain);
    expect(row?.encrypted_key).not.toMatch(/^(44)+$/);
  });
});
