// Copyright 2020 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use libp2p::Multiaddr;
use serde::Deserialize;

const DEFAULT_BOOTSTRAP: &[&str] = &[
    "/dns4/bootstrap-0.testnet.fildev.network/tcp/1347/p2p/12D3KooWJTUBUjtzWJGWU1XSiY21CwmHaCNLNYn2E7jqHEHyZaP7",
    "/dns4/bootstrap-1.testnet.fildev.network/tcp/1347/p2p/12D3KooW9yeKXha4hdrJKq74zEo99T8DhriQdWNoojWnnQbsgB3v",
    "/dns4/bootstrap-2.testnet.fildev.network/tcp/1347/p2p/12D3KooWCrx8yVG9U9Kf7w8KLN3Edkj5ZKDhgCaeMqQbcQUoB6CT",
    "/dns4/bootstrap-4.testnet.fildev.network/tcp/1347/p2p/12D3KooWPkL9LrKRQgHtq7kn9ecNhGU9QaziG8R5tX8v9v7t3h34",
    "/dns4/bootstrap-3.testnet.fildev.network/tcp/1347/p2p/12D3KooWKYSsbpgZ3HAjax5M1BXCwXLa6gVkUARciz7uN3FNtr7T",
    "/dns4/bootstrap-5.testnet.fildev.network/tcp/1347/p2p/12D3KooWQYzqnLASJAabyMpPb1GcWZvNSe7JDcRuhdRqonFoiK9W",
];

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Libp2pConfig {
    pub listening_multiaddr: Multiaddr,
    pub bootstrap_peers: Vec<Multiaddr>,
    pub mdns: bool,
    pub kademlia: bool,
}

impl Default for Libp2pConfig {
    fn default() -> Self {
        let bootstrap_peers = DEFAULT_BOOTSTRAP
            .iter()
            .map(|node| node.parse().unwrap())
            .collect();
        Self {
            listening_multiaddr: "/ip4/0.0.0.0/tcp/0".parse().unwrap(),
            bootstrap_peers,
            mdns: true,
            kademlia: true,
        }
    }
}
