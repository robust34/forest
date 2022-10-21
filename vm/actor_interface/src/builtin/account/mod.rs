// Copyright 2019-2022 ChainSafe Systems
// SPDX-License-Identifier: Apache-2.0, MIT

use cid::Cid;
use fvm::state_tree::ActorState;
use fvm_ipld_blockstore::Blockstore;
use fvm_shared::address::Address;
use serde::Serialize;

use anyhow::Context;
use forest_ipld_blockstore::BlockStoreExt;

/// Account actor method.
pub type Method = fil_actor_account_v8::Method;

/// Account actor state.
#[derive(Serialize)]
#[serde(untagged)]
pub enum State {
    V8(fil_actor_account_v8::State),
}

pub fn is_v8_account_cid(cid: &Cid) -> bool {
    let known_cids = vec![
        // calibnet v8
        Cid::try_from("bafk2bzacecruossn66xqbeutqx5r4k2kjzgd43frmwd4qkw6haez44ubvvpxo").unwrap(),
        // calibnet v9
        Cid::try_from("bafk2bzacedsgsmahbc6wiaigtfb3uoj436rvxptsq3gl6koadig57bwmdulic").unwrap(),
        // mainnet
        Cid::try_from("bafk2bzacedudbf7fc5va57t3tmo63snmt3en4iaidv4vo3qlyacbxaa6hlx6y").unwrap(),
        // devnet
        Cid::try_from("bafk2bzacea4tlgnp7m6tlldpz3termlwxlnyq24nwd4zdzv4r6nsjuaktuuzc").unwrap(),
    ];
    known_cids.contains(cid)
}

impl State {
    pub fn load<BS>(store: &BS, actor: &ActorState) -> anyhow::Result<State>
    where
        BS: Blockstore,
    {
        if is_v8_account_cid(&actor.code) {
            return store
                .get_obj(&actor.state)?
                .map(State::V8)
                .context("Actor state doesn't exist in store");
        }
        Err(anyhow::anyhow!("Unknown account actor code {}", actor.code))
    }

    pub fn pubkey_address(&self) -> Address {
        match self {
            State::V8(st) => st.address,
        }
    }
}
