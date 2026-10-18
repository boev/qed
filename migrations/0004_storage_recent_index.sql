-- Storage owns this index even when history migrations ran before its tables existed.
CREATE INDEX IF NOT EXISTS durable_objects_recent_idx
ON durable_objects (checked_at DESC,key)
WHERE kind='attestation' AND NOT quarantined AND NOT deleted;
