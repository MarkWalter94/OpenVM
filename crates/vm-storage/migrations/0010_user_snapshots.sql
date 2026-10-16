-- 0010: user snapshots in manual use (ADR-0032).
--
-- 1. snapshots: one row per named snapshot of a VM. The disk state is a
--    QCOW2 internal snapshot of the VM's own disk named `disk_tag`; the
--    copy of the UEFI variables and the optional thumbnail live in
--    vms/<vm_id>/snapshots/<snapshot_id>/; the VM configuration at that
--    moment is kept as JSON (opaque to the storage). `parent_id` is the
--    snapshot the state descended from when this one was taken (tree).
-- 2. snapshot_heads: the snapshot the VM's current state descends from
--    ("you are here"): the last one taken or reverted to. No row = none.
-- 3. Journal kinds 'take_snapshot', 'revert_snapshot', 'delete_snapshot'
--    with target kind 'snapshot', and a JSON `payload` column carrying
--    what reconciliation needs to finish or undo the operation (the disk
--    and the tag, the row to insert, the configuration to restore).
--    Reconciliation decides from the QCOW2 snapshot table of the disk (tag
--    present or not, active L1 table equal to the snapshot's), see
--    `snapshot.rs`. SQLite does not alter an existing CHECK: the journal is
--    rebuilt keeping rows, identifiers and the AUTOINCREMENT sequence (same
--    procedure as 0003, 0005, 0006 and 0009).

CREATE TABLE snapshots (
    snapshot_id         TEXT PRIMARY KEY NOT NULL,
    vm_id               TEXT NOT NULL REFERENCES vms(vm_id) ON DELETE RESTRICT,
    parent_id           TEXT REFERENCES snapshots(snapshot_id) ON DELETE RESTRICT,
    name                TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 256),
    description         TEXT NOT NULL DEFAULT '' CHECK (length(description) <= 1000),
    disk_tag            TEXT NOT NULL CHECK (length(disk_tag) BETWEEN 1 AND 128),
    taken_while_running INTEGER NOT NULL CHECK (taken_while_running IN (0,1)),
    clean_shutdown      INTEGER NOT NULL CHECK (clean_shutdown IN (0,1)),
    config_json         TEXT NOT NULL CHECK (json_valid(config_json)),
    config_revision     INTEGER NOT NULL CHECK (config_revision >= 1),
    vars_sha256         TEXT CHECK (vars_sha256 IS NULL OR length(vars_sha256) = 64),
    vars_bytes          INTEGER NOT NULL DEFAULT 0 CHECK (vars_bytes >= 0),
    thumbnail_bytes     INTEGER NOT NULL DEFAULT 0 CHECK (thumbnail_bytes >= 0),
    created_at          TEXT NOT NULL,
    UNIQUE (vm_id, disk_tag),
    CHECK (parent_id IS NULL OR parent_id <> snapshot_id)
) STRICT;
CREATE INDEX snapshots_vm ON snapshots(vm_id);
CREATE INDEX snapshots_parent ON snapshots(parent_id);

CREATE TABLE snapshot_heads (
    vm_id       TEXT PRIMARY KEY NOT NULL REFERENCES vms(vm_id) ON DELETE RESTRICT,
    snapshot_id TEXT NOT NULL REFERENCES snapshots(snapshot_id) ON DELETE RESTRICT
) STRICT;

CREATE TABLE journal_v10 (
    intent_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL
                CHECK (kind IN ('create_disk','create_overlay','copy_vars','promote_baseline',
                                'import_media','delete_file','export','discard_file',
                                'import_vm','take_snapshot','revert_snapshot','delete_snapshot')),
    state       TEXT NOT NULL
                CHECK (state IN ('pending','applied','committed','aborted','needs_review')),
    vm_id       TEXT,
    operation_id TEXT,
    target_kind TEXT NOT NULL
                CHECK (target_kind IN ('disk','firmware','baseline','artifact','media','vm',
                                       'snapshot','none')),
    target_id   TEXT,
    temp_path   TEXT NOT NULL
                CHECK (substr(temp_path,1,4) = 'tmp/' AND instr(temp_path,'..') = 0
                       AND instr(temp_path,':') = 0 AND instr(temp_path,char(92)) = 0),
    final_path  TEXT
                CHECK (final_path IS NULL OR (substr(final_path,1,1) <> '/' AND instr(final_path,'..') = 0
                       AND instr(final_path,':') = 0 AND instr(final_path,char(92)) = 0)),
    resolution  TEXT,
    payload     TEXT CHECK (payload IS NULL OR json_valid(payload)),
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

INSERT INTO journal_v10(intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
                        temp_path, final_path, resolution, payload, created_at, updated_at)
SELECT intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
       temp_path, final_path, resolution, NULL, created_at, updated_at
FROM journal;

DROP TABLE journal;
ALTER TABLE journal_v10 RENAME TO journal;
CREATE INDEX journal_open ON journal(state) WHERE state IN ('pending','applied','needs_review');
