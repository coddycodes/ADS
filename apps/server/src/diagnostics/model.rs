use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

pub const PROCESS_CLAIM_LABEL_MAX_SCALARS: usize = 80;
pub const PROCESS_COMMAND_MAX_SCALARS: usize = 512;

#[must_use]
pub fn bound_diagnostic_string(value: &str, maximum_scalars: usize) -> String {
    value.chars().take(maximum_scalars).collect()
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub started_at: u64,
}

impl ProcessIdentity {
    #[must_use]
    pub fn key(self) -> String {
        format!("{}:{}", self.pid, self.started_at)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRow {
    pub pid: u32,
    pub started_at: u64,
    pub ppid: u32,
    pub pgid: Option<i32>,
    pub status: String,
    pub cpu_percent: f32,
    pub cpu_core_percent: Option<f32>,
    pub rss_bytes: u64,
    pub elapsed: String,
    pub command: String,
}

impl ProcessRow {
    #[doc(hidden)]
    pub fn fixture(pid: u32, ppid: u32, command: impl Into<String>) -> Self {
        Self {
            pid,
            started_at: 0,
            ppid,
            pgid: None,
            status: "Run".to_string(),
            cpu_percent: 0.0,
            cpu_core_percent: Some(0.0),
            rss_bytes: 0,
            elapsed: "00:00:00".to_string(),
            command: command.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessTreeMetadata {
    pub ppid: u32,
    pub pgid: Option<i32>,
    pub status: String,
    pub elapsed: String,
    pub command: String,
    pub depth: usize,
    pub child_pids: Vec<u32>,
}

#[must_use]
pub fn process_tree_metadata(
    rows: &[ProcessRow],
    identities: impl IntoIterator<Item = ProcessIdentity>,
) -> HashMap<ProcessIdentity, ProcessTreeMetadata> {
    let included = identities.into_iter().collect::<HashSet<_>>();
    let rows_by_identity = rows
        .iter()
        .map(|row| {
            (
                ProcessIdentity {
                    pid: row.pid,
                    started_at: row.started_at,
                },
                row,
            )
        })
        .collect::<HashMap<_, _>>();
    let identities_by_pid = included
        .iter()
        .copied()
        .map(|identity| (identity.pid, identity))
        .collect::<HashMap<_, _>>();
    let mut children_by_identity = HashMap::<ProcessIdentity, Vec<u32>>::new();
    for child in &included {
        let Some(row) = rows_by_identity.get(child) else {
            continue;
        };
        let Some(parent) = identities_by_pid.get(&row.ppid).copied() else {
            continue;
        };
        if parent.started_at <= child.started_at {
            children_by_identity
                .entry(parent)
                .or_default()
                .push(child.pid);
        }
    }
    for child_pids in children_by_identity.values_mut() {
        child_pids.sort_unstable();
    }
    let mut depths = HashMap::new();

    included
        .iter()
        .filter_map(|identity| {
            let row = rows_by_identity.get(identity)?;
            let depth = process_depth(
                *identity,
                &rows_by_identity,
                &identities_by_pid,
                &mut depths,
                &mut HashSet::new(),
            );
            Some((
                *identity,
                ProcessTreeMetadata {
                    ppid: row.ppid,
                    pgid: row.pgid,
                    status: row.status.clone(),
                    elapsed: row.elapsed.clone(),
                    command: row.command.clone(),
                    depth,
                    child_pids: children_by_identity
                        .get(identity)
                        .cloned()
                        .unwrap_or_default(),
                },
            ))
        })
        .collect()
}

fn process_depth(
    identity: ProcessIdentity,
    rows_by_identity: &HashMap<ProcessIdentity, &ProcessRow>,
    identities_by_pid: &HashMap<u32, ProcessIdentity>,
    memo: &mut HashMap<ProcessIdentity, usize>,
    visiting: &mut HashSet<ProcessIdentity>,
) -> usize {
    if let Some(depth) = memo.get(&identity) {
        return *depth;
    }
    if !visiting.insert(identity) {
        return 0;
    }
    let depth = rows_by_identity
        .get(&identity)
        .and_then(|row| identities_by_pid.get(&row.ppid))
        .copied()
        .filter(|parent| parent.started_at <= identity.started_at)
        .map_or(0, |parent| {
            process_depth(parent, rows_by_identity, identities_by_pid, memo, visiting)
                .saturating_add(1)
        });
    visiting.remove(&identity);
    memo.insert(identity, depth);
    depth
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescendantEntry {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: Option<i32>,
    pub status: String,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
    pub elapsed: String,
    pub command: String,
    pub depth: usize,
    pub child_pids: Vec<u32>,
}

pub fn build_descendant_entries(rows: &[ProcessRow], root_pid: u32) -> Vec<DescendantEntry> {
    let mut children = HashMap::<u32, Vec<&ProcessRow>>::new();
    for row in rows {
        children.entry(row.ppid).or_default().push(row);
    }
    for values in children.values_mut() {
        values.sort_by_key(|row| row.pid);
    }

    let mut descendants = Vec::new();
    let mut stack = children
        .get(&root_pid)
        .into_iter()
        .flatten()
        .rev()
        .map(|row| (*row, 0usize))
        .collect::<Vec<_>>();
    let mut visited = HashSet::from([root_pid]);
    while let Some((row, depth)) = stack.pop() {
        if !visited.insert(row.pid) {
            continue;
        }
        let child_pids = children
            .get(&row.pid)
            .map(|values| values.iter().map(|child| child.pid).collect())
            .unwrap_or_default();
        descendants.push(DescendantEntry {
            pid: row.pid,
            ppid: row.ppid,
            pgid: row.pgid,
            status: row.status.clone(),
            cpu_percent: row.cpu_percent,
            rss_bytes: row.rss_bytes,
            elapsed: row.elapsed.clone(),
            command: row.command.clone(),
            depth,
            child_pids,
        });
        if let Some(values) = children.get(&row.pid) {
            stack.extend(values.iter().rev().map(|child| (*child, depth + 1)));
        }
    }
    descendants
}

pub fn build_process_tree_entries(rows: &[ProcessRow], root_pid: u32) -> Vec<DescendantEntry> {
    let Some(root) = rows.iter().find(|row| row.pid == root_pid) else {
        return Vec::new();
    };
    let mut descendants = build_descendant_entries(rows, root_pid);
    for entry in &mut descendants {
        entry.depth += 1;
    }
    let child_pids = descendants
        .iter()
        .filter(|entry| entry.depth == 1)
        .map(|entry| entry.pid)
        .collect();
    let mut tree = Vec::with_capacity(descendants.len() + 1);
    tree.push(DescendantEntry {
        pid: root.pid,
        ppid: root.ppid,
        pgid: root.pgid,
        status: root.status.clone(),
        cpu_percent: root.cpu_percent,
        rss_bytes: root.rss_bytes,
        elapsed: root.elapsed.clone(),
        command: root.command.clone(),
        depth: 0,
        child_pids,
    });
    tree.extend(descendants);
    tree
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessDiagnosticsResult {
    pub server_pid: u32,
    pub read_at_ms: i128,
    pub process_count: usize,
    pub total_rss_bytes: u64,
    pub total_cpu_percent: f64,
    pub processes: Vec<DescendantEntry>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_tree_includes_the_native_root_and_offsets_descendant_depths() {
        let mut root = ProcessRow::fixture(10, 1, "coddy-studio.exe");
        root.rss_bytes = 100;
        let mut child = ProcessRow::fixture(11, 10, "codex.exe");
        child.rss_bytes = 50;
        let grandchild = ProcessRow::fixture(12, 11, "git.exe");

        let tree = build_process_tree_entries(&[root, child, grandchild], 10);

        assert_eq!(
            tree.iter().map(|entry| entry.pid).collect::<Vec<_>>(),
            [10, 11, 12]
        );
        assert_eq!(
            tree.iter().map(|entry| entry.depth).collect::<Vec<_>>(),
            [0, 1, 2]
        );
        assert_eq!(tree[0].child_pids, [11]);
    }
}
