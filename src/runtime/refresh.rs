//! Refresh tokens (spec §4A.1, decisions 6/45/61/72): opaque 256-bit values,
//! only their HMAC is stored in each tenant's `_platform_refresh_tokens`.
//! Every use rotates; reuse of a spent token revokes the whole family, except
//! one reissue inside the reuse-grace window.

use serde_json::{Map, Value};

/// Family lifetime, idle timeout and reuse grace, in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RefreshPolicy {
    pub family_ttl: i64,
    pub idle_ttl: i64,
    pub grace: i64,
}

impl Default for RefreshPolicy {
    fn default() -> Self {
        Self {
            family_ttl: 30 * 86_400,
            idle_ttl: 7 * 86_400,
            grace: 10,
        }
    }
}

/// Result of presenting a refresh token.
#[derive(Debug, PartialEq)]
pub enum Rotated {
    /// Rotated: the caller gets a fresh token (already stored).
    /// `expires_in`: seconds until the new token expires if unused, the
    /// smaller of the idle window and what is left of the family lifetime.
    Ok {
        user_id: String,
        family_id: String,
        expires_in: i64,
    },
    /// A spent token was replayed: the family is now revoked.
    Reuse { family_id: String },
    /// Unknown, revoked or expired.
    Invalid,
}

pub enum RevokeTarget<'a> {
    /// The family of this token hash, only if it belongs to `user` (logout).
    FamilyOf { hash: &'a str, user: &'a str },
    /// Every family of this user: `logout-all`, or `email-verified` after
    /// `user.complete-verification` (decision 67).
    User { user: &'a str, reason: &'static str },
}

/// Columns of `_platform_refresh_tokens`.
pub fn table_statements(schema: &str) -> Result<Vec<String>, String> {
    let qualified = format!("{}._platform_refresh_tokens", super::sql::quote(schema)?);
    Ok(vec![
        format!("CREATE SCHEMA IF NOT EXISTS {}", super::sql::quote(schema)?),
        format!(
            "CREATE TABLE IF NOT EXISTS {qualified} (\
             id uuid PRIMARY KEY, tenant_id text NOT NULL, family_id uuid NOT NULL, \
             user_id text NOT NULL, token_hash text NOT NULL UNIQUE, parent_id uuid, \
             replaced_by uuid, created_at timestamptz NOT NULL, used_at timestamptz, \
             grace_reissued_at timestamptz, revoked_at timestamptz, revoke_reason text, \
             family_expires_at timestamptz NOT NULL, idle_expires_at timestamptz NOT NULL)"
        ),
        format!("CREATE INDEX IF NOT EXISTS _platform_refresh_family ON {qualified} (family_id)"),
        format!("CREATE INDEX IF NOT EXISTS _platform_refresh_user ON {qualified} (user_id)"),
        format!("ALTER TABLE {qualified} ENABLE ROW LEVEL SECURITY"),
        format!("ALTER TABLE {qualified} FORCE ROW LEVEL SECURITY"),
        format!("DROP POLICY IF EXISTS tenant_isolation ON {qualified}"),
        format!(
            "CREATE POLICY tenant_isolation ON {qualified} USING (\"tenant_id\" = current_setting('app.tenant_id', false)) WITH CHECK (\"tenant_id\" = current_setting('app.tenant_id', false))"
        ),
        // Until the dedicated refresh-store role lands (#51), app `query`
        // steps run as public_reader / public_writer: take away their access
        // so only the platform (the runtime role) can reach this table.
        format!(
            "DO $$ DECLARE r text; BEGIN FOREACH r IN ARRAY ARRAY['public_reader','public_writer','app_rw','PUBLIC'] LOOP \
             IF r = 'PUBLIC' OR EXISTS (SELECT 1 FROM pg_roles WHERE rolname = r) THEN \
             EXECUTE format('REVOKE ALL ON %s FROM %s', {lit}, CASE WHEN r = 'PUBLIC' THEN 'PUBLIC' ELSE quote_ident(r) END); \
             END IF; END LOOP; END $$",
            lit = sql_literal(&qualified)
        ),
    ])
}

fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// In-memory store logic (used by `MemoryExec`), mirroring the SQL path.
pub fn memory_issue(
    rows: &mut Vec<Map<String, Value>>,
    tenant: &str,
    user_id: &str,
    hash: &str,
    policy: RefreshPolicy,
    now: i64,
) -> String {
    let family = uuid::Uuid::new_v4().to_string();
    rows.push(row(
        tenant,
        &family,
        user_id,
        hash,
        None,
        now + policy.family_ttl,
        now + policy.idle_ttl,
        now,
    ));
    family
}

#[allow(clippy::too_many_arguments)]
fn row(
    tenant: &str,
    family: &str,
    user: &str,
    hash: &str,
    parent: Option<&str>,
    family_exp: i64,
    idle_exp: i64,
    now: i64,
) -> Map<String, Value> {
    let mut r = Map::new();
    r.insert("id".into(), uuid::Uuid::new_v4().to_string().into());
    r.insert("tenant_id".into(), tenant.into());
    r.insert("family_id".into(), family.into());
    r.insert("user_id".into(), user.into());
    r.insert("token_hash".into(), hash.into());
    r.insert(
        "parent_id".into(),
        parent.map(Value::from).unwrap_or(Value::Null),
    );
    r.insert("created_at".into(), now.into());
    r.insert("family_expires_at".into(), family_exp.into());
    r.insert("idle_expires_at".into(), idle_exp.into());
    for k in [
        "replaced_by",
        "used_at",
        "grace_reissued_at",
        "revoked_at",
        "revoke_reason",
    ] {
        r.insert(k.into(), Value::Null);
    }
    r
}

fn int(r: &Map<String, Value>, k: &str) -> Option<i64> {
    r.get(k).and_then(Value::as_i64)
}

fn text(r: &Map<String, Value>, k: &str) -> String {
    r.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

pub fn memory_rotate(
    rows: &mut Vec<Map<String, Value>>,
    tenant: &str,
    hash: &str,
    new_hash: &str,
    policy: RefreshPolicy,
    now: i64,
) -> Rotated {
    let Some(idx) = rows
        .iter()
        .position(|r| text(r, "token_hash") == hash && text(r, "tenant_id") == tenant)
    else {
        return Rotated::Invalid;
    };
    let cur = rows[idx].clone();
    let family = text(&cur, "family_id");
    let revoke_family = |rows: &mut Vec<Map<String, Value>>, reason: &str| {
        for r in rows
            .iter_mut()
            .filter(|r| text(r, "family_id") == family && r["revoked_at"].is_null())
        {
            r.insert("revoked_at".into(), now.into());
            r.insert("revoke_reason".into(), reason.into());
        }
    };
    if !cur["revoked_at"].is_null() {
        if text(&cur, "revoke_reason") == "grace-reissue" {
            revoke_family(rows, "reuse-detected");
            return Rotated::Reuse { family_id: family };
        }
        return Rotated::Invalid;
    }
    let expired = now >= int(&cur, "family_expires_at").unwrap_or(0)
        || now >= int(&cur, "idle_expires_at").unwrap_or(0);
    // A spent token is checked before expiry, so a late replay still
    // revokes a live family.
    let used_at = int(&cur, "used_at");
    let old_child = text(&cur, "replaced_by");
    let child_unused = rows
        .iter()
        .find(|r| text(r, "id") == old_child)
        .is_some_and(|r| r["used_at"].is_null() && r["revoked_at"].is_null());
    let grace_ok = matches!(used_at, Some(used) if now - used < policy.grace)
        && cur["grace_reissued_at"].is_null()
        && child_unused;
    if used_at.is_some() && !grace_ok {
        revoke_family(rows, "reuse-detected");
        return Rotated::Reuse { family_id: family };
    }
    if expired {
        return Rotated::Invalid;
    }
    let user = text(&cur, "user_id");
    let child = row(
        tenant,
        &family,
        &user,
        new_hash,
        Some(&text(&cur, "id")),
        int(&cur, "family_expires_at").unwrap_or(0),
        now + policy.idle_ttl,
        now,
    );
    let child_id = text(&child, "id");
    match used_at {
        None => {
            rows[idx].insert("used_at".into(), now.into());
            rows[idx].insert("replaced_by".into(), child_id.into());
        }
        Some(_) => {
            // Grace reissue: only while the earlier child is still unused.
            for r in rows
                .iter_mut()
                .filter(|r| text(r, "id") == old_child && r["revoked_at"].is_null())
            {
                r.insert("revoked_at".into(), now.into());
                r.insert("revoke_reason".into(), "grace-reissue".into());
            }
            rows[idx].insert("replaced_by".into(), child_id.into());
            rows[idx].insert("grace_reissued_at".into(), now.into());
        }
    }
    let expires_in = policy
        .idle_ttl
        .min(int(&cur, "family_expires_at").unwrap_or(0) - now);
    rows.push(child);
    Rotated::Ok {
        user_id: user,
        family_id: family,
        expires_in,
    }
}

pub fn memory_revoke(
    rows: &mut [Map<String, Value>],
    tenant: &str,
    target: &RevokeTarget<'_>,
    now: i64,
) -> u64 {
    let families: Vec<String> = match target {
        RevokeTarget::FamilyOf { hash, user } => rows
            .iter()
            .filter(|r| {
                text(r, "token_hash") == *hash
                    && text(r, "tenant_id") == tenant
                    && text(r, "user_id") == *user
            })
            .map(|r| text(r, "family_id"))
            .collect(),
        RevokeTarget::User { .. } => Vec::new(),
    };
    let mut n = 0;
    for r in rows
        .iter_mut()
        .filter(|r| text(r, "tenant_id") == tenant && r["revoked_at"].is_null())
    {
        let hit = match target {
            RevokeTarget::FamilyOf { .. } => families.contains(&text(r, "family_id")),
            RevokeTarget::User { user, .. } => text(r, "user_id") == *user,
        };
        if hit {
            r.insert("revoked_at".into(), now.into());
            let reason = match target {
                RevokeTarget::FamilyOf { .. } => "logout",
                RevokeTarget::User { reason, .. } => reason,
            };
            r.insert("revoke_reason".into(), reason.into());
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "tenant0001";

    fn policy() -> RefreshPolicy {
        RefreshPolicy {
            family_ttl: 1000,
            idle_ttl: 100,
            grace: 10,
        }
    }

    fn ok_family(r: Rotated) -> String {
        match r {
            Rotated::Ok {
                family_id, user_id, ..
            } => {
                assert_eq!(user_id, "u1");
                family_id
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_use_rotates_and_the_new_token_works() {
        let mut rows = Vec::new();
        let fam = memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        assert_eq!(
            ok_family(memory_rotate(&mut rows, T, "h0", "h1", policy(), 1)),
            fam
        );
        assert_eq!(
            ok_family(memory_rotate(&mut rows, T, "h1", "h2", policy(), 2)),
            fam
        );
        assert_eq!(
            memory_rotate(&mut rows, T, "nope", "x", policy(), 2),
            Rotated::Invalid
        );
        assert_eq!(
            memory_rotate(&mut rows, "tenant0002", "h2", "x", policy(), 2),
            Rotated::Invalid,
            "other tenant"
        );
    }

    #[test]
    fn replay_after_grace_revokes_the_whole_family() {
        let mut rows = Vec::new();
        let fam = memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        ok_family(memory_rotate(&mut rows, T, "h0", "h1", policy(), 1));
        assert_eq!(
            memory_rotate(&mut rows, T, "h0", "h9", policy(), 20),
            Rotated::Reuse { family_id: fam }
        );
        assert_eq!(
            memory_rotate(&mut rows, T, "h1", "h2", policy(), 21),
            Rotated::Invalid
        );
        assert!(rows.iter().all(|r| !r["revoked_at"].is_null()));
        assert!(rows.iter().any(|r| r["revoke_reason"] == "reuse-detected"));
    }

    #[test]
    fn grace_needs_an_unused_child() {
        let mut rows = Vec::new();
        memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        ok_family(memory_rotate(&mut rows, T, "h0", "h1", policy(), 1));
        ok_family(memory_rotate(&mut rows, T, "h1", "h2", policy(), 2));
        assert!(matches!(
            memory_rotate(&mut rows, T, "h0", "x", policy(), 3),
            Rotated::Reuse { .. }
        ));
    }

    #[test]
    fn expires_in_is_capped_by_the_family_lifetime() {
        let mut rows = Vec::new();
        memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        let expires = |r: Rotated| match r {
            Rotated::Ok { expires_in, .. } => expires_in,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            expires(memory_rotate(&mut rows, T, "h0", "h1", policy(), 1)),
            100
        );
        let mut cur = "h1".to_string();
        let mut now = 1;
        while now < 950 {
            now += 90;
            let next = format!("h{now}");
            let left = expires(memory_rotate(&mut rows, T, &cur, &next, policy(), now));
            assert_eq!(left, 100.min(1000 - now), "at {now}");
            cur = next;
        }
    }

    #[test]
    fn replaying_an_expired_spent_token_still_revokes_the_family() {
        let mut rows = Vec::new();
        let fam = memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        ok_family(memory_rotate(&mut rows, T, "h0", "h1", policy(), 1));
        // h0 is idle-expired by now, h1 was refreshed in time.
        let late = policy().idle_ttl + 5;
        ok_family(memory_rotate(&mut rows, T, "h1", "h2", policy(), late - 10));
        assert!(matches!(
            memory_rotate(&mut rows, T, "h0", "x", policy(), late),
            Rotated::Reuse { family_id } if family_id == fam
        ));
    }

    #[test]
    fn app_query_roles_lose_access_to_the_table() {
        let sql = table_statements("t_tenant0001").unwrap().join("\n");
        assert!(sql.contains("REVOKE ALL ON %s FROM %s"), "{sql}");
        assert!(sql.contains("public_reader") && sql.contains("public_writer"));
    }

    #[test]
    fn grace_reissues_once_and_kills_the_lost_child() {
        let mut rows = Vec::new();
        let fam = memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        ok_family(memory_rotate(&mut rows, T, "h0", "h1", policy(), 1));
        // Client lost the h1 response and retries h0 within the grace window.
        ok_family(memory_rotate(&mut rows, T, "h0", "h1b", policy(), 3));
        // A second retry is reuse.
        assert_eq!(
            memory_rotate(&mut rows, T, "h0", "h1c", policy(), 4),
            Rotated::Reuse {
                family_id: fam.clone()
            }
        );
        let mut rows = Vec::new();
        let fam = memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        ok_family(memory_rotate(&mut rows, T, "h0", "h1", policy(), 1));
        ok_family(memory_rotate(&mut rows, T, "h0", "h1b", policy(), 3));
        // The superseded child h1 surfacing later is theft: family revoked.
        assert_eq!(
            memory_rotate(&mut rows, T, "h1", "x", policy(), 5),
            Rotated::Reuse { family_id: fam }
        );
        assert_eq!(
            memory_rotate(&mut rows, T, "h1b", "y", policy(), 6),
            Rotated::Invalid
        );
    }

    #[test]
    fn idle_and_family_expiry() {
        let mut rows = Vec::new();
        memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        assert_eq!(
            memory_rotate(&mut rows, T, "h0", "h1", policy(), 100),
            Rotated::Invalid,
            "idle"
        );
        let mut rows = Vec::new();
        memory_issue(&mut rows, T, "u1", "h0", policy(), 0);
        let mut cur = "h0".to_string();
        let mut now = 0;
        // Keep it busy: idle never expires, but the family lifetime does.
        loop {
            now += 90;
            let next = format!("h{now}");
            match memory_rotate(&mut rows, T, &cur, &next, policy(), now) {
                Rotated::Ok { .. } => cur = next,
                Rotated::Invalid => break,
                other => panic!("{other:?}"),
            }
        }
        assert!((1000..1100).contains(&now), "{now}");
    }

    #[test]
    fn revoke_family_only_for_its_owner_and_revoke_all() {
        let mut rows = Vec::new();
        memory_issue(&mut rows, T, "u1", "a0", policy(), 0);
        memory_issue(&mut rows, T, "u1", "b0", policy(), 0);
        memory_issue(&mut rows, T, "u2", "c0", policy(), 0);
        let other = RevokeTarget::FamilyOf {
            hash: "a0",
            user: "u2",
        };
        assert_eq!(memory_revoke(&mut rows, T, &other, 1), 0, "not the owner");
        let mine = RevokeTarget::FamilyOf {
            hash: "a0",
            user: "u1",
        };
        assert_eq!(memory_revoke(&mut rows, T, &mine, 1), 1);
        assert_eq!(
            memory_rotate(&mut rows, T, "a0", "x", policy(), 2),
            Rotated::Invalid
        );
        ok_family(memory_rotate(&mut rows, T, "b0", "b1", policy(), 2));
        assert_eq!(
            memory_revoke(
                &mut rows,
                T,
                &RevokeTarget::User {
                    user: "u1",
                    reason: "logout-all"
                },
                3
            ),
            2
        );
        assert_eq!(
            memory_rotate(&mut rows, T, "b1", "x", policy(), 4),
            Rotated::Invalid
        );
        assert!(matches!(
            memory_rotate(&mut rows, T, "c0", "c1", policy(), 4),
            Rotated::Ok { .. }
        ));
    }
}
