//! Non-image file attachments: text extraction for the composer.
//!
//! MSP's `TurnInputPart` is a closed enum — `text` or `image`, and anything
//! else is `invalidParams` — so an attached PDF, spreadsheet or document
//! reaches the model as extracted text in its own text part:
//! `--- file: <name> ---\n<content>`, ahead of the prompt text. Anything that
//! cannot be turned into text is refused with a reason the banner shows.
//!
//! Everything is extracted once, when the file enters the composer, and capped
//! at [`MAX_FILE_BYTES`] with a truncation note: an uncapped spreadsheet would
//! spend the session's context window on its own.

use std::io::{Cursor, Read};
use std::path::Path;

/// Bytes of extracted text kept per file; the rest is cut with a note.
pub const MAX_FILE_BYTES: usize = 64 * 1024;
/// How many file attachments one turn carries.
pub const MAX_FILES: usize = 8;

/// One file ready to be sent, and to be drawn as a composer chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachedFile {
    /// Stable id for the chip and its remove button.
    pub id: String,
    /// What the chip says: the file name.
    pub name: String,
    /// The file's size on disk, for the chip's muted detail.
    pub size_bytes: u64,
    /// The extracted text, already capped at [`MAX_FILE_BYTES`].
    pub text: String,
    /// Whether the text was cut to the cap.
    pub truncated: bool,
}

impl AttachedFile {
    /// The chip's muted suffix (`PDF · 12.4 KB`).
    pub fn detail(&self) -> String {
        format!("{} · {}", kind_label(&self.name), size_label(self.size_bytes))
    }

    /// The wire part: one text part carrying the file's name and content.
    pub fn part(&self) -> muse_client::schema::TurnInputPart {
        muse_client::schema::TurnInputPart::text(format!("--- file: {} ---\n{}", self.name, self.text))
    }
}

/// Text-like extensions, read as UTF-8. The format is sniffed from the bytes
/// everywhere else; here the extension decides, because a `.md` that fails to
/// decode is a broken file, not a binary one.
const TEXT_EXTENSIONS: &[&str] = &[
    "md", "markdown", "mdx", "txt", "text", "csv", "tsv", "json", "jsonl", "ipynb", "toml", "yaml",
    "yml", "xml", "html", "htm", "css", "js", "mjs", "cjs", "ts", "tsx", "jsx", "rs", "py", "rb",
    "go", "java", "kt", "kts", "swift", "c", "h", "hpp", "cpp", "cc", "cs", "sh", "bash", "zsh",
    "fish", "sql", "graphql", "gql", "proto", "log", "ini", "cfg", "conf", "env", "pem", "rst",
    "tex", "textile", "diff", "patch", "vue", "svelte", "r", "lua", "pl", "pm", "scala", "hs", "ex",
    "exs", "erl", "clj", "elm", "dart", "php", "zig",
];

/// A small-file cap for the "unknown extension, but it decodes as UTF-8" path
/// (finding `support-19`): past this it is more likely genuine binary data
/// that happens to decode, not a text file this table simply has never seen.
const UNKNOWN_TEXT_SNIFF_CAP: usize = 256 * 1024;

/// Read a file from disk and extract its text, or say why not.
pub fn from_path(id: impl Into<String>, path: &Path) -> Result<AttachedFile, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
    from_bytes(id, name, path.extension().and_then(|e| e.to_str()).unwrap_or_default(), &bytes)
}

/// Turn raw bytes into an [`AttachedFile`], or say why not.
///
/// `extension` is the file name's extension without the dot (or empty); the
/// format is never guessed from the content — an image offered here is refused
/// rather than re-decoded, because images have their own attachment path.
pub fn from_bytes(
    id: impl Into<String>,
    name: impl Into<String>,
    extension: &str,
    bytes: &[u8],
) -> Result<AttachedFile, String> {
    let name: String = name.into();
    if bytes.is_empty() {
        return Err(format!("{name} is empty"));
    }
    let ext = extension.to_lowercase();
    let content = match ext.as_str() {
        "pdf" => pdf_text(bytes).map_err(|error| format!("{name}: {error}"))?,
        "xlsx" | "xls" => sheet_text(bytes).map_err(|error| format!("{name}: {error}"))?,
        "docx" => docx_text(bytes).map_err(|error| format!("{name}: {error}"))?,
        ext if TEXT_EXTENSIONS.contains(&ext) => {
            String::from_utf8(bytes.to_vec()).map_err(|_| format!("{name} is not UTF-8 text"))?
        }
        // An image handed to the file path is a wrong turn, not content: the
        // composer attaches images through `images`, with their own chip.
        "png" | "jpg" | "jpeg" | "gif" | "webp" => {
            return Err(format!("{name} is an image; attach it as one"));
        }
        "" => String::from_utf8(bytes.to_vec())
            .map_err(|_| format!("{name} is not text (no extension to go on)"))?,
        // An extension this table has never seen (a `.rules`, a vendor's
        // odd config suffix) is not refused outright any more: a small file
        // that decodes clean as UTF-8 is text, whatever its suffix is
        // called (finding `support-19`). The chip's kind label still shows
        // the real extension, so nothing is hidden about what was sniffed.
        _ if bytes.len() <= UNKNOWN_TEXT_SNIFF_CAP => match String::from_utf8(bytes.to_vec()) {
            Ok(text) => text,
            Err(_) => {
                return Err(format!(
                    ".{ext} files cannot be attached (text, PDF, xlsx/xls and docx can)"
                ));
            }
        },
        _ => {
            return Err(format!(
                ".{ext} files cannot be attached (text, PDF, xlsx/xls and docx can)"
            ));
        }
    };
    if content.trim().is_empty() {
        return Err(format!("{name} has no text to send"));
    }
    let (text, truncated) = cap(&content);
    Ok(AttachedFile { id: id.into(), name, size_bytes: bytes.len() as u64, text, truncated })
}

/// Cut `content` to [`MAX_FILE_BYTES`] on a char boundary, noting the cut.
fn cap(content: &str) -> (String, bool) {
    if content.len() <= MAX_FILE_BYTES {
        return (content.to_owned(), false);
    }
    let mut end = MAX_FILE_BYTES;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}\n[file truncated to 64 KB]", content[..end].trim_end()), true)
}

/// A PDF's text layer, via the `pdf-extract` crate (pure Rust).
fn pdf_text(bytes: &[u8]) -> Result<String, String> {
    pdf_extract::extract_text_from_mem(bytes).map_err(|error| format!("the PDF could not be read: {error}"))
}

/// Every sheet as CSV-ish text, via the `calamine` crate (pure Rust): one
/// `## <sheet>` header per sheet, then comma-joined rows.
fn sheet_text(bytes: &[u8]) -> Result<String, String> {
    use calamine::Reader as _;
    let mut workbook = calamine::open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|error| format!("the spreadsheet could not be read: {error}"))?;
    let mut out = String::new();
    let sheets = workbook.worksheets();
    if sheets.is_empty() {
        return Err("the spreadsheet has no sheets".to_owned());
    }
    for (name, range) in sheets {
        if range.rows().len() == 0 {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str("## ");
        out.push_str(&name);
        for row in range.rows() {
            out.push('\n');
            let mut first = true;
            for cell in row {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&cell.to_string());
            }
        }
    }
    Ok(out)
}

/// A `.docx` is a zip: `word/document.xml` holds the paragraphs. Strip the
/// tags, keeping paragraph and line breaks, via the `zip` crate (pure Rust).
fn docx_text(bytes: &[u8]) -> Result<String, String> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| "that is not a Word document".to_owned())?;
    let mut xml = String::new();
    archive
        .by_name("word/document.xml")
        .map_err(|_| "that is not a Word document".to_owned())?
        .read_to_string(&mut xml)
        .map_err(|error| format!("the document could not be read: {error}"))?;
    Ok(strip_word_xml(&xml))
}

/// Strip `document.xml` to paragraphs: `w:p`/`w:tr` close a line, `w:br` breaks
/// one, `w:tab` tabs, everything else is inline formatting and goes away.
fn strip_word_xml(xml: &str) -> String {
    let mut out = String::new();
    let mut chars = xml.chars();
    while let Some(c) = chars.next() {
        if c != '<' {
            out.push(c);
            continue;
        }
        let mut tag = String::new();
        for c in chars.by_ref() {
            if c == '>' {
                break;
            }
            tag.push(c);
        }
        let name = tag.trim_start_matches('/').split_whitespace().next().unwrap_or("");
        let local = name.split(':').next_back().unwrap_or("");
        match local {
            "p" | "tr" => out.push('\n'),
            "br" => out.push('\n'),
            "tab" => out.push('\t'),
            _ => {}
        }
    }
    let decoded =
        out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace(
            "&apos;",
            "'",
        );
    decoded
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The chip detail's kind word: the uppercased extension, or `TEXT`.
fn kind_label(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_uppercase())
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| "TEXT".to_owned())
}

/// A byte count in the chip's words (`512 B`, `12.4 KB`, `3.0 MB`).
fn size_label(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn markdown_arrives_as_text() {
        let file = from_bytes("f1", "notes.md", "md", b"# hello\nworld\n").expect("decodes");
        assert_eq!(file.name, "notes.md");
        assert_eq!(file.text, "# hello\nworld\n");
        assert!(!file.truncated);
        assert_eq!(file.detail(), "MD · 14 B");
        let part = file.part();
        assert!(part.text.is_some_and(|t| t.starts_with("--- file: notes.md ---\n")));
    }

    #[test]
    fn an_oversized_file_is_cut_with_a_note() {
        let big = "x".repeat(MAX_FILE_BYTES + 100);
        let file = from_bytes("f1", "big.txt", "txt", big.as_bytes()).expect("decodes");
        assert!(file.truncated);
        assert!(file.text.ends_with("[file truncated to 64 KB]"));
        assert!(file.text.len() <= MAX_FILE_BYTES + 64);
    }

    #[test]
    fn an_unknown_extension_is_refused() {
        // Genuinely invalid UTF-8 (a lone continuation byte): the sniff
        // fails to decode it, so it is still refused on its extension.
        let error = from_bytes("f1", "app.bin", "bin", &[0xff, 0xfe, 0x00]).expect_err("refused");
        assert!(error.contains(".bin"), "{error}");
    }

    /// **support-19 / A-MECH-23.** `.mdx`, `.ipynb` and `.pem` are plain text
    /// and were refused for having an extension the table did not list.
    #[test]
    fn mdx_ipynb_and_pem_are_accepted_as_text() {
        assert!(from_bytes("f1", "guide.mdx", "mdx", b"# Title\n\nSome text").is_ok());
        assert!(from_bytes("f1", "nb.ipynb", "ipynb", b"{\"cells\": []}").is_ok());
        assert!(from_bytes("f1", "key.pem", "pem", b"-----BEGIN CERTIFICATE-----").is_ok());
    }

    /// A small file with an extension this table has never seen is sniffed
    /// as text when it decodes clean as UTF-8, rather than refused outright.
    #[test]
    fn a_small_unknown_extension_that_decodes_as_utf8_is_accepted() {
        let file = from_bytes("f1", "app.rules", "rules", b"allow: *\ndeny: none").expect("sniffed as text");
        assert_eq!(file.text, "allow: *\ndeny: none");
    }

    /// Genuine binary data under an unknown extension is still refused: the
    /// sniff only ever accepts what actually decodes as UTF-8.
    #[test]
    fn a_small_unknown_extension_that_is_not_utf8_is_still_refused() {
        let error = from_bytes("f1", "app.bin", "bin", &[0xff, 0xfe, 0x00]).expect_err("refused");
        assert!(error.contains(".bin"), "{error}");
    }

    #[test]
    fn extensionless_bytes_must_be_text() {
        assert!(from_bytes("f1", "README", "", b"plain text").is_ok());
        assert!(from_bytes("f1", "blob", "", &[0xff, 0xfe, 0x00]).is_err());
    }

    #[test]
    fn garbage_is_not_a_spreadsheet_pdf_or_doc() {
        assert!(from_bytes("f1", "s.xlsx", "xlsx", b"not a workbook").is_err());
        assert!(from_bytes("f1", "d.pdf", "pdf", b"not a pdf").is_err());
        assert!(from_bytes("f1", "d.docx", "docx", b"not a zip").is_err());
    }

    #[test]
    fn a_docx_yields_its_paragraphs() {
        let mut buf = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(&mut buf);
        archive
            .start_file("word/document.xml", zip::write::SimpleFileOptions::default())
            .expect("zip entry");
        archive
            .write_all(
                br#"<?xml version="1.0"?><w:document xmlns:w="x"><w:body><w:p><w:r><w:t>Hello</w:t></w:r></w:p><w:p><w:r><w:t>A &amp; B</w:t></w:r></w:p></w:body></w:document>"#,
            )
            .expect("zip body");
        drop(archive);
        let file = from_bytes("f1", "letter.docx", "docx", buf.get_ref()).expect("decodes");
        assert_eq!(file.text, "Hello\nA & B");
    }

    #[test]
    fn a_workbook_yields_its_sheets_as_csv() {
        let sheet = r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>task</t></is></c><c r="B1" t="inlineStr"><is><t>owner</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>files</t></is></c><c r="B2"><v>2</v></c></row></sheetData></worksheet>"#;
        let workbook = r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Tasks" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let types = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
        let workbook_rels = r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let mut buf = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(&mut buf);
        for (name, body) in [
            ("[Content_Types].xml", types),
            ("_rels/.rels", rels),
            ("xl/workbook.xml", workbook),
            ("xl/_rels/workbook.xml.rels", workbook_rels),
            ("xl/worksheets/sheet1.xml", sheet),
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .expect("zip entry");
            archive.write_all(body.as_bytes()).expect("zip body");
        }
        drop(archive);
        let file = from_bytes("f1", "table.xlsx", "xlsx", buf.get_ref()).expect("decodes");
        assert!(file.text.contains("## Tasks"), "{}", file.text);
        assert!(file.text.contains("task,owner"), "{}", file.text);
        assert!(file.text.contains("files,2"), "{}", file.text);
    }

    #[test]
    fn an_empty_file_has_nothing_to_send() {
        assert!(from_bytes("f1", "empty.txt", "txt", b"").is_err());
        assert!(from_bytes("f1", "blank.md", "md", b"  \n ").is_err());
    }
}
