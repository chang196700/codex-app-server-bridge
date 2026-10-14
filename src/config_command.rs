use crate::config::{self, Config};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use serde_json::Value as JsonValue;
use std::{fs, io::ErrorKind, path::Path};
use toml_edit::{DocumentMut, Item, Table, Value};

#[derive(Debug, Subcommand)]
pub enum Action {
    /// Print configured values merged with built-in defaults as JSON.
    List,
    /// Print one configured value, or its built-in default, as JSON.
    Get { key: String },
    /// Set a schema-defined key after validating the complete configuration.
    Set { key: String, value: String },
}

pub fn run(action: &Action, config_path: Option<&Path>, user_home: Option<&Path>) -> Result<()> {
    let path = config::resolve_config_path(config_path, user_home)?;
    let defaults = serde_json::to_value(Config::default())?;
    match action {
        Action::List => {
            let document = read_document(&path, config_path.is_none())?;
            let configured = parse_config(&document, &path)?;
            println!("{}", serde_json::to_string_pretty(&configured)?);
        }
        Action::Get { key } => {
            let parts = key_parts(key)?;
            schema_value(&defaults, &parts)?;
            let document = read_document(&path, config_path.is_none())?;
            let configured = serde_json::to_value(parse_config(&document, &path)?)?;
            let value = schema_value(&configured, &parts)?;
            println!("{}", serde_json::to_string(value)?);
        }
        Action::Set { key, value } => {
            let parts = key_parts(key)?;
            let schema = schema_value(&defaults, &parts)?;
            let replacement = parse_value(value, schema)?;
            let home = config::home_directory(user_home)?;
            let _lock = config::lock_configuration(&path, &home)?;
            // Read only after locking so simultaneous setters retain each
            // other's changes. Never serialize runtime Settings over this file.
            let mut document = read_document(&path, true)?;
            set_item(document.as_table_mut(), &parts, replacement)?;
            let configured = parse_config(&document, &path)?;
            configured.validate(user_home)?;
            config::write_configuration(&path, document.to_string().as_bytes(), &home)?;
            let configured = serde_json::to_value(configured)?;
            println!(
                "{}",
                serde_json::to_string(schema_value(&configured, &parts)?)?
            );
        }
    }
    Ok(())
}

fn read_document(path: &Path, allow_missing: bool) -> Result<DocumentMut> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .with_context(|| format!("invalid TOML in {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound && allow_missing => {
            Ok(DocumentMut::new())
        }
        Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn parse_config(document: &DocumentMut, path: &Path) -> Result<Config> {
    toml::from_str(&document.to_string())
        .with_context(|| format!("invalid configuration in {}", path.display()))
}

fn key_parts(key: &str) -> Result<Vec<&str>> {
    let parts: Vec<_> = key.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        bail!("configuration key must contain nonempty dot-separated names");
    }
    Ok(parts)
}

fn schema_value<'a>(schema: &'a JsonValue, parts: &[&str]) -> Result<&'a JsonValue> {
    let mut value = schema;
    for part in parts {
        value = value.as_object().and_then(|table| table.get(*part))
            .with_context(|| format!("unknown configuration key {:?}; only keys defined by the current schema are accepted", parts.join(".")))?;
    }
    Ok(value)
}

fn parse_value(input: &str, schema: &JsonValue) -> Result<Value> {
    // All optional fields in Config are strings/paths. A bare path or host is
    // accepted verbatim; quoted strings must be valid TOML strings.
    if (schema.is_string() || schema.is_null()) && !input.starts_with(['\'', '"']) {
        return Ok(Value::from(input));
    }
    let document: DocumentMut = format!("value = {input}\n")
        .parse()
        .context("value must be a valid TOML value of the field's type")?;
    if document.as_table().len() != 1 {
        bail!("supply one TOML value, not additional configuration assignments");
    }
    let value = document
        .get("value")
        .and_then(Item::as_value)
        .context("a scalar TOML value is required")?
        .clone();
    let correct_type = match schema {
        JsonValue::String(_) | JsonValue::Null => value.is_str(),
        JsonValue::Bool(_) => value.is_bool(),
        JsonValue::Number(_) => value.is_integer(),
        JsonValue::Array(_) => value.is_array(),
        JsonValue::Object(_) => value.is_inline_table(),
    };
    if !correct_type {
        bail!("TOML value has the wrong type for this configuration field");
    }
    Ok(value)
}

fn set_item(table: &mut Table, parts: &[&str], mut value: Value) -> Result<()> {
    let (name, rest) = parts.split_first().context("configuration key is empty")?;
    if rest.is_empty() {
        if let Some(item) = table.get_mut(name) {
            if let Some(previous) = item.as_value() {
                // Value decor owns spacing and inline comments; replacing only
                // the Item also retains the Key's leading comment decoration.
                *value.decor_mut() = previous.decor().clone();
            }
            *item = Item::Value(value);
        } else {
            table.insert(name, Item::Value(value));
        }
    } else {
        if !table.contains_key(name) {
            // The complete key has already been checked against Config's
            // schema, so this can only create an explicitly defined table.
            table.insert(name, Item::Table(Table::new()));
        }
        let nested = table
            .get_mut(name)
            .and_then(Item::as_table_mut)
            .context("configuration key traverses a value instead of a TOML table")?;
        set_item(nested, rest, value)?;
    }
    Ok(())
}
