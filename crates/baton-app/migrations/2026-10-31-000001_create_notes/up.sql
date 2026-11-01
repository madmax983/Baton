-- A runner's writes. Applied to every slot file. (tenant_id, seq) is unique so
-- a client retry of a write that already landed is a no-op, not a duplicate:
-- the console retries on another host whenever an answer does not come back.
CREATE TABLE notes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    tenant_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    body TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL,
    UNIQUE (tenant_id, seq)
);
