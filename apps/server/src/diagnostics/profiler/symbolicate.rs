//! Offline symbolication: function names and source lines for a crash's minidump or a panic's
//! frames, from Breakpad symbol stores laid out as `<module>/<debug id>/<module>.sym` (a `.pdb`
//! module's file is `<name>.sym`).
//!
//! Local stores are read first, with any release symbols fetched before. A release's
//! published symbols are downloaded only when asked and nothing local matched, into
//! `profiler-symbols/` beside the profiler directory, never inside it.

use std::{
    collections::HashMap,
    fs::{self, File},
    future::Future,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use breakpad_symbols::{
    FrameSymbolizer, SimpleModule, SimpleSymbolSupplier, SymbolFile, Symbolizer,
};
use minidump::Module as _;
use serde_json::{Value, json};
use thiserror::Error;

use super::reader::Reader;

/// The repository that publishes releases: the release workflow's `github.repository`, which
/// the desktop updater endpoint in `apps/desktop/src-tauri/tauri.release.conf.json` names too.
const RELEASE_REPOSITORY: &str = "mubeda/BibCode";
/// The desktop's `.sym` is about 250 MB unpacked.
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 1024 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A download that stalls this long fails.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Error)]
pub enum SymbolicateError {
    #[error("{0}")]
    NotFound(String),
    #[error("could not read the minidump: {0}")]
    Minidump(#[from] minidump::Error),
    #[error("could not walk the minidump's stacks: {0}")]
    Process(#[from] minidump_processor::ProcessError),
    #[error("could not download symbols: {0}")]
    Download(String),
    #[error("refused the symbol archive: {0}")]
    Archive(String),
    #[error("{}: {source}", .path.display())]
    Io { path: PathBuf, source: io::Error },
    #[error("could not start symbolication: {0}")]
    Runtime(io::Error),
}

/// An I/O error at `path`, which its message names.
fn at(path: &Path) -> impl FnOnce(io::Error) -> SymbolicateError + '_ {
    move |source| SymbolicateError::Io {
        path: path.to_owned(),
        source,
    }
}

/// What to symbolicate: a crash record by id, or a `panic` event by `<sid>:<seq>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Crash(String),
    Panic { sid: String, seq: u64 },
}

impl Target {
    /// `<sid>:<seq>` names a panic event; anything else is a crash id.
    pub fn parse(text: &str) -> Result<Self, String> {
        let Some((sid, seq)) = text.rsplit_once(':') else {
            return Ok(Self::Crash(text.to_owned()));
        };
        match seq.parse() {
            Ok(seq) if !sid.is_empty() => Ok(Self::Panic {
                sid: sid.to_owned(),
                seq,
            }),
            _ => Err(format!(
                "\"{text}\" is neither a crash id nor a panic event as <sid>:<seq>"
            )),
        }
    }
}

/// The release binary a stack comes from: each ships its symbols as a separate asset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolFamily {
    Desktop,
    Server,
}

impl SymbolFamily {
    /// From a crash sidecar's `processKind`; `crash-smoke` and the rest ship no symbols.
    fn from_process_kind(kind: &str) -> Option<Self> {
        match kind {
            "desktop" => Some(Self::Desktop),
            "server" => Some(Self::Server),
            _ => None,
        }
    }

    /// From a panic frame's module: the executable's name, or its PDB's on Windows.
    fn from_module(module: &str) -> Option<Self> {
        match module.trim_end_matches(".exe").trim_end_matches(".pdb") {
            "bibcode-desktop" | "bibcode_desktop" => Some(Self::Desktop),
            "bibcode" => Some(Self::Server),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Server => "server",
        }
    }
}

/// Symbolicates `target` with the stores in `symbols`, then any release symbols fetched before.
/// When nothing matched and `download` is set, fetches the release's symbols and tries again.
pub fn symbolicate(
    reader: &Reader,
    target: &Target,
    symbols: &[PathBuf],
    download: bool,
) -> Result<Value, SymbolicateError> {
    let cache = reader.symbol_cache();
    match target {
        Target::Crash(id) => {
            let (crash, dump) = reader.crash(id).ok_or_else(|| {
                SymbolicateError::NotFound(format!(
                    "no crash record {id}; `bibcode profiler crashes` lists them"
                ))
            })?;
            let dump = dump.ok_or_else(|| {
                SymbolicateError::NotFound(format!("crash record {id} has no minidump"))
            })?;
            let family = crash["processKind"]
                .as_str()
                .and_then(SymbolFamily::from_process_kind);
            with_release_symbols(
                release_of(&crash, family),
                symbols,
                &cache,
                download,
                |stores| symbolicate_dump(&dump, stores),
            )
        }
        Target::Panic { sid, seq } => {
            let event = reader
                .event(sid, *seq)
                .filter(|event| event["kind"] == "panic")
                .ok_or_else(|| SymbolicateError::NotFound(format!("no panic event {sid}:{seq}")))?;
            with_release_symbols(
                release_of(&event, panic_family(&event)),
                symbols,
                &cache,
                download,
                |stores| Ok(symbolicate_panic(&event, stores)),
            )
        }
    }
}

/// A release's symbols, as [`fetch_release_symbols`] takes them.
struct Release {
    family: SymbolFamily,
    version: String,
    platform: String,
    arch: String,
}

impl Release {
    /// The store a fetch left in `cache`, if there is one.
    fn cached(&self, cache: &Path) -> Option<PathBuf> {
        let (stem, _) =
            release_asset(self.family, &self.version, &self.platform, &self.arch).ok()?;
        Some(cache.join(stem)).filter(|store| store.is_dir())
    }
}

/// The release that built `record`, a crash sidecar or a panic event: both name the build's
/// `version`, `os` and `arch`. `family` is the binary's, when it is a release binary. Without
/// one, why there is nothing to download, and what to do instead.
fn release_of(record: &Value, family: Option<SymbolFamily>) -> Result<Release, String> {
    let family = family.ok_or(
        "these frames come from no BiBCode release binary; pass its symbol store with --symbols DIR",
    )?;
    let field = |key: &str| record.get(key).and_then(Value::as_str).map(str::to_owned);
    match (field("version"), field("os"), field("arch")) {
        (Some(version), Some(platform), Some(arch)) => Ok(Release {
            family,
            version,
            platform,
            arch,
        }),
        _ => Err(
            "this record predates version recording, so its release is unknown; pass its \
                  build's symbol store with --symbols DIR"
                .into(),
        ),
    }
}

/// The binary a panic's frames come from: the module of the frames that carry a debug ID.
fn panic_family(event: &Value) -> Option<SymbolFamily> {
    event["frames"]
        .as_array()?
        .iter()
        .filter(|frame| frame["debugId"].is_string())
        .find_map(|frame| SymbolFamily::from_module(frame["module"].as_str()?))
}

fn with_release_symbols(
    release: Result<Release, String>,
    symbols: &[PathBuf],
    cache: &Path,
    download: bool,
    run: impl Fn(&[PathBuf]) -> Result<Value, SymbolicateError>,
) -> Result<Value, SymbolicateError> {
    let mut stores = symbols.to_vec();
    stores.extend(
        release
            .as_ref()
            .ok()
            .and_then(|release| release.cached(cache)),
    );
    let symbolicated = run(&stores)?;
    if !download || names_any_frame(&symbolicated) {
        return Ok(symbolicated);
    }
    let release = release.map_err(SymbolicateError::Download)?;
    stores.push(fetch_release_symbols(
        release.family,
        &release.version,
        &release.platform,
        &release.arch,
        cache,
    )?);
    run(&stores)
}

fn names_any_frame(symbolicated: &Value) -> bool {
    symbolicated
        .pointer("/crashingThread/frames")
        .or_else(|| symbolicated.get("frames"))
        .and_then(Value::as_array)
        .is_some_and(|frames| frames.iter().any(|frame| frame["function"].is_string()))
}

/// The crash reason and the crashing thread's frames in `dump`, innermost first, named from
/// the `.sym` files in `symbols` (store directories, or single `.sym` files). A call inlined
/// into a frame is listed before it, as minidump-stackwalk lists it.
pub fn symbolicate_dump(dump: &Path, symbols: &[PathBuf]) -> Result<Value, SymbolicateError> {
    let dump = minidump::Minidump::read_path(dump)?;
    let provider = Symbolizer::new(SimpleSymbolSupplier::new(symbols.to_vec()));
    let state = block_on(minidump_processor::process_minidump(&dump, &provider))
        .map_err(SymbolicateError::Runtime)??;
    let crashing = state
        .requesting_thread
        .and_then(|index| state.threads.get(index));
    let frames = crashing.map(|thread| {
        let mut frames = Vec::new();
        for frame in &thread.frames {
            let module = frame.module.as_ref();
            let frame_value = |function: Option<&str>, file: Option<&str>, line: Option<u32>| {
                json!({
                    "module": module.map(|module| breakpad_symbols::basename(&module.code_file()).to_owned()),
                    "function": function,
                    "file": file,
                    "line": line,
                    "offset": module.map(|module| frame.instruction.wrapping_sub(module.base_address())),
                })
            };
            for inline in &frame.inlines {
                frames.push(frame_value(
                    Some(&inline.function_name),
                    inline.source_file_name.as_deref(),
                    inline.source_line,
                ));
            }
            frames.push(frame_value(
                frame.function_name.as_deref(),
                frame.source_file_name.as_deref(),
                frame.source_line,
            ));
        }
        frames
    });
    Ok(json!({
        "crashReason": state.exception_info.as_ref().map(|info| info.reason.to_string()),
        "crashingThread": frames.map(|frames| json!({ "frames": frames })),
    }))
}

/// `event`'s panic with its frames named: each `(module, debugId, offset)` that a `.sym` in
/// `symbols` covers gets its function, file and line, and a call inlined there is listed before
/// it, as for a minidump. Each `.sym` is parsed once.
pub fn symbolicate_panic(event: &Value, symbols: &[PathBuf]) -> Value {
    let mut loaded: HashMap<(&str, &str), Option<SymbolFile>> = HashMap::new();
    let mut frames = Vec::new();
    for frame in event["frames"].as_array().map_or(&[][..], Vec::as_slice) {
        let module = frame["module"].as_str();
        let offset = frame["offset"].as_u64();
        // A recorded offset is a return address; the call is the instruction before it.
        let mut named = Named {
            instruction: offset.unwrap_or(0).saturating_sub(1),
            ..Named::default()
        };
        if let (Some(module), Some(debug_id), Some(_)) = (module, frame["debugId"].as_str(), offset)
            && let Some(symbols) = loaded
                .entry((module, debug_id))
                .or_insert_with(|| load_symbols(symbols, module, debug_id))
        {
            symbols.fill_symbol(&SimpleModule::default(), &mut named);
        }
        let frame_value = |(function, file, line): (
            Option<String>,
            Option<String>,
            Option<u32>,
        )| {
            json!({ "module": module, "offset": offset, "function": function, "file": file, "line": line })
        };
        frames.extend(named.inlines.into_iter().rev().map(frame_value));
        frames.push(frame_value((named.function, named.file, named.line)));
    }
    json!({
        "thread": event["thread"],
        "message": event["message"],
        "location": event["location"],
        "frames": frames,
    })
}

/// What a `.sym` says about one address: its function and line, and the calls inlined there,
/// outermost first, each with the line it reached.
#[derive(Default)]
struct Named {
    instruction: u64,
    function: Option<String>,
    file: Option<String>,
    line: Option<u32>,
    inlines: Vec<(Option<String>, Option<String>, Option<u32>)>,
}

impl FrameSymbolizer for Named {
    fn get_instruction(&self) -> u64 {
        self.instruction
    }

    fn set_function(&mut self, name: &str, _base: u64, _parameter_size: u32) {
        self.function = Some(name.to_owned());
    }

    fn set_source_file(&mut self, file: &str, line: u32, _base: u64) {
        self.file = Some(file.to_owned());
        self.line = Some(line);
    }

    fn add_inline_frame(&mut self, name: &str, file: Option<&str>, line: Option<u32>) {
        self.inlines
            .push((Some(name.to_owned()), file.map(str::to_owned), line));
    }
}

/// The `.sym` for `module` and `debug_id` in the first store that holds it. The names come
/// from event files, so only a plain file name and a hexadecimal identifier are looked up.
fn load_symbols(stores: &[PathBuf], module: &str, debug_id: &str) -> Option<SymbolFile> {
    let plain = !matches!(module, "" | "." | "..") && !module.contains(['/', '\\', ':', '\0']);
    if !plain || debug_id.is_empty() || !debug_id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let leaf = format!("{}.sym", module.strip_suffix(".pdb").unwrap_or(module));
    stores.iter().find_map(|store| {
        let symbols = SymbolFile::from_file(&store.join(module).join(debug_id).join(&leaf)).ok()?;
        symbols
            .module_id
            .eq_ignore_ascii_case(debug_id)
            .then_some(symbols)
    })
}

/// The release asset with `family`'s symbols, as its file stem and its download URL.
/// `platform` and `arch` are spelled as `std::env::consts` spells them, which is how crash
/// sidecars record them; the asset uses the server archives' names (`darwin` for macOS).
fn release_asset(
    family: SymbolFamily,
    version: &str,
    platform: &str,
    arch: &str,
) -> Result<(String, String), SymbolicateError> {
    let unknown = || {
        SymbolicateError::Download(format!(
            "no release symbols for version {version:?} on {platform:?} {arch:?}"
        ))
    };
    let os = match platform {
        "macos" | "darwin" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        _ => return Err(unknown()),
    };
    let plain_version = !version.is_empty()
        && version.len() <= 64
        && !version.contains("..")
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'));
    if !plain_version || !matches!(arch, "x86_64" | "aarch64") {
        return Err(unknown());
    }
    let stem = format!("bibcode-{}-symbols-{version}-{os}-{arch}", family.name());
    let url =
        format!("https://github.com/{RELEASE_REPOSITORY}/releases/download/v{version}/{stem}.zip");
    Ok((stem, url))
}

/// The symbol store of a release binary, downloaded and unpacked into `cache` the first time
/// it is asked for: HTTPS only, at most 512 MiB downloaded and 1 GiB unpacked.
pub fn fetch_release_symbols(
    family: SymbolFamily,
    version: &str,
    platform: &str,
    arch: &str,
    cache: &Path,
) -> Result<PathBuf, SymbolicateError> {
    let (stem, url) = release_asset(family, version, platform, arch)?;
    let store = cache.join(&stem);
    if store.is_dir() {
        return Ok(store);
    }
    fs::create_dir_all(cache).map_err(at(cache))?;
    // Unpacked under a name of its own, then renamed into place: a failed or concurrent fetch
    // never leaves a partial store where a finished one is expected.
    let unique = uuid::Uuid::new_v4();
    let archive = cache.join(format!(".{stem}-{unique}.zip"));
    let unpacked = cache.join(format!(".{stem}-{unique}"));
    let fetched = block_on(download(&url, &archive, MAX_DOWNLOAD_BYTES))
        .map_err(SymbolicateError::Runtime)
        .and_then(|downloaded| downloaded)
        .and_then(|()| unpack(&archive, &unpacked, MAX_UNPACKED_BYTES))
        .and_then(|()| fs::rename(&unpacked, &store).map_err(at(&store)));
    let _ = fs::remove_file(&archive);
    if let Err(error) = fetched {
        let _ = fs::remove_dir_all(&unpacked);
        // Unless another fetch of the same release got there first.
        if !store.is_dir() {
            return Err(error);
        }
    }
    Ok(store)
}

async fn download(url: &str, to: &Path, max_bytes: u64) -> Result<(), SymbolicateError> {
    let failed = |error: reqwest::Error| SymbolicateError::Download(error.to_string());
    let too_large =
        || SymbolicateError::Download(format!("{url} is larger than {} MiB", max_bytes >> 20));
    let client = reqwest::Client::builder()
        .https_only(true)
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT)
        .user_agent(concat!("bibcode/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(failed)?;
    let response = client.get(url).send().await.map_err(failed)?;
    // A release from before symbols were published, or a build that was never released.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(SymbolicateError::Download(format!(
            "the release publishes no symbols at {url}"
        )));
    }
    let mut response = response.error_for_status().map_err(failed)?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(too_large());
    }
    let mut file = File::create_new(to).map_err(at(to))?;
    let mut received = 0;
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        received += chunk.len() as u64;
        if received > max_bytes {
            return Err(too_large());
        }
        file.write_all(&chunk).map_err(at(to))?;
    }
    Ok(())
}

/// Unpacks `archive` into the new directory `into`. The whole archive is refused for an entry
/// that is a link or names a path outside `into`, or when it unpacks to more than `max_bytes`.
fn unpack(archive: &Path, into: &Path, max_bytes: u64) -> Result<(), SymbolicateError> {
    let refused = |reason: String| SymbolicateError::Archive(reason);
    let mut zip = zip::ZipArchive::new(File::open(archive).map_err(at(archive))?)
        .map_err(|error| refused(error.to_string()))?;
    fs::create_dir(into).map_err(at(into))?;
    let mut unpacked = 0;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| refused(error.to_string()))?;
        let name = entry
            .name()
            .map_err(|error| refused(error.to_string()))?
            .into_owned();
        let Some(relative) = archive_path(&name).filter(|_| !entry.is_symlink()) else {
            return Err(refused(format!("unsafe entry {name:?}")));
        };
        if entry.is_dir() {
            continue;
        }
        let path = into.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(at(parent))?;
        }
        let mut file = File::create_new(&path).map_err(at(&path))?;
        // The sizes in the archive's headers are the archive's word; count what is written.
        unpacked += io::copy(&mut (&mut entry).take(max_bytes - unpacked + 1), &mut file)
            .map_err(at(&path))?;
        if unpacked > max_bytes {
            return Err(refused(format!(
                "it unpacks to more than {} MiB",
                max_bytes >> 20
            )));
        }
    }
    Ok(())
}

/// An archive entry's name as a relative path of plain components: `None` for an absolute
/// path, a drive, or an empty, `.` or `..` component.
fn archive_path(name: &str) -> Option<PathBuf> {
    let name = name.strip_suffix('/').unwrap_or(name);
    let parts: Vec<&str> = name.split(['/', '\\']).collect();
    parts
        .iter()
        .all(|part| !matches!(*part, "" | "." | "..") && !part.contains([':', '\0']))
        .then(|| parts.iter().collect())
}

/// Runs `future` to completion on a runtime of its own. The callers are blocking code: the CLI
/// before any runtime exists, or on a `spawn_blocking` thread.
fn block_on<F: Future>(future: F) -> io::Result<F::Output> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(future))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    /// The `.sym` of the task brief: `my_fn` covers 0x1000 to 0x1020, all of it line 42.
    const SYM: &str = "MODULE linux x86_64 ABC123 bibcode\nFILE 0 src/lib.rs\nFUNC 1000 20 0 my_fn\n1000 20 42 0\n";

    fn store_sym(store: &Path, module: &str, id: &str, leaf: &str, text: &str) {
        let dir = store.join(module).join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(leaf), text).unwrap();
    }

    fn frame(module: &str, debug_id: Option<&str>, offset: u64) -> Value {
        json!({ "module": module, "debugId": debug_id, "offset": offset })
    }

    #[test]
    fn panic_frames_are_named_from_the_sym_file_of_their_module_and_debug_id() {
        let symbols = tempfile::tempdir().unwrap();
        store_sym(symbols.path(), "bibcode", "ABC123", "bibcode.sym", SYM);
        let event = json!({
            "thread": "main", "message": "boom", "location": "src/lib.rs:42",
            "frames": [
                frame("bibcode", Some("ABC123"), 0x1010),
                frame("bibcode", Some("ABC123"), 0x9000),
                frame("libc.so.6", None, 0x29d90),
                frame("bibcode", Some("DEF456"), 0x1010),
                frame("../bibcode", Some("ABC123"), 0x1010),
            ],
        });

        let named = symbolicate_panic(&event, &[symbols.path().to_path_buf()]);

        assert_eq!(
            named["frames"][0],
            json!({
                "module": "bibcode", "offset": 0x1010, "function": "my_fn",
                "file": "src/lib.rs", "line": 42,
            })
        );
        for index in 1..5 {
            assert!(
                named["frames"][index]["function"].is_null(),
                "{}",
                named["frames"][index]
            );
        }
        assert_eq!(named["frames"][2]["module"], "libc.so.6");
        assert_eq!(named["message"], "boom");
        assert_eq!(named["location"], "src/lib.rs:42");
        assert!(names_any_frame(&named));
        assert!(!names_any_frame(&symbolicate_panic(&event, &[])));
    }

    /// A panic inside a call the compiler inlined names that call first, at its own line, then
    /// the function it was inlined into, at the call site.
    #[test]
    fn a_call_inlined_at_a_panic_frame_is_listed_before_its_function() {
        let symbols = tempfile::tempdir().unwrap();
        let inlined = "MODULE linux x86_64 ABC123 bibcode\nFILE 0 src/lib.rs\nFILE 1 src/helper.rs\n\
            INLINE_ORIGIN 0 helper_fn\nFUNC 1000 20 0 my_fn\nINLINE 0 42 0 0 1008 8\n\
            1000 8 41 0\n1008 8 7 1\n1010 10 43 0\n";
        store_sym(symbols.path(), "bibcode", "ABC123", "bibcode.sym", inlined);
        let event = json!({ "frames": [
            frame("bibcode", Some("ABC123"), 0x100d),
            frame("bibcode", Some("ABC123"), 0x1018),
        ]});

        let named = symbolicate_panic(&event, &[symbols.path().to_path_buf()]);

        let line = |index: usize| {
            let frame = &named["frames"][index];
            (
                frame["function"].clone(),
                frame["file"].clone(),
                frame["line"].clone(),
                frame["offset"].clone(),
            )
        };
        assert_eq!(named["frames"].as_array().unwrap().len(), 3, "{named}");
        assert_eq!(
            line(0),
            (
                json!("helper_fn"),
                json!("src/helper.rs"),
                json!(7),
                json!(0x100d)
            )
        );
        assert_eq!(
            line(1),
            (
                json!("my_fn"),
                json!("src/lib.rs"),
                json!(42),
                json!(0x100d)
            )
        );
        assert_eq!(
            line(2),
            (
                json!("my_fn"),
                json!("src/lib.rs"),
                json!(43),
                json!(0x1018)
            )
        );
    }

    /// Windows names a module after its PDB, and its store file is `<name>.sym`. A file whose
    /// MODULE record names another build is not used.
    #[test]
    fn a_pdb_module_reads_its_sym_and_a_mismatched_module_record_is_ignored() {
        let symbols = tempfile::tempdir().unwrap();
        let windows = SYM.replace(
            "linux x86_64 ABC123 bibcode",
            "windows x86_64 ABC1231 bibcode.pdb",
        );
        store_sym(
            symbols.path(),
            "bibcode.pdb",
            "ABC1231",
            "bibcode.sym",
            &windows,
        );
        store_sym(symbols.path(), "other", "FFFF", "other.sym", SYM);
        let event = json!({ "frames": [
            frame("bibcode.pdb", Some("ABC1231"), 0x1010),
            frame("other", Some("FFFF"), 0x1010),
        ]});

        let named = symbolicate_panic(&event, &[symbols.path().to_path_buf()]);

        assert_eq!(named["frames"][0]["function"], "my_fn");
        assert!(named["frames"][1]["function"].is_null());
    }

    fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for (name, text) in entries {
            zip.start_file(*name, SimpleFileOptions::default()).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn a_symbol_archive_unpacks_to_its_store_layout_within_the_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("symbols.zip");
        fs::write(
            &archive,
            zip_of(&[("bibcode/ABC123/", ""), ("bibcode/ABC123/bibcode.sym", SYM)]),
        )
        .unwrap();

        unpack(&archive, &dir.path().join("store"), 1024).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("store/bibcode/ABC123/bibcode.sym")).unwrap(),
            SYM
        );

        let error = unpack(&archive, &dir.path().join("small"), 16).unwrap_err();
        assert!(
            matches!(&error, SymbolicateError::Archive(reason) if reason.contains("more than")),
            "{error}"
        );
    }

    /// Zip slip: an entry that climbs out, is absolute, names a drive or is a link refuses the
    /// whole archive, and nothing lands outside the target directory.
    #[test]
    fn an_archive_with_an_unsafe_entry_is_refused_whole() {
        let dir = tempfile::tempdir().unwrap();
        let mut archives: Vec<(String, Vec<u8>)> = [
            "../escape.sym",
            "bibcode/../../escape.sym",
            "/escape.sym",
            "\\escape.sym",
            "C:/escape.sym",
            "bibcode/./x.sym",
            "bibcode/..\\..\\escape.sym",
        ]
        .iter()
        .map(|name| (name.to_string(), zip_of(&[("ok.sym", SYM), (name, SYM)])))
        .collect();
        let mut link = ZipWriter::new(Cursor::new(Vec::new()));
        link.add_symlink("bibcode", "/etc", SimpleFileOptions::default())
            .unwrap();
        archives.push(("a symlink".into(), link.finish().unwrap().into_inner()));

        for (index, (name, bytes)) in archives.iter().enumerate() {
            let work = dir.path().join(format!("case-{index}"));
            fs::create_dir(&work).unwrap();
            let archive = work.join("symbols.zip");
            fs::write(&archive, bytes).unwrap();
            let into = work.join("nested/store");
            fs::create_dir_all(into.parent().unwrap()).unwrap();

            let error = unpack(&archive, &into, 1 << 20).unwrap_err();

            assert!(
                matches!(error, SymbolicateError::Archive(_)),
                "{name}: {error}"
            );
            for escaped in [work.join("escape.sym"), work.join("nested/escape.sym")] {
                assert!(!escaped.exists(), "{name}: {}", escaped.display());
            }
        }
        assert!(!Path::new("/escape.sym").exists());
    }

    #[test]
    fn release_assets_use_the_release_workflow_names() {
        let (stem, url) = release_asset(SymbolFamily::Server, "0.9.0", "macos", "aarch64").unwrap();
        assert_eq!(stem, "bibcode-server-symbols-0.9.0-darwin-aarch64");
        assert_eq!(
            url,
            "https://github.com/mubeda/BibCode/releases/download/v0.9.0/bibcode-server-symbols-0.9.0-darwin-aarch64.zip"
        );
        let (stem, _) =
            release_asset(SymbolFamily::Desktop, "1.2.3-rc.1", "windows", "x86_64").unwrap();
        assert_eq!(stem, "bibcode-desktop-symbols-1.2.3-rc.1-windows-x86_64");
        for (version, platform, arch) in [
            ("../0.9.0", "linux", "x86_64"),
            ("0.9.0/x", "linux", "x86_64"),
            ("", "linux", "x86_64"),
            ("0.9.0", "freebsd", "x86_64"),
            ("0.9.0", "linux", "riscv64"),
        ] {
            assert!(
                release_asset(SymbolFamily::Server, version, platform, arch).is_err(),
                "{version} {platform} {arch}"
            );
        }
        // The desktop updater downloads from the same repository's releases.
        let config = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../desktop/src-tauri/tauri.release.conf.json"),
        )
        .unwrap();
        assert!(config.contains(&format!(
            "https://github.com/{RELEASE_REPOSITORY}/releases/"
        )));
    }

    #[test]
    fn families_come_from_the_process_kind_or_the_module() {
        assert_eq!(
            SymbolFamily::from_process_kind("desktop"),
            Some(SymbolFamily::Desktop)
        );
        assert_eq!(
            SymbolFamily::from_process_kind("server"),
            Some(SymbolFamily::Server)
        );
        assert_eq!(SymbolFamily::from_process_kind("crash-smoke"), None);
        for (module, family) in [
            ("bibcode", Some(SymbolFamily::Server)),
            ("bibcode.pdb", Some(SymbolFamily::Server)),
            ("bibcode-desktop", Some(SymbolFamily::Desktop)),
            ("bibcode_desktop.pdb", Some(SymbolFamily::Desktop)),
            ("crash-smoke", None),
        ] {
            assert_eq!(SymbolFamily::from_module(module), family, "{module}");
        }
    }

    #[test]
    fn targets_are_crash_ids_or_panic_references() {
        assert_eq!(
            Target::parse("3f2a8c1e-0b7d-4e59-9c1a-2d4b6e8f0a12"),
            Ok(Target::Crash("3f2a8c1e-0b7d-4e59-9c1a-2d4b6e8f0a12".into()))
        );
        assert_eq!(
            Target::parse("aaaa1111:42"),
            Ok(Target::Panic {
                sid: "aaaa1111".into(),
                seq: 42
            })
        );
        for bad in ["aaaa1111:x", ":42", "aaaa1111:-1"] {
            assert!(Target::parse(bad).is_err(), "{bad}");
        }
    }

    fn profiler_dir(state: &Path) -> PathBuf {
        let dir = state.join("profiler");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A panic as the hook records it, by a 0.1.0 Linux x86-64 build.
    fn write_panic(dir: &Path, frames: Value) {
        let event = json!({
            "v": 1, "seq": 7, "sid": "aaaa1111", "ts": "2026-10-09T10:00:00Z", "kind": "panic",
            "source": "server", "thread": "main", "message": "boom", "location": "src/lib.rs:42",
            "frames": frames, "version": "0.1.0", "os": "linux", "arch": "x86_64",
        });
        fs::write(dir.join("events-000000.ndjson"), format!("{event}\n")).unwrap();
    }

    /// Symbols fetched before sit in `profiler-symbols/` beside the profiler directory, and
    /// are read without `--symbols` and without a download. They are the symbols of the build
    /// that recorded the panic, whichever build reads it.
    #[test]
    fn release_symbols_fetched_before_are_read_from_the_cache() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        write_panic(&dir, json!([frame("bibcode", Some("ABC123"), 0x1010)]));
        let cache = state.path().join("profiler-symbols");
        let store = cache.join("bibcode-server-symbols-0.1.0-linux-x86_64");
        store_sym(&store, "bibcode", "ABC123", "bibcode.sym", SYM);
        let target = Target::Panic {
            sid: "aaaa1111".into(),
            seq: 7,
        };

        let named = symbolicate(&Reader::new(&dir), &target, &[], false).unwrap();

        assert_eq!(named["frames"][0]["function"], "my_fn");
        assert_eq!(
            fetch_release_symbols(SymbolFamily::Server, "0.1.0", "linux", "x86_64", &cache)
                .unwrap(),
            store
        );
    }

    /// Nothing is downloaded unless asked, and a binary that is no release, such as the crash
    /// smoke, has nothing to download.
    #[test]
    fn a_download_needs_the_flag_and_a_release_binary() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        write_panic(&dir, json!([frame("crash-smoke", Some("ABC123"), 0x1010)]));
        let reader = Reader::new(&dir);
        let target = Target::parse("aaaa1111:7").unwrap();

        let unnamed = symbolicate(&reader, &target, &[], false).unwrap();
        assert!(unnamed["frames"][0]["function"].is_null());
        let error = symbolicate(&reader, &target, &[], true).unwrap_err();
        assert!(matches!(error, SymbolicateError::Download(_)), "{error}");
        assert!(!state.path().join("profiler-symbols").exists());

        let missing = symbolicate(&reader, &Target::parse("aaaa1111:8").unwrap(), &[], false);
        assert!(matches!(missing, Err(SymbolicateError::NotFound(_))));
        let missing = symbolicate(&reader, &Target::Crash("nope".into()), &[], false);
        assert!(matches!(missing, Err(SymbolicateError::NotFound(_))));
    }

    /// A panic recorded before panic events named their build has no release to download:
    /// the error says so and points at local symbols.
    #[test]
    fn a_panic_without_its_build_points_at_local_symbols() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let event = json!({
            "v": 1, "seq": 7, "sid": "aaaa1111", "ts": "2026-10-09T10:00:00Z", "kind": "panic",
            "source": "server", "frames": [frame("bibcode", Some("ABC123"), 0x1010)],
        });
        fs::write(dir.join("events-000000.ndjson"), format!("{event}\n")).unwrap();

        let error = symbolicate(
            &Reader::new(&dir),
            &Target::parse("aaaa1111:7").unwrap(),
            &[],
            true,
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("predates version recording"), "{error}");
        assert!(error.contains("--symbols DIR"), "{error}");
        assert!(!state.path().join("profiler-symbols").exists());
    }

    /// A file system failure names the path it happened at.
    #[test]
    fn an_io_error_names_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("not-a-directory");
        fs::write(&blocker, "").unwrap();

        let error = fetch_release_symbols(
            SymbolFamily::Server,
            "0.1.0",
            "linux",
            "x86_64",
            &blocker.join("cache"),
        )
        .unwrap_err();

        assert!(matches!(error, SymbolicateError::Io { .. }), "{error}");
        assert!(error.to_string().contains("not-a-directory"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_a_minidump_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let dump = dir.path().join("crash.dmp");
        fs::write(&dump, "not a minidump").unwrap();

        let error = symbolicate_dump(&dump, &[]).unwrap_err();

        assert!(matches!(error, SymbolicateError::Minidump(_)), "{error}");
    }
}
