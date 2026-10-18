-- Restore budget-blocked scheduled slots excluded by migration 3's former policy.
-- Only unsigned journal metadata changes; original event/signed documents stay intact.
UPDATE observations SET outcome='miss',miss_reason='budget_rejection',
    scheduled_slot=floor(extract(epoch FROM observed_at)/3600)::bigint
WHERE outcome='rejected' AND source='background'
  AND miss_reason IN ('budget_rejection','read_not_executed');
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
), tracked AS (
    SELECT asset_id,min(observed_at) AS since FROM metadata o
    WHERE outcome IN ('verified','mismatch','no_match','controls_complete','allow','deny','unknown')
      OR (scheduled_slot IS NOT NULL AND outcome<>'rejected' AND
        (miss_reason IS DISTINCT FROM 'chain_probe_failed' OR EXISTS (
          SELECT 1 FROM confirmed c WHERE c.asset_id=o.asset_id AND c.facet=o.facet)))
    GROUP BY asset_id
), eligible AS (
    SELECT o.* FROM metadata o JOIN tracked t USING(asset_id)
    WHERE o.outcome<>'rejected'
      AND (o.outcome NOT IN ('miss','controls_partial') OR (o.scheduled_slot IS NOT NULL AND
        (o.miss_reason IS DISTINCT FROM 'chain_probe_failed' OR EXISTS (
          SELECT 1 FROM confirmed c WHERE c.asset_id=o.asset_id AND c.facet=o.facet))))
      AND (o.scheduled_slot IS NOT NULL OR o.observed_at>=t.since)
), resets AS (
    SELECT DISTINCT ON(asset_id,facet) asset_id,facet,observed_at,outcome FROM eligible
    WHERE outcome NOT IN ('miss','controls_partial')
    ORDER BY asset_id,facet,observed_at DESC,id DESC
), slots AS (
    SELECT asset_id,facet,scheduled_slot AS slot,min(observed_at) AS at
    FROM eligible WHERE scheduled_slot IS NOT NULL AND outcome IN ('miss','controls_partial')
    GROUP BY asset_id,facet,scheduled_slot
), misses AS (
    SELECT s.asset_id,s.facet,max(s.slot) AS slot,
      count(*) FILTER (WHERE r.observed_at IS NULL OR s.at>r.observed_at) AS count,
      max(s.at) FILTER (WHERE r.observed_at IS NULL OR s.at>r.observed_at) + interval '1 hour' AS retry
    FROM slots s LEFT JOIN resets r USING(asset_id,facet) GROUP BY s.asset_id,s.facet
), latest AS (
    SELECT DISTINCT ON(asset_id,facet) asset_id,facet,summary FROM eligible
    ORDER BY asset_id,facet,observed_at DESC,id DESC
), removed AS (
    DELETE FROM asset_state s WHERE NOT EXISTS (
      SELECT 1 FROM eligible e WHERE e.asset_id=s.asset_id AND e.facet=s.facet)
)
INSERT INTO asset_state(asset_id,facet,projection,next_retry_at)
SELECT l.asset_id,l.facet,jsonb_build_object('last_confirmed',c.summary,'latest',l.summary,
    'consecutive_misses',coalesce(m.count,0),'last_miss_slot',m.slot,'next_retry_at',m.retry,
    'availability',CASE WHEN r.outcome IN ('registry_stale','registry_removed','unsupported') THEN r.outcome END),m.retry
FROM latest l LEFT JOIN confirmed c USING(asset_id,facet)
LEFT JOIN resets r USING(asset_id,facet) LEFT JOIN misses m USING(asset_id,facet)
ON CONFLICT(asset_id,facet) DO UPDATE SET projection=EXCLUDED.projection,next_retry_at=EXCLUDED.next_retry_at;
