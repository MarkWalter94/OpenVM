-- TPM state belongs to the same checkpoint as disk and UEFI variables.
-- Opaque permanent state is registered as firmware for accounting/guards.
CREATE TABLE baseline_tpm (
    baseline_id TEXT PRIMARY KEY REFERENCES baselines(baseline_id) ON DELETE CASCADE,
    fw_id TEXT NOT NULL UNIQUE REFERENCES firmware_state(fw_id)
);
CREATE TABLE snapshot_tpm (
    snapshot_id TEXT PRIMARY KEY REFERENCES snapshots(snapshot_id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL,
    size_bytes INTEGER NOT NULL CHECK(size_bytes >= 0)
);
CREATE TABLE current_tpm (
    vm_id TEXT PRIMARY KEY REFERENCES vms(vm_id) ON DELETE CASCADE,
    fw_id TEXT NOT NULL UNIQUE REFERENCES firmware_state(fw_id)
);
