// Copyright 2019-2022 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use std::str::FromStr;

/// Network defines the preconfigured networks to use with address encoding
#[derive(PartialEq, Eq, Copy, Clone, Debug, Hash)]
pub enum Network {
    Mainnet,
    Testnet,
}

impl From<Network> for fvm_shared::address::Network {
    fn from(network: Network) -> Self {
        match network {
            Network::Mainnet => fvm_shared::address::Network::Mainnet,
            Network::Testnet => fvm_shared::address::Network::Testnet,
        }
    }
}

impl FromStr for Network {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mainnet" => Ok(Network::Mainnet),
            "testnet" => Ok(Network::Testnet),
            "calibnet" => Ok(Network::Testnet),
            _ => Err(()),
        }
    }
}

impl Default for Network {
    fn default() -> Self {
        Network::Mainnet
    }
}
