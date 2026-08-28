//! Compiling the Java fixtures and running one harness class against one native library.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use super::libraries::library_path;

/// Long enough for a debug-profile engine on a loaded machine, short enough that a deadlock
/// is reported rather than waited on.
const PATIENCE: Duration = Duration::from_secs(300);

/// Compiles the Java fixtures and runs one harness class against one native library,
/// returning everything it printed.
pub fn run_harness(class: &str, library: &Path, classes: &str) -> String {
    let classes = compile(classes);
    let scratch = scratch_dir(class);
    let out_log = scratch.join("stdout.log");
    let err_log = scratch.join("stderr.log");
    let mut child = Command::new(jdk_tool("java"))
        .arg("-Xcheck:jni")
        .arg("-cp")
        .arg(&classes)
        .arg(format!("-Dembedded.mongodb.library={}", library.display()))
        // Not the system temporary directory. That is a ramdisk on a good many Linux
        // machines, and the engine preallocates a couple of hundred megabytes of WiredTiger
        // journal for every data directory it opens, however few documents go in -- so a JVM
        // killed before the harness deletes its tree would leave that in RAM.
        // `CARGO_TARGET_TMPDIR` is under `target`: real storage, wiped by `cargo clean`.
        .arg(format!("-Djava.io.tmpdir={}", scratch.display()))
        .arg(format!("io.github.jeroenvervaeke.embeddedmongodb.{class}"))
        .env("LD_LIBRARY_PATH", library_path())
        // Redirected to files rather than piped: nothing then has to drain a pipe while the
        // wait below polls, so a chatty harness cannot deadlock against its own reader.
        .stdout(create(&out_log))
        .stderr(create(&err_log))
        .spawn()
        .expect("the JDK's java must be runnable");

    let finished = wait_for(&mut child, PATIENCE);
    let stdout = read(&out_log);
    let stderr = read(&err_log);
    println!(
        "--- {class} against {} ---\n{stdout}--- stderr ---\n{stderr}",
        library.display()
    );
    let Some(status) = finished else {
        panic!(
            "{class} was still running after {PATIENCE:?} and has been killed. A command \
             thread that never stops is what a close which no longer retires its handle looks \
             like.\n{stdout}\n{stderr}"
        );
    };
    assert!(
        status.success(),
        "{class} exited with {status}:\n{stdout}\n{stderr}"
    );
    // `-Xcheck:jni` phrases every complaint one of these ways. It is the throw path this
    // really covers: "JNI call made with exception pending". It is *not* what makes the
    // megabyte round trip safe -- each `command` creates at most one local reference and the
    // JVM reclaims the frame on return, so no run can overflow the table however large the
    // arrays are. Matching phrases rather than "no warnings at all" keeps this working on the
    // JDKs that warn about restricted methods.
    let complaints: Vec<&str> = stderr
        .lines()
        .filter(|line| {
            line.contains("in native method")
                || line.contains("JNI local refs")
                || line.contains("FATAL ERROR")
        })
        .collect();
    assert!(
        complaints.is_empty(),
        "the JVM's JNI checker complained: {complaints:?}"
    );
    stdout
}

/// Waits for the harness, killing it at the deadline rather than blocking forever: a hung
/// `cargo test` reports nothing, while a killed child fails the test with its output attached.
fn wait_for(child: &mut Child, patience: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + patience;
    loop {
        match child.try_wait().expect("waiting on the harness must work") {
            Some(status) => return Some(status),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// A fresh directory under `target` for one harness run: its database, and its output.
fn scratch_dir(class: &str) -> PathBuf {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("jvm-{class}"));
    // Recreated, so a killed run's leftovers cannot accumulate across invocations.
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .unwrap_or_else(|error| panic!("creating {} failed: {error}", scratch.display()));
    scratch
}

fn create(path: &Path) -> File {
    File::create(path).unwrap_or_else(|error| panic!("creating {} failed: {error}", path.display()))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn compile(classes: &str) -> PathBuf {
    let sources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/java");
    let package = sources.join("io/github/jeroenvervaeke/embeddedmongodb");
    let files: Vec<PathBuf> = std::fs::read_dir(&package)
        .expect("the Java fixtures must be readable")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "java")
        })
        .collect();
    assert!(
        !files.is_empty(),
        "no Java fixtures in {}",
        package.display()
    );

    let output_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(classes);
    let status = Command::new(jdk_tool("javac"))
        .arg("-d")
        .arg(&output_dir)
        .args(&files)
        .status()
        .expect("the JDK's javac must be runnable");
    assert!(status.success(), "javac failed with {status}");
    output_dir
}

/// Finds a JDK executable: `JAVA_HOME` first, then `PATH`.
fn jdk_tool(name: &str) -> PathBuf {
    let from_java_home = std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin").join(name))
        .into_iter();
    let from_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .map(|directory| directory.join(name));

    let Some(tool) = from_java_home
        .chain(from_path)
        .find(|candidate| candidate.is_file())
    else {
        panic!(
            "no `{name}` on PATH or under JAVA_HOME. These tests need a JDK: it is what \
             proves the JNI contract against a real JVM."
        );
    };
    tool
}
