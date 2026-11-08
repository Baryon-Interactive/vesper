import { dbOne, dbRun, encoder, Env, hex, json, now, Row } from "./common";

async function stripeValid(
  raw: string,
  header: string,
  secret: string,
): Promise<boolean> {
  const parts = Object.fromEntries(
    header.split(",").map((part) => {
      const [key, value] = part.split("=", 2);
      return [key, value];
    }),
  );
  const timestamp = Number(parts.t);
  if (!timestamp || Math.abs(now() - timestamp) > 300 || !parts.v1)
    return false;
  const key = await crypto.subtle.importKey(
    "raw",
    encoder.encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const expected = hex(
    new Uint8Array(
      await crypto.subtle.sign(
        "HMAC",
        key,
        encoder.encode(`${timestamp}.${raw}`),
      ),
    ),
  );
  return expected === parts.v1;
}

export async function handleWebhook(
  request: Request,
  env: Env,
  path: string,
  body: string,
): Promise<Response | undefined> {
  const match = path.match(/^\/webhooks\/stripe\/([^/]+)$/);
  if (!match || request.method !== "POST") return undefined;
  const appId = match[1];
  const config = await dbOne(
    env.DB,
    "SELECT secret FROM webhook_configs WHERE app_id=?",
    appId,
  );
  if (
    !config ||
    !(await stripeValid(
      body,
      request.headers.get("stripe-signature") ?? "",
      String(config.secret),
    ))
  ) {
    return json({ error: "invalid stripe signature" }, 400);
  }
  const event = JSON.parse(body) as Row;
  const eventId = String(event.id ?? "");
  if (!eventId) return json({ error: "missing event id" }, 400);
  if (
    await dbOne(
      env.DB,
      "SELECT 1 FROM webhook_events WHERE event_id=?",
      eventId,
    )
  ) {
    return json({ duplicate: true });
  }
  await dbRun(
    env.DB,
    "INSERT INTO webhook_events(event_id,provider,received_at) VALUES(?,?,?)",
    eventId,
    "stripe",
    now(),
  );
  const type = String(event.type);
  const object = ((event.data as Row)?.object ?? {}) as Row;
  const metadata = (object.metadata ?? {}) as Row;
  const email = String(metadata.email ?? object.customer_email ?? "");
  const user = metadata.user_id
    ? { id: String(metadata.user_id) }
    : email
      ? await dbOne(env.DB, "SELECT id FROM users WHERE email=?", email)
      : null;
  if (user && type === "checkout.session.completed") {
    await dbRun(
      env.DB,
      "INSERT INTO grants(user_id,app_id,channel,status,source,source_ref,created_at) " +
        "VALUES(?,?,?,?,?,?,?) ON CONFLICT(user_id,app_id,channel) DO UPDATE SET " +
        "status=excluded.status",
      user.id,
      appId,
      String(metadata.channel ?? "stable"),
      "active",
      "stripe",
      eventId,
      now(),
    );
  }
  if (user && type === "charge.refunded") {
    await dbRun(
      env.DB,
      "UPDATE grants SET status='revoked' WHERE user_id=? AND app_id=?",
      user.id,
      appId,
    );
  }
  return json({ accepted: true });
}

export { stripeValid };
