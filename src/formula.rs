//! `Formula` stores the result of a parsing as the tree of its "synctactic proof" 
//! The formula nodes are the equivalent of MMJ2's "ParseNode"s, and the formula itself the equivalent of MMJ2's "ParseTree"
//!

// There are several improvements which could be made to this implementation, without changing the API:
// 
// - `sub_formula`:
//      `sub_formula` currently copies the whole array which backs the formula tree, which is very inefficient.
//      This can be improved by making formulas immutable and providing an `Arc` into their slices, so that no copy occurs.
//      This means that while the `FormulaBuilding` part would work with trees backed by a `Vector`, 
//      they would be converted to slices at the end of the building process.
// - `sub_eq`:
//      We could compute a hash of a formula and store it in every node, to speed up equality testing.
// - `sub_eq`:
//      If the formula is backed by an `Arc`, sub-formula equality testing could be improved by testing pointer equality (`ptr_eq`) for the Arc,
//      and equality of the node index. This would tackle cases where we are comparing two formulas which are actually pointing to the same place
//      in the same array. 
// - `substitute`:
//      A more advanced implementation of `substitute` may act directly on the slice backing the formula to
//      first copy in bulk the formula tree, which will remain mostly intact, then the substitutions, 
//      and then only change the nodes where the formula points to the substitutions.
//      It would even be possible to reuse the nodes, pointing several times to the same node if a substituted variable appears several times
//      in the formula to be substituted.

use core::ops::Index;
use crate::parser::as_str;
use crate::parser::SymbolType;
use crate::parser::TokenIter;
use crate::segment_set::SegmentSet;
use crate::nameck::Atom;
use crate::nameck::Nameset;
use crate::tree::Tree;
use crate::tree::NodeId;
use crate::tree::SiblingIter;
use std::sync::Arc;
use crate::bit_set::Bitset;
use crate::util::HashMap;
use crate::util::new_map;

/// An atom representing a typecode (for "set.mm", that's one of 'wff', 'class', 'setvar' or '|-')
pub type TypeCode = Atom;

/// An atom representing a math symbol
pub type Symbol = Atom;

/// An atom representing a label (nameck suggests LAtom for this)
pub type Label = Atom;

/// A set of substitutions, mapping variables to a formula
/// We also could have used `dyn Index<&Label, Output=Box<Formula>>`
pub struct Substitutions(HashMap<Label, Box<Formula>>);

impl Index<&Label> for Substitutions {
    type Output = Box<Formula>;

    #[inline]
    fn index(&self, label: &Label) -> &Self::Output {
        &self.0[label]
    }
}

/// A parsed formula, in a tree format which is convenient to perform unifications
#[derive(Default)]
pub struct Formula {
    typecode: TypeCode,
    tree: Tree<Label>,
    root: NodeId,
    variables: Bitset,
}

impl Formula {
    /// Convert the formula back to a flat list of symbols
    /// This is slow and shall not normally be called except for showing a result to the user.
    pub fn iter<'a>(&'a self, sset: &'a Arc<SegmentSet>, nset: &'a Arc<Nameset>) -> Flatten<'a> {
        let mut f = Flatten {
            formula: self,
            stack: vec![],
            sset,
            nset,
        };
        f.step_into(self.root);
        f
    }

    /// Displays the formula as a string
    pub fn display(&self, sset: &Arc<SegmentSet>, nset: &Arc<Nameset>) -> String {
        let mut str = String::new();
        str.push_str(as_str(nset.atom_name(self.typecode)));
        for symbol in self.iter(sset, nset) {
            str.push(' ');
            str.push_str(as_str(nset.atom_name(symbol)));
        }
        str
    }

    /// Debug only, dumps the internal structure of the formula.
    pub fn dump(&self, nset: &Arc<Nameset>) {
        println!("  Root: {}", self.root);
        self.tree.dump(|atom| as_str(nset.atom_name(*atom)));
    }

    /// Returns the label obtained when following the given path.
    /// Each element of the path gives the index of the child to retrieve.
    /// For example, the empty 
    pub fn get_by_path(&self, path: &[usize]) -> Option<Label> {
        let mut node_id = self.root;
        for index in path {
            node_id = self.tree.nth_child(node_id, *index)?;
        }
        Some(self.tree[node_id])
    }

    #[inline]
    /// Returns whether the node given by `node_id` is a variable.
    fn is_variable(&self, node_id: NodeId) -> bool {
        self.variables.has_bit(node_id)
    }

    /// Returns a subformula, with its root at the given `node_id`
    fn sub_formula(&self, node_id: NodeId) -> Formula {
        Formula {
            typecode: self.typecode, // TODO
            tree: self.tree.clone(),
            root: node_id,
            variables: self.variables.clone(),
        }
    }

    /// Check for equality of sub-formulas
    fn sub_eq(&self, node_id: NodeId, other: &Formula, other_node_id: NodeId) -> bool {
        self.tree[node_id] == other.tree[other_node_id]
            && self.tree.has_children(node_id) == other.tree.has_children(other_node_id)
            && self.tree.children_iter(node_id).zip(other.tree.children_iter(other_node_id)).all(|(s_id, o_id)| { self.sub_eq(s_id, other, o_id) })
    }

    /// Unify this formula with the given formula model
    /// If successful, this returns the substitutions which needs to be made in `other` in order to match this formula.
    pub fn unify(&self, other: &Formula) -> Option<Box<Substitutions>> {
        let mut substitutions = Substitutions(new_map());
        self.sub_unify(self.root, other, other.root, &mut substitutions)?;
        Some(Box::new(substitutions))
    }

    /// Unify a sub-formula
    fn sub_unify(&self, node_id: NodeId, other: &Formula, other_node_id: NodeId, substitutions: &mut Substitutions) -> Option<()> {
        if other.is_variable(other_node_id) {
            // the model formula is a variable, build or match the substitution
            if let Some(formula) = substitutions.0.get(&other.tree[other_node_id]) {
                // there already is as substitution for that variable, check equality
                self.sub_eq(node_id, formula, formula.root).then(|| {})
            } else {
                // store the new substitution and succeed
                substitutions.0.insert(other.tree[other_node_id], Box::new(self.sub_formula(node_id)));
                Some(())
            }
        } else if self.tree[node_id] == other.tree[other_node_id] && self.tree.has_children(node_id) == other.tree.has_children(other_node_id) {
            // same nodes, we compare all children nodes
            for (s_id, o_id) in self.tree.children_iter(node_id).zip(other.tree.children_iter(other_node_id)) {
                self.sub_unify(s_id, other, o_id, substitutions)?;
            }
            Some(())
        } else {
            // formulas differ, we cannot unify.
            None
        }
    }

    /// Perform substitutions
    /// This returns a new `Formula` object, built from this formula, 
    /// where all instances of the variables specified in the substitutions are replaced by the corresponding formulas.
    pub fn substitute(&self, substitutions: &Substitutions) -> Formula {
        let mut formula_builder = FormulaBuilder::default();
        self.sub_substitute(self.root, substitutions, &mut formula_builder);
        formula_builder.build(self.typecode)
    }

    /// Perform substitutions on a sub-formula, starting from the given `node_id`
    // TODO: shall we enforce that *all* variables occurring in this formula have a substitution?
    fn sub_substitute(&self, node_id: NodeId, substitutions: &Substitutions, formula_builder: &mut FormulaBuilder) {
        let mut done = false;
        // TODO use https://rust-lang.github.io/rfcs/2497-if-let-chains.html once it's out!
        if self.is_variable(node_id) {
            if let Some(formula) = substitutions.0.get(&self.tree[node_id]) {
                // We encounter a variable, perform substitution.
                formula.copy_sub_formula(formula.root, formula_builder);
                done = true;
            }
        }
        if !done {
            let mut children_count = 0;
            for child_node_id in self.tree.children_iter(node_id) {
                self.sub_substitute(child_node_id, substitutions, formula_builder);
                children_count += 1;
            }
            formula_builder.reduce(self.tree[node_id], children_count, 0, self.is_variable(node_id));
        }
    }

    // Copy a sub-formula of this formula to a formula builder
    fn copy_sub_formula(&self, node_id: NodeId, formula_builder: &mut FormulaBuilder) {
        let mut children_count = 0;
        for child_node_id in self.tree.children_iter(node_id) {
            self.copy_sub_formula(child_node_id, formula_builder);
            children_count += 1;
        }
        formula_builder.reduce(self.tree[node_id], children_count, 0, self.is_variable(node_id));
    }
}

impl PartialEq for Formula {
    fn eq(&self, other: &Self) -> bool {
        self.sub_eq(self.root, other, other.root)
    }
}

/// An iterator going through each symbol in a formula
pub struct Flatten<'a> {
    formula: &'a Formula,
    stack: Vec<(TokenIter<'a>, Option<SiblingIter<'a, Label>>)>,
    sset: &'a Arc<SegmentSet>, 
    nset: &'a Arc<Nameset>,
}

impl<'a> Flatten<'a> {
    fn step_into(&mut self, node_id: NodeId) {
        let label = self.formula.tree[node_id];
        let sref = self.sset.statement(self.nset.lookup_label(self.nset.atom_name(label)).unwrap().address);
        let mut math_iter = sref.math_iter();
        math_iter.next(); // Always skip the typecode token.
        if self.formula.tree.has_children(node_id) { 
            self.stack.push((math_iter, Some(self.formula.tree.children_iter(node_id))));
        } 
        else {
            self.stack.push((math_iter, None)); 
        };
        
    }
}

impl<'a> Iterator for Flatten<'a> {
    type Item = Symbol;
    
    fn next(&mut self) -> Option<Self::Item> {
        if self.stack.is_empty() { return None; }
        let stack_end = self.stack.len()-1;
        let (ref mut math_iter, ref mut sibling_iter) = self.stack[stack_end];
        if let Some(token) = math_iter.next() {
            // Continue with next token of this syntax
            let symbol = self.nset.lookup_symbol(token.slice).unwrap();
            match (sibling_iter,symbol.stype) {
                (_, SymbolType::Constant) | (None, SymbolType::Variable) => Some(symbol.atom),
                (Some(ref mut iter), SymbolType::Variable) => {
                    // Variable : push into the next child
                    if let Some(next_child_id) = iter.next() {
                        self.step_into(next_child_id);
                        self.next()
                    } else {
                        panic!("Empty formula!");
                    }
                },
            }
        } else {
            // End of this formula, pop to the parent one
            self.stack.pop();
            self.next()
        }
    }

    // TODO provide an implementation for size_hint?
}

#[derive(Default)]
pub(crate) struct FormulaBuilder {
    stack: Vec<NodeId>,
    formula: Formula,
}

/// A utility to build a formula. 
impl FormulaBuilder {
    /// Every REDUCE pops `var_count` subformula items on the stack, 
    /// and pushes one single new item, with the popped subformulas as children
    pub(crate) fn reduce(&mut self, label: Label, var_count: u8, offset: u8, is_variable: bool) {
        assert!(self.stack.len()>=(var_count + offset).into());
        let reduce_start = self.stack.len().saturating_sub((var_count + offset).into());
        let reduce_end = self.stack.len().saturating_sub(offset.into());
        let new_node_id = {
            let children = self.stack.drain(reduce_start..reduce_end);
            self.formula.tree.add_node(label, children.as_slice())
        };
        if is_variable { self.formula.variables.set_bit(new_node_id); }
        self.stack.insert(reduce_start,new_node_id);
    }

    pub(crate) fn build(mut self, typecode: TypeCode) -> Formula {
        // Only one entry shall remain in the stack at the time of building, the formula root.
        assert!(self.stack.len() == 1, "Final formula building state does not have one root - {:?}", self.stack); 
        self.formula.root = self.stack[0];
        self.formula.typecode = typecode;
        self.formula
    }
}