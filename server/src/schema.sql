PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS templates (
    id            TEXT PRIMARY KEY,
    slug          TEXT NOT NULL UNIQUE,
    name          TEXT NOT NULL,
    description   TEXT NOT NULL DEFAULT '',
    validity_days INTEGER,
    page_count    INTEGER NOT NULL,
    pdf_sha256    TEXT NOT NULL,
    fields        TEXT NOT NULL DEFAULT '[]',
    sections      TEXT NOT NULL DEFAULT '[]',
    source        TEXT,
    archived      INTEGER NOT NULL DEFAULT 0,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS persons (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    email      TEXT,
    zip        TEXT,
    notes      TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS discord_accounts (
    discord_id TEXT PRIMARY KEY,
    person_id  TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    username   TEXT,
    linked_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_discord_person ON discord_accounts(person_id);

CREATE TABLE IF NOT EXISTS submissions (
    id              TEXT PRIMARY KEY,
    template_id     TEXT NOT NULL REFERENCES templates(id) ON DELETE CASCADE,
    person_id       TEXT NOT NULL REFERENCES persons(id) ON DELETE CASCADE,
    token           TEXT NOT NULL UNIQUE,
    status          TEXT NOT NULL DEFAULT 'pending',   -- pending | completed | revoked
    failed_attempts INTEGER NOT NULL DEFAULT 0,
    source          TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    completed_at    TEXT
);
CREATE INDEX IF NOT EXISTS idx_sub_person ON submissions(person_id, template_id);

-- One row per filled field. Signatures are stored as small PNGs.
CREATE TABLE IF NOT EXISTS field_values (
    submission_id TEXT NOT NULL REFERENCES submissions(id) ON DELETE CASCADE,
    field_id      TEXT NOT NULL,
    value         TEXT NOT NULL DEFAULT '',
    image         BLOB,
    round_id      INTEGER NOT NULL,
    PRIMARY KEY (submission_id, field_id)
);

-- Every time someone submits (signs / continues) a form a round is recorded (audit trail).
CREATE TABLE IF NOT EXISTS rounds (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    submission_id TEXT NOT NULL REFERENCES submissions(id) ON DELETE CASCADE,
    sections      TEXT NOT NULL DEFAULT '[]',
    signer_name   TEXT NOT NULL,
    ip            TEXT NOT NULL DEFAULT '',
    user_agent    TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS signer_sessions (
    token_hash    TEXT PRIMARY KEY,
    submission_id TEXT NOT NULL REFERENCES submissions(id) ON DELETE CASCADE,
    expires_at    TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS api_keys (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    key_hash     TEXT NOT NULL UNIQUE,
    created_by   TEXT,
    created_at   TEXT NOT NULL,
    last_used_at TEXT
);

-- Users of the admin UI, authenticated via OIDC (authentik). Role is derived from groups.
CREATE TABLE IF NOT EXISTS users (
    id            TEXT PRIMARY KEY,
    sub           TEXT NOT NULL UNIQUE,
    username      TEXT NOT NULL,
    name          TEXT NOT NULL DEFAULT '',
    email         TEXT,
    role          TEXT NOT NULL,          -- admin | staff
    created_at    TEXT NOT NULL,
    last_login_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS user_sessions (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS oidc_states (
    state      TEXT PRIMARY KEY,
    verifier   TEXT NOT NULL,
    nonce      TEXT NOT NULL,
    next       TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- General (non-personal) links to a template, e.g. printed as QR code. Whoever opens it
-- identifies by name (+ zip code when the name is ambiguous) and signs on the spot.
CREATE TABLE IF NOT EXISTS public_links (
    id          TEXT PRIMARY KEY,
    template_id TEXT NOT NULL REFERENCES templates(id) ON DELETE CASCADE,
    token       TEXT NOT NULL UNIQUE,
    label       TEXT NOT NULL DEFAULT '',
    active      INTEGER NOT NULL DEFAULT 1,
    expires_at  TEXT,
    uses        INTEGER NOT NULL DEFAULT 0,
    created_by  TEXT,
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Recent actions and errors shown in the admin UI (see activity.rs); pruned to the newest entries.
CREATE TABLE IF NOT EXISTS activity_log (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    created_at TEXT NOT NULL,
    level      TEXT NOT NULL,          -- info | warn | error
    category   TEXT NOT NULL,          -- import | export | backup | template | document | person | signing | api | server
    actor      TEXT,
    message    TEXT NOT NULL
);
