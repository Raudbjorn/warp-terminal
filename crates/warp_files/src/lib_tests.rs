use async_channel::{Receiver, unbounded};
use warpui_core::r#async::block_on;
use warpui_core::{App, ModelHandle};

// lib_tests.rs
use super::*;

const WRITE_TEST_PATH: &str = "test_data/test_write/";

/// This enum is used so that we can pass the event through the async channel.
/// io::Error is not clonable, so we can't clone the FileModelEvent.
#[derive(Debug)]
enum TestFileModelEvent {
    FileLoaded {
        id: FileId,
        content: String,
        _version: ContentVersion,
    },
    FileSaved,
    FailedToLoad(String),
    FailedToSave,
}

impl From<&FileModelEvent> for TestFileModelEvent {
    fn from(event: &FileModelEvent) -> Self {
        match event {
            FileModelEvent::FileLoaded {
                id,
                content,
                version,
            } => TestFileModelEvent::FileLoaded {
                id: *id,
                content: content.clone(),
                _version: *version,
            },
            FileModelEvent::FileSaved { .. } => TestFileModelEvent::FileSaved,
            FileModelEvent::FailedToLoad {
                id: _id,
                error: err,
            } => TestFileModelEvent::FailedToLoad(format!("{err:?}")),
            FileModelEvent::FailedToSave { .. } => TestFileModelEvent::FailedToSave,
            FileModelEvent::FileUpdated { .. } => {
                // For now, we don't handle file updated events in tests
                // This could be extended to include a FileUpdated variant in TestFileModelEvent if needed
                TestFileModelEvent::FileLoaded {
                    id: event.file_id(),
                    content: String::new(),
                    _version: ContentVersion::new(),
                }
            }
        }
    }
}

/// Setup a Tokio channel that will forward any events from the FileModel to the receiver.
fn setup_event_channel(
    app: &mut App,
    files: &ModelHandle<FileModel>,
) -> Receiver<TestFileModelEvent> {
    let (sender, receiver) = unbounded();
    app.update(|ctx| {
        ctx.subscribe_to_model(files, move |_model, event, _ctx| {
            block_on(sender.send(TestFileModelEvent::from(event)))
                .expect("Could not send the result");
        });
    });
    receiver
}

#[test]
fn test_load() {
    App::test((), |mut app| async move {
        let app = &mut app;
        let files = app.add_singleton_model(FileModel::new);
        let receiver = setup_event_channel(app, &files);

        // Load the test file.
        files.update(app, |model, ctx| {
            model.open(Path::new("test_data/test_file.rs"), false, ctx);
        });

        // Check that the first event out is the file loaded event.
        let event = receiver.recv().await.expect("Could not receive the result");
        match event {
            TestFileModelEvent::FileLoaded { content, .. } => {
                assert_eq!(content.as_bytes(), TEST_FILE_CONTENT)
            }
            _ => panic!("Failed to load file"),
        }
    });
}

#[test]
fn test_save_uninitialized_file() {
    App::test((), |mut app| async move {
        let app = &mut app;

        let files = app.add_singleton_model(FileModel::new);
        let id = FileId::new();

        // This file has not been initialized with the model.  Make sure trying to save it fails immediately.
        files.update(app, |model, ctx| {
            let result = model.save(
                id,
                "This file doesn't exist".to_string(),
                ContentVersion::new(),
                ctx,
            );
            assert!(
                matches!(result, Err(FileSaveError::NoFilePath(file_id)) if file_id == id),
                "expected NoFilePath error"
            );
        });
    });
}

#[test]
fn test_save_file() {
    // Create the test write directory if it doesn't exist.
    std::fs::create_dir_all(WRITE_TEST_PATH).unwrap();

    // Write the test file content to a random file in the test write directory.
    let path = PathBuf::from(WRITE_TEST_PATH).join("test_save_file.rs");
    std::fs::write(&path, TEST_FILE_CONTENT).unwrap();

    App::test((), |mut app| async move {
        let app = &mut app;
        let files = app.add_singleton_model(FileModel::new);
        let receiver = setup_event_channel(app, &files);

        // Open the newly created file.
        let path_clone = path.clone();
        files.update(app, |model, ctx| {
            model.open(&path_clone, false, ctx);
        });

        let file_id = match receiver.recv().await.expect("Could not receive the result") {
            TestFileModelEvent::FileLoaded { id, .. } => id,
            _ => panic!("Failed to load file"),
        };

        let old_version = files.read(app, |files, _ctx| files.version(file_id));
        let new_version = ContentVersion::new();

        // Save new content to the file.
        let completion = files.update(app, |model, ctx| {
            model
                .save(file_id, "Overwrite content".to_string(), new_version, ctx)
                .unwrap()
        });
        // Dispatch alone does not prove persistence; the per-request completion does.
        completion.await.unwrap();

        // Make sure that the file saved event was emitted.
        match receiver.recv().await.expect("Could not receive the result") {
            TestFileModelEvent::FileSaved => (),
            _ => panic!("Failed to save file"),
        }

        // Make sure the content on disk matches the content we saved.
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "Overwrite content");

        // Make sure the version was updated.
        let model_version = files.read(app, |files, _ctx| files.version(file_id));
        assert_ne!(old_version, model_version);
        assert_eq!(Some(new_version), model_version);
    });
}

#[test]
fn save_completion_reports_a_write_failure_after_dispatch() {
    App::test((), |mut app| async move {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let files = app.add_singleton_model(FileModel::new);
        let events = setup_event_channel(&mut app, &files);
        let file_id = files.update(&mut app, |model, ctx| model.open(&path, false, ctx));
        assert!(matches!(
            events.recv().await.unwrap(),
            TestFileModelEvent::FileLoaded { .. }
        ));
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let completion = files.update(&mut app, |model, ctx| {
            model
                .save(file_id, "replacement".into(), ContentVersion::new(), ctx)
                .unwrap()
        });
        assert!(
            completion.await.is_err(),
            "a dispatched write is not a successful save"
        );
        assert!(path.is_dir());
    });
}

#[test]
fn test_load_missing_file() {
    App::test((), |mut app| async move {
        let app = &mut app;
        let files = app.add_singleton_model(FileModel::new);
        let receiver = setup_event_channel(app, &files);

        // Load a file that doesn't exist.
        files.update(app, |model, ctx| {
            model.open(Path::new("test_data/missing_file.rs"), false, ctx);
        });

        // Check that the first event out is the failed to load event.
        let event = receiver.recv().await.expect("Could not receive the result");
        match event {
            TestFileModelEvent::FailedToLoad(err) => {
                // File not found error strings differ across operating systems.
                #[cfg(not(windows))]
                let os_error_message = "No such file or directory";
                #[cfg(windows)]
                let os_error_message = "The system cannot find the file specified.";

                assert_eq!(
                    err,
                    format!(
                        "IOError(Os {{ code: 2, kind: NotFound, message: \"{os_error_message}\" }})"
                    )
                );
            }
            _ => panic!("Failed to load file"),
        }
    });
}

#[test]
fn test_save_missing_directory() {
    // Create the test write directory if it doesn't exist.
    let directory = PathBuf::from(WRITE_TEST_PATH).join("missing-directory");
    std::fs::create_dir_all(&directory).unwrap();

    // Write the test file content to a random file in the test write directory.
    let path = directory.join("test_save_missing_directory.rs");
    std::fs::write(&path, TEST_FILE_CONTENT).unwrap();

    App::test((), |mut app| async move {
        let app = &mut app;
        let files = app.add_singleton_model(FileModel::new);
        let receiver = setup_event_channel(app, &files);

        // Save a file to a directory that doesn't exist.
        let file_id = files.update(app, |model, ctx| model.open(&path, false, ctx));

        // Check that the first event out is the successful load.
        let event = receiver.recv().await.expect("Could not receive the result");
        match event {
            TestFileModelEvent::FileLoaded { content, .. } => {
                assert_eq!(content.as_bytes(), TEST_FILE_CONTENT)
            }
            event => panic!("Failed to load file {event:?}"),
        }

        // Delete the directory that the file is in.
        std::fs::remove_dir_all(directory).unwrap();

        // Save new content to the file.
        files.update(app, |model, ctx| {
            let result = model.save(
                file_id,
                "Overwrite content".to_string(),
                ContentVersion::new(),
                ctx,
            );
            assert!(result.is_ok());
        });

        // Now we expect the save to succeed because ensure_parent_directories will create the missing directory
        match receiver.recv().await.expect("Could not receive the result") {
            TestFileModelEvent::FileSaved => {
                // Make sure the content on disk matches the content we saved.
                let content = std::fs::read_to_string(&path).unwrap();
                assert_eq!(content, "Overwrite content");
            }
            event => panic!("Save should have succeeded but got event: {event:?}"),
        }
    });
}

/// APP-5243: a bare relative file name has an empty parent, which platform watchers resolve to
/// Warp's own process directory. Watching (or worse, unwatching) that directory is never what the
/// caller asked for, so such files get no individual watcher at all.
#[test]
fn test_watch_path_ignores_empty_parents() {
    assert_eq!(FileModel::watch_path_for(Path::new("README.md")), None);
    assert_eq!(FileModel::watch_path_for(Path::new("")), None);
    assert_eq!(
        FileModel::watch_path_for(Path::new("docs/README.md")),
        Some(PathBuf::from("docs"))
    );

    let directory = std::env::temp_dir().join("app-5243");
    assert_eq!(
        FileModel::watch_path_for(&directory.join("README.md")),
        Some(directory)
    );
}

/// Waits for the read that `FileModel::open` spawned to settle, whichever way it resolves.
async fn await_load(receiver: &Receiver<TestFileModelEvent>) {
    match receiver.recv().await.expect("Could not receive the result") {
        TestFileModelEvent::FileLoaded { .. } | TestFileModelEvent::FailedToLoad(_) => (),
        event => panic!("Expected a load result, got {event:?}"),
    }
}

/// Registration and unregistration must use the exact same path, whichever entry point registered
/// the watcher. `register_file_path` used to watch the file itself while `unsubscribe` unwatched
/// its parent directory, so teardown removed a watch that was never added and left the real one
/// behind — the same asymmetry class as the crash this fixes.
#[test]
fn test_registration_and_unregistration_use_the_same_path() {
    App::test((), |mut app| async move {
        let app = &mut app;
        app.add_singleton_model(|_| DetectedRepositories::default());
        let files = app.add_singleton_model(FileModel::new);
        let receiver = setup_event_channel(app, &files);

        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("watched.md");
        std::fs::write(&path, "# watched").expect("write file");

        // `register_file_path` registers up front...
        let registered_id =
            files.update(app, |model, ctx| model.register_file_path(&path, true, ctx));

        // ...and `open` registers once the read succeeds. Both must land on the same directory.
        let opened_id = files.update(app, |model, ctx| model.open(&path, true, ctx));
        await_load(&receiver).await;

        files.read(app, |model, _| {
            let stored_path = model.file_path(opened_id).expect("stored path");
            let watch_path = FileModel::watch_path_for(&stored_path).expect("watch path");
            assert_eq!(Some(watch_path.as_path()), stored_path.parent());
            assert_eq!(
                model.registered_watch_path(registered_id),
                Some(watch_path.as_path())
            );
            assert_eq!(
                model.registered_watch_path(opened_id),
                Some(watch_path.as_path())
            );
        });

        // Unsubscribing releases exactly what was registered, and nothing is left tracked.
        files.update(app, |model, ctx| {
            model.unsubscribe(registered_id, ctx);
            model.unsubscribe(opened_id, ctx);
        });
        files.read(app, |model, _| {
            assert_eq!(model.registered_watch_path(registered_id), None);
            assert_eq!(model.registered_watch_path(opened_id), None);
            assert_eq!(model.file_path(registered_id), None);
            assert_eq!(model.file_path(opened_id), None);
        });
    });
}

/// A file whose read fails never gets a watcher, so teardown must not try to remove one. This is
/// the exact shape of the crash: an unresolved relative path failed to load, and unsubscribing it
/// handed an empty directory to the platform watcher.
#[test]
fn test_a_failed_open_registers_no_watcher() {
    App::test((), |mut app| async move {
        let app = &mut app;
        app.add_singleton_model(|_| DetectedRepositories::default());
        let files = app.add_singleton_model(FileModel::new);
        let receiver = setup_event_channel(app, &files);

        let file_id = files.update(app, |model, ctx| {
            model.open(Path::new("app-5243-does-not-exist.md"), true, ctx)
        });
        await_load(&receiver).await;

        files.read(app, |model, _| {
            assert_eq!(model.registered_watch_path(file_id), None);
        });

        files.update(app, |model, ctx| model.unsubscribe(file_id, ctx));
        assert_eq!(files.read(app, |model, _| model.file_path(file_id)), None);
    });
}

static TEST_FILE_CONTENT: &[u8] = include_bytes!("../test_data/test_file.rs");

#[test]
fn remote_move_reports_disconnected_host_through_future_and_event() {
    App::test((), |mut app| async move {
        app.add_singleton_model(RemoteServerManager::new);
        let files = app.add_singleton_model(FileModel::new);
        let (tx, rx) = unbounded();
        app.update(|ctx| {
            ctx.subscribe_to_model(&files, move |_, event, _| {
                if let FileModelEvent::FailedToSave { id, error } = event {
                    tx.try_send((*id, error.clone())).unwrap();
                }
            });
        });
        let file_id = files.update(&mut app, |model, _| {
            model.register_remote_file(
                HostId::new("disconnected-host".to_owned()),
                StandardizedPath::try_new("/repo/source.txt").unwrap(),
            )
        });
        let future = files
            .update(&mut app, |model, ctx| {
                model.rename_and_save(
                    file_id,
                    PathBuf::from("/repo/target.txt"),
                    "updated\n".to_owned(),
                    ContentVersion::new(),
                    ctx,
                )
            })
            .expect("remote move must dispatch without a local file path");

        let error = future.await.unwrap_err();
        assert!(matches!(&*error, FileSaveError::RemoteError(_)));
        let (event_id, event_error) = rx.recv().await.unwrap();
        assert_eq!(event_id, file_id);
        assert_eq!(error.to_string(), event_error.to_string());
    });
}

#[test]
fn save_guard_rejects_other_writers_and_authorizes_only_its_own_file() {
    App::test((), |mut app| async move {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let canonical = std::fs::canonicalize(&path).unwrap();
        let transaction = directory.path().join("transaction");
        let guard =
            file_write_guard::WriteGuard::acquire(canonical.clone(), transaction.clone()).unwrap();
        let wrong_guard = file_write_guard::WriteGuard::acquire(
            directory.path().join("other.txt"),
            transaction.clone(),
        )
        .unwrap();
        let files = app.add_singleton_model(FileModel::new);
        let events = setup_event_channel(&mut app, &files);
        let file_id = files.update(&mut app, |model, ctx| model.open(&path, false, ctx));
        assert!(matches!(
            events.recv().await.unwrap(),
            TestFileModelEvent::FileLoaded { .. }
        ));

        files.update(&mut app, |model, ctx| {
            assert!(matches!(
                model.save(
                    file_id,
                    "ordinary overwrite".into(),
                    ContentVersion::new(),
                    ctx
                ),
                Err(FileSaveError::Other(_))
            ));
            assert!(matches!(
                model.save_with_guard(
                    file_id,
                    "wrong owner overwrite".into(),
                    ContentVersion::new(),
                    Some(&wrong_guard),
                    ctx
                ),
                Err(FileSaveError::Other(_))
            ));
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        wrong_guard.cancel_before_dispatch();
        let completion = files.update(&mut app, |model, ctx| {
            model
                .save_with_guard(
                    file_id,
                    "owned write".into(),
                    ContentVersion::new(),
                    Some(&guard),
                    ctx,
                )
                .unwrap()
        });
        completion.await.unwrap();
        guard.write_finished();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "owned write");
        assert!(
            file_write_guard::is_quarantined(&canonical),
            "journal completion must still keep the file quarantined"
        );
        assert!(file_write_guard::transaction_is_pinned(&transaction));
        guard.journal_finished();
        assert!(!file_write_guard::is_quarantined(&canonical));
        assert!(!file_write_guard::transaction_is_pinned(&transaction));
    });
}

#[test]
fn save_guard_blocks_rename_and_delete_of_its_source() {
    App::test((), |mut app| async move {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        let destination = directory.path().join("destination.txt");
        std::fs::write(&path, "original").unwrap();
        let files = app.add_singleton_model(FileModel::new);
        let events = setup_event_channel(&mut app, &files);
        let file_id = files.update(&mut app, |model, ctx| model.open(&path, false, ctx));
        assert!(matches!(
            events.recv().await.unwrap(),
            TestFileModelEvent::FileLoaded { .. }
        ));
        let guard = file_write_guard::WriteGuard::acquire(
            std::fs::canonicalize(&path).unwrap(),
            directory.path().join("transaction"),
        )
        .unwrap();
        files.update(&mut app, |model, ctx| {
            assert!(matches!(
                model.rename_and_save(
                    file_id,
                    destination.clone(),
                    "changed".into(),
                    ContentVersion::new(),
                    ctx
                ),
                Err(FileSaveError::Other(_))
            ));
            assert!(matches!(
                model.delete(file_id, ContentVersion::new(), ctx),
                Err(FileSaveError::Other(_))
            ));
        });
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        assert!(!destination.exists());
        guard.cancel_before_dispatch();
    });
}

#[cfg(unix)]
#[test]
fn save_guard_blocks_rename_to_an_alias_of_its_destination_before_source_write() {
    App::test((), |mut app| async move {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let target = directory.path().join("target.txt");
        let alias = directory.path().join("alias.txt");
        std::fs::write(&source, "source original").unwrap();
        std::fs::write(&target, "target original").unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        let files = app.add_singleton_model(FileModel::new);
        let events = setup_event_channel(&mut app, &files);
        let file_id = files.update(&mut app, |model, ctx| model.open(&source, false, ctx));
        assert!(matches!(
            events.recv().await.unwrap(),
            TestFileModelEvent::FileLoaded { .. }
        ));
        let guard = file_write_guard::WriteGuard::acquire(
            std::fs::canonicalize(&target).unwrap(),
            directory.path().join("transaction"),
        )
        .unwrap();
        files.update(&mut app, |model, ctx| {
            assert!(matches!(
                model.rename_and_save(
                    file_id,
                    std::fs::canonicalize(&target).unwrap(),
                    "changed".into(),
                    ContentVersion::new(),
                    ctx
                ),
                Err(FileSaveError::Other(_))
            ));
        });
        let completion = files.update(&mut app, |model, ctx| {
            model
                .rename_and_save(
                    file_id,
                    alias.clone(),
                    "changed".into(),
                    ContentVersion::new(),
                    ctx,
                )
                .unwrap()
        });
        assert!(completion.await.is_err());
        assert_eq!(std::fs::read_to_string(&source).unwrap(), "source original");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "target original");
        assert!(
            std::fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        guard.cancel_before_dispatch();
    });
}

#[cfg(unix)]
#[test]
fn save_guard_blocks_rename_from_a_retargeted_source_alias_before_write() {
    App::test((), |mut app| async move {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.txt");
        let pending = directory.path().join("pending.txt");
        let alias = directory.path().join("alias.txt");
        let destination = directory.path().join("destination.txt");
        std::fs::write(&original, "original content").unwrap();
        std::fs::write(&pending, "pending content").unwrap();
        std::os::unix::fs::symlink(&original, &alias).unwrap();
        let files = app.add_singleton_model(FileModel::new);
        let events = setup_event_channel(&mut app, &files);
        let file_id = files.update(&mut app, |model, ctx| model.open(&alias, false, ctx));
        assert!(matches!(
            events.recv().await.unwrap(),
            TestFileModelEvent::FileLoaded { .. }
        ));
        let guard = file_write_guard::WriteGuard::acquire(
            std::fs::canonicalize(&pending).unwrap(),
            directory.path().join("transaction"),
        )
        .unwrap();
        // The cached canonical source remains original; rename writes through the entry alias.
        std::fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&pending, &alias).unwrap();
        let completion = files.update(&mut app, |model, ctx| {
            model
                .rename_and_save(
                    file_id,
                    destination.clone(),
                    "changed".into(),
                    ContentVersion::new(),
                    ctx,
                )
                .unwrap()
        });
        assert!(completion.await.is_err());
        assert_eq!(
            std::fs::read_to_string(&original).unwrap(),
            "original content"
        );
        assert_eq!(
            std::fs::read_to_string(&pending).unwrap(),
            "pending content"
        );
        assert!(
            std::fs::symlink_metadata(&alias)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(!destination.exists());
        guard.cancel_before_dispatch();
    });
}

#[test]
fn save_completion_does_not_require_a_live_model_callback() {
    App::test((), |mut app| async move {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let files = app.add_model(FileModel::new);
        let events = setup_event_channel(&mut app, &files);
        let file_id = files.update(&mut app, |model, ctx| model.open(&path, false, ctx));
        assert!(matches!(
            events.recv().await.unwrap(),
            TestFileModelEvent::FileLoaded { .. }
        ));
        let weak = files.downgrade();
        let completion = app.update(move |ctx| {
            let completion = files.update(ctx, |model, ctx| {
                model
                    .save(
                        file_id,
                        "persisted after model dropped".into(),
                        ContentVersion::new(),
                        ctx,
                    )
                    .unwrap()
            });
            // Drop the last owner before queued UI completion callbacks can run.
            drop(files);
            assert!(weak.upgrade(ctx).is_none());
            completion
        });
        completion.await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "persisted after model dropped"
        );
    });
}
