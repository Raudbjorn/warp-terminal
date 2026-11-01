use super::*;

#[test]
fn flattens_expanded_repositories_into_header_and_file_rows() {
    let rows = flatten_rows([(2, true), (0, true), (1, true)]);

    assert_eq!(
        rows,
        [
            OverviewRow::Repo { repo_index: 0 },
            OverviewRow::File {
                repo_index: 0,
                file_index: 0
            },
            OverviewRow::File {
                repo_index: 0,
                file_index: 1
            },
            OverviewRow::Repo { repo_index: 1 },
            OverviewRow::Repo { repo_index: 2 },
            OverviewRow::File {
                repo_index: 2,
                file_index: 0
            },
        ]
    );
}

#[test]
fn collapsed_repositories_contribute_only_their_header() {
    let rows = flatten_rows([(280, false), (3, true)]);

    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0], OverviewRow::Repo { repo_index: 0 });
    assert_eq!(rows[1], OverviewRow::Repo { repo_index: 1 });
    assert_eq!(
        rows[4],
        OverviewRow::File {
            repo_index: 1,
            file_index: 2
        }
    );
}

#[test]
fn no_repositories_produce_no_rows() {
    assert!(flatten_rows([]).is_empty());
}

#[test]
fn only_repositories_with_many_changes_start_collapsed() {
    assert!(is_expanded_by_default(0));
    assert!(is_expanded_by_default(MAX_FILES_EXPANDED_BY_DEFAULT));
    assert!(!is_expanded_by_default(MAX_FILES_EXPANDED_BY_DEFAULT + 1));
    assert!(!is_expanded_by_default(280));
}

#[test]
fn every_status_has_a_distinct_letter() {
    let statuses = [
        GitFileStatus::New,
        GitFileStatus::Modified,
        GitFileStatus::Deleted,
        GitFileStatus::Renamed {
            old_path: "old".to_owned(),
        },
        GitFileStatus::Copied {
            old_path: "old".to_owned(),
        },
        GitFileStatus::Untracked,
        GitFileStatus::Conflicted,
    ];

    let letters: Vec<&str> = statuses.iter().map(status_letter).collect();

    assert_eq!(letters, ["A", "M", "D", "R", "C", "U", "!"]);
}

#[test]
fn change_counts_are_singular_only_for_one() {
    assert_eq!(change_count_label(0), "0 changes");
    assert_eq!(change_count_label(1), "1 change");
    assert_eq!(change_count_label(300), "300 changes");
}

#[test]
fn main_branch_is_recognised_with_or_without_its_remote_prefix() {
    assert!(is_main_branch("main", "main"));
    assert!(is_main_branch("main", "origin/main"));
    assert!(!is_main_branch("feature", "origin/main"));
    assert!(!is_main_branch("origin/main", "main"));
}

#[test]
fn git_operation_timeout_keeps_ready_results_and_errors() {
    futures_lite::future::block_on(async {
        assert_eq!(
            git_operation_with_timeout(async { Ok(7) }, Duration::from_secs(1))
                .await
                .unwrap(),
            7
        );
        let error: anyhow::Result<()> = git_operation_with_timeout(
            async { anyhow::bail!("git failed") },
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(error.unwrap_err().to_string(), "git failed");
    });
}

#[test]
fn git_operation_timeout_drops_pending_work_without_a_tokio_runtime() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct PendingOperation(Arc<AtomicBool>);
    impl Future for PendingOperation {
        type Output = anyhow::Result<()>;
        fn poll(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Self::Output> {
            std::task::Poll::Pending
        }
    }
    impl Drop for PendingOperation {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let dropped = Arc::new(AtomicBool::new(false));
    let result = futures_lite::future::block_on(git_operation_with_timeout(
        PendingOperation(dropped.clone()),
        Duration::from_millis(5),
    ));
    assert!(result.unwrap_err().to_string().contains("timed out"));
    assert!(
        dropped.load(Ordering::SeqCst),
        "the timeout must release the pending operation"
    );
}
