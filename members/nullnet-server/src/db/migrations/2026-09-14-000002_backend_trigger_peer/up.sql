-- Empty triggers never created an edge; each remaining trigger keeps its direct peer.
DELETE FROM service_triggers WHERE json_array_length(chain) = 0;
UPDATE service_triggers SET chain = json_extract(chain, '$[0]');
ALTER TABLE service_triggers RENAME COLUMN chain TO peer;
