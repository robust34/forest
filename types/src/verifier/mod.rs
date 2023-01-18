// Copyright 2019-2023 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use filecoin_proofs_api::{post, PublicReplicaInfo};
use filecoin_proofs_api::{ProverId, SectorId};
use fvm_ipld_encoding::bytes_32;
use fvm_shared::address::Address;
use fvm_shared::commcid::cid_to_replica_commitment_v1;
use fvm_shared::randomness::Randomness;
use fvm_shared::sector::{PoStProof, RegisteredPoStProof, SectorInfo};
use std::collections::BTreeMap;
use std::convert::TryInto;

/// Functionality for verification of seal, winning PoSt and window PoSt proofs.
/// Proof verification will be full validation by default.

/// Verifies winning proof of spacetime. These proofs are generated by the miners that are
/// elected to mine a new block to verify a sector. A failed winning proof leads to a miner
/// being slashed.
pub fn verify_winning_post(
    Randomness(mut randomness): Randomness,
    proofs: &[PoStProof],
    challenge_sectors: &[SectorInfo],
    prover: u64,
) -> Result<(), anyhow::Error> {
    // Necessary to be valid bls12 381 element.
    randomness[31] &= 0x3f;

    // Convert sector info into public replica
    let replicas = to_fil_public_replica_infos(challenge_sectors, ProofType::Winning)
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    // Convert PoSt proofs into proofs-api format
    let proof_bytes = proofs.iter().fold(Vec::new(), |mut proof, p| {
        proof.extend_from_slice(&p.proof_bytes);
        proof
    });

    // Generate prover bytes from ID
    let prover_id = prover_id_from_u64(prover);

    // Verify Proof
    if !post::verify_winning_post(&bytes_32(&randomness), &proof_bytes, &replicas, prover_id)? {
        anyhow::bail!("Winning post was invalid")
    }
    Ok(())
}

/// Generates sector challenge indexes for use in winning PoSt verification.
pub fn generate_winning_post_sector_challenge(
    proof: RegisteredPoStProof,
    prover_id: u64,
    Randomness(mut randomness): Randomness,
    eligible_sector_count: u64,
) -> Result<Vec<u64>, anyhow::Error> {
    // Necessary to be valid bls12 381 element.
    randomness[31] &= 0x3f;

    post::generate_winning_post_sector_challenge(
        proof.try_into().map_err(|e| anyhow::anyhow!("{}", e))?,
        &bytes_32(&randomness),
        eligible_sector_count,
        prover_id_from_u64(prover_id),
    )
}

/// PoSt proof variants.
enum ProofType {
    Winning,
    // Window,
}

fn prover_id_from_u64(id: u64) -> ProverId {
    let mut prover_id = ProverId::default();
    let prover_bytes = Address::new_id(id).payload().to_raw_bytes();
    prover_id[..prover_bytes.len()].copy_from_slice(&prover_bytes);
    prover_id
}

fn to_fil_public_replica_infos(
    src: &[SectorInfo],
    typ: ProofType,
) -> Result<BTreeMap<SectorId, PublicReplicaInfo>, String> {
    let replicas = src
        .iter()
        .map::<Result<(SectorId, PublicReplicaInfo), String>, _>(|sector_info: &SectorInfo| {
            let commr = cid_to_replica_commitment_v1(&sector_info.sealed_cid)?;
            let proof = match typ {
                ProofType::Winning => sector_info.proof.registered_winning_post_proof()?,
                // ProofType::Window => sector_info.proof.registered_window_post_proof()?,
            };
            let replica = PublicReplicaInfo::new(proof.try_into()?, commr);
            Ok((SectorId::from(sector_info.sector_number), replica))
        })
        .collect::<Result<BTreeMap<SectorId, PublicReplicaInfo>, _>>()?;
    Ok(replicas)
}
