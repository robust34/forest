// Copyright 2019-2023 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use super::*;

#[derive(Serialize, Deserialize)]
pub struct VecU8LotusJson(#[serde(with = "base64_standard")] Vec<u8>);

impl HasLotusJson for Vec<u8> {
    type LotusJson = VecU8LotusJson;

    fn snapshots() -> Vec<(serde_json::Value, Self)> {
        vec![(json!("aGVsbG8gd29ybGQh"), Vec::from_iter(*b"hello world!"))]
    }

    fn into_lotus_json(self) -> Self::LotusJson {
        VecU8LotusJson(self)
    }

    fn from_lotus_json(VecU8LotusJson(vec): Self::LotusJson) -> Self {
        vec
    }
}
