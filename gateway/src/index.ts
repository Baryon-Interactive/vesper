import { handleAuth } from "./auth";
import { canonical, masterCrypto } from "./crypto";
import { handleDeveloper } from "./developer";
import { handleGrantRoutes } from "./grants";
import { Env, json, sha256Hex } from "./common";
import { handleWebhook } from "./webhooks";

export async function handle(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  const body = request.method === "GET" ? "" : await request.text();
  try {
    const auth = await handleAuth(request, env, url, body);
    if (auth) return auth;
    const grants = await handleGrantRoutes(request, env, url.pathname, body);
    if (grants) return grants;
    const developer = await handleDeveloper(request, env, url.pathname, body);
    if (developer) return developer;
    const webhook = await handleWebhook(request, env, url.pathname, body);
    if (webhook) return webhook;
    return json({ error: "not found" }, 404);
  } catch (error) {
    console.error(error);
    return json({ error: "internal error" }, 500);
  }
}

export default { fetch: handle };
export { canonical, masterCrypto, sha256Hex };
