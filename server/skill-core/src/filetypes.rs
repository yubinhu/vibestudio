// File extension -> (category, language, label), shared through the file APIs.
// Categories: markdown | code | data | image | text | binary.

fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

/// Returns (category, language, label) for a file name.
pub fn file_type(name: &str) -> (&'static str, &'static str, &'static str) {
    match name {
        "Dockerfile" => return ("code", "dockerfile", "Dockerfile"),
        "Makefile" => return ("code", "makefile", "Makefile"),
        ".gitignore" => return ("text", "plaintext", "gitignore"),
        _ => {}
    }
    match ext_of(name).as_str() {
        "md" => ("markdown", "markdown", "Markdown"),
        "markdown" => ("markdown", "markdown", "Markdown"),
        "mdx" => ("markdown", "markdown", "MDX"),

        "py" => ("code", "python", "Python"),
        "sh" => ("code", "bash", "Shell"),
        "bash" => ("code", "bash", "Shell"),
        "zsh" => ("code", "bash", "Shell"),
        "ps1" => ("code", "powershell", "PowerShell"),

        "js" => ("code", "javascript", "JavaScript"),
        "mjs" => ("code", "javascript", "JavaScript"),
        "cjs" => ("code", "javascript", "JavaScript"),
        "jsx" => ("code", "javascript", "JSX"),
        "ts" => ("code", "typescript", "TypeScript"),
        "tsx" => ("code", "typescript", "TSX"),

        "rb" => ("code", "ruby", "Ruby"),
        "go" => ("code", "go", "Go"),
        "rs" => ("code", "rust", "Rust"),
        "java" => ("code", "java", "Java"),
        "c" => ("code", "c", "C"),
        "h" => ("code", "c", "C Header"),
        "cpp" => ("code", "cpp", "C++"),
        "php" => ("code", "php", "PHP"),

        "json" => ("data", "json", "JSON"),
        "jsonc" => ("data", "json", "JSONC"),
        "yaml" => ("data", "yaml", "YAML"),
        "yml" => ("data", "yaml", "YAML"),
        "toml" => ("data", "ini", "TOML"),
        "ini" => ("data", "ini", "INI"),
        "xml" => ("data", "xml", "XML"),
        "csv" => ("data", "plaintext", "CSV"),
        "tsv" => ("data", "plaintext", "TSV"),

        "html" => ("code", "xml", "HTML"),
        "htm" => ("code", "xml", "HTML"),
        "css" => ("code", "css", "CSS"),
        "scss" => ("code", "scss", "SCSS"),
        "sql" => ("code", "sql", "SQL"),

        "txt" => ("text", "plaintext", "Text"),
        "text" => ("text", "plaintext", "Text"),
        "log" => ("text", "plaintext", "Log"),

        "png" => ("image", "", "PNG"),
        "jpg" => ("image", "", "JPEG"),
        "jpeg" => ("image", "", "JPEG"),
        "gif" => ("image", "", "GIF"),
        "webp" => ("image", "", "WebP"),
        "bmp" => ("image", "", "BMP"),
        "ico" => ("image", "", "Icon"),
        "svg" => ("image", "xml", "SVG"),

        _ => ("text", "plaintext", "Text"),
    }
}

pub fn is_image(name: &str) -> bool {
    file_type(name).0 == "image"
}

/// Text-like category we render with syntax highlighting.
pub fn is_textual(name: &str) -> bool {
    matches!(file_type(name).0, "markdown" | "code" | "data" | "text")
}

/// MIME type for image extensions (matches the old /api/raw route).
pub fn image_mime(name: &str) -> &'static str {
    match ext_of(name).as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "ico" => "image/x-icon",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_types() {
        assert_eq!(file_type("foo.py"), ("code", "python", "Python"));
        assert_eq!(file_type("data.json"), ("data", "json", "JSON"));
        assert_eq!(file_type("Dockerfile"), ("code", "dockerfile", "Dockerfile"));
        assert_eq!(file_type("SKILL.md").0, "markdown");
        assert_eq!(file_type("noext"), ("text", "plaintext", "Text"));
        assert_eq!(file_type("weird.zzz"), ("text", "plaintext", "Text"));
    }

    #[test]
    fn image_and_text_helpers() {
        assert!(is_image("a.PNG")); // case-insensitive
        assert!(!is_image("a.py"));
        assert!(is_textual("a.md") && is_textual("a.py") && is_textual("a.json"));
        assert!(!is_textual("a.png"));
        assert_eq!(image_mime("x.svg"), "image/svg+xml");
        assert_eq!(image_mime("x.JPEG"), "image/jpeg");
        assert_eq!(image_mime("x.bin"), "application/octet-stream");
    }
}
