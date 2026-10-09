//! Enumerazioni persistite come testo, con gli stessi valori dei CHECK SQL.

macro_rules! str_enum {
    ($(#[$doc:meta])* $name:ident { $($(#[$vdoc:meta])* $variant:ident => $text:literal),+ $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
        pub enum $name {
            $($(#[$vdoc])* #[serde(rename = $text)] $variant),+
        }

        impl $name {
            /// Valore testuale persistito.
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $text),+ }
            }

            /// Lettura dal valore testuale.
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
                        format!("valore {s:?} non valido per {}", stringify!($name)).into(),
                    )
                })
            }
        }
    };
}

str_enum!(
    /// Ultimo stato persistito del ciclo di vita. Lo stato osservato deriva da
    /// processo e QMP (sezione 6.3); questo valore serve alla riconciliazione.
    Lifecycle {
        /// Spenta.
        Stopped => "stopped",
        /// In avvio.
        Starting => "starting",
        /// In esecuzione.
        Running => "running",
        /// In arresto.
        Stopping => "stopping",
        /// Fallita.
        Failed => "failed",
        /// In riconciliazione.
        Recovering => "recovering",
    }
);

str_enum!(
    /// Tipo di media del registro.
    MediaKind {
        /// ISO di installazione.
        Iso => "iso",
        /// ISO dati in sola lettura (ADR-0008).
        Data => "data",
    }
);

str_enum!(
    /// Stato di un media del registro.
    MediaStatus {
        /// Approvato.
        Approved => "approved",
        /// Revocato.
        Revoked => "revoked",
        /// File gestito mancante.
        Missing => "missing",
    }
);

str_enum!(
    /// Slot di collegamento di un media a una VM.
    MediaSlot {
        /// Supporto di installazione.
        Install => "install",
        /// Supporto dati.
        Data => "data",
    }
);

str_enum!(
    /// Ruolo di un disco gestito.
    DiskRole {
        /// Disco di lavoro della VM.
        Primary => "primary",
        /// Disco immutabile di una baseline.
        Base => "base",
        /// Overlay di un'esecuzione con backing su una base.
        Overlay => "overlay",
    }
);

str_enum!(
    /// Stato di un disco gestito.
    DiskStatus {
        /// Pronto.
        Ready => "ready",
        /// Cancellazione con journal in corso.
        Deleting => "deleting",
        /// File mancante rilevato dalla riconciliazione.
        Missing => "missing",
        /// Fallito.
        Failed => "failed",
    }
);

str_enum!(
    /// Ambito di uno stato firmware.
    FirmwareScope {
        /// Della VM (disco di lavoro).
        Vm => "vm",
        /// Di una baseline (immutabile).
        Baseline => "baseline",
        /// Di un'esecuzione.
        Run => "run",
    }
);

str_enum!(
    /// Stato di un'esecuzione.
    RunState {
        /// In avvio.
        Starting => "starting",
        /// In esecuzione.
        Running => "running",
        /// In arresto.
        Stopping => "stopping",
        /// Terminata.
        Stopped => "stopped",
        /// Fallita.
        Failed => "failed",
        /// In riconciliazione.
        Recovering => "recovering",
    }
);

str_enum!(
    /// Causa di terminazione di un'esecuzione.
    TerminationCause {
        /// Arresto gentile.
        Shutdown => "shutdown",
        /// Arresto forzato.
        ForceStop => "force_stop",
        /// Emergency Stop.
        EmergencyStop => "emergency_stop",
        /// Uscita del guest.
        GuestExit => "guest_exit",
        /// Crash di QEMU.
        Crash => "crash",
        /// Perdita del supervisor.
        SupervisorLost => "supervisor_lost",
        /// Avvio fallito.
        StartFailed => "start_failed",
        /// Chiusa dalla riconciliazione.
        Recovered => "recovered",
    }
);

str_enum!(
    /// Stato di un'operazione.
    OperationState {
        /// In coda.
        Queued => "queued",
        /// In corso.
        Running => "running",
        /// Riuscita.
        Succeeded => "succeeded",
        /// Fallita.
        Failed => "failed",
        /// Annullata.
        Cancelled => "cancelled",
    }
);

str_enum!(
    /// Stato di una sessione agente.
    SessionState {
        /// In attesa di approvazione.
        Pending => "pending",
        /// Attiva.
        Active => "active",
        /// In pausa.
        Paused => "paused",
        /// Revocata.
        Revoked => "revoked",
        /// Scaduta.
        Expired => "expired",
        /// Chiusa.
        Closed => "closed",
    }
);

str_enum!(
    /// Esito registrato nell'audit.
    AuditOutcome {
        /// Autorizzata.
        Allowed => "allowed",
        /// Negata.
        Denied => "denied",
        /// Riuscita.
        Succeeded => "succeeded",
        /// Fallita.
        Failed => "failed",
    }
);

str_enum!(
    /// Tipo di artefatto.
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
    /// Tipo di intento del journal.
    IntentKind {
        /// Creazione di un disco di lavoro (`qemu-img create`).
        CreateDisk => "create_disk",
        /// Creazione di un overlay da una base.
        CreateOverlay => "create_overlay",
        /// Copia di uno stato firmware.
        CopyVars => "copy_vars",
        /// Materializzazione di una nuova base di baseline.
        PromoteBaseline => "promote_baseline",
        /// Copia gestita di un media approvato.
        ImportMedia => "import_media",
        /// Cancellazione di un file o di una directory gestiti.
        DeleteFile => "delete_file",
        /// Export verso una destinazione scelta dall'utente (fuori dal catalogo).
        Export => "export",
    }
);

impl IntentKind {
    /// `true` per gli intenti che creano un file in una posizione gestita.
    pub fn is_create(self) -> bool {
        matches!(
            self,
            Self::CreateDisk
                | Self::CreateOverlay
                | Self::CopyVars
                | Self::PromoteBaseline
                | Self::ImportMedia
        )
    }
}

str_enum!(
    /// Stato di un intento del journal.
    IntentState {
        /// Registrato, filesystem non ancora completato.
        Pending => "pending",
        /// Filesystem completato, DB non ancora aggiornato.
        Applied => "applied",
        /// DB aggiornato nella stessa transazione del journal.
        Committed => "committed",
        /// Annullato (dall'applicazione o dalla riconciliazione).
        Aborted => "aborted",
    }
);

str_enum!(
    /// Oggetto del catalogo coinvolto da un intento.
    TargetKind {
        /// Riga di `disks`.
        Disk => "disk",
        /// Riga di `firmware_state`.
        Firmware => "firmware",
        /// Riga di `baselines`.
        Baseline => "baseline",
        /// Riga di `artifacts`.
        Artifact => "artifact",
        /// Riga di `media`.
        Media => "media",
        /// Nessuna riga (export).
        None => "none",
    }
);
