-- 0007: guest OS detected from the contents of an installation ISO
-- (ADR-0027).
--
-- media.detected_guest: JSON of lightvm_proto::DetectedGuest (os, arch,
--   build, edition) read from the managed copy at import; NULL when nothing
--   supported was recognized, for data media, or not examined yet.
-- media.guest_examined: 1 once the managed copy has been examined (whatever
--   the outcome), 0 for media imported before this version: those are
--   examined once, lazily, the first time the registry is listed.

ALTER TABLE media ADD COLUMN detected_guest TEXT
    CHECK (detected_guest IS NULL OR json_valid(detected_guest));

ALTER TABLE media ADD COLUMN guest_examined INTEGER NOT NULL DEFAULT 0
    CHECK (guest_examined IN (0,1));
