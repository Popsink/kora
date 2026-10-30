-- Principals for HTTP basic authentication and subject-scoped ACLs.
--
-- `password_hash` is an Argon2id PHC string. `rules` is a JSON array of
-- `{"pattern": "<prefix>*" | "<subject>", "access": "read" | "read_write"}`
-- objects; it only applies to the `tenant` role (`admin` is unrestricted).

CREATE TABLE IF NOT EXISTS principals (
    id            BIGSERIAL PRIMARY KEY,
    name          TEXT UNIQUE NOT NULL,
    password_hash TEXT NOT NULL,
    role          TEXT NOT NULL CHECK (role IN ('admin', 'tenant')),
    rules         JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
