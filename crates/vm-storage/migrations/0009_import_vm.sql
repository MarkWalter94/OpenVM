-- 0009: intent kind 'import_vm' (ADR-0030: import of a VMware VM by
-- conversion). The converted disk is written into the journal temporary,
-- published as vms/<vm_id>/primary.qcow2 and the `vms` row is inserted in
-- the same transaction that commits the intent (target kind 'vm'): an
-- interrupted import never leaves a VM without its disk, and reconciliation
-- removes the temporary and any published disk without a row. SQLite does
-- not alter an existing CHECK: the table is rebuilt keeping rows,
-- identifiers and the AUTOINCREMENT sequence (same procedure as 0003, 0005
-- and 0006).

CREATE TABLE journal_v9 (
    intent_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL
                CHECK (kind IN ('create_disk','create_overlay','copy_vars','promote_baseline',
                                'import_media','delete_file','export','discard_file',
                                'import_vm')),
    state       TEXT NOT NULL
                CHECK (state IN ('pending','applied','committed','aborted','needs_review')),
    vm_id       TEXT,
    operation_id TEXT,
    target_kind TEXT NOT NULL
                CHECK (target_kind IN ('disk','firmware','baseline','artifact','media','vm','none')),
    target_id   TEXT,
    temp_path   TEXT NOT NULL
                CHECK (substr(temp_path,1,4) = 'tmp/' AND instr(temp_path,'..') = 0
                       AND instr(temp_path,':') = 0 AND instr(temp_path,char(92)) = 0),
    final_path  TEXT
                CHECK (final_path IS NULL OR (substr(final_path,1,1) <> '/' AND instr(final_path,'..') = 0
                       AND instr(final_path,':') = 0 AND instr(final_path,char(92)) = 0)),
    resolution  TEXT,
    created_at  TEXT NOT NULL,
    updated_at  TEXT NOT NULL
) STRICT;

INSERT INTO journal_v9(intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
                       temp_path, final_path, resolution, created_at, updated_at)
SELECT intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
       temp_path, final_path, resolution, created_at, updated_at
FROM journal;

DROP TABLE journal;
ALTER TABLE journal_v9 RENAME TO journal;
CREATE INDEX journal_open ON journal(state) WHERE state IN ('pending','applied','needs_review');
