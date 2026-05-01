-- Server profile disable/enable: nullable `disabled_at` timestamp.
--
-- A non-null `disabled_at` blocks new launches and SSH-side setup actions
-- (auth-check, host-key preflight, host-key trust). Existing live terminal
-- sessions are unaffected — disable is a launch-time gate, not a runtime
-- kill switch. See SPEC.md "Inventory lifecycle and destructive-action
-- policy" for the contract this column upholds.
--
-- Existing rows default to enabled (NULL). The column intentionally has no
-- default expression: setting `disabled_at` is always an explicit action
-- and must come from the route layer, not a schema fallback.

ALTER TABLE server_profiles
    ADD COLUMN disabled_at TIMESTAMPTZ;
