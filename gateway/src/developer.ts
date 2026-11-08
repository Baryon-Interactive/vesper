import {
  dbOne,
  dbRun,
  Env,
  json,
  now,
  randomToken,
  Row,
  sha256Hex,
  unb64,
} from "./common";
import { canonical, importEd25519, masterCrypto } from "./crypto";
import { appDeveloperKey } from "./routes";

export async function developerVerify(
  request: Request,
  env: Env,
  appId: string,
  body: string,
): Promise<boolean> {
  const timestamp = request.headers.get("x-vesper-timestamp");
  const signature = request.headers.get("x-vesper-signature");
  const requestId = request.headers.get("x-vesper-request-id");
  if (
    !timestamp ||
    !signature ||
    !requestId ||
    !/^\d+$/.test(timestamp) ||
    Math.abs(now() - Number(timestamp)) > 300
  ) {
    return false;
  }
  if (
    await dbOne(
      env.DB,
      "SELECT event_id FROM webhook_events WHERE event_id=?",
      `dev:${requestId}`,
    )
  ) {
    return false;
  }
  let keyHex: string | null;
  try {
    keyHex = await appDeveloperKey(env, appId);
  } catch {
    return false;
  }
  if (!keyHex) return false;
  try {
    const key = await importEd25519(keyHex, ["verify"]);
    const valid = await crypto.subtle.verify(
      "Ed25519",
      key,
      unb64(signature),
      new TextEncoder().encode(
        canonical(
          request.method,
          new URL(request.url).pathname,
          timestamp,
          await sha256Hex(body),
        ),
      ),
    );
    if (!valid) return false;
    await dbRun(
      env.DB,
      "INSERT INTO webhook_events(event_id,provider,received_at) VALUES(?,?,?)",
      `dev:${requestId}`,
      "developer-request",
      now(),
    );
    return true;
  } catch {
    return false;
  }
}

export async function handleDeveloper(
  request: Request,
  env: Env,
  path: string,
  body: string,
): Promise<Response | undefined> {
  const match = path.match(/^\/dev\/apps\/([^/]+)\/(.+)$/);
  if (!match || request.method === "GET") return undefined;
  const [, appId, action] = match;
  if (!(await developerVerify(request, env, appId, body))) {
    return json({ error: "invalid developer signature" }, 401);
  }

  if (action === "depot-key" && request.method === "PUT") {
    const value = JSON.parse(body) as Row;
    if (
      typeof value.key_id !== "number" ||
      !/^[0-9a-f]{64}$/i.test(String(value.depot_key_hex ?? ""))
    ) {
      return json(
        { error: "key_id and a 32-byte depot_key_hex are required" },
        400,
      );
    }
    const encrypted = await masterCrypto(
      env.MASTER_KEY,
      "encrypt",
      String(value.depot_key_hex),
    );
    await dbRun(
      env.DB,
      "INSERT INTO depot_keys(app_id,key_id,encrypted_key,created_at) " +
        "VALUES(?,?,?,?) ON CONFLICT(app_id,key_id) DO UPDATE SET " +
        "encrypted_key=excluded.encrypted_key",
      appId,
      value.key_id,
      encrypted,
      now(),
    );
    await dbRun(
      env.DB,
      "UPDATE apps SET current_key_id=? WHERE app_id=? AND current_key_id<?",
      value.key_id,
      appId,
      value.key_id,
    );
    return json({ stored: true });
  }

  if (action === "codes" && request.method === "POST") {
    const value = JSON.parse(body) as Row;
    const count = Math.min(1000, Math.max(1, Number(value.count ?? 1)));
    const codes: string[] = [];
    for (let index = 0; index < count; index += 1) {
      const code = `VESPER-${randomToken(12).toUpperCase()}`;
      codes.push(code);
      await dbRun(
        env.DB,
        "INSERT INTO redeem_codes(code_hash,app_id,channel,created_at) VALUES(?,?,?,?)",
        await sha256Hex(code),
        appId,
        String(value.channel ?? "stable"),
        now(),
      );
    }
    return json({ codes });
  }

  if (action === "grants/revoke" && request.method === "POST") {
    const value = JSON.parse(body) as Row;
    await dbRun(
      env.DB,
      "UPDATE grants SET status='revoked' WHERE user_id=? AND app_id=? AND channel=?",
      String(value.user_id),
      appId,
      String(value.channel ?? "stable"),
    );
    return json({ revoked: true });
  }

  if (action === "webhook" && request.method === "PUT") {
    const value = JSON.parse(body) as Row;
    if (value.provider !== "stripe" || typeof value.secret !== "string") {
      return json({ error: "stripe secret required" }, 400);
    }
    await dbRun(
      env.DB,
      "INSERT INTO webhook_configs(app_id,provider,secret,created_at) VALUES(?,?,?,?) ON CONFLICT(app_id) DO UPDATE SET secret=excluded.secret",
      appId,
      value.provider,
      value.secret,
      now(),
    );
    return json({ configured: true });
  }

  return json({ error: "not found" }, 404);
}
