//! Versioned broker storage. References preserve bytes; they never confer authority.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

const MAX_JOURNAL: u64 = 128 * 1024 * 1024;
const MAX_BLOBS: u64 = 128 * 1024 * 1024;
const MAX_EVENTS: usize = 32768;
const MAX_SEGMENTS: usize = 64;
const MAX_BLOB_FILES: usize = 4096;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    segments: Vec<Segment>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Segment {
    path: String,
    first_sequence: u64,
    bytes: Option<u64>,
    sha256: Option<String>,
    last_sequence: Option<u64>,
}
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Reference {
    sha256: String,
    bytes: u64,
    encoding: String,
}
fn regular(path: &Path, bound: u64) -> RuntimeResult<Vec<u8>> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > bound {
        return Err("journal file is not bounded regular storage".into());
    }
    let mut bytes = vec![];
    Read::by_ref(&mut file)
        .take(bound + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > bound {
        return Err("journal file exceeded bound".into());
    }
    Ok(bytes)
}
fn exists(path: &Path) -> RuntimeResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}
fn hash(value: &str) -> RuntimeResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("invalid journal digest".into());
    }
    Ok(())
}
fn blob_directory(directory: &Path) -> RuntimeResult<PathBuf> {
    let path = directory.join("journal-blobs");
    if fs::symlink_metadata(&path)?.file_type().is_symlink() || !path.is_dir() {
        return Err("invalid journal blob directory".into());
    }
    Ok(path)
}
fn reference(value: &Value, encoding: &str) -> RuntimeResult<Reference> {
    let r: Reference = serde_json::from_value(value.clone())?;
    hash(&r.sha256)?;
    if r.encoding != encoding || r.bytes > MAX_FILE {
        return Err("invalid journal blob descriptor".into());
    }
    Ok(r)
}
fn resolve(directory: &Path, r: &Reference) -> RuntimeResult<Vec<u8>> {
    let bytes = regular(&blob_directory(directory)?.join(&r.sha256), MAX_FILE)?;
    if bytes.len() as u64 != r.bytes || sha(&bytes) != r.sha256 {
        return Err("journal blob hash or length mismatch".into());
    }
    Ok(bytes)
}
pub(super) fn source(directory: &Path, value: &Value) -> RuntimeResult<Value> {
    match (value.get("source"), value.get("source_ref")) {
        (Some(source), None) => Ok(source.clone()),
        (None, Some(descriptor)) => Ok(serde_json::from_slice(&resolve(
            directory,
            &reference(descriptor, "json")?,
        )?)?),
        _ => Err("missing or ambiguous source observation".into()),
    }
}
pub(super) fn output(directory: &Path, item: &Value) -> RuntimeResult<Vec<u8>> {
    match (
        item.get("aggregatedOutput"),
        item.get("aggregatedOutput_ref"),
    ) {
        (Some(output), None) => Ok(output
            .as_str()
            .ok_or("invalid actual command output")?
            .as_bytes()
            .to_vec()),
        (None, Some(descriptor)) => {
            let bytes = resolve(directory, &reference(descriptor, "utf8")?)?;
            std::str::from_utf8(&bytes)?;
            Ok(bytes)
        }
        _ => Err("missing or ambiguous actual command output".into()),
    }
}
fn references(log: &[Event]) -> RuntimeResult<BTreeMap<String, Reference>> {
    let mut result = BTreeMap::new();
    for event in log {
        for (descriptor, encoding) in [
            (event.value.get("source_ref"), "json"),
            (event.value["item"].get("aggregatedOutput_ref"), "utf8"),
        ] {
            if let Some(value) = descriptor {
                let r = reference(value, encoding)?;
                if let Some(previous) = result.insert(r.sha256.clone(), r.clone()) {
                    if previous.bytes != r.bytes {
                        return Err("conflicting blob descriptor".into());
                    }
                }
            }
        }
    }
    if result.len() > MAX_BLOB_FILES {
        return Err("journal reference count exceeded".into());
    }
    Ok(result)
}
fn parse(bytes: &[u8], log: &mut Vec<Event>, active: bool) -> RuntimeResult<()> {
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        if line.len() as u64 > MAX_FILE {
            return Err("journal event exceeds bound".into());
        }
        if !line.ends_with(b"\n") {
            if active {
                break;
            }
            return Err("sealed journal has a torn tail".into());
        }
        let event: Event = serde_json::from_slice(line)?;
        if event.sequence != log.len() as u64 + 1 {
            return Err("broker event sequence gap".into());
        }
        if log.len() >= MAX_EVENTS {
            return Err("journal event count exceeded".into());
        }
        log.push(event);
    }
    Ok(())
}
pub(super) fn read(directory: &Path) -> RuntimeResult<(Vec<Event>, Vec<u8>)> {
    let manifest_path = directory.join("journal.json");
    if !exists(&manifest_path)? {
        let path = directory.join("events.jsonl");
        if !exists(&path)? {
            if directory.exists() {
                for entry in fs::read_dir(directory)? {
                    let name = entry?.file_name();
                    let name = name.to_string_lossy();
                    if name.starts_with("events-") && name.ends_with(".jsonl") {
                        return Err("journal segments exist without a manifest".into());
                    }
                }
            }
            return Ok((vec![], vec![]));
        }
        let bytes = regular(&path, MAX_SPOOL)?;
        let mut log = vec![];
        parse(&bytes, &mut log, true)?;
        return Ok((log, bytes));
    }
    if exists(&directory.join("events.jsonl"))? {
        return Err("ambiguous journal formats".into());
    }
    let manifest: Manifest = serde_json::from_slice(&regular(&manifest_path, MAX_FILE)?)?;
    if manifest.version != 2
        || manifest.segments.is_empty()
        || manifest.segments.len() > MAX_SEGMENTS
    {
        return Err("unsupported journal manifest".into());
    }
    let mut log = vec![];
    let mut segments = vec![];
    let mut total = 0;
    for (index, segment) in manifest.segments.iter().enumerate() {
        if segment.path != format!("events-{index:06}.jsonl")
            || segment.first_sequence != log.len() as u64 + 1
        {
            return Err("journal segment identity or sequence mismatch".into());
        }
        let active = index + 1 == manifest.segments.len();
        let bytes = regular(&directory.join(&segment.path), MAX_SPOOL)?;
        total += bytes.len() as u64;
        if total > MAX_JOURNAL {
            return Err("aggregate journal byte limit exceeded".into());
        }
        if active {
            if segment.bytes.is_some()
                || segment.sha256.is_some()
                || segment.last_sequence.is_some()
            {
                return Err("active segment must not be sealed".into());
            }
        } else if segment.bytes != Some(bytes.len() as u64)
            || segment.sha256.as_deref() != Some(sha(&bytes).as_str())
        {
            return Err("sealed segment hash or length mismatch".into());
        }
        parse(&bytes, &mut log, active)?;
        if !active
            && (segment.last_sequence != Some(log.len() as u64)
                || segment.first_sequence > log.len() as u64)
        {
            return Err("sealed segment sequence mismatch".into());
        }
        segments.push(json!({"path":segment.path,"first_sequence":segment.first_sequence,"bytes":bytes.len(),"sha256":sha(&bytes)}));
    }
    // Count orphan payloads too: a crash between blob write and event append must
    // not reset the disk allowance. Payload verification remains lazy until use.
    let refs = references(&log)?;
    let mut blob_bytes = 0;
    let mut count = 0;
    if exists(&directory.join("journal-blobs"))? {
        for entry in fs::read_dir(blob_directory(directory)?)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            count += 1;
            blob_bytes += metadata.len();
            if !metadata.is_file()
                || metadata.nlink() != 1
                || metadata.file_type().is_symlink()
                || count > MAX_BLOB_FILES
                || blob_bytes > MAX_BLOBS
                || metadata.len() > MAX_FILE
            {
                return Err("journal blob storage limit or file type violation".into());
            }
            hash(&entry.file_name().to_string_lossy())?;
        }
    }
    // Validate referenced file identities without expanding every repeated source.
    for r in refs.values() {
        resolve(directory, r)?;
    }
    let receipt = json!({"format":"bokkie-journal-v2","segments":segments,"blobs":refs.values().collect::<Vec<_>>()});
    Ok((log, serde_json::to_vec(&receipt)?))
}
pub(super) fn retain(
    runtime: &EngineeringRuntime,
    directory: &Path,
    receipt: &[u8],
) -> RuntimeResult<String> {
    if !exists(&directory.join("journal.json"))? {
        return runtime.blob(receipt);
    }
    let value: Value = serde_json::from_slice(receipt)?;
    for segment in value["segments"]
        .as_array()
        .ok_or("missing journal segments")?
    {
        let path = segment["path"].as_str().ok_or("missing journal path")?;
        let bytes = regular(&directory.join(path), MAX_SPOOL)?;
        if segment["sha256"] != sha(&bytes) || segment["bytes"] != bytes.len() as u64 {
            return Err("journal changed during evidence retention; reconcile again".into());
        }
        runtime.blob(&bytes)?;
    }
    let mut seen = BTreeSet::new();
    for descriptor in value["blobs"].as_array().ok_or("missing journal blobs")? {
        let encoding = descriptor["encoding"]
            .as_str()
            .ok_or("missing blob encoding")?;
        let r = reference(descriptor, encoding)?;
        if seen.insert(r.sha256.clone()) {
            runtime.blob(&resolve(directory, &r)?)?;
        }
    }
    runtime.blob(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn python(root: &Path, code: &str) {
        let script = format!(
            "import importlib.util, pathlib, json, sys\nspec=importlib.util.spec_from_file_location('broker',sys.argv[1]); b=importlib.util.module_from_spec(spec); spec.loader.exec_module(b)\nroot=pathlib.Path(sys.argv[2])\n{code}"
        );
        let result = Command::new("python3")
            .args(["-c", &script])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tools/engineering-runtime/broker.py"))
            .arg(root)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn python_segments_preserve_exact_sources_outputs_and_retained_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        python(
            root,
            "s=b.Spool(root, segment_limit=1024)\nfor i in range(30):\n s.append('command_source', {'source': {'files': {'é.md': {'sha256': 'a'*64}}, 'clean': True}})\n s.append('item/completed', {'item': {'type': 'commandExecution', 'id': str(i), 'aggregatedOutput': 'é\\r\\n'*12000}})\ns.append('boundary_reaped', {}, terminal=True)",
        );
        let (log, receipt) = read(root).unwrap();
        assert_eq!(log.len(), 61);
        let value: Value = serde_json::from_slice(&receipt).unwrap();
        assert!(value["segments"].as_array().unwrap().len() > 1);
        assert_eq!(value["blobs"].as_array().unwrap().len(), 2);
        for event in &log[..60] {
            if event.kind == "command_source" {
                assert!(event.value.get("source").is_none());
                assert_eq!(
                    source(root, &event.value).unwrap(),
                    json!({"files":{"é.md":{"sha256":"a".repeat(64)}},"clean":true})
                );
            } else {
                assert!(event.value["item"].get("aggregatedOutput").is_none());
                assert_eq!(
                    output(root, &event.value["item"]).unwrap(),
                    "é\r\n".repeat(12000).as_bytes()
                );
            }
        }
        // The retained receipt names exact bounded segments and each unique payload.
        let first = &value["segments"][0];
        assert_eq!(
            sha(&fs::read(root.join(first["path"].as_str().unwrap())).unwrap()),
            first["sha256"]
        );
        let blob = root
            .join("journal-blobs")
            .join(value["blobs"][0]["sha256"].as_str().unwrap());
        fs::write(blob, b"corrupt").unwrap();
        assert!(read(root).is_err());
    }

    #[test]
    fn empty_legacy_torn_and_corrupt_segment_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert!(read(root).unwrap().0.is_empty());
        python(root, "s=b.Spool(root)");
        assert!(read(root).unwrap().0.is_empty());
        assert!(!root.join("journal-blobs").exists());
        python(
            root,
            "s=b.Spool(root, segment_limit=1024)\nfor i in range(12): s.append('progress', {'text': 'x'*200})",
        );
        let manifest: Value =
            serde_json::from_slice(&fs::read(root.join("journal.json")).unwrap()).unwrap();
        let active = root.join(
            manifest["segments"].as_array().unwrap().last().unwrap()["path"]
                .as_str()
                .unwrap(),
        );
        OpenOptions::new()
            .append(true)
            .open(active)
            .unwrap()
            .write_all(b"{\"sequence\":")
            .unwrap();
        assert_eq!(read(root).unwrap().0.len(), 12);
        let sealed = root.join(manifest["segments"][0]["path"].as_str().unwrap());
        fs::write(&sealed, b"truncated").unwrap();
        assert!(read(root).is_err());
        fs::remove_file(root.join("journal.json")).unwrap();
        assert!(read(root).is_err());
        let legacy = tempfile::tempdir().unwrap();
        fs::write(
            legacy.path().join("events.jsonl"),
            b"{\"sequence\":1,\"kind\":\"progress\",\"value\":{}}\n{\"sequence\":",
        )
        .unwrap();
        assert_eq!(read(legacy.path()).unwrap().0.len(), 1);
        let linked = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("absent", linked.path().join("journal.json")).unwrap();
        assert!(read(linked.path()).is_err());
    }

    /// Optional read-only historical replay; ordinary checks use synthetic inputs only.
    #[test]
    #[ignore = "requires an explicitly selected retained legacy journal"]
    fn historical_replay() {
        let input = std::env::var("BOKKIE_JOURNAL_REPLAY").expect("select retained journal");
        let original = fs::read(&input).unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("input.jsonl"), &original).unwrap();
        python(
            temp.path(),
            "s=b.Spool(root, segment_limit=1024*1024)\nfor line in (root/'input.jsonl').read_bytes().splitlines():\n e=json.loads(line); s.append(e['kind'], e['value'], terminal=e['kind'] in ('failure','stderr_diagnostic','boundary_reaped','boundary_not_started'))",
        );
        let (log, receipt) = read(temp.path()).unwrap();
        let expected: Vec<Value> = original
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect();
        assert_eq!(log.len(), expected.len());
        for (event, expected) in log.iter().zip(&expected) {
            let mut hydrated = serde_json::to_value(event).unwrap();
            if event.value.get("source_ref").is_some() {
                let payload = source(temp.path(), &event.value).unwrap();
                let object = hydrated["value"].as_object_mut().unwrap();
                object.remove("source_ref");
                object.insert("source".into(), payload);
            }
            if event.value["item"].get("aggregatedOutput_ref").is_some() {
                let payload =
                    String::from_utf8(output(temp.path(), &event.value["item"]).unwrap()).unwrap();
                let object = hydrated["value"]["item"].as_object_mut().unwrap();
                object.remove("aggregatedOutput_ref");
                object.insert("aggregatedOutput".into(), json!(payload));
            }
            assert_eq!(&hydrated, expected);
        }
        assert_eq!(fs::read(input).unwrap(), original);
        let receipt: Value = serde_json::from_slice(&receipt).unwrap();
        let journal_bytes: u64 = receipt["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["bytes"].as_u64().unwrap())
            .sum();
        let blob_bytes: u64 = receipt["blobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["bytes"].as_u64().unwrap())
            .sum();
        println!(
            "events={} original_bytes={} original_sha256={} journal_bytes={} unique_blob_bytes={} segments={} blobs={}",
            log.len(),
            original.len(),
            sha(&original),
            journal_bytes,
            blob_bytes,
            receipt["segments"].as_array().unwrap().len(),
            receipt["blobs"].as_array().unwrap().len()
        );
    }
}
