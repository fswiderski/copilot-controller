use serde::{Deserialize, Deserializer, Serializer, de};
use uuid::Uuid;

pub fn serialize<S: Serializer>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(&value.hyphenated())
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Uuid, D::Error> {
    let text = String::deserialize(deserializer)?;
    Uuid::parse_str(&text).map_err(de::Error::custom)
}
