import { beforeAll } from "vitest";
import { applyD1Migrations, env } from "cloudflare:test";

beforeAll(async () => {
  await applyD1Migrations(env.DB, env.TEST_MIGRATIONS);
  await env.DB.prepare(
    "INSERT OR IGNORE INTO apps(app_id,developer_pubkey,current_key_id,created_at) VALUES(?,?,?,?)",
  )
    .bind("demo", "00".repeat(32), 1, Math.floor(Date.now() / 1000))
    .run();
});
