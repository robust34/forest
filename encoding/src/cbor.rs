// Copyright 2020 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0

use super::errors::Error;

pub trait Cbor {
    fn unmarshal_cbor(bz: &[u8]) -> Result<Self, Error>
    where
        Self: Sized;
    fn marshal_cbor(&self) -> Result<Vec<u8>, Error>;
}
