// Copyright 2019-2023 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use std::sync::Arc;

use crate::blocks::Tipset;
use crate::chain::store::ChainStore;
use crate::message::ChainMessage;
use crate::networks::{ChainConfig, NetworkChain};
use crate::shim::{
    address::Address,
    econ::TokenAmount,
    executor::{ApplyRet, Receipt},
    externs::{Rand, RandWrapper},
    machine::MultiEngine,
    message::{Message, Message_v3},
    state_tree::ActorState,
    version::NetworkVersion,
};
use ahash::HashSet;
use anyhow::bail;
use cid::Cid;
use fil_actor_interface::{cron, reward, AwardBlockRewardParams};
use fvm2::{
    executor::{DefaultExecutor as DefaultExecutor_v2, Executor as Executor_v2},
    machine::{
        DefaultMachine as DefaultMachine_v2, Machine as Machine_v2,
        NetworkConfig as NetworkConfig_v2,
    },
};
use fvm3::{
    executor::{DefaultExecutor as DefaultExecutor_v3, Executor as Executor_v3},
    machine::{
        DefaultMachine as DefaultMachine_v3, Machine as Machine_v3,
        NetworkConfig as NetworkConfig_v3,
    },
};
use fvm_ipld_blockstore::Blockstore;
use fvm_ipld_encoding::{to_vec, RawBytes};
use fvm_shared2::{clock::ChainEpoch, BLOCK_GAS_LIMIT};
use num::Zero;

use crate::interpreter::{fvm2::ForestExternsV2, fvm3::ForestExterns as ForestExternsV3};

pub(in crate::interpreter) type ForestMachineV2<DB> =
    DefaultMachine_v2<Arc<DB>, ForestExternsV2<DB>>;
pub(in crate::interpreter) type ForestMachineV3<DB> =
    DefaultMachine_v3<Arc<DB>, ForestExternsV3<DB>>;

type ForestKernelV2<DB> =
    fvm2::DefaultKernel<fvm2::call_manager::DefaultCallManager<ForestMachineV2<DB>>>;
type ForestKernelV3<DB> =
    fvm3::DefaultKernel<fvm3::call_manager::DefaultCallManager<ForestMachineV3<DB>>>;
type ForestExecutorV2<DB> = DefaultExecutor_v2<ForestKernelV2<DB>>;
type ForestExecutorV3<DB> = DefaultExecutor_v3<ForestKernelV3<DB>>;

/// Contains all messages to process through the VM as well as miner information
/// for block rewards.
#[derive(Debug)]
pub struct BlockMessages {
    pub miner: Address,
    pub messages: Vec<ChainMessage>,
    pub win_count: i64,
}

/// Interpreter which handles execution of state transitioning messages and
/// returns receipts from the VM execution.
pub enum VM<DB: Blockstore + Send + Sync + 'static> {
    VM2(ForestExecutorV2<DB>),
    VM3(ForestExecutorV3<DB>),
}

pub struct ExecutionContext<DB> {
    // This tipset identifies of the blockchain. It functions as a starting
    // point when searching for ancestors. It may be any tipset as long as its
    // epoch is at or higher than the epoch in `epoch`.
    pub heaviest_tipset: Arc<Tipset>,
    // State-tree generated by the parent tipset.
    pub state_tree_root: Cid,
    // Epoch of the messages to be executed.
    pub epoch: ChainEpoch,
    // Source of deterministic randomness
    pub rand: Box<dyn Rand>,
    // https://spec.filecoin.io/systems/filecoin_vm/gas_fee/
    pub base_fee: TokenAmount,
    // https://filecoin.io/blog/filecoin-circulating-supply/
    pub circ_supply: TokenAmount,
    // The chain config is used to determine which consensus rules to use.
    pub chain_config: Arc<ChainConfig>,
    // Caching interface to the DB
    pub chain_store: Arc<ChainStore<DB>>,
    // UNIX timestamp for epoch
    pub timestamp: u64,
}

impl<DB> VM<DB>
where
    DB: Blockstore + Send + Sync,
{
    pub fn new(
        ExecutionContext {
            heaviest_tipset,
            state_tree_root,
            epoch,
            rand,
            base_fee,
            circ_supply,
            chain_config,
            chain_store,
            timestamp,
        }: ExecutionContext<DB>,
        multi_engine: &MultiEngine,
    ) -> Result<Self, anyhow::Error> {
        let network_version = chain_config.network_version(epoch);
        if network_version >= NetworkVersion::V18 {
            let mut config = NetworkConfig_v3::new(network_version.into());
            // ChainId defines the chain ID used in the Ethereum JSON-RPC endpoint.
            config.chain_id(chain_config.eth_chain_id.into());
            if let NetworkChain::Devnet(_) = chain_config.network {
                config.enable_actor_debugging();
            }

            let engine = multi_engine.v3.get(&config)?;
            let mut context = config.for_epoch(epoch, timestamp, state_tree_root);
            context.set_base_fee(base_fee.into());
            context.set_circulating_supply(circ_supply.into());
            let fvm: ForestMachineV3<DB> = ForestMachineV3::new(
                &context,
                Arc::clone(&chain_store.db),
                ForestExternsV3::new(
                    RandWrapper::from(rand),
                    heaviest_tipset,
                    epoch,
                    state_tree_root,
                    chain_store,
                    chain_config,
                ),
            )?;
            let exec: ForestExecutorV3<DB> = DefaultExecutor_v3::new(engine, fvm)?;
            Ok(VM::VM3(exec))
        } else {
            let config = NetworkConfig_v2::new(network_version.into());
            let engine = multi_engine.v2.get(&config)?;
            let mut context = config.for_epoch(epoch, state_tree_root);
            context.set_base_fee(base_fee.into());
            context.set_circulating_supply(circ_supply.into());
            let fvm: ForestMachineV2<DB> = ForestMachineV2::new(
                &engine,
                &context,
                Arc::clone(&chain_store.db),
                ForestExternsV2::new(
                    RandWrapper::from(rand),
                    heaviest_tipset,
                    epoch,
                    state_tree_root,
                    chain_store,
                    chain_config,
                ),
            )?;
            let exec: ForestExecutorV2<DB> = DefaultExecutor_v2::new(fvm);
            Ok(VM::VM2(exec))
        }
    }

    /// Flush stores in VM and return state root.
    pub fn flush(&mut self) -> anyhow::Result<Cid> {
        match self {
            VM::VM2(fvm_executor) => Ok(fvm_executor.flush()?),
            VM::VM3(fvm_executor) => Ok(fvm_executor.flush()?),
        }
    }

    /// Get actor state from an address. Will be resolved to ID address.
    pub fn get_actor(&self, addr: &Address) -> Result<Option<ActorState>, anyhow::Error> {
        match self {
            VM::VM2(fvm_executor) => Ok(fvm_executor
                .state_tree()
                .get_actor(&addr.into())?
                .map(ActorState::from)),
            VM::VM3(fvm_executor) => {
                if let Some(id) = fvm_executor.state_tree().lookup_id(&addr.into())? {
                    Ok(fvm_executor
                        .state_tree()
                        .get_actor(id)?
                        .map(ActorState::from))
                } else {
                    Ok(None)
                }
            }
        }
    }

    pub fn run_cron(
        &mut self,
        epoch: ChainEpoch,
        callback: Option<
            &mut impl FnMut(&Cid, &ChainMessage, &ApplyRet) -> Result<(), anyhow::Error>,
        >,
    ) -> Result<(), anyhow::Error> {
        let cron_msg: Message = Message_v3 {
            from: Address::SYSTEM_ACTOR.into(),
            to: Address::CRON_ACTOR.into(),
            // Epoch as sequence is intentional
            sequence: epoch as u64,
            // Arbitrarily large gas limit for cron (matching Lotus value)
            gas_limit: BLOCK_GAS_LIMIT as u64 * 10000,
            method_num: cron::Method::EpochTick as u64,
            params: Default::default(),
            value: Default::default(),
            version: Default::default(),
            gas_fee_cap: Default::default(),
            gas_premium: Default::default(),
        }
        .into();

        let ret = self.apply_implicit_message(&cron_msg)?;
        if let Some(err) = ret.failure_info() {
            anyhow::bail!("failed to apply block cron message: {}", err);
        }

        if let Some(callback) = callback {
            callback(&(cron_msg.cid()?), &ChainMessage::Unsigned(cron_msg), &ret)?;
        }
        Ok(())
    }

    /// Apply block messages from a Tipset.
    /// Returns the receipts from the transactions.
    pub fn apply_block_messages(
        &mut self,
        messages: &[BlockMessages],
        epoch: ChainEpoch,
        mut callback: Option<
            impl FnMut(&Cid, &ChainMessage, &ApplyRet) -> Result<(), anyhow::Error>,
        >,
    ) -> Result<Vec<Receipt>, anyhow::Error> {
        let mut receipts = Vec::new();
        let mut processed = HashSet::<Cid>::default();

        for block in messages.iter() {
            let mut penalty = TokenAmount::zero();
            let mut gas_reward = TokenAmount::zero();

            let mut process_msg = |msg: &ChainMessage| -> Result<(), anyhow::Error> {
                let cid = msg.cid()?;
                // Ensure no duplicate processing of a message
                if processed.contains(&cid) {
                    return Ok(());
                }
                let ret = self.apply_message(msg)?;

                if let Some(cb) = &mut callback {
                    cb(&cid, msg, &ret)?;
                }

                // Update totals
                gas_reward += ret.miner_tip();
                penalty += ret.penalty();
                receipts.push(ret.msg_receipt());

                // Add processed Cid to set of processed messages
                processed.insert(cid);
                Ok(())
            };

            for msg in block.messages.iter() {
                process_msg(msg)?;
            }

            // Generate reward transaction for the miner of the block
            if let Some(rew_msg) =
                self.reward_message(epoch, block.miner, block.win_count, penalty, gas_reward)?
            {
                let ret = self.apply_implicit_message(&rew_msg)?;
                if let Some(err) = ret.failure_info() {
                    anyhow::bail!(
                        "failed to apply reward message for miner {}: {}",
                        block.miner,
                        err
                    );
                }
                // This is more of a sanity check, this should not be able to be hit.
                if !ret.msg_receipt().exit_code().is_success() {
                    anyhow::bail!(
                        "reward application message failed (exit: {:?})",
                        ret.msg_receipt().exit_code()
                    );
                }
                if let Some(callback) = &mut callback {
                    callback(&(rew_msg.cid()?), &ChainMessage::Unsigned(rew_msg), &ret)?;
                }
            }
        }

        if let Err(e) = self.run_cron(epoch, callback.as_mut()) {
            tracing::error!("End of epoch cron failed to run: {}", e);
        }
        Ok(receipts)
    }

    /// Applies single message through VM and returns result from execution.
    pub fn apply_implicit_message(&mut self, msg: &Message) -> Result<ApplyRet, anyhow::Error> {
        // raw_length is not used for Implicit messages.
        let raw_length = to_vec(msg).expect("encoding error").len();

        match self {
            VM::VM2(fvm_executor) => {
                let ret = fvm_executor.execute_message(
                    msg.into(),
                    fvm2::executor::ApplyKind::Implicit,
                    raw_length,
                )?;
                Ok(ret.into())
            }
            VM::VM3(fvm_executor) => {
                let ret = fvm_executor.execute_message(
                    msg.into(),
                    fvm3::executor::ApplyKind::Implicit,
                    raw_length,
                )?;
                Ok(ret.into())
            }
        }
    }

    /// Applies the state transition for a single message.
    /// Returns `ApplyRet` structure which contains the message receipt and some
    /// meta data.
    pub fn apply_message(&mut self, msg: &ChainMessage) -> Result<ApplyRet, anyhow::Error> {
        // Basic validity check
        msg.message().check()?;

        let unsigned = msg.message().clone();
        let raw_length = to_vec(msg).expect("encoding error").len();
        let ret: ApplyRet = match self {
            VM::VM2(fvm_executor) => {
                let ret = fvm_executor.execute_message(
                    unsigned.into(),
                    fvm2::executor::ApplyKind::Explicit,
                    raw_length,
                )?;

                if fvm_executor.externs().bail() {
                    bail!("encountered a database lookup error");
                }

                ret.into()
            }
            VM::VM3(fvm_executor) => {
                let ret = fvm_executor.execute_message(
                    unsigned.into(),
                    fvm3::executor::ApplyKind::Explicit,
                    raw_length,
                )?;

                if fvm_executor.externs().bail() {
                    bail!("encountered a database lookup error");
                }

                ret.into()
            }
        };

        let exit_code = ret.msg_receipt().exit_code();

        if !exit_code.is_success() {
            tracing::debug!(?exit_code, "VM message execution failure.")
        }

        Ok(ret)
    }

    fn reward_message(
        &self,
        epoch: ChainEpoch,
        miner: Address,
        win_count: i64,
        penalty: TokenAmount,
        gas_reward: TokenAmount,
    ) -> Result<Option<Message>, anyhow::Error> {
        let params = RawBytes::serialize(AwardBlockRewardParams {
            miner: miner.into(),
            penalty: penalty.into(),
            gas_reward: gas_reward.into(),
            win_count,
        })?;
        let rew_msg = Message_v3 {
            from: Address::SYSTEM_ACTOR.into(),
            to: Address::REWARD_ACTOR.into(),
            method_num: reward::Method::AwardBlockReward as u64,
            params,
            // Epoch as sequence is intentional
            sequence: epoch as u64,
            gas_limit: 1 << 30,
            value: Default::default(),
            version: Default::default(),
            gas_fee_cap: Default::default(),
            gas_premium: Default::default(),
        };
        Ok(Some(rew_msg.into()))
    }
}
