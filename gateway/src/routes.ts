import { dbOne, Env, Row } from "./common";
import { importEd25519, verifyThreshold } from "./crypto";

export async function loadRoutes(env: Env): Promise<Row> {
  const root = (await (
    await fetch(new URL("root.json", env.INDEX_BASE_URL))
  ).json()) as Row;
  const routes = (await (
    await fetch(new URL("routes.json", env.INDEX_BASE_URL))
  ).json()) as Row;
  const keys = (root.keys ?? {}) as Record<string, string>;
  const rootRole = (root.roles as Row | undefined)?.root as Row | undefined;
  const threshold = Number(rootRole?.threshold ?? 1);
  if (env.INDEX_ROOT_KEY) {
    const configured = env.INDEX_ROOT_KEY.toLowerCase();
    const rootKeys = Object.values(keys)
      .map(String)
      .map((key) => key.toLowerCase());
    if (rootKeys.length && !rootKeys.includes(configured)) {
      throw new Error("configured root key is not in root metadata");
    }
  }
  if (!(await verifyThreshold(root, root.signatures, keys, threshold))) {
    throw new Error("root signature verification failed");
  }
  const rootKeyMap: Record<string, string> = {};
  for (const keyId of (rootRole?.keyids as unknown[] | undefined) ?? []) {
    if (keys[String(keyId)])
      rootKeyMap[String(keyId)] = String(keys[String(keyId)]);
  }
  const routeRole = (routes.roles as Row | undefined)?.root as Row | undefined;
  const routeThreshold = Number(routeRole?.threshold ?? 1);
  if (
    !(await verifyThreshold(
      routes,
      routes.signatures,
      Object.keys(rootKeyMap).length ? rootKeyMap : keys,
      routeThreshold,
    ))
  ) {
    throw new Error("routes signature verification failed");
  }
  return routes;
}

export async function appDeveloperKey(
  env: Env,
  appId: string,
): Promise<string | null> {
  const routes = await loadRoutes(env);
  const app = (routes.apps as Row | undefined)?.[appId] as Row | undefined;
  const keys = app?.keys;
  return Array.isArray(keys) && keys.length ? String(keys[0]) : null;
}

export async function validateDeveloperKey(
  env: Env,
  appId: string,
): Promise<boolean> {
  const key = await appDeveloperKey(env, appId);
  if (!key) return false;
  try {
    await importEd25519(key, ["verify"]);
    return true;
  } catch {
    return false;
  }
}
