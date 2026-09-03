use std::fmt;
use std::marker::PhantomData;
use std::str::FromStr;

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::Deserialize;
use vrsc_rpc::json::vrsc::Address;

use crate::coinstaker::StakerStatus;

/// Shared identity filter. Accepts a single `identity_address=`, repeated keys, and
/// `identity_addresses=`. An empty list means "all".
#[derive(Deserialize, Debug, Default)]
pub struct IdentityQuery {
    #[serde(default, deserialize_with = "one_or_many_addresses")]
    pub identity_address: Vec<Address>,
    #[serde(default, deserialize_with = "one_or_many_addresses")]
    pub identity_addresses: Vec<Address>,
}

impl IdentityQuery {
    pub fn addresses(self) -> Vec<Address> {
        if !self.identity_address.is_empty() {
            self.identity_address
        } else {
            self.identity_addresses
        }
    }
}

fn one_or_many_addresses<'de, D>(deserializer: D) -> Result<Vec<Address>, D::Error>
where
    D: Deserializer<'de>,
{
    struct OneOrMany(PhantomData<Address>);

    impl<'de> Visitor<'de> for OneOrMany {
        type Value = Vec<Address>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("an address or a sequence of addresses")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            Address::from_str(v)
                .map(|addr| vec![addr])
                .map_err(E::custom)
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
            self.visit_str(&v)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut out = Vec::new();
            while let Some(item) = seq.next_element::<String>()? {
                out.push(Address::from_str(&item).map_err(de::Error::custom)?);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_any(OneOrMany(PhantomData))
}

#[derive(Deserialize, Debug, Default)]
pub struct GetStakerArgs {
    #[serde(flatten)]
    pub identities: IdentityQuery,
    pub staker_status: Option<StakerStatus>,
}

#[derive(Deserialize, Debug, Default)]
pub struct GetPayoutsArgs {
    #[serde(flatten)]
    pub identities: IdentityQuery,
    pub limit: Option<u32>,
    pub before_height: Option<u64>,
}

impl GetPayoutsArgs {
    pub fn page(&self) -> (Option<u32>, Option<u64>) {
        (self.limit.map(|n| n.clamp(1, 200)), self.before_height)
    }
}
