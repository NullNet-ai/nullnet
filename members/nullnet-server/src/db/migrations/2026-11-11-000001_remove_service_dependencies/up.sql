DROP TABLE IF EXISTS service_dependencies;
DELETE FROM events WHERE kind = 'stale_session_evicted';
UPDATE events
SET kind = 'ingress_owner_lost',
    payload = replace(payload, '"type":"chain_owner_lost"', '"type":"ingress_owner_lost"')
WHERE kind = 'chain_owner_lost';
UPDATE events
SET kind = 'ingress_setup_failed',
    payload = replace(payload, '"type":"proxy_chain_setup_failed"', '"type":"ingress_setup_failed"')
WHERE kind = 'proxy_chain_setup_failed';
