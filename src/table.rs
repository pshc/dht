/// Subdividable data structure that holds known nodes.

use std::fmt::{self, Debug, Formatter};
use std::mem;

use messages::{NODE_ID_LEN, NodeId};

/// XOR-based distance metric for `NodeId`s.
#[derive(Copy, Clone, Eq, Ord, PartialEq, PartialOrd)]
pub struct Distance([u8; NODE_ID_LEN]);

impl Distance {
    pub fn between(a: &NodeId, b: &NodeId) -> Self {
        let mut dist = [0u8; NODE_ID_LEN];
        for i in 0..NODE_ID_LEN {
            dist[i] = a.0[i] ^ b.0[i];
        }
        Distance(dist)
    }

    pub fn count_zeros(&self) -> usize {
        for i in 0..NODE_ID_LEN {
            let bits = self.0[i];
            if bits == 0 {
                continue
            }
            // zeros == i*8 + number of zero bits in `bits`
            let mut mask = 0xf0;
            for n_extra_zeros in 0..8 {
                if (mask & bits) != 0 {
                    return i * 8 + n_extra_zeros;
                }
                mask >>= 1;
            }
            debug_assert_eq!(mask, 1);
        }
        debug_assert_eq!(self.0, [0u8; NODE_ID_LEN]);
        return NODE_ID_LEN * 8;
    }
}

const MAX_BUCKETS: usize = NODE_ID_LEN * 8 + 1;

/// Stores known nodes, bucketing them based on their "distance" from us.
pub struct Table {
    /// Largest buckets first; when a bucket reaches capacity, it overflows to the next.
    buckets: Vec<Bucket>,
    /// Our ID; used for the distance metric.
    id: NodeId,
}

impl Debug for Table {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        if self.buckets.is_empty() {
            return write!(f, "Table (empty)")
        }
        write!(f, "Table ({} buckets) {{", self.buckets.len())?;
        for (i, bucket) in self.buckets.iter().enumerate() {
            write!(f, "\n{:03}: {:?}", i, bucket)?
        }
        write!(f, "\n}}")
    }
}

/// Number of slots per bucket.
const K: usize = 8;

struct Bucket {
    /// Most recently "good" nodes first.
    slots: [Slot; K],
}

impl Bucket {
    fn new() -> Self {
        Bucket {slots: [Slot::Empty; K]}
    }

    /// Finds the given ID, or assigns an empty slot.
    fn locate(&self, id: &NodeId) -> Option<usize> {
        for (i, slot) in self.slots.iter().enumerate() {
            let found = match *slot {
                Slot::Empty => true,
                Slot::Node(ref slot_id, _) => id == slot_id,
            };
            if found {
                return Some(i)
            }
        }
        None
    }
}

impl Debug for Bucket {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        write!(f, "[")?;
        for slot in &self.slots {
            write!(f, "{:?},", slot)?
        }
        write!(f, "]")
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Slot {
    Empty,
    Node(NodeId, NodeState),
}

impl Slot {
    pub fn is_empty(&self) -> bool {
        match *self {
            Slot::Empty => true,
            Slot::Node(..) => false,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum NodeState {
    Pinging,
    Good,
}

impl Table {
    pub fn new(id: NodeId) -> Self {
        Table {
            buckets: vec![Bucket::new()],
            id: id,
        }
    }

    pub fn our_id(&self) -> &NodeId {
        &self.id
    }

    /// Finds and returns an appropriate `Slot` for `node_id`.
    ///
    /// If it already existed, returns the existing entry.
    /// May spill a new bucket as needed.
    pub fn allocate<'a>(&'a mut self, node_id: &NodeId) -> Option<&'a mut Slot> {
        let distance = Distance::between(&self.id, &node_id);
        let common_bits = distance.count_zeros() as usize;
        let n = self.buckets.len();

        if common_bits < n {
            if let Some(i) = self.buckets[common_bits].locate(node_id) {
                return Some(&mut self.buckets[common_bits].slots[i])
            }
        }

        if common_bits >= n && n < MAX_BUCKETS {
            self.spill()
        } else {
            None
        }
    }

    /// Push a new bucket, and spill entries from the previous bucket into it as appropriate.
    ///
    /// Returns the next open slot in the new bucket.
    fn spill(&mut self) -> Option<&mut Slot> {
        assert!(!self.buckets.is_empty());
        assert!(self.buckets.len() < MAX_BUCKETS);

        let bit_index = self.buckets.len() - 1;
        let our_bit = self.id.bit(bit_index);

        // our new bucket & current insertion index
        let mut dest_bucket = Bucket::new();
        let mut dest_slot = 0;
        // tracks the old (source) bucket's empty slot of lowest index; for compaction
        let mut gap = None;
        {
            // rifle through the source bucket's slots, spilling and moving as needed
            let ref mut src_bucket = self.buckets[bit_index];
            for src in 0..K {
                // `before` contains `gap` if present, and `remaining[0]` is the current Slot
                let (before, remaining) = src_bucket.slots.split_at_mut(src);
                let ref mut src_slot = remaining[0];

                match *src_slot {
                    Slot::Empty => {
                        if gap.is_none() {
                            gap = Some(src)
                        }
                    }
                    // unnecessary copy of `id` here?
                    Slot::Node(id, _) if our_bit == id.bit(bit_index) => {
                        // spill it!
                        dest_bucket.slots[dest_slot] = *src_slot;
                        dest_slot += 1;
                        // this slot is now considered empty, so track it as such
                        *src_slot = Slot::Empty;
                        if gap.is_none() {
                            gap = Some(src)
                        }
                    }
                    Slot::Node(_, _) => {
                        // this slot will stay behind in the old bucket
                        if let Some(g) = gap {
                            // move this node up to fill the gap
                            mem::swap(&mut before[g], src_slot);
                            debug_assert!(src_slot.is_empty());
                            gap = Some(src);
                            // now there may be an earlier gap
                            for i in g+1..src {
                                if before[i].is_empty() {
                                    gap = Some(i);
                                    break
                                }
                            }
                        }
                    }
                }
            }
        }
        // now that we've spilled into our new bucket, push it
        let bucket_index = self.buckets.len();
        self.buckets.push(dest_bucket);
        if dest_slot < K {
            Some(&mut self.buckets.get_mut(bucket_index).unwrap().slots[dest_slot])
        } else {
            None // new bucket already completely full
        }
    }
}
