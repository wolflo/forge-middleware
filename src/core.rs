use async_trait::async_trait;
use ethers_core::types::{
    transaction::eip2718::TypedTransaction, Address, BlockId, BlockNumber, Bytes, NameOrAddress,
    H256, U256, U64,
};
use ethers_providers::{JsonRpcClient, Middleware, Provider, ProviderError};
use evm_adapters::Evm;
use std::{
    fmt::Debug,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::sync::RwLock;

use crate::evm::VmShow;

const DEFAULT_SENDER: &str = "0xD3D13a578a53685B4ac36A1Bab31912D2B2A2F36";

#[derive(Debug, Clone)]
pub enum Inner<T> {
    Use(T), // Indicates a valid provider to fallback to
    Not(T), // Indicates a dummy provider that should not be used
}
impl Inner<Provider<NoClient>> {
    pub fn not() -> Self {
        Self::Not(Provider::new(NoClient::new()))
    }
}
impl<T> Inner<T> {
    pub fn is_not(&self) -> bool {
        match self {
            Self::Not(_) => true,
            _ => false,
        }
    }
    pub fn is_use(&self) -> bool {
        match self {
            Self::Use(_) => true,
            _ => false,
        }
    }
    pub fn get(&self) -> &T {
        match self {
            Self::Use(x) => x,
            Self::Not(x) => x,
        }
    }
}

#[derive(Clone)]
pub struct Forge<M, E, S> {
    pub vm: Arc<RwLock<E>>,
    pub inner: Inner<M>,
    _ghost: PhantomData<S>,
}

impl<E, S> Forge<Provider<NoClient>, E, S> {
    pub fn new(vm: Arc<RwLock<E>>) -> Self {
        Self {
            vm,
            inner: Inner::not(),
            _ghost: PhantomData,
        }
    }
}
impl<M, E, S> Forge<M, E, S> {
    pub fn new_with_provider(vm: Arc<RwLock<E>>, inner: M) -> Self {
        Self {
            vm,
            inner: Inner::Use(inner),
            _ghost: PhantomData,
        }
    }
    pub async fn vm(&self) -> impl Deref<Target = E> + '_ {
        self.vm.read().await
    }
    pub async fn vm_mut(&self) -> impl DerefMut<Target = E> + '_ {
        self.vm.write().await
    }
}

pub enum TxOutput {
    CallRes(Bytes),
    CreateRes(Address),
}
impl TxOutput {
    pub fn maybe_addr(&self) -> Option<Address> {
        match self {
            Self::CreateRes(addr) => Some(*addr),
            _ => None,
        }
    }
    pub fn maybe_bytes(self) -> Option<Bytes> {
        match self {
            Self::CallRes(bytes) => Some(bytes),
            _ => None,
        }
    }
}

// Gives some structure to the result of Evm::call_raw()
pub struct TxRes<Ex> {
    pub output: TxOutput,
    pub exit: Ex,
    pub gas: u64,
    pub logs: Vec<String>,
}
impl<M, E, S> Forge<M, E, S>
where
    Self: Middleware,
    E: Evm<S> + VmShow,
    <Self as Middleware>::Error: From<eyre::ErrReport>,
{
    //TODO: incoporate block parameter
    pub async fn apply_tx(
        &self,
        tx: &TypedTransaction,
    ) -> Result<TxRes<E::ReturnReason>, <Self as Middleware>::Error> {
        // Pull fields from tx to pass to evm
        let default_from = DEFAULT_SENDER.parse().unwrap();
        let default_val = U256::zero();

        let from = tx.from().unwrap_or(&default_from);
        let maybe_to = tx.to().map(|id| async move {
            match id {
                NameOrAddress::Name(ens) => self.resolve_name(ens).await,
                NameOrAddress::Address(addr) => Ok(*addr),
            }
        });
        let data = tx.data().map_or(Default::default(), |d| d.clone());
        let val = tx.value().unwrap_or(&default_val);

        if let Some(fut) = maybe_to {
            // (contract) call
            let to = fut.await?;
            let (bytes, exit, gas, logs) =
                self.vm_mut().await.call_raw(*from, to, data, *val, false)?;
            Ok(TxRes {
                output: TxOutput::CallRes(bytes),
                exit,
                gas,
                logs,
            })
        } else {
            // contract deployment
            let (addr, exit, gas, logs) = self.vm_mut().await.deploy(*from, data.clone(), *val)?;
            Ok(TxRes {
                output: TxOutput::CreateRes(addr),
                exit,
                gas,
                logs,
            })
        }
    }

    pub async fn is_latest(&self, id: BlockId) -> Result<bool, <Self as Middleware>::Error> {
        match id {
            BlockId::Hash(hash) => {
                let vm = self.vm().await;
                let last_hash = vm.block_hash(vm.block_number() - U256::one());
                // If we get the default hash back, the vm doesn't have the block data
                Ok(last_hash != Default::default() && hash == last_hash)
            }
            BlockId::Number(n) => match n {
                BlockNumber::Latest => Ok(true),
                BlockNumber::Number(num) => {
                    Ok(num == (self.get_block_number().await? - U64::one()))
                }
                BlockNumber::Pending => Ok(true), //TODO
                _ => Ok(false),
            },
        }
    }

    // Sputnik can provide hashes for any block it produced, but not the rest of the block data
    pub async fn get_block_hash(&self, num: U256) -> H256 {
        // TODO: try to pull historical data if we get back default and have a provider
        self.vm().await.block_hash(num)
    }

    pub async fn to_addr<T: Into<NameOrAddress>>(
        &self,
        id: T,
    ) -> Result<Address, <Self as Middleware>::Error> {
        match id.into() {
            NameOrAddress::Name(ref ens) => self.resolve_name(ens).await,
            NameOrAddress::Address(a) => Ok(a),
        }
    }
}

// TODO: Stand-in impl because some sputnik component is not Debug
impl<M, E, S> Debug for Forge<M, E, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Forge").finish()
    }
}

// Dummy provider / Middleware indicating an empty provider
#[derive(Debug, Default, Clone, Copy)]
pub struct NoClient;
impl NoClient {
    pub fn new() -> Self {
        Self
    }
}
#[async_trait]
impl JsonRpcClient for NoClient {
    type Error = ProviderError;
    async fn request<T, R>(&self, _method: &str, _params: T) -> Result<R, Self::Error>
    where
        T: std::fmt::Debug + serde::Serialize + Send + Sync,
        R: serde::de::DeserializeOwned,
    {
        unreachable!("Cannot send requests")
    }
}
