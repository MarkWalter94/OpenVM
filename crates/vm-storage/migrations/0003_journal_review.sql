-- 0003: stato 'needs_review' del journal (ROADMAP 6.3; ADR-0005, ADR-0025).
-- Un intento che la riconciliazione non riesce a risolvere (errore di I/O,
-- file in uso, collegamento rifiutato) resta "da verificare": la VM rimane
-- occupata e il tentativo si ripete a ogni riconciliazione. SQLite non modifica
-- un CHECK esistente: la tabella e' ricostruita conservando righe, identificativi
-- e sequenza AUTOINCREMENT.

CREATE TABLE journal_v3 (
    intent_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL
                CHECK (kind IN ('create_disk','create_overlay','copy_vars','promote_baseline',
                                'import_media','delete_file','export')),
    state       TEXT NOT NULL
                CHECK (state IN ('pending','applied','committed','aborted','needs_review')),
    vm_id       TEXT,
    operation_id TEXT,
    target_kind TEXT NOT NULL
                CHECK (target_kind IN ('disk','firmware','baseline','artifact','media','none')),
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

INSERT INTO journal_v3(intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
                       temp_path, final_path, resolution, created_at, updated_at)
SELECT intent_id, kind, state, vm_id, operation_id, target_kind, target_id,
       temp_path, final_path, resolution, created_at, updated_at
FROM journal;

DROP TABLE journal;
ALTER TABLE journal_v3 RENAME TO journal;
CREATE INDEX journal_open ON journal(state) WHERE state IN ('pending','applied','needs_review');
