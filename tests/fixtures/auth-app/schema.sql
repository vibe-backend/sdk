-- {schema} and {app} are substituted by tests/auth_ops.rs.
-- Query steps run as public_reader / public_writer (SET LOCAL ROLE); the
-- roles are cluster-wide, so create them only when missing.
DO $$ BEGIN
  CREATE ROLE public_reader NOLOGIN;
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
DO $$ BEGIN
  CREATE ROLE public_writer NOLOGIN;
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
GRANT public_reader, public_writer TO {app};
CREATE SCHEMA {schema};
CREATE TABLE {schema}.users (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id text NOT NULL,
  email text NOT NULL,
  password text,
  roles text[] NOT NULL DEFAULT '{}',
  email_verified_at timestamptz,
  deleted_at timestamptz
);
ALTER TABLE {schema}.users ENABLE ROW LEVEL SECURITY;
ALTER TABLE {schema}.users FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON {schema}.users
  USING (tenant_id = current_setting('app.tenant_id', false))
  WITH CHECK (tenant_id = current_setting('app.tenant_id', false));
GRANT USAGE ON SCHEMA {schema} TO {app};
GRANT SELECT, INSERT, UPDATE ON {schema}.users TO {app};
GRANT USAGE ON SCHEMA {schema} TO public_reader, public_writer;
GRANT SELECT ON {schema}.users TO public_reader;
GRANT SELECT, INSERT, UPDATE ON {schema}.users TO public_writer;
