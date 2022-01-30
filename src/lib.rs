pub mod core;
pub mod evm;
pub mod middleware;

#[cfg(test)]
mod tests {
    use ethers_core::types::{Address, TransactionRequest};
    use ethers_providers::Middleware;
    use evm_adapters::sputnik::{
        helpers::{new_backend, CFG, GAS_LIMIT, VICINITY},
        Executor, PRECOMPILES_MAP,
    };
    use std::sync::Arc;
    use tokio::sync::RwLock;

    use crate::core::Forge;

    #[tokio::test]
    async fn test_forge() {
        let from: Address = "0xEA674fdDe714fd979de3EdF0F56AA9716B898ec8"
            .parse()
            .unwrap();
        let to: Address = "0xD3D13a578a53685B4ac36A1Bab31912D2B2A2F36"
            .parse()
            .unwrap();

        let backend = new_backend(&*VICINITY, Default::default());
        let vm = Arc::new(RwLock::new(Executor::new(
            GAS_LIMIT,
            &*CFG,
            &backend,
            &*PRECOMPILES_MAP,
        )));
        let forge = Forge::new(vm);

        let tx = TransactionRequest::new()
            .to(to)
            .from(from)
            .value(1)
            .gas(2300);
        let receipt = forge
            .send_transaction(tx, None)
            .await
            .unwrap()
            .await
            .unwrap();
        dbg!(receipt);
    }
}
