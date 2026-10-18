CREATE TABLE durable_objects (
    key text PRIMARY KEY,
    id text,
    kind text NOT NULL,
    pool text,
    subject text,
    chain text,
    checked_at timestamptz,
    expires_at timestamptz,
    payload bytea,
    s3_key text,
    sha256 text NOT NULL,
    quarantined boolean NOT NULL DEFAULT false,
    deleted boolean NOT NULL DEFAULT false,
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (deleted OR ((payload IS NULL) <> (s3_key IS NULL)))
);
CREATE UNIQUE INDEX durable_attestation_id ON durable_objects(id) WHERE kind = 'attestation';
CREATE INDEX durable_attestation_pool ON durable_objects(chain,pool,checked_at DESC) WHERE kind = 'attestation' AND NOT quarantined AND NOT deleted;
CREATE INDEX durable_attestation_subject ON durable_objects(chain,subject,checked_at DESC) WHERE kind = 'attestation' AND NOT quarantined AND NOT deleted;
CREATE INDEX durable_attestation_expiry ON durable_objects(expires_at) WHERE kind = 'attestation' AND NOT deleted;
CREATE TABLE storage_import_progress (
    source text PRIMARY KEY,
    cursor text,
    completed boolean NOT NULL DEFAULT false,
    imported bigint NOT NULL DEFAULT 0,
    rejected bigint NOT NULL DEFAULT 0,
    updated_at timestamptz NOT NULL DEFAULT now()
);
