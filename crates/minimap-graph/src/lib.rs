use minimap_core::normalize_label;
use minimap_repo::Graph;
use minimap_schemas::{Edge, Viewport};
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};

#[derive(Debug, Clone)]
pub struct PathPlan {
    pub status: String,
    pub target_slug: String,
    pub current_slug: String,
    pub edges: Vec<Edge>,
    pub skipped_edges: Vec<Value>,
}

impl PathPlan {
    pub fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "target": self.target_slug,
            "current": self.current_slug,
            "edge_ids": self.edges.iter().map(|edge| edge.id.clone()).collect::<Vec<_>>(),
            "skipped_edges": self.skipped_edges
        })
    }
}

pub fn resolve_path(
    graph: &Graph,
    target: &str,
    current_place_id: &str,
    viewport: Option<Viewport>,
) -> PathPlan {
    let target_slug = normalize_label(target);
    let current_slug = graph
        .places
        .get(current_place_id)
        .map(|place| place.slug.clone())
        .unwrap_or_else(|| current_place_id.to_string());

    let Some(target_place) = graph
        .places
        .values()
        .find(|place| place.slug == target_slug)
    else {
        return PathPlan {
            status: "unknown".to_string(),
            target_slug,
            current_slug,
            edges: Vec::new(),
            skipped_edges: Vec::new(),
        };
    };

    if target_place.id == current_place_id {
        return PathPlan {
            status: "ok".to_string(),
            target_slug,
            current_slug,
            edges: Vec::new(),
            skipped_edges: Vec::new(),
        };
    }

    let mut skipped_edges = Vec::new();
    let path = shortest_compatible_path(
        &graph.edges.values().cloned().collect::<Vec<_>>(),
        current_place_id,
        &target_place.id,
        viewport,
        &mut skipped_edges,
    );

    match path {
        Some(edges) => PathPlan {
            status: "ok".to_string(),
            target_slug,
            current_slug,
            edges,
            skipped_edges,
        },
        None if !skipped_edges.is_empty() => PathPlan {
            status: "no_compatible_path".to_string(),
            target_slug,
            current_slug,
            edges: Vec::new(),
            skipped_edges,
        },
        None => PathPlan {
            status: "no_known_path".to_string(),
            target_slug,
            current_slug,
            edges: Vec::new(),
            skipped_edges,
        },
    }
}

fn shortest_compatible_path(
    edges: &[Edge],
    start: &str,
    target: &str,
    viewport: Option<Viewport>,
    skipped_edges: &mut Vec<Value>,
) -> Option<Vec<Edge>> {
    let mut sorted = edges.to_vec();
    sorted.sort_by_key(edge_rank);
    let mut queue = VecDeque::from([(start.to_string(), Vec::<Edge>::new())]);
    let mut visited = BTreeSet::from([start.to_string()]);
    while let Some((place, path)) = queue.pop_front() {
        if place == target {
            return Some(path);
        }
        for edge in sorted.iter().filter(|edge| edge.from.id == place) {
            if !edge_compatible(edge, viewport) {
                skipped_edges.push(json!({
                    "edge": edge.id,
                    "reason": "incompatible_viewport"
                }));
                continue;
            }
            if visited.contains(&edge.to.id) {
                continue;
            }
            let mut next_path = path.clone();
            next_path.push(edge.clone());
            visited.insert(edge.to.id.clone());
            queue.push_back((edge.to.id.clone(), next_path));
        }
    }
    None
}

fn edge_rank(edge: &Edge) -> (usize, usize, String) {
    let geometry = edge
        .recipe
        .iter()
        .any(|step| step.point.is_some() || step.viewport.is_some());
    (usize::from(geometry), edge.recipe.len(), edge.id.clone())
}

pub fn edge_compatible(edge: &Edge, viewport: Option<Viewport>) -> bool {
    edge.recipe.iter().all(|step| {
        if step.point.is_some() {
            step.viewport.is_some() && step.viewport == viewport
        } else {
            true
        }
    })
}

pub fn exit_code_for_status(status: &str) -> i32 {
    match status {
        "ok" | "known" | "known_changed" => 0,
        "needs_label" => 5,
        "unknown" | "no_known_path" | "no_compatible_path" => 5,
        "blocked_by_overlay" | "label_mismatch" | "action_failed" => 2,
        "environment_error" => 6,
        "config_error" => 7,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use minimap_schemas::{ActionStep, EdgeEndpoint, Fingerprint, Place, PlaceBaseline, Selector};
    use std::collections::BTreeMap;

    fn place(slug: &str) -> Place {
        Place {
            schema_version: minimap_schemas::PLACE_SCHEMA_VERSION.to_string(),
            id: format!("place_{slug}"),
            slug: slug.to_string(),
            label: slug.to_string(),
            baseline: PlaceBaseline {
                identity_hash: format!("sha256:{slug}"),
                fingerprint: Fingerprint {
                    selectors: Vec::new(),
                    static_text: Vec::new(),
                    roles: BTreeMap::new(),
                },
            },
            variants: Vec::new(),
        }
    }

    fn edge(id: &str, from: &str, to: &str, geometry: bool) -> Edge {
        Edge {
            schema_version: minimap_schemas::EDGE_SCHEMA_VERSION.to_string(),
            id: id.to_string(),
            from: EdgeEndpoint {
                id: format!("place_{from}"),
                slug: from.to_string(),
            },
            to: EdgeEndpoint {
                id: format!("place_{to}"),
                slug: to.to_string(),
            },
            intent: None,
            recipe: vec![if geometry {
                ActionStep {
                    kind: "tap".to_string(),
                    selector: None,
                    point: Some(minimap_schemas::Point { x: 1, y: 2 }),
                    viewport: Some(Viewport {
                        width: 10,
                        height: 20,
                    }),
                    direction: None,
                }
            } else {
                ActionStep {
                    kind: "tap".to_string(),
                    selector: Some(Selector {
                        kind: "test_tag".to_string(),
                        value: "next".to_string(),
                    }),
                    point: None,
                    viewport: None,
                    direction: None,
                }
            }],
        }
    }

    #[test]
    fn resolves_selector_path_before_geometry() {
        let graph = Graph {
            places: BTreeMap::from([
                ("place_home".to_string(), place("home")),
                ("place_settings".to_string(), place("settings")),
            ]),
            edges: BTreeMap::from([
                (
                    "edge_home__settings__geo".to_string(),
                    edge("edge_home__settings__geo", "home", "settings", true),
                ),
                (
                    "edge_home__settings__selector".to_string(),
                    edge("edge_home__settings__selector", "home", "settings", false),
                ),
            ]),
        };
        let plan = resolve_path(&graph, "settings", "place_home", None);
        assert_eq!(plan.status, "ok");
        assert_eq!(plan.edges[0].id, "edge_home__settings__selector");
    }
}
