// Whether the library carries the entry points this checkout's bridge calls.
//
// The coarse half of the freshness question, and the half that needs no git history, so it
// is what a published crate, a source archive or a vendored tree gets. `build_freshness` has
// the exact half.

use std::fs;
use std::path::Path;

/// The header `cpp/bridge.cc` includes, relative to the crate root. What it declares is what
/// the link will need.
const NATIVE_HEADER: &str = "native/embedded_mongodb_native.h";

/// What every entry point in that header is marked with, and the start of the two lines that
/// define the marker itself.
const MARKER: &str = "EMBEDDED_MONGODB_API";
const DEFINITION: &str = "#define EMBEDDED_MONGODB_API";

/// Hard-errors when the library is missing an entry point this checkout's bridge will call.
///
/// The coarse check, and the one that needs no git: it runs on every resolution path and in
/// every tree, which is what makes it the floor under consumers `check_not_stale` cannot
/// reach. It catches the mismatch that actually reaches people -- a library published before
/// an entry point existed -- while the library is being chosen, instead of leaving it to the
/// linker to report as `undefined symbol: embedded_mongodb_...` against a name the reader
/// has never seen.
///
/// It cannot see an entry point whose *meaning* changed: a struct that grew a field, an enum
/// that gained a value. Nothing but `build_freshness`'s commit comparison catches those,
/// which is why this does not replace it.
pub(crate) fn check_exports(library: &Path, crate_root: &Path) {
    let header_path = crate_root.join(NATIVE_HEADER);
    let header = fs::read_to_string(&header_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", header_path.display()));
    let expected = declared_entry_points(&header);
    // The marker appears once per declaration and once per arm of its own `#define`, so this
    // is how many entry points the header declares. Compared rather than trusted: every way
    // the reader below can fail to recognise a declaration would otherwise leave this
    // checking a smaller set than the header describes, and say nothing about it -- which is
    // the one failure a check like this must not have.
    let declared = header.matches(MARKER).count() - header.matches(DEFINITION).count();
    assert!(
        expected.len() == declared,
        "{} declares {declared} EMBEDDED_MONGODB_API entry point(s) but only {} could be read \
         from it ({}); the library cannot be checked against a set this crate cannot parse",
        header_path.display(),
        expected.len(),
        expected.join(", ")
    );

    // Whole, rather than streamed in windows: the manifest already hashes these same bytes
    // on every build, so the read is warm, and a name that straddled two windows would be
    // missed by a check whose whole value is that it cannot report a false mismatch.
    let bytes = fs::read(library)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", library.display()));
    let (present, missing): (Vec<&String>, Vec<&String>) =
        expected.iter().partition(|name| exports(&bytes, name));

    if present.is_empty() {
        // Not one name found means the scan could not read this file's symbol names at all
        // -- a container that stores them some other way -- rather than that the library
        // exports nothing. Failing a build on the check's own blind spot would be worse than
        // the mismatch it is looking for, so say what was not checked and leave it there.
        println!(
            "cargo:warning=embedded-mongodb: could not read symbol names from {}, so its entry \
             points were not checked against {NATIVE_HEADER}",
            library.display()
        );
        return;
    }
    if missing.is_empty() {
        return;
    }
    panic!(
        "the native library does not export the entry points this checkout needs:\n\n  {}\n\n  \
         library: {}\n\n  \
         The library predates their declaration in {NATIVE_HEADER}, so it is older than the \
         source beside it. Linking against it fails later with `undefined symbol`, which \
         says nothing about why.\n\n\
         Choose one:\n  \
         * build the engine yourself:     EMBEDDED_MONGODB_BUILD_FROM_SOURCE=1 cargo build\n  \
         * publish a build for this tree: gh workflow run native.yml --ref <this branch>\n  \
         * point at a current library:    EMBEDDED_MONGODB_NATIVE_LIB_DIR=<dir>\n",
        missing
            .iter()
            .map(|name| name.as_str())
            .collect::<Vec<_>>()
            .join("\n  "),
        library.display()
    );
}

/// The `EMBEDDED_MONGODB_API` functions a header declares.
///
/// Read from the header rather than listed here, so that adding an entry point cannot leave
/// this check quietly testing the old set.
fn declared_entry_points(header: &str) -> Vec<String> {
    header
        // Split on the marker rather than read line by line: these declarations wrap across
        // lines, and a rewrap that moved a name onto the next one would shrink the set this
        // checks. Whatever precedes the first marker declares nothing.
        .split(MARKER)
        .skip(1)
        // The first `embedded_mongodb_*` the parameter list follows. Not "the last token
        // before the first `(`", which an attribute or a comment carrying a bracket ahead of
        // the name would answer instead; the declared name is always the earliest such match
        // in its segment, because it opens the segment. The two `#define` arms carry no such
        // match and drop out here, which is what the count above accounts for.
        .filter_map(|declaration| entry_point_name(declaration).map(str::to_string))
        .collect()
}

fn entry_point_name(declaration: &str) -> Option<&str> {
    declaration
        .match_indices("embedded_mongodb_")
        .find_map(|(start, _)| {
            let end = start
                + declaration[start..]
                    .find(|character: char| !character.is_alphanumeric() && character != '_')?;
            declaration[end..]
                .trim_start()
                .starts_with('(')
                .then(|| &declaration[start..end])
        })
}

/// Whether `library` exports a symbol named `name`.
///
/// A byte search rather than an ELF or Mach-O parse, and the trailing NUL is what makes it
/// sound: an exported name is stored NUL-terminated in the file's string table, so a name
/// that is exported is always found. Mach-O's leading underscore sits before the name and
/// not after it, so one search covers both formats, every cross-compiled target, and needs
/// no host toolchain -- an Android library read on a Linux host included.
///
/// The converse does not hold: some unrelated string could end in these same bytes. That
/// leaves the check conservative in the only direction that is safe, able to miss a
/// mismatch but never to invent one.
fn exports(library: &[u8], name: &str) -> bool {
    let needle: Vec<u8> = name.bytes().chain(std::iter::once(0)).collect();
    library.windows(needle.len()).any(|window| window == needle)
}
