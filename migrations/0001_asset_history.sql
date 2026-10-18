CREATE TABLE assets (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    chain text NOT NULL,
    subject_kind text NOT NULL CHECK (subject_kind IN ('pool', 'token')),
    address text NOT NULL,
    first_seen_at timestamptz NOT NULL,
    ticker text,
    provenance jsonb NOT NULL DEFAULT '{}',
    UNIQUE (chain, subject_kind, address)
);
CREATE INDEX assets_ticker_idx ON assets (ticker);
CREATE TABLE observations (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    event_id text NOT NULL UNIQUE,
    asset_id bigint NOT NULL REFERENCES assets(id),
    observed_at timestamptz NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now(),
    kind text NOT NULL CHECK (kind IN ('check', 'powers', 'guard')),
    facet text NOT NULL CHECK (facet IN ('contract_match', 'powers', 'guard')),
    outcome text NOT NULL,
    miss_reason text,
    attestation_id text,
    block numeric(20,0),
    slot numeric(20,0),
    source text NOT NULL,
    registry_hash text,
    public boolean NOT NULL DEFAULT true CHECK (public),
    evidence jsonb NOT NULL,
    signed_document jsonb,
    event jsonb NOT NULL,
    CHECK ((outcome = 'miss' AND miss_reason IS NOT NULL) OR outcome <> 'miss'),
    CHECK (outcome IN ('verified','mismatch','no_match','controls_complete','controls_partial','allow','deny','unknown','registry_stale','registry_removed','unsupported','miss'))
);
CREATE INDEX observations_asset_time_idx ON observations (asset_id, observed_at DESC, id DESC);
CREATE INDEX observations_facet_time_idx ON observations (asset_id, facet, observed_at DESC, id DESC);
CREATE INDEX observations_attestation_idx ON observations (attestation_id) WHERE attestation_id IS NOT NULL;
CREATE TABLE asset_state (
    asset_id bigint NOT NULL REFERENCES assets(id),
    facet text NOT NULL,
    projection jsonb NOT NULL,
    next_retry_at timestamptz,
    PRIMARY KEY (asset_id, facet)
);
CREATE INDEX asset_state_retry_idx ON asset_state (next_retry_at) WHERE next_retry_at IS NOT NULL;
