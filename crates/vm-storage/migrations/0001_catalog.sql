-- 0001: catalogo iniziale (ROADMAP 6.3, 8, 9).
-- Tutti i timestamp sono UTC in formato RFC 3339 con millisecondi ("...Z").
-- I percorsi gestiti sono relativi alla radice del catalogo, con separatore "/".
-- I CHECK sui percorsi sono una difesa aggiuntiva: la validazione completa e'
-- in Rust (`ManagedPath`). char(92) e' la barra rovesciata.

CREATE TABLE catalog_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;

CREATE TABLE vms (
    vm_id           TEXT PRIMARY KEY NOT NULL,
    name            TEXT NOT NULL COLLATE NOCASE UNIQUE,
    guest_os        TEXT NOT NULL,
    lifecycle       TEXT NOT NULL DEFAULT 'stopped'
                    CHECK (lifecycle IN ('stopped','starting','running','stopping','failed','recovering')),
    config_version  INTEGER NOT NULL CHECK (config_version >= 1),
    config_json     TEXT NOT NULL CHECK (json_valid(config_json)),
    revision        INTEGER NOT NULL DEFAULT 1 CHECK (revision >= 1),
    manifest_sha256 TEXT CHECK (manifest_sha256 IS NULL OR length(manifest_sha256) = 64),
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL
) STRICT;
CREATE INDEX vms_guest_os ON vms(guest_os);
CREATE INDEX vms_lifecycle ON vms(lifecycle);

CREATE TABLE media (
    media_id        TEXT PRIMARY KEY NOT NULL,
    sha256          TEXT NOT NULL CHECK (length(sha256) = 64),
    size_bytes      INTEGER NOT NULL CHECK (size_bytes >= 0),
    kind            TEXT NOT NULL CHECK (kind IN ('iso','data')),
    declared_origin TEXT NOT NULL,
    approved_at     TEXT NOT NULL,
    managed_path    TEXT NOT NULL UNIQUE
                    CHECK (substr(managed_path,1,1) <> '/' AND instr(managed_path,'..') = 0
                           AND instr(managed_path,':') = 0 AND instr(managed_path,char(92)) = 0),
    status          TEXT NOT NULL DEFAULT 'approved'
                    CHECK (status IN ('approved','revoked','missing'))
) STRICT;
CREATE INDEX media_sha256 ON media(sha256);

-- Supporti collegati a una VM: installazione e supporto dati in sola lettura (ADR-0008).
CREATE TABLE vm_media (
    vm_id    TEXT NOT NULL REFERENCES vms(vm_id) ON DELETE CASCADE,
    slot     TEXT NOT NULL CHECK (slot IN ('install','data')),
    media_id TEXT NOT NULL REFERENCES media(media_id) ON DELETE RESTRICT,
    PRIMARY KEY (vm_id, slot)
) STRICT;
CREATE INDEX vm_media_media ON vm_media(media_id);

-- Dischi QCOW2 gestiti. 'primary' = disco di lavoro della VM fuori dalle
-- baseline; 'base' = disco immutabile di una baseline; 'overlay' = disco di
-- un'esecuzione con backing fissato su una base (nessuna catena, sezione 8).
CREATE TABLE disks (
    disk_id            TEXT PRIMARY KEY NOT NULL,
    vm_id              TEXT NOT NULL REFERENCES vms(vm_id) ON DELETE RESTRICT,
    role               TEXT NOT NULL CHECK (role IN ('primary','base','overlay')),
    format             TEXT NOT NULL DEFAULT 'qcow2' CHECK (format = 'qcow2'),
    backing_disk_id    TEXT REFERENCES disks(disk_id) ON DELETE RESTRICT,
    virtual_size_bytes INTEGER NOT NULL CHECK (virtual_size_bytes > 0),
    managed_path       TEXT NOT NULL UNIQUE
                       CHECK (substr(managed_path,1,1) <> '/' AND instr(managed_path,'..') = 0
                              AND instr(managed_path,':') = 0 AND instr(managed_path,char(92)) = 0),
    sha256             TEXT CHECK (sha256 IS NULL OR length(sha256) = 64),
    status             TEXT NOT NULL DEFAULT 'ready'
                       CHECK (status IN ('ready','deleting','missing','failed')),
    created_at         TEXT NOT NULL,
    CHECK ((role = 'overlay') = (backing_disk_id IS NOT NULL))
) STRICT;
CREATE INDEX disks_vm ON disks(vm_id);
CREATE INDEX disks_backing ON disks(backing_disk_id);

CREATE TRIGGER disks_overlay_backing_is_base
BEFORE INSERT ON disks
WHEN NEW.role = 'overlay'
BEGIN
    SELECT RAISE(ABORT, 'lightvm:backing_not_base')
    WHERE NOT EXISTS (SELECT 1 FROM disks d
                      WHERE d.disk_id = NEW.backing_disk_id AND d.role = 'base');
END;

-- Stato firmware scrivibile (UEFI vars): della VM, di una baseline o di un'esecuzione.
CREATE TABLE firmware_state (
    fw_id         TEXT PRIMARY KEY NOT NULL,
    vm_id         TEXT NOT NULL REFERENCES vms(vm_id) ON DELETE RESTRICT,
    scope         TEXT NOT NULL CHECK (scope IN ('vm','baseline','run')),
    source_fw_id  TEXT REFERENCES firmware_state(fw_id) ON DELETE SET NULL,
    managed_path  TEXT NOT NULL UNIQUE
                  CHECK (substr(managed_path,1,1) <> '/' AND instr(managed_path,'..') = 0
                         AND instr(managed_path,':') = 0 AND instr(managed_path,char(92)) = 0),
    sha256        TEXT CHECK (sha256 IS NULL OR length(sha256) = 64),
    size_bytes    INTEGER NOT NULL CHECK (size_bytes >= 0),
    created_at    TEXT NOT NULL
) STRICT;
CREATE INDEX firmware_vm ON firmware_state(vm_id);

CREATE TABLE baselines (
    baseline_id         TEXT PRIMARY KEY NOT NULL,
    vm_id               TEXT NOT NULL REFERENCES vms(vm_id) ON DELETE RESTRICT,
    base_disk_id        TEXT NOT NULL UNIQUE REFERENCES disks(disk_id) ON DELETE RESTRICT,
    vars_fw_id          TEXT REFERENCES firmware_state(fw_id) ON DELETE RESTRICT,
    label               TEXT NOT NULL,
    manifest_sha256     TEXT NOT NULL CHECK (length(manifest_sha256) = 64),
    -- Sezione 8: una baseline si registra soltanto a VM spenta.
    created_powered_off INTEGER NOT NULL CHECK (created_powered_off = 1),
    -- Arresto forzato: stato non pulito, mai promosso automaticamente a verificato.
    clean_shutdown      INTEGER NOT NULL CHECK (clean_shutdown IN (0,1)),
    protected           INTEGER NOT NULL DEFAULT 0 CHECK (protected IN (0,1)),
    created_at          TEXT NOT NULL
) STRICT;
CREATE INDEX baselines_vm ON baselines(vm_id);

CREATE TABLE runs (
    run_id            TEXT PRIMARY KEY NOT NULL,
    vm_id             TEXT NOT NULL REFERENCES vms(vm_id) ON DELETE RESTRICT,
    baseline_id       TEXT REFERENCES baselines(baseline_id) ON DELETE RESTRICT,
    overlay_disk_id   TEXT REFERENCES disks(disk_id) ON DELETE RESTRICT,
    vars_fw_id        TEXT REFERENCES firmware_state(fw_id) ON DELETE RESTRICT,
    state             TEXT NOT NULL DEFAULT 'starting'
                      CHECK (state IN ('starting','running','stopping','stopped','failed','recovering')),
    started_at        TEXT NOT NULL,
    ended_at          TEXT,
    termination_cause TEXT
                      CHECK (termination_cause IS NULL OR termination_cause IN
                             ('shutdown','force_stop','emergency_stop','guest_exit','crash',
                              'supervisor_lost','start_failed','recovered'))
) STRICT;
CREATE INDEX runs_vm ON runs(vm_id);
CREATE INDEX runs_baseline ON runs(baseline_id);
CREATE INDEX runs_overlay ON runs(overlay_disk_id);

-- Una baseline referenziata da un'esecuzione o da un overlay non e' cancellabile
-- (sezione 8). Le FK RESTRICT bloccano gia'; il trigger produce un errore nominato.
CREATE TRIGGER baselines_no_delete_referenced
BEFORE DELETE ON baselines
BEGIN
    SELECT RAISE(ABORT, 'lightvm:baseline_referenced')
    WHERE EXISTS (SELECT 1 FROM runs r WHERE r.baseline_id = OLD.baseline_id)
       OR EXISTS (SELECT 1 FROM disks d WHERE d.backing_disk_id = OLD.base_disk_id);
END;

CREATE TRIGGER baselines_no_delete_protected
BEFORE DELETE ON baselines
WHEN OLD.protected = 1
BEGIN
    SELECT RAISE(ABORT, 'lightvm:baseline_protected');
END;

-- La base di una baseline e' immutabile: nessun cambio di percorso, hash o ruolo.
CREATE TRIGGER disks_base_immutable
BEFORE UPDATE OF managed_path, sha256, role, backing_disk_id, virtual_size_bytes ON disks
WHEN OLD.role = 'base'
BEGIN
    SELECT RAISE(ABORT, 'lightvm:base_immutable');
END;

CREATE TRIGGER disks_no_delete_referenced
BEFORE DELETE ON disks
BEGIN
    SELECT RAISE(ABORT, 'lightvm:disk_referenced')
    WHERE EXISTS (SELECT 1 FROM baselines b WHERE b.base_disk_id = OLD.disk_id)
       OR EXISTS (SELECT 1 FROM disks d WHERE d.backing_disk_id = OLD.disk_id)
       OR EXISTS (SELECT 1 FROM runs r WHERE r.overlay_disk_id = OLD.disk_id);
END;

CREATE TRIGGER media_no_delete_referenced
BEFORE DELETE ON media
BEGIN
    SELECT RAISE(ABORT, 'lightvm:media_referenced')
    WHERE EXISTS (SELECT 1 FROM vm_media m WHERE m.media_id = OLD.media_id);
END;

CREATE TABLE operations (
    operation_id TEXT PRIMARY KEY NOT NULL,
    vm_id        TEXT NOT NULL REFERENCES vms(vm_id) ON DELETE RESTRICT,
    run_id       TEXT REFERENCES runs(run_id) ON DELETE RESTRICT,
    kind         TEXT NOT NULL,
    state        TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed','cancelled')),
    revision     INTEGER NOT NULL CHECK (revision >= 1),
    error_code   TEXT,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL
) STRICT;
CREATE INDEX operations_vm ON operations(vm_id);
CREATE INDEX operations_run ON operations(run_id);

-- Sessioni e grant: solo persistenza (la logica e' in vm-core). Nessuna
-- credenziale e nessun token sono conservati qui.
CREATE TABLE sessions (
    session_id        TEXT PRIMARY KEY NOT NULL,
    objective         TEXT NOT NULL,
    state             TEXT NOT NULL
                      CHECK (state IN ('pending','active','paused','revoked','expired','closed')),
    policy_revision   INTEGER NOT NULL CHECK (policy_revision >= 1),
    created_at        TEXT NOT NULL,
    expires_at        TEXT NOT NULL,
    ended_at          TEXT,
    termination_cause TEXT
) STRICT;

CREATE TABLE grants (
    grant_id          TEXT PRIMARY KEY NOT NULL,
    session_id        TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    vm_id             TEXT NOT NULL REFERENCES vms(vm_id) ON DELETE RESTRICT,
    capabilities_json TEXT NOT NULL CHECK (json_valid(capabilities_json)),
    quotas_json       TEXT NOT NULL CHECK (json_valid(quotas_json)),
    created_at        TEXT NOT NULL,
    expires_at        TEXT NOT NULL,
    revoked_at        TEXT
) STRICT;
CREATE INDEX grants_session ON grants(session_id);
CREATE INDEX grants_vm ON grants(vm_id);

-- Audit append-only: tipo, identificativi, esito, ordine monotono, timestamp.
-- Nessun payload: niente testo digitato, segreti o percorsi host.
CREATE TABLE audit (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    at           TEXT NOT NULL,
    action       TEXT NOT NULL,
    outcome      TEXT NOT NULL CHECK (outcome IN ('allowed','denied','succeeded','failed')),
    vm_id        TEXT,
    run_id       TEXT,
    session_id   TEXT,
    operation_id TEXT,
    error_code   TEXT
);

CREATE TRIGGER audit_no_update BEFORE UPDATE ON audit
BEGIN
    SELECT RAISE(ABORT, 'lightvm:audit_append_only');
END;
CREATE TRIGGER audit_no_delete BEFORE DELETE ON audit
BEGIN
    SELECT RAISE(ABORT, 'lightvm:audit_append_only');
END;

CREATE TABLE artifacts (
    artifact_id  TEXT PRIMARY KEY NOT NULL,
    run_id       TEXT NOT NULL REFERENCES runs(run_id) ON DELETE RESTRICT,
    kind         TEXT NOT NULL CHECK (kind IN ('screenshot','log','report')),
    managed_path TEXT NOT NULL UNIQUE
                 CHECK (substr(managed_path,1,1) <> '/' AND instr(managed_path,'..') = 0
                        AND instr(managed_path,':') = 0 AND instr(managed_path,char(92)) = 0),
    sha256       TEXT NOT NULL CHECK (length(sha256) = 64),
    size_bytes   INTEGER NOT NULL CHECK (size_bytes >= 0),
    created_at   TEXT NOT NULL,
    retain_until TEXT
) STRICT;
CREATE INDEX artifacts_run ON artifacts(run_id);

-- Journal a intenti per operazioni che attraversano DB e filesystem (sezione 6.3).
CREATE TABLE journal (
    intent_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT NOT NULL
                CHECK (kind IN ('create_disk','create_overlay','copy_vars','promote_baseline',
                                'import_media','delete_file','export')),
    state       TEXT NOT NULL CHECK (state IN ('pending','applied','committed','aborted')),
    vm_id       TEXT,
    -- Intenti della stessa operazione (es. ripristino = overlay + vars) condividono l'id.
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
CREATE INDEX journal_open ON journal(state) WHERE state IN ('pending','applied');
