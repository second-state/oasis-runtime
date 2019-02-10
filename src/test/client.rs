//! Test client to interact with a runtime-ethereum blockchain.

use ekiden_core::mrae::nonce::{Nonce, NONCE_SIZE};
use ekiden_keymanager_common::ContractKey;
use ethcore::{
    rlp,
    state::ConfidentialCtx as EthConfidentialCtx,
    transaction::{Action, Transaction as EthcoreTransaction},
};
use ethereum_api::{Receipt, TransactionRequest};
use ethereum_types::{Address, H256, U256};
use ethkey::{KeyPair, Secret};
use runtime_ethereum_common::confidential::{
    key_manager::TestKeyManager, ConfidentialCtx, CONFIDENTIAL_PREFIX,
};
use std::{
    str::FromStr,
    sync::{Mutex, MutexGuard},
};
use test::*;

lazy_static! {
    static ref CLIENT: Mutex<Client> = Mutex::new(Client::new());
}

pub struct Client {
    /// KeyPair used for signing transactions.
    pub keypair: KeyPair,
    /// Contract key used for encrypting web3c transactions.
    pub ephemeral_key: ContractKey,
}

impl Client {
    fn new() -> Self {
        Self {
            // address: 0x7110316b618d20d0c44728ac2a3d683536ea682
            keypair: KeyPair::from_secret(
                Secret::from_str(
                    "533d62aea9bbcb821dfdda14966bb01bfbbb53b7e9f5f0d69b8326e052e3450c",
                )
                .unwrap(),
            )
            .unwrap(),
            ephemeral_key: TestKeyManager::create_random_key(),
        }
    }

    /// Returns a handle to the client to interact with the blockchain.
    pub fn instance<'a>() -> MutexGuard<'a, Self> {
        CLIENT.lock().unwrap()
    }

    pub fn estimate_gas(&self, contract: Option<&Address>, data: Vec<u8>, value: &U256) -> U256 {
        let tx = TransactionRequest {
            caller: Some(self.keypair.address()),
            is_call: contract.is_some(),
            address: contract.map(|c| *c),
            input: Some(data),
            value: Some(*value),
            nonce: None,
            gas: None,
        };

        with_batch_handler(|ctx| {
            let response = simulate_transaction(&tx, ctx).unwrap();
            response.used_gas + response.refunded_gas
        })
    }

    pub fn confidential_estimate_gas(
        &self,
        contract: Option<&Address>,
        data: Vec<u8>,
        value: &U256,
    ) -> U256 {
        self.estimate_gas(contract, self.confidential_data(contract, data), value)
    }

    /// Returns an encrypted form of the data field to be used in a web3c confidential
    /// transaction
    pub fn confidential_data(&self, contract: Option<&Address>, data: Vec<u8>) -> Vec<u8> {
        if contract.is_none() {
            // Don't encrypt confidential deploys.
            let mut conf_deploy_data = CONFIDENTIAL_PREFIX.to_vec();
            conf_deploy_data.append(&mut data.clone());
            return conf_deploy_data;
        }

        let contract_addr = contract.unwrap();
        let enc_data = self
            .confidential_ctx(contract_addr.clone())
            .encrypt(data)
            .unwrap();

        enc_data
    }

    /// Creates a non-confidential contract, return the transaction hash for the deploy
    /// and the address of the contract.
    pub fn create_contract(&mut self, code: Vec<u8>, balance: &U256) -> (H256, Address) {
        let hash = self.send(None, code, balance);
        let receipt = with_batch_handler(|ctx| get_receipt(&hash, ctx).unwrap().unwrap());
        (hash, receipt.contract_address.unwrap())
    }

    pub fn receipt(&self, tx_hash: H256) -> Receipt {
        with_batch_handler(|ctx| get_receipt(&tx_hash, ctx))
            .unwrap()
            .unwrap()
    }

    /// Returns the transaction hash and address of the confidential contract. The code given
    /// should not have the confidential prefix, as that will be added automatically.
    pub fn create_confidential_contract(
        &mut self,
        code: Vec<u8>,
        balance: &U256,
    ) -> (H256, Address) {
        let hash = self.confidential_send(None, code, balance);
        let receipt = with_batch_handler(|ctx| get_receipt(&hash, ctx).unwrap().unwrap());
        (hash, receipt.contract_address.unwrap())
    }

    /// Makes a simulated transaction, analagous to the web3.js call().
    /// Returns the return value of the contract's method.
    pub fn call(&mut self, contract: &Address, data: Vec<u8>, value: &U256) -> Vec<u8> {
        let tx = TransactionRequest {
            caller: Some(self.keypair.address()),
            is_call: true,
            address: Some(*contract),
            input: Some(data),
            value: Some(*value),
            nonce: None,
            gas: None,
        };

        with_batch_handler(|ctx| simulate_transaction(&tx, ctx).unwrap().result.unwrap())
    }

    /// Sends a transaction onchain that updates the blockchain, analagous to the web3.js send().
    pub fn send(&mut self, contract: Option<&Address>, data: Vec<u8>, value: &U256) -> H256 {
        with_batch_handler(|ctx| {
            let tx = EthcoreTransaction {
                action: if contract == None {
                    Action::Create
                } else {
                    Action::Call(*contract.unwrap())
                },
                nonce: get_account_nonce(&self.keypair.address(), ctx).unwrap(),
                gas_price: U256::from(0),
                gas: U256::from(1000000),
                value: *value,
                data: data,
            }
            .sign(&self.keypair.secret(), None);

            let raw = rlp::encode(&tx);
            execute_raw_transaction(&raw.into_vec(), ctx)
                .unwrap()
                .hash
                .unwrap()
        })
    }

    /// Performs a confidential call, i.e., a simulated transaction that doesn't update
    /// blockchaian state. Returns the return value of the contract's functions.
    pub fn confidential_call(
        &mut self,
        contract: &Address,
        data: Vec<u8>,
        value: &U256,
    ) -> Vec<u8> {
        self.confidential_invocation(Some(contract), data, value, false)
    }

    /// Performs a confidential transaction updating the state of the blockchain.
    /// `Data` should be unencrypted (and without a confidential prefix for deploys).
    /// Such details will be added to the transaction automatically. Returns the
    /// transaction's hash.
    pub fn confidential_send(
        &mut self,
        contract: Option<&Address>,
        data: Vec<u8>,
        value: &U256,
    ) -> H256 {
        let tx_hash = self.confidential_invocation(contract, data, value, true);
        assert!(tx_hash.len() == 32);
        H256::from(tx_hash.as_slice())
    }

    /// Performs confidential calls, sends, and deploys.
    fn confidential_invocation(
        &mut self,
        contract: Option<&Address>,
        data: Vec<u8>,
        value: &U256,
        is_send: bool,
    ) -> Vec<u8> {
        let enc_data = self.confidential_data(contract.clone(), data);
        if is_send {
            self.send(contract, enc_data, value).to_vec()
        } else {
            let contract_addr = contract.unwrap();
            let encrypted_result = self.call(contract_addr, enc_data, value);
            self.confidential_ctx(*contract_addr)
                .decrypt(encrypted_result)
                .unwrap()
        }
    }

    /// Returns an *open* confidential context used from the perspective of the client,
    /// so that it can encrypt/decrypt transactions to/from web3c. This should not be
    /// injected into the parity State, because such a confidential context should be
    /// from the perspective of the keymanager. See `key_manager_confidential_ctx`.
    pub fn confidential_ctx(&self, contract: Address) -> ConfidentialCtx {
        self.make_ctx(contract, false)
    }

    /// Returns an *open* confidential context. Using this with a parity State object will
    /// transparently encrypt/decrypt everything going into and out of contract storage.
    /// Do not use this if you're trying to access *unencrypted* state.
    pub fn key_manager_confidential_ctx(&self, contract: Address) -> ConfidentialCtx {
        self.make_ctx(contract, true)
    }

    /// Returns a new, open ConfidentialCtx. Here we use such a context in two ways: 1)
    /// from the "perspective" of the client and 2) from the perspective of the key manager,
    /// i.e., a contract execution inside an enclave. The former can be used to encrypt/decrypt
    /// to web3c. The latter can be used to encrypt/decrypt inside web3c (just as a compute node
    /// would).
    fn make_ctx(&self, contract: Address, is_key_manager: bool) -> ConfidentialCtx {
        let contract_key = TestKeyManager::instance().contract_key(contract).unwrap();
        // Note that what key is used as the "peer" switches depending upon `is_key_manager`.
        // From the perspective of the client, the "peer" is the contract (i.e. the key
        // manager), and vice versa. This is a result of our mrae's symmetric key derivation.
        let (peer_key, contract_key) = if is_key_manager {
            (self.ephemeral_key.input_keypair.get_pk(), contract_key)
        } else {
            (
                contract_key.input_keypair.get_pk(),
                self.ephemeral_key.clone(),
            )
        };
        // No need to save the Nonce on the Client (for now).
        let nonce = Nonce::new([0; NONCE_SIZE]);
        ConfidentialCtx {
            peer_public_key: Some(peer_key),
            contract_key: Some(contract_key),
            next_nonce: Some(nonce),
        }
    }

    /// Returns the raw underlying storage for the given `contract`--without
    /// encrypting the key or decrypting the return value.
    pub fn raw_storage(&self, contract: Address, storage_key: H256) -> Option<Vec<u8>> {
        with_batch_handler(|ctx| {
            let ectx = ctx.runtime.downcast_mut::<EthereumContext>().unwrap();
            let state = ectx.cache.get_state(ConfidentialCtx::new()).unwrap();
            state._storage_at(&contract, &storage_key).unwrap()
        })
    }

    /// Returns the key that actually stores the confidential contract's storage value.
    /// To be used together with `Client::raw_storage`.
    pub fn confidential_storage_key(&self, contract: Address, storage_key: H256) -> H256 {
        let km_confidential_ctx = self.key_manager_confidential_ctx(contract);
        keccak_hash::keccak(
            &km_confidential_ctx
                .encrypt_storage(storage_key.to_vec())
                .unwrap(),
        )
    }
}