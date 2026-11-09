//! Project dependency graph.

use super::{ConfigError, Project};
use petgraph::algo::toposort;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use petgraph::Direction;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Directed graph with an edge from each dependency to its dependent
/// (`depends_on` target → project).
#[derive(Debug, Clone, Default)]
pub struct DependencyGraph {
    graph: DiGraph<String, ()>,
    index: BTreeMap<String, NodeIndex>,
}

impl DependencyGraph {
    pub fn build(projects: &[Project]) -> Result<Self, ConfigError> {
        let mut graph = DiGraph::new();
        let mut index = BTreeMap::new();
        for p in projects {
            let idx = graph.add_node(p.name.clone());
            index.insert(p.name.clone(), idx);
        }
        for p in projects {
            for d in &p.depends_on {
                let from = index[d];
                let to = index[&p.name];
                graph.add_edge(from, to, ());
            }
        }
        let g = Self { graph, index };
        if toposort(&g.graph, None).is_err() {
            return Err(ConfigError::Validation(
                "depends_on contains a cycle".to_string(),
            ));
        }
        Ok(g)
    }

    /// All transitive dependents (downstream projects) of `names`, excluding
    /// `names` themselves, in stable (BFS, then name) order.
    pub fn descendants(&self, names: &[String]) -> Vec<String> {
        let mut seen: BTreeSet<String> = names.iter().cloned().collect();
        let mut out = Vec::new();
        let mut queue: VecDeque<NodeIndex> = names
            .iter()
            .filter_map(|n| self.index.get(n).copied())
            .collect();
        while let Some(n) = queue.pop_front() {
            let mut next: Vec<&String> = self
                .graph
                .edges_directed(n, Direction::Outgoing)
                .map(|e| &self.graph[e.target()])
                .collect();
            next.sort();
            for name in next {
                if seen.insert(name.clone()) {
                    out.push(name.clone());
                    queue.push_back(self.index[name]);
                }
            }
        }
        out
    }

    /// Stable topological order of the given subset: dependencies first,
    /// ties broken by name.
    pub fn sorted(&self, names: &[String]) -> Vec<String> {
        self.levels(names).into_iter().flatten().collect()
    }

    /// Kahn levels restricted to the subset `names`. Each level only depends
    /// on earlier levels; within a level, names are sorted.
    pub fn levels(&self, names: &[String]) -> Vec<Vec<String>> {
        let subset: BTreeSet<&String> = names.iter().collect();
        let mut indeg: BTreeMap<&String, usize> = BTreeMap::new();
        for n in &subset {
            let idx = match self.index.get(*n) {
                Some(i) => *i,
                None => {
                    indeg.insert(n, 0);
                    continue;
                }
            };
            let deg = self
                .graph
                .edges_directed(idx, Direction::Incoming)
                .filter(|e| subset.contains(&self.graph[e.source()]))
                .count();
            indeg.insert(n, deg);
        }
        let mut levels = Vec::new();
        let mut remaining: BTreeSet<&String> = subset.clone();
        while !remaining.is_empty() {
            let mut level: Vec<String> = remaining
                .iter()
                .filter(|n| indeg[**n] == 0)
                .map(|n| (*n).clone())
                .collect();
            if level.is_empty() {
                // Cycle: should have been rejected at build time; fall back to
                // emitting the rest in name order so we never loop forever.
                level = remaining.iter().map(|n| (*n).clone()).collect();
            }
            level.sort();
            for n in &level {
                remaining.remove(n);
                if let Some(idx) = self.index.get(n) {
                    for e in self.graph.edges_directed(*idx, Direction::Outgoing) {
                        let target = &self.graph[e.target()];
                        if let Some(d) = indeg.get_mut(target) {
                            *d = d.saturating_sub(1);
                        }
                    }
                }
            }
            levels.push(level);
        }
        levels
    }

    /// Direct dependencies (upstream) of `name`.
    pub fn dependencies_of(&self, name: &str) -> Vec<String> {
        let Some(idx) = self.index.get(name) else {
            return vec![];
        };
        let mut v: Vec<String> = self
            .graph
            .edges_directed(*idx, Direction::Incoming)
            .map(|e| self.graph[e.source()].clone())
            .collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(name: &str, deps: &[&str]) -> Project {
        Project {
            name: name.to_string(),
            dir: name.to_string(),
            workspace: "default".into(),
            workflow: "default".into(),
            opentofu: false,
            include_patterns: vec![],
            exclude_patterns: vec![],
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            layer: 0,
            alias: None,
            block_name: None,
            branch: None,
            apply_requirements: vec![],
            drift_detection: true,
            drift_include_patterns: vec![],
            drift_exclude_patterns: vec![],
            aws_role_to_assume: None,
            aws_cognito_oidc: None,
            generated: false,
        }
    }

    #[test]
    fn levels_and_descendants() {
        let ps = vec![
            proj("net", &[]),
            proj("db", &["net"]),
            proj("app", &["db", "net"]),
            proj("other", &[]),
        ];
        let g = DependencyGraph::build(&ps).unwrap();
        let names: Vec<String> = ps.iter().map(|p| p.name.clone()).collect();
        assert_eq!(
            g.levels(&names),
            vec![
                vec!["net".to_string(), "other".to_string()],
                vec!["db".to_string()],
                vec!["app".to_string()]
            ]
        );
        // BFS order with names sorted within a level.
        assert_eq!(g.descendants(&["net".to_string()]), vec!["app", "db"]);
        assert_eq!(
            g.sorted(&["app".to_string(), "net".to_string()]),
            vec!["net", "app"]
        );
    }

    #[test]
    fn rejects_cycles() {
        let ps = vec![proj("a", &["b"]), proj("b", &["a"])];
        assert!(DependencyGraph::build(&ps).is_err());
    }
}
