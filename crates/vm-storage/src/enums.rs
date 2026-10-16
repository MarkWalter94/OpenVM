//! Enumerations persisted as text, with the same values as the SQL CHECK constraints.

macro_rules! str_enum {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $($(#[$vdoc])* #[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// Persisted text value.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            /// Parses the text value.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($text => Some(Self::$variant),)+ _ => None }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl rusqlite::ToSql for $name {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(rusqlite::types::ToSqlOutput::from(self.as_str()))
            }
        }

        impl rusqlite::types::FromSql for $name {
            fn column_result(
                v: rusqlite::types::ValueRef<'_>,
            ) -> rusqlite::types::FromSqlResult<Self> {
                let s = v.as_str()?;
                Self::parse(s).ok_or_else(|| {
                    rusqlite::types::FromSqlError::Other(
                        format!("invalid value {s:?} for {}", stringify!($name)).into(),
                    )
                })
            }
        }
    };
}

str_enum!(
    /// Last persisted lifecycle state. The observed state is derived from the
    /// process and QMP (section 6.3); this value is used for reconciliation.
    Lifecycle {
        /// Powered off.
        Stopped => "stopped",
        /// Starting.
        Starting => "starting",
        /// Running.
        Running => "running",
        /// Stopping.
        Stopping => "stopping",
        /// Failed.
        Failed => "failed",
        /// Under reconciliation.
        Recovering => "recovering",
    }
);

str_enum!(
    /// Kind of a registry media.
    MediaKind {
        /// Installation ISO.
        Iso => "iso",
        /// Read-only data ISO (ADR-0008).
        Data => "data",
    }
);

str_enum!(
    /// Status of a registry media.
    MediaStatus {
        /// Approved.
        Approved => "approved",
        /// Revoked.
        Revoked => "revoked",
        /// Managed file missing.
        Missing => "missing",
    }
);

str_enum!(
    /// Slot through which a media is attached to a VM.
    MediaSlot {
        /// Installation media.
        Install => "install",
        /// Data media.
        Data => "data",
    }
);

str_enum!(
    /// Role of a managed disk.
    DiskRole {
        /// Working disk of the VM.
        Primary => "primary",
        /// Immutable disk of a baseline.
        Base => "base",
        /// Overlay of a run, backed by a base.
        Overlay => "overlay",
    }
);

str_enum!(
    /// Status of a managed disk.
    DiskStatus {
        /// Ready.
        Ready => "ready",
        /// Journaled deletion in progress.
        Deleting => "deleting",
        /// File missing, detected by reconciliation.
        Missing => "missing",
        /// Failed.
        Failed => "failed",
    }
);

str_enum!(
    /// Scope of a firmware state.
    FirmwareScope {
        /// Of the VM (working disk).
        Vm => "vm",
        /// Of a baseline (immutable).
        Baseline => "baseline",
        /// Of a run.
        Run => "run",
    }
);

str_enum!(
    /// State of a run.
    RunState {
        /// Starting.
        Starting => "starting",
        /// Running.
        Running => "running",
        /// Stopping.
        Stopping => "stopping",
        /// Terminated.
        Stopped => "stopped",
        /// Failed.
        Failed => "failed",
        /// Under reconciliation.
        Recovering => "recovering",
    }
);

str_enum!(
    /// Termination cause of a run.
    TerminationCause {
        /// Graceful shutdown.
        Shutdown => "shutdown",
        /// Forced stop.
        ForceStop => "force_stop",
        /// Emergency Stop.
        EmergencyStop => "emergency_stop",
        /// Guest exit.
        GuestExit => "guest_exit",
        /// QEMU crash.
        Crash => "crash",
        /// Supervisor lost.
        SupervisorLost => "supervisor_lost",
        /// Start failed.
        StartFailed => "start_failed",
        /// Closed by reconciliation.
        Recovered => "recovered",
    }
);

str_enum!(
    /// State of an operation.
    OperationState {
        /// Queued.
        Queued => "queued",
        /// In progress.
        Running => "running",
        /// Succeeded.
        Succeeded => "succeeded",
        /// Failed.
        Failed => "failed",
        /// Cancelled.
        Cancelled => "cancelled",
    }
);

str_enum!(
    /// State of an agent session.
    SessionState {
        /// Awaiting approval.
        Pending => "pending",
        /// Active.
        Active => "active",
        /// Paused.
        Paused => "paused",
        /// Revoked.
        Revoked => "revoked",
        /// Expired.
        Expired => "expired",
        /// Closed.
        Closed => "closed",
    }
);

str_enum!(
    /// Outcome recorded in the audit log.
    AuditOutcome {
        /// Allowed.
        Allowed => "allowed",
        /// Denied.
        Denied => "denied",
        /// Succeeded.
        Succeeded => "succeeded",
        /// Failed.
        Failed => "failed",
    }
);

str_enum!(
    /// Kind of artifact.
    ArtifactKind {
        /// Screenshot.
        Screenshot => "screenshot",
        /// Log.
        Log => "log",
        /// Report.
        Report => "report",
    }
);

str_enum!(
    /// Kind of journal intent.
    IntentKind {
        /// Creation of a working disk (`qemu-img create`).
        CreateDisk => "create_disk",
        /// Creation of an overlay from a base.
        CreateOverlay => "create_overlay",
        /// Copy of a firmware state.
        CopyVars => "copy_vars",
        /// Materialization of a new baseline base.
        PromoteBaseline => "promote_baseline",
        /// Managed copy of an approved media (catalogs before ADR-0029: new
        /// imports use the file in place and record no intent; unfinished
        /// intents of this kind still reconcile).
        ImportMedia => "import_media",
        /// Deletion of a managed file or directory.
        DeleteFile => "delete_file",
        /// Export to a user-chosen destination (outside the catalog).
        Export => "export",
        /// Discard of a file of the VM's previous current state inside a
        /// restore (schema v6): unlike `delete_file` it never rolls forward
        /// on its own; until the whole restore commits, reconciliation puts
        /// the file back.
        DiscardFile => "discard_file",
        /// Import of a VMware VM by conversion (schema v9, ADR-0030): the
        /// converted disk is published as the VM's primary disk and the VM
        /// row is inserted in the commit (target kind `vm`).
        ImportVm => "import_vm",
        /// User snapshot taken (schema v10, ADR-0032): the snapshot's files
        /// (UEFI variables, thumbnail) in the temporary directory, the
        /// QCOW2 internal snapshot in the VM's disk, then the row. Settled
        /// by the presence of the tag in the disk.
        TakeSnapshot => "take_snapshot",
        /// Revert to a user snapshot (schema v10, ADR-0032): disk, UEFI
        /// variables and configuration. Settled by comparing the disk's
        /// active L1 table with the snapshot's.
        RevertSnapshot => "revert_snapshot",
        /// Deletion of a user snapshot (schema v10, ADR-0032). Settled by
        /// the absence of the tag in the disk.
        DeleteSnapshot => "delete_snapshot",
    }
);

impl IntentKind {
    /// `true` for the user snapshot intents (settled from the disk's QCOW2
    /// snapshot table, not by the generic file rules).
    pub fn is_snapshot(self) -> bool {
        matches!(
            self,
            Self::TakeSnapshot | Self::RevertSnapshot | Self::DeleteSnapshot
        )
    }
}

impl IntentKind {
    /// `true` for intents that create a file in a managed location.
    pub fn is_create(self) -> bool {
        matches!(
            self,
            Self::CreateDisk
                | Self::CreateOverlay
                | Self::CopyVars
                | Self::PromoteBaseline
                | Self::ImportMedia
                | Self::ImportVm
        )
    }
}

str_enum!(
    /// State of a journal intent.
    IntentState {
        /// Recorded, filesystem work not yet completed.
        Pending => "pending",
        /// Filesystem work completed, DB not yet updated.
        Applied => "applied",
        /// DB updated in the same transaction as the journal.
        Committed => "committed",
        /// Aborted (by the application or by reconciliation).
        Aborted => "aborted",
        /// Reconciliation failed: needs review. The VM stays busy and
        /// reconciliation retries on every open or call to `recover`.
        NeedsReview => "needs_review",
    }
);

str_enum!(
    /// Catalog object involved in an intent.
    TargetKind {
        /// Row of `disks`.
        Disk => "disk",
        /// Row of `firmware_state`.
        Firmware => "firmware",
        /// Row of `baselines`.
        Baseline => "baseline",
        /// Row of `artifacts`.
        Artifact => "artifact",
        /// Row of `media`.
        Media => "media",
        /// Row of `vms` (deletion of a VM with its files, schema v5).
        Vm => "vm",
        /// Row of `snapshots` (user snapshots, schema v10).
        Snapshot => "snapshot",
        /// No row (export).
        None => "none",
    }
);
