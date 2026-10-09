-- Decode and rollback barrier for evidence-report definitions, results and
-- checkpoint events, which use the existing immutable JSON records. Older
-- schema20 binaries must reject this state before reading unknown contracts.
SELECT 1;
