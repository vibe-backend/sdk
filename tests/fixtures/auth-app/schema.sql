-- {schema} and {app} are substituted by tests/auth_ops.rs.
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
