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
  tenant_id text NOT NULL DEFAULT current_setting('app.tenant_id', true),
  email text NOT NULL,
  name text,
  password text,
  roles text[] NOT NULL DEFAULT '{}',
  email_verified_at timestamptz,
  email_verify_hash bytea,
  email_verify_expires_at timestamptz,
  email_verify_purpose text CHECK (email_verify_purpose IN ('register', 'invite')),
  deleted_at timestamptz
);
CREATE UNIQUE INDEX ON {schema}.users (tenant_id, email_verify_hash);
CREATE UNIQUE INDEX ON {schema}.users (tenant_id, email);
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
-- Spec §12.11.1: outbox for the platform mail worker.
CREATE TABLE {schema}.email_outbox (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id text NOT NULL DEFAULT current_setting('app.tenant_id', true),
  user_id uuid NULL,
  email text NOT NULL CHECK (char_length(email) <= 254),
  recipient_hash bytea NOT NULL,
  template text NOT NULL,
  token bytea NULL,
  sent_at timestamptz NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);
ALTER TABLE {schema}.email_outbox ENABLE ROW LEVEL SECURITY;
ALTER TABLE {schema}.email_outbox FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON {schema}.email_outbox
  USING (tenant_id = current_setting('app.tenant_id', false))
  WITH CHECK (tenant_id = current_setting('app.tenant_id', false));
GRANT INSERT ON {schema}.email_outbox TO public_writer;
