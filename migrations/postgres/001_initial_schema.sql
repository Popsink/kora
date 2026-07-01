-- Initial Kora schema
-- Tables: subjects, schema_contents, schema_versions, schema_references, config

CREATE TABLE IF NOT EXISTS subjects (
    id         BIGSERIAL PRIMARY KEY,
    name       TEXT UNIQUE NOT NULL,
    deleted    BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS schema_contents (
    id              BIGSERIAL PRIMARY KEY,
    schema_type     TEXT NOT NULL DEFAULT 'AVRO',
    schema_text     TEXT NOT NULL,
    canonical_form  TEXT,
    fingerprint     TEXT,
    raw_fingerprint TEXT NOT NULL DEFAULT '',
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS schema_versions (
    id          BIGSERIAL PRIMARY KEY,
    subject_id  BIGINT NOT NULL REFERENCES subjects(id),
    version     INT NOT NULL CHECK (version > 0),
    content_id  BIGINT NOT NULL REFERENCES schema_contents(id),
    deleted     BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (subject_id, version)
);

CREATE TABLE IF NOT EXISTS schema_references (
    id         BIGSERIAL PRIMARY KEY,
    content_id BIGINT NOT NULL REFERENCES schema_contents(id),
    name       TEXT NOT NULL,
    subject    TEXT NOT NULL,
    version    INT NOT NULL
);

CREATE TABLE IF NOT EXISTS config (
    id                  BIGSERIAL PRIMARY KEY,
    subject             TEXT UNIQUE,
    compatibility_level TEXT,
    normalize           BOOLEAN,
    mode                TEXT,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_schema_versions_subject_version ON schema_versions(subject_id, version);
CREATE INDEX IF NOT EXISTS idx_schema_versions_content_id ON schema_versions(content_id);
CREATE INDEX IF NOT EXISTS idx_schema_contents_fingerprint ON schema_contents(fingerprint);
CREATE UNIQUE INDEX IF NOT EXISTS idx_schema_contents_raw_fingerprint ON schema_contents(raw_fingerprint);
CREATE INDEX IF NOT EXISTS idx_subjects_name ON subjects(name);
CREATE INDEX IF NOT EXISTS idx_subjects_name_active ON subjects(name) WHERE deleted = false;
CREATE INDEX IF NOT EXISTS idx_schema_references_content_id ON schema_references(content_id);

-- Ensure at most one global config row (NULL subject).
CREATE UNIQUE INDEX IF NOT EXISTS idx_config_global ON config ((true)) WHERE subject IS NULL;

-- Global config row (subject = NULL means global)
INSERT INTO config (subject, compatibility_level, mode)
VALUES (NULL, 'BACKWARD', 'READWRITE')
ON CONFLICT DO NOTHING;
