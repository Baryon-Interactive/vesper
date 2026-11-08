import { dbOne, dbRun, Env, json, now, Row, sha256Hex } from "./common";
import { masterCrypto } from "./crypto";
import { sessionUser } from "./auth";

export async function handleGrantRoutes(
  request: Request,
  env: Env,
  path: string,
  body: string,
): Promise<Response | undefined> {
  if (path === "/me/grants" && request.method === "GET") {
    const user = await sessionUser(request, env);
    if (!user) return json({ error: "unauthorized" }, 401);
    const rows = await env.DB.prepare(
      "SELECT app_id,channel,status,source,source_ref,created_at FROM grants WHERE user_id=? ORDER BY app_id,channel",
    )
      .bind(user.id)
      .all();
    return json({ grants: rows.results });
  }

  const redeem = path.match(/^\/apps\/([^/]+)\/redeem$/);
  if (redeem && request.method === "POST") {
    const user = await sessionUser(request, env);
    if (!user) return json({ error: "unauthorized" }, 401);
    const appId = redeem[1];
    const value = JSON.parse(body) as Row;
    const codeHash = await import("./common").then(({ sha256Hex }) =>
      sha256Hex(String(value.code ?? "")),
    );
    const row = await dbOne(
      env.DB,
      "SELECT * FROM redeem_codes WHERE code_hash=? AND app_id=? AND redeemed_by IS NULL",
      await codeHash,
      appId,
    );
    if (!row) return json({ error: "invalid or redeemed code" }, 400);
    const update = await dbRun(
      env.DB,
      "UPDATE redeem_codes SET redeemed_by=?,redeemed_at=? WHERE code_hash=? AND redeemed_by IS NULL",
      user.id,
      now(),
      row.code_hash,
    );
    if (!update.success || update.meta.changes !== 1) {
      return json({ error: "invalid or redeemed code" }, 400);
    }
    await dbRun(
      env.DB,
      "INSERT INTO grants(user_id,app_id,channel,status,source,source_ref,created_at) " +
        "VALUES(?,?,?,?,?,?,?) ON CONFLICT(user_id,app_id,channel) DO UPDATE SET " +
        "status=excluded.status",
      user.id,
      appId,
      row.channel,
      "active",
      "redeem",
      row.code_hash,
      now(),
    );
    return json({ redeemed: true, app_id: appId, channel: row.channel });
  }

  const key = path.match(/^\/apps\/([^/]+)\/key$/);
  if (key && request.method === "POST") {
    const user = await sessionUser(request, env);
    if (!user) return json({ error: "unauthorized" }, 401);
    const appId = key[1];
    const value = body ? (JSON.parse(body) as Row) : {};
    const grant = await dbOne(
      env.DB,
      "SELECT 1 FROM grants WHERE user_id=? AND app_id=? AND status='active'",
      user.id,
      appId,
    );
    if (!grant) return json({ error: "active grant required" }, 403);
    const app = await dbOne(env.DB, "SELECT * FROM apps WHERE app_id=?", appId);
    if (!app) return json({ error: "unknown app" }, 404);
    const keyId = Number(value.key_id ?? app.current_key_id);
    const stored = await dbOne(
      env.DB,
      "SELECT encrypted_key FROM depot_keys WHERE app_id=? AND key_id=?",
      appId,
      keyId,
    );
    if (!stored) return json({ error: "unknown key" }, 404);
    return json({
      app_id: appId,
      key_id: keyId,
      depot_key_hex: await masterCrypto(
        env.MASTER_KEY,
        "decrypt",
        String(stored.encrypted_key),
      ),
    });
  }

  return undefined;
}
