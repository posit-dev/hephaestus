//! Pick scopes: the logical tree a drawing sits in, recorded alongside the
//! geometry so a hit can say *what* it hit and not merely *which id*.
//!
//! A scope is pushed and popped like a layer, but has no visual effect and
//! imposes no clip. The stack in effect when a primitive is drawn becomes
//! that primitive's ancestor chain, so the stack *is* the bubble path.
//!
//! Nothing here knows what a chart is. A scope carries a `&'static str`
//! kind and two optional fields, and the vocabulary that gives them meaning
//! lives in [`crate::plot::pick`] — the same split the composition module
//! already uses between `Slot::name` and the `Region` trait.

use std::collections::HashMap;
use std::sync::Arc;

/// Whether primitives drawn directly inside a scope are pick targets in
/// their own right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ScopeMode {
    /// A grouping frame. A primitive carrying [`PickId::Skip`] stays
    /// unindexed, exactly as it was before scopes existed. What structural
    /// frames and geoms use.
    ///
    /// [`PickId::Skip`]: crate::pick::PickId::Skip
    #[default]
    Group,
    /// The scope *is* the target. A primitive drawn directly inside it is
    /// indexed whatever its [`PickId`], and reported against this path.
    /// What chrome uses, since chrome has no id of its own.
    ///
    /// [`PickId`]: crate::pick::PickId
    Target,
}

/// One node of the logical tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PickScope {
    kind: &'static str,
    name: Option<Arc<str>>,
    index: Option<u32>,
    mode: ScopeMode,
}

impl PickScope {
    /// A grouping frame — see [`ScopeMode::Group`].
    pub fn group(kind: &'static str) -> Self {
        Self {
            kind,
            name: None,
            index: None,
            mode: ScopeMode::Group,
        }
    }

    /// A frame that is itself a pick target — see [`ScopeMode::Target`].
    pub fn target(kind: &'static str) -> Self {
        Self {
            kind,
            name: None,
            index: None,
            mode: ScopeMode::Target,
        }
    }

    /// Attach a name — a patch id, a scale name, a region name.
    pub fn with_name(mut self, name: impl Into<Arc<str>>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Attach an ordinal — a break index, a legend row, a channel.
    pub fn with_index(mut self, index: u32) -> Self {
        self.index = Some(index);
        self
    }

    /// What kind of node this is. Interpreted by the authoring layer.
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The attached name, if any.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The attached ordinal, if any.
    pub fn index(&self) -> Option<u32> {
        self.index
    }

    /// Whether primitives drawn directly here are targets themselves.
    pub fn mode(&self) -> ScopeMode {
        self.mode
    }
}

/// Index of a node in a [`ScopeTree`]. [`ScopeNode::ROOT`] is the empty path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ScopeNode(u32);

impl ScopeNode {
    /// The empty path — nothing pushed.
    pub(crate) const ROOT: ScopeNode = ScopeNode(u32::MAX);

    pub(crate) fn is_root(self) -> bool {
        self == ScopeNode::ROOT
    }
}

/// Hash-consed tree of scope paths.
///
/// Every indexed primitive stores one [`ScopeNode`], four bytes, rather than
/// its own copy of the chain. A frame draws hundreds of distinct paths and
/// can draw hundreds of thousands of primitives, so sharing is the
/// difference between the scope stack costing nothing and costing more than
/// the geometry.
#[derive(Debug, Default)]
pub(crate) struct ScopeTree {
    nodes: Vec<(ScopeNode, PickScope)>,
    intern: HashMap<(ScopeNode, PickScope), u32>,
    stack: Vec<ScopeNode>,
}

impl ScopeTree {
    /// The path in effect for a primitive recorded right now.
    pub(crate) fn current(&self) -> ScopeNode {
        self.stack.last().copied().unwrap_or(ScopeNode::ROOT)
    }

    /// Whether the innermost scope makes its primitives targets.
    pub(crate) fn current_is_target(&self) -> bool {
        let node = self.current();
        !node.is_root() && self.nodes[node.0 as usize].1.mode() == ScopeMode::Target
    }

    /// Enter `scope`, reusing the node if this exact path has been walked
    /// before — which it will have been, since the five draw phases each
    /// re-establish the same `composition → plot` prefix.
    pub(crate) fn push(&mut self, scope: &PickScope) {
        let parent = self.current();
        let key = (parent, scope.clone());
        let node = match self.intern.get(&key) {
            Some(&existing) => ScopeNode(existing),
            None => {
                self.nodes.push((parent, scope.clone()));
                let id = self.nodes.len() as u32 - 1;
                self.intern.insert(key, id);
                ScopeNode(id)
            }
        };
        self.stack.push(node);
    }

    /// Leave the innermost scope. Unbalanced pops are ignored, for the same
    /// reason unbalanced clip pops are: a malformed scene must not panic a
    /// hover.
    pub(crate) fn pop(&mut self) {
        self.stack.pop();
    }

    /// Forget everything. Called at the frame boundary.
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
        self.intern.clear();
        self.stack.clear();
    }

    /// Number of distinct paths interned.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.nodes.len()
    }

    fn parent_of(&self, node: ScopeNode) -> ScopeNode {
        self.nodes[node.0 as usize].0
    }

    fn scope_of(&self, node: ScopeNode) -> &PickScope {
        &self.nodes[node.0 as usize].1
    }
}

/// The ancestor chain of a hit.
#[derive(Debug, Clone, Copy)]
pub struct PickPath<'a> {
    tree: &'a ScopeTree,
    node: ScopeNode,
}

impl<'a> PickPath<'a> {
    pub(crate) fn new(tree: &'a ScopeTree, node: ScopeNode) -> Self {
        Self { tree, node }
    }

    /// Whether the hit sits in no scope at all.
    pub fn is_empty(&self) -> bool {
        self.node.is_root()
    }

    /// Depth of the chain.
    pub fn len(&self) -> usize {
        self.bubble().count()
    }

    /// The chain innermost first — the order an event bubbles outward.
    pub fn bubble(&self) -> impl Iterator<Item = &'a PickScope> + '_ {
        let mut node = self.node;
        std::iter::from_fn(move || {
            if node.is_root() {
                return None;
            }
            let scope = self.tree.scope_of(node);
            node = self.tree.parent_of(node);
            Some(scope)
        })
    }

    /// The chain outermost first — the order it was captured in.
    ///
    /// Allocates, unlike [`Self::bubble`]: the chain is stored child-to-parent
    /// and has to be reversed. Depth is a handful of frames.
    pub fn frames(&self) -> Vec<&'a PickScope> {
        let mut v: Vec<&PickScope> = self.bubble().collect();
        v.reverse();
        v
    }

    /// The innermost frame of the given kind, searching outward.
    pub fn find(&self, kind: &str) -> Option<&'a PickScope> {
        self.bubble().find(|s| s.kind() == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_the_same_path_twice_reuses_one_node() {
        let mut t = ScopeTree::default();
        let comp = PickScope::group("composition").with_name("root");
        let plot = PickScope::group("plot").with_name("a").with_index(0);

        t.push(&comp);
        t.push(&plot);
        let first = t.current();
        t.pop();
        t.pop();

        // The five draw phases each re-establish this prefix.
        t.push(&comp);
        t.push(&plot);
        assert_eq!(t.current(), first);
        assert_eq!(t.len(), 2, "re-walking must not add nodes");
    }

    #[test]
    fn scopes_differing_in_any_field_are_distinct_nodes() {
        let mut t = ScopeTree::default();
        t.push(&PickScope::group("plot").with_name("a").with_index(0));
        t.pop();
        t.push(&PickScope::group("plot").with_name("a").with_index(1));
        t.pop();
        t.push(&PickScope::group("plot").with_name("b").with_index(0));
        t.pop();
        t.push(&PickScope::target("plot").with_name("a").with_index(0));
        t.pop();
        assert_eq!(t.len(), 4);
    }

    #[test]
    fn a_path_reads_outward_and_inward() {
        let mut t = ScopeTree::default();
        t.push(&PickScope::group("composition"));
        t.push(&PickScope::group("plot").with_name("a"));
        t.push(&PickScope::target("part").with_name("axis_tick_label"));
        t.push(&PickScope::target("item").with_index(3));
        let node = t.current();

        let path = PickPath::new(&t, node);
        assert_eq!(path.len(), 4);
        assert!(!path.is_empty());

        let outward: Vec<&str> = path.frames().iter().map(|s| s.kind()).collect();
        assert_eq!(outward, vec!["composition", "plot", "part", "item"]);

        let inward: Vec<&str> = path.bubble().map(|s| s.kind()).collect();
        assert_eq!(inward, vec!["item", "part", "plot", "composition"]);

        assert_eq!(path.find("plot").and_then(|s| s.name()), Some("a"));
        assert_eq!(path.find("item").and_then(|s| s.index()), Some(3));
        assert!(path.find("legend").is_none());
    }

    #[test]
    fn the_empty_path_has_no_frames() {
        let t = ScopeTree::default();
        let path = PickPath::new(&t, ScopeNode::ROOT);
        assert!(path.is_empty());
        assert_eq!(path.len(), 0);
        assert!(path.frames().is_empty());
        assert!(path.find("plot").is_none());
    }

    #[test]
    fn target_mode_is_read_from_the_innermost_frame_only() {
        let mut t = ScopeTree::default();
        assert!(!t.current_is_target(), "nothing pushed is not a target");
        t.push(&PickScope::group("plot"));
        assert!(!t.current_is_target());
        t.push(&PickScope::target("part"));
        assert!(t.current_is_target());
        // A group nested inside a target is not itself a target.
        t.push(&PickScope::group("inner"));
        assert!(!t.current_is_target());
        t.pop();
        assert!(t.current_is_target());
    }

    #[test]
    fn an_unbalanced_pop_does_not_panic() {
        let mut t = ScopeTree::default();
        t.pop();
        t.pop();
        assert!(t.current().is_root());
        t.push(&PickScope::group("plot"));
        assert!(!t.current().is_root());
    }
}
