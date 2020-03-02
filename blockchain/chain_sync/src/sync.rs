// Copyright 2020 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use super::{Error, SyncManager, SyncNetworkContext};
use address::Address;
use amt::AMT;
use async_std::prelude::*;
use async_std::stream::Stream;
use async_std::sync::{Receiver, Sender};
use blocks::{Block, BlockHeader, FullTipset, TipSetKeys, Tipset, TxMeta};
use chain::ChainStore;
use cid::Cid;
use crypto::is_valid_signature;
use db::Error as DBError;
use encoding::{Cbor, Error as EncodingError};
use forest_libp2p::{NetworkEvent, NetworkMessage};
use futures::{select, FutureExt};
use ipld_blockstore::BlockStore;
use libp2p::core::PeerId;
use log::{info, warn};
use lru::LruCache;
use message::Message;
use num_bigint::BigUint;
use pin_project::pin_project;
use state_manager::StateManager;
use state_tree::{HamtStateTree, StateTree};
use std::cmp::min;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::{
    pin::Pin,
    task::{Context, Poll},
};

#[derive(PartialEq, Debug, Clone)]
/// Current state of the ChainSyncer
enum SyncState {
    /// No useful peers, bootstrapping network to be able to make BlockSync requests
    Stalled,

    /// Syncing to checkpoint (using BlockSync for now)
    _SyncCheckpoint,

    /// Receive new blocks from the network and sync toward heaviest tipset
    _ChainCatchup,

    /// Once all blocks are validated to the heaviest chain, follow network
    /// by receiving blocks over the network and validating them
    _Follow,
}

#[pin_project]
pub struct ChainSyncer<'db, DB, ST> {
    /// Syncing state of chain sync
    _state: SyncState,

    /// manages retrieving and updates state objects
    state_manager: StateManager<'db, DB, ST>,

    /// manages sync buckets
    sync_manager: SyncManager,

    /// access and store tipsets / blocks / messages
    chain_store: ChainStore<'db, DB>,

    /// Context to be able to send requests to p2p network
    network: SyncNetworkContext,

    /// the known genesis tipset
    _genesis: Tipset,

    /// Bad blocks cache, updates based on invalid state transitions.
    /// Will mark any invalid blocks and all childen as bad in this bounded cache
    bad_blocks: LruCache<Cid, String>,

    /// Channel for incoming network events to be handled by syncer
    #[pin]
    network_rx: Receiver<NetworkEvent>,
}

// TODO probably remove this in the future, polling as such probably unnecessary
impl<'db, DB, ST> Future for ChainSyncer<'db, DB, ST> {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.project().network_rx.poll_next(cx) {
            Poll::Ready(Some(_event)) => (),
            Poll::Pending | Poll::Ready(None) => (),
        };
        Poll::Pending
    }
}

/// Message data used to ensure valid state transition
struct MsgMetaData {
    balance: BigUint,
    sequence: u64,
}

impl<'db, DB> ChainSyncer<'db, DB, HamtStateTree>
where
    DB: BlockStore,
{
    pub fn new(
        db: &'db DB,
        network_send: Sender<NetworkMessage>,
        network_rx: Receiver<NetworkEvent>,
    ) -> Result<Self, Error> {
        let sync_manager = SyncManager::default();

        let chain_store = ChainStore::new(db);
        let _genesis = match chain_store.genesis()? {
            Some(gen) => Tipset::new(vec![gen])?,
            None => {
                // TODO change default logic for genesis or setup better initialization
                warn!("no genesis found in data storage, using a default");
                Tipset::new(vec![BlockHeader::default()])?
            }
        };

        let state_manager = StateManager::new(db, HamtStateTree::default());

        let network = SyncNetworkContext::new(network_send);

        Ok(Self {
            _state: SyncState::Stalled,
            state_manager,
            chain_store,
            network,
            _genesis,
            sync_manager,
            network_rx,
            bad_blocks: LruCache::new(1 << 15),
        })
    }
}

impl<'db, DB, ST> ChainSyncer<'db, DB, ST>
where
    DB: BlockStore,
    ST: StateTree,
{
    /// Starts syncing process
    pub async fn sync(&mut self) -> Result<(), Error> {
        let mut nw = self.network_rx.clone().fuse();
        loop {
            select! {
                network_msg = nw.next().fuse() => match network_msg {
                    Some(event) =>(),
                    None => break,
                }
            }
        }

        info!("Starting chain sync");

        // Get heaviest tipset from storage to sync toward
        let heaviest = self.chain_store.heaviest_tipset();

        // TODO remove this and retrieve head from storage
        let head = Tipset::new(vec![BlockHeader::default()]).unwrap();

        // Sync headers from network from head to heaviest from storage
        let headers = self.sync_headers_reverse(head, &heaviest).await?;

        // Persist header chain pulled from network
        self.persist_headers(&headers)?;

        Ok(())
    }

    /// informs the syncer about a new potential tipset
    /// This should be called when connecting to new peers, and additionally
    /// when receiving new blocks from the network
    pub fn inform_new_head(&self, from: &PeerId, fts: &FullTipset) -> Result<(), Error> {
        // check if full block is nil and if so return error
        if fts.blocks().is_empty() {
            return Err(Error::NoBlocks);
        }
        // validate message data
        for block in fts.blocks() {
            self.validate_msg_data(block)?;
        }

        // compare target_weight to heaviest weight stored; ignore otherwise
        let heaviest_tipset = self.chain_store.heaviest_tipset();
        let best_weight = heaviest_tipset.blocks()[0].weight();
        let target_weight = fts.blocks()[0].header().weight();

        if !target_weight.lt(&best_weight) {
            // Store incoming block header
            self.chain_store.persist_headers(&fts.tipset()?)?;
            // Set peer head
            self.sync_manager.set_peer_head(from, fts.tipset()?);
        }
        // incoming tipset from miners does not appear to be better than our best chain, ignoring for now
        Ok(())
    }
    /// Validates message root from header matches message root generated from the
    /// bls and secp messages contained in the passed in block and stores them in a key-value store
    fn validate_msg_data(&self, block: &Block) -> Result<(), Error> {
        let sm_root = self.compute_msg_data(block)?;
        if block.header().messages() != &sm_root {
            return Err(Error::InvalidRoots);
        }

        self.chain_store.put_messages(block.bls_msgs())?;
        self.chain_store.put_messages(block.secp_msgs())?;

        Ok(())
    }
    /// Returns message root CID from bls and secp message contained in the param Block
    fn compute_msg_data(&self, block: &Block) -> Result<Cid, Error> {
        // collect bls and secp cids
        let bls_cids = cids_from_messages(block.bls_msgs())?;
        let secp_cids = cids_from_messages(block.secp_msgs())?;
        // generate AMT and batch set message values
        let bls_root = AMT::new_from_slice(self.chain_store.blockstore(), &bls_cids)?;
        let secp_root = AMT::new_from_slice(self.chain_store.blockstore(), &secp_cids)?;

        let meta = TxMeta {
            bls_message_root: bls_root,
            secp_message_root: secp_root,
        };
        // store message roots and receive meta_root
        let meta_root = self.chain_store.blockstore().put(&meta)?;

        Ok(meta_root)
    }
    /// Returns FullTipset from store if TipSetKeys exist in key-value store otherwise requests FullTipset
    /// from block sync
    pub fn fetch_tipset(&self, _peer_id: PeerId, tsk: &TipSetKeys) -> Result<FullTipset, Error> {
        let fts = match self.load_fts(tsk) {
            Ok(fts) => fts,
            // TODO call into block sync to request FullTipset -> self.blocksync.get_full_tipset(_peer_id, tsk)
            Err(e) => return Err(e), // blocksync
        };
        Ok(fts)
    }
    /// Returns a reconstructed FullTipset from store if keys exist
    fn load_fts(&self, keys: &TipSetKeys) -> Result<FullTipset, Error> {
        let mut blocks = Vec::new();
        // retrieve tipset from store based on passed in TipSetKeys
        let ts = self.chain_store.tipset_from_keys(keys)?;
        for header in ts.blocks() {
            // retrieve bls and secp messages from specified BlockHeader
            let (bls_msgs, secp_msgs) = self.chain_store.messages(&header)?;
            // construct a full block
            let full_block = Block {
                header: header.clone(),
                bls_messages: bls_msgs,
                secp_messages: secp_msgs,
            };
            // push vector of full blocks to build FullTipset
            blocks.push(full_block);
        }
        // construct FullTipset
        let fts = FullTipset::new(blocks);
        Ok(fts)
    }
    // Block message validation checks
    pub fn check_blk_msgs(&self, block: Block, _tip: Tipset) -> Result<(), Error> {
        // TODO retrieve bls public keys for verify_bls_aggregate
        // for _m in block.bls_msgs() {
        // }
        // TODO verify_bls_aggregate

        // check msgs for validity
        fn check_msg<M, ST>(
            msg: &M,
            msg_meta_data: &mut HashMap<Address, MsgMetaData>,
            tree: &ST,
        ) -> Result<(), Error>
        where
            M: Message,
            ST: StateTree,
        {
            let updated_state: MsgMetaData = match msg_meta_data.get(msg.from()) {
                // address is present begin validity checks
                Some(MsgMetaData { sequence, balance }) => {
                    // sequence equality check
                    if *sequence != msg.sequence() {
                        return Err(Error::Validation("Sequences are not equal".to_string()));
                    }

                    // sufficient funds check
                    if *balance < msg.required_funds() {
                        return Err(Error::Validation(
                            "Insufficient funds for message execution".to_string(),
                        ));
                    }
                    // update balance and increment sequence by 1
                    MsgMetaData {
                        balance: balance - msg.required_funds(),
                        sequence: sequence + 1,
                    }
                }
                // MsgMetaData not found with provided address key, insert sequence and balance with address as key
                None => {
                    let actor = tree.get_actor(msg.from()).ok_or_else(|| {
                        Error::State("Could not retrieve actor from state tree".to_owned())
                    })?;

                    MsgMetaData {
                        sequence: actor.sequence,
                        balance: actor.balance,
                    }
                }
            };
            // update hash map with updated state
            msg_meta_data.insert(msg.from().clone(), updated_state);
            Ok(())
        }
        let mut msg_meta_data: HashMap<Address, MsgMetaData> = HashMap::default();
        // TODO retrieve tipset state and load state tree
        // temporary
        let tree = HamtStateTree::default();
        // loop through bls messages and check msg validity
        for m in block.bls_msgs() {
            check_msg(m, &mut msg_meta_data, &tree)?;
        }
        // loop through secp messages and check msg validity and signature
        for m in block.secp_msgs() {
            check_msg(m, &mut msg_meta_data, &tree)?;
            // signature validation
            if !is_valid_signature(&m.cid()?.to_bytes(), m.from(), m.signature()) {
                return Err(Error::Validation(
                    "Message signature is not valid".to_string(),
                ));
            }
        }
        // validate message root from header matches message root
        let sm_root = self.compute_msg_data(&block)?;
        if block.header().messages() != &sm_root {
            return Err(Error::InvalidRoots);
        }

        Ok(())
    }

    /// Validates block semantically according to https://github.com/filecoin-project/specs/blob/6ab401c0b92efb6420c6e198ec387cf56dc86057/validation.md
    pub fn validate(&self, block: Block) -> Result<(), Error> {
        // get header from full block
        let header = block.header();

        // check if block has been signed
        if header.signature().bytes().is_empty() {
            return Err(Error::Validation("Signature is nil in header".to_string()));
        }

        let base_tipset = self.load_fts(&TipSetKeys::new(header.parents().cids.clone()))?;
        // time stamp checks
        header.validate_timestamps(&base_tipset)?;

        // check messages to ensure valid state transitions
        self.check_blk_msgs(block.clone(), base_tipset.tipset()?)?;

        // block signature check
        // TODO need to pass in raw miner address; temp using header miner address
        // see https://github.com/filecoin-project/lotus/blob/master/chain/sync.go#L611
        header.check_block_signature(header.miner_address())?;

        // TODO: incomplete, still need to retrieve power in order to ensure ticket is the winner
        let _slash = self.state_manager.miner_slashed(header.miner_address())?;
        let _sector_size = self
            .state_manager
            .miner_sector_size(header.miner_address())?;

        // TODO winner_check
        // TODO miner_check
        // TODO verify_ticket_vrf
        // TODO verify_election_proof_check

        Ok(())
    }

    /// Syncs chain data and persists it to blockstore
    async fn sync_headers_reverse(
        &mut self,
        head: Tipset,
        to: &Tipset,
    ) -> Result<Vec<Tipset>, Error> {
        info!("Syncing headers from: {:?}", head.key());

        let mut accepted_blocks: Vec<Cid> = Vec::new();

        let mut return_set = vec![head];

        let to_epoch = to
            .blocks()
            .get(0)
            .ok_or_else(|| Error::Blockchain("Tipset must not be empty".to_owned()))?
            .epoch();

        // Loop until most recent tipset height is less than to tipset height
        'sync: while let Some(cur_ts) = return_set.last() {
            // Check if parent cids exist in bad block cache
            self.validate_tipset_against_cache(cur_ts.parents(), &accepted_blocks)?;

            if cur_ts.epoch() < to_epoch {
                // Current tipset is less than epoch of tipset syncing toward
                break;
            }

            // Try to load parent tipset from local storage
            if let Ok(ts) = self.chain_store.tipset_from_keys(cur_ts.parents()) {
                // Add blocks in tipset to accepted chain and push the tipset to return set
                accepted_blocks.extend_from_slice(ts.cids());
                return_set.push(ts);
                continue;
            }

            const REQUEST_WINDOW: u64 = 100;
            let epoch_diff = u64::from(cur_ts.epoch() - to_epoch);
            let _window = min(epoch_diff, REQUEST_WINDOW);

            // // Load blocks from network using blocksync
            // TODO add sending blocksync req back (requires some channel for data back)
            // let tipsets: Vec<Tipset> = self
            //     .network
            //     .get_headers(ts.parents(), window)
            //     .await
            //     .map_err(|e| Error::Other(e))?;
            let tipsets: Vec<Tipset> = vec![];

            // Loop through each tipset received from network
            for ts in tipsets {
                if ts.epoch() < to_epoch {
                    // Break out of sync loop if epoch lower than to tipset
                    // This should not be hit if response from server is correct
                    break 'sync;
                }
                // Check Cids of blocks against bad block cache
                self.validate_tipset_against_cache(&ts.key(), &accepted_blocks)?;

                accepted_blocks.extend_from_slice(ts.cids());
                // Add tipset to vector of tipsets to return
                return_set.push(ts);
            }
        }

        let last_ts = return_set
            .last()
            .ok_or_else(|| Error::Other("Return set should contain a tipset".to_owned()))?;

        // Check if local chain was fork
        if last_ts.key() != to.key() {
            if last_ts.parents() == to.parents() {
                // block received part of same tipset as best block
                // This removes need to sync fork
                return Ok(return_set);
            }
            // TODO add fork to return set
            let _fork = self.sync_fork(&last_ts, &to).await?;
        }

        Ok(return_set)
    }

    fn validate_tipset_against_cache(
        &mut self,
        ts: &TipSetKeys,
        accepted_blocks: &[Cid],
    ) -> Result<(), Error> {
        for cid in ts.cids() {
            if let Some(reason) = self.bad_blocks.get(cid).cloned() {
                for bh in accepted_blocks {
                    self.bad_blocks
                        .put(bh.clone(), format!("chain contained {}", cid));
                }

                return Err(Error::Other(format!(
                    "Chain contained block marked as bad: {}, {}",
                    cid, reason
                )));
            }
        }
        Ok(())
    }

    async fn sync_fork(&mut self, _head: &Tipset, _to: &Tipset) -> Result<Vec<Arc<Tipset>>, Error> {
        // TODO sync fork until tipsets are equal or reaches genesis
        todo!()
    }

    // Persists headers from tipset slice to chain store
    fn persist_headers(&self, tipsets: &[Tipset]) -> Result<(), DBError> {
        tipsets
            .iter()
            .try_for_each(|ts| self.chain_store.persist_headers(ts))
    }
}

fn cids_from_messages<T: Cbor>(messages: &[T]) -> Result<Vec<Cid>, EncodingError> {
    messages.iter().map(Cbor::cid).collect()
}
