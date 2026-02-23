#[cfg(feature = "dev-context-only-utils")]
use qualifier_attr::qualifiers;
use {
    crate::banking_stage::scheduler_messages::TransactionId,
    prio_graph::TopLevelId,
    std::{
        cmp::Ordering,
        hash::{Hash, Hasher},
    },
};

/// A unique identifier tied with priority ordering for a transaction/packet:
#[cfg_attr(feature = "dev-context-only-utils", qualifiers(pub))]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct TransactionPriorityId {
    pub(crate) priority: u64,
    pub(crate) id: TransactionId,
}

impl TransactionPriorityId {
    pub(crate) fn new(priority: u64, id: TransactionId) -> Self {
        Self { priority, id }
    }
}

impl Hash for TransactionPriorityId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state)
    }
}

impl Ord for TransactionPriorityId {
    fn cmp(&self, other: &Self) -> Ordering {
        // Higher priority first. For equal priority, older txs first (FIFO).
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.id.cmp(&self.id))
    }
}

impl PartialOrd for TransactionPriorityId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl TopLevelId<Self> for TransactionPriorityId {
    fn id(&self) -> Self {
        *self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_priority_id_ordering() {
        // Higher priority first
        {
            let id1 = TransactionPriorityId::new(1, 1);
            let id2 = TransactionPriorityId::new(2, 1);
            assert!(id1 < id2);
            assert!(id1 <= id2);
            assert!(id2 > id1);
            assert!(id2 >= id1);
        }

        // Equal priority then compare by id (FIFO: older id first)
        {
            let id1 = TransactionPriorityId::new(1, 1);
            let id2 = TransactionPriorityId::new(1, 2);
            assert!(id1 > id2);
            assert!(id1 >= id2);
            assert!(id2 < id1);
            assert!(id2 <= id1);
        }

        // Equal priority and id
        {
            let id1 = TransactionPriorityId::new(1, 1);
            let id2 = TransactionPriorityId::new(1, 1);
            assert_eq!(id1, id2);
            assert!(id1 >= id2);
            assert!(id1 <= id2);
            assert!(id2 >= id1);
            assert!(id2 <= id1);
        }
    }
}
