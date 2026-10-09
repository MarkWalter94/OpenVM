-- 0002: contabilita' dello storage (ROADMAP 7.2 punto 8, sezione 8, ADR-0017).
-- Budget applicativo con monitoraggio: NON e' una quota rigida del sistema operativo.

-- Byte occupati misurati sul file (non la dimensione virtuale).
ALTER TABLE disks ADD COLUMN allocated_bytes INTEGER NOT NULL DEFAULT 0 CHECK (allocated_bytes >= 0);

CREATE TABLE storage_budgets (
    scope_key          TEXT PRIMARY KEY NOT NULL,
    vm_id              TEXT REFERENCES vms(vm_id) ON DELETE CASCADE,
    budget_bytes       INTEGER NOT NULL CHECK (budget_bytes >= 0),
    host_reserve_bytes INTEGER NOT NULL DEFAULT 0 CHECK (host_reserve_bytes >= 0),
    updated_at         TEXT NOT NULL,
    -- 'global' per il catalogo, 'vm:<vm_id>' per una VM.
    CHECK ((scope_key = 'global' AND vm_id IS NULL) OR (scope_key = 'vm:' || vm_id))
) STRICT;
