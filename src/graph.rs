//! The vault as a typed graph, and the algorithms that read it.
//!
//! One implementation, three consumers. `yGraphy` lays this out and draws it, `yy`
//! expands retrieval over it, and `yReviewy` will pull contradiction pairs out of it.
//! Before this module each of them built its own adjacency inline and none of them agreed
//! on what a neighbour was.
//!
//! **What makes this worth having is that the edges are hand-authored.** Graphify and
//! Semantica both have to *extract* their graph — tree-sitter over source, NER over prose
//! — and tag every edge with how much they trust it. Here a `contradicts::` edge is a
//! judgement the author made with context no extraction pass has, so it can be trusted
//! outright and weighted accordingly.
//!
//! Nothing here touches SQL or rendering. Build a [`Graph`] from [`GraphData`] and the
//! rest is pure computation over indices, which is what makes it testable without a vault.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::model::{GraphData, GraphLink};

/// Index into [`Graph::sections`]. Not a `section_uid` — resolve with
/// [`Graph::index_of`] at the boundary and work in indices inside.
pub type NodeIndex = usize;

/// A section, as the graph sees it.
#[derive(Debug, Clone)]
pub struct Node {
    pub uid: String,
    pub note_id: String,
    pub heading: String,
    pub parent_uid: Option<String>,
    pub level: u32,
    pub start_line: usize,
}

/// One typed edge, resolved to indices.
#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub source: NodeIndex,
    pub target: NodeIndex,
    /// Index into [`Graph::relation_types`], so the string is stored once.
    pub relation: RelationType,
}

/// The relation vocabulary, plus an escape hatch.
///
/// An enum rather than a string because ranking switches on it, and a typo in a weight
/// table that silently means "no boost" is exactly the bug that never gets noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RelationType {
    /// A bare `[[link]]` with no prefix.
    Related,
    Outgoing,
    Ingoing,
    /// The strongest signal in the vocabulary: the author says these disagree.
    Contradicts,
    ExampleOf,
    /// This section replaces the target. Valid time, as distinct from recorded time.
    Supersedes,
    /// A structural parent → child edge, not authored. Never traversed with the same
    /// weight as a real link.
    Parent,
    /// Anything the parser accepted that this enum does not know about.
    Other,
}

impl RelationType {
    pub fn parse(value: &str) -> Self {
        match value {
            "related" => Self::Related,
            "outgoing" => Self::Outgoing,
            "ingoing" => Self::Ingoing,
            "contradicts" => Self::Contradicts,
            "example-of" => Self::ExampleOf,
            "supersedes" => Self::Supersedes,
            "parent" => Self::Parent,
            _ => Self::Other,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Related => "related",
            Self::Outgoing => "outgoing",
            Self::Ingoing => "ingoing",
            Self::Contradicts => "contradicts",
            Self::ExampleOf => "example-of",
            Self::Supersedes => "supersedes",
            Self::Parent => "parent",
            Self::Other => "other",
        }
    }

    /// How much traversing this edge should count for.
    ///
    /// These are starting points to be settled by the Stage 7 benchmark, not measured
    /// facts. The ordering is the part worth defending:
    ///
    /// - `contradicts` boosts hardest. A vault accumulates corrections, and answering
    ///   from the note that was later contradicted is the characteristic failure.
    /// - `supersedes` is symmetric to it: reaching a section *through* a `supersedes`
    ///   edge means arriving at the replacement, which is what you wanted.
    /// - `parent` is structural rather than authored, so it carries least.
    pub fn weight(self) -> f32 {
        match self {
            Self::Contradicts => 1.0,
            Self::Supersedes => 0.9,
            Self::ExampleOf => 0.75,
            Self::Ingoing => 0.7,
            Self::Outgoing => 0.65,
            Self::Related => 0.6,
            Self::Other => 0.5,
            Self::Parent => 0.3,
        }
    }
}

/// The vault's sections and the edges between them.
///
/// Adjacency is stored **both ways**. Backlinks matter as much as forward links for
/// retrieval — "what points at this" is usually the more interesting question — and with
/// only a forward map answering it is a full scan.
pub struct Graph {
    sections: Vec<Node>,
    by_uid: HashMap<String, NodeIndex>,
    /// `outgoing[i]` — edges whose source is `i`.
    outgoing: Vec<Vec<Edge>>,
    /// `incoming[i]` — edges whose target is `i`.
    incoming: Vec<Vec<Edge>>,
}

impl Graph {
    /// Build from a [`GraphData`] snapshot.
    ///
    /// Edges naming a section that does not exist are dropped. Those are broken links,
    /// and the vault reports them as diagnostics — the graph is not the place to
    /// re-litigate them.
    pub fn new(data: &GraphData) -> Self {
        let sections: Vec<Node> = data
            .sections
            .iter()
            .map(|section| Node {
                uid: section.uid.clone(),
                note_id: section.note_id.clone(),
                heading: section.heading.clone(),
                parent_uid: section.parent_uid.clone(),
                level: section.level,
                start_line: section.start_line,
            })
            .collect();

        let by_uid: HashMap<String, NodeIndex> = sections
            .iter()
            .enumerate()
            .map(|(index, node)| (node.uid.clone(), index))
            .collect();

        let mut graph = Self {
            outgoing: vec![Vec::new(); sections.len()],
            incoming: vec![Vec::new(); sections.len()],
            sections,
            by_uid,
        };

        for link in &data.links {
            graph.insert(link);
        }

        // Structural edges last, so an authored link between the same pair is the one
        // found first when walking.
        for index in 0..graph.sections.len() {
            let Some(parent) = graph.sections[index]
                .parent_uid
                .as_deref()
                .and_then(|uid| graph.by_uid.get(uid).copied())
            else {
                continue;
            };
            let edge = Edge {
                source: parent,
                target: index,
                relation: RelationType::Parent,
            };
            graph.outgoing[parent].push(edge);
            graph.incoming[index].push(edge);
        }

        graph
    }

    fn insert(&mut self, link: &GraphLink) {
        let (Some(&source), Some(&target)) = (
            self.by_uid.get(link.source.as_str()),
            self.by_uid.get(link.target.as_str()),
        ) else {
            return;
        };
        let edge = Edge {
            source,
            target,
            relation: RelationType::parse(&link.relation_type),
        };
        self.outgoing[source].push(edge);
        self.incoming[target].push(edge);
    }

    pub fn len(&self) -> usize {
        self.sections.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    pub fn sections(&self) -> &[Node] {
        &self.sections
    }

    pub fn node(&self, index: NodeIndex) -> &Node {
        &self.sections[index]
    }

    pub fn index_of(&self, uid: &str) -> Option<NodeIndex> {
        self.by_uid.get(uid).copied()
    }

    pub fn outgoing(&self, index: NodeIndex) -> &[Edge] {
        &self.outgoing[index]
    }

    pub fn incoming(&self, index: NodeIndex) -> &[Edge] {
        &self.incoming[index]
    }

    /// Edges in both directions, as (neighbour, edge, followed_backwards).
    pub fn neighbours(&self, index: NodeIndex) -> impl Iterator<Item = (NodeIndex, Edge, bool)> {
        self.outgoing[index]
            .iter()
            .map(|edge| (edge.target, *edge, false))
            .chain(
                self.incoming[index]
                    .iter()
                    .map(|edge| (edge.source, *edge, true)),
            )
    }

    /// Total edge count, both directions, ignoring type.
    pub fn degree(&self, index: NodeIndex) -> usize {
        self.outgoing[index].len() + self.incoming[index].len()
    }
}

// --- analytics ------------------------------------------------------------------
//
// Everything below is precomputed at index time and written to a column. The
// interactive path reads `sections.rank` and `sections.community_id`; it must never
// call any of this, which is why none of it takes a query.

/// Damping for [`Graph::pagerank`]. The conventional 0.85 — the probability of following
/// an edge rather than teleporting to a random section.
const PAGERANK_DAMPING: f32 = 0.85;
const PAGERANK_ITERATIONS: usize = 30;

impl Graph {
    /// PageRank over the undirected view of the graph.
    ///
    /// Undirected on purpose. `ingoing::` and `outgoing::` already let the author state a
    /// direction, and treating a backlink as worth less than a forward link would count
    /// that twice. What this is measuring is "how central is this section to the vault",
    /// which has no direction.
    ///
    /// Returns a score per node, summing to 1.
    pub fn pagerank(&self) -> Vec<f32> {
        let count = self.sections.len();
        if count == 0 {
            return Vec::new();
        }

        let initial = 1.0 / count as f32;
        let mut rank = vec![initial; count];
        let mut next = vec![0.0; count];

        let degrees: Vec<f32> = (0..count).map(|index| self.degree(index) as f32).collect();

        for _ in 0..PAGERANK_ITERATIONS {
            // Rank held by nodes with no edges at all has nowhere to flow; without
            // redistributing it the vector stops summing to 1 and the scores of every
            // connected node drift down together.
            let mut dangling = 0.0;
            next.fill(0.0);

            for index in 0..count {
                if degrees[index] == 0.0 {
                    dangling += rank[index];
                    continue;
                }
                let share = rank[index] / degrees[index];
                for (neighbour, _, _) in self.neighbours(index) {
                    next[neighbour] += share;
                }
            }

            let teleport = (1.0 - PAGERANK_DAMPING) / count as f32
                + PAGERANK_DAMPING * dangling / count as f32;
            for index in 0..count {
                rank[index] = teleport + PAGERANK_DAMPING * next[index];
            }
        }

        rank
    }

    /// Community assignment by modularity optimisation (Louvain).
    ///
    /// **Not label propagation**, which was tried first and is unusable here: with edge
    /// weights this uniform it collapses everything reachable into one label — two
    /// triangles joined by a single edge come back as one community. A community id that
    /// is the same for the whole vault boosts every section equally, which is to say it
    /// does nothing.
    ///
    /// Louvain optimises modularity instead, which explicitly compares each cluster's
    /// internal weight against what random wiring would produce, so a single bridge is
    /// not enough to fuse two clusters. Deterministic: nodes are visited in index order
    /// and ties break towards the lowest community id, so the stored `community_id` does
    /// not churn on every reindex.
    ///
    /// Returns a community id per node, compacted to `0..n`.
    pub fn communities(&self) -> Vec<usize> {
        if self.sections.is_empty() {
            return Vec::new();
        }

        // Collapse the typed multigraph into one undirected weighted edge per pair:
        // modularity is defined over weights, and two sections joined by both
        // `related` and `contradicts` are more strongly joined than either alone.
        let mut adjacency: Vec<Vec<(NodeIndex, f32)>> = (0..self.len())
            .map(|index| {
                let mut edges: HashMap<NodeIndex, f32> = HashMap::new();
                for (neighbour, edge, _) in self.neighbours(index) {
                    *edges.entry(neighbour).or_default() += edge.relation.weight();
                }
                edges
            })
            .collect::<Vec<_>>()
            .into_iter()
            .enumerate()
            .map(|(index, edges)| {
                edges
                    .into_iter()
                    // A self-link is seen twice by `neighbours` — once forward, once
                    // backward — but the convention below counts a self-loop's weight
                    // twice on its own, so halve it back.
                    .map(|(other, weight)| {
                        (other, if other == index { weight / 2.0 } else { weight })
                    })
                    .collect()
            })
            .collect();

        // Louvain is two phases repeated: move nodes greedily, then contract each
        // community to a single node and do it again. `assignment` maps original nodes
        // to communities of the graph currently being optimised.
        let mut assignment: Vec<usize> = (0..self.len()).collect();

        loop {
            let local = optimise_modularity(&adjacency);
            let community_count = local.iter().copied().max().map_or(0, |max| max + 1);

            if community_count == adjacency.len() {
                // Nothing merged; another level cannot help.
                break;
            }

            for community in assignment.iter_mut() {
                *community = local[*community];
            }

            adjacency = contract(&adjacency, &local, community_count);
        }

        compact(assignment)
    }
}

/// One Louvain level: move each node to the neighbouring community that most improves
/// modularity, repeating until nothing moves. Returns a compacted community per node.
fn optimise_modularity(adjacency: &[Vec<(usize, f32)>]) -> Vec<usize> {
    let count = adjacency.len();

    // A self-loop contributes to both ends of its own edge, hence twice.
    let degree: Vec<f32> = adjacency
        .iter()
        .enumerate()
        .map(|(index, edges)| {
            edges
                .iter()
                .map(|&(other, weight)| if other == index { 2.0 * weight } else { weight })
                .sum()
        })
        .collect();

    let total = degree.iter().sum::<f32>() / 2.0;
    if total <= 0.0 {
        // No edges at all: every node is its own community and modularity is undefined.
        return (0..count).collect();
    }

    let mut community: Vec<usize> = (0..count).collect();
    let mut community_degree = degree.clone();

    // Bounded so a pathological weight pattern cannot spin forever. Louvain normally
    // settles in a handful of passes.
    for _ in 0..PAGERANK_ITERATIONS {
        let mut moved = false;

        for node in 0..count {
            let mut links: HashMap<usize, f32> = HashMap::new();
            for &(other, weight) in &adjacency[node] {
                if other != node {
                    *links.entry(community[other]).or_default() += weight;
                }
            }

            let current = community[node];
            community_degree[current] -= degree[node];

            // Gain from joining `c`, dropping the terms constant across candidates:
            //     weight into c  −  Σ_tot(c) · k_node / 2m
            let gain = |candidate: usize| {
                links.get(&candidate).copied().unwrap_or(0.0)
                    - community_degree[candidate] * degree[node] / (2.0 * total)
            };

            let mut best = current;
            let mut best_gain = gain(current);
            for &candidate in links.keys() {
                let candidate_gain = gain(candidate);
                // Strictly better, or equal and lower-numbered: without the tie-break
                // the partition depends on HashMap iteration order.
                if candidate_gain > best_gain
                    || (candidate_gain == best_gain && candidate < best)
                {
                    best = candidate;
                    best_gain = candidate_gain;
                }
            }

            community_degree[best] += degree[node];
            if best != current {
                community[node] = best;
                moved = true;
            }
        }

        if !moved {
            break;
        }
    }

    compact(community)
}

/// Contract each community to a single node, preserving total edge weight.
fn contract(
    adjacency: &[Vec<(usize, f32)>],
    community: &[usize],
    community_count: usize,
) -> Vec<Vec<(usize, f32)>> {
    let mut merged: Vec<HashMap<usize, f32>> = vec![HashMap::new(); community_count];

    for (node, edges) in adjacency.iter().enumerate() {
        for &(other, weight) in edges {
            *merged[community[node]]
                .entry(community[other])
                .or_default() += weight;
        }
    }

    merged
        .into_iter()
        .enumerate()
        .map(|(index, edges)| {
            edges
                .into_iter()
                // An edge inside a community was accumulated from both of its endpoints,
                // so halve it back into a single self-loop.
                .map(|(other, weight)| {
                    (other, if other == index { weight / 2.0 } else { weight })
                })
                .collect()
        })
        .collect()
}

/// Renumber arbitrary labels to `0..n`, in order of first appearance.
fn compact(labels: Vec<usize>) -> Vec<usize> {
    let mut seen: HashMap<usize, usize> = HashMap::new();
    labels
        .into_iter()
        .map(|label| {
            let next = seen.len();
            *seen.entry(label).or_insert(next)
        })
        .collect()
}

// --- expansion ------------------------------------------------------------------

/// One section reached from a seed, and what it cost to get there.
#[derive(Debug, Clone)]
pub struct Reached {
    pub index: NodeIndex,
    /// 0 for a seed, 1 for a direct neighbour, and so on.
    pub hops: usize,
    /// Seed score × hop decay × relation weight. Compared against other *expanded*
    /// results only — it is not on the same scale as BM25 and must not be added to one.
    /// Fuse ranks, not scores.
    pub score: f32,
    /// The edge that first reached this node. `None` for a seed.
    pub via: Option<Edge>,
    /// Whether that edge was followed backwards, i.e. this is a backlink.
    pub backwards: bool,
}

/// How far to walk and how fast to give up.
#[derive(Debug, Clone, Copy)]
pub struct Expansion {
    pub max_hops: usize,
    /// Multiplier per hop. 1-hop ≈ 0.6, 2-hop ≈ 0.36 at the default.
    pub decay: f32,
    /// Stop once this many sections have been reached, seeds included. Expansion runs in
    /// the interactive path, and a hub section with two hundred backlinks would otherwise
    /// drag its entire neighbourhood into the prompt.
    pub max_results: usize,
}

impl Default for Expansion {
    fn default() -> Self {
        Self {
            max_hops: 2,
            decay: 0.6,
            max_results: 60,
        }
    }
}

impl Graph {
    /// Walk outward from scored seeds, both directions, decaying by hop and edge type.
    ///
    /// Breadth-first, so the first time a node is reached is by its shortest path and the
    /// recorded `via` edge is the one that actually explains it — which is what the "why
    /// this result" line in the dock shows. A later, higher-scoring path to an
    /// already-seen node raises its score but does not rewrite its explanation, because
    /// the shorter path is the more honest answer to "how did we get here".
    ///
    /// Seeds are returned too, at hop 0 with their original score, so the caller can rank
    /// one list rather than merging two.
    pub fn expand(&self, seeds: &[(NodeIndex, f32)], options: Expansion) -> Vec<Reached> {
        let mut reached: Vec<Reached> = Vec::new();
        let mut position: HashMap<NodeIndex, usize> = HashMap::new();
        let mut queue: VecDeque<NodeIndex> = VecDeque::new();

        for &(index, score) in seeds {
            if index >= self.sections.len() {
                continue;
            }
            if let Some(&existing) = position.get(&index) {
                // A duplicated seed keeps the better score rather than being walked twice.
                reached[existing].score = reached[existing].score.max(score);
                continue;
            }
            position.insert(index, reached.len());
            reached.push(Reached {
                index,
                hops: 0,
                score,
                via: None,
                backwards: false,
            });
            queue.push_back(index);
        }

        while let Some(current) = queue.pop_front() {
            let from = reached[position[&current]].clone();
            if from.hops >= options.max_hops {
                continue;
            }

            for (neighbour, edge, backwards) in self.neighbours(current) {
                let score = from.score * options.decay * edge.relation.weight();

                if let Some(&existing) = position.get(&neighbour) {
                    if score > reached[existing].score {
                        reached[existing].score = score;
                    }
                    continue;
                }

                if reached.len() >= options.max_results {
                    return reached;
                }

                position.insert(neighbour, reached.len());
                reached.push(Reached {
                    index: neighbour,
                    hops: from.hops + 1,
                    score,
                    via: Some(edge),
                    backwards,
                });
                queue.push_back(neighbour);
            }
        }

        reached
    }

    /// Pairs joined by `contradicts` — sections the author has said disagree.
    ///
    /// Each pair is reported once, lowest index first, however many edges join them.
    /// Two uses: `brainctl doctor` reports them as vault health, since an unresolved
    /// disagreement with yourself is what makes the dock answer confidently and wrongly;
    /// and each one is an excellent flashcard.
    pub fn contradictions(&self) -> Vec<(NodeIndex, NodeIndex)> {
        let mut pairs: HashSet<(NodeIndex, NodeIndex)> = HashSet::new();
        for edges in &self.outgoing {
            for edge in edges {
                if edge.relation != RelationType::Contradicts {
                    continue;
                }
                let pair = if edge.source <= edge.target {
                    (edge.source, edge.target)
                } else {
                    (edge.target, edge.source)
                };
                pairs.insert(pair);
            }
        }
        let mut pairs: Vec<_> = pairs.into_iter().collect();
        pairs.sort_unstable();
        pairs
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::model::{GraphNote, GraphSection};

    /// `a → b → c`, plus `d` joined to `a` by `contradicts`, plus an isolated `e`.
    fn fixture() -> GraphData {
        let section = |uid: &str| GraphSection {
            uid: uid.to_string(),
            note_id: "note".into(),
            heading: uid.to_uppercase(),
            parent_uid: None,
            level: 1,
            start_line: 1,
        };
        let link = |source: &str, target: &str, relation: &str| GraphLink {
            source: source.into(),
            target: target.into(),
            relation_type: relation.into(),
        };

        GraphData {
            notes: vec![GraphNote {
                id: "note".into(),
                title: "Note".into(),
                topic: None,
                path: PathBuf::from("note.md"),
            }],
            sections: ["a", "b", "c", "d", "e"].map(section).into(),
            links: vec![
                link("a", "b", "related"),
                link("b", "c", "related"),
                link("d", "a", "contradicts"),
                // Dangling: `z` does not exist and must not create a node.
                link("a", "z", "related"),
            ],
        }
    }

    #[test]
    fn adjacency_is_built_both_ways_and_drops_broken_links() {
        let graph = Graph::new(&fixture());
        let a = graph.index_of("a").unwrap();
        let b = graph.index_of("b").unwrap();

        assert_eq!(graph.len(), 5, "the dangling target must not become a node");
        assert_eq!(graph.outgoing(a).len(), 1, "a → b only; a → z is dropped");
        assert_eq!(graph.incoming(a).len(), 1, "d → a");
        assert_eq!(graph.degree(b), 2, "a → b and b → c");
    }

    #[test]
    fn expansion_reaches_backwards_as_well_as_forwards() {
        let graph = Graph::new(&fixture());
        let a = graph.index_of("a").unwrap();

        let reached = graph.expand(&[(a, 1.0)], Expansion::default());
        let uids: HashSet<&str> = reached
            .iter()
            .map(|hit| graph.node(hit.index).uid.as_str())
            .collect();

        assert!(uids.contains("b"), "one hop forward");
        assert!(uids.contains("c"), "two hops forward");
        assert!(uids.contains("d"), "one hop backwards along contradicts");
        assert!(!uids.contains("e"), "e is connected to nothing");
    }

    #[test]
    fn a_closer_section_outscores_a_further_one() {
        let graph = Graph::new(&fixture());
        let a = graph.index_of("a").unwrap();
        let reached = graph.expand(&[(a, 1.0)], Expansion::default());

        let score = |uid: &str| {
            let index = graph.index_of(uid).unwrap();
            reached
                .iter()
                .find(|hit| hit.index == index)
                .unwrap()
                .score
        };

        assert!(score("a") > score("b"), "the seed ranks above its neighbour");
        assert!(score("b") > score("c"), "one hop beats two");
        assert!(
            score("d") > score("b"),
            "contradicts outweighs related at the same distance"
        );
    }

    #[test]
    fn expansion_stops_at_max_hops() {
        let graph = Graph::new(&fixture());
        let a = graph.index_of("a").unwrap();

        let reached = graph.expand(
            &[(a, 1.0)],
            Expansion {
                max_hops: 1,
                ..Expansion::default()
            },
        );
        let uids: HashSet<&str> = reached
            .iter()
            .map(|hit| graph.node(hit.index).uid.as_str())
            .collect();

        assert!(uids.contains("b"));
        assert!(!uids.contains("c"), "c is two hops away");
    }

    #[test]
    fn expansion_records_how_each_section_was_reached() {
        let graph = Graph::new(&fixture());
        let a = graph.index_of("a").unwrap();
        let d = graph.index_of("d").unwrap();

        let reached = graph.expand(&[(a, 1.0)], Expansion::default());
        let hit = reached.iter().find(|hit| hit.index == d).unwrap();

        assert_eq!(hit.hops, 1);
        assert!(hit.backwards, "d → a was followed against its direction");
        assert_eq!(hit.via.unwrap().relation, RelationType::Contradicts);
    }

    #[test]
    fn a_more_connected_section_ranks_higher() {
        let graph = Graph::new(&fixture());
        let rank = graph.pagerank();

        let of = |uid: &str| rank[graph.index_of(uid).unwrap()];
        assert!(of("a") > of("e"), "a has edges, e has none");
        assert!(
            (rank.iter().sum::<f32>() - 1.0).abs() < 1e-3,
            "scores must stay a distribution: {}",
            rank.iter().sum::<f32>()
        );
    }

    #[test]
    fn an_unconnected_section_gets_a_community_of_its_own() {
        let graph = Graph::new(&fixture());
        let communities = graph.communities();
        let of = |uid: &str| communities[graph.index_of(uid).unwrap()];

        // The only assertion label propagation actually guarantees: a label can only
        // travel along an edge, so separate connected components can never merge. How a
        // *single* component is partitioned is a judgement the weights make — on this
        // fixture `a` joins `d` through `contradicts` (1.0) rather than `b` (0.6), which
        // is the algorithm working, not failing.
        for uid in ["a", "b", "c", "d"] {
            assert_ne!(of(uid), of("e"), "{uid} shares a community with an island");
        }
    }

    #[test]
    fn two_clusters_joined_by_one_weak_edge_stay_apart() {
        // Two triangles, bridged once. This is the shape communities exist to find, and
        // the fixture above is too small to show it.
        let section = |uid: &str| GraphSection {
            uid: uid.to_string(),
            note_id: "note".into(),
            heading: uid.into(),
            parent_uid: None,
            level: 1,
            start_line: 1,
        };
        let link = |source: &str, target: &str| GraphLink {
            source: source.into(),
            target: target.into(),
            relation_type: "related".into(),
        };

        let data = GraphData {
            notes: Vec::new(),
            sections: ["a1", "a2", "a3", "b1", "b2", "b3"].map(section).into(),
            links: vec![
                link("a1", "a2"),
                link("a2", "a3"),
                link("a3", "a1"),
                link("b1", "b2"),
                link("b2", "b3"),
                link("b3", "b1"),
                link("a1", "b1"),
            ],
        };

        let graph = Graph::new(&data);
        let communities = graph.communities();
        let of = |uid: &str| communities[graph.index_of(uid).unwrap()];

        assert_eq!(of("a2"), of("a3"), "the first triangle holds together");
        assert_eq!(of("b2"), of("b3"), "so does the second");
        assert_ne!(
            of("a2"),
            of("b2"),
            "one bridge must not fuse two clusters into one community"
        );
    }

    #[test]
    fn analytics_are_deterministic() {
        // Both are stored on the row and compared across reindexes, so an unstable
        // result would rewrite every section's `community_id` on every index run.
        let graph = Graph::new(&fixture());
        assert_eq!(graph.communities(), graph.communities());
        assert_eq!(graph.pagerank(), graph.pagerank());
    }

    #[test]
    fn contradictions_are_reported_once_per_pair() {
        let graph = Graph::new(&fixture());
        let pairs = graph.contradictions();

        assert_eq!(pairs.len(), 1);
        let (left, right) = pairs[0];
        let uids = [graph.node(left).uid.as_str(), graph.node(right).uid.as_str()];
        assert!(uids.contains(&"a") && uids.contains(&"d"));
    }

    #[test]
    fn an_empty_vault_does_not_panic() {
        let graph = Graph::new(&GraphData {
            notes: Vec::new(),
            sections: Vec::new(),
            links: Vec::new(),
        });

        assert!(graph.is_empty());
        assert!(graph.pagerank().is_empty());
        assert!(graph.communities().is_empty());
        assert!(graph.expand(&[], Expansion::default()).is_empty());
    }
}
