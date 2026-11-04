pub mod config;
pub mod fs;
pub mod keyboard;
pub mod platform;
pub mod protos;

pub use protos::message as message_proto;

pub fn validate_relay_uuid(value: &str) -> Result<(), &'static str> {
    use hbb_common::uuid::{Uuid, Variant, Version};

    let uuid = Uuid::parse_str(value).map_err(|_| "Invalid relay UUID")?;
    if uuid.get_version() != Some(Version::Random)
        || uuid.get_variant() != Variant::RFC4122
        || uuid.hyphenated().to_string() != value
    {
        return Err("Invalid relay UUID");
    }
    Ok(())
}
