// Baton — a failover lab for autumn-web 0.9's slot fleet.
//
//   baton-app        the console (http://localhost:3000): starts host-a and
//                    host-b as child processes in ./baton-lab, kills and
//                    freezes them on request, writes through them, and audits
//                    every acknowledged write.
//   baton-app host   one host (the console runs these): a slot fleet with
//                    [database.fleet.ownership] and [replication], and a JSON
//                    API on loopback. Reads autumn.toml from its own directory.

use autumn_web::migrate::{EmbeddedMigrations, embed_migrations};
use autumn_web::prelude::*;
use baton_app::console::{self, routes};
use baton_app::host;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!();

#[autumn_web::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("host") {
        Box::pin(
            autumn_web::app()
                .migrations(MIGRATIONS)
                .routes(routes![host::write, host::read])
                .run(),
        )
        .await;
    } else {
        Box::pin(
            autumn_web::app()
                .on_startup(console::boot)
                .routes(routes![
                    routes::index,
                    routes::lab_state,
                    routes::host_action,
                    routes::client,
                    routes::audit,
                    routes::reset,
                    routes::script,
                ])
                .run(),
        )
        .await;
    }
}
