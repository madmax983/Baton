//! Baton: a failover lab for autumn-web 0.9's slot fleet.
//!
//! One binary, two roles. `baton-app host` is a host: a slot fleet with
//! ownership and replication, and a JSON API ([`host`]). `baton-app` alone is
//! the console: it starts and stops hosts as child processes, writes through
//! them, and audits what survived ([`console`]).
#![warn(missing_docs)]

pub mod console;
pub mod host;
#[allow(missing_docs)]
pub mod schema;

/// Every console handler `main.rs` must register (checked by a test).
pub const CONSOLE_ROUTES: &[&str] = &[
    "routes::index",
    "routes::lab_state",
    "routes::host_action",
    "routes::client",
    "routes::audit",
    "routes::reset",
    "routes::script",
];

#[cfg(test)]
mod tests {
    #[test]
    fn every_console_route_is_registered() {
        let main_source = include_str!("main.rs");
        for handler in crate::CONSOLE_ROUTES {
            assert!(main_source.contains(handler), "{handler} is not registered");
        }
        assert!(main_source.contains("host::write") && main_source.contains("host::read"));
    }

    #[test]
    fn the_host_template_is_a_slot_fleet_with_ownership_and_replication() {
        let t = include_str!("../host.toml");
        for needle in [
            r#"mode = "slot""#,
            "restore_missing = true",
            "[database.fleet.ownership]",
            "[replication]",
            "enabled = true",
        ] {
            assert!(t.contains(needle), "{needle}");
        }
    }
}
