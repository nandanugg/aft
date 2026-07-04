use aft::callgraph::walk_project_files;
use aft::semantic_index::SemanticIndex;
use aft::symbols::SymbolKind;
use serde::Serialize;
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufWriter, Cursor, Read, Write};
use std::path::PathBuf;

#[derive(Debug, Serialize)]
struct ExportChunk {
    file: String,
    name: String,
    qualified_name: Option<String>,
    kind: SymbolKind,
    start_line: u32,
    end_line: u32,
    exported: bool,
    embed_text: String,
    snippet: String,
    embed_text_chars: usize,
}

#[derive(Debug, Default)]
struct ExportStats {
    chunk_count: usize,
    total_embed_text_chars: usize,
    files_covered: usize,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("export_chunks failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let (project_root, output_path) = parse_args()?;
    let project_root = fs::canonicalize(project_root)?;
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let files = walk_project_files(&project_root).collect::<Vec<_>>();
    let mut embed = |texts: Vec<String>| -> Result<Vec<Vec<f32>>, String> {
        Ok(vec![vec![0.0_f32; 1]; texts.len()])
    };
    let index =
        SemanticIndex::build(&project_root, &files, &mut embed, 256).map_err(io::Error::other)?;
    let bytes = index.to_bytes();

    let output_file = File::create(&output_path)?;
    let mut writer = BufWriter::new(output_file);
    let stats = export_chunks(&bytes, &mut writer)?;
    writer.flush()?;

    eprintln!(
        "exported {} chunks ({} embed_text chars) across {} files to {}",
        stats.chunk_count,
        stats.total_embed_text_chars,
        stats.files_covered,
        output_path.display(),
    );

    Ok(())
}

fn parse_args() -> Result<(PathBuf, PathBuf), Box<dyn Error>> {
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        eprintln!("usage: cargo run --example export_chunks -- [project_root] <output_path>");
        std::process::exit(0);
    }

    match args.as_slice() {
        [output] => Ok((PathBuf::from("."), output.clone().into())),
        [project_root, output] => Ok((project_root.clone().into(), output.clone().into())),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: cargo run --example export_chunks -- [project_root] <output_path>",
        )
        .into()),
    }
}

fn export_chunks(bytes: &[u8], writer: &mut impl Write) -> io::Result<ExportStats> {
    let mut cursor = Cursor::new(bytes);
    let _version = read_u8(&mut cursor)?;
    let dimension = read_u32(&mut cursor)? as usize;
    let entry_count = read_u32(&mut cursor)? as usize;
    let fingerprint_len = read_u32(&mut cursor)? as usize;
    skip_bytes(&mut cursor, fingerprint_len)?;

    let file_mtime_count = read_u32(&mut cursor)? as usize;
    for _ in 0..file_mtime_count {
        let path_len = read_u32(&mut cursor)? as usize;
        skip_bytes(&mut cursor, path_len)?;
        skip_bytes(&mut cursor, 8 + 4 + 8 + 32)?;
    }

    let vector_bytes = dimension
        .checked_mul(std::mem::size_of::<f32>())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "embedding dimension overflow")
        })?;

    let mut stats = ExportStats::default();
    let mut files_covered = HashSet::<String>::new();

    for _ in 0..entry_count {
        let file = read_string(&mut cursor)?;
        let name = read_string(&mut cursor)?;
        let qualified_name = {
            let value = read_string(&mut cursor)?;
            if value.is_empty() {
                None
            } else {
                Some(value)
            }
        };
        let kind = read_symbol_kind(read_u8(&mut cursor)?)?;
        let start_line = read_u32(&mut cursor)?;
        let end_line = read_u32(&mut cursor)?;
        let exported = read_u8(&mut cursor)? != 0;
        let snippet = read_string(&mut cursor)?;
        let embed_text = read_string(&mut cursor)?;
        let embed_text_chars = embed_text.chars().count();
        skip_bytes(&mut cursor, vector_bytes)?;

        files_covered.insert(file.clone());
        serde_json::to_writer(
            &mut *writer,
            &ExportChunk {
                file,
                name,
                qualified_name,
                kind,
                start_line,
                end_line,
                exported,
                embed_text,
                snippet,
                embed_text_chars,
            },
        )
        .map_err(io::Error::other)?;
        writer.write_all(b"\n")?;

        stats.chunk_count += 1;
        stats.total_embed_text_chars += embed_text_chars;
    }

    if cursor.position() != bytes.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "semantic index bytes had trailing data",
        ));
    }

    stats.files_covered = files_covered.len();
    Ok(stats)
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> io::Result<u8> {
    let mut buf = [0u8; 1];
    cursor.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    cursor.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn read_string(cursor: &mut Cursor<&[u8]>) -> io::Result<String> {
    let len = read_u32(cursor)? as usize;
    let mut buf = vec![0u8; len];
    cursor.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn skip_bytes(cursor: &mut Cursor<&[u8]>, bytes: usize) -> io::Result<()> {
    let start = cursor.position() as usize;
    let end = start
        .checked_add(bytes)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "byte count overflow"))?;
    if end > cursor.get_ref().len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "semantic index bytes ended unexpectedly",
        ));
    }
    cursor.set_position(end as u64);
    Ok(())
}

fn read_symbol_kind(value: u8) -> io::Result<SymbolKind> {
    match value {
        0 => Ok(SymbolKind::Function),
        1 => Ok(SymbolKind::Class),
        2 => Ok(SymbolKind::Method),
        3 => Ok(SymbolKind::Struct),
        4 => Ok(SymbolKind::Interface),
        5 => Ok(SymbolKind::Enum),
        6 => Ok(SymbolKind::TypeAlias),
        7 => Ok(SymbolKind::Variable),
        8 => Ok(SymbolKind::Heading),
        9 => Ok(SymbolKind::FileSummary),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown symbol kind byte: {value}"),
        )),
    }
}
