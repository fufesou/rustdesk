use hbb_common::{
    anyhow::{anyhow, Context},
    bail,
    base64::{engine::general_purpose::STANDARD, Engine as _},
    serde_json::{self, Value},
    ResultType,
};
use std::{collections::HashSet, env::VarError};

const TRUSTED_KEYS_ENV: &str = "RUSTDESK_TRUSTED_UPDATE_KEYS";
const ED25519_KEY_SIZE: usize = 32;

#[cfg(not(test))]
pub fn generate() -> ResultType<()> {
    use std::{env, fs, path::PathBuf};

    println!("cargo:rerun-if-env-changed={TRUSTED_KEYS_ENV}");
    println!("cargo:rerun-if-changed=src/update_metadata/build_keys.rs");
    println!("cargo:rustc-check-cfg=cfg(rustdesk_custom_update_keys)");
    let source = match configured_source(env::var(TRUSTED_KEYS_ENV))? {
        Some(source) => source,
        None => {
            println!("cargo:warning={TRUSTED_KEYS_ENV} is unset; using built-in update keys");
            return Ok(());
        }
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").context("missing Cargo OUT_DIR")?)
        .join("trusted_update_keys.rs");
    fs::write(output, source).context("failed to write trusted update keys")?;
    println!("cargo:rustc-cfg=rustdesk_custom_update_keys");
    Ok(())
}

fn configured_source(value: Result<String, VarError>) -> ResultType<Option<String>> {
    match value {
        Ok(encoded) => render_keys(&encoded).map(Some),
        Err(VarError::NotPresent) => Ok(None),
        Err(error) => Err(error).context("invalid trusted update keys environment variable"),
    }
}

fn string_field<'a>(entry: &'a Value, name: &str) -> ResultType<&'a str> {
    entry
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.trim() == *value)
        .ok_or_else(|| anyhow!("invalid trusted update key {name}"))
}

fn parse_key(entry: &Value) -> ResultType<(&str, &str, [u8; ED25519_KEY_SIZE])> {
    let key_id = string_field(entry, "key_id")?;
    let algorithm = string_field(entry, "algorithm")?;
    if algorithm != "ed25519" {
        bail!("unsupported trusted update key algorithm");
    }
    let encoded = string_field(entry, "public_key")?;
    let decoded = STANDARD
        .decode(encoded)
        .context("invalid trusted update public key base64")?;
    if STANDARD.encode(&decoded) != encoded {
        bail!("trusted update public key must use canonical base64");
    }
    let public_key = decoded
        .try_into()
        .map_err(|_| anyhow!("trusted update public key must have {ED25519_KEY_SIZE} bytes"))?;
    Ok((key_id, algorithm, public_key))
}

fn render_keys(encoded: &str) -> ResultType<String> {
    let entries: Vec<Value> =
        serde_json::from_str(encoded).context("invalid trusted update key list JSON")?;
    if entries.is_empty() {
        bail!("{TRUSTED_KEYS_ENV} must be a non-empty list");
    }
    let mut identities = HashSet::new();
    let mut public_keys = HashSet::new();
    let mut source = String::from("const TRUSTED_UPDATE_KEYS: &[TrustedUpdateKey] = &[\n");
    for entry in &entries {
        let (key_id, algorithm, public_key) = parse_key(entry)?;
        if !identities.insert((key_id, algorithm)) || !public_keys.insert(public_key) {
            bail!("duplicate trusted update key identity or public key");
        }
        source.push_str(&format!(
            "TrustedUpdateKey {{ key_id: {key_id:?}, algorithm: {algorithm:?}, public_key: {public_key:?} }},\n"
        ));
    }
    source.push_str("];\n");
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hbb_common::serde_json::json;

    fn key(key_id: &str, byte: u8) -> Value {
        json!({"key_id": key_id, "algorithm": "ed25519",
               "public_key": STANDARD.encode([byte; ED25519_KEY_SIZE])})
    }

    #[test]
    fn generates_all_configured_public_keys() {
        let entries = json!([key("previous", 1), key("current\"key", 2)]);
        let source = render_keys(&entries.to_string()).unwrap();
        assert!(source.contains("key_id: \"previous\""));
        assert!(source.contains("key_id: \"current\\\"key\""));
        assert!(source.contains("algorithm: \"ed25519\""));
        assert!(source.contains(&format!("public_key: {:?}", [2; ED25519_KEY_SIZE])));
    }

    #[test]
    fn preserves_builtin_keys_only_when_unset() {
        assert!(configured_source(Err(VarError::NotPresent))
            .unwrap()
            .is_none());
        assert!(configured_source(Ok(String::new())).is_err());
    }

    #[test]
    fn rejects_invalid_or_ambiguous_key_lists() {
        let mut wrong_algorithm = key("current", 1);
        wrong_algorithm["algorithm"] = json!("rsa");
        let mut short_key = key("current", 1);
        short_key["public_key"] = json!(STANDARD.encode([1; ED25519_KEY_SIZE - 1]));
        for entries in [
            json!([]),
            json!(null),
            json!({}),
            json!([{}]),
            json!([key("", 1)]),
            json!([wrong_algorithm]),
            json!([short_key]),
            json!([key("current", 1), key("current", 2)]),
            json!([key("current", 1), key("alias", 1)]),
        ] {
            assert!(render_keys(&entries.to_string()).is_err(), "{entries}");
        }
    }
}
