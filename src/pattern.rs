use std::fmt;

use itertools::Itertools;
use log::*;
use smallvec::{smallvec, SmallVec};
use symbolic_expressions::Sexp;

use crate::{Applier, EGraph, ENode, Id, Language, Metadata, QuestionMarkName, RecExpr, Searcher};

#[derive(Debug, PartialEq, Clone)]
pub enum Pattern<L> {
    ENode(Box<ENode<L, Pattern<L>>>),
    Wildcard(QuestionMarkName, WildcardKind),
}

#[derive(Debug, PartialEq, Clone, Copy, Hash)]
pub enum WildcardKind {
    Single,
    ZeroOrMore,
}

impl<L: Language> Pattern<L> {
    pub fn from_expr(e: &RecExpr<L>) -> Self {
        Pattern::ENode(
            e.as_ref()
                .map_children(|child| Pattern::from_expr(&child))
                .into(),
        )
    }

    pub fn to_expr(&self) -> Result<RecExpr<L>, String> {
        match self {
            Pattern::ENode(e) => Ok(e.map_children_result(|p| p.to_expr())?.into()),
            Pattern::Wildcard(w, _) => {
                let msg = format!("Found wildcard {:?} instead of expr term", w);
                Err(msg)
            }
        }
    }

    pub fn is_multi_wildcard(&self) -> bool {
        match self {
            Pattern::Wildcard(_, WildcardKind::ZeroOrMore) => true,
            _ => false,
        }
    }

    pub fn subst_and_find<M>(&self, egraph: &mut EGraph<L, M>, mapping: &WildMap) -> Id
    where
        M: Metadata<L>,
    {
        match self {
            Pattern::Wildcard(w, kind) => {
                assert_eq!(*kind, WildcardKind::Single);
                mapping.get(w, *kind).unwrap()[0]
            }
            Pattern::ENode(expr) => {
                let expr = expr.map_children(|pat| pat.subst_and_find(egraph, mapping));
                egraph.add(expr)
            }
        }
    }

    // pub(crate) fn insert_wildcards(&self, set: &mut IndexSet<QuestionMarkName>) {
    //     match self {
    //         Pattern::Wildcard(w, _) => {
    //             set.insert(w.clone());
    //         }
    //         Pattern::ENode(expr) => {
    //             expr.map_children(|pat| pat.insert_wildcards(set));
    //         }
    //     }
    // }

    // pub(crate) fn is_bound(&self, set: &IndexSet<QuestionMarkName>) -> bool {
    //     match self {
    //         Pattern::Wildcard(w, _) => set.contains(w),
    //         Pattern::ENode(e) => e.children.iter().all(|p| p.is_bound(set)),
    //     }
    // }
}

impl<L: Language + fmt::Display> Pattern<L> {
    pub fn to_sexp(&self) -> Sexp {
        match self {
            Pattern::Wildcard(w, _) => Sexp::String(w.to_string()),
            Pattern::ENode(e) => match e.children.len() {
                0 => Sexp::String(e.op.to_string()),
                _ => {
                    let mut vec: Vec<_> = e.children.iter().map(Self::to_sexp).collect();
                    vec.insert(0, Sexp::String(e.op.to_string()));
                    Sexp::List(vec)
                }
            },
        }
    }
}

#[derive(Debug)]
pub struct SearchMatches {
    pub eclass: Id,
    pub mappings: Vec<WildMap>,
}

#[derive(Debug, Clone, PartialEq, Hash)]
pub struct WildMap {
    vec: SmallVec<[(QuestionMarkName, WildcardKind, Vec<Id>); 2]>,
}

impl Default for WildMap {
    fn default() -> Self {
        Self {
            vec: Default::default(),
        }
    }
}

impl WildMap {
    fn insert(&mut self, w: QuestionMarkName, kind: WildcardKind, ids: Vec<Id>) -> Option<&[Id]> {
        // HACK double get is annoying here but you need it for lifetime reasons
        if self.get(&w, kind).is_some() {
            self.get(&w, kind)
        } else {
            self.vec.push((w, kind, ids));
            None
        }
    }
    fn get(&self, w: &QuestionMarkName, kind: WildcardKind) -> Option<&[Id]> {
        for (w2, kind2, ids2) in &self.vec {
            if w == w2 {
                assert_eq!(kind, *kind2);
                return Some(&ids2);
            }
        }
        None
    }
}

impl<'a> std::ops::Index<&'a QuestionMarkName> for WildMap {
    type Output = [Id];
    fn index(&self, q: &QuestionMarkName) -> &Self::Output {
        for (w2, _kind, ids2) in &self.vec {
            if q == w2 {
                return &ids2;
            }
        }
        panic!("Didn't find wildcard {}", q)
    }
}

impl<L, M> Searcher<L, M> for Pattern<L>
where
    L: Language,
    M: Metadata<L>,
{
    fn search(&self, egraph: &EGraph<L, M>) -> Vec<SearchMatches> {
        egraph
            .classes()
            .filter_map(|e| self.search_eclass(egraph, e.id))
            .collect()
    }

    fn search_eclass(&self, egraph: &EGraph<L, M>, eclass: Id) -> Option<SearchMatches> {
        let mappings = search_pat(self, 0, egraph, eclass);
        if mappings.is_empty() {
            None
        } else {
            Some(SearchMatches {
                eclass,
                mappings: mappings.into_vec(),
            })
        }
    }
}

impl<L: Language, M: Metadata<L>> Applier<L, M> for Pattern<L> {
    fn apply_one(&self, egraph: &mut EGraph<L, M>, _: Id, mapping: &WildMap) -> Vec<Id> {
        apply_pat(self, egraph, mapping)
    }
}

fn search_pat<L: Language, M>(
    pat: &Pattern<L>,
    depth: usize,
    egraph: &EGraph<L, M>,
    eclass: Id,
) -> SmallVec<[WildMap; 1]> {
    let pat_expr = match pat {
        Pattern::Wildcard(w, kind) => {
            assert_eq!(*kind, WildcardKind::Single);
            let mut var_mapping = WildMap::default();
            let was_there = var_mapping.insert(w.clone(), *kind, vec![eclass]);
            assert_eq!(was_there, None);

            return smallvec![var_mapping];
        }
        Pattern::ENode(e) => e,
    };

    let mut new_mappings = SmallVec::new();

    if pat_expr.children.is_empty() {
        for e in egraph[eclass].iter() {
            if e.children.is_empty() && pat_expr.op == e.op {
                new_mappings.push(WildMap::default());
                break;
            }
        }
    } else {
        for e in egraph[eclass].iter().filter(|e| e.op == pat_expr.op) {
            let n_multi = pat_expr
                .children
                .iter()
                .filter(|p| p.is_multi_wildcard())
                .count();
            let (range, multi_mapping) = if n_multi > 0 {
                assert_eq!(n_multi, 1, "Patterns can only have one multi match");
                let (position, q) = pat_expr
                    .children
                    .iter()
                    .enumerate()
                    .filter_map(|(i, p)| match p {
                        Pattern::Wildcard(q, WildcardKind::ZeroOrMore) => Some((i, q)),
                        Pattern::Wildcard(_, WildcardKind::Single) => None,
                        Pattern::ENode(_) => None,
                    })
                    .next()
                    .unwrap();
                assert_eq!(
                    position,
                    pat_expr.children.len() - 1,
                    "Multi matches must be in the tail position for now"
                );

                // if the pattern is more than one longer, then we
                // can't match the multi matcher
                let len = pat_expr.children.len();
                if len - 1 > e.children.len() {
                    continue;
                }
                let ids = e.children[len - 1..].to_vec();
                (
                    (0..len - 1),
                    Some((q.clone(), WildcardKind::ZeroOrMore, ids)),
                )
            } else {
                let len = pat_expr.children.len();
                if len != e.children.len() {
                    continue;
                }
                ((0..len), None)
            };

            let mut arg_mappings: Vec<_> = pat_expr.children[range]
                .iter()
                .zip(&e.children)
                .map(|(pa, ea)| search_pat(pa, depth + 1, egraph, *ea))
                .collect();

            if let Some((q, kind, ids)) = multi_mapping {
                let mut m = WildMap::default();
                m.vec.push((q, kind, ids));
                arg_mappings.push(smallvec![m]);
            }

            'outer: for ms in arg_mappings.iter().multi_cartesian_product() {
                let mut combined = ms[0].clone();
                for m in &ms[1..] {
                    for (w, kind, ids) in &m.vec {
                        if let Some(old_ids) = combined.insert(w.clone(), *kind, ids.clone()) {
                            if old_ids != ids.as_slice() {
                                continue 'outer;
                            }
                        }
                    }
                }
                new_mappings.push(combined)
            }
        }
    }

    trace!("new_mapping for {:?}: {:?}", pat_expr, new_mappings);
    new_mappings
}

fn apply_pat<L: Language, M: Metadata<L>>(
    pat: &Pattern<L>,
    egraph: &mut EGraph<L, M>,
    mapping: &WildMap,
) -> Vec<Id> {
    trace!("apply_rec {:2?} {:?}", pat, mapping);

    let result = match &pat {
        Pattern::Wildcard(w, kind) => mapping.get(&w, *kind).unwrap().iter().copied().collect(),
        Pattern::ENode(e) => {
            let children = e
                .children
                .iter()
                .flat_map(|child| apply_pat(child, egraph, mapping));
            let n = ENode::new(e.op.clone(), children);
            trace!("adding: {:?}", n);
            vec![egraph.add(n)]
        }
    };

    trace!("result: {:?}", result);
    result
}

#[cfg(test)]
mod tests {

    use super::WildcardKind;
    use crate::{enode as e, *};

    fn wc<L: Language>(name: &QuestionMarkName) -> Pattern<L> {
        Pattern::Wildcard(name.clone(), WildcardKind::Single)
    }

    #[test]
    fn simple_match() {
        crate::init_logger();
        let mut egraph = EGraph::<String, ()>::default();

        let x = egraph.add(e!("x"));
        let y = egraph.add(e!("y"));
        let plus = egraph.add(e!("+", x, y));

        let z = egraph.add(e!("z"));
        let w = egraph.add(e!("w"));
        let plus2 = egraph.add(e!("+", z, w));

        egraph.union(plus, plus2);
        egraph.rebuild();

        let a: QuestionMarkName = "?a".parse().unwrap();
        let b: QuestionMarkName = "?b".parse().unwrap();

        let pat = |e| Pattern::ENode(Box::new(e));
        let commute_plus = rewrite!(
            "commute_plus";
            { pat(e!("+", wc(&a), wc(&b))) } =>
            { pat(e!("+", wc(&b), wc(&a))) }
        );

        let matches = commute_plus.search(&egraph);
        let n_matches: usize = matches.iter().map(|m| m.mappings.len()).sum();
        assert_eq!(n_matches, 2, "matches is wrong: {:#?}", matches);

        let applications = commute_plus.apply(&mut egraph, &matches);
        egraph.rebuild();
        assert_eq!(applications.len(), 2);

        let wm = |pairs: &[_]| WildMap { vec: pairs.into() };

        use WildcardKind::Single;
        let expected_mappings = vec![
            wm(&[(a.clone(), Single, vec![x]), (b.clone(), Single, vec![y])]),
            wm(&[(a.clone(), Single, vec![z]), (b.clone(), Single, vec![w])]),
        ];
        std::mem::drop((a, b));

        let actual_mappings: Vec<WildMap> =
            matches.iter().flat_map(|m| m.mappings.clone()).collect();

        // for now, I have to check mappings both ways
        if actual_mappings != expected_mappings {
            let e0 = expected_mappings[0].clone();
            let e1 = expected_mappings[1].clone();
            assert_eq!(actual_mappings, vec![e1, e0])
        }

        println!("Here are the mappings!");
        for m in &actual_mappings {
            println!("mappings: {:?}", m);
        }

        egraph.dot().to_dot("target/simple-match.dot").unwrap();

        use crate::extract::{AstSize, Extractor};

        let mut ext = Extractor::new(&egraph, AstSize);
        let (_, best) = ext.find_best(2);
        eprintln!("Best: {:#?}", best);
    }
}
