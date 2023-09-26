use super::{fmt_err, Cheatcodes, Error, Result};
use crate::{
    abi::HEVMCalls,
    executor::{
        backend::DatabaseExt, fork::CreateFork, inspector::cheatcodes::ext::value_to_token,
    },
    utils::RuntimeOrHandle,
};
use alloy_dyn_abi::DynSolValue;
use alloy_primitives::{Bytes, B256, U256};
use alloy_sol_types::sol;
use ethers::{providers::Middleware, types::Filter};
use foundry_abi::hevm::{EthGetLogsCall, RpcCall};
use foundry_common::ProviderBuilder;
use foundry_utils::types::{ToAlloy, ToEthers};
use itertools::Itertools;
use revm::EVMData;
use serde_json::Value;

fn empty<T>(_: T) -> Bytes {
    Bytes::new()
}

sol! {
    struct Rpc { string name; string url; }
}

/// Handles fork related cheatcodes
#[instrument(level = "error", name = "fork", target = "evm::cheatcodes", skip_all)]
pub fn apply<DB: DatabaseExt>(
    state: &mut Cheatcodes,
    data: &mut EVMData<'_, DB>,
    call: &HEVMCalls,
) -> Option<Result> {
    let result = match call {
        HEVMCalls::CreateFork0(fork) => create_fork(state, data, fork.0.clone(), None),
        HEVMCalls::CreateFork1(fork) => {
            create_fork(state, data, fork.0.clone(), Some(fork.1.as_u64()))
        }
        HEVMCalls::CreateFork2(fork) => {
            create_fork_at_transaction(state, data, fork.0.clone(), fork.1.into())
        }
        HEVMCalls::CreateSelectFork0(fork) => create_select_fork(state, data, fork.0.clone(), None),
        HEVMCalls::CreateSelectFork1(fork) => {
            create_select_fork(state, data, fork.0.clone(), Some(fork.1.as_u64()))
        }
        HEVMCalls::CreateSelectFork2(fork) => {
            create_select_fork_at_transaction(state, data, fork.0.clone(), fork.1.into())
        }
        HEVMCalls::SelectFork(fork_id) => select_fork(state, data, fork_id.0.to_alloy()),
        HEVMCalls::MakePersistent0(acc) => {
            data.db.add_persistent_account(acc.0.to_alloy());
            Ok(Bytes::new())
        }
        HEVMCalls::MakePersistent1(acc) => {
            data.db.extend_persistent_accounts(
                (acc.0.clone().into_iter().map(|acc| acc.to_alloy())).collect::<Vec<_>>(),
            );
            Ok(Bytes::new())
        }
        HEVMCalls::MakePersistent2(acc) => {
            data.db.add_persistent_account(acc.0.to_alloy());
            data.db.add_persistent_account(acc.1.to_alloy());
            Ok(Bytes::new())
        }
        HEVMCalls::MakePersistent3(acc) => {
            data.db.add_persistent_account(acc.0.to_alloy());
            data.db.add_persistent_account(acc.1.to_alloy());
            data.db.add_persistent_account(acc.2.to_alloy());
            Ok(Bytes::new())
        }
        HEVMCalls::IsPersistent(acc) => {
            Ok(DynSolValue::Bool(data.db.is_persistent(&acc.0.to_alloy())).encode_single().into())
        }
        HEVMCalls::RevokePersistent0(acc) => {
            data.db.remove_persistent_account(&acc.0.to_alloy());
            Ok(Bytes::new())
        }
        HEVMCalls::RevokePersistent1(acc) => {
            data.db.remove_persistent_accounts(
                acc.0.clone().into_iter().map(|acc| acc.to_alloy()).collect::<Vec<_>>(),
            );
            Ok(Bytes::new())
        }
        HEVMCalls::ActiveFork(_) => data
            .db
            .active_fork_id()
            .map(|id| DynSolValue::Uint(id, 256).encode_single().into())
            .ok_or_else(|| fmt_err!("No active fork")),
        HEVMCalls::RollFork0(fork) => data
            .db
            .roll_fork(None, fork.0.to_alloy(), data.env, &mut data.journaled_state)
            .map(empty)
            .map_err(Into::into),
        HEVMCalls::RollFork1(fork) => data
            .db
            .roll_fork_to_transaction(None, fork.0.into(), data.env, &mut data.journaled_state)
            .map(empty)
            .map_err(Into::into),
        HEVMCalls::RollFork2(fork) => data
            .db
            .roll_fork(
                Some(fork.0).map(|id| id.to_alloy()),
                fork.1.to_alloy(),
                data.env,
                &mut data.journaled_state,
            )
            .map(empty)
            .map_err(Into::into),
        HEVMCalls::RollFork3(fork) => data
            .db
            .roll_fork_to_transaction(
                Some(fork.0).map(|f| f.to_alloy()),
                fork.1.into(),
                data.env,
                &mut data.journaled_state,
            )
            .map(empty)
            .map_err(Into::into),
        HEVMCalls::RpcUrl(rpc) => state
            .config
            .get_rpc_url(&rpc.0)
            .map(|url| DynSolValue::String(url).encode_single().into()),
        HEVMCalls::RpcUrls(_) => {
            let mut urls = Vec::with_capacity(state.config.rpc_endpoints.len());
            for alias in state.config.rpc_endpoints.keys().cloned() {
                match state.config.get_rpc_url(&alias) {
                    Ok(url) => {
                        urls.push([alias, url]);
                    }
                    Err(err) => return Some(Err(err)),
                }
            }
            Ok(DynSolValue::Array(
                urls.into_iter()
                    .map(|t| {
                        DynSolValue::FixedArray(vec![
                            DynSolValue::String(t[0].clone()),
                            DynSolValue::String(t[1].clone()),
                        ])
                    })
                    .collect_vec(),
            )
            .encode_single()
            .into())
        }
        HEVMCalls::RpcUrlStructs(_) => {
            let mut urls = Vec::with_capacity(state.config.rpc_endpoints.len());
            for alias in state.config.rpc_endpoints.keys() {
                match state.config.get_rpc_url(alias) {
                    Ok(url) => {
                        urls.push([alias.clone(), url]);
                    }
                    Err(err) => return Some(Err(err)),
                }
            }
            let urls = DynSolValue::Array(
                urls.into_iter()
                    .map(|u| {
                        DynSolValue::Tuple(vec![
                            DynSolValue::String(u[0].clone()),
                            DynSolValue::String(u[1].clone()),
                        ])
                    })
                    .collect_vec(),
            );
            Ok(urls.encode_single().into())
        }
        HEVMCalls::AllowCheatcodes(addr) => {
            data.db.allow_cheatcode_access(addr.0.to_alloy());
            Ok(Bytes::new())
        }
        HEVMCalls::Transact0(inner) => data
            .db
            .transact(None, inner.0.into(), data.env, &mut data.journaled_state, Some(state))
            .map(empty)
            .map_err(Into::into),
        HEVMCalls::Transact1(inner) => data
            .db
            .transact(
                Some(inner.0.to_alloy()),
                inner.1.into(),
                data.env,
                &mut data.journaled_state,
                Some(state),
            )
            .map(empty)
            .map_err(Into::into),
        HEVMCalls::EthGetLogs(inner) => eth_getlogs(data, inner),
        HEVMCalls::Rpc(inner) => rpc(data, inner),
        _ => return None,
    };
    Some(result)
}

/// Selects the given fork id
fn select_fork<DB: DatabaseExt>(
    state: &mut Cheatcodes,
    data: &mut EVMData<DB>,
    fork_id: U256,
) -> Result {
    if state.broadcast.is_some() {
        return Err(Error::SelectForkDuringBroadcast)
    }

    // No need to correct since the sender's nonce does not get incremented when selecting a fork.
    state.corrected_nonce = true;

    data.db.select_fork(fork_id, data.env, &mut data.journaled_state)?;
    Ok(Bytes::new())
}

/// Creates and then also selects the new fork
fn create_select_fork<DB: DatabaseExt>(
    state: &mut Cheatcodes,
    data: &mut EVMData<'_, DB>,
    url_or_alias: String,
    block: Option<u64>,
) -> Result {
    if state.broadcast.is_some() {
        return Err(Error::SelectForkDuringBroadcast)
    }

    // No need to correct since the sender's nonce does not get incremented when selecting a fork.
    state.corrected_nonce = true;

    let fork = create_fork_request(state, url_or_alias, block, data)?;
    let id = data.db.create_select_fork(fork, data.env, &mut data.journaled_state)?;
    Ok(DynSolValue::Uint(id, 256).encode_single().into())
}

/// Creates a new fork
fn create_fork<DB: DatabaseExt>(
    state: &Cheatcodes,
    data: &mut EVMData<'_, DB>,
    url_or_alias: String,
    block: Option<u64>,
) -> Result {
    let fork = create_fork_request(state, url_or_alias, block, data)?;
    let id = data.db.create_fork(fork)?;
    Ok(DynSolValue::Uint(id, 256).encode_single().into())
}
/// Creates and then also selects the new fork at the given transaction
fn create_select_fork_at_transaction<DB: DatabaseExt>(
    state: &mut Cheatcodes,
    data: &mut EVMData<'_, DB>,
    url_or_alias: String,
    transaction: B256,
) -> Result {
    if state.broadcast.is_some() {
        return Err(Error::SelectForkDuringBroadcast)
    }

    // No need to correct since the sender's nonce does not get incremented when selecting a fork.
    state.corrected_nonce = true;

    let fork = create_fork_request(state, url_or_alias, None, data)?;
    let id = data.db.create_select_fork_at_transaction(
        fork,
        data.env,
        &mut data.journaled_state,
        transaction,
    )?;
    Ok(DynSolValue::Uint(id, 256).encode_single().into())
}

/// Creates a new fork at the given transaction
fn create_fork_at_transaction<DB: DatabaseExt>(
    state: &Cheatcodes,
    data: &mut EVMData<'_, DB>,
    url_or_alias: String,
    transaction: B256,
) -> Result {
    let fork = create_fork_request(state, url_or_alias, None, data)?;
    let id = data.db.create_fork_at_transaction(fork, transaction)?;
    Ok(DynSolValue::Uint(id, 256).encode_single().into())
}

/// Creates the request object for a new fork request
fn create_fork_request<DB: DatabaseExt>(
    state: &Cheatcodes,
    url_or_alias: String,
    block: Option<u64>,
    data: &EVMData<DB>,
) -> Result<CreateFork> {
    let url = state.config.get_rpc_url(url_or_alias)?;
    let mut evm_opts = state.config.evm_opts.clone();
    evm_opts.fork_block_number = block;
    let fork = CreateFork {
        enable_caching: state.config.rpc_storage_caching.enable_for_endpoint(&url),
        url,
        env: data.env.clone(),
        evm_opts,
    };
    Ok(fork)
}

/// Retrieve the logs specified for the current fork.
/// Equivalent to eth_getLogs but on a cheatcode.
fn eth_getlogs<DB: DatabaseExt>(data: &EVMData<DB>, inner: &EthGetLogsCall) -> Result {
    let url = data.db.active_fork_url().ok_or(fmt_err!("No active fork url found"))?;
    if inner.0.to_alloy() > U256::from(u64::MAX) || inner.1.to_alloy() > U256::from(u64::MAX) {
        return Err(fmt_err!("Blocks in block range must be less than 2^64 - 1"))
    }
    // Cannot possibly have more than 4 topics in the topics array.
    if inner.3.len() > 4 {
        return Err(fmt_err!("Topics array must be less than 4 elements"))
    }

    let provider = ProviderBuilder::new(url).build()?;
    let mut filter =
        Filter::new().address(inner.2).from_block(inner.0.as_u64()).to_block(inner.1.as_u64());
    for (i, item) in inner.3.iter().enumerate() {
        match i {
            0 => filter = filter.topic0(U256::from_be_bytes(item.to_owned()).to_ethers()),
            1 => filter = filter.topic1(U256::from_be_bytes(item.to_owned()).to_ethers()),
            2 => filter = filter.topic2(U256::from_be_bytes(item.to_owned()).to_ethers()),
            3 => filter = filter.topic3(U256::from_be_bytes(item.to_owned()).to_ethers()),
            _ => return Err(fmt_err!("Topics array should be less than 4 elements")),
        };
    }

    let logs = RuntimeOrHandle::new()
        .block_on(provider.get_logs(&filter))
        .map_err(|_| fmt_err!("Error in calling eth_getLogs"))?;

    if logs.is_empty() {
        let empty: Bytes = DynSolValue::Array(vec![]).encode_single().into();
        return Ok(empty)
    }

    let result = DynSolValue::Array(
        logs.iter()
            .map(|entry| {
                DynSolValue::Tuple(vec![
                    DynSolValue::Address(entry.address.to_alloy()),
                    DynSolValue::Array(
                        entry
                            .topics
                            .clone()
                            .into_iter()
                            .map(|h| DynSolValue::FixedBytes(h.to_alloy(), 32))
                            .collect_vec(),
                    ),
                    DynSolValue::Bytes(entry.data.0.clone().into()),
                    DynSolValue::Uint(
                        U256::from(
                            entry
                                .block_number
                                .expect("eth_getLogs response should include block_number field")
                                .to_alloy(),
                        ),
                        32,
                    ),
                    DynSolValue::FixedBytes(
                        entry
                            .transaction_hash
                            .expect("eth_getLogs response should include transaction_hash field")
                            .to_alloy(),
                        32,
                    ),
                    DynSolValue::Uint(
                        U256::from(
                            entry
                                .transaction_index
                                .expect(
                                    "eth_getLogs response should include transaction_index field",
                                )
                                .to_alloy(),
                        ),
                        32,
                    ),
                    DynSolValue::FixedBytes(
                        entry
                            .block_hash
                            .expect("eth_getLogs response should include block_hash field")
                            .to_alloy(),
                        32,
                    ),
                    DynSolValue::Uint(
                        U256::from(
                            entry
                                .log_index
                                .expect("eth_getLogs response should include log_index field")
                                .to_alloy(),
                        ),
                        32,
                    ),
                    DynSolValue::Bool(
                        entry.removed.expect("eth_getLogs response should include removed field"),
                    ),
                ])
            })
            .collect::<Vec<DynSolValue>>(),
    )
    .encode_single()
    .into();
    Ok(result)
}

fn rpc<DB: DatabaseExt>(data: &EVMData<DB>, inner: &RpcCall) -> Result {
    let url = data.db.active_fork_url().ok_or(fmt_err!("No active fork url found"))?;
    let provider = ProviderBuilder::new(url).build()?;

    let method = inner.0.as_str();
    let params = inner.1.as_str();
    let params_json: Value = serde_json::from_str(params)?;

    let result: Value = RuntimeOrHandle::new()
        .block_on(provider.request(method, params_json))
        .map_err(|err| fmt_err!("Error in calling {:?}: {:?}", method, err))?;

    let result_as_tokens =
        value_to_token(&result).map_err(|err| fmt_err!("Failed to parse result: {err}"))?;

    Ok(result_as_tokens.encode_single().into())
}
