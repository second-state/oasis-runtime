use std::{collections::BTreeMap, sync::Arc};

use ekiden_core::error::Result;
use ethcore::{executive::{contract_address, Executed, Executive, TransactOptions},
              machine::EthereumMachine,
              spec::CommonParams,
              transaction::{SignedTransaction, Transaction},
              vm};
use ethereum_types::{Address, U256};

use super::state::{block_hashes_since, get_latest_block_number, get_state, BlockOffset};

/// as per https://github.com/paritytech/parity/blob/master/ethcore/res/ethereum/byzantium_test.json
macro_rules! evm_params {
    () => {{
        let mut params = CommonParams::default();
        params.maximum_extra_data_size = 0x20;
        params.min_gas_limit = 0x1388.into();
        params.network_id = 0x01;
        params.max_code_size = 24576;
        params.eip98_transition = <u64>::max_value();
        params.gas_limit_bound_divisor = 0x0400.into();
        params.registrar = "0xc6d9d2cd449a754c494264e1809c50e34d64562b".into();
        params
    }};
}

fn get_env_info() -> vm::EnvInfo {
    let mut env_info = vm::EnvInfo::default();
    env_info.last_hashes = Arc::new(block_hashes_since(BlockOffset::Offset(256)));
    env_info.number = get_latest_block_number() + 1;
    env_info.gas_limit = U256::max_value();
    env_info
}

pub fn simulate_transaction(transaction: &SignedTransaction) -> Result<Executed> {
    let machine = EthereumMachine::regular(evm_params!(), BTreeMap::new() /* builtins */);

    let mut state = get_state()?;
    #[cfg(not(feature = "benchmark"))]
    let options = TransactOptions::with_no_tracing();
    #[cfg(feature = "benchmark")]
    let options = TransactOptions::with_no_tracing().dont_check_nonce();
    let exec = Executive::new(&mut state, &get_env_info(), &machine)
        .transact_virtual(&transaction, options)?;
    Ok(exec)
}

pub fn get_contract_address(transaction: &Transaction) -> Address {
    contract_address(
        vm::CreateContractAddress::FromCodeHash,
        &Address::zero(), // unused
        &U256::zero(),    // unused
        &transaction.data,
    ).0
}