use crate::lock::{find_section, LockDoc, Table};

pub(crate) fn map_fields(table: &mut Table) -> Result<(), String> {
    let accepted = [
        "name/欄位/欄位名/名稱/字段",
        "type/類型/型別/类型",
        "rules/規則/约束/約束/规则",
        "system/系統/系统",
        "required/必填/必須/必需",
    ];
    let mut indices = [None; 5];
    for (index, header) in table.headers.iter().enumerate() {
        let header = header.trim().to_lowercase();
        let Some(slot) = accepted
            .iter()
            .position(|names| names.split('/').any(|name| name == header))
        else {
            return Err(format!(
                "unknown Fields header {header:?}; accepted names: {}",
                accepted.join(", ")
            ));
        };
        if indices[slot].replace(index).is_some() {
            return Err(format!("duplicate Fields header {header}"));
        }
    }
    if indices[0].is_none() || indices[1].is_none() {
        return Err("Fields requires name and type columns".into());
    }
    let mut rows = Vec::new();
    for row in &table.rows {
        let get = |slot: usize| {
            indices[slot]
                .and_then(|i| row.get(i))
                .cloned()
                .unwrap_or_default()
        };
        let mut rules = get(2);
        match get(4).trim().to_lowercase().as_str() {
            "yes" | "true" | "y" | "是" | "必填" | "✓" | "✔" | "required" => {
                if !rules.split(',').any(|r| r.trim() == "required") {
                    rules = if rules.is_empty() {
                        "required".into()
                    } else {
                        format!("required, {rules}")
                    };
                }
            }
            "" | "no" | "false" | "n" | "否" | "唔係" | "-" => (),
            other => return Err(format!("invalid required value {other:?}")),
        }
        let system = get(3);
        rows.push(vec![
            get(0),
            get(1),
            rules,
            if system.is_empty() {
                "no".into()
            } else {
                system
            },
        ]);
    }
    table.headers = ["name", "type", "rules", "system"]
        .map(String::from)
        .to_vec();
    table.rows = rows;
    Ok(())
}

pub(crate) fn alias(key: &str) -> Option<&'static str> {
    match key {
        "max_length" | "maxLength" | "max-length" | "max_len" | "maxlen" | "max_chars" => {
            Some("max")
        }
        "min_length" | "minLength" | "min-length" | "min_len" | "minlen" | "min_chars" => {
            Some("min")
        }
        _ => None,
    }
}

pub(crate) fn normalize_rules(rules: &str) -> Result<String, String> {
    let parts = crate::lock::split_top_level(rules);
    let mut values = std::collections::BTreeMap::new();
    let mut has_alias = false;
    for part in &parts {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        has_alias |= alias(key.trim()).is_some();
        let key = alias(key.trim()).unwrap_or(key.trim());
        if matches!(key, "min" | "max") {
            let value = value.trim();
            if values
                .insert(key.to_string(), value.to_string())
                .is_some_and(|old| old != value)
            {
                return Err(format!("conflicting {key} rules"));
            }
        }
    }
    if !has_alias {
        return Ok(rules.into());
    }
    let mut seen = std::collections::HashSet::new();
    Ok(crate::lock::split_top_level(rules)
        .iter()
        .filter_map(|part| {
            let part = part.trim();
            if let Some((key, value)) = part.split_once(':') {
                let key = alias(key.trim()).unwrap_or(key.trim());
                if matches!(key, "min" | "max") && !seen.insert(key.to_string()) {
                    return None;
                }
                Some(format!("{key}: {}", value.trim()))
            } else {
                Some(part.into())
            }
        })
        .collect::<Vec<_>>()
        .join(", "))
}

pub(crate) fn constraints(doc: &LockDoc) -> Result<Vec<Vec<String>>, String> {
    let Some(section) = find_section(&doc.sections, "Constraints") else {
        return Ok(Vec::new());
    };
    if section.table.is_some() || !section.blocks.is_empty() {
        return Err("Constraints must contain unique lists".into());
    }
    let fields = find_section(&doc.sections, "Fields")
        .and_then(|s| s.table.as_ref())
        .ok_or("Constraints requires Fields")?;
    let mut entries = Vec::new();
    for bullet in &section.bullets {
        if bullet.key != "unique" {
            return Err(format!("unknown Constraints key {}", bullet.key));
        }
        let columns = crate::lock::parse_string_list(&bullet.value)?;
        if columns.len() < 2 {
            return Err("Constraints unique requires at least two columns; use Fields unique for one column".into());
        }
        let mut seen = std::collections::HashSet::new();
        for col in &columns {
            if col == "tenant_id" || !seen.insert(col) {
                return Err(format!("invalid/repeated Constraints column {col}"));
            }
            let row = fields
                .rows
                .iter()
                .find(|r| r.first() == Some(col))
                .ok_or_else(|| format!("unknown Constraints column {col}"))?;
            if row.get(1).is_some_and(|t| t == "password")
                || row.get(2).is_some_and(|r| r.contains("sensitive"))
            {
                return Err(format!("sensitive Constraints column {col}"));
            }
        }
        let mut identity = columns.clone();
        identity.sort();
        if entries.iter().any(|entry: &Vec<String>| {
            let mut other = entry.clone();
            other.sort();
            other == identity
        }) {
            return Err("duplicate Constraints entry".into());
        }
        entries.push(columns);
    }
    Ok(entries)
}

/// Deliberately narrow grammar: unsupported index features cannot prove uniqueness.
pub(crate) fn unique_index_of(sql: &str, table: &str, columns: &[String]) -> bool {
    let ident = r#"(?:[a-z_][a-z0-9_]*|"[a-z_][a-z0-9_]*")"#;
    let re = regex::Regex::new(&format!(r"(?i)^\s*CREATE\s+UNIQUE\s+INDEX\s+(?:IF\s+NOT\s+EXISTS\s+)?{ident}\s+ON\s+({ident})\s*(?:USING\s+btree\s*)?\(([^()]+)\)\s*(?:WHERE\s+deleted_at\s+IS\s+NULL\s*)?$" )).unwrap();
    let Ok(statements) = sqlparser::parser::Parser::parse_sql(
        &sqlparser::dialect::PostgreSqlDialect {},
        sql.trim().trim_matches('`'),
    ) else {
        return false;
    };
    let column_re = regex::Regex::new(&format!("(?i)^{ident}$")).unwrap();
    statements
        .iter()
        .filter(|statement| matches!(statement, sqlparser::ast::Statement::CreateIndex(_)))
        .any(|statement| {
            let rendered = statement.to_string();
            let Some(cap) = re.captures(&rendered) else {
                return false;
            };
            if cap[1].trim_matches('"') != table {
                return false;
            }
            if cap[2].split(',').any(|c| !column_re.is_match(c.trim())) {
                return false;
            }
            let cols = cap[2]
                .split(',')
                .map(|c| c.trim().trim_matches('"').to_string())
                .collect::<Vec<_>>();
            cols == columns
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_indexes_fail_closed() {
        let columns = ["tenant_id", "a", "b"].map(String::from);
        for sql in [
            "CREATE UNIQUE INDEX u ON things (tenant_id, a, b)",
            "CREATE UNIQUE INDEX IF NOT EXISTS u ON things USING btree (tenant_id, a, b) WHERE deleted_at IS NULL;",
            "CREATE UNIQUE INDEX \"u\" ON \"things\" (\"tenant_id\", \"a\", \"b\")",
        ] { assert!(unique_index_of(sql, "things", &columns), "{sql}"); }
        for sql in [
            "CREATE INDEX u ON things (tenant_id, a, b)",
            "CREATE UNIQUE INDEX u ON other (tenant_id, a, b)",
            "CREATE UNIQUE INDEX u ON public.things (tenant_id, a, b)",
            "CREATE UNIQUE INDEX u ON things (a, b)",
            "CREATE UNIQUE INDEX u ON things (tenant_id, b, a)",
            "CREATE UNIQUE INDEX u ON things (tenant_id, a, b) WHERE active = true",
            "CREATE UNIQUE INDEX u ON things (tenant_id, a, b) WHERE deleted_at IS NULL AND active",
            "CREATE UNIQUE INDEX u ON things (tenant_id, lower(a), b)",
            "CREATE UNIQUE INDEX u ON things (tenant_id, a, b) INCLUDE (c)",
            "CREATE UNIQUE INDEX u ON things (tenant_id, a, b) WITH (fillfactor=70)",
            "CREATE UNIQUE INDEX u ON things USING hash (tenant_id, a, b)",
            "CREATE UNIQUE INDEX u ON things (tenant_id, a, b) NULLS NOT DISTINCT",
            "ALTER TABLE things ADD CONSTRAINT u UNIQUE (tenant_id, a, b)",
            "SELECT 'x; CREATE UNIQUE INDEX u ON things (tenant_id, a, b); x'",
            "CREATE UNIQUE INDEX u ON things (tenant_id, a, b) WHERE deleted_at IS NULL OR true",
        ] {
            assert!(!unique_index_of(sql, "things", &columns), "{sql}");
        }
    }

    #[test]
    fn aliases_and_conflicts() {
        for alias in [
            "max_length",
            "maxLength",
            "max-length",
            "max_len",
            "maxlen",
            "max_chars",
        ] {
            assert_eq!(
                normalize_rules(&format!("required, {alias}: 80, max: 80")).unwrap(),
                "required, max: 80"
            );
            assert!(normalize_rules(&format!("{alias}: 80, max: 81")).is_err());
        }
        for alias in [
            "min_length",
            "minLength",
            "min-length",
            "min_len",
            "minlen",
            "min_chars",
        ] {
            assert_eq!(normalize_rules(&format!("{alias}: 1")).unwrap(), "min: 1");
        }
        assert_eq!(
            normalize_rules("required, max: 80").unwrap(),
            "required, max: 80"
        );
        assert_eq!(
            normalize_rules("enum: [a, b], maxLength: 80").unwrap(),
            "enum: [a, b], max: 80"
        );
        assert_eq!(alias("max_size"), None);
    }

    #[test]
    fn headers_and_required_values() {
        for (value, required) in [
            ("yes", true),
            ("TRUE", true),
            ("y", true),
            ("是", true),
            ("必填", true),
            ("✓", true),
            ("✔", true),
            ("required", true),
            ("no", false),
            ("false", false),
            ("n", false),
            ("否", false),
            ("唔係", false),
            ("-", false),
            ("", false),
        ] {
            let mut table = Table {
                headers: vec!["類型".into(), "名稱".into(), "必填".into()],
                rows: vec![vec!["string".into(), "a".into(), value.into()]],
            };
            map_fields(&mut table).unwrap();
            assert_eq!(
                table.rows[0],
                ["a", "string", if required { "required" } else { "" }, "no"]
            );
        }
        for headers in [
            vec!["name", "type", "wat"],
            vec!["name"],
            vec!["name", "type", "名稱"],
        ] {
            let mut table = Table {
                headers: headers.into_iter().map(String::from).collect(),
                rows: vec![],
            };
            assert!(map_fields(&mut table).is_err());
        }
        let mut table = Table {
            headers: vec!["name".into(), "type".into(), "required".into()],
            rows: vec![vec!["a".into(), "string".into(), "maybe".into()]],
        };
        assert!(map_fields(&mut table).is_err());
    }
    #[test]
    fn invalid_constraints_are_rejected() {
        use crate::lock::{parse_sections, Kind, Meta};
        for entry in [
            "- unique: []",
            "- unique: [a]",
            "- unique: [tenant_id, a]",
            "- unique: [a, a]",
            "- unique: [a, missing]",
            "- unique: [a, secret]",
            "- unique: [a, pass]",
            "- foreign: [a, b]",
            "- unique: [a, b]\n- unique: [b, a]",
        ] {
            let doc = LockDoc { meta: Meta::pending(Kind::Model, None, "thing", "models/thing/thing.md"), sections: parse_sections(&format!("## Fields\n| name | type | rules | system |\n| --- | --- | --- | --- |\n| a | string | | no |\n| b | string | | no |\n| secret | string | sensitive | no |\n| pass | password | | no |\n## Constraints\n{entry}\n")).unwrap() };
            assert!(constraints(&doc).is_err(), "{entry}");
        }
    }
}
