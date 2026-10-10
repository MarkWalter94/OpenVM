-- 0004: sessione agente che ha richiesto un'operazione (ROADMAP 6.3, 7.4 riga
-- "Sessioni"). Il core la usa per l'idempotenza per sessione e per l'audit;
-- prima della v4 il catalogo non la conservava. Nessun vincolo verso
-- `sessions`: le sessioni agente vivono nel core e possono non essere
-- persistite (F4). Le operazioni esistenti restano senza sessione (utente).

ALTER TABLE operations ADD COLUMN session_id TEXT;
