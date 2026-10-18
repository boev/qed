-- Older binaries allowed anonymous Guard Unknown/EOA answers to initiate tracking.
-- Remove only projections without a real confirming result or a scheduled attempt;
-- immutable journal rows and signed documents are retained.
DELETE FROM asset_state s
WHERE NOT EXISTS (
    SELECT 1 FROM observations o WHERE o.asset_id=s.asset_id AND (
        o.outcome IN ('verified','mismatch','no_match','controls_complete','allow','deny')
        OR (o.scheduled_slot IS NOT NULL AND o.outcome<>'rejected'
            AND o.miss_reason IS DISTINCT FROM 'chain_probe_failed')
    )
);
