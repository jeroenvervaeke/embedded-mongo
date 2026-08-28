// Running a child process whose output must not reach cargo's pipes. Only Bazel needs this,
// but the reason is about process plumbing rather than about the engine, so it lives here
// rather than in build_native.rs -- which stays the file that decides what the library
// contains, and nothing else.

use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::Duration;

/// Runs a command with its output going to `log_path` instead of to this script's streams,
/// echoing the log to stderr as it fills. `Err` means the command could not be started.
///
/// The redirection is not tidiness, and `Stdio::piped()` is not a substitute for it. Cargo
/// gives a build script pipes for stdout and stderr and waits for both to reach EOF before
/// the script counts as finished. The Bazel client forks a build server that outlives it,
/// and that server keeps whatever the client's stdout and stderr were: it points its own
/// fd 1 and 2 at `jvm.out`, but the inherited pair survives at higher descriptor numbers for
/// the server's whole lifetime. Hand Bazel these pipes and a *successful* build hangs cargo
/// forever -- Bazel has exited 0 and cargo is still waiting for an EOF that nothing short of
/// `bazel shutdown` can deliver. A pipe of our own only moves the hang to whoever drains it.
/// What breaks the cycle is giving the server a descriptor with no EOF to wait for: a
/// regular file. It is a file rather than `Stdio::null()` because an engine build's
/// diagnostics are the whole reason anyone runs one.
///
/// Bazel is the only command here that needs this. The others -- curl and wget in
/// `build_download`, git in `build_freshness`, the compiler probes, and install_name_tool and
/// codesign in `build_link` -- run to completion and leave nothing behind, so they can go on
/// writing straight to cargo. Anything added later that forks a daemon, a compiler wrapper
/// like sccache included, belongs on this path instead.
pub(crate) fn run_logged(command: &mut Command, log_path: &Path) -> std::io::Result<ExitStatus> {
    // Unlinked rather than truncated in place: a Bazel server left over from an earlier
    // build still holds that log at the offset it stopped at, and reusing the inode would
    // let it punch a hole into the middle of this build's log. A new inode leaves the stale
    // descriptor pointing at a file nothing reads.
    match fs::remove_file(log_path) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => panic!("failed to replace {}: {error}", log_path.display()),
    }
    let stdout = fs::File::create(log_path)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", log_path.display()));
    // Duplicated rather than opened twice, so the two streams share one file position and
    // interleave instead of overwriting each other.
    let stderr = stdout
        .try_clone()
        .unwrap_or_else(|error| panic!("failed to duplicate {}: {error}", log_path.display()));
    let mut child = command
        // Given away too: otherwise the server holds the terminal cargo was started from.
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()?;

    // Echoed while the command runs rather than at the end, because silence for the minutes
    // to hours an engine build takes is indistinguishable from the hang above. This is also
    // how the output reaches a human: cargo streams a build script's stderr under `-vv`, and
    // prints all of it when the script fails.
    let mut log = fs::File::open(log_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", log_path.display()));
    loop {
        echo(&mut log);
        let waited = child
            .try_wait()
            .unwrap_or_else(|error| panic!("failed to wait for the build: {error}"));
        if let Some(status) = waited {
            // Whatever landed between the last echo and the exit.
            echo(&mut log);
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Copies everything appended to `log` since the last call to this script's stderr. Failing
/// to read or write the echo is not worth failing a build over: the bytes are in the log
/// either way, and the caller names it.
fn echo(log: &mut fs::File) {
    let mut buffer = [0u8; 16 * 1024];
    loop {
        match log.read(&mut buffer) {
            // Caught up with the writer for now.
            Ok(0) => return,
            Ok(read) => {
                let _ = std::io::stderr().write_all(&buffer[..read]);
            }
            // A signal, not a problem with the log. Giving up here would drop the rest of
            // the output, and the call made after the child exits has no later attempt to
            // recover in.
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}
