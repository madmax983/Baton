//! A host: a slot fleet with ownership and replication, and a small JSON API
//! the console's client writes through.
//!
//! Every call goes through `DatabaseFleet::open`, which checks that this host
//! owns the key's slot at its current epoch before it touches the disk. A
//! host that does not answers `421 Misdirected Request` and the client tries
//! another. The epoch the slot opened under goes back with every answer, so
//! the client can refuse a zombie's acknowledgement on its own.

use autumn_web::AppState;
use autumn_web::db::fleet::{DatabaseFleet, FleetError};
use autumn_web::prelude::*;
use autumn_web::reexports::axum::response::Response;
use autumn_web::reexports::{diesel, diesel_async};
use diesel_async::RunQueryDsl as _;
use serde::{Deserialize, Serialize};

/// A write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteReq {
    /// The tenant (a runner).
    pub tenant: String,
    /// Its sequence number, from 1.
    pub seq: u64,
    /// What to write.
    pub body: String,
}

/// The answer to a write or a read.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Served {
    /// The host that served it.
    pub host: String,
    /// The tenant's slot.
    pub slot: u16,
    /// The ownership epoch the slot is open under (0 without ownership).
    pub epoch: u64,
    /// For a read: every sequence number present, ascending.
    #[serde(default)]
    pub seqs: Vec<u64>,
}

/// A refusal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Refused {
    /// The host.
    pub host: String,
    /// Why.
    pub error: String,
}

fn owner(state: &AppState) -> String {
    state
        .config()
        .database
        .fleet
        .as_ref()
        .and_then(|f| f.ownership.as_ref())
        .map_or_else(|| "unowned".to_owned(), |o| o.owner.clone())
}

fn fleet(state: &AppState) -> Option<DatabaseFleet> {
    state.shards().and_then(|s| s.fleet()).cloned()
}

fn refuse(state: &AppState, status: StatusCode, error: &impl ToString) -> Response {
    (
        status,
        Json(Refused {
            host: owner(state),
            error: error.to_string(),
        }),
    )
        .into_response()
}

fn fleet_refusal(state: &AppState, e: &FleetError) -> Response {
    let status = match e {
        FleetError::NotOwner { .. } => StatusCode::MISDIRECTED_REQUEST,
        FleetError::InvalidKey(_) => StatusCode::BAD_REQUEST,
        FleetError::NotFound { .. } => StatusCode::NOT_FOUND,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    };
    refuse(state, status, e)
}

/// Write one runner's note.
#[post("/api/write")]
#[public]
pub async fn write(State(state): State<AppState>, Json(w): Json<WriteReq>) -> Response {
    let Some(fleet) = fleet(&state) else {
        return refuse(&state, StatusCode::SERVICE_UNAVAILABLE, &"no fleet");
    };
    let key = match fleet.key_for(&w.tenant) {
        Ok(k) => k,
        Err(e) => return fleet_refusal(&state, &e),
    };
    let db = match fleet.open(&key).await {
        Ok(db) => db,
        Err(e) => return fleet_refusal(&state, &e),
    };
    let _lease = db.lease();
    let mut conn = match db.pool().get().await {
        Ok(c) => c,
        Err(e) => return refuse(&state, StatusCode::SERVICE_UNAVAILABLE, &e),
    };
    let done = diesel::sql_query(
        "INSERT OR IGNORE INTO notes (tenant_id, seq, body, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind::<diesel::sql_types::Text, _>(&w.tenant)
    .bind::<diesel::sql_types::BigInt, _>(i64::try_from(w.seq).unwrap_or(i64::MAX))
    .bind::<diesel::sql_types::Text, _>(&w.body)
    .bind::<diesel::sql_types::Timestamp, _>(chrono::Utc::now().naive_utc())
    .execute(&mut *conn)
    .await;
    match done {
        Ok(_) => Json(Served {
            host: owner(&state),
            slot: slot_number(&key),
            epoch: db.epoch().map_or(0, autumn_web::lock::FencingToken::get),
            seqs: vec![],
        })
        .into_response(),
        Err(e) => refuse(&state, StatusCode::SERVICE_UNAVAILABLE, &e),
    }
}

/// The query string of a read.
#[derive(Debug, Deserialize)]
pub struct ReadQuery {
    /// The tenant.
    pub tenant: String,
}

/// Every sequence number a runner has here.
#[get("/api/read")]
#[public]
pub async fn read(State(state): State<AppState>, Query(q): Query<ReadQuery>) -> Response {
    #[derive(diesel::QueryableByName)]
    struct Seq {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        seq: i64,
    }
    let Some(fleet) = fleet(&state) else {
        return refuse(&state, StatusCode::SERVICE_UNAVAILABLE, &"no fleet");
    };
    let key = match fleet.key_for(&q.tenant) {
        Ok(k) => k,
        Err(e) => return fleet_refusal(&state, &e),
    };
    let db = match fleet.open(&key).await {
        Ok(db) => db,
        Err(e) => return fleet_refusal(&state, &e),
    };
    let _lease = db.lease();
    let mut conn = match db.pool().get().await {
        Ok(c) => c,
        Err(e) => return refuse(&state, StatusCode::SERVICE_UNAVAILABLE, &e),
    };
    let rows: Result<Vec<Seq>, _> =
        diesel::sql_query("SELECT seq FROM notes WHERE tenant_id = ? ORDER BY seq")
            .bind::<diesel::sql_types::Text, _>(&q.tenant)
            .load(&mut *conn)
            .await;
    match rows {
        Ok(rows) => Json(Served {
            host: owner(&state),
            slot: slot_number(&key),
            epoch: db.epoch().map_or(0, autumn_web::lock::FencingToken::get),
            seqs: rows
                .into_iter()
                .filter_map(|r| u64::try_from(r.seq).ok())
                .collect(),
        })
        .into_response(),
        Err(e) => refuse(&state, StatusCode::SERVICE_UNAVAILABLE, &e),
    }
}

fn slot_number(key: &autumn_web::fleet_layout::FleetDbKey) -> u16 {
    key.name()
        .strip_prefix("slot:")
        .and_then(|d| d.parse().ok())
        .unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn served_round_trips_and_defaults_seqs() {
        let s: Served = serde_json::from_str(r#"{"host":"host-a","slot":7,"epoch":3}"#).unwrap();
        assert_eq!(s.seqs, Vec::<u64>::new());
        assert_eq!(s.epoch, 3);
    }
}
