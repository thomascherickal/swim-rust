#[cfg(test)]
mod tests;

use smol_str::SmolStr;
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::iter::Peekable;

static_assertions::assert_impl_all!(UriForest<()>: Send, Sync);

/// A trie-like data structure mapping URIs to an associated value. This struct offers operations
/// for inserting a URI and associating data alongside it, removing URIs and querying all the
/// available URIs or by a prefix.
///
/// While the URI forest is not as time efficient for insertion and removal operations as a map, it
/// is more efficient prefix lookups; such as finding all URIs prefixed by "/host/".
///
/// # Internal representation:
/// From running the following:
/// ```
/// use swim_uri_forest::UriForest;
/// let mut forest = UriForest::new();
///
/// forest.insert("/listener", ());
/// forest.insert("/unit/1/cnt/2", ());
/// forest.insert("/unit/2/cnt/3", ());
/// ```
///
/// The internal representation of the URIs in the struct is is:
/// ```ignore
///                     /
///             listener    unit
///                             \
///                         1       2
///                     /           /
///                 cnt         cnt
///             /                     \
///         2                              3
/// ```
///
#[derive(Debug)]
pub struct UriForest<D> {
    /// A collection of trees in this forest.
    trees: HashMap<SmolStr, TreeNode<D>>,
}

impl<D> Default for UriForest<D> {
    fn default() -> Self {
        UriForest {
            trees: HashMap::default(),
        }
    }
}

impl<D> UriForest<D> {
    /// Constructs a new URI forest.
    pub fn new() -> UriForest<D> {
        UriForest {
            trees: HashMap::new(),
        }
    }

    /// Inserts 'uri' into this forest and associates 'node_data' with it.
    pub fn insert(&mut self, uri: &str, node_data: D) {
        let UriForest { trees } = self;
        let mut segment_iter = PathSegmentIterator::new(uri).peekable();

        if let Some(segment) = segment_iter.next() {
            match trees.get_mut(segment) {
                Some(root) => {
                    // A tree exists in the forest so traverse it until we find where to start
                    // inserting segments
                    traverse_insert(segment, root, segment_iter, node_data)
                }
                None => {
                    // No tree exists, build a new one
                    if segment_iter.peek().is_some() {
                        let node = trees.entry(segment.into()).or_insert(TreeNode::new(None));
                        insert_uri(segment_iter, node, node_data);
                    } else {
                        trees.insert(segment.into(), TreeNode::new(Some(node_data)));
                    }
                }
            }
        }
    }

    /// Attempts to remove 'uri' from this forest, returning any associated data.
    pub fn remove(&mut self, uri: &str) -> Option<D> {
        let UriForest { trees } = self;
        let mut segment_iter = PathSegmentIterator::new(uri).peekable();

        match segment_iter.next() {
            Some(segment) => {
                let data = match trees.get_mut(segment) {
                    Some(root) => {
                        if root.has_descendants() {
                            // The node has descendants that need to be traversed
                            let data = traverse_remove(root, segment_iter);
                            if !root.has_data() && !root.has_descendants() {
                                data
                            } else {
                                return data;
                            }
                        } else {
                            // The node has no descendants so remove it directly
                            None
                        }
                    }
                    None => return None,
                };

                match data {
                    Some(data) => {
                        trees.remove(segment);
                        Some(data)
                    }
                    None => trees.remove(segment)?.data,
                }
            }
            None => None,
        }
    }

    /// Returns an optional reference to the data associated at 'uri'
    pub fn get(&self, uri: &str) -> Option<&D> {
        let UriForest { trees } = self;
        let mut segment_iter = PathSegmentIterator::new(uri).peekable();

        match segment_iter.next() {
            Some(segment) => {
                match trees.get(segment) {
                    Some(root) => {
                        let mut current_node = root;
                        while let Some(segment) = segment_iter.next() {
                            match (current_node.get_descendant(segment), segment_iter.peek()) {
                                (Some(descendant), Some(_)) => {
                                    // We found a matching node and there is another segment to come in
                                    // the path. Update the cursor and carry on.
                                    current_node = descendant;
                                }
                                (Some(node), None) => return node.data.as_ref(),
                                (None, Some(_)) => return None,
                                (None, None) => return None,
                            }
                        }
                        current_node.data.as_ref()
                    }
                    None => return None,
                }
            }
            None => return None,
        }
    }

    /// Returns an optional mutable reference to the data associated at 'uri'
    pub fn get_mut(&mut self, uri: &str) -> Option<&mut D> {
        let UriForest { trees } = self;
        let mut segment_iter = PathSegmentIterator::new(uri).peekable();

        match segment_iter.next() {
            Some(segment) => {
                match trees.get_mut(segment) {
                    Some(root) => {
                        let mut current_node = root;
                        while let Some(segment) = segment_iter.next() {
                            match (
                                current_node.get_descendant_mut(segment),
                                segment_iter.peek(),
                            ) {
                                (Some(descendant), Some(_)) => {
                                    // We found a matching node and there is another segment to come in
                                    // the path. Update the cursor and carry on.
                                    current_node = descendant;
                                }
                                (Some(node), None) => return node.data.as_mut(),
                                (None, Some(_)) => return None,
                                (None, None) => return None,
                            }
                        }
                        current_node.data.as_mut()
                    }
                    None => return None,
                }
            }
            None => return None,
        }
    }

    /// Returns whether this URI forest contains 'uri'.
    pub fn contains_uri(&self, uri: &str) -> bool {
        let UriForest { trees } = self;
        let mut segment_iter = PathSegmentIterator::new(uri).peekable();

        match segment_iter.next() {
            Some(segment) => {
                match trees.get(segment) {
                    Some(root) => {
                        let mut current_node = root;
                        while let Some(segment) = segment_iter.next() {
                            match (current_node.get_descendant(segment), segment_iter.peek()) {
                                (Some(descendant), Some(_)) => {
                                    // We found a matching node and there is another segment to come in
                                    // the path. Update the cursor and carry on.
                                    current_node = descendant;
                                }
                                (Some(node), None) => {
                                    // We found a matching node but we *only* want to return true iff
                                    // the node has data
                                    return node.has_data();
                                }
                                (None, Some(_)) => {
                                    return false;
                                }
                                (None, None) => {
                                    return false;
                                }
                            }
                        }
                        // This will be reached if the URI only has one segment in its path. We *only*
                        // want to return true iff the node has data.
                        current_node.has_data()
                    }
                    None => {
                        // No tree exists
                        false
                    }
                }
            }
            None => false,
        }
    }

    /// Returns an iterator that will yield every URI in the forest.
    pub fn uri_iter(&self) -> UriForestIterator<'_, D> {
        let UriForest { trees } = self;
        UriForestIterator::new("".to_string(), trees)
    }

    /// Returns all URIs in this URI forest that are prefixed by 'uri'.
    pub fn prefix_iter(&self, uri: &str) -> UriForestIterator<'_, D> {
        let UriForest { trees } = self;
        let mut segment_iter = PathSegmentIterator::new(uri).peekable();

        match segment_iter.next() {
            Some(segment) => {
                match trees.get(segment) {
                    Some(root) => {
                        let mut current_node = root;
                        while let Some(segment) = segment_iter.next() {
                            match (current_node.get_descendant(segment), segment_iter.peek()) {
                                (Some(descendant), Some(_)) => {
                                    // We found a matching node and there is another segment to come
                                    // in the path. Update the cursor and carry on until we reach
                                    // the end of the prefix
                                    current_node = descendant;
                                }
                                (Some(node), None) => {
                                    return UriForestIterator::new(
                                        uri.to_string(),
                                        &node.descendants,
                                    )
                                }
                                (None, Some(_)) => return UriForestIterator::empty(),
                                (None, None) => return UriForestIterator::empty(),
                            }
                        }

                        return UriForestIterator::new(uri.to_string(), &current_node.descendants);
                    }
                    None => UriForestIterator::empty(),
                }
            }
            None => UriForestIterator::empty(),
        }
    }

    /// Returns whether this URI forest is empty.
    pub fn is_empty(&self) -> bool {
        self.trees.is_empty()
    }
}

fn traverse_remove<'l, D, I>(
    current_node: &mut TreeNode<D>,
    mut segment_iter: Peekable<I>,
) -> Option<D>
where
    I: Iterator<Item = &'l str>,
{
    // Scan down the tree with two cursors. One for the current node and one for the next segment
    // in the URI
    match segment_iter.next() {
        Some(segment) => {
            // Does the current segment exist in the tree?
            return match current_node.get_descendant_mut(segment) {
                // It does. Scan ahead to see if there is another segment in the URI
                Some(descendant) => match segment_iter.peek() {
                    // There is another segment in the URI. We will recursively call ourself if the
                    // next segment exists in the URI or we will return None if it does not
                    Some(next_segment) => {
                        if descendant.has_descendant(next_segment) {
                            // We've made as much progress as we can in this iteration. Recurse
                            let data = traverse_remove(descendant, segment_iter);

                            if !descendant.has_descendants() && !current_node.has_data() {
                                // We want to prune the current node from the tree iff it does not
                                // have any data associated with it and it has no descendants
                                current_node.remove_descendant(segment);
                            }

                            data
                        } else {
                            // The requested node does not exist in the tree
                            None
                        }
                    }
                    // We've reached the end of the URI
                    None => {
                        // This is a junction node so we cannot remove it
                        if descendant.has_descendants() {
                            descendant.take_data()
                        } else {
                            // This is a leaf node, remove it and return the data
                            current_node
                                .remove_descendant(segment)
                                .expect("Missing node")
                                .data
                        }
                    }
                },
                None => {
                    // The requested node does not exist in the tree
                    None
                }
            };
        }
        None => None,
    }
}

fn traverse_insert<'l, D, I>(
    current_segment: &str,
    current_node: &mut TreeNode<D>,
    mut segment_iter: Peekable<I>,
    node_data: D,
) where
    I: Iterator<Item = &'l str>,
{
    if let Some(segment) = segment_iter.next() {
        match current_node.get_descendant_mut(segment) {
            Some(descendant) => {
                if descendant.has_descendants() {
                    if segment_iter.peek().is_some() {
                        traverse_insert(segment, descendant, segment_iter, node_data)
                    } else {
                        // There aren't any more segments in the URI and the descendant node matches
                        // the segment, update the data
                        descendant.update_data(node_data);
                    }
                } else {
                    if segment_iter.peek().is_none() {
                        // There aren't any more segments in the URI and the descendant node matches
                        // the segment, update the data
                        descendant.update_data(node_data);
                    }
                }
            }
            None => {
                if current_segment == segment {
                    // The current node matches the segment, update the data
                    current_node.update_data(node_data);
                } else if segment_iter.peek().is_none() {
                    // There's no more segments left so insert a new node
                    current_node.add_descendant(segment, TreeNode::new(Some(node_data)));
                } else {
                    // We've reached a leaf. Insert the current node and then write the remaining
                    // URI segments from it
                    let current_node = current_node.add_descendant(segment, TreeNode::new(None));
                    insert_uri(segment_iter, current_node, node_data);
                }
            }
        }
    }
}

fn insert_uri<'l, I, D>(segment_iter: I, mut node: &mut TreeNode<D>, node_data: D)
where
    I: Iterator<Item = &'l str>,
{
    let mut segment_iter = segment_iter.peekable();
    loop {
        match (segment_iter.next(), segment_iter.peek().is_some()) {
            (Some(segment), false) => {
                // There are no more segments remaining, write a leaf node
                node.add_descendant(segment, TreeNode::new(Some(node_data)));
                return;
            }
            (Some(segment), true) => {
                // There are more segments remaining
                node = node.add_descendant(segment, TreeNode::new(None));
            }
            (None, _) => {
                // Unreachable when this function is called with more than one segment in the
                // iterator but it's possible that this function will be called with none
                return;
            }
        }
    }
}

#[derive(Debug, PartialEq)]
struct TreeNode<D> {
    data: Option<D>,
    descendants: HashMap<SmolStr, TreeNode<D>>,
}

impl<D> TreeNode<D> {
    fn new(data: Option<D>) -> TreeNode<D> {
        TreeNode {
            data,
            descendants: HashMap::new(),
        }
    }

    fn update_data(&mut self, data: D) {
        self.data = Some(data);
    }

    fn take_data(&mut self) -> Option<D> {
        self.data.take()
    }

    fn has_data(&self) -> bool {
        self.data.is_some()
    }

    fn get_descendant_mut(&mut self, segment: &str) -> Option<&mut TreeNode<D>> {
        self.descendants.get_mut(segment)
    }

    fn get_descendant(&self, segment: &str) -> Option<&TreeNode<D>> {
        self.descendants.get(segment)
    }

    fn has_descendant(&mut self, segment: &str) -> bool {
        self.descendants.contains_key(segment)
    }

    fn add_descendant(&mut self, segment: &str, node: TreeNode<D>) -> &mut TreeNode<D> {
        self.descendants.entry(segment.into()).or_insert(node)
    }

    fn has_descendants(&self) -> bool {
        !self.descendants.is_empty()
    }

    fn remove_descendant(&mut self, segment: &str) -> Option<TreeNode<D>> {
        self.descendants.remove(segment)
    }
}

struct PathSegmentIterator<'a> {
    path: &'a str,
}

impl<'a> PathSegmentIterator<'a> {
    fn new(mut path: &'a str) -> PathSegmentIterator<'a> {
        while path.starts_with('/') {
            path = &path[1..];
        }

        PathSegmentIterator { path }
    }
}

impl<'a> Iterator for PathSegmentIterator<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let PathSegmentIterator { path } = self;

        let lower = path.find(|c| c != '/')?;
        let upper = path[lower..]
            .find('/')
            .map_or(path.len(), |next_slash| lower + next_slash);

        let segment = Some(&path[lower..upper]);

        *path = &path[upper..];

        segment
    }
}

pub struct UriForestIterator<'l, D> {
    /// A prefix that is prepended to each yielded path.
    prefix: String,
    /// A stack of nodes to visit.
    visit: VecDeque<(&'l SmolStr, &'l TreeNode<D>)>,
    /// A stack containing the current path that is being built.
    uri_stack: VecDeque<String>,
    /// A stack of searches that are being performed and a cursor signalling the depth.
    op_stack: VecDeque<usize>,
}

impl<'l, D> UriForestIterator<'l, D> {
    fn empty() -> UriForestIterator<'l, D> {
        UriForestIterator {
            prefix: "".to_string(),
            visit: VecDeque::default(),
            uri_stack: VecDeque::default(),
            op_stack: VecDeque::default(),
        }
    }

    fn new(prefix: String, nodes: &'l HashMap<SmolStr, TreeNode<D>>) -> UriForestIterator<'l, D> {
        UriForestIterator {
            prefix,
            visit: VecDeque::from_iter(nodes),
            uri_stack: VecDeque::default(),
            op_stack: VecDeque::new(),
        }
    }
}

impl<'l, D> Iterator for UriForestIterator<'l, D> {
    type Item = String;

    /// Performs a depth-first search of the tree, yielding every node that contains data (signals
    /// the end of a path).
    fn next(&mut self) -> Option<Self::Item> {
        let UriForestIterator {
            prefix,
            visit,
            uri_stack,
            op_stack,
        } = self;

        loop {
            if visit.is_empty() {
                assert!(op_stack.is_empty());
                return None;
            }

            while let Some((current_segment, node)) = visit.pop_front() {
                uri_stack.push_back(current_segment.to_string());
                op_stack.push_front(node.descendants.len());

                let ret = if node.has_data() {
                    let suffix = uri_stack.iter().cloned().collect::<Vec<String>>().join("/");
                    Some(format!("{}/{}", prefix, suffix))
                } else {
                    None
                };

                if node.has_descendants() {
                    // Insert the next collection of nodes to search
                    for (key, descendant) in &node.descendants {
                        visit.push_front((key, descendant));
                    }
                } else {
                    // Drains any path segments that are no longer required.
                    while let Some(remaining) = op_stack.front_mut() {
                        if *remaining > 0 {
                            *remaining -= 1;
                            // This segment is now complete. We want to update decrement
                            // indices in the callstack and remove any unrequired nodes.
                            if *remaining == 0 {
                                uri_stack.pop_back();
                                op_stack.pop_front();
                            } else {
                                // This node is going to be used as part of another path.
                                break;
                            }
                        } else {
                            // This callstack was the only route to the node (i.e, it had no other
                            // children) so it can be removed

                            uri_stack.pop_back();
                            op_stack.pop_front();
                        }
                    }
                }

                if let Some(ret) = ret {
                    return Some(ret);
                }
            }
        }
    }
}
