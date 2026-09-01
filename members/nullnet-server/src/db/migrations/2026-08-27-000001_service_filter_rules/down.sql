ALTER TABLE services DROP COLUMN egress_filter;
ALTER TABLE services DROP COLUMN ingress_filter;
ALTER TABLE services ADD COLUMN egress_blocked_countries TEXT;
ALTER TABLE services ADD COLUMN egress_allowed_countries TEXT;
ALTER TABLE services ADD COLUMN ingress_blocked_countries TEXT;
ALTER TABLE services ADD COLUMN ingress_allowed_countries TEXT;
