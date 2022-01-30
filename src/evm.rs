use ethers_core::types::{Address, H256, U256};
use evm_adapters::sputnik::{Executor, SputnikExecutor};
use sputnik::backend::Backend;

pub trait VmShow {
    fn gas_price(&self) -> U256;
    fn block_number(&self) -> U256;
    fn chain_id(&self) -> U256;
    fn balance(&self, from: Address) -> U256;
    fn gas_limit(&self) -> U256;
    fn block_hash(&self, num: U256) -> H256;
}

impl<'a, S, E> VmShow for Executor<S, E>
where
    E: SputnikExecutor<S>,
    S: Backend,
{
    fn gas_price(&self) -> U256 {
        self.executor.state().gas_price()
    }
    fn block_number(&self) -> U256 {
        self.executor.state().block_number()
    }
    fn chain_id(&self) -> U256 {
        self.executor.state().block_number()
    }
    fn balance(&self, addr: Address) -> U256 {
        self.executor.state().basic(addr).balance
    }
    fn gas_limit(&self) -> U256 {
        self.executor.state().block_gas_limit()
    }
    fn block_hash(&self, num: U256) -> H256 {
        self.executor.state().block_hash(num)
    }
}
