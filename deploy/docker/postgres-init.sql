\set ON_ERROR_STOP on
\getenv owner_password TERNILO_POSTGRES_OWNER_PASSWORD
\getenv app_password TERNILO_POSTGRES_APP_PASSWORD
CREATE ROLE ternilo_owner LOGIN PASSWORD :'owner_password';
CREATE ROLE ternilo_runtime NOLOGIN;
CREATE ROLE ternilo_app LOGIN PASSWORD :'app_password';
GRANT ternilo_runtime TO ternilo_app;
CREATE DATABASE ternilo OWNER ternilo_owner;
\connect ternilo
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
GRANT USAGE ON SCHEMA public TO ternilo_runtime;
