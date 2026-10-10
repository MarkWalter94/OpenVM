-- 0005: target kind 'vm' in the journal (F2: deletion of a VM with its files).
-- A VM deletion is a group of `delete_file` intents sharing one operation id
-- and targeting the `vms` row: reconciliation rolls the whole group forward
-- once a file has been moved, or back if none was. SQLite does not alter an
-- existing CHECK: the table is rebuilt keeping rows, identifiers and the
-- AUTOINCREMENT sequence (same procedure as 0003).

CREATE TABLE journal_v5 (
    intent_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL
                CHECK (kind IN ('create_disk','create_overlay','copy_vars','promote_baseline',
                                'import_media','delete_file','export')),
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

INSERT INTO journal_v5(intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
                       temp_path, final_path, resolution, created_at, updated_at)
SELECT intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
       temp_path, final_path, resolution, created_at, updated_at
FROM journal;

DROP TABLE journal;
ALTER TABLE journal_v5 RENAME TO journal;
CREATE INDEX journal_open ON journal(state) WHERE state IN ('pending','applied','needs_review');
