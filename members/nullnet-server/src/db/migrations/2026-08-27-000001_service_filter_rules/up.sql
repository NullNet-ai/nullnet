-- Replace the country-only egress/ingress policy columns with a single JSON
-- `FilterPolicy` column per direction (issue #143 — arbitrary AND/OR rules
-- over Country/ASN/Src IP/Dst IP, evaluated via rpn-predicate-interpreter).
-- The feature is unreleased, so existing country-policy data is dropped
-- rather than converted.
ALTER TABLE services DROP COLUMN egress_blocked_countries;
ALTER TABLE services DROP COLUMN egress_allowed_countries;
ALTER TABLE services DROP COLUMN ingress_blocked_countries;
ALTER TABLE services DROP COLUMN ingress_allowed_countries;
ALTER TABLE services ADD COLUMN egress_filter TEXT;
ALTER TABLE services ADD COLUMN ingress_filter TEXT;
