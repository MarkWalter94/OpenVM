-- 0006: baselines and current state of a VM (F3; ROADMAP 8, ADR-0005, ADR-0025).
--
-- 1. Journal kind 'discard_file': deletion of a file of the VM's previous
--    current state inside a restore. Unlike 'delete_file' it never rolls
--    forward on its own: until the whole restore commits (one transaction for
--    every intent of the operation) reconciliation puts the file back.
--    SQLite does not alter an existing CHECK: the table is rebuilt keeping
--    rows, identifiers and the AUTOINCREMENT sequence (as in 0003 and 0005).
-- 2. vm_current_state: the VM's current state is an overlay of a baseline
--    plus a writable copy of the baseline's UEFI variables (after a restore).
--    No row = the VM runs on its standalone working disk (F2).
-- 3. runs.clean_exit: guest shutdown observed at the end of the run (1), a
--    forced, emergency or unobserved end (0), unknown (NULL). A baseline is
--    "verified" only after an observed clean shutdown (ROADMAP 8).
-- 4. baselines.derived_from: baseline the source state was an overlay of
--    (informational, no constraint: the parent may be deleted later; the
--    new baseline is always a standalone image, no backing chain);
--    baselines.config_revision: VM configuration revision at creation
--    (also in the baseline manifest).

CREATE TABLE journal_v6 (
    intent_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL
                CHECK (kind IN ('create_disk','create_overlay','copy_vars','promote_baseline',
                                'import_media','delete_file','export','discard_file')),
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

INSERT INTO journal_v6(intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
                       temp_path, final_path, resolution, created_at, updated_at)
SELECT intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
       temp_path, final_path, resolution, created_at, updated_at
FROM journal;

DROP TABLE journal;
ALTER TABLE journal_v6 RENAME TO journal;
CREATE INDEX journal_open ON journal(state) WHERE state IN ('pending','applied','needs_review');

CREATE TABLE vm_current_state (
    vm_id           TEXT PRIMARY KEY NOT NULL REFERENCES vms(vm_id) ON DELETE CASCADE,
    baseline_id     TEXT NOT NULL REFERENCES baselines(baseline_id) ON DELETE RESTRICT,
    overlay_disk_id TEXT NOT NULL UNIQUE REFERENCES disks(disk_id) ON DELETE RESTRICT,
    vars_fw_id      TEXT UNIQUE REFERENCES firmware_state(fw_id) ON DELETE RESTRICT,
    restored_at     TEXT NOT NULL
) STRICT;
CREATE INDEX vm_current_state_baseline ON vm_current_state(baseline_id);

-- The overlay of the current state is backed by the base disk of its baseline.
CREATE TRIGGER vm_current_state_consistent
BEFORE INSERT ON vm_current_state
BEGIN
    SELECT RAISE(ABORT, 'lightvm:current_state_inconsistent')
    WHERE NOT EXISTS (
        SELECT 1 FROM disks d JOIN baselines b ON d.backing_disk_id = b.base_disk_id
        WHERE d.disk_id = NEW.overlay_disk_id AND d.role = 'overlay'
          AND b.baseline_id = NEW.baseline_id
          AND d.vm_id = NEW.vm_id AND b.vm_id = NEW.vm_id);
END;

CREATE TRIGGER vm_current_state_consistent_update
BEFORE UPDATE ON vm_current_state
BEGIN
    SELECT RAISE(ABORT, 'lightvm:current_state_inconsistent')
    WHERE NOT EXISTS (
        SELECT 1 FROM disks d JOIN baselines b ON d.backing_disk_id = b.base_disk_id
        WHERE d.disk_id = NEW.overlay_disk_id AND d.role = 'overlay'
          AND b.baseline_id = NEW.baseline_id
          AND d.vm_id = NEW.vm_id AND b.vm_id = NEW.vm_id);
END;

ALTER TABLE runs ADD COLUMN clean_exit INTEGER CHECK (clean_exit IS NULL OR clean_exit IN (0,1));

ALTER TABLE baselines ADD COLUMN derived_from TEXT;

ALTER TABLE baselines ADD COLUMN config_revision INTEGER CHECK (config_revision IS NULL OR config_revision >= 1);
