use crate::action::ActionWrapper;
use holochain_core_types::{
    cas::{content::Address, storage::ContentAddressableStorage},
    eav::{EntityAttributeValueIndex, EntityAttributeValueStorage, IndexQuery},
    error::HolochainError,
};

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, RwLock},
};

/// The state-slice for the DHT.
/// Holds the agent's local shard and interacts with the network module
#[derive(Clone, Debug)]
pub struct DhtStore {
    // Storages holding local shard data
    content_storage: Arc<RwLock<ContentAddressableStorage>>,
    meta_storage: Arc<RwLock<EntityAttributeValueStorage>>,

    actions: HashMap<ActionWrapper, Result<Address, HolochainError>>,
}

impl PartialEq for DhtStore {
    fn eq(&self, other: &DhtStore) -> bool {
        let content = &self.content_storage.clone();
        let other_content = &other.content_storage().clone();
        let meta = &self.meta_storage.clone();
        let other_meta = &other.meta_storage.clone();

        self.actions == other.actions
            && (*content.read().unwrap()).get_id() == (*other_content.read().unwrap()).get_id()
            && *meta.read().unwrap() == *other_meta.read().unwrap()
    }
}

impl DhtStore {
    // LifeCycle
    // =========
    pub fn new(
        content_storage: Arc<RwLock<ContentAddressableStorage>>,
        meta_storage: Arc<RwLock<EntityAttributeValueStorage>>,
    ) -> Self {
        DhtStore {
            content_storage,
            meta_storage,
            actions: HashMap::new(),
        }
    }

    pub fn get_links(
        &self,
        address: Address,
        tag: String,
    ) -> Result<BTreeSet<EntityAttributeValueIndex>, HolochainError> {
        let index_query = IndexQuery::new_only_prefixes(vec!["link__", "removed_link__"]);
        let filtered =
            self.meta_storage
                .read()?
                .fetch_eavi(Some(address), Some(tag), None, index_query)?;

        Ok(filtered
            .into_iter()
            .filter(|eav| eav.attribute().starts_with("link__"))
            .collect())
    }

    // Getters (for reducers)
    // =======
    pub(crate) fn content_storage(&self) -> Arc<RwLock<ContentAddressableStorage>> {
        self.content_storage.clone()
    }
    pub(crate) fn meta_storage(&self) -> Arc<RwLock<EntityAttributeValueStorage>> {
        self.meta_storage.clone()
    }
    pub fn actions(&self) -> &HashMap<ActionWrapper, Result<Address, HolochainError>> {
        &self.actions
    }
    pub(crate) fn actions_mut(
        &mut self,
    ) -> &mut HashMap<ActionWrapper, Result<Address, HolochainError>> {
        &mut self.actions
    }
}