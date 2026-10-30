//! Live Fontconfig matching with a dependency-validated warm match cache.
//! The cache never supplies font bytes: the selected face is always opened and parsed.
use ab_glyph::{FontArc, FontVec};
use fontconfig_sys::statics::LIB;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::{CStr, OsString},
    fs::{self, Metadata, OpenOptions},
    io::{self, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAGIC: &[u8] = b"jofi-font-match-v1\0";
const LIMIT: usize = 100_000;
type Stamp = [u64; 11];

#[derive(Clone, Debug, PartialEq, Eq)]
struct Dependency {
    path: PathBuf,
    link: Option<Stamp>,
    target: Option<Stamp>,
}

#[derive(Debug)]
struct Manifest {
    key: Vec<u8>,
    font: PathBuf,
    index: u32,
    dependencies: Vec<Dependency>,
}

fn stamp(m: &Metadata) -> Stamp {
    [
        m.dev(),
        m.ino(),
        u64::from(m.mode()),
        m.nlink(),
        u64::from(m.uid()),
        u64::from(m.gid()),
        m.size(),
        m.mtime() as u64,
        m.mtime_nsec() as u64,
        m.ctime() as u64,
        m.ctime_nsec() as u64,
    ]
}

fn optional_metadata(path: &Path, follow: bool) -> io::Result<Option<Metadata>> {
    let result = if follow {
        fs::metadata(path)
    } else {
        fs::symlink_metadata(path)
    };
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn dependency(path: &Path) -> io::Result<(Dependency, Option<Metadata>)> {
    let link = optional_metadata(path, false)?;
    let target = if link.as_ref().is_some_and(|m| m.is_symlink()) {
        optional_metadata(path, true)?
    } else {
        link.clone()
    };
    Ok((
        Dependency {
            path: path.to_owned(),
            link: link.as_ref().map(stamp),
            target: target.as_ref().map(stamp),
        },
        target,
    ))
}

/// Record directory entries recursively once. On a warm hit, stat each saved entry:
/// directory ctime/mtime detects additions, including creation below a missing root.
/// Both lstat and stat signatures detect symlink replacement and target edits.
fn snapshot(roots: &[PathBuf]) -> io::Result<Vec<Dependency>> {
    fn visit(
        path: &Path,
        entries: &mut BTreeMap<PathBuf, Dependency>,
        seen: &mut BTreeSet<(u64, u64)>,
    ) -> io::Result<()> {
        if entries.contains_key(path) {
            return Ok(());
        }
        if entries.len() >= LIMIT {
            return Err(io::Error::other("font dependency manifest too large"));
        }
        let (entry, metadata) = dependency(path)?;
        entries.insert(path.to_owned(), entry);
        if let Some(m) = metadata.filter(|m| m.is_dir()) {
            if seen.insert((m.dev(), m.ino())) {
                for child in fs::read_dir(path)? {
                    visit(&child?.path(), entries, seen)?;
                }
            }
        }
        Ok(())
    }
    let mut entries = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        visit(root, &mut entries, &mut seen)?;
    }
    Ok(entries.into_values().collect())
}

fn append_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    output.extend_from_slice(bytes);
}

fn environment_key() -> Vec<u8> {
    // Keep unset distinct from empty, and preserve non-UTF8 paths and locale values.
    let relevant = |name: &std::ffi::OsStr| {
        let n = name.as_bytes();
        n.starts_with(b"FONTCONFIG_")
            || n.starts_with(b"FC_")
            || n.starts_with(b"LC_")
            || n.starts_with(b"XDG_")
            || n.starts_with(b"LD_")
            || matches!(n, b"HOME" | b"LANG" | b"LANGUAGE")
    };
    let vars: BTreeMap<_, _> = std::env::vars_os()
        .filter(|(name, _)| relevant(name))
        .collect();
    let mut key = Vec::new();
    for (name, value) in vars {
        append_bytes(&mut key, name.as_bytes());
        append_bytes(&mut key, value.as_bytes());
    }
    // Relative includes and relative FONTCONFIG_* paths are sensitive to cwd.
    append_bytes(
        &mut key,
        std::env::current_dir()
            .unwrap_or_default()
            .as_os_str()
            .as_bytes(),
    );
    key
}

/// These defaults can be absent from Fontconfig's list of *loaded* files. Watch
/// missing user roots too, so installing a user font/config invalidates a match.
fn default_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let xdg = |name, fallback: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(fallback))
    };
    let mut roots = vec![
        home.join(".fonts.conf"),
        home.join(".fonts.conf.d"),
        home.join(".fonts"),
        xdg("XDG_CONFIG_HOME", ".config").join("fontconfig"),
        xdg("XDG_DATA_HOME", ".local/share").join("fonts"),
        xdg("XDG_CACHE_HOME", ".cache").join("fontconfig"),
    ];
    if let Some(paths) = std::env::var_os("FONTCONFIG_PATH") {
        roots.extend(std::env::split_paths(&paths));
    }
    if let Some(path) = std::env::var_os("FONTCONFIG_FILE") {
        roots.push(PathBuf::from(path));
    }
    // A library upgrade can change default substitutions independently of configs.
    if let Ok(maps) = fs::read_to_string("/proc/self/maps") {
        for line in maps.lines() {
            if let Some(path) = line.split_whitespace().nth(5).filter(|p| {
                p.starts_with('/')
                    && (p.contains("libfontconfig.so") || p.contains("libfreetype.so"))
            }) {
                roots.push(PathBuf::from(path));
            }
        }
    }
    roots.push(PathBuf::from("/etc/ld.so.cache"));
    roots.push(PathBuf::from("/etc/ld.so.preload"));
    if let Ok(exe) = std::env::current_exe() {
        roots.push(exe);
    }
    roots
}

fn configuration_roots() -> Option<Vec<PathBuf>> {
    // Sysroot remapping varies across Fontconfig releases. Fail closed instead
    // of treating a host path as a dependency of a remapped configuration.
    if std::env::var_os("FONTCONFIG_SYSROOT").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let mut roots = default_roots();
    let mut files = Vec::new();
    let mut complete = true;
    // SAFETY: Fontconfig::new has successfully initialized the process-global
    // configuration on this one font worker. Null means the current config in
    // these APIs. The returned FcStrList iterators belong to the caller, strings
    // remain owned by Fontconfig and are copied before FcStrListDone. No config
    // object is destroyed or mutated here. See Fontconfig developer reference:
    // FcConfigGetConfigFiles/ConfigDirs/FontDirs/CacheDirs and FcStrListNext/Done.
    unsafe {
        let config = std::ptr::null_mut();
        for (kind, list) in [
            (LIB.FcConfigGetConfigFiles)(config),
            (LIB.FcConfigGetConfigDirs)(config),
            (LIB.FcConfigGetFontDirs)(config),
            (LIB.FcConfigGetCacheDirs)(config),
        ]
        .into_iter()
        .enumerate()
        {
            if list.is_null() {
                complete = false;
                continue;
            }
            loop {
                let item = (LIB.FcStrListNext)(list);
                if item.is_null() {
                    break;
                }
                let path = PathBuf::from(OsString::from_vec(
                    CStr::from_ptr(item.cast()).to_bytes().to_vec(),
                ));
                if kind == 0 {
                    files.push(path.clone());
                }
                roots.push(path);
            }
            (LIB.FcStrListDone)(list);
        }
    }
    if !complete {
        return None;
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    for file in files {
        let metadata = fs::metadata(&file).ok()?;
        if metadata.is_dir() {
            continue;
        }
        if !metadata.is_file() {
            return None;
        }
        let xml = fs::read_to_string(&file).ok()?;
        roots.extend(include_roots(&xml, &home, &xdg, &|name| {
            let name = std::ffi::CString::new(name).ok()?;
            // SAFETY: called only after successful FcInit. CString lives through
            // the call. FcConfigFilename returns a newly allocated string that
            // must be freed using FcStrFree, not Rust or libc allocators.
            unsafe {
                let path = (LIB.FcConfigFilename)(name.as_ptr().cast());
                if path.is_null() {
                    return None;
                }
                let owned = PathBuf::from(OsString::from_vec(
                    CStr::from_ptr(path.cast()).to_bytes().to_vec(),
                ));
                (LIB.FcStrFree)(path);
                Some(owned)
            }
        })?);
    }
    Some(roots)
}

/// Fontconfig omits missing includes from GetConfigFiles. Parse loaded XML on
/// the cold path to watch absolute, home and XDG missing includes as well. An
/// unresolved relative include has an opaque compiled-in search path, so do
/// not cache that configuration rather than silently miss a future creation.
fn include_roots(
    xml: &str,
    home: &Path,
    xdg: &Path,
    resolve: &impl Fn(&str) -> Option<PathBuf>,
) -> Option<Vec<PathBuf>> {
    let document = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
    .ok()?;
    let mut roots = Vec::new();
    for node in document
        .descendants()
        .filter(|node| node.has_tag_name("include"))
    {
        let name: String = node
            .children()
            .filter(|n| n.is_text())
            .filter_map(|n| n.text())
            .collect();
        if name.is_empty() {
            return None;
        }
        let path = if node.attribute("prefix") == Some("xdg") {
            // Fontconfig concatenates even if include text starts with slash.
            let mut path = xdg.as_os_str().to_os_string();
            path.push("/");
            path.push(&name);
            PathBuf::from(path)
        } else if let Some(tail) = name.strip_prefix('~') {
            home.join(tail.trim_start_matches('/'))
        } else if Path::new(&name).is_absolute() {
            PathBuf::from(name)
        } else {
            resolve(&name)?
        };
        roots.push(path);
    }
    Some(roots)
}

fn checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |hash, b| {
        (hash ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

impl Manifest {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        append_bytes(&mut bytes, &self.key);
        append_bytes(&mut bytes, self.font.as_os_str().as_bytes());
        bytes.extend_from_slice(&u64::from(self.index).to_le_bytes());
        bytes.extend_from_slice(&(self.dependencies.len() as u64).to_le_bytes());
        for dep in &self.dependencies {
            append_bytes(&mut bytes, dep.path.as_os_str().as_bytes());
            for stamp in [&dep.link, &dep.target] {
                bytes.push(u8::from(stamp.is_some()));
                if let Some(stamp) = stamp {
                    for field in stamp {
                        bytes.extend_from_slice(&field.to_le_bytes());
                    }
                }
            }
        }
        bytes.extend_from_slice(&checksum(&bytes).to_le_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        struct Reader<'a>(&'a [u8]);
        impl<'a> Reader<'a> {
            fn take(&mut self, n: usize) -> Option<&'a [u8]> {
                let value = self.0.get(..n)?;
                self.0 = &self.0[n..];
                Some(value)
            }
            fn number(&mut self) -> Option<u64> {
                Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
            }
            fn bytes(&mut self) -> Option<Vec<u8>> {
                let n = usize::try_from(self.number()?).ok()?;
                Some(self.take(n)?.to_vec())
            }
            fn stamp(&mut self) -> Option<Option<Stamp>> {
                match self.take(1)?[0] {
                    0 => Some(None),
                    1 => {
                        let mut s = [0; 11];
                        for n in &mut s {
                            *n = self.number()?;
                        }
                        Some(Some(s))
                    }
                    _ => None,
                }
            }
        }
        let (payload, digest) = bytes.split_at_checked(bytes.len().checked_sub(8)?)?;
        if checksum(payload) != u64::from_le_bytes(digest.try_into().ok()?) {
            return None;
        }
        let mut r = Reader(payload.strip_prefix(MAGIC)?);
        let key = r.bytes()?;
        let font = PathBuf::from(OsString::from_vec(r.bytes()?));
        let index = r.number()?.try_into().ok()?;
        let count = usize::try_from(r.number()?).ok()?;
        if count == 0 || count > LIMIT {
            return None;
        }
        let mut dependencies = Vec::new();
        for _ in 0..count {
            dependencies.push(Dependency {
                path: PathBuf::from(OsString::from_vec(r.bytes()?)),
                link: r.stamp()?,
                target: r.stamp()?,
            });
        }
        if !r.0.is_empty()
            || !dependencies
                .iter()
                .any(|d| d.path == font && d.target.is_some())
        {
            return None;
        }
        Some(Self {
            key,
            font,
            index,
            dependencies,
        })
    }

    fn valid(&self, key: &[u8]) -> bool {
        self.key == key
            && self
                .dependencies
                .iter()
                .all(|dep| dependency(&dep.path).is_ok_and(|(now, _)| now == *dep))
    }
}

fn read_manifest(path: &Path, key: &[u8]) -> Option<Manifest> {
    let file = fs::File::open(path).ok()?;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > 32 * 1024 * 1024 {
        return None;
    }
    let manifest = Manifest::decode(&bytes)?;
    manifest.valid(key).then_some(manifest)
}

fn write_manifest(path: &Path, manifest: &Manifest) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing cache parent"))?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(
        ".font-match-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(&manifest.encode())?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn fresh_match(key: Vec<u8>, cache: &Path) -> Result<(PathBuf, u32), Box<dyn Error>> {
    let begin = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let config = fontconfig::Fontconfig::new().ok_or("could not initialize Fontconfig")?;
    let matched = config
        .find("monospace", None)
        .ok_or("Fontconfig could not find a monospace font")?;
    let index = matched.index.unwrap_or(0).try_into()?;
    if let Some(mut roots) = configuration_roots() {
        roots.push(matched.path.clone());
        if let Ok(dependencies) = snapshot(&roots) {
            // Do not cache a match if any dependency changed during FcInit/find.
            // The snapshot must describe the data actually used, not a newer edit.
            let stable = dependencies
                .iter()
                .flat_map(|d| [&d.link, &d.target])
                .flatten()
                .all(|s| {
                    [7, 9]
                        .iter()
                        .all(|&i| u128::from(s[i]) * 1_000_000_000 + u128::from(s[i + 1]) < begin)
                });
            let manifest = Manifest {
                key,
                font: matched.path.clone(),
                index,
                dependencies,
            };
            if stable && manifest.valid(&manifest.key) {
                let _ = write_manifest(cache, &manifest);
            }
        }
    }
    Ok((matched.path, index))
}

#[cfg(test)]
fn resolve() -> Result<(PathBuf, u32), Box<dyn Error>> {
    let config = fontconfig::Fontconfig::new().ok_or("could not initialize Fontconfig")?;
    let matched = config
        .find("monospace", None)
        .ok_or("Fontconfig could not find a monospace font")?;
    Ok((matched.path, matched.index.unwrap_or(0).try_into()?))
}

pub type Pending = std::thread::JoinHandle<Result<FontArc, String>>;
pub fn start() -> Pending {
    std::thread::spawn(|| load().map_err(|error| error.to_string()))
}

fn open_font(path: &Path, index: u32) -> Result<FontArc, Box<dyn Error>> {
    Ok(FontVec::try_from_vec_and_index(fs::read(path)?, index)?.into())
}

pub fn load() -> Result<FontArc, Box<dyn Error>> {
    crate::startup::mark("font lookup begin");
    let cache = crate::catalog::cache_dir().join("font-match");
    let key = environment_key();
    if let Some(manifest) = read_manifest(&cache, &key) {
        if let Ok(font) = open_font(&manifest.font, manifest.index) {
            crate::startup::mark("font cache hit and loaded");
            return Ok(font);
        }
    }
    let (path, index) = fresh_match(key, &cache)?;
    crate::startup::mark("font matched");
    let font = open_font(&path, index)?;
    crate::startup::mark("font loaded");
    Ok(font)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "jofi-font-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn manifest(&self) -> Manifest {
            Manifest {
                key: b"test-environment".to_vec(),
                font: self.0.join("font.ttf"),
                index: 7,
                dependencies: snapshot(std::slice::from_ref(&self.0)).unwrap(),
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn dependency_edits_creations_deletions_and_replacements_invalidate() {
        for root in ["config", "fonts", "cache"] {
            for action in ["edit", "create", "delete", "replace", "newdir"] {
                let fixture = Fixture::new();
                let dir = fixture.0.join(root);
                fs::create_dir(&dir).unwrap();
                let file = dir.join("existing");
                fs::write(&file, b"old").unwrap();
                let m = fixture.manifest();
                assert!(m.valid(&m.key));
                match action {
                    "edit" => fs::write(&file, b"new-content").unwrap(),
                    "create" => fs::write(dir.join("new"), b"new").unwrap(),
                    "delete" => fs::remove_file(&file).unwrap(),
                    "replace" => {
                        let new = dir.join("replacement");
                        fs::write(&new, b"old").unwrap();
                        fs::rename(new, &file).unwrap();
                    }
                    "newdir" => fs::create_dir(dir.join("nested")).unwrap(),
                    _ => unreachable!(),
                }
                assert!(!m.valid(&m.key), "{root}/{action}");
            }
        }
    }

    #[test]
    fn missing_roots_and_symlink_targets_are_watched() {
        let f = Fixture::new();
        let missing = f.0.join("not-yet/config");
        let deps = snapshot(std::slice::from_ref(&missing)).unwrap();
        let m = Manifest {
            key: vec![],
            font: missing.clone(),
            index: 0,
            dependencies: deps,
        };
        assert!(m.valid(&[]));
        fs::create_dir_all(&missing).unwrap();
        assert!(!m.valid(&[]));
        let target = f.0.join("target");
        fs::write(&target, b"original").unwrap();
        let link = f.0.join("alias");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let m = f.manifest();
        fs::write(&target, b"changed!").unwrap();
        assert!(!m.valid(&m.key));
        let m = f.manifest();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("other", &link).unwrap();
        assert!(!m.valid(&m.key));
    }

    #[test]
    fn optional_includes_track_missing_paths_and_xml_escapes() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let xdg = f.0.join("xdg");
        let absolute = f.0.join("absolute.conf");
        let xml = format!(
            r#"<!DOCTYPE fontconfig SYSTEM "urn:fontconfig:fonts.dtd"><fontconfig>
            <include ignore_missing="yes">{}</include>
            <include ignore_missing="yes">~/.fonts.conf</include>
            <include ignore_missing="yes" prefix="xdg">fontconfig/a&amp;b.conf</include>
            <include>co<!-- ignored comment -->nf.d</include></fontconfig>"#,
            absolute.display()
        );
        let roots = include_roots(&xml, &home, &xdg, &|name| {
            (name == "conf.d").then(|| f.0.join("conf.d"))
        })
        .unwrap();
        assert_eq!(
            roots,
            vec![
                absolute,
                home.join(".fonts.conf"),
                xdg.join("fontconfig/a&b.conf"),
                f.0.join("conf.d")
            ]
        );
        for root in &roots {
            let dependencies = snapshot(&roots).unwrap();
            let manifest = Manifest {
                key: vec![],
                font: root.clone(),
                index: 0,
                dependencies,
            };
            assert!(manifest.valid(&[]));
            fs::create_dir_all(root.parent().unwrap()).unwrap();
            fs::write(root, b"new config").unwrap();
            assert!(!manifest.valid(&[]));
        }
        assert!(include_roots(
            "<fontconfig><include>unresolved</include></fontconfig>",
            &home,
            &xdg,
            &|_| None
        )
        .is_none());
        assert!(include_roots("broken XML", &home, &xdg, &|_| None).is_none());
    }

    #[test]
    fn cache_round_trip_corruption_and_environment_changes() {
        let f = Fixture::new();
        fs::write(f.0.join("font.ttf"), b"test face").unwrap();
        let m = f.manifest();
        let bytes = m.encode();
        let decoded = Manifest::decode(&bytes).unwrap();
        assert_eq!(decoded.font, m.font);
        assert_eq!(decoded.index, 7);
        assert_eq!(decoded.dependencies, m.dependencies);
        assert!(decoded.valid(&m.key));
        assert!(!decoded.valid(b"changed environment"));
        assert!(Manifest::decode(b"garbage").is_none());
        for end in [0, 1, MAGIC.len(), bytes.len() - 1] {
            assert!(Manifest::decode(&bytes[..end]).is_none());
        }
        let mut corrupt = bytes;
        corrupt[MAGIC.len() + 8] ^= 1;
        assert!(Manifest::decode(&corrupt).is_none());
        let cache = Fixture::new();
        let path = cache.0.join("font-match");
        assert!(read_manifest(&path, &m.key).is_none());
        write_manifest(&path, &m).unwrap();
        assert!(read_manifest(&path, &m.key).is_some());
        fs::write(&path, b"corrupt").unwrap();
        assert!(read_manifest(&path, &m.key).is_none());
    }

    #[test]
    #[ignore = "requires an installed system monospace font and fc-match"]
    fn matches_system_fc_match() {
        let (path, index) = resolve().unwrap();
        let output = std::process::Command::new("fc-match")
            .args(["-f", "%{file}:%{index}", "monospace"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            format!("{}:{index}", path.display()),
            String::from_utf8(output.stdout).unwrap()
        );
        assert!(load().is_ok());
        assert!(load().is_ok());
    }
}
