import path from "node:path";
import {
  defineWorkersConfig,
  readD1Migrations,
} from "@cloudflare/vitest-pool-workers/config";

const migrations = await readD1Migrations(
  path.resolve(import.meta.dirname, "migrations"),
);

export default defineWorkersConfig({
  test: {
    pool: "./node_modules/@cloudflare/vitest-pool-workers/dist/pool/index.mjs",
    setupFiles: ["./test/setup.ts"],
    poolOptions: {
      workers: {
        main: "./src/index.ts",
        isolatedStorage: false,
        singleWorker: true,
        miniflare: {
          compatibilityDate: "2026-11-06",
          d1Databases: ["DB"],
          bindings: {
            MASTER_KEY: "test-master-key",
            INDEX_BASE_URL: "https://index.test/",
            GITHUB_CLIENT_ID: "client-id",
            GITHUB_CLIENT_SECRET: "client-secret",
            GITHUB_REDIRECT_URI: "https://gateway.test/auth/github/callback",
            DEV_MODE: "true",
            TEST_MIGRATIONS: migrations,
          },
        },
      },
    },
  },
});
