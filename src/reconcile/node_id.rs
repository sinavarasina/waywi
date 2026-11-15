use crate::input::NodeId;

/// Generational allocator for [`NodeId`]s so queued events referencing removed nodes are ignored
/// safely.
pub(crate) struct NodeIdAllocator {
    slots: Vec<u32>,
    free_indices: Vec<u32>,
}

impl Default for NodeIdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

impl NodeIdAllocator {
    pub(crate) fn new() -> Self {
        Self {
            slots: Vec::new(),
            free_indices: Vec::new(),
        }
    }

    pub(crate) fn alloc(&mut self) -> NodeId {
        if let Some(idx) = self.free_indices.pop() {
            let generation = self.slots[idx as usize];
            NodeId {
                index: idx,
                generation,
            }
        } else {
            let idx = self.slots.len() as u32;
            self.slots.push(1);
            NodeId {
                index: idx,
                generation: 1,
            }
        }
    }

    pub(crate) fn free(&mut self, id: NodeId) {
        if let Some(slot) = self.slots.get_mut(id.index as usize)
            && *slot == id.generation
        {
            *slot = slot.wrapping_add(1).max(1);
            self.free_indices.push(id.index);
        }
    }
}
