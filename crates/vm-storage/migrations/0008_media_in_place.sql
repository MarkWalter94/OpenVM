-- 0008: installation and data ISOs used where the user keeps them (ADR-0029,
-- owner decision amending the copy rule of ADR-0008).
--
-- media.storage: 'managed' = read-only copy in the catalog (every medium
--   imported before this version, unchanged and still supported);
--   'referenced' = the user's own file, used in place and never copied.
-- media.managed_path: the managed copy; NULL for a referenced medium.
-- media.external_path: absolute path of the user's file as resolved by the
--   system when it was selected (Windows: final path of the opened handle);
--   NULL for a managed copy. One row per file and kind: selecting the same
--   file again updates its row (hash, size, status), so the VMs that use it
--   keep working.
--
-- SQLite does not alter an existing NOT NULL or CHECK: the table is rebuilt
-- with its rows, index and trigger. `vm_media` keeps referring to
-- `media(media_id)`; the migration runs with foreign keys off and is checked
-- with `foreign_key_check` afterwards (see `migrate.rs`).

CREATE TABLE media_v8 (
    media_id        TEXT PRIMARY KEY NOT NULL,
    sha256          TEXT NOT NULL CHECK (length(sha256) = 64),
    size_bytes      INTEGER NOT NULL CHECK (size_bytes >= 0),
    kind            TEXT NOT NULL CHECK (kind IN ('iso','data')),
    declared_origin TEXT NOT NULL,
    approved_at     TEXT NOT NULL,
    storage         TEXT NOT NULL DEFAULT 'managed'
                    CHECK (storage IN ('managed','referenced')),
    managed_path    TEXT UNIQUE
                    CHECK (managed_path IS NULL OR
                           (substr(managed_path,1,1) <> '/' AND instr(managed_path,'..') = 0
                            AND instr(managed_path,':') = 0 AND instr(managed_path,char(92)) = 0)),
    external_path   TEXT
                    CHECK (external_path IS NULL OR
                           (length(external_path) BETWEEN 2 AND 4096
                            AND instr(external_path, char(0)) = 0)),
    status          TEXT NOT NULL DEFAULT 'approved'
                    CHECK (status IN ('approved','revoked','missing')),
    detected_guest  TEXT
                    CHECK (detected_guest IS NULL OR json_valid(detected_guest)),
    guest_examined  INTEGER NOT NULL DEFAULT 0 CHECK (guest_examined IN (0,1)),
    CHECK ((storage = 'managed' AND managed_path IS NOT NULL AND external_path IS NULL)
        OR (storage = 'referenced' AND managed_path IS NULL AND external_path IS NOT NULL)),
    UNIQUE (external_path, kind)
) STRICT;

INSERT INTO media_v8(media_id, sha256, size_bytes, kind, declared_origin, approved_at,
                     storage, managed_path, external_path, status, detected_guest,
                     guest_examined)
SELECT media_id, sha256, size_bytes, kind, declared_origin, approved_at,
       'managed', managed_path, NULL, status, detected_guest, guest_examined
FROM media;

DROP TABLE media;
ALTER TABLE media_v8 RENAME TO media;

CREATE INDEX media_sha256 ON media(sha256);

CREATE TRIGGER media_no_delete_referenced
BEFORE DELETE ON media
BEGIN
    SELECT RAISE(ABORT, 'lightvm:media_referenced')
    WHERE EXISTS (SELECT 1 FROM vm_media m WHERE m.media_id = OLD.media_id);
END;
