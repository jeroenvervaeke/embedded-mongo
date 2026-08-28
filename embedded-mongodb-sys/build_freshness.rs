// Whether this checkout is still the checkout the published library was built from.
//
// The exact half of the freshness question: it compares commits, so it answers precisely,
// and it can only be asked where git history is. `build_exports` is the half that works
// everywhere else.

use std::path::Path;
use std::process::Command;

use crate::{ARTIFACT_PATHS, RELEASE_TAG, SOURCE_COMMIT};

/// Hard-errors when this checkout's native inputs no longer match the published library.
///
/// Every question runs from the workspace root: a build script's working directory is the
/// package directory, and a pathspec naming `mongo` from inside `embedded-mongodb-sys`
/// matches nothing and exits 0, which would silently drop the engine pin from the
/// comparison.
pub(crate) fn check_not_stale(workspace_root: &Path) {
    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(workspace_root)
            // A build can be started from inside another repository's hook or from
            // `git rebase --exec`, both of which export these. Git would then answer about
            // that repository's objects and index while still reporting this directory as
            // the work tree, and every comparison below would be against the wrong history.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .ok()?;
        // Trailing whitespace only: `git status --porcelain` puts the staged and unstaged
        // status in the first two columns, so a leading space is one of the two answers and
        // trimming it would report an unstaged edit as a staged one.
        output.status.success().then(|| {
            String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_string()
        })
    };

    // A published crate, a release tarball, `cargo vendor` output committed into somebody
    // else's repository: no history of ours to compare against, and none owed. Its sources
    // and the manifest inside them were published together, so there is nothing here that
    // could have drifted, and failing it would turn "this cannot be checked" into "you are
    // broken". `check_exports` is the check those consumers get instead.
    //
    // Asked of the file system rather than of git, deliberately: it is the one question git
    // being absent, broken or refusing cannot change the answer to.
    if !workspace_root.join(".git").exists() {
        return;
    }

    // From here on this is somebody's checkout, so silence is the wrong answer. Git declining
    // -- not installed, or `detected dubious ownership` on a bind-mounted checkout owned by
    // another uid, which is the ordinary container case -- leaves the comparison undone, and
    // saying so is the least it owes the reader.
    let Some(toplevel) = git(&["rev-parse", "--show-toplevel"]) else {
        println!(
            "cargo:warning=embedded-mongodb: {} has a .git but git could not read it, so this \
             checkout was not compared against {RELEASE_TAG}",
            workspace_root.display()
        );
        return;
    };
    let is_workspace_root = Path::new(&toplevel)
        .canonicalize()
        .ok()
        .zip(workspace_root.canonicalize().ok())
        .is_some_and(|(toplevel, workspace_root)| toplevel == workspace_root);
    if !is_workspace_root {
        println!(
            "cargo:warning=embedded-mongodb: git reports {toplevel} as the root of this \
             checkout rather than {}, so it was not compared against {RELEASE_TAG}",
            workspace_root.display()
        );
        return;
    }

    if git(&["cat-file", "-e", &format!("{SOURCE_COMMIT}^{{commit}}")]).is_none() {
        // Shallow is the difference between a history that is incomplete and one that is
        // simply not ours. A complete clone of this repository always holds SOURCE_COMMIT --
        // the manifest naming it was committed on top of it -- so a complete clone without it
        // is a repository built some other way: a "use this template" copy, a fork whose
        // history was rewritten, a tarball someone ran `git init` in. None of those is broken,
        // and none can act on advice to fetch.
        if git(&["rev-parse", "--is-shallow-repository"]).as_deref() != Some("true") {
            println!(
                "cargo:warning=embedded-mongodb: this repository does not contain \
                 {SOURCE_COMMIT}, which {RELEASE_TAG} was built from, so it is not a clone of \
                 this project's history and was not compared against it"
            );
            return;
        }
        // A shallow clone of this repository, on the other hand, is exactly the case this
        // check exists for: the commit is reachable, it was simply never fetched. Warning and
        // carrying on is how a shallow CI checkout came to link a library that predated its
        // own tree and fail minutes later with `undefined symbol:
        // embedded_mongodb_open_with_options`, naming neither the cause nor the remedy.
        panic!(
            "cannot tell whether the prebuilt embedded MongoDB library ({RELEASE_TAG}) matches \
             this checkout: the commit it was built from, {SOURCE_COMMIT}, is not in this \
             repository.\n\n  This clone is shallow, so that commit was never fetched.\n\n\
             Choose one:\n  \
             * fetch the missing history:   git fetch --unshallow   (actions/checkout: \
             fetch-depth: 0)\n  \
             * build the engine yourself:   EMBEDDED_MONGODB_BUILD_FROM_SOURCE=1 cargo build\n  \
             * point at a library you have: EMBEDDED_MONGODB_NATIVE_LIB_DIR=<dir>\n"
        );
    }

    // Read the submodule's own HEAD when it is checked out. The recorded gitlink does not
    // move on a local `cd mongo && git checkout <other-sha>`, so comparing gitlinks alone
    // would miss exactly the developer testing another engine revision.
    let local_pin = if workspace_root.join("mongo/.git").exists() {
        git(&["-C", "mongo", "rev-parse", "HEAD"])
    } else {
        git(&["rev-parse", "HEAD:mongo"])
    };
    // Optional even though the commit above is present: a partial clone has the commit
    // without the tree it names, and cannot produce this without reaching the remote. The
    // rev-list below still catches a moved pin, so a missing answer here costs precision,
    // not the check.
    let published_pin = git(&["rev-parse", &format!("{SOURCE_COMMIT}^{{commit}}:mongo")]);

    let mut reasons = Vec::new();
    if let (Some(local_pin), Some(published_pin)) = (&local_pin, &published_pin)
        && local_pin != published_pin
    {
        reasons.push(format!(
            "the mongo submodule is at {local_pin}, the prebuilt was built from {published_pin}"
        ));
    }

    let range = format!("{SOURCE_COMMIT}..HEAD");
    let mut args = vec!["rev-list", "--count", &range, "--"];
    args.extend_from_slice(ARTIFACT_PATHS);
    args.push("mongo");
    if let Some(count) = git(&args)
        && count != "0"
    {
        reasons.push(format!(
            "{count} commit(s) since {SOURCE_COMMIT} changed the native inputs"
        ));
    }

    // Deliberately not `mongo`. Applying the patches leaves the submodule's working tree
    // dirty, which the parent repository reports as a modification to that path -- so
    // including it here would fail every build for anyone who has run
    // scripts/apply-mongo-patches, which is a required step for a source build and produces
    // exactly the engine the published library was built from. Engine drift that matters
    // still shows up: the gitlink comparison above catches a moved pin, and the submodule's
    // own HEAD is compared directly.
    let mut args = vec!["status", "--porcelain", "--"];
    args.extend_from_slice(ARTIFACT_PATHS);
    if let Some(dirty) = git(&args)
        && !dirty.is_empty()
    {
        reasons.push(format!(
            "uncommitted changes:\n    {}",
            dirty.replace('\n', "\n    ")
        ));
    }

    if reasons.is_empty() {
        return;
    }
    panic!(
        "the native library inputs in this checkout no longer match the published library \
         ({RELEASE_TAG}, built from {SOURCE_COMMIT}).\n\n  {}\n\n\
         Choose one:\n  \
         * build the engine yourself:     EMBEDDED_MONGODB_BUILD_FROM_SOURCE=1 cargo build\n  \
         * publish a build for this tree: gh workflow run native.yml --ref <this branch>\n  \
         * point at a library you have:   EMBEDDED_MONGODB_NATIVE_LIB_DIR=<dir>\n",
        reasons.join("\n  ")
    );
}
