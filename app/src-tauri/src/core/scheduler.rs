//! Feature scheduler with topological sort and cycle detection.
//!
//! Uses Kahn's algorithm for both cycle detection and topological ordering.
//! Supports parallel batch scheduling: `next_ready_batch` returns all features
//! whose dependencies are satisfied, enabling concurrent execution.

use anyhow::{anyhow, Result};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::domain::swarm::{Feature, FeatureStatus, Milestone};

/// Schedules features respecting dependency ordering.
#[derive(Debug, Clone)]
pub struct Scheduler {
    features: Vec<Feature>,
    feature_index: HashMap<String, usize>,
}

impl Scheduler {
    /// Create a new scheduler from a list of features.
    ///
    /// Validates that there are no dependency cycles. Returns an error
    /// with the cycle path if one is detected.
    pub fn new(features: Vec<Feature>) -> Result<Self> {
        let feature_index = Self::build_index(&features);

        // Validate all dependency references exist
        for feature in &features {
            for dep in &feature.dependencies {
                if !feature_index.contains_key(dep) {
                    return Err(anyhow!(
                        "feature '{}' depends on '{}' which does not exist",
                        feature.id,
                        dep
                    ));
                }
            }
        }

        if let Some(cycle) = detect_cycles(&features, &feature_index) {
            return Err(anyhow!("dependency cycle detected: {}", cycle.join(" -> ")));
        }

        Ok(Self {
            features,
            feature_index,
        })
    }

    /// Build a mapping from feature ID to index in the features vec.
    fn build_index(features: &[Feature]) -> HashMap<String, usize> {
        features
            .iter()
            .enumerate()
            .map(|(i, f)| (f.id.clone(), i))
            .collect()
    }

    /// Return all feature IDs that are ready to execute.
    ///
    /// A feature is ready when:
    /// - Its current status is `Pending`
    /// - All of its dependencies have status `Completed`
    ///
    /// This enables parallel execution of independent features.
    pub fn next_ready_batch(&self, statuses: &HashMap<String, FeatureStatus>) -> Vec<String> {
        self.features
            .iter()
            .filter(|f| {
                let status = statuses.get(&f.id).unwrap_or(&f.status);
                if *status != FeatureStatus::Pending {
                    return false;
                }

                f.dependencies.iter().all(|dep| {
                    statuses
                        .get(dep)
                        .map(|s| *s == FeatureStatus::Completed)
                        .unwrap_or(false)
                })
            })
            .map(|f| f.id.clone())
            .collect()
    }

    /// Add new features to the scheduler while respecting milestone seals.
    ///
    /// Phase 5B milestone sealing: once a milestone's validator has passed
    /// and the milestone is marked `sealed = true`, no further features may
    /// be scheduled into it (e.g. fix-features synthesized by Guard). Doing
    /// so would conceptually un-seal the milestone without re-validating.
    ///
    /// Rejects with `Err` if any new feature targets a sealed milestone.
    /// Features with `milestone = None` or that reference a milestone id
    /// not present in `milestones` are accepted (the latter mirrors the
    /// "unknown milestone" pattern handled with a warn elsewhere — don't
    /// double-fail here).
    ///
    /// Otherwise delegates to `add_features`.
    pub fn add_features_respecting_seals(
        &mut self,
        new_features: Vec<Feature>,
        milestones: &[Milestone],
    ) -> Result<()> {
        for feature in &new_features {
            if let Some(m_id) = feature.milestone.as_deref() {
                if let Some(m) = milestones.iter().find(|m| m.id == m_id) {
                    if m.sealed {
                        return Err(anyhow!(
                            "cannot add feature '{}' to sealed milestone '{}'",
                            feature.id,
                            m_id
                        ));
                    }
                }
            }
        }

        self.add_features(new_features)
    }

    /// Replace the dependency list of an existing feature.
    ///
    /// Used by the Queen when injecting fix-features for a failed validator:
    /// the validator gets re-marked `Pending` and needs to wait on the new
    /// fix-features before it re-runs. Re-validates the full graph for cycles
    /// after the swap and rolls back on failure.
    pub fn update_feature_deps(&mut self, feature_id: &str, new_deps: Vec<String>) -> Result<()> {
        let idx = *self
            .feature_index
            .get(feature_id)
            .ok_or_else(|| anyhow!("feature '{}' not found in scheduler", feature_id))?;

        // Validate references against the existing feature set before we
        // commit the swap so we never expose the scheduler to a partially-
        // applied broken state.
        for dep in &new_deps {
            if !self.feature_index.contains_key(dep) {
                return Err(anyhow!(
                    "feature '{}' would depend on '{}' which does not exist",
                    feature_id,
                    dep
                ));
            }
        }

        let old_deps = std::mem::replace(&mut self.features[idx].dependencies, new_deps);

        if let Some(cycle) = detect_cycles(&self.features, &self.feature_index) {
            // Roll back the swap so the scheduler keeps its old graph.
            self.features[idx].dependencies = old_deps;
            return Err(anyhow!(
                "updating deps for '{}' would create a dependency cycle: {}",
                feature_id,
                cycle.join(" -> ")
            ));
        }

        Ok(())
    }

    /// Add new features to the scheduler (e.g., fix features).
    ///
    /// Re-validates that no cycles are introduced. Returns an error if
    /// adding these features would create a cycle.
    pub fn add_features(&mut self, new_features: Vec<Feature>) -> Result<()> {
        let mut combined = self.features.clone();
        combined.extend(new_features);

        let new_index = Self::build_index(&combined);

        // Validate references
        for feature in &combined {
            for dep in &feature.dependencies {
                if !new_index.contains_key(dep) {
                    return Err(anyhow!(
                        "feature '{}' depends on '{}' which does not exist",
                        feature.id,
                        dep
                    ));
                }
            }
        }

        if let Some(cycle) = detect_cycles(&combined, &new_index) {
            return Err(anyhow!(
                "adding features would create a dependency cycle: {}",
                cycle.join(" -> ")
            ));
        }

        self.features = combined;
        self.feature_index = new_index;
        Ok(())
    }

    /// Check whether all features are in a terminal state.
    pub fn all_complete(&self, statuses: &HashMap<String, FeatureStatus>) -> bool {
        self.features.iter().all(|f| {
            let status = statuses.get(&f.id).unwrap_or(&f.status);
            status.is_terminal()
        })
    }
}

/// Detect cycles in the feature dependency graph using Kahn's algorithm.
///
/// Returns `Some(cycle_path)` if a cycle is found, `None` if the graph is acyclic.
pub fn detect_cycles(features: &[Feature], index: &HashMap<String, usize>) -> Option<Vec<String>> {
    let n = features.len();
    if n == 0 {
        return None;
    }

    // Build adjacency list and in-degree counts
    // Edge: dependency -> dependent (dep must come before the feature)
    let mut in_degree = vec![0u32; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, feature) in features.iter().enumerate() {
        for dep in &feature.dependencies {
            if let Some(&dep_idx) = index.get(dep) {
                adj[dep_idx].push(i);
                in_degree[i] += 1;
            }
        }
    }

    // Kahn's: start with all nodes of in-degree 0
    let mut queue: VecDeque<usize> = VecDeque::new();
    for (i, &deg) in in_degree.iter().enumerate() {
        if deg == 0 {
            queue.push_back(i);
        }
    }

    let mut processed = 0usize;
    while let Some(node) = queue.pop_front() {
        processed += 1;
        for &neighbor in &adj[node] {
            in_degree[neighbor] -= 1;
            if in_degree[neighbor] == 0 {
                queue.push_back(neighbor);
            }
        }
    }

    if processed == n {
        // All nodes processed, no cycle
        return None;
    }

    // Cycle exists -- find it via DFS on remaining nodes
    let remaining: HashSet<usize> = (0..n).filter(|&i| in_degree[i] > 0).collect();
    find_cycle_path(features, &adj, &remaining)
}

/// Given the set of nodes still in the graph (those with non-zero in-degree),
/// find and return a cycle path.
fn find_cycle_path(
    features: &[Feature],
    adj: &[Vec<usize>],
    remaining: &HashSet<usize>,
) -> Option<Vec<String>> {
    let mut visited = HashSet::new();
    let mut on_stack = HashSet::new();
    let mut parent = HashMap::new();

    for &start in remaining {
        if visited.contains(&start) {
            continue;
        }

        let mut stack = vec![(start, false)];

        while let Some((node, backtrack)) = stack.pop() {
            if backtrack {
                on_stack.remove(&node);
                continue;
            }

            if on_stack.contains(&node) {
                // Found cycle, reconstruct path
                let mut path = vec![features[node].id.clone()];
                let mut current = *parent.get(&node).unwrap_or(&node);
                while current != node {
                    path.push(features[current].id.clone());
                    current = *parent.get(&current).unwrap_or(&node);
                }
                path.push(features[node].id.clone());
                path.reverse();
                return Some(path);
            }

            if visited.contains(&node) {
                continue;
            }

            visited.insert(node);
            on_stack.insert(node);
            stack.push((node, true)); // backtrack marker

            for &neighbor in &adj[node] {
                if remaining.contains(&neighbor) {
                    if !visited.contains(&neighbor) {
                        parent.insert(neighbor, node);
                        stack.push((neighbor, false));
                    } else if on_stack.contains(&neighbor) {
                        // Cycle found
                        let mut path = vec![features[neighbor].id.clone()];
                        let mut current = node;
                        while current != neighbor {
                            path.push(features[current].id.clone());
                            current = *parent.get(&current).unwrap_or(&neighbor);
                        }
                        path.push(features[neighbor].id.clone());
                        path.reverse();
                        return Some(path);
                    }
                }
            }
        }
    }

    // Fallback: we know there's a cycle but couldn't reconstruct path
    Some(remaining.iter().map(|&i| features[i].id.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_feature(id: &str, deps: Vec<&str>) -> Feature {
        Feature {
            id: id.to_string(),
            name: id.to_string(),
            description: format!("Feature {}", id),
            status: FeatureStatus::Pending,
            dependencies: deps.into_iter().map(String::from).collect(),
            milestone: None,
            fix_attempt_count: 0,
            max_fix_attempts: 3,
            fulfills: Vec::new(),
            interrupted: false,
            resumable: false,
        }
    }

    #[test]
    fn test_no_dependencies() {
        let features = vec![
            make_feature("a", vec![]),
            make_feature("b", vec![]),
            make_feature("c", vec![]),
        ];
        let scheduler = Scheduler::new(features).unwrap();
        let statuses = HashMap::new();
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch.len(), 3);
    }

    #[test]
    fn test_linear_dependencies() {
        let features = vec![
            make_feature("a", vec![]),
            make_feature("b", vec!["a"]),
            make_feature("c", vec!["b"]),
        ];
        let scheduler = Scheduler::new(features).unwrap();

        // Only 'a' is ready initially
        let statuses = HashMap::new();
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["a"]);

        // After 'a' completes, 'b' is ready
        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Completed);
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["b"]);
    }

    #[test]
    fn test_cycle_detection() {
        let features = vec![
            make_feature("a", vec!["c"]),
            make_feature("b", vec!["a"]),
            make_feature("c", vec!["b"]),
        ];
        let result = Scheduler::new(features);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("cycle"), "error should mention cycle: {}", err);
    }

    #[test]
    fn test_all_complete() {
        let features = vec![make_feature("a", vec![]), make_feature("b", vec![])];
        let scheduler = Scheduler::new(features).unwrap();

        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Completed);
        statuses.insert("b".to_string(), FeatureStatus::Failed);

        assert!(scheduler.all_complete(&statuses));
    }

    #[test]
    fn test_add_features() {
        let features = vec![make_feature("a", vec![])];
        let mut scheduler = Scheduler::new(features).unwrap();

        let new = vec![make_feature("b", vec!["a"])];
        scheduler.add_features(new).unwrap();

        // Both features ready (b waits on a).
        let statuses = HashMap::new();
        assert_eq!(scheduler.next_ready_batch(&statuses), vec!["a"]);
    }

    #[test]
    fn test_update_feature_deps_extends_validator_to_wait_on_fixes() {
        // Mirrors the queen.rs validator-failure path: the validator's
        // original deps are the milestone's impl features; after a Guard
        // failure we inject fix-features depending on the same impls and
        // extend the validator's deps to also wait on the fixes. The
        // scheduler should accept this swap.
        let features = vec![
            make_feature("impl-1", vec![]),
            make_feature("impl-2", vec![]),
            make_feature("validate-m1", vec!["impl-1", "impl-2"]),
        ];
        let mut scheduler = Scheduler::new(features).unwrap();
        scheduler
            .add_features(vec![make_feature(
                "validate-m1-fix-1",
                vec!["impl-1", "impl-2"],
            )])
            .unwrap();

        scheduler
            .update_feature_deps(
                "validate-m1",
                vec![
                    "impl-1".to_string(),
                    "impl-2".to_string(),
                    "validate-m1-fix-1".to_string(),
                ],
            )
            .unwrap();

        let mut statuses = HashMap::new();
        statuses.insert("impl-1".to_string(), FeatureStatus::Completed);
        statuses.insert("impl-2".to_string(), FeatureStatus::Completed);
        statuses.insert("validate-m1".to_string(), FeatureStatus::Pending);
        statuses.insert("validate-m1-fix-1".to_string(), FeatureStatus::Pending);

        // Only the fix-feature should be ready; the validator must wait.
        let ready = scheduler.next_ready_batch(&statuses);
        assert_eq!(ready, vec!["validate-m1-fix-1".to_string()]);

        // Once the fix completes, the validator becomes ready.
        statuses.insert("validate-m1-fix-1".to_string(), FeatureStatus::Completed);
        let ready = scheduler.next_ready_batch(&statuses);
        assert_eq!(ready, vec!["validate-m1".to_string()]);
    }

    #[test]
    fn test_update_feature_deps_rejects_unknown_dep() {
        let features = vec![make_feature("a", vec![]), make_feature("b", vec!["a"])];
        let mut scheduler = Scheduler::new(features).unwrap();
        let err = scheduler
            .update_feature_deps("b", vec!["nope".to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn test_update_feature_deps_rejects_cycle() {
        let features = vec![
            make_feature("a", vec![]),
            make_feature("b", vec!["a"]),
            make_feature("c", vec!["b"]),
        ];
        let mut scheduler = Scheduler::new(features).unwrap();
        // Force a → c which is a cycle (c → b → a → c).
        let err = scheduler
            .update_feature_deps("a", vec!["c".to_string()])
            .unwrap_err();
        assert!(err.to_string().contains("cycle"));
    }

    #[test]
    fn test_add_features_creates_cycle() {
        let features = vec![make_feature("a", vec!["b"]), make_feature("b", vec![])];
        let mut scheduler = Scheduler::new(features).unwrap();

        let new = vec![
            make_feature("c", vec![]),
            Feature {
                id: "b".to_string(),
                name: "b".to_string(),
                description: "b modified".to_string(),
                status: FeatureStatus::Pending,
                dependencies: vec!["a".to_string()],
                milestone: None,
                fix_attempt_count: 0,
                max_fix_attempts: 3,
                fulfills: Vec::new(),
                interrupted: false,
                resumable: false,
            },
        ];

        // This creates a cycle: a -> b -> a
        let result = scheduler.add_features(new);
        assert!(result.is_err());
    }

    #[test]
    fn test_missing_dependency_error() {
        let features = vec![make_feature("a", vec!["nonexistent"])];
        let result = Scheduler::new(features);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn test_single_feature_no_deps() {
        let features = vec![make_feature("a", vec![])];
        let scheduler = Scheduler::new(features).unwrap();
        let statuses = HashMap::new();
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["a"]);
    }

    #[test]
    fn test_diamond_dependency() {
        //   a → b
        //   a → c
        //   b → d
        //   c → d
        let features = vec![
            make_feature("a", vec![]),
            make_feature("b", vec!["a"]),
            make_feature("c", vec!["a"]),
            make_feature("d", vec!["b", "c"]),
        ];
        let scheduler = Scheduler::new(features).unwrap();

        let statuses = HashMap::new();
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["a"]);

        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Completed);
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["b", "c"]);

        statuses.insert("b".to_string(), FeatureStatus::Completed);
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["c"]);

        statuses.insert("c".to_string(), FeatureStatus::Completed);
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["d"]);
    }

    #[test]
    fn test_skips_failed_features() {
        let features = vec![make_feature("a", vec![]), make_feature("b", vec![])];
        let scheduler = Scheduler::new(features).unwrap();

        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Failed);
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["b"]);
    }

    #[test]
    fn test_skips_skipped_features() {
        let features = vec![make_feature("a", vec![]), make_feature("b", vec![])];
        let scheduler = Scheduler::new(features).unwrap();

        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Skipped);
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["b"]);
    }

    #[test]
    fn test_dependency_not_met_by_failed() {
        let features = vec![make_feature("a", vec![]), make_feature("b", vec!["a"])];
        let scheduler = Scheduler::new(features).unwrap();

        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Failed);
        let batch = scheduler.next_ready_batch(&statuses);
        assert!(batch.is_empty());
    }

    #[test]
    fn test_dependency_not_met_by_skipped() {
        let features = vec![make_feature("a", vec![]), make_feature("b", vec!["a"])];
        let scheduler = Scheduler::new(features).unwrap();

        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Skipped);
        let batch = scheduler.next_ready_batch(&statuses);
        assert!(batch.is_empty());
    }

    #[test]
    fn test_all_complete_mixed_terminal() {
        let features = vec![
            make_feature("a", vec![]),
            make_feature("b", vec![]),
            make_feature("c", vec![]),
        ];
        let scheduler = Scheduler::new(features).unwrap();

        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Completed);
        statuses.insert("b".to_string(), FeatureStatus::Failed);
        statuses.insert("c".to_string(), FeatureStatus::Skipped);
        assert!(scheduler.all_complete(&statuses));
    }

    #[test]
    fn test_all_complete_not_terminal_yet() {
        let features = vec![make_feature("a", vec![]), make_feature("b", vec![])];
        let scheduler = Scheduler::new(features).unwrap();

        let mut statuses = HashMap::new();
        statuses.insert("a".to_string(), FeatureStatus::Completed);
        assert!(!scheduler.all_complete(&statuses));
    }

    #[test]
    fn test_self_dependency_cycle() {
        let features = vec![make_feature("a", vec!["a"])];
        let result = Scheduler::new(features);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn test_independent_branches_parallel() {
        let features = vec![
            make_feature("a", vec![]),
            make_feature("b", vec!["a"]),
            make_feature("c", vec![]),
            make_feature("d", vec!["c"]),
        ];
        let scheduler = Scheduler::new(features).unwrap();

        let statuses = HashMap::new();
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["a", "c"]);
    }

    #[test]
    fn test_empty_features() {
        let scheduler = Scheduler::new(vec![]).unwrap();
        let statuses = HashMap::new();
        assert!(scheduler.next_ready_batch(&statuses).is_empty());
        assert!(scheduler.all_complete(&statuses));
    }

    #[test]
    fn test_status_from_feature_defaults() {
        let mut f = make_feature("a", vec![]);
        f.status = FeatureStatus::Implementing;
        let scheduler = Scheduler::new(vec![f]).unwrap();

        let statuses = HashMap::new();
        let batch = scheduler.next_ready_batch(&statuses);
        assert!(batch.is_empty());
    }

    #[test]
    fn test_injected_validator_depends_on_milestone_features() {
        // Mirrors the shape `inject_milestone_validators` produces: an
        // impl feature plus a `validate-<m>` feature depending on it.
        // The scheduler should refuse to dispatch the validator until the
        // impl feature is Completed.
        let mut validator = make_feature("validate-m1", vec!["f1"]);
        validator.milestone = Some("m1".to_string());
        validator.fulfills = vec!["VAL-M1-001".to_string()];

        let features = vec![make_feature("f1", vec![]), validator];
        let scheduler = Scheduler::new(features).unwrap();

        // Initially only the impl feature is ready.
        let statuses = HashMap::new();
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["f1"]);

        // After the impl feature finishes, the validator becomes ready.
        let mut statuses = HashMap::new();
        statuses.insert("f1".to_string(), FeatureStatus::Completed);
        let batch = scheduler.next_ready_batch(&statuses);
        assert_eq!(batch, vec!["validate-m1"]);
    }

    // ------------------------------------------------------------------
    // add_features_respecting_seals tests (Phase 5B milestone sealing)
    // ------------------------------------------------------------------

    fn make_milestone(id: &str, features: Vec<&str>) -> Milestone {
        Milestone {
            id: id.to_string(),
            name: id.to_string(),
            features: features.into_iter().map(String::from).collect(),
            assertions: vec![],
            sealed: false,
        }
    }

    fn make_feature_in_milestone(id: &str, deps: Vec<&str>, milestone_id: &str) -> Feature {
        let mut f = make_feature(id, deps);
        f.milestone = Some(milestone_id.to_string());
        f
    }

    /// Pending features visible via `next_ready_batch` (deps-clear) plus the
    /// pending tail (still pending but blocked). Lets the seals tests assert
    /// "was/wasn't added" without relying on the deleted `features()` getter.
    fn count_pending(scheduler: &Scheduler) -> usize {
        scheduler.next_ready_batch(&HashMap::new()).len()
    }

    #[test]
    fn test_add_features_respecting_seals_rejects_sealed_milestone() {
        let features = vec![make_feature("a", vec![])];
        let mut scheduler = Scheduler::new(features).unwrap();
        let before = count_pending(&scheduler);

        let mut sealed_milestone = make_milestone("m1", vec!["a"]);
        sealed_milestone.sealed = true;
        let milestones = vec![sealed_milestone];

        let fix = vec![make_feature_in_milestone("fix-1", vec![], "m1")];
        let result = scheduler.add_features_respecting_seals(fix, &milestones);
        assert!(
            result.is_err(),
            "expected Err when adding feature to sealed milestone, got Ok"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("sealed"),
            "error should mention sealed: {}",
            err
        );
        // Scheduler must not have been mutated on rejection.
        assert_eq!(count_pending(&scheduler), before);
    }

    #[test]
    fn test_add_features_respecting_seals_accepts_unsealed() {
        let features = vec![make_feature("a", vec![])];
        let mut scheduler = Scheduler::new(features).unwrap();

        // Default sealed = false
        let milestones = vec![make_milestone("m1", vec!["a"])];

        let fix = vec![make_feature_in_milestone("fix-1", vec![], "m1")];
        let result = scheduler.add_features_respecting_seals(fix, &milestones);
        assert!(
            result.is_ok(),
            "expected Ok for unsealed milestone, got {:?}",
            result
        );
        assert_eq!(count_pending(&scheduler), 2);
    }

    #[test]
    fn test_add_features_respecting_seals_no_milestone() {
        let features = vec![make_feature("a", vec![])];
        let mut scheduler = Scheduler::new(features).unwrap();

        // Sealed milestone present, but the new feature targets no milestone.
        let mut sealed = make_milestone("m1", vec!["a"]);
        sealed.sealed = true;
        let milestones = vec![sealed];

        let extra = vec![make_feature("b", vec![])]; // milestone: None
        let result = scheduler.add_features_respecting_seals(extra, &milestones);
        assert!(
            result.is_ok(),
            "expected Ok when new feature has milestone: None regardless of seal state, got {:?}",
            result
        );
        assert_eq!(count_pending(&scheduler), 2);
    }

    #[test]
    fn test_add_features_respecting_seals_unknown_milestone_passes() {
        let features = vec![make_feature("a", vec![])];
        let mut scheduler = Scheduler::new(features).unwrap();

        // Milestone list does NOT contain "m-unknown".
        let milestones: Vec<Milestone> = vec![make_milestone("m1", vec!["a"])];

        let extra = vec![make_feature_in_milestone("b", vec![], "m-unknown")];
        let result = scheduler.add_features_respecting_seals(extra, &milestones);
        assert!(
            result.is_ok(),
            "expected Ok when feature references unknown milestone, got {:?}",
            result
        );
        assert_eq!(count_pending(&scheduler), 2);
    }
}
