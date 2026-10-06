use std::{
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};

const MAGIC_V1: &[u8] = b"MELLOW_SESSION_V1\n";
const MAGIC_V2: &[u8] = b"MELLOW_SESSION_V2\n";
const MAGIC: &[u8] = b"MELLOW_SESSION_V3\n";
const MAX_DOCUMENTS: usize = 128;
const MAX_STRING_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDocument {
    pub path: PathBuf,
    pub cursor_row: usize,
    pub cursor_col: usize,
    pub scroll_row: usize,
    pub scroll_col: usize,
    pub visual_scroll_row: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionState {
    pub documents: Vec<SessionDocument>,
    pub active_document: usize,
    pub primary_document: usize,
    pub secondary_document: Option<usize>,
    pub active_pane: u8,
    pub split_ratio: u16,
    pub split_orientation: u8,
    pub word_wrap: bool,
    pub theme_preset: u8,
    pub show_whitespace: bool,
    pub show_indent_guides: bool,
    pub explorer_visible: bool,
    pub explorer_selected: usize,
}

pub fn load(workspace_root: &Path) -> Result<Option<SessionState>> {
    let path = session_path(workspace_root)?;
    if !path.exists() {
        return Ok(None);
    }

    let bytes =
        fs::read(&path).with_context(|| format!("failed to read session {}", path.display()))?;
    parse(&bytes).map(Some)
}

pub fn write(workspace_root: &Path, state: &SessionState) -> Result<PathBuf> {
    let path = session_path(workspace_root)?;
    let dir = path
        .parent()
        .context("session path has no parent directory")?;
    ensure_private_dir(dir)?;

    let bytes = encode(state)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = dir.join(format!(".session-{}-{nonce}.tmp", std::process::id()));

    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let result = (|| -> Result<()> {
        let mut file = options
            .open(&temp)
            .with_context(|| format!("failed to create session {}", temp.display()))?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(&temp, &path)?;
        if let Ok(directory) = fs::File::open(dir) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result?;
    Ok(path)
}

fn session_path(workspace_root: &Path) -> Result<PathBuf> {
    let identity = fs::canonicalize(workspace_root)
        .unwrap_or_else(|_| workspace_root.to_path_buf())
        .to_string_lossy()
        .into_owned();
    Ok(crate::settings::state_root()
        .join("sessions")
        .join(format!("{:016x}.session", fnv1a(identity.as_bytes()))))
}

fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("failed to create session directory {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn encode(state: &SessionState) -> Result<Vec<u8>> {
    if state.documents.len() > MAX_DOCUMENTS {
        bail!("too many documents in session");
    }

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    write_u64(&mut out, state.documents.len());
    write_u64(&mut out, state.active_document);
    write_u64(&mut out, state.primary_document);
    write_optional_index(&mut out, state.secondary_document);
    out.push(state.active_pane);
    out.push(state.split_ratio.clamp(20, 80) as u8);
    out.push(state.split_orientation.min(1));
    out.push(u8::from(state.word_wrap));
    out.push(state.theme_preset);
    out.push(u8::from(state.show_whitespace));
    out.push(u8::from(state.show_indent_guides));
    out.push(u8::from(state.explorer_visible));
    write_u64(&mut out, state.explorer_selected);

    for document in &state.documents {
        write_string(&mut out, &document.path.to_string_lossy())?;
        write_u64(&mut out, document.cursor_row);
        write_u64(&mut out, document.cursor_col);
        write_u64(&mut out, document.scroll_row);
        write_u64(&mut out, document.scroll_col);
        write_u64(&mut out, document.visual_scroll_row);
    }

    Ok(out)
}

fn parse(bytes: &[u8]) -> Result<SessionState> {
    let (version, payload) = if let Some(payload) = bytes.strip_prefix(MAGIC) {
        (3u8, payload)
    } else if let Some(payload) = bytes.strip_prefix(MAGIC_V2) {
        (2u8, payload)
    } else if let Some(payload) = bytes.strip_prefix(MAGIC_V1) {
        (1u8, payload)
    } else {
        bail!("unsupported session format");
    };

    let mut cursor = Cursor::new(payload);
    let count = read_usize(&mut cursor)?;
    if count > MAX_DOCUMENTS {
        bail!("session contains too many documents");
    }

    let active_document = read_usize(&mut cursor)?;
    let primary_document = read_usize(&mut cursor)?;
    let secondary_document = read_optional_index(&mut cursor)?;
    let active_pane = read_byte(&mut cursor)?;
    let split_ratio = if version >= 2 {
        u16::from(read_byte(&mut cursor)?).clamp(20, 80)
    } else {
        50
    };
    let split_orientation = if version >= 3 {
        read_byte(&mut cursor)?.min(1)
    } else {
        0
    };
    let word_wrap = read_bool(&mut cursor)?;
    let theme_preset = read_byte(&mut cursor)?;
    let show_whitespace = read_bool(&mut cursor)?;
    let show_indent_guides = read_bool(&mut cursor)?;
    let explorer_visible = read_bool(&mut cursor)?;
    let explorer_selected = read_usize(&mut cursor)?;

    let mut documents = Vec::with_capacity(count);
    for _ in 0..count {
        documents.push(SessionDocument {
            path: PathBuf::from(read_string(&mut cursor)?),
            cursor_row: read_usize(&mut cursor)?,
            cursor_col: read_usize(&mut cursor)?,
            scroll_row: read_usize(&mut cursor)?,
            scroll_col: read_usize(&mut cursor)?,
            visual_scroll_row: read_usize(&mut cursor)?,
        });
    }

    if !documents.is_empty() {
        if active_document >= documents.len() || primary_document >= documents.len() {
            bail!("session references an invalid active document");
        }
        if secondary_document.is_some_and(|index| index >= documents.len()) {
            bail!("session references an invalid secondary document");
        }
    }

    Ok(SessionState {
        documents,
        active_document,
        primary_document,
        secondary_document,
        active_pane: active_pane.min(1),
        split_ratio,
        split_orientation,
        word_wrap,
        theme_preset,
        show_whitespace,
        show_indent_guides,
        explorer_visible,
        explorer_selected,
    })
}

fn write_string(out: &mut Vec<u8>, value: &str) -> Result<()> {
    if value.len() > MAX_STRING_BYTES {
        bail!("session path is too long");
    }
    write_u64(out, value.len());
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_string(cursor: &mut Cursor<&[u8]>) -> Result<String> {
    let len = read_usize(cursor)?;
    if len > MAX_STRING_BYTES {
        bail!("session string exceeds safety limit");
    }
    let mut bytes = vec![0u8; len];
    cursor
        .read_exact(&mut bytes)
        .context("truncated session string")?;
    String::from_utf8(bytes).context("session path is not UTF-8")
}

fn write_optional_index(out: &mut Vec<u8>, value: Option<usize>) {
    match value {
        Some(index) => {
            out.push(1);
            write_u64(out, index);
        }
        None => out.push(0),
    }
}

fn read_optional_index(cursor: &mut Cursor<&[u8]>) -> Result<Option<usize>> {
    match read_byte(cursor)? {
        0 => Ok(None),
        1 => Ok(Some(read_usize(cursor)?)),
        _ => bail!("invalid optional index tag"),
    }
}

fn write_u64(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u64).to_le_bytes());
}

fn read_usize(cursor: &mut Cursor<&[u8]>) -> Result<usize> {
    let mut bytes = [0u8; 8];
    cursor
        .read_exact(&mut bytes)
        .context("truncated session integer")?;
    usize::try_from(u64::from_le_bytes(bytes)).context("session integer does not fit usize")
}

fn read_byte(cursor: &mut Cursor<&[u8]>) -> Result<u8> {
    let mut byte = [0u8; 1];
    cursor
        .read_exact(&mut byte)
        .context("truncated session byte")?;
    Ok(byte[0])
}

fn read_bool(cursor: &mut Cursor<&[u8]>) -> Result<bool> {
    match read_byte(cursor)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => bail!("invalid session boolean"),
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_state() -> SessionState {
        SessionState {
            documents: vec![
                SessionDocument {
                    path: PathBuf::from("src/main.rs"),
                    cursor_row: 12,
                    cursor_col: 8,
                    scroll_row: 4,
                    scroll_col: 0,
                    visual_scroll_row: 7,
                },
                SessionDocument {
                    path: PathBuf::from("README.md"),
                    cursor_row: 2,
                    cursor_col: 3,
                    scroll_row: 0,
                    scroll_col: 0,
                    visual_scroll_row: 0,
                },
            ],
            active_document: 1,
            primary_document: 0,
            secondary_document: Some(1),
            active_pane: 1,
            split_ratio: 63,
            split_orientation: 1,
            word_wrap: true,
            theme_preset: 2,
            show_whitespace: true,
            show_indent_guides: false,
            explorer_visible: true,
            explorer_selected: 4,
        }
    }

    fn encode_v1_for_test(state: &SessionState) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC_V1);
        write_u64(&mut out, state.documents.len());
        write_u64(&mut out, state.active_document);
        write_u64(&mut out, state.primary_document);
        write_optional_index(&mut out, state.secondary_document);
        out.push(state.active_pane);
        out.push(u8::from(state.word_wrap));
        out.push(state.theme_preset);
        out.push(u8::from(state.show_whitespace));
        out.push(u8::from(state.show_indent_guides));
        out.push(u8::from(state.explorer_visible));
        write_u64(&mut out, state.explorer_selected);
        for document in &state.documents {
            write_string(&mut out, &document.path.to_string_lossy()).unwrap();
            write_u64(&mut out, document.cursor_row);
            write_u64(&mut out, document.cursor_col);
            write_u64(&mut out, document.scroll_row);
            write_u64(&mut out, document.scroll_col);
            write_u64(&mut out, document.visual_scroll_row);
        }
        out
    }

    #[test]
    fn legacy_v1_session_defaults_split_ratio_without_losing_state() {
        let state = sample_state();
        let bytes = encode_v1_for_test(&state);
        let decoded = parse(&bytes).unwrap();
        assert_eq!(decoded.split_ratio, 50);
        assert_eq!(decoded.documents, state.documents);
        assert_eq!(decoded.active_document, state.active_document);
        assert_eq!(decoded.secondary_document, state.secondary_document);
    }

    #[test]
    fn session_binary_round_trip_preserves_workspace_state() {
        let state = sample_state();
        let bytes = encode(&state).unwrap();
        let decoded = parse(&bytes).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn corrupt_or_oversized_session_is_rejected() {
        assert!(parse(b"not-a-session").is_err());

        let mut bytes = MAGIC.to_vec();
        write_u64(&mut bytes, MAX_DOCUMENTS + 1);
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn session_path_is_stable_per_workspace_identity() {
        let one = fnv1a(b"/workspace/one");
        let one_again = fnv1a(b"/workspace/one");
        let two = fnv1a(b"/workspace/two");
        assert_eq!(one, one_again);
        assert_ne!(one, two);
    }
}
