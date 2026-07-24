CREATE TABLE dns_credentials (
    domain         TEXT NOT NULL PRIMARY KEY REFERENCES certificates(domain) ON DELETE CASCADE,
    creds_json_enc TEXT NOT NULL
);
