// Copyright 2020 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use crypto::VRFProof;
use encoding::{serde_bytes, tuple::*};
use fil_types::PoStProof;

/// A Ticket is a marker of a tick of the blockchain's clock.  It is the source
/// of randomness for proofs of storage and leader election.  It is generated
/// by the miner of a block using a VRF and a VDF.
#[derive(
    Clone, Debug, PartialEq, PartialOrd, Eq, Default, Ord, Serialize_tuple, Deserialize_tuple,
)]
pub struct Ticket {
    /// A proof output by running a VRF on the VDFResult of the parent ticket
    pub vrfproof: VRFProof,
}

impl Ticket {
    /// Ticket constructor
    pub fn new(vrfproof: VRFProof) -> Self {
        Self { vrfproof }
    }
}

/// PoSt election candidates
#[derive(Clone, Debug, PartialEq, Default, Eq, Serialize_tuple, Deserialize_tuple)]
pub struct EPostTicket {
    #[serde(with = "serde_bytes")]
    pub partial: Vec<u8>,
    pub sector_id: u64,
    pub challenge_index: u64,
}

/// Proof of Spacetime election proof
#[derive(Clone, Debug, PartialEq, Default, Eq, Serialize_tuple, Deserialize_tuple)]
pub struct EPostProof {
    pub proof: Vec<PoStProof>,
    #[serde(with = "serde_bytes")]
    pub post_rand: Vec<u8>,
    pub candidates: Vec<EPostTicket>,
}
