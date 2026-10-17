//! `vibe build` rules for sign-jwt claims (claims source hardening).
use std::fs;
use std::path::{Path, PathBuf};

use vibe::compile::{regenerate_project, CompileOptions};
use vibe::validate::{validate_project, ValidateOptions};

fn copy_dir(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let to = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to);
        } else {
            fs::copy(entry.path(), to).unwrap();
        }
    }
}

fn method(claims: &str, custom: &str) -> String {
    format!(
        "# issue\n\n## Signature\n\n- input: {{email: string}}\n- output: {{access_token: string}}\n- writes: false\n- capabilities: [sign-token]\n\n## Calls\n\n(none)\n\n## Steps\n\n### s1\n\n- op: query\n- sql: select id::text as id from users where email = $1\n- params: [input.email]\n- out: u\n\n### s2\n\n- op: sign-jwt\n- alg: ES256\n- key-ref: VIBE_JWT_PRIVATE_KEY\n- claims: {claims}\n- custom-claims: {custom}\n- out: t\n\n### s3\n\n- op: respond\n- value: {{access_token: t.access_token}}\n"
    )
}

async fn problems(claims: &str, custom: &str) -> String {
    let root = std::env::temp_dir().join(format!("vibe-claims-{}", uuid::Uuid::new_v4()));
    copy_dir(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/shop"),
        &root,
    );
    fs::create_dir_all(root.join("models/product/methods")).unwrap();
    fs::write(
        root.join("models/product/methods/issue.md"),
        method(claims, custom),
    )
    .unwrap();
    let compiled = regenerate_project(&CompileOptions {
        root: &root,
        llm: None,
        llm_config: None,
        check: false,
    })
    .await;
    if let Err(err) = compiled {
        return err.to_string();
    }
    match validate_project(&ValidateOptions {
        dev: false,
        root: &root,
        deploy: false,
    }) {
        Ok(_) => String::new(),
        Err(report) => report.to_string(),
    }
}

#[tokio::test]
async fn sign_jwt_claims_source_rules() {
    // One test per process: it sets VIBE_JWT_ROLES_CLAIM.
    std::env::set_var("VIBE_JWT_ROLES_CLAIM", "groups");
    let ok = problems("{sub: u.id}", "[]").await;
    assert!(ok.is_empty(), "{ok}");
    for claims in [
        "{sub: input.email}",
        "{sub: ctx.user.sub}",
        "{sub: req.body.sub}",
        "{sub: 'x'}",
    ] {
        let text = problems(claims, "[]").await;
        assert!(
            text.contains("sub must be a field of a query step result"),
            "{claims}: {text}"
        );
    }
    let text = problems("{sub: u.id, groups: u.id}", "[groups]").await;
    assert!(text.contains("configured roles claim"), "{text}");
}
