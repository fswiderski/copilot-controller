use std::{fmt, marker::PhantomData};

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor, value::MapAccessDeserializer},
};

pub(super) fn object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct ObjectVisitor<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for ObjectVisitor<T> {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object")
        }

        fn visit_map<M: MapAccess<'de>>(self, map: M) -> Result<T, M::Error> {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(ObjectVisitor(PhantomData))
}

macro_rules! strict_record {
    (
        $(#[$record_meta:meta])*
        $visibility:vis struct $name:ident as $raw:ident {
            $(
                $(#[$field_meta:meta])*
                $field_visibility:vis $field:ident: $field_type:ty
            ),* $(,)?
        }
    ) => {
        $(#[$record_meta])*
        #[serde(deny_unknown_fields)]
        $visibility struct $name {
            $($(#[$field_meta])* $field_visibility $field: $field_type),*
        }

        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct $raw {
            $($(#[$field_meta])* $field: $field_type),*
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let $raw { $($field),* } = super::strict::object(deserializer)?;
                Ok(Self { $($field),* })
            }
        }
    };
}

pub(super) use strict_record;
