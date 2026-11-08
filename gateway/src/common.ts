export interface Env {
  DB: D1Database;
  MASTER_KEY: string;
  INDEX_BASE_URL: string;
  INDEX_ROOT_KEY?: string;
  GITHUB_CLIENT_ID?: string;
  GITHUB_CLIENT_SECRET?: string;
  GITHUB_REDIRECT_URI?: string;
  DEV_MODE?: string;
  EMAIL_PROVIDER_URL?: string;
  EMAIL_PROVIDER_TOKEN?: string;
}

export type Row = Record<string, unknown>;
export const encoder = new TextEncoder();
export const now = () => Math.floor(Date.now() / 1000);

export const json = (body: unknown, status = 200, extra: HeadersInit = {}) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json; charset=utf-8", ...extra },
  });

export const text = (body: string, status = 200, extra: HeadersInit = {}) =>
  new Response(body, {
    status,
    headers: { "content-type": "text/plain; charset=utf-8", ...extra },
  });

export const b64 = (bytes: Uint8Array) =>
  btoa(String.fromCharCode(...bytes))
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/, "");

export const unb64 = (value: string) =>
  Uint8Array.from(
    atob(
      value.replaceAll("-", "+").replaceAll("_", "/") +
        "=".repeat((4 - (value.length % 4)) % 4),
    ),
    (character) => character.charCodeAt(0),
  );

export const hex = (bytes: Uint8Array) =>
  [...bytes].map((value) => value.toString(16).padStart(2, "0")).join("");

export const unhex = (value: string) =>
  Uint8Array.from(value.match(/.{2}/g) ?? [], (part) => parseInt(part, 16));

export async function digest(data: string | ArrayBuffer): Promise<Uint8Array> {
  return new Uint8Array(
    await crypto.subtle.digest(
      "SHA-256",
      typeof data === "string" ? encoder.encode(data) : data,
    ),
  );
}

export const sha256Hex = async (value: string) => hex(await digest(value));
export const randomToken = (size = 32) =>
  b64(crypto.getRandomValues(new Uint8Array(size)));

export async function dbOne<T extends Row = Row>(
  db: D1Database,
  sql: string,
  ...params: unknown[]
): Promise<T | null> {
  return (await db
    .prepare(sql)
    .bind(...params)
    .first()) as T | null;
}

export async function dbRun(db: D1Database, sql: string, ...params: unknown[]) {
  return db
    .prepare(sql)
    .bind(...params)
    .run();
}
