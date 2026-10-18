-- Retain one observation document, not duplicate evidence/signature columns.
ALTER TABLE observations DROP COLUMN evidence, DROP COLUMN signed_document;
ALTER TABLE observations DROP CONSTRAINT observations_outcome_check;
ALTER TABLE observations ADD CONSTRAINT observations_outcome_check CHECK (outcome IN ('verified','mismatch','no_match','controls_complete','controls_partial','allow','deny','unknown','registry_stale','registry_removed','unsupported','miss','rejected'));
CREATE INDEX observations_asset_keyset_idx ON observations (asset_id, observed_at DESC, event_id DESC);
ALTER TABLE observations ADD COLUMN ticker text, ADD COLUMN scheduled_slot bigint;
UPDATE observations o SET ticker=coalesce(o.event->>'ticker',a.ticker),
    scheduled_slot=CASE WHEN o.event ? 'qed_storage_ref' AND o.source='background' THEN floor(extract(epoch FROM o.observed_at)/3600)::bigint ELSE (o.event->>'scheduled_slot')::bigint END
FROM assets a WHERE a.id=o.asset_id;
UPDATE observations SET event = jsonb_set(event, '{evidence}', 'null'::jsonb)
WHERE event->'signed_document' IS NOT NULL AND event->'signed_document' <> 'null'::jsonb AND event->'evidence' = event->'signed_document';

-- Previous releases counted zero-provider rejections and on-demand misses.
-- Repair only unsigned operational projection metadata, never signed documents.
UPDATE observations SET outcome='rejected', scheduled_slot=NULL, event=jsonb_set(event,'{outcome}','"rejected"'::jsonb)
WHERE outcome='miss' AND miss_reason IN ('budget_rejection','read_not_executed');
WITH metadata AS (
    SELECT o.id,o.asset_id,o.facet,o.observed_at,o.outcome,o.miss_reason,o.scheduled_slot,
      jsonb_build_object('event_id',o.event_id,'asset',jsonb_build_object('chain',a.chain,'kind',a.subject_kind,'address',a.address),
        'facet',o.facet,'observed_at',o.observed_at,'outcome',o.outcome,'miss_reason',o.miss_reason,
        'attestation_id',o.attestation_id,'block',o.block,'slot',o.slot,'source',o.source,
        'registry_hash',o.registry_hash,'ticker',o.ticker,'scheduled_slot',o.scheduled_slot) AS summary
    FROM observations o JOIN assets a ON a.id=o.asset_id
), confirmed AS (
    SELECT DISTINCT ON(asset_id,facet) asset_id,facet,summary FROM metadata
    WHERE outcome IN ('verified','mismatch','no_match','controls_complete','allow','deny','unknown')
    ORDER BY asset_id,facet,observed_at DESC,id DESC
), resets AS (
    SELECT DISTINCT ON(asset_id,facet) asset_id,facet,observed_at,outcome FROM metadata
    WHERE outcome NOT IN ('miss','controls_partial','rejected')
    ORDER BY asset_id,facet,observed_at DESC,id DESC
), slots AS (
    SELECT asset_id,facet,scheduled_slot AS slot,min(observed_at) AS at
    FROM metadata o WHERE scheduled_slot IS NOT NULL AND outcome IN ('miss','controls_partial')
      AND (miss_reason IS DISTINCT FROM 'chain_probe_failed' OR EXISTS (
        SELECT 1 FROM confirmed c WHERE c.asset_id=o.asset_id AND c.facet=o.facet))
    GROUP BY asset_id,facet,scheduled_slot
), misses AS (
    SELECT s.asset_id,s.facet,max(s.slot) AS slot,
      count(*) FILTER (WHERE r.observed_at IS NULL OR s.at>r.observed_at) AS count,
      max(s.at) FILTER (WHERE r.observed_at IS NULL OR s.at>r.observed_at) + interval '1 hour' AS retry
    FROM slots s LEFT JOIN resets r USING(asset_id,facet) GROUP BY s.asset_id,s.facet
), latest AS (
    SELECT DISTINCT ON(o.asset_id,o.facet) o.asset_id,o.facet,o.summary FROM metadata o
    WHERE o.outcome<>'rejected' AND (o.outcome NOT IN ('miss','controls_partial') OR
      (o.scheduled_slot IS NOT NULL AND (o.miss_reason IS DISTINCT FROM 'chain_probe_failed' OR EXISTS (
        SELECT 1 FROM confirmed c WHERE c.asset_id=o.asset_id AND c.facet=o.facet))))
    ORDER BY o.asset_id,o.facet,o.observed_at DESC,o.id DESC
), repaired AS (
    SELECT s.asset_id,s.facet,c.summary AS confirmed,coalesce(l.summary,c.summary,'null'::jsonb) AS latest,
      coalesce(m.count,0) AS count,m.slot,m.retry,
      CASE WHEN r.outcome IN ('registry_stale','registry_removed','unsupported') THEN r.outcome END AS availability
    FROM asset_state s LEFT JOIN confirmed c USING(asset_id,facet)
    LEFT JOIN latest l USING(asset_id,facet) LEFT JOIN misses m USING(asset_id,facet)
    LEFT JOIN resets r USING(asset_id,facet)
)
UPDATE asset_state s SET projection=jsonb_build_object('last_confirmed',r.confirmed,'latest',r.latest,
    'consecutive_misses',r.count,'last_miss_slot',r.slot,'next_retry_at',r.retry,'availability',r.availability),
    next_retry_at=r.retry FROM repaired r WHERE s.asset_id=r.asset_id AND s.facet=r.facet;
DELETE FROM asset_state WHERE projection->'latest'='null'::jsonb AND projection->'last_confirmed'='null'::jsonb;
DO $$ BEGIN
    IF to_regclass('durable_objects') IS NOT NULL THEN
        CREATE INDEX durable_objects_recent_idx ON durable_objects (checked_at DESC,key) WHERE kind='attestation' AND NOT quarantined AND NOT deleted;
    END IF;
END $$;
