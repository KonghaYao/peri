-- Second database for the Peri Swarm state store.
--
-- The image already creates POSTGRES_DB (peri_session) for the Peri session
-- store. The gateway requires the Swarm state schema in a *different* database
-- on the same instance; pointing PERI_SWARM_STATE_STORE at peri_session is
-- refused at startup. This script creates that second database.
--
-- Runs only on an empty data directory (docker-entrypoint-initdb.d); the guard
-- keeps a manual re-run safe.

SELECT 'CREATE DATABASE peri_swarm_state'
WHERE NOT EXISTS (SELECT 1 FROM pg_database WHERE datname = 'peri_swarm_state')
\gexec
