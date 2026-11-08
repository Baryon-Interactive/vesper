declare module "cloudflare:test" {
  export const SELF: {
    fetch(input: RequestInfo | URL, init?: RequestInit): Promise<Response>;
  };
  export const env: import("../src/common").Env & {
    TEST_MIGRATIONS: unknown[];
  };
  export function applyD1Migrations(
    db: D1Database,
    migrations: unknown[],
  ): Promise<void>;
}
