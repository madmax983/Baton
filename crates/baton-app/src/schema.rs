//! Diesel table definitions, mirroring `migrations/`.

diesel::table! {
    notes (id) {
        id -> BigInt,
        tenant_id -> Text,
        seq -> BigInt,
        body -> Text,
        created_at -> Timestamp,
    }
}
