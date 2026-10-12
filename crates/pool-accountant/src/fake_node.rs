//! A node that answers from memory, for tests.

use pool_chain::{NodeError, NodeUtxo, NodeView};
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[derive(Default)]
pub struct FakeNode {
    daa: AtomicU64,
    utxos: Mutex<Vec<NodeUtxo>>,
    colours: Mutex<HashMap<String, bool>>,
    fail_next: AtomicU32,
}

impl FakeNode {
    pub fn set_daa(&self, daa: u64) {
        self.daa.store(daa, Ordering::SeqCst);
    }

    pub fn set_utxos(&self, utxos: Vec<NodeUtxo>) {
        *self.utxos.lock().unwrap() = utxos;
    }

    pub fn set_colour(&self, hash: &str, blue: bool) {
        self.colours.lock().unwrap().insert(hash.to_owned(), blue);
    }

    /// Make the next `n` calls fail, as a node that is restarting would.
    pub fn fail_next(&self, n: u32) {
        self.fail_next.store(n, Ordering::SeqCst);
    }

    fn check(&self) -> Result<(), NodeError> {
        let left = self.fail_next.load(Ordering::SeqCst);
        if left > 0 {
            self.fail_next.store(left - 1, Ordering::SeqCst);
            return Err(NodeError("the fake node is down".into()));
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl NodeView for FakeNode {
    async fn virtual_daa_score(&self) -> Result<u64, NodeError> {
        self.check()?;
        Ok(self.daa.load(Ordering::SeqCst))
    }

    async fn utxos_at(&self, _address: &str) -> Result<Vec<NodeUtxo>, NodeError> {
        self.check()?;
        Ok(self.utxos.lock().unwrap().clone())
    }

    async fn balance_at(&self, _address: &str) -> Result<u64, NodeError> {
        self.check()?;
        Ok(self.utxos.lock().unwrap().iter().map(|u| u.amount).sum())
    }

    async fn block_is_blue(&self, hash: &str) -> Result<bool, NodeError> {
        self.check()?;
        self.colours.lock().unwrap().get(hash).copied().ok_or_else(|| NodeError(format!("block {hash} is not known")))
    }
}
