UPDATE service_triggers SET peer = json_array(peer);
ALTER TABLE service_triggers RENAME COLUMN peer TO chain;
