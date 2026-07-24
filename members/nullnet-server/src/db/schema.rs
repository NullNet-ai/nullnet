// Mirrors src/db/migrations — regenerate by hand (or `diesel print-schema`)
// whenever a migration changes these tables.

diesel::table! {
    certificates (domain) {
        domain -> Text,
        fullchain_pem -> Text,
        key_pem_enc -> Text,
        not_after -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    dns_credentials (domain) {
        domain -> Text,
        creds_json_enc -> Text,
    }
}

diesel::table! {
    services (stack) {
        stack -> Text,
        service_json -> Text,
        updated_at -> BigInt,
    }
}

diesel::joinable!(dns_credentials -> certificates (domain));
diesel::allow_tables_to_appear_in_same_query!(certificates, dns_credentials);
