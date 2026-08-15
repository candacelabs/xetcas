//! Git smart HTTP hosting: autocreate, clone, push, re-clone.

mod common;

use std::path::Path;
use std::sync::atomic::Ordering;

use common::TestServer;
use tempfile::TempDir;

/// The first bytes of a smart-HTTP ref advertisement. A request served against
/// a directory that is not a repository never produces this.
const ADVERTISEMENT: &str = "001e# service=git-upload-pack";

/// Run git with a hermetic configuration.
///
/// The user real gitconfig is excluded on purpose: a developer machine may
/// have git-lfs or git-xet registered globally, which would change what these
/// tests actually exercise.
async fn git(args: &[&str], dir: &Path) -> String {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "xetcasd tests")
        .env("GIT_AUTHOR_EMAIL", "tests@example.invalid")
        .env("GIT_COMMITTER_NAME", "xetcasd tests")
        .env("GIT_COMMITTER_EMAIL", "tests@example.invalid")
        .output()
        .await
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn clone_push_and_reclone_a_plain_repository() {
    let server = TestServer::start().await;
    let work = TempDir::new().unwrap();
    let url = format!("{}/git/t/x.git", server.base_url);

    // The repository does not exist yet; the first touch creates it.
    git(
        &["-c", "init.defaultBranch=main", "clone", &url, "first"],
        work.path(),
    )
    .await;
    let first = work.path().join("first");

    std::fs::write(first.join("a.txt"), "hello from xetcasd\n").unwrap();
    git(&["add", "a.txt"], &first).await;
    git(&["commit", "-m", "first commit"], &first).await;
    // Anonymous push only works because autocreate sets http.receivepack.
    git(&["push", "origin", "HEAD:main"], &first).await;

    // A second commit exercises updating an existing ref, not just creating one.
    std::fs::write(first.join("b.txt"), "second file\n").unwrap();
    git(&["add", "b.txt"], &first).await;
    git(&["commit", "-m", "second commit"], &first).await;
    git(&["push", "origin", "HEAD:main"], &first).await;

    git(&["clone", &url, "second"], work.path()).await;
    let second = work.path().join("second");
    assert_eq!(
        std::fs::read_to_string(second.join("a.txt")).unwrap(),
        "hello from xetcasd\n"
    );
    assert_eq!(
        std::fs::read_to_string(second.join("b.txt")).unwrap(),
        "second file\n"
    );

    let log = git(&["log", "--oneline"], &second).await;
    assert_eq!(log.lines().count(), 2, "expected both commits, got: {log}");
}

/// Every first touch of a missing repository races every other one. Creation
/// must happen exactly once, and no request may be served against a repository
/// that another request is still initializing.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_first_touch_initializes_the_repository_exactly_once() {
    let server = TestServer::start().await;
    let url = format!(
        "{}/git/t/race.git/info/refs?service=git-upload-pack",
        server.base_url
    );

    let client = reqwest::Client::new();
    let mut requests = Vec::new();
    for _ in 0..16 {
        let client = client.clone();
        let url = url.clone();
        requests.push(tokio::spawn(async move {
            let response = client.get(&url).send().await.unwrap();
            (response.status(), response.text().await.unwrap())
        }));
    }

    for request in requests {
        let (status, body) = request.await.unwrap();
        assert_eq!(status, 200, "a concurrent first touch failed: {body}");
        assert!(
            body.starts_with(ADVERTISEMENT),
            "a request was served against a repository that was not ready yet: {body:?}"
        );
    }

    assert_eq!(
        server.state.repos_created.load(Ordering::Relaxed),
        1,
        "the repository was initialized more than once"
    );
}

/// A directory whose initialization or configuration failed part-way is not a
/// repository. Treating its existence as readiness makes every later request
/// fail forever with no way back.
#[tokio::test(flavor = "multi_thread")]
async fn a_leftover_directory_does_not_shadow_repository_creation() {
    let server = TestServer::start().await;
    let work = TempDir::new().unwrap();
    let url = format!("{}/git/t/leftover.git", server.base_url);

    std::fs::create_dir_all(server.state.config.git_root().join("t/leftover.git")).unwrap();

    git(
        &["-c", "init.defaultBranch=main", "clone", &url, "repo"],
        work.path(),
    )
    .await;
    let repo = work.path().join("repo");
    std::fs::write(repo.join("f.txt"), "recovered\n").unwrap();
    git(&["add", "f.txt"], &repo).await;
    git(&["commit", "-m", "recovered"], &repo).await;
    git(&["push", "origin", "HEAD:main"], &repo).await;

    assert_eq!(
        server.state.repos_created.load(Ordering::Relaxed),
        1,
        "the leftover directory should have been replaced by a real repository"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn nested_repository_paths_are_supported() {
    let server = TestServer::start().await;
    let work = TempDir::new().unwrap();
    let url = format!("{}/git/team/sub/project.git", server.base_url);

    git(
        &["-c", "init.defaultBranch=main", "clone", &url, "repo"],
        work.path(),
    )
    .await;
    let repo = work.path().join("repo");
    std::fs::write(repo.join("f.txt"), "nested\n").unwrap();
    git(&["add", "f.txt"], &repo).await;
    git(&["commit", "-m", "nested"], &repo).await;
    git(&["push", "origin", "HEAD:main"], &repo).await;

    git(&["clone", &url, "again"], work.path()).await;
    assert_eq!(
        std::fs::read_to_string(work.path().join("again").join("f.txt")).unwrap(),
        "nested\n"
    );
}
