DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'ternilo_runtime') THEN
        GRANT SELECT, INSERT, UPDATE, DELETE ON cloud_worker_credentials TO ternilo_runtime;
        GRANT SELECT, INSERT ON cloud_storage_roots TO ternilo_runtime;
    END IF;
END $$;
