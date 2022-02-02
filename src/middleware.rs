use async_trait::async_trait;
use ethers_core::types::{
    transaction::eip2718::TypedTransaction, Block, BlockId, Bytes, NameOrAddress,
    TransactionReceipt, TxHash, U256, U64,
};
use ethers_providers::{
    maybe, FromErr, Middleware, PendingTransaction, PendingTxState, ProviderError,
};
use evm_adapters::{Evm, EvmError};
use std::fmt::Debug;
use thiserror::Error;

use crate::{
    core::{Forge, Inner, TxOutput},
    evm::VmShow,
};

#[derive(Error, Debug)]
pub enum ForgeError<M: Middleware> {
    #[error("{0}")]
    MiddlewareError(M::Error),
    #[error("{0}")]
    ProviderError(ProviderError),
    #[error("{0}")]
    EvmError(EvmError),
}
impl<M: Middleware> FromErr<M::Error> for ForgeError<M> {
    fn from(src: M::Error) -> ForgeError<M> {
        ForgeError::MiddlewareError(src)
    }
}
impl<M> From<ProviderError> for ForgeError<M>
where
    M: Middleware,
{
    fn from(src: ProviderError) -> Self {
        Self::ProviderError(src)
    }
}
impl<M> From<EvmError> for ForgeError<M>
where
    M: Middleware,
{
    fn from(src: EvmError) -> Self {
        Self::EvmError(src)
    }
}
impl<M> From<eyre::ErrReport> for ForgeError<M>
where
    M: Middleware,
{
    fn from(src: eyre::ErrReport) -> Self {
        Self::EvmError(EvmError::Eyre(src))
    }
}

#[async_trait]
impl<M, E, S> Middleware for Forge<M, E, S>
where
    M: Middleware,
    E: Evm<S> + VmShow + Send + Sync,
    S: Clone + Send + Sync + Debug,
    E::ReturnReason: Send,
{
    type Error = ForgeError<M>;
    type Provider = M::Provider;
    type Inner = M;

    fn inner(&self) -> &Self::Inner {
        self.inner.get()
    }

    async fn estimate_gas(&self, _tx: &TypedTransaction) -> Result<U256, Self::Error> {
        Ok(self.vm().await.gas_limit())
    }

    async fn get_gas_price(&self) -> Result<U256, Self::Error> {
        Ok(self.vm().await.gas_price())
    }

    async fn get_block_number(&self) -> Result<U64, Self::Error> {
        Ok(self.vm().await.block_number().as_u64().into())
    }

    async fn get_block<T: Into<BlockId> + Send + Sync>(
        &self,
        id: T,
    ) -> Result<Option<Block<TxHash>>, Self::Error> {
        let id = id.into();
        if self.is_latest(id).await? {
            // TODO: don't do a very good job of reconstructing block data here.
            // Try to reconstruct as much of the block from evm data as possible.
            let mut block: Block<TxHash> = Default::default();
            let vm = self.vm().await;
            let num = vm.block_number() - U256::one();
            block.number = Some(num.as_u64().into());
            block.hash = Some(vm.block_hash(num));
            block.parent_hash = self.get_block_hash(num - U256::one()).await;
            Ok(Some(block))
        } else {
            self.inner().get_block(id).await.map_err(FromErr::from)
        }
    }

    async fn get_chainid(&self) -> Result<U256, Self::Error> {
        Ok(self.vm().await.chain_id())
    }

    async fn get_balance<T: Into<NameOrAddress> + Send + Sync>(
        &self,
        from: T,
        block: Option<BlockId>,
    ) -> Result<U256, Self::Error> {
        if block.is_none() || self.is_latest(block.unwrap()).await? {
            let addr = self.to_addr(from).await?;
            Ok(self.vm().await.balance(addr))
        } else {
            self.inner()
                .get_balance(from, block)
                .await
                .map_err(FromErr::from)
        }
    }

    async fn send_transaction<T: Into<TypedTransaction> + Send + Sync>(
        &self,
        tx: T,
        block: Option<BlockId>,
    ) -> Result<PendingTransaction<'_, Self::Provider>, Self::Error> {
        let mut tx = tx.into();

        self.fill_transaction(&mut tx, block).await?;

        // run the tx
        let res = self.apply_tx(&tx).await?;

        // receipt fields
        let gas_used = Some(res.gas.into());
        let status = Some((if E::is_success(&res.exit) { 1usize } else { 0 }).into());
        let contract_address = res.output.maybe_addr();

        // Fake the tx hash for the receipt. Should be able to get a "real"
        // hash modulo signature, which we may not have
        let transaction_hash = tx.sighash();

        let receipt = TransactionReceipt {
            gas_used,
            status,
            contract_address,
            transaction_hash,
            ..Default::default()
        };

        // Set the future to resolve immediately to the populated receipt when polled.
        // This should not attempt to use the provider because the new PendingTransaction
        // has confirmations = 1
        let mut pending = PendingTransaction::new(transaction_hash, self.provider());
        pending.set_state(PendingTxState::CheckingReceipt(Some(receipt)));

        Ok(pending)
    }

    async fn call(
        &self,
        tx: &TypedTransaction,
        _block: Option<BlockId>,
    ) -> Result<Bytes, Self::Error> {
        // Simulate an eth_call by saving the state, running the tx, then resetting state
        let state = (*self.vm().await.state()).clone();

        let res = self.apply_tx(tx).await?;
        let bytes = match res.output {
            TxOutput::CallRes(b) => b,
            // For a contract creation tx, return the deployed bytecode
            TxOutput::CreateRes(addr) => self.get_code(addr, None).await?,
        };

        self.vm_mut().await.reset(state);

        Ok(bytes)
    }

    // Copied from Provider::fill_transaction because we need other middleware
    // method calls to be captured by Forge
    async fn fill_transaction(
        &self,
        tx: &mut TypedTransaction,
        block: Option<BlockId>,
    ) -> Result<(), Self::Error> {
        if let Some(default_sender) = self.default_sender() {
            if tx.from().is_none() {
                tx.set_from(default_sender);
            }
        }

        // TODO: Can we poll the futures below at the same time?
        // Access List + Name resolution and then Gas price + Gas

        // set the ENS name
        if let Some(NameOrAddress::Name(ref ens_name)) = tx.to() {
            let addr = self.resolve_name(ens_name).await?;
            tx.set_to(addr);
        }

        // estimate the gas without the access list
        let gas = maybe(tx.gas().cloned(), self.estimate_gas(tx)).await?;
        let mut al_used = false;

        // set the access lists
        if let Some(access_list) = tx.access_list() {
            if access_list.0.is_empty() {
                if let Ok(al_with_gas) = self.create_access_list(tx, block).await {
                    // only set the access list if the used gas is less than the
                    // normally estimated gas
                    if al_with_gas.gas_used < gas {
                        tx.set_access_list(al_with_gas.access_list);
                        tx.set_gas(al_with_gas.gas_used);
                        al_used = true;
                    }
                }
            }
        }

        if !al_used {
            tx.set_gas(gas);
        }

        match tx {
            TypedTransaction::Eip2930(_) | TypedTransaction::Legacy(_) => {
                let gas_price = maybe(tx.gas_price(), self.get_gas_price()).await?;
                tx.set_gas_price(gas_price);
            }
            TypedTransaction::Eip1559(ref mut inner) => {
                if inner.max_fee_per_gas.is_none() || inner.max_priority_fee_per_gas.is_none() {
                    let (max_fee_per_gas, max_priority_fee_per_gas) =
                        self.estimate_eip1559_fees(None).await?;
                    inner.max_fee_per_gas = Some(max_fee_per_gas);
                    inner.max_priority_fee_per_gas = Some(max_priority_fee_per_gas);
                };
            }
        }

        Ok(())
    }
}
