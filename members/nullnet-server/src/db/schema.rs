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

diesel::table! {
    users (id) {
        id -> Text,
        username -> Text,
        password_hash -> Text,
        role -> Text,
        mfa_secret_enc -> Nullable<Text>,
        mfa_confirmed_at -> Nullable<BigInt>,
        created_at -> BigInt,
        updated_at -> BigInt,
    }
}

diesel::table! {
    user_scopes (user_id, scope) {
        user_id -> Text,
        scope -> Text,
    }
}

diesel::table! {
    refresh_tokens (token_hash) {
        token_hash -> Text,
        user_id -> Text,
        expires_at -> BigInt,
        created_at -> BigInt,
        revoked_at -> Nullable<BigInt>,
    }
}

diesel::table! {
    login_attempts (username) {
        username -> Text,
        failed_count -> Integer,
        locked_until -> Nullable<BigInt>,
        updated_at -> BigInt,
    }
}

diesel::table! {
    events (id) {
        id -> BigInt,
        kind -> Text,
        severity -> Text,
        timestamp -> BigInt,
        payload -> Text,
    }
}

diesel::joinable!(dns_credentials -> certificates (domain));
diesel::joinable!(user_scopes -> users (user_id));
diesel::joinable!(refresh_tokens -> users (user_id));
diesel::allow_tables_to_appear_in_same_query!(
    certificates,
    dns_credentials,
    users,
    user_scopes,
    refresh_tokens,
    login_attempts,
    events,
);
