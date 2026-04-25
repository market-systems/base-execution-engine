DO $$
BEGIN
    -- Runtime role (read/write)
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'execution_engine') THEN
        CREATE ROLE execution_engine LOGIN PASSWORD 'password'
            NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION;
    END IF;

    -- Read-only role for Grafana & analytics
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'execution_engine_ro') THEN
        CREATE ROLE execution_engine_ro LOGIN PASSWORD 'password'
            NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION;
    END IF;
END
$$;


SELECT 'CREATE DATABASE execution_engine OWNER execution_engine ENCODING ''UTF8'' TEMPLATE template1'
WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'execution_engine')
\gexec

GRANT CONNECT ON DATABASE execution_engine TO execution_engine, execution_engine_ro;


\connect execution_engine


GRANT USAGE, CREATE ON SCHEMA public TO execution_engine;
GRANT USAGE          ON SCHEMA public TO execution_engine_ro;

GRANT SELECT ON ALL TABLES    IN SCHEMA public TO execution_engine_ro;
GRANT SELECT ON ALL SEQUENCES IN SCHEMA public TO execution_engine_ro;

ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT SELECT ON TABLES    TO execution_engine_ro;
ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT SELECT ON SEQUENCES TO execution_engine_ro;
