CREATE TABLE IF NOT EXISTS users (id TEXT PRIMARY KEY, github_id TEXT UNIQUE, email TEXT UNIQUE, display_name TEXT, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (token_hash TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id), expires_at INTEGER NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS apps (app_id TEXT PRIMARY KEY, developer_pubkey TEXT NOT NULL, current_key_id INTEGER NOT NULL DEFAULT 1, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS depot_keys (app_id TEXT NOT NULL REFERENCES apps(app_id), key_id INTEGER NOT NULL, encrypted_key TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY (app_id, key_id));
CREATE TABLE IF NOT EXISTS grants (user_id TEXT NOT NULL REFERENCES users(id), app_id TEXT NOT NULL REFERENCES apps(app_id), channel TEXT NOT NULL, status TEXT NOT NULL, source TEXT NOT NULL, source_ref TEXT, created_at INTEGER NOT NULL, PRIMARY KEY (user_id, app_id, channel));
CREATE TABLE IF NOT EXISTS redeem_codes (code_hash TEXT PRIMARY KEY, app_id TEXT NOT NULL, channel TEXT NOT NULL, redeemed_by TEXT, redeemed_at INTEGER, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS webhook_events (event_id TEXT PRIMARY KEY, provider TEXT NOT NULL, received_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS email_codes (email TEXT PRIMARY KEY, code_hash TEXT NOT NULL, attempts INTEGER NOT NULL DEFAULT 0, expires_at INTEGER NOT NULL, created_at INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS webhook_configs (app_id TEXT PRIMARY KEY, provider TEXT NOT NULL, secret TEXT NOT NULL, created_at INTEGER NOT NULL);
