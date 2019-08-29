use super::*;
use ethereum_types::{U128, U256};
use std::fs;
use std::path::Path;
use types::Fork;

pub fn yaml_decode<T: serde::de::DeserializeOwned>(string: &str) -> Result<T, Error> {
    serde_yaml::from_str(string).map_err(|e| Error::FailedToParseTest(format!("{:?}", e)))
}

pub fn yaml_decode_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Error> {
    fs::read_to_string(path)
        .map_err(|e| {
            Error::FailedToParseTest(format!("Unable to load {}: {:?}", path.display(), e))
        })
        .and_then(|s| yaml_decode(&s))
}

pub fn ssz_decode_file<T: ssz::Decode>(path: &Path) -> Result<T, Error> {
    fs::read(path)
        .map_err(|e| {
            Error::FailedToParseTest(format!("Unable to load {}: {:?}", path.display(), e))
        })
        .and_then(|s| {
            T::from_ssz_bytes(&s).map_err(|e| {
                Error::FailedToParseTest(format!(
                    "Unable to parse SSZ at {}: {:?}",
                    path.display(),
                    e
                ))
            })
        })
}

pub trait YamlDecode: Sized {
    /// Decode an object from the test specification YAML.
    fn yaml_decode(string: &str) -> Result<Self, Error>;

    fn yaml_decode_file(path: &Path) -> Result<Self, Error> {
        fs::read_to_string(path)
            .map_err(|e| {
                Error::FailedToParseTest(format!("Unable to load {}: {:?}", path.display(), e))
            })
            .and_then(|s| Self::yaml_decode(&s))
    }
}

/// Basic types can general be decoded with the `parse` fn if they implement `str::FromStr`.
macro_rules! impl_via_parse {
    ($ty: ty) => {
        impl YamlDecode for $ty {
            fn yaml_decode(string: &str) -> Result<Self, Error> {
                string
                    .parse::<Self>()
                    .map_err(|e| Error::FailedToParseTest(format!("{:?}", e)))
            }
        }
    };
}

impl_via_parse!(u8);
impl_via_parse!(u16);
impl_via_parse!(u32);
impl_via_parse!(u64);

/// Some `ethereum-types` methods have a `str::FromStr` implementation that expects `0x`-prefixed:
/// hex, so we use `from_dec_str` instead.
macro_rules! impl_via_from_dec_str {
    ($ty: ty) => {
        impl YamlDecode for $ty {
            fn yaml_decode(string: &str) -> Result<Self, Error> {
                Self::from_dec_str(string).map_err(|e| Error::FailedToParseTest(format!("{:?}", e)))
            }
        }
    };
}

impl_via_from_dec_str!(U128);
impl_via_from_dec_str!(U256);

/// Types that already implement `serde::Deserialize` can be decoded using `serde_yaml`.
macro_rules! impl_via_serde_yaml {
    ($ty: ty) => {
        impl YamlDecode for $ty {
            fn yaml_decode(string: &str) -> Result<Self, Error> {
                serde_yaml::from_str(string)
                    .map_err(|e| Error::FailedToParseTest(format!("{:?}", e)))
            }
        }
    };
}

impl_via_serde_yaml!(Fork);
