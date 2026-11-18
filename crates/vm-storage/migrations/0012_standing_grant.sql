-- Standing grant (ADR-0039): unattended agent access to one VM, created by
-- the user in the app and kept across restarts. Only the policy and the
-- SHA-256 of its pairing credential are stored, never the credential.
-- No foreign key on the VM: deleting the VM revokes the grant in the same
-- transaction and the row stays as history.
CREATE TABLE standing_grants (
    grant_id          TEXT PRIMARY KEY NOT NULL,
    vm_id             TEXT NOT NULL,
    credential_sha256 TEXT NOT NULL CHECK (length(credential_sha256) = 64),
    -- 1 while active or suspended: at most one such row.
    live              INTEGER NOT NULL CHECK (live IN (0, 1)),
    state             TEXT NOT NULL CHECK (state IN ('active','suspended','revoked','expired')),
    state_cause       TEXT,
    state_at          TEXT,
    policy_json       TEXT NOT NULL CHECK (json_valid(policy_json)),
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    CHECK ((live = 1) = (state IN ('active','suspended')))
) STRICT;
CREATE UNIQUE INDEX standing_grants_one_live ON standing_grants(live) WHERE live = 1;
CREATE INDEX standing_grants_vm ON standing_grants(vm_id);

-- Baselines created by sessions of a standing grant: restorable by every
-- later session of the same grant (ADR-0039 §4.6).
CREATE TABLE standing_grant_baselines (
    grant_id    TEXT NOT NULL REFERENCES standing_grants(grant_id) ON DELETE CASCADE,
    baseline_id TEXT NOT NULL,
    PRIMARY KEY (grant_id, baseline_id)
) STRICT;
